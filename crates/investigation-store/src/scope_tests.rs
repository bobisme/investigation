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
    // A preparation cannot search for a claim the load did not locate.
    let claim = Claim::Attempt {
        producer: "example-producer".into(),
        external_run_id: "run-1".into(),
    };
    let _lock = fixture.store.coordinate().unwrap();
    let scoped = fixture
        .store
        .load_scope(Some(fixture.target), Reads::Target)
        .unwrap();
    assert!(scoped.replay.holding(&claim).is_err());
    let located = fixture
        .store
        .load_scope(Some(fixture.target), Reads::Claims(vec![claim.clone()]))
        .unwrap();
    assert_eq!(located.replay.holding(&claim).unwrap().len(), 1);
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

// ---- PERF.09: keyed ingest, capture begin and producer runs are
// case-scoped through the claims the index lists.

use crate::store::{
    capture::{CaptureDeclaration, DeclarationTarget},
    ingest::producer_run::ProducerRunSubmission,
};

/// The three recordings whose key may live in any case.
#[derive(Clone, Copy, Debug)]
enum Claimant {
    KeyedIngest,
    CaptureBegin,
    ProducerRun,
}

const CLAIMANTS: [Claimant; 3] = [
    Claimant::KeyedIngest,
    Claimant::CaptureBegin,
    Claimant::ProducerRun,
];

impl Fixture {
    /// A directory beneath the workspace's only import root.
    fn imports(&self) -> PathBuf {
        let imports = self.root.join("imports");
        if !imports.exists() {
            fs::create_dir(&imports).unwrap();
            let config = self.root.join("investigation.toml");
            let contents = fs::read_to_string(&config).unwrap();
            fs::write(
                &config,
                contents.replace(
                    "import_roots = []",
                    &format!("import_roots = [{:?}]", imports.display().to_string()),
                ),
            )
            .unwrap();
        }
        imports
    }

    fn reopen(&mut self) {
        self.store = Store::open(Workspace::open(&self.root).unwrap());
    }

    /// Record `external` through `claimant` into `case` with command `key`.
    /// The refusal or the case that now holds it.
    fn claim(
        &mut self,
        claimant: Claimant,
        key: &str,
        case: Id,
        external: &str,
    ) -> Result<Id, Failure> {
        let imports = self.imports();
        self.reopen();
        match claimant {
            Claimant::KeyedIngest => {
                let path = imports.join(format!("{key}.json"));
                fs::write(&path, format!("{{\"reading\":\"{key}\"}}\n")).unwrap();
                let manifest = json!({
                    "schema_version":"inv.ingest.v1","command_id":key,"actor":ACTOR,
                    "case_id":case,
                    "source":{"title":"Benchmark output","format":"application/json",
                        "acquisition_key":{"namespace":"example-bench","external_id":external}},
                    "paths":[path],"extract_text":false,
                });
                self.store
                    .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
                    .map(|_| case)
            }
            Claimant::CaptureBegin => {
                let declared = self.store.declare_capture(&CaptureDeclaration {
                    command_id: key.into(),
                    actor: ACTOR.parse().unwrap(),
                    target: DeclarationTarget::Existing(case),
                    run: json!({
                        "intent": {"kind": "exploration", "intent": "synthetic run"},
                        "producer": ACTOR, "tool": "example-producer",
                        "external_ref": external, "evidence_kind": "observational",
                    }),
                    producer: "example-producer".into(),
                    external_run_id: external.into(),
                    external_plan_digest: None,
                    metadata: BTreeMap::new(),
                    classification: investigation_core::header::ClassificationLevel::Internal,
                    request: json!({"opaque": key, "external_run_id": external}),
                })?;
                Ok(declared.case_id)
            }
            Claimant::ProducerRun => {
                fs::write(imports.join("bench.json"), b"{\"p99\":18.7}\n").unwrap();
                let envelope = json!({
                    "schema_version": "inv.producer-run.v1",
                    "producer": {"name": "example-bench", "version": "0.1.0"},
                    "external_run_id": external, "intent": {"label": "synthetic"},
                    "execution": {"state": "completed", "exit_code": 0},
                    "artifacts": [{"role": "original-output", "path": "./bench.json",
                        "media_type": "application/json"}]
                });
                let mut submission = ProducerRunSubmission::new(ACTOR.parse().unwrap());
                submission.case_id = Some(case);
                submission.command_id = Some(key.into());
                self.store
                    .ingest_producer_run(&serde_json::to_vec(&envelope).unwrap(), &submission)
                    .map(|_| case)
            }
        }
    }

    fn the_claim(claimant: Claimant, external: &str) -> Claim {
        match claimant {
            // An envelope's acquisition is its producer and external run ID.
            Claimant::KeyedIngest | Claimant::ProducerRun => Claim::Acquisition {
                namespace: "example-bench".into(),
                external_id: external.into(),
            },
            Claimant::CaptureBegin => Claim::Attempt {
                producer: "example-producer".into(),
                external_run_id: external.into(),
            },
        }
    }
}

/// The refusal of a second recording of one claim in another case.
fn refused_elsewhere(claimant: Claimant, failure: &Failure) {
    let expected = match claimant {
        Claimant::CaptureBegin => ErrorCode::IdempotencyKeyReused,
        Claimant::KeyedIngest | Claimant::ProducerRun => ErrorCode::InvalidInput,
    };
    assert_eq!(failure.error.code, expected, "{claimant:?}: {failure:?}");
}

#[test]
fn a_claim_held_by_another_case_is_found_through_the_index_and_only_that_case_is_loaded() {
    for claimant in CLAIMANTS {
        let mut fixture = fixture();
        let holder = fixture.untouched;
        fixture
            .claim(claimant, "first", holder, "attempt-1")
            .unwrap_or_else(|failure| panic!("{claimant:?}: {failure:?}"));
        // A third case the claim never touched is not loaded: a tamper there
        // stops nothing.
        let third = fixture
            .store
            .record(&command("third", None, "third"))
            .unwrap()
            .created[0]
            .id;
        fixture.record("third-q", third).unwrap();
        fixture.record("third-q2", third).unwrap();
        // Below its head: an edited head file would make the case stale.
        fixture.tamper(third, 2);

        let claim = Fixture::the_claim(claimant, "attempt-1");
        {
            let _lock = fixture.store.coordinate().unwrap();
            let scope = fixture
                .store
                .load_scope(Some(fixture.target), Reads::Claims(vec![claim.clone()]))
                .unwrap();
            let loaded: BTreeSet<Id> = scope.replay.cases.keys().copied().collect();
            assert_eq!(
                loaded,
                BTreeSet::from([fixture.target, holder]),
                "{claimant:?}"
            );
            assert!(!scope.replay.complete);
            assert_eq!(scope.replay.holding(&claim).unwrap().len(), 2);
        }
        // The same claim from another case is refused, as with a full load.
        let refused = fixture
            .claim(claimant, "second", fixture.target, "attempt-1")
            .unwrap_err();
        refused_elsewhere(claimant, &refused);
        // A fresh claim commits without loading the tampered case.
        fixture
            .claim(claimant, "fresh", fixture.target, "attempt-2")
            .unwrap_or_else(|failure| panic!("{claimant:?}: {failure:?}"));
        // The identical retry answers with the original receipt.
        fixture
            .claim(claimant, "fresh", fixture.target, "attempt-2")
            .unwrap_or_else(|failure| panic!("{claimant:?}: {failure:?}"));
        // The holder of a claim is verified before it answers.
        fixture.tamper(holder, 2);
        let blocked = fixture
            .claim(claimant, "blocked", fixture.target, "attempt-1")
            .unwrap_err();
        assert_eq!(
            blocked.error.code,
            ErrorCode::IntegrityFailure,
            "{claimant:?}"
        );
    }
}

#[test]
fn a_claim_behind_a_stale_or_deleted_index_is_still_found() {
    for claimant in CLAIMANTS {
        for damage in ["stale", "deleted"] {
            let mut fixture = fixture();
            let holder = fixture.untouched;
            fixture.imports();
            fixture.reopen();
            // The index from before the holder recorded the claim.
            fixture.record("settle", fixture.target).unwrap();
            let older = fs::read(fixture.index()).unwrap();
            fixture
                .claim(claimant, "first", holder, "attempt-1")
                .unwrap_or_else(|failure| panic!("{claimant:?}: {failure:?}"));
            fs::remove_file(fixture.index()).unwrap();
            if damage == "stale" {
                fs::write(fixture.index(), &older).unwrap();
            }
            let refused = fixture
                .claim(claimant, "second", fixture.target, "attempt-1")
                .unwrap_err();
            refused_elsewhere(claimant, &refused);
            // The retry in the holder still answers with its receipt.
            fixture
                .claim(claimant, "first", holder, "attempt-1")
                .unwrap_or_else(|failure| panic!("{claimant:?} {damage}: {failure:?}"));
            assert!(
                fixture.store.check().unwrap().integrity_ok(),
                "{claimant:?} {damage}"
            );
        }
    }
}

/// The ADR 0011 gap: a crash loses an event the index applied, and another
/// event is published later at the same sequence. The indexed head's
/// sequence is the canonical one, but its file is not (PERF.09).
#[test]
fn a_different_event_at_the_indexed_sequence_is_not_taken_as_current() {
    let mut fixture = fixture();
    let lost = fixture.event(fixture.untouched, 4);
    fixture.record("lost", fixture.untouched).unwrap();
    let indexed = fs::read(fixture.index()).unwrap();
    // The event the index applied is gone, and another takes its sequence.
    fs::remove_file(&lost).unwrap();
    fixture.reopen();
    let republished = fixture.record("republished", fixture.untouched).unwrap();
    assert_eq!(republished.snapshot.sequence, 4);
    // The index is back to the state that applied the lost event: its head
    // sequence is canonical, so only the file tells them apart.
    fs::remove_file(fixture.index()).unwrap();
    fs::write(fixture.index(), &indexed).unwrap();
    {
        let _lock = fixture.store.coordinate().unwrap();
        let scope = fixture
            .store
            .load_scope(Some(fixture.target), Reads::Target)
            .unwrap();
        assert!(
            scope.replay.cases.contains_key(&fixture.untouched),
            "the case whose head file changed is verified"
        );
    }
    // So the republished key is found, not recorded a second time.
    let reused = fixture.record("republished", fixture.target).unwrap_err();
    assert_eq!(reused.error.code, ErrorCode::IdempotencyKeyReused);
    // The commit that loaded the case recorded its head file again.
    fixture.record("after", fixture.target).unwrap();
    let _lock = fixture.store.coordinate().unwrap();
    let scope = fixture
        .store
        .load_scope(Some(fixture.target), Reads::Target)
        .unwrap();
    assert_eq!(
        scope.replay.cases.keys().copied().collect::<Vec<_>>(),
        vec![fixture.target]
    );
}
