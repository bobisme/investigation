//! `capture.append` against a real local workspace, through the public API only.
//! Assertions about staged state read the store directly, as the conformance
//! runner does; the producer code under test never touches it.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
};

use investigation_recording::{
    BeginCapture, CaptureEvent, CaptureHandle, CaptureIntent, CaseTarget, ChannelEvent,
    CommitStatus, ErrorCode, ExternalRunId, IdempotencyKey, ItemReceipt, LifecycleFact,
    LocalRecorder, Position, ProducerIdentity, ProducerName, ReceiptResult, Recorder,
    RecordingError,
};
use investigation_store::{Store, Workspace};
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

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).unwrap()
}

fn position(value: u64) -> Position {
    Position::new(value).unwrap()
}

fn begin(recorder: &LocalRecorder) -> CaptureHandle {
    let request = BeginCapture::new(
        key("run-0001/begin"),
        "agent:example-producer".parse().unwrap(),
        ProducerIdentity::new(ProducerName::new("example-producer").unwrap()),
        ExternalRunId::new("run-0001").unwrap(),
        CaptureIntent::new("synthetic run"),
        CaseTarget::NewRecordCase {
            title: "Synthetic runs".into(),
        },
    );
    recorder.begin(&request).unwrap().handle
}

fn phase(handle: CaptureHandle, item: &str, at: u64) -> CaptureEvent {
    CaptureEvent::new(
        handle,
        key(item),
        position(at),
        LifecycleFact::PhaseChanged { phase: item.into() },
    )
}

fn rejection(result: Result<ItemReceipt, RecordingError>) -> (ErrorCode, Option<String>, Value) {
    let error = result.unwrap_err();
    assert_eq!(error.commit_status(), CommitStatus::NotCommitted);
    let failure = error.failure().expect("a structured rejection");
    (
        failure.error.code,
        failure.error.path.clone(),
        failure.error.expected.clone().unwrap_or(Value::Null),
    )
}

#[test]
fn appended_facts_take_consecutive_positions_with_distinct_reported_and_accepted_times() {
    let (_temporary, root) = workspace();
    let recorder = LocalRecorder::open(&root).unwrap();
    let handle = begin(&recorder);

    let mut started = CaptureEvent::new(
        handle,
        key("started"),
        Position::FIRST,
        LifecycleFact::Started,
    );
    started.reported_at = Some(
        serde_json::from_value(json!({
            "timestamp": {"state": "known", "value": "1999-01-02T03:04:05.000Z"},
            "offset": {"state": "unknown"},
            "clock_quality": "unverified"
        }))
        .unwrap(),
    );
    started
        .metadata
        .insert("example-org.host".into(), json!({"slot": 3}));
    let first = recorder.append(&started).unwrap();
    assert_eq!(first.result, ReceiptResult::Accepted);
    assert_eq!(first.position, Position::FIRST);
    assert_eq!(first.handle, handle);
    assert_eq!(first.key.as_str(), "started");
    // The producer's claim is kept in the request; acceptance is local time.
    assert!(!first.accepted_at.to_string().starts_with("1999"));

    let facts = [
        LifecycleFact::Channel {
            channel: "trace".into(),
            state: ChannelEvent::Opened,
        },
        LifecycleFact::Error {
            message: "connection reset\nretrying".into(),
            external_code: Some("E42".into()),
        },
        LifecycleFact::Channel {
            channel: "trace".into(),
            state: ChannelEvent::Gap {
                detail: Some("ring buffer overrun".into()),
            },
        },
        LifecycleFact::Cancelled { reason: None },
    ];
    for (offset, fact) in facts.into_iter().enumerate() {
        let at = offset as u64 + 2;
        let receipt = recorder
            .append(&CaptureEvent::new(
                handle,
                key(&format!("fact-{at}")),
                position(at),
                fact,
            ))
            .unwrap();
        assert_eq!(receipt.position.get(), at);
        assert_eq!(receipt.run_id, first.run_id);
    }

    let staged = store(&root).capture_items(handle.id()).unwrap();
    assert_eq!(staged.items.len(), 5);
    assert_eq!(
        staged.items[0].request,
        serde_json::to_value(&started).unwrap()
    );
    assert_eq!(
        staged.items[0].request["reported_at"]["timestamp"]["value"],
        "1999-01-02T03:04:05.000Z"
    );
    assert_eq!(staged.items[0].accepted_at, first.accepted_at);
    // Appends are staged; the journal still holds only the declaration.
    assert_eq!(store(&root).case(staged.case_id).unwrap().events.len(), 1);
}

#[test]
fn sending_position_three_when_two_is_missing_is_rejected_with_the_expected_position() {
    let (_temporary, root) = workspace();
    let recorder = LocalRecorder::open(&root).unwrap();
    let handle = begin(&recorder);
    recorder.append(&phase(handle, "one", 1)).unwrap();

    let (code, path, expected) = rejection(recorder.append(&phase(handle, "three", 3)));
    assert_eq!(code, ErrorCode::RevisionConflict);
    assert_eq!(path.as_deref(), Some("/position"));
    assert_eq!(expected["position"], 2);
    assert_eq!(expected["last_position"], 1);

    // The first item of a fresh handle must be position 1.
    let (_, _, expected) = rejection(recorder.append(&phase(handle, "one-again", 1)));
    assert_eq!(expected["position"], 2);
    assert_eq!(
        store(&root).capture_items(handle.id()).unwrap().items.len(),
        1
    );
}

#[test]
fn retrying_an_accepted_item_returns_its_receipt_and_changed_bytes_conflict() {
    let (_temporary, root) = workspace();
    let recorder = LocalRecorder::open(&root).unwrap();
    let handle = begin(&recorder);
    let first = recorder.append(&phase(handle, "one", 1)).unwrap();
    recorder.append(&phase(handle, "two", 2)).unwrap();

    // A lost response: resend the identical request, even through JSON
    // spelled differently, from another recorder instance.
    let mut sparse = serde_json::to_value(phase(handle, "one", 1)).unwrap();
    sparse["metadata"] = json!({});
    let resent: CaptureEvent = serde_json::from_value(sparse).unwrap();
    let retry = LocalRecorder::open(&root).unwrap().append(&resent).unwrap();
    assert_eq!(retry.result, ReceiptResult::AlreadyAccepted);
    let mut expected = first.clone();
    expected.result = ReceiptResult::AlreadyAccepted;
    assert_eq!(retry, expected);

    let mut changed = phase(handle, "one", 1);
    changed.fact = LifecycleFact::PhaseChanged {
        phase: "different".into(),
    };
    let (code, path, expected) = rejection(recorder.append(&changed));
    assert_eq!(code, ErrorCode::IdempotencyKeyReused);
    assert_eq!(path.as_deref(), Some("/key"));
    assert_eq!(expected["position"], 1);
    assert_eq!(expected["request_digest"], json!(first.request_digest));
    assert_eq!(
        store(&root).capture_items(handle.id()).unwrap().items.len(),
        2
    );
}

#[test]
fn invalid_facts_and_unknown_handles_name_their_field_and_stage_nothing() {
    let (_temporary, root) = workspace();
    let recorder = LocalRecorder::open(&root).unwrap();
    let handle = begin(&recorder);
    let at = |fact: LifecycleFact| CaptureEvent::new(handle, key("bad"), Position::FIRST, fact);

    let cases = [
        (
            at(LifecycleFact::PhaseChanged { phase: " ".into() }),
            "/fact/phase",
        ),
        (
            at(LifecycleFact::Error {
                message: String::new(),
                external_code: None,
            }),
            "/fact/message",
        ),
        (
            at(LifecycleFact::Error {
                message: "failed".into(),
                external_code: Some("x".repeat(129)),
            }),
            "/fact/external_code",
        ),
        (
            at(LifecycleFact::Channel {
                channel: "a\u{0}b".into(),
                state: ChannelEvent::Closed,
            }),
            "/fact/channel",
        ),
        (
            at(LifecycleFact::Cancelled {
                reason: Some("r".repeat(4097)),
            }),
            "/fact/reason",
        ),
    ];
    for (event, field) in cases {
        let (code, path, _) = rejection(recorder.append(&event));
        assert_eq!(
            (code, path.as_deref()),
            (ErrorCode::InvalidInput, Some(field))
        );
    }
    let mut metadata = at(LifecycleFact::Started);
    metadata.metadata.insert(String::new(), Value::Null);
    let (code, path, _) = rejection(recorder.append(&metadata));
    assert_eq!(code, ErrorCode::InvalidInput);
    assert!(path.unwrap().starts_with("/metadata"));

    // High-frequency samples belong in an artifact, not in one fact.
    // Within the character bound, but over the handle's byte bound.
    let large = at(LifecycleFact::Error {
        message: "\u{1d11e}".repeat(16 * 1024),
        external_code: None,
    });
    let (code, _, expected) = rejection(recorder.append(&large));
    assert_eq!(code, ErrorCode::InvalidInput);
    assert_eq!(expected["max_event_bytes"], 65536);

    let unknown: CaptureHandle = "handle_019acd16-0000-7000-8000-000000000009"
        .parse()
        .unwrap();
    let (code, path, _) = rejection(recorder.append(&phase(unknown, "one", 1)));
    assert_eq!(
        (code, path.as_deref()),
        (ErrorCode::UnknownReference, Some("/handle"))
    );
    assert!(
        store(&root)
            .capture_items(handle.id())
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn concurrent_producers_racing_for_one_position_get_exactly_one_acceptance() {
    let (_temporary, root) = workspace();
    let recorder = Arc::new(LocalRecorder::open(&root).unwrap());
    let handle = begin(&recorder);
    let barrier = Arc::new(Barrier::new(4));
    let results: Vec<_> = (0..4)
        .map(|index| {
            let recorder = Arc::clone(&recorder);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                recorder.append(&phase(handle, &format!("racer-{index}"), 1))
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    let accepted = results.iter().filter(|result| result.is_ok()).count();
    assert_eq!(accepted, 1, "{results:?}");
    for error in results.iter().filter_map(|result| result.as_ref().err()) {
        assert_eq!(error.code(), Some(ErrorCode::RevisionConflict));
    }
    assert_eq!(
        store(&root).capture_items(handle.id()).unwrap().items.len(),
        1
    );
}
