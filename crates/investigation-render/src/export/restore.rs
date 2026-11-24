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
//! it is not. A non-empty PATH is refused, an interrupted one included; a
//! retry goes to a new or emptied PATH. The vault, when enabled, is rendered
//! afterwards by the caller, like any `vault render`.

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
use rustix::fs::{CWD, RenameFlags, renameat_with};

use super::verify::{VerifiedCopy, VerifyReport, verify_into};

/// The staging folder below PATH. Not a workspace path, so no member of an
/// archive can collide with it.
pub const STAGING: &str = ".restore-staging";
/// The workspace copy inside the staging folder.
const STAGED_WORKSPACE: &str = "workspace";
/// The top-level entry that holds the marker; published last.
const MARKER_ENTRY: &str = "state";

/// An exact restore or a fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Every top-level entry but `state/` is in PATH.
    Publishing,
    /// `state/` is in PATH; the empty staging folder remains.
    Published,
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
    /// Stopped at a [`Step`] by a test, with no cleanup.
    #[doc(hidden)]
    Interrupted(Step),
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
    let at = |step: Step| -> Result<(), Refusal> {
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
    for name in &entries {
        let (from, to) = (staged.join(name), root.join(name));
        renameat_with(CWD, &from, CWD, &to, RenameFlags::NOREPLACE)
            .map_err(|error| io("Moving", &to, error.into()))?;
    }
    sync_directory(root).map_err(|error| io("Syncing", root, error))?;
    stop(Step::Publishing)?;
    let (from, to) = (staged.join(MARKER_ENTRY), root.join(MARKER_ENTRY));
    renameat_with(CWD, &from, CWD, &to, RenameFlags::NOREPLACE)
        .map_err(|error| io("Moving", &to, error.into()))?;
    sync_directory(root).map_err(|error| io("Syncing", root, error))?;
    stop(Step::Published)?;
    // Only empty directories remain; a failure leaves an empty folder in an
    // otherwise complete workspace.
    if let Some(staging) = staged.parent() {
        let _ = fs::remove_dir(staged);
        let _ = fs::remove_dir(staging);
        let _ = sync_directory(root);
    }
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
                let interrupted = if root.join(STAGING).exists() {
                    format!(" It holds {STAGING}, an interrupted restore, and is not a workspace.")
                } else {
                    String::new()
                };
                return refuse(format!(
                    "{shown} is not empty.{interrupted} A restore writes only a new or empty directory; choose another PATH, or empty this one."
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
