//! No silent fallback: a copy that fails because the disk is full rejects the
//! ingest. Nothing is committed, and no reference-only Artifact stands in.
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use investigation_core::catalog::standard_catalog;
use serde_json::json;

use crate::{
    durable::{DurableFile, DurableFs, LocalFs, LockError, WriteGate},
    store::Store,
    workspace::Workspace,
};

/// The local filesystem, except that writing an import stage reports a full disk.
struct FullDisk(LocalFs);

struct StageFile {
    file: std::fs::File,
    full: bool,
}

impl Write for StageFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.full {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "no space left on device",
            ));
        }
        self.file.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl DurableFile for StageFile {
    fn sync_all(&mut self) -> io::Result<()> {
        DurableFile::sync_all(&mut self.file)
    }
    fn set_readonly(&mut self) -> io::Result<()> {
        DurableFile::set_readonly(&mut self.file)
    }
}

impl DurableFs for FullDisk {
    type File = StageFile;
    type Guard = <LocalFs as DurableFs>::Guard;
    fn create_private(&mut self, path: &Path) -> io::Result<StageFile> {
        let full = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("import-"));
        Ok(StageFile {
            file: self.0.create_private(path)?,
            full,
        })
    }
    fn create_dir(&mut self, path: &Path) -> io::Result<()> {
        self.0.create_dir(path)
    }
    fn read(&self, path: &Path) -> io::Result<Box<dyn Read>> {
        self.0.read(path)
    }
    fn sync_existing(&mut self, path: &Path) -> io::Result<()> {
        self.0.sync_existing(path)
    }
    fn list(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.0.list(path)
    }
    fn link_no_replace(&mut self, source: &Path, target: &Path) -> io::Result<()> {
        self.0.link_no_replace(source, target)
    }
    fn rename_no_replace(&mut self, source: &Path, target: &Path) -> io::Result<()> {
        self.0.rename_no_replace(source, target)
    }
    fn sync_dir(&mut self, path: &Path) -> io::Result<()> {
        self.0.sync_dir(path)
    }
    fn remove(&mut self, path: &Path) -> io::Result<()> {
        self.0.remove(path)
    }
    fn remove_dir(&mut self, path: &Path) -> io::Result<()> {
        self.0.remove_dir(path)
    }
    fn lock(&mut self, path: &Path, timeout: Duration) -> Result<Self::Guard, LockError> {
        self.0.lock(path, timeout)
    }
    fn identity(&self, path: &Path) -> io::Result<crate::durable::FileIdentity> {
        self.0.identity(path)
    }
}

#[test]
fn a_full_disk_during_copy_rejects_the_ingest_instead_of_recording_a_reference() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let workspace = Workspace::init(&root, false).unwrap();
    let mut store = Store {
        fs: FullDisk(LocalFs::new(workspace.root())),
        workspace,
        gate: WriteGate::isolated(),
        catalog: standard_catalog(),
    };
    let manifest = json!({
        "schema_version":"inv.ingest.v1","command_id":"full-disk","actor":"agent:reference-test",
        "new_case":{"title":"Does a full disk fall back to a reference?"},
        "source":{"title":"Large capture"},
        "content":{"kind":"stdin","name":"capture"},
    });
    let mut input: &[u8] = b"bytes that cannot be copied";
    let failure = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), Some(&mut input))
        .unwrap_err();
    assert_eq!(
        failure.error.code,
        investigation_core::wire::ErrorCode::IoFailure
    );
    assert!(failure.error.message.contains("StorageFull"), "{failure:?}");
    // Nothing was committed: no case, Source or Artifact, external or otherwise.
    assert!(store.list_cases().unwrap().is_empty());
    let report = store.check().unwrap();
    assert!(report.integrity_ok());
    assert_eq!(report.reference_only_artifacts, 0);
}
