//! Reads share the coordination lock and writers are preferred over them
//! (ADR 0011, "Reads share the coordination lock"; bn-2vek).
//!
//! A read held the exclusive lock for its whole verification, so parallel
//! reads queued behind each other and the last of them could wait past the
//! fixed 2 s and refuse with WORKSPACE_BUSY. A read now shares the lock with
//! other reads and still excludes every writer, so it never reports a
//! publication that is not yet synced. A writer that waits holds the gate,
//! so new reads queue behind it and a stream of reads cannot keep it out.
//!
//! `flock` locks belong to an open file description, so two opens in one
//! process conflict as two processes do.

use std::{
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use investigation_core::{header::Actor, identity::Id, wire::ErrorCode};
use investigation_store::{
    Store, Workspace,
    durable::{CommitLock, LockError},
};
use serde_json::{Value, json};

const LOCK: &str = "state/locks/commit.lock";
/// Longer than the store's fixed wait for the coordination lock.
const PAST_THE_WAIT: Duration = Duration::from_millis(2_600);

fn command(key: &str, case_id: Option<Id>, operation: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema_version": "inv.command.v1",
        "command_id": key,
        "case_id": case_id,
        "actor": "agent:read-lock",
        "preconditions": [],
        "operations": [operation]
    }))
    .unwrap()
}

fn question(key: &str, case: Id) -> Vec<u8> {
    command(
        key,
        Some(case),
        json!({"op": "question.create", "payload": {"wording": key, "importance": "normal"}}),
    )
}

fn workspace() -> (tempfile::TempDir, PathBuf, Id) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let mut store = Store::open(Workspace::init(&root, false).unwrap());
    let create = command(
        "create",
        None,
        json!({"op": "case.create", "payload": {"title": "Locks", "mode": "record"}}),
    );
    let case = store.record(&create).unwrap().created[0].id;
    (temporary, root, case)
}

fn busy(result: Result<CommitLock, LockError>) -> bool {
    matches!(result, Err(LockError::Busy { .. }))
}

#[test]
fn shared_holders_share_the_lock_and_exclude_every_exclusive_holder() {
    let (_temporary, root, _) = workspace();
    let lock = root.join(LOCK);
    let first = CommitLock::acquire_shared(&lock, Duration::ZERO).unwrap();
    let second = CommitLock::acquire_shared(&lock, Duration::ZERO).unwrap();
    // A program that takes only the plain exclusive lock, such as an
    // earlier release, still excludes shared holders, and they exclude it.
    assert!(busy(CommitLock::acquire(&lock, Duration::ZERO)));
    assert!(busy(CommitLock::acquire_preferred(&lock, Duration::ZERO)));
    drop((first, second));
    let writer = CommitLock::acquire(&lock, Duration::ZERO).unwrap();
    assert!(busy(CommitLock::acquire_shared(&lock, Duration::ZERO)));
    drop(writer);
    let writer = CommitLock::acquire_preferred(&lock, Duration::ZERO).unwrap();
    assert!(busy(CommitLock::acquire_shared(&lock, Duration::ZERO)));
    assert!(busy(CommitLock::acquire(&lock, Duration::ZERO)));
    drop(writer);
}

#[test]
fn a_waiting_writer_holds_new_readers_back_and_drops_the_gate_once_it_holds_the_lock() {
    let (_temporary, root, _) = workspace();
    let lock = root.join(LOCK);
    let reader = CommitLock::acquire_shared(&lock, Duration::ZERO).unwrap();
    let waiting = {
        let lock = lock.clone();
        thread::spawn(move || CommitLock::acquire_preferred(&lock, Duration::from_secs(10)))
    };
    // Wait until the writer holds the gate: a new reader is then refused
    // although the only holder of the lock is a reader.
    let started = Instant::now();
    while !busy(CommitLock::acquire_shared(&lock, Duration::ZERO)) {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a new reader still got in beside a reader while a writer waited"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!waiting.is_finished(), "the writer got in beside a reader");
    drop(reader);
    let writer = waiting.join().unwrap().unwrap();
    // The gate is held only while waiting: with the lock held, the gate
    // directory is free again.
    let gate = std::fs::File::open(lock.parent().unwrap()).unwrap();
    gate.try_lock()
        .expect("the gate is released once the lock is held");
    drop(gate);
    drop(writer);
    CommitLock::acquire_shared(&lock, Duration::ZERO).unwrap();
}

#[test]
fn a_read_waits_only_for_writers_never_behind_another_read() {
    let (_temporary, root, case) = workspace();
    // Another read holds the lock for longer than the store waits: the
    // reads of `show`, `brief`, `next`, `frontier` and `receipt` all still
    // answer. Before bn-2vek each took the lock exclusively and refused.
    let other_read = CommitLock::acquire_shared(&root.join(LOCK), Duration::ZERO).unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    assert_eq!(store.case(case).unwrap().snapshot.sequence, 1);
    assert_eq!(store.case_ids().unwrap(), vec![case]);
    assert_eq!(store.list_cases().unwrap().len(), 1);
    assert_eq!(store.cases().unwrap().len(), 1);
    let mut visited = Vec::new();
    store.visit_cases(|id, _| visited.push(id)).unwrap();
    assert_eq!(visited, vec![case]);
    let actor: Actor = "agent:read-lock".parse().unwrap();
    assert!(store.receipt(&actor, "create").is_ok());

    // A commit still excludes reads, and reads exclude a commit, so a read
    // never reports a publication whose directory sync has not finished.
    let refused = store.record(&question("blocked", case)).unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::WorkspaceBusy);
    drop(other_read);
    let writer = CommitLock::acquire(&root.join(LOCK), Duration::ZERO).unwrap();
    let refused = store.case(case).unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::WorkspaceBusy);
    assert!(
        refused
            .error
            .message
            .contains("Retry the identical request")
    );
    drop(writer);
    assert_eq!(
        store
            .record(&question("after", case))
            .unwrap()
            .snapshot
            .sequence,
        2
    );
}

#[test]
fn parallel_reads_beside_a_slow_read_all_answer() {
    let (_temporary, root, case) = workspace();
    let slow = CommitLock::acquire_shared(&root.join(LOCK), Duration::ZERO).unwrap();
    // The trial_tasks_8_10 rehearsal reads with 4 workers side by side.
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let root = root.clone();
            thread::spawn(move || {
                let mut store = Store::open(Workspace::open(Path::new(&root)).unwrap());
                (0..3)
                    .map(|_| store.case(case).map(|replay| replay.snapshot.sequence))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    thread::sleep(PAST_THE_WAIT);
    drop(slow);
    for reader in readers {
        for read in reader.join().unwrap() {
            assert_eq!(read.map_err(|failure| failure.error.code), Ok(1));
        }
    }
}
