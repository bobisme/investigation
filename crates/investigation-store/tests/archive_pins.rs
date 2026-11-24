//! Archive pins (W11.09): the recovery head and every blob it needs are
//! held while an archive is written; a reclamation is refused while the pin
//! lives; a commit does not move the pinned head; and a process killed mid
//! archive leaves no permanent pin.

use std::{
    fs,
    io::{BufRead as _, BufReader, Read as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use investigation_core::wire::{Digest, ErrorCode};
use investigation_store::{Store, Workspace};
use serde_json::json;

const HOLDER_ENV: &str = "INVESTIGATION_PIN_HOLDER_WORKSPACE";

/// A workspace with one case that retains one blob.
fn workspace() -> (tempfile::TempDir, PathBuf, Digest) {
    let temporary = tempfile::tempdir().unwrap();
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
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let manifest = json!({
        "schema_version":"inv.ingest.v0-alpha",
        "command_id":"capture-1",
        "actor":"agent:pin-test",
        "new_case":{"title":"Pinned evidence"},
        "source":{"title":"Log","format":"text/plain"},
        "paths":[root.join("source.txt")],
    });
    let ingested = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap();
    let digest = ingested.items[0].digest.clone();
    (temporary, root, digest)
}

fn open(root: &Path) -> Store {
    Store::open(Workspace::open(root).unwrap())
}

fn blob(root: &Path, digest: &Digest) -> PathBuf {
    let hex = digest.as_str().strip_prefix("sha256:").unwrap();
    root.join("state/blobs/sha256").join(&hex[..2]).join(hex)
}

fn commit_new_case(store: &mut Store, key: &str) {
    let request = json!({
        "schema_version":"inv.command.v0-alpha", "command_id": key,
        "actor":"agent:pin-test", "case_id":null, "preconditions":[],
        "operations":[{"op":"case.create","payload":{"title": key}}],
    });
    store
        .record(&serde_json::to_vec(&request).unwrap())
        .unwrap();
}

#[test]
fn a_commit_during_archiving_does_not_move_the_pinned_head() {
    let (_temporary, root, digest) = workspace();
    let mut archiver = open(&root);
    let (state, pin) = archiver.read_canonical().unwrap();
    let heads = state.heads();
    assert!(!heads.is_empty());
    assert_eq!(pin.heads().len(), heads.len());
    assert_eq!(pin.heads()[0].sequence, heads[0].snapshot.sequence);
    assert_eq!(pin.heads()[0].head_hash, heads[0].snapshot.head_hash);
    assert_eq!(pin.blob_count(), state.blobs.len());

    // Ordinary appends continue, to the same case and to a new one.
    let mut writer = open(&root);
    commit_new_case(&mut writer, "during-archive");
    assert_eq!(
        open(&root).read_canonical().unwrap().0.cases.len(),
        heads.len() + 1
    );

    // The archive still covers exactly the pinned heads, and its blob is intact.
    assert_eq!(state.heads(), heads);
    assert_eq!(archiver.archive_pins().unwrap()[0].heads, pin.heads());
    let bytes = archiver.retained_bytes(&digest).unwrap();
    assert_eq!(bytes, b"retained evidence\n");
    archiver.release_pin(pin).unwrap();
}

#[test]
fn a_reclamation_during_archiving_is_refused_and_works_after_release() {
    let (_temporary, root, digest) = workspace();
    let mut archiver = open(&root);
    let (_state, pin) = archiver.read_canonical().unwrap();
    let other: Digest = format!("sha256:{}", "0".repeat(64)).parse().unwrap();

    let mut reclaimer = open(&root);
    let failure = reclaimer.remove_blob_unless_pinned(&digest).unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::GateFailed);
    assert_eq!(
        failure.error.actual.as_ref().unwrap()["pins"],
        json!([pin.id().unwrap()])
    );
    assert!(blob(&root, &digest).is_file());
    assert!(
        reclaimer
            .pinned_digests()
            .unwrap()
            .unwrap()
            .contains(&digest)
    );
    // A blob the archive does not hold is not blocked.
    reclaimer.remove_blob_unless_pinned(&other).unwrap();

    archiver.release_pin(pin).unwrap();
    assert!(archiver.archive_pins().unwrap().is_empty());
    reclaimer.remove_blob_unless_pinned(&digest).unwrap();
    assert!(!blob(&root, &digest).exists());
}

#[test]
fn a_dropped_pin_protects_nothing_and_its_record_expires() {
    let (_temporary, root, digest) = workspace();
    let mut archiver = open(&root);
    let (_state, pin) = archiver.read_canonical().unwrap();
    drop(pin); // an error path that never reached release_pin
    let pins = archiver.archive_pins().unwrap();
    assert_eq!(pins.len(), 1);
    assert!(!pins[0].live);
    assert_eq!(archiver.release_stale_pins().unwrap().len(), 1);
    assert!(archiver.archive_pins().unwrap().is_empty());
    let (_state, pin) = archiver.read_canonical().unwrap();
    drop(pin);
    // A reclamation sweeps the abandoned record and proceeds.
    archiver.remove_blob_unless_pinned(&digest).unwrap();
    assert!(archiver.archive_pins().unwrap().is_empty());
}

/// Runs only as the child of the kill test: pin, announce, wait to be killed.
#[test]
fn pin_holder_child() {
    let Ok(root) = std::env::var(HOLDER_ENV) else {
        return;
    };
    let mut store = open(Path::new(&root));
    let (_state, _pin) = store.read_canonical().unwrap();
    println!("pinned");
    // Block until the parent kills this process.
    let mut sink = Vec::new();
    std::io::stdin().read_to_end(&mut sink).unwrap();
}

#[test]
fn a_killed_archive_leaves_no_permanent_pin_and_the_next_run_succeeds() {
    let (_temporary, root, digest) = workspace();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "pin_holder_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HOLDER_ENV, &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    loop {
        let line = lines.next().expect("the holder exited early").unwrap();
        if line.ends_with("pinned") {
            break;
        }
    }

    let mut store = open(&root);
    let pins = store.archive_pins().unwrap();
    assert_eq!(pins.len(), 1);
    assert!(pins[0].live);
    let failure = store.remove_blob_unless_pinned(&digest).unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::GateFailed);
    // A second archive may run beside it: pins only protect, they never block.
    let (_state, second) = store.read_canonical().unwrap();
    store.release_pin(second).unwrap();

    child.kill().unwrap();
    child.wait().unwrap();

    // The record is left behind and discoverable, but nothing holds it.
    let record = root.join("local/pins").join(format!("{}.json", pins[0].id));
    assert!(record.is_file());
    let pins = store.archive_pins().unwrap();
    assert_eq!(pins.len(), 1);
    assert!(!pins[0].live);
    // The next archive succeeds, expires the stale record, and loses no data.
    let (state, pin) = open(&root).read_canonical().unwrap();
    assert!(state.blobs.iter().any(|required| required.digest == digest));
    // Only the new pin remains; the dead one's record was replaced.
    let remaining = store.archive_pins().unwrap();
    assert_eq!(remaining.len(), 1);
    assert!(remaining[0].live);
    assert!(blob(&root, &digest).is_file());
    store.release_pin(pin).unwrap();
    assert!(store.archive_pins().unwrap().is_empty());
}
