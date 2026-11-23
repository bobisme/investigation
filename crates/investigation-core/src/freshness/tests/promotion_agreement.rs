//! One predicate for "a later revision is benign to a citation of an earlier
//! one" ([`benign_successor`]): the freshness index, the promotion gates
//! (`conclusion.validate`, `decision.accept`) and the `review.stale_read_set`
//! gap rule agree on measurement judgments that keep or lower validity.

use super::source_use::apply_as;
use super::*;
use crate::{
    review::EffectiveDisposition,
    review_ops::{ValidationReviewBody, effective_disposition, validation_review_current},
    wire::ErrorCode,
};

fn judge(case: &mut Case, observation: RevisionRef, validity: &str) -> RevisionRef {
    apply_as(
        case,
        "agent:reviewer",
        vec![op(
            "measurement.assess",
            json!({
                "observation_ref": observation, "channel": null, "validity": validity,
                "verification": [{"kind":"equivalent","justification":"A known reference value was reproduced."}],
                "rationale": "Judged from the recorded setup.", "limits": []
            }),
        )],
    )
    .unwrap()
}

fn attempt(
    case: &mut Case,
    ops: Vec<Value>,
    preconditions: &[RevisionRef],
) -> Result<RevisionRef, Failure> {
    let accepted = apply(&mut case.state, Some(case.case), ops, preconditions)?;
    case.settle();
    Ok(accepted.records[0].reference())
}

/// A candidate conclusion citing `observation` as direct support.
fn candidate(case: &mut Case, observation: RevisionRef) -> RevisionRef {
    case.one(
        "conclusion.create",
        json!({
            "statement":"The managed queue stayed under the latency limit in the benchmark.",
            "scope":"The recorded benchmark.","significance":"Informs the queue choice.",
            "support_refs":[observation],"contradiction_refs":[],"caveats":["One benchmark."]
        }),
    )
}

fn request_review(case: &mut Case, candidate: RevisionRef) -> RevisionRef {
    let mut reads =
        required_read_set(&case.state, case.state.revision(candidate).unwrap()).unwrap();
    reads.push(candidate);
    attempt(
        case,
        vec![op(
            "conclusion.request_review",
            json!({"conclusion_ref":candidate}),
        )],
        &reads,
    )
    .unwrap()
}

/// `conclusion.validate` pinning the current closure, as a caller reads it.
fn promote(
    case: &mut Case,
    ready: RevisionRef,
    review: RevisionRef,
) -> Result<RevisionRef, Failure> {
    let mut reads =
        required_read_set(&case.state, case.state.revision(ready).unwrap()).unwrap_or_default();
    reads.extend([ready, review]);
    attempt(
        case,
        vec![op(
            "conclusion.validate",
            json!({"conclusion_ref":ready,"review_ref":review}),
        )],
        &reads,
    )
}

fn propose(case: &mut Case, f: &Fixture) -> RevisionRef {
    case.one("decision.propose", json!({
        "question_ref":f.question,"choice":{"kind":"select","option_ref":f.option},
        "compared_alternatives":[f.option],"comparison_refs":[f.comparison],"hard_conditions":[],
        "evidence_refs":[f.observation],"assumption_refs":[],"tradeoffs":[],
        "consequences":["Adopt the managed queue."],
        "reopen_trigger":"Latency evidence changes.","reversal_conditions":[]
    }))
}

fn accept(
    case: &mut Case,
    proposed: RevisionRef,
    review: RevisionRef,
) -> Result<RevisionRef, Failure> {
    attempt(
        case,
        vec![op(
            "decision.accept",
            json!({"decision_ref":proposed,"review_ref":review}),
        )],
        &[],
    )
}

/// `review.stale_read_set` gaps and the stale revisions each names.
fn stale_read_sets(case: &Case) -> BTreeMap<Id, Vec<RevisionRef>> {
    let snapshot = crate::wire::Snapshot {
        sequence: case.state.sequence(),
        head_hash: format!("sha256:{}", "0".repeat(64)).parse().unwrap(),
    };
    crate::gap_rules::enumerate_gaps(&case.state, &snapshot, case.case)
        .unwrap()
        .gaps
        .into_iter()
        .filter(|gap| gap.rule_id == "review.stale_read_set")
        .map(|gap| {
            let review = gap.subject_refs[0];
            (review.id, gap.prerequisites[0].subject_refs.clone())
        })
        .collect()
}

/// The gate's verdict on the fixture's stored validation reviews.
fn reviews_current(case: &Case, f: &Fixture) -> [Result<(), Failure>; 2] {
    [
        (f.conclusion, f.conclusion_review),
        (f.decision, f.decision_review),
    ]
    .map(|(subject, review)| {
        let subject = case.state.current(subject.id).unwrap();
        let review: ValidationReviewBody =
            serde_json::from_value(case.state.current(review.id).unwrap().body.clone()).unwrap();
        validation_review_current(&case.state, subject, &review)
    })
}

#[test]
fn a_valid_judgment_after_citation_still_validates_and_accepts() {
    let mut case = Case::new();
    let f = case.fixture();
    // Cited before the judgment: one candidate already reviewed, one not yet.
    let cited = candidate(&mut case, f.observation);
    let reviewed = request_review(&mut case, cited);
    let reviewed_review = case.validate(reviewed);
    let unreviewed = candidate(&mut case, f.observation);
    let proposed = propose(&mut case, &f);
    let decision_review = case.validate(proposed);

    let judged = judge(&mut case, f.observation, "valid");
    assert_eq!(judged.revision, 2);
    assert!(benign_successor(&case.state, case.case, f.observation).is_some());
    assert_eq!(
        successor_change(&case.state, case.case, f.observation),
        None
    );

    // Views: everything that cites revision 1 is current.
    for result in case.index.records() {
        assert_eq!(result.freshness, Freshness::Current, "{result:#?}");
    }
    // Gates: the stored reviews still cover the closure, which now reads the
    // judged revision.
    for verdict in reviews_current(&case, &f) {
        verdict.unwrap();
    }
    let conclusion = case.state.current(f.conclusion.id).unwrap();
    assert!(
        required_read_set(&case.state, conclusion)
            .unwrap()
            .contains(&judged)
    );
    assert_eq!(
        effective_disposition(&case.state, conclusion),
        EffectiveDisposition::Current
    );
    // Gaps: no read set is stale.
    assert!(stale_read_sets(&case).is_empty());

    // A review over revision 1 promotes once the caller pins revision 2.
    let validated = promote(&mut case, reviewed, reviewed_review).unwrap();
    assert_eq!(
        case.state.revision(validated).unwrap().body["status"],
        "validated"
    );
    // A claim still citing revision 1 is reviewed over the judged revision.
    let ready = request_review(&mut case, unreviewed);
    assert_eq!(
        case.state.revision(ready).unwrap().body["support_refs"],
        json!([f.observation])
    );
    let review = case.validate(ready);
    let read: ValidationReviewBody =
        serde_json::from_value(case.state.revision(review).unwrap().body.clone()).unwrap();
    assert!(read.read_set.contains(&judged));
    promote(&mut case, ready, review).unwrap();
    // A decision reviewed over revision 1 is accepted, and its comparison cell
    // still counts as verified.
    let accepted = accept(&mut case, proposed, decision_review).unwrap();
    let accepted = case.state.revision(accepted).unwrap();
    assert_eq!(accepted.body["lifecycle"], "accepted");
    assert_eq!(
        accepted.body["acceptance_challenges"]["uncertain_comparisons"],
        json!([])
    );
    assert!(stale_read_sets(&case).is_empty());
}

#[test]
fn a_lowering_or_invalidating_judgment_blocks_promotion_naming_the_observation() {
    for (validity, trigger, described) in [
        (
            "partial",
            Trigger::ValidityLowered {
                from: MeasurementValidity::Valid,
                to: MeasurementValidity::Partial,
            },
            "measurement validity lowered (valid to partial)",
        ),
        (
            "invalid",
            Trigger::InvalidMeasurement,
            "invalid measurement",
        ),
    ] {
        let mut case = Case::new();
        let f = case.fixture();
        let cited = candidate(&mut case, f.observation);
        let reviewed = request_review(&mut case, cited);
        let reviewed_review = case.validate(reviewed);
        let proposed = propose(&mut case, &f);
        let decision_review = case.validate(proposed);

        let judged = judge(&mut case, f.observation, validity);
        assert!(benign_successor(&case.state, case.case, f.observation).is_none());
        assert_eq!(
            successor_change(&case.state, case.case, f.observation),
            Some(trigger.clone())
        );

        // Views: the fixture's validated claims and reviews need review.
        for reference in [
            f.conclusion,
            f.conclusion_review,
            f.decision,
            f.decision_review,
        ] {
            let reason = only_reason(case.freshness(reference));
            assert_eq!(reason.trigger, trigger, "{validity}");
            assert_eq!(reason.cause, judged);
        }
        // Gates: the stored reviews no longer cover the closure, which itself
        // refuses the changed observation by name.
        for verdict in reviews_current(&case, &f) {
            let failure = verdict.unwrap_err();
            assert_eq!(
                failure.error.code,
                ErrorCode::RevisionConflict,
                "{failure:?}"
            );
            assert!(
                failure
                    .error
                    .message
                    .contains(&f.observation.id.to_string()),
                "{failure:?}"
            );
            assert!(failure.error.message.contains(described), "{failure:?}");
        }
        // Gaps: every review that read revision 1 has a stale read set naming it.
        let stale = stale_read_sets(&case);
        for review in [
            f.conclusion_review,
            f.decision_review,
            reviewed_review,
            decision_review,
        ] {
            assert_eq!(
                stale.get(&review.id),
                Some(&vec![f.observation]),
                "{validity}"
            );
        }

        let before = case.state.sequence();
        for refused in [
            promote(&mut case, reviewed, reviewed_review).unwrap_err(),
            accept(&mut case, proposed, decision_review).unwrap_err(),
        ] {
            assert_eq!(
                refused.error.code,
                ErrorCode::RevisionConflict,
                "{refused:?}"
            );
            assert!(
                refused
                    .error
                    .message
                    .contains(&f.observation.id.to_string()),
                "{refused:?}"
            );
        }
        assert_eq!(case.state.sequence(), before);
    }
}
