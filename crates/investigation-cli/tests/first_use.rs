//! A cold agent in an ordinary working directory reaches a receipt naming a
//! case and its retained source by following onboarding, and a successor
//! started in the same directory finds the workspace and its limitation.

use std::{fs, path::Path, process::Command};

use serde_json::Value;

fn investigation(cwd: &Path, args: &[&str]) -> (i32, Value, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value = serde_json::from_str(&stdout).unwrap_or(Value::Null);
    (
        output.status.code().unwrap(),
        value,
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn argv(step: &Value) -> Vec<String> {
    step["argv"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|part| part.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn onboarding_reaches_a_source_and_case_receipt_in_two_further_calls() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    fs::write(cwd.join("source.txt"), "09:12 driver 4.2 installed\n").unwrap();
    fs::write(cwd.join("NOTES.md"), "").unwrap();

    let (code, onboard, _) = investigation(cwd, &["onboard", "--format", "json"]);
    assert_eq!(code, 0);
    assert_eq!(onboard["state"], "no_workspace");
    let next = onboard["next"].as_array().unwrap();
    assert_eq!(next[0]["kind"], "initialize_workspace");
    assert_eq!(next[1]["kind"], "start_case_with_source");

    let init: Vec<String> = argv(&next[0]);
    let init: Vec<&str> = init.iter().map(String::as_str).collect();
    let (code, initialized, _) = investigation(cwd, &init);
    assert_eq!(code, 0, "{initialized}");
    assert_eq!(initialized["result"], "initialized");
    assert_eq!(
        initialized["import_roots"][0].as_str().unwrap(),
        fs::canonicalize(cwd).unwrap().to_str().unwrap()
    );

    // Fill only the declared unresolved fields of the onboarding template.
    let start: Vec<String> = argv(&next[1])
        .into_iter()
        .map(|part| match part.as_str() {
            "QUESTION" => "What stalled the jobs?".to_owned(),
            "FILE" => "./source.txt".to_owned(),
            _ => part,
        })
        .collect();
    let start: Vec<&str> = start.iter().map(String::as_str).collect();
    let (code, receipt, stderr) = investigation(cwd, &start);
    assert_eq!(code, 0, "{receipt}");
    assert!(stderr.starts_with("command_id: tx_"), "{stderr}");
    let created: Vec<&str> = receipt["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["id"].as_str().unwrap())
        .collect();
    assert!(created.iter().any(|id| id.starts_with("case_")));
    let source = created
        .iter()
        .find(|id| id.starts_with("src_"))
        .copied()
        .unwrap();

    let workspace = cwd.join("casework");
    let (code, observed, _) = investigation(
        cwd,
        &[
            "--workspace",
            workspace.to_str().unwrap(),
            "--actor",
            "agent:investigator",
            "observe",
            "Driver 4.2 was installed at 09:12",
            "--source",
            source,
            "--quote",
            "driver 4.2 installed",
            "--limitations",
            r#"["Only one server log was exported."]"#,
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 0, "{observed}");

    // A successor in the same directory is pointed at the workspace.
    let (code, missing, _) = investigation(cwd, &["brief", "--format", "json"]);
    assert_eq!(code, 3);
    assert_eq!(missing["error"]["code"], "WORKSPACE_REQUIRED");
    assert_eq!(
        missing["error"]["repair"]["argv"][2].as_str().unwrap(),
        "./casework"
    );
    let (code, onboard, _) = investigation(cwd, &["onboard", "--format", "json"]);
    assert_eq!(code, 0);
    assert_eq!(onboard["state"], "case_selected");
    let handoff = onboard["next"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["kind"] == "read_handoff")
        .unwrap();
    let brief: Vec<String> = argv(handoff);
    let brief: Vec<&str> = brief.iter().map(String::as_str).collect();
    let (code, brief, _) = investigation(cwd, &brief);
    assert_eq!(code, 0);
    let evidence = &brief["evidence"][0];
    assert_eq!(evidence["source_ref"]["id"], source);
    assert_eq!(
        evidence["recorded_limitations"][0],
        "Only one server log was exported."
    );
}

#[test]
fn paths_outside_import_roots_explain_the_repair() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    fs::write(cwd.join("source.txt"), "x\n").unwrap();
    let (code, _, _) = investigation(cwd, &["init", "casework", "--no-vault", "--format", "json"]);
    assert_eq!(code, 0);
    let (code, refused, _) = investigation(
        cwd,
        &[
            "--workspace",
            "casework",
            "--actor",
            "agent:investigator",
            "start",
            "Q?",
            "--attach",
            "source.txt",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 2);
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains("none are configured"), "{message}");
    assert!(message.contains("--import-root"), "{message}");
    let (code, refused, _) = investigation(
        cwd,
        &[
            "init",
            "other",
            "--import-root",
            "missing-directory",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 2);
    assert_eq!(refused["error"]["path"], "--import-root");
    assert!(!cwd.join("other").exists());
}

fn strings(argv: &Value) -> Vec<String> {
    argv.as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap().to_owned())
        .collect()
}

fn run_argv(cwd: &Path, argv: &[String]) -> (i32, Value, String) {
    assert_eq!(argv[0], "investigation");
    let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    investigation(cwd, &args)
}

#[test]
fn an_existing_workspace_gains_an_import_root_through_the_repair() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    fs::write(cwd.join("source.txt"), "09:12 driver 4.2 installed\n").unwrap();
    let (code, initialized, _) =
        investigation(cwd, &["init", "casework", "--no-vault", "--format", "json"]);
    assert_eq!(code, 0);
    assert_eq!(initialized["import_roots"], serde_json::json!([]));
    let pointer = fs::read_to_string(cwd.join("casework/README.txt")).unwrap();
    assert!(pointer.contains("investigation --workspace DIR brief"));
    assert!(!pointer.contains(cwd.to_str().unwrap()));

    let start = [
        "--workspace",
        "casework",
        "--actor",
        "agent:investigator",
        "start",
        "What changed?",
        "--attach",
        "source.txt",
        "--format",
        "json",
    ];
    let (code, refused, _) = investigation(cwd, &start);
    assert_eq!(code, 2, "{refused}");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(!message.contains("investigation.toml"), "{message}");
    let repair = strings(&refused["error"]["repair"]["argv"]);
    assert_eq!(repair[1], "init");
    assert_eq!(repair[3], "--import-root");
    let (code, added, _) = run_argv(cwd, &repair);
    assert_eq!(code, 0, "{added}");
    assert_eq!(added["result"], "import_roots_added");
    assert_eq!(added["workspace_id"], initialized["workspace_id"]);
    let root = fs::canonicalize(cwd).unwrap();
    assert_eq!(
        added["added_import_roots"],
        serde_json::json!([root.to_str().unwrap()])
    );
    let (code, again, _) = run_argv(cwd, &repair);
    assert_eq!(code, 0);
    assert_eq!(again["result"], "already_initialized");
    assert_eq!(again["added_import_roots"], serde_json::json!([]));

    let (code, receipt, _) = investigation(cwd, &start);
    assert_eq!(code, 0, "{receipt}");
    // The JSON receipt's next write is the observation template.
    let step = &receipt["next_write"];
    assert_eq!(step["kind"], "record_observation");
    let template = strings(&step["argv"]);
    assert!(template.contains(&"observe".to_owned()));
    for placeholder in ["STATEMENT", "EXACT TEXT"] {
        assert!(template.contains(&placeholder.to_owned()), "{template:?}");
    }
    assert!(template.contains(&r#"["LIMITATION"]"#.to_owned()));
    assert_eq!(
        step["unresolved_fields"],
        serde_json::json!(["STATEMENT", "EXACT TEXT", "LIMITATION"])
    );
    let filled: Vec<String> = template
        .into_iter()
        .map(|part| match part.as_str() {
            "STATEMENT" => "Driver 4.2 was installed at 09:12".to_owned(),
            "EXACT TEXT" => "driver 4.2 installed".to_owned(),
            r#"["LIMITATION"]"# => r#"["One log only."]"#.to_owned(),
            _ => part,
        })
        .collect();
    let (code, observed, _) = run_argv(cwd, &filled);
    assert_eq!(code, 0, "{observed}");
    // The v1 receipt list still holds only local reads.
    for step in receipt["next"].as_array().unwrap() {
        assert_eq!(step["kind"], "local_read");
    }
}

#[test]
fn a_missing_relative_attachment_names_the_resolved_path_and_import_roots() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    fs::write(cwd.join("source.txt"), "x\n").unwrap();
    let (code, _, _) = investigation(
        cwd,
        &[
            "init",
            "casework",
            "--no-vault",
            "--import-root",
            ".",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 0);
    let inside = cwd.join("casework");
    let root = fs::canonicalize(cwd).unwrap();
    let (code, refused, _) = investigation(
        &inside,
        &[
            "--actor",
            "agent:investigator",
            "start",
            "Q?",
            "--attach",
            "source.txt",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 2, "{refused}");
    let error = &refused["error"];
    assert_eq!(error["path"], "--attach");
    let resolved = root.join("casework/source.txt");
    assert_eq!(error["actual"]["resolved_path"], resolved.to_str().unwrap());
    assert_eq!(
        error["expected"]["import_roots"],
        serde_json::json!([root.to_str().unwrap()])
    );
    let message = error["message"].as_str().unwrap();
    assert!(message.contains(resolved.to_str().unwrap()), "{message}");
    assert!(message.contains(root.to_str().unwrap()), "{message}");
    let repair = strings(&error["repair"]["argv"]);
    assert!(repair.contains(&root.join("source.txt").to_str().unwrap().to_owned()));
    let (code, receipt, _) = run_argv(&inside, &repair);
    assert_eq!(code, 0, "{receipt}");

    // A file found nowhere has no argv, only the resolved path and roots.
    let (code, refused, _) = investigation(
        &inside,
        &[
            "--actor",
            "agent:investigator",
            "start",
            "Q?",
            "--attach",
            "absent.txt",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 2);
    assert!(refused["error"]["repair"].is_null());
    assert_eq!(
        refused["error"]["actual"]["resolved_path"],
        root.join("casework/absent.txt").to_str().unwrap()
    );
}
