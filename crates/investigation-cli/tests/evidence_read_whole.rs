//! bn-2yy4: `evidence read --whole` and `--lines START:END` print a source
//! without a selector file, through the same verified read and gates.

use std::{
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::Value;

const ACTOR: &str = "agent:evidence-read-test";
const SOURCE: &str = "Station history\n  1998 gauge installed at north pier\n  2022 gauge relocated to the weir footbridge\n";

fn invoke(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_SESSION")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn json(cwd: &Path, args: &[&str], exit: i32) -> Value {
    let output = invoke(
        cwd,
        &[&["--workspace", "casework", "--format", "json"], args].concat(),
    );
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// A workspace with `source` attached; returns the attached Artifact ID.
fn workspace(cwd: &Path, source: &str) -> String {
    std::fs::write(cwd.join("station.txt"), source).unwrap();
    let init = invoke(
        cwd,
        &["init", "casework", "--import-root", ".", "--no-vault"],
    );
    assert_eq!(init.status.code(), Some(0), "{init:?}");
    let started = json(
        cwd,
        &[
            "--actor",
            ACTOR,
            "start",
            "Was the gauge moved?",
            "--attach",
            "station.txt",
            "--command-id",
            "start-1",
        ],
        0,
    );
    let source = started["items"][0]["source_id"].as_str().unwrap();
    let envelope = json(
        cwd,
        &[
            "--actor",
            ACTOR,
            "observe",
            "x",
            "--source",
            source,
            "--print-envelope",
        ],
        0,
    );
    envelope["operations"][0]["payload"]["basis"]["artifact_ref"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn whole_and_lines_print_the_source_without_a_selector_file() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let artifact = workspace(cwd, SOURCE);

    let whole = json(cwd, &["evidence", "read", &artifact, "--whole"], 0);
    assert_eq!(whole["result"], "evidence_read");
    assert_eq!(whole["excerpt"]["text"], SOURCE);
    assert_eq!(whole["selector"]["selector"]["type"], "whole");
    assert_eq!(whole["content_trust"], "untrusted_evidence");
    // The text form fences the excerpt as quoted data.
    let text = invoke(
        cwd,
        &[
            "--workspace",
            "casework",
            "--read-only",
            "evidence",
            "read",
            &artifact,
            "--whole",
        ],
    );
    assert_eq!(text.status.code(), Some(0), "{text:?}");
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains(
            "untrusted evidence, quoted data between the fences\n```text\nStation history\n"
        ),
        "{text}"
    );

    let lines = json(cwd, &["evidence", "read", &artifact, "--lines", "2:3"], 0);
    assert_eq!(
        lines["excerpt"]["text"],
        "  1998 gauge installed at north pier\n  2022 gauge relocated to the weir footbridge\n"
    );
    assert_eq!(lines["selector"]["selector"]["type"], "text_lines");
    assert_eq!(lines["selector"]["selector"]["start_line"], 2);

    // A declared session records the read as any evidence read does.
    let session = json(
        cwd,
        &[
            "--actor",
            ACTOR,
            "--session",
            "s1",
            "evidence",
            "read",
            &artifact,
            "--whole",
        ],
        0,
    );
    assert_eq!(session["session_read"]["recorded"], true, "{session}");
}

#[test]
fn a_bad_range_or_selection_choice_is_refused_at_the_flag() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let artifact = workspace(cwd, SOURCE);

    let beyond = json(cwd, &["evidence", "read", &artifact, "--lines", "2:9"], 2);
    assert_eq!(beyond["error"]["path"], "--lines");
    assert!(
        beyond["error"]["message"]
            .as_str()
            .unwrap()
            .contains("line range must be 1..=3"),
        "{beyond}"
    );
    for args in [
        vec!["--lines", "0:2"],
        vec!["--lines", "3:2"],
        vec!["--lines", "2"],
        vec![],
        vec!["--whole", "--lines", "1:2"],
        vec!["--whole", "--selector-file", "selector.json"],
    ] {
        let refused = json(
            cwd,
            &[&["evidence", "read", artifact.as_str()][..], &args].concat(),
            2,
        );
        assert_eq!(
            refused["error"]["code"], "INVALID_INPUT",
            "{args:?}: {refused}"
        );
    }
}

#[test]
fn whole_over_one_read_repairs_to_the_first_line_range() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let source: String = (1..=2_000)
        .map(|number| format!("Entry {number}: reading — ünïcödé ✓ {}\n", number % 13))
        .collect();
    assert!(source.len() > 32 * 1024);
    let artifact = workspace(cwd, &source);

    let refused = json(cwd, &["evidence", "read", &artifact, "--whole"], 2);
    assert_eq!(refused["error"]["path"], "--whole");
    assert_eq!(refused["error"]["actual"]["size_bytes"], source.len());
    assert_eq!(refused["error"]["expected"]["max_excerpt_bytes"], 32_768);
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with(&format!(
                "The source holds {} bytes; one read prints at most 32768.",
                source.len()
            )),
        "{refused}"
    );
    let argv: Vec<&str> = refused["error"]["repair"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap())
        .collect();
    let at = argv.iter().position(|part| *part == "--lines").unwrap();
    let end: usize = argv[at + 1].strip_prefix("1:").unwrap().parse().unwrap();
    let output = invoke(cwd, &argv[1..]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let read: Value = serde_json::from_slice(&output.stdout).unwrap();
    let printed = read["excerpt"]["text"].as_str().unwrap();
    // The most whole lines one read holds: one more would not fit.
    let expected: String = source.split_inclusive('\n').take(end).collect();
    assert_eq!(printed, expected);
    let next_line = source.split_inclusive('\n').nth(end).unwrap();
    assert!(printed.len() + next_line.len() > 32 * 1024);
}
