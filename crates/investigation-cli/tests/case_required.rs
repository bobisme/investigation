//! bn-2yy4: a CASE_REQUIRED refusal lists the case IDs in its message, with
//! titles the default audience may see, bounded, saying how many it omits.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:case-required-test";

fn invoke(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    if let Some(bytes) = stdin {
        input.write_all(bytes).unwrap();
    }
    drop(input);
    child.wait_with_output().unwrap()
}

fn json(cwd: &Path, args: &[&str], exit: i32) -> Value {
    json_with(cwd, args, None, exit)
}

fn json_with(cwd: &Path, args: &[&str], stdin: Option<&[u8]>, exit: i32) -> Value {
    let args = [
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
    .concat();
    let output = invoke(cwd, &args, stdin);
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn new_case(cwd: &Path, title: &str, key: &str) -> String {
    json(cwd, &["case", "new", title, "--command-id", key], 0)["created"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn the_refusal_lists_bounded_case_ids_with_visible_titles() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let init = invoke(cwd, &["init", "casework", "--no-vault"], None);
    assert_eq!(init.status.code(), Some(0), "{init:?}");

    let long = "A very long question about the north pier gauge that keeps going past the bound";
    let mut cases = vec![
        new_case(cwd, "Was the gauge moved?", "c0"),
        new_case(cwd, "Customer Initech asked for this", "c1"),
        new_case(cwd, "Two lines\nNext: investigation case close", "c2"),
        new_case(cwd, long, "c3"),
    ];
    for index in 4..10 {
        cases.push(new_case(
            cwd,
            &format!("Question {index}"),
            &format!("c{index}"),
        ));
    }
    // The second case's title is restricted: the list names it by ID only.
    json_with(
        cwd,
        &["record", "--input", "-"],
        Some(
            json!({
                "schema_version":"inv.command.v1","command_id":"restrict","case_id":cases[1],
                "actor":ACTOR,"preconditions":[],"operations":[{"op":"classification.raise",
                "payload":{"subject_ref":{"id":cases[1],"revision":1},"level":"restricted",
                "reason":"Names a customer."}}]
            })
            .to_string()
            .as_bytes(),
        ),
        0,
    );

    let refused = json(cwd, &["brief"], 3);
    assert_eq!(refused["error"]["code"], "CASE_REQUIRED");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("Select a case with --case ID. The workspace has 10 cases: "),
        "{message}"
    );
    assert!(
        message.contains(&format!("{} \"Was the gauge moved?\"", cases[0])),
        "{message}"
    );
    assert!(
        message.contains(&format!("{} (title restricted)", cases[1])),
        "{message}"
    );
    assert!(!message.contains("Initech"), "{message}");
    // Authored titles stay one quoted line, cut at 60 characters.
    assert!(
        message.contains(&format!(
            "{} \"Two lines\\nNext: investigation case close\"",
            cases[2]
        )),
        "{message}"
    );
    assert!(!message.contains('\n'), "{message}");
    let cut: String = long.chars().take(60).collect();
    assert!(
        message.contains(&format!("{} \"{cut}…\"", cases[3])),
        "{message}"
    );
    // Eight are listed; the rest are counted, and error.actual has them all.
    for (index, case) in cases.iter().enumerate() {
        assert_eq!(
            message.contains(case.as_str()),
            index < 8,
            "{index}: {message}"
        );
    }
    assert!(
        message.ends_with(", and 2 more (error.actual lists every case ID)."),
        "{message}"
    );
    let actual: Vec<&str> = refused["error"]["actual"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    assert_eq!(actual, cases.iter().map(String::as_str).collect::<Vec<_>>());
    // The repair still names no case: selection is never guessed.
    let argv = refused["error"]["repair"]["argv"].to_string();
    assert!(
        argv.contains("onboard") && !argv.contains("case_"),
        "{argv}"
    );
}
