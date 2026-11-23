//! Streaming ingestion of large inputs (W06b.09, §16.4, §20.2): the commit
//! lock is free while bytes are copied or hashed, and a stream cut short by a
//! full disk or the import limit leaves no blob, no reference and no copy.
//!
//! A child of `ingest`, so the tests drive the private staging path and the
//! crash-model filesystem directly. Peak memory and multi-gigabyte runs are in
//! `tests/streaming_ingest.rs`.
use std::{
    cell::Cell,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use investigation_core::{
    catalog::standard_catalog,
    wire::{CommitStatus, ErrorCode},
};
use serde_json::{Value, json};

use super::{Limited, Store, stage_input};
use crate::{
    durable::{CommitLock, DurableFs, LocalFs, LockError, MemoryFs, WriteGate},
    store::crash_tests::setup,
    workspace::Workspace,
};

const MIB: u64 = 1024 * 1024;
const COMMIT_LOCK: &str = "state/locks/commit.lock";

/// Deterministic, incompressible bytes: a byte stream nothing can mistake for
/// zeros or for a shifted copy of itself.
struct Generated {
    state: u64,
    left: u64,
}

impl Generated {
    fn new(seed: u64, size: u64) -> Self {
        Self {
            state: seed | 1,
            left: size,
        }
    }
}

impl Read for Generated {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let take = usize::try_from(self.left)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        for byte in &mut buffer[..take] {
            self.state ^= self.state << 13;
            self.state ^= self.state >> 7;
            self.state ^= self.state << 17;
            *byte = (self.state >> 24) as u8;
        }
        self.left -= take as u64;
        Ok(take)
    }
}

fn stdin_manifest(command: &str) -> Value {
    json!({
        "schema_version": "inv.ingest.v1",
        "command_id": command,
        "actor": "agent:streaming-test",
        "new_case": {"title": "Large capture"},
        "source": {"title": "Generated capture"},
        "content": {"kind": "stdin", "name": "capture.bin"},
        "extract_text": true,
    })
}

fn entries(fs: &MemoryFs, directory: &str) -> Vec<PathBuf> {
    fs.list(Path::new(directory)).unwrap()
}

/// Every blob under the sharded blob store.
fn blobs(fs: &MemoryFs) -> Vec<PathBuf> {
    entries(fs, "state/blobs/sha256")
        .into_iter()
        .flat_map(|shard| fs.list(&shard).unwrap())
        .collect()
}

#[test]
fn standard_input_over_the_import_limit_is_refused_and_leaves_no_copy() {
    let (mut store, root) = setup(1);
    let limit = MIB;
    let failure = stage_input(
        &mut store.fs,
        &store.gate,
        Limited::with_limit(io::repeat(b'a').take(limit + 1), limit),
    )
    .unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);
    assert!(
        failure.error.message.contains("64 GiB import limit"),
        "{}",
        failure.error.message
    );
    assert_eq!(failure.commit_status, CommitStatus::NotCommitted);
    assert!(entries(&store.fs, "state/tmp").is_empty());
    // Exactly the limit is accepted.
    let staged = stage_input(
        &mut store.fs,
        &store.gate,
        Limited::with_limit(io::repeat(b'a').take(limit), limit),
    )
    .unwrap();
    assert_eq!(staged.size, limit);
    let _ = fs::remove_dir_all(root);
}

/// The disk fills at each point of a 1 MiB stream, including in the middle of
/// one buffer, at the last byte, and while the manifest copy is written. Each
/// failure commits nothing, publishes no blob and removes its copy; the
/// identical retry then succeeds once space is back.
#[test]
fn a_full_disk_mid_stream_leaves_no_blob_no_reference_and_no_copy() {
    let size = MIB;
    for free in [
        0,
        1,
        64 * 1024 - 1,
        64 * 1024,
        300_001,
        size - 1,
        size,
        size + 10,
    ] {
        let (mut store, root) = setup(2);
        store.fs.fill_after(free);
        let manifest = serde_json::to_vec(&stdin_manifest("full-disk")).unwrap();
        let mut copied = 0;
        let failure = store
            .ingest_observed(
                &manifest,
                Some(&mut Generated::new(7, size)),
                &mut |bytes| copied = bytes,
            )
            .unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::IoFailure, "free {free}");
        assert!(
            failure.error.message.contains("StorageFull"),
            "free {free}: {}",
            failure.error.message
        );
        assert_eq!(failure.commit_status, CommitStatus::NotCommitted);
        assert!(!store.gate.is_stopped(), "free {free}");
        assert!(copied <= size, "free {free}");
        assert!(entries(&store.fs, "state/tmp").is_empty(), "free {free}");
        assert!(blobs(&store.fs).is_empty(), "free {free}");
        assert!(entries(&store.fs, "state/cases").is_empty(), "free {free}");
        assert!(store.fs.held_locks().is_empty(), "free {free}");

        store.fs.fill_after(u64::MAX);
        let receipt = store
            .ingest(&manifest, Some(&mut Generated::new(7, size)))
            .unwrap_or_else(|error| panic!("free {free}: {error:?}"));
        assert_eq!(receipt.items[0].size_bytes, size);
        assert!(entries(&store.fs, "state/tmp").is_empty(), "free {free}");
        // The capture and the manifest.
        assert_eq!(blobs(&store.fs).len(), 2, "free {free}");
        let _ = fs::remove_dir_all(root);
    }
}

// ---- The commit lock --------------------------------------------------------

/// The local filesystem, except that each read of a blob or of an import copy
/// first checks that the commit lock is free, as another process would see it.
struct Probing {
    local: LocalFs,
    lock: PathBuf,
    reads: Rc<Cell<usize>>,
}

struct ProbedRead {
    input: Box<dyn Read>,
    lock: PathBuf,
    reads: Rc<Cell<usize>>,
}

impl Read for ProbedRead {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        assert!(
            CommitLock::acquire(&self.lock, Duration::ZERO).is_ok(),
            "the commit lock is held while bytes are hashed"
        );
        self.reads.set(self.reads.get() + 1);
        self.input.read(buffer)
    }
}

impl DurableFs for Probing {
    type File = <LocalFs as DurableFs>::File;
    type Guard = <LocalFs as DurableFs>::Guard;
    fn create_private(&mut self, path: &Path) -> io::Result<Self::File> {
        self.local.create_private(path)
    }
    fn create_dir(&mut self, path: &Path) -> io::Result<()> {
        self.local.create_dir(path)
    }
    fn read(&self, path: &Path) -> io::Result<Box<dyn Read>> {
        let input = self.local.read(path)?;
        let name = path.to_string_lossy();
        if name.starts_with("state/blobs/") || name.starts_with("state/tmp/import-") {
            return Ok(Box::new(ProbedRead {
                input,
                lock: self.lock.clone(),
                reads: self.reads.clone(),
            }));
        }
        Ok(input)
    }
    fn sync_existing(&mut self, path: &Path) -> io::Result<()> {
        self.local.sync_existing(path)
    }
    fn list(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.local.list(path)
    }
    fn link_no_replace(&mut self, source: &Path, target: &Path) -> io::Result<()> {
        self.local.link_no_replace(source, target)
    }
    fn rename_no_replace(&mut self, source: &Path, target: &Path) -> io::Result<()> {
        self.local.rename_no_replace(source, target)
    }
    fn sync_dir(&mut self, path: &Path) -> io::Result<()> {
        self.local.sync_dir(path)
    }
    fn remove(&mut self, path: &Path) -> io::Result<()> {
        self.local.remove(path)
    }
    fn remove_dir(&mut self, path: &Path) -> io::Result<()> {
        self.local.remove_dir(path)
    }
    fn lock(&mut self, path: &Path, timeout: Duration) -> Result<Self::Guard, LockError> {
        self.local.lock(path, timeout)
    }
    fn reclaim_publication_temporaries(&mut self) {
        self.local.reclaim_publication_temporaries();
    }
}

/// While a copy, a reference hash and a deduplicated reimport stream, another
/// store on the same workspace takes the real advisory lock and commits, and
/// every read of the bytes finds the lock free. Before W06b.09 a reimport of
/// known bytes rehashed the existing blob under the lock.
#[test]
fn another_writer_commits_while_a_large_ingest_streams() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        contents.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    let workspace = Workspace::open(&root).unwrap();
    let size = 3 * MIB;
    let mut file = fs::File::create(root.join("capture.bin")).unwrap();
    io::copy(&mut Generated::new(11, size), &mut file).unwrap();
    drop(file);

    let lock = root.join(COMMIT_LOCK);
    let reads = Rc::new(Cell::new(0));
    let mut store = Store {
        workspace: workspace.clone(),
        fs: Probing {
            local: LocalFs::new(workspace.root()),
            lock: lock.clone(),
            reads: reads.clone(),
        },
        gate: WriteGate::isolated(),
        catalog: standard_catalog(),
    };
    let free = || CommitLock::acquire(&lock, Duration::ZERO).is_ok();
    let mut elsewhere = 0;
    let mut commit_elsewhere = || {
        let command = json!({
            "schema_version": "inv.command.v1",
            "command_id": format!("concurrent-{elsewhere}"),
            "actor": "agent:another-writer", "case_id": null, "preconditions": [],
            "operations": [{"op": "case.create",
                "payload": {"title": "Concurrent work", "mode": "explore"}}],
        });
        elsewhere += 1;
        Store::open(workspace.clone())
            .record(&serde_json::to_vec(&command).unwrap())
            .unwrap();
    };

    let path = |command: &str, retention: &str| {
        json!({
            "schema_version": "inv.ingest.v1",
            "command_id": command,
            "actor": "agent:streaming-test",
            "new_case": {"title": "Large capture"},
            "source": {"title": "Generated capture"},
            "paths": ["capture.bin"],
            "retention": retention,
            "extract_text": retention == "copy",
        })
    };
    let requests = [
        (path("copy", "copy"), false),
        // Known bytes: the existing blob is verified before the lock.
        (stdin_manifest("stdin-reimport"), true),
        (path("reference", "reference"), false),
    ];
    let mut digests = Vec::new();
    for (manifest, stdin) in requests {
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let mut progress = Vec::new();
        let mut generated = Generated::new(11, size);
        let receipt = store
            .ingest_observed(
                &bytes,
                stdin.then_some(&mut generated as &mut dyn Read),
                &mut |copied| {
                    assert!(free(), "the commit lock is held while bytes stream");
                    if progress.is_empty() {
                        commit_elsewhere();
                    }
                    progress.push(copied);
                },
            )
            .unwrap();
        assert!(progress.len() > 1, "{manifest}");
        assert_eq!(progress.last(), Some(&size));
        assert!(progress.is_sorted());
        assert_eq!(receipt.items[0].size_bytes, size);
        digests.push(receipt.items[0].digest.clone());
    }
    assert!(digests.iter().all(|digest| *digest == digests[0]));
    // The whole known blob was read back through the probe. (The UTF-8
    // checks stop at the first invalid generated byte.)
    assert!(
        reads.get() > (size / (64 * 1024)) as usize,
        "{}",
        reads.get()
    );
    assert!(free());
    let mut store = Store::open(workspace);
    assert_eq!(store.list_cases().unwrap().len(), 6);
    assert!(store.check().unwrap().integrity_ok());
}
