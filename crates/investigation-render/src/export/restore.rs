//! `archive restore DIR PATH` and `archive fork DIR PATH`: a verified
//! workspace archive, published as a workspace at a new or empty PATH
//! (plan §10.4, §10.7; W11.18, ADR 0013).
//!
//! 1. PATH must be a new directory whose parent exists, or an empty one.
//!    It must not lie inside DIR. Nothing is ever written outside PATH.
//! 2. The archive is verified by [`verify_into`], the checks of
//!    `archive verify`, with the copy that `check` runs on restored into
//!    private staging below PATH (`PATH/.restore-staging/workspace`), on the
//!    destination filesystem. An archive with any problem is never
//!    restored: the staging is removed and PATH is left as it was found.
//! 3. A fork gives the staged copy a new workspace ID and a fork marker
//!    naming the origin workspace, the archive and every inherited case
//!    head (see `investigation_store::workspace::ForkMarker`).
//! 4. The disposable index is rebuilt from the journal, and `check` runs on
//!    the staged workspace. It must pass with every case head equal to the
//!    archive's, and with a current index. Cache, index and vault are never
//!    copied from the archive, which holds none of them.
//! 5. Every staged directory is synced. Publication then moves each
//!    top-level entry into PATH with a no-replace rename, and moves `state/`,
//!    which holds the workspace marker, last. Until that last rename PATH
//!    has no marker, so `check` and every other command refuse it as no
//!    workspace; after it PATH is the checked workspace. The empty staging
//!    folder is removed last.
//!
//! A restore killed at any point therefore leaves PATH either refused as
//! incomplete or complete and checked, never a workspace that passes for one
//! it is not. A non-empty PATH is refused, an interrupted one included. The
//! vault, when enabled, is rendered afterwards by the caller, like any
//! `vault render`.
//!
//! **Ownership and resume.** The staging folder holds `owner.json`, written
//! (staged, synced, renamed) before anything else, naming the archive's
//! `export_id` and the mode; before the first entry moves into PATH it also
//! holds `moved.json`, the list of entries publication moves. The restore
//! holds an exclusive lock on the staging folder while it runs.
//! [`resume_archive`] (`--resume`) clears an interrupted restore and runs it
//! again, and only when all of these hold; otherwise it refuses, naming
//! what it found, and removes nothing:
//!
//! - PATH holds `.restore-staging`, a directory no running restore locks;
//! - its `owner.json` names this archive's `export_id` and the same mode
//!   (an empty staging folder, or one holding only the unrenamed
//!   `owner.json.tmp`, is a restore killed before it wrote its marker);
//! - the staging folder holds nothing but the marker files and the staged
//!   `workspace`;
//! - every other entry of PATH is one `moved.json` lists.
//!
//! When `state/` was already published, the restore had finished but for
//! removing its empty staging folder: resume removes only that, after
//! checking that PATH is the workspace this archive restores, and reports
//! the workspace as already restored.
//!
//! **Test pause.** In a build with debug assertions, setting
//! `INVESTIGATION_TEST_PAUSE_RESTORE` to a [`Step`] name makes the process
//! create the file named by `INVESTIGATION_TEST_PAUSE_READY` at that step
//! and then wait until it is killed, so a test can kill a real process
//! there. A release build has no such code.

use std::{
    fs, io,
    os::unix::fs::DirBuilderExt,
    path::{Component, Path, PathBuf},
};

use investigation_core::{identity::Id, wire::Digest};
use investigation_store::{
    Store, Workspace,
    store::{CaseHead, CheckReport},
    workspace::{ForkMarker, WorkspaceError},
};
use rustix::fs::{
    CWD, FlockOperation, Mode as FileMode, OFlags, RenameFlags, flock, open, renameat_with,
};
use serde::{Deserialize, Serialize};

use super::{
    MANIFEST_FILE,
    verify::{VerifiedCopy, VerifyReport, archived_heads, read_manifest, read_small, verify_into},
};

/// The staging folder below PATH. Not a workspace path, so no member of an
/// archive can collide with it.
pub const STAGING: &str = ".restore-staging";
/// The workspace copy inside the staging folder.
const STAGED_WORKSPACE: &str = "workspace";
/// The top-level entry that holds the marker; published last.
const MARKER_ENTRY: &str = "state";
/// The ownership marker in the staging folder.
const OWNER: &str = "owner.json";
/// The entries publication moves into PATH, written before the first.
const MOVED: &str = "moved.json";
/// The suffix of a marker file before its rename.
const UNRENAMED: &str = ".tmp";
/// What an ownership marker says it is.
const OWNER_KIND: &str = "investigation.restore-staging";
/// Environment variables of the test pause; see the module documentation.
pub const PAUSE_AT: &str = "INVESTIGATION_TEST_PAUSE_RESTORE";
pub const PAUSE_READY: &str = "INVESTIGATION_TEST_PAUSE_READY";

/// `owner.json`: which archive and mode a staging folder belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    kind: String,
    /// The archive's `export_id` as its manifest gave it; absent when the
    /// manifest could not be read, and then the staging cannot be resumed.
    export_id: Option<String>,
    mode: Mode,
}

/// `moved.json`: the top-level entries publication moves, `state/` last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Moved {
    entries: Vec<String>,
}

/// An exact restore or a fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The same workspace: its ID, journals, heads and receipts. For
    /// recovery, in place of the original, never beside it.
    Exact,
    /// A new workspace ID with a fork marker; journals, heads, record and
    /// command IDs kept. For working beside the original.
    Fork,
}

/// Points a test can stop a restore at, as a kill there would: nothing
/// after the point runs, cleanup included.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The staging folder exists and holds nothing yet.
    Staging,
    /// The verified copy is in staging, before any change to it.
    Verified,
    /// A fork's copy has its new identity.
    Forked,
    /// The index is rebuilt and `check` passed in staging.
    Checked,
    /// The first top-level entry is in PATH.
    Moving,
    /// Every top-level entry but `state/` is in PATH.
    Publishing,
    /// `state/` is in PATH; the empty staging folder remains.
    Published,
    /// A resume removed the entries an interrupted restore moved into PATH;
    /// its staging folder remains.
    Cleared,
}

impl Step {
    /// The pause a test asked for, in a build with debug assertions: mark
    /// the ready file and wait to be killed.
    fn pause_for_test(self) {
        #[cfg(debug_assertions)]
        if std::env::var(PAUSE_AT).is_ok_and(|at| at == format!("{self:?}")) {
            if let Ok(ready) = std::env::var(PAUSE_READY) {
                let _ = fs::write(ready, format!("{self:?}"));
            }
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }
}

/// A published restore.
#[derive(Debug, Clone)]
pub struct Restored {
    pub mode: Mode,
    /// PATH, canonical.
    pub root: PathBuf,
    pub workspace_id: Id,
    /// The workspace the archive was made from; equal to `workspace_id`
    /// for an exact restore.
    pub origin_workspace_id: Id,
    pub export_id: String,
    pub recovery_head: Digest,
    /// The fork marker, for a fork.
    pub fork: Option<ForkMarker>,
    /// The `check` that passed in staging before publication.
    pub check: CheckReport,
    pub human_notes: usize,
    pub vault_enabled: bool,
    /// The verification of the archive, as `archive verify` reports it.
    pub verification: VerifyReport,
}

/// Why nothing was published.
#[derive(Debug)]
pub enum Refusal {
    /// PATH is not a new or empty directory with an existing parent, lies
    /// inside DIR, or is on an unsupported filesystem. Nothing was written.
    Destination(String),
    /// DIR cannot be read as an export directory. Nothing was written.
    Unreadable(io::Error),
    /// The archive failed verification. PATH is as it was found.
    Unverified(Box<VerifyReport>),
    /// The verified copy failed a later step. PATH is as it was found.
    Failed(String),
    /// `--resume` found something it does not own or cannot clear. Nothing
    /// was removed.
    NotResumable(String),
    /// Another restore holds PATH's staging folder. Nothing was removed.
    Busy(String),
    /// Stopped at a [`Step`] by a test, with no cleanup.
    #[doc(hidden)]
    Interrupted(Step),
}

/// The result of [`resume_archive`].
#[derive(Debug)]
pub enum Resumed {
    /// Restored, after clearing any interrupted restore.
    Restored(Box<Restored>),
    /// The interrupted restore had published; only its empty staging folder
    /// was removed.
    Completed(Completed),
}

/// A restore found already published by [`resume_archive`].
#[derive(Debug, Clone)]
pub struct Completed {
    pub mode: Mode,
    pub root: PathBuf,
    pub workspace_id: Id,
    pub export_id: String,
    pub check: CheckReport,
}

/// Restore or fork the archive in `dir` into `path`.
pub fn restore_archive(dir: &Path, path: &Path, mode: Mode) -> Result<Restored, Refusal> {
    restore_archive_until(dir, path, mode, None)
}

/// [`restore_archive`], stopped at `stop` as a kill there would stop it.
#[doc(hidden)]
pub fn restore_archive_until(
    dir: &Path,
    path: &Path,
    mode: Mode,
    stop: Option<Step>,
) -> Result<Restored, Refusal> {
    let destination = Destination::prepare(dir, path)?;
    let staging = destination.root.join(STAGING);
    if let Err(error) = fs::DirBuilder::new().mode(0o700).create(&staging) {
        destination.abandon(None);
        return Err(Refusal::Failed(format!(
            "The staging folder could not be made in PATH ({error})."
        )));
    }
    let owner = Owner {
        kind: OWNER_KIND.to_owned(),
        export_id: read_manifest(&dir.join(MANIFEST_FILE))
            .ok()
            .map(|manifest| manifest.export_id),
        mode,
    };
    let _held = match lock(&staging) {
        Ok(Some(held)) => held,
        Ok(None) => {
            return Err(Refusal::Busy(format!(
                "Another restore holds {}.",
                staging.display()
            )));
        }
        Err(error) => {
            destination.abandon(Some(&staging));
            return Err(Refusal::Failed(format!(
                "The staging folder could not be locked ({error})."
            )));
        }
    };
    if let Err(error) = write_marker(&staging, OWNER, &owner) {
        destination.abandon(Some(&staging));
        return Err(Refusal::Failed(format!(
            "The ownership marker could not be written in the staging folder ({error})."
        )));
    }
    let at = |step: Step| -> Result<(), Refusal> {
        step.pause_for_test();
        if stop == Some(step) {
            return Err(Refusal::Interrupted(step));
        }
        Ok(())
    };
    at(Step::Staging)?;
    let staged = staging.join(STAGED_WORKSPACE);
    let verified = match verify_into(dir, &staged) {
        Ok(Ok(verified)) => verified,
        Ok(Err(report)) => {
            destination.abandon(Some(&staging));
            return Err(Refusal::Unverified(Box::new(report)));
        }
        Err(error) => {
            destination.abandon(Some(&staging));
            return Err(Refusal::Unreadable(error));
        }
    };
    at(Step::Verified)?;
    let prepared = match prepare(&staged, &verified, mode, &at) {
        Ok(prepared) => prepared,
        Err(Refusal::Failed(reason)) => {
            destination.abandon(Some(&staging));
            return Err(Refusal::Failed(reason));
        }
        Err(other) => return Err(other),
    };
    at(Step::Checked)?;
    if let Err(error) = publish(&staged, &destination.root, &at) {
        return Err(match error {
            Published::Interrupted(step) => Refusal::Interrupted(step),
            // Entries already moved stay where they are. Before the marker
            // moved, PATH is refused as incomplete; a sync that failed after
            // it leaves durability unknown (§10.2), so check decides.
            Published::Io(reason) => Refusal::Failed(format!(
                "{reason} PATH may be incomplete; if check refuses it, remove it and restore again."
            )),
        });
    }
    let workspace = Workspace::open(&destination.root).map_err(|error| {
        Refusal::Failed(format!(
            "The published workspace does not open ({error}); remove PATH and restore again."
        ))
    })?;
    Ok(Restored {
        mode,
        root: workspace.root().to_path_buf(),
        workspace_id: workspace.id(),
        origin_workspace_id: verified.index.workspace_id,
        export_id: verified.manifest.export_id.clone(),
        recovery_head: verified.index.recovery_head.clone(),
        fork: workspace.fork().cloned(),
        check: prepared,
        human_notes: verified.index.human_notes.len(),
        vault_enabled: verified.index.vault_enabled,
        verification: verified.report,
    })
}

/// Clear an interrupted restore of the archive in `dir` at `path`, then
/// restore it there, as [`restore_archive`] does; see the module
/// documentation for what is cleared and what is refused. A new or empty
/// PATH is restored without clearing anything.
pub fn resume_archive(dir: &Path, path: &Path, mode: Mode) -> Result<Resumed, Refusal> {
    resume_archive_until(dir, path, mode, None)
}

/// [`resume_archive`], stopped at `stop` as a kill there would stop it.
#[doc(hidden)]
pub fn resume_archive_until(
    dir: &Path,
    path: &Path,
    mode: Mode,
    stop: Option<Step>,
) -> Result<Resumed, Refusal> {
    let refuse = |reason: String| Err(Refusal::NotResumable(reason));
    let shown = path.display();
    let root = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => path.to_path_buf(),
        Ok(_) => return refuse(format!("{shown} is not a directory; nothing was removed.")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return restore_archive_until(dir, path, mode, stop)
                .map(|restored| Resumed::Restored(Box::new(restored)));
        }
        Err(error) => return refuse(format!("{shown} cannot be read ({error}).")),
    };
    let names = |directory: &Path| -> Result<Vec<String>, Refusal> {
        let mut names = fs::read_dir(directory)
            .and_then(|entries| {
                entries
                    .map(|entry| {
                        entry.map(|entry| entry.file_name().to_string_lossy().into_owned())
                    })
                    .collect::<io::Result<Vec<_>>>()
            })
            .map_err(|error| {
                Refusal::NotResumable(format!("{} cannot be read ({error}).", directory.display()))
            })?;
        names.sort();
        Ok(names)
    };
    let mut entries = names(&root)?;
    if entries.is_empty() {
        return restore_archive_until(dir, path, mode, stop)
            .map(|restored| Resumed::Restored(Box::new(restored)));
    }
    let staging = root.join(STAGING);
    if !entries.iter().any(|name| name == STAGING) {
        return refuse(format!(
            "{shown} is not empty and holds no {STAGING}, so it is no interrupted restore; --resume clears only a restore's own staging. Nothing was removed; choose another PATH."
        ));
    }
    if !fs::symlink_metadata(&staging).is_ok_and(|metadata| metadata.is_dir()) {
        return refuse(format!(
            "{} is not a directory, so no restore made it. Nothing was removed.",
            staging.display()
        ));
    }
    let Some(_held) = lock(&staging).map_err(|error| {
        Refusal::NotResumable(format!("{} cannot be locked ({error}).", staging.display()))
    })?
    else {
        return Err(Refusal::Busy(format!(
            "A restore is still running into {shown}: it holds {STAGING}. Nothing was removed; wait for it to finish."
        )));
    };
    entries.retain(|name| name != STAGING);
    let staged = names(&staging)?;
    let unrenamed = format!("{OWNER}{UNRENAMED}");
    let owner: Option<Owner> = if staged.iter().any(|name| name == OWNER) {
        let owner = read_small(&staging.join(OWNER))
            .and_then(|bytes| {
                serde_json::from_slice::<Owner>(&bytes).map_err(|error| error.to_string())
            })
            .ok()
            .filter(|owner| owner.kind == OWNER_KIND);
        if owner.is_none() {
            return refuse(format!(
                "{} is not an ownership marker this program wrote. Nothing was removed; inspect {STAGING} and remove it yourself.",
                staging.join(OWNER).display()
            ));
        }
        owner
    } else {
        None
    };
    let Some(owner) = owner else {
        // A restore killed between making the folder and renaming its
        // marker leaves the folder empty or holding only the staged marker.
        if entries.is_empty() && staged.iter().all(|name| *name == unrenamed) {
            remove_marker_files(&staging, &staged)?;
            remove_staging_folder(&root, &staging)?;
            drop(_held);
            return restore_archive_until(dir, path, mode, stop)
                .map(|restored| Resumed::Restored(Box::new(restored)));
        }
        return refuse(format!(
            "{} holds no ownership marker, so this program cannot tell it made it. Nothing was removed; inspect {STAGING} and remove it yourself, or choose another PATH.",
            staging.display()
        ));
    };
    let archive_id = read_manifest(&dir.join(MANIFEST_FILE))
        .map(|manifest| manifest.export_id)
        .map_err(|reason| {
            Refusal::NotResumable(format!(
                "{} cannot be read as an export ({reason}), so it cannot be matched to the interrupted restore. Nothing was removed.",
                dir.display()
            ))
        })?;
    if owner.export_id.as_deref() != Some(archive_id.as_str()) {
        return refuse(format!(
            "The interrupted restore in {shown} is of archive {}, not of {} ({archive_id}). Nothing was removed; resume it with the archive it names, or choose another PATH.",
            owner.export_id.as_deref().unwrap_or("(unknown)"),
            dir.display()
        ));
    }
    if owner.mode != mode {
        return refuse(format!(
            "The interrupted run in {shown} was an archive {}, not an archive {}. Nothing was removed; resume it with the same command.",
            verb(owner.mode),
            verb(mode)
        ));
    }
    let allowed = [OWNER, MOVED, STAGED_WORKSPACE];
    let foreign: Vec<&String> = staged
        .iter()
        .filter(|name| {
            !allowed.contains(&name.as_str())
                && !allowed
                    .iter()
                    .any(|allowed| **name == format!("{allowed}{UNRENAMED}"))
        })
        .collect();
    if !foreign.is_empty() {
        return refuse(format!(
            "{} holds {}, which a restore does not write. Nothing was removed.",
            staging.display(),
            join(&foreign)
        ));
    }
    let moved: Vec<String> = if staged.iter().any(|name| name == MOVED) {
        read_small(&staging.join(MOVED))
            .and_then(|bytes| {
                serde_json::from_slice::<Moved>(&bytes).map_err(|error| error.to_string())
            })
            .map_err(|reason| {
                Refusal::NotResumable(format!(
                    "{} cannot be read ({reason}). Nothing was removed.",
                    staging.join(MOVED).display()
                ))
            })?
            .entries
    } else {
        Vec::new()
    };
    let unknown: Vec<&String> = entries
        .iter()
        .filter(|name| !moved.contains(name))
        .collect();
    if !unknown.is_empty() {
        return refuse(format!(
            "{shown} holds {}, which the interrupted restore did not write. Nothing was removed; move those out of PATH, or choose another PATH.",
            join(&unknown)
        ));
    }
    if entries.iter().any(|name| name == MARKER_ENTRY) {
        return complete(dir, &root, &staging, &staged, owner, archive_id);
    }
    // Nothing in PATH is a workspace yet: remove what the restore moved,
    // then its staging, the marker last, so a resume killed part way can
    // run again.
    for name in &entries {
        remove_entry(&root.join(name))?;
    }
    sync_directory(&root).map_err(|error| failed_clear(&root, error))?;
    Step::Cleared.pause_for_test();
    if stop == Some(Step::Cleared) {
        return Err(Refusal::Interrupted(Step::Cleared));
    }
    if staged.iter().any(|name| name == STAGED_WORKSPACE) {
        remove_entry(&staging.join(STAGED_WORKSPACE))?;
    }
    remove_marker_files(&staging, &staged)?;
    remove_staging_folder(&root, &staging)?;
    drop(_held);
    restore_archive_until(dir, path, mode, stop)
        .map(|restored| Resumed::Restored(Box::new(restored)))
}

/// The interrupted restore had published `state/`: check that PATH is the
/// workspace this archive restores, then remove only the staging folder,
/// which holds nothing but its markers and an empty `workspace`.
fn complete(
    dir: &Path,
    root: &Path,
    staging: &Path,
    staged: &[String],
    owner: Owner,
    export_id: String,
) -> Result<Resumed, Refusal> {
    let refuse = |reason: String| Err(Refusal::NotResumable(reason));
    let workspace = match Workspace::open(root) {
        Ok(workspace) => workspace,
        Err(error) => {
            return refuse(format!(
                "{} holds state/, but it does not open as a workspace ({error}). Nothing was removed.",
                root.display()
            ));
        }
    };
    let archived = archived_heads(dir).map_err(|reason| {
        Refusal::NotResumable(format!(
            "{} cannot be read ({reason}). Nothing was removed.",
            dir.display()
        ))
    })?;
    let same = match owner.mode {
        Mode::Exact => workspace.id() == archived.workspace_id && workspace.fork().is_none(),
        Mode::Fork => workspace
            .fork()
            .is_some_and(|fork| fork.origin_export_id == export_id),
    };
    if !same {
        return refuse(format!(
            "{} is a workspace, but not the one this archive {}s. Nothing was removed.",
            root.display(),
            verb(owner.mode)
        ));
    }
    let workspace_dir = staging.join(STAGED_WORKSPACE);
    if staged.iter().any(|name| name == STAGED_WORKSPACE) {
        fs::remove_dir(&workspace_dir).map_err(|error| {
            Refusal::NotResumable(format!(
                "{} is not empty ({error}), though the restore published. Nothing was removed.",
                workspace_dir.display()
            ))
        })?;
    }
    remove_marker_files(staging, staged)?;
    remove_staging_folder(root, staging)?;
    let mut store = Store::open(workspace);
    let check = store.check().map_err(|failure| {
        Refusal::Failed(format!("check cannot run: {}", failure.error.message))
    })?;
    if !check.integrity_ok() {
        return Err(Refusal::Failed(
            "the published workspace does not pass check; run check for the findings".to_owned(),
        ));
    }
    Ok(Resumed::Completed(Completed {
        mode: owner.mode,
        root: store.workspace().root().to_path_buf(),
        workspace_id: store.workspace().id(),
        export_id,
        check,
    }))
}

fn verb(mode: Mode) -> &'static str {
    match mode {
        Mode::Exact => "restore",
        Mode::Fork => "fork",
    }
}

fn join(names: &[&String]) -> String {
    names
        .iter()
        .map(|name| name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn failed_clear(path: &Path, error: io::Error) -> Refusal {
    Refusal::Failed(format!(
        "Clearing {} failed ({error}); run the same command with --resume again.",
        path.display()
    ))
}

/// Remove one entry a restore wrote, never following a link.
fn remove_entry(path: &Path) -> Result<(), Refusal> {
    let result = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    };
    result.map_err(|error| failed_clear(path, error))
}

/// Remove the marker files among `staged`, the ownership marker last.
fn remove_marker_files(staging: &Path, staged: &[String]) -> Result<(), Refusal> {
    let mut markers: Vec<&String> = staged
        .iter()
        .filter(|name| name.as_str() != STAGED_WORKSPACE)
        .collect();
    markers.sort_by_key(|name| name.as_str() == OWNER);
    for name in markers {
        let path = staging.join(name);
        fs::remove_file(&path).map_err(|error| failed_clear(&path, error))?;
    }
    Ok(())
}

fn remove_staging_folder(root: &Path, staging: &Path) -> Result<(), Refusal> {
    fs::remove_dir(staging).map_err(|error| failed_clear(staging, error))?;
    sync_directory(root).map_err(|error| failed_clear(root, error))
}

/// Lock the staging folder for this process, or `None` when another holds
/// it. The lock ends with the returned descriptor, or when the process dies.
fn lock(staging: &Path) -> io::Result<Option<rustix::fd::OwnedFd>> {
    let directory = open(
        staging,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        FileMode::empty(),
    )?;
    match flock(&directory, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(directory)),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Write a marker file into the staging folder: staged under a temporary
/// name, synced, renamed, and the folder synced.
fn write_marker<T: Serialize>(staging: &Path, name: &str, value: &T) -> io::Result<()> {
    use std::io::Write as _;
    let staged = staging.join(format!("{name}{UNRENAMED}"));
    let _ = fs::remove_file(&staged);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)?;
    file.write_all(&serde_json::to_vec(value).map_err(io::Error::other)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&staged, staging.join(name))?;
    sync_directory(staging)
}

/// Give a fork its identity, rebuild the index and check the staged copy.
fn prepare(
    staged: &Path,
    verified: &VerifiedCopy,
    mode: Mode,
    at: &dyn Fn(Step) -> Result<(), Refusal>,
) -> Result<CheckReport, Refusal> {
    let failed = |what: &str, detail: String| Refusal::Failed(format!("{what}: {detail}"));
    let opened = |result: Result<Workspace, WorkspaceError>| {
        result.map_err(|error| failed("The verified copy does not open", error.to_string()))
    };
    let mut workspace = opened(Workspace::open(staged))?;
    if mode == Mode::Fork {
        let cases = Store::open(workspace)
            .fork_heads()
            .map_err(|failure| failed("The fork heads cannot be read", failure.error.message))?;
        let fork = ForkMarker {
            origin_workspace_id: verified.index.workspace_id,
            origin_recovery_head: verified.index.recovery_head.clone(),
            origin_export_id: verified.manifest.export_id.clone(),
            cases,
        };
        workspace = opened(Workspace::give_fork_identity(staged, fork))?;
        at(Step::Forked)?;
    }
    let mut store = Store::open(workspace);
    store
        .repair_index()
        .map_err(|failure| failed("The index cannot be rebuilt", failure.error.message))?;
    let check = store
        .check()
        .map_err(|failure| failed("check cannot run", failure.error.message))?;
    if !check.integrity_ok() {
        let codes: Vec<&str> = check
            .issues
            .iter()
            .map(|issue| issue.code.as_str())
            .collect();
        return Err(failed(
            "check failed on the staged workspace",
            codes.join(", "),
        ));
    }
    let archived: Vec<(Id, u64, &Digest)> = verified
        .index
        .cases
        .iter()
        .map(|case| (case.case_id, case.sequence, &case.head_hash))
        .collect();
    if archived != heads(&check.heads) {
        return Err(failed(
            "check found other case heads",
            "the staged workspace verifies to other heads than archive.json names".to_owned(),
        ));
    }
    Ok(check)
}

fn heads(heads: &[CaseHead]) -> Vec<(Id, u64, &Digest)> {
    heads
        .iter()
        .map(|head| {
            (
                head.case_id,
                head.snapshot.sequence,
                &head.snapshot.head_hash,
            )
        })
        .collect()
}

enum Published {
    Interrupted(Step),
    Io(String),
}

/// Sync the staged copy, then move it into `root`, `state/` last.
fn publish(
    staged: &Path,
    root: &Path,
    at: &dyn Fn(Step) -> Result<(), Refusal>,
) -> Result<(), Published> {
    let io = |what: &str, path: &Path, error: io::Error| {
        Published::Io(format!("{what} {} failed ({error}).", path.display()))
    };
    let stop = |step| {
        at(step).map_err(|refusal| match refusal {
            Refusal::Interrupted(step) => Published::Interrupted(step),
            _ => unreachable!("a step only interrupts"),
        })
    };
    sync_tree(staged).map_err(|(path, error)| io("Syncing", &path, error))?;
    let mut entries: Vec<_> = fs::read_dir(staged)
        .and_then(|entries| {
            entries
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<io::Result<Vec<_>>>()
        })
        .map_err(|error| io("Listing", staged, error))?;
    entries.sort();
    entries.retain(|name| name != MARKER_ENTRY);
    let staging = staged
        .parent()
        .expect("the staged copy is in the staging folder");
    let mut listed: Vec<String> = entries
        .iter()
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    listed.push(MARKER_ENTRY.to_owned());
    write_marker(staging, MOVED, &Moved { entries: listed })
        .map_err(|error| io("Writing", &staging.join(MOVED), error))?;
    for (position, name) in entries.iter().enumerate() {
        let (from, to) = (staged.join(name), root.join(name));
        renameat_with(CWD, &from, CWD, &to, RenameFlags::NOREPLACE)
            .map_err(|error| io("Moving", &to, error.into()))?;
        if position == 0 {
            stop(Step::Moving)?;
        }
    }
    sync_directory(root).map_err(|error| io("Syncing", root, error))?;
    stop(Step::Publishing)?;
    let (from, to) = (staged.join(MARKER_ENTRY), root.join(MARKER_ENTRY));
    renameat_with(CWD, &from, CWD, &to, RenameFlags::NOREPLACE)
        .map_err(|error| io("Moving", &to, error.into()))?;
    sync_directory(root).map_err(|error| io("Syncing", root, error))?;
    stop(Step::Published)?;
    // Only empty directories and the markers remain; a failure leaves them
    // in an otherwise complete workspace, which a resume removes.
    let _ = fs::remove_dir(staged);
    let _ = fs::remove_file(staging.join(MOVED));
    let _ = fs::remove_file(staging.join(OWNER));
    let _ = fs::remove_dir(staging);
    let _ = sync_directory(root);
    Ok(())
}

/// Sync every directory below and including `root`, deepest first, so
/// every restored name is durable before publication.
fn sync_tree(root: &Path) -> Result<(), (PathBuf, io::Error)> {
    for entry in fs::read_dir(root).map_err(|error| (root.to_path_buf(), error))? {
        let entry = entry.map_err(|error| (root.to_path_buf(), error))?;
        let file_type = entry.file_type().map_err(|error| (entry.path(), error))?;
        if file_type.is_dir() {
            sync_tree(&entry.path())?;
        }
    }
    sync_directory(root).map_err(|error| (root.to_path_buf(), error))
}

fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

/// PATH, checked and created when it did not exist.
struct Destination {
    root: PathBuf,
    /// Whether this restore created PATH, so abandoning removes it.
    created: bool,
}

impl Destination {
    fn prepare(dir: &Path, path: &Path) -> Result<Self, Refusal> {
        let refuse = |reason: String| Err(Refusal::Destination(reason));
        let shown = path.display();
        let absolute = absolute(path).map_err(|error| {
            Refusal::Destination(format!("{shown} cannot be resolved ({error})."))
        })?;
        let Some(name) = absolute.file_name().map(ToOwned::to_owned) else {
            return refuse(format!("{shown} names no directory."));
        };
        let parent = absolute.parent().unwrap_or(Path::new("/"));
        let parent = match fs::canonicalize(parent) {
            Ok(parent) if parent.is_dir() => parent,
            _ => {
                return refuse(format!(
                    "The parent of {shown} does not exist. A restore writes nothing outside PATH; create the parent first."
                ));
            }
        };
        let root = parent.join(name);
        let existed = match fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.is_dir() => true,
            Ok(metadata) if metadata.is_symlink() => {
                return refuse(format!("{shown} is a symbolic link; name a directory."));
            }
            Ok(_) => return refuse(format!("{shown} is a file, not a directory.")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return refuse(format!("{shown} cannot be read ({error}).")),
        };
        if existed {
            let mut entries = fs::read_dir(&root).map_err(|error| {
                Refusal::Destination(format!("{shown} cannot be read ({error})."))
            })?;
            if entries.next().is_some() {
                if root.join(STAGING).exists() {
                    return refuse(format!(
                        "{shown} is not empty: it holds {STAGING}, an interrupted restore. Run the same command with --resume to clear what that restore wrote and restore again, or choose another PATH."
                    ));
                }
                return refuse(format!(
                    "{shown} is not empty. A restore writes only a new or empty directory; choose another PATH, or empty this one."
                ));
            }
        }
        if let Ok(archive) = fs::canonicalize(dir)
            && root.starts_with(&archive)
        {
            return refuse(format!(
                "{shown} is inside the archive; restore somewhere else."
            ));
        }
        Workspace::check_filesystem(&parent)
            .map_err(|error| Refusal::Destination(format!("{shown}: {error}.")))?;
        if !existed {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .map_err(|error| {
                    Refusal::Destination(format!("{shown} cannot be made ({error})."))
                })?;
            let _ = sync_directory(&parent);
        }
        Ok(Self {
            root,
            created: !existed,
        })
    }

    /// Leave PATH as it was found: remove the staging this restore made,
    /// and PATH if this restore made it.
    fn abandon(&self, staging: Option<&Path>) {
        if let Some(staging) = staging {
            let _ = fs::remove_dir_all(staging);
        }
        if self.created {
            let _ = fs::remove_dir(&self.root);
        }
    }
}

/// `path` made absolute against the working directory, `.` and `..`
/// resolved lexically.
fn absolute(path: &Path) -> io::Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other),
        }
    }
    Ok(result)
}
