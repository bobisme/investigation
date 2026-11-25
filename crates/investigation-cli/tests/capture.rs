//! `capture` commands: the JSON-CLI protocol and the Rust recorder share one
//! request type, one implementation and one receipt.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use investigation_recording::{
    ArtifactRef, BeginCapture, BeginReceipt, CaptureEvent, FinalizeCapture, FinalizeReceipt,
    ItemReceipt, LocalRecorder, Recorder, StableArtifact,
};
use serde_json::{Value, json};

fn invoke(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = command.spawn().unwrap();
    if let Some(bytes) = stdin {
        process.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    let output = process.wait_with_output().unwrap();
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    output
}

fn json_run(cwd: &Path, args: &[&str], stdin: Option<&[u8]>, exit: i32) -> Value {
    let output = invoke(cwd, args, stdin);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(exit), "{stdout}");
    serde_json::from_str(&stdout).expect(&stdout)
}

fn begin_request() -> Value {
    json!({
        "key": "run-0001/begin",
        "actor": "agent:example-producer",
        "producer": {"name": "example-producer", "version": "0.1.0"},
        "external_run_id": "run-0001",
        "intent": {"label": "synthetic run"},
        "case": {"kind": "new_record_case", "title": "Synthetic runs"},
        "metadata": {"example-org.fixture": {"seed": 7}}
    })
}

#[test]
fn capture_begin_declares_a_run_and_a_retry_returns_the_same_receipt() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    json_run(
        root,
        &["init", "ws", "--no-vault", "--format", "json"],
        None,
        0,
    );
    let request = serde_json::to_vec(&begin_request()).unwrap();
    fs::write(root.join("begin.json"), &request).unwrap();

    let first = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "begin",
            "--input",
            "begin.json",
            "--format",
            "json",
        ],
        None,
        0,
    );
    // Stdout is exactly the recorder's receipt type; nothing is added.
    let receipt: BeginReceipt = serde_json::from_value(first.clone()).unwrap();
    assert_eq!(first["protocol"], "inv.capture.v1");
    assert_eq!(first["commit"]["result"], "committed");
    assert_eq!(
        first["staging_dir"],
        format!("state/staging/{}", receipt.run_id)
    );
    assert!(
        root.join("ws")
            .join(&receipt.staging_dir)
            .join("handle.json")
            .is_file()
    );

    let retry = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "begin",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(&request),
        0,
    );
    assert_eq!(retry["commit"]["result"], "already_committed");
    for field in [
        "handle",
        "run_id",
        "case_id",
        "staging_dir",
        "limits",
        "protocol",
    ] {
        assert_eq!(retry[field], first[field], "{field}");
    }
    assert_eq!(
        retry["commit"]["transaction_id"],
        first["commit"]["transaction_id"]
    );

    // A Rust producer sending the same request gets the identical receipt,
    // less the attribution the CLI adds to its journal receipt (bn-2wct).
    let mut retry = retry;
    assert_eq!(
        retry["commit"]["attribution"],
        json!({"actor":"agent:example-producer","source":"request"})
    );
    retry["commit"]
        .as_object_mut()
        .unwrap()
        .remove("attribution");
    let parsed: BeginCapture = serde_json::from_slice(&request).unwrap();
    let native = LocalRecorder::open(root.join("ws"))
        .unwrap()
        .begin(&parsed)
        .unwrap();
    assert_eq!(serde_json::to_value(&native).unwrap(), retry);

    let check = json_run(
        root,
        &["--workspace", "ws", "check", "--format", "json"],
        None,
        0,
    );
    assert_eq!(check["ok"], true);
    assert_eq!(check["issues"], json!([]));
}

#[test]
fn capture_begin_text_states_that_nothing_was_authorized_or_started() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    json_run(
        root,
        &["init", "ws", "--no-vault", "--format", "json"],
        None,
        0,
    );
    let request = serde_json::to_vec(&begin_request()).unwrap();
    let output = invoke(
        root,
        &["--workspace", "ws", "capture", "begin", "--input", "-"],
        Some(&request),
    );
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("declared  committed"), "{text}");
    assert!(text.contains("nothing was authorized or started"), "{text}");

    let help = invoke(root, &["capture", "begin", "--help"], None);
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(
        help.contains("does not authorize, approve or start any"),
        "{help}"
    );
}

#[test]
fn capture_begin_rejections_are_structured_and_record_nothing() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    json_run(
        root,
        &["init", "ws", "--no-vault", "--format", "json"],
        None,
        0,
    );
    let args = [
        "--workspace",
        "ws",
        "capture",
        "begin",
        "--input",
        "-",
        "--format",
        "json",
    ];

    let mut unknown = begin_request();
    unknown["surprise"] = json!(true);
    let failure = json_run(root, &args, Some(&serde_json::to_vec(&unknown).unwrap()), 2);
    assert_eq!(failure["error"]["code"], "INVALID_INPUT");
    assert_eq!(failure["commit_status"], "not_committed");

    let mut blank = begin_request();
    blank["intent"]["label"] = json!(" ");
    let failure = json_run(root, &args, Some(&serde_json::to_vec(&blank).unwrap()), 2);
    assert_eq!(failure["error"]["path"], "/intent/label");

    let request = serde_json::to_vec(&begin_request()).unwrap();
    let failure = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "--actor",
            "agent:someone-else",
            "capture",
            "begin",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(&request),
        2,
    );
    assert_eq!(failure["error"]["code"], "INVALID_INPUT");

    let failure = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "--read-only",
            "capture",
            "begin",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(&request),
        2,
    );
    assert_eq!(failure["error"]["path"], "--read-only");

    let staging = fs::read_dir(root.join("ws/state/staging")).unwrap().count();
    assert_eq!(staging, 0);
    assert!(
        fs::read_dir(root.join("ws/state/cases"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn capture_append_stages_facts_in_order_and_replays_receipts() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    json_run(
        root,
        &["init", "ws", "--no-vault", "--format", "json"],
        None,
        0,
    );
    let begun = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "begin",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(&serde_json::to_vec(&begin_request()).unwrap()),
        0,
    );
    let handle = begun["handle"].as_str().unwrap();
    let event = |key: &str, position: u64, fact: Value| {
        serde_json::to_vec(&json!({
            "handle": handle, "key": key, "position": position, "fact": fact
        }))
        .unwrap()
    };
    let args = [
        "--workspace",
        "ws",
        "capture",
        "append",
        "--input",
        "-",
        "--format",
        "json",
    ];
    let started = event("started", 1, json!({"kind": "started"}));
    fs::write(root.join("started.json"), &started).unwrap();
    let first = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "append",
            "--input",
            "started.json",
            "--format",
            "json",
        ],
        None,
        0,
    );
    // Stdout is exactly the recorder's receipt type; nothing is added.
    let receipt: ItemReceipt = serde_json::from_value(first.clone()).unwrap();
    assert_eq!(first["result"], "accepted");
    assert_eq!(first["position"], 1);
    assert_eq!(first["run_id"], begun["run_id"]);
    assert_eq!(receipt.handle.to_string(), handle);

    let retry = json_run(root, &args, Some(&started), 0);
    assert_eq!(retry["result"], "already_accepted");
    for field in ["position", "request_digest", "accepted_at", "key", "handle"] {
        assert_eq!(retry[field], first[field], "{field}");
    }

    // A Rust producer resending the same request gets the identical receipt.
    let parsed: CaptureEvent = serde_json::from_slice(&started).unwrap();
    let native = LocalRecorder::open(root.join("ws"))
        .unwrap()
        .append(&parsed)
        .unwrap();
    assert_eq!(serde_json::to_value(&native).unwrap(), retry);

    // Position 2 is missing.
    let gap = event("phase", 3, json!({"kind": "phase_changed", "phase": "run"}));
    let failure = json_run(root, &args, Some(&gap), 4);
    assert_eq!(failure["error"]["code"], "REVISION_CONFLICT");
    assert_eq!(failure["error"]["path"], "/position");
    assert_eq!(failure["error"]["expected"]["position"], 2);
    assert_eq!(failure["commit_status"], "not_committed");

    // The same key with changed bytes conflicts.
    let changed = event("started", 1, json!({"kind": "cancelled"}));
    let failure = json_run(root, &args, Some(&changed), 4);
    assert_eq!(failure["error"]["code"], "IDEMPOTENCY_KEY_REUSED");
    assert_eq!(failure["error"]["path"], "/key");

    let read_only = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "--read-only",
            "capture",
            "append",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(&event("phase", 2, json!({"kind": "started"}))),
        2,
    );
    assert_eq!(read_only["error"]["path"], "--read-only");

    let output = invoke(
        root,
        &["--workspace", "ws", "capture", "append", "--input", "-"],
        Some(&event(
            "phase",
            2,
            json!({"kind": "phase_changed", "phase": "run"}),
        )),
    );
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("accepted  position 2  key phase"), "{text}");
    assert!(
        text.contains("nothing was started, cancelled or rerun"),
        "{text}"
    );

    // Appends are staged: the journal holds only the declaration.
    let check = json_run(
        root,
        &["--workspace", "ws", "check", "--format", "json"],
        None,
        0,
    );
    assert_eq!(check["ok"], true);
    let events = fs::read_dir(root.join("ws/state/cases"))
        .unwrap()
        .map(|case| {
            fs::read_dir(case.unwrap().path().join("events"))
                .unwrap()
                .count()
        })
        .sum::<usize>();
    assert_eq!(events, 1);
}

#[test]
fn capture_attach_retains_verified_bytes_and_refuses_mismatches_without_staging() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    json_run(
        root,
        &["init", "ws", "--no-vault", "--format", "json"],
        None,
        0,
    );
    let outputs = root.join("outputs");
    fs::create_dir(&outputs).unwrap();
    let config = root.join("ws/investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        contents.replace(
            "import_roots = []",
            &format!("import_roots = [{:?}]", outputs.display().to_string()),
        ),
    )
    .unwrap();
    let begun = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "begin",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(&serde_json::to_vec(&begin_request()).unwrap()),
        0,
    );
    let handle = begun["handle"].as_str().unwrap();
    fs::write(outputs.join("result.json"), b"{\"p99\": 18.7}\n").unwrap();
    let args = [
        "--workspace",
        "ws",
        "capture",
        "attach",
        "--input",
        "-",
        "--format",
        "json",
    ];
    let request = |key: &str, position: u64, extra: Value| {
        let mut value = json!({
            "handle": handle,
            "key": key,
            "position": position,
            "role": "original-output",
            "source": {"kind": "file", "path": "result.json"},
            "media_type": "application/json",
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::to_vec(&value).unwrap()
    };

    // A digest mismatch is refused and stages nothing.
    let wrong = request(
        "result",
        1,
        json!({"expected_digest": format!("sha256:{}", "0".repeat(64))}),
    );
    let failure = json_run(root, &args, Some(&wrong), 2);
    assert_eq!(failure["error"]["code"], "INVALID_INPUT");
    assert_eq!(failure["error"]["path"], "/expected_digest");
    assert_eq!(failure["commit_status"], "not_committed");

    let first = json_run(root, &args, Some(&request("result", 1, json!({}))), 0);
    let receipt: ArtifactRef = serde_json::from_value(first.clone()).unwrap();
    assert_eq!(first["receipt"]["result"], "accepted");
    assert_eq!(first["receipt"]["position"], 1);
    assert_eq!(first["availability"], "staged");
    assert_eq!(first["role"], "original-output");
    assert_eq!(first["size_bytes"], 14);
    assert_eq!(receipt.receipt.handle.to_string(), handle);

    // A Rust producer resending the same request gets the identical receipt.
    let parsed: StableArtifact = serde_json::from_slice(&request("result", 1, json!({}))).unwrap();
    let native = LocalRecorder::open(root.join("ws"))
        .unwrap()
        .attach(&parsed)
        .unwrap();
    let mut replayed = first.clone();
    replayed["receipt"]["result"] = json!("already_accepted");
    assert_eq!(serde_json::to_value(&native).unwrap(), replayed);

    // An absent file is recorded as missing.
    let absent = serde_json::to_vec(&json!({
        "handle": handle, "key": "trace", "position": 2, "role": "log",
        "source": {"kind": "file", "path": "trace.log"},
    }))
    .unwrap();
    let output = invoke(
        root,
        &["--workspace", "ws", "capture", "attach", "--input", "-"],
        Some(&absent),
    );
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("accepted  position 2  key trace"), "{text}");
    assert!(text.contains("missing  no bytes retained"), "{text}");
    assert!(
        text.contains("nothing was parsed, started or rerun"),
        "{text}"
    );

    let read_only = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "--read-only",
            "capture",
            "attach",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(&request("other", 3, json!({}))),
        2,
    );
    assert_eq!(read_only["error"]["path"], "--read-only");

    // Attachments are staged: the journal holds only the declaration.
    let check = json_run(
        root,
        &["--workspace", "ws", "check", "--format", "json"],
        None,
        0,
    );
    assert_eq!(check["ok"], true);
}

#[test]
fn capture_finalize_seals_the_handle_and_commits_the_manifest_once() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    json_run(
        root,
        &["init", "ws", "--no-vault", "--format", "json"],
        None,
        0,
    );
    let begun = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "begin",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(&serde_json::to_vec(&begin_request()).unwrap()),
        0,
    );
    let handle = begun["handle"].as_str().unwrap();
    let append = |key: &str, position: u64, fact: Value| {
        json_run(
            root,
            &[
                "--workspace",
                "ws",
                "capture",
                "append",
                "--input",
                "-",
                "--format",
                "json",
            ],
            Some(
                &serde_json::to_vec(&json!({
                    "handle": handle, "key": key, "position": position, "fact": fact
                }))
                .unwrap(),
            ),
            0,
        )
    };
    append("started", 1, json!({"kind": "started"}));
    append(
        "crash",
        2,
        json!({"kind": "error", "message": "worker exited with 3"}),
    );
    let args = [
        "--workspace",
        "ws",
        "capture",
        "finalize",
        "--input",
        "-",
        "--format",
        "json",
    ];
    let request = |last: u64, completion: &str| {
        serde_json::to_vec(&json!({
            "handle": handle,
            "key": "run-0001/finalize",
            "expected_last_position": last,
            "completion": completion,
            "execution": {"state": "failed", "exit_code": 3, "error": "worker exited with 3"},
            "assertions": [{"kind": "reported_metric", "name": "latency.p99", "value": 18.7, "unit": "ms"}],
        }))
        .unwrap()
    };

    // A complete claim with a stale cursor is refused and seals nothing.
    let stale = json_run(root, &args, Some(&request(1, "complete")), 4);
    assert_eq!(stale["error"]["code"], "REVISION_CONFLICT");
    assert_eq!(stale["error"]["path"], "/expected_last_position");
    assert_eq!(stale["commit_status"], "not_committed");

    let first = json_run(root, &args, Some(&request(2, "complete")), 0);
    let receipt: FinalizeReceipt = serde_json::from_value(first.clone()).unwrap();
    assert_eq!(receipt.handle.to_string(), handle);
    assert_eq!(first["finalization"], "finalized");
    assert_eq!(first["execution"], "failed");
    assert_eq!(first["sealed_position"], 2);
    assert_eq!(first["run_id"], begun["run_id"]);
    assert_eq!(first["commit"]["result"], "committed");

    // A Rust producer resending the same request gets the identical receipt.
    let parsed: FinalizeCapture = serde_json::from_slice(&request(2, "complete")).unwrap();
    let native = LocalRecorder::open(root.join("ws"))
        .unwrap()
        .finalize(&parsed)
        .unwrap();
    let mut replayed = first.clone();
    replayed["commit"]["result"] = json!("already_committed");
    assert_eq!(
        serde_json::to_value(&native).unwrap()["commit"]["transaction_id"],
        replayed["commit"]["transaction_id"]
    );
    assert_eq!(
        serde_json::to_value(&native).unwrap()["manifest_digest"],
        replayed["manifest_digest"]
    );

    let output = invoke(
        root,
        &["--workspace", "ws", "capture", "finalize", "--input", "-"],
        Some(&request(2, "complete")),
    );
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("finalized  already_committed"), "{text}");
    assert!(text.contains("execution failed"), "{text}");
    assert!(
        text.contains("no Observation, Assessment or Conclusion"),
        "{text}"
    );

    let read_only = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "--read-only",
            "capture",
            "finalize",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(&request(2, "complete")),
        2,
    );
    assert_eq!(read_only["error"]["path"], "--read-only");

    // The finalization is a verified journal transaction with its blobs.
    let check = json_run(
        root,
        &["--workspace", "ws", "check", "--format", "json"],
        None,
        0,
    );
    assert_eq!(check["ok"], true);
}

fn capture_request(root: &Path, verb: &str, request: &Value, exit: i32) -> Value {
    json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            verb,
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(request.to_string().as_bytes()),
        exit,
    )
}

fn stage_capture_inventory(root: &Path) -> Value {
    json_run(
        root,
        &["init", "ws", "--no-vault", "--format", "json"],
        None,
        0,
    );
    let outputs = root.join("outputs");
    fs::create_dir(&outputs).unwrap();
    let config = root.join("ws/investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        contents.replace(
            "import_roots = []",
            &format!("import_roots = [{:?}]", outputs.display().to_string()),
        ),
    )
    .unwrap();
    fs::write(outputs.join("body.json"), b"{\"value\": 42}\n").unwrap();
    fs::write(outputs.join("run.log"), b"Finished\n").unwrap();
    let begun = capture_request(root, "begin", &begin_request(), 0);
    let handle = &begun["handle"];
    for (key, position, role, source) in [
        (
            "body",
            1,
            "original-output",
            json!({"kind": "file", "path": "body.json"}),
        ),
        ("log", 2, "log", json!({"kind": "file", "path": "run.log"})),
        (
            "trace",
            3,
            "log",
            json!({"kind": "missing", "reason": "Tracing was disabled"}),
        ),
    ] {
        let receipt = capture_request(
            root,
            "attach",
            &json!({"handle": handle, "key": key, "position": position, "role": role, "source": source}),
            0,
        );
        assert_eq!(receipt["receipt"]["result"], "accepted");
        assert_eq!(
            receipt["availability"],
            if key == "trace" { "missing" } else { "staged" },
        );
    }
    capture_request(
        root,
        "append",
        &json!({"handle": handle, "key": "started", "position": 4, "fact": {"kind": "started"}}),
        0,
    );
    begun
}

#[test]
fn strict_finalize_rejects_inventory_atomically_and_replays_the_corrected_request() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let begun = stage_capture_inventory(root);
    let handle = begun["handle"].as_str().unwrap();
    let recover = || {
        json_run(
            root,
            &[
                "--workspace",
                "ws",
                "capture",
                "recover",
                "--handle",
                handle,
                "--format",
                "json",
            ],
            None,
            0,
        )
    };
    let before = recover();
    assert_eq!(before["handles"][0]["seal"], "open");
    assert_eq!(before["handles"][0]["next_position"], 5);
    assert_eq!(before["handles"][0]["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        before["handles"][0]["artifacts"].as_array().unwrap().len(),
        3
    );
    let state = || {
        let mut files = walk(&root.join("ws/state"));
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    };
    let before_state = state();
    let mut request = json!({
        "handle": handle,
        "key": "run-0001/finalize",
        "expected_last_position": 4,
        "completion": "complete",
        "execution": {"state": "completed"},
        "strict_artifact_inventory": true,
        "declared_artifacts": ["body", "started", "other"],
    });
    let refused = capture_request(root, "finalize", &request, 4);
    assert_eq!(refused["commit_status"], "not_committed");
    assert_eq!(refused["error"]["code"], "REVISION_CONFLICT");
    assert_eq!(refused["error"]["path"], "/declared_artifacts");
    assert_eq!(
        refused["error"]["actual"],
        json!({"missing": ["other", "started"], "unexpected": ["log", "trace"]})
    );
    let repair: Vec<&str> = refused["error"]["repair"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap())
        .collect();
    assert_eq!(
        &repair[3..],
        ["capture", "recover", "--handle", handle, "--format", "json"]
    );
    assert_eq!(
        json_run(root, &repair[1..], None, 0),
        before,
        "the open handle keeps its original receipts"
    );
    assert_eq!(
        state(),
        before_state,
        "refusal writes no seal, blobs or journal event"
    );

    // The acknowledged missing attachment belongs in the inventory too.
    request["declared_artifacts"] = json!(["trace", "body", "log"]);
    let committed = capture_request(root, "finalize", &request, 0);
    assert_eq!(committed["commit"]["result"], "committed");
    assert_eq!(committed["finalization"], "finalized");

    // Model a lost stdout response by retrying the original request.
    let mut replay = committed;
    replay["commit"]["result"] = json!("already_committed");
    assert_eq!(capture_request(root, "finalize", &request, 0), replay);
    assert_eq!(recover()["handles"][0]["finalized"], replay);

    let mut changed = request.clone();
    changed["strict_artifact_inventory"] = json!(false);
    let reused = capture_request(root, "finalize", &changed, 4);
    assert_eq!(reused["error"]["code"], "IDEMPOTENCY_KEY_REUSED");
    assert_eq!(reused["error"]["path"], "/key");

    let retired = json_run(
        root,
        &["--workspace", "ws", "repair", "staging", "--format", "json"],
        None,
        0,
    );
    assert_eq!(retired["handles"][0]["result"], "retired");
    assert_eq!(capture_request(root, "finalize", &request, 0), replay);
    assert_eq!(recover()["handles"][0]["finalized"], replay);
}

#[test]
fn permissive_finalize_preserves_omissions_and_default_key_equivalence() {
    for (completion, cursor, finalization, missing_positions) in [
        ("complete", 4, "finalized", json!(null)),
        ("partial", 6, "finalized_partial", json!([5, 6])),
        ("interrupted", 3, "finalized_partial", json!(null)),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let begun = stage_capture_inventory(root);
        let mut request = json!({
            "handle": begun["handle"],
            "key": "run-0001/finalize",
            "expected_last_position": cursor,
            "completion": completion,
            "execution": {"state": "completed"},
            "declared_artifacts": ["body", "other"],
        });
        if completion != "complete" {
            let mut invalid = request.clone();
            invalid["strict_artifact_inventory"] = json!(true);
            let refused = capture_request(root, "finalize", &invalid, 2);
            assert_eq!(refused["error"]["code"], "INVALID_INPUT");
            assert_eq!(refused["error"]["path"], "/strict_artifact_inventory");
        }
        let mut receipt = capture_request(root, "finalize", &request, 0);
        assert_eq!(receipt["commit"]["result"], "committed");
        assert_eq!(receipt["finalization"], finalization);
        assert_eq!(receipt["sealed_position"], 4);
        assert_eq!(receipt["missing_positions"], missing_positions);
        let run_id = begun["run_id"].as_str().unwrap();
        let run = json_run(
            root,
            &[
                "--workspace",
                "ws",
                "show",
                run_id,
                "--depth",
                "full",
                "--format",
                "json",
            ],
            None,
            0,
        );
        let body = &run["record"]["body"];
        assert_eq!(body["evidence_completeness"], "partial");
        let artifacts = body["capture_finalization"]["artifacts"]
            .as_array()
            .unwrap();
        assert_eq!(artifacts[1]["key"], "log");
        assert_eq!(artifacts[1]["declared"], false);
        assert_eq!(artifacts[1]["retention"]["state"], "retained");
        assert_eq!(artifacts[3]["key"], "other");
        assert_eq!(artifacts[3]["retention"]["cause"], "not_attached");

        request["strict_artifact_inventory"] = json!(false);
        receipt["commit"]["result"] = json!("already_committed");
        assert_eq!(capture_request(root, "finalize", &request, 0), receipt);
    }
}

#[test]
fn capture_recover_reports_accepted_state_through_the_same_recorder_and_writes_nothing() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    json_run(
        root,
        &["init", "ws", "--no-vault", "--format", "json"],
        None,
        0,
    );
    let begun = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "begin",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(&serde_json::to_vec(&begin_request()).unwrap()),
        0,
    );
    let handle = begun["handle"].as_str().unwrap();
    json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "append",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(
            &serde_json::to_vec(&json!({
                "handle": handle, "key": "started", "position": 1, "fact": {"kind": "started"}
            }))
            .unwrap(),
        ),
        0,
    );
    // A gap is refused with the command that reports where to resume.
    let gap = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "append",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(
            &serde_json::to_vec(&json!({
                "handle": handle, "key": "late", "position": 3, "fact": {"kind": "started"}
            }))
            .unwrap(),
        ),
        4,
    );
    let repair: Vec<String> =
        serde_json::from_value(gap["error"]["repair"]["argv"].clone()).unwrap();
    let workspace = root.join("ws").canonicalize().unwrap();
    assert_eq!(
        repair,
        [
            "investigation",
            "--workspace",
            workspace.to_str().unwrap(),
            "capture",
            "recover",
            "--handle",
            handle,
            "--format",
            "json"
        ]
    );

    let journal = |root: &Path| {
        let mut entries: Vec<_> = walk(&root.join("ws/state"));
        entries.sort();
        entries
    };
    let before = journal(root);
    // The repair argv runs as given, also read-only.
    let mut args = vec!["--read-only"];
    args.extend(repair[1..].iter().map(String::as_str));
    let by_handle = json_run(root, &args, None, 0);
    let unsettled = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "recover",
            "--format",
            "json",
        ],
        None,
        0,
    );
    let scoped = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "recover",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(&serde_json::to_vec(&json!({"kind": "unsettled"})).unwrap()),
        0,
    );
    assert_eq!(journal(root), before, "recover writes nothing");
    assert_eq!(unsettled, scoped);
    assert_eq!(unsettled, by_handle);
    let reported = &by_handle["handles"][0];
    assert_eq!(reported["handle"], handle);
    assert_eq!(reported["seal"], "open");
    assert_eq!(reported["staging"], "present");
    assert_eq!(reported["execution"], "unknown");
    assert_eq!(reported["last_position"], 1);
    assert_eq!(reported["next_position"], 2);
    assert_eq!(reported["items"][0]["key"], "started");

    // The CLI prints what the Rust recorder returns, with each next step
    // scoped to the workspace.
    let native = LocalRecorder::open(root.join("ws"))
        .unwrap()
        .recover(&serde_json::from_value(json!({"kind": "handle", "handle": handle})).unwrap())
        .unwrap();
    let mut native = serde_json::to_value(&native).unwrap();
    for step in native["handles"][0]["next"].as_array_mut().unwrap() {
        let argv = step["argv"].as_array_mut().unwrap();
        assert_eq!(argv[1], "capture");
        argv.splice(
            1..1,
            [json!("--workspace"), json!(workspace.to_str().unwrap())],
        );
    }
    assert_eq!(native, by_handle);

    let output = invoke(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "recover",
            "--handle",
            handle,
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("open  present"), "{text}");
    assert!(text.contains("execution unknown"), "{text}");
    assert!(
        text.contains("next      finalize: investigation --workspace"),
        "{text}"
    );
    assert!(text.contains("nothing was rerun"), "{text}");

    let unknown = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "recover",
            "--handle",
            "handle_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
            "--format",
            "json",
        ],
        None,
        3,
    );
    assert_eq!(unknown["error"]["code"], "UNKNOWN_REFERENCE");
    assert_eq!(unknown["error"]["path"], "/handle");
    let malformed = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "recover",
            "--handle",
            "run_x",
            "--format",
            "json",
        ],
        None,
        2,
    );
    assert_eq!(malformed["error"]["path"], "--handle");
}

/// Every file beneath `directory` with its bytes.
fn walk(directory: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            entries.extend(walk(&path));
        } else {
            let bytes = fs::read(&path).unwrap();
            entries.push((path, bytes));
        }
    }
    entries
}

#[test]
fn evidence_read_returns_exact_bytes_from_a_finalized_capture() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::create_dir(root.join("outputs")).unwrap();
    json_run(
        root,
        &[
            "init",
            "ws",
            "--no-vault",
            "--import-root",
            "outputs",
            "--json",
        ],
        None,
        0,
    );
    let bytes = b"header\r\n\0\xff\xfe\r\nfooter\r\n";
    fs::write(root.join("outputs/result.bin"), bytes).unwrap();
    let capture = |op, request: Value| {
        json_run(
            root,
            &["--workspace", "ws", "capture", op, "--input", "-", "--json"],
            Some(&serde_json::to_vec(&request).unwrap()),
            0,
        )
    };
    let begun = capture("begin", begin_request());
    let handle = begun["handle"].as_str().unwrap();
    let attached = capture(
        "attach",
        json!({
            "handle": handle, "key": "result", "position": 1, "role": "original-output",
            "source": {"kind": "file", "path": "result.bin"},
            "media_type": "application/octet-stream",
        }),
    );
    let hex = "78d110bb8c12ac2c1b3abb7798599fed1320c5ab14086fefdd3f620329bdf6ef";
    let digest = format!("sha256:{hex}");
    assert_eq!(attached["digest"], digest);
    capture(
        "finalize",
        json!({
            "handle": handle, "key": "run-0001/finalize", "expected_last_position": 1,
            "completion": "complete", "execution": {"state": "completed", "exit_code": 0},
        }),
    );
    let run = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "show",
            begun["run_id"].as_str().unwrap(),
            "--depth",
            "full",
            "--json",
        ],
        None,
        0,
    );
    let retained = &run["record"]["body"]["capture_finalization"]["artifacts"][0]["retention"];
    assert_eq!(retained["state"], "retained");
    assert_eq!(retained["digest"], digest);
    let artifact = &retained["artifact_ref"];
    let id = artifact["id"].as_str().unwrap();
    let read = |artifact_digest: &str, selector: &Value, exit| {
        let request = json!({
            "schema_version": "inv.selector.v1", "artifact_digest": artifact_digest,
            "selector": selector,
        });
        json_run(
            root,
            &[
                "--workspace",
                "ws",
                "evidence",
                "read",
                id,
                "--selector-file",
                "-",
                "--json",
            ],
            Some(&serde_json::to_vec(&request).unwrap()),
            exit,
        )
    };
    let range = |start, end| json!({"type": "byte_range", "start": start, "end": end});
    let whole = json!({"type": "whole"});
    let bounded = range(8, 13);
    let end = bytes.len() as i32;
    for (selector, start, end, encoded) in [
        (&whole, 0, end, "aGVhZGVyDQoA//4NCmZvb3Rlcg0K"),
        (&bounded, 8, 13, "AP/+DQo="),
    ] {
        let result = read(&digest, selector, 0);
        assert_eq!(result["schema_version"], "inv.response.v1");
        assert_eq!(result["result"], "evidence_read");
        assert_eq!(result["artifact"], *artifact);
        assert_eq!(result["selector"]["artifact_digest"], digest);
        assert_eq!(result["selector"]["selector"], *selector);
        assert_eq!(result["content_trust"], "untrusted_evidence");
        assert_eq!(
            result["excerpt"],
            json!({"availability": "binary", "start": start, "end": end, "bytes_base64": encoded})
        );
    }
    let text = read(&digest, &range(0, 8), 0);
    assert_eq!(
        text["excerpt"],
        json!({"availability": "text", "start": 0, "end": 8, "text": "header\r\n"})
    );

    for selector in [
        range(-1, 13),
        range(8, -1),
        range(13, 8),
        range(8, 8),
        range(8, end + 1),
    ] {
        let result = read(&digest, &selector, 2);
        assert_eq!(result["error"]["code"], "INVALID_INPUT", "{selector}");
        assert!(result.get("excerpt").is_none(), "{result}");
    }
    let wrong = read(&format!("sha256:{}", "0".repeat(64)), &whole, 2);
    assert_eq!(wrong["error"]["code"], "INVALID_INPUT");
    assert!(wrong.get("excerpt").is_none(), "{wrong}");

    // Corrupt a byte outside the selected range without changing the size.
    let blob = root.join("ws/state/blobs/sha256").join(&hex[..2]).join(hex);
    let mut corrupted = *bytes;
    corrupted[0] = b'H';
    fs::remove_file(&blob).unwrap();
    fs::write(&blob, corrupted).unwrap();
    let corrupt = read(&digest, &bounded, 6);
    assert_eq!(corrupt["error"]["code"], "INTEGRITY_FAILURE");
    assert!(corrupt.get("excerpt").is_none(), "{corrupt}");

    // Effective classification takes precedence even over blob verification.
    json_run(
        root,
        &["--workspace", "ws", "record", "--input", "-", "--json"],
        Some(
            &serde_json::to_vec(&json!({
                "schema_version": "inv.command.v1", "command_id": "restrict-result",
                "case_id": begun["case_id"], "actor": "agent:example-producer",
                "preconditions": [], "operations": [{"op": "classification.raise", "payload": {
                    "subject_ref": artifact, "level": "restricted", "reason": "Restricted result"
                }}]
            }))
            .unwrap(),
        ),
        0,
    );
    for selector in [&whole, &bounded] {
        let restricted = read(&digest, selector, 5);
        assert_eq!(restricted["error"]["code"], "GATE_FAILED");
        assert_eq!(restricted["error"]["path"], "/artifact");
        assert!(restricted.get("excerpt").is_none(), "{restricted}");
    }
}

#[test]
fn repair_staging_retires_only_finalized_staging_and_check_names_it() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    json_run(
        root,
        &["init", "ws", "--no-vault", "--format", "json"],
        None,
        0,
    );
    let begun = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "begin",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(&serde_json::to_vec(&begin_request()).unwrap()),
        0,
    );
    let handle = begun["handle"].as_str().unwrap();
    let staging = root.join("ws").join(begun["staging_dir"].as_str().unwrap());
    let fact =
        json!({"handle": handle, "key": "started", "position": 1, "fact": {"kind": "started"}});
    json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "append",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(&serde_json::to_vec(&fact).unwrap()),
        0,
    );
    let retire = ["--workspace", "ws", "repair", "staging", "--format", "json"];

    // An open handle is kept, with its reason.
    let open = json_run(root, &retire, None, 0);
    assert_eq!(open["result"], "staging_retired");
    assert_eq!(open["handles"][0]["handle"], handle);
    assert_eq!(open["handles"][0]["result"], "kept");
    assert_eq!(open["handles"][0]["reason"], "open");
    assert!(staging.join("handle.json").exists());
    let check = json_run(
        root,
        &["--workspace", "ws", "check", "--format", "json"],
        None,
        0,
    );
    assert_eq!(check["staging"]["active"], 1);

    json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "finalize",
            "--input",
            "-",
            "--format",
            "json",
        ],
        Some(
            &serde_json::to_vec(&json!({
                "handle": handle,
                "key": "run-0001/finalize",
                "expected_last_position": 1,
                "completion": "complete",
                "execution": {"state": "completed", "exit_code": 0},
            }))
            .unwrap(),
        ),
        0,
    );
    let check = json_run(
        root,
        &["--workspace", "ws", "check", "--format", "json"],
        None,
        0,
    );
    assert_eq!(check["ok"], true);
    assert_eq!(check["staging"]["retirable"], 1);
    let issue = check["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "STAGING_RETIRABLE")
        .unwrap();
    assert_eq!(issue["severity"], "warning");
    assert_eq!(issue["objects"][0], begun["run_id"]);
    let repair: Vec<&str> = issue["repair"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap())
        .collect();
    assert_eq!(repair[1], "--workspace");
    assert_eq!(repair[3..], ["repair", "staging", "--format", "json"]);

    let read_only = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "--read-only",
            "repair",
            "staging",
            "--format",
            "json",
        ],
        None,
        2,
    );
    assert_eq!(read_only["error"]["path"], "--read-only");
    assert!(staging.exists());

    let retired = json_run(root, &retire, None, 0);
    assert_eq!(retired["handles"][0]["result"], "retired");
    assert_eq!(retired["handles"][0]["run_id"], begun["run_id"]);
    assert!(!staging.exists());
    let again = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "repair",
            "staging",
            "--handle",
            handle,
            "--format",
            "json",
        ],
        None,
        0,
    );
    assert_eq!(again["handles"][0]["result"], "already_retired");

    let output = invoke(root, &["--workspace", "ws", "repair", "staging"], None);
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("No capture staging to retire."), "{text}");

    let check = json_run(
        root,
        &["--workspace", "ws", "check", "--format", "json"],
        None,
        0,
    );
    assert_eq!(check["staging"]["retired"], 1);
    assert_eq!(check["issues"], json!([]));
    let recovered = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "recover",
            "--handle",
            handle,
            "--format",
            "json",
        ],
        None,
        0,
    );
    assert_eq!(recovered["handles"][0]["staging"], "retired");
    assert_eq!(recovered["handles"][0]["items"][0]["key"], "started");

    let unknown = json_run(
        root,
        &[
            "--workspace",
            "ws",
            "repair",
            "staging",
            "--handle",
            "not-a-handle",
            "--format",
            "json",
        ],
        None,
        2,
    );
    assert_eq!(unknown["error"]["path"], "--handle");
}
