//! A command that names one case verifies that case and what it reads, and
//! nothing else; integrity is not weakened (PERF.06, ADR 0011).

use std::fs;

use serde_json::json;

use super::*;

const ACTOR: &str = "agent:scope-fixture";

fn command(key: &str, case: Option<Id>, wording: &str) -> Vec<u8> {
    let operation = match case {
        None => json!({"op":"case.create","payload":{"title":wording,"mode":"record"}}),
        Some(_) => json!({
            "op":"question.create","payload":{"wording":wording,"importance":"normal"},
        }),
    };
    serde_json::to_vec(&json!({
        "schema_version":"inv.command.v0-alpha","command_id":key,"case_id":case,
        "actor":ACTOR,"preconditions":[],"operations":[operation],
    }))
    .unwrap()
}

/// Two cases of three events each, with a current index.
struct Fixture {
    _scratch: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    target: Id,
    untouched: Id,
}

fn fixture() -> Fixture {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().to_path_buf();
    let mut store = Store::open(Workspace::init(&root, false).unwrap());
    let mut create = |key: &str| {
        let case = store.record(&command(key, None, key)).unwrap().created[0].id;
        for index in 1..=2 {
            store
                .record(&command(
                    &format!("{key}-q{index}"),
                    Some(case),
                    &format!("Which {key} {index}?"),
                ))
                .unwrap();
        }
        case
    };
    let target = create("target");
    let untouched = create("untouched");
    Fixture {
        _scratch: scratch,
        root,
        store,
        target,
        untouched,
    }
}

impl Fixture {
    fn event(&self, case: Id, sequence: u64) -> PathBuf {
        self.root
            .join(case_directory(case))
            .join("events")
            .join(event_filename(sequence).unwrap())
    }

    /// Edit a published event in place, as a tamperer would.
    fn tamper(&self, case: Id, sequence: u64) {
        let path = self.event(case, sequence);
        let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        event["request"]["input"]["operations"][0]["payload"]["wording"] =
            json!("Edited after commit?");
        fs::remove_file(&path).unwrap();
        fs::write(&path, serde_json::to_vec(&event).unwrap()).unwrap();
    }

    fn index(&self) -> PathBuf {
        self.root.join("cache/index.sqlite")
    }

    fn record(&mut self, key: &str, case: Id) -> Result<Receipt, Failure> {
        self.store
            .record(&command(key, Some(case), &format!("{key}?")))
    }
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
fn a_tampered_case_blocks_neither_a_commit_nor_a_read_elsewhere_and_check_reports_it() {
    let mut fixture = fixture();
    fixture.tamper(fixture.untouched, 2);

    // The commit and the read verify the target alone.
    let receipt = fixture.record("after", fixture.target).unwrap();
    assert_eq!(receipt.snapshot.sequence, 4);
    assert_eq!(receipt.projection.index, ProjectionStatus::Current);
    assert_eq!(
        fixture.store.case(fixture.target).unwrap().snapshot,
        receipt.snapshot
    );
    // A retry of a key the untouched case does not hold is answered too.
    let retry = fixture.record("after", fixture.target).unwrap();
    assert_eq!(retry.transaction_id, receipt.transaction_id);

    // Every command that loads the tampered case refuses, and check reports it.
    let read = fixture.store.case(fixture.untouched).unwrap_err();
    assert_eq!(read.error.code, ErrorCode::IntegrityFailure);
    assert_eq!(diagnostic(&read), Some("EVENT_REQUEST_HASH_MISMATCH"));
    let commit = fixture
        .record("into-tampered", fixture.untouched)
        .unwrap_err();
    assert_eq!(diagnostic(&commit), diagnostic(&read));
    assert!(fixture.store.cases().is_err());
    assert!(fixture.store.list_cases().is_err());
    let report = fixture.store.check().unwrap();
    assert!(!report.integrity_ok());
    assert_eq!(report.issues[0].code, "EVENT_REQUEST_HASH_MISMATCH");
}

#[test]
fn a_tampered_target_still_refuses() {
    for sequence in [1, 3] {
        let mut fixture = fixture();
        fixture.tamper(fixture.target, sequence);
        let refused = fixture.record("after", fixture.target).unwrap_err();
        assert_eq!(refused.error.code, ErrorCode::IntegrityFailure);
        assert_eq!(diagnostic(&refused), Some("EVENT_REQUEST_HASH_MISMATCH"));
        assert!(fixture.store.case(fixture.target).is_err());
        // Nothing was published.
        assert!(!fixture.event(fixture.target, 4).exists());
    }
}

#[test]
fn a_key_recorded_in_another_case_is_found_and_verified_through_the_index() {
    let mut fixture = fixture();
    // The same key with the same request answers with the original receipt,
    // although the commit loaded only its own case first.
    let original = fixture.record("shared", fixture.untouched).unwrap();
    let retry = fixture.record("shared", fixture.untouched).unwrap();
    assert_eq!(retry.transaction_id, original.transaction_id);
    // The same key from the other case is a different request: refused.
    let reused = fixture.record("shared", fixture.target).unwrap_err();
    assert_eq!(reused.error.code, ErrorCode::IdempotencyKeyReused);
    // So is a key created with the untouched case itself.
    let created = fixture.record("untouched-q1", fixture.target).unwrap_err();
    assert_eq!(created.error.code, ErrorCode::IdempotencyKeyReused);

    // The case that holds the key is verified before it answers: a
    // tampered holder refuses instead of answering from the index.
    fixture.tamper(fixture.untouched, 2);
    let refused = fixture.record("shared", fixture.target).unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::IntegrityFailure);
    assert_eq!(diagnostic(&refused), Some("EVENT_REQUEST_HASH_MISMATCH"));
}

#[test]
fn a_case_extended_behind_the_index_is_verified_and_answered_from_its_events() {
    let mut fixture = fixture();
    // An older copy of the index: the untouched case's indexed head is one
    // event behind, as after a writer whose index update failed.
    let older = fs::read(fixture.index()).unwrap();
    let extended = fixture.record("behind", fixture.untouched).unwrap();
    fs::remove_file(fixture.index()).unwrap();
    fs::write(fixture.index(), &older).unwrap();

    // The key the index lacks is still found, so a retry is not recorded twice.
    let refused = fixture.record("behind", fixture.target).unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::IdempotencyKeyReused);
    let retry = fixture.record("behind", fixture.untouched).unwrap();
    assert_eq!(retry.transaction_id, extended.transaction_id);
    // A commit elsewhere brings the index up to date for the case it loaded.
    fixture.record("elsewhere", fixture.target).unwrap();
    let heads: BTreeMap<_, _> = Projection::inspect(&fixture.index(), fixture.store.workspace.id())
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(heads[&fixture.untouched], extended.snapshot);
    assert!(fixture.store.check().unwrap().integrity_ok());

    // A tampered case that the index is behind on is verified, so it refuses.
    let older = fs::read(fixture.index()).unwrap();
    fixture.record("behind-again", fixture.untouched).unwrap();
    fs::remove_file(fixture.index()).unwrap();
    fs::write(fixture.index(), &older).unwrap();
    fixture.tamper(fixture.untouched, 2);
    let refused = fixture.record("blocked", fixture.target).unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::IntegrityFailure);
}

#[test]
fn an_unusable_index_gives_way_to_verifying_every_case() {
    let mut fixture = fixture();
    let shared = fixture.record("shared", fixture.untouched).unwrap();
    for damage in ["absent", "garbage", "another case"] {
        match damage {
            "absent" => fs::remove_file(fixture.index()).unwrap(),
            "garbage" => {
                fs::remove_file(fixture.index()).unwrap();
                fs::write(fixture.index(), b"not an index").unwrap();
            }
            _ => {
                // The index names a case that is not published.
                let database = rusqlite::Connection::open(fixture.index()).unwrap();
                database
                    .execute(
                        "INSERT INTO heads (case_id, sequence, head_hash) VALUES (?1, 1, ?2)",
                        rusqlite::params![
                            "case_01a20914-25fa-7041-91e9-0fe450b811f5",
                            hash_bytes(b"x").as_str()
                        ],
                    )
                    .unwrap();
            }
        }
        // Every case is verified, so the key is found in the other case.
        let reused = fixture.record("shared", fixture.target).unwrap_err();
        assert_eq!(
            reused.error.code,
            ErrorCode::IdempotencyKeyReused,
            "{damage}"
        );
        let retry = fixture.record("shared", fixture.untouched).unwrap();
        assert_eq!(retry.transaction_id, shared.transaction_id, "{damage}");
        // A commit rebuilds the index.
        fixture
            .record(
                &format!("rebuild-{}", damage.replace(' ', "-")),
                fixture.target,
            )
            .unwrap();
        assert!(fixture.store.check().unwrap().integrity_ok(), "{damage}");
    }
    // With the index unusable, a tampered case anywhere blocks the commit:
    // it is verified like every other.
    fs::remove_file(fixture.index()).unwrap();
    fixture.tamper(fixture.untouched, 2);
    let refused = fixture.record("blocked", fixture.target).unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::IntegrityFailure);
}

#[test]
fn an_id_recorded_in_an_unloaded_case_is_refused() {
    let mut fixture = fixture();
    let scope = {
        let _lock = fixture.store.coordinate().unwrap();
        fixture
            .store
            .load_scope(Some(fixture.target), Reads::Target)
            .unwrap()
    };
    // Only the target was verified; the index answers for the other case.
    assert_eq!(
        scope.replay.cases.keys().copied().collect::<Vec<_>>(),
        vec![fixture.target]
    );
    let elsewhere = fixture
        .store
        .case(fixture.untouched)
        .unwrap()
        .state
        .records()
        .map(|record| record.header.id)
        .collect::<Vec<_>>();
    let transaction = fixture.store.case(fixture.untouched).unwrap().events[1]
        .event
        .transaction_id;
    let fresh: Id = "q_01a20914-25fa-7041-91e9-0fe450b811f5".parse().unwrap();
    for id in elsewhere.iter().copied().chain([transaction]) {
        assert!(
            fixture.store.already_recorded(&scope, [fresh, id]).unwrap(),
            "{id}"
        );
    }
    assert!(!fixture.store.already_recorded(&scope, [fresh]).unwrap());
    drop(scope);
    // A workspace-wide preparation cannot run on a case-scoped load.
    let _lock = fixture.store.coordinate().unwrap();
    let scoped = fixture
        .store
        .load_scope(Some(fixture.target), Reads::Target)
        .unwrap();
    assert!(scoped.replay.all().is_err());
    let full = fixture
        .store
        .load_scope(Some(fixture.target), Reads::Workspace)
        .unwrap();
    assert_eq!(full.replay.all().unwrap().len(), 2);
}

#[test]
fn a_selected_case_is_listed_without_verifying_any_case() {
    let mut fixture = fixture();
    fixture.tamper(fixture.untouched, 2);
    let mut ids = fixture.store.case_ids().unwrap();
    ids.sort_unstable();
    let mut expected = vec![fixture.target, fixture.untouched];
    expected.sort_unstable();
    assert_eq!(ids, expected);
    // An entry that is not a case is still an integrity failure.
    fs::create_dir(fixture.root.join(CASES).join("stray")).unwrap();
    assert_eq!(
        fixture.store.case_ids().unwrap_err().error.code,
        ErrorCode::IntegrityFailure
    );
}
