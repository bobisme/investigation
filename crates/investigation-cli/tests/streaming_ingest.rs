//! A large capture piped to `ingest` (W06b.09): it is retained exactly, the
//! JSON receipt alone is on stdout, and streaming progress goes to stderr, at
//! `-v` only, as byte counts.
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    thread,
};

use investigation_store::{journal::hash_bytes, store::ingest::PROGRESS_INTERVAL_BYTES};
use serde_json::{Value, json};

const MIB: usize = 1024 * 1024;
/// Past the old 64 MiB import limit and past one progress interval.
const SIZE: usize = 72 * MIB;
const NAME: &str = "SENTINEL-STREAM-NAME";

fn generated() -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    (0..SIZE)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

fn ingest(
    ws: &Path,
    command: &str,
    verbosity: &[&str],
    bytes: &'static [u8],
) -> (i32, Value, String) {
    let manifest = json!({
        "schema_version": "inv.ingest.v1",
        "command_id": command,
        "actor": "agent:stream-test",
        "new_case": {"title": "Large capture"},
        "source": {"title": "Piped capture"},
        "content": {"kind": "stdin", "name": NAME},
    });
    fs::write(ws.join("manifest.json"), manifest.to_string()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(ws)
        .env_remove("INVESTIGATION_LOG")
        .args(["ingest", "--input", "manifest.json", "--format", "json"])
        .args(["--actor", "agent:stream-test"])
        .args(verbosity)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = thread::spawn(move || stdin.write_all(bytes).unwrap());
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let receipt = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not one JSON receipt ({error}): {stdout}"));
    (
        output.status.code().unwrap(),
        receipt,
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn a_large_piped_capture_is_retained_exactly_with_progress_on_stderr_only() {
    let bytes: &'static [u8] = Box::leak(generated().into_boxed_slice());
    let dir = tempfile::tempdir().unwrap();
    let init = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(dir.path())
        .args(["init", "ws", "--no-vault", "--format", "json"])
        .output()
        .unwrap();
    assert!(init.status.success());
    let ws = dir.path().join("ws");

    let (code, receipt, stderr) = ingest(&ws, "large-verbose", &["-v"], bytes);
    assert_eq!(code, 0, "{stderr}");
    let item = &receipt["items"][0];
    assert_eq!(item["size_bytes"], SIZE);
    assert_eq!(item["digest"], hash_bytes(bytes).as_str());
    let digest = item["digest"]
        .as_str()
        .unwrap()
        .trim_start_matches("sha256:");
    let blob = ws
        .join("state/blobs/sha256")
        .join(&digest[..2])
        .join(digest);
    assert!(fs::read(blob).unwrap() == bytes, "retained bytes differ");
    assert!(
        stderr.contains(&format!("bytes={PROGRESS_INTERVAL_BYTES}"))
            && stderr.contains("streaming"),
        "{stderr}"
    );
    assert!(stderr.contains(&format!("bytes={SIZE}")), "{stderr}");
    // Counts only: never the stream's name or a local path.
    assert!(!stderr.contains(NAME), "{stderr}");
    assert!(!stderr.contains(dir.path().to_str().unwrap()), "{stderr}");

    // A native selector reads its artifact into memory, so one this large is
    // refused as an input error that names the size, not as corruption.
    let mut read = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(&ws)
        .args(["evidence", "read", item["artifact_id"].as_str().unwrap()])
        .args(["--selector-file", "-", "--format", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let selector = json!({"schema_version": "inv.selector.v1",
        "artifact_digest": item["digest"], "selector": {"type": "whole"}});
    read.stdin
        .take()
        .unwrap()
        .write_all(selector.to_string().as_bytes())
        .unwrap();
    let read = read.wait_with_output().unwrap();
    assert!(!read.status.success());
    let refusal: Value = serde_json::from_slice(&read.stdout).unwrap();
    assert_eq!(refusal["error"]["code"], "INVALID_INPUT", "{refusal}");
    assert_eq!(refusal["error"]["actual"]["size_bytes"], SIZE, "{refusal}");

    let (code, receipt, stderr) = ingest(&ws, "large-quiet", &[], bytes);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(receipt["items"][0]["digest"], item["digest"]);
    assert!(
        !stderr.contains("streaming"),
        "default level is noisy: {stderr}"
    );
}
