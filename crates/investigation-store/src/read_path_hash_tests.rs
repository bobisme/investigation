//! Read paths and retained bytes (MB.06, §20.1 Y15, §10.5).
//!
//! `show` and the other read paths ask [`Store::unavailable_artifacts`]
//! whether the bytes a record cites are intact. These tests run it on the
//! read-counting filesystem of the streaming ingest tests, with a new store
//! for each read as a new process would have, and check that:
//!
//! - a blob already hashed is not read again while its file is unchanged;
//! - an overwrite in place, with size and modification time kept, is caught
//!   by the next read, which hashes again;
//! - a restricted blob is never read: its availability comes from a recorded
//!   verdict for its current file or from its presence and size, and `check`
//!   (which hashes under the commit lock, so on the plain local filesystem)
//!   still finds a same-size forgery and the next read reports it.
use std::{
    cell::Cell,
    fs,
    io::{Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    rc::Rc,
};

use investigation_core::{
    catalog::standard_catalog,
    command::RevisionRef,
    identity::{Id, Kind},
};
use serde_json::json;

use crate::{
    durable::WriteGate,
    store::{Store, ingest::streaming_tests::Probing},
    workspace::Workspace,
};

const MIB: u64 = 1024 * 1024;
const ACTOR: &str = "agent:read-path-test";

struct Fixture {
    _temporary: tempfile::TempDir,
    workspace: Workspace,
    reads: Rc<Cell<usize>>,
    case: Id,
    artifact: Id,
    blob: PathBuf,
    size: u64,
}

impl Fixture {
    /// One case holding one retained Artifact of `size` mostly-zero bytes.
    fn new(size: u64) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        Workspace::init(&root, false).unwrap();
        let config = root.join("investigation.toml");
        let contents = fs::read_to_string(&config).unwrap();
        fs::write(
            &config,
            contents.replace("import_roots = []", "import_roots = [\".\"]"),
        )
        .unwrap();
        let mut file = fs::File::create(root.join("capture.bin")).unwrap();
        file.set_len(size).unwrap();
        file.write_all(b"capture").unwrap();
        drop(file);
        let workspace = Workspace::open(&root).unwrap();
        let reads = Rc::new(Cell::new(0));
        let manifest = json!({
            "schema_version": "inv.ingest.v1", "command_id": "ingest", "actor": ACTOR,
            "new_case": {"title": "Large capture"}, "source": {"title": "Generated capture"},
            "paths": ["capture.bin"],
        });
        let receipt = probed(&workspace, &reads)
            .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
            .unwrap();
        let case = receipt
            .receipt
            .created
            .iter()
            .find(|change| change.id.kind() == Kind::Case)
            .unwrap()
            .id;
        let item = &receipt.items[0];
        let digest = item.digest.to_string();
        let hex = digest.trim_start_matches("sha256:");
        Self {
            blob: root.join("state/blobs/sha256").join(&hex[..2]).join(hex),
            artifact: item.artifact_id,
            _temporary: temporary,
            workspace,
            reads,
            case,
            size,
        }
    }

    /// A store as a new process opens it, counting blob reads.
    fn store(&self) -> Store<Probing> {
        probed(&self.workspace, &self.reads)
    }

    /// Ask, as `show` does, whether the Artifact's bytes are unavailable.
    /// Returns that and how many blob reads it took.
    fn read(&self) -> (bool, usize) {
        let mut store = self.store();
        let replay = store.case(self.case).unwrap();
        let current: RevisionRef = replay.state.current(self.artifact).unwrap().reference();
        let before = self.reads.get();
        let unavailable = !store
            .unavailable_artifacts(&replay.state, &replay.state, &[current])
            .is_empty();
        (unavailable, self.reads.get() - before)
    }

    /// `check` hashes under the commit lock, which the probing filesystem
    /// refuses, so it runs on the plain local one.
    fn check(&self) -> crate::store::CheckReport {
        Store::open(self.workspace.clone()).check().unwrap()
    }

    fn forget_verdicts(&self) {
        let _ = fs::remove_dir_all(self.workspace.root().join("cache/blob-verdicts"));
    }

    fn restrict(&self) {
        let mut store = self.store();
        let replay = store.case(self.case).unwrap();
        let current = replay.state.current(self.artifact).unwrap().reference();
        let command = json!({
            "schema_version": "inv.command.v1", "command_id": "raise", "actor": ACTOR,
            "case_id": self.case, "preconditions": [],
            "operations": [{"op": "classification.raise", "payload": {
                "subject_ref": current, "level": "restricted",
                "reason": "Holds unreleased figures.", "tags": []}}],
        });
        store
            .record(&serde_json::to_vec(&command).unwrap())
            .unwrap();
    }

    /// Change one byte of the blob in place, keeping its inode, size and
    /// modification time: only the change time tells.
    fn overwrite_in_place(&self) {
        overwrite_in_place(&self.blob);
    }
}

fn probed(workspace: &Workspace, reads: &Rc<Cell<usize>>) -> Store<Probing> {
    Store {
        workspace: workspace.clone(),
        fs: Probing::new(workspace.root(), reads.clone()),
        gate: WriteGate::isolated(),
        catalog: standard_catalog(),
    }
}

fn overwrite_in_place(blob: &Path) {
    let metadata = fs::metadata(blob).unwrap();
    let modified = metadata.modified().unwrap();
    let mut permissions = metadata.permissions();
    let mode = permissions.mode();
    permissions.set_mode(0o600);
    fs::set_permissions(blob, permissions.clone()).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(blob).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"forged!").unwrap();
    file.set_modified(modified).unwrap();
    drop(file);
    permissions.set_mode(mode);
    fs::set_permissions(blob, permissions).unwrap();
    assert_eq!(fs::metadata(blob).unwrap().modified().unwrap(), modified);
}

#[test]
fn a_second_read_of_a_large_artifact_does_not_read_its_bytes() {
    let fixture = Fixture::new(16 * MIB);
    // Ingest hashed the bytes as it wrote the blob, and recorded that.
    assert_eq!(fixture.read(), (false, 0));
    // Without the disposable record the first read hashes once, and the
    // next read reuses it.
    fixture.forget_verdicts();
    let (unavailable, reads) = fixture.read();
    assert!(!unavailable);
    assert!(reads as u64 >= fixture.size / (64 * 1024), "{reads} reads");
    assert_eq!(fixture.read(), (false, 0));
    assert_eq!(fixture.read(), (false, 0));
    // A garbled record is a miss, not a verdict.
    for entry in fs::read_dir(fixture.workspace.root().join("cache/blob-verdicts")).unwrap() {
        fs::write(entry.unwrap().path(), b"intact").unwrap();
    }
    let (unavailable, reads) = fixture.read();
    assert!(!unavailable && reads > 0);
    assert_eq!(fixture.read(), (false, 0));
}

#[test]
fn an_overwrite_in_place_is_caught_on_the_next_read() {
    let fixture = Fixture::new(4 * MIB);
    assert_eq!(fixture.read(), (false, 0));
    fixture.overwrite_in_place();
    let (unavailable, reads) = fixture.read();
    assert!(unavailable, "the forgery went unnoticed");
    assert!(reads > 0);
    // The corrupt verdict is kept for this file too: no read needed.
    assert_eq!(fixture.read(), (true, 0));
    assert!(!fixture.check().integrity_ok());
}

#[test]
fn a_restricted_blob_is_never_read_on_a_read_path() {
    let fixture = Fixture::new(4 * MIB);
    fixture.restrict();
    // A verdict recorded for the current file is used; none is taken.
    assert_eq!(fixture.read(), (false, 0));
    fixture.forget_verdicts();
    // With no verdict: present at the recorded size, and never opened.
    assert_eq!(fixture.read(), (false, 0));
    // A same-size forgery is invisible to a stat, so the read path still
    // calls the blob available; it does not open restricted bytes to know.
    fixture.overwrite_in_place();
    assert_eq!(fixture.read(), (false, 0));
    // `check` hashes every blob, finds the forgery and records it, so the
    // next read reports it without reading.
    assert!(!fixture.check().integrity_ok());
    assert_eq!(fixture.read(), (true, 0));
    // A change of size or a missing file shows at the stat.
    fixture.forget_verdicts();
    let mut permissions = fs::metadata(&fixture.blob).unwrap().permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&fixture.blob, permissions).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&fixture.blob)
        .unwrap()
        .set_len(fixture.size - 1)
        .unwrap();
    assert_eq!(fixture.read(), (true, 0));
    fs::remove_file(&fixture.blob).unwrap();
    assert_eq!(fixture.read(), (true, 0));
}
