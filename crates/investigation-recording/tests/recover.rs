//! `capture.recover` against a real local workspace, through the public API
//! only. A producer that crashed or lost a response asks the recorder what is
//! durable, then continues its recording; it never repeats its action.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use investigation_recording::{
    ArtifactRole, BeginCapture, CaptureCompletion, CaptureEvent, CaptureHandle, CaptureIntent,
    CaseTarget, CommitStatus, ErrorCode, EvidenceCompleteness, ExecutionReport, ExecutionStatus,
    ExternalRunId, Finalization, FinalizeCapture, HandleRecovery, IdempotencyKey, LifecycleFact,
    LocalRecorder, Position, ProducerIdentity, ProducerName, ReceiptResult, Recorder, RecoverScope,
    RecoveryAction, SealState, StableArtifact, StagedAvailability, StagingState,
};
use investigation_store::{Store, Workspace};
use serde_json::Value;

struct Producer {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    outputs: PathBuf,
    recorder: LocalRecorder,
    begin: BeginCapture,
    handle: CaptureHandle,
}

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).unwrap()
}

fn position(value: u64) -> Position {
    Position::new(value).unwrap()
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
        begin,
        handle,
    }
}

impl Producer {
    /// Started, a retained result, a declared-missing trace, then a crash.
    fn interrupted(&self) {
        self.recorder
            .append(&CaptureEvent::new(
                self.handle,
                key("started"),
                position(1),
                LifecycleFact::Started,
            ))
            .unwrap();
        fs::write(self.outputs.join("result.json"), b"{\"partial\":true}\n").unwrap();
        self.recorder
            .attach(&StableArtifact::file(
                self.handle,
                key("result"),
                position(2),
                ArtifactRole::OriginalOutput,
                "result.json",
            ))
            .unwrap();
        self.recorder
            .attach(&StableArtifact::missing(
                self.handle,
                key("trace"),
                position(3),
                ArtifactRole::Log,
                "the producer stopped before the trace was flushed",
            ))
            .unwrap();
    }

    fn settle(&self, last: Option<u64>) -> FinalizeCapture {
        let mut summary = FinalizeCapture::new(
            self.handle,
            key("run-0001/finalize"),
            last.map(position),
            CaptureCompletion::Interrupted,
            ExecutionReport::new(ExecutionStatus::Unknown),
        );
        summary.declared_artifacts = vec![key("result"), key("trace")];
        summary
    }

    fn recover(&self) -> HandleRecovery {
        let mut state = self
            .recorder
            .recover(&RecoverScope::Handle {
                handle: self.handle,
            })
            .unwrap();
        assert_eq!(state.handles.len(), 1);
        state.handles.remove(0)
    }

    fn unsettled(&self) -> Vec<HandleRecovery> {
        self.recorder
            .recover(&RecoverScope::Unsettled)
            .unwrap()
            .handles
    }

    fn staging(&self) -> PathBuf {
        self.root
            .join(format!("state/staging/{}", self.recover().run_id))
    }

    fn run(&self, recovered: &HandleRecovery) -> Value {
        let mut store = Store::open(Workspace::open(&self.root).unwrap());
        let case = store.case(recovered.case_id).unwrap();
        let run = case.state.current(recovered.run_id).unwrap();
        serde_json::to_value(run).unwrap()["body"].clone()
    }
}

/// Every path beneath `root` with its bytes, minus the disposable index.
fn tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_owned();
            if relative.starts_with("cache") {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
                entries.push((relative, Vec::new()));
            } else {
                entries.push((relative, fs::read(&path).unwrap()));
            }
        }
    }
    entries.sort();
    entries
}

fn actions(recovered: &HandleRecovery) -> Vec<RecoveryAction> {
    recovered.next.iter().map(|step| step.action).collect()
}

#[test]
fn an_interrupted_producer_recovers_partial_outputs_with_unknown_execution_and_no_rerun() {
    let producer = producer();
    producer.interrupted();
    let before = tree(&producer.root);

    // A restarted producer lists what it left unsettled.
    let unsettled = producer.unsettled();
    assert_eq!(tree(&producer.root), before, "recover writes nothing");
    assert_eq!(unsettled.len(), 1);
    let recovered = &unsettled[0];
    assert_eq!(recovered, &producer.recover());
    assert_eq!(recovered.handle, producer.handle);
    assert_eq!(recovered.producer, producer.begin.producer);
    assert_eq!(recovered.external_run_id, producer.begin.external_run_id);
    assert_eq!(recovered.seal, SealState::Open);
    assert_eq!(recovered.staging, Some(StagingState::Present));
    assert!(recovered.staging_dir.is_some());
    // The external effect is unknown: no record of an outcome is not "did not happen".
    assert_eq!(recovered.execution, ExecutionStatus::Unknown);
    assert_eq!(recovered.last_position, Some(position(3)));
    assert_eq!(recovered.next_position, Some(position(4)));
    assert!(recovered.finalized.is_none());

    // The original receipts of what was accepted, in position order.
    assert_eq!(recovered.items.len(), 1);
    assert_eq!(recovered.items[0].key, key("started"));
    assert_eq!(recovered.items[0].result, ReceiptResult::AlreadyAccepted);
    let availability: Vec<_> = recovered
        .artifacts
        .iter()
        .map(|artifact| (artifact.receipt.position.get(), artifact.availability))
        .collect();
    assert_eq!(
        availability,
        [
            (2, StagedAvailability::Staged),
            (3, StagedAvailability::Missing)
        ]
    );
    // Resending an accepted item returns the same receipt recover reported.
    let replay = producer
        .recorder
        .append(&CaptureEvent::new(
            producer.handle,
            key("started"),
            position(1),
            LifecycleFact::Started,
        ))
        .unwrap();
    assert_eq!(replay, recovered.items[0]);

    // The next steps only record. None reruns or resumes the producer's action.
    assert_eq!(
        actions(recovered),
        [
            RecoveryAction::Append,
            RecoveryAction::Attach,
            RecoveryAction::Finalize
        ]
    );
    for step in &recovered.next {
        assert_eq!(step.argv[1], "capture");
        assert!(["append", "attach", "finalize"].contains(&step.argv[2].as_str()));
    }

    // The producer cannot vouch for what happened after the crash, so it
    // settles the handle as interrupted with unknown execution.
    let receipt = producer
        .recorder
        .finalize(&producer.settle(Some(3)))
        .unwrap();
    assert_eq!(receipt.finalization, Finalization::FinalizedPartial);
    assert_eq!(receipt.execution, ExecutionStatus::Unknown);
    let recovered = producer.recover();
    let run = producer.run(&recovered);
    assert_eq!(run["execution_status"], "unknown");
    assert_eq!(
        run["evidence_completeness"],
        serde_json::to_value(EvidenceCompleteness::Partial).unwrap()
    );
    // Staged bytes survive recovery and finalization.
    let blobs = producer.staging().join("blobs");
    assert_eq!(fs::read_dir(blobs).unwrap().count(), 1);
    assert!(producer.unsettled().is_empty());
}

#[test]
fn recover_after_a_lost_finalize_acknowledgement_reports_the_original_receipt() {
    let producer = producer();
    producer.interrupted();
    // The receipt is lost on the way back to the producer.
    let lost = producer
        .recorder
        .finalize(&producer.settle(Some(3)))
        .unwrap();

    let recovered = producer.recover();
    assert_eq!(recovered.seal, SealState::Finalized);
    assert_eq!(recovered.execution, ExecutionStatus::Unknown);
    assert_eq!(recovered.last_position, Some(position(3)));
    assert_eq!(recovered.next_position, None);
    let original = recovered.finalized.clone().expect("finalized");
    assert_eq!(original.commit.transaction_id, lost.commit.transaction_id);
    assert_eq!(original.commit.created, lost.commit.created);
    assert_eq!(original.manifest_digest, lost.manifest_digest);
    // Recover returns what an identical retry returns.
    let retry = producer
        .recorder
        .finalize(&producer.settle(Some(3)))
        .unwrap();
    assert_eq!(original, retry);
    assert_eq!(actions(&recovered), [RecoveryAction::LookupReceipt]);
    assert_eq!(
        recovered.next[0].argv[1..3],
        ["receipt".to_owned(), "run-0001/finalize".to_owned()]
    );

    // After staging is retired, the journal still answers.
    let staging = producer.staging();
    fs::remove_dir_all(&staging).unwrap();
    let retired = producer.recover();
    assert_eq!(retired.staging, Some(StagingState::Retired));
    assert_eq!(retired.staging_dir, None);
    assert_eq!(retired.seal, SealState::Finalized);
    assert_eq!(retired.finalized, Some(original));
    assert_eq!(retired.last_position, Some(position(3)));
}

#[test]
fn a_sealed_handle_names_the_finalize_request_that_completes_it() {
    let producer = producer();
    producer.interrupted();
    let staging = producer.staging();
    // Make the canonical blob store refuse new shards, so finalize seals the
    // handle and then fails to commit: the state a crash between the two leaves.
    let blobs = producer.root.join("state/blobs/sha256");
    let writable = fs::metadata(&blobs).unwrap().permissions();
    fs::set_permissions(&blobs, fs::Permissions::from_mode(0o500)).unwrap();
    if fs::create_dir(blobs.join("probe")).is_ok() {
        // Permissions are not enforced for this user; nothing to simulate.
        fs::remove_dir(blobs.join("probe")).unwrap();
        fs::set_permissions(&blobs, writable).unwrap();
        return;
    }
    let summary = producer.settle(Some(3));
    let failed = producer.recorder.finalize(&summary);
    fs::set_permissions(&blobs, writable).unwrap();
    assert!(failed.is_err());
    assert!(staging.join("sealed.json").exists());

    let before = tree(&producer.root);
    let recovered = producer.recover();
    assert_eq!(tree(&producer.root), before, "recover writes nothing");
    assert_eq!(recovered.seal, SealState::Sealed);
    assert_eq!(recovered.execution, ExecutionStatus::Unknown);
    assert_eq!(recovered.next_position, None);
    assert_eq!(recovered.artifacts.len(), 2);
    assert_eq!(producer.unsettled(), std::slice::from_ref(&recovered));
    assert_eq!(actions(&recovered), [RecoveryAction::ResendFinalize]);
    assert_eq!(recovered.next[0].key, Some(key("run-0001/finalize")));
    assert!(recovered.next[0].request_digest.is_some());

    // A different request is refused and points at recover.
    let mut other = producer.settle(Some(3));
    other.key = key("another");
    let error = producer.recorder.finalize(&other).unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::RevisionConflict));
    let argv = error
        .failure()
        .unwrap()
        .error
        .repair
        .clone()
        .unwrap()
        .argv
        .unwrap();
    assert_eq!(
        argv[1..5],
        [
            "capture",
            "recover",
            "--handle",
            &producer.handle.to_string()
        ]
    );

    // The identical request completes it.
    let receipt = producer.recorder.finalize(&summary).unwrap();
    assert_eq!(receipt.execution, ExecutionStatus::Unknown);
    assert_eq!(producer.recover().seal, SealState::Finalized);
    assert!(producer.unsettled().is_empty());
}

#[test]
fn a_declared_handle_without_staging_is_resumed_by_the_identical_begin() {
    let producer = producer();
    let staging = producer.staging();
    // The state a crash between the declaration and the allocation leaves.
    fs::remove_dir_all(&staging).unwrap();
    let recovered = producer.recover();
    assert_eq!(recovered.seal, SealState::Open);
    assert_eq!(recovered.staging, Some(StagingState::Unallocated));
    assert_eq!(recovered.staging_dir, None);
    assert_eq!(recovered.last_position, None);
    assert_eq!(recovered.next_position, None);
    assert_eq!(actions(&recovered), [RecoveryAction::ResendBegin]);
    assert_eq!(recovered.next[0].key, Some(key("run-0001/begin")));
    assert!(!staging.exists(), "recover allocates nothing");

    producer.recorder.begin(&producer.begin).unwrap();
    let resumed = producer.recover();
    assert_eq!(resumed.staging, Some(StagingState::Present));
    assert_eq!(resumed.next_position, Some(Position::FIRST));
}

#[test]
fn refused_calls_point_at_recover_and_unknown_handles_are_refused() {
    let producer = producer();
    producer.interrupted();
    let gap = producer
        .recorder
        .append(&CaptureEvent::new(
            producer.handle,
            key("late"),
            position(7),
            LifecycleFact::Started,
        ))
        .unwrap_err();
    assert_eq!(gap.code(), Some(ErrorCode::RevisionConflict));
    let argv = gap
        .failure()
        .unwrap()
        .error
        .repair
        .clone()
        .unwrap()
        .argv
        .unwrap();
    assert_eq!(argv[1..3], ["capture", "recover"]);

    let mut stale = producer.settle(Some(2));
    stale.completion = CaptureCompletion::Complete;
    let error = producer.recorder.finalize(&stale).unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::RevisionConflict));
    let argv = error
        .failure()
        .unwrap()
        .error
        .repair
        .clone()
        .unwrap()
        .argv
        .unwrap();
    assert_eq!(argv[1..3], ["capture", "recover"]);

    let unknown: CaptureHandle = "handle_01936f0e-7c6a-7d3e-8a1b-0123456789ab"
        .parse()
        .unwrap();
    let error = producer
        .recorder
        .recover(&RecoverScope::Handle { handle: unknown })
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::UnknownReference));
    assert_eq!(error.commit_status(), CommitStatus::NotCommitted);
}
