//! Gaps closed by the CLI conventions audit (docs/cli-conventions-audit.md).

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:conventions-test";

fn invoke(cwd: &Path, args: &[&str], env: &[(&str, &str)], stdin: &[u8]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .envs(env.iter().copied())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = command.spawn().unwrap();
    process.stdin.take().unwrap().write_all(stdin).unwrap();
    process.wait_with_output().unwrap()
}

fn json(cwd: &Path, args: &[&str], exit: i32) -> Value {
    let output = invoke(cwd, &[&["--format", "json"], args].concat(), &[], b"");
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn pretty_is_accepted_everywhere_text_is_and_renders_the_same_text() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let reference = invoke(cwd, &["schema", "--list", "--format", "text"], &[], b"");
    assert_eq!(reference.status.code(), Some(0));
    for (args, env) in [
        (vec!["schema", "--list", "--format", "pretty"], vec![]),
        (vec!["schema", "--list", "--format=pretty"], vec![]),
        (vec!["--format", "pretty", "schema", "--list"], vec![]),
        (vec!["schema", "--list"], vec![("FORMAT", "pretty")]),
        (
            vec!["schema", "--list", "--format", "pretty"],
            vec![("FORMAT", "json")],
        ),
    ] {
        let output = invoke(cwd, &args, &env, b"");
        assert_eq!(output.status.code(), Some(0), "{args:?} {output:?}");
        assert_eq!(text(&output), text(&reference), "{args:?}");
        assert!(!text(&output).contains('\u{1b}'));
    }
    // The last explicit format wins, also across pretty and json.
    let output = invoke(
        cwd,
        &["schema", "--list", "--format", "pretty", "--json"],
        &[],
        b"",
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"], "schema_list");
}

#[test]
fn version_subcommand_matches_the_version_flag_and_stays_out_of_help() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let flag = invoke(cwd, &["--version"], &[], b"");
    let command = invoke(cwd, &["version"], &[], b"");
    assert_eq!(command.status.code(), Some(0));
    assert_eq!(text(&command), text(&flag));
    assert_eq!(json(cwd, &["version"], 0), json(cwd, &["--version"], 0));
    assert_eq!(json(cwd, &["version"], 0)["result"], "version");
    let help = json(cwd, &["--help"], 0);
    assert!(!help["help"].as_str().unwrap().contains("\n  version "));
}

#[test]
fn a_refused_argument_points_at_the_help_of_the_subcommand_it_was_given_to() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let help = |path: &[&str]| {
        let mut argv = vec!["investigation"];
        argv.extend(path);
        argv.extend(["--help", "--format", "json"]);
        json!(argv)
    };
    for (args, path) in [
        (vec!["doctor", "--fix"], vec!["doctor"]),
        (
            vec!["--workspace", "vault", "vault", "render", "--bogus"],
            vec!["vault", "render"],
        ),
        (
            vec!["--case=case_0", "repair", "staging", "--bogus"],
            vec!["repair", "staging"],
        ),
        (vec!["case", "new", "--bogus"], vec!["case", "new"]),
        (vec!["typo", "--bogus"], vec![]),
        (vec!["--bogus", "--", "doctor"], vec![]),
    ] {
        let failure = json(cwd, &args, 2);
        assert_eq!(failure["error"]["code"], "INVALID_INPUT", "{args:?}");
        assert_eq!(failure["error"]["repair"]["argv"], help(&path), "{args:?}");
    }
}

#[test]
fn error_text_is_punctuated_and_names_the_os_reason_not_a_debug_dump() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let failure = json(cwd, &["next"], 3);
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("up to the filesystem root. Run investigation init PATH"),
        "{message}"
    );

    json(
        cwd,
        &["init", "work", "--no-vault", "--import-root", "."],
        0,
    );
    let created = json(
        cwd,
        &[
            "--workspace",
            "work",
            "--actor",
            ACTOR,
            "case",
            "new",
            "Conventions",
            "--command-id",
            "case-1",
        ],
        0,
    );
    let case = created["created"][0]["id"].as_str().unwrap().to_owned();
    let manifest = json!({
        "schema_version": "inv.ingest.v1", "command_id": "ingest-1", "actor": ACTOR,
        "case_id": case, "source": {"title": "missing", "format": "text/plain"},
        "paths": ["missing.txt"],
    });
    let output = invoke(
        cwd,
        &["--workspace", "work", "ingest", "--input", "-"],
        &[],
        manifest.to_string().as_bytes(),
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("No such file or directory"), "{stderr}");
    assert!(!stderr.contains("Os {"), "{stderr}");

    // A failure at the document root has no empty ": " path prefix.
    let output = invoke(
        cwd,
        &["--workspace", "work", "ingest", "--input", "-"],
        &[],
        b"not json",
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.starts_with(':'), "{stderr}");
}

#[test]
fn an_unknown_record_id_repairs_to_the_case_brief() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    json(cwd, &["init", "work", "--no-vault"], 0);
    let created = json(
        cwd,
        &[
            "--workspace",
            "work",
            "--actor",
            ACTOR,
            "case",
            "new",
            "Conventions",
            "--command-id",
            "case-1",
        ],
        0,
    );
    let case = created["created"][0]["id"].as_str().unwrap();
    let unknown = "obs_01a20911-904b-75b0-bc6f-577dad8ef6b2";
    for (command, selector, exit) in [
        ("show", unknown, 3),
        ("explain", unknown, 3),
        ("show", "obs_nothex", 2),
    ] {
        let failure = json(
            cwd,
            &["--workspace", "work", "--case", case, command, selector],
            exit,
        );
        let argv = failure["error"]["repair"]["argv"].as_array().unwrap();
        let tail: Vec<_> = argv.iter().rev().take(4).rev().collect();
        assert_eq!(
            tail,
            [case, "brief", "--format", "json"]
                .map(|part| json!(part))
                .iter()
                .collect::<Vec<_>>(),
            "{command} {selector}: {failure}"
        );
    }
}

#[test]
fn a_text_receipt_for_a_retry_says_already_committed() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    json(cwd, &["init", "work", "--no-vault"], 0);
    let args = [
        "--workspace",
        "work",
        "--actor",
        ACTOR,
        "case",
        "new",
        "Retry",
        "--command-id",
        "case-1",
        "--format",
        "text",
    ];
    let first = text(&invoke(cwd, &args, &[], b""));
    let retry = text(&invoke(cwd, &args, &[], b""));
    let first_line = first.lines().next().unwrap();
    assert!(first_line.starts_with("tx_") && !first_line.contains("already_committed"));
    assert_eq!(
        retry.lines().next().unwrap(),
        format!("{first_line}  already_committed")
    );
}

/// W17.03 help-only arm: a successor ran `cd casework` and then
/// `--workspace ./casework`, was told to run init, and nested a second
/// workspace. The refusal now names the enclosing workspace instead.
#[test]
fn a_relative_workspace_from_inside_a_workspace_names_the_enclosing_one() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path().canonicalize().unwrap();
    let output = invoke(&cwd, &["init", "casework", "--no-vault"], &[], b"");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let inside = cwd.join("casework");
    let root = inside.display().to_string();
    let failure = json(&inside, &["--workspace", "./casework", "brief"], 3);
    assert_eq!(failure["error"]["code"], "WORKSPACE_REQUIRED");
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("inside the workspace {root}")),
        "{message}"
    );
    assert!(!message.contains("Run investigation init"), "{message}");
    assert_eq!(
        failure["error"]["repair"]["argv"],
        json!([
            "investigation",
            "--workspace",
            root,
            "onboard",
            "--format",
            "json"
        ])
    );
    // Outside any workspace the advice is unchanged.
    let elsewhere = json(&cwd, &["--workspace", "./nope", "brief"], 3);
    assert!(
        elsewhere["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Run investigation init PATH")
    );
}

/// W17.03: agents passed prose to `--limitations`; help and the refusal now
/// say that the value is a JSON array of strings and show one.
#[test]
fn a_string_list_flag_says_it_takes_a_json_array_of_strings() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let help = invoke(cwd, &["observe", "--help"], &[], b"");
    assert!(
        text(&help).contains(
            r#"limitations: a JSON array of strings, for example '["first item","second item"]'"#
        ),
        "{help:?}"
    );
    let refused = invoke(
        cwd,
        &[
            "observe",
            "x",
            "--source",
            "src_1",
            "--limitations",
            "only one log",
        ],
        &[],
        b"",
    );
    assert_eq!(refused.status.code(), Some(2));
    let stderr = String::from_utf8(refused.stderr).unwrap();
    assert!(
        stderr.contains("value must be a JSON array of strings"),
        "{stderr}"
    );
}

/// W17.03: most refused quotes in the first-use runs held a typed `\n` for a
/// line break; the refusal now says so.
#[test]
fn a_quote_with_a_typed_newline_is_explained() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    std::fs::write(cwd.join("two.txt"), "line one\nline two\n").unwrap();
    let init = invoke(
        cwd,
        &["init", "casework", "--import-root", ".", "--no-vault"],
        &[],
        b"",
    );
    assert_eq!(init.status.code(), Some(0), "{init:?}");
    let started = json(
        cwd,
        &[
            "--workspace",
            "casework",
            "--actor",
            ACTOR,
            "start",
            "Q",
            "--attach",
            "two.txt",
            "--command-id",
            "start-1",
        ],
        0,
    );
    let source = started["items"][0]["source_id"].as_str().unwrap();
    for (key, quote, advice) in [
        ("q1", r"line one\nline two", "typed \\n"),
        (
            "q2",
            "line one line two",
            "do not shorten, reword or join lines",
        ),
    ] {
        let failure = json(
            cwd,
            &[
                "--workspace",
                "casework",
                "--actor",
                ACTOR,
                "observe",
                "x",
                "--source",
                source,
                "--quote",
                quote,
                "--command-id",
                key,
            ],
            2,
        );
        let message = failure["error"]["message"].as_str().unwrap();
        assert!(
            message.starts_with("OBSERVATION_TEXT_QUOTE_NOT_FOUND"),
            "{message}"
        );
        assert!(message.contains(advice), "{message}");
    }
}

/// W17.03: an investigator in the import root passed `--attach ../source.txt`;
/// the refusal now names the file of that name in the import root and repairs
/// to it.
#[test]
fn a_wrong_relative_attachment_repairs_to_the_file_in_the_import_root() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path().canonicalize().unwrap();
    std::fs::write(cwd.join("source.txt"), "one line\n").unwrap();
    let init = invoke(
        &cwd,
        &["init", "casework", "--import-root", ".", "--no-vault"],
        &[],
        b"",
    );
    assert_eq!(init.status.code(), Some(0), "{init:?}");
    let failure = json(
        &cwd,
        &[
            "--workspace",
            "casework",
            "--actor",
            ACTOR,
            "start",
            "Q",
            "--attach",
            "../source.txt",
            "--command-id",
            "start-1",
        ],
        2,
    );
    let found = cwd.join("source.txt").display().to_string();
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("The file exists at {found}.")),
        "{message}"
    );
    let argv = failure["error"]["repair"]["argv"].as_array().unwrap();
    assert!(argv.iter().any(|arg| arg == &json!(found)), "{argv:?}");
}

/// W17.03: help-only agents passed prose where the command goes, then spent
/// a call on --help. The refusal now shows the two first-use steps; an
/// unknown single word points at the operations.
#[test]
fn prose_in_the_command_position_is_answered_with_the_first_steps() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let prose = json(cwd, &["Initial discovery of the source"], 2);
    let message = prose["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("takes a command first, not free text"),
        "{message}"
    );
    assert!(
        message.contains(
            "investigation init ./casework --actor agent:investigator --no-vault --import-root ."
        ),
        "{message}"
    );
    // With no workspace near, the repair is the first step itself (bn-gef3);
    // beside a workspace it is onboard, which names that workspace's next step.
    assert_eq!(
        prose["error"]["repair"]["argv"],
        json!([
            "investigation",
            "init",
            "./casework",
            "--actor",
            "agent:investigator",
            "--no-vault",
            "--import-root",
            ".",
            "--format",
            "json"
        ])
    );
    std::fs::create_dir(cwd.join("casework")).unwrap();
    std::fs::write(cwd.join("casework/investigation.toml"), "").unwrap();
    let beside = json(cwd, &["Initial discovery of the source"], 2);
    assert_eq!(
        beside["error"]["repair"]["argv"],
        json!(["investigation", "onboard", "--format", "json"])
    );
    // An unknown word points at the operations; its repair stays the root help.
    for word in ["hypothesize", "obseve"] {
        let refused = json(cwd, &[word], 2);
        let message = refused["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("investigation schema --list lists the operations"),
            "{message}"
        );
        assert_eq!(
            refused["error"]["repair"]["argv"],
            json!(["investigation", "--help", "--format", "json"])
        );
    }
    // A group's unknown subcommand is unchanged: its own help.
    let group = json(cwd, &["case", "bogus"], 2);
    assert_eq!(
        group["error"]["repair"]["argv"],
        json!(["investigation", "case", "--help", "--format", "json"])
    );
}

/// W17.03: an init repaired from a non-empty directory names an example actor
/// when the call named none, so the next start is not refused.
#[test]
fn an_init_repair_names_an_actor_when_the_call_named_none() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path().canonicalize().unwrap();
    std::fs::write(cwd.join("source.txt"), "one line\n").unwrap();
    let root = cwd.display().to_string();
    let refused = json(&cwd, &["init", "--workspace", &root], 2);
    let argv = refused["error"]["repair"]["argv"].as_array().unwrap();
    let tail: Vec<_> = argv[argv.len() - 2..].to_vec();
    assert_eq!(
        tail,
        vec![json!("--actor"), json!("agent:investigator")],
        "{argv:?}"
    );
    let named = json(
        &cwd,
        &["init", "--workspace", &root, "--actor", "agent:me"],
        2,
    );
    let argv = named["error"]["repair"]["argv"].as_array().unwrap();
    assert_eq!(
        argv.iter().filter(|arg| *arg == "--actor").count(),
        1,
        "{argv:?}"
    );
}

/// W17.03: source lines that start with "- " were refused as unknown flags
/// when passed as TEXT or --quote, and agents looped on the refusal.
#[test]
fn text_values_may_start_with_a_hyphen() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    std::fs::write(cwd.join("dash.txt"), "- Each line here\nplain line\n").unwrap();
    let init = invoke(
        cwd,
        &["init", "casework", "--import-root", ".", "--no-vault"],
        &[],
        b"",
    );
    assert_eq!(init.status.code(), Some(0), "{init:?}");
    let started = json(
        cwd,
        &[
            "--workspace",
            "casework",
            "--actor",
            ACTOR,
            "start",
            "Q",
            "--attach",
            "dash.txt",
            "--command-id",
            "start-1",
        ],
        0,
    );
    let source = started["items"][0]["source_id"].as_str().unwrap();
    for (index, args) in [
        vec![
            "observe",
            "x",
            "--source",
            source,
            "--quote",
            "- Each line here",
        ],
        vec!["observe", "- Each line here", "--source", source],
        vec!["observe", "--source", source, "- Each line here"],
    ]
    .into_iter()
    .enumerate()
    {
        let key = format!("dash-{index}");
        let mut argv = vec!["--workspace", "casework", "--actor", ACTOR];
        argv.extend(args);
        argv.extend(["--command-id", &key]);
        let receipt = json(cwd, &argv, 0);
        assert_eq!(receipt["result"], "committed", "{receipt}");
    }
}

/// bn-gef3: the run-4 help-only misses. A command written as a flag, a
/// workspace path where the command expects its own argument, and a file
/// given to `source` without `add` each get a refusal whose repair is the
/// command that works; `source add` in a workspace with no case repairs to
/// `start` with the same file.
#[test]
fn first_use_slips_of_the_help_only_runs_repair_to_the_working_command() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path().canonicalize().unwrap();
    let cwd = cwd.as_path();
    std::fs::write(cwd.join("source.txt"), "one line\n").unwrap();

    let flag = json(cwd, &["--init", "Investigation into the overbaking"], 2);
    let message = flag["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("--init is not an option: init is a command"),
        "{message}"
    );
    assert_eq!(flag["error"]["repair"]["argv"][1], json!("init"));
    let other = json(cwd, &["--brief"], 2);
    assert_eq!(
        other["error"]["repair"]["argv"],
        json!(["investigation", "brief", "--help", "--format", "json"])
    );
    // An unknown flag that names no command keeps the generic refusal.
    let bogus = json(cwd, &["--bogus"], 2);
    assert_eq!(
        bogus["error"]["repair"]["argv"],
        json!(["investigation", "--help", "--format", "json"])
    );

    let init = invoke(
        cwd,
        &[
            "init",
            "ws",
            "--no-vault",
            "--import-root",
            ".",
            "--format",
            "json",
        ],
        &[],
        b"",
    );
    assert_eq!(init.status.code(), Some(0), "{init:?}");
    let source = cwd.join("source.txt").display().to_string();
    let positional = json(
        cwd,
        &[
            "start",
            "ws",
            "--actor",
            ACTOR,
            "What happened?",
            "--attach",
            &source,
        ],
        2,
    );
    let repair: Vec<String> =
        serde_json::from_value(positional["error"]["repair"]["argv"].clone()).unwrap();
    assert_eq!(
        repair,
        [
            "investigation",
            "--workspace",
            "ws",
            "--format",
            "json",
            "start",
            "--actor",
            ACTOR,
            "What happened?",
            "--attach",
            &source
        ]
    );
    assert!(
        positional["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("ws is a workspace"),
        "{positional}"
    );

    let bare = json(cwd, &["--workspace", "ws", "source", &source], 2);
    let repair: Vec<String> =
        serde_json::from_value(bare["error"]["repair"]["argv"].clone()).unwrap();
    assert_eq!(
        repair,
        [
            "investigation",
            "--format",
            "json",
            "--workspace",
            "ws",
            "source",
            "add",
            &source
        ]
    );
    let no_case = json(
        cwd,
        &[
            "--workspace",
            "ws",
            "--actor",
            ACTOR,
            "source",
            "add",
            &source,
        ],
        3,
    );
    let repair: Vec<String> =
        serde_json::from_value(no_case["error"]["repair"]["argv"].clone()).unwrap();
    assert_eq!(
        &repair[3..],
        [
            "--actor", ACTOR, "start", "QUESTION", "--attach", &source, "--format", "json"
        ]
    );
    // The repaired start runs.
    let mut start = repair[1..].to_vec();
    let question = start.iter().position(|word| word == "QUESTION").unwrap();
    start[question] = "What happened?".to_owned();
    let started = invoke(
        cwd,
        &start.iter().map(String::as_str).collect::<Vec<_>>(),
        &[],
        b"",
    );
    assert_eq!(started.status.code(), Some(0), "{started:?}");
}
