//! Retention obligations (W11.11): which live holders keep a stored blob
//! once references give up their availability. A planned withdrawal
//! (`released`) gives the same answer as the committed one; a live archive
//! pin holds the digest until it ends; nothing here removes a blob.

use std::{collections::BTreeSet, fs, path::Path};

use investigation_core::{
    command::RevisionRef,
    identity::{Id, Kind},
    wire::Digest,
};
use investigation_store::{Store, Workspace, store::retention::DigestObligations};
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
    assert_eq!(committed, planned);
    // Nothing removed the bytes: reclamation is a separate step.
    let hex = digest.as_str().strip_prefix("sha256:").unwrap();
    assert!(
        root.join("state/blobs/sha256")
            .join(&hex[..2])
            .join(hex)
            .is_file()
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
}
