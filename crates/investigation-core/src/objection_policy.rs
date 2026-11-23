//! Objection policy (§14.3): which open Objections bear on a promotion, what
//! a Decision must acknowledge, and how disagreement stays visible.
//!
//! Critical objections block Conclusion validation (enforced in `review_ops`).
//! Decisions may proceed under disagreement, but the open objections and
//! uncertain comparisons are stored with the accepted Decision, and the pinned
//! policy decides which of them need an explicit, attributed acknowledgment.
//! Nothing here deletes or rewrites an Objection: resolution and withdrawal are
//! new revisions carrying their own rationale.
use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    basis::{MembershipRule, MembershipScope, ObjectionBody, membership},
    command::{Reference, RevisionRef},
    identity::{Id, Kind},
    review::{ObjectionSeverity, ObjectionStatus},
    review_ops::{ValidationReviewBody, required_read_set},
    state::{Record, State},
    wire::{ErrorCode, Failure},
};

/// One open Objection that bears on a subject or on a record in its basis.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenObjection {
    pub objection_ref: RevisionRef,
    /// The exact disputed revision named by the objection.
    pub subject_ref: RevisionRef,
    pub severity: ObjectionSeverity,
    pub statement: String,
}

fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "Invalid stored objection dependency.",
    )
}

/// Open objections in `case` whose subject is one of `subjects`, in ID order.
pub fn open_objections(
    state: &State,
    case: Id,
    subjects: &BTreeSet<Id>,
) -> Result<Vec<OpenObjection>, Failure> {
    let mut found = Vec::new();
    for record in state.records().filter(|record| {
        record.header.case_id == case && record.header.id.kind() == Kind::Objection
    }) {
        let body: ObjectionBody =
            serde_json::from_value(record.body.clone()).map_err(|_| integrity())?;
        if body.status == ObjectionStatus::Open && subjects.contains(&body.subject_ref.id) {
            found.push(OpenObjection {
                objection_ref: record.reference(),
                subject_ref: body.subject_ref,
                severity: body.severity,
                statement: body.statement,
            });
        }
    }
    found.sort_by_key(|objection| objection.objection_ref.id);
    Ok(found)
}

/// Open objections on the record itself or on any record of its promotion
/// dependency closure. An unreadable closure falls back to the record alone.
pub fn relevant_open_objections(
    state: &State,
    record: &Record,
) -> Result<Vec<OpenObjection>, Failure> {
    let mut subjects: BTreeSet<Id> = [record.header.id].into();
    if matches!(
        record.header.id.kind(),
        Kind::Conclusion | Kind::Decision | Kind::Assessment | Kind::Comparison
    ) && let Ok(read_set) = required_read_set(state, record)
    {
        subjects.extend(read_set.iter().map(|reference| reference.id));
    }
    open_objections(state, record.header.case_id, &subjects)
}

/// True when an open objection names `record` but was not part of the
/// open-objection set its validation review captured: the challenge arrived
/// after promotion, so the subject is effectively disputed rather than merely
/// awaiting a fresh review.
pub fn raised_after_review(
    state: &State,
    record: &Record,
    review: &ValidationReviewBody,
) -> Result<bool, Failure> {
    let scope = MembershipScope::Subject {
        case_id: record.header.case_id,
        subject_id: record.header.id,
    };
    let captured: BTreeSet<Id> = review
        .memberships
        .iter()
        .filter(|basis| basis.rule == MembershipRule::OpenObjections && basis.scope == scope)
        .flat_map(|basis| basis.members.iter().map(|member| member.id))
        .collect();
    let now = membership(state, MembershipRule::OpenObjections, scope)?;
    Ok(now
        .members
        .iter()
        .any(|member| !captured.contains(&member.id)))
}

/// Which open objections `decision.accept` must acknowledge explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AcknowledgmentPolicy {
    /// Critical objections need an explicit acknowledgment; material ones are
    /// recorded and shown, and may be acknowledged.
    Critical,
    /// Every open material or critical objection needs an acknowledgment.
    AllOpen,
}

impl AcknowledgmentPolicy {
    fn requires(self, severity: ObjectionSeverity) -> bool {
        match self {
            Self::Critical => severity == ObjectionSeverity::Critical,
            Self::AllOpen => true,
        }
    }
}

/// The acknowledgment policy in force for a case.
///
/// Seam for pack policy inputs (W09.07): a pinned pack will be able to select
/// [`AcknowledgmentPolicy::AllOpen`] here. Until such inputs exist every case
/// uses the core default, which is recorded with each accepted decision so a
/// later policy change never rewrites what was required at acceptance.
pub fn acknowledgment_policy(_state: &State, _case: Id) -> AcknowledgmentPolicy {
    AcknowledgmentPolicy::Critical
}

/// Caller-supplied acknowledgment of one open objection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectionAcknowledgmentInput {
    pub objection_ref: Reference,
    /// Why the choice is acceptable despite the objection.
    pub rationale: String,
}

/// One open objection as stored with the accepted decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcknowledgedObjection {
    pub objection_ref: RevisionRef,
    pub subject_ref: RevisionRef,
    pub severity: ObjectionSeverity,
    pub acknowledgment_required: bool,
    /// The attributed rationale; `None` when the objection was only made visible.
    pub acknowledgment: Option<String>,
}

/// Disagreement and uncertainty the decision was accepted over. Stored on the
/// accepted revision and never recomputed; the live state is read separately.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceChallenges {
    pub policy: AcknowledgmentPolicy,
    pub open_objections: Vec<AcknowledgedObjection>,
    /// Cited comparisons whose outcome is unknown or lacks verified evidence.
    pub uncertain_comparisons: Vec<RevisionRef>,
    /// Hard criteria accepted as explicit conditions rather than as satisfied.
    pub conditional_hard_criteria: Vec<RevisionRef>,
}

impl AcceptanceChallenges {
    pub fn is_empty(&self) -> bool {
        self.open_objections.is_empty()
            && self.uncertain_comparisons.is_empty()
            && self.conditional_hard_criteria.is_empty()
    }

    /// Receipt warnings naming every item, so acceptance is never silent.
    pub fn warnings(&self, decision: Id) -> Vec<String> {
        let mut out = Vec::new();
        for objection in &self.open_objections {
            out.push(format!(
                "Decision {decision} accepted over open {} objection {} ({}).",
                severity_label(objection.severity),
                objection.objection_ref.id,
                if objection.acknowledgment.is_some() {
                    "acknowledged"
                } else {
                    "visible, not acknowledged"
                }
            ));
        }
        for comparison in &self.uncertain_comparisons {
            out.push(format!(
                "Decision {decision} cites uncertain comparison {}.",
                comparison.id
            ));
        }
        for criterion in &self.conditional_hard_criteria {
            out.push(format!(
                "Decision {decision} is conditional on hard criterion {}.",
                criterion.id
            ));
        }
        out
    }
}

fn severity_label(severity: ObjectionSeverity) -> &'static str {
    match severity {
        ObjectionSeverity::Critical => "critical",
        ObjectionSeverity::Material => "material",
    }
}

fn gate(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::GateFailed, message).at(path)
}

/// Pair each relevant open objection with its acknowledgment and enforce the
/// policy. Acknowledgments must name relevant open objections exactly once.
pub fn acknowledge(
    open: Vec<OpenObjection>,
    acknowledgments: &[(RevisionRef, String)],
    policy: AcknowledgmentPolicy,
) -> Result<Vec<AcknowledgedObjection>, Failure> {
    let mut seen = BTreeSet::new();
    for (index, (reference, rationale)) in acknowledgments.iter().enumerate() {
        let path = format!("/acknowledgments/{index}");
        if rationale.trim().is_empty() || rationale.chars().count() > 2048 {
            return Err(gate(
                &format!("{path}/rationale"),
                "Supply nonempty bounded acknowledgment text.",
            ));
        }
        if !seen.insert(reference.id) {
            return Err(
                Failure::new(ErrorCode::InvalidInput, "Acknowledge each objection once.")
                    .at(format!("{path}/objection_ref")),
            );
        }
        if !open
            .iter()
            .any(|objection| objection.objection_ref == *reference)
        {
            let mut failure = gate(
                &format!("{path}/objection_ref"),
                "Acknowledge only current open objections on this decision or its basis.",
            );
            failure.error.expected = Some(json!(
                open.iter()
                    .map(|objection| objection.objection_ref)
                    .collect::<Vec<_>>()
            ));
            return Err(failure);
        }
    }
    let records: Vec<_> = open
        .into_iter()
        .map(|objection| AcknowledgedObjection {
            acknowledgment: acknowledgments
                .iter()
                .find(|(reference, _)| *reference == objection.objection_ref)
                .map(|(_, rationale)| rationale.clone()),
            acknowledgment_required: policy.requires(objection.severity),
            objection_ref: objection.objection_ref,
            subject_ref: objection.subject_ref,
            severity: objection.severity,
        })
        .collect();
    let missing: Vec<_> = records
        .iter()
        .filter(|objection| objection.acknowledgment_required && objection.acknowledgment.is_none())
        .map(|objection| objection.objection_ref)
        .collect();
    if !missing.is_empty() {
        let mut failure = gate(
            "/acknowledgments",
            "Acknowledge each open objection the decision policy requires, with a rationale.",
        );
        failure.error.expected = Some(json!(missing));
        failure.error.actual = Some(json!({"policy": policy}));
        return Err(failure.with_schema("decision.accept"));
    }
    Ok(records)
}

/// Read-time challenge view for Conclusions and Decisions: currently open
/// objections on the record or its basis, and for an accepted decision the
/// stored acceptance record with each objection's present status.
pub fn challenges_view(state: &State, record: &Record) -> Option<Value> {
    if !matches!(record.header.id.kind(), Kind::Conclusion | Kind::Decision) {
        return None;
    }
    // References and severity only: objection text is read from the objection
    // itself, under its own classification.
    let open = match relevant_open_objections(state, record) {
        Ok(open) => open
            .into_iter()
            .map(|objection| {
                json!({
                    "objection_ref": objection.objection_ref,
                    "subject_ref": objection.subject_ref,
                    "severity": objection.severity,
                })
            })
            .collect::<Vec<_>>(),
        Err(failure) => return Some(json!({"unavailable": failure.error})),
    };
    let mut view = json!({"open_objections": open});
    if let Some(stored) = record.body.get("acceptance_challenges")
        && let Ok(stored) = serde_json::from_value::<AcceptanceChallenges>(stored.clone())
    {
        let objections: Vec<_> = stored
            .open_objections
            .iter()
            .map(|objection| {
                let status = state
                    .current(objection.objection_ref.id)
                    .map(|current| current.body["status"].clone())
                    .unwrap_or(Value::Null);
                let mut item = json!(objection);
                item["current_status"] = status;
                item
            })
            .collect();
        view["accepted_over"] = json!({
            "policy": stored.policy,
            "open_objections": objections,
            "uncertain_comparisons": stored.uncertain_comparisons,
            "conditional_hard_criteria": stored.conditional_hard_criteria,
        });
    }
    Some(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(n: u64) -> RevisionRef {
        RevisionRef {
            id: format!("obj_019acd16-0000-7000-8000-{n:012}")
                .parse()
                .unwrap(),
            revision: 1,
        }
    }

    fn open(n: u64, severity: ObjectionSeverity) -> OpenObjection {
        OpenObjection {
            objection_ref: reference(n),
            subject_ref: RevisionRef {
                id: "dec_019acd16-0000-7000-8000-000000000009".parse().unwrap(),
                revision: 1,
            },
            severity,
            statement: "A concern.".into(),
        }
    }

    #[test]
    fn policy_decides_which_open_objections_need_an_acknowledgment() {
        let set = || {
            vec![
                open(1, ObjectionSeverity::Material),
                open(2, ObjectionSeverity::Critical),
            ]
        };
        let critical = vec![(reference(2), "Accepted risk.".to_owned())];
        let recorded = acknowledge(set(), &critical, AcknowledgmentPolicy::Critical).unwrap();
        assert_eq!(recorded.len(), 2, "material objections stay visible");
        assert!(!recorded[0].acknowledgment_required);
        assert_eq!(recorded[0].acknowledgment, None);
        assert_eq!(
            recorded[1].acknowledgment.as_deref(),
            Some("Accepted risk.")
        );

        let missing = acknowledge(set(), &[], AcknowledgmentPolicy::Critical).unwrap_err();
        assert_eq!(missing.error.expected, Some(json!([reference(2)])));
        let missing = acknowledge(set(), &critical, AcknowledgmentPolicy::AllOpen).unwrap_err();
        assert_eq!(missing.error.expected, Some(json!([reference(1)])));

        let twice = [critical[0].clone(), critical[0].clone()];
        assert_eq!(
            acknowledge(set(), &twice, AcknowledgmentPolicy::Critical)
                .unwrap_err()
                .error
                .code,
            ErrorCode::InvalidInput
        );
        let blank = [(reference(2), " ".to_owned())];
        assert!(acknowledge(set(), &blank, AcknowledgmentPolicy::Critical).is_err());
        let stranger = [(reference(3), "Unrelated.".to_owned())];
        assert!(acknowledge(set(), &stranger, AcknowledgmentPolicy::Critical).is_err());
        let stale = [(
            RevisionRef {
                revision: 2,
                ..reference(2)
            },
            "Stale.".to_owned(),
        )];
        assert!(acknowledge(set(), &stale, AcknowledgmentPolicy::Critical).is_err());
    }
}
