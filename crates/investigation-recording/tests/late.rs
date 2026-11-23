//! Late items after sealing, and receipts after staging retirement, through the
//! public API. A finalized manifest never changes; a late item is refused with
//! a closed-handle response and preserved as a new attributed record linked to
//! the Run; an identical retry of an accepted item still gets its receipt.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
};

use investigation_core::identity::Kind;
use investigation_recording::{
    ArtifactRef, ArtifactRole, BeginCapture, CaptureCompletion, CaptureEvent, CaptureHandle,
    CaptureIntent, CaseTarget, ErrorCode, ExecutionReport, ExecutionStatus, ExternalRunId,
    FinalizeCapture, FinalizeReceipt, HandleRecovery, IdempotencyKey, LifecycleFact, LocalRecorder,
    Position, ProducerIdentity, ProducerName, ReceiptResult, Recorder, RecordingError,
    RecoverScope, StableArtifact, StagingState,
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

struct Producer {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    outputs: PathBuf,
    recorder: LocalRecorder,
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
    fs::write(outputs.join("result.json"), b"{\"metric\":1}\n").unwrap();
    fs::write(outputs.join("trace.log"), b"late trace\n").unwrap();
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
    fn started(&self) -> CaptureEvent {
        CaptureEvent::new(
            self.handle,
            key("started"),
            position(1),
            LifecycleFact::Started,
        )
    }

    fn result(&self) -> StableArtifact {
        StableArtifact::file(
            self.handle,
            key("result"),
            position(2),
            ArtifactRole::OriginalOutput,
            "result.json",
        )
    }

    fn trace(&self, at: u64) -> StableArtifact {
        StableArtifact::file(
            self.handle,
            key("trace"),
            position(at),
            ArtifactRole::Log,
            "trace.log",
        )
    }

    fn finalize(&self, completion: CaptureCompletion, last: u64) -> FinalizeCapture {
        FinalizeCapture::new(
            self.handle,
            key("run-0001/finalize"),
            Some(position(last)),
            completion,
            ExecutionReport::new(ExecutionStatus::Completed),
        )
    }

    fn recover(&self) -> HandleRecovery {
        self.recorder
            .recover(&RecoverScope::Handle {
                handle: self.handle,
            })
            .unwrap()
            .handles
            .remove(0)
    }

    fn staging(&self) -> PathBuf {
        self.root
            .join(format!("state/staging/{}", self.recover().run_id))
    }

    fn store(&self) -> Store {
        Store::open(Workspace::open(&self.root).unwrap())
    }

    fn record(&self, id: investigation_recording::Id) -> Value {
        let receipt = self.recover();
        let case = self.store().case(receipt.case_id).unwrap();
        serde_json::to_value(case.state.current(id).unwrap()).unwrap()
    }

    fn manifest_bytes(&self, digest: &str) -> Vec<u8> {
        let hex = digest.strip_prefix("sha256:").unwrap();
        fs::read(
            self.root
                .join("state/blobs/sha256")
                .join(&hex[..2])
                .join(hex),
        )
        .unwrap()
    }
}

fn closed(error: &RecordingError) -> Value {
    let failure = error.failure().unwrap();
    assert_eq!(failure.error.code, ErrorCode::RevisionConflict);
    assert_eq!(failure.error.path.as_deref(), Some("/handle"));
    failure.error.expected.clone().unwrap()
}

/// Every manifest item key, in position order.
fn manifest_keys(bytes: &[u8]) -> Vec<String> {
    let manifest: Value = serde_json::from_slice(bytes).unwrap();
    manifest["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["key"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn racing_the_last_attachment_with_sealing_includes_it_or_refuses_it_never_loses_it() {
    for round in 0..12 {
        let producer = producer();
        producer.recorder.append(&producer.started()).unwrap();
        producer.recorder.attach(&producer.result()).unwrap();
        // Alternate the producer's claim: `interrupted` accepts any cursor, so
        // a finalize that loses still seals; `complete` refuses a stale one.
        let completion = if round % 2 == 0 {
            CaptureCompletion::Interrupted
        } else {
            CaptureCompletion::Complete
        };
        let barrier = Arc::new(Barrier::new(2));
        let attaching = {
            let (recorder, barrier, trace) = (
                producer.recorder.clone(),
                Arc::clone(&barrier),
                producer.trace(3),
            );
            thread::spawn(move || {
                barrier.wait();
                recorder.attach(&trace)
            })
        };
        let finalizing = {
            let (recorder, barrier, summary) = (
                producer.recorder.clone(),
                Arc::clone(&barrier),
                producer.finalize(completion, 2),
            );
            thread::spawn(move || {
                barrier.wait();
                recorder.finalize(&summary)
            })
        };
        let attached = attaching.join().unwrap();
        let finalized: FinalizeReceipt = match finalizing.join().unwrap() {
            Ok(receipt) => receipt,
            Err(stale) => {
                // Only a stale `complete` cursor loses this way, and it sealed
                // nothing: the attachment was accepted first.
                assert_eq!(completion, CaptureCompletion::Complete);
                assert_eq!(stale.code(), Some(ErrorCode::RevisionConflict));
                assert!(attached.is_ok());
                producer
                    .recorder
                    .finalize(&producer.finalize(completion, 3))
                    .unwrap()
            }
        };
        let keys = manifest_keys(&producer.manifest_bytes(finalized.manifest_digest.as_str()));
        match attached {
            // Acknowledged: the manifest includes it at its position.
            Ok(receipt) => {
                assert_eq!(receipt.receipt.position, position(3));
                assert_eq!(finalized.sealed_position, Some(position(3)));
                assert_eq!(keys, ["started", "result", "trace"]);
            }
            // Refused with the closed-handle response naming this manifest.
            Err(error) => {
                let expected = closed(&error);
                assert_eq!(
                    expected["manifest_digest"],
                    json!(finalized.manifest_digest)
                );
                assert_eq!(finalized.sealed_position, Some(position(2)));
                assert_eq!(keys, ["started", "result"]);
            }
        }
    }
}

#[test]
fn after_staging_is_retired_an_identical_retry_recovers_its_receipt_and_a_late_item_changes_nothing()
 {
    let producer = producer();
    let started = producer.recorder.append(&producer.started()).unwrap();
    let result: ArtifactRef = producer.recorder.attach(&producer.result()).unwrap();
    let finalized = producer
        .recorder
        .finalize(&producer.finalize(CaptureCompletion::Complete, 2))
        .unwrap();
    let manifest = producer.manifest_bytes(finalized.manifest_digest.as_str());
    let run_before = producer.record(finalized.run_id);

    // Staging retirement (W07.09) removes the whole directory.
    let staging = producer.staging();
    fs::remove_dir_all(&staging).unwrap();

    let replayed = producer.recorder.append(&producer.started()).unwrap();
    assert_eq!(replayed.result, ReceiptResult::AlreadyAccepted);
    assert_eq!(replayed.position, started.position);
    assert_eq!(replayed.request_digest, started.request_digest);
    assert_eq!(replayed.accepted_at, started.accepted_at);
    let replayed = producer.recorder.attach(&producer.result()).unwrap();
    assert_eq!(replayed.receipt.result, ReceiptResult::AlreadyAccepted);
    assert_eq!(replayed.receipt.accepted_at, result.receipt.accepted_at);
    assert_eq!(replayed.digest, result.digest);

    // Recover reads the same receipts from the manifest.
    let recovered = producer.recover();
    assert_eq!(recovered.staging, Some(StagingState::Retired));
    assert_eq!(recovered.items.len(), 1);
    assert_eq!(recovered.items[0].accepted_at, started.accepted_at);
    assert_eq!(recovered.artifacts.len(), 1);
    assert_eq!(recovered.artifacts[0].digest, result.digest);

    // A late item is refused, whatever position it claims.
    for at in [1, 3] {
        let error = producer.recorder.attach(&producer.trace(at)).unwrap_err();
        let expected = closed(&error);
        assert_eq!(expected["seal"], "finalized");
        assert_eq!(
            expected["manifest_digest"],
            json!(finalized.manifest_digest)
        );
    }
    // The manifest, the Run and staging are as they were.
    assert_eq!(
        producer.manifest_bytes(finalized.manifest_digest.as_str()),
        manifest
    );
    assert_eq!(producer.record(finalized.run_id), run_before);
    assert!(!staging.exists());
    let retry = producer
        .recorder
        .finalize(&producer.finalize(CaptureCompletion::Complete, 2))
        .unwrap();
    assert_eq!(retry.manifest_digest, finalized.manifest_digest);
}

#[test]
fn late_evidence_is_preserved_as_a_new_attributed_record_linked_to_the_run() {
    let producer = producer();
    producer.recorder.append(&producer.started()).unwrap();
    producer.recorder.attach(&producer.result()).unwrap();
    let finalized = producer
        .recorder
        .finalize(&producer.finalize(CaptureCompletion::Complete, 2))
        .unwrap();
    let late = producer.recorder.attach(&producer.trace(3)).unwrap_err();
    let expected = closed(&late);
    assert_eq!(expected["run_id"], json!(finalized.run_id));
    assert_eq!(expected["case_id"], json!(finalized.case_id));
    let before = producer.record(finalized.run_id)["body"].clone();
    assert_eq!(before["evidence_completeness"], "complete");

    // 1. Ingest the late bytes into the Run's case: a new Source with its own
    //    acquisition time, attributed to whoever submits it.
    let manifest = json!({
        "schema_version": "inv.ingest.v1",
        "command_id": "run-0001/late-trace",
        "actor": "agent:example-producer",
        "case_id": finalized.case_id,
        "source": {"title": "Trace flushed after run-0001 was finalized"},
        "paths": [producer.outputs.join("trace.log")],
    });
    let mut store = producer.store();
    let ingested = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap();
    let created = |kind| {
        ingested
            .receipt
            .created
            .iter()
            .find(|change| change.id.kind() == kind)
            .unwrap()
            .id
    };
    let (source, artifact) = (created(Kind::Source), created(Kind::Artifact));

    // 2. Link it to the Run with run.reconcile. The outcome stays as recorded.
    let run = producer.record(finalized.run_id);
    let mut outcome = run["body"].clone();
    for meta in [
        "capture",
        "capture_finalization",
        "chronology",
        "declared_at_sequence",
        "external_ref",
        "evidence_validity",
        "ordering_receipt",
    ] {
        outcome.as_object_mut().unwrap().remove(meta);
    }
    outcome["artifact_refs"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": artifact, "revision": 1}));
    let reconcile = |command_id: &str, outcome: &Value| {
        json!({
            "schema_version": "inv.command.v1", "command_id": command_id,
            "actor": "agent:example-producer", "case_id": finalized.case_id,
            "preconditions": [],
            "operations": [{"op": "run.reconcile", "payload": {
                "run_ref": {"id": finalized.run_id, "revision": run["header"]["revision"]},
                "outcome": outcome,
                "rationale": "The trace arrived after the capture was finalized."
            }}],
        })
    };
    // Rewriting the manifest digest is refused.
    let mut rewritten = outcome.clone();
    rewritten["manifest_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
    let refused = store
        .record(&serde_json::to_vec(&reconcile("rewrite", &rewritten)).unwrap())
        .unwrap_err();
    assert!(
        refused
            .error
            .path
            .as_deref()
            .is_some_and(|path| path.ends_with("/outcome/manifest_digest")),
        "{refused:?}"
    );
    store
        .record(&serde_json::to_vec(&reconcile("link-late-trace", &outcome)).unwrap())
        .unwrap();

    let linked = producer.record(finalized.run_id)["body"].clone();
    let references = linked["artifact_refs"].as_array().unwrap();
    assert!(references.contains(&json!({"id": artifact, "revision": 1})));
    assert_eq!(
        linked["reconciliation_reason"],
        "The trace arrived after the capture was finalized."
    );
    // The frozen capture is untouched: same manifest, section and completeness.
    for field in [
        "manifest_digest",
        "capture_finalization",
        "evidence_completeness",
        "execution_status",
    ] {
        assert_eq!(linked[field], before[field], "{field}");
    }
    assert_eq!(linked["manifest_digest"], json!(finalized.manifest_digest));
    // The late Source is its own acquisition, distinct from the capture Source.
    let capture_source = &before["capture_finalization"]["manifest"]["source_ref"]["id"];
    assert_ne!(&json!(source), capture_source);
    let late_source = producer.record(source);
    assert!(
        late_source["body"]["acquisition"]["acquired_at"]["timestamp"]["state"] == "known",
        "{late_source}"
    );
    assert_eq!(
        late_source["header"]["created_by"],
        "agent:example-producer"
    );
    assert!(Path::new(&producer.staging()).exists());
}
