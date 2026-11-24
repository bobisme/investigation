//! `frontier`, `next` and `retrospective` pinned with `--snapshot SEQUENCE
//! [--head-hash DIGEST]` (bn-1i89), with the same semantics and refusal as
//! `brief` and `show`, over a copy of the checked-in Scenario A workspace:
//!
//! - a pinned read after later commits returns the view the head gave before
//!   them, labelled historical, while the unpinned read moves on;
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
    let reads: [&[&str]; 4] = [
        &["frontier"],
        &["next"],
        &["retrospective"],
        &["retrospective", "--draft"],
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
        assert_eq!(view["historical"], false, "{view}");
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
        }
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
            if !args.contains(&"--snapshot") {
                continue;
            }
            let view = json(&workspace, args, 0);
            let snapshot = if view["historical_read"].is_object() {
                assert_historical(&name, &view, &pin);
                &view["historical_read"]["snapshot"]
            } else {
                // `blindspots` names its snapshot without the label.
                &view["snapshot"]
            };
            assert_eq!(snapshot["sequence"].to_string(), pin[1], "{name}");
            assert_eq!(snapshot["head_hash"], pin[3].as_str(), "{name}");
            if ["frontier", "next", "retrospective"].contains(&args[2]) {
                let (mut old, mut new) = (unlabelled(before), unlabelled(&view));
                // Only the draft a pinned retrospective offers carries the pin.
                if args[2] == "retrospective" {
                    old.as_object_mut().unwrap().remove("next");
                    new.as_object_mut().unwrap().remove("next");
                }
                assert_eq!(old, new, "{name}");
            }
        }
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
