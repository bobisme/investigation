//! W17.10: a successor citation from `brief` alone. Each Observation a brief
//! shows carries its citation: the source, the line span of its quote in that
//! source, the exact quote and the quote state as recorded. Only a verified
//! quote is located and called verified; restricted content is never shown.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:citation-test";

/// The source a first-use investigator attaches. Line 4 holds the decisive
/// reading; lines 6 and 7 hold a two-line passage; "(old site)" occurs twice.
const SOURCE: &str = "\
Station history
  2019  5.6  (old site)
  2020  5.9  (old site)
  2024  6.1  (new site, provisional; data not yet quality checked)

Provisional values can change after quality checks.
We do not describe a reading as a record until the series is adjusted.
";

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>) -> (String, bool) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut process = command.spawn().unwrap();
    if let Some(stdin) = stdin {
        let mut pipe = process.stdin.take().unwrap();
        pipe.write_all(&serde_json::to_vec(stdin).unwrap()).unwrap();
    }
    let output = process.wait_with_output().unwrap();
    (
        String::from_utf8(output.stdout).unwrap(),
        output.status.success(),
    )
}

fn ok(cwd: &Path, args: &[&str], stdin: Option<&Value>) -> String {
    let (out, success) = run(cwd, args, stdin);
    assert!(success, "{args:?}\n{out}");
    out
}

fn json_of(cwd: &Path, args: &[&str]) -> Value {
    let mut argv = args.to_vec();
    argv.extend(["--format", "json"]);
    serde_json::from_str(&ok(cwd, &argv, None)).unwrap()
}

/// A first-use workspace: `init` with an import root, then `start --attach`.
struct FirstUse {
    _dir: tempfile::TempDir,
    root: PathBuf,
    case: String,
    source: String,
    original: String,
    text: String,
    digest: Value,
    step: usize,
}

impl FirstUse {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("source.txt"), SOURCE).unwrap();
        ok(
            &root,
            &[
                "init",
                "casework",
                "--no-vault",
                "--import-root",
                ".",
                "--actor",
                ACTOR,
                "--format",
                "json",
            ],
            None,
        );
        let started = json_of(
            &root,
            &[
                "--workspace",
                "casework",
                "--actor",
                ACTOR,
                "start",
                "Did the river reach a record?",
                "--attach",
                "source.txt",
                "--command-id",
                "start",
            ],
        );
        let item = &started["items"][0];
        Self {
            case: started["created"][0]["id"].as_str().unwrap().to_owned(),
            source: item["source_id"].as_str().unwrap().to_owned(),
            original: item["artifact_id"].as_str().unwrap().to_owned(),
            text: item["derivative_id"].as_str().unwrap().to_owned(),
            digest: item["digest"].clone(),
            root,
            _dir: dir,
            step: 0,
        }
    }

    fn workspace(&self) -> PathBuf {
        self.root.join("casework")
    }

    fn args<'a>(&'a self, tail: &[&'a str]) -> Vec<&'a str> {
        let mut args = vec!["--workspace", "casework", "--actor", ACTOR, "--case"];
        args.push(&self.case);
        args.extend(tail);
        args
    }

    /// `observe` with an exact quote; returns the Observation ID.
    fn observe(&self, statement: &str, quote: &str) -> String {
        let args = self.args(&[
            "observe",
            statement,
            "--source",
            &self.source,
            "--quote",
            quote,
        ]);
        let receipt = json_of(&self.root, &args);
        receipt["created"][0]["id"].as_str().unwrap().to_owned()
    }

    /// Record one `observation.add` payload; returns the Observation ID.
    fn record(&mut self, payload: Value) -> String {
        self.step += 1;
        let command = json!({
            "schema_version":"inv.command.v1","command_id":format!("step-{}", self.step),
            "case_id":self.case,"actor":ACTOR,"preconditions":[],
            "operations":[{"op":"observation.add","payload":payload}]
        });
        let args = self.args(&["record", "--input", "-", "--format", "json"]);
        let receipt: Value = serde_json::from_str(&ok(&self.root, &args, Some(&command))).unwrap();
        receipt["created"][0]["id"].as_str().unwrap().to_owned()
    }

    fn raise(&mut self, artifact: &str) {
        self.step += 1;
        let command = json!({
            "schema_version":"inv.command.v1","command_id":format!("step-{}", self.step),
            "case_id":self.case,"actor":ACTOR,"preconditions":[],
            "operations":[{"op":"classification.raise","payload":{
                "subject_ref":{"id":artifact,"revision":1},"level":"restricted",
                "reason":"Names a customer."}}]
        });
        let args = self.args(&["record", "--input", "-", "--format", "json"]);
        ok(&self.root, &args, Some(&command));
    }

    fn brief(&self, tail: &[&str]) -> Value {
        let mut args = self.args(&["brief"]);
        args.extend(tail);
        json_of(&self.root, &args)
    }

    fn brief_text(&self) -> String {
        ok(&self.root, &self.args(&["brief", "--format", "text"]), None)
    }
}

fn item<'a>(brief: &'a Value, id: &str) -> &'a Value {
    brief["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id)
        .unwrap_or_else(|| panic!("{id} not in {brief}"))
}

/// The text lines of one record: its ID-first line and the indented lines below it.
fn text_of(text: &str, id: &str) -> Vec<String> {
    let mut lines = text.lines().skip_while(|line| !line.starts_with(id));
    let mut out = vec![lines.next().unwrap().to_owned()];
    out.extend(
        lines
            .take_while(|line| line.starts_with("  "))
            .map(str::to_owned),
    );
    out
}

fn source_line(number: u64) -> &'static str {
    SOURCE.lines().nth(number as usize - 1).unwrap()
}

#[test]
fn a_successor_brief_gives_each_verified_quote_with_its_source_line() {
    let work = FirstUse::start();
    let decisive = work.observe(
        "The 6.1 m reading is provisional",
        "2024  6.1  (new site, provisional; data not yet quality checked)",
    );
    let passage = "can change after quality checks.\nWe do not describe a reading";
    let spanning = work.observe("A provisional value is not a record", passage);

    let brief = work.brief(&[]);
    let guide = brief["citation_guide"].as_str().unwrap();
    assert!(
        guide.contains("Never merge lines into a range or list"),
        "{guide}"
    );
    let citation = &item(&brief, &decisive)["citation"];
    assert_eq!(citation["quote_state"], "verified");
    assert_eq!(
        citation["quote"],
        "2024  6.1  (new site, provisional; data not yet quality checked)"
    );
    assert_eq!(citation["lines"], json!({"start":4,"end":4}));
    assert!(source_line(4).contains(citation["quote"].as_str().unwrap()));
    // The source to cite is the item's own source title.
    assert_eq!(
        item(&brief, &decisive)["source_title"],
        "Initial attachment / source.txt"
    );
    let citation = &item(&brief, &spanning)["citation"];
    assert_eq!(citation["quote"], passage);
    assert_eq!(citation["lines"], json!({"start":6,"end":7}));
    // Citation first: one ready cite per verified quote, before the evidence.
    let cites: Vec<&Value> = brief["citations"].as_array().unwrap().iter().collect();
    assert_eq!(cites.len(), 2, "{}", brief["citations"]);
    assert_eq!(cites[0]["observation"], decisive.as_str());
    assert_eq!(
        cites[0]["cite"],
        "\"Initial attachment / source.txt\" line 4 \"2024  6.1  (new site, provisional; data not yet quality checked)\""
    );
    assert_eq!(cites[1]["lines"], json!({"start":6,"end":7}));
    assert!(
        cites[1]["cite"]
            .as_str()
            .unwrap()
            .contains("\" lines 6-7 \"")
    );
    assert_eq!(cites[0]["content_trust"], "untrusted_evidence");

    // The compact index stays an index: no citation and no guide.
    let compact = work.brief(&["--level", "compact"]);
    assert!(item(&compact, &decisive).get("citation").is_none());
    assert!(compact.get("citation_guide").is_none());
    assert_eq!(compact["citations"], json!([]), "{compact}");

    // The text a successor reads states how to cite, then one cite line per
    // quote, ready to copy, before the evidence; the evidence item keeps its
    // line and does not repeat the quote.
    let text = work.brief_text();
    assert!(text.contains(guide), "{text}");
    let cite = format!(
        "cite {decisive} [untrusted_evidence]: \"Initial attachment / source.txt\" line 4 \"2024  6.1  (new site, provisional; data not yet quality checked)\""
    );
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle}\n{text}"))
    };
    assert!(at(&cite) < at("\nevidence  "), "{text}");
    let lines = text_of(&text, &decisive);
    assert!(
        lines.contains(
            &"  citation  quote verified  line 4  source [authored_data]: \"Initial attachment / source.txt\""
                .to_owned()
        ),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("  quote ")),
        "{lines:#?}"
    );
    let lines = text_of(&text, &spanning);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("  citation  quote verified  lines 6-7  ")),
        "{lines:#?}"
    );
    assert!(
        text.contains(&format!(
            "cite {spanning} [untrusted_evidence]: \"Initial attachment / source.txt\" lines 6-7 \"can change after quality checks.\\nWe do not describe a reading\""
        )),
        "{text}"
    );

    // The JSON stays within its budget with the citations in it.
    let (out, _) = run(
        &work.root,
        &work.args(&["brief", "--max-bytes", "4096", "--format", "json"]),
        None,
    );
    assert!(out.len() <= 4096, "{}", out.len());
}

#[test]
fn a_quote_the_core_did_not_verify_is_shown_as_recorded_with_no_line() {
    let mut work = FirstUse::start();
    // The quote occurs twice, so it identifies no one location.
    let ambiguous = work.observe("The old site readings", "(old site)");
    // Bytes that are not UTF-8 have no text derivative, so the quote was
    // not checked. (A text original's quote is checked through its
    // derivative, bn-1xnl.)
    fs::write(work.root.join("reading.bin"), b"\xff\xfe2024  6.1\n").unwrap();
    let added = json_of(&work.root, &work.args(&["source", "add", "reading.bin"]));
    let binary = &added["items"][0];
    assert!(
        binary.get("derivative_id").is_none_or(Value::is_null),
        "{added}"
    );
    let unchecked = work.record(json!({
        "value":{"kind":"text","statement":"The 2024 reading"},
        "basis":{"kind":"source","artifact_ref":{"id":binary["artifact_id"],"revision":1},
            "selector":{"schema_version":"inv.selector.v1","artifact_digest":binary["digest"],
                "selector":{"type":"whole"}},
            "text_quote":{"exact":"2024  6.1","prefix":null,"suffix":null}},
        "validity":"unknown","quality":[],"limitations":[],"evidence_kind":"observational",
        "variables_changed":[]
    }));

    let brief = work.brief(&[]);
    let citation = &item(&brief, &ambiguous)["citation"];
    assert_eq!(citation["quote_state"], "ambiguous");
    assert_eq!(citation["quote"], "(old site)");
    assert!(citation.get("lines").is_none(), "{citation}");
    assert_eq!(
        citation["lines_unavailable"],
        "the quote occurs more than once in the source bytes"
    );
    let citation = &item(&brief, &unchecked)["citation"];
    assert_eq!(citation["quote_state"], "unavailable");
    assert!(citation.get("lines").is_none(), "{citation}");

    let text = work.brief_text();
    for (id, state) in [(&ambiguous, "ambiguous"), (&unchecked, "unavailable")] {
        let lines = text_of(&text, id);
        let citation = lines
            .iter()
            .find(|line| line.starts_with("  citation  "))
            .unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(
            citation.starts_with(&format!("  citation  quote {state}  line not located: ")),
            "{citation}"
        );
        assert!(!citation.contains("verified"), "{citation}");
    }
}

#[test]
fn a_verified_line_selector_without_a_quote_gives_its_own_lines() {
    let mut work = FirstUse::start();
    let measured = work.record(json!({
        "value":{"kind":"measurement","value":"6.1","unit":"m"},
        "basis":{"kind":"source","artifact_ref":{"id":work.text,"revision":1},
            "selector":{"schema_version":"inv.selector.v1","artifact_digest":work.digest,
                "selector":{"type":"text_lines","start_line":4,"end_line":4,
                    "encoding":"utf-8","parser_version":"inv.text.utf8.v1"}},
            "text_quote":null},
        "validity":"unknown","quality":[],"limitations":[],"evidence_kind":"observational",
        "variables_changed":[]
    }));
    let brief = work.brief(&[]);
    let citation = &item(&brief, &measured)["citation"];
    assert_eq!(citation["lines"], json!({"start":4,"end":4}));
    assert_eq!(citation["quote_state"], "not_applicable");
    assert!(citation.get("quote").is_none(), "{citation}");
    // Its cite line names the file and line, with no quote.
    assert_eq!(
        brief["citations"],
        json!([{"observation":measured,"lines":{"start":4,"end":4},
            "cite":"\"Initial attachment / source.txt\" line 4","content_trust":"untrusted_evidence"}])
    );
    let lines = text_of(&work.brief_text(), &measured);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("  citation  line 4  source ")),
        "{lines:#?}"
    );
}

#[test]
fn restricted_content_shows_no_quote_and_missing_bytes_locate_no_line() {
    let quote = "2024  6.1  (new site, provisional; data not yet quality checked)";

    // Missing bytes: the recorded state stays, and no line is claimed.
    let work = FirstUse::start();
    let observation = work.observe("The 6.1 m reading is provisional", quote);
    let hex = work
        .digest
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let blob = work
        .workspace()
        .join("state/blobs/sha256")
        .join(&hex[..2])
        .join(hex);
    fs::remove_file(&blob).unwrap();
    let brief = work.brief(&[]);
    let citation = &item(&brief, &observation)["citation"];
    assert_eq!(citation["quote_state"], "verified");
    assert!(citation.get("lines").is_none(), "{citation}");
    assert_eq!(
        citation["lines_unavailable"],
        "the retained bytes are missing or do not match their digest"
    );

    // Restricted: neither the quote nor a line is shown, in JSON or text.
    let mut work = FirstUse::start();
    let observation = work.observe("The 6.1 m reading is provisional", quote);
    let original = work.original.clone();
    work.raise(&original);
    let brief = work.brief(&["--level", "deep"]);
    let shown = item(&brief, &observation);
    assert_eq!(shown["content_state"], "restricted", "{shown}");
    assert!(shown.get("citation").is_none(), "{shown}");
    assert!(brief.get("citation_guide").is_none(), "{brief}");
    assert!(!brief.to_string().contains("new site"), "{brief}");
    assert!(!work.brief_text().contains("new site"));
}

#[test]
fn a_citation_of_withdrawn_evidence_says_so_on_its_line() {
    let work = FirstUse::start();
    let observation = work.observe(
        "The 6.1 m reading is provisional",
        "2024  6.1  (new site, provisional; data not yet quality checked)",
    );
    let command = json!({
        "schema_version":"inv.command.v1","command_id":"withdraw","case_id":work.case,
        "actor":ACTOR,"preconditions":[],"operations":[{"op":"artifact.withdraw","payload":{
            "artifact_ref":{"id":work.original,"revision":1},"reason":"collection_error",
            "scope":{"extent":"entire"},"rationale":"The wrong station was exported.",
            "evidence_refs":[]}}]
    });
    let args = work.args(&["record", "--input", "-", "--format", "json"]);
    ok(&work.root, &args, Some(&command));
    // Withdrawn evidence gets no ready cite line; its item keeps the quote
    // and says why it is not current.
    assert_eq!(work.brief(&[])["citations"], json!([]));
    let text = work.brief_text();
    assert!(!text.contains("\ncite "), "{text}");
    let lines = text_of(&text, &observation);
    assert!(
        lines.iter().any(|line| line.starts_with("  quote ")),
        "{lines:#?}"
    );
    let citation = lines
        .iter()
        .find(|line| line.starts_with("  citation  "))
        .unwrap_or_else(|| panic!("{lines:#?}"));
    // The recorded quote state stays; the line says the evidence is not current.
    assert!(
        citation.starts_with("  citation  quote verified  line 4  freshness "),
        "{citation}"
    );
    assert!(citation.contains("  use withdrawn  "), "{citation}");
}

#[test]
fn a_budget_that_cuts_evidence_keeps_every_cite_line() {
    let work = FirstUse::start();
    let quotes = [
        ("Station history", 1),
        ("2019  5.6  (old site)", 2),
        ("2020  5.9  (old site)", 3),
        (
            "2024  6.1  (new site, provisional; data not yet quality checked)",
            4,
        ),
        ("Provisional values can change after quality checks.", 6),
        (
            "We do not describe a reading as a record until the series is adjusted.",
            7,
        ),
    ];
    let ids: Vec<String> = quotes
        .iter()
        .map(|(quote, line)| work.observe(&format!("Source line {line} reads as quoted"), quote))
        .collect();
    let brief = work.brief(&["--max-bytes", "8192"]);
    assert!(brief.to_string().len() <= 8192);
    let cut = brief["omitted_items"]["evidence"].as_u64().unwrap();
    assert!(
        cut >= 3,
        "the budget cuts evidence: {}",
        brief["omitted_items"]
    );
    assert_eq!(brief["omitted_items"]["citations"], 0);
    let cites = brief["citations"].as_array().unwrap();
    assert_eq!(cites.len(), quotes.len(), "{}", brief["citations"]);
    for ((cite, (quote, line)), id) in cites.iter().zip(quotes).zip(&ids) {
        assert_eq!(cite["observation"], id.as_str());
        assert!(source_line(line).contains(quote));
        // One file, one line and one quote per cite, never a merged range.
        assert_eq!(
            cite["cite"],
            format!("\"Initial attachment / source.txt\" line {line} \"{quote}\"")
        );
    }
}
