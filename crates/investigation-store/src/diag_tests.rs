//! Diagnostic logs name the persistence stage a failed commit reached, carry
//! transaction IDs, and never carry request content.
use super::crash_tests::{apply, request, restart, setup};
use super::*;
use std::{
    fs,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Run `body` and return everything logged at `level` or above.
fn capture(level: tracing::Level, body: impl FnOnce()) -> String {
    let buffer = Buffer::default();
    let sink = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(move || sink.clone())
        .with_ansi(false)
        .without_time()
        .finish();
    tracing::subscriber::with_default(subscriber, body);
    let bytes = buffer.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
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
