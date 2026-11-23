//! `capture` commands: the JSON-CLI protocol and the Rust recorder share one
//! request type, one implementation and one receipt.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use investigation_recording::{BeginCapture, BeginReceipt, LocalRecorder, Recorder};
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
