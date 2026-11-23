//! Attributed judgments that are not changes to the judged claim: measurement
//! validity (`measurement.assess`) and classification changes. Only a judgment
//! that makes effective validity worse reaches dependents.

use super::source_use::apply_as;
use super::*;
use crate::{review::EffectiveDisposition, review_ops::effective_disposition};

fn judge(
    case: &mut Case,
    actor: &str,
    observation: RevisionRef,
    validity: &str,
    verification: Value,
) -> RevisionRef {
    apply_as(
        case,
        actor,
        vec![op(
            "measurement.assess",
            json!({
                "observation_ref": observation, "channel": null, "validity": validity,
                "verification": verification,
                "rationale": "Judged from the recorded setup.", "limits": []
            }),
        )],
    )
    .unwrap()
}

fn equivalent() -> Value {
    json!([{"kind":"equivalent","justification":"A known reference value was reproduced."}])
}

/// Every current record that cites the fixture Observation, directly or not.
fn dependents_of_observation(f: &Fixture) -> [RevisionRef; 7] {
    [
        f.assessment,
        f.conclusion,
        f.conclusion_review,
        f.note,
        f.comparison,
        f.decision,
        f.decision_review,
    ]
}

fn all_current(case: &Case) {
    for result in case.index.records() {
        assert_eq!(result.freshness, Freshness::Current, "{result:#?}");
    }
}

#[test]
fn a_valid_measurement_judgment_keeps_dependents_current_and_an_invalid_one_flags_them() {
    let mut case = Case::new();
    let f = case.fixture();
    let valid = judge(
        &mut case,
        "agent:reviewer",
        f.observation,
        "valid",
        equivalent(),
    );
    assert_eq!(valid.revision, 2);
    all_current(&case);
    // The earlier revision, as cited by dependents, is not stale either.
    let historical = case
        .index
        .assess(&case.state, case.state.revision(f.observation).unwrap());
    assert_eq!(historical.freshness, Freshness::Current);
    let disposition = |case: &Case, reference: RevisionRef| {
        effective_disposition(&case.state, case.state.current(reference.id).unwrap())
    };
    for reference in [f.assessment, f.conclusion, f.comparison, f.decision] {
        assert_eq!(disposition(&case, reference), EffectiveDisposition::Current);
    }

    let invalid = judge(
        &mut case,
        "agent:second-reader",
        valid,
        "invalid",
        json!([]),
    );
    for reference in dependents_of_observation(&f) {
        let result = case.freshness(reference);
        assert_eq!(result.freshness, Freshness::NeedsReview, "{result:#?}");
        let reason = only_reason(result);
        assert_eq!(reason.trigger, Trigger::InvalidMeasurement);
        assert_eq!(reason.cause, invalid);
        assert_eq!(reason.path[0].record, reference.id);
        assert_eq!(reason.path.last().unwrap().cites, f.observation);
    }
    for reference in [f.assessment, f.conclusion, f.comparison, f.decision] {
        assert_eq!(
            disposition(&case, reference),
            EffectiveDisposition::NeedsReview
        );
    }
    // The judged Observation itself keeps its history and its own freshness.
    assert_eq!(case.freshness(f.observation).freshness, Freshness::Current);
    let reason = only_reason(case.freshness(f.comparison));
    assert_eq!(
        reason.describe(),
        format!(
            "invalid measurement: {} r3 via {} evidence_refs",
            f.observation.id, f.comparison.id
        )
    );
}

#[test]
fn a_lowered_validity_names_both_states() {
    let mut case = Case::new();
    let f = case.fixture();
    let partial = judge(
        &mut case,
        "agent:reviewer",
        f.observation,
        "partial",
        equivalent(),
    );
    let reason = only_reason(case.freshness(f.conclusion));
    assert_eq!(
        reason.trigger,
        Trigger::ValidityLowered {
            from: MeasurementValidity::Valid,
            to: MeasurementValidity::Partial
        }
    );
    assert_eq!(reason.cause, partial);
    // Raising validity again is not a new trigger, but does not clear the
    // earlier citation either: dependents still cite the valid revision.
    judge(&mut case, "agent:reviewer", partial, "valid", equivalent());
    all_current(&case);
}

#[test]
fn a_verification_going_stale_reaches_dependents_incrementally() {
    let mut case = Case::new();
    let f = case.fixture();
    let check = case.state.revision(f.observation).unwrap().body.clone();
    let check = case.image(Kind::Observation, check, None);
    judge(
        &mut case,
        "agent:reviewer",
        f.observation,
        "valid",
        json!([{"kind":"instrument_check","evidence_ref":check,"rationale":"The probe read a known value."}]),
    );
    all_current(&case);
    // Retracting the instrument check leaves the reviewer's judgment without a
    // current verification, so effective validity drops from valid to unknown.
    // `Case::settle` checks that the incremental index sees this too.
    case.one(
        "observation.retract",
        json!({"observation_ref":check,"revision_reason":"The probe was miscalibrated."}),
    );
    let reason = only_reason(case.freshness(f.assessment));
    assert_eq!(
        reason.trigger,
        Trigger::ValidityLowered {
            from: MeasurementValidity::Valid,
            to: MeasurementValidity::Unknown
        }
    );
}

#[test]
fn classification_changes_do_not_change_freshness_or_the_overview() {
    let mut case = Case::new();
    let f = case.fixture();
    let before = case.index.overview();
    for subject in [f.observation, f.conclusion] {
        case.one(
            "classification.raise",
            json!({"subject_ref":subject,"level":"restricted","reason":"Names a customer host."}),
        );
    }
    all_current(&case);
    assert_eq!(
        (
            case.index.overview().current,
            case.index.overview().affected
        ),
        (before.current, before.affected)
    );
    // A later withdrawal reaches every evidentiary layer; the classification
    // changes are handling metadata and stay out of the Overview.
    case.one(
        "source.withdraw",
        json!({
            "source_ref": f.source, "reason": "retracted_by_origin", "scope": {"extent":"entire"},
            "rationale": "The publisher retracted the benchmark.", "evidence_refs": []
        }),
    );
    let overview = case.index.overview();
    assert_eq!(overview.needs_review, 9, "{overview:#?}");
    assert!(
        overview
            .affected
            .iter()
            .all(|reference| reference.id.kind() != Kind::Annotation || reference.id == f.note.id)
    );
}
