//! Crash and retry qualification of the whole capture protocol (W07.14,
//! §20.3). A child of `capture`, so every schedule drives the same private
//! staging, seal, publication and commit paths as production, on the
//! crash-model filesystem of `durable`.
//!
//! The targeted cases next to each operation's own tests cover single
//! windows. This module adds what spans the protocol:
//!
//! - a crash at every filesystem call of `begin`, settled with one Run;
//! - every crash window of `finalize`, classified by what is durable (open,
//!   sealed with no event, bytes published with no event, event durable with
//!   staging not retired), each with its own recovery, including retirement;
//! - the last item and the seal serialized by the handle lock, in both orders;
//! - the commit lock free while producer bytes are copied and while the
//!   manifest is planned, on the real filesystem and on the crash model;
//! - a seeded property test over random begin, append, attach, gap, retry,
//!   changed-retry, finalize, retirement and crash interleavings with
//!   injected faults. It asserts that every acknowledged item stays
//!   recoverable at its position with its original receipt, that one attempt
//!   has one Run and one finalization, and that the finalized manifest binds
//!   exactly the acknowledged items.
//!
//! The property test runs `DEFAULT_CASES` cases for each of `DEFAULT_SEEDS`.
//! `INV_CAPTURE_SEED=<u64>` replays one seed and `INV_CAPTURE_CASES=<n>`
//! runs more cases, for long local runs; `INV_CAPTURE_STATS=1` prints how
//! often each protocol path was taken. A failure prints the seed and the
//! shrunk step list; commit it as a regression in `REGRESSIONS`.

use std::{fs, io::Read as _, time::Duration};

use investigation_core::{
    explain::{EvidenceCompleteness, ExecutionStatus},
    wire::{CommitResult, Digest},
};
use proptest::{
    prelude::*,
    test_runner::{Config, RngAlgorithm, TestCaseError, TestError, TestRng, TestRunner},
};

use super::*;
use crate::{
    durable::{CommitLock, MemoryFs},
    journal::hash_bytes,
    store::capture::{
        attach::{AttachOutcome, AttachSource, AttachSubmission},
        finalize::{
            FINALIZE_OPERATION, FinalizeContext, FinalizePlan, FinalizeSubmission,
            FinalizedCapture, PlannedArtifact, PlannedRole,
        },
        items::{
            HANDLE_LOCK_FILE, ItemAcceptance, ItemOperation, ItemResult, ItemSubmission, SEAL_FILE,
            StagedItem,
            tests::{Ticks, declaration, memory_store, restart},
        },
        late::item_map,
        recover::RecoveredStaging,
        retire::{KeptReason, RetirementResult},
    },
};

const KEY: &str = "run-0001/finalize";
const FILES: [(&str, &[u8]); 2] = [
    ("result.bin", b"result bytes of the synthetic run\n"),
    ("trace.log", b"trace line one\ntrace line two\n"),
];

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Import roots for attachments, with the fixture files.
fn imports() -> tempfile::TempDir {
    let imports = tempfile::tempdir().unwrap();
    for (name, bytes) in FILES {
        fs::write(imports.path().join(name), bytes).unwrap();
    }
    imports
}

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

/// A crash-model store laid out durably as `Workspace::init` lays it out.
fn crash_model_store(imports: &Path) -> (Store<MemoryFs>, PathBuf) {
    let (mut store, root) = memory_store();
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

/// A second writer on the same crash-model filesystem: another process on the
/// same workspace.
fn twin(store: &Store<MemoryFs>) -> Store<MemoryFs> {
    Store {
        workspace: store.workspace.clone(),
        fs: store.fs.clone(),
        gate: WriteGate::isolated(),
        catalog: standard_catalog(),
    }
}

fn fact(handle: Id, key: &str, position: u64) -> ItemSubmission {
    ItemSubmission {
        operation: ItemOperation::Append,
        handle,
        key: key.into(),
        position,
        request: json!({"handle": handle, "key": key, "position": position, "fact": "phase"}),
        outcome: None,
    }
}

fn file(handle: Id, key: &str, position: u64, name: &str) -> AttachSubmission {
    AttachSubmission {
        item: ItemSubmission {
            operation: ItemOperation::Attach,
            handle,
            key: key.into(),
            position,
            request: json!({"handle": handle, "key": key, "position": position, "file": name}),
            outcome: None,
        },
        source: AttachSource::File(name.into()),
        expected_digest: None,
        expected_size_bytes: None,
        classification: None,
        derived_from: None,
    }
}

fn finalize_request(completion: &str, last_position: Option<u64>) -> FinalizeSubmission {
    finalize_keyed(KEY, completion, last_position)
}

fn finalize_keyed(key: &str, completion: &str, last_position: Option<u64>) -> FinalizeSubmission {
    FinalizeSubmission {
        handle: "handle_01936f0e-7c6a-7d3e-8a1b-0123456789ab"
            .parse()
            .unwrap(),
        command_id: key.into(),
        request: json!({"key": key, "completion": completion, "last_position": last_position}),
    }
}

fn for_handle(mut submission: FinalizeSubmission, handle: Id) -> FinalizeSubmission {
    submission.handle = handle;
    submission
}

/// A recorder-shaped plan. A `complete` request claims the stream ends at its
/// `last_position` and is refused, sealing nothing, on any other cursor; an
/// `interrupted` one seals whatever the handle accepted. The manifest binds
/// the item map and the sealed snapshot; every staged attachment is retained.
fn plan(context: &FinalizeContext<'_>, request: &Value) -> Result<FinalizePlan, Failure> {
    let last = context.items.last().map(|item| item.position);
    if request["completion"] == "complete" && request["last_position"] != json!(last) {
        let mut failure = Failure::new(
            ErrorCode::RevisionConflict,
            "The claimed last position is not the handle's last accepted position.",
        )
        .at("/last_position");
        failure.error.expected = Some(json!({"last_position": last}));
        return Err(failure);
    }
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
            "completion": request["completion"],
            "items": item_map(context.items),
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

fn finalize<F: DurableFs>(
    store: &mut Store<F>,
    submission: &FinalizeSubmission,
    clock: &mut Ticks,
) -> Result<FinalizedCapture, Failure> {
    let request = submission.request.clone();
    store.finalize_capture(submission, clock, |context| plan(context, &request))
}

fn finalize_events<F: DurableFs>(store: &mut Store<F>, case_id: Id) -> usize {
    store
        .case(case_id)
        .unwrap()
        .events
        .iter()
        .filter(|verified| verified.event.request.input["operation"] == FINALIZE_OPERATION)
        .count()
}

fn blob_path(digest: &Digest) -> PathBuf {
    let hex = digest.as_str().strip_prefix("sha256:").unwrap();
    Path::new(BLOBS).join(&hex[..2]).join(hex)
}

/// Bytes at `path` on the store's filesystem, if present.
fn read_bytes<F: DurableFs>(store: &Store<F>, path: &Path) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    store.fs.read(path).ok()?.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// Every Run record in the workspace and the number of cases.
fn runs_and_cases<F: DurableFs>(store: &mut Store<F>) -> (Vec<Id>, usize) {
    let heads = store.list_cases().unwrap();
    let mut runs = Vec::new();
    for head in &heads {
        let case = store.case(head.case_id).unwrap();
        runs.extend(
            case.state
                .records()
                .filter(|record| record.header.id.kind() == Kind::Run)
                .map(|record| record.header.id),
        );
    }
    (runs, heads.len())
}

fn recovered_items(staging: &RecoveredStaging) -> &[StagedItem] {
    match staging {
        RecoveredStaging::Present { items, .. } | RecoveredStaging::Retired { items } => items,
        RecoveredStaging::Unallocated => &[],
    }
}

// ---------------------------------------------------------------------------
// Begin
// ---------------------------------------------------------------------------

/// Inject a failure at every filesystem call of a `begin`, then crash. The
/// identical retry settles it with exactly one record case and one Run; a
/// begin acknowledged before the crash keeps its Run and handle.
#[test]
fn every_crash_point_of_a_begin_is_settled_by_the_identical_retry_with_one_run() {
    let imports = imports();
    let mut injected = 0;
    let mut committed_unacknowledged = 0;
    let mut acknowledged_first_try = 0;
    let mut consecutive = 0;
    // Past the last call every seed acknowledges; a few in a row end the walk.
    for seed in 1.. {
        if consecutive == 5 {
            break;
        }
        let (mut store, root) = crash_model_store(imports.path());
        store.fs.fail_at(seed);
        let first = store.declare_capture(&declaration());
        consecutive = if first.is_ok() { consecutive + 1 } else { 0 };
        restart(&mut store);
        let (runs, _) = runs_and_cases(&mut store);
        match &first {
            Ok(_) => acknowledged_first_try += 1,
            Err(_) if runs.is_empty() => injected += 1,
            Err(_) => {
                injected += 1;
                committed_unacknowledged += 1;
            }
        }
        assert!(runs.len() <= 1, "seed {seed}: {runs:?}");

        let retried = store
            .declare_capture(&declaration())
            .unwrap_or_else(|error| panic!("seed {seed}: {error:?}"));
        if let Ok(first) = &first {
            assert_eq!(retried.run_id, first.run_id, "seed {seed}");
            assert_eq!(retried.binding, first.binding, "seed {seed}");
            assert_eq!(
                retried.receipt.transaction_id, first.receipt.transaction_id,
                "seed {seed}"
            );
        }
        restart(&mut store);
        let (runs, cases) = runs_and_cases(&mut store);
        assert_eq!(runs, [retried.run_id], "seed {seed}");
        assert_eq!(cases, 1, "seed {seed}");
        let handles = store.capture_handles().unwrap();
        assert_eq!(handles.len(), 1, "seed {seed}");
        assert_eq!(handles[0].staging, StagingState::Present, "seed {seed}");
        // The settled handle accepts its first item.
        store
            .stage_capture_item(&fact(retried.binding.handle, "started", 1), &mut Ticks(0))
            .unwrap_or_else(|error| panic!("seed {seed}: {error:?}"));
        let again = store.declare_capture(&declaration()).unwrap();
        assert_eq!(again.receipt.result, CommitResult::AlreadyCommitted);
        assert_eq!(again.run_id, retried.run_id, "seed {seed}");
        let _ = fs::remove_dir_all(root);
    }
    assert!(injected > 10, "the schedule must reach the begin path");
    assert!(
        committed_unacknowledged > 0,
        "the schedule must crash after the commit and before the acknowledgement"
    );
    assert!(acknowledged_first_try > 0, "the schedule must run past it");
}

// ---------------------------------------------------------------------------
// Finalize crash windows
// ---------------------------------------------------------------------------

/// What a crash in the middle of a finalization left durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Window {
    /// No seal and no event: the handle is still open.
    Open,
    /// Sealed, no retained byte in the canonical store, no event.
    SealedNoEvent,
    /// Sealed, some retained bytes in the canonical store, no event.
    PublishedNoEvent,
    /// The finalizing event is durable; staging is not retired.
    EventDurable,
}

/// Inject a failure at every filesystem call of a finalization, then crash,
/// and classify what is durable. Each window has its own recovery, and every
/// window is reached:
///
/// - open: recovery reports no seal, retirement keeps the staging as open;
/// - sealed or published with no event: recovery names the sealing request, a
///   late item and any other finalize request are refused, retirement keeps
///   the staging as sealed, and the identical retry commits, reusing the
///   published bytes;
/// - event durable: recovery returns the original receipt, `repair staging`
///   retires the staging, and identical finalize and item retries still answer
///   from the journal and the manifest.
#[test]
fn every_finalize_crash_window_recovers_without_a_second_finalization() {
    let imports = imports();
    let retained = hash_bytes(FILES[0].1);
    let mut reached = BTreeMap::<Window, usize>::new();
    let mut lost_acknowledgements = 0;
    let mut consecutive = 0;
    // Past the last call every seed acknowledges; a few in a row end the walk.
    for seed in 1.. {
        if consecutive == 5 {
            break;
        }
        let (mut store, root) = crash_model_store(imports.path());
        let declared = store.declare_capture(&declaration()).unwrap();
        let handle = declared.binding.handle;
        let staging = PathBuf::from(&declared.binding.staging);
        let mut clock = Ticks(0);
        let started = store
            .stage_capture_item(&fact(handle, "started", 1), &mut clock)
            .unwrap();
        let result = store
            .stage_capture_attachment(&file(handle, "result", 2, FILES[0].0), &mut clock)
            .unwrap();
        let request = for_handle(finalize_request("complete", Some(2)), handle);
        restart(&mut store);

        store.fs.fail_at(seed);
        let first = finalize(&mut store, &request, &mut clock);
        consecutive = if first.is_ok() { consecutive + 1 } else { 0 };
        restart(&mut store);

        let events = finalize_events(&mut store, declared.case_id);
        assert!(events <= 1, "seed {seed}");
        let recovered = store.capture_recovery(Some(handle)).unwrap().remove(0);
        let seal = match &recovered.staging {
            RecoveredStaging::Present { items, seal } => {
                assert_eq!(items, &[started.item.clone(), result.item.clone()]);
                seal.clone()
            }
            other => panic!("seed {seed}: staging is not retired here: {other:?}"),
        };
        let published = std::iter::once(&retained)
            .chain(seal.as_ref().map(|seal| &seal.manifest_digest))
            .any(|digest| read_bytes(&store, &blob_path(digest)).is_some());
        let window = match (events, &seal) {
            (1, _) => Window::EventDurable,
            (_, None) => Window::Open,
            (_, Some(_)) if published => Window::PublishedNoEvent,
            (_, Some(_)) => Window::SealedNoEvent,
        };
        *reached.entry(window).or_default() += 1;
        if first.is_ok() {
            assert_eq!(window, Window::EventDurable, "seed {seed}");
        }

        let retirement = store
            .retire_staging(Some(handle))
            .unwrap()
            .handles
            .remove(0);
        match window {
            Window::Open => {
                assert!(recovered.finalized.is_none(), "seed {seed}");
                assert_eq!(retirement.reason, Some(KeptReason::Open), "seed {seed}");
            }
            Window::SealedNoEvent | Window::PublishedNoEvent => {
                let seal = seal.as_ref().unwrap();
                assert!(recovered.finalized.is_none(), "seed {seed}");
                assert_eq!(seal.command_id, KEY, "seed {seed}");
                assert_eq!(seal.sealed_position, Some(2), "seed {seed}");
                assert_eq!(retirement.reason, Some(KeptReason::Sealed), "seed {seed}");
                let late = store
                    .stage_capture_item(&fact(handle, "late", 3), &mut clock)
                    .unwrap_err();
                assert_eq!(
                    (late.error.code, late.error.path.as_deref()),
                    (ErrorCode::RevisionConflict, Some("/handle")),
                    "seed {seed}"
                );
                assert_eq!(late.error.expected.as_ref().unwrap()["seal"], "sealed");
                let other = for_handle(
                    finalize_keyed("run-0001/finalize-again", "interrupted", None),
                    handle,
                );
                let refused = finalize(&mut store, &other, &mut clock).unwrap_err();
                assert_eq!(
                    refused.error.code,
                    ErrorCode::RevisionConflict,
                    "seed {seed}"
                );
            }
            Window::EventDurable => {
                // The process died before the producer read the receipt.
                if first.is_ok() {
                    lost_acknowledgements += 1;
                }
                let finalized = recovered.finalized.as_ref().unwrap();
                if let Ok(first) = &first {
                    assert_eq!(
                        finalized.receipt.transaction_id, first.receipt.transaction_id,
                        "seed {seed}"
                    );
                }
                // `repair staging` retires it; the canonical record answers.
                assert_eq!(
                    retirement.result,
                    RetirementResult::Retired,
                    "seed {seed}: {retirement:?}"
                );
                assert!(!store.exists(&staging.join(BINDING_FILE)).unwrap());
            }
        }

        let retried = finalize(&mut store, &request, &mut clock)
            .unwrap_or_else(|error| panic!("seed {seed} {window:?}: {error:?}"));
        if window == Window::EventDurable {
            assert_eq!(retried.receipt.result, CommitResult::AlreadyCommitted);
            assert_eq!(
                retried.receipt.transaction_id,
                recovered.finalized.as_ref().unwrap().receipt.transaction_id,
                "seed {seed}"
            );
        }
        restart(&mut store);

        // One finalization, its bytes in the canonical store, every item bound.
        assert_eq!(
            finalize_events(&mut store, declared.case_id),
            1,
            "seed {seed}"
        );
        let case = store.case(declared.case_id).unwrap();
        let event = &case
            .events
            .iter()
            .find(|verified| verified.event.request.input["operation"] == FINALIZE_OPERATION)
            .unwrap()
            .event;
        assert_eq!(
            event.request.attachments,
            std::slice::from_ref(&retained),
            "seed {seed}"
        );
        for digest in event
            .request
            .attachments
            .iter()
            .chain(event.manifest_digest.iter())
        {
            let bytes = read_bytes(&store, &blob_path(digest))
                .unwrap_or_else(|| panic!("seed {seed}: {digest} is not retained"));
            assert_eq!(hash_bytes(&bytes), *digest, "seed {seed}");
        }
        let manifest = store
            .finalized_manifest(&case.state, declared.run_id, handle)
            .unwrap()
            .unwrap();
        assert_eq!(manifest.items, [started.item.clone(), result.item.clone()]);
        let sources = case
            .state
            .records()
            .filter(|record| record.header.id.kind() == Kind::Source)
            .count();
        assert_eq!(sources, 1, "seed {seed}: one capture acquisition");

        // Retire whatever is left; identical retries still answer.
        store.retire_staging(Some(handle)).unwrap();
        restart(&mut store);
        let again = finalize(&mut store, &request, &mut clock).unwrap();
        assert_eq!(again.receipt.transaction_id, retried.receipt.transaction_id);
        let replayed = store
            .stage_capture_attachment(&file(handle, "result", 2, FILES[0].0), &mut clock)
            .unwrap();
        assert_eq!(replayed.result, ItemResult::AlreadyAccepted, "seed {seed}");
        assert_eq!(replayed.item, result.item, "seed {seed}");
        let late = store
            .stage_capture_item(&fact(handle, "late", 3), &mut clock)
            .unwrap_err();
        assert_eq!(late.error.expected.as_ref().unwrap()["seal"], "finalized");
        let _ = fs::remove_dir_all(root);
    }
    for window in [
        Window::Open,
        Window::SealedNoEvent,
        Window::PublishedNoEvent,
        Window::EventDurable,
    ] {
        assert!(
            reached.contains_key(&window),
            "the schedule must reach {window:?}: {reached:?}"
        );
    }
    assert!(
        lost_acknowledgements > 0,
        "the schedule must run past the event and lose its acknowledgement"
    );
}

// ---------------------------------------------------------------------------
// The last item and the seal
// ---------------------------------------------------------------------------

/// The last attachment is in flight (its position checked, its bytes being
/// copied) when finalize arrives: finalize cannot seal until the attachment
/// is durable, so the attachment is acknowledged and the manifest binds it.
/// Then the reverse: finalize has read the items and not yet sealed when the
/// last item arrives: the item cannot be acknowledged, and after the seal it
/// gets the closed-handle response, so the manifest binds every acknowledged
/// item and nothing else.
#[test]
fn the_last_item_and_the_seal_are_serialized_by_the_handle_lock() {
    let imports = imports();

    // The attachment holds the handle while finalize tries to seal.
    let (mut store, root) = crash_model_store(imports.path());
    let mut other = twin(&store);
    let declared = store.declare_capture(&declaration()).unwrap();
    let handle = declared.binding.handle;
    let staging = PathBuf::from(&declared.binding.staging);
    let mut clock = Ticks(0);
    store
        .stage_capture_item(&fact(handle, "started", 1), &mut clock)
        .unwrap();
    let mut raced = None;
    let attached = store
        .stage_attachment_observed(
            &file(handle, "result", 2, FILES[0].0),
            &mut clock,
            &mut |_| {
                if raced.is_none() {
                    let request = for_handle(finalize_request("interrupted", None), handle);
                    raced = Some(finalize(&mut other, &request, &mut Ticks(100)));
                }
            },
        )
        .unwrap();
    let raced = raced.expect("the copy reports progress").unwrap_err();
    assert_eq!(raced.error.code, ErrorCode::WorkspaceBusy);
    assert!(!store.exists(&staging.join(SEAL_FILE)).unwrap());
    assert_eq!(attached.result, ItemResult::Accepted);
    assert_eq!(attached.item.position, 2);
    let finalized = finalize(
        &mut store,
        &for_handle(finalize_request("interrupted", None), handle),
        &mut clock,
    )
    .unwrap();
    let case = store.case(declared.case_id).unwrap();
    let manifest = store
        .finalized_manifest(&case.state, declared.run_id, handle)
        .unwrap()
        .unwrap();
    assert_eq!(manifest.items.last(), Some(&attached.item));
    assert_eq!(json!(manifest.digest), finalized.section["manifest_digest"]);
    let _ = fs::remove_dir_all(root);

    // Finalize holds the handle while the last item tries to land.
    let (mut store, root) = crash_model_store(imports.path());
    let mut other = twin(&store);
    let declared = store.declare_capture(&declaration()).unwrap();
    let handle = declared.binding.handle;
    let staging = PathBuf::from(&declared.binding.staging);
    let started = store
        .stage_capture_item(&fact(handle, "started", 1), &mut clock)
        .unwrap();
    let last = file(handle, "result", 2, FILES[0].0);
    let mut raced = None;
    let request = for_handle(finalize_request("interrupted", None), handle);
    let finalized = store
        .finalize_capture(&request, &mut clock, |context| {
            raced = Some(other.stage_capture_attachment(&last, &mut Ticks(100)));
            plan(context, &request.request)
        })
        .unwrap();
    let raced = raced.unwrap().unwrap_err();
    assert_eq!(raced.error.code, ErrorCode::WorkspaceBusy);
    assert!(store.exists(&staging.join(SEAL_FILE)).unwrap());
    // The retry after the seal is refused as closed; nothing was staged.
    let closed = other
        .stage_capture_attachment(&last, &mut Ticks(200))
        .unwrap_err();
    assert_eq!(
        (closed.error.code, closed.error.path.as_deref()),
        (ErrorCode::RevisionConflict, Some("/handle"))
    );
    assert_eq!(
        closed.error.expected.as_ref().unwrap()["manifest_digest"],
        finalized.section["manifest_digest"]
    );
    let items = store.capture_items(handle).unwrap();
    assert_eq!(items.items, std::slice::from_ref(&started.item));
    assert!(items.sealed);
    let case = store.case(declared.case_id).unwrap();
    let manifest = store
        .finalized_manifest(&case.state, declared.run_id, handle)
        .unwrap()
        .unwrap();
    assert_eq!(manifest.items, [started.item]);
    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// The commit lock and external work
// ---------------------------------------------------------------------------

/// Another writer commits while the recorder copies producer bytes and while
/// it plans the manifest, and between every call: the canonical commit lock
/// is never held across work whose duration the producer controls. On the
/// real filesystem the lock is probed without waiting and another store
/// commits a case; on the crash model the exact set of held locks is the
/// handle lock alone.
#[test]
fn the_commit_lock_is_never_held_across_producer_work() {
    // Real filesystem and real advisory locks.
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let outputs = temporary.path().join("outputs");
    fs::create_dir(&outputs).unwrap();
    // Large enough that the copy reports progress more than once.
    fs::write(outputs.join("result.bin"), vec![5_u8; 300 * 1024]).unwrap();
    Workspace::init(&root, false).unwrap();
    let workspace = with_imports(&root, &outputs);
    let lock = root.join(COORDINATION_LOCK);
    let free = || CommitLock::acquire(&lock, Duration::ZERO).is_ok();
    let mut concurrent = 0;
    let mut commit_elsewhere = |label: &str| {
        let mut other = Store::open(workspace.clone());
        let command = json!({
            "schema_version": "inv.command.v1",
            "command_id": format!("concurrent-{concurrent}-{label}"),
            "actor": "agent:another-writer", "case_id": null, "preconditions": [],
            "operations": [{"op": "case.create",
                "payload": {"title": "Concurrent work", "mode": "explore"}}],
        });
        concurrent += 1;
        other
            .record(&serde_json::to_vec(&command).unwrap())
            .unwrap_or_else(|error| panic!("{label}: {error:?}"));
    };

    let mut store = Store::open(workspace.clone());
    let mut clock = Ticks(0);
    let declared = store.declare_capture(&declaration()).unwrap();
    let handle = declared.binding.handle;
    assert!(free(), "after begin");
    commit_elsewhere("between begin and append");
    store
        .stage_capture_item(&fact(handle, "started", 1), &mut clock)
        .unwrap();
    assert!(free(), "after append");
    let mut during_copy = 0;
    store
        .stage_attachment_observed(
            &file(handle, "result", 2, "result.bin"),
            &mut clock,
            &mut |_| {
                assert!(free(), "while producer bytes are copied");
                if during_copy == 0 {
                    commit_elsewhere("during the copy");
                }
                during_copy += 1;
            },
        )
        .unwrap();
    assert!(during_copy > 1, "the copy must report progress");
    assert!(free(), "after attach");
    let request = for_handle(finalize_request("complete", Some(2)), handle);
    let mut planned = false;
    store
        .finalize_capture(&request, &mut clock, |context| {
            assert!(free(), "while the manifest is planned");
            commit_elsewhere("during planning");
            planned = true;
            plan(context, &request.request)
        })
        .unwrap();
    assert!(planned);
    assert!(free(), "after finalize");
    assert!(store.retire_staging(Some(handle)).is_ok());
    assert!(free(), "after retirement");
    assert_eq!(store.list_cases().unwrap().len(), 4);

    // Crash model: exactly which locks each window holds.
    let imports = imports();
    let (mut store, root) = crash_model_store(imports.path());
    let declared = store.declare_capture(&declaration()).unwrap();
    let handle = declared.binding.handle;
    let handle_lock = PathBuf::from(&declared.binding.staging).join(HANDLE_LOCK_FILE);
    assert!(store.fs.held_locks().is_empty());
    let memory = store.fs.clone();
    let mut observed = Vec::new();
    store
        .stage_attachment_observed(
            &file(handle, "result", 1, FILES[0].0),
            &mut clock,
            &mut |_| observed.push(memory.held_locks()),
        )
        .unwrap();
    assert!(!observed.is_empty());
    for held in observed {
        assert_eq!(held, std::slice::from_ref(&handle_lock));
    }
    assert!(store.fs.held_locks().is_empty());
    let request = for_handle(finalize_request("complete", Some(1)), handle);
    let mut held = None;
    store
        .finalize_capture(&request, &mut clock, |context| {
            held = Some(memory.held_locks());
            plan(context, &request.request)
        })
        .unwrap();
    assert_eq!(held.unwrap(), [handle_lock]);
    assert!(store.fs.held_locks().is_empty());
    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// Property: random interleavings with crashes and retries
// ---------------------------------------------------------------------------

const DEFAULT_SEEDS: [u64; 2] = [0xCA97_0714, 0x5EA1_0F14];
const DEFAULT_CASES: u32 = 16;
/// Upper bound of an injected fault's call number. An item takes about 35 to
/// 45 filesystem calls and a small finalization about 70; a fault beyond an
/// operation's last call crashes after it.
const MAX_FAULT: usize = 120;

/// What a producer's item carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body {
    Fact,
    /// An attachment of one of the fixture files.
    File(usize),
    /// An attachment the producer declares missing.
    Missing,
}

/// One producer action. `fault` injects a failure at that filesystem call of
/// the action; the process then crashes, whatever the action returned.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// Begin until acknowledged, then send a new item at the producer's next
    /// position, even while an earlier item is unacknowledged.
    Next { body: Body, fault: Option<usize> },
    /// Resend the earliest unacknowledged item identically.
    Resend { fault: Option<usize> },
    /// Send a new item one position beyond the producer's next.
    Gap,
    /// Resend an acknowledged item identically.
    Replay { pick: usize, fault: Option<usize> },
    /// Resend an acknowledged item's key with a changed request.
    Changed { pick: usize },
    /// Finalize, resending an unacknowledged finalize request identically.
    Finalize {
        complete: bool,
        fault: Option<usize>,
    },
    /// `repair staging` for the handle.
    Retire { fault: Option<usize> },
    /// Lose power: only synced state survives.
    Crash,
}

fn step_strategy() -> impl Strategy<Value = Step> {
    // Most faults land inside an item call; some reach deep into finalize.
    let fault = || prop::option::weighted(0.35, prop_oneof![3 => 1..=48_usize, 2 => 1..=MAX_FAULT]);
    let body = prop_oneof![
        3 => Just(Body::Fact),
        2 => (0..FILES.len()).prop_map(Body::File),
        1 => Just(Body::Missing),
    ];
    prop_oneof![
        8 => (body, fault()).prop_map(|(body, fault)| Step::Next { body, fault }),
        3 => fault().prop_map(|fault| Step::Resend { fault }),
        1 => Just(Step::Gap),
        2 => (0..64_usize, fault()).prop_map(|(pick, fault)| Step::Replay { pick, fault }),
        1 => (0..64_usize).prop_map(|pick| Step::Changed { pick }),
        2 => (any::<bool>(), fault())
            .prop_map(|(complete, fault)| Step::Finalize { complete, fault }),
        1 => fault().prop_map(|fault| Step::Retire { fault }),
        2 => Just(Step::Crash),
    ]
}

/// One item the producer has sent at least once, at its position.
#[derive(Debug, Clone)]
struct Sent {
    key: String,
    position: u64,
    body: Body,
}

impl Sent {
    fn request(&self, handle: Id) -> Value {
        let body = match self.body {
            Body::Fact => json!({"fact": "phase"}),
            Body::File(index) => json!({"file": FILES[index].0}),
            Body::Missing => json!({"missing": "not produced"}),
        };
        json!({"handle": handle, "key": self.key, "position": self.position, "body": body})
    }

    fn submit(
        &self,
        store: &mut Store<MemoryFs>,
        handle: Id,
        clock: &mut Ticks,
        changed: bool,
    ) -> Result<ItemAcceptance, Failure> {
        let mut request = self.request(handle);
        if changed {
            request["changed"] = json!(true);
        }
        let item = ItemSubmission {
            operation: match self.body {
                Body::Fact => ItemOperation::Append,
                Body::File(_) | Body::Missing => ItemOperation::Attach,
            },
            handle,
            key: self.key.clone(),
            position: self.position,
            request,
            outcome: None,
        };
        let source = match self.body {
            Body::Fact => return store.stage_capture_item(&item, clock),
            Body::File(index) => AttachSource::File(FILES[index].0.into()),
            Body::Missing => AttachSource::Missing,
        };
        store.stage_capture_attachment(
            &AttachSubmission {
                item,
                source,
                expected_digest: None,
                expected_size_bytes: None,
                classification: None,
                derived_from: None,
            },
            clock,
        )
    }
}

/// The producer's knowledge and the crash-model workspace it records into.
struct Harness {
    store: Store<MemoryFs>,
    root: PathBuf,
    clock: Ticks,
    /// The acknowledged begin.
    declared: Option<DeclaredCapture>,
    /// Items sent at least once; `sent[p - 1]` is at position `p`.
    sent: Vec<Sent>,
    /// Acknowledged receipts by position.
    acked: BTreeMap<u64, StagedItem>,
    /// Positions refused as closed: known never accepted.
    refused: BTreeSet<u64>,
    /// An unacknowledged finalize request, resent identically.
    pending_finalize: Option<FinalizeSubmission>,
    finalized: Option<FinalizedCapture>,
    counter: usize,
    log: Vec<String>,
    /// How often each protocol path was taken, for coverage.
    stats: BTreeMap<&'static str, usize>,
}

type Checked = Result<(), String>;

macro_rules! ensure {
    ($condition:expr, $($message:tt)+) => {
        if !$condition {
            return Err(format!($($message)+));
        }
    };
}

impl Harness {
    fn new(imports: &Path) -> Self {
        let (store, root) = crash_model_store(imports);
        Self {
            store,
            root,
            clock: Ticks(0),
            declared: None,
            sent: Vec::new(),
            acked: BTreeMap::new(),
            refused: BTreeSet::new(),
            pending_finalize: None,
            finalized: None,
            counter: 0,
            log: Vec::new(),
            stats: BTreeMap::new(),
        }
    }

    fn count(&mut self, path: &'static str) {
        *self.stats.entry(path).or_default() += 1;
    }

    fn handle(&self) -> Option<Id> {
        self.declared
            .as_ref()
            .map(|declared| declared.binding.handle)
    }

    fn crash(&mut self) {
        restart(&mut self.store);
    }

    /// Run one step; a faulted step ends in a crash.
    fn step(&mut self, step: Step) -> Checked {
        let fault = match step {
            Step::Next { fault, .. }
            | Step::Resend { fault }
            | Step::Replay { fault, .. }
            | Step::Finalize { fault, .. }
            | Step::Retire { fault } => fault,
            Step::Gap | Step::Changed { .. } | Step::Crash => None,
        };
        if let Some(call) = fault {
            self.store.fs.fail_at(call);
        }
        let outcome = self.act(step, fault.is_some());
        if fault.is_some() || matches!(step, Step::Crash) {
            self.count("crash");
            self.crash();
        }
        outcome?;
        self.check()
    }

    fn act(&mut self, step: Step, faulted: bool) -> Checked {
        let Some(handle) = self.handle() else {
            if matches!(step, Step::Next { .. } | Step::Resend { .. }) {
                return self.begin();
            }
            return Ok(());
        };
        match step {
            Step::Next { body, .. } => {
                self.counter += 1;
                let sent = Sent {
                    key: format!("item-{}", self.counter),
                    position: self.sent.len() as u64 + 1,
                    body,
                };
                self.sent.push(sent.clone());
                let result = sent.submit(&mut self.store, handle, &mut self.clock, false);
                self.settle_item(&sent, result, faulted)
            }
            Step::Resend { .. } => {
                let Some(sent) = self.unacknowledged().next().cloned() else {
                    return Ok(());
                };
                let result = sent.submit(&mut self.store, handle, &mut self.clock, false);
                self.settle_item(&sent, result, faulted)
            }
            Step::Gap => {
                self.counter += 1;
                let sent = Sent {
                    key: format!("gap-{}", self.counter),
                    position: self.sent.len() as u64 + 2,
                    body: Body::Fact,
                };
                match sent.submit(&mut self.store, handle, &mut self.clock, false) {
                    Ok(accepted) => Err(format!("a gap was accepted: {accepted:?}")),
                    Err(failure) => {
                        ensure!(
                            failure.error.code == ErrorCode::RevisionConflict,
                            "a gap was refused with {failure:?}"
                        );
                        self.count("gap refused");
                        Ok(())
                    }
                }
            }
            Step::Replay { pick, .. } => {
                let Some((position, item)) = self.pick(pick) else {
                    return Ok(());
                };
                let sent = self.sent[position as usize - 1].clone();
                match sent.submit(&mut self.store, handle, &mut self.clock, false) {
                    Ok(replayed) => {
                        ensure!(
                            replayed.result == ItemResult::AlreadyAccepted && replayed.item == item,
                            "an identical retry of position {position} returned {replayed:?}, acknowledged {item:?}"
                        );
                        self.count(if self.finalized.is_some() {
                            "replay after finalize"
                        } else {
                            "replay"
                        });
                        Ok(())
                    }
                    Err(failure) if faulted => {
                        self.log
                            .push(format!("  replay failed: {:?}", failure.error.code));
                        Ok(())
                    }
                    Err(failure) => Err(format!(
                        "an identical retry of position {position} was refused: {failure:?}"
                    )),
                }
            }
            Step::Changed { pick } => {
                let Some((position, _)) = self.pick(pick) else {
                    return Ok(());
                };
                let sent = self.sent[position as usize - 1].clone();
                match sent.submit(&mut self.store, handle, &mut self.clock, true) {
                    Err(failure) if failure.error.code == ErrorCode::IdempotencyKeyReused => {
                        self.count("changed retry refused");
                        Ok(())
                    }
                    other => Err(format!(
                        "a changed retry of position {position} returned {other:?}"
                    )),
                }
            }
            Step::Finalize { complete, .. } => self.finalize(handle, complete, faulted),
            Step::Retire { .. } => self.retire(handle, faulted),
            Step::Crash => Ok(()),
        }
    }

    fn begin(&mut self) -> Checked {
        match self.store.declare_capture(&declaration()) {
            Ok(declared) => {
                self.declared = Some(declared);
                Ok(())
            }
            Err(failure) => {
                self.log
                    .push(format!("  begin failed: {:?}", failure.error.code));
                Ok(())
            }
        }
    }

    fn unacknowledged(&self) -> impl Iterator<Item = &Sent> {
        self.sent
            .iter()
            .filter(|sent| !self.acked.contains_key(&sent.position))
            .filter(|sent| !self.refused.contains(&sent.position))
    }

    fn pick(&self, pick: usize) -> Option<(u64, StagedItem)> {
        if self.acked.is_empty() {
            return None;
        }
        self.acked
            .iter()
            .nth(pick % self.acked.len())
            .map(|(position, item)| (*position, item.clone()))
    }

    /// Record what a sent item's answer tells the producer.
    fn settle_item(
        &mut self,
        sent: &Sent,
        result: Result<ItemAcceptance, Failure>,
        faulted: bool,
    ) -> Checked {
        match result {
            Ok(accepted) => {
                ensure!(
                    accepted.item.position == sent.position && accepted.item.key == sent.key,
                    "item {sent:?} was acknowledged as {accepted:?}"
                );
                if let Some(earlier) = self.acked.get(&sent.position) {
                    ensure!(
                        *earlier == accepted.item,
                        "position {} was acknowledged twice differently",
                        sent.position
                    );
                }
                self.log.push(format!(
                    "  acknowledged {} at {} ({:?})",
                    sent.key, sent.position, accepted.result
                ));
                self.count(match accepted.result {
                    ItemResult::Accepted => "item accepted",
                    ItemResult::AlreadyAccepted => "uncertain item settled by retry",
                });
                self.acked.insert(sent.position, accepted.item);
                Ok(())
            }
            Err(failure)
                if failure.error.code == ErrorCode::RevisionConflict
                    && failure.error.path.as_deref() == Some("/handle") =>
            {
                // Closed: this and every later unacknowledged item are refused.
                self.log.push(format!("  closed at {}", sent.position));
                self.count("item refused as closed");
                let refused: Vec<u64> = self
                    .unacknowledged()
                    .filter(|other| other.position >= sent.position)
                    .map(|other| other.position)
                    .collect();
                self.refused.extend(refused);
                Ok(())
            }
            Err(failure)
                if failure.error.code == ErrorCode::RevisionConflict
                    && failure.error.path.as_deref() == Some("/position") =>
            {
                // An earlier item was lost: resume at the expected position.
                let expected = failure.error.expected.as_ref().unwrap()["position"]
                    .as_u64()
                    .unwrap();
                ensure!(
                    expected < sent.position,
                    "position {} was refused expecting {expected}",
                    sent.position
                );
                ensure!(
                    self.acked.keys().all(|position| *position < expected),
                    "the handle expects position {expected} after acknowledging {:?}",
                    self.acked.keys().collect::<Vec<_>>()
                );
                self.log
                    .push(format!("  resume at {expected} (sent {})", sent.position));
                self.count("lost item resumed at expected position");
                let mut dropped = self.sent.split_off(expected as usize - 1);
                for (offset, item) in dropped.iter_mut().enumerate() {
                    item.position = expected + offset as u64;
                }
                self.refused.retain(|position| *position < expected);
                self.sent.extend(dropped);
                Ok(())
            }
            Err(failure) if faulted => {
                self.log
                    .push(format!("  unacknowledged: {:?}", failure.error.code));
                Ok(())
            }
            Err(failure) => Err(format!("item {sent:?} was refused: {failure:?}")),
        }
    }

    fn finalize(&mut self, handle: Id, complete: bool, faulted: bool) -> Checked {
        let submission = match &self.pending_finalize {
            Some(pending) => pending.clone(),
            None => for_handle(
                finalize_request(
                    if complete { "complete" } else { "interrupted" },
                    // The producer's cursor: its last item not refused as closed.
                    self.sent
                        .iter()
                        .map(|sent| sent.position)
                        .filter(|position| !self.refused.contains(position))
                        .max(),
                ),
                handle,
            ),
        };
        match finalize(&mut self.store, &submission, &mut self.clock) {
            Ok(finalized) => {
                if let Some(earlier) = &self.finalized {
                    ensure!(
                        earlier.receipt.transaction_id == finalized.receipt.transaction_id
                            && earlier.section == finalized.section,
                        "a second finalization: {earlier:?} then {finalized:?}"
                    );
                }
                self.log.push(format!(
                    "  finalized {:?} ({:?})",
                    submission.request["completion"], finalized.receipt.result
                ));
                self.count(match finalized.receipt.result {
                    CommitResult::AlreadyCommitted => "finalize answered by the journal",
                    _ => "finalize committed",
                });
                self.finalized = Some(finalized);
                self.pending_finalize = Some(submission);
                Ok(())
            }
            Err(failure)
                if failure.error.code == ErrorCode::RevisionConflict
                    && failure.error.path.as_deref() == Some("/last_position") =>
            {
                // A stale claimed cursor seals nothing; plan again later.
                self.log.push("  finalize refused: stale cursor".into());
                self.count("finalize refused: stale cursor");
                self.pending_finalize = None;
                Ok(())
            }
            Err(failure) if faulted => {
                self.log.push(format!(
                    "  finalize unacknowledged: {:?}",
                    failure.error.code
                ));
                self.count("finalize unacknowledged");
                self.pending_finalize = Some(submission);
                Ok(())
            }
            Err(failure) => Err(format!("finalize was refused: {failure:?}")),
        }
    }

    fn retire(&mut self, handle: Id, faulted: bool) -> Checked {
        // A faulted step must spend its fault on the retirement itself.
        let finalized = if faulted {
            None
        } else {
            Some(self.journal_finalized(handle)?)
        };
        match self.store.retire_staging(Some(handle)) {
            Ok(report) => {
                let result = report.handles[0].result;
                self.log.push(format!("  retirement {result:?}"));
                if result == RetirementResult::Retired {
                    self.count("staging retired");
                }
                if let Some(finalized) = finalized {
                    ensure!(
                        (result != RetirementResult::Kept) == finalized,
                        "retirement {report:?} with the Run finalized: {finalized}"
                    );
                }
                Ok(())
            }
            Err(failure) if faulted => {
                self.log
                    .push(format!("  retirement failed: {:?}", failure.error.code));
                Ok(())
            }
            Err(failure) => Err(format!("retirement failed: {failure:?}")),
        }
    }

    fn journal_finalized(&mut self, handle: Id) -> Result<bool, String> {
        let recovered = self
            .store
            .capture_recovery(Some(handle))
            .map_err(|failure| format!("recovery failed: {failure:?}"))?;
        Ok(recovered[0].finalized.is_some())
    }

    /// The invariants, read without writing anything.
    fn check(&mut self) -> Checked {
        let held = self.store.fs.held_locks();
        ensure!(held.is_empty(), "locks outlived their call: {held:?}");
        let (runs, cases) = runs_and_cases(&mut self.store);
        ensure!(
            runs.len() <= 1 && cases <= 1,
            "one attempt has {} Runs in {cases} cases",
            runs.len()
        );
        let handles = self
            .store
            .capture_handles()
            .map_err(|failure| format!("handles: {failure:?}"))?;
        ensure!(handles.len() <= 1, "two handles: {handles:?}");
        let Some(declared) = handles.first() else {
            ensure!(
                self.declared.is_none(),
                "an acknowledged begin is not recoverable"
            );
            return Ok(());
        };
        if let Some(acknowledged) = &self.declared {
            ensure!(
                declared.binding == acknowledged.binding && declared.run_id == acknowledged.run_id,
                "the recovered handle differs from the acknowledged begin"
            );
        }
        let handle = declared.binding.handle;
        let recovered = self
            .store
            .capture_recovery(Some(handle))
            .map_err(|failure| format!("recovery: {failure:?}"))?
            .remove(0);
        let items = recovered_items(&recovered.staging).to_vec();

        // Acknowledged implies recoverable, at its position, with its receipt.
        for (position, item) in &self.acked {
            ensure!(
                items.get(*position as usize - 1) == Some(item),
                "acknowledged position {position} is not recoverable: {:?}",
                items.get(*position as usize - 1)
            );
        }
        // Every durable item is one the producer sent, at the position sent.
        for item in &items {
            let sent = self.sent.get(item.position as usize - 1);
            let expected = sent.map(|sent| (sent.key.clone(), sent.request(handle)));
            ensure!(
                expected.as_ref() == Some(&(item.key.clone(), item.request.clone())),
                "durable item {} was never sent there: {item:?}",
                item.position
            );
            ensure!(
                !self.refused.contains(&item.position),
                "a refused item is durable at {}",
                item.position
            );
        }
        // Acknowledged attachment bytes are retained.
        let finalized = recovered.finalized.is_some();
        for item in self.acked.values() {
            let Some(digest) = AttachOutcome::of(item)
                .map_err(|failure| format!("{failure:?}"))?
                .and_then(|outcome| outcome.digest)
            else {
                continue;
            };
            let path = match &recovered.staging {
                RecoveredStaging::Present { .. } if !finalized => {
                    PathBuf::from(&declared.binding.staging).join(format!(
                        "blobs/{}",
                        digest.as_str().trim_start_matches("sha256:")
                    ))
                }
                _ => blob_path(&digest),
            };
            let bytes = read_bytes(&self.store, &path);
            ensure!(
                bytes.as_deref().map(hash_bytes) == Some(digest.clone()),
                "acknowledged bytes {digest} are not at {}",
                path.display()
            );
        }

        match &recovered.finalized {
            Some(committed) => {
                let events = finalize_events(&mut self.store, declared.case_id);
                ensure!(events == 1, "{events} finalizations of one Run");
                let case = self
                    .store
                    .case(declared.case_id)
                    .map_err(|failure| format!("{failure:?}"))?;
                let manifest = self
                    .store
                    .finalized_manifest(&case.state, declared.run_id, handle)
                    .map_err(|failure| format!("manifest: {failure:?}"))?
                    .ok_or("a finalized Run without a manifest")?;
                ensure!(
                    manifest.items == items,
                    "the manifest differs from the recovered items"
                );
                if let Some(acknowledged) = &self.finalized {
                    ensure!(
                        acknowledged.receipt.transaction_id == committed.receipt.transaction_id,
                        "the acknowledged finalization is not the committed one"
                    );
                }
            }
            None => ensure!(
                self.finalized.is_none(),
                "an acknowledged finalization is not recoverable"
            ),
        }
        Ok(())
    }

    /// Settle without faults as a well-behaved producer does: resolve every
    /// unacknowledged item, finalize, retire, lose power. Then the manifest
    /// binds exactly the acknowledged items, and every one still replays.
    fn settle(&mut self) -> Checked {
        self.log.push("settle".into());
        if self.handle().is_none() {
            self.begin()?;
            self.check()?;
        }
        let handle = self
            .handle()
            .ok_or("an unfaulted begin was not acknowledged")?;
        for _ in 0..(self.sent.len() * 2 + 4) {
            if self.unacknowledged().next().is_none() {
                break;
            }
            self.step(Step::Resend { fault: None })?;
        }
        ensure!(
            self.unacknowledged().next().is_none(),
            "unacknowledged items remain: {:?}",
            self.unacknowledged().collect::<Vec<_>>()
        );
        for _ in 0..3 {
            if self.finalized.is_some() {
                break;
            }
            self.step(Step::Finalize {
                complete: true,
                fault: None,
            })?;
        }
        ensure!(self.finalized.is_some(), "an unfaulted finalize failed");
        self.step(Step::Retire { fault: None })?;
        self.step(Step::Crash)?;

        let recovered = self
            .store
            .capture_recovery(Some(handle))
            .map_err(|failure| format!("{failure:?}"))?
            .remove(0);
        let items = match &recovered.staging {
            RecoveredStaging::Retired { items } => items.clone(),
            other => return Err(format!("staging is not retired: {other:?}")),
        };
        let acknowledged: Vec<StagedItem> = self.acked.values().cloned().collect();
        ensure!(
            items == acknowledged,
            "the manifest binds {:?}, the producer acknowledged {:?}",
            items.iter().map(|item| &item.key).collect::<Vec<_>>(),
            acknowledged
                .iter()
                .map(|item| &item.key)
                .collect::<Vec<_>>()
        );
        for (position, item) in self.acked.clone() {
            let sent = self.sent[position as usize - 1].clone();
            let replayed = sent
                .submit(&mut self.store, handle, &mut self.clock, false)
                .map_err(|failure| format!("replay after retirement: {failure:?}"))?;
            ensure!(replayed.item == item, "replay after retirement differs");
        }
        // One acquisition: one capture Source and one Artifact per retained
        // attachment plus the manifest.
        let declared = self.declared.clone().unwrap();
        let case = self
            .store
            .case(declared.case_id)
            .map_err(|failure| format!("{failure:?}"))?;
        let count = |kind| {
            case.state
                .records()
                .filter(|record| record.header.id.kind() == kind)
                .count()
        };
        let retained = acknowledged
            .iter()
            .filter(|item| {
                AttachOutcome::of(item)
                    .ok()
                    .flatten()
                    .is_some_and(|outcome| outcome.digest.is_some())
            })
            .count();
        ensure!(count(Kind::Source) == 1, "{} Sources", count(Kind::Source));
        ensure!(
            count(Kind::Artifact) == retained + 1,
            "{} Artifacts for {retained} retained attachments",
            count(Kind::Artifact)
        );
        self.check()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

type Stats = BTreeMap<&'static str, usize>;

/// Run one step list and settle it. On failure, the step log up to the
/// failure; on success, the protocol paths it took.
fn execute(steps: &[Step], imports: &Path) -> Result<Stats, String> {
    let mut harness = Harness::new(imports);
    for (index, step) in steps.iter().enumerate() {
        harness.log.push(format!("#{index} {step:?}"));
        if let Err(message) = harness.step(*step) {
            return Err(format!("{message}\n{}", harness.log.join("\n")));
        }
    }
    harness
        .settle()
        .map_err(|message| format!("{message}\n{}", harness.log.join("\n")))?;
    Ok(std::mem::take(&mut harness.stats))
}

fn seeds() -> Vec<u64> {
    match std::env::var("INV_CAPTURE_SEED") {
        Ok(seed) => vec![seed.trim().parse().expect("INV_CAPTURE_SEED is a u64")],
        Err(_) => DEFAULT_SEEDS.to_vec(),
    }
}

fn cases() -> u32 {
    std::env::var("INV_CAPTURE_CASES")
        .ok()
        .map(|cases| cases.trim().parse().expect("INV_CAPTURE_CASES is a u32"))
        .unwrap_or(DEFAULT_CASES)
}

fn runner(seed: u64, cases: u32) -> TestRunner {
    let mut bytes = [0_u8; 32];
    for chunk in bytes.chunks_mut(8) {
        chunk.copy_from_slice(&seed.to_le_bytes());
    }
    TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            max_shrink_iters: 256,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &bytes),
    )
}

/// Random interleavings of begin, append, attach, gaps, identical and changed
/// retries, finalize, retirement and power loss, with injected faults, keep
/// every acknowledged item recoverable at its position, record one Run and
/// one finalization, and finalize a manifest that binds exactly the
/// acknowledged items.
#[test]
fn random_interleavings_with_crashes_keep_every_acknowledged_item() {
    let imports = imports();
    let stats = std::cell::RefCell::new(Stats::new());
    for seed in seeds() {
        let mut runner = runner(seed, cases());
        let result = runner.run(&prop::collection::vec(step_strategy(), 1..32), |steps| {
            let taken = execute(&steps, imports.path()).map_err(TestCaseError::fail)?;
            let mut stats = stats.borrow_mut();
            for (path, count) in taken {
                *stats.entry(path).or_default() += count;
            }
            Ok(())
        });
        match result {
            Ok(()) => {}
            Err(TestError::Fail(reason, steps)) => panic!(
                "capture interleaving failed\nseed: {seed:#x} (rerun with INV_CAPTURE_SEED={seed})\nshrunk steps: {steps:?}\n{reason}"
            ),
            Err(error) => panic!("seed {seed:#x}: {error}"),
        }
    }
    // The default run must reach every protocol path it qualifies.
    let stats = stats.into_inner();
    if std::env::var_os("INV_CAPTURE_STATS").is_some() {
        eprintln!("capture interleaving paths: {stats:#?}");
    }
    if std::env::var_os("INV_CAPTURE_SEED").is_none() && cases() >= DEFAULT_CASES {
        for path in [
            "crash",
            "item accepted",
            "uncertain item settled by retry",
            "lost item resumed at expected position",
            "item refused as closed",
            "gap refused",
            "replay",
            "replay after finalize",
            "changed retry refused",
            "finalize committed",
            "finalize answered by the journal",
            "finalize unacknowledged",
            "finalize refused: stale cursor",
            "staging retired",
        ] {
            assert!(
                stats.get(path).is_some_and(|count| *count > 0),
                "the default schedules never reach {path:?}: {stats:?}"
            );
        }
    }
}

/// Step lists kept as regressions: every failing list the property test
/// finds, shrunk, and hand-pinned interleavings of the §20.3 cases. No
/// default or long run (1,200 cases) has failed yet; the pinned lists keep
/// the scenarios fixed whatever the generator draws.
const REGRESSIONS: &[&[Step]] = &[
    // Begin, a fact, an attachment lost mid-call, the next item sent past
    // it, a gap, the lost item resent, an interrupted finalize lost between
    // seal and commit, a late item, a changed retry, the identical finalize,
    // retirement, and a replay after it.
    &[
        Step::Next {
            body: Body::Fact,
            fault: None,
        },
        Step::Next {
            body: Body::Fact,
            fault: None,
        },
        Step::Next {
            body: Body::File(0),
            fault: Some(12),
        },
        Step::Next {
            body: Body::Fact,
            fault: None,
        },
        Step::Gap,
        Step::Resend { fault: None },
        Step::Next {
            body: Body::Missing,
            fault: None,
        },
        Step::Finalize {
            complete: false,
            fault: Some(40),
        },
        Step::Next {
            body: Body::Fact,
            fault: None,
        },
        Step::Changed { pick: 0 },
        Step::Finalize {
            complete: false,
            fault: None,
        },
        Step::Retire { fault: None },
        Step::Replay {
            pick: 1,
            fault: None,
        },
        Step::Crash,
    ],
    // A begin lost after its commit, then power loss after every early call.
    &[
        Step::Next {
            body: Body::Fact,
            fault: Some(20),
        },
        Step::Next {
            body: Body::Fact,
            fault: None,
        },
        Step::Next {
            body: Body::File(1),
            fault: Some(30),
        },
        Step::Crash,
        Step::Resend { fault: Some(5) },
        Step::Finalize {
            complete: true,
            fault: Some(60),
        },
        Step::Crash,
        Step::Finalize {
            complete: true,
            fault: None,
        },
        Step::Retire { fault: Some(8) },
        Step::Replay {
            pick: 0,
            fault: Some(3),
        },
    ],
];

#[test]
fn committed_capture_interleaving_regressions_still_pass() {
    let imports = imports();
    for (index, steps) in REGRESSIONS.iter().enumerate() {
        if let Err(message) = execute(steps, imports.path()).map(drop) {
            panic!("regression {index}: {message}");
        }
    }
}
