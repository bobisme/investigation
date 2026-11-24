//! A hard criterion added late, end to end through the public binary
//! (bn-dla3; plan §20.2 P6).
//!
//! Before promotion a new hard criterion for the Decision's question is a
//! phantom in the reviewed set: `decision.accept` is refused with a repair
//! and commits nothing. After acceptance the same change turns the accepted
//! Decision `needs_review`, naming the active hard criteria membership as
//! the trigger, and `brief` counts it; a criterion of another question
//! changes nothing, and retiring the late one restores the reviewed set.
//! A material Objection blocks acceptance the same way, and raised after
//! acceptance it marks the Decision `disputed` (objection_raised_after_review).
//! At every step `show` gives the same standing before and after
//! `repair index` rebuilds the disposable index from the journal, so the
//! incrementally maintained index and a full rebuild agree.

#[path = "support/choice.rs"]
mod choice;

use choice::{Choice, hard_criterion, reference, run};
use serde_json::{Value, json};

/// The Decision's standing, which a rebuilt index must give unchanged.
fn standing_agrees(choice: &Choice, step: &str) -> (Value, Value) {
    let incremental = choice.standing();
    let (exit, rebuilt) = run(&choice.ws, &["repair", "index"], None);
    assert_eq!(exit, 0, "{step}: {rebuilt:#}");
    assert_eq!(choice.standing(), incremental, "{step}");
    incremental
}

fn snapshot(choice: &Choice) -> Value {
    let (exit, brief) = run(
        &choice.ws,
        &["--case", &choice.case, "brief", "--level", "standard"],
        None,
    );
    assert_eq!(exit, 0, "{brief:#}");
    json!([
        brief["snapshot"],
        brief["freshness"]["overview"]["needs_review"]
    ])
}

#[test]
fn a_hard_criterion_added_late_blocks_acceptance_then_turns_the_accepted_decision_needs_review() {
    let directory = tempfile::tempdir().unwrap();
    let choice = Choice::new(directory.path(), "late");

    // Before promotion: the late criterion is a phantom in the reviewed set.
    let phantom = choice.create(
        "criterion.create",
        hard_criterion(&choice.question, "Names an owner for incidents"),
    );
    let before = snapshot(&choice);
    let (exit, refused) = choice.record(&choice.accept("accept-with-phantom"));
    assert_ne!(exit, 0, "{refused:#}");
    assert_eq!(refused["commit_status"], "not_committed", "{refused:#}");
    assert!(refused["error"]["repair"].is_object(), "{refused:#}");
    assert_eq!(snapshot(&choice), before, "a refusal commits nothing");
    choice.write(
        "criterion.retire",
        json!({"criterion_ref":reference(&phantom, 1),"reason":"The phantom check is complete"}),
    );
    let accepted = choice.record(&choice.accept("accept"));
    assert_eq!(accepted.0, 0, "{:#}", accepted.1);
    let (status, freshness) = standing_agrees(&choice, "accepted");
    assert_eq!(status["recorded"], "accepted");
    assert_eq!(freshness["effective"], "current", "{freshness:#}");

    // A hard criterion of another question is outside the reviewed set.
    let other = choice.create(
        "question.create",
        json!({"wording":"Who runs the store?","importance":"low","context":"Operations"}),
    );
    choice.create(
        "criterion.create",
        hard_criterion(&other, "Has an on-call rota"),
    );
    let (_, freshness) = standing_agrees(&choice, "other question");
    assert_eq!(freshness["effective"], "current", "{freshness:#}");

    // A new hard criterion of the Decision's question after acceptance.
    let late = choice.create(
        "criterion.create",
        hard_criterion(&choice.question, "Replicates to a second site"),
    );
    let (status, freshness) = standing_agrees(&choice, "late criterion");
    assert_eq!(status["recorded"], "accepted", "{status:#}");
    assert_eq!(status["state"], "needs_review", "{status:#}");
    assert_eq!(freshness["effective"], "needs_review", "{freshness:#}");
    let triggers: Vec<&Value> = freshness["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reason| &reason["trigger"])
        .collect();
    assert_eq!(
        triggers,
        [&json!({"kind":"membership_changed","rule":"active_hard_criteria"})],
        "{freshness:#}"
    );
    assert!(
        snapshot(&choice)[1].as_u64().unwrap() >= 1,
        "brief counts the Decision"
    );

    // Retiring it restores the reviewed membership.
    choice.write(
        "criterion.retire",
        json!({"criterion_ref":reference(&late, 1),"reason":"Replication is out of scope"}),
    );
    let (status, freshness) = standing_agrees(&choice, "retired");
    assert_eq!(status["recorded"], "accepted", "{status:#}");
    assert_ne!(status["state"], "needs_review", "{status:#}");
    assert_eq!(freshness["effective"], "current", "{freshness:#}");
}

/// The same for a material Objection: open against the proposed Decision
/// it blocks acceptance; raised against the accepted Decision it turns it
/// `disputed`, and withdrawing it restores the standing.
#[test]
fn a_material_objection_blocks_acceptance_then_disputes_the_accepted_decision() {
    let directory = tempfile::tempdir().unwrap();
    let choice = Choice::new(directory.path(), "objection");
    let objection = |revision: u64| {
        json!({"subject_ref":reference(&choice.decision, revision),
            "statement":"The unknown cells hide a known data-loss defect of the current store.",
            "scope":"The comparison of the current store.","severity":"material",
            "evidence_refs":{"state":"unknown"}})
    };
    let early = choice.create("objection.raise", objection(1));
    let before = snapshot(&choice);
    let (exit, refused) = choice.record(&choice.accept("accept-under-objection"));
    assert_ne!(exit, 0, "{refused:#}");
    assert_eq!(refused["commit_status"], "not_committed", "{refused:#}");
    assert_eq!(snapshot(&choice), before, "a refusal commits nothing");
    choice.write(
        "objection.withdraw",
        json!({"objection_ref":reference(&early, 1),"reason":"Raised against the wrong store","evidence_refs":[]}),
    );
    let accepted = choice.record(&choice.accept("accept"));
    assert_eq!(accepted.0, 0, "{:#}", accepted.1);
    let (_, freshness) = standing_agrees(&choice, "accepted");
    assert_eq!(freshness["effective"], "current", "{freshness:#}");

    let late = choice.create("objection.raise", objection(2));
    let (status, freshness) = standing_agrees(&choice, "late objection");
    assert_eq!(status["recorded"], "accepted", "{status:#}");
    // The product marks an Objection after review `disputed`, a stronger
    // state than `needs_review`, and names the trigger.
    assert_eq!(status["state"], "disputed", "{status:#}");
    assert_eq!(freshness["effective"], "disputed", "{freshness:#}");
    assert!(
        freshness["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason["trigger"]["kind"] == "objection_raised_after_review"),
        "{freshness:#}"
    );
    choice.write(
        "objection.withdraw",
        json!({"objection_ref":reference(&late, 1),"reason":"The defect was in another version","evidence_refs":[]}),
    );
    let (_, freshness) = standing_agrees(&choice, "withdrawn");
    assert_eq!(freshness["effective"], "current", "{freshness:#}");
}
