//! Journal files must be the exact bytes this program writes for their
//! content (ADR 0016).
//!
//! - Every checked-in genesis and event file in the repository is canonical,
//!   so a change
//!   to the serialization cannot slip in without refusing old journals.
//! - A whitespace, key-order, escape or line-ending change to a genesis, a
//!   middle event or the head event keeps the content but is refused by
//!   `check` and by every load, at that file, as `NON_CANONICAL_JOURNAL_FILE`
//!   with `repair journal` as the repair.
//! - Number reformatting and a byte-order mark stay `CORRUPT_*` refusals.
//! - `repair journal` restores the written bytes exactly when the next file
//!   or the index head records their hash, reports a dry run without
//!   writing, and refuses without a witness, naming the file and writing
//!   nothing.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use investigation_core::{header::Actor, identity::Id, wire::ErrorCode};
use investigation_store::{
    Store, Workspace,
    journal::{canonical_genesis, decode_event, hash_bytes},
    store::journal_repair::{JournalRepair, Witness},
};
use serde_json::{Value, json};

const ACTOR: &str = "agent:canonical-journal";

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn journal_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap();
        if path.is_dir() {
            if !matches!(name, "target" | ".git" | ".maw" | "node_modules") {
                journal_files(&path, found);
            }
        } else if path.to_str().unwrap().contains("/state/cases/")
            && (name == "genesis.json"
                || (path.parent().unwrap().ends_with("events") && name.ends_with(".json")))
        {
            found.push(path);
        }
    }
}

#[test]
fn every_checked_in_journal_file_is_the_bytes_this_program_writes() {
    let mut files = Vec::new();
    for root in ["fixtures", "crates", "training", "examples"] {
        journal_files(&repository().join(root), &mut files);
    }
    let geneses = files
        .iter()
        .filter(|path| path.ends_with("genesis.json"))
        .count();
    assert!(geneses > 5, "found only {geneses} genesis files");
    assert!(
        files.len() - geneses > 200,
        "found only {} event files",
        files.len()
    );
    for path in files {
        let bytes = fs::read(&path).unwrap();
        if path.ends_with("genesis.json") {
            let content: Value = serde_json::from_slice(&bytes).unwrap();
            let id = |key: &str| content[key].as_str().unwrap().parse::<Id>().unwrap();
            let rewritten =
                canonical_genesis(&path, &bytes, id("workspace_id"), id("case_id")).unwrap();
            assert_eq!(rewritten, None, "{}", path.display());
        } else {
            let sequence = path.file_stem().unwrap().to_str().unwrap().parse().unwrap();
            decode_event(&path, sequence, &bytes).unwrap_or_else(|error| panic!("{error}"));
        }
    }
}

/// A workspace with one case of three events, and the case.
fn workspace(root: &Path) -> (Store, Id) {
    let mut store = Store::open(Workspace::init(root, false).unwrap());
    let record = |store: &mut Store, key: &str, case: Option<Id>, operation: Value| {
        let request = json!({
            "schema_version": "inv.command.v1", "command_id": key, "case_id": case,
            "actor": ACTOR, "preconditions": [], "operations": [operation]
        });
        store
            .record(&serde_json::to_vec(&request).unwrap())
            .unwrap()
    };
    let created = record(
        &mut store,
        "create",
        None,
        json!({"op": "case.create", "payload": {"title": "Café latency", "mode": "record"}}),
    );
    let case = created.created[0].id;
    for (key, wording) in [
        ("q1", "Why is p99 18.7 ms?"),
        ("q2", "Does the cache help?"),
    ] {
        record(
            &mut store,
            key,
            Some(case),
            json!({"op": "question.create", "payload": {"wording": wording, "importance": "normal"}}),
        );
    }
    (store, case)
}

fn case_file(root: &Path, case: Id, name: &str) -> PathBuf {
    root.join("state/cases").join(case.to_string()).join(name)
}

/// Overwrite a journal file the store keeps read-only.
fn overwrite(path: &Path, bytes: &[u8]) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(path, bytes).unwrap();
}

fn swap_first_two_keys(bytes: &[u8]) -> Vec<u8> {
    let text = std::str::from_utf8(bytes).unwrap();
    let body = text.strip_prefix('{').unwrap();
    // The first two members are short strings without commas.
    let mut parts = body.splitn(3, ',');
    let (first, second, rest) = (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    );
    format!("{{{second},{first},{rest}").into_bytes()
}

fn escape_first_letter_of(bytes: &[u8], key: &str) -> Vec<u8> {
    let text = std::str::from_utf8(bytes).unwrap();
    let at = text.find(&format!("\"{key}\":\"")).unwrap() + key.len() + 4;
    let letter = text[at..].chars().next().unwrap();
    assert!(letter.is_ascii_alphabetic());
    format!("{}\\u{:04x}{}", &text[..at], letter as u32, &text[at + 1..]).into_bytes()
}

type Mutation = (&'static str, fn(&[u8]) -> Vec<u8>);

/// Changes that keep the parsed content of any journal file.
const KEEPING: [Mutation; 4] = [
    ("whitespace", |bytes| {
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .replacen("\"schema_version\":", "\"schema_version\" :", 1)
            .into_bytes()
    }),
    ("key order", swap_first_two_keys),
    ("unicode escape", |bytes| {
        escape_first_letter_of(bytes, "schema_version")
    }),
    ("trailing newline", |bytes| [bytes, b"\n"].concat()),
];

fn issue_codes(store: &mut Store) -> Vec<String> {
    store
        .check()
        .unwrap()
        .issues
        .into_iter()
        .map(|issue| issue.code)
        .collect()
}

#[test]
fn a_reformatted_journal_file_is_refused_at_that_file_and_repaired_from_its_witness() {
    let actor: Actor = ACTOR.parse().unwrap();
    for (position, name, witness) in [
        ("genesis", "genesis.json", Witness::NextEvent),
        ("middle", "events/000000000002.json", Witness::NextEvent),
        ("head", "events/000000000003.json", Witness::Index),
    ] {
        for (mutation, change) in KEEPING {
            let label = format!("{mutation} @ {position}");
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("workspace");
            let (mut store, case) = workspace(&root);
            let path = case_file(&root, case, name);
            let written = fs::read(&path).unwrap();
            let changed = change(&written);
            assert_ne!(changed, written, "{label}");
            let content = |bytes: &[u8]| serde_json::from_slice::<Value>(bytes).unwrap();
            assert_eq!(content(&changed), content(&written), "{label}");
            overwrite(&path, &changed);

            // Refused by check, a case load and a receipt lookup, at this file.
            let check = store.check().unwrap();
            assert!(!check.integrity_ok(), "{label}");
            let issue = &check.issues[0];
            assert_eq!(issue.code, "NON_CANONICAL_JOURNAL_FILE", "{label}");
            assert!(issue.reason.contains(name), "{label}: {}", issue.reason);
            assert_eq!(
                issue.repair.as_ref().unwrap()[1..3],
                ["repair".to_owned(), "journal".to_owned()]
            );
            let refused = store.case(case).unwrap_err();
            assert!(
                refused.error.message.contains("NON_CANONICAL_JOURNAL_FILE"),
                "{label}"
            );
            assert!(store.receipt(&actor, "q1").is_err(), "{label}");

            // A dry run reports and writes nothing; the repair restores the
            // written bytes, and the journal verifies again.
            let planned = store.repair_journal(true).unwrap();
            assert!(planned.dry_run);
            assert_eq!(planned.files.len(), 1, "{label}");
            assert_eq!(planned.files[0].witness, witness, "{label}");
            assert!(planned.files[0].path.ends_with(name));
            assert_eq!(fs::read(&path).unwrap(), changed, "{label}: dry run wrote");
            let JournalRepair { dry_run, files } = store.repair_journal(false).unwrap();
            assert!(!dry_run);
            assert_eq!(files, planned.files);
            assert_eq!(fs::read(&path).unwrap(), written, "{label}");
            assert!(issue_codes(&mut store).is_empty(), "{label}");
            // Nothing is left to repair.
            assert!(store.repair_journal(false).unwrap().files.is_empty());
        }
    }
}

#[test]
fn reformatted_numbers_and_a_byte_order_mark_stay_corrupt() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let (mut store, case) = workspace(&root);
    let path = case_file(&root, case, "events/000000000003.json");
    let written = fs::read(&path).unwrap();
    let text = String::from_utf8(written.clone()).unwrap();
    for changed in [
        text.replacen("\"sequence\":3", "\"sequence\":3.0", 1),
        text.replacen("\"sequence\":3", "\"sequence\":3e0", 1),
        format!("\u{feff}{text}"),
    ] {
        overwrite(&path, changed.as_bytes());
        assert_eq!(issue_codes(&mut store), ["CORRUPT_EVENT"], "{changed:.40}");
        // Nothing the repair could restore: it refuses with the same error.
        let refused = store.repair_journal(true).unwrap_err();
        assert!(refused.error.message.contains("CORRUPT_EVENT"));
        assert_eq!(fs::read(&path).unwrap(), changed.as_bytes());
    }
    overwrite(&path, &written);
    assert!(issue_codes(&mut store).is_empty());
}

#[test]
fn without_a_witness_the_repair_names_the_file_and_writes_nothing() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let (mut store, case) = workspace(&root);
    let head = case_file(&root, case, "events/000000000003.json");
    let changed = [fs::read(&head).unwrap(), b"\n".to_vec()].concat();
    overwrite(&head, &changed);
    // The index is disposable: without it nothing records the head's hash.
    fs::remove_file(root.join("cache/index.sqlite")).unwrap();
    let refused = store.repair_journal(false).unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::IntegrityFailure);
    assert!(
        refused.error.message.contains("events/000000000003.json"),
        "{}",
        refused.error.message
    );
    assert!(refused.error.message.contains("archive restore"));
    assert_eq!(
        refused.error.actual.unwrap()["unwitnessed"][0]
            .as_str()
            .unwrap(),
        format!("state/cases/{case}/events/000000000003.json")
    );
    assert_eq!(fs::read(&head).unwrap(), changed);
    assert_eq!(issue_codes(&mut store)[0], "NON_CANONICAL_JOURNAL_FILE");
}

#[test]
fn a_reformatted_file_the_next_event_records_was_accepted_before_and_stays_verified() {
    // A program before ADR 0016 accepted a reformatted head and committed
    // the next event onto its hash. Those bytes are recorded, so they verify
    // as they are, and the repair leaves them alone.
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let (mut store, case) = workspace(&root);
    let second = case_file(&root, case, "events/000000000002.json");
    let third = case_file(&root, case, "events/000000000003.json");
    let reformatted = [fs::read(&second).unwrap(), b"\n".to_vec()].concat();
    let written = String::from_utf8(fs::read(&third).unwrap()).unwrap();
    let parent: Value = serde_json::from_str(&written).unwrap();
    let parent = parent["previous_hash"].as_str().unwrap();
    let chained = written.replacen(parent, hash_bytes(&reformatted).as_str(), 1);
    overwrite(&second, &reformatted);
    overwrite(&third, chained.as_bytes());

    // The new head hash leaves the index behind; it rebuilds from the
    // verified journal.
    store.repair_index().unwrap();
    assert_eq!(issue_codes(&mut store), Vec::<String>::new());
    assert_eq!(store.case(case).unwrap().snapshot.sequence, 3);
    assert!(store.repair_journal(false).unwrap().files.is_empty());
    assert_eq!(fs::read(&second).unwrap(), reformatted);

    // Without the record, the same bytes are refused at that file, not at
    // the next one.
    overwrite(&third, written.as_bytes());
    let check = store.check().unwrap();
    assert_eq!(check.issues[0].code, "NON_CANONICAL_JOURNAL_FILE");
    assert!(check.issues[0].reason.contains("000000000002.json"));
}

#[test]
fn an_edited_parent_link_keeps_its_own_diagnostic_in_any_layout() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let (mut store, case) = workspace(&root);
    let head = case_file(&root, case, "events/000000000003.json");
    let mut event: Value = serde_json::from_slice(&fs::read(&head).unwrap()).unwrap();
    event["previous_hash"] = json!(format!("sha256:{}", "a".repeat(64)));
    // Sorted keys: not this program's layout.
    overwrite(&head, &serde_json::to_vec(&event).unwrap());
    assert_eq!(issue_codes(&mut store), ["EVENT_PARENT_HASH_MISMATCH"]);
    // The repair has nothing it may restore and names the file.
    let refused = store.repair_journal(true).unwrap_err();
    assert!(refused.error.message.contains("000000000003.json"));
}
