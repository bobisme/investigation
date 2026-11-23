//! Advice feedback (plan §7.5): an attributed, reversible disposition of one
//! recurring suggestion, recorded with `annotation.add` `role: advice_feedback`.
//!
//! A deferral names the exact suggestion, a gap rule ID and version plus the
//! gap's subject revisions, and stores the gap's read set as its material
//! basis. While the deferral holds, the gap's status is `deferred`: `next`
//! stops ranking it and `frontier` still lists it. The deferral lapses, and
//! the gap is presented again under its rule status, when
//!
//! - the gap rule version changes,
//! - a basis revision has a later revision that [`successor_change`] reports
//!   (the shared "is this later revision material" predicate), or
//! - the gap now reads a record the deferral did not cover.
//!
//! A later `resume` ends it explicitly. Feedback changes only how advice is
//! presented. Rules still run, and gates, caveats and validation never read
//! feedback, so a deferred blocker still blocks its promotion.
//!
//! The authored reason and revisit condition stay in the annotation. A gap
//! names the annotation, whose `show` applies its classification; it never
//! copies the text.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    command::RevisionRef,
    freshness::{Trigger, successor_change},
    gap_explain::require_rule,
    gap_rules::{GAP_RULE_VERSION, Gap, GapStatus, case_gaps},
    header::Actor,
    identity::{Id, Kind},
    state::{OperationContext, Record, State},
    wire::{ErrorCode, Failure},
};

pub const ADVICE_FEEDBACK_ROLE: &str = "advice_feedback";

const NOTICE: &str = "Advice feedback changes only how this suggestion is presented. The rule still runs, and gates, caveats and validation still come from recorded evidence and policy; a deferral satisfies nothing.";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdviceDisposition {
    /// Stop ranking the suggestion until it changes materially or is resumed.
    Defer,
    /// End a recorded deferral now.
    Resume,
}

/// The `advice_feedback` object of `annotation.add` with
/// `role: advice_feedback`. `subject_refs` are the gap's exact subject
/// revisions and `content` is the reason, as text.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdviceFeedbackInput {
    /// The gap rule ID that emits the suggestion, as `frontier` reports it.
    pub rule_id: String,
    /// The gap rule version that emitted it, as `frontier` reports it.
    pub rule_version: String,
    pub disposition: AdviceDisposition,
    /// When the investigator means to look again. Recorded, never evaluated.
    #[serde(default)]
    pub revisit_condition: Option<String>,
}

/// What the stored feedback looks like under the annotation body's
/// `advice_feedback` key.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredFeedback {
    gap_id: String,
    rule_id: String,
    rule_version: String,
    disposition: AdviceDisposition,
    revisit_condition: Option<String>,
    /// The gap's read set when the feedback was recorded.
    material_basis: Vec<RevisionRef>,
    /// The status the rule emitted when the feedback was recorded.
    rule_status: GapStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdviceState {
    /// A deferral holds; the gap's status is `deferred`.
    Deferred,
    /// A deferral was recorded but no longer holds; see `resurfaced_because`.
    Resurfaced,
    /// The latest feedback resumed the suggestion.
    Resumed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdviceChangeKind {
    RuleVersionChanged,
    BasisChanged,
    BasisExpanded,
}

/// One reason a deferral no longer holds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdviceChange {
    pub kind: AdviceChangeKind,
    /// The basis revision that changed, or the newly read record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_ref: Option<RevisionRef>,
    /// The freshness trigger for a changed basis revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<Trigger>,
    pub description: String,
}

/// The recorded advice feedback that governs one gap.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GapAdvice {
    pub state: AdviceState,
    /// The latest feedback annotation for this gap; `show` it for the reason.
    pub feedback_ref: RevisionRef,
    pub recorded_by: Actor,
    pub disposition: AdviceDisposition,
    /// The gap rule version the feedback named.
    pub feedback_rule_version: String,
    /// The status the rule emits now. While deferred, `status` is `deferred`.
    pub rule_status: GapStatus,
    pub has_revisit_condition: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resurfaced_because: Vec<AdviceChange>,
    pub notice: String,
}

fn failure(message: impl Into<String>, path: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(path)
}

pub(crate) fn gap_id(rule_id: &str, subjects: &[RevisionRef]) -> String {
    let ids: BTreeSet<Id> = subjects.iter().map(|subject| subject.id).collect();
    format!(
        "gap:{rule_id}:{}",
        ids.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("+")
    )
}

fn stored(record: &Record) -> Option<StoredFeedback> {
    if record.header.id.kind() != Kind::Annotation
        || record.body.get("role").and_then(Value::as_str) != Some(ADVICE_FEEDBACK_ROLE)
    {
        return None;
    }
    serde_json::from_value(record.body.get("advice_feedback")?.clone()).ok()
}

/// The latest feedback for each gap ID in the case, by acceptance order.
fn latest_feedback<'a>(
    records: impl Iterator<Item = &'a Record>,
    case_id: Id,
) -> BTreeMap<String, (&'a Record, StoredFeedback)> {
    let mut latest = BTreeMap::<String, (&Record, StoredFeedback)>::new();
    for record in records.filter(|record| record.header.case_id == case_id) {
        let Some(feedback) = stored(record) else {
            continue;
        };
        let newer = latest.get(&feedback.gap_id).is_none_or(|(held, _)| {
            (held.header.acceptance_sequence, held.header.id)
                < (record.header.acceptance_sequence, record.header.id)
        });
        if newer {
            latest.insert(feedback.gap_id.clone(), (record, feedback));
        }
    }
    latest
}

/// Validate an `advice_feedback` annotation and return the object stored
/// under the body's `advice_feedback` key. `subjects` are the resolved
/// subject revisions, already checked to be distinct.
pub(crate) fn feedback_body(
    ctx: &OperationContext<'_>,
    input: &AdviceFeedbackInput,
    subjects: &[RevisionRef],
) -> Result<Value, Failure> {
    require_rule(&input.rule_id, "/advice_feedback/rule_id")?;
    if input.rule_version != GAP_RULE_VERSION {
        let mut failure = failure(
            format!(
                "The current gap rule version is {GAP_RULE_VERSION}. Feedback names the version that emits the suggestion; read frontier again."
            ),
            "/advice_feedback/rule_version",
        );
        failure.error.expected = Some(json!(GAP_RULE_VERSION));
        failure.error.actual = Some(json!(input.rule_version));
        return Err(failure);
    }
    if input
        .revisit_condition
        .as_deref()
        .is_some_and(|condition| condition.trim().is_empty())
    {
        return Err(failure(
            "State the revisit condition, or leave it out.",
            "/advice_feedback/revisit_condition",
        ));
    }
    let case_id = ctx.case_id().ok_or_else(|| {
        Failure::new(
            ErrorCode::CaseRequired,
            "Advice feedback belongs to one case.",
        )
    })?;
    let gap_id = gap_id(&input.rule_id, subjects);
    let state = ctx.state();
    let prior = latest_feedback(ctx.records(), case_id).remove(&gap_id);
    if prior
        .as_ref()
        .is_some_and(|(record, _)| record.header.acceptance_sequence == ctx.acceptance_sequence())
    {
        return Err(failure(
            "Record one advice feedback per suggestion in a transaction.",
            "/advice_feedback",
        ));
    }
    let mut subjects = subjects.to_vec();
    subjects.sort_by_key(|reference| (reference.id, reference.revision));
    let (material_basis, rule_status) = match input.disposition {
        AdviceDisposition::Defer => {
            let gaps = case_gaps(state, case_id)?;
            let Some(gap) = gaps.iter().find(|gap| gap.gap_id == gap_id) else {
                let mut failure = failure(
                    format!(
                        "Rule {} emits no suggestion for exactly these subjects now; only an emitted suggestion can be deferred. Read frontier for its rule ID and subject revisions.",
                        input.rule_id
                    ),
                    "/advice_feedback/rule_id",
                );
                let near: Vec<&str> = gaps
                    .iter()
                    .filter(|gap| {
                        gap.subject_refs
                            .iter()
                            .any(|subject| subjects.iter().any(|named| named.id == subject.id))
                    })
                    .map(|gap| gap.gap_id.as_str())
                    .take(20)
                    .collect();
                failure.error.expected = Some(json!(near));
                failure.error.actual = Some(json!(gap_id));
                return Err(failure);
            };
            if gap.subject_refs != subjects {
                let mut failure = failure(
                    "Name the exact subject revisions the suggestion reports.",
                    "/subject_refs",
                );
                failure.error.expected = Some(json!(gap.subject_refs));
                failure.error.actual = Some(json!(subjects));
                return Err(failure);
            }
            let rule_status = gap
                .advice
                .as_ref()
                .map_or(gap.status, |advice| advice.rule_status);
            if rule_status == GapStatus::Resolved {
                return Err(failure(
                    "A resolved gap has nothing to defer.",
                    "/advice_feedback/disposition",
                ));
            }
            (gap.read_set.clone(), rule_status)
        }
        AdviceDisposition::Resume => {
            let Some((_, prior)) =
                prior.filter(|(_, prior)| prior.disposition == AdviceDisposition::Defer)
            else {
                let mut failure = failure(
                    "No deferral of this suggestion is in force; there is nothing to resume.",
                    "/advice_feedback/disposition",
                );
                failure.error.actual = Some(json!(gap_id));
                return Err(failure);
            };
            (subjects.clone(), prior.rule_status)
        }
    };
    Ok(serde_json::to_value(StoredFeedback {
        gap_id,
        rule_id: input.rule_id.clone(),
        rule_version: input.rule_version.clone(),
        disposition: input.disposition,
        revisit_condition: input.revisit_condition.clone(),
        material_basis,
        rule_status,
    })
    .expect("advice feedback serializes"))
}

/// Present each gap under its latest feedback: defer it while the deferral
/// holds, or say why the deferral no longer applies.
pub(crate) fn apply(state: &State, case_id: Id, gaps: &mut [Gap]) {
    let latest = latest_feedback(state.records(), case_id);
    if latest.is_empty() {
        return;
    }
    for gap in gaps.iter_mut() {
        let Some((record, feedback)) = latest.get(&gap.gap_id) else {
            continue;
        };
        let changes = match feedback.disposition {
            AdviceDisposition::Resume => Vec::new(),
            AdviceDisposition::Defer => changes(state, case_id, gap, feedback),
        };
        let advice_state = match feedback.disposition {
            AdviceDisposition::Resume => AdviceState::Resumed,
            AdviceDisposition::Defer if changes.is_empty() => AdviceState::Deferred,
            AdviceDisposition::Defer => AdviceState::Resurfaced,
        };
        let rule_status = gap.status;
        if advice_state == AdviceState::Deferred {
            gap.status = GapStatus::Deferred;
        }
        gap.advice = Some(Box::new(GapAdvice {
            state: advice_state,
            feedback_ref: record.reference(),
            recorded_by: record.header.created_by.clone(),
            disposition: feedback.disposition,
            feedback_rule_version: feedback.rule_version.clone(),
            rule_status,
            has_revisit_condition: feedback.revisit_condition.is_some(),
            resurfaced_because: changes,
            notice: NOTICE.into(),
        }));
    }
}

/// Every material change since the deferral, in a stable order.
fn changes(state: &State, case_id: Id, gap: &Gap, feedback: &StoredFeedback) -> Vec<AdviceChange> {
    let mut changes = Vec::new();
    if feedback.rule_version != gap.rule_version {
        changes.push(AdviceChange {
            kind: AdviceChangeKind::RuleVersionChanged,
            record_ref: None,
            trigger: None,
            description: format!(
                "The gap rule version changed from {} to {}; the suggestion is re-evaluated under the new rule.",
                feedback.rule_version, gap.rule_version
            ),
        });
    }
    for basis in &feedback.material_basis {
        if let Some(trigger) = successor_change(state, case_id, *basis) {
            changes.push(AdviceChange {
                kind: AdviceChangeKind::BasisChanged,
                record_ref: Some(*basis),
                description: format!(
                    "{} revision {} changed materially after the deferral: {}.",
                    basis.id,
                    basis.revision,
                    trigger.describe()
                ),
                trigger: Some(trigger),
            });
        }
    }
    let covered: BTreeSet<Id> = feedback
        .material_basis
        .iter()
        .map(|basis| basis.id)
        .collect();
    for read in gap
        .read_set
        .iter()
        .filter(|read| !covered.contains(&read.id))
    {
        changes.push(AdviceChange {
            kind: AdviceChangeKind::BasisExpanded,
            record_ref: Some(*read),
            trigger: None,
            description: format!(
                "The suggestion now also reads {} revision {}, which the deferral did not cover.",
                read.id, read.revision
            ),
        });
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gap_rules::{GapFields, make_gap};

    fn reference(value: &str) -> RevisionRef {
        RevisionRef {
            id: value.parse().unwrap(),
            revision: 1,
        }
    }

    #[test]
    fn a_changed_rule_version_or_a_wider_read_set_lapses_a_deferral() {
        let subject = reference("hyp_019acd16-0000-7000-8000-000000000001");
        let read = reference("q_019acd16-0000-7000-8000-000000000002");
        let gap = make_gap(GapFields {
            rule_id: "hypothesis.no_assessment",
            subject_refs: vec![subject],
            read_set: vec![read],
            status: GapStatus::ReadyToAnalyze,
            reason: "fixture",
            prerequisites: vec![],
            suggested_operation: None,
            unresolved_fields: vec![],
            outcome_branches: vec![],
            decision_effect: "unknown",
            time_sensitive: false,
            creation_sequence: 1,
        });
        let feedback = StoredFeedback {
            gap_id: gap.gap_id.clone(),
            rule_id: gap.rule_id.clone(),
            rule_version: "slice-gaps.v0".into(),
            disposition: AdviceDisposition::Defer,
            revisit_condition: None,
            material_basis: vec![],
            rule_status: GapStatus::ReadyToAnalyze,
        };
        let case = "case_019acd16-0000-7000-8000-000000000003".parse().unwrap();
        let found = changes(&State::default(), case, &gap, &feedback);
        let kinds: Vec<_> = found.iter().map(|change| change.kind).collect();
        assert_eq!(
            kinds,
            [
                AdviceChangeKind::RuleVersionChanged,
                AdviceChangeKind::BasisExpanded,
                AdviceChangeKind::BasisExpanded,
            ]
        );
        assert!(found[0].description.contains("slice-gaps.v0"));
        assert!(found[0].description.contains(GAP_RULE_VERSION));
    }
}
