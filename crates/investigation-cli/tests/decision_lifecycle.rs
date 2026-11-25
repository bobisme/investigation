//! Replacing a proposed Decision, choosing an option that is not yet
//! eligible, and the accept receipt's uncertain comparisons (bn-30vf, DF.07).
//!
//! A proposal is replaced with `decision.supersede`, as an accepted Decision
//! is: the old one is kept, marked superseded, with the reason and a link to
//! its successor, and a `defer` replacement withdraws the choice. Proposing a
//! choice that is still `proposed` warns, and `decision.accept` refuses it
//! with the order that makes it choosable. Accepting over uncertain
//! comparisons gives one warning that explains them, not one per comparison.

#[path = "support/choice.rs"]
mod choice;

use choice::{ACTOR, Choice, REVIEWER, envelope, reference, run};
use serde_json::{Value, json};

fn warnings(receipt: &Value) -> Vec<String> {
    receipt["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning.as_str().unwrap().to_owned())
        .collect()
}

fn supersede(choice: &Choice, key: &str, decision: Value, replacement: Value) -> (i32, Value) {
    choice.record(&envelope(
        key,
        Some(&choice.case),
        ACTOR,
        "decision.supersede",
        json!({"decision_ref":decision,"reason":"The comparisons were taken too early.",
            "replacement":replacement}),
    ))
}

fn defer(choice: &Choice) -> Value {
    json!({"question_ref":reference(&choice.question, 1),
        "choice":{"kind":"defer","reason":"Measure both stores first"},
        "compared_alternatives":choice.options,"comparison_refs":[],"hard_conditions":[],
        "evidence_refs":[],"assumption_refs":[],"tradeoffs":["Nothing is chosen yet"],
        "consequences":["Keep the current store until a trial"],"reopen_trigger":"A trial completes",
        "expected_outcome":null,"check_when":"After a trial","reversal_conditions":[]})
}

#[test]
fn a_proposed_decision_is_replaced_with_a_reason_and_reads_show_only_the_live_one() {
    let root = tempfile::tempdir().unwrap();
    let choice = Choice::new(root.path(), "replace");
    let (exit, receipt) = supersede(
        &choice,
        "replace-1",
        reference(&choice.decision, 1),
        defer(&choice),
    );
    assert_eq!(exit, 0, "{receipt:#}");
    let successor = receipt["created"][0]["id"].as_str().unwrap().to_owned();

    let old = choice.show(&choice.decision);
    assert_eq!(old["record"]["body"]["lifecycle"], "superseded", "{old:#}");
    assert_eq!(
        old["record"]["body"]["superseded_by"]["id"],
        successor.as_str()
    );
    assert_eq!(
        old["record"]["body"]["change_reason"],
        "The comparisons were taken too early."
    );
    let new = choice.show(&successor);
    assert_eq!(new["record"]["body"]["lifecycle"], "proposed", "{new:#}");
    assert_eq!(
        new["record"]["body"]["supersedes"]["id"],
        choice.decision.as_str()
    );

    // brief leads with the live proposal; the replaced one follows, marked
    // superseded, and `next` proposes no work on it.
    let (exit, brief) = run(&choice.ws, &["--case", &choice.case, "brief"], None);
    assert_eq!(exit, 0, "{brief:#}");
    let outcomes = brief["outcomes"].as_array().unwrap();
    assert_eq!(outcomes[0]["id"], successor.as_str(), "{brief:#}");
    let old_outcome = outcomes
        .iter()
        .find(|outcome| outcome["id"] == choice.decision.as_str())
        .unwrap_or_else(|| panic!("{brief:#}"));
    assert!(old_outcome.to_string().contains("superseded"), "{brief:#}");
    let (exit, next) = run(&choice.ws, &["--case", &choice.case, "next"], None);
    assert_eq!(exit, 0, "{next:#}");
    assert!(!next.to_string().contains(&choice.decision), "{next:#}");

    // A superseded proposal is not replaced twice.
    let (exit, refusal) = supersede(
        &choice,
        "replace-2",
        reference(&choice.decision, 2),
        defer(&choice),
    );
    assert_eq!(exit, 5, "{refusal:#}");
    assert!(
        refusal["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Supersede its successor"),
        "{refusal:#}"
    );
}

#[test]
fn a_choice_still_proposed_warns_at_propose_and_the_accept_refusal_gives_the_order() {
    let root = tempfile::tempdir().unwrap();
    let choice = Choice::new(root.path(), "eligible");
    let option = choice.create(
        "option.create",
        json!({"question_ref":reference(&choice.question, 1),"description":"Adopt a third store",
            "dependencies":[],"tradeoff_notes":[]}),
    );
    let comparison = choice.create(
        "comparison.add",
        json!({"option_ref":reference(&option, 1),"criterion_ref":reference(&choice.criterion, 1),
            "outcome":"unknown","method":"Not measured","value":null,"evidence_refs":[],
            "assumption_refs":[],"uncertainty":"No trial","rationale":"No evidence yet"}),
    );
    let mut alternatives = choice.options.clone();
    alternatives.push(reference(&option, 1));
    let receipt = choice.write(
        "decision.propose",
        json!({"question_ref":reference(&choice.question, 1),
            "choice":{"kind":"select","option_ref":reference(&option, 1)},
            "compared_alternatives":alternatives,"comparison_refs":[reference(&comparison, 1)],
            "hard_conditions":[{"criterion_ref":reference(&choice.criterion, 1),
                "reason":"Verify that no acknowledged write is lost"}],
            "evidence_refs":[],"assumption_refs":[],"tradeoffs":["Nothing is measured yet"],
            "consequences":["Adopt the third store pending a check"],"reopen_trigger":"A trial loses a write",
            "expected_outcome":null,"check_when":"After a trial","reversal_conditions":[]}),
    );
    let decision = receipt["created"][0]["id"].as_str().unwrap().to_owned();
    let warned = warnings(&receipt);
    let eligibility: Vec<&String> = warned
        .iter()
        .filter(|warning| warning.contains("decision.accept refuses a choice that is not eligible"))
        .collect();
    assert_eq!(eligibility.len(), 1, "{receipt:#}");
    assert!(eligibility[0].contains(&option), "{receipt:#}");
    assert!(
        eligibility[0].contains("option.disposition to eligible, then comparison.revise"),
        "{receipt:#}"
    );
    // The fixture's own proposal for the question is still live: named once.
    assert!(
        warned
            .iter()
            .any(|warning| warning.contains(&choice.decision)
                && warning.contains("decision.supersede")),
        "{receipt:#}"
    );

    let basis = choice.show(&decision)["review_basis"].clone();
    let (exit, review) = choice.record(&envelope(
        "eligible-review",
        Some(&choice.case),
        REVIEWER,
        "review.submit",
        json!({"kind":"validation","subject_ref":reference(&decision, 1),
            "basis_digest":basis["basis_digest"],"read_set":basis["read_set"],
            "method":"Read the comparisons","materials_examined":[],"shared_dependencies":[],
            "objection_refs":[],"verdict":"supports","caveats":["Nothing is measured yet"],
            "context_isolation":"fresh_context_unblinded","context":{
                "saw_original_conclusion":{"state":"unknown"},"gathered_new_evidence":{"state":"unknown"},
                "method_or_source_changes":[],"activity":"Review the choice","dependencies":"unknown",
                "dependence_basis":"No evidence source"}}),
    ));
    assert_eq!(exit, 0, "{review:#}");
    let review = review["created"][0]["id"].as_str().unwrap().to_owned();
    let (exit, refusal) = choice.record(&envelope(
        "eligible-accept",
        Some(&choice.case),
        ACTOR,
        "decision.accept",
        json!({"decision_ref":reference(&decision, 1),"review_ref":reference(&review, 1)}),
    ));
    assert_eq!(exit, 5, "{refusal:#}");
    let error = &refusal["error"];
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("{option} is proposed")),
        "{refusal:#}"
    );
    assert!(
        message.contains("then decision.supersede this proposal"),
        "{refusal:#}"
    );
    let argv: Vec<&str> = error["repair"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        argv.ends_with(&["schema", "option.disposition", "--format", "json"]),
        "{refusal:#}"
    );
    assert_eq!(error["actual"]["lifecycle"], "proposed", "{refusal:#}");
}

#[test]
fn accepting_over_uncertain_comparisons_gives_one_warning_that_explains_them() {
    let root = tempfile::tempdir().unwrap();
    let choice = Choice::new(root.path(), "uncertain");
    let (exit, receipt) = choice.record(&choice.accept("uncertain-accept"));
    assert_eq!(exit, 0, "{receipt:#}");
    let uncertain: Vec<String> = warnings(&receipt)
        .into_iter()
        .filter(|warning| warning.contains("uncertain comparison"))
        .collect();
    assert_eq!(uncertain.len(), 1, "{receipt:#}");
    assert!(
        uncertain[0].contains("cites 2 uncertain comparison(s)"),
        "{receipt:#}"
    );
    assert!(uncertain[0].contains("outcome is unknown"), "{receipt:#}");
    for comparison in &choice.comparisons {
        assert!(uncertain[0].contains(comparison.as_str()), "{receipt:#}");
    }
}

fn validation(choice: &Choice, decision: &str, read_set: Value, objections: Value) -> Value {
    let basis = choice.show(decision)["review_basis"].clone();
    json!({"kind":"validation","subject_ref":reference(decision, 1),
        "basis_digest":basis["basis_digest"],"read_set":read_set,
        "method":"Read the comparisons","materials_examined":[],"shared_dependencies":[],
        "objection_refs":objections,"verdict":"supports","caveats":["Nothing is measured yet"],
        "context_isolation":"fresh_context_unblinded","context":{
            "saw_original_conclusion":{"state":"unknown"},"gathered_new_evidence":{"state":"unknown"},
            "method_or_source_changes":[],"activity":"Review the choice","dependencies":"unknown",
            "dependence_basis":"No evidence source"}})
}

/// A record the reviewer cites but left out of read_set is named; a missing
/// member of the dependency closure is not: the repair names the read that
/// lists it (ADR 0018).
#[test]
fn the_read_set_refusal_names_the_cited_records_it_lacks_and_no_closure_member() {
    let root = tempfile::tempdir().unwrap();
    let choice = Choice::new(root.path(), "read-set");
    let objection = choice.create(
        "objection.raise",
        json!({"subject_ref":reference(&choice.decision, 1),
            "statement":"The unknown cells hide a known data-loss defect.",
            "scope":"The comparison of the current store.","severity":"material",
            "evidence_refs":{"state":"unknown"}}),
    );
    let basis = choice.show(&choice.decision)["review_basis"].clone();
    let closure: Vec<Value> = basis["read_set"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|reference| reference["id"] != objection.as_str())
        .cloned()
        .collect();
    let submit = |key: &str, read_set: Value| {
        choice.record(&envelope(
            key,
            Some(&choice.case),
            REVIEWER,
            "review.submit",
            validation(
                &choice,
                &choice.decision,
                read_set,
                json!([reference(&objection, 1)]),
            ),
        ))
    };
    let (exit, refusal) = submit("read-set-1", json!(closure));
    assert_eq!(exit, 5, "{refusal:#}");
    assert_eq!(refusal["error"]["path"], "/operations/0/payload/read_set");
    assert_eq!(
        refusal["error"]["actual"]["records"],
        json!([objection]),
        "{refusal:#}"
    );
    assert!(
        refusal["error"]["message"]
            .as_str()
            .unwrap()
            .contains("objection_refs, materials_examined"),
        "{refusal:#}"
    );

    // Leave out a closure member as well: the refusal names the cited
    // objection, not the closure member, and points at review-packet.
    let mut partial = closure.clone();
    let dropped = partial.pop().unwrap();
    partial.push(reference(&objection, 1));
    let (exit, refusal) = submit("read-set-2", json!(partial));
    assert_eq!(exit, 5, "{refusal:#}");
    assert!(
        !refusal["error"]
            .to_string()
            .contains(dropped["id"].as_str().unwrap()),
        "{refusal:#}"
    );
    assert!(refusal["error"]["actual"].is_null(), "{refusal:#}");
    let argv = refusal["error"]["repair"]["argv"].to_string();
    assert!(argv.contains("review-packet"), "{refusal:#}");
}

/// The decision record leads with the accepted Decision, whatever its age,
/// and a proposal that was never replaced follows it.
#[test]
fn the_decision_record_leads_with_the_accepted_decision() {
    let root = tempfile::tempdir().unwrap();
    let choice = Choice::new(root.path(), "record");
    let comparisons: Vec<Value> = choice
        .comparisons
        .iter()
        .map(|id| reference(id, 1))
        .collect();
    let later = choice.create(
        "decision.propose",
        json!({"question_ref":reference(&choice.question, 1),
            "choice":{"kind":"select","option_ref":choice.options[1]},
            "compared_alternatives":choice.options,"comparison_refs":comparisons,
            "hard_conditions":[{"criterion_ref":reference(&choice.criterion, 1),
                "reason":"Verify that no acknowledged write is lost"}],
            "evidence_refs":[],"assumption_refs":[],"tradeoffs":["Nothing is measured yet"],
            "consequences":["Adopt the new store pending a check"],"reopen_trigger":"A trial loses a write",
            "expected_outcome":"No lost writes","check_when":"After a trial","reversal_conditions":[]}),
    );
    let read_set = choice.show(&later)["review_basis"]["read_set"].clone();
    let (exit, review) = choice.record(&envelope(
        "record-review",
        Some(&choice.case),
        REVIEWER,
        "review.submit",
        validation(&choice, &later, read_set, json!([])),
    ));
    assert_eq!(exit, 0, "{review:#}");
    let review = review["created"][0]["id"].as_str().unwrap().to_owned();
    let (exit, accepted) = choice.record(&envelope(
        "record-accept",
        Some(&choice.case),
        ACTOR,
        "decision.accept",
        json!({"decision_ref":reference(&later, 1),"review_ref":reference(&review, 1)}),
    ));
    assert_eq!(exit, 0, "{accepted:#}");

    let out = root.path().join("record-export");
    let (exit, exported) = run(
        &choice.ws,
        &[
            "--case",
            &choice.case,
            "export",
            "--kind",
            "decision-record",
            "--audience",
            "internal",
            "--out",
            out.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(exit, 0, "{exported:#}");
    let text = std::fs::read_to_string(out.join("decision-record.md")).unwrap();
    let first = text.find("## Decision 1: ").unwrap();
    let second = text.find("## Decision 2: ").unwrap();
    let accepted_at = text.find(&later).unwrap();
    let proposed_at = text.find(&choice.decision).unwrap();
    assert!(first < accepted_at && accepted_at < second, "{text}");
    assert!(second < proposed_at, "{text}");
}

/// What a blinded Decision packet supports, and review.submit's read_set and
/// materials_examined rules, are stated where a reviewer looks.
#[test]
fn packet_help_and_the_review_schema_state_what_a_reviewer_must_cite() {
    let root = tempfile::tempdir().unwrap();
    let ws = choice::workspace(root.path(), "help");
    let help = choice::command(&ws, &["review-packet", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("omits the choice"), "{help}");
    assert!(help.contains("Judging the"), "{help}");
    assert!(help.contains("choice itself needs the Decision"), "{help}");
    let (exit, schema) = run(&ws, &["schema", "review.submit"], None);
    assert_eq!(exit, 0, "{schema:#}");
    let text = schema.to_string();
    assert!(
        text.contains("plus each record cited in `materials_examined` and\\n`objection_refs`"),
        "{schema:#}"
    );
    assert!(
        text.contains("Observations, Assessments, Artifacts, Sources, Runs"),
        "{schema:#}"
    );
}
