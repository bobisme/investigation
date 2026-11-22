#![no_main]

#[path = "support/journal.rs"]
mod journal;

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
};

use investigation_store::journal::{JournalErrorKind, hash_bytes, replay_case};
use journal::{ReadOnlyJournal, assert_diagnostic, event_path, fixture};
use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};

fuzz_target!(|bytes: &[u8]| {
    let Some((&scenario, bytes)) = bytes.split_first() else {
        return;
    };
    let fixture = fixture();
    let mut files = fixture.files.clone();
    let path = event_path(2);
    let later = event_path(3);
    let expected = match scenario % 8 {
        0 => None,
        1 => {
            let event = files.get_mut(&path).unwrap();
            let offset = bytes
                .iter()
                .take(2)
                .fold(0usize, |offset, &byte| (offset << 8) | usize::from(byte));
            event.truncate(offset % (event.len() - 2));
            Some(JournalErrorKind::CorruptEvent)
        }
        2 | 5 | 6 => {
            let mut event: Value = serde_json::from_slice(&files[&path]).unwrap();
            let (field, value, expected) = match scenario % 8 {
                2 => (
                    "previous_hash",
                    json!(format!("sha256:{}", "0".repeat(64))),
                    JournalErrorKind::ParentHashMismatch,
                ),
                5 => (
                    "schema_version",
                    json!("inv.event.future"),
                    JournalErrorKind::UnsupportedEventVersion,
                ),
                _ => (
                    "request_hash",
                    json!(format!("sha256:{}", "0".repeat(64))),
                    JournalErrorKind::RequestHashMismatch,
                ),
            };
            event[field] = value;
            files.insert(path.clone(), serde_json::to_vec(&event).unwrap());
            Some(expected)
        }
        3 => {
            files.remove(&path);
            Some(JournalErrorKind::MissingEvent)
        }
        4 => {
            files.insert(PathBuf::from("case/events/2.json"), files[&path].clone());
            Some(JournalErrorKind::DuplicateSequence)
        }
        _ => {
            files.insert(path.clone(), bytes[..bytes.len().min(32768)].to_vec());
            None
        }
    };
    let journal = ReadOnlyJournal {
        files,
        reads: RefCell::new(Vec::new()),
    };
    match replay_case(&journal, Path::new("case"), fixture.workspace, fixture.case) {
        Ok(replay) => {
            assert!(expected.is_none());
            assert_eq!(replay.snapshot.sequence, 3);
            assert_eq!(replay.state.sequence(), 3);
            assert_eq!(replay.events.len(), 3);
            assert_eq!(
                replay.snapshot.head_hash,
                hash_bytes(&journal.files[&later])
            );
        }
        Err(error) => {
            assert_ne!(scenario % 8, 0, "unmodified fixture must replay");
            if error.object == path {
                assert!(!journal.reads.borrow().contains(&later));
            }
            if let Some(expected) = expected {
                assert_eq!(error.kind, expected);
                let reads = journal.reads.borrow();
                assert!(!reads.contains(&later), "replay read past a bad event");
                if expected == JournalErrorKind::DuplicateSequence {
                    assert_eq!(
                        reads.len(),
                        1,
                        "duplicate detected before opening event files"
                    );
                }
            }
            assert_diagnostic(error);
        }
    }
});
