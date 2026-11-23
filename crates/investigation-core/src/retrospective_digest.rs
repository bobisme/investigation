//! The process digest and the expected-versus-actual comparison that a
//! Retrospective rests on (§8.6). Both are computed from the replayed journal at
//! one verified snapshot, and neither writes anything. The closing agent may not
//! remember the process, so it annotates these recorded facts instead of
//! recalling them.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    PROGRAM_NAME,
    classification::{AudiencePolicy, Classifier, Field},
    command::RevisionRef,
    freshness::{Freshness, FreshnessIndex},
    header::UtcTimestamp,
    identity::{Id, Kind},
    state::{Record, State},
    wire::{ErrorCode, Failure, Snapshot},
};

pub const DIGEST_VERSION: &str = "inv.retrospective-digest.v1";

pub const COVERAGE: &str = "This digest covers recorded work only. Effort, dead ends, rejected writes and decisions that were never recorded in this case are absent, so it is not a complete account of the process.";

pub const CITATION_RULE: &str = "Cite each effective_steps, invalid_or_misleading_steps and lessons entry in support_refs: use the cite ID of a digest item or of an expected-versus-actual entry, or another same-case record ID. Give each lesson its applicability and transfer limits.";

/// What the journal cannot show, stated so that no reader takes silence for absence.
pub const NOT_IN_JOURNAL: &[&str] = &[
    "Rejected writes, including revision conflicts, commit nothing and are not journaled.",
    "Work done outside Investigation appears only where a Run, Observation or note records it.",
    "Free text of records withheld from the internal audience is omitted.",
];

/// One recorded process fact. `id` is stable for the record and category, so a
/// later digest of the same case keeps it; `record_ref` pins the revision seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DigestItem {
    pub id: String,
    pub category: &'static str,
    pub record_ref: RevisionRef,
    /// The record ID to put in `support_refs` when a step or lesson rests on this item.
    pub cite: Id,
    pub fact: &'static str,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub detail: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SequenceSpan {
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub transactions: usize,
    pub first_recorded_at: UtcTimestamp,
    pub last_recorded_at: UtcTimestamp,
    pub actors: Vec<String>,
    /// Current records by ID prefix.
    pub records_by_type: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedState {
    Recorded,
    NoExpectationRecorded,
    Withheld,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActualState {
    Recorded,
    NoOutcomeRecorded,
}

/// A recorded expectation beside the recorded outcome. The digest does not
/// judge the difference; the Retrospective reports it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExpectationComparison {
    pub id: String,
    pub kind: &'static str,
    pub cite: Id,
    /// The revision that holds the expectation, when one is recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expectation_ref: Option<RevisionRef>,
    pub expected_state: ExpectedState,
    pub expected: Value,
    /// True when the expectation was written after a Run or Observation was
    /// already recorded, so hindsight may have shaped it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_after_work_began: Option<bool>,
    pub actual_state: ActualState,
    pub actual: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProcessDigest {
    pub schema_version: &'static str,
    pub case_id: Id,
    pub snapshot: Snapshot,
    pub coverage: &'static str,
    pub not_in_journal: &'static [&'static str],
    pub items: Vec<DigestItem>,
    pub sequence_span: SequenceSpan,
    pub expected_versus_actual: Vec<ExpectationComparison>,
    pub citation_rule: &'static str,
    /// Pre-filled `retrospective.record` fields bound to this snapshot.
    pub record_template: Value,
}

/// Argv that computes the digest for a case; repairs point here.
pub fn draft_argv(case: Id) -> Vec<String> {
    [
        PROGRAM_NAME,
        "--case",
        &case.to_string(),
        "retrospective",
        "--draft",
        "--format",
        "json",
    ]
    .map(str::to_owned)
    .into()
}

fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "The recorded case cannot be summarized.",
    )
}

fn text<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str)
}

fn id_at(value: &Value) -> Option<Id> {
    value.get("id")?.as_str()?.parse().ok()
}

/// The case's records as they stood at `sequence`, in ID order.
fn records_at(state: &State, case: Id, sequence: u64) -> Vec<&Record> {
    state
        .records()
        .filter(|record| record.header.case_id == case)
        .filter_map(|record| state.record_at(record.header.id, sequence))
        .collect()
}

fn history_at(state: &State, id: Id, sequence: u64) -> impl Iterator<Item = &Record> {
    state
        .history(id)
        .filter(move |record| record.header.acceptance_sequence <= sequence)
}

fn item(category: &'static str, record: &Record, fact: &'static str, detail: Value) -> DigestItem {
    DigestItem {
        id: format!("{category}/{}", record.header.id),
        category,
        record_ref: record.reference(),
        cite: record.header.id,
        fact,
        detail,
    }
}

/// Runs and Observations whose recorded outcome is invalid, failed, unfinished
/// or incomplete. Used by the digest and by `retrospective.record` warnings.
fn work_items(record: &Record) -> Vec<DigestItem> {
    let body = &record.body;
    let mut items = Vec::new();
    match record.header.id.kind() {
        Kind::Run => {
            let status = text(body, "execution_status");
            let validity = text(body, "evidence_validity");
            let completeness = text(body, "evidence_completeness");
            let detail = json!({
                "execution_status": status,
                "evidence_validity": validity,
                "evidence_completeness": completeness,
            });
            if validity == Some("invalid") {
                items.push(item(
                    "run.invalid",
                    record,
                    "The Run's evidence is recorded as invalid.",
                    detail.clone(),
                ));
            }
            if matches!(status, Some("failed" | "cancelled" | "blocked")) {
                items.push(item(
                    "run.failed",
                    record,
                    "The Run ended failed, cancelled or blocked.",
                    detail.clone(),
                ));
            }
            if matches!(status, Some("declared" | "running" | "unknown")) {
                items.push(item(
                    "run.unfinished",
                    record,
                    "No terminal outcome is recorded for the Run.",
                    detail.clone(),
                ));
            }
            if matches!(completeness, Some("partial" | "missing")) || validity == Some("partial") {
                items.push(item(
                    "run.incomplete",
                    record,
                    "The Run's evidence is recorded as partial, missing or partly valid.",
                    detail,
                ));
            }
            if text(body, "chronology") == Some("retrospective") {
                items.push(item(
                    "run.plan_after_run",
                    record,
                    "The Run's plan was frozen after the Run was declared.",
                    json!({"plan_ref": body.get("retrospective_plan_ref")}),
                ));
            }
        }
        Kind::Observation => {
            if text(body, "validity") == Some("invalid") {
                items.push(item(
                    "observation.invalid",
                    record,
                    "The Observation is recorded or assessed as invalid.",
                    Value::Null,
                ));
            }
            match text(body, "status") {
                Some("retracted") => items.push(item(
                    "observation.retracted",
                    record,
                    "The Observation was retracted.",
                    Value::Null,
                )),
                Some("corrected") => items.push(item(
                    "observation.corrected",
                    record,
                    "The Observation was corrected.",
                    Value::Null,
                )),
                _ => {}
            }
        }
        _ => {}
    }
    items
}

/// Recorded invalid, failed or unfinished work at `sequence`, for warnings
/// about an account that states none was observed.
pub(crate) fn unsuccessful_work(state: &State, case: Id, sequence: u64) -> Vec<RevisionRef> {
    records_at(state, case, sequence)
        .into_iter()
        .filter(|record| {
            work_items(record).iter().any(|item| {
                matches!(
                    item.category,
                    "run.invalid" | "run.failed" | "observation.invalid"
                )
            })
        })
        .map(Record::reference)
        .collect()
}

fn predictions(body: &Value) -> BTreeSet<(Option<Id>, String)> {
    body.pointer("/inquiry/confirmatory/predictions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|prediction| {
            (
                prediction.get("rival_ref").and_then(id_at),
                text(prediction, "predicted_outcome")
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect()
}

fn amended_plan(state: &State, record: &Record) -> Option<DigestItem> {
    let predecessor: RevisionRef =
        serde_json::from_value(record.body.get("predecessor_ref")?.clone()).ok()?;
    let before = state.revision(predecessor)?;
    let changed = predictions(&before.body) != predictions(&record.body);
    Some(item(
        "inquiry.amended_after_freeze",
        record,
        if changed {
            "A frozen plan was amended and its predictions changed."
        } else {
            "A frozen plan was amended; its predictions are unchanged."
        },
        json!({
            "predecessor_ref": predecessor,
            "predictions_changed": changed,
            "lifecycle": text(&record.body, "lifecycle"),
        }),
    ))
}

fn disposition_item(record: &Record, freshness: &FreshnessIndex) -> Option<DigestItem> {
    let body = &record.body;
    let lifecycle = text(body, "lifecycle");
    let status = text(body, "status");
    let found = match record.header.id.kind() {
        Kind::Hypothesis => match lifecycle {
            Some("rejected_within_scope") => (
                "hypothesis.rejected",
                "The hypothesis was rejected within its scope.",
            ),
            Some("retired") => ("hypothesis.retired", "The hypothesis was retired."),
            _ => return None,
        },
        Kind::Option => match lifecycle {
            Some("ruled_out") => ("option.ruled_out", "The option was ruled out."),
            Some("withdrawn") => ("option.withdrawn", "The option was withdrawn."),
            _ => return None,
        },
        Kind::Conclusion => match status {
            Some("superseded") => ("conclusion.superseded", "The conclusion was superseded."),
            Some("retracted") => ("conclusion.retracted", "The conclusion was retracted."),
            Some("rejected") => ("conclusion.rejected", "The conclusion was rejected."),
            _ => return None,
        },
        Kind::Decision if lifecycle == Some("superseded") || status == Some("superseded") => {
            ("decision.superseded", "The decision was superseded.")
        }
        Kind::Assessment => match (status, text(body, "relation")) {
            (Some("retracted"), _) => ("assessment.retracted", "The assessment was retracted."),
            (Some("current") | None, Some("inconclusive" | "non_diagnostic")) => (
                "assessment.inconclusive",
                "The assessment found the evidence inconclusive or non-diagnostic.",
            ),
            _ => return None,
        },
        Kind::Review => {
            if status == Some("superseded") {
                ("review.superseded", "The review was superseded.")
            } else {
                let state = freshness.get(record.header.id)?;
                if state.freshness == Freshness::Current {
                    return None;
                }
                let triggers: BTreeSet<_> = state
                    .reasons
                    .iter()
                    .filter_map(|reason| {
                        serde_json::to_value(&reason.trigger)
                            .ok()
                            .and_then(|value| value["kind"].as_str().map(str::to_owned))
                    })
                    .collect();
                return Some(item(
                    "review.stale",
                    record,
                    "The review no longer matches the current basis of its subject.",
                    json!({"freshness": state.freshness, "triggers": triggers, "subject_ref": body.get("subject_ref")}),
                ));
            }
        }
        _ => return None,
    };
    let detail = match record.header.id.kind() {
        Kind::Assessment => {
            json!({"relation": text(body, "relation"), "subject_ref": body.get("subject_ref")})
        }
        _ => Value::Null,
    };
    Some(item(found.0, record, found.1, detail))
}

fn contended(state: &State, record: &Record, sequence: u64) -> Option<DigestItem> {
    let actors: BTreeSet<String> = history_at(state, record.header.id, sequence)
        .map(|revision| revision.header.recorded_by.to_string())
        .collect();
    (actors.len() > 1).then(|| {
        item(
            "record.revised_by_several_actors",
            record,
            "Several actors recorded revisions of this record.",
            json!({"actors": actors}),
        )
    })
}

const ORDER: &[&str] = &[
    "run.invalid",
    "run.failed",
    "run.unfinished",
    "run.incomplete",
    "run.plan_after_run",
    "observation.invalid",
    "observation.retracted",
    "observation.corrected",
    "inquiry.amended_after_freeze",
    "hypothesis.rejected",
    "hypothesis.retired",
    "option.ruled_out",
    "option.withdrawn",
    "assessment.inconclusive",
    "assessment.retracted",
    "review.stale",
    "review.superseded",
    "conclusion.superseded",
    "conclusion.retracted",
    "conclusion.rejected",
    "decision.superseded",
    "record.revised_by_several_actors",
];

fn span(state: &State, records: &[&Record], sequence: u64) -> Result<SequenceSpan, Failure> {
    let revisions: Vec<&Record> = records
        .iter()
        .flat_map(|record| history_at(state, record.header.id, sequence))
        .collect();
    let first = revisions
        .iter()
        .min_by_key(|record| record.header.acceptance_sequence)
        .ok_or_else(integrity)?;
    let last = revisions
        .iter()
        .max_by_key(|record| record.header.acceptance_sequence)
        .ok_or_else(integrity)?;
    let mut records_by_type = BTreeMap::new();
    for record in records {
        *records_by_type
            .entry(record.header.id.kind().prefix().to_owned())
            .or_default() += 1;
    }
    Ok(SequenceSpan {
        first_sequence: first.header.acceptance_sequence,
        last_sequence: last.header.acceptance_sequence,
        transactions: revisions
            .iter()
            .map(|record| record.header.acceptance_sequence)
            .collect::<BTreeSet<_>>()
            .len(),
        first_recorded_at: first.header.recorded_at.clone(),
        last_recorded_at: last.header.recorded_at.clone(),
        actors: revisions
            .iter()
            .map(|record| record.header.recorded_by.to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        records_by_type,
    })
}

struct Expectations<'a> {
    state: &'a State,
    classifier: Classifier<'a>,
    policy: AudiencePolicy,
    records: &'a [&'a Record],
    sequence: u64,
    /// Sequence of the first recorded Run or Observation.
    work_began: Option<u64>,
}

impl Expectations<'_> {
    /// Free text of a record, or null when the audience may not see it.
    fn visible(&self, record: &Record, value: Option<&Value>) -> Value {
        if self
            .classifier
            .may_show(record, Field::Summary, &self.policy)
        {
            value.cloned().unwrap_or(Value::Null)
        } else {
            Value::Null
        }
    }

    fn after_work(&self, sequence: u64) -> Option<bool> {
        Some(self.work_began.is_some_and(|began| began < sequence))
    }

    fn of_kind(&self, kind: Kind) -> impl Iterator<Item = &&Record> {
        self.records
            .iter()
            .filter(move |record| record.header.id.kind() == kind)
    }

    fn outcome(&self, case: &Record) -> (ActualState, Value) {
        let conclusions: Vec<Value> = self
            .of_kind(Kind::Conclusion)
            .filter(|record| text(&record.body, "status") == Some("validated"))
            .map(|record| json!({"ref": record.reference(), "statement": self.visible(record, record.body.get("statement"))}))
            .collect();
        let decisions: Vec<Value> = self
            .of_kind(Kind::Decision)
            .filter(|record| {
                text(&record.body, "lifecycle") == Some("accepted")
                    || text(&record.body, "status") == Some("accepted")
            })
            .map(|record| json!({"ref": record.reference(), "choice": record.body.get("choice")}))
            .collect();
        let closure = case
            .body
            .get("closure_outcome")
            .filter(|value| !value.is_null());
        let state = if conclusions.is_empty() && decisions.is_empty() && closure.is_none() {
            ActualState::NoOutcomeRecorded
        } else {
            ActualState::Recorded
        };
        (
            state,
            json!({
                "closure_outcome": closure,
                "validated_conclusions": conclusions,
                "accepted_decisions": decisions,
            }),
        )
    }

    fn case_entries(&self, case: &Record, span: &SequenceSpan) -> Vec<ExpectationComparison> {
        let current = case
            .body
            .get("expectations")
            .filter(|value| !value.is_null());
        // The earliest revision from which the current expectations have stood unchanged.
        let origin = current.and_then(|current| {
            let history: Vec<_> = history_at(self.state, case.header.id, self.sequence).collect();
            let unchanged = history
                .iter()
                .rev()
                .take_while(|revision| revision.body.get("expectations") == Some(current))
                .count();
            history.get(history.len() - unchanged).copied()
        });
        let (actual_state, outcome) = self.outcome(case);
        let field = |key: &str| {
            current.and_then(|value| value.get(key)).filter(|value| {
                !value.is_null() && value.as_array().is_none_or(|list| !list.is_empty())
            })
        };
        let entry = |kind: &'static str, key: &str, actual_state: ActualState, actual: Value| {
            let expected = field(key);
            let withheld = !self.classifier.may_show(case, Field::Summary, &self.policy);
            ExpectationComparison {
                id: format!("{kind}/{}", case.header.id),
                kind,
                cite: case.header.id,
                expectation_ref: expected.and(origin.map(Record::reference)),
                expected_state: match (expected, withheld) {
                    (None, _) => ExpectedState::NoExpectationRecorded,
                    (Some(_), true) => ExpectedState::Withheld,
                    (Some(_), false) => ExpectedState::Recorded,
                },
                expected: if withheld {
                    Value::Null
                } else {
                    expected.cloned().unwrap_or(Value::Null)
                },
                recorded_after_work_began: expected
                    .and(origin)
                    .and_then(|origin| self.after_work(origin.header.acceptance_sequence)),
                actual_state,
                actual,
            }
        };
        let runs = self.of_kind(Kind::Run).count();
        vec![
            entry(
                "case.answer_or_choice",
                "expected_answer_or_choice",
                actual_state,
                outcome.clone(),
            ),
            entry(
                "case.effort",
                "expected_effort",
                ActualState::Recorded,
                json!({
                    "transactions": span.transactions,
                    "first_sequence": span.first_sequence,
                    "last_sequence": span.last_sequence,
                    "runs": runs,
                    "inquiries": self.of_kind(Kind::Inquiry).count(),
                    "note": "Recorded effort only; time and work outside Investigation are not measured.",
                }),
            ),
            entry(
                "case.surprising_outcomes",
                "surprising_outcomes",
                actual_state,
                outcome,
            ),
        ]
    }

    fn plan_runs(&self, inquiry: Id) -> Vec<Value> {
        self.of_kind(Kind::Run)
            .filter(|run| {
                [
                    run.body.pointer("/intent/plan_ref"),
                    run.body.get("retrospective_plan_ref"),
                ]
                .into_iter()
                .flatten()
                .any(|plan| id_at(plan) == Some(inquiry))
            })
            .map(|run| {
                json!({
                    "ref": run.reference(),
                    "execution_status": text(&run.body, "execution_status"),
                    "evidence_validity": text(&run.body, "evidence_validity"),
                })
            })
            .collect()
    }

    fn plan_entries(&self) -> Vec<ExpectationComparison> {
        let mut entries = Vec::new();
        for inquiry in self.of_kind(Kind::Inquiry) {
            // The expectation is the plan as frozen, before any outcome was known.
            let Some(frozen) = history_at(self.state, inquiry.header.id, self.sequence)
                .filter(|revision| text(&revision.body, "lifecycle") == Some("frozen"))
                .last()
            else {
                continue;
            };
            let runs = self.plan_runs(inquiry.header.id);
            let predictions = frozen
                .body
                .pointer("/inquiry/confirmatory/predictions")
                .and_then(Value::as_array);
            let Some(predictions) = predictions else {
                let state = if runs.is_empty() {
                    ActualState::NoOutcomeRecorded
                } else {
                    ActualState::Recorded
                };
                let expected = frozen
                    .body
                    .get("desired_observations")
                    .filter(|value| value.as_array().is_some_and(|list| !list.is_empty()));
                entries.push(ExpectationComparison {
                    id: format!("inquiry.desired_observations/{}", inquiry.header.id),
                    kind: "inquiry.desired_observations",
                    cite: inquiry.header.id,
                    expectation_ref: expected.map(|_| frozen.reference()),
                    expected_state: if expected.is_some() {
                        ExpectedState::Recorded
                    } else {
                        ExpectedState::NoExpectationRecorded
                    },
                    expected: self.visible(frozen, expected),
                    recorded_after_work_began: None,
                    actual_state: state,
                    actual: json!({"runs": runs}),
                });
                continue;
            };
            for prediction in predictions {
                let rival = prediction.get("rival_ref").and_then(id_at);
                let assessments: Vec<Value> = self
                    .of_kind(Kind::Assessment)
                    .filter(|record| record.body.get("subject_ref").and_then(id_at) == rival && rival.is_some())
                    .map(|record| json!({"ref": record.reference(), "relation": text(&record.body, "relation"), "status": text(&record.body, "status")}))
                    .collect();
                let lifecycle = rival
                    .and_then(|rival| self.state.record_at(rival, self.sequence))
                    .and_then(|record| text(&record.body, "lifecycle"));
                let state = if runs.is_empty() && assessments.is_empty() {
                    ActualState::NoOutcomeRecorded
                } else {
                    ActualState::Recorded
                };
                entries.push(ExpectationComparison {
                    id: format!(
                        "inquiry.prediction/{}/{}",
                        inquiry.header.id,
                        rival.map(|rival| rival.to_string()).unwrap_or_default()
                    ),
                    kind: "inquiry.prediction",
                    cite: inquiry.header.id,
                    expectation_ref: Some(frozen.reference()),
                    expected_state: ExpectedState::Recorded,
                    expected: json!({
                        "rival_ref": prediction.get("rival_ref"),
                        "predicted_outcome": self.visible(frozen, prediction.get("predicted_outcome")),
                    }),
                    recorded_after_work_began: self.after_work(frozen.header.acceptance_sequence),
                    actual_state: state,
                    actual: json!({"rival_lifecycle": lifecycle, "assessments": assessments, "runs": runs}),
                });
            }
        }
        entries
    }

    fn run_entries(&self) -> Vec<ExpectationComparison> {
        self.of_kind(Kind::Run)
            .filter(|run| {
                run.body.pointer("/intent/kind").and_then(Value::as_str) == Some("exploration")
            })
            .map(|run| {
                let first = self.state.history(run.header.id).next().unwrap_or(run);
                let status = text(&run.body, "execution_status");
                ExpectationComparison {
                    id: format!("run.intent/{}", run.header.id),
                    kind: "run.intent",
                    cite: run.header.id,
                    expectation_ref: Some(first.reference()),
                    expected_state: ExpectedState::Recorded,
                    expected: self.visible(first, first.body.pointer("/intent/intent")),
                    recorded_after_work_began: None,
                    actual_state: if matches!(status, Some("declared" | "running" | "unknown")) {
                        ActualState::NoOutcomeRecorded
                    } else {
                        ActualState::Recorded
                    },
                    actual: json!({
                        "execution_status": status,
                        "evidence_validity": text(&run.body, "evidence_validity"),
                        "evidence_completeness": text(&run.body, "evidence_completeness"),
                    }),
                }
            })
            .collect()
    }

    fn decision_entries(&self) -> Vec<ExpectationComparison> {
        self.of_kind(Kind::Decision)
            .filter(|record| {
                text(&record.body, "lifecycle") == Some("accepted")
                    || text(&record.body, "status") == Some("accepted")
            })
            .map(|record| {
                let expected = record.body.get("expected_outcome").filter(|value| !value.is_null());
                ExpectationComparison {
                    id: format!("decision.expected_outcome/{}", record.header.id),
                    kind: "decision.expected_outcome",
                    cite: record.header.id,
                    expectation_ref: expected.map(|_| record.reference()),
                    expected_state: if expected.is_some() { ExpectedState::Recorded } else { ExpectedState::NoExpectationRecorded },
                    expected: json!({
                        "expected_outcome": self.visible(record, expected),
                        "check_when": self.visible(record, record.body.get("check_when")),
                    }),
                    recorded_after_work_began: None,
                    actual_state: ActualState::NoOutcomeRecorded,
                    actual: json!({"note": "No outcome check of this decision is recorded in the case."}),
                }
            })
            .collect()
    }
}

/// Compute the digest at the verified head of `state`. It reads only; the
/// caller's state is unchanged.
pub fn process_digest(
    state: &State,
    snapshot: &Snapshot,
    case: Id,
) -> Result<ProcessDigest, Failure> {
    if snapshot.sequence != state.sequence()
        || state.verified_snapshot(snapshot.sequence) != Some(&snapshot.head_hash)
    {
        return Err(Failure::new(
            ErrorCode::RetrospectiveStale,
            "Compute the digest at the verified head of the case journal.",
        ));
    }
    let sequence = snapshot.sequence;
    let records = records_at(state, case, sequence);
    let case_record = state.record_at(case, sequence).ok_or_else(integrity)?;
    let freshness = FreshnessIndex::build(state, case);
    let mut items = Vec::new();
    for record in &records {
        items.extend(work_items(record));
        if record.header.id.kind() == Kind::Inquiry
            && let Some(found) = amended_plan(state, record)
        {
            items.push(found);
        }
        items.extend(disposition_item(record, &freshness));
        items.extend(contended(state, record, sequence));
    }
    let rank = |category: &str| ORDER.iter().position(|known| *known == category);
    items.sort_by(|a, b| {
        (rank(a.category), a.record_ref.id).cmp(&(rank(b.category), b.record_ref.id))
    });
    let sequence_span = span(state, &records, sequence)?;
    let expectations = Expectations {
        state,
        classifier: Classifier::new(state),
        policy: AudiencePolicy::default(),
        records: &records,
        sequence,
        work_began: records
            .iter()
            .filter(|record| matches!(record.header.id.kind(), Kind::Run | Kind::Observation))
            .filter_map(|record| state.history(record.header.id).next())
            .map(|record| record.header.acceptance_sequence)
            .min(),
    };
    let mut expected_versus_actual = expectations.case_entries(case_record, &sequence_span);
    expected_versus_actual.extend(expectations.plan_entries());
    expected_versus_actual.extend(expectations.run_entries());
    expected_versus_actual.extend(expectations.decision_entries());
    Ok(ProcessDigest {
        schema_version: DIGEST_VERSION,
        case_id: case,
        snapshot: snapshot.clone(),
        coverage: COVERAGE,
        not_in_journal: NOT_IN_JOURNAL,
        items,
        sequence_span,
        expected_versus_actual,
        citation_rule: CITATION_RULE,
        record_template: json!({
            "case_ref": case_record.reference(),
            "snapshot": snapshot,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::standard_catalog,
        command::Command,
        header::SystemClock,
        identity::SystemIdSource,
        state::{Acceptance, AcceptedTransaction, validate_and_apply},
    };

    struct Case {
        state: State,
        case: Option<Id>,
    }

    impl Case {
        fn new(payload: Value) -> Self {
            let mut case = Self {
                state: State::default(),
                case: None,
            };
            let accepted = case
                .transact(vec![json!({"op":"case.create","payload":payload})])
                .unwrap();
            case.case = Some(accepted.case_id);
            case
        }

        fn id(&self) -> Id {
            self.case.unwrap()
        }

        fn transact(&mut self, operations: Vec<Value>) -> Result<AcceptedTransaction, Failure> {
            let sequence = self.state.sequence() + 1;
            let command: Command = serde_json::from_value(json!({
                "schema_version":"inv.command.v1",
                "command_id":format!("digest-{sequence}"),"case_id":self.case,"actor":"agent:method",
                "preconditions":[],"operations":operations,
            }))
            .unwrap();
            let accepted = validate_and_apply(
                &self.state,
                &command,
                &standard_catalog(),
                Acceptance {
                    sequence,
                    clock: &mut SystemClock,
                    ids: &mut SystemIdSource,
                },
            )?;
            self.state.apply(&accepted)?;
            // Emulates the Store's verified replay interface without file publication.
            self.state.note_verified_snapshot(Snapshot {
                sequence,
                head_hash: format!("sha256:{sequence:064x}").parse().unwrap(),
            })?;
            Ok(accepted)
        }

        fn submit(&mut self, op: &str, payload: Value) -> RevisionRef {
            self.transact(vec![json!({"op":op,"payload":payload})])
                .unwrap()
                .records
                .last()
                .unwrap()
                .reference()
        }

        fn snapshot(&self) -> Snapshot {
            let sequence = self.state.sequence();
            Snapshot {
                sequence,
                head_hash: self.state.verified_snapshot(sequence).unwrap().clone(),
            }
        }

        fn digest(&self) -> ProcessDigest {
            process_digest(&self.state, &self.snapshot(), self.id()).unwrap()
        }

        fn run(&mut self, intent: Value, status: &str, validity: &str) -> RevisionRef {
            self.submit(
                "run.record",
                json!({
                    "intent":intent,"producer":"agent:producer","artifact_refs":[],
                    "execution_status":status,"evidence_completeness":"complete",
                    "evidence_validity":validity,"evidence_kind":"observational","variables_changed":[],
                }),
            )
        }

        fn explore(&mut self, status: &str, validity: &str) -> RevisionRef {
            self.run(
                json!({"kind":"exploration","intent":"Inspect the synthetic trace."}),
                status,
                validity,
            )
        }

        fn retrospective(&self, extra: Value) -> Value {
            let mut payload = json!({
                "case_ref": self.state.current(self.id()).unwrap().reference(),
                "snapshot": self.snapshot(),
                "methods_attempted": [],
                "effective_steps": [],
                "invalid_or_misleading_steps": [],
                "none_observed_reason": "No invalid work was recorded.",
                "costs_or_constraints": "One short synthetic session.",
                "residual_unknowns": [],
                "lessons": [],
            });
            payload
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            payload
        }
    }

    fn lesson(support: &[Id]) -> Value {
        json!({
            "guidance": "Map trace clocks to one reference before ordering events.",
            "applies_when": "Ordering events from traces captured on separate hosts.",
            "support_refs": support,
            "transfer_limits": "Observed in one synthetic case; not a rule for single-host traces.",
        })
    }

    #[test]
    fn digest_lists_recorded_process_facts_and_compares_recorded_expectations() {
        let mut case = Case::new(json!({
            "title":"Why is the stage slow?","mode":"explain",
            "expectations":{"expected_answer_or_choice":"The local queue grows.","expected_effort":"Two runs.","surprising_outcomes":[]},
        }));
        let question = case.submit(
            "question.create",
            json!({"wording":"Why is the stage slow?","importance":"normal"}),
        );
        let hypothesis = |statement: &str, rivals: Vec<RevisionRef>| {
            json!({
                "question_ref":question,"statement":statement,"possible_falsifiers":[],"rival_refs":rivals,"assumption_refs":[],
            })
        };
        let first = case.submit(
            "hypothesis.create",
            hypothesis("The local queue grows.", vec![]),
        );
        let second = case.submit(
            "hypothesis.create",
            hypothesis("The downstream queue grows.", vec![first]),
        );
        let plan = |first_outcome: &str| {
            json!({
                "purpose":"Separate the queues.","method":"Compare stage timings.","controls":["Same load."],
                "limits":["Ten traces."],"desired_observations":["Stage timings."],
                "inquiry":{"mode":"confirmatory","confirmatory":{
                    "predictions":[{"rival_ref":first,"predicted_outcome":first_outcome},{"rival_ref":second,"predicted_outcome":"Downstream timing rises."}],
                    "evidence_holdout_boundary":"Traces 11 to 20 are held out.","prior_exposure":"unseen_declared",
                    "stop_analysis_rule":"Compare after ten traces.",
                }},
            })
        };
        let draft = case.submit("inquiry.create", plan("Local timing rises."));
        let frozen = case.submit(
            "inquiry.freeze",
            json!({"inquiry_ref":draft,"question_ref":question}),
        );
        let failed = case.run(
            json!({"kind":"plan","plan_ref":frozen}),
            "failed",
            "unknown",
        );
        let successor = case.submit(
            "inquiry.amend",
            json!({"inquiry_ref":frozen,"replacement":plan("Local timing stays flat."),"rationale":"Revise after the failed run."}),
        );
        let invalid = case.explore("completed", "invalid");
        case.submit(
            "hypothesis.disposition",
            json!({"hypothesis_ref":second,"lifecycle":"retired","rationale":"The sampled workload excludes this mechanism.","evidence_refs":[]}),
        );
        let before = case.state.clone();
        let digest = case.digest();
        assert_eq!(case.state, before, "the digest is a read");
        assert_eq!(digest, case.digest(), "the digest is deterministic");
        assert!(digest.coverage.contains("recorded work only"));

        let items: BTreeMap<_, _> = digest
            .items
            .iter()
            .map(|item| (item.id.as_str(), item))
            .collect();
        assert_eq!(
            items[format!("run.failed/{}", failed.id).as_str()].cite,
            failed.id
        );
        assert_eq!(
            items[format!("run.invalid/{}", invalid.id).as_str()].record_ref,
            invalid
        );
        let amended = items[format!("inquiry.amended_after_freeze/{}", successor.id).as_str()];
        assert_eq!(amended.detail["predecessor_ref"], json!(frozen));
        assert_eq!(amended.detail["predictions_changed"], true);
        assert!(items.contains_key(format!("hypothesis.retired/{}", second.id).as_str()));
        assert_eq!(digest.sequence_span.first_sequence, 1);
        assert_eq!(
            digest.sequence_span.transactions,
            case.state.sequence() as usize
        );

        let entries: BTreeMap<_, _> = digest
            .expected_versus_actual
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect();
        let answer = entries[format!("case.answer_or_choice/{}", case.id()).as_str()];
        assert_eq!(answer.expected, "The local queue grows.");
        assert_eq!(answer.expectation_ref.unwrap().revision, 1);
        assert_eq!(answer.recorded_after_work_began, Some(false));
        assert_eq!(answer.actual_state, ActualState::NoOutcomeRecorded);
        let surprise = entries[format!("case.surprising_outcomes/{}", case.id()).as_str()];
        assert_eq!(
            surprise.expected_state,
            ExpectedState::NoExpectationRecorded
        );
        let retired = entries[format!("inquiry.prediction/{}/{}", frozen.id, second.id).as_str()];
        assert_eq!(retired.expectation_ref, Some(frozen));
        assert_eq!(
            retired.expected["predicted_outcome"],
            "Downstream timing rises."
        );
        assert_eq!(retired.actual["rival_lifecycle"], "retired");
        assert_eq!(retired.actual["runs"][0]["ref"], json!(failed));
        assert_eq!(
            entries[format!("run.intent/{}", invalid.id).as_str()].actual["evidence_validity"],
            "invalid"
        );
        assert_eq!(
            digest.record_template,
            json!({"case_ref": case.state.current(case.id()).unwrap().reference(), "snapshot": case.snapshot()})
        );
    }

    #[test]
    fn missing_expectations_are_reported_not_invented() {
        let case = Case::new(json!({"title":"Bounded question","mode":"explore"}));
        let digest = case.digest();
        assert!(digest.items.is_empty());
        assert_eq!(digest.expected_versus_actual.len(), 3);
        for entry in &digest.expected_versus_actual {
            assert_eq!(entry.expected_state, ExpectedState::NoExpectationRecorded);
            assert_eq!(entry.expected, Value::Null);
            assert_eq!(entry.expectation_ref, None);
        }
        let stale = Snapshot {
            sequence: 1,
            head_hash: format!("sha256:{}", "f".repeat(64)).parse().unwrap(),
        };
        assert_eq!(
            process_digest(&case.state, &stale, case.id())
                .unwrap_err()
                .error
                .code,
            ErrorCode::RetrospectiveStale
        );
    }

    #[test]
    fn uncited_steps_and_lessons_fail_with_a_repair_naming_the_digest() {
        let mut case = Case::new(json!({"title":"Bounded question","mode":"explore"}));
        let failed = case.explore("failed", "unknown");
        let before = case.state.clone();
        for (extra, path) in [
            (json!({"lessons":[lesson(&[])]}), "/lessons/0/support_refs"),
            (
                json!({"effective_steps":[{"description":"Isolated the stage.","support_refs":[]}]}),
                "/effective_steps/0/support_refs",
            ),
            (
                json!({"invalid_or_misleading_steps":[{"description":"Unordered trace.","support_refs":[]}],"none_observed_reason":null}),
                "/invalid_or_misleading_steps/0/support_refs",
            ),
        ] {
            let payload = case.retrospective(extra);
            let failure = case
                .transact(vec![json!({"op":"retrospective.record","payload":payload})])
                .unwrap_err();
            assert_eq!(failure.error.code, ErrorCode::GateFailed);
            assert_eq!(
                failure.error.path.as_deref(),
                Some(format!("/operations/0/payload{path}").as_str())
            );
            let repair = failure.error.repair.unwrap();
            assert_eq!(repair.argv, Some(draft_argv(case.id())));
            assert_eq!(repair.schema.as_deref(), Some("retrospective.record"));
            assert_eq!(case.state, before);
        }
        let payload = case.retrospective(json!({
            "invalid_or_misleading_steps":[{"description":"The run failed before tracing.","support_refs":[failed.id]}],
            "none_observed_reason":null,
            "lessons":[lesson(&[failed.id])],
        }));
        let accepted = case
            .transact(vec![json!({"op":"retrospective.record","payload":payload})])
            .unwrap();
        assert!(accepted.warnings.is_empty());
    }

    #[test]
    fn lessons_from_failed_inconclusive_or_successful_work_and_explicit_none_observed() {
        let mut case = Case::new(json!({"title":"Bounded question","mode":"explain"}));
        let question = case.submit(
            "question.create",
            json!({"wording":"Why is the stage slow?","importance":"normal"}),
        );
        let hypothesis = case.submit("hypothesis.create", json!({
            "question_ref":question,"statement":"The local queue grows.","possible_falsifiers":[],"rival_refs":[],"assumption_refs":[],
        }));
        let succeeded = case.explore("completed", "valid");
        let inconclusive = case.submit(
            "assessment.add",
            json!({
                "subject_ref":hypothesis,"evidence_refs":[],"relation":"inconclusive",
                "rationale":"The retained trace lacks stage timings.",
            }),
        );
        let digest = case.digest();
        assert!(
            digest
                .items
                .iter()
                .any(|item| item.id == format!("assessment.inconclusive/{}", inconclusive.id))
        );

        // No failure is recorded, so an explicit none-observed account needs no fabricated claim.
        let none = case.retrospective(json!({
            "effective_steps":[{"description":"The completed run gave stage timings.","support_refs":[succeeded.id]}],
            "lessons":[lesson(&[succeeded.id]), lesson(&[inconclusive.id])],
        }));
        let accepted = case
            .transact(vec![json!({"op":"retrospective.record","payload":none})])
            .unwrap();
        assert!(accepted.warnings.is_empty());

        let failed = case.explore("failed", "invalid");
        let mut limitless = case.retrospective(json!({"lessons":[lesson(&[failed.id])]}));
        limitless["lessons"][0]["transfer_limits"] = json!(" ");
        let failure = case
            .transact(vec![
                json!({"op":"retrospective.record","payload":limitless}),
            ])
            .unwrap_err();
        assert_eq!(
            failure.error.path.as_deref(),
            Some("/operations/0/payload/lessons/0/transfer_limits")
        );
        // Recorded failed work is not hidden by a none-observed account: it is accepted with a warning.
        let unlisted = case.retrospective(json!({"lessons":[lesson(&[failed.id])]}));
        let accepted = case
            .transact(vec![
                json!({"op":"retrospective.record","payload":unlisted}),
            ])
            .unwrap();
        assert_eq!(accepted.warnings.len(), 1);
        assert!(accepted.warnings[0].contains(&failed.id.to_string()));
    }
}
