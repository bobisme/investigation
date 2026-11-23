//! A dry run: one command checked exactly as a commit would check it, with
//! nothing written to the workspace.
//!
//! [`crate::durable::LocalFs`] in dry-run mode keeps the bytes a command
//! stages in unnamed temporary files outside the workspace, which the
//! operating system removes when the process closes them, and refuses every
//! other write: no journal, blob, staging, directory, cache entry, kept head
//! or recovery marker. Reads and the commit lock behave as in a commit, so
//! the command is validated against the same verified snapshot. The store
//! stops where a commit would publish and returns the receipt it would have
//! returned (see `Store::set_dry_run`).

use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
};

use investigation_core::{identity::Id, state::State};

/// The only workspace directory whose writes a dry run keeps, in memory of
/// the process and never on the workspace's disk.
const STAGING: &str = "state/tmp";

/// What a dry run would have written: staged bytes and the planned state.
#[derive(Default)]
pub struct DryRun {
    staged: BTreeMap<PathBuf, File>,
    planned: Option<(Id, State)>,
}

impl DryRun {
    /// Stage under `state/tmp` into an unnamed temporary file; refuse any
    /// other path.
    pub(crate) fn create(&mut self, path: &Path) -> io::Result<File> {
        if !path.starts_with(STAGING) {
            return Err(refused(path));
        }
        if self.staged.contains_key(path) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let file = tempfile::tempfile()?;
        let writer = file.try_clone()?;
        self.staged.insert(path.to_owned(), file);
        Ok(writer)
    }

    /// Read staged bytes from the start, independently of other readers.
    pub(crate) fn read(&self, path: &Path) -> Option<io::Result<Box<dyn Read>>> {
        let file = self.staged.get(path)?;
        Some(
            file.try_clone()
                .map(|file| Box::new(StagedReader { file, position: 0 }) as Box<dyn Read>),
        )
    }

    pub(crate) fn staged(&self, path: &Path) -> Option<&File> {
        self.staged.get(path)
    }

    /// Forget staged bytes; any other removal is refused.
    pub(crate) fn remove(&mut self, path: &Path) -> io::Result<()> {
        self.staged
            .remove(path)
            .map(drop)
            .ok_or_else(|| refused(path))
    }

    /// Remember the state the planned transaction would leave its case in.
    pub(crate) fn plan(&mut self, case_id: Id, state: State) {
        self.planned = Some((case_id, state));
    }

    /// The planned state of `case_id`, when the dry run planned a transaction
    /// in it.
    pub(crate) fn planned(&self, case_id: Id) -> Option<&State> {
        self.planned
            .as_ref()
            .filter(|(id, _)| *id == case_id)
            .map(|(_, state)| state)
    }
}

/// The error for a write a dry run never makes.
pub(crate) fn refused(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "A dry run writes nothing to the workspace; refused a write to {}.",
            path.display()
        ),
    )
}

/// Reads with its own position, so staged bytes can be read again while the
/// stager's handle is still open.
struct StagedReader {
    file: File,
    position: u64,
}

impl Read for StagedReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.file.read_at(buffer, self.position)?;
        self.position += read as u64;
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn staging_stays_out_of_the_workspace_and_other_writes_are_refused() {
        let mut dry = DryRun::default();
        let path = Path::new("state/tmp/import-1.tmp");
        let mut writer = dry.create(path).unwrap();
        writer.write_all(b"staged bytes").unwrap();
        for _ in 0..2 {
            let mut bytes = Vec::new();
            dry.read(path)
                .unwrap()
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(bytes, b"staged bytes");
        }
        assert!(dry.create(path).is_err(), "staging names stay unique");
        let refusal = dry.create(Path::new("state/cases/x")).unwrap_err();
        assert_eq!(refusal.kind(), io::ErrorKind::PermissionDenied);
        assert!(dry.read(Path::new("state/tmp/other")).is_none());
        dry.remove(path).unwrap();
        assert!(dry.read(path).is_none());
        assert!(dry.remove(path).is_err());
    }
}
