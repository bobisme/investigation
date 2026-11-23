//! Recovery read tests against a real local workspace.
use std::fs;

use investigation_core::{
    explain::{EvidenceCompleteness, ExecutionStatus},
    wire::CommitResult,
};

use super::*;
use crate::store::capture::{
    attach::{AttachOutcome, AttachSource, AttachSubmission},
    finalize::{FinalizeContext, FinalizePlan, FinalizeSubmission, PlannedArtifact, PlannedRole},
    items::{
        ItemOperation, ItemSubmission,
        tests::{Ticks, code_and_path, declaration},
    },
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
            "snapshot": context.snapshot,
            "items": crate::store::capture::late::item_map(context.items),
        }))
        .unwrap(),
        artifacts,
        execution_status: ExecutionStatus::Unknown,
        evidence_completeness: EvidenceCompleteness::Partial,
        started_at: None,
        finished_at: None,
        limitations: Vec::new(),
        metadata: BTreeMap::new(),
        warnings: Vec::new(),
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

    /// A fact and a retained attachment: what an interrupted producer leaves.
    fn stage_items(&mut self) {
        let handle = self.handle();
        self.store
            .stage_capture_item(&fact(handle, "started", 1), &mut Ticks(0))
            .unwrap();
        self.store
            .stage_capture_attachment(
                &AttachSubmission {
                    item: ItemSubmission {
                        operation: ItemOperation::Attach,
                        handle,
                        key: "result".into(),
                        position: 2,
                        request: json!({"handle": handle, "key": "result", "position": 2}),
                        outcome: None,
                    },
                    source: AttachSource::File("result.bin".into()),
                    expected_digest: None,
                    expected_size_bytes: None,
                    classification: None,
                    derived_from: None,
                },
                &mut Ticks(10),
            )
            .unwrap();
    }

    fn submission(&self) -> FinalizeSubmission {
        FinalizeSubmission {
            handle: self.handle(),
            command_id: KEY.into(),
            request: json!({"key": KEY, "completion": "interrupted"}),
        }
    }

    fn only(&mut self, handle: Option<Id>) -> RecoveredHandle {
        let mut recovered = self.store.capture_recovery(handle).unwrap();
        assert_eq!(recovered.len(), 1);
        recovered.remove(0)
    }

    /// Every path and its bytes beneath the workspace, minus the disposable index.
    fn tree(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut entries = Vec::new();
        let mut pending = vec![self.root.clone()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(&self.root).unwrap().to_owned();
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
}

#[test]
fn an_open_handle_reports_its_durable_prefix_and_recovery_writes_nothing() {
    let mut fixture = fixture();
    fixture.stage_items();
    let before = fixture.tree();
    let recovered = fixture.only(None);
    assert_eq!(fixture.tree(), before);
    assert_eq!(recovered.declared.binding.handle, fixture.handle());
    assert_eq!(recovered.declared.command_id, "run-0001/begin");
    assert!(recovered.finalized.is_none());
    let RecoveredStaging::Present { items, seal } = &recovered.staging else {
        panic!("staging is present: {:?}", recovered.staging);
    };
    assert!(seal.is_none());
    let positions: Vec<_> = items
        .iter()
        .map(|item| (item.position, item.key.as_str()))
        .collect();
    assert_eq!(positions, [(1, "started"), (2, "result")]);
    let outcome = AttachOutcome::of(&items[1]).unwrap().unwrap();
    assert!(outcome.digest.is_some());
    // The same report for the handle itself.
    assert_eq!(fixture.only(Some(fixture.handle())), recovered);
}

#[test]
fn a_handle_whose_staging_was_never_allocated_is_unsettled() {
    let mut fixture = fixture();
    fs::remove_dir_all(fixture.staging()).unwrap();
    let recovered = fixture.only(None);
    assert_eq!(recovered.staging, RecoveredStaging::Unallocated);
    assert!(recovered.finalized.is_none());
    assert!(!fixture.staging().exists());
}

#[test]
fn a_sealed_handle_names_its_sealing_request_until_the_identical_request_commits() {
    let mut fixture = fixture();
    fixture.stage_items();
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
    let before = fixture.tree();
    let recovered = fixture.only(None);
    assert_eq!(fixture.tree(), before);
    let RecoveredStaging::Present { items, seal } = &recovered.staging else {
        panic!("staging is present");
    };
    let seal = seal.as_ref().expect("sealed");
    assert_eq!(seal.command_id, KEY);
    assert_eq!(seal.sealed_position, Some(2));
    assert_eq!(items.len(), 2);
    assert!(recovered.finalized.is_none());

    // Another request is refused with the read that names the sealing one.
    let other = fixture
        .store
        .finalize_capture(
            &FinalizeSubmission {
                handle: fixture.handle(),
                command_id: "another".into(),
                request: json!({"key": "another"}),
            },
            &mut Ticks(30),
            plan,
        )
        .unwrap_err();
    assert_eq!(
        code_and_path(&other),
        (ErrorCode::RevisionConflict, Some("/handle"))
    );
    assert_eq!(
        other.error.repair.unwrap().argv.unwrap(),
        recover_argv(fixture.handle())
    );

    let committed = fixture
        .store
        .finalize_capture(&submission, &mut Ticks(40), plan)
        .unwrap();
    assert!(fixture.store.capture_recovery(None).unwrap().is_empty());
    let recovered = fixture.only(Some(fixture.handle()));
    let finalized = recovered.finalized.expect("finalized");
    assert_eq!(finalized.receipt.result, CommitResult::AlreadyCommitted);
    assert_eq!(
        finalized.receipt.transaction_id,
        committed.receipt.transaction_id
    );
    assert_eq!(finalized.section, committed.section);
}

#[test]
fn a_finalized_handle_reports_its_original_receipt_also_after_staging_is_gone() {
    let mut fixture = fixture();
    fixture.stage_items();
    let submission = fixture.submission();
    let committed = fixture
        .store
        .finalize_capture(&submission, &mut Ticks(20), plan)
        .unwrap();
    // Finalize copied, never moved, the staged bytes.
    let blobs = fixture.staging().join("blobs");
    assert_eq!(fs::read_dir(&blobs).unwrap().count(), 1);

    let recovered = fixture.only(Some(fixture.handle()));
    let finalized = recovered.finalized.clone().expect("finalized");
    assert_eq!(finalized.receipt.result, CommitResult::AlreadyCommitted);
    assert_eq!(finalized.receipt.command_id, KEY);
    assert_eq!(
        finalized.receipt.transaction_id,
        committed.receipt.transaction_id
    );
    assert_eq!(finalized.receipt.created, committed.receipt.created);
    assert!(matches!(
        recovered.staging,
        RecoveredStaging::Present { seal: Some(_), .. }
    ));
    assert_eq!(fs::read_dir(&blobs).unwrap().count(), 1);

    // Retired staging: the journal still answers, and the manifest holds the items.
    let RecoveredStaging::Present { items: staged, .. } = recovered.staging.clone() else {
        unreachable!()
    };
    fs::remove_dir_all(fixture.staging()).unwrap();
    let retired = fixture.only(Some(fixture.handle()));
    assert_eq!(retired.staging, RecoveredStaging::Retired { items: staged });
    assert_eq!(retired.finalized, recovered.finalized);
    assert!(fixture.store.capture_recovery(None).unwrap().is_empty());
}

#[test]
fn an_undeclared_handle_is_an_unknown_reference() {
    let mut fixture = fixture();
    let unknown: Id = "handle_01936f0e-7c6a-7d3e-8a1b-0123456789ab"
        .parse()
        .unwrap();
    let failure = fixture.store.capture_recovery(Some(unknown)).unwrap_err();
    assert_eq!(
        code_and_path(&failure),
        (ErrorCode::UnknownReference, Some("/handle"))
    );
}

#[test]
fn refused_items_point_at_recover() {
    let mut fixture = fixture();
    fixture.stage_items();
    let handle = fixture.handle();
    let gap = fixture
        .store
        .stage_capture_item(&fact(handle, "late", 5), &mut Ticks(50))
        .unwrap_err();
    assert_eq!(
        code_and_path(&gap),
        (ErrorCode::RevisionConflict, Some("/position"))
    );
    assert_eq!(
        gap.error.repair.unwrap().argv.unwrap(),
        recover_argv(handle)
    );
    let reused = fixture
        .store
        .stage_capture_item(&fact(handle, "started", 3), &mut Ticks(50))
        .unwrap_err();
    assert_eq!(
        code_and_path(&reused),
        (ErrorCode::IdempotencyKeyReused, Some("/key"))
    );
    assert_eq!(
        reused.error.repair.unwrap().argv.unwrap(),
        recover_argv(handle)
    );
    fixture
        .store
        .finalize_capture(&fixture.submission(), &mut Ticks(60), plan)
        .unwrap();
    let sealed = fixture
        .store
        .stage_capture_item(&fact(handle, "late", 3), &mut Ticks(70))
        .unwrap_err();
    assert_eq!(
        code_and_path(&sealed),
        (ErrorCode::RevisionConflict, Some("/handle"))
    );
    assert_eq!(
        sealed.error.repair.unwrap().argv.unwrap(),
        recover_argv(handle)
    );
}
