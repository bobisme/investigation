//! Write an [`ExportPlan`] into a fresh destination.
//!
//! The destination is a directory that does not exist yet (it is created) or
//! an existing empty one; anything else is refused before a byte is written.
//! A review product ([`super::Product::Review`]) is written only into a
//! directory this call creates: an existing one, even empty, is refused, so
//! a review copy never lands beside or inside anything else.
//! An existing destination that is a symbolic link is refused. Every
//! directory below it is created with `mkdirat` and opened without following
//! links, and every file is written to a private temporary name, synced and
//! renamed into place with a no-replace rename, so nothing is ever
//! overwritten, even a file that appears while the export is written. The
//! manifest is written last: a directory without it is not a finished
//! export. When any step fails, the files and directories this call created
//! are removed again, one by one and never recursively.
//!
//! Files are created owner-only (0600, directories 0700); the export stays
//! private until the user shares it. Nothing here opens a network
//! connection, starts a process or calls a model.

use std::{ffi::OsStr, os::fd::OwnedFd, path::Path};

use rustix::{
    fs::{AtFlags, CWD, Dir, Mode, OFlags, fsync, mkdirat, openat, unlinkat},
    io::Errno,
};

use super::{ExportError, ExportPlan, MANIFEST_FILE, Product};
use crate::{RenderError, publish::write_bytes_atomically};

const DIRECTORY: OFlags = OFlags::DIRECTORY
    .union(OFlags::RDONLY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// What [`write_export`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// Files written, the manifest included.
    pub files: usize,
    /// Bytes written, the manifest included.
    pub bytes: u64,
    /// Whether the destination directory was created by this call.
    pub created_directory: bool,
}

/// Paths this call created, in creation order, for removal on failure.
#[derive(Default)]
struct Created {
    files: Vec<(OwnedFd, String)>,
    directories: Vec<(OwnedFd, String)>,
}

impl Created {
    fn remove(self) {
        for (directory, name) in self.files.iter().rev() {
            let _ = unlinkat(directory, name.as_str(), AtFlags::empty());
        }
        for (directory, name) in self.directories.iter().rev() {
            let _ = unlinkat(directory, name.as_str(), AtFlags::REMOVEDIR);
        }
    }
}

fn destination(path: &str, error: impl Into<std::io::Error>) -> ExportError {
    ExportError::Destination {
        path: path.to_owned(),
        error: error.into(),
    }
}

fn from_render(error: RenderError) -> ExportError {
    match error {
        RenderError::Io { path, error } => ExportError::Destination { path, error },
        other => ExportError::Render(other),
    }
}

/// Write `plan` into `out`, which must not exist or must be an empty
/// directory. Its parent directory must exist.
pub fn write_export(plan: &ExportPlan, out: &Path) -> Result<Written, ExportError> {
    let name =
        out.file_name()
            .filter(|name| !name.is_empty())
            .ok_or(ExportError::DestinationUnsafe(
                "it must end in a directory name",
            ))?;
    let parent = out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent_fd = openat(
        CWD,
        parent,
        OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| match error {
        Errno::NOENT | Errno::NOTDIR => {
            ExportError::DestinationUnsafe("its parent directory does not exist")
        }
        error => destination("destination parent", error),
    })?;
    let label = name.to_string_lossy().into_owned();
    let fresh_only = plan.kind().product == Product::Review;
    let (root, created_directory) = open_destination(&parent_fd, name, &label, fresh_only)?;
    let mut created = Created::default();
    if created_directory {
        let parent_copy = parent_fd
            .try_clone()
            .map_err(|error| destination(&label, error))?;
        created.directories.push((parent_copy, label.clone()));
    }
    let tag = plan
        .manifest()
        .export_id
        .trim_start_matches("exp_")
        .get(..12)
        .unwrap_or("export")
        .to_owned();
    match write_files(plan, &root, &tag, &mut created) {
        Ok(written) => {
            if created_directory {
                fsync(&parent_fd).map_err(|error| destination(&label, error))?;
            }
            Ok(Written {
                created_directory,
                ..written
            })
        }
        Err(error) => {
            created.remove();
            Err(error)
        }
    }
}

/// Create `name` in `parent`, or open it when it is an existing empty
/// directory. Returns the directory and whether it was created.
fn open_destination(
    parent: &OwnedFd,
    name: &OsStr,
    label: &str,
    fresh_only: bool,
) -> Result<(OwnedFd, bool), ExportError> {
    let created = match mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
        Ok(()) => true,
        Err(Errno::EXIST) if fresh_only => return Err(ExportError::DestinationExists),
        Err(Errno::EXIST) => false,
        Err(error) => return Err(destination(label, error)),
    };
    let root = match openat(parent, name, DIRECTORY, Mode::empty()) {
        Ok(root) => root,
        Err(error) => {
            if created {
                let _ = unlinkat(parent, name, AtFlags::REMOVEDIR);
            }
            return Err(match error {
                Errno::LOOP | Errno::NOTDIR => {
                    ExportError::DestinationUnsafe("it is a symbolic link or not a directory")
                }
                error => destination(label, error),
            });
        }
    };
    if !created && !is_empty(&root).map_err(|error| destination(label, error))? {
        return Err(ExportError::DestinationNotEmpty);
    }
    Ok((root, created))
}

fn is_empty(directory: &OwnedFd) -> Result<bool, Errno> {
    let mut listing = Dir::read_from(directory)?;
    while let Some(item) = listing.read() {
        let name = item?.file_name().to_bytes().to_vec();
        if name != b"." && name != b".." {
            return Ok(false);
        }
    }
    Ok(true)
}

fn write_files(
    plan: &ExportPlan,
    root: &OwnedFd,
    tag: &str,
    created: &mut Created,
) -> Result<Written, ExportError> {
    let mut written = Written {
        files: 0,
        bytes: 0,
        created_directory: false,
    };
    let manifest = (MANIFEST_FILE, plan.manifest_bytes());
    let entries = plan
        .files()
        .entries()
        .iter()
        .map(|entry| (entry.path().as_str(), entry.content()))
        .chain(std::iter::once(manifest));
    for (path, content) in entries {
        let mut components: Vec<&str> = path.split('/').collect();
        let file = components.pop().unwrap_or(path);
        let directory = walk(root, &components, path, created)?;
        let fresh = write_bytes_atomically(&directory, path, file, content, tag, false)
            .map_err(from_render)?;
        if !fresh {
            return Err(ExportError::DestinationUnsafe(
                "a file appeared in it while the export was written",
            ));
        }
        created.files.push((directory, file.to_owned()));
        written.files += 1;
        written.bytes += content.len() as u64;
    }
    fsync(root).map_err(|error| destination("destination", error))?;
    Ok(written)
}

/// Open the directory `components` below `root`, creating missing ones and
/// never following a link.
fn walk(
    root: &OwnedFd,
    components: &[&str],
    path: &str,
    created: &mut Created,
) -> Result<OwnedFd, ExportError> {
    let mut directory = root.try_clone().map_err(|error| destination(path, error))?;
    for component in components {
        let next = match openat(&directory, *component, DIRECTORY, Mode::empty()) {
            Err(Errno::NOENT) => {
                mkdirat(&directory, *component, Mode::from_raw_mode(0o700))
                    .map_err(|error| destination(path, error))?;
                let parent = directory
                    .try_clone()
                    .map_err(|error| destination(path, error))?;
                created.directories.push((parent, (*component).to_owned()));
                openat(&directory, *component, DIRECTORY, Mode::empty())
            }
            other => other,
        };
        directory = next.map_err(|error| match error {
            Errno::LOOP | Errno::NOTDIR => ExportError::DestinationUnsafe(
                "a directory inside it was replaced while the export was written",
            ),
            error => destination(path, error),
        })?;
    }
    Ok(directory)
}
