//! Reading the person-owned vault files (`Notes/`, `Boards/`, `Inbox/`)
//! without following links, through the projection root (plan §12.1).
//!
//! The one reader behind both consumers of human files:
//!
//! - a workspace archive copies every file below the human folders
//!   ([`read_all`]), listing what it leaves out and why;
//! - `vault snapshot-note` captures one file a record relies on
//!   ([`read_one`]), and `vault note-snapshots` and the working vault's
//!   annotation notes compare it with the bytes captured earlier
//!   ([`read_live`], [`LiveFile::status`]).
//!
//! The root is the dedicated vault's `vault/` or, in existing-vault mode, the
//! managed prefix inside the person's vault ([`crate::projection`]). Every
//! directory below it is opened from its parent's handle with `O_NOFOLLOW`,
//! and every file with `O_NOFOLLOW | O_NONBLOCK` and checked with `fstat`, so
//! no symbolic link, special file or path outside the root is ever read. A
//! path is a plain vault-relative path checked by
//! [`investigation_core::human_file::folder`]: no absolute, `..`, hidden or
//! empty component, so it cannot name anything outside the prefix either.
//! Nothing here writes.

use std::{fs::File, io::Read, os::fd::OwnedFd};

use investigation_core::{
    human_file::{self, HUMAN_FOLDERS},
    wire::Digest,
};
use investigation_store::{
    Workspace,
    journal::hash_bytes,
    store::archive::{MAX_HUMAN_NOTE_BYTES, MAX_HUMAN_NOTES, MAX_NOTE_SNAPSHOT_BYTES},
};
use rustix::{
    fs::{AtFlags, Dir, FileType, Mode, OFlags, fstat, openat, statat},
    io::Errno,
};

use crate::{
    RenderError,
    export::archive::{HumanNote, SkippedNote},
    projection,
};

const DIRECTORY: OFlags = OFlags::DIRECTORY
    .union(OFlags::RDONLY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

/// A reason a link is refused or skipped.
pub const SYMBOLIC_LINK: &str = "symbolic link";
/// A reason a device, socket, FIFO or folder is refused or skipped.
pub const NOT_A_REGULAR_FILE: &str = "not a regular file";

/// What reading one person-owned file found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRead {
    /// The exact bytes of a regular file within the limit.
    Bytes(Vec<u8>),
    /// Nothing at the path (or the vault or prefix holds no such folder).
    Missing,
    /// The path is a link, lies below one, is not a regular file or is over
    /// the limit; the reason is a fixed phrase.
    Refused(String),
}

/// A person-owned file as read now, for comparing with a note snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveFile {
    /// A regular file within the limit: the digest and size of its bytes.
    Read { sha256: Digest, bytes: u64 },
    /// Nothing at the path.
    Missing,
    /// A link, special file or file over the limit; the reason is a fixed
    /// phrase ([`FileRead::Refused`]).
    Refused(String),
}

impl From<FileRead> for LiveFile {
    fn from(read: FileRead) -> Self {
        match read {
            FileRead::Bytes(bytes) => Self::Read {
                sha256: hash_bytes(&bytes),
                bytes: bytes.len() as u64,
            },
            FileRead::Missing => Self::Missing,
            FileRead::Refused(reason) => Self::Refused(reason),
        }
    }
}

/// How the live file compares with the bytes a snapshot captured: what
/// `vault note-snapshots` reports and the working vault's annotation note
/// shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveStatus {
    /// The live file holds exactly the captured bytes.
    Unchanged,
    /// The live file holds other bytes: their digest and size.
    Changed {
        sha256: Digest,
        bytes: u64,
    },
    Missing,
    Refused(String),
}

impl LiveFile {
    /// Compare with the `captured` digest.
    pub fn status(&self, captured: &Digest) -> LiveStatus {
        match self {
            Self::Read { sha256, .. } if sha256 == captured => LiveStatus::Unchanged,
            Self::Read { sha256, bytes } => LiveStatus::Changed {
                sha256: sha256.clone(),
                bytes: *bytes,
            },
            Self::Missing => LiveStatus::Missing,
            Self::Refused(reason) => LiveStatus::Refused(reason.clone()),
        }
    }
}

impl LiveStatus {
    /// `unchanged`, `changed`, `missing` or `refused`.
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Changed { .. } => "changed",
            Self::Missing => "missing",
            Self::Refused(_) => "refused",
        }
    }
}

/// Read the person-owned file at `path` ([`read_one`]) for comparison.
pub fn read_live(workspace: &Workspace, path: &str) -> Result<LiveFile, RenderError> {
    read_one(workspace, path).map(LiveFile::from)
}

/// The projection root, opened without following links; `None` when the
/// vault is disabled or the managed prefix does not exist yet.
fn open_root(workspace: &Workspace) -> Result<Option<OwnedFd>, RenderError> {
    if !workspace.config().vault_policy.enabled {
        return Ok(None);
    }
    projection::open_root(workspace, false)
}

/// Read the person-owned file at vault-relative `path` (relative to the
/// managed prefix in existing-vault mode), at most
/// `vault.note_snapshot_bytes`. A path that is not a plain path below the
/// human folders is a [`RenderError::InvalidPath`].
pub fn read_one(workspace: &Workspace, path: &str) -> Result<FileRead, RenderError> {
    human_file::folder(path).map_err(|reason| RenderError::InvalidPath {
        path: path.to_owned(),
        reason,
    })?;
    let Some(root) = open_root(workspace)? else {
        return Ok(FileRead::Missing);
    };
    let components: Vec<&str> = path.split('/').collect();
    let (name, parents) = components.split_last().expect("a checked path has a file");
    let mut directory = root;
    for (depth, component) in parents.iter().enumerate() {
        directory = match openat(&directory, *component, DIRECTORY, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(FileRead::Missing),
            Err(Errno::LOOP) => return Ok(FileRead::Refused(SYMBOLIC_LINK.to_owned())),
            Err(Errno::NOTDIR) => {
                return match statat(&directory, *component, AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
                        Ok(FileRead::Refused(SYMBOLIC_LINK.to_owned()))
                    }
                    _ => Ok(FileRead::Missing),
                };
            }
            Err(error) => return Err(RenderError::io(components[..=depth].join("/"), error)),
        };
    }
    read_file(
        &directory,
        name,
        path,
        MAX_NOTE_SNAPSHOT_BYTES,
        "vault.note_snapshot_bytes",
    )
}

/// Open and read one entry of `directory` without following a link.
fn read_file(
    directory: &OwnedFd,
    name: &str,
    label: &str,
    limit: u64,
    limit_name: &str,
) -> Result<FileRead, RenderError> {
    let fd = match openat(directory, name, FILE, Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(FileRead::Missing),
        Err(Errno::LOOP) => return Ok(FileRead::Refused(SYMBOLIC_LINK.to_owned())),
        // A FIFO with no writer, or a socket or device that refuses a read.
        Err(Errno::NXIO | Errno::OPNOTSUPP) => {
            return Ok(FileRead::Refused(NOT_A_REGULAR_FILE.to_owned()));
        }
        Err(error) => return Err(RenderError::io(label, error)),
    };
    let stat = fstat(&fd).map_err(|error| RenderError::io(label, error))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Ok(FileRead::Refused(NOT_A_REGULAR_FILE.to_owned()));
    }
    let over = || FileRead::Refused(format!("larger than {limit_name}"));
    if u64::try_from(stat.st_size).unwrap_or(u64::MAX) > limit {
        return Ok(over());
    }
    let mut bytes = Vec::new();
    File::from(fd)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| RenderError::io(label, error))?;
    if bytes.len() as u64 > limit {
        return Ok(over());
    }
    Ok(FileRead::Bytes(bytes))
}

/// Every regular file below the human folders of the projection root, never
/// following a link, with what was left out and why: links, special files,
/// names that are not UTF-8, render temporaries and files over the archive
/// limits. Paths are relative to the root (`Notes/plan.md`).
pub fn read_all(workspace: &Workspace) -> Result<(Vec<HumanNote>, Vec<SkippedNote>), RenderError> {
    let mut notes = Vec::new();
    let mut skipped = Vec::new();
    let Some(root) = open_root(workspace)? else {
        return Ok((notes, skipped));
    };
    for folder in HUMAN_FOLDERS {
        match openat(&root, folder, DIRECTORY, Mode::empty()) {
            Ok(directory) => walk(&directory, folder, &mut notes, &mut skipped)?,
            Err(Errno::NOENT) => {}
            Err(Errno::LOOP | Errno::NOTDIR) => skipped.push(SkippedNote {
                path: folder.to_owned(),
                reason: match statat(&root, folder, AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
                        SYMBOLIC_LINK
                    }
                    _ => NOT_A_REGULAR_FILE,
                }
                .to_owned(),
            }),
            Err(error) => return Err(RenderError::io(folder, error)),
        }
    }
    Ok((notes, skipped))
}

fn walk(
    directory: &OwnedFd,
    relative: &str,
    notes: &mut Vec<HumanNote>,
    skipped: &mut Vec<SkippedNote>,
) -> Result<(), RenderError> {
    let mut names = Vec::new();
    let mut listing =
        Dir::read_from(directory).map_err(|error| RenderError::io(relative, error))?;
    while let Some(item) = listing.read() {
        let item = item.map_err(|error| RenderError::io(relative, error))?;
        let raw = item.file_name().to_bytes();
        if raw != b"." && raw != b".." {
            names.push(raw.to_vec());
        }
    }
    names.sort();
    for raw in names {
        let shown = format!("{relative}/{}", String::from_utf8_lossy(&raw));
        let skip = |reason: &str, skipped: &mut Vec<SkippedNote>| {
            skipped.push(SkippedNote {
                path: shown.clone(),
                reason: reason.to_owned(),
            });
        };
        let Ok(name) = std::str::from_utf8(&raw) else {
            skip("name is not UTF-8", skipped);
            continue;
        };
        let stat = match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => continue,
            Err(error) => return Err(RenderError::io(shown, error)),
        };
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::Symlink => skip(SYMBOLIC_LINK, skipped),
            FileType::Directory => match openat(directory, name, DIRECTORY, Mode::empty()) {
                Ok(child) => walk(&child, &format!("{relative}/{name}"), notes, skipped)?,
                Err(Errno::NOENT) => {}
                Err(Errno::LOOP | Errno::NOTDIR) => skip(SYMBOLIC_LINK, skipped),
                Err(error) => return Err(RenderError::io(shown, error)),
            },
            FileType::RegularFile => {
                if name.ends_with(".inv-tmp") || name.ends_with(".inv-del") {
                    skip("render temporary", skipped);
                } else if u64::try_from(stat.st_size).unwrap_or(u64::MAX) > MAX_HUMAN_NOTE_BYTES {
                    skip("larger than archive.human_note_bytes", skipped);
                } else if notes.len() >= MAX_HUMAN_NOTES {
                    skip("over archive.human_notes", skipped);
                } else {
                    match read_file(
                        directory,
                        name,
                        &shown,
                        MAX_HUMAN_NOTE_BYTES,
                        "archive.human_note_bytes",
                    )? {
                        FileRead::Bytes(bytes) => notes.push(HumanNote { path: shown, bytes }),
                        FileRead::Missing => {}
                        FileRead::Refused(reason) => skip(&reason, skipped),
                    }
                }
            }
            _ => skip(NOT_A_REGULAR_FILE, skipped),
        }
    }
    Ok(())
}
