//! `artifact.expire` through the real operation: one reference loses its
//! availability, its record and history stay, and the dependents the preview
//! predicts are exactly those the freshness index then marks.

use super::*;
use crate::retention_ops::{self, LossOutcome};

fn expire(artifact: RevisionRef) -> Value {
    json!({
        "artifact_ref": artifact, "reason": "retention_period_ended",
        "rationale": "The case keeps raw captures for 90 days."
    })
}

fn marked(case: &Case, artifact: Id) -> Vec<Id> {
    let mut ids: Vec<Id> = case
        .index
        .records()
        .filter(|result| {
            result.reasons.iter().any(|reason| {
                reason.trigger == Trigger::AvailabilityRemoved && reason.cause.id == artifact
            })
        })
        .map(|result| result.record.id)
        .collect();
    ids.sort();
    ids
}

#[test]
fn expiring_a_reference_keeps_its_record_and_marks_exactly_the_previewed_dependents() {
    let mut case = Case::new();
    let f = case.fixture();
    let before = case.state.revision(f.artifact).unwrap().body.clone();
    let earlier = case.state.sequence();

    let preview = retention_ops::preview(&case.state, case.case, &[f.artifact.id]);
    assert_eq!(preview.len(), 1);
    let loss = &preview[0];
    assert_eq!(loss.outcome, LossOutcome::BecomesUnavailable);
    assert_eq!(loss.artifact_ref, f.artifact);
    assert_eq!(json!(loss.digest), before["digest"]);
    assert_eq!(json!(loss.size_bytes), before["size_bytes"]);
    let previewed: Vec<Id> = loss.dependents.iter().map(|d| d.record.id).collect();
    for id in [
        f.observation.id,
        f.assessment.id,
        f.conclusion.id,
        f.decision.id,
    ] {
        assert!(previewed.contains(&id), "{id} missing from {previewed:?}");
    }
    assert!(
        loss.dependents
            .iter()
            .all(|d| d.freshness_after == Freshness::NeedsReview)
    );
    // The preview ran the operation on a copy: nothing changed.
    assert_eq!(
        case.state.current(f.artifact.id).unwrap().reference(),
        f.artifact
    );
    assert!(marked(&case, f.artifact.id).is_empty());

    let expired = case.one("artifact.expire", expire(f.artifact));
    assert_eq!(expired.revision, f.artifact.revision + 1);
    let body = &case.state.current(f.artifact.id).unwrap().body;
    assert_eq!(body["availability"], "intentionally_removed");
    for key in [
        "digest",
        "size_bytes",
        "format",
        "source",
        "transforms",
        "integrity",
        "sensitivity",
    ] {
        assert_eq!(body[key], before[key], "{key} changed");
    }
    let change = &body["availability_change"];
    assert_eq!(change["actor"], "agent:test");
    assert_eq!(change["revision"], f.artifact.revision);
    assert_eq!(change["from"], "retained");
    assert_eq!(change["reason"], "retention_period_ended");
    // History is not rewritten: the cited revision is still readable as recorded.
    assert_eq!(case.state.revision(f.artifact).unwrap().body, before);

    // Exactly the previewed records now need review for the lost availability.
    assert_eq!(marked(&case, f.artifact.id), previewed);
    for id in &previewed {
        assert_eq!(
            case.index.get(*id).unwrap().freshness,
            Freshness::NeedsReview
        );
    }

    // Every read follows today's availability, also at an earlier snapshot.
    let historical = case.state.as_of(earlier);
    assert!(!retention_ops::availability_withdrawn(
        &historical,
        f.artifact.id
    ));
    assert!(retention_ops::bytes_withdrawn(
        &historical,
        Some(&case.state),
        f.artifact.id
    ));
    assert!(!retention_ops::bytes_withdrawn(
        &historical,
        None,
        f.artifact.id
    ));

    // A withdrawn reference cannot be withdrawn again, and the preview says so.
    let again = super::source_use::apply_as(
        &mut case,
        "agent:test",
        vec![op("artifact.expire", expire(expired))],
    )
    .unwrap_err();
    assert_eq!(again.error.code, ErrorCode::GateFailed);
    let preview = retention_ops::preview(&case.state, case.case, &[f.artifact.id]);
    assert_eq!(preview[0].outcome, LossOutcome::AlreadyUnavailable);
    assert!(preview[0].dependents.is_empty());
}

#[test]
fn a_stale_revision_is_refused_and_the_preview_reports_the_commit_refusal() {
    let mut case = Case::new();
    let f = case.fixture();
    // A use-only revision moves the Artifact on; the stale citation conflicts.
    let withdrawn = case.one(
        "artifact.withdraw",
        json!({
            "artifact_ref": f.artifact, "reason": "collection_error",
            "scope": {"extent": "partial", "statement": "The warm-up lines."},
            "rationale": "Warm-up lines were captured by mistake.", "evidence_refs": []
        }),
    );
    let stale = super::source_use::apply_as(
        &mut case,
        "agent:test",
        vec![op("artifact.expire", expire(f.artifact))],
    )
    .unwrap_err();
    assert_eq!(stale.error.code, ErrorCode::RevisionConflict);
    // An Artifact withdrawn from evidentiary use can still lose availability.
    let preview = retention_ops::preview(&case.state, case.case, &[f.artifact.id]);
    assert_eq!(preview[0].artifact_ref, withdrawn);
    assert_eq!(preview[0].outcome, LossOutcome::BecomesUnavailable);
    // A non-Artifact is not a reference: the shape check refuses it.
    let wrong = super::source_use::apply_as(
        &mut case,
        "agent:test",
        vec![op("artifact.expire", expire(f.source))],
    )
    .unwrap_err();
    assert_eq!(wrong.error.code, ErrorCode::InvalidInput);
}
