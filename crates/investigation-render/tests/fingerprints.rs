//! Stat fingerprints ([`investigation_render::fingerprint`]): an owned file
//! whose status equals the recorded fingerprint is confirmed without being
//! read, and any edit, including one that restores the size and the
//! modification time, is still read, hashed and diagnosed as drift.
//!
//! A fingerprint is recorded only for a file last changed well before the
//! render. These tests do not wait for that: they record the fingerprints of
//! the files as they stand in the manifest, as a later render would have.

use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use investigation_core::identity::Id;
use investigation_render::{
    Action, DriftKind, Manifest, PlanPath, RenderLock, RenderReport, VaultRenderer, ViewPolicy,
    render_vault,
};
use investigation_store::{Store, Workspace, journal::hash_bytes};
use serde_json::{Value, json};

fn submit(store: &mut Store, key: &str, case_id: Option<Id>, operation: Value) -> Id {
    let input = json!({
        "schema_version":"inv.command.v0-alpha",
        "command_id":key,
        "case_id":case_id,
        "actor":"agent:fingerprint-test",
        "preconditions":[],
        "operations":[operation]
    });
    store
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap()
        .created[0]
        .id
}

fn question(store: &mut Store, key: &str, case_id: Id) -> Id {
    submit(
        store,
        key,
        Some(case_id),
        json!({"op":"question.create", "payload":{"wording":key, "importance":"normal"}}),
    )
}

fn workspace(directory: &Path) -> (Store, Id, Id) {
    let workspace = Workspace::init(&directory.join("workspace"), true).unwrap();
    let mut store = Store::open(workspace);
    let case_id = submit(
        &mut store,
        "case",
        None,
        json!({"op":"case.create", "payload":{"title":"fingerprints", "mode":"record"}}),
    );
    let first = question(&mut store, "q1", case_id);
    question(&mut store, "q2", case_id);
    (store, case_id, first)
}

fn vault(store: &Store) -> PathBuf {
    store.workspace().root().join("vault")
}

fn manifest_file(store: &Store) -> PathBuf {
    store.workspace().root().join("local/render-manifest.json")
}

fn render(store: &mut Store) -> RenderReport {
    render_vault(store, &VaultRenderer, &ViewPolicy::default(), &mut |_| {}).unwrap()
}

fn manifest(store: &Store) -> Manifest {
    let lock = RenderLock::acquire(store.workspace(), Duration::ZERO).unwrap();
    Manifest::load(store.workspace(), &lock).unwrap().manifest
}

fn action(report: &RenderReport, path: &str) -> Action {
    report
        .publish
        .outcomes
        .iter()
        .find(|outcome| outcome.path.as_str() == path)
        .unwrap()
        .action
}

/// The fingerprint of a file in the manifest's `device:inode:size:mtime:ctime`
/// form.
fn fingerprint_of(path: &Path) -> String {
    let meta = fs::symlink_metadata(path).unwrap();
    let nanos = |seconds: i64, nanoseconds: i64| seconds * 1_000_000_000 + nanoseconds;
    format!(
        "{}:{}:{}:{}:{}",
        meta.dev(),
        meta.ino(),
        meta.size(),
        nanos(meta.mtime(), meta.mtime_nsec()),
        nanos(meta.ctime(), meta.ctime_nsec())
    )
}

/// Record every owned file's current fingerprint in the manifest, as a render
/// that found them settled would.
fn record_fingerprints(store: &Store) {
    let path = manifest_file(store);
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for owned in value["owned"].as_array_mut().unwrap() {
        let file = vault(store).join(owned["path"].as_str().unwrap());
        owned["stat"] = Value::String(fingerprint_of(&file));
    }
    let mut bytes = serde_json::to_vec_pretty(&value).unwrap();
    bytes.push(b'\n');
    fs::write(&path, bytes).unwrap();
}

fn note(case_id: Id, id: Id) -> String {
    format!("Cases/{case_id}/Questions/{id}.md")
}

/// Whether permission bits bind this user (they do not for root).
fn permissions_bind(file: &Path) -> bool {
    fs::set_permissions(file, fs::Permissions::from_mode(0o000)).unwrap();
    let binds = fs::read(file).is_err();
    fs::set_permissions(file, fs::Permissions::from_mode(0o600)).unwrap();
    binds
}

#[test]
fn a_file_with_its_recorded_fingerprint_is_confirmed_without_being_read() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id, first) = workspace(directory.path());
    render(&mut store);
    let path = note(case_id, first);
    let file = vault(&store).join(&path);
    if !permissions_bind(&file) {
        return;
    }
    let renders = |store: &mut Store| {
        render_vault(store, &VaultRenderer, &ViewPolicy::default(), &mut |_| {})
    };

    // An unreadable note: without a fingerprint that matches its status, the
    // render reads it, and stops on it.
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(renders(&mut store).is_err(), "an unrecorded file is read");

    // With its status recorded as it stands, the render never opens it: it
    // is confirmed unchanged, and keeps its fingerprint.
    record_fingerprints(&store);
    let report = renders(&mut store).unwrap();
    assert_eq!(action(&report, &path), Action::Unchanged);
    let record = manifest(&store)
        .get(&PlanPath::new(path.clone()).unwrap())
        .cloned()
        .unwrap();
    assert_eq!(
        record.stat.map(|stat| stat.encode()),
        Some(fingerprint_of(&file))
    );

    // Any status change (here the permissions) and the file is read again.
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let report = renders(&mut store).unwrap();
    assert_eq!(action(&report, &path), Action::Unchanged);
    assert_eq!(
        manifest(&store)
            .get(&PlanPath::new(path).unwrap())
            .unwrap()
            .sha256,
        hash_bytes(&fs::read(&file).unwrap())
    );
}

#[test]
fn an_edit_that_keeps_the_size_and_modification_time_is_still_drift() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id, first) = workspace(directory.path());
    render(&mut store);
    record_fingerprints(&store);
    let path = note(case_id, first);
    let file = vault(&store).join(&path);
    let original = fs::read(&file).unwrap();
    let modified = fs::metadata(&file).unwrap().modified().unwrap();

    // Same length, different bytes, and the modification time put back.
    let mut edited = original.clone();
    let last = edited.len() - 2;
    edited[last] = if edited[last] == b'x' { b'y' } else { b'x' };
    fs::write(&file, &edited).unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let meta = fs::metadata(&file).unwrap();
    assert_eq!(meta.len(), original.len() as u64);
    assert_eq!(meta.modified().unwrap(), modified);

    let report = render(&mut store);
    let drift = report
        .drift
        .iter()
        .find(|drift| drift.path.as_str() == path)
        .expect("the edit is diagnosed");
    assert_eq!(drift.kind, DriftKind::Edited);
    assert_eq!(drift.observed, Some(hash_bytes(&edited)));
    assert_eq!(fs::read(&file).unwrap(), edited, "the edit is kept");
    // Drift never records the edited file's fingerprint for the owned bytes.
    let record = manifest(&store)
        .get(&PlanPath::new(path).unwrap())
        .cloned()
        .unwrap();
    assert_eq!(record.sha256, hash_bytes(&original));
    assert!(
        record
            .stat
            .is_none_or(|stat| stat.encode() != fingerprint_of(&file))
    );
}

#[test]
fn a_write_drops_the_fingerprint_its_path_had() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id, _) = workspace(directory.path());
    render(&mut store);
    record_fingerprints(&store);
    let overview = format!("Cases/{case_id}/Overview.md");
    let before = fs::read(vault(&store).join(&overview)).unwrap();

    // A commit changes the overview; it is rewritten and its fingerprint,
    // which named the earlier file, is dropped.
    question(&mut store, "q3", case_id);
    let report = render(&mut store);
    assert_eq!(action(&report, &overview), Action::Written);
    assert_ne!(fs::read(vault(&store).join(&overview)).unwrap(), before);
    let record = manifest(&store)
        .get(&PlanPath::new(overview).unwrap())
        .cloned()
        .unwrap();
    assert!(record.stat.is_none());
    // Files the commit did not change kept theirs.
    assert!(manifest(&store).owned().any(|record| record.stat.is_some()));
}

#[test]
fn a_fingerprint_that_does_not_parse_is_ignored() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, _, _) = workspace(directory.path());
    render(&mut store);
    let path = manifest_file(&store);
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for owned in value["owned"].as_array_mut().unwrap() {
        owned["stat"] = Value::String("not:a:fingerprint".to_owned());
    }
    fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    assert!(manifest(&store).owned().all(|record| record.stat.is_none()));
    let report = render(&mut store);
    assert_eq!(report.publish.refused(), 0);
    assert!(report.drift.is_empty());
}

/// A fingerprint can only confirm a file unchanged; it never lets one be
/// replaced. Forge the worst case: the owned overview is edited, then its
/// fingerprint is recorded as if the edited file held the generated bytes.
/// A commit then changes the overview. The publisher stages the write on the
/// fingerprint's word, but hashes the file again just before the rename,
/// finds the edit, and keeps it.
#[test]
fn a_forged_fingerprint_never_lets_an_edit_be_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id, _) = workspace(directory.path());
    render(&mut store);
    let overview = format!("Cases/{case_id}/Overview.md");
    let file = vault(&store).join(&overview);
    let edited = b"edited by hand\n".to_vec();
    fs::write(&file, &edited).unwrap();
    record_fingerprints(&store);

    question(&mut store, "q3", case_id);
    let report = render(&mut store);
    assert_eq!(
        action(&report, &overview),
        Action::Refused(investigation_render::Refusal::Drifted)
    );
    assert_eq!(fs::read(&file).unwrap(), edited);
    let drift = report
        .drift
        .iter()
        .find(|drift| drift.path.as_str() == overview)
        .expect("the edit is diagnosed");
    assert_eq!(drift.observed, Some(hash_bytes(&edited)));
}
