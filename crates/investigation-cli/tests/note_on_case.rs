//! W17.08: `note TEXT` without `--on` annotates the selected case, and the
//! receipt says so. In the W17.03 help-only runs agents wanted a case-level
//! note and were refused for the missing `--on` in 5 of 12 assignments.

use std::{
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::Value;

const ACTOR: &str = "agent:note-test";

fn invoke(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
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
        &[
            &[
                "--workspace",
                "casework",
                "--actor",
                ACTOR,
                "--format",
                "json",
            ],
            args,
        ]
        .concat(),
    );
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// A workspace with one case opened by `start`; returns the case ID.
fn one_case(cwd: &Path) -> String {
    std::fs::write(cwd.join("log.txt"), "Mon 29 Apr rye batch dark crust\n").unwrap();
    let init = invoke(
        cwd,
        &["init", "casework", "--import-root", ".", "--no-vault"],
    );
    assert_eq!(init.status.code(), Some(0), "{init:?}");
    json(
        cwd,
        &[
            "start",
            "Why are Monday batches overbaked?",
            "--attach",
            "log.txt",
            "--command-id",
            "start-1",
        ],
        0,
    );
    let shown = json(cwd, &["brief"], 0).to_string();
    let start = shown.find("\"case_").unwrap() + 1;
    shown[start..].split('"').next().unwrap().to_owned()
}

fn subject_of(cwd: &Path, case: &str, annotation: &str) -> (Value, Value) {
    let shown = json(
        cwd,
        &["--case", case, "show", annotation, "--depth", "full"],
        0,
    );
    let body = &shown["record"]["body"];
    (body["subject_refs"][0]["id"].clone(), body["role"].clone())
}

#[test]
fn the_first_note_on_a_one_case_workspace_is_on_that_case() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let case = one_case(cwd);
    let receipt = json(
        cwd,
        &[
            "note",
            "Initial analysis of Monday overbaking evidence",
            "--command-id",
            "note-1",
        ],
        0,
    );
    assert_eq!(receipt["result"], "committed");
    let warning = format!("--on was omitted, so the note is on the selected case {case}.");
    assert_eq!(receipt["warnings"][0], warning.as_str());
    let annotation = receipt["created"][0]["id"].as_str().unwrap();
    assert!(annotation.starts_with("ann_"), "{receipt}");
    let (subject, role) = subject_of(cwd, &case, annotation);
    assert_eq!(subject, case.as_str());
    // Still a note: no evidentiary standing.
    assert_eq!(role, "note");

    // The text receipt says so too, and a retry says it again.
    let retry = invoke(
        cwd,
        &[
            "--workspace",
            "casework",
            "--actor",
            ACTOR,
            "note",
            "Initial analysis of Monday overbaking evidence",
            "--command-id",
            "note-1",
        ],
    );
    assert_eq!(retry.status.code(), Some(0), "{retry:?}");
    let text = String::from_utf8(retry.stdout).unwrap();
    assert!(text.contains("already_committed"), "{text}");
    assert!(text.contains(&format!("Warning: {warning}")), "{text}");
}

#[test]
fn an_explicit_on_still_names_the_subject_and_says_nothing_about_the_case() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let case = one_case(cwd);
    let first = json(cwd, &["note", "Case-level note", "--command-id", "n1"], 0);
    let earlier = first["created"][0]["id"].as_str().unwrap();
    let receipt = json(
        cwd,
        &[
            "note",
            "A note on the note",
            "--on",
            earlier,
            "--command-id",
            "n2",
        ],
        0,
    );
    assert_eq!(
        receipt["warnings"].as_array().unwrap().len(),
        0,
        "{receipt}"
    );
    let annotation = receipt["created"][0]["id"].as_str().unwrap();
    assert_eq!(subject_of(cwd, &case, annotation).0, earlier);
    let on_case = json(
        cwd,
        &[
            "note",
            "Same as the default",
            "--on",
            &case,
            "--command-id",
            "n3",
        ],
        0,
    );
    assert_eq!(on_case["warnings"].as_array().unwrap().len(), 0);
}

#[test]
fn with_several_cases_case_selects_the_subject() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let first = one_case(cwd);
    let second = json(
        cwd,
        &["case", "new", "Second question", "--command-id", "c2"],
        0,
    );
    let second = second["created"][0]["id"].as_str().unwrap().to_owned();

    // Without --case the case is not guessed; the refusal says how.
    let refused = json(cwd, &["note", "Which case?", "--command-id", "n1"], 3);
    assert_eq!(refused["error"]["code"], "CASE_REQUIRED");
    assert_eq!(refused["error"]["message"], "Select a case with --case ID.");
    let cases = refused["error"]["actual"].to_string();
    assert!(cases.contains(&first) && cases.contains(&second), "{cases}");

    let receipt = json(
        cwd,
        &[
            "--case",
            &second,
            "note",
            "On the second case",
            "--command-id",
            "n2",
        ],
        0,
    );
    let annotation = receipt["created"][0]["id"].as_str().unwrap();
    assert_eq!(subject_of(cwd, &second, annotation).0, second.as_str());
    assert!(
        receipt["warnings"][0]
            .as_str()
            .unwrap()
            .ends_with(&format!("selected case {second}.")),
        "{receipt}"
    );
}

#[test]
fn a_lone_unknown_option_is_refused_rather_than_noted() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    one_case(cwd);
    // With --on optional, `note --typo` would otherwise be a note reading
    // "--typo". Prose that starts with a hyphen is still text.
    let refused = json(cwd, &["note", "--typo"], 2);
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unexpected argument --typo"),
        "{refused}"
    );
    let bullet = json(cwd, &["note", "- check the clock", "--command-id", "b"], 0);
    assert_eq!(bullet["result"], "committed");
}
