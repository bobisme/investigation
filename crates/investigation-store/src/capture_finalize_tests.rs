//! Finalization tests. A child of `capture::finalize`, so the crash schedules
//! drive the same private seal, copy and commit path as production.
use std::{fs, os::unix::fs::PermissionsExt};

use investigation_core::wire::CommitResult;

use super::*;
use crate::{
    durable::MemoryFs,
    store::capture::{
        attach::{AttachSource, AttachSubmission},
        items::{
            ItemAcceptance, ItemOperation, ItemSubmission,
            tests::{Ticks, code_and_path, declaration, memory_store, restart},
        },
    },
};

const KEY: &str = "run-0001/finalize";

fn with_imports(root: &Path, imports: &Path) -> Workspace {
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        contents.replace(
            "import_roots = []",
            &format!("import_roots = [{:?}]", imports.display().to_string()),
        ),
    )
    .unwrap();
    Workspace::open(root).unwrap()
}

fn fact(handle: Id, key: &str, position: u64) -> ItemSubmission {
    ItemSubmission {
        operation: ItemOperation::Append,
        handle,
        key: key.into(),
        position,
        request: json!({"handle": handle, "key": key, "position": position, "fact": {"kind": "started"}}),
        outcome: None,
    }
}

fn attachment(
    handle: Id,
    key: &str,
    position: u64,
    source: AttachSource,
    derived_from: Option<&str>,
) -> AttachSubmission {
    AttachSubmission {
        item: ItemSubmission {
            operation: ItemOperation::Attach,
            handle,
            key: key.into(),
            position,
            request: json!({"handle": handle, "key": key, "position": position, "source": format!("{source:?}")}),
            outcome: None,
        },
        source,
        expected_digest: None,
        expected_size_bytes: None,
        classification: None,
        derived_from: derived_from.map(str::to_owned),
    }
}

fn file(handle: Id, key: &str, position: u64, path: &str) -> AttachSubmission {
    attachment(handle, key, position, AttachSource::File(path.into()), None)
}

fn submission(handle: Id, request: Value) -> FinalizeSubmission {
    FinalizeSubmission {
        handle,
        command_id: KEY.into(),
        request,
    }
}

/// A recorder-shaped plan: every retained attachment becomes an Artifact, and
/// the manifest binds the item map and the sealed snapshot.
fn plan(context: &FinalizeContext<'_>) -> Result<FinalizePlan, Failure> {
    let mut artifacts = Vec::new();
    for item in context.items {
        let Some(outcome) = AttachOutcome::of(item)? else {
            continue;
        };
        if outcome.availability != AttachAvailability::Staged {
            continue;
        }
        artifacts.push(PlannedArtifact {
            position: item.position,
            role: match outcome.derived_from {
                Some(original) => PlannedRole::Derivative {
                    from: original.position,
                    transformation: "extract".into(),
                    version: "1".into(),
                },
                None => PlannedRole::Original,
            },
            media_type: Some("application/octet-stream".into()),
        });
    }
    let manifest = serde_json::to_vec(&json!({
        "run_id": context.run_id,
        "snapshot": context.snapshot,
        "items": crate::store::capture::late::item_map(context.items),
    }))
    .unwrap();
    Ok(FinalizePlan {
        manifest,
        artifacts,
        execution_status: ExecutionStatus::Failed,
        evidence_completeness: EvidenceCompleteness::Partial,
        started_at: None,
        finished_at: None,
        limitations: vec!["synthetic".into()],
        metadata: BTreeMap::new(),
        warnings: Vec::new(),
        receipts: Vec::new(),
        section: Box::new(|refs| {
            Ok(json!({
                "manifest": refs.manifest,
                "manifest_digest": refs.manifest_digest,
                "artifacts": refs.artifacts,
            }))
        }),
    })
}

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    imports: PathBuf,
    store: Store,
    declared: DeclaredCapture,
}

fn fixture() -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let imports = temporary.path().join("outputs");
    fs::create_dir(&imports).unwrap();
    Workspace::init(&root, false).unwrap();
    let mut store = Store::open(with_imports(&root, &imports));
    let declared = store.declare_capture(&declaration()).unwrap();
    Fixture {
        _temporary: temporary,
        root,
        imports,
        store,
        declared,
    }
}

impl Fixture {
    fn handle(&self) -> Id {
        self.declared.binding.handle
    }

    fn staging(&self) -> PathBuf {
        self.root.join(&self.declared.binding.staging)
    }

    fn attach(&mut self, submission: &AttachSubmission) -> ItemAcceptance {
        self.store
            .stage_capture_attachment(submission, &mut Ticks(0))
            .unwrap()
    }

    /// An original, its derivative, a missing artifact and a fact.
    fn stage_items(&mut self) {
        let handle = self.handle();
        fs::write(self.imports.join("result.bin"), b"original bytes").unwrap();
        fs::write(self.imports.join("extract.txt"), b"extracted").unwrap();
        self.store
            .stage_capture_item(&fact(handle, "started", 1), &mut Ticks(0))
            .unwrap();
        self.attach(&file(handle, "result", 2, "result.bin"));
        self.attach(&attachment(
            handle,
            "extract",
            3,
            AttachSource::File("extract.txt".into()),
            Some("result"),
        ));
        self.attach(&attachment(handle, "trace", 4, AttachSource::Missing, None));
    }

    fn finalize(&mut self, request: Value) -> Result<FinalizedCapture, Failure> {
        let handle = self.handle();
        self.store
            .finalize_capture(&submission(handle, request), &mut Ticks(0), plan)
    }
}

fn request() -> Value {
    json!({"key": KEY, "completion": "partial"})
}

#[test]
fn finalize_seals_staging_and_commits_one_transaction() {
    let mut fixture = fixture();
    fixture.stage_items();
    let finalized = fixture.finalize(request()).unwrap();
    assert_eq!(finalized.receipt.result, CommitResult::Committed);
    assert_eq!(finalized.run_id, fixture.declared.run_id);
    assert_eq!(finalized.case_id, fixture.declared.case_id);

    // One transaction after the declaration, journaled as capture.finalize
    // with the manifest digest bound to the event.
    let case = fixture.store.case(fixture.declared.case_id).unwrap();
    assert_eq!(case.events.len(), 2);
    let event = &case.events[1].event;
    assert_eq!(event.request.input["operation"], FINALIZE_OPERATION);
    assert_eq!(event.request.input["payload"], request());
    assert_eq!(event.request.attachments.len(), 2);
    let manifest_digest = event.manifest_digest.clone().unwrap();
    assert_eq!(finalized.section["manifest_digest"], json!(manifest_digest));

    // A Source, two retained Artifacts, the manifest Artifact and the next
    // revision of the Run; never an Observation, Assessment or Conclusion.
    let kinds: Vec<Kind> = event
        .accepted
        .records
        .iter()
        .map(|record| record.header.id.kind())
        .collect();
    assert_eq!(
        kinds,
        [
            Kind::Source,
            Kind::Artifact,
            Kind::Artifact,
            Kind::Artifact,
            Kind::Run
        ]
    );
    let records = &event.accepted.records;
    let original: ArtifactBody = investigation_core::input::decode(&records[1].body).unwrap();
    let derivative: ArtifactBody = investigation_core::input::decode(&records[2].body).unwrap();
    let manifest: ArtifactBody = investigation_core::input::decode(&records[3].body).unwrap();
    assert_eq!(original.role, Some(ArtifactRole::Original));
    assert_eq!(derivative.role, Some(ArtifactRole::Derivative));
    assert_eq!(derivative.transforms[0].input, records[1].reference());
    assert_eq!(derivative.transforms[0].input_digest, original.digest);
    assert_eq!(manifest.role, Some(ArtifactRole::Manifest));
    assert_eq!(manifest.digest.as_ref(), Some(&manifest_digest));
    let run = &records[4];
    assert_eq!(run.header.id, fixture.declared.run_id);
    assert_eq!(run.header.revision.0, 2);
    assert_eq!(run.body["execution_status"], "failed");
    assert_eq!(run.body["evidence_completeness"], "partial");
    assert_eq!(run.body["evidence_validity"], "unknown");
    assert_eq!(run.body["manifest_digest"], json!(manifest_digest));
    assert_eq!(run.body["artifact_refs"].as_array().unwrap().len(), 3);
    assert_eq!(
        run.body["capture"],
        case.events[0].event.accepted.records[1].body["capture"]
    );
    assert_eq!(run.body[CAPTURE_FINALIZATION_KEY], finalized.section);

    // Bytes are in the canonical store; staging keeps its own copies.
    for digest in event.request.attachments.iter().chain([&manifest_digest]) {
        let hex = digest.as_str().strip_prefix("sha256:").unwrap();
        assert!(
            fixture
                .root
                .join("state/blobs/sha256")
                .join(&hex[..2])
                .join(hex)
                .is_file()
        );
    }
    assert_eq!(
        fs::read_dir(fixture.staging().join(BLOBS_DIRECTORY))
            .unwrap()
            .count(),
        2
    );
    let report = fixture.store.check().unwrap();
    assert!(report.integrity_ok(), "{:?}", report.issues);

    // The seal closes the handle.
    let seal = fixture.staging().join(SEAL_FILE);
    assert_eq!(
        fs::metadata(&seal).unwrap().permissions().mode() & 0o777,
        0o400
    );
    let sealed: SealFile = serde_json::from_slice(&fs::read(&seal).unwrap()).unwrap();
    assert_eq!(sealed.command_id, KEY);
    assert_eq!(sealed.sealed_position, Some(4));
    assert_eq!(sealed.manifest_digest, manifest_digest);
    let handle = fixture.handle();
    let late = fixture
        .store
        .stage_capture_item(&fact(handle, "late", 5), &mut Ticks(0))
        .unwrap_err();
    assert_eq!(
        code_and_path(&late),
        (ErrorCode::RevisionConflict, Some("/handle"))
    );

    // An identical retry returns the original receipt and records nothing.
    let retried = fixture.finalize(request()).unwrap();
    assert_eq!(retried.receipt.result, CommitResult::AlreadyCommitted);
    assert_eq!(
        retried.receipt.transaction_id,
        finalized.receipt.transaction_id
    );
    assert_eq!(retried.section, finalized.section);
    assert_eq!(
        fixture
            .store
            .case(fixture.declared.case_id)
            .unwrap()
            .events
            .len(),
        2
    );
}

#[test]
fn the_seal_snapshot_is_the_case_head_before_finalization() {
    let mut fixture = fixture();
    fixture.stage_items();
    let before = fixture
        .store
        .case(fixture.declared.case_id)
        .unwrap()
        .snapshot;
    fixture.finalize(request()).unwrap();
    let seal: SealFile =
        serde_json::from_slice(&fs::read(fixture.staging().join(SEAL_FILE)).unwrap()).unwrap();
    assert_eq!(seal.snapshot, before);
}

#[test]
fn conflicting_finalizations_are_refused_with_the_original() {
    let mut fixture = fixture();
    fixture.stage_items();
    let handle = fixture.handle();
    // A key that already names another transaction is refused before sealing.
    let reused = fixture
        .store
        .finalize_capture(
            &FinalizeSubmission {
                handle,
                command_id: "run-0001/begin".into(),
                request: request(),
            },
            &mut Ticks(0),
            plan,
        )
        .unwrap_err();
    assert_eq!(
        code_and_path(&reused),
        (ErrorCode::IdempotencyKeyReused, Some("/key"))
    );
    assert!(!fixture.staging().join(SEAL_FILE).exists());

    fixture.finalize(request()).unwrap();
    let changed = fixture
        .finalize(json!({"key": KEY, "completion": "complete"}))
        .unwrap_err();
    assert_eq!(
        code_and_path(&changed),
        (ErrorCode::IdempotencyKeyReused, Some("/key"))
    );
    let other = fixture
        .store
        .finalize_capture(
            &FinalizeSubmission {
                handle,
                command_id: "another".into(),
                request: json!({"key": "another"}),
            },
            &mut Ticks(0),
            plan,
        )
        .unwrap_err();
    assert_eq!(
        code_and_path(&other),
        (ErrorCode::RevisionConflict, Some("/handle"))
    );
    assert_eq!(other.error.expected.as_ref().unwrap()["command_id"], KEY);
    let argv = other
        .error
        .repair
        .as_ref()
        .and_then(|repair| repair.argv.clone())
        .unwrap();
    assert!(argv.contains(&KEY.to_owned()), "{argv:?}");
}

#[test]
fn a_plan_refusal_seals_nothing() {
    let mut fixture = fixture();
    fixture.stage_items();
    let handle = fixture.handle();
    let error = fixture
        .store
        .finalize_capture(&submission(handle, request()), &mut Ticks(0), |_| {
            Err(Failure::new(ErrorCode::RevisionConflict, "stale cursor")
                .at("/expected_last_position"))
        })
        .unwrap_err();
    assert_eq!(error.error.path.as_deref(), Some("/expected_last_position"));
    assert!(!fixture.staging().join(SEAL_FILE).exists());
    fixture
        .store
        .stage_capture_item(&fact(handle, "more", 5), &mut Ticks(0))
        .unwrap();
}

#[test]
fn a_seal_binds_its_request_until_the_identical_request_commits() {
    let mut fixture = fixture();
    fixture.stage_items();
    let handle = fixture.handle();
    // Sealed, then the transaction fails: the state a crash between the two leaves.
    let failed = fixture
        .store
        .finalize_capture(&submission(handle, request()), &mut Ticks(0), |context| {
            let mut planned = plan(context)?;
            planned.section = Box::new(|_| Err(integrity("interrupted")));
            Ok(planned)
        })
        .unwrap_err();
    assert_eq!(failed.error.message, "interrupted");
    assert!(fixture.staging().join(SEAL_FILE).exists());
    assert_eq!(
        fixture
            .store
            .case(fixture.declared.case_id)
            .unwrap()
            .events
            .len(),
        1
    );
    let late = fixture
        .store
        .stage_capture_item(&fact(handle, "late", 5), &mut Ticks(0))
        .unwrap_err();
    assert_eq!(
        code_and_path(&late),
        (ErrorCode::RevisionConflict, Some("/handle"))
    );

    let other = fixture
        .store
        .finalize_capture(
            &FinalizeSubmission {
                handle,
                command_id: "another".into(),
                request: json!({"key": "another"}),
            },
            &mut Ticks(0),
            plan,
        )
        .unwrap_err();
    assert_eq!(
        code_and_path(&other),
        (ErrorCode::RevisionConflict, Some("/handle"))
    );
    assert_eq!(other.error.expected.as_ref().unwrap()["command_id"], KEY);
    let changed = fixture
        .finalize(json!({"key": KEY, "completion": "complete"}))
        .unwrap_err();
    assert_eq!(
        code_and_path(&changed),
        (ErrorCode::IdempotencyKeyReused, Some("/key"))
    );

    let finalized = fixture.finalize(request()).unwrap();
    assert_eq!(finalized.receipt.result, CommitResult::Committed);
    assert_eq!(
        fixture
            .store
            .case(fixture.declared.case_id)
            .unwrap()
            .events
            .len(),
        2
    );
}

#[test]
fn finalization_sweeps_temporaries_and_unreferenced_staged_blobs() {
    let mut fixture = fixture();
    fixture.stage_items();
    let blobs = fixture.staging().join(BLOBS_DIRECTORY);
    let mut kept: Vec<_> = fs::read_dir(&blobs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    kept.sort();
    fs::write(blobs.join("000000000009.tmp"), b"partial copy").unwrap();
    fs::write(blobs.join("ab".repeat(32)), b"crashed attach").unwrap();
    fs::write(blobs.join("README"), b"not ours").unwrap();
    fixture.finalize(request()).unwrap();
    let mut left: Vec<_> = fs::read_dir(&blobs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    left.sort();
    kept.push("README".into());
    kept.sort();
    assert_eq!(left, kept);
}

#[test]
fn an_empty_handle_finalizes_with_only_its_manifest() {
    let mut fixture = fixture();
    let finalized = fixture.finalize(request()).unwrap();
    let case = fixture.store.case(fixture.declared.case_id).unwrap();
    let kinds: Vec<Kind> = case.events[1]
        .event
        .accepted
        .records
        .iter()
        .map(|record| record.header.id.kind())
        .collect();
    assert_eq!(kinds, [Kind::Source, Kind::Artifact, Kind::Run]);
    assert!(
        finalized.section["artifacts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_undeclared_handle_is_refused() {
    let mut fixture = fixture();
    let error = fixture
        .store
        .finalize_capture(
            &submission(
                "handle_01936f0e-7c6a-7d3e-8a1b-0123456789ab"
                    .parse()
                    .unwrap(),
                request(),
            ),
            &mut Ticks(0),
            plan,
        )
        .unwrap_err();
    assert_eq!(
        code_and_path(&error),
        (ErrorCode::UnknownReference, Some("/handle"))
    );
}

fn memory_finalize_store(imports: &Path) -> (Store<MemoryFs>, PathBuf) {
    let (mut store, root) = memory_store();
    for directory in ["state/blobs", "state/blobs/sha256"] {
        store.fs.create_dir(Path::new(directory)).unwrap();
        store
            .fs
            .sync_dir(Path::new(directory).parent().unwrap())
            .unwrap();
    }
    store.workspace = with_imports(&root, imports);
    (store, root)
}

fn finalize_events(store: &mut Store<MemoryFs>, case_id: Id) -> usize {
    store
        .case(case_id)
        .unwrap()
        .events
        .iter()
        .filter(|verified| verified.event.request.input["operation"] == FINALIZE_OPERATION)
        .count()
}

/// Inject a failure at every filesystem call of a finalization, then crash.
/// The identical retry must complete it exactly once, with every retained
/// byte in the canonical store and the handle sealed.
#[test]
fn every_crash_point_of_a_finalization_is_completed_by_the_identical_retry() {
    let imports = tempfile::tempdir().unwrap();
    fs::write(imports.path().join("result.bin"), vec![3_u8; 70 * 1024]).unwrap();
    let mut injected = 0;
    let mut acknowledged_first_try = 0;
    let mut sealed_uncommitted = 0;
    for seed in 1..=160 {
        let (mut store, root) = memory_finalize_store(imports.path());
        let declared = store.declare_capture(&declaration()).unwrap();
        let handle = declared.binding.handle;
        let staging = PathBuf::from(&declared.binding.staging);
        store
            .stage_capture_item(&fact(handle, "started", 1), &mut Ticks(0))
            .unwrap();
        store
            .stage_capture_attachment(&file(handle, "result", 2, "result.bin"), &mut Ticks(0))
            .unwrap();
        restart(&mut store);

        store.fs.fail_at(seed);
        let first = store.finalize_capture(&submission(handle, request()), &mut Ticks(0), plan);
        match &first {
            Ok(_) => acknowledged_first_try += 1,
            Err(_) => injected += 1,
        }
        restart(&mut store);
        if first.is_err()
            && store.exists(&staging.join(SEAL_FILE)).unwrap()
            && finalize_events(&mut store, declared.case_id) == 0
        {
            sealed_uncommitted += 1;
            // A sealed handle refuses new items until it is finalized.
            let late = store
                .stage_capture_item(&fact(handle, "late", 3), &mut Ticks(0))
                .unwrap_err();
            assert_eq!(late.error.code, ErrorCode::RevisionConflict, "seed {seed}");
        }
        let retried = store
            .finalize_capture(&submission(handle, request()), &mut Ticks(1000), plan)
            .unwrap_or_else(|error| panic!("seed {seed}: {error:?}"));
        if let Ok(first) = &first {
            assert_eq!(
                retried.receipt.result,
                CommitResult::AlreadyCommitted,
                "seed {seed}"
            );
            assert_eq!(
                retried.receipt.transaction_id, first.receipt.transaction_id,
                "seed {seed}"
            );
        }
        restart(&mut store);

        assert_eq!(
            finalize_events(&mut store, declared.case_id),
            1,
            "seed {seed}"
        );
        let case = store.case(declared.case_id).unwrap();
        let event = &case.events.last().unwrap().event;
        for digest in event
            .request
            .attachments
            .iter()
            .chain(event.manifest_digest.iter())
        {
            let hex = digest.as_str().strip_prefix("sha256:").unwrap();
            let mut bytes = Vec::new();
            store
                .fs
                .read(&Path::new("state/blobs/sha256").join(&hex[..2]).join(hex))
                .unwrap_or_else(|error| panic!("seed {seed}: {error}"))
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(hash_bytes(&bytes), *digest, "seed {seed}");
        }
        let again = store
            .finalize_capture(&submission(handle, request()), &mut Ticks(2000), plan)
            .unwrap();
        assert_eq!(
            again.receipt.transaction_id, retried.receipt.transaction_id,
            "seed {seed}"
        );
        assert_eq!(again.section, retried.section, "seed {seed}");
        let late = store
            .stage_capture_item(&fact(handle, "late", 3), &mut Ticks(0))
            .unwrap_err();
        assert_eq!(late.error.code, ErrorCode::RevisionConflict, "seed {seed}");
        let _ = fs::remove_dir_all(root);
    }
    assert!(injected > 20, "the schedule must reach the finalize path");
    assert!(
        sealed_uncommitted > 0,
        "the schedule must crash between seal and commit"
    );
    assert!(acknowledged_first_try > 0, "the schedule must run past it");
}

#[test]
fn an_investigator_can_reconcile_a_finalized_run_and_keep_what_finalize_froze() {
    let mut fixture = fixture();
    fixture.stage_items();
    let finalized = fixture.finalize(request()).unwrap();
    let run = fixture
        .store
        .case(fixture.declared.case_id)
        .unwrap()
        .state
        .current(fixture.declared.run_id)
        .unwrap()
        .clone();
    let mut outcome = run.body.clone();
    for key in [
        "capture",
        CAPTURE_FINALIZATION_KEY,
        "chronology",
        "declared_at_sequence",
        "external_ref",
        "evidence_validity",
        "ordering_receipt",
    ] {
        outcome.as_object_mut().unwrap().remove(key);
    }
    let command = json!({
        "schema_version": "inv.command.v1", "command_id": "reconcile",
        "actor": "agent:reviewer", "case_id": fixture.declared.case_id, "preconditions": [],
        "operations": [{"op": "run.reconcile", "payload": {
            "run_ref": run.reference(),
            "outcome": outcome,
            "evidence_validity": "invalid",
            "rationale": "The instrument was saturated."
        }}],
    });
    fixture
        .store
        .record(&serde_json::to_vec(&command).unwrap())
        .unwrap();
    let current = fixture
        .store
        .case(fixture.declared.case_id)
        .unwrap()
        .state
        .current(fixture.declared.run_id)
        .unwrap()
        .clone();
    assert_eq!(current.header.revision.0, 3);
    assert_eq!(current.body["evidence_validity"], "invalid");
    assert_eq!(current.body["execution_status"], "failed");
    assert_eq!(current.body[CAPTURE_FINALIZATION_KEY], finalized.section);
    // The finalization is still answered from the journal.
    let retried = fixture.finalize(request()).unwrap();
    assert_eq!(
        retried.receipt.transaction_id,
        finalized.receipt.transaction_id
    );
}
