//! A refused request gets a bounded error, however large the request
//! (bn-dla3; plan §20.2 P11).
//!
//! Each request below is at the largest size `record` accepts (just under
//! `MAX_REQUEST_BYTES`) or over it, and puts the bulk of its bytes where an
//! error could echo them: a field value, an unknown key, a duplicate key, an
//! operation name, a reference ID, the command ID, the actor, a number, a
//! nesting depth, invalid UTF-8, thousands of invalid operations. For every
//! one, in JSON and in text, through standard input and through a file:
//! the exit code is a refusal, nothing is committed, the response is at
//! most [`MAX_RESPONSE`] bytes, standard error at most [`MAX_RESPONSE`],
//! the JSON error names a field path of at most [`MAX_PATH`] bytes, and no
//! output holds a run of [`ECHO`] bytes of the filler.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use investigation_core::input::MAX_REQUEST_BYTES;
use serde_json::{Value, json};

/// The most bytes an error response or its standard error may take.
const MAX_RESPONSE: usize = 2048;
/// The most bytes of a field path in an error.
const MAX_PATH: usize = 256;
/// A run of filler this long in any output is an echo of the input.
const ECHO: usize = 160;

fn command(cwd: &Path, args: &[&str]) -> Command {
    let empty_path = cwd.join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_clear()
        .env("PATH", &empty_path)
        .args(["--workspace", "casework"])
        .args(args);
    command
}

/// Run `args` with `stdin`: exit code, standard output, standard error.
fn run(cwd: &Path, args: &[&str], stdin: &[u8]) -> (i32, Vec<u8>, Vec<u8>) {
    let mut child = command(cwd, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    // The binary may refuse before it reads everything.
    let _ = input.write_all(stdin);
    drop(input);
    let output = child.wait_with_output().unwrap();
    (output.status.code().unwrap(), output.stdout, output.stderr)
}

fn envelope(key: &str, case: Option<&str>, operations: Value) -> Value {
    json!({"schema_version":"inv.command.v1","command_id":key,"case_id":case,
        "actor":"agent:bounds","preconditions":[],"operations":operations})
}

/// The bad requests, each with the filler byte it is made of.
fn requests(case: &str) -> Vec<(&'static str, Vec<u8>, u8)> {
    // Room for the envelope around the filler, under the limit.
    let fill = MAX_REQUEST_BYTES - 2048;
    let big = "x".repeat(fill);
    let create = json!([{"op":"case.create","payload":{"title":"T","mode":"measure"}}]);
    let bytes = |value: Value| serde_json::to_vec(&value).unwrap();
    let invalid_operation = json!({"op":"case.create","payload":{"title":1,"mode":"zzzz"}});
    let operations = fill / (bytes(invalid_operation.clone()).len() + 1);
    let mut requests = vec![
        (
            "over the limit",
            bytes(envelope(
                "over",
                None,
                json!([{"op":"case.create","payload":{"title":"x".repeat(MAX_REQUEST_BYTES),"mode":"measure"}}]),
            )),
            b'x',
        ),
        (
            "an overlong field value",
            bytes(envelope(
                "title",
                None,
                json!([{"op":"case.create","payload":{"title":big,"mode":"measure"}}]),
            )),
            b'x',
        ),
        (
            "an overlong unknown key",
            bytes(envelope(
                "unknown",
                None,
                json!([{"op":"case.create","payload":{"title":"T","mode":"measure",big.clone():1}}]),
            )),
            b'x',
        ),
        (
            "an overlong operation name",
            bytes(envelope(
                "operation",
                Some(case),
                json!([{"op":big,"payload":{}}]),
            )),
            b'x',
        ),
        (
            "an overlong reference ID",
            bytes(envelope(
                "reference",
                Some(case),
                json!([{"op":"case.close","payload":{"retrospective_ref":{"id":big,"revision":1}}}]),
            )),
            b'x',
        ),
        (
            "an overlong command ID",
            bytes(envelope(&big, None, create.clone())),
            b'x',
        ),
        (
            "an overlong actor",
            {
                let mut value = envelope("actor", None, create);
                value["actor"] = json!(format!("agent:{big}"));
                bytes(value)
            },
            b'x',
        ),
        (
            "an overlong invalid statement",
            bytes(envelope(
                "statement",
                Some(case),
                json!([{"op":"observation.add","payload":{"value":{"kind":"text","statement":big}}}]),
            )),
            b'x',
        ),
        (
            "thousands of invalid operations",
            bytes(envelope(
                "many",
                None,
                Value::Array(std::iter::repeat_n(invalid_operation.clone(), operations).collect()),
            )),
            b'z',
        ),
    ];
    let head = r#"{"schema_version":"inv.command.v1","command_id":"raw","case_id":null,"actor":"agent:bounds","preconditions":[],"operations":[{"op":"case.create","payload":{"#;
    let tail = r#"}}]}"#;
    let raw = |body: String| format!("{head}{body}{tail}").into_bytes();
    requests.push((
        "a duplicate key after an overlong value",
        raw(format!(
            r#""title":"{big}","title":"again","mode":"measure""#
        )),
        b'x',
    ));
    let key = "k".repeat(fill / 2);
    requests.push((
        "an overlong duplicate key",
        raw(format!(r#""{key}":1,"{key}":2"#)),
        b'k',
    ));
    requests.push((
        "an overlong number",
        raw(format!(
            r#""title":"T","mode":"measure","x":{}"#,
            "9".repeat(fill)
        )),
        b'9',
    ));
    requests.push((
        "nesting past the depth limit",
        format!("{}{}", "[".repeat(fill / 2), "]".repeat(fill / 2)).into_bytes(),
        b'[',
    ));
    let mut invalid = br#"{"command_id":""#.to_vec();
    invalid.extend(std::iter::repeat_n(0xff, fill));
    invalid.extend(br#""}"#);
    requests.push(("invalid UTF-8", invalid, 0xff));
    for (_, request, _) in &requests[1..] {
        assert!(request.len() <= MAX_REQUEST_BYTES, "{}", request.len());
        assert!(
            request.len() > MAX_REQUEST_BYTES - 4096,
            "{}",
            request.len()
        );
    }
    requests
}

/// The longest run of `byte` in `output`.
fn longest_run(output: &[u8], byte: u8) -> usize {
    output
        .split(|each| *each != byte)
        .map(<[u8]>::len)
        .max()
        .unwrap_or(0)
}

fn assert_bounded(
    name: &str,
    how: &str,
    filler: u8,
    (exit, stdout, stderr): (i32, Vec<u8>, Vec<u8>),
) {
    let shown = String::from_utf8_lossy(&stdout[..stdout.len().min(4096)]).into_owned();
    assert_ne!(exit, 0, "{name} ({how}): {shown}");
    assert!(
        stdout.len() <= MAX_RESPONSE,
        "{name} ({how}): {} bytes: {shown}",
        stdout.len()
    );
    assert!(
        stderr.len() <= MAX_RESPONSE,
        "{name} ({how}): {} bytes on standard error",
        stderr.len()
    );
    for output in [&stdout, &stderr] {
        let run = longest_run(output, filler);
        assert!(run < ECHO, "{name} ({how}) echoes {run} bytes: {shown}");
    }
    if how.starts_with("json") {
        let value: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(value["ok"], false, "{name} ({how}): {value:#}");
        assert_eq!(value["commit_status"], "not_committed", "{name} ({how})");
        let path = value["error"]["path"].as_str().unwrap_or_default();
        assert!(path.len() <= MAX_PATH, "{name} ({how}): {path}");
    }
}

#[test]
fn every_refused_request_at_the_size_limit_gets_a_bounded_error() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let empty_path = cwd.join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let init = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_clear()
        .env("PATH", &empty_path)
        .args(["init", "./casework", "--no-vault", "--format", "json"])
        .output()
        .unwrap();
    assert!(init.status.success(), "{init:?}");
    let created = run(
        cwd,
        &["record", "--input", "-", "--format", "json"],
        &serde_json::to_vec(&envelope(
            "case",
            None,
            json!([{"op":"case.create","payload":{"title":"Bounds","mode":"measure"}}]),
        ))
        .unwrap(),
    );
    assert_eq!(created.0, 0, "{}", String::from_utf8_lossy(&created.1));
    let created: Value = serde_json::from_slice(&created.1).unwrap();
    let case = created["created"][0]["id"].as_str().unwrap().to_owned();
    let journal = || {
        fs::read_dir(cwd.join("casework/state/cases"))
            .unwrap()
            .count()
    };
    let before = journal();

    for (name, request, filler) in requests(&case) {
        let file = cwd.join("request.json");
        fs::write(&file, &request).unwrap();
        for format in ["json", "text"] {
            let stdin = run(
                cwd,
                &["record", "--input", "-", "--format", format],
                &request,
            );
            assert_bounded(name, &format!("{format}, standard input"), filler, stdin);
            let from_file = run(
                cwd,
                &["record", "--input", "request.json", "--format", format],
                b"",
            );
            assert_bounded(name, &format!("{format}, file"), filler, from_file);
        }
    }
    assert_eq!(journal(), before, "no request created a case");
}
