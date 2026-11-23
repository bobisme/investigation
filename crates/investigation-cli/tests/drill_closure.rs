//! Adversarial drill 6 (§17.7, §6.4, §8.6, §10): the closure transaction
//! through the public CLI.
//!
//! - A material case closed without a Retrospective is refused whole, and its
//!   repair, followed literally, leads to one batch that records the account
//!   and closes the case.
//! - The batch is idempotent under its command ID and keeps CAS semantics: a
//!   new command ID with the old case revision is a conflict, and the same
//!   command ID with a changed body is refused.
//! - An account that predates new material evidence is stale; its repair
//!   leads to an updated account that supersedes it.
//! - A non-material case closes with `retrospective_waived`; a `record` case
//!   holding only data needs neither.
//!
//! The kill-at-every-commit-step half of the drill drives the store's
//! crash-model filesystem in `investigation-store/src/closure_drill_tests.rs`.
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:closer";

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = command.spawn().unwrap();
    if let Some(stdin) = stdin {
        let mut pipe = process.stdin.take().unwrap();
        pipe.write_all(&serde_json::to_vec(stdin).unwrap()).unwrap();
    }
    let output = process.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{args:?}\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str::<Value>(&stdout).expect(&stdout)
}

fn command(key: &str, case: &Value, preconditions: Value, ops: Value) -> Value {
    json!({
        "schema_version":"inv.command.v1","command_id":key,"case_id":case,"actor":ACTOR,
        "preconditions":preconditions,"operations":ops,
    })
}

fn submit(root: &Path, command: &Value, exit: i32) -> Value {
    run(
        root,
        &["record", "--input", "-", "--json"],
        Some(command),
        exit,
    )
}

fn write(root: &Path, key: &str, case: &Value, ops: Value) -> Value {
    submit(root, &command(key, case, json!([]), ops), 0)
}

fn workspace() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    run(
        directory.path(),
        &["init", ".", "--no-vault", "--json"],
        None,
        0,
    );
    directory
}

fn create_case(root: &Path, key: &str, mode: &str) -> Value {
    write(
        root,
        key,
        &Value::Null,
        json!([{"op":"case.create","payload":{"title":"Is the nightly benchmark trustworthy?","mode":mode}}]),
    )["created"][0]["id"]
        .clone()
}

fn case_record(root: &Path, case: &Value) -> Value {
    let case = case.as_str().unwrap();
    run(
        root,
        &["--case", case, "show", case, "--depth", "full", "--json"],
        None,
        0,
    )["record"]
        .clone()
}

fn case_ref(root: &Path, case: &Value) -> Value {
    json!({"id":case,"revision":case_record(root, case)["header"]["revision"]})
}

fn run_record(root: &Path, key: &str, case: &Value, status: &str) -> Value {
    write(
        root,
        key,
        case,
        json!([{"op":"run.record","payload":{
            "intent":{"kind":"exploration","intent":"Run the nightly benchmark once."},
            "producer":"agent:bench","artifact_refs":[],"execution_status":status,
            "evidence_completeness":"complete","evidence_kind":"observational","variables_changed":[]}}]),
    )["created"][0]
        .clone()
}

fn close(extra: Value) -> Value {
    let mut payload = json!({
        "outcome":"inconclusive","reason":"The benchmark never produced a usable run.",
        "unresolved_questions":[],"reopen_condition":"A run completes on an idle host.",
    });
    payload
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    json!({"op":"case.close","payload":payload})
}

/// Run a repair's argv as given, minus the program name.
fn follow(root: &Path, failure: &Value) -> Value {
    let argv: Vec<&str> = failure["error"]["repair"]["argv"]
        .as_array()
        .unwrap_or_else(|| panic!("no repair argv: {failure}"))
        .iter()
        .skip(1)
        .map(|argument| argument.as_str().unwrap())
        .collect();
    run(root, &argv, None, 0)
}

/// A complete account built only from what the digest hands back: its
/// template, and the citation of each listed fact.
fn account_from(draft: &Value) -> Value {
    let cites: Vec<Value> = draft["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["cite"].clone())
        .collect();
    assert!(!cites.is_empty(), "the digest lists the material work");
    let mut payload = draft["record_template"].clone();
    payload.as_object_mut().unwrap().extend(
        json!({
            "methods_attempted":[{"name":"Nightly benchmark"}],"effective_steps":[],
            "invalid_or_misleading_steps":[{"description":"The benchmark runs failed.","support_refs":cites}],
            "costs_or_constraints":"Shared nightly host.","residual_unknowns":["The true nightly regression."],
            "lessons":[],"no_reusable_lesson_reason":"One host; nothing portable was learned.",
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    payload
}

fn retrospectives(root: &Path, case: &Value) -> Vec<Value> {
    run(
        root,
        &["--case", case.as_str().unwrap(), "retrospective", "--json"],
        None,
        0,
    )["retrospectives"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn a_material_close_is_refused_with_a_repair_that_leads_to_one_idempotent_batch() {
    let directory = workspace();
    let root = directory.path();
    let case = create_case(root, "case", "measure");
    let failed = run_record(root, "run", &case, "failed");
    let before = case_ref(root, &case);

    // Neither a waiver nor silence closes a material case; nothing commits.
    let mut refused = Value::Null;
    for (key, extra) in [
        (
            "waive",
            json!({"retrospective_waived":"Nothing worth recording."}),
        ),
        ("bare", json!({})),
    ] {
        refused = submit(
            root,
            &command(key, &case, json!([before]), json!([close(extra)])),
            5,
        );
        assert_eq!(refused["error"]["code"], "RETROSPECTIVE_REQUIRED", "{key}");
        assert_eq!(refused["commit_status"], "not_committed");
        assert_eq!(refused["error"]["actual"], json!([failed]));
        assert_eq!(
            refused["error"]["path"],
            "/operations/0/payload/retrospective_ref"
        );
        assert_eq!(refused["error"]["repair"]["schema"], "retrospective.record");
        assert_eq!(case_ref(root, &case), before, "{key}");
        assert_eq!(case_record(root, &case)["body"]["lifecycle"], "active");
    }

    // Following the repair yields the digest: the case revision, a verified
    // snapshot and the citable facts, enough for the batch.
    let draft = follow(root, &refused);
    assert_eq!(draft["record_template"]["case_ref"], before);
    assert!(
        draft["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["cite"] == failed["id"]),
        "{draft}"
    );
    let batch = command(
        "close-batch",
        &case,
        json!([before]),
        json!([
            {"op":"retrospective.record","client_ref":"account","payload":account_from(&draft)},
            close(json!({"retrospective_ref":{"client_ref":"account"}})),
        ]),
    );
    let committed = submit(root, &batch, 0);
    let retrospective = committed["created"][0].clone();
    assert_eq!(retrospective["client_ref"], "account");
    assert!(retrospective["id"].as_str().unwrap().starts_with("retro_"));
    let closed = case_record(root, &case);
    assert_eq!(closed["body"]["lifecycle"], "closed");
    assert_eq!(
        closed["body"]["closure"]["retrospective_ref"],
        json!({"id":retrospective["id"],"revision":1})
    );
    assert_eq!(committed["updated"][0]["id"], case);

    // The identical retry returns the original IDs and writes nothing.
    let retried = submit(root, &batch, 0);
    for field in ["transaction_id", "created", "updated"] {
        assert_eq!(retried[field], committed[field], "{field}");
    }
    assert_eq!(committed["result"], "committed");
    assert_eq!(retried["result"], "already_committed", "{retried}");
    assert_eq!(retrospectives(root, &case).len(), 1);
    assert_eq!(case_record(root, &case), closed);

    // CAS holds: the same batch under a new command ID read revision 1, and a
    // changed body under the original command ID is refused.
    let mut stale = batch.clone();
    stale["command_id"] = json!("close-batch-again");
    let conflict = submit(root, &stale, 4);
    assert_eq!(conflict["commit_status"], "not_committed", "{conflict}");
    assert_eq!(conflict["error"]["code"], "REVISION_CONFLICT");
    let mut changed = batch.clone();
    changed["operations"][1]["payload"]["reason"] = json!("A different account of the close.");
    let reused = submit(root, &changed, 4);
    assert_eq!(reused["commit_status"], "not_committed", "{reused}");
    assert_eq!(reused["error"]["code"], "IDEMPOTENCY_KEY_REUSED");
    assert_eq!(retrospectives(root, &case).len(), 1);
    assert_eq!(case_record(root, &case), closed);
}

#[test]
fn new_material_evidence_after_the_account_requires_an_updated_one() {
    let directory = workspace();
    let root = directory.path();
    let case = create_case(root, "case", "explore");
    run_record(root, "first-run", &case, "failed");
    let draft = run(
        root,
        &[
            "--case",
            case.as_str().unwrap(),
            "retrospective",
            "--draft",
            "--json",
        ],
        None,
        0,
    );
    let old = write(
        root,
        "account",
        &case,
        json!([{"op":"retrospective.record","payload":account_from(&draft)}]),
    )["created"][0]
        .clone();
    let old = json!({"id":old["id"],"revision":1});

    // A second failed run arrives after the account was written.
    let second = run_record(root, "second-run", &case, "failed");
    let expected = case_ref(root, &case);
    let refused = submit(
        root,
        &command(
            "close-stale",
            &case,
            json!([expected]),
            json!([close(json!({"retrospective_ref":old}))]),
        ),
        5,
    );
    assert_eq!(refused["error"]["code"], "RETROSPECTIVE_STALE");
    assert_eq!(refused["commit_status"], "not_committed");
    assert_eq!(refused["error"]["actual"], json!([second]));
    assert_eq!(case_record(root, &case)["body"]["lifecycle"], "active");

    // The repair's digest now covers the second run; the updated account
    // supersedes the old one and closes in the same batch.
    let draft = follow(root, &refused);
    assert!(
        draft["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["cite"] == second["id"]),
        "{draft}"
    );
    let committed = submit(
        root,
        &command(
            "close-updated",
            &case,
            json!([expected]),
            json!([
                {"op":"retrospective.record","client_ref":"updated","payload":account_from(&draft)},
                {"op":"retrospective.supersede","payload":{
                    "previous":old,"successor":{"client_ref":"updated"},
                    "reason":"Cover the second failed run."}},
                close(json!({"retrospective_ref":{"client_ref":"updated"}})),
            ]),
        ),
        0,
    );
    let updated = committed["created"][0]["id"].clone();
    let closed = case_record(root, &case);
    assert_eq!(closed["body"]["lifecycle"], "closed");
    assert_eq!(
        closed["body"]["closure"]["retrospective_ref"]["id"],
        updated
    );
    // The earlier account stays readable, marked superseded.
    let accounts = retrospectives(root, &case);
    let status = |id: &Value| {
        accounts
            .iter()
            .find(|account| &account["id"] == id)
            .unwrap_or_else(|| panic!("{id} in {accounts:?}"))["status"]
            .clone()
    };
    assert_eq!(status(&old["id"]), "superseded");
    assert_eq!(status(&updated), "recorded");
}

#[test]
fn a_nonmaterial_case_waives_and_a_record_case_holding_data_needs_neither() {
    let directory = workspace();
    let root = directory.path();

    let quiet = create_case(root, "quiet", "explore");
    run_record(root, "quiet-run", &quiet, "completed");
    let expected = case_ref(root, &quiet);
    let refused = submit(
        root,
        &command(
            "quiet-bare",
            &quiet,
            json!([expected]),
            json!([close(json!({}))]),
        ),
        5,
    );
    assert_eq!(refused["error"]["code"], "GATE_FAILED", "{refused}");
    assert_eq!(refused["commit_status"], "not_committed");
    submit(
        root,
        &command(
            "quiet-waive",
            &quiet,
            json!([expected]),
            json!([close(
                json!({"retrospective_waived":"One routine run; no method question arose."})
            )]),
        ),
        0,
    );
    let closure = &case_record(root, &quiet)["body"]["closure"];
    assert_eq!(
        closure["retrospective_waived"],
        "One routine run; no method question arose."
    );
    assert_eq!(closure["retrospective_ref"], Value::Null);

    let capture = create_case(root, "capture", "record");
    run_record(root, "capture-run", &capture, "completed");
    let expected = case_ref(root, &capture);
    submit(
        root,
        &command(
            "capture-close",
            &capture,
            json!([expected]),
            json!([close(json!({}))]),
        ),
        0,
    );
    let closure = &case_record(root, &capture)["body"]["closure"];
    assert_eq!(case_record(root, &capture)["body"]["lifecycle"], "closed");
    assert_eq!(closure["retrospective_ref"], Value::Null);
    assert_eq!(closure["retrospective_waived"], Value::Null);
    assert!(retrospectives(root, &capture).is_empty());
}
