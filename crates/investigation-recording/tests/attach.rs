//! `capture.attach` against a real local workspace, through the public API only.
//! Assertions about staged state read the store directly, as the conformance
//! runner does; the producer code under test never touches it.

use std::{fs, path::PathBuf};

use investigation_recording::{
    ArtifactRef, ArtifactRole, ArtifactSource, BeginCapture, CaptureEvent, CaptureHandle,
    CaptureIntent, CaseTarget, ClassificationLevel, CommitStatus, Derivation, Digest, ErrorCode,
    ExternalRunId, IdempotencyKey, LifecycleFact, LocalRecorder, Position, ProducerIdentity,
    ProducerName, ReceiptResult, Recorder, RecordingError, StableArtifact, StagedAvailability,
};
use investigation_store::{Store, Workspace};
use serde_json::json;

struct Producer {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    outputs: PathBuf,
    recorder: LocalRecorder,
    handle: CaptureHandle,
}

/// A workspace whose import root is the producer's output directory, with one
/// declared handle.
fn producer() -> Producer {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let outputs = temporary.path().join("outputs");
    fs::create_dir(&outputs).unwrap();
    Workspace::init(&root, false).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        contents.replace(
            "import_roots = []",
            &format!("import_roots = [{:?}]", outputs.display().to_string()),
        ),
    )
    .unwrap();
    let recorder = LocalRecorder::open(&root).unwrap();
    let begin = BeginCapture::new(
        key("run-0001/begin"),
        "agent:example-producer".parse().unwrap(),
        ProducerIdentity::new(ProducerName::new("example-producer").unwrap()),
        ExternalRunId::new("run-0001").unwrap(),
        CaptureIntent::new("synthetic run"),
        CaseTarget::NewRecordCase {
            title: "Synthetic runs".into(),
        },
    );
    let handle = recorder.begin(&begin).unwrap().handle;
    Producer {
        _temporary: temporary,
        root,
        outputs,
        recorder,
        handle,
    }
}

impl Producer {
    fn output(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.outputs.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn file(&self, item: &str, at: u64, role: ArtifactRole, path: &str) -> StableArtifact {
        StableArtifact::file(self.handle, key(item), position(at), role, path)
    }

    fn staged_positions(&self) -> Vec<u64> {
        Store::open(Workspace::open(&self.root).unwrap())
            .capture_items(self.handle.id())
            .unwrap()
            .items
            .iter()
            .map(|item| item.position)
            .collect()
    }
}

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).unwrap()
}

fn position(value: u64) -> Position {
    Position::new(value).unwrap()
}

fn sha256(bytes: &[u8]) -> Digest {
    investigation_store::journal::hash_bytes(bytes)
}

fn rejection(result: Result<ArtifactRef, RecordingError>) -> (ErrorCode, Option<String>) {
    let error = result.unwrap_err();
    assert_eq!(error.commit_status(), CommitStatus::NotCommitted);
    let failure = error.failure().expect("a structured rejection");
    (failure.error.code, failure.error.path.clone())
}

#[test]
fn attachments_share_positions_with_facts_and_return_the_retained_digest_and_size() {
    let producer = producer();
    let recorder = &producer.recorder;
    recorder
        .append(&CaptureEvent::new(
            producer.handle,
            key("started"),
            Position::FIRST,
            LifecycleFact::Started,
        ))
        .unwrap();
    let bytes = b"{\"latency_ms\": 18.7}\n";
    producer.output("result.json", bytes);

    let mut original = producer.file("result", 2, ArtifactRole::OriginalOutput, "result.json");
    original.media_type = Some("application/json".into());
    original.expected_digest = Some(sha256(bytes));
    original.expected_size_bytes = Some(bytes.len() as u64);
    original.channel = Some("results".into());
    let attached = recorder.attach(&original).unwrap();
    assert_eq!(attached.receipt.result, ReceiptResult::Accepted);
    assert_eq!(attached.receipt.position.get(), 2);
    assert_eq!(attached.role, ArtifactRole::OriginalOutput);
    assert_eq!(attached.availability, StagedAvailability::Staged);
    assert_eq!(attached.digest, Some(sha256(bytes)));
    assert_eq!(attached.size_bytes, Some(bytes.len() as u64));

    // The retry returns the first receipt, even after the producer reuses
    // the path for other bytes.
    producer.output("result.json", b"next run");
    let retry = recorder.attach(&original).unwrap();
    assert_eq!(retry.receipt.result, ReceiptResult::AlreadyAccepted);
    assert_eq!(retry.receipt.accepted_at, attached.receipt.accepted_at);
    assert_eq!(retry.digest, attached.digest);

    // A derivative of the original, at the next shared position.
    let mut derivative = producer.file("summary", 3, ArtifactRole::Derivative, "summary.txt");
    producer.output("summary.txt", b"p99 18.7 ms");
    derivative.derived_from = Some(Derivation::new(key("result"), "summary/1"));
    let derived = recorder.attach(&derivative).unwrap();
    assert_eq!(derived.receipt.position.get(), 3);
    assert_eq!(derived.role, ArtifactRole::Derivative);
    assert_eq!(producer.staged_positions(), [1, 2, 3]);

    // The receipt is exactly what the staged item records.
    let staged = Store::open(Workspace::open(&producer.root).unwrap())
        .capture_items(producer.handle.id())
        .unwrap();
    assert_eq!(
        staged.items[1].request,
        serde_json::to_value(&original).unwrap()
    );
    assert_eq!(
        staged.items[1].outcome.as_ref().unwrap()["digest"],
        json!(sha256(bytes))
    );
}

#[test]
fn a_digest_or_size_mismatch_rejects_the_item_and_leaves_the_handle_usable() {
    let producer = producer();
    producer.output("result.json", b"actual");
    let mut wrong = producer.file("result", 1, ArtifactRole::OriginalOutput, "result.json");
    wrong.expected_digest = Some(sha256(b"expected"));
    assert_eq!(
        rejection(producer.recorder.attach(&wrong)),
        (ErrorCode::InvalidInput, Some("/expected_digest".into()))
    );
    let mut short = producer.file("result", 1, ArtifactRole::OriginalOutput, "result.json");
    short.expected_size_bytes = Some(1);
    assert_eq!(
        rejection(producer.recorder.attach(&short)),
        (ErrorCode::InvalidInput, Some("/expected_size_bytes".into()))
    );
    assert!(producer.staged_positions().is_empty());

    let mut right = producer.file("result", 1, ArtifactRole::OriginalOutput, "result.json");
    right.expected_digest = Some(sha256(b"actual"));
    assert_eq!(
        producer.recorder.attach(&right).unwrap().receipt.position,
        Position::FIRST
    );
}

#[test]
fn missing_artifacts_are_recorded_as_missing_with_no_bytes() {
    let producer = producer();
    let declared = StableArtifact::missing(
        producer.handle,
        key("trace"),
        Position::FIRST,
        ArtifactRole::Log,
        "the collector crashed",
    );
    let missing = producer.recorder.attach(&declared).unwrap();
    assert_eq!(missing.availability, StagedAvailability::Missing);
    assert_eq!((missing.digest, missing.size_bytes), (None, None));

    let absent = producer.file("receipt", 2, ArtifactRole::Receipt, "receipt.sig");
    let absent = producer.recorder.attach(&absent).unwrap();
    assert_eq!(absent.availability, StagedAvailability::Missing);
    assert_eq!(absent.role, ArtifactRole::Receipt);
    // No empty file stands in for either.
    let staging = Store::open(Workspace::open(&producer.root).unwrap())
        .capture_handles()
        .unwrap()[0]
        .binding
        .staging
        .clone();
    assert!(!producer.root.join(staging).join("blobs").exists());
}

#[test]
fn invalid_descriptors_name_their_field_and_stage_nothing() {
    let producer = producer();
    producer.output("result.json", b"x");
    let base = || producer.file("result", 1, ArtifactRole::OriginalOutput, "result.json");

    let mut derivative_without_origin = base();
    derivative_without_origin.role = ArtifactRole::Derivative;
    let mut origin_without_derivative = base();
    origin_without_derivative.derived_from = Some(Derivation::new(key("other"), "copy/1"));
    let mut self_derived = base();
    self_derived.role = ArtifactRole::Derivative;
    self_derived.derived_from = Some(Derivation::new(key("result"), "copy/1"));
    let mut unknown_origin = base();
    unknown_origin.role = ArtifactRole::Derivative;
    unknown_origin.derived_from = Some(Derivation::new(key("absent"), "copy/1"));
    let mut media = base();
    media.media_type = Some("json".into());
    let mut channel = base();
    channel.channel = Some("bad\nchannel".into());
    let mut looser = base();
    looser.classification = Some(ClassificationLevel::Public);
    let mut empty_reason = base();
    empty_reason.source = ArtifactSource::Missing { reason: " ".into() };
    let mut escape = base();
    escape.source = ArtifactSource::File {
        path: "../workspace/investigation.toml".into(),
    };
    let mut outside = base();
    outside.source = ArtifactSource::File {
        path: producer.root.join("investigation.toml"),
    };
    for (artifact, code, path) in [
        (
            derivative_without_origin,
            ErrorCode::InvalidInput,
            "/derived_from",
        ),
        (
            origin_without_derivative,
            ErrorCode::InvalidInput,
            "/derived_from",
        ),
        (self_derived, ErrorCode::InvalidInput, "/derived_from/from"),
        (
            unknown_origin,
            ErrorCode::UnknownReference,
            "/derived_from/from",
        ),
        (media, ErrorCode::InvalidInput, "/media_type"),
        (channel, ErrorCode::InvalidInput, "/channel"),
        (looser, ErrorCode::InvalidInput, "/classification"),
        (empty_reason, ErrorCode::InvalidInput, "/source/reason"),
        (escape, ErrorCode::InvalidInput, "/source/path"),
        (outside, ErrorCode::InvalidInput, "/source/path"),
    ] {
        assert_eq!(
            rejection(producer.recorder.attach(&artifact)),
            (code, Some(path.into())),
            "{artifact:?}"
        );
    }
    assert!(producer.staged_positions().is_empty());

    // A key used for a fact cannot be reused for an attachment.
    producer
        .recorder
        .append(&CaptureEvent::new(
            producer.handle,
            key("result"),
            Position::FIRST,
            LifecycleFact::Started,
        ))
        .unwrap();
    assert_eq!(
        rejection(producer.recorder.attach(&base())),
        (ErrorCode::IdempotencyKeyReused, Some("/key".into()))
    );
}

#[test]
fn an_attachment_out_of_order_is_refused_with_the_expected_position() {
    let producer = producer();
    producer.output("result.json", b"x");
    let early = producer.file("result", 2, ArtifactRole::OriginalOutput, "result.json");
    let error = producer.recorder.attach(&early).unwrap_err();
    let failure = error.failure().unwrap();
    assert_eq!(failure.error.code, ErrorCode::RevisionConflict);
    assert_eq!(failure.error.expected.as_ref().unwrap()["position"], 1);
    assert!(producer.staged_positions().is_empty());
}
