//! Rules version 7: resolving a question with a record that rests on its
//! reader is benign to that reader ([`resolution_only`]), while a resolution
//! citing other evidence, or a changed framing, stays a material revision.
//! The index, the promotion gates and the `review.stale_read_set` gap rule
//! agree, and the reader inherits nothing back through the answered question.

use super::promotion_agreement::{reviews_current, stale_read_sets};
use super::*;

fn resolve(case: &mut Case, question: RevisionRef, evidence: &[RevisionRef]) -> RevisionRef {
    case.one(
        "question.resolve",
        json!({
            "question_ref":question,"disposition":"answered",
            "answer":"Adopt the managed queue.","reason":"The accepted decision settles it.",
            "evidence_refs":evidence
        }),
    )
}

fn material(case: &Case, reader: RevisionRef, question: RevisionRef) {
    let result = case.freshness(reader);
    let reason = only_reason(result);
    assert_eq!(reason.trigger, Trigger::MaterialRevision, "{result:#?}");
    assert_eq!(reason.cause, question, "{result:#?}");
}

fn reader(case: &Case, reference: RevisionRef) -> &Record {
    case.state.current(reference.id).unwrap()
}

#[test]
fn a_question_answered_with_its_reader_is_unchanged_to_that_reader() {
    let mut case = Case::new();
    let f = case.fixture();
    let resolved = resolve(&mut case, f.question, &[f.decision]);
    assert_eq!(resolved.revision, 2);

    // The decision answers the question; its review read the question for
    // it; the option and criterion are what the decision rests on.
    for reference in [f.decision, f.decision_review, f.option, f.criterion] {
        let result = case.freshness(reference);
        assert_eq!(result.freshness, Freshness::Current, "{result:#?}");
        assert!(
            benign_successor_for(&case.state, case.case, f.question, reader(&case, reference))
                .is_some_and(|current| current.reference() == resolved)
        );
    }
    // No circular basis: the review inherits nothing back through the
    // question's answer, which rests on the review's own subject.
    for result in case.index.records() {
        assert!(
            result
                .reasons
                .iter()
                .all(|reason| reason.trigger != Trigger::CircularBasis),
            "{result:#?}"
        );
    }
    assert_eq!(
        case.freshness(f.question).freshness,
        Freshness::Current,
        "{:#?}",
        case.freshness(f.question)
    );
    // A reader the answer does not rest on sees new evidence about the
    // question: the hypothesis and the criterion scoped to the other option.
    material(&case, f.hypothesis, resolved);
    material(&case, f.other_criterion, resolved);
    // Without a reader the predicate stays conservative.
    assert_eq!(
        successor_change(&case.state, case.case, f.question),
        Some(Trigger::MaterialRevision)
    );

    // Gates: the decision's stored review still covers its closure, which
    // reads the resolved question. Gaps: no read set is stale.
    let [_, decision] = reviews_current(&case, &f);
    decision.unwrap();
    let decision = reader(&case, f.decision);
    assert!(
        required_read_set(&case.state, decision)
            .unwrap()
            .contains(&resolved)
    );
    assert!(stale_read_sets(&case).is_empty());
}

#[test]
fn a_reviews_subject_counts_as_its_reader_at_any_revision() {
    // Answered with the proposed revision the review judged, which does not
    // cite the review: the review still read the question for that subject.
    let mut case = Case::new();
    let f = case.fixture();
    let proposed = case.state.revision(f.decision).unwrap().header.history[0];
    let proposed = RevisionRef {
        id: f.decision.id,
        revision: proposed.0,
    };
    assert!(case.state.revision(proposed).unwrap().body["review_ref"].is_null());
    resolve(&mut case, f.question, &[proposed]);
    for reference in [f.decision, f.decision_review] {
        let result = case.freshness(reference);
        assert_eq!(result.freshness, Freshness::Current, "{result:#?}");
    }
    assert!(stale_read_sets(&case).is_empty());
}

#[test]
fn a_resolution_citing_other_evidence_is_material_to_every_reader() {
    let mut case = Case::new();
    let f = case.fixture();
    let resolved = resolve(&mut case, f.question, &[f.decision, f.observation]);

    // The observation is not downstream of the decision.
    material(&case, f.decision, resolved);
    let result = case.freshness(f.decision_review);
    assert!(
        result.reasons.iter().any(|reason| {
            reason.trigger == Trigger::MaterialRevision && reason.cause == resolved
        }),
        "{result:#?}"
    );
    assert!(
        benign_successor_for(
            &case.state,
            case.case,
            f.question,
            reader(&case, f.decision_review)
        )
        .is_none()
    );
    assert!(stale_read_sets(&case).contains_key(&f.decision_review.id));
    let [_, decision] = reviews_current(&case, &f);
    assert!(decision.is_err());
}

#[test]
fn a_changed_framing_is_material_even_when_answered_with_the_reader() {
    for (field, value) in [
        ("wording", json!("Which queue should carry the batch jobs?")),
        ("scope", json!({"state":"known","value":"Batch jobs only"})),
        ("importance", json!("high")),
    ] {
        let mut case = Case::new();
        let f = case.fixture();
        let mut body = case.state.revision(f.question).unwrap().body.clone();
        body[field] = value;
        body["disposition"] = json!("answered");
        body["answer"] = json!("Adopt the managed queue.");
        body["resolution_reason"] = json!("The accepted decision settles it.");
        body["evidence_refs"] = json!([f.decision]);
        let changed = case.image(Kind::Question, body, Some(f.question));
        material(&case, f.decision, changed);
        assert!(
            successor_change_for(
                &case.state,
                case.case,
                f.question,
                reader(&case, f.decision)
            ) == Some(Trigger::MaterialRevision),
            "{field}"
        );
    }
}
