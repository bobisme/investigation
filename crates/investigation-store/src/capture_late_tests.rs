//! Closed handles and retired staging, against a real local workspace.
use std::fs;

use investigation_core::explain::{EvidenceCompleteness, ExecutionStatus};

use super::*;
use crate::store::capture::{
    attach::{AttachOutcome, AttachSource, AttachSubmission},
    finalize::{FinalizeContext, FinalizePlan, FinalizeSubmission, PlannedArtifact, PlannedRole},
    items::tests::{Ticks, code_and_path, declaration},
    recover::RecoveredStaging,
    retire::RetirementResult,
};

const KEY: &str = "run-0001/finalize";

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    declared: DeclaredCapture,
}

fn fixture() -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let imports = temporary.path().join("outputs");
    fs::create_dir(&imports).unwrap();
    Workspace::init(&root, false).unwrap();
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
    fs::write(imports.join("result.bin"), b"original bytes").unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let declared = store.declare_capture(&declaration()).unwrap();
    Fixture {
        _temporary: temporary,
        root,
        store,
        declared,
    }
}

fn fact(handle: Id, key: &str, position: u64) -> ItemSubmission {
    ItemSubmission {
        operation: ItemOperation::Append,
        handle,
        key: key.into(),
        position,
        request: json!({"handle": handle, "key": key, "position": position}),
        outcome: None,
    }
}

fn attachment(handle: Id, key: &str, position: u64) -> AttachSubmission {
    AttachSubmission {
        item: ItemSubmission {
            operation: ItemOperation::Attach,
            handle,
            key: key.into(),
            position,
            request: json!({"handle": handle, "key": key, "position": position}),
            outcome: None,
        },
        source: AttachSource::File("result.bin".into()),
        expected_digest: None,
        expected_size_bytes: None,
        classification: None,
        derived_from: None,
    }
}

fn plan(context: &FinalizeContext<'_>) -> Result<FinalizePlan, Failure> {
    let mut artifacts = Vec::new();
    for item in context.items {
        if AttachOutcome::of(item)?.is_some_and(|outcome| outcome.digest.is_some()) {
            artifacts.push(PlannedArtifact {
                position: item.position,
                role: PlannedRole::Original,
                media_type: None,
            });
        }
    }
    Ok(FinalizePlan {
        manifest: serde_json::to_vec(&json!({
            "run_id": context.run_id,
            "items": item_map(context.items),
        }))
        .unwrap(),
        artifacts,
        execution_status: ExecutionStatus::Completed,
        evidence_completeness: EvidenceCompleteness::Complete,
        started_at: None,
        finished_at: None,
        limitations: Vec::new(),
        metadata: BTreeMap::new(),
        warnings: Vec::new(),
        receipts: Vec::new(),
        section: Box::new(|refs| Ok(json!({"manifest_digest": refs.manifest_digest}))),
    })
}

impl Fixture {
    fn handle(&self) -> Id {
        self.declared.binding.handle
    }

    fn staging(&self) -> PathBuf {
        self.root.join(&self.declared.binding.staging)
    }

    /// A fact and a retained attachment, as first accepted.
    fn stage_items(&mut self) -> Vec<ItemAcceptance> {
        let handle = self.handle();
        vec![
            self.store
                .stage_capture_item(&fact(handle, "started", 1), &mut Ticks(0))
                .unwrap(),
            self.store
                .stage_capture_attachment(&attachment(handle, "result", 2), &mut Ticks(10))
                .unwrap(),
        ]
    }

    fn submission(&self) -> FinalizeSubmission {
        FinalizeSubmission {
            handle: self.handle(),
            command_id: KEY.into(),
            request: json!({"key": KEY, "completion": "complete"}),
        }
    }

    fn finalize(&mut self) {
        let submission = self.submission();
        self.store
            .finalize_capture(&submission, &mut Ticks(20), plan)
            .unwrap();
    }

    fn run(&mut self) -> Value {
        self.store
            .case(self.declared.case_id)
            .unwrap()
            .state
            .current(self.declared.run_id)
            .unwrap()
            .body
            .clone()
    }

    /// Journal events and canonical blobs: what a late item must not change.
    fn canonical(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut entries = Vec::new();
        let mut pending = vec![self.root.join(CASES), self.root.join("state/blobs")];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    entries.push((path.clone(), fs::read(&path).unwrap()));
                }
            }
        }
        entries.sort();
        entries
    }
}

#[test]
fn a_new_item_on_a_sealed_handle_is_refused_with_the_manifest_the_seal_fixes() {
    let mut fixture = fixture();
    let accepted = fixture.stage_items();
    // Sealed, then the transaction fails: the state a crash between the two leaves.
    let submission = fixture.submission();
    fixture
        .store
        .finalize_capture(&submission, &mut Ticks(20), |context| {
            let mut planned = plan(context)?;
            planned.section = Box::new(|_| Err(integrity("interrupted")));
            Ok(planned)
        })
        .unwrap_err();
    let RecoveredStaging::Present {
        seal: Some(seal), ..
    } = fixture
        .store
        .capture_recovery(Some(fixture.handle()))
        .unwrap()
        .remove(0)
        .staging
    else {
        panic!("the handle is sealed");
    };

    let late = fixture
        .store
        .stage_capture_item(&fact(fixture.handle(), "late", 3), &mut Ticks(30))
        .unwrap_err();
    assert_eq!(
        code_and_path(&late),
        (ErrorCode::RevisionConflict, Some("/handle"))
    );
    let expected = late.error.expected.clone().unwrap();
    assert_eq!(expected["seal"], "sealed");
    assert_eq!(expected["command_id"], KEY);
    assert_eq!(expected["manifest_digest"], json!(seal.manifest_digest));
    assert_eq!(expected["sealed_position"], 2);
    assert!(late.error.message.contains("run.reconcile"));
    assert_eq!(
        late.error.repair.unwrap().argv.unwrap(),
        recover_argv(fixture.handle())
    );
    // An accepted item still replays.
    let retry = fixture
        .store
        .stage_capture_item(&fact(fixture.handle(), "started", 1), &mut Ticks(40))
        .unwrap();
    assert_eq!(retry.item, accepted[0].item);

    // Once committed, the closed-handle response names the manifest Artifact.
    fixture
        .store
        .finalize_capture(&submission, &mut Ticks(50), plan)
        .unwrap();
    let run = fixture.run();
    let late = fixture
        .store
        .stage_capture_item(&fact(fixture.handle(), "late", 3), &mut Ticks(60))
        .unwrap_err();
    assert_eq!(
        code_and_path(&late),
        (ErrorCode::RevisionConflict, Some("/handle"))
    );
    let expected = late.error.expected.unwrap();
    assert_eq!(expected["seal"], "finalized");
    assert_eq!(expected["manifest_digest"], run["manifest_digest"]);
    assert_eq!(expected["manifest_digest"], json!(seal.manifest_digest));
    assert_eq!(expected["manifest_ref"], run["artifact_refs"][0]);
    assert_eq!(expected["run_id"], json!(fixture.declared.run_id));
    assert_eq!(expected["case_id"], json!(fixture.declared.case_id));
}

#[test]
fn after_staging_is_retired_an_identical_retry_returns_its_original_receipt() {
    let mut fixture = fixture();
    let accepted = fixture.stage_items();
    fixture.finalize();
    let handle = fixture.handle();
    let retired = fixture.store.retire_staging(Some(handle)).unwrap();
    assert_eq!(retired.handles[0].result, RetirementResult::Retired);
    let before = fixture.canonical();

    let fact_retry = fixture
        .store
        .stage_capture_item(&fact(handle, "started", 1), &mut Ticks(100))
        .unwrap();
    assert_eq!(fact_retry.result, ItemResult::AlreadyAccepted);
    assert_eq!(fact_retry.item, accepted[0].item);
    assert_eq!(fact_retry.case_id, fixture.declared.case_id);
    let attach_retry = fixture
        .store
        .stage_capture_attachment(&attachment(handle, "result", 2), &mut Ticks(110))
        .unwrap();
    assert_eq!(attach_retry.result, ItemResult::AlreadyAccepted);
    // The original outcome, with the digest the attachment was staged under.
    assert_eq!(attach_retry.item, accepted[1].item);

    // A changed retry still conflicts with the accepted item.
    let mut changed = fact(handle, "started", 1);
    changed.request["note"] = json!("changed");
    let conflict = fixture
        .store
        .stage_capture_item(&changed, &mut Ticks(120))
        .unwrap_err();
    assert_eq!(
        code_and_path(&conflict),
        (ErrorCode::IdempotencyKeyReused, Some("/key"))
    );
    // A late item is refused with the manifest it cannot change.
    let late = fixture
        .store
        .stage_capture_attachment(&attachment(handle, "late", 3), &mut Ticks(130))
        .unwrap_err();
    assert_eq!(
        code_and_path(&late),
        (ErrorCode::RevisionConflict, Some("/handle"))
    );
    let expected = late.error.expected.unwrap();
    assert_eq!(expected["seal"], "finalized");
    assert_eq!(expected["sealed_position"], 2);
    assert_eq!(
        expected["manifest_digest"],
        fixture.run()["manifest_digest"]
    );

    // Nothing was written: no staging, no event, no blob.
    assert!(!fixture.staging().exists());
    assert_eq!(fixture.canonical(), before);

    // Recovery reads the same receipts from the manifest.
    let recovered = fixture.store.capture_recovery(Some(handle)).unwrap();
    assert_eq!(
        recovered[0].staging,
        RecoveredStaging::Retired {
            items: accepted.into_iter().map(|accepted| accepted.item).collect()
        }
    );
}

#[test]
fn an_identical_begin_after_finalization_allocates_no_staging() {
    let mut fixture = fixture();
    fixture.stage_items();
    fixture.finalize();
    fixture.store.retire_staging(None).unwrap();
    assert!(!fixture.staging().exists());
    let again = fixture.store.declare_capture(&declaration()).unwrap();
    assert_eq!(again.run_id, fixture.declared.run_id);
    assert_eq!(again.binding, fixture.declared.binding);
    assert!(!fixture.staging().exists());
    // So a new item is still refused rather than staged in a fresh directory.
    let late = fixture
        .store
        .stage_capture_item(&fact(fixture.handle(), "late", 1), &mut Ticks(10))
        .unwrap_err();
    assert_eq!(
        code_and_path(&late),
        (ErrorCode::RevisionConflict, Some("/handle"))
    );
}

#[test]
fn a_finalized_handle_whose_staging_reappears_still_answers_from_its_manifest() {
    let mut fixture = fixture();
    let accepted = fixture.stage_items();
    fixture.finalize();
    // Staging rebuilt without its items or seal, as by a stray re-allocation.
    let binding = fs::read(fixture.staging().join(BINDING_FILE)).unwrap();
    fs::remove_dir_all(fixture.staging()).unwrap();
    fs::create_dir(fixture.staging()).unwrap();
    fs::write(fixture.staging().join(BINDING_FILE), binding).unwrap();

    let handle = fixture.handle();
    let late = fixture
        .store
        .stage_capture_item(&fact(handle, "late", 1), &mut Ticks(10))
        .unwrap_err();
    assert_eq!(late.error.expected.unwrap()["seal"], "finalized");
    let retry = fixture
        .store
        .stage_capture_item(&fact(handle, "started", 1), &mut Ticks(20))
        .unwrap();
    assert_eq!(retry.item, accepted[0].item);
    assert!(
        fixture
            .store
            .capture_items(handle)
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn finalize_seals_nothing_when_the_manifest_does_not_bind_the_staged_items() {
    let mut fixture = fixture();
    fixture.stage_items();
    let submission = fixture.submission();
    let error = fixture
        .store
        .finalize_capture(&submission, &mut Ticks(20), |context| {
            let mut planned = plan(context)?;
            let mut items = item_map(context.items);
            items[1]["accepted_at"] = json!("2026-11-22T09:00:00Z");
            planned.manifest =
                serde_json::to_vec(&json!({"run_id": context.run_id, "items": items})).unwrap();
            Ok(planned)
        })
        .unwrap_err();
    assert_eq!(error.error.code, ErrorCode::IntegrityFailure);
    assert!(!fixture.staging().join(SEAL_FILE).exists());
    // The handle is still open.
    fixture
        .store
        .stage_capture_item(&fact(fixture.handle(), "more", 3), &mut Ticks(30))
        .unwrap();
}
