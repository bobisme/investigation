//! bn-3qkp: every read a refusal or explain prints for an original Artifact
//! runs as printed. A line or quote selection of an original reads its
//! recorded text derivative: the same digest-checked bytes, through the same
//! gates, and the output names the derivative it read.
//!
//! Each test takes the argv a response printed and executes it unchanged.
//!
//! bn-1xnl: a quote under a `whole` or `byte_range` selector over an
//! original is verified through the same derivative, and a refusal carries
//! the nearest-passage hint.

use std::{
    io::Write as _,
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:repair-test";

fn invoke(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_SESSION")
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

fn json(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> (i32, Value) {
    let output = invoke(
        cwd,
        &[&["--workspace", "casework", "--format", "json"], args].concat(),
        stdin,
    );
    let value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{args:?}: {output:?}"));
    (output.status.code().unwrap(), value)
}

/// Run a printed argv exactly as printed; it must succeed.
fn runs_as_printed(cwd: &Path, argv: &Value, stdin: Option<&Value>) -> Value {
    let argv: Vec<&str> = argv
        .as_array()
        .unwrap_or_else(|| panic!("no argv: {argv}"))
        .iter()
        .map(|part| part.as_str().unwrap())
        .collect();
    assert_eq!(argv[0], "investigation", "{argv:?}");
    let bytes = stdin.map(|value| serde_json::to_vec(value).unwrap());
    let output = invoke(cwd, &argv[1..], bytes.as_deref());
    let value: Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{argv:?}: {output:?}"));
    assert_eq!(
        output.status.code(),
        Some(0),
        "{argv:?} printed, then: {value}"
    );
    assert_eq!(value["ok"], true, "{argv:?}: {value}");
    value
}

struct Fixture {
    source: String,
    case: String,
    source_id: String,
    original: String,
    derivative: String,
    digest: String,
}

/// A workspace holding a text source larger than one read.
fn fixture(cwd: &Path) -> Fixture {
    let source: String = (1..=1_000)
        .map(|number| {
            format!(
                "Entry {number:04}: gauge reading {} at the weir footbridge\n",
                number % 17
            )
        })
        .collect();
    assert!(source.len() > 32 * 1024);
    std::fs::write(cwd.join("station.txt"), &source).unwrap();
    let init = invoke(
        cwd,
        &["init", "casework", "--import-root", ".", "--no-vault"],
        None,
    );
    assert_eq!(init.status.code(), Some(0), "{init:?}");
    let (code, started) = json(
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
        None,
    );
    assert_eq!(code, 0, "{started}");
    let item = &started["items"][0];
    let text = |value: &Value| value.as_str().unwrap().to_owned();
    Fixture {
        source,
        case: text(&started["created"][0]["id"]),
        source_id: text(&item["source_id"]),
        original: text(&item["artifact_id"]),
        derivative: text(&item["derivative_id"]),
        digest: text(&item["digest"]),
    }
}

impl Fixture {
    fn lines(&self, start: usize, end: usize) -> String {
        self.source
            .split_inclusive('\n')
            .skip(start - 1)
            .take(end + 1 - start)
            .collect()
    }

    /// An `observation.add` envelope whose basis cites the original Artifact
    /// with a `text_lines` selector and `quote`.
    fn observation_of_original(
        &self,
        cwd: &Path,
        key: &str,
        lines: (u64, u64),
        quote: &str,
    ) -> Value {
        self.observation_of_original_under(
            cwd,
            key,
            json!({
                "type": "text_lines", "start_line": lines.0, "end_line": lines.1,
                "encoding": "utf-8", "parser_version": "inv.text.utf8.v1",
            }),
            quote,
        )
    }

    /// An `observation.add` envelope whose basis cites the original Artifact
    /// with `selector` and `quote`.
    fn observation_of_original_under(
        &self,
        cwd: &Path,
        key: &str,
        selector: Value,
        quote: &str,
    ) -> Value {
        let (code, mut envelope) = json(
            cwd,
            &[
                "--actor",
                ACTOR,
                "--case",
                &self.case,
                "observe",
                "The gauge reading is recorded",
                "--source",
                &self.source_id,
                "--quote",
                "Entry 0002",
                "--print-envelope",
            ],
            None,
        );
        assert_eq!(code, 0, "{envelope}");
        envelope["command_id"] = json!(key);
        envelope["operations"][0]["payload"]["basis"] = json!({
            "kind": "source",
            "artifact_ref": {"id": self.original, "revision": 1},
            "selector": {
                "schema_version": "inv.selector.v1",
                "artifact_digest": self.digest,
                "selector": selector,
            },
            "text_quote": {"exact": quote, "prefix": null, "suffix": null},
        });
        envelope
    }
}

/// Every value under `key` anywhere in `value`.
fn find_all<'a>(value: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            for (name, child) in map {
                if name == key {
                    out.push(child);
                }
                find_all(child, key, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| find_all(item, key, out)),
        _ => {}
    }
}

#[test]
fn lines_of_an_original_read_its_text_derivative_and_say_so() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let f = fixture(cwd);
    let (code, read) = json(
        cwd,
        &["evidence", "read", &f.original, "--lines", "2:3"],
        None,
    );
    assert_eq!(code, 0, "{read}");
    assert_eq!(read["excerpt"]["text"], f.lines(2, 3));
    assert_eq!(read["artifact"]["id"], f.original);
    assert_eq!(read["text_derivative"]["id"], f.derivative);
    // The derivative read as itself names no derivative.
    let (code, own) = json(
        cwd,
        &["evidence", "read", &f.derivative, "--lines", "2:3"],
        None,
    );
    assert_eq!(code, 0, "{own}");
    assert_eq!(own["excerpt"]["text"], f.lines(2, 3));
    assert!(own.get("text_derivative").is_none(), "{own}");
    let output = invoke(
        cwd,
        &[
            "--workspace",
            "casework",
            "evidence",
            "read",
            &f.original,
            "--lines",
            "2:3",
        ],
        None,
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains(&format!("read through text derivative {}", f.derivative)),
        "{text}"
    );
}

#[test]
fn every_printed_read_repair_for_an_original_runs_as_printed() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let f = fixture(cwd);
    for (args, path) in [
        // Too large for one read: repaired to the first lines one read holds.
        (vec!["--whole"], "--whole"),
        // A start past the last line: repaired to the last lines one read holds.
        (vec!["--lines", "1200:1300"], "--lines"),
        // A range over one read: repaired to the longest range that fits.
        (vec!["--lines", "1:900"], "--lines"),
    ] {
        let (code, refused) = json(
            cwd,
            &[&["evidence", "read", f.original.as_str()][..], &args].concat(),
            None,
        );
        assert_eq!(code, 2, "{args:?}: {refused}");
        assert_eq!(refused["error"]["path"], path, "{refused}");
        let read = runs_as_printed(cwd, &refused["error"]["repair"]["argv"], None);
        assert_eq!(read["text_derivative"]["id"], f.derivative, "{read}");
        let printed = read["excerpt"]["text"].as_str().unwrap();
        assert!(printed.len() <= 32 * 1024 && !printed.is_empty());
        assert!(f.source.contains(printed));
    }
    // An end past the last line reads to the last line and says so.
    let (code, clamped) = json(
        cwd,
        &["evidence", "read", &f.original, "--lines", "995:1200"],
        None,
    );
    assert_eq!(code, 0, "{clamped}");
    assert_eq!(clamped["lines"]["read"], "995:1000");
    assert_eq!(clamped["excerpt"]["text"], f.lines(995, 1000));
    assert!(clamped["note"].as_str().unwrap().contains("1000 line(s)"));
}

#[test]
fn explain_prints_a_read_of_an_original_line_citation_that_runs() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let f = fixture(cwd);
    let quote = f.lines(2, 2).trim_end().to_owned();
    let envelope = f.observation_of_original(cwd, "observe-original", (2, 3), &quote);
    let (code, receipt) = json(
        cwd,
        &["record", "--input", "-"],
        Some(&serde_json::to_vec(&envelope).unwrap()),
    );
    assert_eq!(code, 0, "{receipt}");
    let observation = receipt["created"]
        .as_array()
        .unwrap()
        .iter()
        .find(|change| change["id"].as_str().unwrap().starts_with("obs_"))
        .map(|change| change["id"].as_str().unwrap().to_owned())
        .expect("an Observation");

    let (code, explained) = json(cwd, &["--case", &f.case, "explain", &observation], None);
    assert_eq!(code, 0, "{explained}");
    let mut reads = Vec::new();
    find_all(&explained, "evidence_read", &mut reads);
    let read = reads
        .into_iter()
        .find(|read| read["stdin"]["selector"]["type"] == "text_lines")
        .unwrap_or_else(|| panic!("no line read in {explained}"));
    // The line selector resolved at acceptance, so the quote was checked.
    let mut states = Vec::new();
    find_all(&explained, "quote_state", &mut states);
    assert!(
        states.iter().any(|state| **state == "verified"),
        "{explained}"
    );
    let printed = runs_as_printed(cwd, &read["argv"], Some(&read["stdin"]));
    assert_eq!(printed["excerpt"]["text"], f.lines(2, 3));
    assert_eq!(printed["text_derivative"]["id"], f.derivative);
}

#[test]
fn the_reworded_quote_repair_for_an_original_runs_as_printed() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let f = fixture(cwd);
    let envelope = f.observation_of_original(
        cwd,
        "observe-reworded",
        (1, 3),
        "Völlig anders formuliert, ohne jeden Bezug zur Quelle",
    );
    let (code, refused) = json(
        cwd,
        &["record", "--input", "-"],
        Some(&serde_json::to_vec(&envelope).unwrap()),
    );
    assert_ne!(code, 0, "{refused}");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains("reads as reworded"), "{refused}");
    assert!(message.contains(&f.original), "{refused}");
    let read = runs_as_printed(cwd, &refused["error"]["repair"]["argv"], None);
    assert_eq!(read["text_derivative"]["id"], f.derivative);
    assert!(
        f.source
            .starts_with(read["excerpt"]["text"].as_str().unwrap())
    );
}

/// Record `envelope` and return the exit code and response.
fn record(cwd: &Path, envelope: &Value) -> (i32, Value) {
    json(
        cwd,
        &["record", "--input", "-"],
        Some(&serde_json::to_vec(envelope).unwrap()),
    )
}

/// The quote state recorded on the Observation a receipt created.
fn recorded_quote_state(cwd: &Path, receipt: &Value) -> Value {
    let id = receipt["created"][0]["id"].as_str().unwrap();
    let (code, shown) = json(cwd, &["show", id, "--depth", "full"], None);
    assert_eq!(code, 0, "{shown}");
    let mut states = Vec::new();
    find_all(&shown, "text_quote", &mut states);
    states
        .iter()
        .find_map(|quote| quote.get("state").cloned())
        .unwrap_or_else(|| panic!("no recorded quote state: {shown}"))
}

/// bn-1xnl: a quote under a `whole` or `byte_range` selector over an
/// original is checked in the text its recorded derivative decodes, the same
/// lookup and gates as a line selector; before, it was recorded unchecked as
/// `unavailable`.
#[test]
fn a_quote_under_whole_or_byte_range_over_an_original_is_verified() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let f = fixture(cwd);
    let line = |number: usize| f.lines(number, number).trim_end().to_owned();
    let start = f.lines(1, 4).len();
    let end = f.lines(1, 6).len();
    for (key, selector, quote) in [
        ("whole-found", json!({"type": "whole"}), line(700)),
        (
            "range-found",
            json!({"type": "byte_range", "start": start, "end": end}),
            line(6),
        ),
    ] {
        let envelope = f.observation_of_original_under(cwd, key, selector, &quote);
        let (code, receipt) = record(cwd, &envelope);
        assert_eq!(code, 0, "{key}: {receipt}");
        assert_eq!(recorded_quote_state(cwd, &receipt), "verified", "{key}");
    }
}

#[test]
fn a_quote_the_whole_original_does_not_hold_is_refused_with_its_nearest_line() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let f = fixture(cwd);
    // One word changed in line 700.
    let near = f.lines(700, 700).trim_end().replace("weir", "wier");
    let envelope =
        f.observation_of_original_under(cwd, "whole-near", json!({"type": "whole"}), &near);
    let (code, refused) = record(cwd, &envelope);
    assert_ne!(code, 0, "{refused}");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("OBSERVATION_TEXT_QUOTE_NOT_FOUND"),
        "{refused}"
    );
    assert_eq!(
        refused["error"]["expected"]["nearest_source_line"], 700,
        "{refused}"
    );
    // A reworded quote gets the read of the source, which runs as printed.
    let envelope = f.observation_of_original_under(
        cwd,
        "whole-reworded",
        json!({"type": "whole"}),
        "Völlig anders formuliert, ohne jeden Bezug zur Quelle",
    );
    let (code, refused) = record(cwd, &envelope);
    assert_ne!(code, 0, "{refused}");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains("reads as reworded"), "{refused}");
    let read = runs_as_printed(cwd, &refused["error"]["repair"]["argv"], None);
    assert_eq!(read["text_derivative"]["id"], f.derivative);
}

#[test]
fn a_quote_outside_an_original_byte_range_is_refused_and_located() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let f = fixture(cwd);
    let (start, end) = (f.lines(1, 4).len(), f.lines(1, 6).len());
    let range = json!({"type": "byte_range", "start": start, "end": end});
    // In the source at line 9, outside the range of lines 5-6.
    let outside = f.lines(9, 9).trim_end().to_owned();
    let envelope = f.observation_of_original_under(cwd, "range-outside", range.clone(), &outside);
    let (code, refused) = record(cwd, &envelope);
    assert_ne!(code, 0, "{refused}");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("The quote is in the source from line 9, outside the selected lines 5-6."),
        "{refused}"
    );
    // A near miss inside the range names its line in the source.
    let near = f.lines(6, 6).trim_end().replace("weir", "wier");
    let envelope = f.observation_of_original_under(cwd, "range-near", range, &near);
    let (code, refused) = record(cwd, &envelope);
    assert_ne!(code, 0, "{refused}");
    assert_eq!(
        refused["error"]["expected"]["nearest_source_line"], 6,
        "{refused}"
    );
}

/// A byte range over an original may cut a character of its decoded text:
/// the quote is looked for among the characters the range holds entirely.
#[test]
fn a_byte_range_that_cuts_a_character_verifies_the_characters_it_holds() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let source = "Größe des Pegels: 41 cm über Null\n";
    std::fs::write(cwd.join("pegel.txt"), source).unwrap();
    let init = invoke(
        cwd,
        &["init", "casework", "--import-root", ".", "--no-vault"],
        None,
    );
    assert_eq!(init.status.code(), Some(0), "{init:?}");
    let (code, started) = json(
        cwd,
        &[
            "--actor",
            ACTOR,
            "start",
            "Wie hoch?",
            "--attach",
            "pegel.txt",
        ],
        None,
    );
    assert_eq!(code, 0, "{started}");
    let item = &started["items"][0];
    let f = Fixture {
        source: source.to_owned(),
        case: started["created"][0]["id"].as_str().unwrap().to_owned(),
        source_id: item["source_id"].as_str().unwrap().to_owned(),
        original: item["artifact_id"].as_str().unwrap().to_owned(),
        derivative: item["derivative_id"].as_str().unwrap().to_owned(),
        digest: item["digest"].as_str().unwrap().to_owned(),
    };
    // Bytes 3..: the second byte of "ö" onwards; the range ends inside "ü".
    let cut = source.find("über").unwrap() + 1;
    assert!(!source.is_char_boundary(3) && !source.is_char_boundary(cut));
    let range = json!({"type": "byte_range", "start": 3, "end": cut});
    for (key, quote, verified) in [
        ("cut-held", "e des Pegels: 41 cm", true),
        ("cut-first", "öße", false),
        ("cut-last", "cm ü", false),
    ] {
        let envelope = f.observation_of_original_under(cwd, key, range.clone(), quote);
        let (code, receipt) = record(cwd, &envelope);
        if verified {
            assert_eq!(code, 0, "{key}: {receipt}");
            assert_eq!(recorded_quote_state(cwd, &receipt), "verified", "{key}");
        } else {
            assert_ne!(code, 0, "{key}: {receipt}");
            assert!(
                receipt["error"]["message"]
                    .as_str()
                    .unwrap()
                    .starts_with("OBSERVATION_TEXT_QUOTE_NOT_FOUND"),
                "{key}: {receipt}"
            );
        }
    }
}
