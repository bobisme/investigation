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

    // A Rust producer sending the same request gets the identical receipt.
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
