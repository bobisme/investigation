//! The indexes a State derives from its history (bn-9muo) answer as the walks
//! over every record they replaced did, on the scenario fixtures and on
//! generated bench fixtures (bn-3hr2). Each case is rebuilt one transaction
//! at a time from its verified journal; before every transaction the indexes
//! are built (by the check itself), and the transaction is then applied both
//! to that state and to a clone of it, so each step checks that a later
//! transaction, and a clone changed after the index was built, see fresh
//! results. Hand-made states that hold every kind of indexed record are in
//! investigation-core's derived_check tests.

use std::{
    fs,
    path::{Path, PathBuf},
};

use investigation_bench::fixtures::{self, ChoiceShape, LargeShape};
use investigation_core::{
    derived_check::{Checked, check},
    identity::{Id, Kind},
    state::State,
};
use investigation_store::{Store, Workspace};
use serde_json::json;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

/// Copy a checked-in workspace, so that reading it writes no cache entry
/// into the repository, with every copy writable.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
            let mut permissions = fs::metadata(&target).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            fs::set_permissions(&target, permissions).unwrap();
        }
    }
}

fn agree(label: &str, state: &State) -> Checked {
    let checked = check(state);
    assert!(
        checked.mismatches.is_empty(),
        "{label}: {:#?}",
        checked.mismatches
    );
    checked
}

/// Rebuild every case of `root` one transaction at a time, checking every
/// index before and after each transaction, on the state and on a clone.
fn rebuild(label: &str, root: &Path) -> Checked {
    let mut store = Store::open(Workspace::open(root).unwrap());
    let mut total = Checked::default();
    for (case_id, replay) in store.cases().unwrap() {
        let mut state = State::default();
        for (step, verified) in replay.events.iter().enumerate() {
            let label = format!("{label} {case_id} event {}", step + 1);
            agree(&label, &state);
            let mut clone = state.clone();
            clone.apply(&verified.event.accepted).unwrap();
            agree(&format!("{label} (clone)"), &clone);
            state.apply(&verified.event.accepted).unwrap();
            agree(&label, &state);
        }
        let checked = agree(&format!("{label} {case_id}"), &state);
        // The rebuilt state holds the verified one's records.
        assert_eq!(
            replay.state.records().count(),
            state.records().count(),
            "{label}"
        );
        total.keys += checked.keys;
        total.classification_changes += checked.classification_changes;
        total.attestations += checked.attestations;
        total.reviews += checked.reviews;
    }
    total
}

#[test]
fn derived_indexes_answer_as_their_walks_on_every_scenario_fixture() {
    let temporary = tempfile::tempdir().unwrap();
    let mut reviews = 0;
    for name in [
        "fixtures/vault-goldens/scenario-a/workspace",
        "fixtures/vault-goldens/scenario-b/workspace",
        "fixtures/vault-goldens/scenario-c/workspace",
        "fixtures/vault-goldens/scenario-d/workspace",
        "fixtures/historical/v1-2026-11-24",
    ] {
        let copy = temporary.path().join(name.replace('/', "-"));
        copy_tree(&repository().join(name), &copy);
        // Git keeps no empty directory: restore the empty parts of the layout.
        for directory in [
            "state/cases",
            "state/events",
            "state/blobs/sha256",
            "state/checkpoints",
            "state/staging",
            "state/locks",
            "state/tmp",
            "cache",
            "local",
            "exports",
            "vault/Notes",
            "vault/Boards",
            "vault/Inbox",
        ] {
            fs::create_dir_all(copy.join(directory)).unwrap();
        }
        let checked = rebuild(name, &copy);
        assert!(checked.keys > 0, "{name}");
        reviews += checked.reviews;
    }
    // The fixtures hold submitted Reviews, so the review index is compared
    // on real records; they hold no classification change or attestation.
    assert!(reviews > 0, "the scenario fixtures hold reviews");
}

#[test]
fn derived_indexes_answer_as_their_walks_on_generated_bench_fixtures() {
    let temporary = tempfile::tempdir().unwrap();
    let large = fixtures::large(&temporary.path().join("large"), LargeShape::smoke()).unwrap();
    let sparse = fixtures::choice(
        &temporary.path().join("sparse"),
        "sparse",
        ChoiceShape::smoke_sparse(),
    )
    .unwrap();
    for fixture in [&large, &sparse] {
        // Real classification changes, committed through the store, on
        // Observations the generator made.
        let mut store = Store::open(Workspace::open(&fixture.root).unwrap());
        let observations: Vec<Id> = fixture
            .revisions
            .keys()
            .copied()
            .filter(|id| id.kind() == Kind::Observation)
            .take(3)
            .collect();
        assert!(!observations.is_empty(), "{}", fixture.name);
        for (n, subject) in observations.iter().enumerate() {
            let request = json!({
                "schema_version": "inv.command.v1", "command_id": format!("raise-{n}"),
                "case_id": fixture.case, "actor": "agent:bench", "preconditions": [],
                "operations": [{"op": "classification.raise", "payload": {
                    "subject_ref": fixture.reference(*subject), "level": "restricted",
                    "reason": "Bench classification change."
                }}]
            });
            store
                .record(&serde_json::to_vec(&request).unwrap())
                .unwrap_or_else(|failure| panic!("{}: {failure:?}", fixture.name));
        }
        let checked = rebuild(&fixture.name, &fixture.root);
        assert!(
            checked.classification_changes >= observations.len(),
            "{}: {checked:?}",
            fixture.name
        );
    }
}
