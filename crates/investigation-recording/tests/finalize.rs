//! `capture.finalize` against a real local workspace, through the public API
//! only. Assertions about canonical state read the store directly, as the
//! conformance runner does; the producer code under test never touches it.

use std::{fs, path::PathBuf};

use investigation_recording::{
    ArtifactRole, BeginCapture, CaptureCompletion, CaptureEvent, CaptureHandle, CaptureIntent,
    CaseTarget, ChannelEvent, ChannelInventory, CommitStatus, Derivation, ErrorCode,
    EvidenceCompleteness, ExecutionReport, ExecutionStatus, ExternalReceipt, ExternalRunId,
    Finalization, FinalizeCapture, FinalizeReceipt, IdempotencyKey, LifecycleFact, LocalRecorder,
    Position, ProducerAssertion, ProducerControl, ProducerIdentity, ProducerName, Recorder,
    RecordingError, StableArtifact,
};
use investigation_store::{Store, Workspace, journal::hash_bytes};
use serde_json::{Value, json};

const ORIGINAL_ERROR: &str = "worker 3: segmentation fault at 0x0\n  (core dumped)  ";

struct Producer {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    outputs: PathBuf,
    recorder: LocalRecorder,
    handle: CaptureHandle,
}

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
    let mut identity = ProducerIdentity::new(ProducerName::new("example-producer").unwrap());
    identity.version = Some("0.1.0".into());
    let begin = BeginCapture::new(
        key("run-0001/begin"),
        "agent:example-producer".parse().unwrap(),
        identity,
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

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).unwrap()
}

fn position(value: u64) -> Position {
    Position::new(value).unwrap()
}

impl Producer {
    fn append(&self, item: &str, at: u64, fact: LifecycleFact) {
        self.recorder
            .append(&CaptureEvent::new(
                self.handle,
                key(item),
                position(at),
                fact,
            ))
            .unwrap();
    }

    fn attach(&self, item: &str, at: u64, role: ArtifactRole, bytes: &[u8]) -> StableArtifact {
        let name = format!("{item}.out");
        fs::write(self.outputs.join(&name), bytes).unwrap();
        let mut artifact = StableArtifact::file(self.handle, key(item), position(at), role, name);
        artifact.channel = Some("results".into());
        artifact.media_type = Some("application/json".into());
        artifact
    }

    fn summary(
        &self,
        last: Option<u64>,
        completion: CaptureCompletion,
        state: ExecutionStatus,
    ) -> FinalizeCapture {
        FinalizeCapture::new(
            self.handle,
            key("run-0001/finalize"),
            last.map(position),
            completion,
            ExecutionReport::new(state),
        )
    }

    fn store(&self) -> Store {
        Store::open(Workspace::open(&self.root).unwrap())
    }

    /// The Run record as currently committed.
    fn run(&self, receipt: &FinalizeReceipt) -> Value {
        let case = self.store().case(receipt.case_id).unwrap();
        let run = case.state.current(receipt.run_id).unwrap();
        serde_json::to_value(run).unwrap()["body"].clone()
    }

    fn kinds(&self, receipt: &FinalizeReceipt) -> Vec<String> {
        let case = self.store().case(receipt.case_id).unwrap();
        case.state
            .records()
            .map(|record| record.header.id.kind().to_string())
            .collect()
    }

    /// The retained manifest bytes, read from the canonical blob store.
    fn manifest(&self, receipt: &FinalizeReceipt) -> Value {
        let hex = receipt
            .manifest_digest
            .as_str()
            .strip_prefix("sha256:")
            .unwrap();
        let bytes = fs::read(
            self.root
                .join("state/blobs/sha256")
                .join(&hex[..2])
                .join(hex),
        )
        .unwrap();
        assert_eq!(hash_bytes(&bytes), receipt.manifest_digest);
        serde_json::from_slice(&bytes).unwrap()
    }
}

fn rejection(result: Result<FinalizeReceipt, RecordingError>) -> (ErrorCode, Option<String>) {
    let error = result.unwrap_err();
    assert_eq!(error.commit_status(), CommitStatus::NotCommitted);
    let failure = error.failure().expect("a structured rejection");
    (failure.error.code, failure.error.path.clone())
}

#[test]
fn a_complete_capture_finalizes_into_one_manifest_bound_to_the_run() {
    let producer = producer();
    producer.append("started", 1, LifecycleFact::Started);
    let result = producer.attach(
        "result",
        2,
        ArtifactRole::OriginalOutput,
        br#"{"metric":"latency.p99","value":18.7,"unit":"ms"}"#,
    );
    producer.recorder.attach(&result).unwrap();
    let mut summary_artifact = producer.attach("summary", 3, ArtifactRole::Derivative, b"p99 18.7");
    summary_artifact.derived_from = Some(Derivation::new(key("result"), "summarize/2"));
    producer.recorder.attach(&summary_artifact).unwrap();
    producer.append(
        "results-closed",
        4,
        LifecycleFact::Channel {
            channel: "results".into(),
            state: ChannelEvent::Closed,
        },
    );

    let mut summary = producer.summary(
        Some(4),
        CaptureCompletion::Complete,
        ExecutionStatus::Completed,
    );
    summary.execution.exit_code = Some(0);
    summary.channels.push(ChannelInventory::new(
        "results",
        EvidenceCompleteness::Complete,
    ));
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
    let receipt = producer.recorder.finalize(&summary).unwrap();

    assert_eq!(receipt.handle, producer.handle);
    assert_eq!(receipt.finalization, Finalization::Finalized);
    assert_eq!(receipt.execution, ExecutionStatus::Completed);
    assert_eq!(receipt.sealed_position, Some(position(4)));
    assert!(receipt.missing_positions.is_empty());
    let commit = serde_json::to_value(&receipt.commit).unwrap();
    assert_eq!(commit["result"], "committed");

    let run = producer.run(&receipt);
    assert_eq!(run["execution_status"], "completed");
    assert_eq!(run["evidence_completeness"], "complete");
    // Exit code 0, a completed state and a reported_pass control set no validity.
    assert_eq!(run["evidence_validity"], "unknown");
    assert_eq!(run["result_assertion"], Value::Null);
    assert_eq!(run["manifest_digest"], json!(receipt.manifest_digest));
    let section = &run["capture_finalization"];
    assert_eq!(section["attribution"], "producer_reported");
    assert_eq!(
        section["producer"],
        json!({"name": "example-producer", "version": "0.1.0"})
    );
    assert_eq!(section["external_run_id"], "run-0001");
    assert_eq!(
        section["assertions"][0]["validity_claim"],
        "producer_reported"
    );
    assert_eq!(section["controls"][0]["producer_result"], "reported_pass");
    assert_eq!(
        section["manifest"]["digest"],
        json!(receipt.manifest_digest)
    );
    assert_eq!(
        section["channels"],
        json!([{"channel": "results", "claimed": "complete", "recorded": "complete",
                "retained_artifacts": 2, "missing_artifacts": 0, "closed_at": 4}])
    );
    let artifacts = section["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 2);
    assert_eq!(artifacts[0]["retention"]["state"], "retained");
    assert_eq!(artifacts[1]["retention"]["derived_from"], "result");
    // The Run references the manifest and both retained Artifacts.
    let refs = run["artifact_refs"].as_array().unwrap();
    assert_eq!(refs.len(), 3);
    assert!(refs.contains(&section["manifest"]["artifact_ref"]));
    assert!(refs.contains(&artifacts[1]["retention"]["artifact_ref"]));

    // No interpretation is created: only the case, the Run, the capture
    // Source and three Artifacts.
    let mut kinds = producer.kinds(&receipt);
    kinds.sort();
    assert_eq!(kinds, ["art", "art", "art", "case", "run", "src"]);

    // The manifest holds every item's key, digest and request, and excludes
    // itself and its own transaction.
    let manifest = producer.manifest(&receipt);
    assert_eq!(manifest["schema_version"], "inv.capture-manifest.v1");
    assert_eq!(manifest["items"].as_array().unwrap().len(), 4);
    assert_eq!(manifest["items"][1]["key"], "result");
    assert_eq!(manifest["snapshot"]["sequence"], 1);
    assert!(
        !manifest
            .to_string()
            .contains(receipt.manifest_digest.as_str())
    );
    assert!(
        !manifest
            .to_string()
            .contains(&receipt.commit.transaction_id.to_string())
    );

    // An identical retry returns the original receipt; a late item is refused.
    let retry = producer.recorder.finalize(&summary).unwrap();
    assert_eq!(
        serde_json::to_value(&retry.commit).unwrap()["result"],
        "already_committed"
    );
    assert_eq!(retry.commit.transaction_id, receipt.commit.transaction_id);
    assert_eq!(retry.manifest_digest, receipt.manifest_digest);
    let late = producer.recorder.append(&CaptureEvent::new(
        producer.handle,
        key("late"),
        position(5),
        LifecycleFact::Started,
    ));
    assert_eq!(late.unwrap_err().code(), Some(ErrorCode::RevisionConflict));
}

#[test]
fn a_claimed_complete_finalize_with_a_stale_cursor_is_rejected_and_seals_nothing() {
    let producer = producer();
    producer.append("started", 1, LifecycleFact::Started);
    producer.append(
        "phase",
        2,
        LifecycleFact::PhaseChanged {
            phase: "run".into(),
        },
    );
    for stale in [None, Some(1), Some(3)] {
        let summary = producer.summary(
            stale,
            CaptureCompletion::Complete,
            ExecutionStatus::Completed,
        );
        let error = producer.recorder.finalize(&summary).unwrap_err();
        let failure = error.failure().unwrap();
        assert_eq!(failure.error.code, ErrorCode::RevisionConflict, "{stale:?}");
        assert_eq!(
            failure.error.path.as_deref(),
            Some("/expected_last_position")
        );
        assert_eq!(failure.error.expected.as_ref().unwrap()["last_position"], 2);
    }
    // A partial claim behind the accepted cursor is stale too.
    let behind = producer.summary(
        Some(1),
        CaptureCompletion::Partial,
        ExecutionStatus::Unknown,
    );
    assert_eq!(
        rejection(producer.recorder.finalize(&behind)),
        (
            ErrorCode::RevisionConflict,
            Some("/expected_last_position".into())
        )
    );
    // Nothing was sealed: the handle still takes its next item.
    producer.append("phase-2", 3, LifecycleFact::Started);
    let current = producer.summary(
        Some(3),
        CaptureCompletion::Complete,
        ExecutionStatus::Completed,
    );
    assert_eq!(
        producer.recorder.finalize(&current).unwrap().finalization,
        Finalization::Finalized
    );
}

#[test]
fn a_failed_process_is_finalized_with_its_exact_error_and_what_survived() {
    let producer = producer();
    producer.append("started", 1, LifecycleFact::Started);
    let partial = producer.attach("partial", 2, ArtifactRole::OriginalOutput, b"[1,2,3]");
    producer.recorder.attach(&partial).unwrap();
    producer.append(
        "crash",
        3,
        LifecycleFact::Error {
            message: ORIGINAL_ERROR.into(),
            external_code: Some("SIGSEGV".into()),
        },
    );
    let mut trace = StableArtifact::missing(
        producer.handle,
        key("trace"),
        position(4),
        ArtifactRole::Log,
        "the collector died with the worker",
    );
    trace.channel = Some("trace".into());
    producer.recorder.attach(&trace).unwrap();

    // The producer sent position 5 but it never arrived.
    let mut summary =
        producer.summary(Some(5), CaptureCompletion::Partial, ExecutionStatus::Failed);
    summary.execution.exit_code = Some(139);
    summary.execution.error = Some(ORIGINAL_ERROR.into());
    summary.declared_artifacts = vec![key("partial"), key("trace"), key("report")];
    summary.channels.push(ChannelInventory::new(
        "trace",
        EvidenceCompleteness::Complete,
    ));
    let receipt = producer.recorder.finalize(&summary).unwrap();

    assert_eq!(receipt.finalization, Finalization::FinalizedPartial);
    assert_eq!(receipt.execution, ExecutionStatus::Failed);
    assert_eq!(receipt.sealed_position, Some(position(4)));
    assert_eq!(receipt.missing_positions, [position(5)]);
    assert!(receipt.commit.warnings.iter().any(|w| w.contains("report")));

    let run = producer.run(&receipt);
    assert_eq!(run["execution_status"], "failed");
    assert_eq!(run["evidence_completeness"], "partial");
    assert_eq!(run["evidence_validity"], "unknown");
    let section = &run["capture_finalization"];
    // The original error, byte for byte; no verdict of any kind.
    assert_eq!(section["execution"]["error"], ORIGINAL_ERROR);
    assert_eq!(section["execution"]["exit_code"], 139);
    for absent in ["verdict", "result", "conclusion", "measurement_validity"] {
        assert!(section.get(absent).is_none(), "{absent}");
    }
    let retention: Vec<_> = section["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| (entry["key"].clone(), entry["retention"].clone()))
        .collect();
    assert_eq!(retention[0].0, "partial");
    assert_eq!(retention[0].1["state"], "retained");
    assert_eq!(
        retention[1],
        (
            json!("trace"),
            json!({"state": "missing", "cause": "declared", "reason": "the collector died with the worker"})
        )
    );
    assert_eq!(
        retention[2],
        (
            json!("report"),
            json!({"state": "missing", "cause": "not_attached"})
        )
    );
    // The producer's channel claim does not survive a missing artifact.
    assert_eq!(section["channels"][0]["claimed"], "complete");
    assert_eq!(section["channels"][0]["recorded"], "missing");

    // The partial output is retained bytes; the error fact survives in the
    // manifest with its exact text.
    let manifest = producer.manifest(&receipt);
    assert_eq!(
        manifest["items"][2]["request"]["fact"]["message"],
        ORIGINAL_ERROR
    );
    assert_eq!(manifest["execution"]["error"], ORIGINAL_ERROR);
    assert_eq!(manifest["missing_positions"], json!([5]));
    let mut kinds = producer.kinds(&receipt);
    kinds.sort();
    assert_eq!(kinds, ["art", "art", "case", "run", "src"]);
}

#[test]
fn an_interrupted_capture_with_unknown_execution_is_representable() {
    let producer = producer();
    producer.append(
        "log-open",
        1,
        LifecycleFact::Channel {
            channel: "log".into(),
            state: ChannelEvent::Opened,
        },
    );
    let mut log = producer.attach("log", 2, ArtifactRole::Log, b"line 1\n");
    log.channel = Some("log".into());
    producer.recorder.attach(&log).unwrap();
    let summary = producer.summary(
        None,
        CaptureCompletion::Interrupted,
        ExecutionStatus::Unknown,
    );
    let receipt = producer.recorder.finalize(&summary).unwrap();
    assert_eq!(receipt.finalization, Finalization::FinalizedPartial);
    assert_eq!(receipt.execution, ExecutionStatus::Unknown);
    let run = producer.run(&receipt);
    assert_eq!(run["execution_status"], "unknown");
    assert_eq!(run["evidence_completeness"], "partial");
    // The log was never closed and the producer cannot vouch for its stream.
    assert_eq!(
        run["capture_finalization"]["channels"][0]["recorded"],
        "partial"
    );
}

#[test]
fn conflicting_retries_are_refused_with_the_original_request() {
    let producer = producer();
    producer.append("started", 1, LifecycleFact::Started);
    let summary = producer.summary(
        Some(1),
        CaptureCompletion::Complete,
        ExecutionStatus::Failed,
    );
    let receipt = producer.recorder.finalize(&summary).unwrap();

    let mut changed = summary.clone();
    changed.execution.exit_code = Some(1);
    assert_eq!(
        rejection(producer.recorder.finalize(&changed)),
        (ErrorCode::IdempotencyKeyReused, Some("/key".into()))
    );
    let mut other = summary.clone();
    other.key = key("run-0001/finalize-again");
    let error = producer.recorder.finalize(&other).unwrap_err();
    let failure = error.failure().unwrap();
    assert_eq!(failure.error.code, ErrorCode::RevisionConflict);
    assert_eq!(failure.error.path.as_deref(), Some("/handle"));
    assert_eq!(
        failure.error.expected.as_ref().unwrap()["command_id"],
        "run-0001/finalize"
    );
    assert!(failure.error.repair.as_ref().unwrap().argv.is_some());
    // The original is untouched.
    let retry = producer.recorder.finalize(&summary).unwrap();
    assert_eq!(retry.commit.transaction_id, receipt.commit.transaction_id);
}

#[test]
fn invalid_summaries_name_their_field_and_seal_nothing() {
    let producer = producer();
    producer.append("started", 1, LifecycleFact::Started);
    let base = || {
        producer.summary(
            Some(1),
            CaptureCompletion::Complete,
            ExecutionStatus::Completed,
        )
    };
    let mut fact_declared = base();
    fact_declared.declared_artifacts = vec![key("started")];
    let mut twice = base();
    twice.declared_artifacts = vec![key("a"), key("a")];
    let mut bare_receipt = base();
    bare_receipt
        .receipts
        .push(ExternalReceipt::new("signed-statement"));
    let mut unknown_receipt = base();
    let mut receipt = ExternalReceipt::new("signed-statement");
    receipt.artifact = Some(key("absent"));
    unknown_receipt.receipts.push(receipt);
    let mut channel = base();
    channel.channels.push(ChannelInventory::new(
        "bad\nname",
        EvidenceCompleteness::Complete,
    ));
    let mut empty_error = base();
    empty_error.execution.error = Some(String::new());
    let mut beyond = base();
    beyond.completion = CaptureCompletion::Partial;
    beyond.expected_last_position = Some(position(1_000_000));
    for (summary, path) in [
        (fact_declared, "/declared_artifacts/0"),
        (twice, "/declared_artifacts/1"),
        (bare_receipt, "/receipts/0"),
        (unknown_receipt, "/receipts/0/artifact"),
        (channel, "/channels/0/channel"),
        (empty_error, "/execution/error"),
        (beyond, "/expected_last_position"),
    ] {
        assert_eq!(
            rejection(producer.recorder.finalize(&summary)),
            (ErrorCode::InvalidInput, Some(path.into())),
            "{summary:?}"
        );
    }
    // Nothing was sealed.
    producer.append("next", 2, LifecycleFact::Started);
}

#[test]
fn an_undeclared_handle_is_refused() {
    let producer = producer();
    let summary = FinalizeCapture::new(
        "handle_01936f0e-7c6a-7d3e-8a1b-0123456789ab"
            .parse()
            .unwrap(),
        key("f"),
        None,
        CaptureCompletion::Interrupted,
        ExecutionReport::new(ExecutionStatus::Unknown),
    );
    assert_eq!(
        rejection(producer.recorder.finalize(&summary)),
        (ErrorCode::UnknownReference, Some("/handle".into()))
    );
}
