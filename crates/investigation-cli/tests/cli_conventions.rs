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
