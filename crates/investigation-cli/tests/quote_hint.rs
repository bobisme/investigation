//! W17.07: a refused quote points at the nearest source passage, so the agent
//! can copy the exact text; what is accepted does not change.

use std::{
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::Value;

const ACTOR: &str = "agent:quote-hint-test";

const SOURCE: &str = "Station history\n  1998 gauge installed at north pier\n  2022 gauge relocated to the weir footbridge; readings at the new site are not comparable\nwithout a published datum adjustment.\nHard requirements\nH1. Every singer must be able to get the current score without creating any online account.\n";

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
    let output = invoke(cwd, &[&["--format", "json"], args].concat());
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// A workspace with SOURCE attached; returns its Source ID.
fn workspace(cwd: &Path) -> String {
    std::fs::write(cwd.join("station.txt"), SOURCE).unwrap();
    let init = invoke(
        cwd,
        &["init", "casework", "--import-root", ".", "--no-vault"],
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
            "Was the gauge moved?",
            "--attach",
            "station.txt",
            "--command-id",
            "start-1",
        ],
        0,
    );
    started["items"][0]["source_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn observe(cwd: &Path, source: &str, statement: &str, quote: Option<&str>, key: &str) -> Value {
    let mut args = vec![
        "--workspace",
        "casework",
        "--actor",
        ACTOR,
        "observe",
        statement,
        "--source",
        source,
        "--command-id",
        key,
    ];
    if let Some(quote) = quote {
        args.extend(["--quote", quote]);
    }
    json(cwd, &args, 2)
}

fn observations(cwd: &Path) -> usize {
    let brief = json(cwd, &["--workspace", "casework", "brief"], 0).to_string();
    let mut ids: Vec<_> = brief
        .match_indices("\"obs_")
        .filter_map(|(start, _)| brief[start + 1..].split('"').next())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids.len()
}

#[test]
fn a_joined_lines_quote_names_the_line_it_starts_on_and_the_retry_is_accepted() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let source = workspace(cwd);
    let line =
        "2022 gauge relocated to the weir footbridge; readings at the new site are not comparable";
    let failure = observe(
        cwd,
        &source,
        "The gauge was relocated in 2022.",
        Some("readings at the new site are not comparable without a published datum adjustment."),
        "joined",
    );
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("OBSERVATION_TEXT_QUOTE_NOT_FOUND"),
        "{message}"
    );
    assert!(
        message.contains("The first 43 of 81 characters of the quote are in the source, from line 3, where the source continues with a line break."),
        "{message}"
    );
    assert!(
        message.contains(&format!("The nearest source passage is line 3: \"{line}\"")),
        "{message}"
    );
    assert_eq!(failure["error"]["expected"]["nearest_source_line"], 3);
    assert_eq!(failure["error"]["expected"]["text"], line);
    // Nothing was accepted: the refusal is advice, never a fuzzy match.
    assert_eq!(failure["commit_status"], "not_committed");
    assert_eq!(observations(cwd), 0);

    // The repair repeats the call with the exact line, and it is accepted.
    let argv: Vec<String> = failure["error"]["repair"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(argv[0], "investigation");
    let quote_at = argv.iter().position(|part| part == "--quote").unwrap();
    assert_eq!(argv[quote_at + 1], line);
    let retry: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    let accepted = invoke(cwd, &retry);
    assert_eq!(accepted.status.code(), Some(0), "{accepted:?}");
    assert_eq!(observations(cwd), 1);
}

#[test]
fn a_lightly_paraphrased_quote_names_its_line() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let source = workspace(cwd);
    // TEXT doubles as the quote: the agent restated the requirement.
    let failure = observe(
        cwd,
        &source,
        "Hard requirement H1 states that every singer must be able to get the current score without creating any online account.",
        None,
        "paraphrase",
    );
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("The nearest source passage is line 6: \"H1. Every singer must be able to get the current score without creating any online account.\""),
        "{message}"
    );
    assert!(!message.contains("The first "), "{message}");
    let argv = failure["error"]["repair"]["argv"].as_array().unwrap();
    assert!(argv.iter().any(|part| part
        == "H1. Every singer must be able to get the current score without creating any online account."));
}

#[test]
fn a_reworded_quote_with_no_near_line_names_none() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let source = workspace(cwd);
    let failure = observe(
        cwd,
        &source,
        "The river gauge moved to a different bridge.",
        None,
        "reworded",
    );
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("No line of the source is near this quote, so it reads as reworded"),
        "{message}"
    );
    assert!(failure["error"].get("expected").is_none(), "{failure}");
    assert!(failure["error"].get("repair").is_none(), "{failure}");
    assert_eq!(observations(cwd), 0);
}
