//! Generated case graphs and legal edit histories compare the incremental
//! freshness index with an independent full recomputation (W10.27, §20.1).
//!
//! Each case builds a small typed graph through the real store (sources,
//! artifacts, observations, a validated conclusion, options, criteria,
//! comparisons and an accepted decision, each with a validation review), then
//! applies a generated history of criterion edits, evidence corrections,
//! withdrawals, objections, relinking, unrelated metadata, multi-operation
//! batches and stale (rejected) edits. After every step the incrementally
//! updated index must agree with `support/freshness_oracle.rs` on every
//! current record, every historical revision and the Overview. A rejected
//! command must leave the journal, the state and the index unchanged. At the
//! end, every snapshot is rebuilt from the journal and re-read, and the
//! store's dependency projection is rebuilt and compared.
//!
//! The executor uses core helpers (`basis_digest`, `required_read_set`) only
//! to build legal review payloads; the oracle never calls the index, its
//! dependency table or the shared successor predicate.
//!
//! Seeds are fixed. `INV_FRESHNESS_SEED=<u64>` replays one seed,
//! `INV_FRESHNESS_CASES=<n>` runs more cases per seed locally, and
//! `INV_FRESHNESS_DEBUG=1` prints rejected and skipped steps. A failure
//! prints the seed, the shrunk history, the operations as executed and the
//! first divergent review with both the index's and the oracle's reasons.

#[path = "support/freshness_oracle.rs"]
mod oracle;

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::PathBuf,
};

use investigation_core::{
    basis::basis_digest,
    command::RevisionRef,
    freshness::{Freshness, FreshnessIndex, RecordFreshness, Trigger},
    identity::{Id, Kind},
    objection_policy::open_objections,
    review_ops::required_read_set,
    state::{Record, State},
    wire::{ErrorCode, Failure, Receipt, Snapshot},
};
use investigation_store::{Store, Workspace, journal::CaseReplay, projection::Projection};
use oracle::{Level, Oracle, Role, Verdict};
use proptest::{
    prelude::*,
    test_runner::{Config, RngAlgorithm, TestCaseError, TestError, TestRng, TestRunner},
};
use serde_json::{Value, json};

const ACTOR: &str = "agent:freshness-oracle";
const DEFAULT_SEEDS: [u64; 2] = [0x5EED_2710, 0x0B5E_55ED];
const DEFAULT_CASES: u32 = 12;

// ---------------------------------------------------------------------------
// Generated graphs and histories
// ---------------------------------------------------------------------------

/// Which options a criterion covers: both, only the chosen one, only the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    All,
    Chosen,
    Other,
    Both,
}

#[derive(Clone, Debug)]
struct Graph {
    sources: u8,
    /// The source each observation reads.
    observations: Vec<u8>,
    /// (hard, scope) per criterion; the first always covers the chosen option.
    criteria: Vec<(bool, Scope)>,
    /// Bit masks over observations (empty masks fall back to the first one).
    conclusion_support: u8,
    comparison_evidence: Vec<u8>,
    cite_assumption: bool,
}

#[derive(Clone, Copy, Debug)]
enum CriterionChange {
    Threshold(u8),
    Unit,
    Weighting,
    Rescope(Scope),
    Soften,
    Restate,
}

#[derive(Clone, Copy, Debug)]
enum ObservationChange {
    Value(u8),
    Invalid,
    Restate,
}

#[derive(Clone, Debug)]
enum Edit {
    AmendCriterion {
        criterion: u8,
        change: CriterionChange,
    },
    CorrectObservation {
        observation: u8,
        change: ObservationChange,
    },
    RetractObservation {
        observation: u8,
    },
    /// An `entire` withdrawal, or a `partial` one that later readers can
    /// acknowledge.
    WithdrawSource {
        source: u8,
        partial: bool,
    },
    WithdrawArtifact {
        source: u8,
        partial: bool,
    },
    AssessSource {
        source: u8,
        concern: bool,
    },
    RaiseObjection {
        target: u8,
        critical: bool,
    },
    CloseObjection {
        objection: u8,
        withdraw: bool,
    },
    AssessAssumption {
        assumption: u8,
        status: u8,
    },
    AddMaterialAssumption {
        target: u8,
    },
    Relink {
        comparison: u8,
        evidence: u8,
    },
    RetractComparison {
        comparison: u8,
    },
    DisposeOption {
        option: u8,
        lifecycle: u8,
    },
    SubmitReview {
        subject: u8,
    },
    Annotate {
        target: u8,
    },
    UnrelatedQuestion,
}

#[derive(Clone, Debug)]
enum Step {
    One(Edit),
    /// Several edits in one command.
    Batch(Vec<Edit>),
    /// An edit or precondition naming a superseded revision: must be rejected.
    StaleEdit {
        target: u8,
    },
}

fn scope_strategy() -> impl Strategy<Value = Scope> {
    prop_oneof![
        Just(Scope::All),
        Just(Scope::Chosen),
        Just(Scope::Other),
        Just(Scope::Both)
    ]
}

fn graph_strategy() -> impl Strategy<Value = Graph> {
    (
        1u8..=3,
        prop::collection::vec(0u8..3, 1..=4),
        (
            any::<bool>(),
            prop_oneof![Just(Scope::All), Just(Scope::Chosen), Just(Scope::Both)],
        ),
        prop::option::of((any::<bool>(), scope_strategy())),
        any::<u8>(),
        prop::collection::vec(any::<u8>(), 1..=3),
        any::<bool>(),
    )
        .prop_map(
            |(sources, observations, first, second, support, evidence, cite_assumption)| Graph {
                sources,
                observations: observations.into_iter().map(|s| s % sources).collect(),
                criteria: std::iter::once(first).chain(second).collect(),
                conclusion_support: support,
                comparison_evidence: evidence,
                cite_assumption,
            },
        )
}

fn criterion_change() -> impl Strategy<Value = CriterionChange> {
    prop_oneof![
        3 => (0u8..3).prop_map(CriterionChange::Threshold),
        1 => Just(CriterionChange::Unit),
        1 => Just(CriterionChange::Weighting),
        2 => scope_strategy().prop_map(CriterionChange::Rescope),
        1 => Just(CriterionChange::Soften),
        2 => Just(CriterionChange::Restate),
    ]
}

fn observation_change() -> impl Strategy<Value = ObservationChange> {
    prop_oneof![
        2 => (0u8..3).prop_map(ObservationChange::Value),
        1 => Just(ObservationChange::Invalid),
        2 => Just(ObservationChange::Restate),
    ]
}

fn edit_strategy() -> impl Strategy<Value = Edit> {
    let n = || 0u8..8;
    prop_oneof![
        4 => (n(), criterion_change())
            .prop_map(|(criterion, change)| Edit::AmendCriterion { criterion, change }),
        3 => (n(), observation_change())
            .prop_map(|(observation, change)| Edit::CorrectObservation { observation, change }),
        1 => n().prop_map(|observation| Edit::RetractObservation { observation }),
        2 => (n(), any::<bool>()).prop_map(|(source, partial)| Edit::WithdrawSource { source, partial }),
        1 => (n(), any::<bool>()).prop_map(|(source, partial)| Edit::WithdrawArtifact { source, partial }),
        2 => (n(), any::<bool>()).prop_map(|(source, concern)| Edit::AssessSource { source, concern }),
        4 => (any::<u8>(), any::<bool>())
            .prop_map(|(target, critical)| Edit::RaiseObjection { target, critical }),
        4 => (n(), any::<bool>())
            .prop_map(|(objection, withdraw)| Edit::CloseObjection { objection, withdraw }),
        2 => (n(), 0u8..4).prop_map(|(assumption, status)| Edit::AssessAssumption { assumption, status }),
        2 => any::<u8>().prop_map(|target| Edit::AddMaterialAssumption { target }),
        3 => (n(), any::<u8>()).prop_map(|(comparison, evidence)| Edit::Relink { comparison, evidence }),
        1 => n().prop_map(|comparison| Edit::RetractComparison { comparison }),
        2 => (n(), 0u8..3).prop_map(|(option, lifecycle)| Edit::DisposeOption { option, lifecycle }),
        3 => any::<u8>().prop_map(|subject| Edit::SubmitReview { subject }),
        2 => any::<u8>().prop_map(|target| Edit::Annotate { target }),
        1 => Just(Edit::UnrelatedQuestion),
    ]
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        8 => edit_strategy().prop_map(Step::One),
        2 => prop::collection::vec(edit_strategy(), 2..=3).prop_map(Step::Batch),
        1 => any::<u8>().prop_map(|target| Step::StaleEdit { target }),
    ]
}

fn history_strategy() -> impl Strategy<Value = (Graph, Vec<Step>)> {
    (
        graph_strategy(),
        prop::collection::vec(step_strategy(), 1..=12),
    )
}

// ---------------------------------------------------------------------------
// Execution through the real store
// ---------------------------------------------------------------------------

fn op(name: &str, payload: Value) -> Value {
    json!({"op": name, "payload": payload})
}

fn exact(reference: RevisionRef) -> Value {
    json!(reference)
}

#[derive(Default)]
struct Pools {
    sources: Vec<Id>,
    artifacts: Vec<Id>,
    digests: Vec<String>,
    observations: Vec<Id>,
    question: Option<Id>,
    hypothesis: Option<Id>,
    conclusion: Option<Id>,
    options: Vec<Id>,
    criteria: Vec<Id>,
    assumptions: Vec<Id>,
    comparisons: Vec<Id>,
    decision: Option<Id>,
    objections: Vec<Id>,
    reviews: Vec<Id>,
}

/// One planned operation of a step.
struct Planned {
    label: String,
    op: Value,
    /// Records this operation revises.
    touches: Vec<Id>,
    /// Records whose current revision this operation cites.
    reads: Vec<Id>,
}

#[derive(Default)]
struct Stats {
    accepted: BTreeMap<String, usize>,
    rejected: BTreeMap<String, usize>,
    stale_rejections: usize,
    verdicts: BTreeMap<&'static str, usize>,
    /// How often each oracle rule fired as a local cause.
    rules: BTreeMap<&'static str, usize>,
    historical_reads: usize,
    snapshots_rebuilt: usize,
}

struct Run {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    case: Id,
    replay: CaseReplay,
    commands: u32,
    pools: Pools,
    labels: BTreeMap<Id, String>,
    log: Vec<String>,
}

impl Run {
    fn state(&self) -> &State {
        &self.replay.state
    }

    fn current(&self, id: Id) -> &Record {
        self.state().current(id).expect("pooled record exists")
    }

    fn cur(&self, id: Id) -> RevisionRef {
        self.current(id).reference()
    }

    fn label(&mut self, id: Id, label: String) {
        self.labels.insert(id, label);
    }

    fn name(&self, id: Id) -> String {
        match self.labels.get(&id) {
            Some(label) => format!("{label} ({id})"),
            None => id.to_string(),
        }
    }

    fn refresh(&mut self) {
        self.replay = self.store.case(self.case).expect("replay the case journal");
    }

    /// Record one command. A gate that asks for preconditions is retried once
    /// with exactly the revisions it names.
    fn submit(&mut self, ops: &[Value], preconditions: &[RevisionRef]) -> Result<Receipt, Failure> {
        let mut command = json!({
            "schema_version":"inv.command.v1","case_id":self.case,"actor":ACTOR,
            "preconditions":preconditions,"operations":ops,
        });
        for attempt in 0..2 {
            self.commands += 1;
            command["command_id"] = json!(format!("step-{}", self.commands));
            match self.store.record(&serde_json::to_vec(&command).unwrap()) {
                Err(failure)
                    if attempt == 0
                        && preconditions.is_empty()
                        && failure
                            .error
                            .message
                            .contains("as a transaction precondition") =>
                {
                    command["preconditions"] = failure.error.expected.clone().unwrap_or_default();
                }
                other => return other,
            }
        }
        unreachable!()
    }

    fn must(&mut self, ops: Vec<Value>) -> Receipt {
        let receipt = self
            .submit(&ops, &[])
            .unwrap_or_else(|failure| panic!("scaffold rejected: {:?}\n{ops:#?}", failure.error));
        self.refresh();
        receipt
    }

    fn validation_review(&self, subject: RevisionRef) -> Result<Value, String> {
        let record = self.state().revision(subject).expect("subject revision");
        let digest = basis_digest(record).map_err(|f| f.error.message)?;
        let mut reads = required_read_set(self.state(), record).map_err(|f| f.error.message)?;
        // A supporting review answers each open objection on its subject by
        // citing it (ADR 0017); the cited revision joins its read set.
        let answered: Vec<RevisionRef> =
            open_objections(self.state(), record.header.case_id, &[subject.id].into())
                .map_err(|f| f.error.message)?
                .into_iter()
                .map(|objection| objection.objection_ref)
                .collect();
        reads.extend(answered.iter().copied());
        Ok(json!({
            "kind":"validation","subject_ref":subject,"basis_digest":digest,"read_set":reads,
            "method":"Inspect the cited measurements and sources.","materials_examined":[],
            "shared_dependencies":[],"objection_refs":answered,"verdict":"supports","caveats":["Generated."],
            "context_isolation":"fresh_context_unblinded","context":{
                "saw_original_conclusion":{"state":"known","value":true},
                "gathered_new_evidence":{"state":"known","value":false},
                "method_or_source_changes":[],"activity":"Read the recorded basis.",
                "dependencies":"shared","dependence_basis":"Same recorded data."}
        }))
    }

    /// Build the generated graph. Every record is created through real
    /// operations; sources and artifacts through ingest of local files.
    fn scaffold(graph: &Graph) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        Workspace::init(&root, false).unwrap();
        let config = root.join("investigation.toml");
        let contents = fs::read_to_string(&config).unwrap();
        fs::write(
            &config,
            contents.replace("import_roots = []", "import_roots = [\".\"]"),
        )
        .unwrap();
        let mut store = Store::open(Workspace::open(&root).unwrap());
        let mut case = None;
        let mut pools = Pools::default();
        let mut labels = BTreeMap::new();
        for index in 0..graph.sources {
            let file = format!("bench-{index}.txt");
            fs::write(root.join(&file), format!("p99 {} ms\n", 18 + index)).unwrap();
            let mut manifest = json!({
                "schema_version":"inv.ingest.v1","command_id":format!("ingest-{index}"),"actor":ACTOR,
                "source":{"title":format!("Benchmark {index}"),"format":"text/plain"},
                "paths":[root.join(&file)],
            });
            match case {
                Some(case) => manifest["case_id"] = json!(case),
                None => manifest["new_case"] = json!({"title":"Which queue carries the jobs?"}),
            }
            let receipt = store
                .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
                .expect("ingest a benchmark");
            if case.is_none() {
                case = receipt
                    .receipt
                    .created
                    .iter()
                    .find(|change| change.id.kind() == Kind::Case)
                    .map(|change| change.id);
            }
            let item = &receipt.items[0];
            pools.sources.push(item.source_id);
            pools.artifacts.push(item.artifact_id);
            pools.digests.push(item.digest.to_string());
            labels.insert(item.source_id, format!("source#{index}"));
            labels.insert(item.artifact_id, format!("artifact#{index}"));
        }
        let case = case.expect("first ingest creates the case");
        let replay = store.case(case).unwrap();
        let mut run = Run {
            _dir: dir,
            root,
            store,
            case,
            replay,
            commands: 0,
            pools,
            labels,
            log: Vec::new(),
        };
        run.build(graph);
        run.log.push(format!(
            "scaffold: {} records at sequence {}",
            run.state().records().count(),
            run.state().sequence()
        ));
        run
    }

    fn build(&mut self, graph: &Graph) {
        let question = self
            .must(vec![op(
                "question.create",
                json!({"importance":"normal","wording":"Which queue should carry the jobs?"}),
            )])
            .created[0]
            .id;
        self.pools.question = Some(question);
        self.label(question, "question".into());
        let q = self.cur(question);
        // Evidence, options, criteria and the shared assumption in one batch.
        let mut ops = Vec::new();
        for (index, source) in graph.observations.iter().enumerate() {
            let source = *source as usize;
            ops.push(op(
                "observation.add",
                self.observation_body(
                    self.cur(self.pools.artifacts[source]),
                    &self.pools.digests[source],
                    &format!("{}", 18 + index),
                    "valid",
                ),
            ));
        }
        for description in ["Managed queue", "Broker"] {
            ops.push(op(
                "option.create",
                json!({"question_ref":q,"description":description,"dependencies":[],"tradeoff_notes":[]}),
            ));
        }
        ops.push(op(
            "assumption.create",
            json!({"affected_refs":[q],"claim":"The benchmark load matches production.",
                "consequence":"Latency results transfer.","materiality":"nonmaterial"}),
        ));
        let created = self.must(ops).created;
        let observations = graph.observations.len();
        for (index, change) in created[..observations].iter().enumerate() {
            self.pools.observations.push(change.id);
            self.label(change.id, format!("observation#{index}"));
        }
        for (index, change) in created[observations..observations + 2].iter().enumerate() {
            self.pools.options.push(change.id);
            self.label(change.id, format!("option#{index}"));
        }
        let assumption = created[observations + 2].id;
        self.pools.assumptions.push(assumption);
        self.label(assumption, "assumption#0".into());
        let ops = self
            .pools
            .options
            .clone()
            .into_iter()
            .map(|option| {
                op(
                    "option.disposition",
                    json!({"option_ref":self.cur(option),"lifecycle":"eligible","reason":"Feasible"}),
                )
            })
            .collect();
        self.must(ops);
        let ops = graph
            .criteria
            .iter()
            .enumerate()
            .map(|(index, (hard, scope))| {
                op(
                    "criterion.create",
                    self.criterion_body(index, *hard, *scope),
                )
            })
            .collect();
        for (index, change) in self.must(ops).created.iter().enumerate() {
            self.pools.criteria.push(change.id);
            self.label(change.id, format!("criterion#{index}"));
        }
        // Hypothesis, conclusion and comparisons.
        let evidence = |run: &Run, mask: u8| -> Vec<RevisionRef> {
            let picked: Vec<_> = run
                .pools
                .observations
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, id)| run.cur(*id))
                .collect();
            if picked.is_empty() {
                vec![run.cur(run.pools.observations[0])]
            } else {
                picked
            }
        };
        let mut ops = vec![
            op(
                "hypothesis.create",
                json!({
                    "question_ref":q,"statement":"The managed queue meets the latency limit.",
                    "applicability":"Projected load","assumption_refs":[],"possible_falsifiers":["p99 above the limit"],"rival_refs":[]
                }),
            ),
            op(
                "conclusion.create",
                json!({
                    "statement":"The managed queue met the latency limit in the benchmark.",
                    "scope":"The recorded benchmarks.","significance":"Informs the queue choice.",
                    "support_refs":evidence(self, graph.conclusion_support),"contradiction_refs":[],"caveats":["Generated."]
                }),
            ),
        ];
        let mut cells = Vec::new();
        for (criterion_index, (_, scope)) in graph.criteria.iter().enumerate() {
            for option_index in 0..2 {
                if covers(*scope, option_index) {
                    cells.push((option_index, criterion_index));
                }
            }
        }
        for (cell, (option_index, criterion_index)) in cells.iter().enumerate() {
            let mask = graph.comparison_evidence[cell % graph.comparison_evidence.len()];
            let assumptions = if graph.cite_assumption {
                vec![self.cur(assumption)]
            } else {
                vec![]
            };
            ops.push(op("comparison.add", json!({
                "option_ref":self.cur(self.pools.options[*option_index]),
                "criterion_ref":self.cur(self.pools.criteria[*criterion_index]),
                "outcome":"meets","method":"Benchmark","value":self.cell_value(&evidence(self, mask)),
                "evidence_refs":evidence(self, mask),"assumption_refs":assumptions,
                "uncertainty":"Few runs.","rationale":"Measured p99 is below the limit."
            })));
        }
        let created = self.must(ops).created;
        let (hypothesis, conclusion) = (created[0].id, created[1].id);
        self.pools.hypothesis = Some(hypothesis);
        self.pools.conclusion = Some(conclusion);
        self.label(hypothesis, "hypothesis".into());
        self.label(conclusion, "conclusion".into());
        for (index, change) in created[2..].iter().enumerate() {
            self.pools.comparisons.push(change.id);
            let (option, criterion) = cells[index];
            self.label(
                change.id,
                format!("comparison#{index}(option#{option},criterion#{criterion})"),
            );
        }
        let support = evidence(self, graph.conclusion_support);
        self.must(vec![op(
            "assessment.add",
            json!({
                "subject_ref":self.cur(hypothesis),"evidence_refs":support,"relation":"supports",
                "rationale":"The retained p99 is below the limit."
            }),
        )]);
        // Validate the conclusion through a validation review.
        self.must(vec![op(
            "conclusion.request_review",
            json!({"conclusion_ref":self.cur(conclusion)}),
        )]);
        let review = self.validation_review(self.cur(conclusion)).unwrap();
        let review = self.must(vec![op("review.submit", review)]).created[0].id;
        self.pools.reviews.push(review);
        self.label(review, "review#0(conclusion)".into());
        self.must(vec![op(
            "conclusion.validate",
            json!({"conclusion_ref":self.cur(conclusion),"review_ref":self.cur(review)}),
        )]);
        // Propose, review and accept a decision for the first option.
        let chosen = self.cur(self.pools.options[0]);
        let comparisons: Vec<_> = cells
            .iter()
            .enumerate()
            .filter(|(_, (option, _))| *option == 0)
            .map(|(index, _)| self.cur(self.pools.comparisons[index]))
            .collect();
        let decision = self.must(vec![op("decision.propose", json!({
            "question_ref":q,"choice":{"kind":"select","option_ref":chosen},
            "compared_alternatives":[chosen],"comparison_refs":comparisons,"hard_conditions":[],
            "evidence_refs":[],"assumption_refs":[],"tradeoffs":[],"consequences":["Adopt the managed queue."],
            "reopen_trigger":"Latency evidence changes.","reversal_conditions":[]
        }))])
        .created[0]
            .id;
        self.pools.decision = Some(decision);
        self.label(decision, "decision".into());
        let review = self.validation_review(self.cur(decision)).unwrap();
        let review = self.must(vec![op("review.submit", review)]).created[0].id;
        self.pools.reviews.push(review);
        self.label(review, "review#1(decision)".into());
        self.must(vec![op(
            "decision.accept",
            json!({"decision_ref":self.cur(decision),"review_ref":self.cur(review)}),
        )]);
    }

    fn observation_body(
        &self,
        artifact: RevisionRef,
        digest: &str,
        value: &str,
        validity: &str,
    ) -> Value {
        json!({
            "value":{"kind":"measurement","value":value,"unit":"ms"},
            "basis":{"kind":"source","artifact_ref":artifact,
                "selector":{"schema_version":"inv.selector.v1","artifact_digest":digest,"selector":{"type":"whole"}},
                "text_quote":null},
            "extraction_method":"p99 over the retained samples","validity":validity,"quality":[],
            "limitations":[],"evidence_kind":"observational","variables_changed":[]
        })
    }

    /// A comparison value equal to its first evidence's measurement, so the
    /// cell counts as verified for decision gates.
    fn cell_value(&self, evidence: &[RevisionRef]) -> Value {
        let value = &self.state().revision(evidence[0]).unwrap().body["value"];
        json!({"decimal":value["value"],"unit":value["unit"]})
    }

    fn scope_json(&self, scope: Scope) -> Value {
        let q = self.cur(self.pools.question.unwrap());
        let option = |index: usize| self.cur(self.pools.options[index]);
        match scope {
            Scope::All => json!({"kind":"all_options","question_ref":q}),
            Scope::Chosen => {
                json!({"kind":"selected_options","question_ref":q,"option_refs":[option(0)]})
            }
            Scope::Other => {
                json!({"kind":"selected_options","question_ref":q,"option_refs":[option(1)]})
            }
            Scope::Both => json!({"kind":"selected_options","question_ref":q,
                "option_refs":[option(0), option(1)]}),
        }
    }

    fn criterion_body(&self, index: usize, hard: bool, scope: Scope) -> Value {
        json!({
            "name":format!("Latency {index}"),"kind":if hard {"hard"} else {"soft"},
            "preferred_direction":"lower",
            "scale":{"anchors":[],"description":"p99 latency","unit":"ms"},
            "scope":self.scope_json(scope),
            "threshold":{"kind":"measured","operator":"at_most","value":{"decimal":"25","unit":"ms"}}
        })
    }

    /// Records an objection can name (review subjects).
    fn review_subjects(&self) -> Vec<Id> {
        let p = &self.pools;
        [p.conclusion, p.decision]
            .into_iter()
            .flatten()
            .chain(p.comparisons.iter().copied())
            .collect()
    }

    fn objection_targets(&self) -> Vec<Id> {
        let p = &self.pools;
        [p.conclusion, p.decision, p.hypothesis]
            .into_iter()
            .flatten()
            .chain(p.comparisons.iter().copied())
            .collect()
    }

    /// Records a material assumption can name.
    fn assumption_targets(&self) -> Vec<Id> {
        let mut targets = self.objection_targets();
        targets.extend(self.pools.criteria.iter().copied());
        targets.extend(self.pools.options.iter().copied());
        targets
    }

    /// Records an annotation can name: anything in the graph.
    fn annotation_targets(&self) -> Vec<Id> {
        let mut targets = self.assumption_targets();
        targets.extend(self.pools.observations.iter().copied());
        targets.extend(self.pools.sources.iter().copied());
        targets
    }

    fn pick(list: &[Id], index: u8) -> Option<Id> {
        (!list.is_empty()).then(|| list[index as usize % list.len()])
    }

    fn plan(&self, edit: &Edit) -> Option<Planned> {
        let body = |id: Id| &self.current(id).body;
        let planned = |label: String, op: Value, touches: Vec<Id>, reads: Vec<Id>| {
            Some(Planned {
                label,
                op,
                touches,
                reads,
            })
        };
        match edit {
            Edit::AmendCriterion { criterion, change } => {
                let id = Self::pick(&self.pools.criteria, *criterion)?;
                let mut replacement = self.criterion_replacement(id);
                match change {
                    CriterionChange::Threshold(step) => {
                        replacement["threshold"]["value"]["decimal"] =
                            json!(format!("{}", 25 + 5 * step));
                    }
                    CriterionChange::Unit => {
                        let unit = if replacement["scale"]["unit"] == "ms" {
                            "s"
                        } else {
                            "ms"
                        };
                        replacement["scale"]["unit"] = json!(unit);
                        replacement["threshold"]["value"]["unit"] = json!(unit);
                    }
                    CriterionChange::Weighting => {
                        replacement["weighting_rationale"] =
                            if replacement["weighting_rationale"].is_null() {
                                json!("Latency dominates.")
                            } else {
                                Value::Null
                            };
                    }
                    CriterionChange::Rescope(scope) => {
                        replacement["scope"] = self.scope_json(*scope);
                    }
                    CriterionChange::Soften => {
                        let kind = if replacement["kind"] == "hard" {
                            "soft"
                        } else {
                            "hard"
                        };
                        replacement["kind"] = json!(kind);
                    }
                    CriterionChange::Restate => {}
                }
                planned(
                    format!("criterion.amend {} ({change:?})", self.name(id)),
                    op(
                        "criterion.amend",
                        json!({
                            "criterion_ref":self.cur(id),"reason":"Revised requirement","replacement":replacement
                        }),
                    ),
                    vec![id],
                    vec![],
                )
            }
            Edit::CorrectObservation {
                observation,
                change,
            } => {
                let id = Self::pick(&self.pools.observations, *observation)?;
                let current = body(id);
                if current["status"] == "retracted" {
                    return None;
                }
                let cited: RevisionRef =
                    serde_json::from_value(current["basis"]["artifact_ref"].clone()).unwrap();
                let artifact = self.cur(cited.id);
                let digest = current["basis"]["selector"]["artifact_digest"]
                    .as_str()
                    .unwrap();
                let value = match change {
                    ObservationChange::Value(step) => format!("{}", 30 + step),
                    _ => current["value"]["value"].as_str().unwrap().to_owned(),
                };
                let validity = match change {
                    ObservationChange::Invalid => "invalid",
                    _ => current["validity"].as_str().unwrap(),
                };
                let mut payload = self.observation_body(artifact, digest, &value, validity);
                payload["observation_ref"] = exact(self.cur(id));
                payload["revision_reason"] = json!("Corrected transcription.");
                planned(
                    format!("observation.correct {} ({change:?})", self.name(id)),
                    op("observation.correct", payload),
                    vec![id],
                    vec![artifact.id],
                )
            }
            Edit::RetractObservation { observation } => {
                let id = Self::pick(&self.pools.observations, *observation)?;
                if body(id)["status"] == "retracted" {
                    return None;
                }
                planned(
                    format!("observation.retract {}", self.name(id)),
                    op(
                        "observation.retract",
                        json!({
                            "observation_ref":self.cur(id),"revision_reason":"Measured on the wrong host."
                        }),
                    ),
                    vec![id],
                    vec![],
                )
            }
            Edit::WithdrawSource { source, partial }
            | Edit::WithdrawArtifact { source, partial } => {
                let (name, key, pool) = match edit {
                    Edit::WithdrawSource { .. } => {
                        ("source.withdraw", "source_ref", &self.pools.sources)
                    }
                    _ => ("artifact.withdraw", "artifact_ref", &self.pools.artifacts),
                };
                let id = Self::pick(pool, *source)?;
                if body(id)["status"] == "withdrawn" {
                    return None;
                }
                let (extent, scope) = if *partial {
                    (
                        "partial",
                        json!({"extent":"partial","statement":"The warm-up table."}),
                    )
                } else {
                    ("entire", json!({"extent":"entire"}))
                };
                planned(
                    format!("{name} {} ({extent})", self.name(id)),
                    op(
                        name,
                        json!({
                            key:self.cur(id),"reason":"retracted_by_origin","scope":scope,
                            "rationale":"The publisher retracted the benchmark.","evidence_refs":[]
                        }),
                    ),
                    vec![id],
                    vec![],
                )
            }
            Edit::AssessSource { source, concern } => {
                let id = Self::pick(&self.pools.sources, *source)?;
                let judgment = if *concern { "outdated" } else { "current" };
                planned(
                    format!("source.assess {} (currency {judgment})", self.name(id)),
                    op(
                        "source.assess",
                        json!({
                            "source_ref":self.cur(id),
                            "currency":{"judgment":judgment,"rationale":"Checked for later editions."}
                        }),
                    ),
                    vec![id],
                    vec![],
                )
            }
            Edit::RaiseObjection { target, critical } => {
                let id = Self::pick(&self.objection_targets(), *target)?;
                let severity = if *critical { "critical" } else { "material" };
                planned(
                    format!("objection.raise {severity} on {}", self.name(id)),
                    op(
                        "objection.raise",
                        json!({
                            "subject_ref":self.cur(id),"statement":"The samples exclude the warm-up.",
                            "scope":"The measurement.","severity":severity,"evidence_refs":{"state":"unknown"}
                        }),
                    ),
                    vec![],
                    vec![id],
                )
            }
            Edit::CloseObjection {
                objection,
                withdraw,
            } => {
                let open: Vec<Id> = self
                    .pools
                    .objections
                    .iter()
                    .copied()
                    .filter(|id| body(*id)["status"] == "open")
                    .collect();
                let id = Self::pick(&open, *objection)?;
                let evidence = self
                    .pools
                    .observations
                    .iter()
                    .copied()
                    .find(|id| body(*id)["status"] != "retracted");
                let (name, evidence) = match evidence {
                    Some(evidence) if !*withdraw => ("objection.resolve", vec![evidence]),
                    _ => ("objection.withdraw", vec![]),
                };
                let refs: Vec<_> = evidence.iter().map(|id| self.cur(*id)).collect();
                planned(
                    format!("{name} {}", self.name(id)),
                    op(
                        name,
                        json!({
                            "objection_ref":self.cur(id),"reason":"The warm-up was excluded by design.",
                            "evidence_refs":refs
                        }),
                    ),
                    vec![id],
                    evidence,
                )
            }
            Edit::AssessAssumption { assumption, status } => {
                let id = Self::pick(&self.pools.assumptions, *assumption)?;
                if body(id)["status"] == "superseded" {
                    return None;
                }
                let status =
                    ["supported", "challenged", "rejected", "superseded"][*status as usize % 4];
                planned(
                    format!("assumption.assess {} ({status})", self.name(id)),
                    op(
                        "assumption.assess",
                        json!({
                            "assumption_ref":self.cur(id),"evidence_refs":[],
                            "rationale":format!("Reassessed as {status}."),"status":status
                        }),
                    ),
                    vec![id],
                    vec![],
                )
            }
            Edit::AddMaterialAssumption { target } => {
                let id = Self::pick(&self.assumption_targets(), *target)?;
                planned(
                    format!("assumption.create material about {}", self.name(id)),
                    op(
                        "assumption.create",
                        json!({
                            "affected_refs":[self.cur(id)],"claim":"Production traffic is as bursty as the benchmark.",
                            "consequence":"The measured tail transfers.","materiality":"material"
                        }),
                    ),
                    vec![],
                    vec![id],
                )
            }
            Edit::Relink {
                comparison,
                evidence,
            } => {
                let id = Self::pick(&self.pools.comparisons, *comparison)?;
                let current = body(id);
                if current["retracted"] == true {
                    return None;
                }
                let active: Vec<Id> = self
                    .pools
                    .observations
                    .iter()
                    .copied()
                    .filter(|id| body(*id)["status"] != "retracted")
                    .collect();
                let mut chosen: Vec<Id> = active
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| evidence & (1 << index) != 0)
                    .map(|(_, id)| *id)
                    .collect();
                if chosen.is_empty() {
                    chosen.extend(active.first());
                }
                if chosen.is_empty() {
                    return None;
                }
                let refs = |value: &Value| -> Vec<Id> {
                    value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|r| serde_json::from_value::<RevisionRef>(r.clone()).unwrap().id)
                        .collect()
                };
                let option = refs(&json!([current["option_ref"]]))[0];
                let criterion = refs(&json!([current["criterion_ref"]]))[0];
                let assumptions = refs(&current["assumption_refs"]);
                let evidence: Vec<_> = chosen.iter().map(|id| self.cur(*id)).collect();
                let replacement = json!({
                    "option_ref":self.cur(option),"criterion_ref":self.cur(criterion),
                    "outcome":current["outcome"],"method":current["method"],"value":self.cell_value(&evidence),
                    "evidence_refs":evidence,
                    "assumption_refs":assumptions.iter().map(|id| self.cur(*id)).collect::<Vec<_>>(),
                    "uncertainty":current["uncertainty"],"rationale":current["rationale"]
                });
                let mut reads = vec![option, criterion];
                reads.extend(chosen);
                reads.extend(assumptions);
                planned(
                    format!(
                        "comparison.revise {} (relink to current revisions)",
                        self.name(id)
                    ),
                    op(
                        "comparison.revise",
                        json!({
                            "comparison_ref":self.cur(id),"reason":"Relink to current evidence","replacement":replacement
                        }),
                    ),
                    vec![id],
                    reads,
                )
            }
            Edit::RetractComparison { comparison } => {
                let id = Self::pick(&self.pools.comparisons, *comparison)?;
                if body(id)["retracted"] == true {
                    return None;
                }
                planned(
                    format!("comparison.retract {}", self.name(id)),
                    op(
                        "comparison.retract",
                        json!({
                            "comparison_ref":self.cur(id),"reason":"The cell used the wrong workload."
                        }),
                    ),
                    vec![id],
                    vec![],
                )
            }
            Edit::DisposeOption { option, lifecycle } => {
                let id = Self::pick(&self.pools.options, *option)?;
                let lifecycle = ["ruled_out", "withdrawn", "eligible"][*lifecycle as usize % 3];
                if body(id)["lifecycle"] == lifecycle {
                    return None;
                }
                let criterion = self.pools.criteria[0];
                let mut payload = json!({
                    "option_ref":self.cur(id),"lifecycle":lifecycle,"reason":"Reconsidered."
                });
                if lifecycle == "ruled_out" {
                    payload["criterion_ref"] = exact(self.cur(criterion));
                }
                planned(
                    format!("option.disposition {} ({lifecycle})", self.name(id)),
                    op("option.disposition", payload),
                    vec![id],
                    vec![criterion],
                )
            }
            Edit::SubmitReview { subject } => {
                let subject = Self::pick(&self.review_subjects(), *subject)?;
                let payload = match self.validation_review(self.cur(subject)) {
                    Ok(payload) => payload,
                    Err(message) => {
                        if std::env::var("INV_FRESHNESS_DEBUG").is_ok() {
                            eprintln!("cannot review {}: {message}", self.name(subject));
                        }
                        return None;
                    }
                };
                let mut reads = vec![subject];
                reads.extend(
                    payload["read_set"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|r| serde_json::from_value::<RevisionRef>(r.clone()).unwrap().id),
                );
                planned(
                    format!("review.submit validation of {}", self.name(subject)),
                    op("review.submit", payload),
                    vec![],
                    reads,
                )
            }
            Edit::Annotate { target } => {
                let id = Self::pick(&self.annotation_targets(), *target)?;
                planned(
                    format!("annotation.add on {}", self.name(id)),
                    op(
                        "annotation.add",
                        json!({
                            "subject_refs":[self.cur(id)],"content":{"kind":"text","text":"Reviewed in standup."}
                        }),
                    ),
                    vec![],
                    vec![id],
                )
            }
            Edit::UnrelatedQuestion => planned(
                "question.create (unrelated)".into(),
                op(
                    "question.create",
                    json!({"importance":"normal","wording":"Which region hosts the queue?"}),
                ),
                vec![],
                vec![],
            ),
        }
    }

    fn criterion_replacement(&self, id: Id) -> Value {
        let body = &self.current(id).body;
        let scope = match &body["option_refs"] {
            Value::Null => json!({"kind":"all_options","question_ref":body["question_ref"]}),
            refs => {
                json!({"kind":"selected_options","question_ref":body["question_ref"],"option_refs":refs})
            }
        };
        json!({
            "name":body["name"],"kind":body["kind"],"preferred_direction":body["preferred_direction"],
            "scale":body["scale"],"scope":scope,"threshold":body["threshold"],
            "weighting_rationale":body["weighting_rationale"]
        })
    }

    /// A command naming a superseded revision of a record.
    fn stale(&self, target: u8) -> (String, Vec<Value>, Vec<RevisionRef>) {
        let p = &self.pools;
        let candidates: Vec<Id> = [p.conclusion, p.decision]
            .into_iter()
            .flatten()
            .chain(p.criteria.iter().copied())
            .chain(p.observations.iter().copied())
            .chain(p.comparisons.iter().copied())
            .chain(p.sources.iter().copied())
            .chain(p.options.iter().copied())
            .filter(|id| self.cur(*id).revision > 1)
            .collect();
        let id = Self::pick(&candidates, target).expect("the validated conclusion has history");
        let stale = RevisionRef { id, revision: 1 };
        let label = format!("stale edit of {} r1", self.name(id));
        match id.kind() {
            Kind::Criterion => (
                label,
                vec![op(
                    "criterion.amend",
                    json!({
                        "criterion_ref":stale,"reason":"Stale edit","replacement":self.criterion_replacement(id)
                    }),
                )],
                vec![],
            ),
            Kind::Observation => (
                label,
                vec![op(
                    "observation.retract",
                    json!({
                        "observation_ref":stale,"revision_reason":"Stale edit."
                    }),
                )],
                vec![],
            ),
            Kind::Comparison => (
                label,
                vec![op(
                    "comparison.retract",
                    json!({"comparison_ref":stale,"reason":"Stale edit."}),
                )],
                vec![],
            ),
            _ => (
                format!("{label} (as a precondition)"),
                vec![op(
                    "question.create",
                    json!({"importance":"normal","wording":"Stale?"}),
                )],
                vec![stale],
            ),
        }
    }

    fn register(&mut self, receipt: &Receipt) {
        for change in &receipt.created {
            let kind = change.id.kind();
            let count = |pool: &Vec<Id>| pool.len();
            let label = match kind {
                Kind::Objection => {
                    self.pools.objections.push(change.id);
                    format!("objection#{}", count(&self.pools.objections) - 1)
                }
                Kind::Review => {
                    self.pools.reviews.push(change.id);
                    format!("review#{}", count(&self.pools.reviews) - 1)
                }
                Kind::Assumption => {
                    self.pools.assumptions.push(change.id);
                    format!("assumption#{}", count(&self.pools.assumptions) - 1)
                }
                other => format!("{} (new)", other.prefix()),
            };
            self.label(change.id, label);
        }
    }
}

fn covers(scope: Scope, option: usize) -> bool {
    match scope {
        Scope::All | Scope::Both => true,
        Scope::Chosen => option == 0,
        Scope::Other => option == 1,
    }
}

// ---------------------------------------------------------------------------
// Comparison of the index with the oracle
// ---------------------------------------------------------------------------

/// What the index under test says about one record revision.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    level: Level,
    needs_review: bool,
    disputed_by: BTreeSet<Id>,
    reasons: Vec<String>,
}

fn seen(result: &RecordFreshness) -> Seen {
    Seen {
        level: match result.freshness {
            Freshness::Current => Level::Current,
            Freshness::NeedsReview => Level::NeedsReview,
            Freshness::Disputed => Level::Disputed,
        },
        needs_review: result.needs_review(),
        disputed_by: result
            .reasons
            .iter()
            .filter(|reason| reason.freshness == Freshness::Disputed)
            .map(|reason| reason.cause.id)
            .collect(),
        reasons: result
            .reasons
            .iter()
            .map(|reason| reason.describe())
            .collect(),
    }
}

fn agrees(seen: &Seen, verdict: &Verdict) -> bool {
    seen.level == verdict.level
        && seen.needs_review == verdict.needs_review
        && seen.disputed_by == verdict.disputed_by
}

/// A deliberately broken incremental index, used to show the harness catches
/// a missed invalidation. It is a test-only mutation of the index's output;
/// production behaviour is untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// Reassess only records revised by the step and keep every other
    /// record's previous result: dependents are never invalidated.
    SkipDependents,
}

struct Divergence {
    reference: RevisionRef,
    historical: bool,
    seen: Seen,
    verdict: Verdict,
}

/// The index's view of current records, after applying the fault.
fn current_view(
    index: &FreshnessIndex,
    fault: Fault,
    previous: &BTreeMap<Id, Seen>,
    state: &State,
) -> BTreeMap<Id, Seen> {
    index
        .records()
        .map(|result| {
            let id = result.record.id;
            let revised = state
                .current(id)
                .is_some_and(|record| record.header.acceptance_sequence == state.sequence());
            let view = match (fault, previous.get(&id)) {
                (Fault::SkipDependents, Some(old)) if !revised => old.clone(),
                _ => seen(result),
            };
            (id, view)
        })
        .collect()
}

/// Compare every current record, every historical revision and the
/// Overview. Returns the divergences, current records first.
fn compare(
    state: &State,
    case: Id,
    index: &FreshnessIndex,
    view: &BTreeMap<Id, Seen>,
    stats: &mut Stats,
) -> (Vec<Divergence>, Vec<String>) {
    let oracle = Oracle::new(state, case);
    let mut divergences = Vec::new();
    let records: Vec<&Record> = state
        .records()
        .filter(|record| record.header.case_id == case)
        .collect();
    assert_eq!(
        view.keys().copied().collect::<BTreeSet<_>>(),
        records.iter().map(|r| r.header.id).collect::<BTreeSet<_>>(),
        "the index covers exactly the case's current records"
    );
    for record in &records {
        let verdict = oracle.verdict(record.reference());
        for (rule, _) in &verdict.why {
            *stats.rules.entry(rule).or_default() += 1;
        }
        let seen = &view[&record.header.id];
        *stats
            .verdicts
            .entry(match verdict.level {
                Level::Current => "current",
                Level::NeedsReview => "needs_review",
                Level::Disputed => "disputed",
            })
            .or_default() += 1;
        if !agrees(seen, &verdict) {
            divergences.push(Divergence {
                reference: record.reference(),
                historical: false,
                seen: seen.clone(),
                verdict,
            });
        }
    }
    for record in &records {
        for historic in state.history(record.header.id) {
            if historic.header.revision == record.header.revision {
                continue;
            }
            stats.historical_reads += 1;
            let verdict = oracle.verdict(historic.reference());
            let seen = seen(&index.assess(state, historic));
            if !agrees(&seen, &verdict) {
                divergences.push(Divergence {
                    reference: historic.reference(),
                    historical: true,
                    seen,
                    verdict,
                });
            }
        }
    }
    let mut overview = Vec::new();
    let summary = index.overview();
    let in_use: Vec<&&Record> = records.iter().filter(|r| oracle.in_use(r)).collect();
    let count = |level: Level| {
        in_use
            .iter()
            .filter(|r| oracle.verdict(r.reference()).level == level)
            .count()
    };
    let expected = (
        count(Level::Current),
        count(Level::NeedsReview),
        count(Level::Disputed),
    );
    if fault_free(view, index)
        && (summary.current, summary.needs_review, summary.disputed) != expected
    {
        overview.push(format!(
            "overview: index (current, needs_review, disputed) = {:?}, oracle = {expected:?}",
            (summary.current, summary.needs_review, summary.disputed)
        ));
    }
    (divergences, overview)
}

/// The Overview is read from the index itself; compare it only when the view
/// is the index's own.
fn fault_free(view: &BTreeMap<Id, Seen>, index: &FreshnessIndex) -> bool {
    index
        .records()
        .all(|r| view.get(&r.record.id) == Some(&seen(r)))
}

fn describe_divergence(run: &Run, divergence: &Divergence) -> String {
    let record = run.state().revision(divergence.reference).unwrap();
    let mut text = format!(
        "{} r{}{}\n    index:  {:?} needs_review={} disputed_by={:?}\n",
        run.name(divergence.reference.id),
        divergence.reference.revision,
        if divergence.historical {
            " (historical read)"
        } else {
            ""
        },
        divergence.seen.level,
        divergence.seen.needs_review,
        divergence.seen.disputed_by,
    );
    for reason in &divergence.seen.reasons {
        let _ = writeln!(text, "      - {reason}");
    }
    let _ = writeln!(
        text,
        "    oracle: {:?} needs_review={} disputed_by={:?}",
        divergence.verdict.level, divergence.verdict.needs_review, divergence.verdict.disputed_by
    );
    for (rule, detail) in &divergence.verdict.why {
        let _ = writeln!(text, "      - {rule}: {detail}");
    }
    if record.header.id.kind() == Kind::Review {
        let _ = writeln!(
            text,
            "    review of {} reading {} records",
            record.body["subject_ref"]["id"],
            record.body["read_set"].as_array().map_or(0, Vec::len)
        );
    }
    text
}

// ---------------------------------------------------------------------------
// One generated case
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Settings {
    fault: Fault,
    /// Rebuild every snapshot from the journal and the projection at the end.
    replay_checks: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fault: Fault::None,
            replay_checks: true,
        }
    }
}

fn check_history(
    seed: u64,
    graph: &Graph,
    steps: &[Step],
    settings: Settings,
    stats: &mut Stats,
) -> Result<(), String> {
    let mut run = Run::scaffold(graph);
    let case = run.case;
    let mut index = FreshnessIndex::build(run.state(), case);
    let mut view = current_view(&index, Fault::None, &BTreeMap::new(), run.state());
    let mut snapshots: Vec<(Snapshot, FreshnessIndex)> =
        vec![(run.replay.snapshot.clone(), index.clone())];
    let failure = |run: &Run, step: usize, divergences: &[Divergence], overview: &[String]| {
        let mut report = format!(
            "freshness index and independent recomputation disagree\nseed: {seed:#x} (rerun with INV_FRESHNESS_SEED={seed})\ngraph: {graph:?}\nsteps: {steps:#?}\n"
        );
        if settings.fault != Fault::None {
            let _ = writeln!(
                report,
                "index under test: injected fault {:?}",
                settings.fault
            );
        }
        report.push_str("operations:\n");
        for line in &run.log {
            let _ = writeln!(report, "  {line}");
        }
        if let Some(first) = divergences
            .iter()
            .find(|d| d.reference.id.kind() == Kind::Review)
            .or(divergences.first())
        {
            let _ = write!(
                report,
                "first divergent review after step #{step} (sequence {}):\n  {}",
                run.state().sequence(),
                describe_divergence(run, first)
            );
        }
        let _ = writeln!(
            report,
            "all divergences at this step ({}):",
            divergences.len()
        );
        for divergence in divergences {
            let _ = write!(report, "  {}", describe_divergence(run, divergence));
        }
        for line in overview {
            let _ = writeln!(report, "  {line}");
        }
        report
    };
    let (divergences, overview) = compare(run.state(), case, &index, &view, stats);
    if !divergences.is_empty() || !overview.is_empty() {
        return Err(failure(&run, 0, &divergences, &overview));
    }
    for (number, step) in steps.iter().enumerate() {
        let number = number + 1;
        let before = run.replay.clone();
        let (label, ops, preconditions, stale) = match step {
            Step::One(edit) => match run.plan(edit) {
                Some(planned) => (planned.label, vec![planned.op], vec![], false),
                None => {
                    run.log
                        .push(format!("#{number} {edit:?}: skipped (not applicable)"));
                    continue;
                }
            },
            Step::Batch(edits) => {
                let mut touched = BTreeSet::new();
                let mut read = BTreeSet::new();
                let mut labels = Vec::new();
                let mut ops = Vec::new();
                for planned in edits.iter().filter_map(|edit| run.plan(edit)) {
                    let clash = planned
                        .touches
                        .iter()
                        .any(|id| touched.contains(id) || read.contains(id))
                        || planned.reads.iter().any(|id| touched.contains(id));
                    if clash {
                        continue;
                    }
                    touched.extend(planned.touches);
                    read.extend(planned.reads);
                    labels.push(planned.label);
                    ops.push(planned.op);
                }
                if ops.is_empty() {
                    run.log
                        .push(format!("#{number} batch: skipped (not applicable)"));
                    continue;
                }
                (format!("batch [{}]", labels.join("; ")), ops, vec![], false)
            }
            Step::StaleEdit { target } => {
                let (label, ops, preconditions) = run.stale(*target);
                (label, ops, preconditions, true)
            }
        };
        match run.submit(&ops, &preconditions) {
            Ok(receipt) => {
                if stale {
                    return Err(format!("#{number} {label}: a stale edit was accepted"));
                }
                run.refresh();
                run.register(&receipt);
                for op in &ops {
                    *stats
                        .accepted
                        .entry(op["op"].as_str().unwrap().to_owned())
                        .or_default() += 1;
                }
                run.log.push(format!(
                    "#{number} {label}: accepted at sequence {}",
                    run.state().sequence()
                ));
            }
            Err(failure) => {
                if stale {
                    if failure.error.code != ErrorCode::RevisionConflict {
                        return Err(format!(
                            "#{number} {label}: rejected with {:?}, expected a revision conflict",
                            failure.error
                        ));
                    }
                    stats.stale_rejections += 1;
                } else {
                    if std::env::var("INV_FRESHNESS_DEBUG").is_ok() {
                        eprintln!("rejected {label}: {:?}", failure.error.message);
                    }
                    for op in &ops {
                        *stats
                            .rejected
                            .entry(op["op"].as_str().unwrap().to_owned())
                            .or_default() += 1;
                    }
                }
                run.log.push(format!(
                    "#{number} {label}: rejected ({:?}: {})",
                    failure.error.code, failure.error.message
                ));
                // A rejected command changes nothing: journal, state, index.
                run.refresh();
                if run.replay != before {
                    return Err(format!(
                        "#{number} {label}: a rejected command changed the journal"
                    ));
                }
                let unchanged = index.clone();
                index
                    .update(run.state())
                    .map_err(|f| format!("index update failed: {:?}", f.error))?;
                if index != unchanged {
                    return Err(format!(
                        "#{number} {label}: a rejected command changed the index"
                    ));
                }
                continue;
            }
        }
        index
            .update(run.state())
            .map_err(|f| format!("index update failed: {:?}", f.error))?;
        view = current_view(&index, settings.fault, &view, run.state());
        snapshots.push((run.replay.snapshot.clone(), index.clone()));
        let (divergences, overview) = compare(run.state(), case, &index, &view, stats);
        if !divergences.is_empty() || !overview.is_empty() {
            return Err(failure(&run, number, &divergences, &overview));
        }
    }
    if settings.replay_checks {
        replay_checks(&mut run, &snapshots, stats)?;
    }
    Ok(())
}

/// Rebuild every snapshot from the journal: the rebuilt index must equal the
/// incrementally updated one, and current and historical reads must still
/// agree with the oracle. Then rebuild the store's dependency projection and
/// compare it with the incrementally maintained one and with the oracle's
/// citation table.
fn replay_checks(
    run: &mut Run,
    snapshots: &[(Snapshot, FreshnessIndex)],
    stats: &mut Stats,
) -> Result<(), String> {
    let case = run.case;
    let replay = run.store.case(case).map_err(|f| format!("{:?}", f.error))?;
    for (snapshot, live) in snapshots {
        let pinned = replay
            .at_snapshot(snapshot)
            .map_err(|f| format!("{:?}", f.error))?;
        let rebuilt = FreshnessIndex::build(&pinned.state, case);
        if &rebuilt != live {
            return Err(format!(
                "index rebuilt from the journal at sequence {} differs from the incremental one",
                snapshot.sequence
            ));
        }
        let view = current_view(&rebuilt, Fault::None, &BTreeMap::new(), &pinned.state);
        let (divergences, overview) = compare(&pinned.state, case, &rebuilt, &view, stats);
        if let Some(first) = divergences.first() {
            return Err(format!(
                "after replay to sequence {}: {}",
                snapshot.sequence,
                describe_divergence(run, first)
            ));
        }
        if let Some(line) = overview.first() {
            return Err(format!(
                "after replay to sequence {}: {line}",
                snapshot.sequence
            ));
        }
        stats.snapshots_rebuilt += 1;
    }
    let path = run.root.join("cache/index.sqlite");
    let workspace = run.store.workspace().id();
    let rows = |run: &Run| -> Result<BTreeSet<(RevisionRef, RevisionRef)>, String> {
        let projection =
            Projection::open(&path, workspace).map_err(|f| format!("{:?}", f.error))?;
        let mut rows = BTreeSet::new();
        for record in run.state().records().filter(|r| r.header.case_id == case) {
            for row in projection
                .dependents(case, record.header.id)
                .map_err(|f| format!("{:?}", f.error))?
            {
                rows.insert((row.dependent, row.target));
            }
        }
        Ok(rows)
    };
    let maintained = rows(run)?;
    run.store
        .repair_index()
        .map_err(|f| format!("{:?}", f.error))?;
    let rebuilt = rows(run)?;
    if maintained != rebuilt {
        return Err("the rebuilt dependency projection differs from the maintained one".into());
    }
    let oracle = Oracle::new(run.state(), case);
    let mut cited = BTreeSet::new();
    for record in run.state().records().filter(|r| r.header.case_id == case) {
        for citation in oracle.citations(record) {
            if citation.role != Role::Ignored {
                cited.insert((record.reference(), citation.target));
            }
        }
    }
    let current: BTreeSet<_> = rebuilt
        .into_iter()
        .filter(|(dependent, _)| run.cur(dependent.id) == *dependent)
        .collect();
    if current != cited {
        let missing: Vec<_> = cited.difference(&current).collect();
        let extra: Vec<_> = current.difference(&cited).collect();
        return Err(format!(
            "projection dependencies differ from the oracle's citations\n  missing: {missing:?}\n  extra: {extra:?}"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Property runs
// ---------------------------------------------------------------------------

fn seeds() -> Vec<u64> {
    match std::env::var("INV_FRESHNESS_SEED") {
        Ok(seed) => vec![seed.trim().parse().expect("INV_FRESHNESS_SEED is a u64")],
        Err(_) => DEFAULT_SEEDS.to_vec(),
    }
}

fn cases() -> u32 {
    std::env::var("INV_FRESHNESS_CASES")
        .ok()
        .map(|cases| cases.trim().parse().expect("INV_FRESHNESS_CASES is a u32"))
        .unwrap_or(DEFAULT_CASES)
}

fn runner(seed: u64, cases: u32) -> TestRunner {
    let mut bytes = [0u8; 32];
    for chunk in bytes.chunks_mut(8) {
        chunk.copy_from_slice(&seed.to_le_bytes());
    }
    TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            max_shrink_iters: 512,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &bytes),
    )
}

/// Proptest's minimal failing case and its report.
struct Shrunk {
    report: String,
    graph: Graph,
    steps: Vec<Step>,
}

/// Run one seed; on failure return proptest's minimal failing case.
fn explore(
    seed: u64,
    cases: u32,
    settings: Settings,
    stats: &RefCell<Stats>,
) -> Result<(), Box<Shrunk>> {
    runner(seed, cases)
        .run(&history_strategy(), |(graph, steps)| {
            check_history(seed, &graph, &steps, settings, &mut stats.borrow_mut())
                .map_err(TestCaseError::fail)
        })
        .map_err(|error| match error {
            TestError::Fail(reason, (graph, steps)) => Box::new(Shrunk {
                report: reason.to_string(),
                graph,
                steps,
            }),
            TestError::Abort(reason) => panic!("aborted: {reason}"),
        })
}

/// Fixed histories for rules that random histories reach only rarely.
fn curated() -> Vec<Vec<Step>> {
    vec![
        // A review that saw an open objection, which is then resolved.
        vec![
            Step::One(Edit::RaiseObjection {
                target: 0,
                critical: false,
            }),
            Step::One(Edit::SubmitReview { subject: 0 }),
            Step::One(Edit::CloseObjection {
                objection: 0,
                withdraw: false,
            }),
        ],
    ]
}

#[test]
fn incremental_freshness_matches_an_independent_recomputation_over_generated_histories() {
    let stats = RefCell::new(Stats::default());
    for steps in curated() {
        check_history(
            0,
            &small_graph(),
            &steps,
            Settings::default(),
            &mut stats.borrow_mut(),
        )
        .unwrap_or_else(|report| panic!("{report}"));
    }
    for seed in seeds() {
        if let Err(shrunk) = explore(seed, cases(), Settings::default(), &stats) {
            panic!("{}", shrunk.report);
        }
    }
    let stats = stats.into_inner();
    eprintln!(
        "accepted: {:?}\nrejected: {:?}\nstale rejections: {}\nverdicts: {:?}\nrules: {:?}\nhistorical reads: {}\nsnapshots rebuilt: {}",
        stats.accepted,
        stats.rejected,
        stats.stale_rejections,
        stats.verdicts,
        stats.rules,
        stats.historical_reads,
        stats.snapshots_rebuilt
    );
    // Not vacuous: every kind of edit was accepted, stale edits were
    // rejected, and the runs saw affected as well as unaffected records.
    for name in [
        "criterion.amend",
        "observation.correct",
        "observation.retract",
        "source.withdraw",
        "artifact.withdraw",
        "source.assess",
        "objection.raise",
        "objection.resolve",
        "objection.withdraw",
        "assumption.assess",
        "assumption.create",
        "comparison.revise",
        "comparison.retract",
        "option.disposition",
        "review.submit",
        "annotation.add",
        "question.create",
    ] {
        assert!(
            stats.accepted.get(name).copied().unwrap_or(0) > 0,
            "no accepted {name} in the generated histories: {:?}",
            stats.accepted
        );
    }
    assert!(stats.stale_rejections > 0);
    for rule in [
        "acknowledged",
        "assumption_disposition",
        "basis_expanded",
        "critical_objection",
        "eligible_options_changed",
        "ended",
        "hard_criteria_changed",
        "inherited",
        "invalid_measurement",
        "material_assumptions_changed",
        "objection_closed_since_review",
        "objection_raised_after_review",
        "source_concern",
        "substantive_change",
        "use_change_after_reading",
        "withdrawal",
    ] {
        assert!(
            stats.rules.get(rule).copied().unwrap_or(0) > 0,
            "oracle rule {rule} never fired: {:?}",
            stats.rules
        );
    }
    for level in ["current", "needs_review", "disputed"] {
        assert!(
            stats.verdicts.get(level).copied().unwrap_or(0) > 0,
            "{level}"
        );
    }
    assert!(stats.historical_reads > 0 && stats.snapshots_rebuilt > 0);
}

/// The harness is not vacuous: an incremental index that never invalidates
/// dependents (a test-only mutation of the index's output) is caught, the
/// history is shrunk to one step, and the report names the seed, the
/// operations and the first divergent review.
#[test]
fn a_deliberately_missed_invalidation_is_caught_shrunk_and_reported() {
    let settings = Settings {
        fault: Fault::SkipDependents,
        replay_checks: false,
    };
    let seed = DEFAULT_SEEDS[0];
    let stats = RefCell::new(Stats::default());
    let Shrunk {
        report,
        graph,
        steps,
    } = *explore(seed, 32, settings, &stats)
        .expect_err("a missed invalidation must not go unnoticed");
    eprintln!("{report}");
    assert_eq!(steps.len(), 1, "shrunk to a single step: {steps:#?}");
    assert!(!matches!(steps[0], Step::Batch(_)), "{steps:#?}");
    for expected in [
        format!("seed: {seed:#x} (rerun with INV_FRESHNESS_SEED={seed})"),
        "operations:\n  scaffold:".to_owned(),
        "#1 ".to_owned(),
        "first divergent review after step #1".to_owned(),
        "    index:  Current".to_owned(),
        "    oracle: NeedsReview".to_owned(),
    ] {
        assert!(
            report.contains(&expected),
            "missing {expected:?} in\n{report}"
        );
    }
    // Without the fault the same minimal history agrees.
    check_history(
        seed,
        &graph,
        &steps,
        Settings::default(),
        &mut Stats::default(),
    )
    .unwrap();
}

fn small_graph() -> Graph {
    Graph {
        sources: 1,
        observations: vec![0],
        criteria: vec![(true, Scope::All)],
        conclusion_support: 1,
        comparison_evidence: vec![1],
        cite_assumption: false,
    }
}

/// Run a fixed history through the harness (the index and the oracle must
/// agree after every step), then replay it and return the run, a full index
/// of its final state and the harness counts, so a test can read the index.
fn pinned(graph: &Graph, steps: &[Step]) -> (Run, FreshnessIndex, Stats) {
    let mut stats = Stats::default();
    let settings = Settings {
        replay_checks: false,
        ..Settings::default()
    };
    check_history(1, graph, steps, settings, &mut stats)
        .unwrap_or_else(|report| panic!("{report}"));
    let mut run = Run::scaffold(graph);
    for step in steps {
        let Step::One(edit) = step else {
            unreachable!("pinned histories are single edits")
        };
        let planned = run.plan(edit).expect("applicable");
        let receipt = run.submit(&[planned.op], &[]).expect("accepted");
        run.refresh();
        run.register(&receipt);
    }
    let index = FreshnessIndex::build(run.state(), run.case);
    (run, index, stats)
}

impl Run {
    fn by_label(&self, label: &str) -> Id {
        *self
            .labels
            .iter()
            .find(|(_, name)| name.as_str() == label)
            .unwrap_or_else(|| panic!("{label} in {:?}", self.labels))
            .0
    }
}

// Targets in `small_graph`: review_subjects() is [conclusion, decision,
// comparison#0, comparison#1]; objection_targets() is [conclusion, decision,
// hypothesis, comparison#0, comparison#1]; assumption_targets() appends
// [criterion#0, option#0, option#1].
const COMPARISON_0: u8 = 3;

/// Rules version 6 (W10.05e): an objection resolved on the strength of
/// evidence rests on that evidence. Correcting it makes the resolved objection
/// need review (§14.3: a resolution cites its evidence), with the
/// `resolution_evidence_refs` edge in the reason chain.
#[test]
fn a_corrected_resolution_evidence_makes_the_objection_need_review() {
    let steps = [
        Step::One(Edit::RaiseObjection {
            target: COMPARISON_0,
            critical: false,
        }),
        Step::One(Edit::CloseObjection {
            objection: 0,
            withdraw: false,
        }),
        Step::One(Edit::CorrectObservation {
            observation: 0,
            change: ObservationChange::Value(1),
        }),
    ];
    let (run, index, _) = pinned(&small_graph(), &steps);
    let objection = run.by_label("objection#0");
    assert_eq!(run.current(objection).body["status"], "resolved");
    let freshness = index.get(objection).unwrap();
    assert_eq!(freshness.freshness, Freshness::NeedsReview);
    assert!(
        freshness
            .reasons
            .iter()
            .any(|reason| reason.trigger == Trigger::MaterialRevision
                && reason.path[0].field == "resolution_evidence_refs"),
        "{:?}",
        freshness.reasons
    );
}

/// Rules version 6 (W10.05e): an option's disposition rests on the
/// criterion it names. When that criterion changes, the ruled-out option
/// needs review, and so does a comparison of it against another criterion,
/// which relies on the option. The promotion closure walks the same edge, so
/// the gates refuse that comparison's closure until the option is disposed of
/// against the current criterion, which makes both current again.
#[test]
fn a_ruled_out_option_rests_on_its_criterion() {
    let steps = [
        Step::One(Edit::DisposeOption {
            option: 1,
            lifecycle: 0,
        }),
        Step::One(Edit::AmendCriterion {
            criterion: 0,
            change: CriterionChange::Threshold(1),
        }),
    ];
    // A second, soft criterion that covers only option#1.
    let graph = Graph {
        criteria: vec![(true, Scope::All), (false, Scope::Other)],
        ..small_graph()
    };
    let (mut run, index, _) = pinned(&graph, &steps);
    let option = run.by_label("option#1");
    let cell = run.by_label("comparison#2(option#1,criterion#1)");
    assert_eq!(run.current(option).body["lifecycle"], "ruled_out");
    let reasons = &index.get(option).unwrap().reasons;
    assert!(
        reasons
            .iter()
            .any(|reason| reason.trigger == Trigger::MaterialRevision
                && reason.path[0].field == "criterion_ref"),
        "{reasons:?}"
    );
    let cell_freshness = index.get(cell).unwrap();
    assert_eq!(cell_freshness.freshness, Freshness::NeedsReview);
    assert_eq!(cell_freshness.reasons[0].path[0].field, "option_ref");
    assert!(required_read_set(run.state(), run.current(cell)).is_err());

    // Rule the option out again against the current criterion.
    let criterion = run.by_label("criterion#0");
    let receipt = run
        .submit(
            &[op(
                "option.disposition",
                json!({"option_ref":run.cur(option),"lifecycle":"ruled_out",
                    "reason":"Still too slow.","criterion_ref":exact(run.cur(criterion))}),
            )],
            &[],
        )
        .expect("accepted");
    run.refresh();
    run.register(&receipt);
    let index = FreshnessIndex::build(run.state(), run.case);
    assert_eq!(index.get(option).unwrap().freshness, Freshness::Current);
    assert_eq!(index.get(cell).unwrap().freshness, Freshness::Current);
    let reads = required_read_set(run.state(), run.current(cell)).unwrap();
    assert!(reads.contains(&run.cur(criterion)), "{reads:?}");
}

/// Fixed index gap found by this harness (W10.05d): after a benign revision
/// of a record in a decision's closure (a restated criterion), a material
/// assumption about a compared cell is a new required read the decision
/// review never saw. The promotion closure walk used to refuse any
/// non-current Criterion revision, and the index skipped the check whenever
/// the walk failed, so the review and the accepted decision stayed current.
/// The walk now reads through every benign successor, so the index, the
/// promotion gates and the `review.stale_read_set` gap rule agree.
#[test]
fn basis_expansion_is_caught_behind_a_benign_revision() {
    let steps = [
        Step::One(Edit::AmendCriterion {
            criterion: 0,
            change: CriterionChange::Restate,
        }),
        Step::One(Edit::AddMaterialAssumption {
            target: COMPARISON_0,
        }),
    ];
    // The index agrees with the oracle's rule 4 on this history, and that
    // rule fires.
    let (run, index, stats) = pinned(&small_graph(), &steps);
    assert!(
        stats.rules.contains_key("basis_expanded"),
        "{:?}",
        stats.rules
    );
    let (criterion, review, decision) = (
        run.by_label("criterion#0"),
        run.by_label("review#1(decision)"),
        run.by_label("decision"),
    );
    assert_eq!(run.current(criterion).header.revision.0, 2, "restated");
    let review_freshness = index.get(review).unwrap();
    assert_eq!(review_freshness.freshness, Freshness::NeedsReview);
    let expanded: Vec<_> = review_freshness
        .reasons
        .iter()
        .filter(|reason| reason.trigger == Trigger::BasisExpanded)
        .map(|reason| reason.cause)
        .collect();
    assert_eq!(expanded.len(), 1, "{:?}", review_freshness.reasons);
    assert_eq!(expanded[0].id.kind(), Kind::Assumption);
    assert_eq!(
        index.get(decision).unwrap().freshness,
        Freshness::NeedsReview
    );

    // The promotion closure reads the restated criterion's current revision
    // instead of refusing the walk, and now includes the new assumption.
    let reads = required_read_set(run.state(), run.current(decision)).unwrap();
    assert!(reads.contains(&run.cur(criterion)), "{reads:?}");
    assert!(reads.contains(&expanded[0]), "{reads:?}");
}

// Material-assumption targets in `small_graph`: criterion#0 follows the
// five objection targets.
const CRITERION_0: u8 = 5;

/// The same rule-4 check where a promotion would refuse the closure walk (a
/// generated history found it with more cases): a material assumption about
/// a criterion in the closures is added after the reviews and then rejected.
/// Neither review read it and no review edge reports it, so the check must
/// not lapse just because the gate refuses a rejected dependency.
#[test]
fn basis_expansion_is_caught_behind_a_rejected_assumption() {
    let steps = [
        Step::One(Edit::AddMaterialAssumption {
            target: CRITERION_0,
        }),
        Step::One(Edit::AssessAssumption {
            assumption: 1,
            status: 2,
        }),
    ];
    let mut stats = Stats::default();
    let settings = Settings {
        replay_checks: false,
        ..Settings::default()
    };
    check_history(1, &small_graph(), &steps, settings, &mut stats).unwrap();
    assert!(
        stats.rules.contains_key("basis_expanded"),
        "{:?}",
        stats.rules
    );
    assert_eq!(stats.accepted.get("assumption.assess"), Some(&1));
}

/// Rules version 6 (W10.05e): a validation review's membership checks hold
/// after its subject changes in substance. An objection raised on the
/// reviewed revision after the review disputes the review; the relinked
/// subject also makes it need review. A later edit never hides the dispute.
#[test]
fn a_changed_subject_keeps_objections_raised_after_its_review() {
    let steps = [
        Step::One(Edit::SubmitReview { subject: 2 }),
        Step::One(Edit::RaiseObjection {
            target: COMPARISON_0,
            critical: false,
        }),
        // Relink the cell to the second observation.
        Step::One(Edit::Relink {
            comparison: 0,
            evidence: 2,
        }),
    ];
    let graph = Graph {
        observations: vec![0, 0],
        ..small_graph()
    };
    let (run, index, stats) = pinned(&graph, &steps);
    assert!(
        stats.rules.contains_key("objection_raised_after_review"),
        "{:?}",
        stats.rules
    );
    let (review, objection) = (run.by_label("review#2"), run.by_label("objection#0"));
    let freshness = index.get(review).unwrap();
    assert_eq!(freshness.freshness, Freshness::Disputed);
    assert!(freshness.needs_review(), "{:?}", freshness.reasons);
    let triggers: Vec<_> = freshness
        .reasons
        .iter()
        .map(|reason| (&reason.trigger, reason.cause.id))
        .collect();
    assert!(
        triggers.contains(&(&Trigger::ObjectionRaisedAfterReview, objection)),
        "{triggers:?}"
    );
    assert!(
        freshness
            .reasons
            .iter()
            .any(|reason| reason.trigger == Trigger::MaterialRevision
                && reason.path[0].field == "subject_ref"),
        "{:?}",
        freshness.reasons
    );
}

/// A stale edit is rejected as a revision conflict and leaves the journal,
/// the replayed state and the index unchanged (checked inside the harness).
#[test]
fn a_rejected_stale_edit_changes_nothing() {
    let mut stats = Stats::default();
    // Stale candidates with history: [conclusion, decision, criterion#0, …].
    let steps = [
        Step::One(Edit::AmendCriterion {
            criterion: 0,
            change: CriterionChange::Threshold(2),
        }),
        Step::StaleEdit { target: 2 },
        Step::StaleEdit { target: 0 },
    ];
    check_history(1, &small_graph(), &steps, Settings::default(), &mut stats).unwrap();
    assert_eq!(stats.stale_rejections, 2);
    assert!(stats.snapshots_rebuilt > 0);
}
