use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:cli-test";

fn invocation(cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT");
    command
}

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let mut command = invocation(cwd);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
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
        "{stdout}\n{:?}",
        output.stderr
    );
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert!(!stdout.contains('\u{1b}'));
    let response = serde_json::from_str::<Value>(&stdout).expect(&stdout);
    assert!(response.is_object());
    response
}

fn create_case(cwd: &Path, key: &str) -> (Value, Value) {
    let schema = run(
        cwd,
        &["schema", "case.create", "--actor", ACTOR, "--json"],
        None,
        0,
    );
    assert_eq!(schema["transaction_example"]["status"], "runnable_example");
    let mut command = schema["transaction_example"]["input"].clone();
    command["command_id"] = json!(key);
    let receipt = run(
        cwd,
        &["record", "--input", "-", "--json"],
        Some(&command),
        0,
    );
    (command, receipt)
}

#[test]
fn json_discovery_and_usage_are_single_objects() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    for args in [
        vec!["--json", "typo"],
        vec!["typo", "--json"],
        vec!["typo", "--format=json"],
        vec!["typo", "--format", "json"],
        vec!["record", "--json"],
    ] {
        let response = run(cwd, &args, None, 2);
        assert_eq!(response["error"]["code"], "INVALID_INPUT");
    }
    let response = run(cwd, &["--json", "--help"], None, 0);
    assert!(
        response["help"]
            .as_str()
            .unwrap()
            .contains("investigation schema case.create")
    );
    assert!(!response["help"].as_str().unwrap().contains("--json"));
    let response = run(cwd, &["schema", "--list", "--format", "json"], None, 0);
    assert!(response["stages"].as_array().unwrap().iter().any(|stage| {
        stage["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("case.create"))
    }));
    let response = run(cwd, &["schema", "case.creat", "--json"], None, 2);
    assert!(
        response["error"]["expected"]
            .as_array()
            .unwrap()
            .contains(&json!("case.create"))
    );
    let response = run(
        cwd,
        &["schema", "record", "--schema-only", "--json"],
        None,
        0,
    );
    assert_eq!(response["type"], "object");
    assert!(response.get("result").is_none());
    let response = run(cwd, &["schema", "question.create", "--json"], None, 0);
    assert_eq!(response["transaction_example"]["status"], "incomplete_form");
    assert_eq!(
        response["transaction_example"]["unresolved_fields"],
        json!(["/actor", "/case_id"])
    );
}

#[test]
fn init_is_idempotent_and_discovery_uses_nearest_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    let outer = run(cwd, &["init", "outer", "--json"], None, 0);
    assert!(cwd.join("outer/vault/Notes").is_dir());
    let refreshed = run(cwd, &["init", "outer", "--no-vault", "--json"], None, 0);
    assert_eq!(refreshed["workspace_id"], outer["workspace_id"]);
    assert_eq!(refreshed["result"], "already_initialized");
    assert_eq!(refreshed["layout"]["projection"], "vault/");

    let inner = run(
        cwd,
        &["init", "outer/vault/Notes/inner", "--no-vault", "--json"],
        None,
        0,
    );
    assert!(inner["layout"]["projection"].is_null());
    let inner = cwd.join("outer/vault/Notes/inner");
    let shell = inner.join("local");
    let (_, receipt) = create_case(&shell, "inner-only");
    let found = run(
        &shell,
        &["receipt", "inner-only", "--actor", ACTOR, "--json"],
        None,
        0,
    );
    assert_eq!(found["transaction_id"], receipt["transaction_id"]);
    let outer = cwd.join("outer");
    let missing = run(
        &shell,
        &[
            "receipt",
            "inner-only",
            "--actor",
            ACTOR,
            "--workspace",
            outer.to_str().unwrap(),
            "--json",
        ],
        None,
        0,
    );
    assert_eq!(missing["result"], "not_found_at_snapshot");
}

#[test]
fn record_retries_and_rejects_input_without_changing_the_case() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    run(cwd, &["init", "workspace", "--json"], None, 0);
    let workspace = cwd.join("workspace");
    let (command, receipt) = create_case(&workspace, "first-case");
    fs::write(
        workspace.join("case.json"),
        serde_json::to_vec(&command).unwrap(),
    )
    .unwrap();
    let retry = run(
        &workspace,
        &["record", "--input", "case.json", "--json"],
        None,
        0,
    );
    assert_eq!(retry["result"], "already_committed");
    for field in ["transaction_id", "created", "snapshot"] {
        assert_eq!(retry[field], receipt[field]);
    }
    assert_eq!(
        retry["next"][0]["argv"].as_array().unwrap()[..2],
        [json!("investigation"), json!("--workspace")]
    );

    let response = run(
        &workspace,
        &["record", "--input", "-", "--actor", "agent:other", "--json"],
        Some(&command),
        2,
    );
    assert_eq!(response["error"]["path"], "/actor");
    let mut invalid = command.clone();
    invalid["command_id"] = json!("malformed");
    invalid["operations"][0]["payload"]["unknown_field"] = json!(true);
    let response = run(
        &workspace,
        &["record", "--input", "-", "--json"],
        Some(&invalid),
        2,
    );
    assert_eq!(response["commit_status"], "not_committed");
    assert_eq!(
        response["error"]["path"],
        "/operations/0/payload/unknown_field"
    );
    let absent = run(
        &workspace,
        &["receipt", "malformed", "--actor", ACTOR, "--json"],
        None,
        0,
    );
    assert_eq!(absent["result"], "not_found_at_snapshot");
    assert_eq!(absent["cases"][0]["snapshot"], receipt["snapshot"]);

    let case_id = receipt["created"][0]["id"].as_str().unwrap();
    let question = run(
        &workspace,
        &["schema", "question.create", "--actor", ACTOR, "--json"],
        None,
        0,
    );
    assert_eq!(question["transaction_example"]["input"]["case_id"], case_id);
    let (_, second) = create_case(&workspace, "second-case");
    let response = run(
        &workspace,
        &[
            "receipt",
            "first-case",
            "--case",
            &case_id[..8],
            "--actor",
            ACTOR,
            "--json",
        ],
        None,
        3,
    );
    assert_eq!(response["error"]["code"], "AMBIGUOUS_REFERENCE");
    assert_eq!(response["error"]["actual"].as_array().unwrap().len(), 2);

    let response = run(
        &workspace,
        &[
            "schema",
            "question.create",
            "--case",
            case_id,
            "--actor",
            ACTOR,
            "--json",
        ],
        None,
        0,
    );
    let question = &response["transaction_example"]["input"];
    let wrong_case = second["created"][0]["id"].as_str().unwrap();
    let response = run(
        &workspace,
        &["record", "--input", "-", "--case", wrong_case, "--json"],
        Some(question),
        2,
    );
    assert_eq!(response["error"]["path"], "/case_id");
    let response = run(
        &workspace,
        &["record", "--input", "-", "--case", case_id, "--json"],
        Some(question),
        0,
    );
    assert_eq!(response["snapshot"]["sequence"], 2);
}

#[test]
fn check_and_index_repair_preserve_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    run(cwd, &["init", "workspace", "--json"], None, 0);
    let workspace = cwd.join("workspace");
    let (_, receipt) = create_case(&workspace, "survive-repair");
    let response = run(&workspace, &["check", "--json"], None, 0);
    assert_eq!(response["ok"], true);
    fs::remove_file(workspace.join("cache/index.sqlite")).unwrap();
    let degraded = run(&workspace, &["check", "--json"], None, 0);
    assert_eq!(degraded["index"], "degraded");
    assert_eq!(degraded["issues"][0]["code"], "INDEX_STALE");
    assert_eq!(degraded["issues"][0]["repair"][1], "--workspace");
    assert!(!workspace.join("cache/index.sqlite").exists());
    run(
        &workspace,
        &["repair", "index", "--read-only", "--json"],
        None,
        2,
    );
    assert!(!workspace.join("cache/index.sqlite").exists());
    let response = run(&workspace, &["repair", "index", "--json"], None, 0);
    assert_eq!(response["result"], "index_rebuilt");
    let found = run(
        &workspace,
        &["receipt", "survive-repair", "--actor", ACTOR, "--json"],
        None,
        0,
    );
    assert_eq!(found["transaction_id"], receipt["transaction_id"]);
    assert_eq!(found["snapshot"], receipt["snapshot"]);

    let case = receipt["created"][0]["id"].as_str().unwrap();
    let case = workspace.join("state/cases").join(case);
    fs::rename(case.join("events"), case.join("original-events")).unwrap();
    let failure = run(&workspace, &["check", "--json"], None, 6);
    assert_eq!(failure["ok"], false);
    assert_eq!(failure["issues"][0]["code"], "MISSING_EVENTS_DIRECTORY");
    let failure = run(
        &workspace,
        &["receipt", "survive-repair", "--actor", ACTOR, "--json"],
        None,
        6,
    );
    assert_eq!(failure["commit_status"], "unknown");
    run(&workspace, &["repair", "index", "--json"], None, 6);
}

#[test]
fn layered_read_and_brief_use_verified_scope_and_exact_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    run(cwd, &["init", ".", "--no-vault", "--json"], None, 0);
    let schema = run(
        cwd,
        &["schema", "case.create", "--actor", ACTOR, "--json"],
        None,
        0,
    );
    let mut command = schema["transaction_example"]["input"].clone();
    command["command_id"] = json!("layered");
    command["operations"][0]["payload"]["title"] = json!("Évidence \"quoted\"");
    let receipt = run(
        cwd,
        &["record", "--input", "-", "--json"],
        Some(&command),
        0,
    );
    let id = receipt["created"][0]["id"].as_str().unwrap();

    let title = run(cwd, &["show", id, "--depth", "title", "--json"], None, 0);
    assert_eq!(title["depth"], "title");
    assert!(title.get("record").is_none());
    assert_eq!(title["snapshot"], receipt["snapshot"]);
    let summary = run(cwd, &["show", id, "--json"], None, 0);
    assert_eq!(summary["depth"], "summary");
    assert!(summary.get("summary_basis").is_some());
    let full = run(
        cwd,
        &["show", id, "--at", "1", "--depth", "full", "--json"],
        None,
        0,
    );
    assert_eq!(full["historical"], true);
    assert_eq!(full["record"]["body"]["title"], "Évidence \"quoted\"");
    assert_eq!(full["basis_revision"], 1);

    let error = run(cwd, &["brief", "--max-bytes", "1", "--json"], None, 2);
    assert_eq!(error["error"]["code"], "BUDGET_TOO_SMALL");
    let minimum = error["error"]["expected"]["minimum_bytes"]
        .as_u64()
        .unwrap();
    let budget = minimum.to_string();
    let brief = run(cwd, &["brief", "--max-bytes", &budget, "--json"], None, 0);
    assert!(serde_json::to_vec(&brief).unwrap().len() < minimum as usize);
    assert_eq!(brief["snapshot"], receipt["snapshot"]);
    assert_eq!(brief["omitted_items"]["deeper_read_commands"], 1);

    create_case(cwd, "other");
    let missing_case = run(cwd, &["show", id, "--json"], None, 3);
    assert_eq!(missing_case["error"]["code"], "CASE_REQUIRED");
    assert_eq!(missing_case["error"]["actual"].as_array().unwrap().len(), 2);
}
