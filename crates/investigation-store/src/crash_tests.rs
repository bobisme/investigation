//! Commit-path crash schedules. This is a child of `store`, so it exercises the
//! same private commit method as production, with a crash-model filesystem.
use super::*;
use crate::{
    durable::{DurableFile, MemoryFs},
    workspace::{FilesystemDetector, FilesystemInfo, FilesystemKind},
};
use investigation_core::header::UtcTimestamp;
use std::{
    fs, io,
    sync::atomic::{AtomicU64, Ordering},
};
use uuid::Uuid;

struct LocalDetector;
impl FilesystemDetector for LocalDetector {
    fn detect(&self, _: &Path) -> io::Result<FilesystemInfo> {
        Ok(FilesystemInfo {
            kind: FilesystemKind::Local,
            name: "test-local".into(),
            magic: None,
        })
    }
}
struct FixedClock;
impl Clock for FixedClock {
    fn now_utc(&mut self) -> UtcTimestamp {
        "2026-11-22T08:12:44.000Z".parse().unwrap()
    }
}
struct Ids(u64);
impl IdSource for Ids {
    fn next_uuid(&mut self) -> Uuid {
        self.0 += 1;
        format!("018d87fc-5234-7000-8000-{:012x}", self.0)
            .parse()
            .unwrap()
    }
}

fn setup(seed: u64) -> (Store<MemoryFs>, PathBuf) {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "inv-crash-{}-{seed}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let workspace = Workspace::init_with_detector(&root, false, &LocalDetector).unwrap();
    let mut fake = MemoryFs::new();
    for dir in [
        "state",
        "state/cases",
        "state/tmp",
        "state/locks",
        "state/blobs",
        "state/blobs/sha256",
        "local",
    ] {
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

fn request(case_id: Option<Id>, command_id: &str) -> (Command, SemanticRequest) {
    let operation = if case_id.is_none() {
        json!({"op":"case.create","payload":{"title":"Crash fixture","mode":"explore"}})
    } else {
        json!({"op":"question.create","payload":{"wording":"What changed?","importance":"normal"}})
    };
    Command::parse(&serde_json::to_vec(&json!({"schema_version":"inv.command.v0-alpha","command_id":command_id,"case_id":case_id,"actor":"agent:crash-test","preconditions":[],"operations":[operation]})).unwrap()).unwrap()
}

fn apply(
    store: &mut Store<MemoryFs>,
    c: &Command,
    r: &SemanticRequest,
    seed: u64,
) -> Result<Receipt, Failure> {
    store.commit(c, r.clone(), &[], &mut FixedClock, &mut Ids(seed))
}
fn restart(store: &mut Store<MemoryFs>) {
    store.fs.crash();
    store.gate = WriteGate::isolated();
}

/// Every numbered filesystem call is a replayable injected fault schedule.
/// The no-fault tail covers the post-acknowledgement crash as well.
#[test]
fn seeded_failure_schedules_preserve_idempotency_and_atomic_publication() {
    for append in [false, true] {
        let mut failures = 0;
        let mut uncertain = 0;
        let mut acknowledged = 0;
        for seed in 1..=70 {
            let (mut store, root) = setup(seed);
            let case_id = if append {
                let (c, r) = request(None, "initial-case");
                Some(apply(&mut store, &c, &r, 0).unwrap().created[0].id)
            } else {
                None
            };
            let (command, semantic) = request(case_id, if append { "append" } else { "create" });
            store.fs.fail_at(seed as usize);
            let first = apply(&mut store, &command, &semantic, 100);
            match &first {
                Ok(_) => acknowledged += 1,
                Err(e) => {
                    failures += 1;
                    if e.error.code == ErrorCode::DurabilityUnknown {
                        uncertain += 1;
                        assert!(store.gate.is_stopped(), "seed {seed} append {append}");
                        assert!(
                            apply(&mut store, &command, &semantic, 200).is_err(),
                            "stopped gate accepted a write, seed {seed}"
                        );
                    }
                }
            }
            restart(&mut store);
            let lookup = store
                .receipt(&command.actor, &command.command_id)
                .unwrap_or_else(|e| panic!("recovery seed {seed} append {append}: {e:?}"));
            if let Ok(ack) = &first {
                match &lookup {
                    ReceiptLookup::Found { receipt } => {
                        assert_eq!(receipt.transaction_id, ack.transaction_id)
                    }
                    _ => panic!("acknowledged commit lost: seed {seed}, append {append}"),
                }
            }
            let found_id = match &lookup {
                ReceiptLookup::Found { receipt } => Some(receipt.transaction_id),
                _ => None,
            };
            let retried = apply(&mut store, &command, &semantic, 10_000 + seed)
                .unwrap_or_else(|e| panic!("retry seed {seed} append {append}: {e:?}"));
            if let Some(id) = found_id {
                assert_eq!(
                    retried.transaction_id, id,
                    "duplicate transaction: seed {seed}"
                );
            }
            let case = store
                .case(
                    retried.created[0]
                        .id
                        .kind()
                        .eq(&Kind::Case)
                        .then_some(retried.created[0].id)
                        .or(case_id)
                        .unwrap(),
                )
                .unwrap();
            assert_eq!(
                case.events.len(),
                if append { 2 } else { 1 },
                "partial or duplicate event seed {seed}"
            );
            assert_eq!(case.state.records().count(), if append { 2 } else { 1 });
            let rebuilt = store.repair_index().unwrap();
            let index = Projection::open(&store.index_path(), store.workspace.id()).unwrap();
            let actual = index.heads().unwrap();
            let expected: Vec<_> = rebuilt
                .into_iter()
                .map(|head| (head.case_id, head.snapshot))
                .collect();
            assert_eq!(
                actual, expected,
                "index heads disagree with verified replay at seed {seed}"
            );
            drop(store);
            fs::remove_dir_all(root).unwrap();
        }
        assert!(
            failures > 0 && uncertain > 0 && acknowledged > 0,
            "schedule did not cover failure, fsync and acknowledgement for append={append}"
        );
    }
}

#[test]
fn committed_attachment_is_durable_before_its_event() {
    use crate::blob::{stage, verify_blob};
    let mut saw_fault = false;
    let mut saw_commit = false;
    for seed in 1..=60 {
        let (mut store, root) = setup(seed + 100);
        let (command, mut request) = request(None, "with-attachment");
        let blob = stage(
            &mut store.fs,
            &store.gate,
            Path::new("state/tmp/fixture"),
            &b"source bytes"[..],
        )
        .unwrap();
        request
            .attachments
            .push(format!("sha256:{}", blob.digest).try_into().unwrap());
        store.fs.fail_at(seed as usize);
        let first = store.commit(
            &command,
            request.clone(),
            std::slice::from_ref(&blob),
            &mut FixedClock,
            &mut Ids(100),
        );
        if first.is_err() {
            saw_fault = true;
        }
        restart(&mut store);
        let lookup = store
            .receipt(&command.actor, &command.command_id)
            .unwrap_or_else(|e| panic!("attachment recovery seed {seed}: {e:?}"));
        if let ReceiptLookup::Found { receipt } = lookup {
            saw_commit = true;
            let target = Path::new(BLOBS).join(&blob.digest[..2]).join(&blob.digest);
            verify_blob(&store.fs, &target, &blob.digest, blob.size)
                .unwrap_or_else(|e| panic!("event without durable blob seed {seed}: {e:?}"));
            if let Ok(ack) = first {
                assert_eq!(receipt.transaction_id, ack.transaction_id);
            }
        } else {
            assert!(
                first.is_err(),
                "acknowledged attachment event disappeared seed {seed}"
            );
            let retry_blob = stage(
                &mut store.fs,
                &store.gate,
                Path::new("state/tmp/retry"),
                &b"source bytes"[..],
            )
            .unwrap();
            let committed = store
                .commit(
                    &command,
                    request.clone(),
                    &[retry_blob],
                    &mut FixedClock,
                    &mut Ids(10_000 + seed),
                )
                .unwrap();
            assert_eq!(store.case(committed.created[0].id).unwrap().events.len(), 1);
        }
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
    assert!(saw_fault && saw_commit);
}
