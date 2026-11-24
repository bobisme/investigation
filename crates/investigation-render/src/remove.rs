//! `vault remove`: take the projection out of the vault, deleting only what
//! the renderer owns.
//!
//! A removal is a render of nothing. Every path the manifest owns (or an
//! unfinished render intended) is retired through the publisher's verified
//! removal: it is deleted only while it holds exactly the bytes recorded for
//! it, after being moved aside and hashed again. Then the folders that
//! leaves empty are removed with non-recursive `rmdir`s, the top-level
//! generated folders and, in existing-vault mode, the managed prefix last.
//! Nothing is ever deleted recursively, and no directory listing chooses a
//! file to delete.
//!
//! What stays, and why:
//!
//! | Left in place                                   | Because                                   |
//! |-------------------------------------------------|-------------------------------------------|
//! | an owned file someone edited (drift)            | its bytes are not the ones generated      |
//! | a released path (`release-path`)                | it is the person's                        |
//! | a file moved into `Notes/Reconciled/`           | it is the person's edit                   |
//! | `Notes/`, `Boards/`, `Inbox/`, hidden folders   | they are the person's                     |
//! | any file the manifest does not own              | nothing proves the renderer wrote it      |
//!
//! Edited files stay owned with their drift records, so `vault conflicts`
//! still lists them and `vault reconcile` can resolve them; released and
//! handed-over paths stay recorded. The manifest is stored last with no
//! complete generation, so `vault status` reads `unrendered`, and the next
//! `vault render` projects the casebook again. In existing-vault mode that
//! render is a first render again when nothing is recorded any more, so it
//! refuses a prefix that now holds files the renderer does not own.
//!
//! [`preview`] answers the same question without taking the render lock or
//! writing anything.

use investigation_store::Workspace;

use crate::{
    Action, Decision, Manifest, OnDisk, Outcome, Ownership, PlanPath, Publisher, RENDER_LOCK_WAIT,
    Refusal, RenderError, RenderLock, layout,
    lock::{self},
    projection::{self, Projection},
    publish::{Walk, inspect},
    sweep::{self, Swept},
};

/// Temporary-name tag for files moved aside before they are deleted.
const TAG: &str = "remove";

/// One owned path and what the removal did, or would do, with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub path: PlanPath,
    pub action: Action,
}

/// What [`remove_projection`] did, or [`preview`] would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveReport {
    /// Every owned or pending path in path order. `Removed` deleted it,
    /// `Unchanged` found it already gone, `Refused` left it in place.
    pub paths: Vec<Removed>,
    /// Paths released to a person, left in place.
    pub released: Vec<PlanPath>,
    /// Files moved into `Notes/Reconciled/`, left in place.
    pub handed_over: Vec<String>,
    /// Temporaries of a killed render cleaned first.
    pub swept: Vec<Swept>,
    /// Whether the managed prefix folder itself was removed (it was empty).
    pub removed_root: bool,
}

impl RemoveReport {
    pub fn removed(&self) -> usize {
        self.count(|action| *action == Action::Removed)
    }

    /// Owned files left in place (edited, or not a regular file).
    pub fn kept(&self) -> usize {
        self.count(|action| matches!(action, Action::Refused(_)))
    }

    fn count(&self, matches: impl Fn(&Action) -> bool) -> usize {
        self.paths
            .iter()
            .filter(|path| matches(&path.action))
            .count()
    }
}

/// Remove the projection: see the module documentation. Holds the render
/// lock throughout, so no render runs meanwhile.
pub fn remove_projection(workspace: &Workspace) -> Result<RemoveReport, RenderError> {
    let lock = RenderLock::acquire(workspace, RENDER_LOCK_WAIT)?;
    let manifest = Manifest::load(workspace, &lock)?
        .manifest
        .bound_to(&Projection::of(workspace))?;
    if !root_exists(workspace)? {
        // The managed prefix is gone (or was never created): every path the
        // record owns is already absent.
        let outcomes: Vec<Outcome> = manifest
            .known_paths()
            .into_iter()
            .map(|path| Outcome {
                path,
                previous: Some(OnDisk::Absent),
                action: Action::Unchanged,
                stat: None,
            })
            .collect();
        let manifest = manifest.removed(&outcomes);
        manifest.store(workspace, &lock)?;
        let paths = outcomes.into_iter().map(removed).collect();
        return Ok(report(&manifest, paths, Vec::new(), false));
    }
    let publisher = Publisher::open(workspace, &lock)?;
    let targets = manifest.known_paths();
    let mut swept = sweep::sweep_local(workspace)?;
    swept.extend(publisher.sweep(&targets, &|path, sha256| manifest.accepts(path, sha256))?);
    // Stored first: a removal killed part-way reads `updating`, so the next
    // render recreates the owned files it already deleted instead of
    // diagnosing them as deleted by someone, and the next removal finishes.
    manifest.removing().store(workspace, &lock)?;
    let mut outcomes = Vec::with_capacity(targets.len());
    let mut result = Ok(());
    for path in &targets {
        // Each removal is made durable by the barrier below, not one by one.
        match publisher.retire(TAG, path, &manifest, false) {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) => {
                result = Err(error);
                break;
            }
        }
    }
    // Nothing is forgotten before its removal is durable: when the barrier
    // fails, the `updating` record stays, and the next removal finishes.
    publisher.barrier()?;
    // The outcomes reached are recorded even when a file operation failed,
    // so every deleted path is forgotten and every kept one stays owned.
    let manifest = manifest.removed(&outcomes);
    manifest.store(workspace, &lock)?;
    result?;
    for folder in [layout::CASES, layout::ATTACHMENTS] {
        publisher.remove_empty_folder(folder);
    }
    drop(publisher);
    let removed_root = projection::remove_root_if_empty(workspace);
    let paths = outcomes.into_iter().map(removed).collect();
    Ok(report(&manifest, paths, swept, removed_root))
}

/// What [`remove_projection`] would do now, without taking the render lock
/// or writing anything. Busy while a render runs.
pub fn preview(workspace: &Workspace) -> Result<RemoveReport, RenderError> {
    let probe = lock::probe(workspace)?;
    let Some(publisher) = Publisher::inspect(workspace, &probe)? else {
        return Err(RenderError::Busy {
            waited: std::time::Duration::ZERO,
        });
    };
    let manifest = Manifest::read(workspace)?
        .manifest
        .bound_to(&Projection::of(workspace))?;
    let mut paths = Vec::new();
    for path in manifest.known_paths() {
        let components: Vec<&str> = path.components().collect();
        let (name, parents) = components
            .split_last()
            .expect("plan paths have at least one component");
        let action = match publisher.walk(&path, parents, false)? {
            Walk::Unsafe => Action::Refused(Refusal::UnsafeParent),
            Walk::Missing => Action::Unchanged,
            Walk::Directory(directory) => {
                let on_disk = inspect(&directory, &path, name)?;
                match (manifest.decide_removal(&path, &on_disk), on_disk) {
                    (_, OnDisk::Absent) => Action::Unchanged,
                    (_, OnDisk::Other) => Action::Refused(Refusal::NotARegularFile),
                    (Decision::Remove, _) => Action::Removed,
                    (Decision::Refuse(refusal), _) => Action::Refused(refusal),
                    (Decision::Write | Decision::Keep, _) => {
                        Action::Refused(Refusal::NotProvenOwned)
                    }
                }
            }
        };
        paths.push(Removed { path, action });
    }
    Ok(report(&manifest, paths, Vec::new(), false))
}

/// Whether the projection root exists. The workspace's own `vault/` always
/// does while the vault policy is enabled; a managed prefix exists once a
/// render created it.
fn root_exists(workspace: &Workspace) -> Result<bool, RenderError> {
    Ok(projection::open_root(workspace, false)?.is_some())
}

fn removed(outcome: Outcome) -> Removed {
    Removed {
        path: outcome.path,
        action: outcome.action,
    }
}

fn report(
    manifest: &Manifest,
    paths: Vec<Removed>,
    swept: Vec<Swept>,
    removed_root: bool,
) -> RemoveReport {
    RemoveReport {
        paths,
        released: manifest
            .released()
            .map(|released| released.path.clone())
            .collect(),
        handed_over: manifest
            .handed_over()
            .map(|handed| handed.path.clone())
            .collect(),
        swept,
        removed_root,
    }
}
