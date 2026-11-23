//! `capture.begin` against a real local workspace, through the public API only.
//! Assertions about canonical state read the store directly, as the conformance
//! runner does; the producer code under test never touches it.

use std::path::{Path, PathBuf};

use investigation_recording::{
    BeginCapture, BeginReceipt, CaptureIntent, CaptureVersion, CaseTarget, CommitStatus, ErrorCode,
    ExternalRunId, IdempotencyKey, LocalRecorder, ProducerIdentity, ProducerName, Recorder,
    RecordingError, RevisionRef,
};
use investigation_store::{Store, Workspace, store::capture::StagingState};
use serde_json::{Value, json};

fn workspace() -> (tempfile::TempDir, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    (temporary, root)
}

fn store(root: &Path) -> Store {
    Store::open(Workspace::open(root).unwrap())
}

fn begin(key: &str, external_run_id: &str) -> BeginCapture {
    let mut producer = ProducerIdentity::new(ProducerName::new("example-producer").unwrap());
    producer.version = Some("0.1.0".into());
    BeginCapture::new(
        IdempotencyKey::new(key).unwrap(),
        "agent:example-producer".parse().unwrap(),
        producer,
        ExternalRunId::new(external_run_id).unwrap(),
        CaptureIntent::new("synthetic run"),
        CaseTarget::NewRecordCase {
            title: "Synthetic runs".into(),
        },
    )
}

fn rejection(result: Result<BeginReceipt, RecordingError>) -> (ErrorCode, Option<String>) {
    let error = result.unwrap_err();
    assert_eq!(error.commit_status(), CommitStatus::NotCommitted);
    let failure = error.failure().expect("a structured rejection");
    (failure.error.code, failure.error.path.clone())
}

#[test]
fn begin_declares_one_run_and_returns_its_handle_protocol_limits_and_staging() {
    let (_temporary, root) = workspace();
    let recorder = LocalRecorder::open(&root).unwrap();
    let receipt = recorder
        .begin(&begin("run-0001/begin", "run-0001"))
        .unwrap();

    assert_eq!(receipt.protocol, CaptureVersion::V1);
    assert_eq!(receipt.limits.max_event_bytes, 64 * 1024);
    assert_eq!(receipt.limits.max_attachment_bytes, 64 * 1024 * 1024);
    assert_eq!(receipt.limits.max_items, 100_000);
    assert_eq!(
        receipt.staging_dir,
        PathBuf::from(format!("state/staging/{}", receipt.run_id))
    );
    assert!(
        root.join(&receipt.staging_dir)
            .join("handle.json")
            .is_file()
    );
    assert_eq!(receipt.commit.command_id, "run-0001/begin");
    let created: Vec<_> = receipt
        .commit
        .created
        .iter()
        .map(|change| change.id)
        .collect();
    assert_eq!(created, [receipt.case_id, receipt.run_id]);

    // The Run is declared, never started, and its case needs no theory.
    let replay = store(&root).case(receipt.case_id).unwrap();
    let case = replay.state.current(receipt.case_id).unwrap();
    assert_eq!(case.body["mode"], "record");
    let run = replay.state.current(receipt.run_id).unwrap();
    assert_eq!(run.body["execution_status"], "declared");
    assert_eq!(run.body["tool"], "example-producer 0.1.0");
    assert_eq!(run.body["capture"]["handle"], json!(receipt.handle));
    assert_eq!(replay.state.records().count(), 2);

    // Staging is listed for recovery as soon as begin returns.
    let handles = store(&root).capture_handles().unwrap();
    assert_eq!(handles.len(), 1);
    assert_eq!(handles[0].binding.handle, receipt.handle.id());
    assert_eq!(handles[0].staging, StagingState::Present);
}

#[test]
fn a_retried_begin_returns_the_same_run_and_handle_however_it_is_spelled() {
    let (_temporary, root) = workspace();
    let recorder = LocalRecorder::open(&root).unwrap();
    let request = begin("run-0001/begin", "run-0001");
    let first = recorder.begin(&request).unwrap();
    assert_eq!(json!(first.commit.result), "committed");

    // A JSON-CLI client may omit defaulted fields; the request is the same.
    let mut sparse = serde_json::to_value(&request).unwrap();
    sparse.as_object_mut().unwrap().remove("classification");
    let resent: BeginCapture = serde_json::from_value(sparse).unwrap();
    let retry = LocalRecorder::open(&root).unwrap().begin(&resent).unwrap();
    assert_eq!(json!(retry.commit.result), "already_committed");
    assert_eq!(
        (
            retry.handle,
            retry.run_id,
            retry.case_id,
            &retry.staging_dir
        ),
        (
            first.handle,
            first.run_id,
            first.case_id,
            &first.staging_dir
        )
    );
    assert_eq!(retry.commit.transaction_id, first.commit.transaction_id);
    assert_eq!(retry.limits, first.limits);
    assert_eq!(store(&root).list_cases().unwrap().len(), 1);
    assert_eq!(store(&root).capture_handles().unwrap().len(), 1);
}

#[test]
fn the_same_attempt_under_another_key_or_changed_content_is_refused() {
    let (_temporary, root) = workspace();
    let recorder = LocalRecorder::open(&root).unwrap();
    let first = recorder
        .begin(&begin("run-0001/begin", "run-0001"))
        .unwrap();

    let error = recorder
        .begin(&begin("run-0001/begin-again", "run-0001"))
        .unwrap_err();
    let failure = error.failure().unwrap();
    assert_eq!(failure.error.code, ErrorCode::IdempotencyKeyReused);
    assert_eq!(failure.error.path.as_deref(), Some("/external_run_id"));
    assert_eq!(
        failure.error.expected.as_ref().unwrap()["handle"],
        json!(first.handle)
    );

    let mut changed = begin("run-0001/begin", "run-0001");
    changed.intent = CaptureIntent::new("a different intent");
    assert_eq!(
        rejection(recorder.begin(&changed)),
        (ErrorCode::IdempotencyKeyReused, Some("/command_id".into()))
    );
    assert_eq!(store(&root).capture_handles().unwrap().len(), 1);
}

#[test]
fn begin_records_into_an_existing_case_with_the_requested_classification() {
    let (_temporary, root) = workspace();
    let recorder = LocalRecorder::open(&root).unwrap();
    let first = recorder
        .begin(&begin("run-0001/begin", "run-0001"))
        .unwrap();
    let mut second = begin("run-0002/begin", "run-0002");
    second.case = CaseTarget::Existing {
        case_id: first.case_id,
    };
    second.classification = investigation_recording::ClassificationLevel::Restricted;
    second
        .metadata
        .insert("example-org.fixture".into(), json!({"seed": 7}));
    let receipt = recorder.begin(&second).unwrap();
    assert_eq!(receipt.case_id, first.case_id);
    assert_eq!(receipt.commit.created.len(), 1);
    let replay = store(&root).case(first.case_id).unwrap();
    let run = replay.state.current(receipt.run_id).unwrap();
    assert_eq!(
        serde_json::to_value(run.header.classification.level).unwrap(),
        "restricted"
    );
}

#[test]
fn invalid_requests_name_their_field_and_record_nothing() {
    let (_temporary, root) = workspace();
    let recorder = LocalRecorder::open(&root).unwrap();

    let mut blank = begin("k1", "r1");
    blank.intent = CaptureIntent::new("   ");
    assert_eq!(
        rejection(recorder.begin(&blank)),
        (ErrorCode::InvalidInput, Some("/intent/label".into()))
    );

    let plan = RevisionRef {
        id: "inq_019acd16-0000-7000-8000-000000000001".parse().unwrap(),
        revision: 1,
    };
    let mut planned_new_case = begin("k2", "r2");
    planned_new_case.intent.inquiry_plan = Some(plan);
    assert_eq!(
        rejection(recorder.begin(&planned_new_case)),
        (ErrorCode::InvalidInput, Some("/intent/inquiry_plan".into()))
    );

    let mut untitled = begin("k3", "r3");
    untitled.case = CaseTarget::NewRecordCase { title: " ".into() };
    assert_eq!(
        rejection(recorder.begin(&untitled)),
        (ErrorCode::InvalidInput, Some("/case/title".into()))
    );

    let mut blank_version = begin("k4", "r4");
    blank_version.producer.version = Some(String::new());
    assert_eq!(
        rejection(recorder.begin(&blank_version)),
        (ErrorCode::InvalidInput, Some("/producer/version".into()))
    );

    let mut metadata = begin("k5", "r5");
    metadata.metadata.insert("k".repeat(200), Value::Null);
    let (code, path) = rejection(recorder.begin(&metadata));
    assert_eq!(code, ErrorCode::InvalidInput);
    assert!(path.unwrap().starts_with("/metadata/"));

    let mut absent_case = begin("k6", "r6");
    absent_case.case = CaseTarget::Existing {
        case_id: "case_019acd16-0000-7000-8000-000000000009".parse().unwrap(),
    };
    assert_eq!(
        rejection(recorder.begin(&absent_case)),
        (ErrorCode::CaseRequired, Some("/case/case_id".into()))
    );
    assert!(store(&root).list_cases().unwrap().is_empty());

    // An unknown plan in an existing case is a catalog failure, reported
    // against the begin request rather than the composed transaction.
    let existing = recorder.begin(&begin("k7", "r7")).unwrap();
    let mut unknown_plan = begin("k8", "r8");
    unknown_plan.case = CaseTarget::Existing {
        case_id: existing.case_id,
    };
    unknown_plan.intent.inquiry_plan = Some(plan);
    assert_eq!(
        rejection(recorder.begin(&unknown_plan)),
        (
            ErrorCode::UnknownReference,
            Some("/intent/inquiry_plan".into())
        )
    );
    assert_eq!(store(&root).capture_handles().unwrap().len(), 1);
}

#[test]
fn a_run_can_follow_an_exact_frozen_inquiry_plan_declared_earlier() {
    let (_temporary, root) = workspace();
    let recorder = LocalRecorder::open(&root).unwrap();
    let first = recorder
        .begin(&begin("run-0001/begin", "run-0001"))
        .unwrap();
    let catalog = investigation_core::catalog::standard_catalog();
    let submit = |key: &str, op: &str, payload: Value| {
        let command = json!({
            "schema_version": "inv.command.v1", "command_id": key,
            "actor": "agent:planner", "case_id": first.case_id, "preconditions": [],
            "operations": [{"op": op, "payload": payload}],
        });
        store(&root)
            .record(&serde_json::to_vec(&command).unwrap())
            .unwrap()
    };
    let example = catalog
        .get("inquiry.create")
        .unwrap()
        .example()
        .unwrap()
        .clone();
    let draft = submit("plan", "inquiry.create", example).created[0].id;
    let frozen = submit(
        "freeze",
        "inquiry.freeze",
        json!({"inquiry_ref": {"id": draft, "revision": 1}}),
    )
    .updated[0]
        .clone();

    let mut planned = begin("run-0002/begin", "run-0002");
    planned.case = CaseTarget::Existing {
        case_id: first.case_id,
    };
    planned.intent.inquiry_plan = Some(RevisionRef {
        id: frozen.id,
        revision: frozen.revision,
    });
    let digest = format!("sha256:{}", "e".repeat(64));
    planned.intent.external_plan_digest = Some(digest.parse().unwrap());
    let receipt = recorder.begin(&planned).unwrap();
    let replay = store(&root).case(first.case_id).unwrap();
    let run = replay.state.current(receipt.run_id).unwrap();
    assert_eq!(run.body["intent"]["kind"], "plan");
    assert_eq!(run.body["chronology"], "recorded_before_run_declaration");
    assert!(
        run.body["plan_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(run.body["capture"]["external_plan_digest"], digest);
}
