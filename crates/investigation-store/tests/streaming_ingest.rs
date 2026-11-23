//! Large captures stream into the workspace with bounded memory and keep their
//! exact bytes (W06b.09, §16.4, §20.2): from a local file and from standard
//! input, past the old 64 MiB limit, with the peak heap of the ingest measured
//! at two sizes and required to be the same small bound.
//!
//! `just check` streams 2 MiB and 72 MiB. For a long local run set
//! `INV_LARGE_INGEST_GIB=<n>` to also stream `n` GiB from a sparse file and
//! from a generator; temporary workspaces follow `TMPDIR`, so point it at a
//! disk with room for the copy. With `n` above 64 the same run also streams
//! one byte more than `ingest.file_bytes` from standard input and checks the
//! refusal. The commit lock and full-disk behaviour are tested next to the
//! staging code in `src/ingest_streaming_tests.rs`.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use investigation_core::wire::{CommitStatus, ErrorCode};
use investigation_store::{
    Store, Workspace,
    store::ingest::{IngestReceipt, MAX_IMPORT_BYTES},
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

// ---- Per-thread heap accounting -------------------------------------------

/// Forwards to the system allocator and counts live bytes per thread, so a
/// test measures its own peak while other tests run in parallel.
struct Counting;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn record(delta: isize) {
    // `try_with`: the allocator also runs while thread locals are torn down.
    let _ = LIVE.try_with(|live| {
        let now = live.get() + delta;
        live.set(now);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
    });
}

// A global allocator needs `unsafe impl`. This one only forwards to `System`
// and updates two const-initialized thread-local counters, which allocate
// nothing. It exists in this test binary only.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record(layout.size() as isize);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            record(layout.size() as isize);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        record(-(layout.size() as isize));
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(pointer, layout, size) };
        if !moved.is_null() {
            record(size as isize - layout.size() as isize);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Run `work` and return its result with the most heap it held at once,
/// above what this thread held before it started.
fn peak<T>(work: impl FnOnce() -> T) -> (T, usize) {
    let before = LIVE.with(Cell::get);
    PEAK.with(|peak| peak.set(before));
    let value = work();
    let most = PEAK.with(Cell::get) - before;
    (value, most.max(0) as usize)
}

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;
/// Peak heap allowed for one whole ingest, whatever its size: the 64 KiB
/// copy buffer, the UTF-8 buffer, the journal, index and receipt work.
const HEAP_BOUND: usize = MIB as usize;
/// How much more a large ingest may hold at peak than a small one.
const HEAP_SLACK: usize = 32 * KIB as usize;
/// The small size: above `ingest.probe_bytes`, so neither size is probed for
/// a producer envelope (a bounded read of at most 1 MiB of smaller content).
const SMALL: u64 = 2 * MIB;
/// The large size in `just check`: past the old 64 MiB import limit.
const LARGE: u64 = 72 * MIB;

// ---- Inputs --------------------------------------------------------------------

/// Deterministic, incompressible bytes, so no byte of the copy can be right
/// by accident.
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

/// A sparse file of `size` bytes: zeros except distinct markers at the start,
/// across the first buffer boundary, at the old 64 MiB limit and at the end.
/// It takes almost no disk however large it is.
fn sparse(path: &Path, size: u64) {
    let mut file = fs::File::create(path).unwrap();
    file.set_len(size).unwrap();
    for (index, offset) in [0, 64 * KIB - 3, 64 * MIB - 4, size - 8]
        .into_iter()
        .enumerate()
        .filter(|(_, offset)| offset + 8 <= size)
    {
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&[0xA5, index as u8, 0x5A, 0xC3, 0x3C, 0xFF, 0x01, 0x80])
            .unwrap();
    }
}

fn sha256(mut input: impl Read) -> String {
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * KIB as usize];
    loop {
        let read = input.read(&mut buffer).unwrap();
        if read == 0 {
            return format!("sha256:{:x}", hash.finalize());
        }
        hash.update(&buffer[..read]);
    }
}

/// Whether two streams yield the same bytes, compared a buffer at a time.
fn same_bytes(mut left: impl Read, mut right: impl Read) -> bool {
    let mut a = vec![0; 64 * KIB as usize];
    let mut b = vec![0; 64 * KIB as usize];
    loop {
        let read = left.read(&mut a).unwrap();
        if read == 0 {
            return right.read(&mut b[..1]).unwrap() == 0;
        }
        if right.read_exact(&mut b[..read]).is_err() || a[..read] != b[..read] {
            return false;
        }
    }
}

// ---- Workspace -----------------------------------------------------------------

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    store: Store,
}

fn fixture() -> Fixture {
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
    let store = Store::open(Workspace::open(&root).unwrap());
    Fixture {
        _temporary: temporary,
        root,
        store,
    }
}

fn manifest(command: &str, item: Value) -> Vec<u8> {
    let mut manifest = json!({
        "schema_version": "inv.ingest.v1",
        "command_id": command,
        "actor": "agent:streaming-test",
        "new_case": {"title": "Large capture"},
        "source": {"title": "Generated capture"},
        "extract_text": true,
    });
    let key = if item.is_array() { "paths" } else { "content" };
    manifest[key] = item;
    serde_json::to_vec(&manifest).unwrap()
}

impl Fixture {
    fn blob(&self, receipt: &IngestReceipt) -> PathBuf {
        let hex = receipt.items[0]
            .digest
            .as_str()
            .trim_start_matches("sha256:");
        self.root
            .join("state/blobs/sha256")
            .join(&hex[..2])
            .join(hex)
    }

    /// Ingest a sparse file of `size` bytes; return the peak heap.
    fn ingest_file(&mut self, size: u64) -> usize {
        let name = format!("capture-{size}.bin");
        let source = self.root.join(&name);
        sparse(&source, size);
        let bytes = manifest(&format!("file-{size}"), json!([name]));
        let (receipt, most) = peak(|| self.store.ingest(&bytes, None));
        let receipt = receipt.unwrap();
        let item = &receipt.items[0];
        assert_eq!(item.size_bytes, size);
        assert_eq!(
            item.digest.as_str(),
            sha256(fs::File::open(&source).unwrap())
        );
        assert!(same_bytes(
            fs::File::open(&source).unwrap(),
            fs::File::open(self.blob(&receipt)).unwrap()
        ));
        fs::remove_file(self.blob(&receipt)).unwrap();
        fs::remove_file(source).unwrap();
        most
    }

    /// Ingest `size` generated bytes from standard input; return the peak heap.
    fn ingest_stdin(&mut self, size: u64) -> usize {
        let bytes = manifest(
            &format!("stdin-{size}"),
            json!({"kind": "stdin", "name": "capture.bin"}),
        );
        let mut input = Generated::new(size, size);
        let (receipt, most) = peak(|| self.store.ingest(&bytes, Some(&mut input)));
        let receipt = receipt.unwrap();
        let item = &receipt.items[0];
        assert_eq!(item.size_bytes, size);
        assert_eq!(item.digest.as_str(), sha256(Generated::new(size, size)));
        assert!(same_bytes(
            Generated::new(size, size),
            fs::File::open(self.blob(&receipt)).unwrap()
        ));
        fs::remove_file(self.blob(&receipt)).unwrap();
        most
    }

    /// Nothing is left under `state/tmp`.
    fn no_copies(&self) {
        let left: Vec<_> = fs::read_dir(self.root.join("state/tmp"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
}

fn assert_bounded(small: usize, large: usize, what: &str) {
    eprintln!("{what}: peak heap {small} bytes small, {large} bytes large");
    assert!(small < HEAP_BOUND, "{what}: small peak {small} bytes");
    assert!(large < HEAP_BOUND, "{what}: large peak {large} bytes");
    assert!(
        large <= small + HEAP_SLACK,
        "{what}: peak grew from {small} to {large} bytes"
    );
}

/// The long variant's size in GiB, when `INV_LARGE_INGEST_GIB` asks for one.
fn long_run() -> Option<u64> {
    std::env::var("INV_LARGE_INGEST_GIB")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|gib| *gib > 0)
}

// ---- Tests -------------------------------------------------------------------------

/// Each size streams into its own fresh workspace, so the two peaks differ
/// only by the input's size.
fn compare(size: u64, ingest: fn(&mut Fixture, u64) -> usize, what: &str) {
    let small = ingest(&mut fixture(), SMALL);
    let mut large_fixture = fixture();
    let large = ingest(&mut large_fixture, size);
    large_fixture.no_copies();
    assert_bounded(small, large, what);
}

#[test]
fn a_file_past_the_old_limit_streams_in_bounded_memory_and_keeps_exact_bytes() {
    compare(LARGE, Fixture::ingest_file, "file");
}

#[test]
fn standard_input_past_the_old_limit_streams_in_bounded_memory_and_keeps_exact_bytes() {
    compare(LARGE, Fixture::ingest_stdin, "stdin");
}

/// `INV_LARGE_INGEST_GIB=<n>` only: `n` GiB from a sparse file and from a
/// generator, with the same heap bound as 2 MiB.
#[test]
fn multi_gigabyte_inputs_stream_in_the_same_bounded_memory() {
    let Some(gib) = long_run() else {
        eprintln!("skipped: set INV_LARGE_INGEST_GIB=<n> to stream n GiB");
        return;
    };
    let size = (gib * GIB).min(MAX_IMPORT_BYTES);
    compare(size, Fixture::ingest_file, "file");
    compare(size, Fixture::ingest_stdin, "stdin");
}

/// `INV_LARGE_INGEST_GIB` above 64 only: one byte past `ingest.file_bytes` on
/// standard input is an input error that keeps no copy. The fast check of the
/// same refusal, at a small limit, is in `src/ingest_streaming_tests.rs`.
#[test]
fn standard_input_over_the_import_limit_is_an_input_error_and_leaves_no_copy() {
    if long_run().is_none_or(|gib| gib * GIB <= MAX_IMPORT_BYTES) {
        eprintln!("skipped: set INV_LARGE_INGEST_GIB above 64 to stream past the limit");
        return;
    }
    let mut fixture = fixture();
    let bytes = manifest(
        "stdin-huge",
        json!({"kind": "stdin", "name": "../../stream.bin"}),
    );
    let mut stream = io::repeat(b'a').take(MAX_IMPORT_BYTES + 1);
    let (failure, most) = peak(|| fixture.store.ingest(&bytes, Some(&mut stream)).unwrap_err());
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);
    assert_eq!(failure.commit_status, CommitStatus::NotCommitted);
    assert!(failure.error.message.contains("import limit"));
    assert!(most < HEAP_BOUND, "peak {most} bytes");
    fixture.no_copies();
    // The stream's name is a label, never a path.
    assert!(!fixture.root.join("../stream.bin").exists());
    assert!(fixture.store.list_cases().unwrap().is_empty());
}
