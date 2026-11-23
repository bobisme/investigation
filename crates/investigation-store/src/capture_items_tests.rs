//! Staged capture item tests. A child of `capture::items`, so the crash
//! schedules drive the same private publication path as production.
use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicU64, Ordering},
};

use investigation_core::{header::Clock, wire::CommitStatus};

use super::*;
use crate::{
    durable::{DurableFile, MemoryFs},
    workspace::{FilesystemDetector, FilesystemInfo, FilesystemKind},
};

const ACTOR: &str = "agent:example-producer";

/// A clock that never repeats, so a replayed receipt is distinguishable from a
/// fresh acceptance.
pub(crate) struct Ticks(pub(crate) u32);
impl Clock for Ticks {
    fn now_utc(&mut self) -> UtcTimestamp {
        self.0 += 1;
        format!("2026-11-22T08:00:00.{:09}Z", self.0)
            .parse()
            .unwrap()
    }
}

pub(crate) fn declaration() -> CaptureDeclaration {
    CaptureDeclaration {
        command_id: "run-0001/begin".into(),
        actor: ACTOR.parse().unwrap(),
        target: DeclarationTarget::NewRecordCase {
            title: "Synthetic runs".into(),
        },
        run: json!({
            "intent": {"kind": "exploration", "intent": "synthetic run"},
            "producer": ACTOR,
            "tool": "example-producer",
            "external_ref": "run-0001",
            "evidence_kind": "observational",
        }),
        producer: "example-producer".into(),
        external_run_id: "run-0001".into(),
        external_plan_digest: None,
        metadata: BTreeMap::new(),
        classification: ClassificationLevel::Internal,
        request: json!({"opaque": "begin"}),
    }
}

fn item(handle: Id, key: &str, position: u64) -> ItemSubmission {
    ItemSubmission {
        operation: ItemOperation::Append,
        handle,
        key: key.into(),
        position,
        request: json!({
            "handle": handle,
            "key": key,
            "position": position,
            "fact": {"kind": "phase_changed", "phase": format!("phase-{key}")},
        }),
        outcome: None,
    }
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
    let mut store = Store::open(Workspace::init(&root, false).unwrap());
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

    fn stage(&mut self, submission: &ItemSubmission) -> Result<ItemAcceptance, Failure> {
        self.store.stage_capture_item(submission, &mut Ticks(0))
    }
}

pub(crate) fn code_and_path(failure: &Failure) -> (ErrorCode, Option<&str>) {
    (failure.error.code, failure.error.path.as_deref())
}

#[test]
fn items_take_consecutive_positions_durably_and_commit_no_transactions() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    let mut clock = Ticks(0);
    for (position, key) in [(1, "started"), (2, "phase-a"), (3, "phase-b")] {
        let accepted = fixture
            .store
            .stage_capture_item(&item(handle, key, position), &mut clock)
            .unwrap();
        assert_eq!(accepted.result, ItemResult::Accepted);
        assert_eq!(accepted.case_id, fixture.declared.case_id);
        assert_eq!(accepted.item.position, position);
        assert_eq!(accepted.item.run_id, fixture.declared.run_id);
        assert_eq!(
            accepted.item.request_digest,
            hash_bytes(&serde_json::to_vec(&item(handle, key, position).request).unwrap())
        );
    }

    let staging = fixture.staging();
    let first = staging.join(ITEMS_DIRECTORY).join("000000000001.json");
    assert_eq!(
        fs::metadata(&first).unwrap().permissions().mode() & 0o777,
        0o400
    );
    let names = |directory: &str| -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(staging.join(directory))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        names(ITEMS_DIRECTORY),
        [
            "000000000001.json",
            "000000000002.json",
            "000000000003.json"
        ]
    );
    assert_eq!(names(KEYS_DIRECTORY).len(), 3, "no temporaries remain");

    let staged = fixture.store.capture_items(handle).unwrap();
    assert!(!staged.sealed);
    assert_eq!(staged.run_id, fixture.declared.run_id);
    assert_eq!(staged.binding, fixture.declared.binding);
    let keys: Vec<_> = staged.items.iter().map(|item| item.key.as_str()).collect();
    assert_eq!(keys, ["started", "phase-a", "phase-b"]);

    // Staging only: the journal still holds the single declaring transaction.
    let replay = fixture.store.case(fixture.declared.case_id).unwrap();
    assert_eq!(replay.events.len(), 1);
    assert!(fixture.store.check().unwrap().integrity_ok());
}

#[test]
fn a_missing_position_is_refused_with_the_expected_position() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    fixture.stage(&item(handle, "one", 1)).unwrap();
    fixture.stage(&item(handle, "two", 2)).unwrap();

    // Position 3 is lost; position 4 must not hide the gap.
    let error = fixture.stage(&item(handle, "four", 4)).unwrap_err();
    assert_eq!(
        code_and_path(&error),
        (ErrorCode::RevisionConflict, Some("/position"))
    );
    assert_eq!(error.commit_status, CommitStatus::NotCommitted);
    assert_eq!(
        error.error.expected,
        Some(json!({"position": 3, "last_position": 2}))
    );
    assert_eq!(error.error.actual, Some(json!({"position": 4})));
    assert!(error.error.message.contains("Position 3 is missing"));

    let error = fixture.stage(&item(handle, "five", 5)).unwrap_err();
    assert!(error.error.message.contains("Positions 3 to 4 are missing"));

    // A taken position is refused the same way.
    let error = fixture.stage(&item(handle, "other", 2)).unwrap_err();
    assert_eq!(
        code_and_path(&error),
        (ErrorCode::RevisionConflict, Some("/position"))
    );
    assert_eq!(error.error.expected.unwrap()["position"], 3);

    // Nothing was staged for the refused items; the gap stays fillable.
    assert_eq!(fixture.store.capture_items(handle).unwrap().items.len(), 2);
    fixture.stage(&item(handle, "three", 3)).unwrap();
    fixture.stage(&item(handle, "four", 4)).unwrap();
}

#[test]
fn an_identical_retry_returns_the_original_receipt_and_changed_bytes_conflict() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    let first = fixture.stage(&item(handle, "started", 1)).unwrap();
    fixture.stage(&item(handle, "next", 2)).unwrap();

    // A later clock would give another time; the replay keeps the original.
    let retry = fixture
        .store
        .stage_capture_item(&item(handle, "started", 1), &mut Ticks(500))
        .unwrap();
    assert_eq!(retry.result, ItemResult::AlreadyAccepted);
    assert_eq!(retry.item, first.item);

    let mut changed = item(handle, "started", 1);
    changed.request["fact"]["phase"] = json!("something else");
    let error = fixture.stage(&changed).unwrap_err();
    assert_eq!(
        code_and_path(&error),
        (ErrorCode::IdempotencyKeyReused, Some("/key"))
    );
    let expected = error.error.expected.unwrap();
    assert_eq!(expected["position"], 1);
    assert_eq!(expected["request_digest"], json!(first.item.request_digest));
    assert_ne!(
        error.error.actual.unwrap()["request_digest"],
        expected["request_digest"]
    );

    // The same key at the next position is still a conflict, not a new item.
    let error = fixture.stage(&item(handle, "started", 3)).unwrap_err();
    assert_eq!(error.error.code, ErrorCode::IdempotencyKeyReused);
    let mut attach = item(handle, "started", 1);
    attach.operation = ItemOperation::Attach;
    let error = fixture.stage(&attach).unwrap_err();
    assert_eq!(error.error.code, ErrorCode::IdempotencyKeyReused);
    assert_eq!(fixture.store.capture_items(handle).unwrap().items.len(), 2);
}

#[test]
fn unknown_handles_oversized_items_and_the_position_limit_are_refused() {
    let mut fixture = fixture();
    let before = fs::read_dir(fixture.staging()).unwrap().count();
    let unknown: Id = "handle_019acd16-0000-7000-8000-000000000009"
        .parse()
        .unwrap();
    let error = fixture.stage(&item(unknown, "started", 1)).unwrap_err();
    assert_eq!(
        code_and_path(&error),
        (ErrorCode::UnknownReference, Some("/handle"))
    );
    assert_eq!(fs::read_dir(fixture.staging()).unwrap().count(), before);

    let handle = fixture.handle();
    let mut large = item(handle, "large", 1);
    large.request["fact"]["phase"] = json!("x".repeat(64 * 1024));
    let error = fixture.stage(&large).unwrap_err();
    assert_eq!(error.error.code, ErrorCode::InvalidInput);
    assert_eq!(
        error.error.expected,
        Some(json!({"max_event_bytes": 65536}))
    );

    let error = fixture.stage(&item(handle, "far", 100_001)).unwrap_err();
    assert_eq!(
        code_and_path(&error),
        (ErrorCode::InvalidInput, Some("/position"))
    );
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
fn a_sealed_handle_refuses_new_items_but_replays_accepted_ones() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    let first = fixture.stage(&item(handle, "started", 1)).unwrap();
    // Finalization publishes the seal; its contents belong to finalize.
    fs::write(fixture.staging().join(SEAL_FILE), b"{}\n").unwrap();

    let error = fixture.stage(&item(handle, "late", 2)).unwrap_err();
    assert_eq!(
        code_and_path(&error),
        (ErrorCode::RevisionConflict, Some("/handle"))
    );
    assert!(error.error.message.contains("sealed"));
    let retry = fixture.stage(&item(handle, "started", 1)).unwrap();
    assert_eq!(retry.result, ItemResult::AlreadyAccepted);
    assert_eq!(retry.item, first.item);
    assert!(fixture.store.capture_items(handle).unwrap().sealed);
}

#[test]
fn a_key_entry_lost_after_its_item_is_repaired_before_any_lookup() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    fixture.stage(&item(handle, "started", 1)).unwrap();
    let second = fixture.stage(&item(handle, "phase", 2)).unwrap();
    // A crash after the item was published and before its key entry was.
    let key_file = fixture
        .staging()
        .join(KEYS_DIRECTORY)
        .join(key_name("phase"));
    fs::remove_file(&key_file).unwrap();

    // Reusing the key for a new item is still refused.
    let error = fixture.stage(&item(handle, "phase", 3)).unwrap_err();
    assert_eq!(error.error.code, ErrorCode::IdempotencyKeyReused);
    assert!(key_file.is_file());

    fs::remove_file(&key_file).unwrap();
    let retry = fixture.stage(&item(handle, "phase", 2)).unwrap();
    assert_eq!(retry.result, ItemResult::AlreadyAccepted);
    assert_eq!(retry.item, second.item);
    assert!(key_file.is_file());
}

#[test]
fn staged_items_that_disagree_with_their_digest_or_leave_a_gap_are_integrity_failures() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    fixture.stage(&item(handle, "one", 1)).unwrap();
    fixture.stage(&item(handle, "two", 2)).unwrap();
    let items = fixture.staging().join(ITEMS_DIRECTORY);

    let second = items.join("000000000002.json");
    let original = fs::read(&second).unwrap();
    let mut tampered: Value = serde_json::from_slice(&original).unwrap();
    tampered["request"]["fact"]["phase"] = json!("rewritten");
    fs::remove_file(&second).unwrap();
    fs::write(&second, serde_json::to_vec(&tampered).unwrap()).unwrap();
    let error = fixture.store.capture_items(handle).unwrap_err();
    assert_eq!(error.error.code, ErrorCode::IntegrityFailure);

    fs::remove_file(&second).unwrap();
    fs::write(items.join("000000000003.json"), &original).unwrap();
    let error = fixture.store.capture_items(handle).unwrap_err();
    assert_eq!(error.error.code, ErrorCode::IntegrityFailure);
    assert!(error.error.message.contains("item 2 is missing"));
}

#[test]
fn a_staging_binding_that_disagrees_with_the_journal_is_refused() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    let binding = fixture.staging().join(BINDING_FILE);
    let mut value: Value = serde_json::from_slice(&fs::read(&binding).unwrap()).unwrap();
    // Point another directory's binding at this handle.
    let other = fixture
        .root
        .join(STAGING_ROOT)
        .join("run_019acd16-0000-7000-8000-000000000001");
    fs::create_dir(&other).unwrap();
    value["run_id"] = json!("run_019acd16-0000-7000-8000-000000000001");
    fs::remove_file(&binding).unwrap();
    fs::write(
        other.join(BINDING_FILE),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    let error = fixture.stage(&item(handle, "started", 1)).unwrap_err();
    assert_eq!(error.error.code, ErrorCode::IntegrityFailure);
}

// Crash schedules over the in-memory filesystem model, where directory
// entries and file data become durable separately.

pub(crate) struct LocalDetector;
impl FilesystemDetector for LocalDetector {
    fn detect(&self, _: &Path) -> io::Result<FilesystemInfo> {
        Ok(FilesystemInfo {
            kind: FilesystemKind::Local,
            name: "test-local".into(),
            magic: None,
        })
    }
}

pub(crate) fn memory_store() -> (Store<MemoryFs>, PathBuf) {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "inv-capture-items-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let workspace = Workspace::init_with_detector(&root, false, &LocalDetector).unwrap();
    let mut fake = MemoryFs::new();
    for dir in ["state", "state/cases", "state/tmp", "state/locks", "local"] {
        fake.create_dir(Path::new(dir)).unwrap();
        fake.sync_dir(Path::new(dir).parent().unwrap()).unwrap();
    }
    let mut lock = fake.create_private(Path::new(COORDINATION_LOCK)).unwrap();
    lock.sync_all().unwrap();
    drop(lock);
    fake.sync_dir(Path::new("state/locks")).unwrap();
    (
        Store {
            workspace,
            fs: fake,
            gate: WriteGate::isolated(),
            catalog: standard_catalog(),
        },
        root,
    )
}

pub(crate) fn restart(store: &mut Store<MemoryFs>) {
    store.fs.crash();
    store.gate = WriteGate::isolated();
}

/// Inject a failure at every filesystem call of an append, then crash. Every
/// acknowledged item must survive at its position with its original receipt,
/// and an identical retry must settle the uncertain one without duplicating it.
#[test]
fn every_acknowledged_item_survives_a_crash_at_its_position() {
    let mut injected = 0;
    let mut acknowledged_first_try = 0;
    for seed in 1..=60 {
        let (mut store, root) = memory_store();
        let declared = store.declare_capture(&declaration()).unwrap();
        let handle = declared.binding.handle;
        let mut clock = Ticks(0);
        let first = store
            .stage_capture_item(&item(handle, "started", 1), &mut clock)
            .unwrap();
        restart(&mut store);

        store.fs.fail_at(seed);
        let second = match store.stage_capture_item(&item(handle, "phase", 2), &mut clock) {
            Ok(accepted) => {
                acknowledged_first_try += 1;
                Some(accepted)
            }
            Err(_) => {
                injected += 1;
                None
            }
        };
        restart(&mut store);
        let retried = store
            .stage_capture_item(&item(handle, "phase", 2), &mut Ticks(1000))
            .unwrap();
        if let Some(second) = &second {
            // Acknowledged before the crash: the retry replays it exactly.
            assert_eq!(retried.result, ItemResult::AlreadyAccepted, "seed {seed}");
            assert_eq!(retried.item, second.item, "seed {seed}");
        }
        restart(&mut store);

        let staged = store.capture_items(handle).unwrap();
        assert_eq!(staged.items.len(), 2, "seed {seed}");
        assert_eq!(staged.items[0], first.item, "seed {seed}");
        assert_eq!(staged.items[1], retried.item, "seed {seed}");
        // The next position continues after the survivors.
        let third = store
            .stage_capture_item(&item(handle, "phase-3", 3), &mut clock)
            .unwrap();
        assert_eq!(third.item.position, 3);
        let error = store
            .stage_capture_item(&item(handle, "phase", 4), &mut clock)
            .unwrap_err();
        assert_eq!(
            error.error.code,
            ErrorCode::IdempotencyKeyReused,
            "seed {seed}"
        );
        let _ = fs::remove_dir_all(root);
    }
    assert!(injected > 10, "the schedule must reach the append path");
    assert!(acknowledged_first_try > 0, "the schedule must run past it");
}
