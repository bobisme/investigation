//! Diagnostic logs name the persistence stage a failed commit reached, carry
//! transaction IDs, and never carry request content.
use super::crash_tests::{apply, request, restart, setup};
use super::*;
use std::{cell::RefCell, fs, sync::Once};

thread_local! {
    /// The capture active on this thread: its level and buffer.
    static CAPTURE: RefCell<Option<(tracing::Level, Vec<u8>)>> = const { RefCell::new(None) };
}

/// Writes one formatted event into this thread's capture, if any.
struct ThreadWriter {
    keep: bool,
}

impl Write for ThreadWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.keep {
            CAPTURE.with(|capture| {
                if let Some((_, buffer)) = capture.borrow_mut().as_mut() {
                    buffer.extend_from_slice(bytes);
                }
            });
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct ThreadSink;

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ThreadSink {
    type Writer = ThreadWriter;

    fn make_writer(&'a self) -> ThreadWriter {
        ThreadWriter { keep: false }
    }

    fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> ThreadWriter {
        let keep = CAPTURE.with(|capture| {
            capture
                .borrow()
                .as_ref()
                .is_some_and(|(level, _)| meta.level() <= level)
        });
        ThreadWriter { keep }
    }
}

/// Run `body` and return everything it logged on this thread at `level` or
/// above.
///
/// One global subscriber, enabled for every level and installed once, writes
/// to a per-thread buffer. Scoped `with_default` subscribers were flaky: tests
/// running at once with different levels race on tracing's global callsite
/// interest cache, and an event could be dropped.
pub(super) fn capture(level: tracing::Level, body: impl FnOnce()) -> String {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(ThreadSink)
            .with_ansi(false)
            .without_time()
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("no other global subscriber in the store tests");
    });
    CAPTURE.with(|capture| *capture.borrow_mut() = Some((level, Vec::new())));
    body();
    let bytes = CAPTURE
        .with(|capture| capture.borrow_mut().take())
        .map(|(_, bytes)| bytes)
        .unwrap_or_default();
    String::from_utf8(bytes).unwrap()
}

pub(super) fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let rest = line.split(&format!("{name}=\"")).nth(1)?;
    rest.split('"').next()
}

#[test]
fn failed_commit_log_names_the_stage_reached() {
    let mut stages = BTreeSet::new();
    let mut with_transaction = 0;
    for seed in 1..=70 {
        let (mut store, root) = setup(seed);
        let (command, semantic) = request(None, "diag-create");
        store.fs.fail_at(seed as usize);
        let mut outcome = None;
        let log = capture(tracing::Level::WARN, || {
            outcome = Some(apply(&mut store, &command, &semantic, 100));
        });
        if let Some(Err(failure)) = outcome {
            assert!(
                matches!(
                    failure.error.code,
                    ErrorCode::IoFailure | ErrorCode::DurabilityUnknown
                ),
                "seed {seed}"
            );
            let line = log
                .lines()
                .find(|line| line.contains("commit failed"))
                .unwrap_or_else(|| panic!("seed {seed} logged no failure:\n{log}"));
            assert_eq!(field(line, "command_id"), Some("diag-create"));
            stages.insert(field(line, "stage").unwrap().to_owned());
            if field(line, "transaction_id").is_some_and(|id| id.starts_with("tx_")) {
                with_transaction += 1;
            }
            if failure.error.code == ErrorCode::DurabilityUnknown {
                assert_eq!(field(line, "commit_status"), Some("unknown"), "{line}");
            }
            assert!(!log.contains("Crash fixture"), "request text leaked: {log}");
            assert!(!log.contains(&root.display().to_string()), "path leaked");
        }
        restart(&mut store);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
    for expected in ["journal_write", "journal_publish", "directory_sync"] {
        assert!(
            stages.contains(expected),
            "no failure reached {expected}: {stages:?}"
        );
    }
    assert!(with_transaction > 0, "no failure carried a transaction ID");
}

#[test]
fn debug_log_traces_stages_without_request_content() {
    let (mut store, root) = setup(900);
    let (command, semantic) = request(None, "diag-ok");
    let log = capture(tracing::Level::DEBUG, || {
        apply(&mut store, &command, &semantic, 5).unwrap();
    });
    let order: Vec<_> = log.lines().filter_map(|l| field(l, "stage")).collect();
    let position = |name: &str| order.iter().position(|stage| *stage == name).unwrap();
    assert!(position("validate") < position("journal_write"));
    assert!(position("journal_write") < position("journal_publish"));
    assert!(position("directory_sync") < position("index_update"));
    assert!(log.contains("commit complete") || order.contains(&"receipt"));
    assert!(!log.contains("Crash fixture"), "request text leaked: {log}");
    assert!(!log.contains(&root.display().to_string()), "path leaked");
    fs::remove_dir_all(root).unwrap();
}
