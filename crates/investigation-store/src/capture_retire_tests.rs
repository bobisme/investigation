//! Staging retirement and the staging checks, against a real local workspace,
//! a process killed at every filesystem call, and the in-memory power-loss
//! model.
use std::{
    cell::Cell,
    fs,
    io::{self, Read as _},
    os::unix::fs::PermissionsExt,
    rc::Rc,
    time::Duration,
};

use investigation_core::explain::{EvidenceCompleteness, ExecutionStatus};

use super::*;
use crate::{
    durable::{LocalFs, LockError, MemoryFs, WriteGate},
    store::capture::{
        attach::{AttachSource, AttachSubmission},
        finalize::{
            FinalizeContext, FinalizePlan, FinalizeSubmission, PlannedArtifact, PlannedRole,
        },
        items::{
            ItemAcceptance, ItemOperation, ItemSubmission,
            tests::{Ticks, declaration, memory_store, restart},
        },
        late::item_map,
        recover::RecoveredStaging,
    },
};
use investigation_core::catalog::standard_catalog;

const KEY: &str = "run-0001/finalize";

fn with_imports(root: &Path, imports: &Path) -> Workspace {
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    if !contents.contains("import_roots = []") {
        return Workspace::open(root).unwrap();
    }
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

fn submission(handle: Id) -> FinalizeSubmission {
    FinalizeSubmission {
        handle,
        command_id: KEY.into(),
        request: json!({"key": KEY, "completion": "complete"}),
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

/// A fact and a retained attachment on a fresh handle.
fn stage_items<F: DurableFs>(store: &mut Store<F>, handle: Id) -> Vec<ItemAcceptance> {
    vec![
        store
            .stage_capture_item(&fact(handle, "started", 1), &mut Ticks(0))
            .unwrap(),
        store
            .stage_capture_attachment(&attachment(handle, "result", 2), &mut Ticks(10))
            .unwrap(),
    ]
}

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
    fs::write(imports.join("result.bin"), b"original bytes").unwrap();
    let mut store = Store::open(with_imports(&root, &imports));
    let declared = store.declare_capture(&declaration()).unwrap();
    Fixture {
        _temporary: temporary,
        root,
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

    fn stage_items(&mut self) -> Vec<ItemAcceptance> {
        let handle = self.handle();
        stage_items(&mut self.store, handle)
    }

    fn finalize(&mut self) {
        let submission = submission(self.handle());
        self.store
            .finalize_capture(&submission, &mut Ticks(20), plan)
            .unwrap();
    }

    fn retire(&mut self) -> StagingRetirement {
        let handle = self.handle();
        let mut report = self.store.retire_staging(Some(handle)).unwrap();
        assert_eq!(report.handles.len(), 1);
        report.handles.remove(0)
    }

    fn check(&mut self) -> CheckReport {
        self.store.check().unwrap()
    }

    /// Every file of the staging directory with its bytes.
    fn staged_tree(&self) -> Vec<(PathBuf, Vec<u8>)> {
        tree(&self.staging())
    }

    fn retiring(&self) -> Vec<PathBuf> {
        retiring_entries(&self.root)
    }
}

fn tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        let Ok(listing) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in listing {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap().to_owned();
                entries.push((relative, fs::read(&path).unwrap()));
            }
        }
    }
    entries.sort();
    entries
}

fn retiring_entries(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root.join(TMP))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(RETIRING_PREFIX))
        })
        .collect()
}

fn codes(report: &CheckReport) -> Vec<&str> {
    report
        .issues
        .iter()
        .map(|issue| issue.code.as_str())
        .collect()
}

fn kept_reason(retirement: &StagingRetirement) -> Option<KeptReason> {
    assert_eq!(retirement.result, RetirementResult::Kept, "{retirement:?}");
    retirement.reason
}

#[test]
fn finalized_staging_is_retired_once_and_identical_retries_still_answer() {
    let mut fixture = fixture();
    let accepted = fixture.stage_items();
    fixture.finalize();
    let handle = fixture.handle();

    let report = fixture.check();
    assert!(report.integrity_ok(), "{:?}", report.issues);
    assert_eq!(report.staging.retirable, 1);
    let retirable = report
        .issues
        .iter()
        .find(|issue| issue.code == "STAGING_RETIRABLE")
        .unwrap();
    assert_eq!(retirable.severity, Severity::Warning);
    assert_eq!(retirable.objects, [fixture.declared.run_id]);
    assert_eq!(retirable.repair, Some(retire_argv()));

    let before = tree(&fixture.root.join("state/cases"));
    let blobs = tree(&fixture.root.join("state/blobs"));
    let report = fixture.store.retire_staging(None).unwrap();
    assert!(report.completed.is_empty());
    assert_eq!(report.handles.len(), 1);
    let retired = &report.handles[0];
    assert_eq!(retired.result, RetirementResult::Retired);
    assert_eq!(retired.handle, handle);
    assert_eq!(retired.staging_dir, fixture.declared.binding.staging);
    assert_eq!(retired.reason, None);
    assert!(!fixture.staging().exists());
    assert!(fixture.retiring().is_empty());
    // Retirement changes no journal event and no canonical blob.
    assert_eq!(tree(&fixture.root.join("state/cases")), before);
    assert_eq!(tree(&fixture.root.join("state/blobs")), blobs);

    // Idempotent: nothing left to retire, and the handle reports it so.
    let again = fixture.store.retire_staging(None).unwrap();
    assert!(again.handles.is_empty());
    assert_eq!(fixture.retire().result, RetirementResult::AlreadyRetired);

    let report = fixture.check();
    assert!(report.integrity_ok(), "{:?}", report.issues);
    assert_eq!(
        report.staging,
        StagingSummary {
            retired: 1,
            ..StagingSummary::default()
        }
    );
    assert!(
        codes(&report)
            .iter()
            .all(|code| !code.starts_with("STAGING") && !code.starts_with("CAPTURE")),
        "{:?}",
        report.issues
    );

    // The manifest answers identical retries and recovery.
    let retry = fixture
        .store
        .stage_capture_item(&fact(handle, "started", 1), &mut Ticks(100))
        .unwrap();
    assert_eq!(retry.item, accepted[0].item);
    let finalized = fixture
        .store
        .finalize_capture(&submission(handle), &mut Ticks(110), plan)
        .unwrap();
    assert_eq!(
        finalized.receipt.result,
        investigation_core::wire::CommitResult::AlreadyCommitted
    );
    let recovered = fixture.store.capture_recovery(Some(handle)).unwrap();
    assert_eq!(
        recovered[0].staging,
        RecoveredStaging::Retired {
            items: accepted.into_iter().map(|accepted| accepted.item).collect()
        }
    );
    // An identical begin allocates nothing, so nothing reappears to retire.
    fixture.store.declare_capture(&declaration()).unwrap();
    assert!(!fixture.staging().exists());
}

#[test]
fn open_sealed_and_unallocated_staging_is_never_removed() {
    let mut fixture = fixture();
    fixture.stage_items();
    let open = fixture.staged_tree();
    assert_eq!(kept_reason(&fixture.retire()), Some(KeptReason::Open));
    assert_eq!(fixture.staged_tree(), open);
    let report = fixture.check();
    assert_eq!(report.staging.active, 1);
    assert!(!codes(&report).contains(&"STAGING_RETIRABLE"));

    // Sealed, then the transaction fails: what a crash between seal and
    // canonical publication leaves.
    let handle = fixture.handle();
    fixture
        .store
        .finalize_capture(&submission(handle), &mut Ticks(20), |context| {
            let mut planned = plan(context)?;
            planned.section = Box::new(|_| Err(integrity("interrupted")));
            Ok(planned)
        })
        .unwrap_err();
    let sealed = fixture.staged_tree();
    assert!(sealed.iter().any(|(path, _)| path == Path::new(SEAL_FILE)));
    let retirement = fixture.retire();
    assert_eq!(kept_reason(&retirement), Some(KeptReason::Sealed));
    assert!(retirement.detail.unwrap().contains("finalize"));
    assert_eq!(fixture.staged_tree(), sealed);
    let report = fixture.check();
    assert!(report.integrity_ok(), "{:?}", report.issues);
    assert_eq!(report.staging.sealed, 1);
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.code == "CAPTURE_SEALED_UNCOMMITTED")
        .unwrap();
    assert_eq!(issue.repair, Some(recover_argv(handle)));
    // Still recoverable: recovery reads the seal and the identical retry commits.
    assert!(matches!(
        fixture.store.capture_recovery(Some(handle)).unwrap()[0].staging,
        RecoveredStaging::Present { seal: Some(_), .. }
    ));
    fixture.finalize();
    assert_eq!(fixture.retire().result, RetirementResult::Retired);

    // Declared, never allocated: nothing to retire, and the begin still allocates.
    let mut unallocated = self::fixture();
    fs::remove_dir_all(unallocated.staging()).unwrap();
    let retirement = unallocated.retire();
    assert_eq!(kept_reason(&retirement), Some(KeptReason::Unallocated));
    let report = unallocated.check();
    assert_eq!(report.staging.unallocated, 1);
    assert!(codes(&report).contains(&"CAPTURE_STAGING_UNALLOCATED"));
    unallocated.store.declare_capture(&declaration()).unwrap();
    assert!(unallocated.staging().join(BINDING_FILE).exists());
}

#[test]
fn staging_whose_bytes_the_canonical_store_lacks_is_kept() {
    let mut fixture = fixture();
    fixture.stage_items();
    fixture.finalize();
    let staged = fixture.staged_tree();
    let blob = staged
        .iter()
        .find(|(path, _)| path.starts_with(BLOBS_DIRECTORY))
        .map(|(path, bytes)| (path.file_name().unwrap().to_owned(), bytes.clone()))
        .unwrap();
    let hex = blob.0.to_str().unwrap();
    let canonical = fixture.root.join(BLOBS).join(&hex[..2]).join(hex);

    // Missing from the canonical store.
    fs::remove_file(&canonical).unwrap();
    let retirement = fixture.retire();
    assert_eq!(kept_reason(&retirement), Some(KeptReason::Unpublished));
    assert!(retirement.detail.unwrap().contains(hex));
    assert_eq!(fixture.staged_tree(), staged);

    // Present with other bytes.
    fs::write(&canonical, b"other bytes").unwrap();
    assert_eq!(
        kept_reason(&fixture.retire()),
        Some(KeptReason::Unpublished)
    );
    assert_eq!(fixture.staged_tree(), staged);

    // Restored: retirement proceeds.
    fs::write(&canonical, &blob.1).unwrap();
    assert_eq!(fixture.retire().result, RetirementResult::Retired);
}

#[test]
fn staging_with_an_entry_the_store_did_not_write_is_kept() {
    let mut fixture = fixture();
    fixture.stage_items();
    fixture.finalize();
    let foreign = fixture.staging().join("notes.txt");
    fs::write(&foreign, b"a person's notes").unwrap();
    let retirement = fixture.retire();
    assert_eq!(kept_reason(&retirement), Some(KeptReason::Inconsistent));
    assert!(retirement.detail.unwrap().contains("notes.txt"));
    assert!(foreign.exists());
    fs::remove_file(&foreign).unwrap();

    let nested = fixture.staging().join(ITEMS_DIRECTORY).join("README");
    fs::write(&nested, b"not an item").unwrap();
    assert_eq!(
        kept_reason(&fixture.retire()),
        Some(KeptReason::Inconsistent)
    );
    assert!(nested.exists());
    fs::remove_file(&nested).unwrap();
    assert_eq!(fixture.retire().result, RetirementResult::Retired);
}

#[test]
fn check_verifies_each_staging_binding_and_reports_orphans() {
    let mut fixture = fixture();
    fixture.stage_items();
    let report = fixture.check();
    assert!(report.integrity_ok(), "{:?}", report.issues);
    assert_eq!(
        report.staging,
        StagingSummary {
            active: 1,
            ..StagingSummary::default()
        }
    );

    // A binding that is not the one its declaration allocates.
    let binding = fixture.staging().join(BINDING_FILE);
    let original = fs::read(&binding).unwrap();
    let changed = String::from_utf8(original.clone())
        .unwrap()
        .replace("agent:", "agent:other-");
    fs::set_permissions(&binding, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&binding, changed).unwrap();
    let report = fixture.check();
    assert!(!report.integrity_ok());
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.code == "STAGING_BINDING_MISMATCH")
        .unwrap();
    assert_eq!(issue.severity, Severity::Error);
    assert_eq!(issue.objects, [fixture.handle(), fixture.declared.run_id]);
    assert!(issue.reason.contains("digest sha256:"), "{}", issue.reason);
    fs::write(&binding, &original).unwrap();
    assert!(fixture.check().integrity_ok());

    // Directories bound to no declared handle are reported and kept.
    let stray_run = "run_01a20705-7fe2-7bc3-ae85-416bf664a62c";
    let stray = fixture.root.join(STAGING_ROOT).join(stray_run);
    fs::create_dir(&stray).unwrap();
    fs::write(stray.join("items.json"), b"unknown").unwrap();
    let other = fixture.root.join(STAGING_ROOT).join("scratch");
    fs::create_dir(&other).unwrap();
    let report = fixture.check();
    assert!(report.integrity_ok(), "{:?}", report.issues);
    assert_eq!(report.staging.orphaned, 2);
    let orphans: Vec<_> = report
        .issues
        .iter()
        .filter(|issue| issue.code == "STAGING_ORPHAN")
        .collect();
    assert_eq!(orphans.len(), 2);
    assert!(orphans.iter().all(|issue| issue.repair.is_none()));
    assert!(
        orphans
            .iter()
            .any(|issue| issue.objects == [stray_run.parse::<Id>().unwrap()])
    );

    fixture.finalize();
    assert_eq!(fixture.retire().result, RetirementResult::Retired);
    // Retirement never touches what it is not bound to.
    assert_eq!(tree(&stray).len(), 1);
    assert!(other.is_dir());
    // Check reads only: it changed nothing.
    let staging_root = tree(&fixture.root.join(STAGING_ROOT));
    fixture.check();
    assert_eq!(tree(&fixture.root.join(STAGING_ROOT)), staging_root);
}

#[test]
fn an_interrupted_removal_is_reported_and_completed() {
    let mut fixture = fixture();
    fixture.stage_items();
    fixture.finalize();
    // The state a crash after the rename and during removal leaves.
    let retiring = fixture
        .root
        .join(TMP)
        .join(format!("{RETIRING_PREFIX}{}", fixture.declared.run_id));
    fs::rename(fixture.staging(), &retiring).unwrap();
    fs::remove_file(retiring.join(BINDING_FILE)).unwrap();
    for entry in fs::read_dir(retiring.join(ITEMS_DIRECTORY)).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    fs::remove_dir(retiring.join(ITEMS_DIRECTORY)).unwrap();

    let handle = fixture.handle();
    assert!(matches!(
        fixture.store.capture_recovery(Some(handle)).unwrap()[0].staging,
        RecoveredStaging::Retired { .. }
    ));
    let report = fixture.check();
    assert!(report.integrity_ok(), "{:?}", report.issues);
    assert_eq!(report.staging.retired, 1);
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.code == "STAGING_RETIREMENT_INCOMPLETE")
        .unwrap();
    assert_eq!(issue.objects, [fixture.declared.run_id]);
    assert_eq!(issue.repair, Some(retire_argv()));

    let report = fixture.store.retire_staging(None).unwrap();
    assert_eq!(report.completed, [fixture.declared.run_id]);
    assert!(report.handles.is_empty());
    assert!(!retiring.exists());
    assert!(!codes(&fixture.check()).contains(&"STAGING_RETIREMENT_INCOMPLETE"));
}

/// A filesystem whose process dies at call `limit`: that call and every later
/// one fail, and everything before it stays on disk, as after a kill.
struct KillFs {
    fs: LocalFs,
    calls: Rc<Cell<usize>>,
    limit: usize,
}

impl KillFs {
    fn step(&self) -> io::Result<()> {
        self.calls.set(self.calls.get() + 1);
        if self.calls.get() >= self.limit {
            return Err(io::Error::other("killed"));
        }
        Ok(())
    }
}

impl DurableFs for KillFs {
    type File = <LocalFs as DurableFs>::File;
    type Guard = <LocalFs as DurableFs>::Guard;
    fn create_private(&mut self, path: &Path) -> io::Result<Self::File> {
        self.step()?;
        self.fs.create_private(path)
    }
    fn create_dir(&mut self, path: &Path) -> io::Result<()> {
        self.step()?;
        self.fs.create_dir(path)
    }
    fn read(&self, path: &Path) -> io::Result<Box<dyn io::Read>> {
        self.step()?;
        self.fs.read(path)
    }
    fn sync_existing(&mut self, path: &Path) -> io::Result<()> {
        self.step()?;
        self.fs.sync_existing(path)
    }
    fn list(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.step()?;
        self.fs.list(path)
    }
    fn link_no_replace(&mut self, source: &Path, target: &Path) -> io::Result<()> {
        self.step()?;
        self.fs.link_no_replace(source, target)
    }
    fn rename_no_replace(&mut self, source: &Path, target: &Path) -> io::Result<()> {
        self.step()?;
        self.fs.rename_no_replace(source, target)
    }
    fn sync_dir(&mut self, path: &Path) -> io::Result<()> {
        self.step()?;
        self.fs.sync_dir(path)
    }
    fn remove(&mut self, path: &Path) -> io::Result<()> {
        self.step()?;
        self.fs.remove(path)
    }
    fn remove_dir(&mut self, path: &Path) -> io::Result<()> {
        self.step()?;
        self.fs.remove_dir(path)
    }
    fn lock(&mut self, path: &Path, timeout: Duration) -> Result<Self::Guard, LockError> {
        self.step().map_err(LockError::Io)?;
        self.fs.lock(path, timeout)
    }
    fn identity(&self, path: &Path) -> io::Result<crate::durable::FileIdentity> {
        self.step()?;
        self.fs.identity(path)
    }
}

/// Kill the retiring process at every filesystem call. The staging is then
/// either complete, byte for byte, or gone from `state/staging`; recovery
/// reports the same items either way; and the next retirement finishes the
/// job without leaving anything behind.
#[test]
fn a_kill_at_any_point_of_retirement_leaves_the_full_staging_or_none() {
    let mut killed_with_staging = 0;
    let mut killed_without_staging = 0;
    let mut completed_first_try = 0;
    for limit in 1..=200 {
        let mut fixture = fixture();
        let accepted = fixture.stage_items();
        fixture.finalize();
        let handle = fixture.handle();
        let full = fixture.staged_tree();
        let items: Vec<_> = accepted.into_iter().map(|accepted| accepted.item).collect();

        let calls = Rc::new(Cell::new(0));
        let mut dying = Store {
            workspace: Workspace::open(&fixture.root).unwrap(),
            fs: KillFs {
                fs: LocalFs::new(&fixture.root),
                calls: calls.clone(),
                limit,
            },
            gate: WriteGate::isolated(),
            catalog: standard_catalog(),
        };
        let first = dying.retire_staging(Some(handle));
        drop(dying);

        // A fresh process.
        let mut store = Store::open(Workspace::open(&fixture.root).unwrap());
        let present = fixture.staging().exists();
        if present {
            assert_eq!(fixture.staged_tree(), full, "limit {limit}");
        } else {
            assert!(
                !fixture
                    .root
                    .join(STAGING_ROOT)
                    .join(fixture.declared.run_id.to_string())
                    .exists(),
                "limit {limit}"
            );
        }
        let recovered = store.capture_recovery(Some(handle)).unwrap().remove(0);
        if present {
            let RecoveredStaging::Present {
                items: staged,
                seal,
            } = recovered.staging
            else {
                panic!("limit {limit}: {:?}", recovered.staging);
            };
            assert_eq!(staged, items, "limit {limit}");
            assert!(seal.is_some(), "limit {limit}");
        } else {
            assert_eq!(
                recovered.staging,
                RecoveredStaging::Retired {
                    items: items.clone()
                },
                "limit {limit}"
            );
        }
        assert!(recovered.finalized.is_some(), "limit {limit}");
        // The manifest or the staging answers an identical retry either way.
        let retry = store
            .stage_capture_item(&fact(handle, "started", 1), &mut Ticks(100))
            .unwrap();
        assert_eq!(retry.item, items[0], "limit {limit}");

        match first {
            Ok(report) if report.handles[0].result == RetirementResult::Retired => {
                completed_first_try += 1;
                assert!(!present, "limit {limit}");
                assert!(fixture.retiring().is_empty(), "limit {limit}");
            }
            // A read the kill broke can only keep the staging.
            Ok(report) => {
                assert_eq!(report.handles[0].result, RetirementResult::Kept);
                assert!(present, "limit {limit}");
                killed_with_staging += 1;
            }
            Err(_) if present => killed_with_staging += 1,
            Err(_) => killed_without_staging += 1,
        }
        let report = store.retire_staging(Some(handle)).unwrap();
        let expected = if present {
            RetirementResult::Retired
        } else {
            RetirementResult::AlreadyRetired
        };
        assert_eq!(report.handles[0].result, expected, "limit {limit}");
        assert!(!fixture.staging().exists(), "limit {limit}");
        assert!(fixture.retiring().is_empty(), "limit {limit}");
        let check = store.check().unwrap();
        assert!(check.integrity_ok(), "limit {limit}: {:?}", check.issues);
        assert_eq!(check.staging.retired, 1, "limit {limit}");
        if completed_first_try > 0 {
            break;
        }
    }
    assert!(
        killed_with_staging > 0,
        "the schedule must kill before the rename"
    );
    assert!(
        killed_without_staging > 0,
        "the schedule must kill during removal"
    );
    assert_eq!(
        completed_first_try, 1,
        "the schedule must run past retirement"
    );
}

fn memory_retire_store(imports: &Path) -> (Store<MemoryFs>, PathBuf) {
    let (mut store, root) = memory_store();
    // A workspace creates these durably, as `Workspace::init` does.
    for directory in [STAGING_ROOT, "state/blobs", "state/blobs/sha256"] {
        store.fs.create_dir(Path::new(directory)).unwrap();
        store
            .fs
            .sync_dir(Path::new(directory).parent().unwrap())
            .unwrap();
    }
    store.workspace = with_imports(&root, imports);
    (store, root)
}

/// Every file under a directory of the in-memory filesystem, with its bytes.
fn memory_tree(store: &Store<MemoryFs>, root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        let Ok(listing) = store.fs.list(&directory) else {
            continue;
        };
        for path in listing {
            if store.fs.list(&path).is_ok() {
                pending.push(path);
                continue;
            }
            let mut bytes = Vec::new();
            store
                .fs
                .read(&path)
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            entries.push((path.strip_prefix(root).unwrap().to_owned(), bytes));
        }
    }
    entries.sort();
    entries
}

/// Lose power at every filesystem call of a retirement: only synced entries
/// survive. Staging is still complete or retired, never partial, and the next
/// retirement settles it.
#[test]
fn a_power_loss_at_any_point_of_retirement_leaves_the_full_staging_or_none() {
    let imports = tempfile::tempdir().unwrap();
    fs::write(imports.path().join("result.bin"), vec![7_u8; 70 * 1024]).unwrap();
    let mut lost_with_staging = 0;
    let mut lost_without_staging = 0;
    let mut acknowledged = 0;
    for seed in 1..=200 {
        let (mut store, root) = memory_retire_store(imports.path());
        let declared = store.declare_capture(&declaration()).unwrap();
        let handle = declared.binding.handle;
        let staging = PathBuf::from(&declared.binding.staging);
        let items: Vec<_> = stage_items(&mut store, handle)
            .into_iter()
            .map(|accepted| accepted.item)
            .collect();
        store
            .finalize_capture(&submission(handle), &mut Ticks(20), plan)
            .unwrap();
        restart(&mut store);
        let full = memory_tree(&store, &staging);
        assert!(!full.is_empty());

        store.fs.fail_at(seed);
        let first = store.retire_staging(Some(handle));
        restart(&mut store);

        let left = memory_tree(&store, &staging);
        let present = store.exists(&staging.join(BINDING_FILE)).unwrap();
        if present {
            assert_eq!(left, full, "seed {seed}");
        } else {
            assert!(left.is_empty(), "seed {seed}: {left:?}");
        }
        let recovered = store.capture_recovery(Some(handle)).unwrap().remove(0);
        match &recovered.staging {
            RecoveredStaging::Present { items: staged, .. } => {
                assert!(present, "seed {seed}");
                assert_eq!(staged, &items, "seed {seed}");
            }
            RecoveredStaging::Retired { items: retired } => {
                assert!(!present, "seed {seed}");
                assert_eq!(retired, &items, "seed {seed}");
            }
            other => panic!("seed {seed}: {other:?}"),
        }
        match &first {
            Ok(report) if report.handles[0].result == RetirementResult::Retired => {
                assert!(!present, "seed {seed}: retired but staging survived");
                acknowledged += 1;
            }
            // A read the fault broke can only keep the staging.
            Ok(report) => {
                assert_eq!(report.handles[0].result, RetirementResult::Kept);
                assert!(present, "seed {seed}");
                lost_with_staging += 1;
            }
            Err(_) if present => lost_with_staging += 1,
            Err(_) => lost_without_staging += 1,
        }
        let settled = store
            .retire_staging(Some(handle))
            .unwrap_or_else(|error| panic!("seed {seed}: {first:?} then {error:?}"));
        assert_ne!(
            settled.handles[0].result,
            RetirementResult::Kept,
            "seed {seed}: {settled:?}"
        );
        restart(&mut store);
        assert!(
            !store.exists(&staging.join(BINDING_FILE)).unwrap(),
            "seed {seed}"
        );
        assert!(
            store.retiring_directories().unwrap().is_empty(),
            "seed {seed}"
        );
        let _ = fs::remove_dir_all(root);
        if acknowledged > 0 {
            break;
        }
    }
    assert!(
        lost_with_staging > 0,
        "the schedule must fail before the rename"
    );
    assert!(acknowledged > 0, "the schedule must run past retirement");
    // Failures after the durable rename leave the staging retired.
    assert!(
        lost_without_staging > 0,
        "the schedule must fail after the rename"
    );
}
