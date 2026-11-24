//! `frontier`, `next`, `retrospective` (bn-1i89) and `blindspots` (bn-8fwv)
//! pinned with `--snapshot SEQUENCE [--head-hash DIGEST]`, with the same
//! semantics and refusal as `brief` and `show`, over a copy of the checked-in
//! Scenario A workspace:
//!
//! - a pinned read after later commits returns the view the head gave before
//!   them, labelled historical, while the unpinned read moves on;
//! - a pinned `show`, `brief` or `explain` differs from the head's read of
//!   the same snapshot only by its label, `current_evidentiary_use` and, for
//!   a bounded read, the items its budget keeps, each counted as omitted;
//! - a wrong head hash, or a sequence that is no verified snapshot, is
//!   refused with a repair argv that reads the current head;
//! - every command a canonical handoff prints runs as written, and each
//!   pinned one reads the exported snapshot after later commits;
//! - a remapped or public handoff prints no pin, case command or head hash.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

#[path = "support/fixtures.rs"]
mod fixtures;

/// A Conclusion of the Scenario A case, cited by Observations of an
/// Artifact.
const CONCLUSION: &str = "concl_01a20751-91b0-7003-9309-18ce4a1e3796";

const BRIEF: &str = "handoff-brief.md";
const OVERVIEW: &str = "handoff-overview.md";

/// Keys a historical read adds to the view the head gave: its label, and
/// the newer head a frontier page reports with the restart that reads it.
const LABELS: [&str; 4] = [
    "historical",
    "historical_read",
    "newer_head",
    "restart_argv",
];

/// Run the binary in `cwd` and return its exit code and stdout.
fn invoke(cwd: &Path, args: &[&str], stdin: Option<&Value>) -> (i32, String) {
    let mut process = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_READ_ONLY")
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(stdin) = stdin {
        let mut pipe = process.stdin.take().unwrap();
        pipe.write_all(&serde_json::to_vec(stdin).unwrap()).unwrap();
    }
    let output = process.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    (output.status.code().unwrap(), stdout)
}

/// Run `args` with `--format json`, require `exit`, and parse the output.
fn json(cwd: &Path, args: &[&str], exit: i32) -> Value {
    let mut args = args.to_vec();
    if !args.contains(&"--format") {
        args.extend(["--format", "json"]);
    }
    let (code, stdout) = invoke(cwd, &args, None);
    assert_eq!(code, exit, "{args:?}\n{stdout}");
    serde_json::from_str(&stdout).expect(&stdout)
}

/// A copy of Scenario A and the case whose handoff, gaps and Retrospective
/// the tests read.
fn scenario(root: &Path) -> (PathBuf, String) {
    let workspace = fixtures::workspace("scenario-a", &root.join("ws"), false);
    let mut cases: Vec<String> = fs::read_dir(workspace.join("state/cases"))
        .unwrap()
        .map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.split('.').next().unwrap().to_owned()
        })
        .collect();
    cases.sort();
    (workspace, cases[0].clone())
}

/// The case's current snapshot as `--snapshot` and `--head-hash` arguments.
fn head_pin(workspace: &Path, case: &str) -> [String; 4] {
    let snapshot = &json(workspace, &["--case", case, "next"], 0)["snapshot"];
    [
        "--snapshot".into(),
        snapshot["sequence"].to_string(),
        "--head-hash".into(),
        snapshot["head_hash"].as_str().unwrap().to_owned(),
    ]
}

/// Commit a Run with an unknown outcome, which adds a gap to the case.
fn commit_later_run(workspace: &Path, case: &str, key: &str) {
    let (code, stdout) = invoke(
        workspace,
        &["record", "--input", "-", "--format", "json"],
        Some(&json!({
            "schema_version":"inv.command.v1", "command_id":key,
            "case_id":case, "actor":"agent:snapshot-pin", "preconditions":[],
            "operations":[{"op":"run.record", "payload":{
                "intent":{"kind":"exploration", "intent":"Replay the failing request once."},
                "producer":"agent:snapshot-pin", "artifact_refs":[],
                "execution_status":"unknown", "evidence_completeness":"partial",
                "evidence_kind":"observational", "variables_changed":[]}}]
        })),
    );
    assert_eq!(code, 0, "{stdout}");
}

/// `value` without the keys a historical read adds.
fn unlabelled(value: &Value) -> Value {
    let mut value = value.clone();
    for key in LABELS {
        value.as_object_mut().unwrap().remove(key);
    }
    value
}

/// `value` without `keys`, at any depth.
fn without(value: &Value, keys: &[&str]) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(key, _)| !keys.contains(&key.as_str()))
                .map(|(key, child)| (key.clone(), without(child, keys)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(|item| without(item, keys)).collect()),
        other => other.clone(),
    }
}

/// Every value of `key` in `value`, at any depth.
fn every<'v>(value: &'v Value, key: &str, found: &mut Vec<(&'v Value, &'v Value)>) {
    match value {
        Value::Object(object) => {
            if let Some(child) = object.get(key) {
                found.push((value, child));
            }
            for child in object.values() {
                every(child, key, found);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| every(item, key, found)),
        _ => {}
    }
}

/// A bounded read of one snapshot keeps or counts every item the head's
/// read of it kept or counted: per section, shown plus omitted is equal.
fn assert_same_items(name: &str, head: &Value, pinned: &Value) {
    let sections = head["omitted_items"].as_object().unwrap();
    assert!(!sections.is_empty(), "{name}");
    for (section, head_omitted) in sections {
        let total = |view: &Value, omitted: &Value| {
            view[section].as_array().map_or(0, Vec::len) as u64 + omitted.as_u64().unwrap()
        };
        assert_eq!(
            total(pinned, &pinned["omitted_items"][section]),
            total(head, head_omitted),
            "{name}: {section}"
        );
    }
}

/// The read is labelled historical at `pin`, with a newer latest head.
fn assert_historical(name: &str, view: &Value, pin: &[String; 4]) {
    assert_eq!(view["historical"], true, "{name}: {view}");
    let label = &view["historical_read"];
    assert_eq!(label["snapshot"]["sequence"].to_string(), pin[1], "{name}");
    assert_eq!(label["snapshot"]["head_hash"], pin[3].as_str(), "{name}");
    assert!(
        label["latest_snapshot"]["sequence"].as_u64() > label["snapshot"]["sequence"].as_u64(),
        "{name}: {label}"
    );
    assert!(
        label["policy"]
            .as_str()
            .unwrap()
            .contains("today's audience"),
        "{name}: {label}"
    );
}

#[test]
fn a_pinned_read_after_later_commits_returns_the_old_view() {
    let directory = tempfile::tempdir().unwrap();
    let (workspace, case) = scenario(directory.path());
    let pin = head_pin(&workspace, &case);
    let pin: Vec<&str> = pin.iter().map(String::as_str).collect();
    let reads: [&[&str]; 6] = [
        &["frontier"],
        &["next"],
        &["retrospective"],
        &["retrospective", "--draft"],
        &["blindspots"],
        &["blindspots", "--subject", CONCLUSION],
    ];
    let read = |tail: &[&str], pinned: bool| {
        let mut args = vec!["--case", case.as_str()];
        args.extend(tail);
        if pinned {
            args.extend(&pin);
        }
        json(&workspace, &args, 0)
    };
    let before: Vec<Value> = reads.iter().map(|tail| read(tail, false)).collect();
    let gap = before[0]["gaps"][0]["gap_id"].as_str().unwrap().to_owned();
    let explain_before = read(&["frontier", "--explain", &gap], false);
    for view in before.iter().chain([&explain_before]) {
        // `blindspots` keeps its `inv.audit.v1` bytes at the head: no label.
        let head = if view["schema_version"] == "inv.audit.v1" {
            Value::Null
        } else {
            json!(false)
        };
        assert_eq!(view["historical"], head, "{view}");
    }

    commit_later_run(&workspace, &case, "snapshot-pin-later");

    for (tail, before) in reads.iter().zip(&before) {
        let name = tail.join(" ");
        let now = read(tail, false);
        assert_ne!(
            now["snapshot"], before["snapshot"],
            "{name}: head did not move"
        );
        let pinned = read(tail, true);
        assert_historical(&name, &pinned, &head_pin_of(before));
        let (mut old, mut new) = (unlabelled(before), unlabelled(&pinned));
        if name == "retrospective" {
            // The draft it offers stays on the pinned snapshot.
            let draft = new["next"][0]["argv"].as_array().unwrap().clone();
            let draft: Vec<&str> = draft.iter().map(|arg| arg.as_str().unwrap()).collect();
            assert!(
                draft.windows(4).any(|window| window == pin.as_slice()),
                "{draft:?}"
            );
            assert_eq!(json(&workspace, &draft[1..], 0)["historical"], true);
            old.as_object_mut().unwrap().remove("next");
            new.as_object_mut().unwrap().remove("next");
        }
        assert_eq!(old, new, "{name}: the pinned read is not the old view");
    }
    // The later Run adds a gap the head lists and the pinned inventory does not.
    let gaps = |view: &Value| view["inventory_total"].as_u64().unwrap();
    assert!(gaps(&read(&["frontier"], false)) > gaps(&before[0]));
    assert_eq!(gaps(&read(&["frontier"], true)), gaps(&before[0]));
    // A pinned page continues at its snapshot; the restart reads the head.
    let page = read(&["frontier", "--limit", "1"], true);
    let next_page: Vec<String> = serde_json::from_value(page["next_page_argv"].clone()).unwrap();
    let next_page: Vec<&str> = next_page.iter().map(String::as_str).collect();
    let second = json(&workspace, &next_page[1..], 0);
    assert_eq!(second["snapshot"], before[0]["snapshot"], "{second}");
    assert_eq!(second["gaps"][0], before[0]["gaps"][1]);
    let restart: Vec<String> = serde_json::from_value(page["restart_argv"].clone()).unwrap();
    let restart: Vec<&str> = restart.iter().map(String::as_str).collect();
    assert_ne!(
        json(&workspace, &restart[1..], 0)["snapshot"],
        before[0]["snapshot"]
    );
    // --explain explains the gap as it stood at the snapshot.
    let explained = read(&["frontier", "--explain", &gap], true);
    assert_historical("frontier --explain", &explained, &head_pin_of(&before[0]));
    assert_eq!(unlabelled(&explained), unlabelled(&explain_before));
    // The text form carries the same label.
    let mut args = vec!["--case", case.as_str(), "next"];
    args.extend(&pin);
    let (code, text) = invoke(&workspace, &args, None);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.lines()
            .nth(1)
            .unwrap()
            .starts_with("historical_read  snapshot "),
        "{text}"
    );
}

/// The `--snapshot`/`--head-hash` arguments naming `view`'s snapshot.
fn head_pin_of(view: &Value) -> [String; 4] {
    [
        "--snapshot".into(),
        view["snapshot"]["sequence"].to_string(),
        "--head-hash".into(),
        view["snapshot"]["head_hash"].as_str().unwrap().to_owned(),
    ]
}

#[test]
fn a_wrong_head_hash_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let (workspace, case) = scenario(directory.path());
    let pin = head_pin(&workspace, &case);
    commit_later_run(&workspace, &case, "snapshot-pin-refusal");
    let wrong = format!("sha256:{}", "a".repeat(64));
    for tail in [
        &["frontier"][..],
        &["frontier", "--explain", "gap:none"],
        &["next"],
        &["retrospective"],
        &["retrospective", "--draft"],
        &["blindspots"],
    ] {
        let name = tail.join(" ");
        let mut args = vec!["--case", case.as_str()];
        args.extend(tail);
        // A wrong head hash at a verified sequence.
        let mut hashed = args.clone();
        hashed.extend(["--snapshot", &pin[1], "--head-hash", &wrong]);
        let refused = json(&workspace, &hashed, 3);
        assert_eq!(refused["ok"], false, "{name}: {refused}");
        assert_eq!(refused["error"]["code"], "UNKNOWN_REFERENCE", "{name}");
        assert_eq!(
            refused["error"]["actual"]["head_hash"],
            wrong.as_str(),
            "{name}"
        );
        let repair: Vec<String> =
            serde_json::from_value(refused["error"]["repair"]["argv"].clone()).unwrap();
        assert!(
            !repair.contains(&"--snapshot".to_owned()),
            "{name}: {repair:?}"
        );
        assert!(repair.contains(&tail[0].to_owned()), "{name}: {repair:?}");
        // A sequence that is no verified snapshot.
        let mut unknown = args.clone();
        unknown.extend(["--snapshot", "999999"]);
        let refused = json(&workspace, &unknown, 3);
        assert_eq!(refused["error"]["code"], "UNKNOWN_REFERENCE", "{name}");
        assert_eq!(refused["error"]["path"], "--snapshot", "{name}");
        // A malformed head hash.
        let mut malformed = args.clone();
        malformed.extend(["--snapshot", &pin[1], "--head-hash", "not-a-digest"]);
        let refused = json(&workspace, &malformed, 2);
        assert_eq!(
            refused["error"]["code"], "INVALID_INPUT",
            "{name}: {refused}"
        );
        assert_eq!(refused["error"]["path"], "--head-hash", "{name}");
        // The right hash after later commits reads the snapshot.
        let mut right = args.clone();
        right.extend(pin.iter().map(String::as_str));
        if name != "frontier --explain gap:none" {
            assert_eq!(json(&workspace, &right, 0)["historical"], true, "{name}");
        }
    }
    // A pin conflicts with the reads it cannot bind.
    for tail in [
        &["frontier", "--cursor", "gpc1.x"][..],
        &["retrospective", "--lessons"],
    ] {
        let mut args = vec!["--case", case.as_str()];
        args.extend(tail);
        args.extend(["--snapshot", &pin[1]]);
        assert_eq!(invoke(&workspace, &args, None).0, 2, "{args:?}");
    }
}

#[test]
fn a_pinned_brief_or_explain_differs_from_the_head_only_by_its_label_current_use_and_budget() {
    let directory = tempfile::tempdir().unwrap();
    let (workspace, case) = scenario(directory.path());
    let pin = head_pin(&workspace, &case);
    let pin: Vec<&str> = pin.iter().map(String::as_str).collect();
    let ample = "1000000";
    let reads: Vec<Vec<&str>> = vec![
        vec!["show", CONCLUSION, "--depth", "full"],
        vec!["brief"],
        vec!["brief", "--level", "deep"],
        vec!["explain", case.as_str()],
        vec!["explain", CONCLUSION],
    ];
    let read = |tail: &[&str], budget: Option<&str>, pinned: bool| {
        let mut args = vec!["--case", case.as_str()];
        args.extend(tail);
        if let Some(budget) = budget {
            args.extend(["--max-bytes", budget]);
        }
        if pinned {
            args.extend(&pin);
        }
        json(&workspace, &args, 0)
    };
    let bounded = |tail: &[&str]| tail[0] != "show";
    let budget = |tail: &[&str]| bounded(tail).then_some(ample);
    let head: Vec<(Value, Value)> = reads
        .iter()
        .map(|tail| (read(tail, None, false), read(tail, budget(tail), false)))
        .collect();

    commit_later_run(&workspace, &case, "snapshot-pin-bounded");

    let (mut current_uses, mut complete_reads) = (0, 0);
    for (tail, (head_default, head_ample)) in reads.iter().zip(&head) {
        let name = tail.join(" ");
        let pinned_ample = read(tail, budget(tail), true);
        assert_historical(&name, &pinned_ample, &head_pin_of(head_default));
        // At the head, evidentiary use is today's use; nothing repeats it.
        let mut uses = Vec::new();
        every(head_ample, "current_evidentiary_use", &mut uses);
        assert!(uses.is_empty(), "{name}: the head shows current use");
        // A historical read adds today's use beside the use it recorded,
        // which nothing has changed here.
        every(&pinned_ample, "current_evidentiary_use", &mut uses);
        for (record, current) in &uses {
            assert_eq!(record["evidentiary_use"], **current, "{name}: {record}");
        }
        current_uses += uses.len();
        // With room for everything, nothing else differs.
        let strip = ["historical", "historical_read", "current_evidentiary_use"];
        assert_eq!(
            without(&pinned_ample, &strip),
            without(head_ample, &strip),
            "{name}: a pinned read with room for everything differs from the head"
        );
        if !bounded(tail) {
            continue;
        }
        // Within the default budget the label and today's use take room, so
        // the pinned read may keep fewer items, but it counts every one it
        // leaves out and its complete read gives them all back.
        let pinned_default = read(tail, None, true);
        assert_same_items(&name, head_default, &pinned_default);
        let bytes = serde_json::to_vec(&pinned_default).unwrap().len() + 1;
        let limit: usize = if tail[0] == "brief" { 16_384 } else { 32_768 };
        assert!(bytes <= limit, "{name}: {bytes} bytes");
        if let Some(argv) = pinned_default["complete_read_argv"].as_array() {
            let argv: Vec<&str> = argv.iter().map(|arg| arg.as_str().unwrap()).collect();
            let complete = json(&workspace, &argv[1..], 0);
            complete_reads += 1;
            let strip = ["historical_read", "complete_read_argv"];
            assert_eq!(
                without(&complete, &strip),
                without(&pinned_ample, &strip),
                "{name}: the complete read is not the whole pinned read"
            );
        }
    }
    assert!(current_uses > 0, "no historical read showed current use");
    // Scenario A's default brief is full, so some pinned read is cut.
    assert!(complete_reads > 0, "no pinned read was cut by its budget");
}

/// Every inline `investigation ...` command of `text`, in order.
fn commands(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| span.starts_with("investigation "))
        .map(str::to_owned)
        .collect()
}

fn export(workspace: &Path, case: &str, out: &Path, audience: &str, remap: bool) -> [String; 2] {
    let mut args = vec![
        "--case",
        case,
        "export",
        "--kind",
        "handoff",
        "--audience",
        audience,
        "--out",
        out.to_str().unwrap(),
    ];
    if remap {
        args.push("--remap-ids");
    }
    assert_eq!(json(workspace, &args, 0)["result"], "exported");
    [BRIEF, OVERVIEW].map(|file| fs::read_to_string(out.join(file)).unwrap())
}

#[test]
fn every_command_a_handoff_prints_runs_as_written_and_gives_the_pinned_view() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (workspace, case) = scenario(root);
    // Every Scenario A record is internal, so a restricted handoff prints the
    // same commands.
    for audience in ["internal"] {
        let pin = head_pin(&workspace, &case);
        let pin_text = pin.join(" ");
        let files = export(&workspace, &case, &root.join(audience), audience, false);
        let mut printed: Vec<String> = files.iter().flat_map(|text| commands(text)).collect();
        printed.sort();
        printed.dedup();
        for (name, tail) in [
            ("frontier", "frontier"),
            ("next", "next"),
            ("retrospective", "retrospective"),
        ] {
            let pinned = format!("investigation --case {case} {tail} {pin_text}");
            assert!(
                printed.iter().any(|command| command.starts_with(&pinned)),
                "{audience}: no pinned {name} in {printed:#?}"
            );
            assert!(
                !printed
                    .iter()
                    .any(|command| command.contains(&format!(" {tail} "))
                        && !command.contains("--snapshot")),
                "{audience}: an unpinned {name}"
            );
        }
        for text in &files {
            assert!(!text.contains("current head"), "{audience}: {text}");
            assert!(!text.contains("--selector-file"), "{audience}: {text}");
        }
        // Escalation gives runnable reads of the bytes behind named records;
        // Scenario A's sources each fit one read.
        let reads: Vec<&String> = printed
            .iter()
            .filter(|command| command.contains(" evidence read art_"))
            .collect();
        assert!(!reads.is_empty(), "{audience}: no evidence read");
        assert!(
            reads.iter().all(|command| command.ends_with(" --whole")),
            "{reads:#?}"
        );
        // What every pinned read gives before later commits.
        let runnable: Vec<Vec<&str>> = printed
            .iter()
            .map(|command| command.split(' ').skip(1).collect::<Vec<_>>())
            // Placeholders are filled by the reader; `record` writes.
            .filter(|args| !args.iter().any(|arg| arg.starts_with('<')))
            .filter(|args| args.first() != Some(&"record"))
            .collect();
        let before: Vec<Value> = runnable
            .iter()
            .map(|args| json(&workspace, args, 0))
            .collect();

        commit_later_run(
            &workspace,
            &case,
            &format!("snapshot-pin-handoff-{audience}"),
        );

        for (args, before) in runnable.iter().zip(&before) {
            let name = format!("{audience}: {}", args.join(" "));
            // As written: the text form, or JSON where it says so.
            let (code, stdout) = invoke(&workspace, args, None);
            assert_eq!(code, 0, "{name}\n{stdout}");
            if args.get(2) == Some(&"evidence") {
                // Bytes are read as retained today: the same bytes as before.
                let after = json(&workspace, args, 0);
                for field in ["artifact", "selector", "excerpt"] {
                    assert_eq!(after[field], before[field], "{name}: {field}");
                }
                continue;
            }
            if !args.contains(&"--snapshot") {
                continue;
            }
            let view = json(&workspace, args, 0);
            assert_historical(&name, &view, &pin);
            assert_eq!(view["snapshot"], before["snapshot"], "{name}");
            let (mut old, mut new) = (unlabelled(before), unlabelled(&view));
            match args[2] {
                // Only the draft a pinned retrospective offers carries the pin.
                "retrospective" => {
                    old.as_object_mut().unwrap().remove("next");
                    new.as_object_mut().unwrap().remove("next");
                }
                "show" => {
                    old = without(&old, &["current_evidentiary_use"]);
                    new = without(&new, &["current_evidentiary_use"]);
                }
                // Bounded reads: see the brief and explain test below.
                "brief" | "explain" => {
                    assert_same_items(&name, before, &view);
                    continue;
                }
                _ => {}
            }
            assert_eq!(old, new, "{name}");
        }
    }
}

/// A source larger than one `evidence read` (32768 bytes): the handoff gives
/// the Observation's cited lines as a runnable `--lines` read, never
/// `--whole`, and a remapped handoff gives no read at all.
#[test]
fn a_handoff_reads_a_large_source_by_its_cited_lines() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut log = String::new();
    for line in 1..=2000 {
        if line == 1500 {
            log.push_str("request 1500 waited 28 seconds for a pooled connection\n");
        } else {
            log.push_str(&format!(
                "request {line} finished in 2 ms after a routine wait of no consequence\n"
            ));
        }
    }
    assert!(log.len() > 32_768);
    fs::write(root.join("big.log"), &log).unwrap();
    fs::write(root.join("small.log"), "request 7 waited 3 seconds\n").unwrap();
    json(
        root,
        &[
            "init",
            "./ws",
            "--actor",
            "agent:snapshot-pin",
            "--no-vault",
            "--import-root",
            ".",
        ],
        0,
    );
    let workspace = root.join("ws");
    let started = json(
        &workspace,
        &[
            "--actor",
            "agent:snapshot-pin",
            "start",
            "Why did requests wait?",
            "--attach",
            "../big.log",
        ],
        0,
    );
    let case = started["created"][0]["id"].as_str().unwrap().to_owned();
    assert!(case.starts_with("case_"), "{started}");
    let added = json(
        &workspace,
        &[
            "--actor",
            "agent:snapshot-pin",
            "--case",
            &case,
            "source",
            "add",
            "../small.log",
        ],
        0,
    );
    let original = |response: &Value| {
        response["items"][0]["artifact_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (big, small) = (original(&started), original(&added));
    for (source, quote) in [(&big, "waited 28 seconds"), (&small, "waited 3 seconds")] {
        json(
            &workspace,
            &[
                "--actor",
                "agent:snapshot-pin",
                "--case",
                &case,
                "observe",
                &format!("The log reports that a request {quote}"),
                "--source",
                source,
                "--quote",
                quote,
            ],
            0,
        );
    }
    let files = export(&workspace, &case, &root.join("internal"), "internal", false);
    let mut printed: Vec<String> = files.iter().flat_map(|text| commands(text)).collect();
    printed.sort();
    printed.dedup();
    let reads: Vec<&String> = printed
        .iter()
        .filter(|command| command.contains(" evidence read art_"))
        .collect();
    // The quote in the large source, at line 1500, read through the text the
    // source was decoded to; the small source whole.
    let lines: Vec<&&String> = reads
        .iter()
        .filter(|command| command.ends_with(" --lines 1500:1500"))
        .collect();
    assert_eq!(lines.len(), 1, "{reads:#?}");
    // The small source, or the text it was decoded to, is read whole.
    let small_text = added["items"][0]["derivative_id"]
        .as_str()
        .unwrap_or(&small);
    assert!(
        reads.iter().any(|command| (command.contains(small.as_str())
            || command.contains(small_text))
            && command.ends_with(" --whole")),
        "{reads:#?}"
    );
    for command in &reads {
        let args: Vec<&str> = command.split(' ').skip(1).collect();
        let (code, stdout) = invoke(&workspace, &args, None);
        assert_eq!(code, 0, "{command}\n{stdout}");
        if command.ends_with(" --lines 1500:1500") {
            assert!(
                stdout.contains("request 1500 waited 28 seconds"),
                "{stdout}"
            );
        }
    }
    // No `--whole` of the large source or its text: one read cannot print
    // either. The small source is read whole.
    let text = started["items"][0]["derivative_id"].as_str().unwrap();
    let whole_big: Vec<&&String> = reads
        .iter()
        .filter(|command| command.ends_with(" --whole"))
        .filter(|command| command.contains(big.as_str()) || command.contains(text))
        .collect();
    assert!(whole_big.is_empty(), "{whole_big:#?}");
    // A remapped handoff names no workspace read.
    for text in export(&workspace, &case, &root.join("remapped"), "internal", true) {
        assert!(!text.contains("evidence read art_"), "{text}");
        assert!(!text.contains("--lines"), "{text}");
    }
}

#[test]
fn remapped_and_public_handoffs_carry_no_pins() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (workspace, case) = scenario(root);
    let pin = head_pin(&workspace, &case);
    for (audience, remap) in [("public", false), ("internal", true), ("restricted", true)] {
        let name = format!("{audience} remap {remap}");
        let out = root.join(format!("{audience}-{remap}"));
        let mut publisher_reads = 0;
        for text in export(&workspace, &case, &out, audience, remap) {
            for forbidden in [
                "--snapshot",
                "--head-hash",
                "investigation --case",
                case.as_str(),
                pin[3].trim_start_matches("sha256:"),
            ] {
                assert!(!text.contains(forbidden), "{name}: shows {forbidden:?}");
            }
            for line in [
                "Every gap at this snapshot",
                "Ranked again at this snapshot",
                "Lessons and the process digest at this snapshot",
            ] {
                if let Some(found) = text.lines().find(|shown| shown.contains(line)) {
                    assert!(
                        found.ends_with("available to the publisher"),
                        "{name}: {found}"
                    );
                    publisher_reads += 1;
                }
            }
            assert!(!text.contains("current head"), "{name}");
        }
        // A public handoff of Scenario A shows no gap, step or Retrospective:
        // every record is internal.
        let expected = if audience == "public" { 0 } else { 6 };
        assert_eq!(
            publisher_reads, expected,
            "{name}: frontier, next and retrospective lines in both files"
        );
    }
}
