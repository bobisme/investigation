//! Ownership-aware publication of a [`RenderPlan`] into `vault/`, or into
//! the managed prefix of an existing vault ([`crate::projection`]).
//!
//! Every write goes through one path:
//!
//! 1. Refuse entries in a human-owned area ([`layout::VAULT_AREAS`]: `Notes/`,
//!    `Boards/`, `Inbox/`, hidden folders), and entries under `Attachments/`
//!    that are not `Attachments/<artifact-id>.<ext>` approved by
//!    [`Ownership::approves_attachment`]. These are hard checks made before
//!    anything is inspected, whatever the ownership rules answer.
//! 2. Walk the parent directories from an open vault directory handle with
//!    `O_NOFOLLOW`; a symbolic link or non-directory on the way refuses the
//!    entry, so nothing is ever written outside the vault.
//! 3. Inspect the current file without following links and hash its bytes.
//! 4. Ask [`Ownership`] for a [`Decision`].
//! 5. On [`Decision::Write`], write a private temporary file in the same
//!    directory. Once its batch is staged and durable, inspect the path and
//!    decide again, then rename the temporary into place. A path that was
//!    absent at inspection is created with a no-replace rename, so a file
//!    that appears meanwhile is refused rather than overwritten.
//!
//! ## Durability in batches
//!
//! [`Publisher::publish`] does not sync each file and its directory, which
//! cost two `fsync` calls per note and nearly all of a full render's time.
//! It stages up to [`STAGE_BATCH`] plan entries as temporaries, makes them
//! durable with one barrier ([`Publisher::barrier`]: `syncfs` of the
//! projection's filesystem), and only then renames them into place. So a
//! crash at any moment leaves every path with either its earlier bytes or
//! the complete planned bytes, never a torn file, as per-file syncs did.
//! Each barrier also makes the renames and removals before it durable, and
//! a last barrier follows the final rename or removal. An outcome counts as
//! durable once a barrier has followed it; only durable outcomes are handed
//! back when the publication fails ([`Publisher::publish_durably`]), so the
//! manifest never records a write or a removal that a crash could still
//! undo. Paths written but not yet durable keep their write-ahead intent
//! ([`crate::Manifest::begin`]). Where `syncfs` does not exist, every file
//! and directory is synced as it is written, and the barrier does nothing.
//!
//! Removal goes through the same inspect-then-decide step. [`Ownership::retired`]
//! names the paths it proved generated that the plan no longer contains, and
//! [`Ownership::decide_removal`] must answer [`Decision::Remove`] for the bytes
//! found. The file is then renamed aside, hashed again, and unlinked only if
//! the bytes are still the ones approved; otherwise it is renamed back. Emptied
//! parent folders below the top level are removed with a single non-recursive
//! `rmdir`, which the kernel refuses while anything, foreign or hidden, remains.
//! No code path here deletes recursively.
//!
//! An advisory lock and an atomic rename do not coordinate an editor's unsaved
//! buffer or a sync client writing between the inspection and the rename. Drift
//! detection preserves the edits it observes; it cannot guarantee more.

use std::{
    collections::BTreeSet,
    ffi::{CStr, CString},
    fs::File,
    io::{Read, Write},
    marker::PhantomData,
    os::fd::{AsFd, OwnedFd},
};

use investigation_core::wire::Digest;
use investigation_store::{Workspace, journal::hash_bytes};
use rustix::{
    fs::{
        AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, fstat, fsync, mkdirat, openat, renameat,
        renameat_with, statat, unlinkat,
    },
    io::Errno,
};

use crate::{
    Generation, PlanEntry, PlanPath, RenderError, RenderLock, RenderPlan,
    fingerprint::{self, Fingerprint},
    layout,
    lock::Probe,
};

const DIRECTORY: OFlags = OFlags::DIRECTORY
    .union(OFlags::RDONLY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
pub(crate) const TEMP_ATTEMPTS: usize = 8;

/// Plan entries staged as temporaries before one durability barrier. A
/// batch holds no open descriptors; its cost is temporaries on disk.
pub const STAGE_BATCH: usize = 2048;

/// Whether a filesystem-wide barrier (`syncfs`) replaces per-file syncs.
/// Elsewhere each file and directory is synced as it changes.
pub(crate) const BATCHED: bool = cfg!(any(target_os = "linux", target_os = "android"));

/// Called by [`Publisher::visit`] with a vault-relative path, its directory,
/// its name and its type.
pub(crate) type VisitFile<'a> =
    dyn FnMut(&str, &OwnedFd, &CStr, FileType) -> Result<(), RenderError> + 'a;

/// What is at a plan path before publication. Links are never followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnDisk {
    Absent,
    Regular {
        sha256: Digest,
    },
    /// A symbolic link, directory, device or other non-regular file.
    Other,
}

/// Why an entry was left untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A file exists that the ownership rules do not prove generated by us.
    NotProvenOwned,
    /// An owned file's bytes differ from the last generated hash: someone
    /// edited it. It stays as found until the drift is reconciled.
    Drifted,
    /// The path holds a symbolic link, directory or other non-regular file.
    NotARegularFile,
    /// A parent component is a symbolic link or not a directory.
    UnsafeParent,
    /// The path lies in a human-owned area.
    HumanOwned,
    /// An `Attachments/` entry that is not an approved derivative preview.
    UnlistedAttachment,
}

/// An [`Ownership`] answer for one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Create or replace the file with the plan's content.
    Write,
    /// Leave the file; valid only when its bytes already equal the plan's.
    Keep,
    /// Delete a retired file; valid only from [`Ownership::decide_removal`]
    /// for a regular file whose bytes are proven generated.
    Remove,
    Refuse(Refusal),
}

/// Decides, per path, whether the publisher may write or delete. The path
/// ownership manifest ([`crate::Manifest`]) implements this from the last
/// generated hash of each owned path.
pub trait Ownership {
    /// Whether the plan entry may be written over what is at its path.
    fn decide(&self, entry: &PlanEntry, on_disk: &OnDisk) -> Decision;

    /// Owned paths the plan no longer contains, in any order. They are
    /// computed from the ownership record and the plan, never from a
    /// directory listing. The default owns nothing, so retires nothing.
    fn retired(&self, _plan: &RenderPlan) -> Vec<PlanPath> {
        Vec::new()
    }

    /// Whether a retired path may be deleted given what is there. Only
    /// [`Decision::Remove`] deletes. The default refuses.
    fn decide_removal(&self, _path: &PlanPath, _on_disk: &OnDisk) -> Decision {
        Decision::Refuse(Refusal::NotProvenOwned)
    }

    /// Whether an `Attachments/<artifact-id>.<ext>` entry is an approved
    /// derivative preview that may be written. Asked before [`Self::decide`]
    /// for every attachment entry; nothing else in that folder is ever
    /// written. The default approves nothing. Removing an attachment the
    /// ownership record proves generated needs no approval.
    fn approves_attachment(&self, _entry: &PlanEntry) -> bool {
        false
    }

    /// The bytes recorded as last generated at `path`, with the fingerprint
    /// of the file they were last observed in, when one was recorded
    /// ([`crate::fingerprint`]). A file found with that fingerprint is taken
    /// to hold those bytes without being read. The default records nothing.
    fn recorded(&self, _path: &PlanPath) -> Option<(&Digest, Option<Fingerprint>)> {
        None
    }
}

/// Conservative rules for a vault with no ownership record: create absent
/// files, keep files whose bytes already match, refuse everything else.
/// Nothing that existed before is ever overwritten.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoManifest;

impl Ownership for NoManifest {
    fn decide(&self, entry: &PlanEntry, on_disk: &OnDisk) -> Decision {
        match on_disk {
            OnDisk::Absent => Decision::Write,
            OnDisk::Regular { sha256 } if sha256 == entry.sha256() => Decision::Keep,
            OnDisk::Regular { .. } => Decision::Refuse(Refusal::NotProvenOwned),
            OnDisk::Other => Decision::Refuse(Refusal::NotARegularFile),
        }
    }
}

/// What the publisher did with one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Written,
    /// A planned file already held the plan's bytes, or a retired path was
    /// already empty.
    Unchanged,
    /// A retired file was deleted.
    Removed,
    Refused(Refusal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub path: PlanPath,
    /// What was found before acting; `None` when the path was not inspected.
    pub previous: Option<OnDisk>,
    pub action: Action,
    /// For a planned file found [`Action::Unchanged`], the fingerprint that
    /// proves those bytes at the next inspection, when it may be recorded
    /// ([`crate::fingerprint`]).
    pub stat: Option<Fingerprint>,
}

/// Passed to the observer after each step, in publish order: every plan
/// entry first, then every retired path.
#[derive(Debug)]
pub struct PublishStep<'a> {
    pub index: usize,
    pub total: usize,
    /// The plan entry, or `None` for a retired path.
    pub entry: Option<&'a PlanEntry>,
    pub outcome: &'a Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishReport {
    pub generation: Generation,
    /// One outcome per plan entry in plan order, then one per retired path.
    pub outcomes: Vec<Outcome>,
}

impl PublishReport {
    fn count(&self, matches: impl Fn(&Action) -> bool) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| matches(&outcome.action))
            .count()
    }

    pub fn written(&self) -> usize {
        self.count(|action| *action == Action::Written)
    }

    pub fn unchanged(&self) -> usize {
        self.count(|action| *action == Action::Unchanged)
    }

    pub fn removed(&self) -> usize {
        self.count(|action| *action == Action::Removed)
    }

    pub fn refused(&self) -> usize {
        self.count(|action| matches!(action, Action::Refused(_)))
    }
}

/// A publication stopped by an I/O failure ([`Publisher::publish_durably`]).
#[derive(Debug)]
pub struct Interrupted {
    pub error: RenderError,
    /// The outcomes reached and known durable, in publish order: a prefix
    /// of what the observer saw.
    pub durable: Vec<Outcome>,
}

impl Interrupted {
    fn new(error: RenderError, mut outcomes: Vec<Outcome>, durable: usize) -> Self {
        outcomes.truncate(durable);
        Self {
            error,
            durable: outcomes,
        }
    }
}

/// One plan entry of a batch, before its commit.
enum Staged {
    /// Decided without writing: refused, or already holding the bytes.
    Done(Outcome),
    /// The planned bytes are in `temp` beside the target, in the folder
    /// with this (device, inode).
    Written { temp: String, directory: (u64, u64) },
}

/// The (device, inode) of an open folder.
fn identity(directory: &OwnedFd, label: &str) -> Result<(u64, u64), RenderError> {
    let stat = fstat(directory).map_err(|error| RenderError::io(label, error))?;
    #[allow(clippy::useless_conversion, clippy::unnecessary_cast)]
    Ok((stat.st_dev as u64, stat.st_ino as u64))
}

/// Applies plans to one workspace's vault. Holding a [`RenderLock`] is
/// required to construct it, so no publication can run unserialized.
pub struct Publisher<'lock> {
    /// The [`RenderLock`] it was opened under, or for [`Self::inspect`] the
    /// idle probe; the publisher cannot outlive either.
    _serialized: PhantomData<&'lock ()>,
    /// The projection root ([`crate::projection`]): `vault/`, or the managed
    /// prefix of an existing vault. `None` only for [`Self::inspect`] before
    /// the first render created the prefix; it then reads as empty.
    vault: Option<OwnedFd>,
    /// The root as messages and sweep reports name it: `vault` for the
    /// workspace's own vault, the managed prefix in existing-vault mode.
    root: String,
    /// When the root's filesystem is trusted for fingerprints
    /// ([`fingerprint::trusted`]), the time the publisher was opened, in
    /// nanoseconds since the epoch: only files last changed well before it
    /// get a fingerprint.
    opened: Option<i64>,
}

pub(crate) enum Walk {
    Directory(OwnedFd),
    Missing,
    Unsafe,
}

impl<'lock> Publisher<'lock> {
    pub fn open(workspace: &Workspace, lock: &'lock RenderLock) -> Result<Self, RenderError> {
        if lock.workspace_root() != workspace.root() {
            return Err(RenderError::WrongWorkspace);
        }
        let vault = crate::projection::open_root(workspace, true)?;
        let opened = vault
            .as_ref()
            .filter(|root| fingerprint::trusted(*root))
            .and_then(|_| fingerprint::now());
        Ok(Self {
            _serialized: PhantomData,
            vault,
            root: crate::projection::Projection::of(workspace)
                .label()
                .to_owned(),
            opened,
        })
    }

    /// A publisher for reading the vault ([`Self::unknown_files`]) while no
    /// render runs, or `None` while one does. `probe` holds the render lock
    /// when its file exists, so no render starts during the read. Crate
    /// callers only read through it; every write needs [`Self::open`].
    pub(crate) fn inspect(
        workspace: &Workspace,
        probe: &'lock Probe,
    ) -> Result<Option<Self>, RenderError> {
        match probe {
            Probe::Running => Ok(None),
            Probe::Idle { .. } => Ok(Some(Self {
                _serialized: PhantomData,
                vault: crate::projection::open_root(workspace, false)?,
                root: crate::projection::Projection::of(workspace)
                    .label()
                    .to_owned(),
                opened: None,
            })),
        }
    }

    /// Apply every entry in plan order, then remove retired paths. A refused
    /// entry does not stop the rest; an I/O failure stops the render and is
    /// returned (the observer has seen every outcome reached before it).
    /// Every outcome in the report is durable when it returns. See
    /// [`Self::publish_durably`].
    pub fn publish(
        &self,
        plan: &RenderPlan,
        ownership: &dyn Ownership,
        observe: &mut dyn FnMut(&PublishStep<'_>),
    ) -> Result<PublishReport, RenderError> {
        self.publish_durably(plan, ownership, observe)
            .map_err(|interrupted| interrupted.error)
    }

    /// [`Self::publish`] in durability batches (see the module
    /// documentation). The observer sees each outcome as soon as it is
    /// reached, in plan order, then each retired path; a write is reached
    /// when its temporary is renamed into place. On an I/O failure the
    /// outcomes known durable are returned with the error: those a barrier
    /// followed. A failed barrier is never retried here.
    pub fn publish_durably(
        &self,
        plan: &RenderPlan,
        ownership: &dyn Ownership,
        observe: &mut dyn FnMut(&PublishStep<'_>),
    ) -> Result<PublishReport, Interrupted> {
        // A retired path that folds to a planned one may be the same file on a
        // case-insensitive filesystem; it is never a removal candidate.
        let planned: BTreeSet<String> = plan
            .entries()
            .iter()
            .map(|entry| entry.path().as_str().to_lowercase())
            .collect();
        let retired: BTreeSet<PlanPath> = ownership
            .retired(plan)
            .into_iter()
            .filter(|path| !planned.contains(&path.as_str().to_lowercase()))
            .collect();
        let total = plan.entries().len() + retired.len();
        let generation = plan.generation();
        let mut outcomes = Vec::with_capacity(total);
        // `outcomes[..durable]` are known durable: a barrier followed them.
        let mut durable = 0;
        for batch in plan.entries().chunks(STAGE_BATCH) {
            // An entry that fails to stage stops the publication there, as
            // an unbatched write would: the entries staged before it are
            // still committed, and the error is returned after them.
            let mut staged = Vec::with_capacity(batch.len());
            let mut failure = None;
            for entry in batch {
                match self.stage(generation, entry, ownership) {
                    Ok(stage) => staged.push(stage),
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            if staged
                .iter()
                .any(|stage| matches!(stage, Staged::Written { .. }))
            {
                if let Err(error) = self.barrier() {
                    self.discard(batch, &staged);
                    return Err(Interrupted::new(error, outcomes, durable));
                }
                durable = outcomes.len();
            }
            let mut staged = staged.into_iter();
            let mut offset = 0;
            while let Some(stage) = staged.next() {
                let entry = &batch[offset];
                offset += 1;
                let outcome = match stage {
                    Staged::Done(outcome) => outcome,
                    Staged::Written { temp, directory } => {
                        match self.commit(entry, &temp, directory, ownership) {
                            Ok(outcome) => outcome,
                            Err(error) => {
                                self.discard(&batch[offset..], staged.as_slice());
                                return Err(self.interrupted(error, outcomes, durable));
                            }
                        }
                    }
                };
                observe(&PublishStep {
                    index: outcomes.len(),
                    total,
                    entry: Some(entry),
                    outcome: &outcome,
                });
                outcomes.push(outcome);
            }
            if let Some(error) = failure {
                return Err(self.interrupted(error, outcomes, durable));
            }
        }
        for path in &retired {
            let outcome = match self.retire(temp_tag(generation), path, ownership, !BATCHED) {
                Ok(outcome) => outcome,
                Err(error) => return Err(self.interrupted(error, outcomes, durable)),
            };
            observe(&PublishStep {
                index: outcomes.len(),
                total,
                entry: None,
                outcome: &outcome,
            });
            outcomes.push(outcome);
        }
        // A publication that changed nothing, and only confirmed bytes the
        // ownership record already holds, has nothing to make durable: the
        // record only ever holds durable outcomes. Skipping the barrier
        // keeps such a render from waiting on other writers' data.
        let changed = outcomes.iter().any(|outcome| match outcome.action {
            Action::Written | Action::Removed => true,
            Action::Unchanged => match &outcome.previous {
                Some(OnDisk::Regular { sha256 }) => ownership
                    .recorded(&outcome.path)
                    .is_none_or(|(recorded, _)| recorded != sha256),
                _ => false,
            },
            Action::Refused(_) => false,
        });
        if changed && let Err(error) = self.barrier() {
            return Err(Interrupted::new(error, outcomes, durable));
        }
        Ok(PublishReport {
            generation: generation.clone(),
            outcomes,
        })
    }

    /// The failure of a publication that reached `outcomes`, after one
    /// barrier attempt to make all of them durable.
    fn interrupted(
        &self,
        error: RenderError,
        outcomes: Vec<Outcome>,
        durable: usize,
    ) -> Interrupted {
        let durable = if self.barrier().is_ok() {
            outcomes.len()
        } else {
            durable
        };
        Interrupted::new(error, outcomes, durable)
    }

    /// Make every change made so far under the projection root durable:
    /// written temporaries, renames, removals and new folders. One `syncfs`
    /// of the root's filesystem; nothing where each change was synced as it
    /// was made.
    pub fn barrier(&self) -> Result<(), RenderError> {
        match &self.vault {
            Some(root) => barrier_on(root, &self.root),
            None => Ok(()),
        }
    }

    /// Inspect one entry and decide; on [`Decision::Write`], write the
    /// planned bytes to a temporary beside the target, without syncing it.
    fn stage(
        &self,
        generation: &Generation,
        entry: &PlanEntry,
        ownership: &dyn Ownership,
    ) -> Result<Staged, RenderError> {
        let path = entry.path();
        let outcome = |previous, action| {
            Staged::Done(Outcome {
                path: path.clone(),
                previous,
                action,
                stat: None,
            })
        };
        let components: Vec<&str> = path.components().collect();
        let (name, parents) = components
            .split_last()
            .expect("plan paths have at least one component");
        if let Some(refusal) = self.screen(entry, ownership) {
            return Ok(outcome(None, Action::Refused(refusal)));
        }
        let (on_disk, stat) = match self.walk(path, parents, false)? {
            Walk::Unsafe => return Ok(outcome(None, Action::Refused(Refusal::UnsafeParent))),
            Walk::Missing => (OnDisk::Absent, None),
            Walk::Directory(directory) => self.observe(&directory, path, name, ownership)?,
        };
        let action = match (ownership.decide(entry, &on_disk), &on_disk) {
            (_, OnDisk::Other) => Action::Refused(Refusal::NotARegularFile),
            (Decision::Refuse(refusal), _) => Action::Refused(refusal),
            (Decision::Keep, OnDisk::Regular { sha256 }) if sha256 == entry.sha256() => {
                return Ok(Staged::Done(Outcome {
                    path: path.clone(),
                    previous: Some(on_disk),
                    action: Action::Unchanged,
                    stat,
                }));
            }
            (Decision::Keep | Decision::Remove, _) => Action::Refused(Refusal::NotProvenOwned),
            (Decision::Write, _) => match self.walk_with(path, parents, true, !BATCHED)? {
                Walk::Directory(directory) => {
                    let temp = write_temporary(
                        &directory,
                        path.as_str(),
                        name,
                        entry.content(),
                        temp_tag(generation),
                        !BATCHED,
                    )?;
                    let directory = identity(&directory, path.as_str())?;
                    return Ok(Staged::Written { temp, directory });
                }
                Walk::Missing | Walk::Unsafe => Action::Refused(Refusal::UnsafeParent),
            },
        };
        Ok(outcome(Some(on_disk), action))
    }

    /// [`inspect`] for a plan entry, using fingerprints when the root's
    /// filesystem is trusted for them ([`crate::fingerprint`]): a regular
    /// file whose status equals the fingerprint `ownership` recorded is
    /// taken to hold the recorded bytes, unread. Otherwise it is read and
    /// hashed, and its fingerprint is returned for recording when its status
    /// did not change while it was read and its last change is settled.
    fn observe(
        &self,
        directory: &OwnedFd,
        path: &PlanPath,
        name: &str,
        ownership: &dyn Ownership,
    ) -> Result<(OnDisk, Option<Fingerprint>), RenderError> {
        let Some(opened) = self.opened else {
            return Ok((inspect(directory, path, name)?, None));
        };
        let label = path.as_str();
        let io = |error: Errno| RenderError::io(label, error);
        let stat = match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => return Ok((OnDisk::Absent, None)),
            Err(error) => return Err(io(error)),
        };
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Ok((OnDisk::Other, None));
        }
        let seen = Fingerprint::of(&stat);
        if let Some((sha256, Some(recorded))) = ownership.recorded(path)
            && recorded == seen
        {
            return Ok((
                OnDisk::Regular {
                    sha256: sha256.clone(),
                },
                Some(seen),
            ));
        }
        let Some((on_disk, before, after)) = read_and_hash(directory, label, name)? else {
            return Ok((OnDisk::Absent, None));
        };
        let stable = match (before, after) {
            (Some(before), Some(after)) if before == after && before.settled(opened) => {
                Some(before)
            }
            _ => None,
        };
        Ok((on_disk, stable))
    }

    /// Put a staged temporary in place: the path is inspected and decided
    /// again just before the rename, so the window in which another writer
    /// can race it is as short as an unbatched write's. A temporary that is
    /// not renamed is removed; one whose folder was moved meanwhile cannot
    /// be reached and is left for the sweep.
    fn commit(
        &self,
        entry: &PlanEntry,
        temp: &str,
        staged_in: (u64, u64),
        ownership: &dyn Ownership,
    ) -> Result<Outcome, RenderError> {
        let path = entry.path();
        let label = path.as_str();
        let outcome = |previous, action| Outcome {
            path: path.clone(),
            previous,
            action,
            stat: None,
        };
        let components: Vec<&str> = path.components().collect();
        let (name, parents) = components
            .split_last()
            .expect("plan paths have at least one component");
        let directory = match self.walk(path, parents, false)? {
            Walk::Directory(directory) if identity(&directory, label)? == staged_in => directory,
            _ => return Ok(outcome(None, Action::Refused(Refusal::UnsafeParent))),
        };
        let decided = inspect(&directory, path, name).map(|on_disk| {
            let decision = ownership.decide(entry, &on_disk);
            (on_disk, decision)
        });
        let (on_disk, decision) = match decided {
            Ok(decided) => decided,
            Err(error) => {
                let _ = unlinkat(&directory, temp, AtFlags::empty());
                return Err(error);
            }
        };
        let action = match (decision, &on_disk) {
            (_, OnDisk::Other) => Action::Refused(Refusal::NotARegularFile),
            (Decision::Refuse(refusal), _) => Action::Refused(refusal),
            (Decision::Keep, OnDisk::Regular { sha256 }) if sha256 == entry.sha256() => {
                Action::Unchanged
            }
            (Decision::Keep | Decision::Remove, _) => Action::Refused(Refusal::NotProvenOwned),
            (Decision::Write, _) => {
                // A path inspected as absent is created without replacing,
                // so a file that appears meanwhile is never overwritten.
                let replace = !matches!(on_disk, OnDisk::Absent);
                if rename_into_place(&directory, label, temp, name, replace, !BATCHED)? {
                    return Ok(outcome(Some(on_disk), Action::Written));
                }
                Action::Refused(Refusal::NotProvenOwned)
            }
        };
        let _ = unlinkat(&directory, temp, AtFlags::empty());
        Ok(outcome(Some(on_disk), action))
    }

    /// Remove the temporaries of staged entries that will not be committed,
    /// best effort; whatever is left, the next render's sweep removes.
    fn discard(&self, entries: &[PlanEntry], staged: &[Staged]) {
        for (entry, stage) in entries.iter().zip(staged) {
            let Staged::Written { temp, directory } = stage else {
                continue;
            };
            let path = entry.path();
            let components: Vec<&str> = path.components().collect();
            let parents = &components[..components.len() - 1];
            if let Ok(Walk::Directory(found)) = self.walk(path, parents, false)
                && identity(&found, path.as_str()).is_ok_and(|found| found == *directory)
            {
                let _ = unlinkat(&found, temp.as_str(), AtFlags::empty());
            }
        }
    }

    /// The hard checks made before anything is inspected: a human-owned
    /// area, and an attachment that is not an approved preview.
    fn screen(&self, entry: &PlanEntry, ownership: &dyn Ownership) -> Option<Refusal> {
        let path = entry.path();
        if layout::is_human_owned(path) {
            return Some(Refusal::HumanOwned);
        }
        if layout::is_attachment(path)
            && (layout::attachment_artifact(path).is_none()
                || !ownership.approves_attachment(entry))
        {
            return Some(Refusal::UnlistedAttachment);
        }
        None
    }

    /// Vault-relative paths of files that are neither owned nor human-owned:
    /// everything outside the human-owned areas for which `owned`
    /// answers false. Hidden entries (a leading dot, such as the viewer's
    /// configuration folder) are skipped, symbolic links are reported and
    /// never followed, and nothing is changed. Sorted.
    pub fn unknown_files(&self, owned: &dyn Fn(&str) -> bool) -> Result<Vec<String>, RenderError> {
        let mut found = Vec::new();
        self.visit(false, &mut |relative, _, _, _| {
            if !owned(relative) {
                found.push(relative.to_owned());
            }
            Ok(())
        })?;
        found.sort();
        Ok(found)
    }

    /// Call `visit` with every non-directory entry of the vault: its
    /// vault-relative path, its directory, its name and its type. Hidden
    /// entries are skipped, the human-owned roots unless `human_roots`, and
    /// links are reported, never followed. Nothing is changed. In no
    /// particular order.
    pub(crate) fn visit(
        &self,
        human_roots: bool,
        visit: &mut VisitFile<'_>,
    ) -> Result<(), RenderError> {
        let mut pending: Vec<(String, Vec<CString>)> = vec![(String::new(), Vec::new())];
        while let Some((prefix, components)) = pending.pop() {
            let Some(directory) = self.open_directory(&components, &prefix)? else {
                continue;
            };
            let mut listing =
                Dir::read_from(&directory).map_err(|error| RenderError::io(&prefix, error))?;
            while let Some(item) = listing.read() {
                let item = item.map_err(|error| RenderError::io(&prefix, error))?;
                let raw = item.file_name();
                let name = raw.to_string_lossy();
                if name.starts_with('.') {
                    continue;
                }
                if !human_roots && prefix.is_empty() && layout::is_human_root(&name) {
                    continue;
                }
                let relative = if prefix.is_empty() {
                    name.into_owned()
                } else {
                    format!("{prefix}/{name}")
                };
                let mut kind = item.file_type();
                if kind == FileType::Unknown {
                    kind = match statat(&directory, raw, AtFlags::SYMLINK_NOFOLLOW) {
                        Ok(stat) => FileType::from_raw_mode(stat.st_mode),
                        Err(Errno::NOENT) => continue,
                        Err(error) => return Err(RenderError::io(relative, error)),
                    };
                }
                if kind == FileType::Directory {
                    let mut child = components.clone();
                    child.push(raw.to_owned());
                    pending.push((relative, child));
                } else {
                    visit(&relative, &directory, raw, kind)?;
                }
            }
        }
        Ok(())
    }

    /// Open a vault directory by components without following links; `None`
    /// when it vanished or became something else meanwhile.
    fn open_directory(
        &self,
        components: &[CString],
        label: &str,
    ) -> Result<Option<OwnedFd>, RenderError> {
        let Some(root) = &self.vault else {
            return Ok(None);
        };
        let mut directory = openat(root, ".", DIRECTORY, Mode::empty())
            .map_err(|error| RenderError::io(&self.root, error))?;
        for component in components {
            directory = match openat(&directory, component.as_c_str(), DIRECTORY, Mode::empty()) {
                Ok(fd) => fd,
                Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR) => return Ok(None),
                Err(error) => return Err(RenderError::io(label, error)),
            };
        }
        Ok(Some(directory))
    }

    /// Delete one retired path if [`Ownership::decide_removal`] proves it
    /// generated and its bytes do not change while it is being removed.
    /// `tag` names the hidden file it is moved aside to first. Without
    /// `sync`, a removal is durable only after the next [`Self::barrier`].
    pub(crate) fn retire(
        &self,
        tag: &str,
        path: &PlanPath,
        ownership: &dyn Ownership,
        sync: bool,
    ) -> Result<Outcome, RenderError> {
        let outcome = |previous, action| Outcome {
            path: path.clone(),
            previous,
            action,
            stat: None,
        };
        if layout::is_human_owned(path) {
            return Ok(outcome(None, Action::Refused(Refusal::HumanOwned)));
        }
        let components: Vec<&str> = path.components().collect();
        let (name, parents) = components
            .split_last()
            .expect("plan paths have at least one component");
        let directory = match self.walk(path, parents, false)? {
            Walk::Unsafe => return Ok(outcome(None, Action::Refused(Refusal::UnsafeParent))),
            Walk::Missing => return Ok(outcome(Some(OnDisk::Absent), Action::Unchanged)),
            Walk::Directory(directory) => directory,
        };
        let on_disk = inspect(&directory, path, name)?;
        let action = match (ownership.decide_removal(path, &on_disk), &on_disk) {
            (_, OnDisk::Absent) => Action::Unchanged,
            (_, OnDisk::Other) => Action::Refused(Refusal::NotARegularFile),
            (Decision::Refuse(refusal), _) => Action::Refused(refusal),
            (Decision::Remove, OnDisk::Regular { sha256 }) => {
                match remove_verified(&directory, tag, path.as_str(), name, sha256, sync)? {
                    Removal::Removed => {
                        drop(directory);
                        self.prune_empty_parents(path, parents);
                        Action::Removed
                    }
                    Removal::Vanished => Action::Unchanged,
                    Removal::Changed => Action::Refused(Refusal::Drifted),
                }
            }
            (Decision::Write | Decision::Keep, _) => Action::Refused(Refusal::NotProvenOwned),
        };
        Ok(outcome(Some(on_disk), action))
    }

    /// Remove the top-level folder `name` of the projection root when it is
    /// empty: one non-recursive `rmdir`. Best effort; `true` when removed.
    /// Only a removal of the whole projection ([`crate::remove`]) asks.
    pub(crate) fn remove_empty_folder(&self, name: &str) -> bool {
        self.vault
            .as_ref()
            .is_some_and(|root| remove_empty_directory(root, name))
    }

    /// Remove parent folders a removal left empty, deepest first, stopping at
    /// the first that is not empty. Top-level vault folders are never
    /// removed. Each removal is a single non-recursive `rmdir`, which fails
    /// while any entry remains, so a foreign file always keeps its folder.
    /// Best effort: a failure here leaves an empty folder, nothing worse.
    fn prune_empty_parents(&self, path: &PlanPath, parents: &[&str]) {
        for depth in (2..=parents.len()).rev() {
            let Ok(Walk::Directory(directory)) = self.walk(path, &parents[..depth - 1], false)
            else {
                return;
            };
            if !remove_empty_directory(&directory, parents[depth - 1]) {
                return;
            }
        }
    }

    pub(crate) fn apply(
        &self,
        generation: &Generation,
        entry: &PlanEntry,
        ownership: &dyn Ownership,
    ) -> Result<Outcome, RenderError> {
        let path = entry.path();
        let outcome = |previous, action| Outcome {
            path: path.clone(),
            previous,
            action,
            stat: None,
        };
        let components: Vec<&str> = path.components().collect();
        let (name, parents) = components
            .split_last()
            .expect("plan paths have at least one component");
        if let Some(refusal) = self.screen(entry, ownership) {
            return Ok(outcome(None, Action::Refused(refusal)));
        }

        let on_disk = match self.walk(path, parents, false)? {
            Walk::Unsafe => return Ok(outcome(None, Action::Refused(Refusal::UnsafeParent))),
            Walk::Missing => OnDisk::Absent,
            Walk::Directory(directory) => inspect(&directory, path, name)?,
        };
        let action = match (ownership.decide(entry, &on_disk), &on_disk) {
            (_, OnDisk::Other) => Action::Refused(Refusal::NotARegularFile),
            (Decision::Refuse(refusal), _) => Action::Refused(refusal),
            (Decision::Keep, OnDisk::Regular { sha256 }) if sha256 == entry.sha256() => {
                Action::Unchanged
            }
            (Decision::Keep | Decision::Remove, _) => Action::Refused(Refusal::NotProvenOwned),
            (Decision::Write, _) => match self.walk(path, parents, true)? {
                Walk::Directory(directory) => {
                    // A path inspected as absent is created without replacing,
                    // so a file that appears meanwhile is never overwritten.
                    let replace = !matches!(on_disk, OnDisk::Absent);
                    if write_atomically(&directory, generation, entry, name, replace)? {
                        Action::Written
                    } else {
                        Action::Refused(Refusal::NotProvenOwned)
                    }
                }
                Walk::Missing | Walk::Unsafe => Action::Refused(Refusal::UnsafeParent),
            },
        };
        Ok(outcome(Some(on_disk), action))
    }

    /// Open each parent directory without following links, optionally creating
    /// missing ones (mode 0700).
    /// The label of a folder below the root: `<root>/<parents>`, where the
    /// root is `vault` or, in existing-vault mode, the managed prefix
    /// ([`crate::projection::Projection::label`]).
    pub(crate) fn folder_label(&self, parents: &[&str]) -> String {
        if parents.is_empty() {
            self.root.clone()
        } else {
            format!("{}/{}", self.root, parents.join("/"))
        }
    }

    pub(crate) fn walk(
        &self,
        path: &PlanPath,
        parents: &[&str],
        create: bool,
    ) -> Result<Walk, RenderError> {
        self.walk_with(path, parents, create, true)
    }

    /// [`Self::walk`]; without `sync`, a folder it creates is durable only
    /// after the next [`Self::barrier`].
    fn walk_with(
        &self,
        path: &PlanPath,
        parents: &[&str],
        create: bool,
        sync: bool,
    ) -> Result<Walk, RenderError> {
        let Some(root) = &self.vault else {
            return Ok(Walk::Missing);
        };
        let mut directory = openat(root, ".", DIRECTORY, Mode::empty())
            .map_err(|error| RenderError::io(&self.root, error))?;
        for component in parents {
            let next = match openat(&directory, *component, DIRECTORY, Mode::empty()) {
                Err(Errno::NOENT) if create => {
                    match mkdirat(&directory, *component, Mode::from_raw_mode(0o700)) {
                        Ok(()) | Err(Errno::EXIST) => {}
                        Err(error) => return Err(RenderError::io(path.as_str(), error)),
                    }
                    if sync {
                        fsync(&directory).map_err(|error| RenderError::io(path.as_str(), error))?;
                    }
                    openat(&directory, *component, DIRECTORY, Mode::empty())
                }
                other => other,
            };
            directory = match next {
                Ok(fd) => fd,
                Err(Errno::NOENT) => return Ok(Walk::Missing),
                Err(Errno::LOOP | Errno::NOTDIR) => return Ok(Walk::Unsafe),
                Err(error) => return Err(RenderError::io(path.as_str(), error)),
            };
        }
        Ok(Walk::Directory(directory))
    }
}

/// The one way the renderer removes a folder: a single non-recursive
/// `rmdir` of `name` in `directory`, which the kernel refuses while any
/// entry, foreign or hidden, remains. `true` when it was removed.
pub(crate) fn remove_empty_directory(directory: &OwnedFd, name: &str) -> bool {
    if unlinkat(directory, name, AtFlags::REMOVEDIR).is_err() {
        return false;
    }
    let _ = fsync(directory);
    true
}

pub(crate) fn inspect(
    directory: &OwnedFd,
    path: &PlanPath,
    name: &str,
) -> Result<OnDisk, RenderError> {
    inspect_named(directory, path.as_str(), name)
}

/// [`inspect`] for a file whose vault-relative path `label` need not be a
/// plan path (a sync client's conflict copy, a moved note).
pub(crate) fn inspect_named(
    directory: &OwnedFd,
    label: &str,
    name: &str,
) -> Result<OnDisk, RenderError> {
    let io = |error: Errno| RenderError::io(label, error);
    let stat = match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(OnDisk::Absent),
        Err(error) => return Err(io(error)),
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Ok(OnDisk::Other);
    }
    Ok(read_and_hash(directory, label, name)?.map_or(OnDisk::Absent, |(on_disk, _, _)| on_disk))
}

/// What [`read_and_hash`] found: the file, and its fingerprint before and
/// after the read.
type Hashed = (OnDisk, Option<Fingerprint>, Option<Fingerprint>);

/// Open `name` without following links and hash its bytes, with the
/// fingerprint of the open file before and after the read (`None` when it
/// is not a regular file). `None` when it vanished.
fn read_and_hash(
    directory: &OwnedFd,
    label: &str,
    name: &str,
) -> Result<Option<Hashed>, RenderError> {
    let io = |error: Errno| RenderError::io(label, error);
    // NONBLOCK: a FIFO swapped in after the stat must not hang the render.
    let fd = match openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(None),
        Err(Errno::LOOP) => return Ok(Some((OnDisk::Other, None, None))),
        Err(error) => return Err(io(error)),
    };
    let before = fstat(&fd).map_err(io)?;
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {
        return Ok(Some((OnDisk::Other, None, None)));
    }
    let mut file = File::from(fd);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| RenderError::io(label, error))?;
    let after = fstat(&file).map_err(io)?;
    Ok(Some((
        OnDisk::Regular {
            sha256: hash_bytes(&bytes),
        },
        Some(Fingerprint::of(&before)),
        Some(Fingerprint::of(&after)),
    )))
}

/// A short, file-name-safe tag for temporary names from a generation ID.
fn temp_tag(generation: &Generation) -> &str {
    generation
        .id()
        .trim_start_matches("gen_")
        .get(..12)
        .unwrap_or("render")
}

/// Create a fresh hidden file `.{name}.{tag}-{n}.{suffix}` beside `name`.
fn create_temporary(
    directory: &OwnedFd,
    label: &str,
    name: &str,
    tag: &str,
    suffix: &str,
) -> Result<(String, OwnedFd), RenderError> {
    for attempt in 0..TEMP_ATTEMPTS {
        let temp = format!(".{name}.{tag}-{attempt}.{suffix}");
        match openat(
            directory,
            temp.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(fd) => return Ok((temp, fd)),
            Err(Errno::EXIST) => {}
            Err(error) => return Err(RenderError::io(label, error)),
        }
    }
    Err(leftovers(label, suffix))
}

fn leftovers(label: &str, suffix: &str) -> RenderError {
    RenderError::io(
        label,
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "leftover temporary files block this render; remove the hidden .{suffix} files beside it"
            ),
        ),
    )
}

/// Write `entry` beside its target under a hidden temporary name, then rename
/// it into place. Without `replace`, an existing target is left alone and the
/// result is `false`.
fn write_atomically(
    directory: &OwnedFd,
    generation: &Generation,
    entry: &PlanEntry,
    name: &str,
    replace: bool,
) -> Result<bool, RenderError> {
    write_bytes_atomically(
        directory,
        entry.path().as_str(),
        name,
        entry.content(),
        temp_tag(generation),
        replace,
    )
}

/// Write `bytes` to a private temporary file in `directory`, sync it, and
/// rename it to `name`, then sync the directory. Without `replace` the rename
/// never replaces an existing file and the result is `false` when one is
/// there. A temporary file this call created is always removed unless it was
/// renamed into place. `label` names the target in errors.
pub(crate) fn write_bytes_atomically(
    directory: &OwnedFd,
    label: &str,
    name: &str,
    bytes: &[u8],
    tag: &str,
    replace: bool,
) -> Result<bool, RenderError> {
    write_bytes_with(directory, label, name, bytes, tag, replace, true)
}

/// [`write_bytes_atomically`], with the file and its directory synced only
/// when `sync`; otherwise a later [`barrier_on`] makes the change durable.
pub(crate) fn write_bytes_with(
    directory: &OwnedFd,
    label: &str,
    name: &str,
    bytes: &[u8],
    tag: &str,
    replace: bool,
    sync: bool,
) -> Result<bool, RenderError> {
    let temp = write_temporary(directory, label, name, bytes, tag, sync)?;
    let renamed = rename_into_place(directory, label, &temp, name, replace, sync);
    if !matches!(renamed, Ok(true)) {
        let _ = unlinkat(directory, temp.as_str(), AtFlags::empty());
    }
    renamed
}

/// Make every change made so far on the filesystem holding `fd` durable:
/// one `syncfs`. Nothing where [`BATCHED`] is false, because there each
/// change was synced as it was made.
pub(crate) fn barrier_on(fd: &OwnedFd, label: &str) -> Result<(), RenderError> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    rustix::fs::syncfs(fd).map_err(|error| RenderError::io(label, error))?;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let _ = (fd, label);
    Ok(())
}

/// Write `bytes` to a fresh private temporary beside `name` and return its
/// name; synced when `sync`. A temporary that could not be written whole is
/// removed.
fn write_temporary(
    directory: &OwnedFd,
    label: &str,
    name: &str,
    bytes: &[u8],
    tag: &str,
    sync: bool,
) -> Result<String, RenderError> {
    let (temp, fd) = create_temporary(directory, label, name, tag, "inv-tmp")?;
    let mut file = File::from(fd);
    let written = file
        .write_all(bytes)
        .and_then(|()| if sync { file.sync_all() } else { Ok(()) });
    drop(file);
    if let Err(error) = written {
        let _ = unlinkat(directory, temp.as_str(), AtFlags::empty());
        return Err(RenderError::io(label, error));
    }
    Ok(temp)
}

/// Rename `temp` to `name` in `directory`, then sync the directory when
/// `sync`. Without `replace` an existing `name` is left alone and the result
/// is `false`. The temporary is left in place whenever the result is not
/// `Ok(true)`.
fn rename_into_place(
    directory: &OwnedFd,
    label: &str,
    temp: &str,
    name: &str,
    replace: bool,
    sync: bool,
) -> Result<bool, RenderError> {
    let renamed = if replace {
        renameat(directory, temp, directory, name)
    } else {
        renameat_with(directory, temp, directory, name, RenameFlags::NOREPLACE)
    };
    match renamed {
        Ok(()) => {}
        Err(Errno::EXIST) if !replace => return Ok(false),
        Err(error) => return Err(RenderError::io(label, error)),
    }
    if sync {
        fsync(directory.as_fd()).map_err(|error| RenderError::io(label, error))?;
    }
    Ok(true)
}

pub(crate) enum Removal {
    Removed,
    /// The file was gone before it could be moved aside.
    Vanished,
    /// The bytes changed after inspection; the file is back at its path.
    Changed,
}

/// Delete `name` only if its bytes are still `expected`.
///
/// The file is first renamed to a fresh hidden name in the same directory, so
/// later writers through the original path create a new file instead of
/// racing the deletion. The moved file is hashed again; it is unlinked only
/// when the hash still matches, and otherwise renamed back without replacing
/// anything. A file that cannot be put back is reported with its location and
/// never deleted. `label` names the file in errors; `tag` its temporary.
/// Without `sync` a removal is durable only after the caller's barrier; a
/// file put back is always synced.
pub(crate) fn remove_verified(
    directory: &OwnedFd,
    tag: &str,
    label: &str,
    name: &str,
    expected: &Digest,
    sync: bool,
) -> Result<Removal, RenderError> {
    let mut aside = None;
    for attempt in 0..TEMP_ATTEMPTS {
        let candidate = format!(".{name}.{tag}-{attempt}.inv-del");
        match renameat_with(
            directory,
            name,
            directory,
            candidate.as_str(),
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                aside = Some(candidate);
                break;
            }
            Err(Errno::EXIST) => {}
            Err(Errno::NOENT) => return Ok(Removal::Vanished),
            Err(error) => return Err(RenderError::io(label, error)),
        }
    }
    let Some(aside) = aside else {
        return Err(leftovers(label, "inv-del"));
    };
    let unchanged = match inspect_named(directory, label, &aside) {
        Ok(OnDisk::Regular { sha256 }) => Ok(&sha256 == expected),
        Ok(_) => Ok(false),
        Err(error) => Err(error),
    };
    if matches!(unchanged, Ok(true)) {
        unlinkat(directory, aside.as_str(), AtFlags::empty())
            .map_err(|error| RenderError::io(label, error))?;
        if sync {
            fsync(directory.as_fd()).map_err(|error| RenderError::io(label, error))?;
        }
        return Ok(Removal::Removed);
    }
    match renameat_with(
        directory,
        aside.as_str(),
        directory,
        name,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {
            fsync(directory.as_fd()).map_err(|error| RenderError::io(label, error))?;
            unchanged.map(|_| Removal::Changed)
        }
        Err(error) => Err(RenderError::io(
            label,
            std::io::Error::other(format!(
                "the file changed while it was being removed and could not be put back ({error}); \
                 it is preserved beside its path as {aside}"
            )),
        )),
    }
}
