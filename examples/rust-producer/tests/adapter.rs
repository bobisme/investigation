//! End-to-end tests of the example adapter. They run the real binary against a
//! temporary workspace and read the result back through the public recording API.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use investigation_recording::{
    ExecutionStatus, Finalization, LocalRecorder, Position, Recorder, RecoverScope,
};
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_example-rust-producer");
const RESULT: &str = "{\"metric\":\"latency.p99\",\"value\":18.7,\"unit\":\"ms\"}\n";

struct Fixture {
    _temporary: tempfile::TempDir,
    workspace: PathBuf,
    output: PathBuf,
}

fn fixture() -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let output = temporary.path().join("out");
    fs::create_dir(&output).unwrap();
    let root = fs::canonicalize(&output).unwrap();
    investigation_store::Workspace::init_with_import_roots(&workspace, false, &[root]).unwrap();
    Fixture {
        _temporary: temporary,
        workspace,
        output,
    }
}

fn adapter(fixture: &Fixture, mode: &str, extra: &[&str]) -> Output {
    adapter_scenario(fixture, mode, "synthetic-run", extra)
}

fn adapter_scenario(fixture: &Fixture, mode: &str, scenario: &str, extra: &[&str]) -> Output {
    Command::new(BIN)
        .arg(mode)
        .args(["--workspace".as_ref(), fixture.workspace.as_os_str()])
        .args(["--output-dir".as_ref(), fixture.output.as_os_str()])
        .args([
            "--scenario",
            scenario,
            "--producer",
            "example-producer",
            "--external-run-id",
            "run-0001",
        ])
        .args(extra)
        .output()
        .unwrap()
}

fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("exactly one JSON object on stdout")
}

fn results(report: &Value) -> Vec<(String, String)> {
    report["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|call| {
            (
                call["operation"].as_str().unwrap().to_owned(),
                call["result"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn recovered(fixture: &Fixture) -> investigation_recording::HandleRecovery {
    let recorder = LocalRecorder::open(&fixture.workspace).unwrap();
    let mut state = recorder.recover(&RecoverScope::Unsettled).unwrap();
    state.handles.pop().expect("one unsettled handle")
}

fn settled(fixture: &Fixture, handle: &str) -> investigation_recording::HandleRecovery {
    let recorder = LocalRecorder::open(&fixture.workspace).unwrap();
    let handle = handle.parse().unwrap();
    let mut state = recorder.recover(&RecoverScope::Handle { handle }).unwrap();
    state.handles.pop().unwrap()
}

#[test]
fn describe_lists_the_scenario_and_faults() {
    let out = Command::new(BIN).arg("describe").output().unwrap();
    assert!(out.status.success());
    let value = report(&out);
    assert_eq!(value["schema_version"], "inv.producer-adapter.v1");
    assert_eq!(value["protocols"][0], "inv.capture.v1");
    assert_eq!(value["scenarios"][0], "synthetic-run");
    assert_eq!(value["faults"][0], "drop-response");
    assert_eq!(value["faults"][1], "crash-after");
}

#[test]
fn run_finalizes_a_record_mode_run_offline() {
    let fixture = fixture();
    let out = adapter(&fixture, "run", &[]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let value = report(&out);
    assert_eq!(value["outcome"], "finalized");
    assert_eq!(value["external_run_id"], "run-0001");
    assert_eq!(
        results(&value),
        [
            ("capture.begin", "committed"),
            ("capture.append", "accepted"),
            ("capture.attach", "accepted"),
            ("capture.append", "accepted"),
            ("capture.finalize", "committed"),
        ]
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
    );
    assert_eq!(
        fs::read_to_string(fixture.output.join("result.json")).unwrap(),
        RESULT
    );

    let handle = value["handle"].as_str().unwrap();
    let state = settled(&fixture, handle);
    let finalized = state.finalized.expect("finalized");
    assert_eq!(finalized.finalization, Finalization::Finalized);
    assert_eq!(finalized.execution, ExecutionStatus::Completed);
    assert_eq!(finalized.sealed_position, Position::new(3));
    assert_eq!(state.last_position, Position::new(3));
    assert_eq!(state.items.len(), 2, "two lifecycle facts");
    assert_eq!(state.artifacts.len(), 1);
    assert_eq!(state.producer.name.as_str(), "example-producer");
    assert_eq!(state.external_run_id.as_str(), "run-0001");
    assert_eq!(value["run_id"], state.run_id.to_string());
    assert_eq!(value["finalize"]["run_id"], value["run_id"]);
    // No unsettled handle is left behind.
    let recorder = LocalRecorder::open(&fixture.workspace).unwrap();
    assert!(
        recorder
            .recover(&RecoverScope::Unsettled)
            .unwrap()
            .handles
            .is_empty()
    );
}

#[test]
fn an_identical_retry_is_idempotent() {
    let fixture = fixture();
    let first = report(&adapter(&fixture, "run", &[]));
    let second_out = adapter(&fixture, "run", &[]);
    assert_eq!(second_out.status.code(), Some(0));
    let second = report(&second_out);
    assert_eq!(first["run_id"], second["run_id"]);
    assert_eq!(first["handle"], second["handle"]);
    assert_eq!(
        first["finalize"]["manifest_digest"],
        second["finalize"]["manifest_digest"]
    );
    // The recorded Run is not duplicated: `begin` replays and the run stops there.
    assert_eq!(
        results(&second),
        [("capture.begin".to_owned(), "already_committed".to_owned())]
    );
}

#[test]
fn a_dropped_response_resends_the_identical_request() {
    for (operation, calls) in [("begin", 6), ("append", 6), ("attach", 6), ("finalize", 6)] {
        let fixture = fixture();
        let fault = format!("drop-response:{operation}:1");
        let out = adapter(&fixture, "run", &["--fault", &fault]);
        assert_eq!(out.status.code(), Some(0), "{operation}: {out:?}");
        let value = report(&out);
        assert_eq!(value["outcome"], "finalized");
        let got = results(&value);
        assert_eq!(got.len(), calls, "{operation}: {got:?}");
        assert!(
            got.iter().any(|(_, r)| r.starts_with("already_")),
            "{operation}: the resend reports a replay: {got:?}"
        );
        let state = settled(&fixture, value["handle"].as_str().unwrap());
        assert_eq!(state.items.len(), 2, "{operation}: nothing recorded twice");
        assert_eq!(state.artifacts.len(), 1);
        assert!(state.finalized.is_some());
    }
}

#[test]
fn a_crash_then_resume_finalizes_the_same_run_without_repeating_steps() {
    for fault in [
        "crash-after:begin:1",
        "crash-after:append:1",
        "crash-after:attach:1",
        "crash-after:append:2",
        "crash-after:finalize:1",
    ] {
        let fixture = fixture();
        let crashed = adapter(&fixture, "run", &["--fault", fault]);
        assert_eq!(crashed.status.code(), Some(75), "{fault}");
        let before = report(&crashed);
        assert_eq!(before["outcome"], "crashed");
        let run_id = before["run_id"].clone();
        assert!(run_id.is_string(), "{fault}: begin had returned");

        let resumed_out = adapter(&fixture, "resume", &[]);
        assert_eq!(
            resumed_out.status.code(),
            Some(0),
            "{fault}: {resumed_out:?}"
        );
        let resumed = report(&resumed_out);
        assert_eq!(resumed["outcome"], "finalized", "{fault}");
        assert_eq!(resumed["run_id"], run_id, "{fault}: same Run");
        assert_eq!(resumed["handle"], before["handle"]);

        // The resumed run sent no step that the crashed run had completed.
        let sent: Vec<_> = before["calls"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|call| call["operation"] != "capture.begin")
            .map(|call| call["key"].clone())
            .collect();
        for call in resumed["calls"].as_array().unwrap() {
            if call["operation"] != "capture.begin" && call["operation"] != "capture.finalize" {
                assert!(!sent.contains(&call["key"]), "{fault}: repeated {call}");
            }
        }
        let state = settled(&fixture, resumed["handle"].as_str().unwrap());
        assert_eq!(state.last_position, Position::new(3), "{fault}");
        assert_eq!(state.items.len(), 2, "{fault}");
        assert_eq!(state.artifacts.len(), 1, "{fault}");
        assert_eq!(
            state.finalized.unwrap().execution,
            ExecutionStatus::Completed
        );
    }
}

#[test]
fn recover_reports_the_resume_point_after_a_crash() {
    let fixture = fixture();
    let crashed = adapter(&fixture, "run", &["--fault", "crash-after:attach:1"]);
    assert_eq!(crashed.status.code(), Some(75));
    let state = recovered(&fixture);
    assert_eq!(state.last_position, Position::new(2));
    assert_eq!(state.next_position, Position::new(3));
    assert!(state.finalized.is_none());
    assert_eq!(state.artifacts.len(), 1);
}

#[test]
fn a_result_file_it_cannot_vouch_for_settles_the_run_as_interrupted() {
    let fixture = fixture();
    let crashed = adapter(&fixture, "run", &["--fault", "crash-after:append:1"]);
    assert_eq!(crashed.status.code(), Some(75));
    fs::write(fixture.output.join("result.json"), b"somebody else's bytes").unwrap();
    let out = adapter(&fixture, "resume", &[]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let value = report(&out);
    assert_eq!(value["outcome"], "finalized");
    assert_eq!(value["finalize"]["execution"], "unknown");
    let state = settled(&fixture, value["handle"].as_str().unwrap());
    assert_eq!(state.last_position, Position::new(1));
    assert_eq!(
        fs::read(fixture.output.join("result.json")).unwrap(),
        b"somebody else's bytes",
        "never overwrites bytes it did not write"
    );
}

#[test]
fn usage_errors_exit_2_with_empty_stdout() {
    let fixture = fixture();
    let cases: Vec<Vec<&str>> = vec![
        vec![],
        vec!["bogus"],
        vec!["run"],
        vec!["run", "--scenario", "nope"],
    ];
    for args in cases {
        let out = Command::new(BIN).args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    for (fault, why) in [
        ("explode:begin:1", "unknown fault"),
        ("crash-after:recover:1", "unknown operation"),
        ("crash-after:append:0", "call numbers start at 1"),
    ] {
        let out = adapter(&fixture, "run", &["--fault", fault]);
        assert_eq!(out.status.code(), Some(2), "{why}");
        assert!(out.stdout.is_empty(), "{why}");
    }
    let out = Command::new(BIN)
        .args(["run", "--scenario", "other"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    // Nothing was called: the workspace holds no handle.
    let recorder = LocalRecorder::open(Path::new(&fixture.workspace)).unwrap();
    assert!(
        recorder
            .recover(&RecoverScope::Unsettled)
            .unwrap()
            .handles
            .is_empty()
    );
}

#[test]
fn describe_lists_every_scenario() {
    let out = Command::new(BIN).arg("describe").output().unwrap();
    assert_eq!(
        report(&out)["scenarios"],
        serde_json::json!([
            "synthetic-run",
            "partial-evidence",
            "interrupted-run",
            "late-item"
        ])
    );
}

#[test]
fn partial_evidence_records_a_missing_artifact_and_a_gap() {
    let fixture = fixture();
    let out = adapter_scenario(&fixture, "run", "partial-evidence", &[]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let value = report(&out);
    assert_eq!(value["outcome"], "finalized");
    assert_eq!(value["calls"].as_array().unwrap().len(), 6);
    let state = settled(&fixture, value["handle"].as_str().unwrap());
    assert_eq!(state.last_position, Position::new(4));
    assert_eq!(state.artifacts.len(), 2, "one retained and one missing");
    assert_eq!(state.items.len(), 2);
}

#[test]
fn interrupted_run_finalizes_as_unknown() {
    let fixture = fixture();
    let out = adapter_scenario(&fixture, "run", "interrupted-run", &[]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let value = report(&out);
    assert_eq!(value["outcome"], "finalized");
    assert_eq!(value["finalize"]["execution"], "unknown");
    assert_eq!(value["finalize"]["finalization"], "finalized_partial");
}

#[test]
fn a_late_item_is_refused_after_finalization() {
    let fixture = fixture();
    let out = adapter_scenario(&fixture, "run", "late-item", &[]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let value = report(&out);
    assert_eq!(value["outcome"], "finalized");
    let last = value["calls"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["key"], "late-note");
    assert_eq!(last["result"], "rejected");
    assert_eq!(last["error_code"], "REVISION_CONFLICT");
}

#[test]
fn an_unsupported_protocol_is_rejected_at_begin() {
    let fixture = fixture();
    let out = adapter(&fixture, "run", &["--protocol", "inv.capture.v2"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let value = report(&out);
    assert_eq!(value["outcome"], "rejected");
    assert_eq!(value["calls"][0]["error_code"], "INVALID_INPUT");
    assert!(value["run_id"].is_null());
}
