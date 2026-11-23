//! First use with an actor named once: `init --actor` records a machine-local
//! actor that a write without `--actor` falls back to, the receipt says so,
//! and nothing else ever supplies an actor silently (bn-6vlm).

use std::{fs, path::Path, process::Command};

use serde_json::Value;

fn run(cwd: &Path, agent: Option<&str>, args: &[&str]) -> (i32, Value) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args);
    if let Some(agent) = agent {
        command.env("AGENT", agent);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    (
        output.status.code().unwrap(),
        serde_json::from_str(&stdout).unwrap_or(Value::Null),
    )
}

fn strings(argv: &Value) -> Vec<String> {
    argv.as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|part| part.as_str().unwrap().to_owned())
        .collect()
}

fn run_argv(cwd: &Path, argv: &[String]) -> (i32, Value) {
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    run(cwd, None, &argv)
}

/// The actor of the journal event that created `case`.
fn case_actor(workspace: &Path, receipt: &Value) -> String {
    let case = receipt["created"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|change| change["id"].as_str().filter(|id| id.starts_with("case_")))
        .unwrap();
    let event = workspace.join(format!("state/cases/{case}/events/000000000001.json"));
    let event: Value = serde_json::from_slice(&fs::read(event).unwrap()).unwrap();
    event["actor"].as_str().unwrap().to_owned()
}

fn start(cwd: &Path, agent: Option<&str>, extra: &[&str], question: &str) -> (i32, Value) {
    let mut args = vec!["--workspace", "./casework"];
    args.extend_from_slice(extra);
    args.extend([
        "start",
        question,
        "--attach",
        "./source.txt",
        "--format",
        "json",
    ]);
    run(cwd, agent, &args)
}

#[test]
fn the_onboarding_init_records_the_actor_and_a_start_without_actor_succeeds_attributed_to_it() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    fs::write(cwd.join("source.txt"), "kiln K2 read 12 degrees low\n").unwrap();

    // The first onboarding step is the init a cold agent copies; it names the actor.
    let (code, onboard) = run(cwd, None, &["onboard", "--format", "json"]);
    assert_eq!(code, 0);
    let init = strings(&onboard["next"][0]["argv"]);
    assert!(
        init.windows(2)
            .any(|pair| pair[0] == "--actor" && pair[1] == "agent:investigator"),
        "{init:?}"
    );
    let (code, initialized) = run_argv(cwd, &init);
    assert_eq!(code, 0, "{initialized}");
    assert_eq!(initialized["actor_setting"]["actor"], "agent:investigator");
    assert_eq!(initialized["actor_setting"]["file"], "local/actor.toml");
    assert_eq!(initialized["actor_setting"]["scope"], "machine_local");
    let workspace = cwd.join("casework");
    assert!(workspace.join("local/actor.toml").is_file());
    let config = fs::read_to_string(workspace.join("investigation.toml")).unwrap();
    assert!(
        !config.contains("agent:investigator"),
        "the shared config is untouched: {config}"
    );

    // A start that omits --actor, as cold agents typed it, now succeeds in one call.
    let (code, receipt) = start(cwd, None, &[], "How accurate is K2?");
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(case_actor(&workspace, &receipt), "agent:investigator");
    let warnings = receipt["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|warning| {
            let warning = warning.as_str().unwrap();
            warning.contains("agent:investigator") && warning.contains("local/actor.toml")
        }),
        "the receipt says where the attribution came from: {warnings:?}"
    );
    assert!(
        receipt["next_write"]["argv"]
            .as_array()
            .unwrap()
            .windows(2)
            .any(|pair| pair[0] == "--actor" && pair[1] == "agent:investigator"),
        "the next write names the actor explicitly"
    );

    // --actor and AGENT both take precedence, and then no warning is given.
    let (code, receipt) = start(cwd, None, &["--actor", "agent:other"], "Second?");
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(case_actor(&workspace, &receipt), "agent:other");
    assert!(receipt["warnings"].as_array().unwrap().is_empty());
    let (code, receipt) = start(cwd, Some("agent:from-env"), &[], "Third?");
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(case_actor(&workspace, &receipt), "agent:from-env");
    assert!(receipt["warnings"].as_array().unwrap().is_empty());

    // Running init again with another actor replaces the setting and says so.
    let (code, again) = run(
        cwd,
        None,
        &[
            "init",
            "./casework",
            "--actor",
            "agent:successor",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 0, "{again}");
    assert_eq!(again["result"], "already_initialized");
    assert_eq!(again["actor_setting"]["replaced"], "agent:investigator");
    let (code, receipt) = start(cwd, None, &[], "Fourth?");
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(case_actor(&workspace, &receipt), "agent:successor");
}

#[test]
fn without_init_actor_a_start_without_actor_is_still_refused_and_names_both_fixes() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    fs::write(cwd.join("source.txt"), "one line\n").unwrap();
    let (code, initialized) = run(
        cwd,
        None,
        &[
            "init",
            "./casework",
            "--no-vault",
            "--import-root",
            ".",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 0, "{initialized}");
    assert!(initialized.get("actor_setting").is_none());
    assert!(!cwd.join("casework/local/actor.toml").exists());

    let (code, refused) = start(cwd, None, &[], "Q?");
    assert_eq!(code, 2, "{refused}");
    assert_eq!(refused["error"]["path"], "/actor");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains("--actor"), "{message}");
    assert!(message.contains("init DIR --actor ACTOR"), "{message}");
    let repair = strings(&refused["error"]["repair"]["argv"]);
    let (code, receipt) = run_argv(cwd, &repair);
    assert_eq!(code, 0, "{receipt}");
    assert!(receipt["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn a_malformed_actor_setting_fails_the_write_instead_of_dropping_the_attribution() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    fs::write(cwd.join("source.txt"), "one line\n").unwrap();
    let (code, _) = run(
        cwd,
        None,
        &["init", "./casework", "--no-vault", "--import-root", "."],
    );
    assert_eq!(code, 0);
    fs::write(
        cwd.join("casework/local/actor.toml"),
        "schema_version = \"inv.local-actor.v1\"\nactor = \"not an actor\"\n",
    )
    .unwrap();
    let (code, refused) = start(cwd, None, &[], "Q?");
    assert_ne!(code, 0, "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("local/actor.toml"),
        "{refused}"
    );
    // An explicit actor does not need the setting.
    let (code, receipt) = start(cwd, None, &["--actor", "agent:explicit"], "Q?");
    assert_eq!(code, 0, "{receipt}");
}

#[test]
fn init_takes_its_destination_from_workspace_and_repairs_a_directory_that_holds_files() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    fs::write(cwd.join("source.txt"), "one line\n").unwrap();

    // --workspace X init X names one destination twice; it is accepted.
    let (code, same) = run(
        cwd,
        None,
        &[
            "--workspace",
            "./casework",
            "init",
            "./casework",
            "--import-root",
            ".",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 0, "{same}");
    assert_eq!(same["result"], "initialized");
    // Two different destinations are refused.
    let (code, refused) = run(
        cwd,
        None,
        &["--workspace", "./a", "init", "./b", "--format", "json"],
    );
    assert_eq!(code, 2, "{refused}");
    assert_eq!(refused["error"]["path"], "--workspace");
    // init --workspace DIR with no PATH initializes DIR.
    let (code, flag) = run(
        cwd,
        None,
        &["init", "--workspace", "./other", "--format", "json"],
    );
    assert_eq!(code, 0, "{flag}");
    assert!(cwd.join("other/investigation.toml").is_file());

    // init on the directory that holds the files is refused with a repair
    // that creates a child workspace importing that directory.
    let work = tempfile::tempdir().unwrap();
    let work = work.path();
    fs::write(work.join("source.txt"), "one line\n").unwrap();
    let root = work.to_str().unwrap();
    let (code, refused) = run(
        work,
        None,
        &[
            "init",
            "--workspace",
            root,
            "--actor",
            "agent:investigator",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 2, "{refused}");
    let repair = strings(&refused["error"]["repair"]["argv"]);
    let child = work.join("casework");
    assert!(
        repair.contains(&child.to_str().unwrap().to_owned()),
        "{repair:?}"
    );
    let (code, repaired) = run_argv(work, &repair);
    assert_eq!(code, 0, "{repaired}");
    assert_eq!(
        repaired["import_roots"][0].as_str().unwrap(),
        fs::canonicalize(work).unwrap().to_str().unwrap()
    );
    assert_eq!(repaired["actor_setting"]["actor"], "agent:investigator");

    // With neither PATH nor --workspace, the repair names ./casework.
    let empty = tempfile::tempdir().unwrap();
    let (code, missing) = run(empty.path(), None, &["init", "--format", "json"]);
    assert_eq!(code, 2, "{missing}");
    let repair = strings(&missing["error"]["repair"]["argv"]);
    assert!(repair.contains(&"./casework".to_owned()), "{repair:?}");
}
