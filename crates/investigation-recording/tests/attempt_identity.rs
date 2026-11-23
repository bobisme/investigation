//! One producer attempt identity across the capture protocol and the one-shot
//! `inv.producer-run.v1` envelope, one producer-name rule, terminal execution
//! status kept on a changed envelope, and recordings made before the paths
//! shared that identity still replaying unchanged.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use investigation_core::{
    attempt,
    explain::ExecutionStatus,
    header::Actor,
    identity::Id,
    producer_run::ProducerRun,
    wire::{CommitResult, ErrorCode, Failure},
};
use investigation_recording::{
    BeginCapture, CaptureEvent, CaptureIntent, CaseTarget, ExternalRunId, FinalizeCapture,
    IdempotencyKey, LocalRecorder, ProducerIdentity, ProducerName, Recorder, RecordingError,
    RecoverScope, SealState,
};
use investigation_store::{
    Store, Workspace,
    store::ingest::producer_run::{ProducerItemOutcome, ProducerRunSubmission},
};
use serde_json::{Value, json};

const LEGACY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/legacy-attempt-identity"
);

fn rejected(error: RecordingError) -> Failure {
    match error {
        RecordingError::Rejected(failure) => failure,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

fn fresh() -> (tempfile::TempDir, PathBuf, LocalRecorder) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let recorder = LocalRecorder::open(&root).unwrap();
    (temporary, root, recorder)
}

fn begin_request(producer: &str, external_run_id: &str, key: &str) -> BeginCapture {
    BeginCapture::new(
        IdempotencyKey::new(key).unwrap(),
        "agent:example-producer".parse().unwrap(),
        ProducerIdentity::new(ProducerName::new(producer).unwrap()),
        ExternalRunId::new(external_run_id).unwrap(),
        CaptureIntent::new("synthetic run"),
        CaseTarget::NewRecordCase {
            title: "Synthetic runs".into(),
        },
    )
}

fn envelope(producer: &str, external_run_id: &str, state: &str) -> Value {
    json!({
        "schema_version": "inv.producer-run.v1",
        "producer": {"name": producer, "version": "0.1.0"},
        "external_run_id": external_run_id,
        "intent": {"label": "synthetic run"},
        "execution": {"state": state},
    })
}

type ProducerRunResult =
    Result<investigation_store::store::ingest::producer_run::ProducerRunReceipt, Failure>;

fn ingest(root: &Path, envelope: &Value, actor: &str) -> ProducerRunResult {
    ingest_into(root, envelope, actor, None)
}

/// A changed envelope names the case that holds its attempt.
fn ingest_into(root: &Path, envelope: &Value, actor: &str, case: Option<Id>) -> ProducerRunResult {
    let mut submission = ProducerRunSubmission::new(actor.parse::<Actor>().unwrap());
    submission.case_id = case;
    Store::open(Workspace::open(root).unwrap())
        .ingest_producer_run(&serde_json::to_vec(envelope).unwrap(), &submission)
}

fn run_count(root: &Path) -> usize {
    let report = Store::open(Workspace::open(root).unwrap()).check().unwrap();
    assert!(report.integrity_ok(), "{:?}", report.issues);
    let mut store = Store::open(Workspace::open(root).unwrap());
    report
        .heads
        .iter()
        .map(|head| {
            store
                .case(head.case_id)
                .unwrap()
                .state
                .records()
                .filter(|record| record.header.id.kind() == investigation_core::identity::Kind::Run)
                .count()
        })
        .sum()
}

fn argv_names(failure: &Failure, word: &str) -> bool {
    failure
        .error
        .repair
        .as_ref()
        .and_then(|repair| repair.argv.as_ref())
        .is_some_and(|argv| argv.iter().any(|part| part == word))
}

#[test]
fn capture_then_envelope_names_the_captured_run() {
    let (_temporary, root, recorder) = fresh();
    let begun = recorder
        .begin(&begin_request(
            "example-bench",
            "attempt-1",
            "attempt-1/begin",
        ))
        .unwrap();

    let refused = ingest(
        &root,
        &envelope("example-bench", "attempt-1", "completed"),
        "agent:submitter",
    )
    .unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::IdempotencyKeyReused);
    assert_eq!(refused.error.path.as_deref(), Some("/external_run_id"));
    let expected = refused.error.expected.as_ref().unwrap();
    assert_eq!(expected["run_id"], json!(begun.run_id));
    assert_eq!(expected["case_id"], json!(begun.case_id));
    assert_eq!(expected["handle"], json!(begun.handle));
    assert_eq!(expected["recorded_by"], "capture.begin");
    assert!(argv_names(&refused, "recover"), "{refused:?}");
    assert!(refused.error.message.contains(&begun.run_id.to_string()));
    assert_eq!(run_count(&root), 1);

    // Still refused once the capture is finalized; another attempt is accepted.
    let finalize: FinalizeCapture = serde_json::from_value(json!({
        "handle": begun.handle,
        "key": "attempt-1/finalize",
        "expected_last_position": null,
        "completion": "complete",
        "execution": {"state": "completed", "exit_code": 0},
    }))
    .unwrap();
    recorder.finalize(&finalize).unwrap();
    let refused = ingest(
        &root,
        &envelope("example-bench", "attempt-1", "completed"),
        "agent:submitter",
    )
    .unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::IdempotencyKeyReused);
    ingest(
        &root,
        &envelope("example-bench", "attempt-2", "completed"),
        "agent:submitter",
    )
    .unwrap();
    // Another producer may reuse the external run ID.
    ingest(
        &root,
        &envelope("other-bench", "attempt-1", "completed"),
        "agent:submitter",
    )
    .unwrap();
    assert_eq!(run_count(&root), 3);
}

#[test]
fn envelope_then_capture_names_the_envelope_run() {
    let (_temporary, root, recorder) = fresh();
    let ingested = ingest(
        &root,
        &envelope("example-bench", "attempt-1", "completed"),
        "agent:submitter",
    )
    .unwrap();

    let refused = rejected(
        recorder
            .begin(&begin_request(
                "example-bench",
                "attempt-1",
                "attempt-1/begin",
            ))
            .unwrap_err(),
    );
    assert_eq!(refused.error.code, ErrorCode::IdempotencyKeyReused);
    assert_eq!(refused.error.path.as_deref(), Some("/external_run_id"));
    let expected = refused.error.expected.as_ref().unwrap();
    assert_eq!(expected["run_id"], json!(ingested.run.id));
    assert_eq!(expected["case_id"], json!(ingested.case_id));
    assert_eq!(expected["recorded_by"], "producer-run");
    assert!(argv_names(&refused, "show"), "{refused:?}");
    assert_eq!(run_count(&root), 1);

    // The envelope path itself still maps the same pair onto its Run.
    let again = ingest(
        &root,
        &envelope("example-bench", "attempt-1", "completed"),
        "agent:other-submitter",
    )
    .unwrap();
    assert_eq!(again.run.id, ingested.run.id);
    assert_eq!(again.run_outcome, ProducerItemOutcome::Deduplicated);
    recorder
        .begin(&begin_request(
            "example-bench",
            "attempt-2",
            "attempt-2/begin",
        ))
        .unwrap();
    assert_eq!(run_count(&root), 2);
}

#[test]
fn a_changed_envelope_keeps_a_terminal_status() {
    let (_temporary, root, _recorder) = fresh();
    let first = ingest(
        &root,
        &envelope("example-bench", "attempt-1", "completed"),
        "agent:submitter",
    )
    .unwrap();
    assert_eq!(first.execution, ExecutionStatus::Completed);

    for state in ["failed", "unknown", "running"] {
        let refused = ingest_into(
            &root,
            &envelope("example-bench", "attempt-1", state),
            "agent:submitter",
            Some(first.case_id),
        )
        .unwrap_err();
        assert_eq!(refused.error.code, ErrorCode::GateFailed, "{state}");
        assert_eq!(refused.error.path.as_deref(), Some("/execution/state"));
        assert_eq!(
            refused.error.expected.as_ref().unwrap()["execution_status"],
            "completed"
        );
        assert_eq!(
            refused.error.actual.as_ref().unwrap()["execution_status"],
            state
        );
        assert!(refused.error.message.contains(&first.run.id.to_string()));
    }

    // A change that keeps the terminal status still revises the Run.
    let mut changed = envelope("example-bench", "attempt-1", "completed");
    changed["limitations"] = json!(["Single synthetic workload"]);
    let revised = ingest_into(&root, &changed, "agent:submitter", Some(first.case_id)).unwrap();
    assert_eq!(revised.run.id, first.run.id);
    assert_eq!(revised.run_outcome, ProducerItemOutcome::Revised);
    assert_eq!(revised.execution, ExecutionStatus::Completed);

    // An open status may still become terminal.
    let open = ingest(
        &root,
        &envelope("example-bench", "attempt-2", "unknown"),
        "agent:submitter",
    )
    .unwrap();
    let closed = ingest_into(
        &root,
        &envelope("example-bench", "attempt-2", "failed"),
        "agent:submitter",
        Some(open.case_id),
    )
    .unwrap();
    assert_eq!(closed.run.id, open.run.id);
    assert_eq!(closed.execution, ExecutionStatus::Failed);
}

#[test]
fn one_producer_name_rule_for_every_path() {
    let names = [
        ("example-bench", true),
        ("example.org_bench-2", true),
        ("example-org:bench", false),
        ("example-org/bench", false),
        ("org:tool/v1", false),
        ("caf\u{e9}-bench", false),
        ("\u{30c4}\u{30fc}\u{30eb}", false),
        ("bench\u{200b}", false),
        ("-bench", false),
        ("", false),
    ];
    let (_temporary, root, recorder) = fresh();
    for (index, (name, valid)) in names.into_iter().enumerate() {
        assert_eq!(attempt::is_producer_name(name), valid, "{name:?}");
        assert_eq!(ProducerName::new(name).is_ok(), valid, "{name:?}");
        assert_eq!(name.parse::<ProducerName>().is_ok(), valid, "{name:?}");
        let run: ProducerRun =
            serde_json::from_value(envelope(name, "attempt-1", "completed")).unwrap();
        assert_eq!(run.validate().is_ok(), valid, "{name:?}");

        // The JSON forms reach the store's checks.
        let refused = ingest(
            &root,
            &envelope(name, &format!("attempt-{index}"), "completed"),
            "agent:submitter",
        );
        assert_eq!(refused.is_ok(), valid, "{name:?}: {refused:?}");
        let begin = json!({
            "key": format!("names-{index}/begin"),
            "actor": "agent:example-producer",
            "producer": {"name": name},
            "external_run_id": format!("capture-{index}"),
            "intent": {"label": "synthetic run"},
            "case": {"kind": "new_record_case", "title": "Synthetic runs"},
        });
        match serde_json::from_value::<BeginCapture>(begin) {
            // Only names an earlier recording may hold decode; begin refuses them.
            Ok(request) => match recorder.begin(&request) {
                Ok(_) => assert!(valid, "{name:?}"),
                Err(error) => {
                    assert!(!valid, "{name:?}");
                    let failure = rejected(error);
                    assert_eq!(failure.error.code, ErrorCode::InvalidInput);
                    assert_eq!(failure.error.path.as_deref(), Some("/producer/name"));
                    assert!(attempt::is_recorded_producer_name(name));
                }
            },
            Err(_) => {
                assert!(!valid, "{name:?}");
                assert!(!attempt::is_recorded_producer_name(name));
            }
        }
    }
    // An ingest acquisition namespace follows the same rule.
    for (index, (namespace, valid)) in [("example-bench", true), ("example-org:bench", false)]
        .into_iter()
        .enumerate()
    {
        let manifest = json!({
            "schema_version": "inv.ingest.v1",
            "command_id": format!("namespace-{index}"),
            "actor": "agent:submitter",
            "new_case": {"title": "Namespaces", "mode": "explore"},
            "source": {
                "title": "A note",
                "acquisition_key": {"namespace": namespace, "external_id": "one"},
            },
            "content": {"kind": "stdin", "name": "note.txt"},
        });
        let mut bytes: &[u8] = b"bytes";
        let result = Store::open(Workspace::open(&root).unwrap())
            .ingest(&serde_json::to_vec(&manifest).unwrap(), Some(&mut bytes));
        match result {
            Ok(_) => assert!(valid, "{namespace}"),
            Err(failure) => {
                assert!(!valid, "{namespace}: {failure:?}");
                assert_eq!(
                    failure.error.path.as_deref(),
                    Some("/source/acquisition_key/namespace")
                );
            }
        }
    }
}

/// Copy the checked-in workspace that the build before this change recorded.
fn legacy_workspace() -> (tempfile::TempDir, PathBuf) {
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        fs::set_permissions(to, fs::Permissions::from_mode(0o700)).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), &target).unwrap();
            }
        }
    }
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    copy(&Path::new(LEGACY).join("workspace"), &root);
    // Git keeps no empty directories; the rebuildable cache starts empty.
    fs::create_dir(root.join("cache")).unwrap();
    (temporary, root)
}

fn legacy_request<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path = Path::new(LEGACY).join("requests").join(name);
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn id(value: &str) -> Id {
    value.parse().unwrap()
}

#[test]
fn a_journal_recorded_before_the_change_replays_unchanged() {
    let (_temporary, root) = legacy_workspace();
    // Replay verifies every event, hash and blob as recorded.
    assert_eq!(run_count(&root), 4);

    let recorder = LocalRecorder::open(&root).unwrap();
    let finalized_run = id("run_01a20705-6926-7ad2-8a19-e043bfeeefc6");
    let open_run = id("run_01a20705-7fe2-7bc3-ae85-416bf664a62c");
    let open_handle: investigation_recording::CaptureHandle =
        "handle_01a20705-7fe2-7bc3-ae85-4154a15a9df8"
            .parse()
            .unwrap();

    // A finalized capture under a ':' '/' name reads back exactly.
    let finalized = recorder
        .recover(&RecoverScope::Handle {
            handle: "handle_01a20705-6925-7992-9c18-27be4d0fdea6"
                .parse()
                .unwrap(),
        })
        .unwrap();
    let finalized = &finalized.handles[0];
    assert_eq!(finalized.run_id, finalized_run);
    assert_eq!(finalized.producer.name.as_str(), "example-org:bench/v1");
    assert!(!finalized.producer.name.is_current());
    assert_eq!(finalized.seal, SealState::Finalized);

    // The open one is still unsettled and keeps its identity.
    let unsettled = recorder.recover(&RecoverScope::Unsettled).unwrap();
    let open = unsettled
        .handles
        .iter()
        .find(|handle| handle.handle == open_handle)
        .unwrap();
    assert_eq!(open.run_id, open_run);
    assert_eq!(open.producer.name.as_str(), "example-org:bench/v1");
    assert_eq!(open.external_run_id.as_str(), "legacy-run-2");

    // Resending its identical begin returns the original handle; the recording
    // continues and finalizes under the name it was declared with.
    let retry = recorder
        .begin(&legacy_request::<BeginCapture>("begin2.json"))
        .unwrap();
    assert_eq!(retry.handle, open_handle);
    assert_eq!(retry.run_id, open_run);
    assert_eq!(retry.commit.result, CommitResult::AlreadyCommitted);
    let started: CaptureEvent = serde_json::from_value(json!({
        "handle": open_handle, "key": "started", "position": 1, "fact": {"kind": "started"},
    }))
    .unwrap();
    recorder.append(&started).unwrap();
    let finalize: FinalizeCapture = serde_json::from_value(json!({
        "handle": open_handle,
        "key": "legacy-run-2/finalize",
        "expected_last_position": 1,
        "completion": "complete",
        "execution": {"state": "failed", "exit_code": 2},
    }))
    .unwrap();
    let receipt = recorder.finalize(&finalize).unwrap();
    assert_eq!(receipt.run_id, open_run);

    // No new attempt may use the legacy name.
    let mut fresh: BeginCapture = legacy_request("begin2.json");
    fresh.key = IdempotencyKey::new("legacy-run-3/begin").unwrap();
    fresh.external_run_id = ExternalRunId::new("legacy-run-3").unwrap();
    let refused = rejected(recorder.begin(&fresh).unwrap_err());
    assert_eq!(refused.error.code, ErrorCode::InvalidInput);
    assert_eq!(refused.error.path.as_deref(), Some("/producer/name"));

    // The workspace already held one capture Run and one envelope Run for the
    // same attempt. Both keep their identities; each path keeps its own Run.
    let envelope_run = id("run_01a20705-699a-7460-9859-179c5bae9d30");
    let resubmitted = Store::open(Workspace::open(&root).unwrap())
        .ingest_producer_run(
            &fs::read(Path::new(LEGACY).join("requests/env-dup.json")).unwrap(),
            &ProducerRunSubmission::new("agent:another-submitter".parse().unwrap()),
        )
        .unwrap();
    assert_eq!(resubmitted.run.id, envelope_run);
    assert_eq!(resubmitted.run_outcome, ProducerItemOutcome::Deduplicated);
    let duplicate = recorder
        .begin(&legacy_request::<BeginCapture>("begin-dup.json"))
        .unwrap();
    assert_eq!(
        duplicate.run_id,
        id("run_01a20705-69c1-7a73-94cb-70991c47587d")
    );
    assert_eq!(duplicate.commit.result, CommitResult::AlreadyCommitted);
    let mut other_key: BeginCapture = legacy_request("begin-dup.json");
    other_key.key = IdempotencyKey::new("legacy-dup/begin-again").unwrap();
    let refused = rejected(recorder.begin(&other_key).unwrap_err());
    assert_eq!(refused.error.code, ErrorCode::IdempotencyKeyReused);
    assert_eq!(
        refused.error.expected.as_ref().unwrap()["run_id"],
        json!(duplicate.run_id)
    );

    assert_eq!(run_count(&root), 4);
}
