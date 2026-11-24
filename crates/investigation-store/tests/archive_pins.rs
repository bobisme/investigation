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
use investigation_store::{
    Store, Workspace,
    store::pins::{PinRelease, PinnedHead, is_pin_name},
};
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

/// bn-397s: an export pins the retained blobs it reads like an archive
/// does: reclamation is refused for them while the pin lives, and works
/// once it is released.
#[test]
fn an_export_pin_holds_the_blobs_it_reads_until_released() {
    let (_temporary, root, digest) = workspace();
    let mut exporter = open(&root);
    let (state, archive) = exporter.read_canonical().unwrap();
    exporter.release_pin(archive).unwrap();
    let head = &state.heads()[0];
    let pin = exporter
        .pin_retained(
            PinnedHead {
                case_id: head.case_id,
                sequence: head.snapshot.sequence,
                head_hash: head.snapshot.head_hash.clone(),
            },
            std::slice::from_ref(&digest),
        )
        .unwrap();
    assert_eq!(pin.blob_count(), 1);
    let mut reclaimer = open(&root);
    let failure = reclaimer.remove_blob_unless_pinned(&digest).unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::GateFailed);
    assert!(blob(&root, &digest).is_file());
    let listed = reclaimer.archive_pins().unwrap();
    assert!(
        listed
            .iter()
            .any(|info| info.live && Some(info.id.as_str()) == pin.id()),
        "{listed:?}"
    );

    exporter.release_pin(pin).unwrap();
    assert!(exporter.archive_pins().unwrap().is_empty());
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

/// Every file below `root` with its bytes, for "nothing was written".
fn tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path.clone());
                found.push((path, Vec::new()));
            } else {
                let bytes = fs::read(&path).unwrap();
                found.push((path, bytes));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn pin_names_are_never_reused_and_earlier_numbered_records_are_still_handled() {
    let (_temporary, root, _digest) = workspace();
    let mut store = open(&root);
    let mut seen = Vec::new();
    // Each pin dies without release; the next creation expires its record.
    // Earlier builds then reused archive-0001 for every new pin.
    for _ in 0..3 {
        let (_state, pin) = store.read_canonical().unwrap();
        let id = pin.id().unwrap().to_owned();
        assert!(is_pin_name(&id), "{id}");
        assert_eq!(id.len(), "archive-".len() + 32, "{id}");
        assert!(!seen.contains(&id), "{id} was reused");
        seen.push(id);
        drop(pin);
    }
    let pins = store.archive_pins().unwrap();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].id, seen[2]);
    assert!(!pins[0].live && pins[0].readable);
    assert!(pins[0].recorded_at.is_some());

    // A record an earlier build named by number is listed and released;
    // other files in the directory are never touched.
    let directory = root.join("local/pins");
    fs::write(directory.join("archive-0001.json"), b"{}\n").unwrap();
    fs::write(directory.join("notes.json"), b"{}\n").unwrap();
    let pins = store.archive_pins().unwrap();
    let legacy = pins.iter().find(|pin| pin.id == "archive-0001").unwrap();
    assert!(!legacy.live && !legacy.readable);
    assert_eq!(pins.len(), 2);
    let mut released = store.release_stale_pins().unwrap();
    released.sort();
    assert_eq!(released, ["archive-0001".to_owned(), seen[2].clone()]);
    assert!(directory.join("notes.json").is_file());
    for name in [
        "archive-0001",
        "archive-",
        "archive-ABCDEF",
        "../investigation",
        "notes",
    ] {
        assert_eq!(is_pin_name(name), name == "archive-0001", "{name}");
    }
}

#[test]
fn a_named_release_refuses_a_live_pin_releases_nothing_and_is_retry_safe() {
    let (_temporary, root, digest) = workspace();
    let mut archiver = open(&root);
    let (_state, live) = archiver.read_canonical().unwrap();
    let live_id = live.id().unwrap().to_owned();
    let (_state, dead) = archiver.read_canonical().unwrap();
    let dead_id = dead.id().unwrap().to_owned();
    drop(dead);

    let mut store = open(&root);
    let failure = store
        .release_named_pins(&[dead_id.clone(), live_id.clone()])
        .unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::GateFailed);
    assert_eq!(failure.error.actual.unwrap()["live"], json!([live_id]));
    // All or none: the stale pin named beside it is still there.
    assert_eq!(store.archive_pins().unwrap().len(), 2);
    // --stale never touches the live pin, which still protects the blob.
    assert_eq!(
        store.release_stale_pins().unwrap(),
        std::slice::from_ref(&dead_id)
    );
    let pins = store.archive_pins().unwrap();
    assert_eq!(pins.len(), 1);
    assert!(pins[0].live);
    let refused = store.remove_blob_unless_pinned(&digest).unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::GateFailed);

    // A name whose record is gone is reported, so a repeat succeeds.
    assert_eq!(
        store
            .release_named_pins(std::slice::from_ref(&dead_id))
            .unwrap(),
        [(dead_id, PinRelease::AlreadyAbsent)]
    );
    for bad in ["../investigation.toml", "archive-0001.json", "pin"] {
        let failure = store.release_named_pins(&[bad.to_owned()]).unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::InvalidInput, "{bad}");
    }

    // Once its archive has ended the pin is stale and a named release frees it.
    drop(live);
    assert_eq!(
        store
            .release_named_pins(std::slice::from_ref(&live_id))
            .unwrap(),
        [(live_id, PinRelease::Released)]
    );
    assert!(store.archive_pins().unwrap().is_empty());
}

#[test]
fn opening_and_reading_never_sweep_a_stale_pin() {
    let (_temporary, root, _digest) = workspace();
    let mut store = open(&root);
    let (_state, pin) = store.read_canonical().unwrap();
    let id = pin.id().unwrap().to_owned();
    drop(pin);
    // Settle the disposable caches a first check writes.
    assert!(open(&root).check().unwrap().integrity_ok());
    let before = tree(&root);

    let mut reader = open(&root);
    assert!(reader.check().unwrap().integrity_ok());
    let listed = reader.archive_pins().unwrap();
    let status = reader.archive_pin_status().unwrap();
    drop(reader);

    assert_eq!(tree(&root), before, "a read changed the workspace");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    assert!(!listed[0].live);
    assert_eq!(status[0].holds_back, Some(Vec::new()));
}
