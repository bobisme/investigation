//! The process digest (§8.6) through the public CLI: a read bound to the
//! verified snapshot, whose items and template feed `retrospective.record`.
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:cli-test";

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
    assert_eq!(output.status.code(), Some(exit), "{stdout}");
    serde_json::from_str::<Value>(&stdout).expect(&stdout)
}

fn record(root: &Path, key: &str, case: &Value, op: &str, payload: Value, exit: i32) -> Value {
    let command = json!({
        "schema_version":"inv.command.v1","command_id":key,"case_id":case,"actor":ACTOR,
        "preconditions":[],"operations":[{"op":op,"payload":payload}],
    });
    run(
        root,
        &["record", "--input", "-", "--json"],
        Some(&command),
        exit,
    )
}

#[test]
fn draft_digest_is_a_snapshot_bound_read_that_grounds_the_recorded_account() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    run(root, &["init", ".", "--no-vault", "--json"], None, 0);
    let created = record(
        root,
        "case",
        &Value::Null,
        "case.create",
        json!({"title":"Why is the stage slow?","mode":"explain",
               "expectations":{"expected_answer_or_choice":"The local queue grows.","surprising_outcomes":[]}}),
        0,
    );
    let case = created["created"][0]["id"].clone();
    let failed = record(
        root,
        "run",
        &case,
        "run.record",
        json!({"intent":{"kind":"exploration","intent":"Trace one burst."},"producer":"agent:producer",
               "artifact_refs":[],"execution_status":"failed","evidence_completeness":"missing",
               "evidence_kind":"observational","variables_changed":[]}),
        0,
    )["created"][0]["id"]
        .clone();
    let case_arg = case.as_str().unwrap();
    let draft = [
        "--case",
        case_arg,
        "--read-only",
        "retrospective",
        "--draft",
        "--json",
    ];
    let digest = run(root, &draft, None, 0);
    assert_eq!(digest["schema_version"], "inv.retrospective-digest.v1");
    assert!(
        digest["coverage"]
            .as_str()
            .unwrap()
            .contains("recorded work only")
    );
    let ids: Vec<_> = digest["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    let run_id = failed.as_str().unwrap();
    assert_eq!(
        ids,
        [
            format!("run.failed/{run_id}"),
            format!("run.incomplete/{run_id}")
        ]
    );
    let entries = digest["expected_versus_actual"].as_array().unwrap();
    assert_eq!(entries[0]["expected"], "The local queue grows.");
    assert_eq!(entries[0]["actual_state"], "no_outcome_recorded");
    assert_eq!(entries[1]["expected_state"], "no_expectation_recorded");
    assert_eq!(
        run(root, &draft, None, 0),
        digest,
        "the digest is a deterministic read"
    );
    let missing = run(
        root,
        &["--case", case_arg, "retrospective", "--json"],
        None,
        2,
    );
    assert_eq!(missing["commit_status"], "not_committed");

    let mut payload = digest["record_template"].clone();
    let fields = payload.as_object_mut().unwrap();
    fields.extend(json!({
        "methods_attempted":[{"name":"Burst tracing"}],"effective_steps":[],
        "invalid_or_misleading_steps":[{"description":"The trace run failed.","support_refs":[failed]}],
        "costs_or_constraints":"One synthetic session.","residual_unknowns":["The cause of the stall."],
        "lessons":[{"guidance":"Confirm the tracer attaches before the burst.","applies_when":"Short synthetic bursts.",
                    "support_refs":[],"transfer_limits":"One case; tracer specifics may differ."}],
    }).as_object().unwrap().clone());
    let rejected = record(
        root,
        "uncited",
        &case,
        "retrospective.record",
        payload.clone(),
        5,
    );
    assert_eq!(
        rejected["error"]["path"],
        "/operations/0/payload/lessons/0/support_refs"
    );
    let argv = rejected["error"]["repair"]["argv"].as_array().unwrap();
    let tail = &argv[argv.len() - 6..];
    assert_eq!(
        tail,
        [
            json!("--case"),
            case.clone(),
            json!("retrospective"),
            json!("--draft"),
            json!("--format"),
            json!("json")
        ]
    );
    payload["lessons"][0]["support_refs"] = json!([failed]);
    let accepted = record(root, "cited", &case, "retrospective.record", payload, 0);
    assert_eq!(accepted["created"][0]["revision"], 1);
}
