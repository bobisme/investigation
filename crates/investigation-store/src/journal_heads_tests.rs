//! A kept head speeds verification up and never weakens it (PERF.01).

use std::fs;

use serde_json::json;

use super::*;

struct Fixture {
    _scratch: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    case: Id,
}

fn command(key: &str, case: Option<Id>, wording: &str) -> Vec<u8> {
    let operation = match case {
        None => json!({"op":"case.create","payload":{"title":wording,"mode":"record"}}),
        Some(_) => json!({
            "op":"question.create","payload":{"wording":wording,"importance":"normal"},
        }),
    };
    serde_json::to_vec(&json!({
        "schema_version":"inv.command.v0-alpha","command_id":key,"case_id":case,
        "actor":"agent:head-fixture","preconditions":[],"operations":[operation],
    }))
    .unwrap()
}

/// A case with four events whose head was kept by a load.
fn fixture() -> Fixture {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().to_path_buf();
    let mut store = Store::open(Workspace::init(&root, false).unwrap());
    let case = store
        .record(&command("create", None, "Kept head fixture"))
        .unwrap()
        .created[0]
        .id;
    for key in ["q-1", "q-2", "q-3"] {
        store
            .record(&command(key, Some(case), &format!("Which {key}?")))
            .unwrap();
    }
    keep(&store);
    let fixture = Fixture {
        _scratch: scratch,
        root,
        store,
        case,
    };
    assert_eq!(
        fixture.store.known_head(case).map(|head| head.sequence),
        Some(4)
    );
    fixture
}

impl Fixture {
    fn event(&self, sequence: u64) -> PathBuf {
        self.root
            .join(case_directory(self.case))
            .join("events")
            .join(event_filename(sequence).unwrap())
    }

    fn replace(&self, sequence: u64, bytes: &[u8]) {
        let path = self.event(sequence);
        fs::remove_file(&path).unwrap();
        fs::write(&path, bytes).unwrap();
    }

    fn edit(&self, sequence: u64, change: impl FnOnce(&mut Value)) {
        let mut event: Value =
            serde_json::from_slice(&fs::read(self.event(sequence)).unwrap()).unwrap();
        change(&mut event);
        // Written in this program's layout when the edit still decodes, so
        // the chain, not the layout (ADR 0016), is what a load refuses.
        let bytes = serde_json::from_value::<Event>(event.clone())
            .map_or_else(
                |_| serde_json::to_vec(&event),
                |event| serde_json::to_vec(&event),
            )
            .unwrap();
        self.replace(sequence, &bytes);
    }

    fn head(&self) -> Snapshot {
        self.store.load_fully_verified().unwrap().cases[&self.case]
            .snapshot
            .clone()
    }

    fn keep(&self, sequence: u64, head_hash: Digest) {
        self.store.keep_head(
            self.case,
            &Snapshot {
                sequence,
                head_hash,
            },
        );
    }
}

/// Load as a commit does, and keep the verified heads as a published commit
/// does.
fn keep(store: &Store) -> WorkspaceReplay {
    let replay = store.load_verified().unwrap();
    store.keep_verified_heads(&replay);
    replay
}

fn diagnostic(failure: &Failure) -> Option<&str> {
    failure
        .error
        .actual
        .as_ref()?
        .get("diagnostic_code")?
        .as_str()
}

#[test]
fn a_kept_head_gives_the_same_replay_and_idempotency_as_a_full_verification() {
    let mut fixture = fixture();
    let reused = fixture.store.load_verified().unwrap();
    let full = fixture.store.load_fully_verified().unwrap();
    assert_eq!(reused.cases, full.cases);
    assert_eq!(reused.commands, full.commands);
    assert_eq!(reused.transactions, full.transactions);
    assert_eq!(reused.records, full.records);

    // Reads, check, retries and refused commands use kept heads but never
    // write one; only a published commit does.
    let entry = fixture.root.join(entry_path(fixture.case));
    fs::remove_file(&entry).unwrap();
    fixture.store.case(fixture.case).unwrap();
    fixture.store.check().unwrap();
    fixture
        .store
        .record(&command("q-1", Some(fixture.case), "Which q-1?"))
        .unwrap();
    let refused = serde_json::to_vec(&json!({
        "schema_version":"inv.command.v0-alpha","command_id":"refused",
        "case_id":fixture.case,"actor":"agent:head-fixture","preconditions":[],
        "operations":[{"op":"question.create","payload":{"importance":"normal"}}],
    }))
    .unwrap();
    fixture.store.record(&refused).unwrap_err();
    assert!(!entry.exists());
    fixture
        .store
        .record(&command("q-0", Some(fixture.case), "Which q-0?"))
        .unwrap();
    assert_eq!(fixture.store.known_head(fixture.case).unwrap(), {
        let head = fixture.store.load_fully_verified().unwrap().cases[&fixture.case].events[3]
            .hash
            .clone();
        KnownHead {
            sequence: 4,
            head_hash: head,
        }
    });

    // A retry with a warm head returns the original receipt; a different
    // request under the same command ID is refused.
    let original = full.find(&"agent:head-fixture".parse().unwrap(), "q-2");
    let retry = fixture
        .store
        .record(&command("q-2", Some(fixture.case), "Which q-2?"))
        .unwrap();
    assert_eq!(
        Some(retry.transaction_id),
        original.map(|event| event.event.transaction_id)
    );
    assert_eq!(retry.snapshot.sequence, 3);
    let reused_key = fixture
        .store
        .record(&command("q-2", Some(fixture.case), "Something else?"))
        .unwrap_err();
    assert_eq!(reused_key.error.code, ErrorCode::IdempotencyKeyReused);

    // A commit after the kept head verifies the new tail and keeps it.
    let next = fixture
        .store
        .record(&command("q-4", Some(fixture.case), "Which q-4?"))
        .unwrap();
    keep(&fixture.store);
    let kept = fixture.store.known_head(fixture.case).unwrap();
    assert_eq!(
        (kept.sequence, kept.head_hash),
        (next.snapshot.sequence, next.snapshot.head_hash)
    );
}

/// A name, the diagnostic a full verification reports, and the tampering.
type Tamper = (&'static str, &'static str, fn(&Fixture));

#[test]
fn a_tampered_or_truncated_tail_is_detected_with_a_warm_head() {
    let cases: [Tamper; 7] = [
        ("truncated head", "CORRUPT_EVENT", |fixture| {
            let bytes = fs::read(fixture.event(4)).unwrap();
            fixture.replace(4, &bytes[..bytes.len() / 2]);
        }),
        (
            "head with another parent",
            "EVENT_PARENT_HASH_MISMATCH",
            |fixture| {
                fixture.edit(4, |event| {
                    event["previous_hash"] = json!(format!("sha256:{}", "a".repeat(64)));
                });
            },
        ),
        (
            "head with an edited request",
            "EVENT_REQUEST_HASH_MISMATCH",
            |fixture| {
                fixture.edit(4, |event| {
                    event["request"]["input"]["operations"][0]["payload"]["wording"] =
                        json!("Edited after commit?");
                });
            },
        ),
        (
            "edited event before the head",
            "EVENT_PARENT_HASH_MISMATCH",
            |fixture| {
                fixture.edit(2, |event| {
                    event["accepted_operations"]["records"][0]["body"]["wording"] =
                        json!("Edited after commit?");
                });
            },
        ),
        (
            "missing event before the head",
            "MISSING_EVENT",
            |fixture| {
                fs::remove_file(fixture.event(2)).unwrap();
            },
        ),
        (
            "unsupported head version",
            "UNSUPPORTED_EVENT_VERSION",
            |fixture| {
                fixture.edit(4, |event| {
                    event["schema_version"] = json!("inv.event.future")
                });
            },
        ),
        (
            "edited head under a kept head ahead of the journal",
            "EVENT_REQUEST_HASH_MISMATCH",
            |fixture| {
                fixture.edit(4, |event| {
                    event["request"]["input"]["operations"][0]["payload"]["wording"] =
                        json!("Edited after commit?");
                });
                fixture.keep(9, hash_bytes(b"a head beyond the journal"));
            },
        ),
    ];
    for (name, code, tamper) in cases {
        let mut fixture = fixture();
        tamper(&fixture);
        let refused = fixture
            .store
            .record(&command("after", Some(fixture.case), "After tampering?"))
            .unwrap_err();
        assert_eq!(refused.error.code, ErrorCode::IntegrityFailure, "{name}");
        assert_eq!(diagnostic(&refused), Some(code), "{name}: {refused:?}");
        let report = fixture.store.check().unwrap();
        assert!(!report.integrity_ok(), "{name}");
        assert_eq!(
            report.issues[0].code,
            diagnostic(&refused).unwrap(),
            "{name}"
        );
        // Reads verify the same way.
        assert!(fixture.store.case(fixture.case).is_err(), "{name}");
        assert!(fixture.store.cases().is_err(), "{name}");
    }
}

#[test]
fn a_stale_or_corrupt_head_falls_back_to_full_verification() {
    let fixture = fixture();
    let head = fixture.head();
    let other = hash_bytes(b"another head");
    let entry = entry_path(fixture.case);
    for name in [
        "garbage",
        "another hash at the head",
        "another hash before the head",
        "ahead of the journal",
        "another workspace",
    ] {
        match name {
            "garbage" => fixture.store.fs.write_cache(&entry, b"\x00garbage"),
            "another hash at the head" => fixture.keep(4, other.clone()),
            "another hash before the head" => fixture.keep(2, other.clone()),
            "ahead of the journal" => fixture.keep(9, other.clone()),
            _ => {
                let line = fs::read_to_string(fixture.root.join(&entry)).unwrap();
                let workspace = fixture.store.workspace.id().to_string();
                let foreign = format!("{}beef", &workspace[..workspace.len() - 4]);
                assert_ne!(foreign, workspace);
                let line = line.replace(&workspace, &foreign);
                fixture.store.fs.write_cache(&entry, line.as_bytes());
            }
        }
        let loaded = keep(&fixture.store);
        assert_eq!(
            loaded.cases,
            fixture.store.load_fully_verified().unwrap().cases,
            "{name}"
        );
        // The fallback keeps the head it verified.
        let kept = fixture.store.known_head(fixture.case).unwrap();
        assert_eq!(
            (kept.sequence, &kept.head_hash),
            (head.sequence, &head.head_hash),
            "{name}"
        );
    }

    // An event lost before its directory sync leaves a journal shorter than
    // the kept head: that is not an error, and the shorter head is kept.
    let mut fixture = fixture;
    fs::remove_file(fixture.event(4)).unwrap();
    let shorter = keep(&fixture.store);
    assert_eq!(shorter.cases[&fixture.case].snapshot.sequence, 3);
    assert_eq!(fixture.store.known_head(fixture.case).unwrap().sequence, 3);
    let receipt = fixture
        .store
        .record(&command("q-3", Some(fixture.case), "Which q-3?"))
        .unwrap();
    assert_eq!(receipt.snapshot.sequence, 4);
}

#[test]
fn check_verifies_every_byte_even_when_a_kept_head_matches() {
    let mut fixture = fixture();
    // Edit the head so its bytes still decode but fail metadata validation,
    // and keep the edited head as if this program had verified it. A load
    // that trusts the entry accepts it; check verifies in full regardless.
    fixture.edit(4, |event| {
        event["command_id"] = json!("not-the-request-key")
    });
    let edited = hash_bytes(&fs::read(fixture.event(4)).unwrap());
    fixture.keep(4, edited);
    assert!(fixture.store.load_verified().is_ok());
    let report = fixture.store.check().unwrap();
    assert!(!report.integrity_ok());
    assert_eq!(report.issues[0].code, "EVENT_METADATA_MISMATCH");
}

#[test]
fn a_decode_of_verified_bytes_equals_the_full_decode() {
    let fixture = fixture();
    for sequence in 1..=4 {
        let path = fixture.event(sequence);
        let bytes = fs::read(&path).unwrap();
        assert_eq!(
            crate::journal::decode_event(&path, sequence, &bytes).unwrap(),
            crate::journal::decode_known(&path, sequence, &bytes).unwrap()
        );
    }
}
