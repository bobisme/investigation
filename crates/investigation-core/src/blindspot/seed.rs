//! Seed checks of `builtin-blindspot.v1`, built from signals that already
//! exist: open objections, effective freshness, advisory measurement flags and
//! the structural gap rules that match §8.5 checks one to one.

use crate::{
    basis::MembershipRule,
    freshness::{Freshness, Trigger},
    identity::Kind,
    measurement_flags::FLAG_RULE_VERSION,
    objection_policy::relevant_open_objections,
    review::ObjectionSeverity,
    wire::Failure,
};

use super::{AuditContext, CheckSource, CheckSpec, Detection, FindingFact, GapRuleMatch, Severity};

pub(super) const CHECKS: &[CheckSpec] = &[
    CheckSpec {
        check_id: "promoted.open_critical_objection",
        version: "v1",
        subject_kinds: &[Kind::Conclusion, Kind::Decision],
        reads: &[
            "Conclusion status and Decision lifecycle or status",
            "status, severity and subject_ref of each Objection",
            "the promotion dependency closure of the promoted record",
        ],
        condition: "A validated Conclusion or accepted Decision has an open critical Objection on itself or on a record of its promotion dependency closure.",
        why: "A record relied on as validated or accepted is contested by an open critical Objection.",
        severity: Severity::Critical,
        severity_reason: "An open critical challenge bears directly on a record others may rely on.",
        suggested_schema: Some("objection.resolve"),
        guide: Some("review"),
        source: CheckSource::Native(open_critical_objection),
    },
    CheckSpec {
        check_id: "promoted.basis_not_current",
        version: "v1",
        subject_kinds: &[Kind::Conclusion, Kind::Decision],
        reads: &[
            "Conclusion status and Decision lifecycle or status",
            "effective freshness and its reasons (freshness rules v1)",
        ],
        condition: "A validated Conclusion or accepted Decision has effective freshness needs_review or disputed for a reason other than an Objection: a critical objection trigger and a change in the open-objection membership a review captured are excluded (critical ones are reported by promoted.open_critical_objection).",
        why: "A record relied on as validated or accepted rests on a basis that changed after promotion.",
        severity: Severity::Material,
        severity_reason: "The promoted result may no longer follow from what it cites until the change is reviewed.",
        suggested_schema: Some("review.submit"),
        guide: Some("review"),
        source: CheckSource::Native(basis_not_current),
    },
    CheckSpec {
        check_id: "conclusion.causal_claim_without_rival_account",
        version: "v1",
        subject_kinds: &[Kind::Conclusion],
        reads: &[
            "lifecycle",
            "causal_factors[].hypothesis_ref",
            "rival_refs of each named Hypothesis",
            "alternatives_not_useful_reason",
        ],
        condition: "The gap rule conclusion.causal_claim_without_rival_account emits a gap.",
        why: "A causal Conclusion records no acknowledged rival account and no reason alternatives were not useful.",
        severity: Severity::Material,
        severity_reason: "An unexamined rival explanation could account for the same evidence.",
        suggested_schema: Some("hypothesis.create"),
        guide: Some("alternatives"),
        source: CheckSource::GapRules {
            rules: GapRuleMatch::Exact(&["conclusion.causal_claim_without_rival_account"]),
            rule_versions: &[],
        },
    },
    CheckSpec {
        check_id: "decision.accepted_with_unresolved_hard_cell",
        version: "v1",
        subject_kinds: &[Kind::Decision],
        reads: &["Decision lifecycle", "hard-criterion cell gaps in the case"],
        condition: "The gap rule decision.accepted_with_unresolved_hard_cell emits a gap.",
        why: "An accepted Decision coexists with a hard-criterion cell that is missing or unknown.",
        severity: Severity::Material,
        severity_reason: "An unassessed hard requirement could rule out the chosen option.",
        suggested_schema: Some("comparison.revise"),
        guide: Some("criteria"),
        source: CheckSource::GapRules {
            rules: GapRuleMatch::Exact(&["decision.accepted_with_unresolved_hard_cell"]),
            rule_versions: &[],
        },
    },
    CheckSpec {
        check_id: "case.ready_to_close_without_retrospective",
        version: "v1",
        subject_kinds: &[Kind::Case],
        reads: &[
            "header.extensions.inv.closure_ready",
            "header.extensions.inv.ready_to_close",
            "retrospective records in the case",
        ],
        condition: "The gap rule case.ready_to_close_without_retrospective emits a gap.",
        why: "The case is marked ready to close and has no recorded Retrospective.",
        severity: Severity::Advisory,
        severity_reason: "What the methods cost and what remains unknown would be lost at closure.",
        suggested_schema: Some("retrospective.record"),
        guide: Some("retrospective"),
        source: CheckSource::GapRules {
            rules: GapRuleMatch::Exact(&["case.ready_to_close_without_retrospective"]),
            rule_versions: &[],
        },
    },
    CheckSpec {
        check_id: "measurement.advisory_flags",
        version: "v1",
        subject_kinds: &[
            Kind::Observation,
            Kind::Assessment,
            Kind::Comparison,
            Kind::Conclusion,
            Kind::Decision,
        ],
        reads: &["measurement fields named by each advisory measurement flag"],
        condition: "An advisory measurement flag rule emits a gap; rule_id names the flag.",
        why: "Recorded measurement fields leave a condition that bears on comparability or a statistic unstated or inconsistent.",
        severity: Severity::Advisory,
        severity_reason: "Advisory: the flag never blocks recording or changes validity, but the measurement may not mean what it is used for.",
        suggested_schema: Some("measurement.assess"),
        guide: Some("measure"),
        source: CheckSource::GapRules {
            rules: GapRuleMatch::Prefix("measurement."),
            rule_versions: &[FLAG_RULE_VERSION],
        },
    },
];

fn open_critical_objection(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let mut detections = Vec::new();
    for record in context.promoted() {
        let critical: Vec<_> = relevant_open_objections(context.state, record)?
            .into_iter()
            .filter(|objection| objection.severity == ObjectionSeverity::Critical)
            .collect();
        if critical.is_empty() {
            continue;
        }
        let objections = critical.iter().map(|objection| objection.objection_ref);
        let disputed = critical.iter().map(|objection| objection.subject_ref);
        detections.push(Detection {
            subject_refs: vec![record.reference()],
            related_refs: objections.clone().chain(disputed.clone()).collect(),
            facts: vec![
                FindingFact {
                    fact: "open_objection:critical".into(),
                    refs: objections.collect(),
                },
                FindingFact {
                    fact: "objection_subject".into(),
                    refs: disputed.collect(),
                },
            ],
        });
    }
    Ok(detections)
}

fn token(value: impl serde::Serialize, field: Option<&str>) -> String {
    let value = serde_json::to_value(value).unwrap_or_default();
    let value = field.map_or(&value, |field| &value[field]);
    value.as_str().unwrap_or_default().to_owned()
}

fn trigger_name(trigger: &Trigger) -> String {
    match trigger {
        Trigger::MembershipChanged { rule } => {
            format!("membership_changed:{}", token(rule, None))
        }
        other => token(other, Some("kind")),
    }
}

/// Objection-driven freshness: a challenge, not a change of the cited basis.
fn objection_trigger(trigger: &Trigger) -> bool {
    matches!(
        trigger,
        Trigger::CriticalObjection
            | Trigger::ObjectionRaisedAfterReview
            | Trigger::MembershipChanged {
                rule: MembershipRule::OpenObjections
            }
    )
}

fn basis_not_current(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let mut detections = Vec::new();
    for record in context.promoted() {
        let Some(freshness) = context.freshness.get(record.header.id) else {
            continue;
        };
        if freshness.freshness == Freshness::Current {
            continue;
        }
        let reasons: Vec<_> = freshness
            .reasons
            .iter()
            .filter(|reason| !objection_trigger(&reason.trigger))
            .collect();
        if reasons.is_empty() {
            continue;
        }
        let mut related = Vec::new();
        let mut facts = vec![FindingFact {
            fact: format!("freshness:{}", freshness.freshness.as_str()),
            refs: vec![freshness.record],
        }];
        for reason in reasons {
            related.push(reason.cause);
            related.extend(reason.path.iter().map(|step| step.cites));
            facts.push(FindingFact {
                fact: format!("freshness_trigger:{}", trigger_name(&reason.trigger)),
                refs: vec![reason.cause],
            });
        }
        detections.push(Detection {
            subject_refs: vec![record.reference()],
            related_refs: related,
            facts,
        });
    }
    Ok(detections)
}
