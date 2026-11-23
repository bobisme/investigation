//! `--read-only` and `INVESTIGATION_READ_ONLY` through the public binary: every
//! mutating or exporting command is refused before it touches the filesystem,
//! and reads behave the same with and without the mode.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::Value;

fn run(cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> (i32, Value) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_READ_ONLY")
        .envs(env.iter().copied())
        .args(args)
        .arg("--json")
        .stdin(Stdio::null());
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value = serde_json::from_str(&stdout).unwrap_or(Value::Null);
    (output.status.code().unwrap(), value)
}

/// Every file and directory beneath `root` with its length.
fn snapshot(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            found.push((entry.path(), meta.len()));
            if meta.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    found.sort();
    found
}

const WRITES: &[&[&str]] = &[
    &["init", "fresh"],
    &["record", "--input", "missing.json"],
    &["ingest", "--input", "missing.json"],
    &["start", "Why?", "--attach", "missing.log"],
    &["capture", "begin", "--input", "missing.json"],
    &["capture", "append", "--input", "missing.json"],
    &["capture", "attach", "--input", "missing.json"],
    &["capture", "finalize", "--input", "missing.json"],
    &["repair", "index"],
    &["observe", "--source", "src_x", "text"],
    &["note", "--on", "obs_x", "text"],
    &["case", "new", "title"],
    &["source", "add", "missing.log"],
    &[
        "--case",
        "case_x",
        "retrospective",
        "--lessons",
        "--consult",
        "k",
    ],
];

#[test]
fn every_write_is_refused_with_a_code_and_alternatives_and_changes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    assert_eq!(run(root, &[], &["init", ".", "--no-vault"]).0, 0);
    let before = snapshot(root);
    for (env, flag) in [
        (&[][..], Some("--read-only")),
        (&[("INVESTIGATION_READ_ONLY", "1")][..], None),
    ] {
        for command in WRITES {
            let mut args: Vec<&str> = flag.into_iter().collect();
            args.extend_from_slice(command);
            let (exit, value) = run(root, env, &args);
            assert_eq!(exit, 2, "{args:?}: {value}");
            let error = &value["error"];
            assert!(
                error["message"]
                    .as_str()
                    .unwrap()
                    .starts_with("READ_ONLY_REFUSED"),
                "{args:?}: {value}"
            );
            assert_eq!(error["path"], "--read-only");
            let alternatives = error["expected"]["read_only_commands"].as_array().unwrap();
            assert!(alternatives.iter().any(|name| name == "show"));
            assert!(!alternatives.iter().any(|name| name == "record"));
            assert_eq!(snapshot(root), before, "{args:?} changed the workspace");
        }
    }
}

#[test]
fn print_envelope_and_reads_behave_the_same_under_the_mode() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    assert_eq!(run(root, &[], &["init", ".", "--no-vault"]).0, 0);
    for args in [
        &["check"][..],
        &["onboard"],
        &["schema", "--list"],
        &["method", "list"],
        &["next"],
        &[
            "--actor",
            "agent:t",
            "note",
            "--on",
            "obs_x",
            "text",
            "--print-envelope",
        ],
    ] {
        let plain = run(root, &[], args);
        let mut guarded = vec!["--read-only"];
        guarded.extend_from_slice(args);
        assert_eq!(run(root, &[], &guarded), plain, "{args:?}");
        assert_eq!(
            run(root, &[("INVESTIGATION_READ_ONLY", "1")], args),
            plain,
            "{args:?}"
        );
    }
    // A falsey environment value leaves writes available.
    let (exit, _) = run(
        root,
        &[("INVESTIGATION_READ_ONLY", "0")],
        &["repair", "index"],
    );
    assert_eq!(exit, 0);
}
