//! Blindspot reviews go stale through the same index as everything else: when
//! a record they read changes materially, or when the check set of the audit
//! they reviewed is no longer the registered one. Pack policy reads the same
//! predicate.

use super::*;
use crate::{
    blindspot::{AuditRequest, audit},
    blindspot_review::{current_blindspot_reviews, current_check_set_digest},
    wire::Snapshot,
};

fn verified(case: &mut Case) -> Snapshot {
    let snapshot = Snapshot {
        sequence: case.state.sequence(),
        head_hash: format!("sha256:{:064x}", case.state.sequence() + 11)
            .parse()
            .unwrap(),
    };
    case.state.note_verified_snapshot(snapshot.clone()).unwrap();
    snapshot
}

fn blindspot(case: &mut Case, subject: RevisionRef, read_set: Value) -> RevisionRef {
    let snapshot = verified(case);
    let response = audit(&case.state, &snapshot, case.case, &AuditRequest::default()).unwrap();
    case.one("review.submit", json!({
        "kind":"blindspot","subject_ref":subject,"audited_snapshot":snapshot,"read_set":read_set,
        "method":"Look for unrecorded mechanisms and measurement failures.",
        "scope_examined":"The validated conclusion and the benchmark it cites.",
        "checks_considered":["instrument limits"],"gaps":["The benchmark host was shared."],
        "no_gap_reason":null,"evidence_refs":[],"assumption_refs":[],
        "limitations":["Read recorded material only."],"verdict":"gaps_identified",
        "context_isolation":"fresh_context_unblinded","context":{
            "saw_original_conclusion":{"state":"known","value":true},
            "gathered_new_evidence":{"state":"known","value":false},
            "method_or_source_changes":[],"activity":"Challenge the recorded claim.",
            "dependencies":"shared","dependence_basis":"Same recorded data."},
        "audit":{"check_version":response.check_version,"check_set_digest":response.check_set_digest}
    }))
}

fn current_reviews(case: &Case, subject: RevisionRef) -> Vec<RevisionRef> {
    current_blindspot_reviews(&case.state, case.state.current(subject.id).unwrap()).unwrap()
}

#[test]
fn withdrawing_a_source_the_review_read_makes_it_stale_and_an_unrelated_write_does_not() {
    let mut case = Case::new();
    let f = case.fixture();
    let review = blindspot(
        &mut case,
        f.conclusion,
        json!([f.source, f.artifact, f.observation]),
    );
    assert_eq!(case.freshness(review).label(), "submitted / current");
    assert_eq!(current_reviews(&case, f.conclusion), [review]);

    // Unrelated writes, including a new revision of an unread record.
    case.one(
        "question.create",
        json!({"importance":"normal","wording":"Does the broker need a second region?"}),
    );
    case.one(
        "option.disposition",
        json!({"option_ref":case.current(f.option),"lifecycle":"eligible","reason":"Still feasible."}),
    );
    assert_eq!(case.freshness(review).freshness, Freshness::Current);
    assert_eq!(current_reviews(&case, f.conclusion), [review]);

    case.one(
        "source.withdraw",
        json!({
            "source_ref": f.source, "reason": "retracted_by_origin", "scope": {"extent":"entire"},
            "rationale": "The publisher retracted the benchmark report.", "evidence_refs": []
        }),
    );
    let result = case.freshness(review);
    assert_eq!(result.freshness, Freshness::NeedsReview, "{result:#?}");
    assert!(
        result
            .reasons
            .iter()
            .any(|reason| reason.trigger == Trigger::Withdrawn && reason.cause.id == f.source.id),
        "{result:#?}"
    );
    // Pack policy reads the same judgment: the review no longer counts.
    assert!(current_reviews(&case, f.conclusion).is_empty());
    // The review itself is never rewritten.
    assert_eq!(case.current(review), review);
}

#[test]
fn a_changed_check_set_makes_an_audit_review_stale() {
    let mut case = Case::new();
    let f = case.fixture();
    let review = blindspot(&mut case, f.conclusion, json!([f.observation]));
    let mut body = case.state.revision(review).unwrap().body.clone();
    assert_eq!(
        body["audit"]["check_set_digest"],
        json!(current_check_set_digest())
    );
    // A review recorded under an earlier check registry.
    body["audit"]["check_set_digest"] = json!(format!("sha256:{}", "a".repeat(64)));
    let older = case.image(Kind::Review, body, None);
    let reason = only_reason(case.freshness(older));
    assert_eq!(reason.trigger, Trigger::CheckSetChanged);
    assert_eq!(reason.cause, older);
    assert_eq!(case.freshness(older).freshness, Freshness::NeedsReview);
    assert_eq!(current_reviews(&case, f.conclusion), [review]);
    // Nothing relies on a review's freshness through inheritance.
    assert_eq!(case.freshness(f.conclusion).freshness, Freshness::Current);
}
