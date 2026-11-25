//! `vault clean-conflicts`: remove the bytes the renderer and `vault
//! reconcile` kept in the workspace's conflicts area, and nothing else.
//!
//! Three areas hold them:
//!
//! - `discard` and `propose-change` keep the observed files of a drifted path
//!   ([`crate::reconcile`]) at `local/conflicts/<area>/<hash prefix>/<vault
//!   path>`, where `<area>` is `discarded` or `proposed` and `<hash prefix>`
//!   is the first 16 hex digits of the SHA-256 of the bytes kept there.
//! - A render keeps the content it withheld from a drifted path at
//!   `local/conflicts/<generation>/<vault path>` ([`crate::drift`]), and the
//!   render manifest records the hash of every such file until it is
//!   removed ([`crate::Withheld`]), after the drift is reconciled too.
//!
//! They stay until a person removes them. This module is that removal, made
//! explicit and previewable.
//!
//! A file is removed only when it is identified as one the tool wrote: a
//! regular file (never a link, never followed) under a folder named by 16
//! lowercase hex digits directly inside a set-aside area, whose bytes still
//! hash to that prefix; or a file at the path of a withheld version the
//! manifest records, whose bytes still hash to the recorded hash, and which
//! no open drift record names (that one is reconciled first). Everything
//! else is listed as kept with the reason, and never opened for writing: a
//! file whose bytes changed since it was kept, a link or special file, a
//! file or folder not under a hash-named folder, a file no record names
//! (such as a withheld version written before records were kept), a
//! leftover temporary. A withheld version's record whose file is already
//! gone is forgotten.
//!
//! [`preview`] lists what [`clean`] would do without taking the render lock
//! or writing anything (not even the lock file). [`clean`] walks the area
//! again under the render lock, so no reconcile sets bytes aside meanwhile,
//! and removes each identified file through the publisher's verified
//! removal: renamed aside, hashed again, and unlinked only when the bytes
//! are still the ones identified; otherwise put back and kept. Folders the
//! removal leaves empty are removed with non-recursive `rmdir`s, the area
//! folder last (never `local/conflicts/` itself). Nothing is ever deleted
//! recursively. Then the manifest is stored without the records of the
//! withheld versions removed or found gone; nothing else in it changes, and
//! canonical state and the vault are not touched. A clean killed between the
//! two leaves records naming missing files, which the next clean forgets.

use std::{
    collections::{BTreeMap, BTreeSet},
    os::fd::OwnedFd,
};

use investigation_core::wire::Digest;
use investigation_store::Workspace;
use rustix::{
    fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, statat, unlinkat},
    io::Errno,
};

use crate::{
    Manifest, ManifestStatus, RENDER_LOCK_WAIT, RenderError, RenderLock,
    drift::{CONFLICTS_DIR, read_regular},
    lock::{self, Probe, open_local},
    publish::{Removal, remove_verified},
    reconcile::{DISCARDED_DIR, PROPOSED_DIR},
};

/// Entries one request visits across its areas. A larger area is cleaned
/// over several requests; the report says when the walk stopped early.
pub const MAX_ENTRIES: usize = 10_000;
/// Folder depth below a hash-named folder: a vault path's components.
const MAX_DEPTH: usize = 32;
/// Temporary-name tag for files moved aside before they are deleted.
const TAG: &str = "clean";
/// Length of the hash prefix that names a set-aside folder.
const PREFIX_LEN: usize = 16;

/// One of the places the tool keeps bytes in the conflicts area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Area {
    /// `local/conflicts/discarded/`: kept by a confirmed `discard`.
    Discarded,
    /// `local/conflicts/proposed/`: kept by `propose-change`.
    Proposed,
    /// `local/conflicts/<generation>/`: generated content a render withheld
    /// from a drifted path.
    Withheld,
}

impl Area {
    pub const ALL: [Self; 3] = [Self::Discarded, Self::Proposed, Self::Withheld];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discarded => "discarded",
            Self::Proposed => "proposed",
            Self::Withheld => "withheld",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|area| area.as_str() == text)
    }

    /// The workspace-relative folder, as shown; the withheld versions are
    /// in one folder per render generation.
    pub const fn directory(self) -> &'static str {
        match self {
            Self::Discarded => DISCARDED_DIR,
            Self::Proposed => PROPOSED_DIR,
            Self::Withheld => "local/conflicts/<generation>",
        }
    }
}

/// Why an entry of an area is left in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// Not inside a folder named by a 16-digit hash prefix.
    UnexpectedEntry,
    /// A link, a special file, or a file too large to be a set-aside note.
    NotARegularFile,
    /// Its bytes no longer hash to the prefix it was kept under.
    BytesDiffer,
    /// Its bytes changed while it was being removed; it was put back.
    Changed,
    /// Nested deeper than any vault path; not walked.
    TooDeep,
    /// A file in a generation folder that no withheld-version record names:
    /// written before records were kept, its record lost with the manifest,
    /// or put there by someone else.
    Unrecorded,
    /// A withheld version an open drift record still names; reconcile the
    /// drift first.
    InUse,
}

impl Kept {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnexpectedEntry => "unexpected_entry",
            Self::NotARegularFile => "not_a_regular_file",
            Self::BytesDiffer => "bytes_differ",
            Self::Changed => "changed",
            Self::TooDeep => "too_deep",
            Self::Unrecorded => "unrecorded",
            Self::InUse => "in_use",
        }
    }
}

/// What the request did, or would do, with one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Identified; [`preview`] would remove it.
    WouldRemove,
    /// Identified and removed.
    Removed,
    /// Identified, and gone before it could be removed.
    Vanished,
    /// Left in place.
    Kept(Kept),
}

/// One entry of an area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub area: Area,
    /// Workspace-relative path, such as
    /// `local/conflicts/discarded/0123456789abcdef/Cases/<id>/Overview.md`.
    pub path: String,
    /// The vault path the bytes were observed at, for an entry under a
    /// hash-named folder.
    pub vault_path: Option<String>,
    /// The hash of the bytes, for an identified file or one whose bytes
    /// differ.
    pub sha256: Option<Digest>,
    pub bytes: Option<u64>,
    pub action: Action,
}

/// What [`clean`] did, or [`preview`] would do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CleanReport {
    /// Every entry visited, in path order.
    pub entries: Vec<Entry>,
    /// Workspace-relative folders removed because they were left empty.
    pub removed_folders: Vec<String>,
    /// The walk stopped at [`MAX_ENTRIES`]; run again for the rest.
    pub truncated: bool,
    /// Withheld-version records whose file is already gone: forgotten by
    /// [`clean`], or that would be.
    pub forgotten: Vec<String>,
}

impl CleanReport {
    pub fn count(&self, wanted: fn(&Action) -> bool) -> usize {
        self.entries
            .iter()
            .filter(|entry| wanted(&entry.action))
            .count()
    }

    /// Bytes of the files removed, or that would be.
    pub fn removed_bytes(&self) -> u64 {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.action, Action::Removed | Action::WouldRemove))
            .filter_map(|entry| entry.bytes)
            .sum()
    }
}

/// List what [`clean`] would remove and keep in `areas`, without taking the
/// render lock or writing anything. Busy while a render or reconcile runs.
pub fn preview(workspace: &Workspace, areas: &[Area]) -> Result<CleanReport, RenderError> {
    let probe = lock::probe(workspace)?;
    if matches!(probe, Probe::Running) {
        return Err(RenderError::Busy {
            waited: std::time::Duration::ZERO,
        });
    }
    walk_areas(workspace, areas, None)
}

/// [`preview`] while a render or reconcile may run, for a diagnosis that
/// must not wait or fail on one (`investigation exposure`): it reads the
/// areas and the manifest as they are and writes nothing.
pub fn list(workspace: &Workspace, areas: &[Area]) -> Result<CleanReport, RenderError> {
    walk_areas(workspace, areas, None)
}

/// Remove every identified file in `areas` under the render lock, and the
/// folders that leaves empty. See the module documentation.
pub fn clean(workspace: &Workspace, areas: &[Area]) -> Result<CleanReport, RenderError> {
    let lock = RenderLock::acquire(workspace, RENDER_LOCK_WAIT)?;
    walk_areas(workspace, areas, Some(&lock))
}

/// Walk `areas`, removing identified files when `lock` is given.
fn walk_areas(
    workspace: &Workspace,
    areas: &[Area],
    lock: Option<&RenderLock>,
) -> Result<CleanReport, RenderError> {
    let local = open_local(workspace.root())?;
    let mut areas = areas.to_vec();
    areas.sort();
    areas.dedup();
    // Only a manifest that was read whole identifies withheld versions; an
    // unreadable one identifies none and is never stored over.
    let loaded = Manifest::read(workspace)?;
    let recorded = loaded.status == ManifestStatus::Loaded;
    let manifest = loaded.manifest;
    let mut walk = Walker {
        remove: lock.is_some(),
        report: CleanReport::default(),
        visited: 0,
        withheld: manifest
            .withheld()
            .map(|withheld| (withheld.path.clone(), withheld.sha256.clone()))
            .collect(),
        in_use: manifest
            .withheld_in_use()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    };
    for area in &areas {
        if *area == Area::Withheld {
            if let Some(directory) = open_folder(&local, &["conflicts"], CONFLICTS_DIR)? {
                walk.generations(&directory)?;
            }
        } else if let Some(directory) =
            open_folder(&local, &area_components(*area), area.directory())?
        {
            let emptied = walk.area(*area, &directory)?;
            if walk.remove && emptied && remove_folder(&local, *area)? {
                walk.report
                    .removed_folders
                    .push(area.directory().to_owned());
            }
        }
        if walk.report.truncated {
            break;
        }
    }
    let mut report = walk.report;
    report
        .entries
        .sort_by(|left, right| left.path.cmp(&right.path));
    if recorded && areas.contains(&Area::Withheld) {
        let visited: BTreeSet<&str> = report
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        if !report.truncated {
            report.forgotten = walk
                .withheld
                .keys()
                .filter(|path| !visited.contains(path.as_str()) && !walk.in_use.contains(*path))
                .cloned()
                .collect();
        }
        let gone: BTreeSet<String> = report
            .entries
            .iter()
            .filter(|entry| entry.area == Area::Withheld)
            .filter(|entry| matches!(entry.action, Action::Removed | Action::Vanished))
            .map(|entry| entry.path.clone())
            .chain(report.forgotten.iter().cloned())
            .collect();
        if let Some(lock) = lock
            && !gone.is_empty()
        {
            manifest.without_withheld(&gone).store(workspace, lock)?;
        }
    }
    Ok(report)
}

/// `["conflicts", "<area>"]`: the area below `local/`.
fn area_components(area: Area) -> [&'static str; 2] {
    ["conflicts", area.as_str()]
}

/// Remove the empty area folder itself.
fn remove_folder(local: &OwnedFd, area: Area) -> Result<bool, RenderError> {
    let Some(conflicts) = open_folder(local, &["conflicts"], CONFLICTS_DIR)? else {
        return Ok(false);
    };
    rmdir(&conflicts, area.as_str(), area.directory())
}

/// Open `components` below `directory` without following links; `None` when
/// one is missing. A link or file in the way is refused.
fn open_folder(
    directory: &OwnedFd,
    components: &[&str],
    label: &str,
) -> Result<Option<OwnedFd>, RenderError> {
    let flags = OFlags::DIRECTORY | OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut current: Option<OwnedFd> = None;
    for component in components {
        let parent = current.as_ref().unwrap_or(directory);
        current = Some(match openat(parent, *component, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(Errno::LOOP | Errno::NOTDIR) => {
                return Err(RenderError::UnsafeDirectory("workspace local/conflicts/"));
            }
            Err(error) => return Err(RenderError::io(label, error)),
        });
    }
    Ok(current)
}

/// Remove the folder `name` in `directory` if it is empty.
fn rmdir(directory: &OwnedFd, name: &str, label: &str) -> Result<bool, RenderError> {
    match unlinkat(directory, name, AtFlags::REMOVEDIR) {
        Ok(()) => Ok(true),
        Err(Errno::NOTEMPTY | Errno::EXIST | Errno::NOENT | Errno::NOTDIR) => Ok(false),
        Err(error) => Err(RenderError::io(label, error)),
    }
}

/// Whether `name` is a 16-digit lowercase hex hash prefix.
fn is_prefix(name: &str) -> bool {
    name.len() == PREFIX_LEN
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The entries of `directory` as (name, type), sorted, without `.` and `..`.
fn listing(directory: &OwnedFd, label: &str) -> Result<Vec<(String, FileType)>, RenderError> {
    let mut items = Vec::new();
    let mut dir = Dir::read_from(directory).map_err(|error| RenderError::io(label, error))?;
    while let Some(item) = dir.read() {
        let item = item.map_err(|error| RenderError::io(label, error))?;
        let raw = item.file_name().to_bytes();
        if raw == b"." || raw == b".." {
            continue;
        }
        let name = String::from_utf8_lossy(raw).into_owned();
        let mut kind = item.file_type();
        if kind == FileType::Unknown {
            kind = match statat(directory, item.file_name(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => FileType::from_raw_mode(stat.st_mode),
                Err(Errno::NOENT) => continue,
                Err(error) => return Err(RenderError::io(label, error)),
            };
        }
        // A name that is not UTF-8 cannot be named again; keep it listed.
        if std::str::from_utf8(raw).is_err() {
            kind = FileType::Unknown;
        }
        items.push((name, kind));
    }
    items.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(items)
}

struct Walker {
    remove: bool,
    report: CleanReport,
    visited: usize,
    /// Recorded withheld versions: workspace-relative path to hash.
    withheld: BTreeMap<String, Digest>,
    /// Withheld versions an open drift record names.
    in_use: BTreeSet<String>,
}

impl Walker {
    /// Count one visited entry; false once the bound is reached.
    fn visit(&mut self) -> bool {
        if self.visited >= MAX_ENTRIES {
            self.report.truncated = true;
            return false;
        }
        self.visited += 1;
        true
    }

    fn keep(&mut self, area: Area, path: String, vault_path: Option<String>, why: Kept) {
        self.report.entries.push(Entry {
            area,
            path,
            vault_path,
            sha256: None,
            bytes: None,
            action: Action::Kept(why),
        });
    }

    /// Walk one area. Returns whether it is now empty.
    fn area(&mut self, area: Area, directory: &OwnedFd) -> Result<bool, RenderError> {
        let label = area.directory();
        let mut empty = true;
        for (name, kind) in listing(directory, label)? {
            if !self.visit() {
                return Ok(false);
            }
            let path = format!("{label}/{name}");
            if kind != FileType::Directory || !is_prefix(&name) {
                self.keep(area, path, None, Kept::UnexpectedEntry);
                empty = false;
                continue;
            }
            let Some(folder) = open_folder(directory, &[name.as_str()], &path)? else {
                continue;
            };
            let emptied = self.folder(area, &folder, &name, &path, "", 0)?;
            if self.remove && emptied && rmdir(directory, &name, &path)? {
                self.report.removed_folders.push(path);
            } else {
                empty = false;
            }
            if self.report.truncated {
                return Ok(false);
            }
        }
        Ok(empty)
    }

    /// Walk the generation folders of `local/conflicts/`: every entry but
    /// the two set-aside areas.
    fn generations(&mut self, directory: &OwnedFd) -> Result<(), RenderError> {
        let area = Area::Withheld;
        for (name, kind) in listing(directory, CONFLICTS_DIR)? {
            if name == Area::Discarded.as_str() || name == Area::Proposed.as_str() {
                continue;
            }
            if !self.visit() {
                return Ok(());
            }
            let path = format!("{CONFLICTS_DIR}/{name}");
            if kind != FileType::Directory {
                self.keep(area, path, None, Kept::UnexpectedEntry);
                continue;
            }
            let Some(folder) = open_folder(directory, &[name.as_str()], &path)? else {
                continue;
            };
            let emptied = self.folder(area, &folder, &name, &path, "", 0)?;
            if self.remove && emptied && rmdir(directory, &name, &path)? {
                self.report.removed_folders.push(path);
            }
            if self.report.truncated {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Walk a folder below the hash-named or generation folder `prefix`; `inside` is its
    /// vault-relative path so far. Returns whether it is now empty.
    fn folder(
        &mut self,
        area: Area,
        directory: &OwnedFd,
        prefix: &str,
        label: &str,
        inside: &str,
        depth: usize,
    ) -> Result<bool, RenderError> {
        let mut empty = true;
        for (name, kind) in listing(directory, label)? {
            if !self.visit() {
                return Ok(false);
            }
            let path = format!("{label}/{name}");
            let vault_path = if inside.is_empty() {
                name.clone()
            } else {
                format!("{inside}/{name}")
            };
            match kind {
                FileType::Directory if depth < MAX_DEPTH => {
                    let Some(folder) = open_folder(directory, &[name.as_str()], &path)? else {
                        continue;
                    };
                    let emptied =
                        self.folder(area, &folder, prefix, &path, &vault_path, depth + 1)?;
                    if self.remove && emptied && rmdir(directory, &name, &path)? {
                        self.report.removed_folders.push(path);
                    } else {
                        empty = false;
                    }
                    if self.report.truncated {
                        return Ok(false);
                    }
                }
                FileType::Directory => {
                    self.keep(area, path, Some(vault_path), Kept::TooDeep);
                    empty = false;
                }
                FileType::RegularFile => {
                    if !self.file(area, directory, prefix, &name, path, vault_path)? {
                        empty = false;
                    }
                }
                _ => {
                    self.keep(area, path, Some(vault_path), Kept::NotARegularFile);
                    empty = false;
                }
            }
        }
        Ok(empty)
    }

    /// Identify, and when removing remove, one regular file. Returns whether
    /// it is gone.
    fn file(
        &mut self,
        area: Area,
        directory: &OwnedFd,
        prefix: &str,
        name: &str,
        path: String,
        vault_path: String,
    ) -> Result<bool, RenderError> {
        let Some(bytes) = read_regular(directory, name, &path)? else {
            self.keep(area, path, Some(vault_path), Kept::NotARegularFile);
            return Ok(false);
        };
        // Known as reconcile and the render knew it: a `.canvas` file by
        // its content (ADR 0019).
        let sha256 = crate::canvas::digest(name, &bytes);
        let size = bytes.len() as u64;
        let refusal = if area == Area::Withheld {
            match self.withheld.get(&path) {
                None => Some(Kept::Unrecorded),
                Some(recorded) if *recorded != sha256 => Some(Kept::BytesDiffer),
                Some(_) if self.in_use.contains(&path) => Some(Kept::InUse),
                Some(_) => None,
            }
        } else {
            let identified = sha256
                .as_str()
                .trim_start_matches("sha256:")
                .starts_with(prefix);
            (!identified).then_some(Kept::BytesDiffer)
        };
        let action = if let Some(why) = refusal {
            Action::Kept(why)
        } else if !self.remove {
            Action::WouldRemove
        } else {
            match remove_verified(directory, TAG, &path, name, &sha256, true)? {
                Removal::Removed => Action::Removed,
                Removal::Vanished => Action::Vanished,
                Removal::Changed => Action::Kept(Kept::Changed),
            }
        };
        let gone = matches!(action, Action::Removed | Action::Vanished);
        self.report.entries.push(Entry {
            area,
            path,
            vault_path: Some(vault_path),
            sha256: Some(sha256),
            bytes: Some(size),
            action,
        });
        Ok(gone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn areas_round_trip_through_their_names() {
        for area in Area::ALL {
            assert_eq!(Area::parse(area.as_str()), Some(area));
        }
        assert!(Area::Discarded.directory().ends_with("discarded"));
        assert!(Area::Proposed.directory().ends_with("proposed"));
        assert_eq!(Area::parse("all"), None);
    }

    #[test]
    fn only_sixteen_lowercase_hex_digits_name_a_set_aside_folder() {
        assert!(is_prefix("0123456789abcdef"));
        for name in [
            "0123456789ABCDEF",
            "0123456789abcde",
            "0123456789abcdef0",
            "gen_0123456789ab",
            "0123456789abcdeg",
        ] {
            assert!(!is_prefix(name), "{name}");
        }
    }
}
