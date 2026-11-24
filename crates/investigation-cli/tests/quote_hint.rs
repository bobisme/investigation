//! W17.07: a refused quote points at the nearest source passage, so the agent
//! can copy the exact text; what is accepted does not change. bn-2yy4: the
//! same for raw `record` requests and sources over one excerpt, and a quote
//! with no near line repairs to an `evidence read` of the whole source.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::{Value, json};

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
    workspace_with(cwd, SOURCE)
}

/// A workspace with `source` attached; returns its Source ID.
fn workspace_with(cwd: &Path, source: &str) -> String {
    std::fs::write(cwd.join("station.txt"), source).unwrap();
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

fn invoke_with(cwd: &Path, args: &[&str], stdin: &[u8]) -> Output {
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
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

/// The `record` envelope observe would send, as a starting point.
fn envelope(cwd: &Path, source: &str, quote: &str, key: &str) -> Value {
    json(
        cwd,
        &[
            "--workspace",
            "casework",
            "--actor",
            ACTOR,
            "observe",
            "The gauge moved.",
            "--source",
            source,
            "--quote",
            quote,
            "--command-id",
            key,
            "--print-envelope",
        ],
        0,
    )
}

fn record(cwd: &Path, envelope: &Value, exit: i32) -> Value {
    let output = invoke_with(
        cwd,
        &[
            "--workspace",
            "casework",
            "--format",
            "json",
            "record",
            "--input",
            "-",
        ],
        envelope.to_string().as_bytes(),
    );
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn argv(failure: &Value) -> Vec<String> {
    failure["error"]["repair"]["argv"]
        .as_array()
        .unwrap_or_else(|| panic!("no repair argv: {failure}"))
        .iter()
        .map(|part| part.as_str().unwrap().to_owned())
        .collect()
}

/// Run a repair argv (without the program name) and return its JSON.
fn run(cwd: &Path, argv: &[String], exit: i32) -> Value {
    assert_eq!(argv[0], "investigation");
    let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    let output = invoke(cwd, &args);
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
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
    assert_eq!(observations(cwd), 0);

    // The repair prints the whole source, with no selector file.
    let argv = argv(&failure);
    assert!(
        argv.windows(2)
            .any(|pair| pair[0] == "evidence" && pair[1] == "read")
            && argv.contains(&"--whole".to_owned()),
        "{argv:?}"
    );
    assert!(
        message.contains(&format!(
            "Read the source with investigation evidence read {} --whole and copy a passage as it stands.",
            argv[argv.iter().position(|part| part == "read").unwrap() + 1]
        )),
        "{message}"
    );
    let read = run(cwd, &argv, 0);
    assert_eq!(read["excerpt"]["text"], SOURCE);
    assert_eq!(read["selector"]["selector"]["type"], "whole");
}

#[test]
fn a_raw_record_quote_names_the_nearest_line_and_the_pointer_to_change() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let source = workspace(cwd);
    let line =
        "2022 gauge relocated to the weir footbridge; readings at the new site are not comparable";
    let mut request = envelope(
        cwd,
        &source,
        "gauge relocated to the weir footbridge",
        "raw",
    );
    // A second operation that is fine; the refused quote is in the second one.
    let mut refused = request["operations"][0].clone();
    refused["payload"]["basis"]["text_quote"]["exact"] =
        json!("2022 gauge relocated to the weir foot bridge; readings at the new site");
    request["operations"].as_array_mut().unwrap().push(refused);
    let failure = record(cwd, &request, 2);
    let message = failure["error"]["message"].as_str().unwrap();
    assert_eq!(
        failure["error"]["path"],
        "/operations/1/payload/basis/text_quote"
    );
    assert!(
        message.contains(&format!(
            "The nearest source passage is line 3: \"{line}\"."
        )),
        "{message}"
    );
    assert!(
        message.contains("set /operations/1/payload/basis/text_quote/exact to that line"),
        "{message}"
    );
    assert_eq!(failure["error"]["expected"]["nearest_source_line"], 3);
    assert_eq!(failure["error"]["expected"]["text"], line);
    // A request is a file: no argv is offered for a near line.
    assert!(
        failure["error"]["repair"].get("argv").is_none(),
        "{failure}"
    );
    assert_eq!(failure["commit_status"], "not_committed");
    assert_eq!(observations(cwd), 0);

    // The corrected request is accepted, and nothing looser is.
    request["operations"][1]["payload"]["basis"]["text_quote"]["exact"] = json!(line);
    record(cwd, &request, 0);
    assert_eq!(observations(cwd), 2);
}

#[test]
fn a_raw_record_quote_with_a_wrong_prefix_says_the_exact_text_is_there() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let source = workspace(cwd);
    let mut request = envelope(cwd, &source, "gauge installed at north pier", "prefix");
    request["operations"][0]["payload"]["basis"]["text_quote"]["prefix"] = json!("2001 ");
    let failure = record(cwd, &request, 2);
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("The exact text is in the source from line 2, so /operations/0/payload/basis/text_quote/prefix or /operations/0/payload/basis/text_quote/suffix does not match the text beside it there."),
        "{message}"
    );
    assert_eq!(observations(cwd), 0);
}

#[test]
fn a_raw_record_quote_inside_text_lines_is_compared_with_those_lines() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let source = workspace(cwd);
    let mut request = envelope(cwd, &source, "Hard requirements", "lines");
    let basis = &mut request["operations"][0]["payload"]["basis"];
    basis["selector"]["selector"] = json!({
        "type":"text_lines","start_line":5,"end_line":6,
        "encoding":"utf-8","parser_version":"inv.text.utf8.v1"
    });
    // Line 2 of the source is near this quote, but outside the selection.
    basis["text_quote"]["exact"] =
        json!("Every singer must be able to get the current score without an online account.");
    let failure = record(cwd, &request, 2);
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("The nearest source passage is line 6: \"H1. Every singer must be able to get the current score without creating any online account.\""),
        "{message}"
    );
    assert_eq!(failure["error"]["expected"]["nearest_source_line"], 6);

    basis_quote(
        &mut request,
        "1998 gauge installed at north pier, east bank",
    );
    let failure = record(cwd, &request, 2);
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("No line of the selected lines 5-6 is near this quote"),
        "{message}"
    );
    assert!(!message.contains("1998"), "{message}");

    // An exact quote outside the selection is named as outside it.
    basis_quote(&mut request, "1998 gauge installed at north pier");
    let failure = record(cwd, &request, 2);
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(
        message.ends_with(" The quote is in the source from line 2, outside the selected lines 5-6. Widen the selector to cover it, or cite the whole source."),
        "{message}"
    );
    assert_eq!(observations(cwd), 0);
}

fn basis_quote(request: &mut Value, quote: &str) {
    request["operations"][0]["payload"]["basis"]["text_quote"]["exact"] = json!(quote);
}

/// A text source of about `bytes` bytes whose lines hold multibyte
/// characters, so chunk boundaries fall inside characters; `marker` replaces
/// line `at`.
fn large_source(lines: usize, at: usize, marker: &str) -> String {
    let mut text = String::new();
    for number in 1..=lines {
        if number == at {
            text.push_str(marker);
        } else {
            text.push_str(&format!(
                "Entry {number}: hourly reading {} cm — ünïcödé ✓ 🌊 noted\n",
                number * 7 % 101
            ));
        }
        if number == at {
            text.push('\n');
        }
    }
    text
}

#[test]
fn a_source_over_one_excerpt_is_read_in_chunks_and_names_the_line() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let marker = "H1. Every singer must be able to get the current score without creating any online account.";
    let text = large_source(4_000, 3_700, marker);
    assert!(text.len() > 200 * 1024 && text.len() < 512 * 1024);
    let source = workspace_with(cwd, &text);
    let failure = observe(
        cwd,
        &source,
        "Hard requirement H1 states that every singer must be able to get the current score without creating any online account.",
        None,
        "large",
    );
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&format!(
            "The nearest source passage is line 3700: \"{marker}\""
        )),
        "{message}"
    );
    let retry = argv(&failure);
    run(cwd, &retry, 0);
    assert_eq!(observations(cwd), 1);

    // With no near line, the repair is the first line range one read prints.
    let failure = observe(
        cwd,
        &source,
        "The river gauge moved to a different bridge.",
        None,
        "large-reworded",
    );
    let repair = argv(&failure);
    let lines_at = repair.iter().position(|part| part == "--lines").unwrap();
    assert!(repair[lines_at + 1].starts_with("1:"), "{repair:?}");
    let read = run(cwd, &repair, 0);
    let printed = read["excerpt"]["text"].as_str().unwrap();
    assert!(text.starts_with(printed) && printed.len() <= 32 * 1024);
    assert!(printed.len() > 32 * 1024 - 200, "{}", printed.len());
}

#[test]
fn a_source_over_the_hint_bound_repairs_to_a_line_range() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let text = large_source(9_000, 1, "Station history");
    assert!(text.len() > 512 * 1024);
    let source = workspace_with(cwd, &text);
    let failure = observe(cwd, &source, "Entry 5: hourly reading 36 cm", None, "huge");
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&format!(
            "The source holds {} bytes, more than the 524288 this hint searches, so no nearest line is named.",
            text.len()
        )),
        "{message}"
    );
    assert!(failure["error"].get("expected").is_none(), "{failure}");
    let repair = argv(&failure);
    assert!(repair.contains(&"--lines".to_owned()), "{repair:?}");
    run(cwd, &repair, 0);
    assert_eq!(observations(cwd), 0);
}

#[test]
fn a_restricted_source_gets_no_passage_and_no_read() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let source = workspace(cwd);
    let request = envelope(cwd, &source, "Station history", "probe");
    let artifact = request["operations"][0]["payload"]["basis"]["artifact_ref"].clone();
    let restrict = json!({
        "schema_version":"inv.command.v1","command_id":"restrict","case_id":request["case_id"],
        "actor":ACTOR,"preconditions":[],"operations":[{"op":"classification.raise","payload":{
            "subject_ref":artifact,"level":"restricted","reason":"Names a customer."}}]
    });
    record(cwd, &restrict, 0);
    let failure = observe(
        cwd,
        &source,
        "The gauge moved.",
        Some("2022 gauge relocated to the weir foot bridge"),
        "restricted",
    );
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("OBSERVATION_TEXT_QUOTE_NOT_FOUND"),
        "{message}"
    );
    assert!(
        !message.contains("weir footbridge") && !message.contains("line 3"),
        "{message}"
    );
    assert!(failure["error"].get("expected").is_none(), "{failure}");
    assert!(failure["error"].get("repair").is_none(), "{failure}");
    // The read the hint would offer is refused the same way.
    let refused = json(
        cwd,
        &[
            "--workspace",
            "casework",
            "evidence",
            "read",
            artifact["id"].as_str().unwrap(),
            "--whole",
        ],
        5,
    );
    assert_eq!(refused["error"]["code"], "GATE_FAILED", "{refused}");
}
