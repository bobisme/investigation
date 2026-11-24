//! `explain`: a bounded, deterministic walk of the recorded chain behind one
//! record.
//!
//! From a conclusion, decision, comparison or any other record, the walk
//! follows the exact revisions each record cites (support, contradiction,
//! causal test, evidence, assumptions, comparisons, artifact and derivation
//! links) and the records that name it as their subject (assessments,
//! objections and reviews), breadth first, down to Observations, Artifacts and
//! Sources. Every node shows the record's effective status and freshness with
//! reasons, open objections, confidence with named downgrade reasons, measurement
//! validity and flags, claim origins, absence and equivalence readings,
//! evidence reuse, retention and lineage, as `show` computes them. Observations
//! and Artifacts end their branch with the exact selector and an `evidence
//! read` argv; raw bytes are never expanded.
//!
//! It is a presentation of committed records, not new reasoning. Recorded and
//! authored text is quoted data with a `content_trust` label. A record the
//! audience may not see appears by ID alone and is not walked.
//!
//! Bounds: link hops (`--depth`), records (`--max-nodes`) and serialized bytes
//! (`--max-bytes`). Records past the hop or record bound are listed in `more`
//! with an argv that continues the walk from them; nodes cut for budget are
//! counted in `omitted` with an argv that reads the whole walk. Every argv
//! names the same snapshot. Children are ordered contradicting material first,
//! so a bound never silently drops every contradiction while keeping support.

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::{
    absence_gate::{self, AbsenceBasis, EquivalenceBasis},
    authored_text,
    basis::basis_digest,
    classification::{AudiencePolicy, Classifier, Field},
    command::RevisionRef,
    freshness::{FreshnessIndex, Propagation, dependency_edges},
    identity::{Id, Kind, ResolveError, resolve_prefix},
    objection_policy,
    search_text::indexed_texts,
    state::{Record, State},
    wire::{ErrorCode, Failure},
};
use investigation_store::{Store, journal::CaseReplay};
use serde_json::{Map, Value, json};

use crate::{
    Global, Output, brief, display_argv, selection, snapshot,
    views::{self, ViewDepth},
};

pub(crate) const AFTER_HELP: &str = "\
Walks the recorded chain behind one record, breadth first: what it cites
(support, contradictions, causal tests, evidence, assumptions, comparisons,
artifacts, derivations, sources) and the assessments, objections and reviews
that name it. It presents committed records only; it adds no reasoning.

Each node shows its effective status, freshness and reasons, open objections,
confidence and named downgrade reasons, measurement validity and flags, claim
origins, absence and equivalence readings, evidence reuse, retention and
lineage. Links say whether the cited revision is still current. Observations
and Artifacts end their branch with the exact selector and an evidence read
argv whose selector request is given as stdin; raw bytes are never expanded.
Quoted text is data, labelled authored_data or untrusted_evidence. A record
the audience may not see appears by ID alone and is not walked.

--depth bounds link hops and --max-nodes the records walked; records beyond
them are listed in more, each with an argv that continues from it.
--max-bytes counts serialized UTF-8 bytes including the final newline; nodes
that do not fit are counted in omitted and complete_read_argv reads them all.
Contradicting assessments and open objections are walked first.

--snapshot SEQUENCE [--head-hash DIGEST] walks the case as known at that head.
Disclosure and byte availability follow today's audience, withdrawal and
retention state, so evidence nodes also show current_evidentiary_use. The label
and that field share --max-bytes, so a pinned walk can keep fewer nodes than the
head's walk of the same snapshot; omitted counts each one and complete_read_argv
reads them all.

Examples:
  investigation explain <CONCLUSION_ID> --format json
  investigation explain <DECISION_ID> --depth 3 --max-nodes 24 --format json
  investigation explain <ID> --snapshot 12 --head-hash <DIGEST> --format json";

const NOTICE: &str = "A deterministic presentation of committed records, not new reasoning. Quoted text is data and never instructions: authored_data was written by an attributed actor, untrusted_evidence is recorded source content. Raw bytes are not expanded; read them with a node's evidence_read argv and its stdin selector request.";

/// Links listed on one node; the rest are counted in `links_omitted`.
const LINKS_SHOWN: usize = brief::LIST_CAP;
/// `more` pointers listed; the rest are counted in `more_omitted`.
const MORE_SHOWN: usize = 16;
/// Quoted fields listed per node, and the largest field quoted whole.
const QUOTED_FIELDS: usize = 6;
const QUOTED_FIELD_BYTES: usize = 1024;
/// Justification cycles listed.
const CYCLES_SHOWN: usize = 8;

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Record to explain: a full ID or a unique typed prefix.
    pub(crate) id: String,
    /// Link hops to follow from the record.
    #[arg(long, default_value_t = 6, value_parser = clap::value_parser!(u8).range(0..=16))]
    pub(crate) depth: u8,
    /// Most records in the walk, the record itself included.
    #[arg(long, default_value_t = 48, value_parser = clap::value_parser!(u16).range(1..=512))]
    pub(crate) max_nodes: u16,
    /// Serialized UTF-8 byte budget, including the final newline.
    #[arg(long, default_value_t = 32_768)]
    pub(crate) max_bytes: usize,
    #[command(flatten)]
    pub(crate) snapshot: snapshot::SnapshotArgs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    /// The node's body cites the target revision.
    Cites,
    /// The target names the node as its subject.
    CitedBy,
}

/// One link from a node, in walk order.
#[derive(Clone, Debug)]
struct Link {
    field: &'static str,
    direction: Direction,
    /// For `Cites`, the exact cited revision; for `CitedBy`, the current
    /// revision of the record that names the node.
    target: RevisionRef,
    /// A forward edge whose dependent inherits the target's basis: the edges
    /// a justification cycle is made of.
    justification: bool,
    /// Lower walks first: contradicting material and open objections lead.
    priority: u8,
    /// False for context links (questions, scopes) that are listed only.
    walk: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Expansion {
    Expanded,
    DepthBound,
    Leaf,
    Withheld,
}

impl Expansion {
    fn as_str(self) -> &'static str {
        match self {
            Self::Expanded => "expanded",
            Self::DepthBound => "depth_bound",
            Self::Leaf => "leaf",
            Self::Withheld => "withheld",
        }
    }
}

#[derive(Clone, Debug)]
struct Via {
    from: Id,
    field: &'static str,
    direction: Direction,
}

#[derive(Clone, Debug)]
struct Node {
    id: Id,
    depth: u8,
    via: Option<Via>,
    expansion: Expansion,
    links: Vec<Link>,
}

#[derive(Clone, Debug)]
struct More {
    id: Id,
    from: Id,
    field: &'static str,
    cause: &'static str,
}

/// The walked graph: nodes in breadth-first order, the first being the root.
#[derive(Debug, Default)]
struct Graph {
    nodes: Vec<Node>,
    index: BTreeMap<Id, usize>,
    more: Vec<More>,
}

/// Body fields that are context, not rationale: they are listed but not walked.
fn followed(kind: Kind, field: &str) -> bool {
    !(field.ends_with("question_ref")
        || field == "parent"
        || (kind == Kind::Criterion && field == "option_refs")
        || (kind == Kind::Objection && matches!(field, "subject_ref" | "review_ref")))
}

/// Records whose own links are listed by `show`, not walked: a review's read
/// set restates the chain it reviewed.
fn leaf(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Review | Kind::Retrospective | Kind::Annotation | Kind::Case
    )
}

fn decode_ref(value: &Value) -> Option<RevisionRef> {
    serde_json::from_value(value.clone()).ok()
}

/// Records that name another record as their subject, keyed by that subject.
fn reverse_index(state: &State, case: Id) -> BTreeMap<Id, Vec<Link>> {
    let mut index: BTreeMap<Id, Vec<(u64, Link)>> = BTreeMap::new();
    for record in state.records() {
        if record.header.case_id != case {
            continue;
        }
        let body = &record.body;
        let (field, priority) = match record.header.id.kind() {
            Kind::Assessment => (
                "assessed_by",
                match body["relation"].as_str() {
                    Some("contradicts") => 0,
                    Some("supports") => 2,
                    _ => 1,
                },
            ),
            Kind::Objection => ("objected_by", if body["status"] == "open" { 0 } else { 1 }),
            Kind::Review => ("reviewed_by", 3),
            _ => continue,
        };
        let Some(subject) = decode_ref(&body["subject_ref"]) else {
            continue;
        };
        index.entry(subject.id).or_default().push((
            record.header.acceptance_sequence,
            Link {
                field,
                direction: Direction::CitedBy,
                target: record.reference(),
                justification: false,
                priority,
                walk: true,
            },
        ));
    }
    index
        .into_iter()
        .map(|(subject, mut links)| {
            links.sort_by_key(|(sequence, link)| (link.priority, *sequence, link.target.id));
            (subject, links.into_iter().map(|(_, link)| link).collect())
        })
        .collect()
}

struct Walker<'a> {
    state: &'a State,
    classifier: Classifier<'a>,
    policy: AudiencePolicy,
    reverse: BTreeMap<Id, Vec<Link>>,
}

impl<'a> Walker<'a> {
    /// `today` governs disclosure; `state` is the snapshot walked.
    fn new(state: &'a State, today: &'a State, case: Id) -> Self {
        Self {
            state,
            classifier: Classifier::new(today),
            policy: AudiencePolicy::default(),
            reverse: reverse_index(state, case),
        }
    }

    fn hidden(&self, record: &Record) -> bool {
        !self.classifier.may_show(record, Field::Title, &self.policy)
    }

    /// Every link of `record` in walk order: forward edges in field order,
    /// then the records naming it, contradicting material first throughout.
    fn links(&self, record: &Record) -> Vec<Link> {
        let kind = record.header.id.kind();
        let mut links: Vec<Link> = dependency_edges(record)
            .unwrap_or_default()
            .into_iter()
            .map(|edge| Link {
                walk: followed(kind, edge.field),
                field: edge.field,
                direction: Direction::Cites,
                target: edge.target,
                justification: edge.propagation == Propagation::Inherit,
                priority: if edge.field == "contradiction_refs" {
                    0
                } else {
                    2
                },
            })
            .collect();
        links.extend(
            self.reverse
                .get(&record.header.id)
                .into_iter()
                .flatten()
                .cloned(),
        );
        links.sort_by_key(|link| link.priority);
        links
    }

    fn walk(&self, root: Id, max_depth: u8, max_nodes: usize) -> Graph {
        let mut graph = Graph::default();
        graph.index.insert(root, 0);
        graph.nodes.push(Node {
            id: root,
            depth: 0,
            via: None,
            expansion: Expansion::Expanded,
            links: Vec::new(),
        });
        let mut cursor = 0;
        while cursor < graph.nodes.len() {
            let (id, depth) = (graph.nodes[cursor].id, graph.nodes[cursor].depth);
            let position = cursor;
            cursor += 1;
            let Some(record) = self.state.current(id) else {
                continue;
            };
            if self.hidden(record) {
                graph.nodes[position].expansion = Expansion::Withheld;
                continue;
            }
            if leaf(id.kind()) && position != 0 {
                graph.nodes[position].expansion = Expansion::Leaf;
                continue;
            }
            let links = self.links(record);
            if depth >= max_depth {
                graph.nodes[position].expansion = Expansion::DepthBound;
                for link in links.iter().filter(|link| link.walk) {
                    if self.state.current(link.target.id).is_some() {
                        graph.more.push(More {
                            id: link.target.id,
                            from: id,
                            field: link.field,
                            cause: "depth_bound",
                        });
                    }
                }
            } else {
                for link in links.iter().filter(|link| link.walk) {
                    let target = link.target.id;
                    if graph.index.contains_key(&target) || self.state.current(target).is_none() {
                        continue;
                    }
                    if graph.nodes.len() >= max_nodes {
                        graph.more.push(More {
                            id: target,
                            from: id,
                            field: link.field,
                            cause: "node_bound",
                        });
                        continue;
                    }
                    graph.index.insert(target, graph.nodes.len());
                    graph.nodes.push(Node {
                        id: target,
                        depth: depth + 1,
                        via: Some(Via {
                            from: id,
                            field: link.field,
                            direction: link.direction,
                        }),
                        expansion: Expansion::Expanded,
                        links: Vec::new(),
                    });
                }
            }
            graph.nodes[position].links = links;
        }
        let mut seen = BTreeSet::new();
        let index = &graph.index;
        graph
            .more
            .retain(|more| !index.contains_key(&more.id) && seen.insert(more.id));
        graph
    }
}

/// Justification cycles among walked nodes: paths of inheriting forward
/// edges that return to a record already on the path. Each cycle is listed
/// from the record it returns to, once, in discovery order.
fn cycles(graph: &Graph) -> Vec<Vec<Id>> {
    fn visit(
        graph: &Graph,
        node: usize,
        path: &mut Vec<usize>,
        color: &mut [u8],
        found: &mut Vec<Vec<Id>>,
    ) {
        color[node] = 1;
        path.push(node);
        for link in &graph.nodes[node].links {
            if !(link.justification && link.direction == Direction::Cites) {
                continue;
            }
            let Some(&target) = graph.index.get(&link.target.id) else {
                continue;
            };
            match color[target] {
                0 => visit(graph, target, path, color, found),
                1 => {
                    let start = path
                        .iter()
                        .position(|step| *step == target)
                        .expect("an open node is on the path");
                    let mut cycle: Vec<Id> = path[start..]
                        .iter()
                        .map(|step| graph.nodes[*step].id)
                        .collect();
                    cycle.push(graph.nodes[target].id);
                    found.push(cycle);
                }
                _ => {}
            }
        }
        path.pop();
        color[node] = 2;
    }
    let mut found = Vec::new();
    if !graph.nodes.is_empty() {
        let mut color = vec![0; graph.nodes.len()];
        visit(graph, 0, &mut Vec::new(), &mut color, &mut found);
    }
    found
}

/// Program, workspace, case and snapshot arguments shared by every argv.
struct Argv {
    prefix: Vec<String>,
    pin: Vec<String>,
}

impl Argv {
    fn read(&self, tail: &[&str]) -> Vec<String> {
        let mut argv = self.prefix.clone();
        argv.extend(tail.iter().map(|part| (*part).to_owned()));
        argv.extend(self.pin.iter().cloned());
        argv.extend(["--format".into(), "json".into()]);
        argv
    }

    fn show(&self, id: Id) -> Vec<String> {
        self.read(&["show", &id.to_string(), "--depth", "full"])
    }

    fn explain(&self, id: Id, depth: u8, max_nodes: u16) -> Vec<String> {
        self.read(&[
            "explain",
            &id.to_string(),
            "--depth",
            &depth.to_string(),
            "--max-nodes",
            &max_nodes.to_string(),
        ])
    }

    /// `evidence read` reads today's bytes and takes no snapshot.
    fn evidence_read(&self, artifact: Id) -> Vec<String> {
        let mut argv = self.prefix.clone();
        argv.extend(
            [
                "evidence",
                "read",
                &artifact.to_string(),
                "--selector-file",
                "-",
                "--format",
                "json",
            ]
            .map(str::to_owned),
        );
        argv
    }
}

struct Context<'a> {
    store: &'a Store,
    replay: &'a CaseReplay,
    today: Option<&'a State>,
    index: FreshnessIndex,
    walker: Walker<'a>,
    argv: Argv,
}

/// Fields of a `show` summary projection that a node carries.
const PROJECTED: &[&str] = &[
    "title",
    "basis_revision",
    "effective_status",
    "freshness",
    "material_caveat",
    "contradiction",
    "confidence",
    "coverage",
    "measurement_validity",
    "selector_validation",
    "observation_validity",
    "measurement_flags",
    "evidentiary_use",
    "current_evidentiary_use",
    "retention",
    "source_standing",
    "lineage",
    "choice",
    "objection",
    "inherited_trust",
];

/// Small enumerated body fields recorded on the node, as stored.
const RECORDED: &[&str] = &[
    "relation",
    "outcome",
    "verdict",
    "kind",
    "severity",
    "status",
    "lifecycle",
    "materiality",
    "disposition",
    "validity",
    "evidence_kind",
];

impl Context<'_> {
    fn state(&self) -> &State {
        &self.replay.state
    }

    fn may_show(&self, record: &Record, field: Field) -> bool {
        self.walker
            .classifier
            .may_show(record, field, &self.walker.policy)
    }

    fn node(&self, node: &Node, graph: &Graph) -> Value {
        let mut out = Map::new();
        out.insert("id".into(), json!(node.id));
        out.insert("type".into(), json!(node.id.kind().to_string()));
        out.insert("depth".into(), json!(node.depth));
        if let Some(via) = &node.via {
            out.insert(
                "via".into(),
                json!({"from":via.from,"field":via.field,"direction":direction(via.direction)}),
            );
        }
        out.insert("expansion".into(), json!(node.expansion.as_str()));
        let Some(record) = self.state().current(node.id) else {
            return Value::Object(out);
        };
        if node.expansion == Expansion::Withheld {
            out.insert("content_state".into(), json!("restricted"));
            return Value::Object(out);
        }
        let effective =
            views::effective_state_as_of(self.store, self.replay, &self.index, record, self.today);
        let projected = views::project_record(
            record,
            self.replay,
            self.today,
            ViewDepth::Summary,
            &effective,
        );
        for key in PROJECTED {
            if let Some(value) = projected.get(*key) {
                out.insert((*key).into(), value.clone());
            }
        }
        // An Observation's lineage is its Artifact's, shown on that node.
        if node.id.kind() == Kind::Observation {
            out.remove("lineage");
        }
        if out
            .get("measurement_flags")
            .is_some_and(|report| report["flags"].as_array().is_some_and(Vec::is_empty))
        {
            out.remove("measurement_flags");
        }
        let recorded: Map<String, Value> = RECORDED
            .iter()
            .filter_map(|key| {
                let value = record.body.get(*key)?;
                value.as_str().map(|text| ((*key).to_owned(), json!(text)))
            })
            .collect();
        if !recorded.is_empty() {
            out.insert("recorded".into(), Value::Object(recorded));
        }
        if let Some(reuse) = projected.get("evidence_reuse") {
            out.insert("evidence_reuse".into(), reuse_axes(reuse));
        }
        self.kind_fields(record, &mut out);
        if let Some((key, origins)) = views::origin_projection(self.replay, record) {
            out.insert(key.into(), origins);
        }
        out.insert("open_objections".into(), self.open_objections(record));
        let (quoted, omitted) = quoted(record);
        out.insert("quoted".into(), json!(quoted));
        if omitted > 0 {
            out.insert("quoted_omitted".into(), json!(omitted));
        }
        if node.expansion != Expansion::Leaf {
            let links: Vec<Value> = node
                .links
                .iter()
                .take(LINKS_SHOWN)
                .map(|link| self.link(link, graph))
                .collect();
            out.insert("links".into(), json!(links));
            if node.links.len() > LINKS_SHOWN {
                out.insert(
                    "links_omitted".into(),
                    json!(node.links.len() - LINKS_SHOWN),
                );
            }
        }
        // Records not fully shown here name the read that shows them.
        let partial = node.expansion != Expansion::Expanded
            || node.depth == 0
            || out.contains_key("quoted_omitted")
            || out.contains_key("links_omitted");
        if partial {
            out.insert("show_argv".into(), json!(self.argv.show(node.id)));
        }
        Value::Object(out)
    }

    fn link(&self, link: &Link, graph: &Graph) -> Value {
        let mut out = json!({
            "field":link.field,
            "direction":direction(link.direction),
            "ref":link.target,
            "in_walk":graph.index.contains_key(&link.target.id),
        });
        let state = match self.state().current(link.target.id) {
            None => "missing",
            Some(current) if current.header.revision.0 == link.target.revision => "current",
            Some(current) => {
                out["current_revision"] = json!(current.header.revision.0);
                let cited = self.state().revision(link.target);
                let same_basis = cited.is_some_and(|cited| {
                    matches!(
                        (basis_digest(cited), basis_digest(current)),
                        (Ok(left), Ok(right)) if left == right
                    )
                });
                if same_basis { "revised" } else { "stale" }
            }
        };
        out["link_state"] = json!(state);
        if link.direction == Direction::CitedBy
            && let Some(record) = self.state().current(link.target.id)
            && !self.walker.hidden(record)
            && let Some(relation) = record.body["relation"].as_str()
        {
            out["relation"] = json!(relation);
        }
        out
    }

    /// Open objections on the record, or for a reviewed record on its basis:
    /// references and severity only; the text is read from each objection.
    fn open_objections(&self, record: &Record) -> Value {
        let reviewed = matches!(
            record.header.id.kind(),
            Kind::Conclusion | Kind::Decision | Kind::Assessment | Kind::Comparison
        );
        let found = if reviewed {
            objection_policy::relevant_open_objections(self.state(), record)
        } else {
            objection_policy::open_objections(
                self.state(),
                record.header.case_id,
                &[record.header.id].into(),
            )
        };
        match found {
            Ok(found) => json!(
                found
                    .into_iter()
                    .map(|objection| json!({
                        "objection_ref":objection.objection_ref,
                        "subject_ref":objection.subject_ref,
                        "severity":objection.severity,
                    }))
                    .collect::<Vec<_>>()
            ),
            Err(failure) => json!({"unavailable":failure.error}),
        }
    }

    fn kind_fields(&self, record: &Record, out: &mut Map<String, Value>) {
        let body = &record.body;
        match record.header.id.kind() {
            Kind::Assessment => {
                let rivals: Vec<Value> = body["rival_expectations"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|item| json!({"rival_ref":item["rival_ref"],"expectation":item["expectation"]}))
                    .collect();
                out.insert("rival_expectations".into(), json!(rivals));
                self.gate_readings(record, out);
            }
            Kind::Comparison => {
                out.insert(
                    "comparison".into(),
                    json!({"option_ref":body["option_ref"],"criterion_ref":body["criterion_ref"],
                        "outcome":body["outcome"],"value":body["value"],"retracted":body["retracted"]}),
                );
                self.gate_readings(record, out);
            }
            Kind::Review => {
                out.insert(
                    "review".into(),
                    json!({"subject_ref":body["subject_ref"],
                        "read_set_size":body["read_set"].as_array().map_or(0, Vec::len),
                        "objection_refs":body["objection_refs"]}),
                );
            }
            Kind::Observation => {
                out.insert("evidence".into(), self.observation_evidence(record));
            }
            Kind::Artifact => {
                out.insert("bytes".into(), self.artifact_bytes(record));
            }
            _ => {}
        }
    }

    /// The absence gate's and the equivalence gate's present reading of an
    /// Assessment or Comparison that carries such a basis.
    fn gate_readings(&self, record: &Record, out: &mut Map<String, Value>) {
        let body = &record.body;
        let case = record.header.case_id;
        let evidence: Vec<RevisionRef> =
            serde_json::from_value(body["evidence_refs"].clone()).unwrap_or_default();
        if let Ok(basis) = serde_json::from_value::<AbsenceBasis>(body["absence"].clone()) {
            out.insert(
                "absence".into(),
                json!(absence_gate::evaluate_absence(
                    self.state(),
                    case,
                    &basis,
                    &evidence
                )),
            );
        }
        if let Ok(basis) = serde_json::from_value::<EquivalenceBasis>(body["equivalence"].clone()) {
            out.insert(
                "equivalence".into(),
                json!(absence_gate::evaluate_equivalence(
                    self.state(),
                    case,
                    &basis,
                    Some(record.header.acceptance_sequence)
                )),
            );
        }
    }

    /// The exact selector and quote status of a source-backed Observation and
    /// the argv that reads its bytes. An attributed assertion has none.
    fn observation_evidence(&self, record: &Record) -> Value {
        let basis = &record.body["basis"];
        if basis["kind"] != "source" {
            return json!({"basis":basis["kind"],"evidence_read":null,
                "reason":"An attributed assertion cites no source bytes."});
        }
        let Some(reference) = decode_ref(&basis["artifact_ref"]) else {
            return json!({"basis":"source","state":"unreadable"});
        };
        let artifact = self.state().revision(reference);
        if artifact.is_none_or(|artifact| !self.may_show(artifact, Field::Metadata)) {
            return json!({"basis":"source","artifact_ref":reference,"content_state":"restricted"});
        }
        let selector = &basis["selector"];
        let available = self
            .store
            .unavailable_artifacts(
                self.state(),
                self.today.unwrap_or(self.state()),
                &[reference],
            )
            .is_empty()
            && !self.withdrawn_today(reference);
        json!({
            "basis":"source",
            "artifact_ref":reference,
            "selector":selector,
            "quote_state":basis["text_quote"]["state"],
            "bytes_available":available,
            "evidence_read":{
                "argv":self.argv.evidence_read(reference.id),
                "stdin":{"schema_version":selector["schema_version"],
                    "artifact_digest":selector["artifact_digest"],"selector":selector["selector"]},
            },
        })
    }

    /// Retention of an Artifact's bytes and, when they are retained, the argv
    /// that reads the whole Artifact.
    fn artifact_bytes(&self, record: &Record) -> Value {
        let body = &record.body;
        if !self.may_show(record, Field::Metadata) {
            return json!({"content_state":"restricted"});
        }
        let reference = record.reference();
        let reference_only = body["availability"] == "external";
        let retained = !reference_only
            && self
                .store
                .unavailable_artifacts(
                    self.state(),
                    self.today.unwrap_or(self.state()),
                    &[reference],
                )
                .is_empty()
            && !self.withdrawn_today(reference);
        let mut out = json!({
            "availability":body["availability"],
            "integrity":body["integrity"],
            "role":body.get("role").cloned().unwrap_or(Value::Null),
            "retained":retained,
            "reference_only":reference_only,
        });
        out["evidence_read"] = match body["digest"].as_str() {
            Some(digest) if retained => json!({
                "argv":self.argv.evidence_read(reference.id),
                "stdin":{"schema_version":"inv.selector.v1","artifact_digest":digest,
                    "selector":{"type":"whole"}},
            }),
            _ => Value::Null,
        };
        out
    }

    /// True when a historical read cites an Artifact that today's state has
    /// withdrawn: availability follows today.
    fn withdrawn_today(&self, reference: RevisionRef) -> bool {
        self.today.is_some_and(|today| {
            today.current(reference.id).is_some_and(|record| {
                investigation_core::source_ops::evidentiary_use(today, record).state
                    == investigation_core::source_ops::UseState::Withdrawn
            })
        })
    }
}

fn direction(direction: Direction) -> &'static str {
    match direction {
        Direction::Cites => "cites",
        Direction::CitedBy => "cited_by",
    }
}

/// Evidence reuse as three separate axes: the reuse classification, the
/// recorded deviations from the frozen plan, and the Runs' own chronology.
fn reuse_axes(reuse: &Value) -> Value {
    json!({
        "rule_version":reuse["rule_version"],
        "reuse":{
            "classification":reuse["classification"],
            "label":reuse["label"],
            "counts_as_prospective":reuse["counts_as_prospective"],
            "plan_ref":reuse["plan_ref"],
            "plan_reasons":reuse["plan_reasons"],
        },
        "deviations":{
            "items":reuse["deviations"],
            "early_stop":reuse["early_stop"],
            "stop_rule_applied":reuse["stop_rule_applied"],
            "stop_rule_differs_without_deviation":reuse["stop_rule_differs_without_deviation"],
        },
        "chronology":reuse["chronology"],
    })
}

/// Recorded and authored text of a record as labelled, quoted data. A field
/// is quoted whole or not at all; the rest are counted.
fn quoted(record: &Record) -> (Vec<Value>, usize) {
    let mut shown = Vec::new();
    let mut omitted = 0;
    for text in indexed_texts(record) {
        if shown.len() < QUOTED_FIELDS && text.text.len() <= QUOTED_FIELD_BYTES {
            shown.push(json!({
                "field":text.field,
                "content_trust":text.label.content_trust(),
                "text":text.text,
            }));
        } else {
            omitted += 1;
        }
    }
    (shown, omitted)
}

fn resolve(replay: &CaseReplay, selector: &str, records: Vec<String>) -> Result<Id, Failure> {
    resolve_prefix(
        selector,
        replay.state.records().map(|record| record.header.id),
    )
    .map_err(|error| match error {
        ResolveError::InvalidPrefix => Failure::new(
            ErrorCode::InvalidInput,
            "Supply an ID or unique typed prefix.",
        )
        .at("/id")
        .with_argv(records),
        ResolveError::NotFound => Failure::new(
            ErrorCode::UnknownReference,
            "No record in this case matches that ID.",
        )
        .at("/id")
        .with_argv(records),
        ResolveError::Ambiguous(candidates) => {
            let mut failure = Failure::new(
                ErrorCode::AmbiguousReference,
                "Several records match. Use a full ID.",
            )
            .at("/id");
            failure.error.actual = Some(json!(candidates));
            failure
        }
    })
}

pub(crate) fn run(store: &mut Store, global: &Global, args: Args) -> Result<Output, Failure> {
    let case = selection::select_case(store, global.case.as_deref())?;
    let mut prefix = selection::workspace_argv(store.workspace());
    prefix.extend(["--case".into(), case.to_string()]);
    let retry = brief::follow_up(&prefix, &["explain", &args.id]);
    let pinned = snapshot::resolve(store.case(case)?, &args.snapshot, retry)?;
    let replay = pinned.read();
    let today = pinned.today();
    let root = resolve(replay, &args.id, brief::follow_up(&prefix, &["brief"]))?;
    let context = Context {
        store,
        replay,
        today,
        index: FreshnessIndex::build(&replay.state, case),
        walker: Walker::new(&replay.state, today.unwrap_or(&replay.state), case),
        argv: Argv {
            prefix: prefix.clone(),
            pin: snapshot::pin_argv(&replay.snapshot),
        },
    };
    let graph = context
        .walker
        .walk(root, args.depth, usize::from(args.max_nodes));
    let mut nodes = graph.nodes.iter().map(|node| context.node(node, &graph));
    let mut subject = nodes.next().expect("the walk holds its root");
    let mut rest: Vec<Value> = nodes.collect();
    // Each node keeps its level; the limit is stated once below.
    for node in std::iter::once(&mut subject).chain(&mut rest) {
        crate::inherited_trust::compact(node);
    }
    let mut caps = brief::Caps::default();
    caps.cap("subject", &mut subject, Some(context.argv.show(root)));
    let more: Vec<Value> = graph
        .more
        .iter()
        .take(MORE_SHOWN)
        .map(|more| {
            json!({"id":more.id,"from":more.from,"field":more.field,"cause":more.cause,
                "argv":context.argv.explain(more.id, args.depth, args.max_nodes)})
        })
        .collect();
    let found = cycles(&graph);
    let mut output = json!({
        "schema_version":"inv.explain.v0-alpha",
        "ok":true,
        "result":"explained",
        "snapshot":replay.snapshot,
        "historical":pinned.label().is_some(),
        "notice":NOTICE,
        // The byte budget is not echoed, so the minimum size does not depend on it.
        "bounds":{"depth":args.depth,"max_nodes":args.max_nodes,"walked_nodes":graph.nodes.len()},
        "subject":subject,
        "cycles":found.iter().take(CYCLES_SHOWN).collect::<Vec<_>>(),
        "more":more,
        "omitted":caps.into_entries(),
    });
    if found.len() > CYCLES_SHOWN {
        output["cycles_omitted"] = json!(found.len() - CYCLES_SHOWN);
    }
    if graph.more.len() > MORE_SHOWN {
        output["more_omitted"] = json!(graph.more.len() - MORE_SHOWN);
    }
    if let Some(label) = pinned.label() {
        output["historical_read"] = label;
    }
    let argv = &context.argv;
    let budget = args.max_bytes.saturating_sub(1);
    let complete = |bytes: usize| {
        argv.read(&[
            "explain",
            &root.to_string(),
            "--depth",
            &args.depth.to_string(),
            "--max-nodes",
            &args.max_nodes.to_string(),
            "--max-bytes",
            &bytes
                .saturating_add(crate::inherited_trust::BRIEF_MARGIN)
                .to_string(),
        ])
    };
    let view = brief::fill(
        output,
        vec![brief::Section::new("nodes", rest)],
        budget,
        complete,
    )
    .map_err(|error| {
        let minimum = error.minimum_bytes + 1;
        let mut failure =
            Failure::new(ErrorCode::BudgetTooSmall, error.to_string()).at("/max_bytes");
        failure.error.expected = Some(json!({"minimum_bytes":minimum}));
        failure.error.actual = Some(json!({"requested_bytes":args.max_bytes}));
        failure.with_argv(complete(minimum))
    })?;
    let view = crate::inherited_trust::brief_limits(view, budget);
    let text = text(&view);
    Ok(Output::new(view, text))
}

fn text(view: &Value) -> String {
    let mut lines = Vec::new();
    let mut line = |node: &Value| {
        let indent = "  ".repeat(node["depth"].as_u64().unwrap_or(0) as usize);
        let mut entry = format!(
            "{indent}{} r{}",
            node["id"].as_str().unwrap_or("?"),
            node["basis_revision"].as_u64().unwrap_or(0)
        );
        if let Some(via) = node.get("via") {
            entry.push_str(&format!(
                " via {} ({})",
                via["field"].as_str().unwrap_or("?"),
                via["direction"].as_str().unwrap_or("?")
            ));
        }
        if node["expansion"] == "withheld" {
            entry.push_str("  restricted");
        } else {
            entry.push_str(&format!(
                "  [{} / {}]  {}",
                node["effective_status"]["state"]
                    .as_str()
                    .unwrap_or("unknown"),
                node["freshness"]["label"]
                    .as_str()
                    .or_else(|| node["freshness"]["state"].as_str())
                    .unwrap_or("unknown"),
                authored_text::quoted(
                    "title",
                    authored_text::AUTHORED_DATA,
                    node["title"].as_str().unwrap_or("")
                )
            ));
        }
        if let Some(relation) = node["recorded"]["relation"].as_str() {
            entry.push_str(&format!("  relation {relation}"));
        }
        if let Some(level) = node["inherited_trust"]["level"].as_str() {
            entry.push_str(&format!("  inherited trust {level}"));
        }
        if let Some(read) = node["evidence"]["evidence_read"]
            .get("argv")
            .or_else(|| node["bytes"]["evidence_read"].get("argv"))
            .and_then(Value::as_array)
        {
            let argv: Vec<String> = read
                .iter()
                .filter_map(|part| part.as_str().map(str::to_owned))
                .collect();
            entry.push_str(&format!(
                "\n{indent}  read: {} < selector",
                display_argv(&argv)
            ));
        }
        lines.push(entry);
    };
    line(&view["subject"]);
    for node in view["nodes"].as_array().into_iter().flatten() {
        line(node);
    }
    for cycle in view["cycles"].as_array().into_iter().flatten() {
        let ids: Vec<&str> = cycle
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        lines.push(format!("Justification cycle: {}", ids.join(" -> ")));
    }
    let more = view["more"].as_array().map_or(0, Vec::len)
        + view["more_omitted"].as_u64().unwrap_or(0) as usize;
    if more > 0 {
        lines.push(format!(
            "More: {more} record(s) beyond the walk bounds; each entry in more has an argv."
        ));
    }
    if let Some(limits) = view["inherited_trust"]["limits"].as_str() {
        lines.push(format!("Trust limits: {limits}"));
    }
    if let Some(argv) = view["complete_read_argv"].as_array() {
        let argv: Vec<String> = argv
            .iter()
            .filter_map(|part| part.as_str().map(str::to_owned))
            .collect();
        lines.push(format!(
            "Omitted for budget: {} node(s). Read all: {}",
            view["omitted_items"]["nodes"].as_u64().unwrap_or(0),
            display_argv(&argv)
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(kind: &str, n: u32) -> Id {
        format!("{kind}_00000000-0000-7000-8000-{n:012}")
            .parse()
            .unwrap()
    }

    fn node(id: Id, links: &[(Id, bool)]) -> Node {
        Node {
            id,
            depth: 0,
            via: None,
            expansion: Expansion::Expanded,
            links: links
                .iter()
                .map(|(target, justification)| Link {
                    field: "evidence_refs",
                    direction: Direction::Cites,
                    target: RevisionRef {
                        id: *target,
                        revision: 1,
                    },
                    justification: *justification,
                    priority: 2,
                    walk: true,
                })
                .collect(),
        }
    }

    fn graph(nodes: Vec<Node>) -> Graph {
        let index = nodes
            .iter()
            .enumerate()
            .map(|(position, node)| (node.id, position))
            .collect();
        Graph {
            nodes,
            index,
            more: Vec::new(),
        }
    }

    #[test]
    fn justification_cycles_are_reported_from_the_record_they_return_to() {
        let (a, b, c) = (id("assess", 1), id("assess", 2), id("obs", 3));
        let cyclic = graph(vec![
            node(a, &[(b, true)]),
            node(b, &[(c, true), (a, true)]),
            node(c, &[]),
        ]);
        assert_eq!(cycles(&cyclic), vec![vec![a, b, a]]);
    }

    #[test]
    fn subject_and_change_only_links_do_not_form_cycles() {
        let (a, b) = (id("concl", 1), id("assess", 2));
        let acyclic = graph(vec![node(a, &[(b, true)]), node(b, &[(a, false)])]);
        assert!(cycles(&acyclic).is_empty());
    }

    #[test]
    fn evidence_reuse_keeps_reuse_deviations_and_chronology_apart() {
        let reuse = json!({
            "rule_version":"evidence-reuse.v1","classification":"post_hoc","label":"post hoc",
            "counts_as_prospective":false,"plan_ref":{"id":"inq","revision":1},"plan_reasons":[],
            "deviations":[{"kind":"early_stop"}],"early_stop":true,"stop_rule_applied":"n=10",
            "stop_rule_differs_without_deviation":false,"chronology":{"runs":[],"note":"verbatim"},
        });
        let axes = reuse_axes(&reuse);
        assert_eq!(axes["reuse"]["classification"], "post_hoc");
        assert!(axes["reuse"].get("early_stop").is_none());
        assert_eq!(axes["deviations"]["early_stop"], true);
        assert_eq!(axes["deviations"]["items"][0]["kind"], "early_stop");
        assert_eq!(axes["chronology"]["note"], "verbatim");
        assert!(axes["chronology"].get("classification").is_none());
    }

    #[test]
    fn context_fields_are_listed_but_not_walked() {
        assert!(!followed(Kind::Hypothesis, "question_ref"));
        assert!(!followed(Kind::Objection, "subject_ref"));
        assert!(followed(Kind::Assessment, "subject_ref"));
        assert!(followed(Kind::Conclusion, "contradiction_refs"));
        assert!(leaf(Kind::Review));
    }
}
