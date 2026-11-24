//! Expiry and reclamation racing a workspace archive (plan §16.5, §20.2;
//! W11.19). Two independent acquisitions in cases A and B hold identical
//! bytes. A is expired while an archive of the workspace holds its pin:
//!
//! - A's reference refuses reads, also through the revision an old head
//!   cites; B keeps reading the same bytes;
//! - reclamation waits for the last obligation: B's reference, then the
//!   live pin, and only then removes the bytes;
//! - the pinned archive verifies and restores with the bytes B needs;
//! - an archive taken after the reclamation omits the withdrawn bytes,
//!   verifies, restores, passes `check` and reindexes.

use std::{collections::BTreeSet, fs, path::Path};

use investigation_core::{
    evidence::{ArtifactBody, SelectorRequest},
    identity::{Id, Kind},
    input,
    wire::{Digest, ErrorCode},
};
use investigation_render::{
    export::{
        archive::{ARCHIVE_AUDIENCE, plan_archive},
        kind,
        source::read_archive_source,
        verify::{verify_export, verify_into},
        write_export,
    },
    policy::Audience,
};
use investigation_store::{
    Store, Workspace,
    store::{evidence_read::Excerpt, retention::ReclaimOutcome},
};
use serde_json::json;

const ACTOR: &str = "agent:reclaim-test";
const BYTES: &[u8] = b"token=abc123 captured twice\n";

fn open(root: &Path) -> Store {
    Store::open(Workspace::open(root).unwrap())
}

fn workspace(temporary: &tempfile::TempDir) -> std::path::PathBuf {
    let root = temporary.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        config,
        contents.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    root
}

/// A new case with its own acquisition of `BYTES`: (case, digest).
fn acquire(store: &mut Store, key: &str) -> (Id, Digest) {
    let manifest = json!({
        "schema_version":"inv.ingest.v1","command_id":key,"actor":ACTOR,
        "new_case":{"title":"Which token leaked?"},
        "source":{"title":"Capture","format":"text/plain",
            "acquisition_key":{"namespace":"example-capture","external_id":key}},
        "content":{"kind":"stdin","name":"capture.txt"},
    });
    let mut input = BYTES;
    let receipt = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), Some(&mut input))
        .unwrap();
    let case = receipt
        .receipt
        .created
        .iter()
        .find(|change| change.id.kind() == Kind::Case)
        .unwrap()
        .id;
    (case, receipt.items[0].digest.clone())
}

fn holders(
    store: &mut Store,
    case: Id,
    digest: &Digest,
) -> Vec<investigation_core::command::RevisionRef> {
    store
        .case(case)
        .unwrap()
        .state
        .records_of_kind(Kind::Artifact)
        .filter(|record| record.body["digest"] == json!(digest))
        .map(|record| record.reference())
        .collect()
}

fn expire(store: &mut Store, case: Id, digest: &Digest) {
    let operations: Vec<_> = holders(store, case, digest)
        .into_iter()
        .map(|artifact| {
            json!({"op":"artifact.expire","payload":{
                "artifact_ref": artifact, "reason":"sensitive_content",
                "rationale":"The capture holds a live token."
            }})
        })
        .collect();
    let request = json!({
        "schema_version":"inv.command.v1","command_id":format!("expire-{case}"),
        "case_id":case,"actor":ACTOR,"preconditions":[],"operations":operations,
    });
    store
        .record(&serde_json::to_vec(&request).unwrap())
        .unwrap();
}

/// Read the whole of `revision` of the Artifact `id` through the current
/// replay of `case`; the error code when refused.
fn read(store: &mut Store, case: Id, id: Id, revision: u64) -> Result<Vec<u8>, ErrorCode> {
    let replay = store.case(case).unwrap();
    let record = replay
        .state
        .revision(investigation_core::command::RevisionRef { id, revision })
        .unwrap();
    let body: ArtifactBody = input::decode(&record.body).unwrap();
    let selector: SelectorRequest = input::decode(&json!({
        "schema_version":"inv.selector.v1","artifact_digest":body.digest,
        "selector":{"type":"whole"}
    }))
    .unwrap();
    store
        .evidence_read(&replay, record, selector)
        .map(|read| match read.excerpt {
            Excerpt::Text { text, .. } => text.into_bytes(),
            other => panic!("not text: {other:?}"),
        })
        .map_err(|failure| failure.error.code)
}

fn blob(root: &Path, digest: &Digest) -> std::path::PathBuf {
    let hex = digest.as_str().strip_prefix("sha256:").unwrap();
    root.join("state/blobs/sha256").join(&hex[..2]).join(hex)
}

fn reclaim(store: &mut Store, digest: &Digest) -> ReclaimOutcome {
    store
        .reclaim_blobs(&BTreeSet::from([digest.clone()]))
        .unwrap()[0]
        .outcome
}

/// Write an archive from a source read earlier, then verify and restore it.
fn archive_to(
    source: &investigation_render::export::archive::ArchiveSource,
    out: &Path,
    restored: &Path,
) -> investigation_render::export::verify::VerifiedCopy {
    let plan = plan_archive(kind("workspace-archive").unwrap(), source, ARCHIVE_AUDIENCE).unwrap();
    write_export(&plan, out).unwrap();
    let scratch = out.with_extension("scratch");
    fs::create_dir(&scratch).unwrap();
    let report = verify_export(out, &scratch).unwrap();
    assert!(report.ok(), "{:?}", report.problems);
    verify_into(out, restored).unwrap().unwrap()
}

#[test]
fn expiry_during_an_archive_keeps_the_survivor_and_reclamation_waits_for_the_last_obligation() {
    assert_eq!(ARCHIVE_AUDIENCE, Audience::Restricted);
    let temporary = tempfile::tempdir().unwrap();
    let root = workspace(&temporary);
    let mut store = open(&root);
    let (case_a, digest) = acquire(&mut store, "capture-a");
    let (case_b, same) = acquire(&mut store, "capture-b");
    assert_eq!(digest, same, "identical bytes are stored once");
    let old_a = holders(&mut store, case_a, &digest)[0];
    let b = holders(&mut store, case_b, &digest)[0];

    // An archive of the workspace pins its heads and blobs ...
    let mut archiver = open(&root);
    let (pinned, pin) = read_archive_source(&mut archiver, Vec::new()).unwrap();
    // ... while A's reference is withdrawn.
    expire(&mut store, case_a, &digest);
    assert_eq!(
        read(&mut store, case_a, old_a.id, old_a.revision),
        Err(ErrorCode::GateFailed),
        "the revision an old head cites follows today's availability"
    );
    assert_eq!(read(&mut store, case_b, b.id, b.revision).unwrap(), BYTES);
    assert_eq!(reclaim(&mut store, &digest), ReclaimOutcome::HeldByRecords);

    // B's reference goes too; the live pin still holds the bytes.
    expire(&mut store, case_b, &digest);
    let waiting = store
        .reclaim_blobs(&BTreeSet::from([digest.clone()]))
        .unwrap();
    assert_eq!(waiting[0].outcome, ReclaimOutcome::Waiting);
    assert_eq!(
        waiting[0].obligations.archive_pins,
        [pin.id().unwrap().to_owned()]
    );
    assert!(blob(&root, &digest).is_file());

    // The pinned archive is written from its own heads, verifies and
    // restores with the bytes both references could read then.
    let first = archive_to(
        &pinned,
        &temporary.path().join("pinned"),
        &temporary.path().join("restored-pinned"),
    );
    assert!(first.index.blobs.iter().any(|blob| blob.sha256 == digest));
    archiver.release_pin(pin).unwrap();
    let mut restored = open(&temporary.path().join("restored-pinned"));
    assert!(restored.check().unwrap().integrity_ok());
    assert_eq!(
        read(&mut restored, case_b, b.id, b.revision).unwrap(),
        BYTES
    );

    // With the pin released the last obligation is gone.
    assert_eq!(reclaim(&mut store, &digest), ReclaimOutcome::Reclaimed);
    assert!(!blob(&root, &digest).exists());
    assert_eq!(reclaim(&mut store, &digest), ReclaimOutcome::AlreadyAbsent);
    let report = store.check().unwrap();
    assert!(report.integrity_ok(), "{:?}", report.issues);
    assert!(report.reclaimed_blobs >= 1);
    // Provenance stays: the records keep digest and size at every revision.
    for case in [case_a, case_b] {
        for reference in holders(&mut store, case, &digest) {
            assert_eq!(reference.revision, 2);
            assert_eq!(
                read(&mut store, case, reference.id, 1),
                Err(ErrorCode::GateFailed)
            );
        }
    }

    // A new archive omits the withdrawn bytes and still restores whole.
    let mut later = open(&root);
    let (source, pin) = read_archive_source(&mut later, Vec::new()).unwrap();
    let second = archive_to(
        &source,
        &temporary.path().join("later"),
        &temporary.path().join("restored-later"),
    );
    later.release_pin(pin).unwrap();
    assert!(second.index.blobs.iter().all(|blob| blob.sha256 != digest));
    assert!(
        second
            .index
            .not_retained
            .iter()
            .any(|entry| entry.sha256.as_ref() == Some(&digest))
    );
    let restored_root = temporary.path().join("restored-later");
    let mut restored = open(&restored_root);
    let report = restored.check().unwrap();
    assert!(report.integrity_ok(), "{:?}", report.issues);
    assert!(report.reclaimed_blobs >= 1);
    assert!(!blob(&restored_root, &digest).exists());
    assert_eq!(
        read(&mut restored, case_b, b.id, b.revision),
        Err(ErrorCode::GateFailed)
    );
    restored.repair_index().unwrap();
}

#[test]
fn a_reimport_of_a_withdrawn_acquisition_is_refused_and_a_new_acquisition_keeps_its_own_policy() {
    let temporary = tempfile::tempdir().unwrap();
    let root = workspace(&temporary);
    let mut store = open(&root);
    let (case, digest) = acquire(&mut store, "capture-a");
    expire(&mut store, case, &digest);
    assert_eq!(reclaim(&mut store, &digest), ReclaimOutcome::Reclaimed);

    let same = |key: &str, external: &str| {
        json!({
            "schema_version":"inv.ingest.v1","command_id":key,"actor":ACTOR,"case_id":case,
            "source":{"title":"Capture","format":"text/plain",
                "acquisition_key":{"namespace":"example-capture","external_id":external}},
            "content":{"kind":"stdin","name":"capture.txt"},
        })
    };
    let mut input = BYTES;
    let refused = store
        .ingest(
            &serde_json::to_vec(&same("again", "capture-a")).unwrap(),
            Some(&mut input),
        )
        .unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::GateFailed);
    assert_eq!(refused.error.path.as_deref(), Some("/acquisition"));
    assert!(
        !blob(&root, &digest).exists(),
        "a refused reimport stores nothing"
    );

    // Another acquisition of the same bytes is a new reference with its own
    // availability: it retains the bytes again.
    let mut input = BYTES;
    let receipt = store
        .ingest(
            &serde_json::to_vec(&same("new", "capture-a-reacquired")).unwrap(),
            Some(&mut input),
        )
        .unwrap();
    assert!(blob(&root, &digest).is_file());
    let artifact = receipt.items[0].artifact_id;
    let current = holders(&mut store, case, &digest);
    assert!(current.iter().any(|reference| reference.id == artifact));
    assert_eq!(read(&mut store, case, artifact, 1).unwrap(), BYTES);
    assert_eq!(reclaim(&mut store, &digest), ReclaimOutcome::HeldByRecords);
    assert!(store.check().unwrap().integrity_ok());
}
