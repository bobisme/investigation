//! `artifact.expire` and `retention preview` through the public binary
//! (plan §16.5, §20.2; W11.11).
//!
//! - Identical bytes imported into two cases: withdrawing one reference's
//!   availability stops every read through it, also at an earlier snapshot,
//!   while the other case reads the same bytes. The preview, taken first,
//!   names exactly the records that then need review, counts the other
//!   case's references without naming them, and lists the quoting record.
//! - Physical reclamation stays disabled: the bytes stay stored, `check`
//!   passes, and the record keeps its digest, size and provenance.
//! - Unique bytes become reclaimable only once every reference is withdrawn.
//! - An adopted method pack's bytes cannot be withdrawn.
//! - An export pinned before the withdrawal copies no bytes through it.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:retention-test";
const SHARED: &str = "p99 18.7 ms over 30 runs\n";
const PACK: &str = include_str!("../../../fixtures/method-packs/bench-notes.toml");

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_READ_ONLY")
        .args(args)
        .arg("--json")
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
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{args:?}\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&stdout).expect(&stdout)
}

fn init(root: &Path) {
    run(root, &["init", ".", "--no-vault"], None, 0);
    let config = root.join("investigation.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
}

/// A new case started from `file`: (case, original artifact, digest).
fn start(root: &Path, key: &str, file: &str) -> (String, String, String) {
    let started = run(
        root,
        &[
            "start",
            "Does the queue meet the latency limit?",
            "--attach",
            file,
            "--command-id",
            key,
            "--actor",
            ACTOR,
        ],
        None,
        0,
    );
    let item = &started["items"][0];
    (
        started["created"][0]["id"].as_str().unwrap().to_owned(),
        item["artifact_id"].as_str().unwrap().to_owned(),
        item["digest"].as_str().unwrap().to_owned(),
    )
}

fn record(root: &Path, case: &str, key: &str, operations: Value, exit: i32) -> Value {
    run(
        root,
        &["record", "--input", "-"],
        Some(&json!({
            "schema_version": "inv.command.v1", "command_id": key, "case_id": case,
            "actor": ACTOR, "preconditions": [], "operations": operations
        })),
        exit,
    )
}

fn expire(artifact: &Value) -> Value {
    json!({"op": "artifact.expire", "payload": {
        "artifact_ref": artifact, "reason": "retention_period_ended",
        "rationale": "The case keeps raw captures for 90 days."
    }})
}

fn exact(id: &str, revision: u64) -> Value {
    json!({"id": id, "revision": revision})
}

fn selector(root: &Path, digest: &str) -> String {
    let path = root.join("whole.json");
    fs::write(
        &path,
        json!({"schema_version": "inv.selector.v1", "artifact_digest": digest,
            "selector": {"type": "whole"}})
        .to_string(),
    )
    .unwrap();
    "whole.json".to_owned()
}

fn blob(root: &Path, digest: &str) -> std::path::PathBuf {
    let hex = digest.strip_prefix("sha256:").unwrap();
    root.join("state/blobs/sha256").join(&hex[..2]).join(hex)
}

fn preview(root: &Path, case: &str, artifacts: &[&str]) -> Value {
    let mut args = vec!["--case", case, "retention", "preview"];
    args.extend(artifacts);
    let previewed = run(root, &args, None, 0);
    assert_eq!(previewed["result"], "retention_previewed");
    assert_eq!(previewed["writes"], "nothing");
    previewed
}

fn ids(values: &Value) -> BTreeSet<String> {
    values
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value["record"]["id"].as_str().unwrap().to_owned())
        .collect()
}

fn reason_kinds(shown: &Value) -> Vec<String> {
    shown["freshness"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reason| reason["trigger"]["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn identical_bytes_in_two_cases_stay_readable_only_through_the_reference_that_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init(root);
    fs::write(root.join("shared.txt"), SHARED).unwrap();
    let (case_a, art_a, digest) = start(root, "start-a", "shared.txt");
    let (case_b, art_b, digest_b) = start(root, "start-b", "shared.txt");
    assert_eq!(digest, digest_b, "deduplicated bytes");
    let observe = |key: &str, artifact: &str, selector: Value, quote: Value| {
        record(
            root,
            &case_a,
            key,
            json!([{"op": "observation.add", "payload": {
                "value": if quote.is_null() {
                    json!({"kind": "measurement", "value": "18.7", "unit": "ms"})
                } else {
                    json!({"kind": "text", "statement": "The capture reports p99 18.7 ms."})
                },
                "basis": {"kind": "source", "artifact_ref": exact(artifact, 1),
                    "selector": {"schema_version": "inv.selector.v1", "artifact_digest": digest,
                        "selector": selector},
                    "text_quote": quote},
                "validity": "valid", "quality": [], "limitations": [],
                "evidence_kind": "observational", "variables_changed": []
            }}]),
            0,
        )["created"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let observation = observe("observe", &art_a, json!({"type": "whole"}), Value::Null);
    // A quote is checked in the decoded-text derivative of the original.
    let derivative = preview(root, &case_a, &[&art_a])["references"][0]["derivatives"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let lines = json!({"type": "text_lines", "start_line": 1, "end_line": 1,
        "encoding": "utf-8", "parser_version": "inv.text.utf8.v1"});
    let quoting = observe("quote", &derivative, lines, json!({"exact": "p99 18.7 ms"}));
    let conclusion = record(
        root,
        &case_a,
        "conclude",
        json!([{"op": "conclusion.create", "payload": {
            "statement": "The queue met the latency limit in the benchmark.",
            "scope": "The recorded benchmark.", "significance": "Informs the queue choice.",
            "support_refs": [exact(&observation, 1)], "contradiction_refs": [],
            "caveats": ["One benchmark."]
        }}]),
        0,
    )["created"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sel = selector(root, &digest);
    let read = |case: &str, artifact: &str, exit: i32| {
        run(
            root,
            &[
                "--case",
                case,
                "evidence",
                "read",
                artifact,
                "--selector-file",
                &sel,
            ],
            None,
            exit,
        )
    };
    assert_eq!(read(&case_a, &art_a, 0)["excerpt"]["text"], SHARED);
    let head = run(root, &["--case", &case_a, "brief"], None, 0)["snapshot"]["sequence"]
        .as_u64()
        .unwrap();
    let stored = fs::read(blob(root, &digest)).unwrap();

    // The preview is exact and changes nothing.
    let journal = |case: &str| {
        let mut names: Vec<_> = fs::read_dir(root.join("state/cases").join(case).join("events"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let events_before = journal(&case_a);
    let previewed = preview(root, &case_a, &[&art_a]);
    assert_eq!(journal(&case_a), events_before);
    let loss = &previewed["references"][0];
    assert_eq!(loss["outcome"], "becomes_unavailable");
    assert_eq!(loss["evidence_read_after"], "refused");
    assert_eq!(loss["digest"], digest.as_str());
    assert_eq!(loss["size_bytes"], SHARED.len());
    let dependents = ids(&loss["dependents"]);
    assert!(dependents.contains(&observation) && dependents.contains(&conclusion));
    assert_eq!(previewed["logical_loss"]["references"], 1);
    assert_eq!(previewed["logical_loss"]["bytes"], SHARED.len());
    // Held for others: case B's references are counted, never named.
    let held = &previewed["held_for_others"]["digests"][0];
    assert_eq!(held["digest"], digest.as_str());
    assert!(held["other_cases"]["references"].as_u64().unwrap() >= 1);
    assert!(!previewed.to_string().contains(&case_b));
    assert!(!previewed.to_string().contains(&art_b));
    assert_eq!(previewed["held_for_others"]["bytes"], SHARED.len());
    assert_eq!(previewed["reclaimable"]["digests"], json!([]));
    assert_eq!(previewed["reclaimable"]["reclamation"], "not_performed");
    let quoted: Vec<&Value> = previewed["downstream_copies"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|copy| copy["kind"] == "quoted_text")
        .collect();
    assert_eq!(quoted.len(), 1);
    assert_eq!(quoted[0]["record"]["id"], quoting.as_str());
    // The derivative holds the same digest in this case and keeps it.
    assert_eq!(held["references"][0]["id"], derivative.as_str());

    // Commit the withdrawal the preview described.
    let receipt = record(
        root,
        &case_a,
        "expire-a",
        json!([expire(&loss["artifact_ref"])]),
        0,
    );
    assert_eq!(receipt["updated"][0]["id"], art_a.as_str());

    // Only the reference that keeps the bytes can read them.
    let refused = read(&case_a, &art_a, 5);
    assert_eq!(refused["error"]["code"], "GATE_FAILED");
    assert_eq!(
        refused["error"]["actual"]["availability"],
        "intentionally_removed"
    );
    assert_eq!(
        refused["error"]["actual"]["availability_change"]["actor"],
        ACTOR
    );
    assert_eq!(read(&case_b, &art_b, 0)["excerpt"]["text"], SHARED);
    let other = run(root, &["--case", &case_b, "show", &art_b], None, 0);
    assert_eq!(other["freshness"]["effective"], "current", "{other}");
    assert_eq!(
        other["evidentiary_use"]["availability"], "retained",
        "{other}"
    );

    // Exactly the previewed records now need review for lost availability.
    for id in &dependents {
        let shown = run(root, &["--case", &case_a, "show", id], None, 0);
        assert_eq!(shown["freshness"]["effective"], "needs_review", "{shown}");
        assert!(
            reason_kinds(&shown).contains(&"availability_removed".to_owned()),
            "{shown}"
        );
    }
    let shown = run(root, &["--case", &case_a, "show", &observation], None, 0);
    assert_eq!(
        shown["material_caveat"]["availability_withdrawn"][0]["id"],
        art_a.as_str(),
        "{shown}"
    );
    // At the snapshot before the withdrawal, reads follow today's availability.
    let historical = run(
        root,
        &[
            "--case",
            &case_a,
            "show",
            &observation,
            "--snapshot",
            &head.to_string(),
        ],
        None,
        0,
    );
    assert_eq!(historical["historical"], true);
    assert_eq!(historical["freshness"]["state"], "evidence_unavailable");
    assert_eq!(
        historical["material_caveat"]["availability_withdrawn"][0]["id"],
        art_a.as_str(),
        "{historical}"
    );

    // History stays and nothing is deleted: physical reclamation is off.
    let artifact = run(
        root,
        &["--case", &case_a, "show", &art_a, "--depth", "full"],
        None,
        0,
    );
    let body = &artifact["record"]["body"];
    assert_eq!(body["availability"], "intentionally_removed");
    assert_eq!(body["digest"], digest.as_str());
    assert_eq!(body["size_bytes"], SHARED.len());
    assert_eq!(body["availability_change"]["revision"], 1);
    let first = run(
        root,
        &[
            "--case", &case_a, "show", &art_a, "--at", "1", "--depth", "full",
        ],
        None,
        0,
    );
    assert_eq!(first["record"]["body"]["availability"], "retained");
    assert_eq!(fs::read(blob(root, &digest)).unwrap(), stored);
    let check = run(root, &["check"], None, 0);
    assert_eq!(check["issues"], json!([]), "{check}");

    // A second withdrawal is refused, and the preview says it is already done.
    let again = record(
        root,
        &case_a,
        "expire-a-again",
        json!([expire(&exact(&art_a, 2))]),
        5,
    );
    assert_eq!(again["error"]["code"], "GATE_FAILED");
    let after = preview(root, &case_a, &[&art_a]);
    assert_eq!(after["references"][0]["outcome"], "already_unavailable");
    assert_eq!(after["logical_loss"]["references"], 0);
}

#[test]
fn unique_bytes_are_reclaimable_only_when_every_reference_is_withdrawn() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init(root);
    fs::write(root.join("only.txt"), "token=abc123 one copy\n").unwrap();
    let (case, original, digest) = start(root, "start", "only.txt");
    // Every other Artifact of the case holding the same digest, such as the
    // decoded-text derivative of a UTF-8 file.
    let alone = preview(root, &case, &[&original]);
    let holders: Vec<String> = alone["held_for_others"]["digests"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|held| held["references"].as_array().unwrap().clone())
        .map(|reference| reference["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(alone["reclaimable"]["digests"], json!([]));
    let mut every: Vec<&str> = vec![&original];
    every.extend(holders.iter().map(String::as_str));
    let all = preview(root, &case, &every);
    assert_eq!(all["held_for_others"]["digests"], json!([]), "{all}");
    let size = fs::metadata(blob(root, &digest)).unwrap().len();
    assert_eq!(
        all["reclaimable"]["digests"],
        json!([{"digest": digest, "size_bytes": size}])
    );
    assert_eq!(all["reclaimable"]["bytes"], size);

    let operations: Vec<Value> = all["references"]
        .as_array()
        .unwrap()
        .iter()
        .map(|loss| expire(&loss["artifact_ref"]))
        .collect();
    record(root, &case, "expire-all", Value::Array(operations), 0);
    // Truthful with reclamation disabled: unavailable everywhere, still stored.
    assert!(blob(root, &digest).is_file());
    for id in &every {
        let shown = run(root, &["--case", &case, "show", id], None, 0);
        assert_eq!(
            shown["retention"][0]["label"], "intentionally_removed",
            "{shown}"
        );
    }
    let check = run(root, &["check"], None, 0);
    assert_eq!(check["issues"], json!([]), "{check}");
}

#[test]
fn an_adopted_method_packs_bytes_cannot_be_withdrawn() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init(root);
    fs::write(root.join("reading.txt"), SHARED).unwrap();
    let (case, _, _) = start(root, "start", "reading.txt");
    fs::write(root.join("pack.toml"), PACK).unwrap();
    let retained = run(
        root,
        &["ingest", "--input", "-"],
        Some(&json!({
            "schema_version": "inv.ingest.v1", "command_id": "retain-pack", "actor": ACTOR,
            "case_id": case, "source": {"title": "pack", "format": "application/toml"},
            "paths": ["pack.toml"]
        })),
        0,
    );
    let item = &retained["items"][0];
    let artifact = item["artifact_id"].as_str().unwrap().to_owned();
    let shown = run(root, &["show", &case, "--depth", "full"], None, 0);
    let revision = shown["record"]["header"]["revision"].clone();
    run(
        root,
        &["record", "--input", "-"],
        Some(&json!({
            "schema_version": "inv.command.v1", "command_id": "adopt", "case_id": case,
            "actor": ACTOR, "preconditions": [{"id": case, "revision": revision}],
            "operations": [{"op": "method_pack.adopt", "payload": {
                "artifact_ref": exact(&artifact, 1), "pack_id": "bench-notes",
                "version": "1.0.0", "digest": item["digest"], "reason": "Bench notes."
            }}]
        })),
        0,
    );
    let previewed = preview(root, &case, &[&artifact]);
    assert_eq!(previewed["references"][0]["outcome"], "refused");
    assert_eq!(previewed["references"][0]["refusal"]["code"], "GATE_FAILED");
    assert_eq!(previewed["logical_loss"]["references"], 0);
    let refused = record(
        root,
        &case,
        "expire-pack",
        json!([expire(&exact(&artifact, 1))]),
        5,
    );
    assert_eq!(refused["error"]["code"], "GATE_FAILED");
    assert_eq!(
        refused["error"]["actual"]["method_packs"][0]["pack_id"],
        "bench-notes"
    );
}

#[test]
fn an_export_pinned_before_the_withdrawal_copies_no_bytes_through_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init(root);
    fs::write(root.join("reading.txt"), SHARED).unwrap();
    let (case, _, _) = start(root, "start", "reading.txt");
    let bench = b"{\"metric\":\"latency.p99\",\"value\":18.7}\n";
    fs::write(root.join("bench.json"), bench).unwrap();
    fs::write(
        root.join("envelope.json"),
        json!({
            "schema_version": "inv.producer-run.v1",
            "producer": {"name": "example-benchmark", "version": "0.1.0"},
            "external_run_id": "bench-1", "intent": {"label": "baseline"},
            "execution": {"state": "completed", "exit_code": 0},
            "artifacts": [{"role": "original-output", "path": "./bench.json",
                "media_type": "application/json"}]
        })
        .to_string(),
    )
    .unwrap();
    let ingested = run(
        root,
        &[
            "--case",
            &case,
            "ingest",
            "--input",
            "envelope.json",
            "--actor",
            ACTOR,
        ],
        None,
        0,
    );
    let output = ingested["artifacts"][0]["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let before = ingested["snapshot"]["sequence"].as_u64().unwrap();
    let export = |out: &str, snapshot: Option<u64>| {
        let mut args = vec![
            "--case".to_owned(),
            case.clone(),
            "export".to_owned(),
            "--kind".to_owned(),
            "producer-bundle".to_owned(),
            "--audience".to_owned(),
            "restricted".to_owned(),
            "--out".to_owned(),
            out.to_owned(),
        ];
        if let Some(snapshot) = snapshot {
            args.extend(["--snapshot".to_owned(), snapshot.to_string()]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run(root, &args, None, 0);
        let index: Value =
            serde_json::from_slice(&fs::read(root.join(out).join("producer-bundle.json")).unwrap())
                .unwrap();
        index["runs"][0]["artifacts"][0].clone()
    };
    let shown = export("exports/before", None);
    assert_eq!(shown["retention"], "retained");
    assert!(shown["file"].is_string());
    // That export is a known downstream copy of the bytes.
    let copies = preview(root, &case, &[&output])["downstream_copies"].clone();
    assert!(
        copies
            .as_array()
            .unwrap()
            .iter()
            .any(|copy| copy["kind"] == "export"
                && copy["path"]
                    .as_str()
                    .unwrap()
                    .starts_with("exports/before/")),
        "{copies}"
    );

    record(
        root,
        &case,
        "expire-output",
        json!([expire(&exact(&output, 1))]),
        0,
    );
    // The earlier snapshot still describes the artifact; today's availability governs its bytes.
    let pinned = export("exports/pinned", Some(before));
    assert_eq!(pinned["retention"], "intentionally_removed");
    assert_eq!(pinned["file"], Value::Null);
    let copied = fs::read_dir(root.join("exports/pinned/runs/0001"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    assert!(!copied.contains(&"files".to_owned()), "{copied:?}");
}
