//! Refusals name what the failing field needs, and one attempt reports every
//! refusal it can (bn-29t7, DF.02; dogfood F4, F6, F7, F9, F12, F13, F14).
//!
//! - A schema mismatch names the enum values or the nested `$defs` type, the
//!   required fields the value lacks, and one example.
//! - A bare blocker code is followed by what the payload needs.
//! - A reference of the wrong kind lists the kinds the field accepts.
//! - A gate that depends on another field names that field.
//! - A missing case precondition is at `/preconditions`, with its shape and a
//!   read that gives the revision.
//! - `record --dry-run` reports every refused operation, not the first; an
//!   operation that depends on a refused one's alias is not reported again.
//! - A repair schema the operation set for another operation is kept.
//! - A warning several operations repeat is given once.

#[path = "support/choice.rs"]
mod choice;

use choice::{ACTOR, Choice, reference, run};
use serde_json::{Value, json};

fn submit(choice: &Choice, ops: Value, preconditions: Value, dry: bool) -> (i32, Value) {
    let bytes = serde_json::to_vec(&json!({
        "schema_version":"inv.command.v1","command_id":format!("k-{}", rand_key(&ops)),
        "case_id":choice.case,"actor":ACTOR,"preconditions":preconditions,"operations":ops,
    }))
    .unwrap();
    let mut args = vec!["record", "--input", "-"];
    if dry {
        args.push("--dry-run");
    }
    run(&choice.ws, &args, Some(&bytes))
}

/// A command ID from the operations, so each distinct request has its own.
fn rand_key(ops: &Value) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ops.to_string().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn message(failure: &Value) -> &str {
    failure["error"]["message"].as_str().unwrap()
}

fn case_ref(choice: &Choice) -> Value {
    let shown = choice.show(&choice.case);
    reference(
        &choice.case,
        shown["record"]["header"]["revision"].as_u64().unwrap(),
    )
}

#[test]
fn a_schema_mismatch_names_the_enum_or_the_nested_type_and_an_example() {
    let root = tempfile::tempdir().unwrap();
    let choice = Choice::new(root.path(), "shape");
    let (exit, refused) = submit(
        &choice,
        json!([{"op":"assumption.create","payload":{"claim":"c","consequence":"d",
            "materiality":"high","affected_refs":[]}}]),
        json!([]),
        false,
    );
    assert_eq!(exit, 2, "{refused:#}");
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/payload/materiality"
    );
    assert!(
        message(&refused).contains("expected one of material, nonmaterial, unknown"),
        "{refused:#}"
    );
    assert_eq!(
        refused["error"]["expected"]["enum"],
        json!(["material", "nonmaterial", "unknown"])
    );
    assert_eq!(refused["error"]["expected"]["example"], "material");

    let (exit, refused) = submit(
        &choice,
        json!([{"op":"case.amend","payload":{"reason":"Name the stop",
            "stop_condition":{"state":"known","value":"When the queue is chosen"}}}]),
        json!([case_ref(&choice)]),
        false,
    );
    assert_eq!(exit, 2, "{refused:#}");
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/payload/stop_condition/value"
    );
    assert_eq!(refused["error"]["expected"]["type"], "StopCondition");
    assert!(
        message(&refused).contains("$defs/StopCondition in `investigation schema case.amend`"),
        "{refused:#}"
    );
    let example = &refused["error"]["expected"]["example"];
    for field in [
        "description",
        "reasons",
        "residual_risks",
        "reopening_conditions",
    ] {
        assert!(example.get(field).is_some(), "{refused:#}");
    }
    assert_eq!(refused["error"]["repair"]["schema"], "case.amend");
}

#[test]
fn a_missing_case_precondition_is_named_at_the_transaction_with_its_shape() {
    let root = tempfile::tempdir().unwrap();
    let choice = Choice::new(root.path(), "precondition");
    let (exit, refused) = submit(
        &choice,
        json!([{"op":"case.amend","payload":{"reason":"Retitle","title":"Queue store"}}]),
        json!([]),
        false,
    );
    assert_eq!(exit, 2, "{refused:#}");
    assert_eq!(refused["error"]["path"], "/preconditions");
    assert_eq!(
        refused["error"]["expected"]["preconditions"],
        json!([case_ref(&choice)])
    );
    let argv: Vec<&str> = refused["error"]["repair"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        argv.contains(&"show") && argv.contains(&choice.case.as_str()),
        "{refused:#}"
    );
}

#[test]
fn a_bare_code_is_followed_by_what_the_payload_needs() {
    let root = tempfile::tempdir().unwrap();
    let choice = Choice::new(root.path(), "code");
    let (exit, refused) = submit(
        &choice,
        json!([{"op":"observation.add","payload":{
            "value":{"kind":"text","statement":"The queue grew."},
            "basis":{"kind":"assertion","assertion":"operator","context":null},
            "validity":"valid","quality":[],"limitations":[],
            "evidence_kind":"interventional","variables_changed":[]}}]),
        json!([]),
        false,
    );
    assert_eq!(exit, 2, "{refused:#}");
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/payload/variables_changed"
    );
    let text = message(&refused);
    assert!(
        text.starts_with("OBSERVATION_INTERVENTION_VARIABLE_REQUIRED "),
        "{refused:#}"
    );
    assert!(
        text.contains("variables_changed, a nonempty array"),
        "{refused:#}"
    );
}

#[test]
fn a_reference_of_the_wrong_kind_lists_the_accepted_kinds() {
    let root = tempfile::tempdir().unwrap();
    let choice = Choice::new(root.path(), "kinds");
    let hypothesis = choice.create(
        "hypothesis.create",
        json!({"question_ref":reference(&choice.question, 1),"statement":"The store loses writes.",
            "applicability":"The current workload","possible_falsifiers":["A trial loses none."],
            "rival_refs":[],"assumption_refs":[]}),
    );
    let (exit, refused) = submit(
        &choice,
        json!([{"op":"hypothesis.disposition","payload":{
            "hypothesis_ref":reference(&hypothesis, 1),"lifecycle":"retired",
            "rationale":"Out of scope.","evidence_refs":[reference(&choice.question, 1)]}}]),
        json!([]),
        false,
    );
    assert_ne!(exit, 0, "{refused:#}");
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/payload/evidence_refs/0"
    );
    let text = message(&refused);
    assert!(text.contains("Observation (obs_)"), "{refused:#}");
    assert!(text.contains("it names a Question (q_)"), "{refused:#}");
    assert!(
        refused["error"]["expected"]["kinds"]
            .as_array()
            .unwrap()
            .contains(&json!("obs")),
        "{refused:#}"
    );
    assert_eq!(refused["error"]["actual"]["kind"], "q");
}

#[test]
fn gates_name_the_field_they_depend_on() {
    let root = tempfile::tempdir().unwrap();
    let choice = Choice::new(root.path(), "fields");
    // Ruling out needs criterion_ref.
    let (exit, refused) = submit(
        &choice,
        json!([{"op":"option.disposition","payload":{"option_ref":choice.options[1],
            "lifecycle":"ruled_out","reason":"Too slow"}}]),
        json!([]),
        false,
    );
    assert_ne!(exit, 0, "{refused:#}");
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/payload/criterion_ref"
    );
    assert!(
        message(&refused).starts_with("lifecycle ruled_out needs criterion_ref"),
        "{refused:#}"
    );
    // The schema says so too.
    let (_, schema) = run(&choice.ws, &["schema", "option.disposition"], None);
    assert!(
        schema
            .to_string()
            .contains("Required when `lifecycle` is `ruled_out`"),
        "{schema:#}"
    );

    // The chosen option must be among compared_alternatives, and a reversal
    // condition needs a link; discovery says both, and the close's citation.
    let (_, schema) = run(&choice.ws, &["schema", "decision.propose"], None);
    let when = schema["when_to_use"].as_str().unwrap();
    assert!(
        when.contains("lists its option in compared_alternatives"),
        "{when}"
    );
    assert!(
        when.contains("links an assumption_ref, criterion_ref or comparison_ref"),
        "{when}"
    );
    let (_, schema) = run(&choice.ws, &["schema", "case.close"], None);
    assert!(
        schema["when_to_use"]
            .as_str()
            .unwrap()
            .contains(r#""retrospective_ref": {"client_ref": ALIAS}"#),
        "{schema:#}"
    );
    let proposal = |alternatives: Value, reversal: Value| {
        json!([{"op":"decision.propose","payload":{"question_ref":reference(&choice.question, 1),
            "choice":{"kind":"select","option_ref":choice.options[0]},
            "compared_alternatives":alternatives,"comparison_refs":[],
            "hard_conditions":[{"criterion_ref":reference(&choice.criterion, 1),"reason":"Verify"}],
            "evidence_refs":[],"assumption_refs":[],"tradeoffs":[],"consequences":["Keep it"],
            "reopen_trigger":"A trial","expected_outcome":null,"check_when":null,
            "reversal_conditions":reversal}}])
    };
    let (_, refused) = submit(
        &choice,
        proposal(json!([choice.options[1]]), json!([])),
        json!([]),
        false,
    );
    assert_eq!(
        refused["error"]["path"], "/operations/0/payload/choice/option_ref",
        "{refused:#}"
    );
    assert!(
        message(&refused).contains("listed in compared_alternatives"),
        "{refused:#}"
    );
    let (_, refused) = submit(
        &choice,
        proposal(
            json!(choice.options),
            json!([{"option_ref":choice.options[1],"condition":"The new store is faster"}]),
        ),
        json!([]),
        false,
    );
    assert_eq!(
        refused["error"]["path"], "/operations/0/payload/reversal_conditions/0",
        "{refused:#}"
    );
    assert!(
        message(&refused).contains("assumption_ref, criterion_ref or comparison_ref"),
        "{refused:#}"
    );
}

#[test]
fn a_stale_reference_names_the_current_revision_and_the_read() {
    let root = tempfile::tempdir().unwrap();
    let choice = Choice::new(root.path(), "stale");
    // The options are at revision 2 after their disposition.
    let option = choice.options[0]["id"].as_str().unwrap().to_owned();
    let (exit, refused) = submit(
        &choice,
        json!([{"op":"decision.propose","payload":{"question_ref":reference(&choice.question, 1),
            "choice":{"kind":"defer","reason":"Wait"},"compared_alternatives":[reference(&option, 1)],
            "comparison_refs":[],"hard_conditions":[],"evidence_refs":[],"assumption_refs":[],
            "tradeoffs":[],"consequences":["Wait"],"reopen_trigger":"A trial",
            "expected_outcome":null,"check_when":null,"reversal_conditions":[]}}]),
        json!([]),
        false,
    );
    assert_eq!(exit, 4, "{refused:#}");
    let text = message(&refused);
    assert!(
        text.contains(&format!("{option} is at revision 2, not the cited 1")),
        "{refused:#}"
    );
    assert!(
        refused["error"]["repair"]["argv"]
            .to_string()
            .contains("\"show\""),
        "{refused:#}"
    );
}

#[test]
fn a_dry_run_reports_every_refused_operation_but_not_its_dependents() {
    let root = tempfile::tempdir().unwrap();
    let choice = Choice::new(root.path(), "every");
    let (exit, refused) = submit(
        &choice,
        json!([
            {"op":"assumption.create","payload":{"claim":"c","consequence":"d",
                "materiality":"high","affected_refs":[]}},
            {"op":"question.create","client_ref":"q","payload":{"wording":"","importance":"normal"}},
            {"op":"option.create","payload":{"question_ref":{"client_ref":"q"},
                "description":"Depends on the refused question","dependencies":[],"tradeoff_notes":[]}},
            {"op":"option.disposition","payload":{"option_ref":choice.options[1],
                "lifecycle":"ruled_out","reason":"Too slow"}},
        ]),
        json!([]),
        true,
    );
    assert_eq!(exit, 2, "{refused:#}");
    assert_eq!(refused["commit_status"], "not_committed");
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/payload/materiality"
    );
    let more = refused["error"]["additional_errors"].as_array().unwrap();
    let paths: Vec<&str> = more.iter().map(|e| e["path"].as_str().unwrap()).collect();
    assert_eq!(
        paths,
        [
            "/operations/1/payload/wording",
            "/operations/3/payload/criterion_ref"
        ],
        "{refused:#}"
    );
    // Each carries its own repair.
    assert_eq!(more[0]["repair"]["schema"], "question.create");
    assert_eq!(more[1]["repair"]["schema"], "option.disposition");
    // The text form lists them too.
    let bytes = serde_json::to_vec(&json!({
        "schema_version":"inv.command.v1","command_id":"text-form","case_id":choice.case,
        "actor":ACTOR,"preconditions":[],"operations":[
            {"op":"assumption.create","payload":{"claim":"c","consequence":"d",
                "materiality":"high","affected_refs":[]}},
            {"op":"question.create","payload":{"wording":"","importance":"normal"}}]
    }))
    .unwrap();
    let output = {
        use std::io::Write;
        let empty_path = choice.ws.parent().unwrap().join(".empty-path");
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_investigation"))
            .current_dir(choice.ws.parent().unwrap())
            .env_clear()
            .env("PATH", &empty_path)
            .arg("--workspace")
            .arg(&choice.ws)
            .args(["record", "--input", "-", "--dry-run", "--format", "text"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&bytes).unwrap();
        let output = child.wait_with_output().unwrap();
        String::from_utf8(output.stdout).unwrap() + &String::from_utf8(output.stderr).unwrap()
    };
    assert!(
        output.contains("1 more refusal(s) in this transaction:"),
        "{output}"
    );
    assert!(
        output.contains("- /operations/1/payload/wording:"),
        "{output}"
    );
}

#[test]
fn a_repair_schema_for_another_operation_is_kept_and_warnings_are_said_once() {
    let root = tempfile::tempdir().unwrap();
    let choice = Choice::new(root.path(), "kept");
    // An answered close with an open question names question.resolve.
    let (exit, refused) = submit(
        &choice,
        json!([{"op":"case.close","payload":{"outcome":"answered","reason":"Chosen",
            "unresolved_questions":[],"reopen_condition":null,
            "retrospective_waived":"Nothing material happened."}}]),
        json!([case_ref(&choice)]),
        false,
    );
    assert_eq!(exit, 5, "{refused:#}");
    assert_eq!(
        refused["error"]["path"], "/operations/0/payload/unresolved_questions",
        "{refused:#}"
    );
    assert_eq!(refused["error"]["repair"]["schema"], "question.resolve");

    // Two proposals choosing the same option that is still proposed: its
    // eligibility warning is given once.
    let option = choice.create(
        "option.create",
        json!({"question_ref":reference(&choice.question, 1),"description":"A third store",
            "dependencies":[],"tradeoff_notes":[]}),
    );
    let mut alternatives = choice.options.clone();
    alternatives.push(reference(&option, 1));
    let propose = json!({"op":"decision.propose","payload":{"question_ref":reference(&choice.question, 1),
        "choice":{"kind":"select","option_ref":reference(&option, 1)},
        "compared_alternatives":alternatives,"comparison_refs":[],
        "hard_conditions":[{"criterion_ref":reference(&choice.criterion, 1),"reason":"Verify"}],
        "evidence_refs":[],"assumption_refs":[],"tradeoffs":[],"consequences":["Adopt it"],
        "reopen_trigger":"A trial","expected_outcome":null,"check_when":null,"reversal_conditions":[]}});
    let (exit, receipt) = choice.record(
        &serde_json::to_vec(&json!({
            "schema_version":"inv.command.v1","command_id":"twice","case_id":choice.case,
            "actor":ACTOR,"preconditions":[],"operations":[propose, propose]
        }))
        .unwrap(),
    );
    assert_eq!(exit, 0, "{receipt:#}");
    let eligibility = receipt["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|warning| {
            warning
                .as_str()
                .unwrap()
                .contains("decision.accept refuses a choice that is not eligible")
        })
        .count();
    assert_eq!(eligibility, 1, "{receipt:#}");
}
