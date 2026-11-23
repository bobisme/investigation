//! Hostile raw files fail safely: compressed bombs and archives stay opaque,
//! nesting stops at its bound, paths cannot leave the import root, huge lines
//! and invalid UTF-8 are refused or reported. Each case asserts its refusal
//! and, where the work is in this thread, its peak heap allocation.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    fs,
    io::{self, Read},
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    time::Instant,
};

use investigation_core::{
    evidence::SelectorKind,
    input::{self, InputErrorKind},
    method_pack,
    wire::{CommitStatus, ErrorCode, Failure},
};
use investigation_store::{
    Store, Workspace,
    delimited::{self, CellQuery, Format},
    limits::{self, EXPANSION},
    opaque::{EnvelopeProbe, probe_envelope},
    selectors::{SelectorError, TextDerivative, resolve, verify_quote},
    store::ingest::{MAX_IMPORT_BYTES, producer_run::parse_envelope},
    utf8,
};
use proptest::{
    prelude::*,
    test_runner::{Config, RngAlgorithm, TestRng, TestRunner},
};
use serde_json::{Value, json};

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

const MIB: usize = 1024 * 1024;

// ---- Fixtures --------------------------------------------------------------

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/hardening");

struct Fixture {
    _temporary: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    store: Store,
}

/// A workspace whose import root is the workspace directory itself, with a
/// sibling directory outside every root.
fn fixture() -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path().to_owned();
    let root = base.join("workspace");
    Workspace::init(&root, false).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        contents.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    fs::create_dir(base.join("outside")).unwrap();
    fs::write(base.join("outside/secret.txt"), b"outside the import root").unwrap();
    let store = Store::open(Workspace::open(&root).unwrap());
    Fixture {
        _temporary: temporary,
        base,
        root,
        store,
    }
}

fn manifest(command: &str, paths: Value) -> Value {
    json!({
        "schema_version": "inv.ingest.v1",
        "command_id": command,
        "actor": "agent:hardening-test",
        "new_case": {"title": "Hostile inputs"},
        "source": {"title": "Untrusted file"},
        "paths": paths,
        "extract_text": true,
    })
}

impl Fixture {
    fn ingest(&mut self, manifest: &Value) -> Result<Value, Failure> {
        self.store
            .ingest(&serde_json::to_vec(manifest).unwrap(), None)
            .map(|receipt| serde_json::to_value(receipt).unwrap())
    }

    fn refused(&mut self, command: &str, path: &str) -> Failure {
        let failure = self.ingest(&manifest(command, json!([path]))).unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::InvalidInput, "{path}");
        assert_eq!(failure.commit_status, CommitStatus::NotCommitted, "{path}");
        failure
    }

    /// Every file and link under the temporary directory, relative to it.
    fn tree(&self) -> Vec<PathBuf> {
        fn walk(directory: &Path, base: &Path, out: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                out.push(path.strip_prefix(base).unwrap().to_owned());
                if fs::symlink_metadata(&path).unwrap().is_dir() {
                    walk(&path, base, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.base, &self.base, &mut out);
        out
    }
}

fn sha256(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let digest = investigation_store::journal::hash_bytes(bytes);
    let mut out = String::new();
    write!(out, "{}", digest.as_str()).unwrap();
    out
}

// ---- Decompression and archives --------------------------------------------

#[test]
fn a_nested_gzip_bomb_is_retained_as_its_own_bytes_and_never_expanded() {
    // 269 bytes; two gzip layers expand to 66,279 bytes and then 65 MiB of
    // zeros, past the 64 MiB import limit.
    let bomb = fs::read(format!("{FIXTURES}/nested-bomb.gz")).unwrap();
    assert_eq!(bomb.len(), 269);
    const { assert!(!EXPANSION.decompression && !EXPANSION.archive_extraction) };
    let mut fixture = fixture();
    fs::write(fixture.root.join("bomb.gz"), &bomb).unwrap();
    let (receipt, most) = peak(|| fixture.ingest(&manifest("bomb", json!(["bomb.gz"]))));
    let receipt = receipt.unwrap();
    let item = &receipt["items"][0];
    assert_eq!(item["size_bytes"], 269);
    assert_eq!(item["digest"], sha256(&bomb));
    // A gzip header is not UTF-8, so no text derivative exists.
    assert_eq!(item["text_status"], "invalid_utf8");
    assert!(item.get("derivative_id").is_none_or(Value::is_null));
    assert!(most < 8 * MIB, "peak {most} bytes");
    let stored: u64 = fixture
        .tree()
        .iter()
        .filter_map(|path| fs::symlink_metadata(fixture.base.join(path)).ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum();
    assert!(stored < 4 * MIB as u64, "workspace holds {stored} bytes");
}

#[test]
fn an_archive_with_traversal_absolute_and_link_entries_is_never_extracted() {
    // Entries `../../escape.txt`, `/tmp/absolute.txt` and a symlink `link`
    // to `/etc/passwd`.
    let archive = fs::read(format!("{FIXTURES}/traversal.zip")).unwrap();
    let mut fixture = fixture();
    fs::write(fixture.root.join("upload.zip"), &archive).unwrap();
    let before = fixture.tree();
    let receipt = fixture
        .ingest(&manifest("archive", json!(["upload.zip"])))
        .unwrap();
    assert_eq!(receipt["items"][0]["digest"], sha256(&archive));
    assert_eq!(receipt["items"][0]["size_bytes"], archive.len());
    for path in fixture.tree() {
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(
            !["escape.txt", "absolute.txt", "link"].contains(&name.as_ref()),
            "{path:?} was extracted"
        );
        let metadata = fs::symlink_metadata(fixture.base.join(&path)).unwrap();
        assert!(!metadata.is_symlink(), "{path:?} is a link");
    }
    // Only the workspace changed; nothing appeared beside it.
    let outside: Vec<_> = fixture
        .tree()
        .into_iter()
        .filter(|path| !path.starts_with("workspace") && !before.contains(path))
        .collect();
    assert!(outside.is_empty(), "{outside:?}");
}

// ---- Path traversal ---------------------------------------------------------

#[test]
fn ingest_paths_cannot_leave_the_import_root() {
    let mut fixture = fixture();
    let secret = fixture.base.join("outside/secret.txt");
    symlink(&secret, fixture.root.join("link.txt")).unwrap();
    symlink(
        fixture.base.join("outside"),
        fixture.root.join("linked-dir"),
    )
    .unwrap();
    fs::hard_link(&secret, fixture.root.join("hard.txt")).unwrap();
    fs::create_dir(fixture.root.join("inside")).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        fixture.root.join("pipe"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();

    let cases = [
        ("../outside/secret.txt", ".."),
        ("inside/../../outside/secret.txt", ".."),
        (secret.to_str().unwrap(), "outside configured import roots"),
        ("link.txt", ""),
        ("linked-dir/secret.txt", ""),
        ("hard.txt", "MultipleLinks"),
        ("inside", "NotRegular"),
        // Opened non-blocking and refused as not regular: no hang on a FIFO.
        ("pipe", "NotRegular"),
    ];
    for (index, (path, reason)) in cases.into_iter().enumerate() {
        let started = Instant::now();
        let failure = fixture.refused(&format!("traversal-{index}"), path);
        assert!(started.elapsed().as_secs() < 10, "{path} took too long");
        assert_eq!(failure.error.path.as_deref(), Some("/paths/0"), "{path}");
        let message = &failure.error.message;
        assert!(
            message.contains(reason) || (reason == ".." && message.contains("InvalidPath")),
            "{path}: {message}"
        );
        assert!(!message.contains("outside the import root\""));
    }
    // The secret was never copied into the workspace.
    let secret_digest = sha256(&fs::read(&secret).unwrap());
    let hex = secret_digest.strip_prefix("sha256:").unwrap();
    assert!(
        !fixture
            .root
            .join("state/blobs/sha256")
            .join(&hex[..2])
            .join(hex)
            .exists()
    );
}

#[test]
fn a_file_over_the_import_limit_is_refused_before_it_is_read() {
    let mut fixture = fixture();
    // Sparse: the size is visible to the open check without writing it.
    let file = fs::File::create(fixture.root.join("huge.bin")).unwrap();
    file.set_len(MAX_IMPORT_BYTES + 1).unwrap();
    drop(file);
    let (failure, most) = peak(|| fixture.refused("huge", "huge.bin"));
    assert!(
        failure.error.message.contains("TooLarge"),
        "{}",
        failure.error.message
    );
    assert!(most < MIB, "peak {most} bytes");
}

#[test]
fn standard_input_over_the_import_limit_is_an_input_error_and_leaves_no_copy() {
    let mut fixture = fixture();
    let manifest = json!({
        "schema_version": "inv.ingest.v1",
        "command_id": "stdin-huge",
        "actor": "agent:hardening-test",
        "new_case": {"title": "Hostile inputs"},
        "source": {"title": "Endless stream"},
        "content": {"kind": "stdin", "name": "../../stream.bin"},
    });
    let mut stream = io::repeat(b'a').take(MAX_IMPORT_BYTES + 1);
    let (failure, most) = peak(|| {
        fixture
            .store
            .ingest(&serde_json::to_vec(&manifest).unwrap(), Some(&mut stream))
            .unwrap_err()
    });
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);
    assert!(failure.error.message.contains("import limit"));
    assert!(most < 2 * MIB, "peak {most} bytes");
    let leftovers: Vec<_> = fixture
        .tree()
        .into_iter()
        .filter(|path| path.to_string_lossy().contains("import-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // The stream's name is a label, never a path.
    assert!(!fixture.base.join("stream.bin").exists());
}

// ---- Nesting ----------------------------------------------------------------

fn nested(depth: usize) -> Vec<u8> {
    let mut bytes = vec![b'['; depth];
    bytes.extend(std::iter::repeat_n(b']', depth));
    bytes
}

#[test]
fn deep_nesting_stops_at_each_bound_without_recursion() {
    // An ingest manifest nested far past the request bound.
    let mut fixture = fixture();
    let mut bytes = br#"{"schema_version":"inv.ingest.v1","x":"#.to_vec();
    bytes.extend(nested(200_000));
    bytes.push(b'}');
    let (failure, most) = peak(|| fixture.store.ingest(&bytes, None).unwrap_err());
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);
    assert_eq!(failure.commit_status, CommitStatus::NotCommitted);
    assert!(most < 2 * MIB, "peak {most} bytes");
    let error = input::parse_request(&bytes).unwrap_err();
    assert_eq!(error.kind, InputErrorKind::TooDeep);
    assert_eq!(
        error.path.matches('/').count(),
        input::MAX_REQUEST_DEPTH + 1
    );

    // A producer-run envelope.
    let mut envelope = br#"{"schema_version":"inv.producer-run.v1","x":"#.to_vec();
    envelope.extend(nested(100_000));
    envelope.push(b'}');
    let failure = parse_envelope(&envelope).unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);

    // A JSON artifact read by the json_pointer selector.
    let artifact = nested(4 * MIB);
    let selector = SelectorKind::JsonPointer {
        pointer: "/0".into(),
        parser: None,
    };
    let (result, most) = peak(|| resolve(Some(&artifact), &selector, None, 1024));
    assert!(
        matches!(result, Err(SelectorError::Invalid(_))),
        "{result:?}"
    );
    assert!(most < 2 * MIB, "peak {most} bytes");
    let at_bound = nested(investigation_store::selectors::MAX_JSON_DEPTH + 1);
    assert!(resolve(Some(&at_bound), &selector, None, 1024).is_ok());
    let past = nested(investigation_store::selectors::MAX_JSON_DEPTH + 2);
    assert!(resolve(Some(&past), &selector, None, 1024).is_err());

    // The envelope probe skips nested values without building them, so a
    // deep but well-formed envelope still reports its version.
    let mut probe = br#"{"x":"#.to_vec();
    probe.extend(nested(500_000));
    probe.extend(br#","schema_version":"inv.producer-run.v9"}"#);
    let (result, most) = peak(|| probe_envelope(&probe));
    assert_eq!(
        result,
        EnvelopeProbe::UnsupportedVersion("inv.producer-run.v9".into())
    );
    assert!(most < 2 * MIB, "peak {most} bytes");
    let unclosed = vec![b'['; limits::get("ingest.probe_bytes").unwrap().value as usize];
    assert_eq!(probe_envelope(&unclosed), EnvelopeProbe::NotAnEnvelope);

    // A method pack.
    let mut pack = b"a = ".to_vec();
    pack.extend(nested(100_000));
    assert!(method_pack::load(&pack).is_err());
}

// ---- Huge lines ---------------------------------------------------------------

#[test]
fn a_huge_single_line_is_decoded_in_bounded_memory_and_selected_only_within_limits() {
    let mut fixture = fixture();
    let line = vec![b'a'; 16 * MIB];
    fs::write(fixture.root.join("line.txt"), &line).unwrap();
    let (receipt, most) = peak(|| fixture.ingest(&manifest("line", json!(["line.txt"]))));
    let receipt = receipt.unwrap();
    assert_eq!(receipt["items"][0]["text_status"], "decoded");
    // The copy, the digest and the UTF-8 check all stream.
    assert!(most < 4 * MIB, "peak {most} bytes for a 16 MiB line");

    let text = TextDerivative {
        encoding: "utf-8".into(),
        parser_version: "inv.text.utf8.v1".into(),
    };
    let lines = SelectorKind::TextLines {
        start_line: 1,
        end_line: 1,
        encoding: "utf-8".into(),
        parser_version: "inv.text.utf8.v1".into(),
    };
    let limit = limits::get("selector.excerpt_bytes").unwrap().value as usize;
    let (result, most) = peak(|| resolve(Some(&line), &lines, Some(&text), limit));
    assert_eq!(
        result,
        Err(SelectorError::TooLarge {
            size: line.len(),
            limit
        })
    );
    assert!(most < MIB, "peak {most} bytes");

    // Quote search is linear: every one of the overlapping matches is
    // counted in one pass, with memory proportional to the quote.
    let quote = "a".repeat(4096);
    let started = Instant::now();
    let (result, most) = peak(|| verify_quote(Some(&line), &quote, None, None));
    assert!(matches!(
        result,
        Ok(investigation_store::selectors::QuoteResult::Ambiguous { .. })
    ));
    assert!(most < MIB, "peak {most} bytes");
    assert!(started.elapsed().as_secs() < 30);

    // A table whose one row has a 16 MiB field, or too many fields.
    let mut table = b"wide\n".to_vec();
    table.extend(&line);
    let query = CellQuery {
        column: "wide",
        row_index: Some(0),
        key: None,
    };
    let (span, most) = peak(|| delimited::locate(&table, Format::Csv, query).unwrap());
    assert_eq!(span, (5, table.len()));
    assert!(most < MIB, "peak {most} bytes");
    let columns = limits::get("selector.table_columns").unwrap().value as usize;
    let wide = vec!["c"; columns + 1].join(",");
    assert!(matches!(
        delimited::locate(wide.as_bytes(), Format::Csv, CellQuery { column: "c", ..query }),
        Err(SelectorError::Invalid(message)) if message.contains("more than 4096 fields")
    ));
}

// ---- Invalid UTF-8 ------------------------------------------------------------

const INVALID: &[&[u8]] = &[
    b"\xff\xfe plain",
    b"overlong \xc0\xaf slash",
    b"surrogate \xed\xa0\x80 half",
    b"beyond \xf4\x90\x80\x80 range",
    b"truncated at the end \xe2\x82",
];

#[test]
fn invalid_utf8_is_reported_or_refused_never_decoded() {
    let mut fixture = fixture();
    for (index, bytes) in INVALID.iter().enumerate() {
        let name = format!("invalid-{index}.txt");
        fs::write(fixture.root.join(&name), bytes).unwrap();
        let receipt = fixture
            .ingest(&manifest(&format!("utf8-{index}"), json!([name])))
            .unwrap();
        let item = &receipt["items"][0];
        assert_eq!(item["text_status"], "invalid_utf8", "{bytes:?}");
        assert!(item.get("derivative_id").is_none_or(Value::is_null));
        assert_eq!(item["size_bytes"], bytes.len());

        // Readers over the same bytes refuse them.
        let text = TextDerivative {
            encoding: "utf-8".into(),
            parser_version: "inv.text.utf8.v1".into(),
        };
        assert!(matches!(
            verify_quote(Some(bytes), "plain", None, None),
            Err(SelectorError::Invalid(_))
        ));
        let lines = SelectorKind::TextLines {
            start_line: 1,
            end_line: 1,
            encoding: "utf-8".into(),
            parser_version: "inv.text.utf8.v1".into(),
        };
        assert!(matches!(
            resolve(Some(bytes), &lines, Some(&text), 1024),
            Err(SelectorError::Invalid(_))
        ));
        let query = CellQuery {
            column: "a",
            row_index: Some(0),
            key: None,
        };
        assert!(matches!(
            delimited::locate(bytes, Format::Tsv, query),
            Err(SelectorError::Invalid(_))
        ));
        assert_eq!(
            input::parse_request(bytes).unwrap_err().kind,
            InputErrorKind::InvalidUtf8
        );
    }
    // A manifest that is not UTF-8 is refused before anything is opened.
    let failure = fixture
        .store
        .ingest(b"{\"schema_version\":\"\xff\"}", None)
        .unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);
}

// ---- Properties -----------------------------------------------------------------

fn runner(seed: u8) -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases: 256,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[seed; 32]),
    )
}

/// Yields its input in pieces of a fixed size.
struct Pieces<'a>(&'a [u8], usize);

impl Read for Pieces<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let take = self.0.len().min(self.1).min(buffer.len());
        buffer[..take].copy_from_slice(&self.0[..take]);
        self.0 = &self.0[take..];
        Ok(take)
    }
}

#[test]
fn streamed_utf8_check_agrees_with_the_whole_input_check() {
    // Mostly UTF-8 with stray bytes, so both answers occur.
    let byte = prop_oneof![
        3 => prop::sample::select("aé☕𝄞\n".as_bytes().to_vec()),
        1 => any::<u8>(),
    ];
    let cases = (prop::collection::vec(byte, 0..512), 1usize..9);
    runner(7)
        .run(&cases, |(bytes, piece)| {
            prop_assert_eq!(
                utf8::is_valid(Pieces(&bytes, piece)).unwrap(),
                std::str::from_utf8(&bytes).is_ok()
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn the_table_reader_never_panics_and_returns_spans_inside_the_input() {
    let byte = prop::sample::select(b"ab,\t\"\r\n\xc3\xa9".to_vec());
    let cases = (
        prop::collection::vec(byte, 0..256),
        any::<bool>(),
        0u64..6,
        any::<bool>(),
    );
    runner(11)
        .run(&cases, |(bytes, csv, row, keyed)| {
            let format = if csv { Format::Csv } else { Format::Tsv };
            let query = CellQuery {
                column: "a",
                row_index: Some(row),
                key: keyed.then_some(("b", "ab")),
            };
            let first = delimited::locate(&bytes, format, query);
            prop_assert_eq!(&first, &delimited::locate(&bytes, format, query));
            if let Ok((start, end)) = first {
                prop_assert!(start <= end && end <= bytes.len());
                prop_assert!(std::str::from_utf8(&bytes[start..end]).is_ok());
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn bounded_json_never_panics_and_rejects_only_past_its_depth() {
    let byte = prop::sample::select(b"[]{}\":,1a ".to_vec());
    let cases = (prop::collection::vec(byte, 0..256), 1usize..12);
    runner(23)
        .run(&cases, |(bytes, depth)| {
            match input::parse_json(&bytes, 4096, depth) {
                Ok(value) => prop_assert!(nesting(&value) <= depth + 1),
                Err(error) => prop_assert!(error.kind != InputErrorKind::TooLarge),
            }
            Ok(())
        })
        .unwrap();
    for depth in 1..12 {
        let value = input::parse_json(&nested(depth), 4096, depth).unwrap();
        assert_eq!(nesting(&value), depth);
        let error = input::parse_json(&nested(depth + 2), 4096, depth).unwrap_err();
        assert_eq!(error.kind, InputErrorKind::TooDeep);
    }
}

/// Container levels, counting the root container as one.
fn nesting(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(nesting).max().unwrap_or(0),
        Value::Object(map) => 1 + map.values().map(nesting).max().unwrap_or(0),
        _ => 0,
    }
}

// ---- Documentation ----------------------------------------------------------------

/// The documented row for one limit, exactly as `docs/support-matrix.md`
/// shows it.
fn documented_row(limit: &limits::Limit) -> String {
    let unit = serde_json::to_value(limit.unit).unwrap();
    let refusal = limit.refusal.map_or("—".to_owned(), |code| {
        format!(
            "`{}`",
            serde_json::to_value(code).unwrap().as_str().unwrap()
        )
    });
    let readable = match limit.unit {
        limits::Unit::Bytes => {
            let mut value = limit.value;
            let mut suffix = "bytes";
            for next in ["KiB", "MiB", "GiB", "TiB"] {
                if value < 1024 || !value.is_multiple_of(1024) {
                    break;
                }
                value /= 1024;
                suffix = next;
            }
            format!("{value} {suffix}")
        }
        _ => limit.value.to_string(),
    };
    format!(
        "| `{}` | {readable} | {} | {} | {refusal} |",
        limit.name,
        unit.as_str().unwrap(),
        limit.applies_to,
    )
}

#[test]
fn the_documented_limits_match_the_table() {
    let document = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/support-matrix.md"
    ))
    .unwrap();
    let rows: Vec<&str> = document
        .lines()
        .filter(|line| line.starts_with("| `"))
        .collect();
    let expected: Vec<String> = limits::LIMITS.iter().map(documented_row).collect();
    assert_eq!(
        rows,
        expected,
        "docs/support-matrix.md limits table differs; expected rows:\n{}",
        expected.join("\n")
    );
    assert!(document.contains(EXPANSION.statement));
}
