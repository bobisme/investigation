//! Retention obligations (W11.11) and reclamation (W11.19): which live
//! holders keep a stored blob once references give up their availability.
//! A planned withdrawal (`released`) gives the same answer as the committed
//! one; a live archive pin holds the digest until it ends; only reclamation
//! removes a blob, and `check` then accepts it as reclaimed.

use std::{collections::BTreeSet, fs, path::Path};

use investigation_core::{
    command::RevisionRef,
    identity::{Id, Kind},
    wire::Digest,
};
use investigation_store::{
    Store, Workspace,
    store::retention::{DigestObligations, ReclaimOutcome},
};
use serde_json::json;

const ACTOR: &str = "agent:retention-test";

fn open(root: &Path) -> Store {
    Store::open(Workspace::open(root).unwrap())
}

/// A workspace with one case retaining one file: (root, case, digest).
fn workspace(temporary: &tempfile::TempDir) -> (std::path::PathBuf, Id, Digest) {
    let root = temporary.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        config,
        contents.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    fs::write(root.join("source.txt"), b"retained evidence\n").unwrap();
    let manifest = json!({
        "schema_version":"inv.ingest.v1", "command_id":"capture-1", "actor":ACTOR,
        "new_case":{"title":"Retained evidence"},
        "source":{"title":"Log","format":"text/plain"},
        "paths":[root.join("source.txt")],
    });
    let ingested = open(&root)
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap();
    let case = ingested
        .receipt
        .created
        .iter()
        .find(|change| change.id.kind() == Kind::Case)
        .unwrap()
        .id;
    (root, case, ingested.items[0].digest.clone())
}

/// Every current Artifact of `case` recording `digest`.
fn references(store: &mut Store, case: Id, digest: &Digest) -> Vec<RevisionRef> {
    let state = store.case(case).unwrap().state;
    state
        .records_of_kind(Kind::Artifact)
        .filter(|record| record.body["digest"] == json!(digest))
        .map(|record| record.reference())
        .collect()
}

fn obligations(store: &mut Store, digest: &Digest, released: &[Id]) -> DigestObligations {
    let digests = BTreeSet::from([digest.clone()]);
    let released: BTreeSet<Id> = released.iter().copied().collect();
    let mut found = store.retention_obligations(&digests, &released).unwrap();
    assert_eq!(found.len(), 1);
    found.remove(0)
}

#[test]
fn a_planned_withdrawal_and_the_committed_one_leave_the_same_obligations() {
    let temporary = tempfile::tempdir().unwrap();
    let (root, case, digest) = workspace(&temporary);
    let mut store = open(&root);
    let holders = references(&mut store, case, &digest);
    assert!(!holders.is_empty());

    let now = obligations(&mut store, &digest, &[]);
    assert!(now.stored);
    assert_eq!(now.references.len(), holders.len());
    assert!(now.held_by_records() && !now.reclaimable());
    assert_eq!(now.size_bytes, Some(18));

    // Releasing only some references leaves the rest holding the bytes.
    let partial = obligations(&mut store, &digest, &[holders[0].id]);
    assert_eq!(partial.reclaimable(), holders.len() == 1);

    let ids: Vec<Id> = holders.iter().map(|holder| holder.id).collect();
    let planned = obligations(&mut store, &digest, &ids);
    assert!(planned.reclaimable(), "{planned:?}");

    let operations: Vec<_> = holders
        .iter()
        .map(|holder| {
            json!({"op":"artifact.expire","payload":{
                "artifact_ref": holder, "reason":"retention_period_ended",
                "rationale":"The case keeps raw captures for 90 days."
            }})
        })
        .collect();
    let request = json!({
        "schema_version":"inv.command.v1", "command_id":"expire-all", "actor":ACTOR,
        "case_id":case, "preconditions":[], "operations":operations,
    });
    store
        .record(&serde_json::to_vec(&request).unwrap())
        .unwrap();
    let committed = obligations(&mut store, &digest, &[]);
    // The same references, now at their withdrawn revisions.
    let withdrawn = |found: &DigestObligations| -> Vec<Id> {
        found
            .withdrawn
            .iter()
            .map(|holder| holder.artifact_ref.id)
            .collect()
    };
    assert_eq!(withdrawn(&committed), withdrawn(&planned));
    assert!(
        committed
            .withdrawn
            .iter()
            .all(|holder| holder.artifact_ref.revision == 2)
    );
    let (mut now, mut then) = (committed.clone(), planned.clone());
    now.withdrawn.clear();
    then.withdrawn.clear();
    assert_eq!(now, then);
    // Nothing removed the bytes: reclamation is a separate step.
    let hex = digest.as_str().strip_prefix("sha256:").unwrap();
    let blob = root.join("state/blobs/sha256").join(&hex[..2]).join(hex);
    assert!(blob.is_file());
    assert_eq!(store.check().unwrap().reclaimed_blobs, 0);

    let digests = BTreeSet::from([digest.clone()]);
    let done = store.reclaim_blobs(&digests).unwrap();
    assert_eq!(done[0].outcome, ReclaimOutcome::Reclaimed);
    assert!(!blob.exists());
    let again = store.reclaim_blobs(&digests).unwrap();
    assert_eq!(again[0].outcome, ReclaimOutcome::AlreadyAbsent);
    let report = store.check().unwrap();
    assert!(report.integrity_ok(), "{:?}", report.issues);
    assert_eq!(report.reclaimed_blobs, 1);
    // The record keeps its digest and size.
    assert!(
        references(&mut store, case, &digest)
            .iter()
            .all(|reference| reference.revision == 2)
    );
}

#[test]
fn a_live_archive_pin_holds_an_otherwise_reclaimable_digest_until_it_ends() {
    let temporary = tempfile::tempdir().unwrap();
    let (root, case, digest) = workspace(&temporary);
    let mut store = open(&root);
    let ids: Vec<Id> = references(&mut store, case, &digest)
        .iter()
        .map(|holder| holder.id)
        .collect();
    assert!(obligations(&mut store, &digest, &ids).reclaimable());

    let mut archiver = open(&root);
    let (_state, pin) = archiver.read_canonical().unwrap();
    let pinned = obligations(&mut store, &digest, &ids);
    assert!(!pinned.held_by_records());
    assert_eq!(pinned.archive_pins, [pin.id().unwrap().to_owned()]);
    assert!(!pinned.reclaimable());

    archiver.release_pin(pin).unwrap();
    assert!(obligations(&mut store, &digest, &ids).reclaimable());
    // Nothing was withdrawn yet: reclamation refuses the bytes.
    let digests = BTreeSet::from([digest.clone()]);
    let refused = store.reclaim_blobs(&digests).unwrap();
    assert_eq!(refused[0].outcome, ReclaimOutcome::NotWithdrawn);
}

#[test]
fn reclamation_is_refused_while_the_format_on_disk_forbids_writes() {
    let temporary = tempfile::tempdir().unwrap();
    let (root, case, digest) = workspace(&temporary);
    let mut store = open(&root);
    let operations: Vec<_> = references(&mut store, case, &digest)
        .iter()
        .map(|holder| {
            json!({"op":"artifact.expire","payload":{
                "artifact_ref": holder, "reason":"owner_request",
                "rationale":"The owner asked for the capture to be dropped."
            }})
        })
        .collect();
    let request = json!({
        "schema_version":"inv.command.v1", "command_id":"expire-all", "actor":ACTOR,
        "case_id":case, "preconditions":[], "operations":operations,
    });
    store
        .record(&serde_json::to_vec(&request).unwrap())
        .unwrap();
    // A newer program, or a migration in progress, changes the marker after
    // this store was opened: the removal reads it under the lock.
    let marker = root.join("state/workspace.json");
    let original = fs::read(&marker).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    value["format"] = json!({"version": 2, "read_requires": 1, "write_requires": 2});
    fs::write(&marker, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let digests = BTreeSet::from([digest.clone()]);
    let refused = store.reclaim_blobs(&digests).unwrap_err();
    assert_eq!(
        refused.error.code,
        investigation_core::wire::ErrorCode::InvalidInput
    );
    let hex = digest.as_str().strip_prefix("sha256:").unwrap();
    assert!(
        root.join("state/blobs/sha256")
            .join(&hex[..2])
            .join(hex)
            .is_file()
    );
    fs::write(&marker, original).unwrap();
    let done = store.reclaim_blobs(&digests).unwrap();
    assert_eq!(done[0].outcome, ReclaimOutcome::Reclaimed);
}

#[test]
fn pin_status_names_what_a_live_pin_holds_back_and_a_stale_pin_holds_nothing() {
    let temporary = tempfile::tempdir().unwrap();
    let (root, case, digest) = workspace(&temporary);
    // An archive is running, and another one stopped without releasing:
    // both records name the digest; only the live one holds it.
    let mut archiver = open(&root);
    let (_state, live) = archiver.read_canonical().unwrap();
    let live_id = live.id().unwrap().to_owned();
    let (_state, dead) = archiver.read_canonical().unwrap();
    let dead_id = dead.id().unwrap().to_owned();
    drop(dead);

    let mut store = open(&root);
    // Nothing is withdrawn yet: records hold the bytes, not the pins.
    let status = store.archive_pin_status().unwrap();
    assert!(
        status
            .iter()
            .all(|entry| entry.holds_back == Some(Vec::new()))
    );

    let operations: Vec<_> = references(&mut store, case, &digest)
        .iter()
        .map(|holder| {
            json!({"op":"artifact.expire","payload":{
                "artifact_ref": holder, "reason":"owner_request",
                "rationale":"The owner asked for this capture to be withdrawn."
            }})
        })
        .collect();
    let request = json!({
        "schema_version":"inv.command.v1", "command_id":"expire-pinned", "actor":ACTOR,
        "case_id":case, "preconditions":[], "operations":operations,
    });
    store
        .record(&serde_json::to_vec(&request).unwrap())
        .unwrap();

    let status = store.archive_pin_status().unwrap();
    let find = |id: &str| status.iter().find(|entry| entry.pin.id == id).unwrap();
    let held = find(&live_id).holds_back.clone().unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].digest, digest);
    assert_eq!(held[0].size_bytes, Some(18));
    assert!(find(&live_id).pin.live);
    let stale = find(&dead_id);
    assert!(!stale.pin.live, "{stale:?}");
    assert_eq!(stale.pin.blobs, find(&live_id).pin.blobs, "{stale:?}");
    assert_eq!(stale.holds_back, Some(Vec::new()));

    // What the live pin holds back is exactly what reclamation waits on.
    let digests = BTreeSet::from([digest.clone()]);
    let waiting = store.reclaim_blobs(&digests).unwrap();
    assert_eq!(waiting[0].outcome, ReclaimOutcome::Waiting);
    archiver.release_pin(live).unwrap();
    assert_eq!(store.release_stale_pins().unwrap(), [dead_id]);
    let done = store.reclaim_blobs(&digests).unwrap();
    assert_eq!(done[0].outcome, ReclaimOutcome::Reclaimed);
}

/// Every file below `root` with its bytes.
fn files(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                let bytes = fs::read(&path).unwrap();
                out.insert(path, bytes);
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, &mut out);
    out
}

fn expire_all(store: &mut Store, case: Id, digest: &Digest, key: &str) {
    let operations: Vec<_> = references(store, case, digest)
        .iter()
        .map(|holder| {
            json!({"op":"artifact.expire","payload":{
                "artifact_ref": holder, "reason":"retention_period_ended",
                "rationale":"The case keeps raw captures for 90 days."
            }})
        })
        .collect();
    let request = json!({
        "schema_version":"inv.command.v1", "command_id":key, "actor":ACTOR,
        "case_id":case, "preconditions":[], "operations":operations,
    });
    store
        .record(&serde_json::to_vec(&request).unwrap())
        .unwrap();
}

fn ingest_new_case(root: &Path, key: &str, file: &str) -> (Id, Digest) {
    let manifest = json!({
        "schema_version":"inv.ingest.v1", "command_id":key, "actor":ACTOR,
        "new_case":{"title":"Another case"},
        "source":{"title":"Log","format":"text/plain"},
        "paths":[root.join(file)],
    });
    let ingested = open(root)
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap();
    let case = ingested
        .receipt
        .created
        .iter()
        .find(|change| change.id.kind() == Kind::Case)
        .unwrap()
        .id;
    (case, ingested.items[0].digest.clone())
}

fn outcomes(
    done: &[investigation_store::store::retention::Reclamation],
) -> Vec<(Digest, ReclaimOutcome)> {
    done.iter()
        .map(|done| (done.obligations.digest.clone(), done.outcome))
        .collect()
}

#[test]
fn a_workspace_sweep_rechecks_every_withdrawn_digest_and_removes_only_when_applied() {
    let temporary = tempfile::tempdir().unwrap();
    let (root, case, digest) = workspace(&temporary);
    fs::write(root.join("kept.txt"), b"kept, never withdrawn\n").unwrap();
    // The same bytes in a second case, and other bytes in a third.
    let (other, same) = ingest_new_case(&root, "capture-2", "source.txt");
    assert_eq!(same, digest);
    let (_, kept) = ingest_new_case(&root, "capture-3", "kept.txt");
    let hex = digest.as_str().strip_prefix("sha256:").unwrap();
    let blob = root.join("state/blobs/sha256").join(&hex[..2]).join(hex);
    let mut store = open(&root);

    // Nothing withdrawn: the sweep considers nothing.
    assert!(store.reclaim_withdrawn(true).unwrap().is_empty());

    // A reference in another case still holds the bytes.
    expire_all(&mut store, case, &digest, "expire-1");
    let held = store.reclaim_withdrawn(true).unwrap();
    assert_eq!(
        outcomes(&held),
        [(digest.clone(), ReclaimOutcome::HeldByRecords)]
    );
    assert!(blob.is_file());

    // An archive pin taken while the other case still retained the bytes
    // holds them, in a dry run and when applied, until it ends.
    let mut archiver = open(&root);
    let (_state, pin) = archiver.read_canonical().unwrap();
    expire_all(&mut store, other, &digest, "expire-2");
    for apply in [false, true] {
        let waiting = store.reclaim_withdrawn(apply).unwrap();
        assert_eq!(
            outcomes(&waiting),
            [(digest.clone(), ReclaimOutcome::Waiting)]
        );
        assert!(blob.is_file());
    }
    archiver.release_pin(pin).unwrap();

    // Withdrawn everywhere and unpinned: the dry run decides and writes
    // nothing.
    let before = files(&root);
    let planned = store.reclaim_withdrawn(false).unwrap();
    assert_eq!(
        outcomes(&planned),
        [(digest.clone(), ReclaimOutcome::WouldReclaim)]
    );
    assert_eq!(files(&root), before, "the dry run wrote");

    // A format that forbids writes refuses the removal, not the dry run.
    let marker = root.join("state/workspace.json");
    let original = fs::read(&marker).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    value["format"] = json!({"version": 2, "read_requires": 1, "write_requires": 2});
    fs::write(&marker, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    assert!(store.reclaim_withdrawn(true).is_err());
    assert!(blob.is_file());
    let planned = store.reclaim_withdrawn(false).unwrap();
    assert_eq!(planned[0].outcome, ReclaimOutcome::WouldReclaim);
    fs::write(&marker, original).unwrap();

    let done = store.reclaim_withdrawn(true).unwrap();
    assert_eq!(
        outcomes(&done),
        [(digest.clone(), ReclaimOutcome::Reclaimed)]
    );
    assert!(!blob.exists());
    let kept_hex = kept.as_str().strip_prefix("sha256:").unwrap();
    assert!(
        root.join("state/blobs/sha256")
            .join(&kept_hex[..2])
            .join(kept_hex)
            .is_file()
    );
    let again = store.reclaim_withdrawn(true).unwrap();
    assert_eq!(
        outcomes(&again),
        [(digest.clone(), ReclaimOutcome::AlreadyAbsent)]
    );
    let report = store.check().unwrap();
    assert!(report.integrity_ok(), "{:?}", report.issues);
    assert_eq!(report.reclaimed_blobs, 1);
}
