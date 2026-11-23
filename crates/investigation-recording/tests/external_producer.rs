//! External producer fixture.
//!
//! Integration tests compile as a separate crate, so this file sees only the
//! public API. It is the compile-time contract a native producer relies on: if a
//! public type, constructor or signature used here changes, this fixture fails.
//! Workspace setup uses the store directly; the producer code does not.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use investigation_recording::{
    ArtifactRef, ArtifactRole, BeginCapture, BeginReceipt, CaptureCompletion, CaptureEvent,
    CaptureHandle, CaptureIntent, CaseTarget, ChannelEvent, ChannelInventory, CommitStatus,
    Derivation, ErrorCode, EvidenceCompleteness, ExecutionReport, ExecutionStatus, ExternalReceipt,
    ExternalRunId, Failure, Finalization, FinalizeCapture, FinalizeReceipt, IdempotencyKey,
    ItemReceipt, LifecycleFact, LocalRecorder, Position, ProducerAssertion, ProducerControl,
    ProducerIdentity, ProducerName, Recorder, RecordingError, RecoverScope, RecoveryState,
    SealState, StableArtifact,
};
use serde_json::{Value, json};

const HANDLE: &str = "handle_01936f0e-7c6a-7d3e-8a1b-0123456789ab";

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).unwrap()
}

/// A complete synthetic producer flow written against the public trait only.
///
/// It resends the identical request after an ambiguous failure, which is the
/// retry rule every producer follows, and resumes from `recover` after a restart.
fn record_synthetic<R: Recorder + ?Sized>(
    recorder: &R,
    output: &Path,
) -> Result<FinalizeReceipt, RecordingError> {
    let mut producer = ProducerIdentity::new(ProducerName::new("example-producer").unwrap());
    producer.version = Some("0.1.0".into());
    let mut begin = BeginCapture::new(
        key("run-0001/begin"),
        "agent:example-producer".parse().unwrap(),
        producer,
        ExternalRunId::new("run-0001").unwrap(),
        CaptureIntent::new("synthetic fixture run"),
        CaseTarget::NewRecordCase {
            title: "Synthetic runs".into(),
        },
    );
    begin
        .metadata
        .insert("example-org.fixture".into(), json!({"seed": 7}));

    let receipt: BeginReceipt = match recorder.begin(&begin) {
        Err(error) if error.commit_status() == CommitStatus::Unknown => recorder.begin(&begin)?,
        other => other?,
    };
    let handle = receipt.handle;

    // A restarted producer asks where to resume instead of repeating its action.
    let state = recorder.recover(&RecoverScope::Handle { handle })?;
    let recovered = state.handles.first().expect("the declared handle");
    if let Some(finalized) = &recovered.finalized {
        return Ok(finalized.clone());
    }
    // Step `n` takes position `n`; steps at or before the last accepted
    // position were recorded already and are skipped, never repeated.
    let done = recovered.last_position.map_or(0, Position::get);
    let at = |step: u64| Position::new(step).expect("nonzero");

    if done < 1 {
        let started = CaptureEvent::new(handle, key("started"), at(1), LifecycleFact::Started);
        let _: ItemReceipt = recorder.append(&started)?;
    }
    if done < 2 {
        let channel = CaptureEvent::new(
            handle,
            key("log-open"),
            at(2),
            LifecycleFact::Channel {
                channel: "log".into(),
                state: ChannelEvent::Opened,
            },
        );
        recorder.append(&channel)?;
    }
    if done < 3 {
        let mut original = StableArtifact::file(
            handle,
            key("result"),
            at(3),
            ArtifactRole::OriginalOutput,
            output,
        );
        original.media_type = Some("application/json".into());
        let _: ArtifactRef = recorder.attach(&original)?;
    }
    if done < 4 {
        let mut summary_artifact = StableArtifact::missing(
            handle,
            key("summary"),
            at(4),
            ArtifactRole::Derivative,
            "the summarizer did not run",
        );
        summary_artifact.derived_from = Some(Derivation::new(key("result"), "summary/1"));
        recorder.attach(&summary_artifact)?;
    }
    let last = at(4);

    let mut execution = ExecutionReport::new(ExecutionStatus::Completed);
    execution.exit_code = Some(0);
    let mut summary = FinalizeCapture::new(
        handle,
        key("run-0001/finalize"),
        Some(last),
        CaptureCompletion::Complete,
        execution,
    );
    summary
        .channels
        .push(ChannelInventory::new("log", EvidenceCompleteness::Partial));
    summary.declared_artifacts = vec![key("result"), key("summary")];
    let mut metric = ProducerAssertion::new("reported_metric", "latency.p99", json!(18.7));
    metric.unit = Some("ms".into());
    summary.assertions.push(metric);
    summary
        .controls
        .push(ProducerControl::new("steady-load", "reported_pass"));
    let mut statement = ExternalReceipt::new("signed-statement");
    statement.reference = Some("urn:example:statement:1".into());
    summary.receipts.push(statement);
    summary.limitations.push("Single synthetic workload".into());
    recorder.finalize(&summary)
}

/// A producer-side wrapper, as a spool or retry layer would be.
struct Counting<R> {
    inner: R,
    calls: AtomicUsize,
}

impl<R: Recorder> Recorder for Counting<R> {
    fn begin(&self, request: &BeginCapture) -> Result<BeginReceipt, RecordingError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.begin(request)
    }
    fn append(&self, event: &CaptureEvent) -> Result<ItemReceipt, RecordingError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.append(event)
    }
    fn attach(&self, artifact: &StableArtifact) -> Result<ArtifactRef, RecordingError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.attach(artifact)
    }
    fn finalize(&self, summary: &FinalizeCapture) -> Result<FinalizeReceipt, RecordingError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.finalize(summary)
    }
    fn recover(&self, scope: &RecoverScope) -> Result<RecoveryState, RecordingError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.recover(scope)
    }
}

/// A workspace whose import root is the producer's output directory, outside
/// the workspace, as `attach` requires.
fn workspace() -> (tempfile::TempDir, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let outputs = temporary.path().join("outputs");
    fs::create_dir(&outputs).unwrap();
    investigation_store::Workspace::init(&root, false).unwrap();
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
    (temporary, root)
}

fn output(root: &Path) -> PathBuf {
    let output = root.parent().unwrap().join("outputs/result.json");
    fs::write(
        &output,
        b"{\"metric\":\"latency.p99\",\"value\":18.7,\"unit\":\"ms\"}\n",
    )
    .unwrap();
    output
}

fn tree(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                pending.push(entry.path());
            }
            entries.push((
                entry.path().strip_prefix(root).unwrap().to_owned(),
                metadata.len(),
            ));
        }
    }
    entries.sort();
    entries
}

/// The tree minus the disposable index, whose page files vary between reads.
fn tree_without_index(root: &Path) -> Vec<(PathBuf, u64)> {
    tree_without_index_of(tree(root))
}

fn tree_without_index_of(entries: Vec<(PathBuf, u64)>) -> Vec<(PathBuf, u64)> {
    entries
        .into_iter()
        .filter(|(path, _)| !path.starts_with("cache"))
        .collect()
}

#[test]
fn external_producer_runs_against_every_recorder_form() {
    let (_temporary, root) = workspace();
    let output = output(&root);
    let before = tree(&root);
    let local = LocalRecorder::open(&root).unwrap();
    assert_eq!(local.workspace_root(), root.as_path());

    let counting = Counting {
        inner: local.clone(),
        calls: AtomicUsize::new(0),
    };
    let boxed: Box<dyn Recorder> = Box::new(local.clone());
    let shared = Arc::new(local.clone());
    let as_dyn: &dyn Recorder = &local;

    // The first flow records the whole run. Every later flow resends the
    // identical begin, recovers the finalized handle and returns its original
    // receipt without repeating a step, for every way of holding a recorder.
    let first = record_synthetic(&local, &output).unwrap();
    assert_eq!(first.finalization, Finalization::Finalized);
    assert_eq!(first.execution, ExecutionStatus::Completed);
    assert_eq!(first.sealed_position, Position::new(4));
    let finalized = tree(&root);
    assert_ne!(finalized, before);
    for receipt in [
        record_synthetic(as_dyn, &output).unwrap(),
        record_synthetic(&boxed, &output).unwrap(),
        record_synthetic(&shared, &output).unwrap(),
        record_synthetic(&counting, &output).unwrap(),
    ] {
        assert_eq!(receipt.run_id, first.run_id);
        assert_eq!(receipt.manifest_digest, first.manifest_digest);
        assert_eq!(receipt.commit.transaction_id, first.commit.transaction_id);
        assert_eq!(
            serde_json::to_value(receipt.commit.result).unwrap(),
            "already_committed"
        );
    }
    // Begin and recover only.
    assert_eq!(counting.calls.load(Ordering::Relaxed), 2);
    assert_eq!(tree_without_index(&root), tree_without_index_of(finalized));
    let mut store =
        investigation_store::Store::open(investigation_store::Workspace::open(&root).unwrap());
    assert_eq!(store.list_cases().unwrap().len(), 1);
    assert_eq!(store.capture_handles().unwrap().len(), 1);
}

#[test]
fn a_restarted_producer_resumes_after_the_last_accepted_position() {
    let (_temporary, root) = workspace();
    let output = output(&root);
    let local = LocalRecorder::open(&root).unwrap();
    // The first attempt crashes after its first step.
    let crashing = CrashAfter {
        inner: local.clone(),
        appends: AtomicUsize::new(0),
    };
    let error = record_synthetic(&crashing, &output).unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::IoFailure));
    let unsettled = local.recover(&RecoverScope::Unsettled).unwrap().handles;
    assert_eq!(unsettled.len(), 1);
    assert_eq!(unsettled[0].seal, SealState::Open);
    assert_eq!(unsettled[0].last_position, Some(Position::FIRST));

    // The restart continues at position 2 and never resends the first step.
    let counting = Counting {
        inner: local.clone(),
        calls: AtomicUsize::new(0),
    };
    let receipt = record_synthetic(&counting, &output).unwrap();
    assert_eq!(receipt.finalization, Finalization::Finalized);
    // Begin, recover, three items and finalize.
    assert_eq!(counting.calls.load(Ordering::Relaxed), 6);
    let recovered = local
        .recover(&RecoverScope::Handle {
            handle: receipt.handle,
        })
        .unwrap();
    let keys: Vec<_> = recovered.handles[0]
        .items
        .iter()
        .map(|item| item.key.as_str().to_owned())
        .collect();
    assert_eq!(keys, ["started", "log-open"]);
    assert!(
        local
            .recover(&RecoverScope::Unsettled)
            .unwrap()
            .handles
            .is_empty()
    );
}

/// A producer whose process dies after its first appended fact.
struct CrashAfter<R> {
    inner: R,
    appends: AtomicUsize,
}

impl<R: Recorder> Recorder for CrashAfter<R> {
    fn begin(&self, request: &BeginCapture) -> Result<BeginReceipt, RecordingError> {
        self.inner.begin(request)
    }
    fn append(&self, event: &CaptureEvent) -> Result<ItemReceipt, RecordingError> {
        if self.appends.fetch_add(1, Ordering::Relaxed) >= 1 {
            return Err(RecordingError::Rejected(Failure::new(
                ErrorCode::IoFailure,
                "the producer stopped",
            )));
        }
        self.inner.append(event)
    }
    fn attach(&self, artifact: &StableArtifact) -> Result<ArtifactRef, RecordingError> {
        self.inner.attach(artifact)
    }
    fn finalize(&self, summary: &FinalizeCapture) -> Result<FinalizeReceipt, RecordingError> {
        self.inner.finalize(summary)
    }
    fn recover(&self, scope: &RecoverScope) -> Result<RecoveryState, RecordingError> {
        self.inner.recover(scope)
    }
}

#[test]
fn every_local_operation_on_an_undeclared_handle_records_nothing() {
    let (_temporary, root) = workspace();
    let before = tree(&root);
    let recorder = LocalRecorder::open(&root).unwrap();
    let handle: CaptureHandle = HANDLE.parse().unwrap();
    // `append` on an undeclared handle is refused and stages nothing.
    let event = CaptureEvent::new(handle, key("e1"), Position::FIRST, LifecycleFact::Started);
    let error = recorder.append(&event).unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::UnknownReference));
    assert_eq!(error.commit_status(), CommitStatus::NotCommitted);
    let artifact = StableArtifact::file(
        handle,
        key("a1"),
        Position::FIRST,
        ArtifactRole::Log,
        root.join("absent.log"),
    );
    // So is `attach`.
    let error = recorder.attach(&artifact).unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::UnknownReference));
    assert_eq!(error.commit_status(), CommitStatus::NotCommitted);
    let summary = FinalizeCapture::new(
        handle,
        key("finalize"),
        None,
        CaptureCompletion::Interrupted,
        ExecutionReport::new(ExecutionStatus::Unknown),
    );
    // So is `finalize`.
    let error = recorder.finalize(&summary).unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::UnknownReference));
    assert_eq!(error.commit_status(), CommitStatus::NotCommitted);
    // `recover` of that handle too; with nothing declared, nothing is unsettled.
    let error = recorder
        .recover(&RecoverScope::Handle { handle })
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::UnknownReference));
    let state = recorder.recover(&RecoverScope::Unsettled).unwrap();
    assert!(state.handles.is_empty());
    assert_eq!(tree(&root), before);
}

#[test]
fn opening_a_directory_without_a_workspace_is_a_structured_failure() {
    let temporary = tempfile::tempdir().unwrap();
    let error = LocalRecorder::open(temporary.path().join("absent")).unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::WorkspaceRequired));
    assert_eq!(error.commit_status(), CommitStatus::NotCommitted);
    let failure = error.failure().unwrap();
    assert_eq!(failure.error.path.as_deref(), Some("/workspace"));
}

#[test]
fn recorders_can_be_shared_across_threads() {
    fn shareable<T: Send + Sync + Clone>() {}
    shareable::<LocalRecorder>();
    fn object(_: &dyn Recorder) {}
    let (_temporary, root) = workspace();
    object(&LocalRecorder::open(root).unwrap());
}

fn round_trip<T>(value: Value) -> T
where
    T: serde::de::DeserializeOwned + serde::Serialize + PartialEq + std::fmt::Debug,
{
    let parsed: T = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
    parsed
}

/// The serialized forms are the JSON-CLI capture payloads. These literals pin
/// their field names so a Rust rename cannot silently change the wire.
#[test]
fn request_wire_shapes_are_pinned() {
    let begin: BeginCapture = round_trip(json!({
        "key": "run-0001/begin",
        "actor": "agent:example-producer",
        "producer": {"name": "example-producer", "version": "0.1.0", "instance": "local-a"},
        "external_run_id": "run-0001",
        "intent": {
            "label": "compare two queue settings",
            "external_plan_digest": format!("sha256:{}", "ab".repeat(32))
        },
        "case": {"kind": "new_record_case", "title": "Queue setting runs"},
        "classification": "restricted",
        "metadata": {"example-org.instrument": {"rate": 3}}
    }));
    assert_eq!(begin.external_run_id.as_str(), "run-0001");

    let event: CaptureEvent = round_trip(json!({
        "handle": HANDLE,
        "key": "phase-2",
        "position": 2,
        "fact": {"kind": "error", "message": "connection reset", "external_code": "E42"},
        "reported_at": {
            "timestamp": {"state": "known", "value": "2026-01-02T03:04:05.000Z"},
            "offset": {"state": "unknown"},
            "clock_quality": "unverified"
        }
    }));
    assert_eq!(event.position.get(), 2);
    round_trip::<CaptureEvent>(json!({
        "handle": HANDLE, "key": "c", "position": 3,
        "fact": {"kind": "channel", "channel": "trace", "state": {"kind": "gap", "detail": "ring buffer overrun"}}
    }));

    round_trip::<StableArtifact>(json!({
        "handle": HANDLE,
        "key": "extract",
        "position": 4,
        "role": "derivative",
        "source": {"kind": "file", "path": "out/extract.txt"},
        "channel": "trace",
        "derived_from": {"from": "result", "transform": "text-extract/1"},
        "media_type": "text/plain",
        "expected_digest": format!("sha256:{}", "0".repeat(64)),
        "expected_size_bytes": 12,
        "classification": "internal"
    }));
    round_trip::<StableArtifact>(json!({
        "handle": HANDLE, "key": "result", "position": 1, "role": "original-output",
        "source": {"kind": "missing", "reason": "process killed before write"}
    }));

    round_trip::<FinalizeCapture>(json!({
        "handle": HANDLE,
        "key": "run-0001/finalize",
        "expected_last_position": 4,
        "completion": "partial",
        "execution": {"state": "failed", "exit_code": 3, "error": "worker exited"},
        "channels": [{"channel": "trace", "completeness": "partial"}],
        "declared_artifacts": ["result", "extract"],
        "assertions": [{"kind": "reported_metric", "name": "latency.p99", "value": 18.7, "unit": "ms", "validity_claim": "producer_reported"}],
        "controls": [{"name": "steady-load", "producer_result": "reported_pass"}],
        "receipts": [{"kind": "signed-statement", "artifact": "statement"}],
        "limitations": ["Single synthetic workload"]
    }));
    round_trip::<FinalizeCapture>(json!({
        "handle": HANDLE, "key": "f", "expected_last_position": null,
        "completion": "interrupted", "execution": {"state": "unknown"}
    }));
    round_trip::<RecoverScope>(json!({"kind": "unsettled"}));
    round_trip::<RecoverScope>(json!({"kind": "handle", "handle": HANDLE}));
}

#[test]
fn receipt_wire_shapes_are_pinned() {
    let digest = format!("sha256:{}", "c".repeat(64));
    let commit = json!({
        "schema_version": "inv.response.v1",
        "ok": true,
        "result": "committed",
        "command_id": "run-0001/begin",
        "transaction_id": "tx_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
        "snapshot": {"sequence": 1, "head_hash": digest},
        "created": [{"id": "run_01936f0e-7c6a-7d3e-8a1b-0123456789ab", "revision": 1}],
        "updated": [],
        "projection": {"index": "current", "vault": "disabled"},
        "warnings": [],
        "next": []
    });
    let begin: BeginReceipt = round_trip(json!({
        "handle": HANDLE,
        "run_id": "run_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
        "case_id": "case_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
        "protocol": "inv.capture.v1",
        "limits": {"max_event_bytes": 65536, "max_attachment_bytes": 67108864, "max_items": 100000},
        "staging_dir": "state/staging/run_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
        "commit": commit
    }));
    assert_eq!(begin.handle.to_string(), HANDLE);

    let item = json!({
        "result": "already_accepted",
        "handle": HANDLE,
        "run_id": "run_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
        "position": 1,
        "key": "result",
        "request_digest": digest,
        "accepted_at": "2026-01-02T03:04:05.123Z"
    });
    round_trip::<ItemReceipt>(item.clone());
    let artifact: ArtifactRef = round_trip(json!({
        "receipt": item, "role": "original-output", "availability": "staged",
        "digest": digest, "size_bytes": 12
    }));
    let finalized = json!({
        "handle": HANDLE,
        "run_id": "run_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
        "case_id": "case_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
        "finalization": "finalized_partial",
        "execution": "failed",
        "sealed_position": 4,
        "missing_positions": [3],
        "manifest_digest": digest,
        "commit": commit
    });
    round_trip::<FinalizeReceipt>(finalized.clone());
    let state: RecoveryState = round_trip(json!({
        "handles": [{
            "handle": HANDLE,
            "run_id": "run_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
            "case_id": "case_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
            "producer": {"name": "example-producer"},
            "external_run_id": "run-0001",
            "seal": "finalized",
            "last_position": 4,
            "items": [],
            "artifacts": [serde_json::to_value(&artifact).unwrap()],
            "finalized": finalized,
            "staging": "retired",
            "execution": "failed",
            "next": [{
                "action": "lookup_receipt",
                "argv": ["investigation", "receipt", "run-0001/finalize", "--actor", "agent:example-producer", "--format", "json"],
                "key": "run-0001/finalize"
            }]
        }, {
            "handle": HANDLE,
            "run_id": "run_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
            "case_id": "case_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
            "producer": {"name": "example-producer"},
            "external_run_id": "run-0001",
            "seal": "sealed",
            "last_position": 4,
            "items": [],
            "artifacts": [],
            "staging_dir": "state/staging/run_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
            "staging": "present",
            "execution": "unknown",
            "next": [{
                "action": "resend_finalize",
                "argv": ["investigation", "capture", "finalize", "--input", "-", "--format", "json"],
                "key": "run-0001/finalize",
                "request_digest": digest
            }]
        }]
    }));
    assert_eq!(state.handles[0].seal, SealState::Finalized);
    assert_eq!(state.handles[1].execution, ExecutionStatus::Unknown);
    // Fields added after the first shape are optional on input.
    let minimal: RecoveryState = serde_json::from_value(json!({"handles": [{
        "handle": HANDLE,
        "run_id": "run_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
        "case_id": "case_01936f0e-7c6a-7d3e-8a1b-0123456789ab",
        "producer": {"name": "example-producer"},
        "external_run_id": "run-0001",
        "seal": "open",
        "last_position": null,
        "items": [],
        "artifacts": []
    }]}))
    .unwrap();
    assert_eq!(minimal.handles[0].execution, ExecutionStatus::Unknown);
}

#[test]
fn invalid_values_are_refused_before_reaching_a_recorder() {
    assert!(IdempotencyKey::new("").is_err());
    assert!(IdempotencyKey::new("a\nb").is_err());
    assert!(IdempotencyKey::new("k".repeat(129)).is_err());
    assert!(IdempotencyKey::new("é".repeat(128)).is_ok());
    assert!(ExternalRunId::new("r".repeat(257)).is_err());
    assert!(ProducerName::new("-leading").is_err());
    assert!(ProducerName::new("has space").is_err());
    assert!(ProducerName::new("example-org/bench.v2").is_ok());
    assert_eq!(Position::new(0), None);
    assert_eq!(Position::FIRST.next().map(Position::get), Some(2));
    assert_eq!(Position::new(u64::MAX).and_then(Position::next), None);
    assert!(
        "run_01936f0e-7c6a-7d3e-8a1b-0123456789ab"
            .parse::<CaptureHandle>()
            .is_err()
    );

    let refuse = |value: Value| serde_json::from_value::<CaptureEvent>(value).is_err();
    let valid = json!({"handle": HANDLE, "key": "k", "position": 1, "fact": {"kind": "started"}});
    assert!(!refuse(valid.clone()));
    let mut zero = valid.clone();
    zero["position"] = json!(0);
    assert!(refuse(zero));
    let mut unknown = valid.clone();
    unknown["command"] = json!("rm -rf /");
    assert!(refuse(unknown));
    let mut empty_key = valid;
    empty_key["key"] = json!("");
    assert!(refuse(empty_key));
}
