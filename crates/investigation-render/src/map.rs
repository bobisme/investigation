//! The bounded case map: `Cases/<case-id>/Map.canvas` (plan §12.6). Pure.
//!
//! A JSON Canvas 1.0 file with a mode-sensitive focus, planned only when the
//! view policy allows every feature in [`FEATURES`] (the viewer release is
//! inside the compatibility profile's supported range). It is a picture of
//! the typed record, never the place meaning lives:
//!
//! - Every node is a file node pointing at a generated note, so opening a
//!   box opens the record and backlinks behave as for any note. No text,
//!   link or group node is emitted, so nothing essential lives only in the
//!   canvas.
//! - Every edge carries its relation in words ([`LABELS`]). No colour is
//!   emitted anywhere; the map reads the same with colour disabled.
//! - The canvas holds no authored text: node IDs are record IDs, files are
//!   planned paths and labels are fixed words. A relation word that comes
//!   from a record's content (an assessment's relation, a comparison's
//!   outcome) is shown only when the audience may see that content, and an
//!   edge is drawn only from a record whose lineage the audience may see.
//! - It is bounded: at most [`MAX_PER_COLUMN`] records per column and
//!   [`MAX_EDGES`] edges. `Map Left Out.md` says what was left out and links
//!   the tables that list it; `Map Legend.md` explains every label used.
//! - Layout is a fixed grid: one column per focus stage, left to right, rows
//!   in a stable order (records linked to the map first, then a per-kind
//!   priority, then record ID). The same snapshot and policy give the same
//!   bytes.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

use investigation_core::{
    classification::Field,
    freshness::dependency_edges,
    identity::{Id, Kind},
    state::Record,
};
use serde_json::Value;

use crate::{
    PlanPath, RenderError, layout, marker,
    note::{BANNER, NoteContext, VAULT_SCHEMA, mentions, text_at},
    records::{
        lifecycle,
        overview::{Focus, focus},
        views::ViewNote,
    },
    text::inline_code,
    yaml::Frontmatter,
};

/// Compatibility-profile features the map uses. It is planned only when the
/// view policy allows all of them.
pub const FEATURES: &[&str] = &["canvas.file", "canvas.file_nodes", "canvas.labeled_edges"];
/// Most records in one column.
pub const MAX_PER_COLUMN: usize = 8;
/// Most columns of any focus.
pub const MAX_COLUMNS: usize = 6;
/// Most edges on one map.
pub const MAX_EDGES: usize = 96;
/// Most nodes on one map: the record columns, the legend and the left-out
/// note.
pub const MAX_NODES: usize = MAX_COLUMNS * MAX_PER_COLUMN + 2;

/// The legend note beside the map.
pub const LEGEND: &str = "Map Legend.md";
/// The note that says what the map leaves out.
pub const LEFT_OUT: &str = "Map Left Out.md";
/// Node IDs of the two key nodes; record nodes use record IDs.
pub const LEGEND_NODE: &str = "map-legend";
pub const LEFT_OUT_NODE: &str = "map-left-out";

const NODE_WIDTH: i64 = 400;
const NODE_HEIGHT: i64 = 240;
const COLUMN_PITCH: i64 = 640;
const ROW_PITCH: i64 = 320;
/// The key nodes sit in a row above the first record row.
const KEY_Y: i64 = -400;

/// Every edge label and what it means, in legend order.
pub const LABELS: &[(&str, &str)] = &[
    (
        "proposed answer to",
        "The hypothesis is a candidate explanation for the question.",
    ),
    (
        "rival of",
        "The hypothesis is recorded as a rival of the other explanation.",
    ),
    (
        "supports",
        "An assessment judged the evidence it weighs to support this hypothesis or claim.",
    ),
    (
        "contradicts",
        "An assessment judged the evidence it weighs to contradict this hypothesis or claim.",
    ),
    (
        "non-diagnostic for",
        "An assessment judged the evidence about equally expected under the rivals: it does \
         not discriminate.",
    ),
    (
        "inconclusive for",
        "An assessment could not tell how the evidence bears on this record.",
    ),
    (
        "assesses",
        "An assessment of this record whose relation word the renderer does not recognise; \
         read the assessment.",
    ),
    (
        "assesses, relation withheld",
        "An assessment of this record whose relation the view policy withholds. It may be \
         decisive.",
    ),
    ("weighs", "The assessment weighs this evidence."),
    (
        "weighs the rival",
        "The assessment states what this rival explanation expects.",
    ),
    ("cites", "The record cites this evidence."),
    ("observed in", "The observation was made in this run."),
    ("read from", "The observation was read from this artifact."),
    (
        "traces its claim to",
        "The observation's claim traces to an earlier observation.",
    ),
    (
        "shares a root with",
        "The observation depends on the same original account.",
    ),
    (
        "acquired from",
        "The artifact was acquired from this source.",
    ),
    (
        "derived from",
        "The artifact is derived reporting made from this input.",
    ),
    ("carries out", "The run executes this inquiry's plan."),
    ("produced", "The run produced this artifact."),
    (
        "inquires into",
        "The inquiry gathers information for this question.",
    ),
    (
        "tests",
        "The inquiry states a prediction for this hypothesis before its outcome is known.",
    ),
    (
        "option for",
        "The option is a candidate answer to this question.",
    ),
    (
        "ruled on against",
        "The option's disposition rests on this criterion.",
    ),
    ("depends on", "The option depends on this record."),
    (
        "criterion for",
        "The criterion judges the options of this question.",
    ),
    ("applies to", "The criterion applies to this option."),
    ("compares", "The comparison measures this option."),
    (
        "judged against: meets",
        "The comparison found the option meets this criterion.",
    ),
    (
        "judged against: fails",
        "The comparison found the option fails this criterion.",
    ),
    (
        "judged against: partially meets",
        "The comparison found the option partially meets this criterion.",
    ),
    (
        "judged against: unknown",
        "The comparison could not establish how the option fares on this criterion.",
    ),
    (
        "judged against: not applicable",
        "The comparison found this criterion does not apply to the option.",
    ),
    (
        "judged against, outcome withheld",
        "A comparison against this criterion whose outcome the view policy withholds.",
    ),
    ("rests on", "The record rests on this evidence."),
    ("chooses", "The decision chooses this option."),
    ("decides", "The decision answers this question."),
    ("relies on", "The decision relies on this comparison."),
    (
        "hard condition",
        "The decision holds only while this hard criterion is met.",
    ),
    (
        "compared against",
        "The decision was taken over this alternative.",
    ),
    (
        "reversal condition",
        "A change in this record is a stated reason to reconsider the decision.",
    ),
    (
        "names as cause",
        "The conclusion names this hypothesis as a causal factor.",
    ),
    ("supported by", "The conclusion rests on this assessment."),
    (
        "contradicted by",
        "The conclusion acknowledges this contradicting assessment.",
    ),
    (
        "sub-question of",
        "The question refines this broader question.",
    ),
];

/// One stage of a focus: the kinds it shows and the plural noun that names
/// them.
#[derive(Debug, Clone, Copy)]
pub struct Column {
    pub noun: &'static str,
    pub kinds: &'static [Kind],
}

/// The columns of a focus, left to right (plan §12.6).
pub(crate) const fn columns(focus: Focus) -> &'static [Column] {
    match focus {
        // question → rival hypotheses → discriminating observations → next
        // inquiry → bounded conclusion
        Focus::Explanation => &[
            Column {
                noun: "questions",
                kinds: &[Kind::Question],
            },
            Column {
                noun: "rival hypotheses",
                kinds: &[Kind::Hypothesis],
            },
            Column {
                noun: "assessments",
                kinds: &[Kind::Assessment],
            },
            Column {
                noun: "observations",
                kinds: &[Kind::Observation],
            },
            Column {
                noun: "inquiries and runs",
                kinds: &[Kind::Inquiry, Kind::Run],
            },
            Column {
                noun: "conclusions",
                kinds: &[Kind::Conclusion],
            },
        ],
        // question → options → major criteria / hard constraints → decisive
        // comparisons → conditional decision
        Focus::Evaluation => &[
            Column {
                noun: "questions",
                kinds: &[Kind::Question],
            },
            Column {
                noun: "options",
                kinds: &[Kind::Option],
            },
            Column {
                noun: "criteria and hard constraints",
                kinds: &[Kind::Criterion],
            },
            Column {
                noun: "comparisons",
                kinds: &[Kind::Comparison],
            },
            Column {
                noun: "decisions",
                kinds: &[Kind::Decision],
            },
        ],
        // questioned claim → primary acquisition roots → derived reporting →
        // conflicting sources and corrections
        Focus::Research => &[
            Column {
                noun: "questions",
                kinds: &[Kind::Question],
            },
            Column {
                noun: "sources",
                kinds: &[Kind::Source],
            },
            Column {
                noun: "acquisitions and derived reporting",
                kinds: &[Kind::Artifact],
            },
            Column {
                noun: "observations",
                kinds: &[Kind::Observation],
            },
            Column {
                noun: "conflicting and other assessments",
                kinds: &[Kind::Assessment],
            },
            Column {
                noun: "conclusions",
                kinds: &[Kind::Conclusion],
            },
        ],
        Focus::Measurement => &[
            Column {
                noun: "questions",
                kinds: &[Kind::Question],
            },
            Column {
                noun: "inquiries and runs",
                kinds: &[Kind::Inquiry, Kind::Run],
            },
            Column {
                noun: "measurements and observations",
                kinds: &[Kind::Observation],
            },
            Column {
                noun: "assessments and comparisons",
                kinds: &[Kind::Assessment, Kind::Comparison],
            },
            Column {
                noun: "conclusions and decisions",
                kinds: &[Kind::Conclusion, Kind::Decision],
            },
        ],
    }
}

/// A drawable relation: `from` states it about `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub from: Id,
    pub to: Id,
    pub label: &'static str,
}

/// A record that may appear in a column, with its rank inside the column
/// (lower first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    pub id: Id,
    pub column: usize,
    pub priority: u8,
}

/// The bounded selection: records per column in row order, every column's
/// candidate count, the edges drawn and how many edges there were.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub columns: Vec<Vec<Id>>,
    pub totals: Vec<usize>,
    pub edges: Vec<Relation>,
    pub edges_total: usize,
}

impl Selection {
    /// Column and row of a selected record.
    pub fn position(&self, id: Id) -> Option<(usize, usize)> {
        self.columns.iter().enumerate().find_map(|(column, ids)| {
            ids.iter()
                .position(|candidate| *candidate == id)
                .map(|row| (column, row))
        })
    }

    pub fn shown(&self) -> usize {
        self.columns.iter().map(Vec::len).sum()
    }
}

/// Column and row of a node.
type Position = (usize, usize);

/// Chooses the records and edges of a map. Columns fill left to right: in
/// each, records with a relation to a record already selected come first,
/// then lower `priority`, then record ID, up to [`MAX_PER_COLUMN`]. Edges
/// join two selected records, ordered by the position of their ends, up to
/// [`MAX_EDGES`].
pub fn select(columns: usize, candidates: &[Candidate], relations: &[Relation]) -> Selection {
    let mut selected: BTreeSet<Id> = BTreeSet::new();
    let mut chosen = Vec::with_capacity(columns);
    let mut totals = Vec::with_capacity(columns);
    for column in 0..columns {
        let mut here: Vec<(bool, u8, Id)> = candidates
            .iter()
            .filter(|candidate| candidate.column == column)
            .map(|candidate| {
                let linked = relations.iter().any(|relation| {
                    (relation.from == candidate.id && selected.contains(&relation.to))
                        || (relation.to == candidate.id && selected.contains(&relation.from))
                });
                (!linked, candidate.priority, candidate.id)
            })
            .collect();
        here.sort();
        here.dedup_by_key(|entry| entry.2);
        totals.push(here.len());
        let ids: Vec<Id> = here
            .into_iter()
            .take(MAX_PER_COLUMN)
            .map(|(_, _, id)| id)
            .collect();
        selected.extend(ids.iter().copied());
        chosen.push(ids);
    }
    let mut selection = Selection {
        columns: chosen,
        totals,
        edges: Vec::new(),
        edges_total: 0,
    };
    let mut edges: Vec<(Position, Position, &'static str, Relation)> = Vec::new();
    for relation in relations {
        if relation.from == relation.to {
            continue;
        }
        let (Some(from), Some(to)) = (
            selection.position(relation.from),
            selection.position(relation.to),
        ) else {
            continue;
        };
        if edges.iter().any(|(_, _, _, seen)| seen == relation) {
            continue;
        }
        edges.push((from, to, relation.label, relation.clone()));
    }
    edges.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    selection.edges_total = edges.len();
    selection.edges = edges
        .into_iter()
        .take(MAX_EDGES)
        .map(|(_, _, _, relation)| relation)
        .collect();
    selection
}

/// The relation `record` states through body `field`, in words, or `None`
/// for a field the map does not draw. `shown` is whether the audience may
/// see the record's content; the words that come from content need it.
fn label(record: &Record, field: &str, shown: bool) -> Option<&'static str> {
    let body = &record.body;
    Some(match (record.header.id.kind(), field) {
        (Kind::Hypothesis, "question_ref") => "proposed answer to",
        (Kind::Hypothesis, "rival_refs" | "alternative_to") => "rival of",
        (Kind::Hypothesis | Kind::Question, "evidence_refs") => "cites",
        (Kind::Assessment, "subject_ref") if !shown => "assesses, relation withheld",
        (Kind::Assessment, "subject_ref") => match text_at(body, "relation") {
            Some("supports") => "supports",
            Some("contradicts") => "contradicts",
            Some("non_diagnostic") => "non-diagnostic for",
            Some("inconclusive") => "inconclusive for",
            _ => "assesses",
        },
        (Kind::Assessment, "evidence_refs") => "weighs",
        (Kind::Assessment, "rival_expectations.rival_ref") => "weighs the rival",
        (Kind::Observation, "run_ref") => "observed in",
        (Kind::Observation, "basis.artifact_ref") => "read from",
        (Kind::Observation, "claim_origin.traces.origin.observation_ref") => "traces its claim to",
        (Kind::Observation, "claim_origin.dependence.roots") => "shares a root with",
        (Kind::Artifact, "source") => "acquired from",
        (Kind::Artifact, "transforms.input") => "derived from",
        (Kind::Run, "intent.plan_ref") => "carries out",
        (Kind::Run, "artifact_refs") => "produced",
        (Kind::Inquiry, "question_ref") => "inquires into",
        (Kind::Inquiry, "inquiry.confirmatory.predictions.rival_ref") => "tests",
        (Kind::Option, "question_ref") => "option for",
        (Kind::Option, "criterion_ref") => "ruled on against",
        (Kind::Option, "dependencies") => "depends on",
        (Kind::Criterion, "question_ref") => "criterion for",
        (Kind::Criterion, "option_refs") => "applies to",
        (Kind::Comparison, "option_ref") => "compares",
        (Kind::Comparison, "criterion_ref") if !shown => "judged against, outcome withheld",
        (Kind::Comparison, "criterion_ref") => match text_at(body, "outcome") {
            Some("meets") => "judged against: meets",
            Some("fails") => "judged against: fails",
            Some("partially_meets") => "judged against: partially meets",
            Some("not_applicable") => "judged against: not applicable",
            _ => "judged against: unknown",
        },
        (Kind::Comparison | Kind::Decision, "evidence_refs") => "rests on",
        (Kind::Decision, "choice.option_ref") => "chooses",
        (Kind::Decision, "question_ref") => "decides",
        (Kind::Decision, "comparison_refs") => "relies on",
        (Kind::Decision, "hard_conditions.criterion_ref") => "hard condition",
        (Kind::Decision, "compared_alternatives") => "compared against",
        (Kind::Decision, field) if field.starts_with("reversal_conditions.") => {
            "reversal condition"
        }
        (Kind::Conclusion, "causal_factors.hypothesis_ref") => "names as cause",
        (Kind::Conclusion, "support_refs") => "supported by",
        (Kind::Conclusion, "contradiction_refs") => "contradicted by",
        (Kind::Question, "parent") => "sub-question of",
        _ => return None,
    })
}

/// Whether a record has left evidentiary use.
fn ended(record: &Record) -> bool {
    lifecycle(record).is_some_and(|word| {
        matches!(
            word,
            "superseded" | "retracted" | "withdrawn" | "retired" | "rejected"
        )
    })
}

/// Rank inside a column, lower first, from content the audience may see.
fn priority(notes: &NoteContext<'_>, record: &Record, focus: Focus) -> u8 {
    if !(notes.visible(record) && notes.may_show(record, Field::Content)) {
        return 1;
    }
    let body = &record.body;
    let rank = match record.header.id.kind() {
        Kind::Assessment => match text_at(body, "relation") {
            Some("contradicts") => 0,
            Some("supports") => 1,
            _ => 2,
        },
        Kind::Criterion => u8::from(text_at(body, "kind") != Some("hard")),
        Kind::Comparison => {
            if notes
                .citations(record.header.id)
                .iter()
                .any(|citation| citation.by.kind() == Kind::Decision)
            {
                0
            } else if matches!(text_at(body, "outcome"), Some("unknown") | None) {
                2
            } else {
                1
            }
        }
        Kind::Artifact => u8::from(!is_original(record)),
        Kind::Observation => u8::from((focus == Focus::Measurement) != is_measurement(record)),
        _ => 0,
    };
    rank + if ended(record) { 4 } else { 0 }
}

fn is_measurement(record: &Record) -> bool {
    record.header.id.kind() == Kind::Observation
        && text_at(&record.body, "value.kind") == Some("measurement")
}

fn is_original(artifact: &Record) -> bool {
    match artifact.body.get("role").and_then(Value::as_str) {
        Some(role) => role == "original",
        None => artifact.body["transforms"]
            .as_array()
            .is_none_or(Vec::is_empty),
    }
}

/// The table view that lists a record, by its file name.
fn view_of(record: &Record) -> Option<&'static str> {
    Some(match record.header.id.kind() {
        Kind::Question => "Questions",
        Kind::Hypothesis => "Hypotheses",
        Kind::Option => "Options",
        Kind::Criterion => "Criteria",
        Kind::Comparison => "Comparison Matrix",
        Kind::Source => "Sources",
        Kind::Observation if is_measurement(record) => "Measurements",
        Kind::Artifact | Kind::Observation => "Evidence",
        Kind::Inquiry | Kind::Run => "Inquiries and Runs",
        Kind::Conclusion => "Conclusions",
        Kind::Decision => "Decisions",
        Kind::Review | Kind::Objection => "Reviews and Blindspots",
        Kind::Retrospective => "Retrospectives and Lessons",
        _ => return None,
    })
}

/// One planned case map: the canvas and its two notes.
pub(crate) struct CaseMap {
    pub files: Vec<(PlanPath, String)>,
    canvas: PlanPath,
    legend: PlanPath,
    left_out: PlanPath,
    focus: Focus,
    shown: usize,
    edges: usize,
}

impl CaseMap {
    /// The Overview's `## Map` section.
    pub(crate) fn overview_section(&self) -> String {
        format!(
            "\n## Map\n\n{} is a bounded picture of this case with {} focus: {} {} and {} \
             labelled {}. Its {} explains every label and {} says what it leaves out. The \
             notes and tables are the record; the map only arranges links to them.\n",
            layout::wikilink(&self.canvas, "Case map"),
            article(self.focus.word()),
            self.shown,
            if self.shown == 1 { "record" } else { "records" },
            self.edges,
            if self.edges == 1 { "arrow" } else { "arrows" },
            layout::wikilink(&self.legend, "legend"),
            layout::wikilink(&self.left_out, "Map Left Out"),
        )
    }
}

fn article(word: &str) -> String {
    if word.starts_with(['a', 'e', 'i', 'o', 'u']) {
        format!("an {word}")
    } else {
        format!("a {word}")
    }
}

/// Plans the case map of one case, or `None` when the view policy does not
/// allow canvas files or the case has no record to place.
pub(crate) fn plan(
    notes: &NoteContext<'_>,
    views: &[ViewNote],
) -> Result<Option<CaseMap>, RenderError> {
    if !notes.policy().allows_all(FEATURES) {
        return Ok(None);
    }
    let case = notes.case();
    let case_id = case.case_id();
    let state = case.state();
    let Some(case_record) = state.current(case_id) else {
        return Ok(None);
    };
    let focus = focus(case_record, state);
    let columns = columns(focus);
    let column_of = |kind: Kind| {
        columns
            .iter()
            .position(|column| column.kinds.contains(&kind))
    };
    // Every record of this case the audience may know exists, by ID.
    let mut known: BTreeMap<Id, &Record> = BTreeMap::new();
    for record in state.records() {
        let id = record.header.id;
        if record.header.case_id == case_id
            && id.kind() != Kind::Case
            && layout::record_kind(id.kind()).is_some()
            && notes.may_show(record, Field::Identity)
        {
            known.insert(id, record);
        }
    }
    let placeable: Vec<&Record> = known
        .values()
        .copied()
        .filter(|record| column_of(record.header.id.kind()).is_some())
        .collect();
    if placeable.is_empty() {
        return Ok(None);
    }
    let candidates: Vec<Candidate> = placeable
        .iter()
        .filter_map(|record| {
            Some(Candidate {
                id: record.header.id,
                column: column_of(record.header.id.kind())?,
                priority: priority(notes, record, focus),
            })
        })
        .collect();
    let placeable_ids: BTreeSet<Id> = placeable.iter().map(|record| record.header.id).collect();
    let mut relations: Vec<Relation> = Vec::new();
    for record in &placeable {
        if !notes.may_show(record, Field::Lineage) {
            continue;
        }
        let shown = notes.visible(record) && notes.may_show(record, Field::Content);
        let fields = dependency_edges(record)
            .unwrap_or_default()
            .into_iter()
            .map(|edge| (edge.field.to_owned(), edge.target.id))
            .chain(
                mentions(record)
                    .into_iter()
                    .map(|mention| (mention.field, mention.target)),
            );
        for (field, target) in fields {
            if !placeable_ids.contains(&target) {
                continue;
            }
            let Some(label) = label(record, &field, shown) else {
                continue;
            };
            let relation = Relation {
                from: record.header.id,
                to: target,
                label,
            };
            if !relations.contains(&relation) {
                relations.push(relation);
            }
        }
    }
    let selection = select(columns.len(), &candidates, &relations);

    let canvas_path = layout::case_map(case_id)?;
    let legend_path = layout::case_map_note(case_id, LEGEND)?;
    let left_out_path = layout::case_map_note(case_id, LEFT_OUT)?;
    let canvas = canvas(notes, &selection, &legend_path, &left_out_path)?;
    let legend = legend(
        notes,
        focus,
        columns,
        &selection,
        &canvas_path,
        &left_out_path,
    )?;
    let left_out = left_out(
        notes,
        columns,
        &selection,
        &known,
        &column_of,
        views,
        &canvas_path,
    )?;
    Ok(Some(CaseMap {
        files: vec![
            (canvas_path.clone(), canvas),
            (legend_path.clone(), legend),
            (left_out_path.clone(), left_out),
        ],
        canvas: canvas_path,
        legend: legend_path,
        left_out: left_out_path,
        focus,
        shown: selection.shown(),
        edges: selection.edges.len(),
    }))
}

/// A JSON string literal.
fn json(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

fn node(out: &mut String, id: &str, file: &PlanPath, x: i64, y: i64) {
    let _ = write!(
        out,
        "\n    {{\"id\": {}, \"type\": \"file\", \"file\": {}, \"x\": {x}, \"y\": {y}, \
         \"width\": {NODE_WIDTH}, \"height\": {NODE_HEIGHT}}}",
        json(id),
        json(file.as_str())
    );
}

/// The canvas bytes: JSON Canvas 1.0, file nodes and labelled edges only,
/// one node or edge per line.
fn canvas(
    notes: &NoteContext<'_>,
    selection: &Selection,
    legend: &PlanPath,
    left_out: &PlanPath,
) -> Result<String, RenderError> {
    let mut nodes = Vec::new();
    let mut line = String::new();
    node(&mut line, LEGEND_NODE, legend, 0, KEY_Y);
    nodes.push(std::mem::take(&mut line));
    node(&mut line, LEFT_OUT_NODE, left_out, COLUMN_PITCH, KEY_Y);
    nodes.push(std::mem::take(&mut line));
    for (column, ids) in selection.columns.iter().enumerate() {
        for (row, id) in ids.iter().enumerate() {
            let Some(path) = notes.note_path(*id)? else {
                continue;
            };
            let x = i64::try_from(column).unwrap_or(0) * COLUMN_PITCH;
            let y = i64::try_from(row).unwrap_or(0) * ROW_PITCH;
            node(&mut line, &id.to_string(), &path, x, y);
            nodes.push(std::mem::take(&mut line));
        }
    }
    let mut edges = Vec::new();
    let mut pairs: BTreeMap<(Id, Id), usize> = BTreeMap::new();
    for edge in &selection.edges {
        let (Some(from), Some(to)) = (selection.position(edge.from), selection.position(edge.to))
        else {
            continue;
        };
        let (from_side, to_side) = match from.0.cmp(&to.0) {
            std::cmp::Ordering::Less => ("right", "left"),
            std::cmp::Ordering::Greater => ("left", "right"),
            std::cmp::Ordering::Equal if from.1 < to.1 => ("bottom", "top"),
            std::cmp::Ordering::Equal => ("top", "bottom"),
        };
        let count = pairs.entry((edge.from, edge.to)).or_default();
        *count += 1;
        let id = if *count == 1 {
            format!("{}--{}", edge.from, edge.to)
        } else {
            format!("{}--{}--{count}", edge.from, edge.to)
        };
        edges.push(format!(
            "\n    {{\"id\": {}, \"fromNode\": {}, \"fromSide\": \"{from_side}\", \"toNode\": {}, \
             \"toSide\": \"{to_side}\", \"label\": {}}}",
            json(&id),
            json(&edge.from.to_string()),
            json(&edge.to.to_string()),
            json(edge.label)
        ));
    }
    let list = |items: &[String]| {
        if items.is_empty() {
            "[]".to_owned()
        } else {
            format!("[{}\n  ]", items.join(","))
        }
    };
    Ok(format!(
        "{{\n  \"nodes\": {},\n  \"edges\": {}\n}}\n",
        list(&nodes),
        list(&edges)
    ))
}

/// Frontmatter, H1, banner and the case line of a map note. Map notes are
/// views (`inv_kind: view`), so a native Base over the case's records, such
/// as the timeline's, leaves them out as it leaves out the table views.
fn opening(notes: &NoteContext<'_>, name: &str, role: &str) -> Result<String, RenderError> {
    let case = notes.case();
    let head = case.head();
    let mut front = Frontmatter::new();
    front
        .text("inv_kind", "view")
        .text("inv_view", role)
        .text("inv_case_id", &case.case_id().to_string())
        .link("inv_case", &layout::case_overview(case.case_id())?)
        .integer(marker::SEQUENCE_KEY, head.sequence)
        .text(marker::SNAPSHOT_KEY, head.head_hash.as_str())
        .text("inv_schema", VAULT_SCHEMA)
        .list("tags", &["investigation/map"]);
    let mut out = front.finish();
    let _ = write!(out, "# {name}\n\n{BANNER}\n");
    let _ = writeln!(
        out,
        "Case: {}. Snapshot: sequence {}, head {}.",
        notes.link(case.case_id())?,
        head.sequence,
        inline_code(head.head_hash.as_str())
    );
    Ok(out)
}

fn legend(
    notes: &NoteContext<'_>,
    focus: Focus,
    columns: &[Column],
    selection: &Selection,
    canvas: &PlanPath,
    left_out: &PlanPath,
) -> Result<String, RenderError> {
    let mut out = opening(notes, "Map Legend", "map_legend")?;
    let nouns: Vec<&str> = columns.iter().map(|column| column.noun).collect();
    let _ = write!(
        out,
        "\n## What the map shows\n\n{} is a bounded picture of this case with {} focus. Its \
         columns, left to right: {}.\n\n\
         Every box is a generated note: opening it opens the record, and backlinks work as for \
         any note. Every arrow runs from the record that states a relation to the record it \
         names, and carries that relation in words. The map uses no colour, so no meaning \
         depends on colour. It is a picture of the typed record, not the record: each note and \
         the tables under Views are authoritative, and only the relations listed below are \
         drawn. Every recorded relation is listed under Typed links in each note.\n",
        layout::wikilink(canvas, "The case map"),
        article(focus.word()),
        nouns.join(", then ")
    );
    out.push_str("\n## Arrow labels\n\n");
    let used: BTreeSet<&str> = selection.edges.iter().map(|edge| edge.label).collect();
    if used.is_empty() {
        out.push_str("No arrow is drawn: no relation joins two records on the map.\n");
    } else {
        out.push_str("| Label | Meaning |\n| --- | --- |\n");
        for (label, meaning) in LABELS.iter().filter(|(label, _)| used.contains(label)) {
            let _ = writeln!(out, "| {} | {meaning} |", inline_code(label));
        }
    }
    let total: usize = selection.totals.iter().sum();
    let _ = write!(
        out,
        "\n## Bounds\n\nAt most {MAX_PER_COLUMN} records per column and {MAX_EDGES} arrows. In \
         each column, records linked to a record already on the map come first, then the more \
         decisive ones (contradicting before supporting assessments, hard criteria, comparisons \
         a decision relies on and those with a known outcome, original acquisitions), then the \
         oldest. This map shows {} of {} \
         records in its columns and {} of {} arrows between them; {} lists what it leaves out \
         and where to find it.\n",
        selection.shown(),
        total,
        selection.edges.len(),
        selection.edges_total,
        layout::wikilink(left_out, "Map Left Out"),
    );
    out.push_str(
        "\n## Your own copy\n\nThe map is regenerated on every render; editing it is drift. To \
         keep an editable copy, run `investigation vault copy-map --to Boards/<name>.canvas`. \
         Rendering never writes in `Boards/`, and nothing drawn on a board is recorded.\n",
    );
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn left_out(
    notes: &NoteContext<'_>,
    columns: &[Column],
    selection: &Selection,
    known: &BTreeMap<Id, &Record>,
    column_of: &dyn Fn(Kind) -> Option<usize>,
    views: &[ViewNote],
    canvas: &PlanPath,
) -> Result<String, RenderError> {
    let mut out = opening(notes, "Map Left Out", "map_left_out")?;
    let overview = notes.link(notes.case().case_id())?.replace('|', "\\|");
    let planned: BTreeMap<&str, &PlanPath> =
        views.iter().map(|view| (view.name, &view.path)).collect();
    // Where records are listed: their planned table views, or the Overview,
    // whose Records section lists every record.
    let places = |records: &[&Record]| -> String {
        let mut names: Vec<&str> = records
            .iter()
            .filter_map(|record| view_of(record))
            .collect();
        names.sort_unstable();
        names.dedup();
        let links: Vec<String> = names
            .into_iter()
            .filter_map(|name| planned.get(name).map(|path| layout::wikilink(path, name)))
            .map(|link| link.replace('|', "\\|"))
            .collect();
        if links.is_empty() {
            overview.clone()
        } else {
            links.join(", ")
        }
    };
    let shown: BTreeSet<Id> = selection.columns.iter().flatten().copied().collect();
    let _ = write!(
        out,
        "\n{} shows at most {MAX_PER_COLUMN} records per column and {MAX_EDGES} arrows. This \
         note says what it leaves out; the tables and the Overview list every record.\n\n\
         ## Columns\n\n",
        layout::wikilink(canvas, "The case map"),
    );
    out.push_str("| Column | Shown | Left out | Listed in |\n| --- | --- | --- | --- |\n");
    for (index, column) in columns.iter().enumerate() {
        let missing: Vec<&Record> = known
            .values()
            .copied()
            .filter(|record| {
                column_of(record.header.id.kind()) == Some(index)
                    && !shown.contains(&record.header.id)
            })
            .collect();
        let all: Vec<&Record> = known
            .values()
            .copied()
            .filter(|record| column_of(record.header.id.kind()) == Some(index))
            .collect();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            column.noun,
            selection.columns.get(index).map_or(0, Vec::len),
            missing.len(),
            if all.is_empty() {
                "none recorded".to_owned()
            } else {
                places(if missing.is_empty() { &all } else { &missing })
            }
        );
    }
    out.push_str("\n## Outside this focus\n\n");
    let mut outside: BTreeMap<&str, Vec<&Record>> = BTreeMap::new();
    for record in known.values() {
        if column_of(record.header.id.kind()).is_none()
            && let Some((folder, _)) = layout::record_kind(record.header.id.kind())
        {
            outside.entry(folder).or_default().push(record);
        }
    }
    if outside.is_empty() {
        out.push_str("Every record of this case belongs to a column of the map.\n");
    } else {
        out.push_str(
            "Records of these kinds are not drawn on the map at all.\n\n\
             | Kind | Records | Listed in |\n| --- | --- | --- |\n",
        );
        for (folder, records) in &outside {
            let _ = writeln!(
                out,
                "| {folder} | {} | {} |",
                records.len(),
                places(records)
            );
        }
    }
    out.push_str("\n## Arrows\n\n");
    if selection.edges_total > selection.edges.len() {
        let _ = writeln!(
            out,
            "Showing {} of {} arrows between records on the map. Every relation is listed under \
             Typed links in each note.",
            selection.edges.len(),
            selection.edges_total
        );
    } else {
        let _ = writeln!(
            out,
            "Every drawn relation between two records on the map is shown ({}). Relations to \
             records left out are listed under Typed links in each note.",
            selection.edges.len()
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(kind: Kind, n: u64) -> Id {
        format!("{}_01890a5d-ac96-774b-bcce-{n:012x}", kind.prefix())
            .parse()
            .unwrap()
    }

    #[test]
    fn every_focus_fits_the_bounds_and_places_each_kind_once() {
        assert_eq!(MAX_NODES, MAX_COLUMNS * MAX_PER_COLUMN + 2);
        for focus in [
            Focus::Explanation,
            Focus::Evaluation,
            Focus::Research,
            Focus::Measurement,
        ] {
            let columns = columns(focus);
            assert!(columns.len() <= MAX_COLUMNS, "{focus:?}");
            let kinds: Vec<Kind> = columns
                .iter()
                .flat_map(|c| c.kinds.iter().copied())
                .collect();
            let unique: BTreeSet<_> = kinds.iter().map(|k| k.prefix()).collect();
            assert_eq!(unique.len(), kinds.len(), "{focus:?}");
            for kind in kinds {
                assert!(layout::record_kind(kind).is_some(), "{kind:?} has a note");
            }
        }
    }

    #[test]
    fn labels_are_plain_words_with_a_meaning_each() {
        let mut seen = BTreeSet::new();
        for (label, meaning) in LABELS {
            assert!(seen.insert(*label), "{label} twice");
            assert!(
                label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || matches!(c, ' ' | '-' | ':' | ',')),
                "{label}"
            );
            assert!(
                meaning.ends_with('.') && !meaning.contains('|'),
                "{meaning}"
            );
        }
        // Every label the planner can produce is in the legend.
        let kinds = [
            Kind::Hypothesis,
            Kind::Question,
            Kind::Assessment,
            Kind::Observation,
            Kind::Artifact,
            Kind::Run,
            Kind::Inquiry,
            Kind::Option,
            Kind::Criterion,
            Kind::Comparison,
            Kind::Decision,
            Kind::Conclusion,
        ];
        let fields = [
            "question_ref",
            "rival_refs",
            "alternative_to",
            "evidence_refs",
            "subject_ref",
            "rival_expectations.rival_ref",
            "run_ref",
            "basis.artifact_ref",
            "claim_origin.traces.origin.observation_ref",
            "claim_origin.dependence.roots",
            "source",
            "transforms.input",
            "intent.plan_ref",
            "artifact_refs",
            "inquiry.confirmatory.predictions.rival_ref",
            "criterion_ref",
            "dependencies",
            "option_refs",
            "option_ref",
            "choice.option_ref",
            "comparison_refs",
            "hard_conditions.criterion_ref",
            "compared_alternatives",
            "reversal_conditions.assumption_ref",
            "causal_factors.hypothesis_ref",
            "support_refs",
            "contradiction_refs",
            "parent",
        ];
        let bodies = [
            serde_json::json!({}),
            serde_json::json!({"relation":"supports","outcome":"meets"}),
            serde_json::json!({"relation":"contradicts","outcome":"fails"}),
            serde_json::json!({"relation":"non_diagnostic","outcome":"partially_meets"}),
            serde_json::json!({"relation":"inconclusive","outcome":"not_applicable"}),
        ];
        let mut produced = BTreeSet::new();
        for kind in kinds {
            for body in &bodies {
                let record = fixture_record(id(kind, 1), body.clone());
                for field in fields {
                    for shown in [true, false] {
                        if let Some(label) = label(&record, field, shown) {
                            assert!(seen.contains(label), "{label} not in LABELS");
                            produced.insert(label);
                        }
                    }
                }
            }
        }
        // And the legend lists nothing the planner never draws.
        assert_eq!(produced.len(), LABELS.len());
    }

    fn fixture_record(id: Id, body: Value) -> Record {
        const TIME: &str = "2026-11-21T22:00:00.000Z";
        serde_json::from_value(serde_json::json!({
            "header":{
                "id":id, "case_id":id, "schema_version":"inv.record.v0-alpha",
                "created_by":"agent:map-test", "actor_descriptor_self_reported":null,
                "created_at":TIME, "recorded_at":TIME, "recorded_by":"agent:map-test",
                "observed_at":null, "acquired_at":null, "acceptance_sequence":1,
                "revision":1, "history":[],
                "classification":{"level":"internal", "reason":null, "tags":[]},
                "provenance":{"references":[]}, "extensions":{}
            },
            "body":body
        }))
        .unwrap()
    }

    #[test]
    fn selection_is_bounded_linked_first_and_stable() {
        let question = id(Kind::Question, 1);
        let hypotheses: Vec<Id> = (2..14).map(|n| id(Kind::Hypothesis, n)).collect();
        let mut candidates = vec![Candidate {
            id: question,
            column: 0,
            priority: 0,
        }];
        let mut relations = Vec::new();
        for (index, hypothesis) in hypotheses.iter().enumerate() {
            candidates.push(Candidate {
                id: *hypothesis,
                column: 1,
                priority: 0,
            });
            // Only the later hypotheses name the question.
            if index >= 6 {
                relations.push(Relation {
                    from: *hypothesis,
                    to: question,
                    label: "proposed answer to",
                });
            }
        }
        // Rivals in both directions; one duplicate.
        for pair in hypotheses.windows(2) {
            relations.push(Relation {
                from: pair[1],
                to: pair[0],
                label: "rival of",
            });
        }
        relations.push(relations[0].clone());
        let selection = select(2, &candidates, &relations);
        assert_eq!(selection.totals, [1, 12]);
        assert_eq!(selection.columns[0], [question]);
        assert_eq!(selection.columns[1].len(), MAX_PER_COLUMN);
        // The six linked hypotheses first, then the two oldest.
        assert_eq!(selection.columns[1][..6], hypotheses[6..]);
        assert_eq!(selection.columns[1][6..], hypotheses[..2]);
        assert!(selection.edges.len() <= MAX_EDGES);
        let unique: BTreeSet<_> = selection
            .edges
            .iter()
            .map(|e| (e.from, e.to, e.label))
            .collect();
        assert_eq!(unique.len(), selection.edges.len());
        // Reversed input gives the same selection.
        let mut reversed_candidates = candidates.clone();
        reversed_candidates.reverse();
        let mut reversed_relations = relations.clone();
        reversed_relations.reverse();
        assert_eq!(
            select(2, &reversed_candidates, &reversed_relations),
            selection
        );
    }

    #[test]
    fn edges_are_capped_and_counted() {
        let left: Vec<Id> = (0..8).map(|n| id(Kind::Hypothesis, n)).collect();
        let right: Vec<Id> = (100..108).map(|n| id(Kind::Observation, n)).collect();
        let mut candidates = Vec::new();
        for &l in &left {
            candidates.push(Candidate {
                id: l,
                column: 0,
                priority: 0,
            });
        }
        for &r in &right {
            candidates.push(Candidate {
                id: r,
                column: 1,
                priority: 0,
            });
        }
        let mut relations = Vec::new();
        for &l in &left {
            for &r in &right {
                for label in ["cites", "rests on"] {
                    relations.push(Relation {
                        from: l,
                        to: r,
                        label,
                    });
                }
            }
        }
        let selection = select(2, &candidates, &relations);
        assert_eq!(selection.edges_total, 128);
        assert_eq!(selection.edges.len(), MAX_EDGES);
    }
}
