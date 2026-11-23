//! `vault reconcile`: one explicit disposition for each drifted path.
//!
//! A render leaves a drifted generated file exactly as found ([`crate::drift`]).
//! Reconciling is how a person or an agent decides what happens to it. The
//! tool never guesses: nothing is inferred from the Markdown, and a checkbox,
//! a property or a board card changed in a viewer never becomes a domain
//! change on its own. There are exactly four dispositions:
//!
//! | Disposition        | The observed bytes                                   | The path                                  |
//! |--------------------|------------------------------------------------------|-------------------------------------------|
//! | `preserve-as-note` | moved into `Notes/Reconciled/` under a new name      | regenerated                               |
//! | `propose-change`   | kept in `local/conflicts/proposed/`                  | regenerated after the change is recorded  |
//! | `discard`          | kept in `local/conflicts/discarded/` until removed   | regenerated; needs an explicit flag       |
//! | `release-path`     | left in place, now the person's file                 | released; the record moves to a new path  |
//!
//! "Observed bytes" are every file the drift record names: the edited file at
//! the path, the sync client's conflict copies beside it, and a moved copy
//! outside the human-owned folders. A moved copy already inside `Notes/`,
//! `Boards/` or `Inbox/` is the person's and is never touched. No disposition
//! loses observed bytes: a move into `Notes/` is a no-replace rename, and a
//! file set aside is first copied into the conflicts area, then removed only
//! if its bytes are still the ones copied ([`crate::publish`]'s verified
//! removal). `propose-change` records the change itself through the ordinary
//! `record` path before this module runs; here it only sets the observed
//! bytes aside, like `discard` without the flag.
//!
//! One request is one step:
//!
//! 1. Take the render lock and read the manifest.
//! 2. Check every listed path against its drift record: what is there now
//!    must be what the last render diagnosed (a compare-and-swap on the
//!    observed hash). Any refusal stops the request before anything moves.
//! 3. Set the observed files aside, path by path, and drop the path's claim
//!    from the manifest: a regenerated path is forgotten so the render
//!    creates it afresh, without replacing; a released path is recorded as
//!    released ([`crate::manifest::Released`]) with the path its projection
//!    moves to. A file moved into `Notes/Reconciled/` is recorded as handed
//!    over ([`crate::manifest::HandedOver`]). The manifest is stored before
//!    the render.
//! 4. Render under the same lock ([`crate::render_vault`]), so the vault,
//!    the entry note and the manifest end consistent.
//!
//! [`preview`] runs steps 1–2 without taking the lock or writing anything; a
//! dry run prints it. If a later path fails in step 3, the paths before it
//! are recorded as done and the error says so; their files are already in
//! their new places, and the next render completes them.
//!
//! `release-path` relocates the projection of a released path to
//! `<folder>/<name>-<n>.md` beside it, the first `n` from 2 that is free on
//! disk and in the manifest, and rewrites every generated link to the old
//! path to point there ([`relocate`]). The released file keeps every byte,
//! is never written, removed, adopted or reported as unknown again, and
//! links inside human notes keep pointing at it. Only Markdown notes under
//! `Cases/` can be released.
//!
//! Like rendering, reconciling works with any viewer closed: the filesystem
//! is the only interface. It cannot coordinate a viewer's unsaved buffer; the
//! contract stays "do not edit generated notes directly".

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::{identity::Id, wire::Digest};
use investigation_store::{Store, Workspace, durable::DurableFs, journal::hash_bytes};
use rustix::{
    fs::{RenameFlags, fsync, renameat_with},
    io::Errno,
};

use crate::{
    CaseRenderer, Drift, DriftKind, Manifest, OnDisk, PlanBuilder, PlanPath, PublishMode,
    PublishStep, Publisher, RENDER_LOCK_WAIT, RenderError, RenderLock, RenderPlan, RenderReport,
    VerifiedSnapshot, ViewPolicy,
    drift::{conflicts_directory, read_regular},
    layout,
    lock::{self, open_local},
    manifest::{HandedOver, Released},
    projection::Projection,
    publish::{Removal, Walk, inspect_named, remove_verified, write_bytes_atomically},
    render_locked,
    start_here::EntryHealth,
};

/// The folder, inside the human-owned `Notes/`, that `preserve-as-note`
/// moves observed files into.
pub const NOTES_FOLDER: &str = "Notes/Reconciled";
/// Where `discard` keeps the observed bytes, workspace-relative.
pub const DISCARDED_DIR: &str = "local/conflicts/discarded";
/// Where `propose-change` keeps the observed bytes, workspace-relative.
pub const PROPOSED_DIR: &str = "local/conflicts/proposed";
/// Paths one request may list. A batch is always an explicit list.
pub const MAX_PATHS: usize = 100;
/// Name candidates tried in `Notes/Reconciled/` and for a relocated path.
const MAX_CANDIDATES: usize = 100;

/// What to do with one drifted path. See the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    PreserveAsNote,
    ProposeChange,
    Discard,
    ReleasePath,
}

impl Disposition {
    pub const ALL: [Self; 4] = [
        Self::PreserveAsNote,
        Self::ProposeChange,
        Self::Discard,
        Self::ReleasePath,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreserveAsNote => "preserve-as-note",
            Self::ProposeChange => "propose-change",
            Self::Discard => "discard",
            Self::ReleasePath => "release-path",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }
}

/// One reconcile request: a disposition applied to an explicit list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileRequest {
    pub disposition: Disposition,
    /// Vault-relative paths, as `vault conflicts` lists them.
    pub paths: Vec<String>,
    /// The explicit confirmation `discard` requires.
    pub discard_observed: bool,
}

/// Which file of a drift record an observed file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The edited file at the drifted path.
    Edited,
    /// A sync client's conflict copy beside it.
    ConflictCopy,
    /// The file found moved elsewhere in the vault.
    Moved,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Edited => "edited",
            Self::ConflictCopy => "conflict_copy",
            Self::Moved => "moved",
        }
    }
}

/// What reconciling does, or did, with one observed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handling {
    /// Moved by a no-replace rename to this vault-relative path in
    /// [`NOTES_FOLDER`].
    MovedToNotes { to: String },
    /// Copied to this workspace-relative path in the conflicts area, then
    /// removed from the vault once its bytes were verified unchanged.
    SetAside { to: String },
    /// Left where it is: in a human-owned folder, or a released file.
    Left,
}

/// One observed file and its handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// Vault-relative path.
    pub path: String,
    pub role: Role,
    pub sha256: Digest,
    pub handling: Handling,
}

/// The step for one listed path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathStep {
    pub path: PlanPath,
    /// The recorded drift; `None` for a clean owned path being released.
    pub kind: Option<DriftKind>,
    /// The record the path projects.
    pub projects: Option<Id>,
    pub observed: Vec<Observed>,
    /// Whether the path is rendered afresh after the observed files move.
    pub regenerate: bool,
    /// For `release-path`, where the projection moves.
    pub relocated_to: Option<PlanPath>,
}

/// A checked request: every listed path with what happens to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    pub disposition: Disposition,
    pub steps: Vec<PathStep>,
}

/// What [`reconcile_vault`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    pub done: Reconciliation,
    /// The render that completed the request.
    pub render: RenderReport,
}

impl ReconcileReport {
    /// Drift the completing render still records for a listed path or the
    /// path its projection moved to. Empty when the request cleared them.
    pub fn remaining(&self) -> Vec<&Drift> {
        let listed: BTreeSet<&PlanPath> = self
            .done
            .steps
            .iter()
            .flat_map(|step| std::iter::once(&step.path).chain(step.relocated_to.as_ref()))
            .collect();
        self.render
            .drift
            .iter()
            .filter(|drift| listed.contains(&drift.path))
            .collect()
    }
}

/// Check `request` against the manifest and the vault, and say what it would
/// do, without taking the render lock or writing anything (not even the lock
/// file). Busy while a render runs.
pub fn preview(
    workspace: &Workspace,
    request: &ReconcileRequest,
) -> Result<Reconciliation, RenderError> {
    let probe = lock::probe(workspace)?;
    let Some(publisher) = Publisher::inspect(workspace, &probe)? else {
        return Err(RenderError::Busy {
            waited: std::time::Duration::ZERO,
        });
    };
    let manifest = Manifest::read(workspace)?
        .manifest
        .bound_to(&Projection::of(workspace))?;
    check(&publisher, &manifest, request)
}

/// Apply `request` and render, as one step under the render lock. See the
/// module documentation. `propose-change` must already have recorded its
/// change; this sets the observed bytes aside and regenerates.
pub fn reconcile_vault<F: DurableFs>(
    store: &mut Store<F>,
    renderer: &impl CaseRenderer,
    policy: &ViewPolicy,
    request: &ReconcileRequest,
    observe: &mut dyn FnMut(&PublishStep<'_>),
) -> Result<ReconcileReport, RenderError> {
    let lock = RenderLock::acquire(store.workspace(), RENDER_LOCK_WAIT)?;
    let workspace = store.workspace();
    let manifest = Manifest::load(workspace, &lock)?
        .manifest
        .bound_to(&Projection::of(workspace))?;
    let publisher = Publisher::open(workspace, &lock)?;
    let checked = check(&publisher, &manifest, request)?;
    let tag = tag(&manifest);
    let since = manifest
        .last_complete()
        .map_or_else(|| "unrendered".to_owned(), |last| last.id.clone());
    let mut manifest = manifest;
    let mut done = Vec::with_capacity(checked.steps.len());
    for step in checked.steps {
        match apply(&publisher, workspace, &tag, step) {
            Ok(step) => {
                manifest = settle(&publisher, manifest, &step, &since)?;
                done.push(step);
            }
            Err(error) => {
                // Paths already reconciled stay so; the next render
                // completes them.
                manifest.store(workspace, &lock)?;
                return Err(after(error, &done));
            }
        }
    }
    manifest.store(workspace, &lock)?;
    drop(publisher);
    let render = render_locked(
        store,
        &lock,
        renderer,
        policy,
        PublishMode::Incremental,
        observe,
    )?;
    Ok(ReconcileReport {
        done: Reconciliation {
            disposition: checked.disposition,
            steps: done,
        },
        render,
    })
}

/// A temporary-file tag from the last complete generation.
fn tag(manifest: &Manifest) -> String {
    manifest
        .last_complete()
        .and_then(|last| last.id.trim_start_matches("gen_").get(..12))
        .unwrap_or("reconcile")
        .to_owned()
}

/// Name the paths finished before `error` stopped the request.
fn after(error: RenderError, done: &[PathStep]) -> RenderError {
    if done.is_empty() {
        return error;
    }
    let finished: Vec<&str> = done.iter().map(|step| step.path.as_str()).collect();
    let note = format!(
        "; these listed paths were reconciled before it stopped and the next render completes \
         them: {}",
        finished.join(", ")
    );
    match error {
        RenderError::Reconcile {
            path,
            reason,
            changed,
        } => RenderError::Reconcile {
            path,
            reason: format!("{reason}{note}"),
            changed,
        },
        RenderError::Io { path, error } => RenderError::Io {
            path,
            error: std::io::Error::new(error.kind(), format!("{error}{note}")),
        },
        error => error,
    }
}

fn refuse(path: &str, reason: impl Into<String>) -> RenderError {
    RenderError::Reconcile {
        path: path.to_owned(),
        reason: reason.into(),
        changed: false,
    }
}

fn changed(path: &str, reason: impl Into<String>) -> RenderError {
    RenderError::Reconcile {
        path: path.to_owned(),
        reason: reason.into(),
        changed: true,
    }
}

const RENDER_AGAIN: &str =
    "run vault render, check vault conflicts, then reconcile again with what it lists";

/// Validate the whole request before anything changes.
fn check(
    publisher: &Publisher<'_>,
    manifest: &Manifest,
    request: &ReconcileRequest,
) -> Result<Reconciliation, RenderError> {
    let disposition = request.disposition;
    if request.paths.is_empty() {
        return Err(refuse(
            "",
            "list at least one drifted path; vault conflicts lists them",
        ));
    }
    if request.paths.len() > MAX_PATHS {
        return Err(refuse(
            "",
            format!("list at most {MAX_PATHS} paths in one request"),
        ));
    }
    if disposition == Disposition::Discard && !request.discard_observed {
        return Err(refuse(
            &request.paths[0],
            format!(
                "discard drops the observed edit from the vault; pass --discard-observed to \
                 confirm. The observed bytes are kept under {DISCARDED_DIR}/ until you remove \
                 them"
            ),
        ));
    }
    let mut seen = BTreeSet::new();
    let mut names = Names::default();
    let mut steps = Vec::with_capacity(request.paths.len());
    for text in &request.paths {
        let path = PlanPath::new(text.as_str())
            .map_err(|_| refuse(text, "is not a vault-relative path the renderer could own"))?;
        if !seen.insert(path.as_str().to_lowercase()) {
            return Err(refuse(text, "is listed twice"));
        }
        steps.push(check_path(
            publisher,
            manifest,
            disposition,
            &path,
            &mut names,
        )?);
    }
    Ok(Reconciliation { disposition, steps })
}

/// Names chosen earlier in the same request.
#[derive(Default)]
struct Names {
    notes: BTreeSet<String>,
    relocated: BTreeSet<String>,
}

fn check_path(
    publisher: &Publisher<'_>,
    manifest: &Manifest,
    disposition: Disposition,
    path: &PlanPath,
    names: &mut Names,
) -> Result<PathStep, RenderError> {
    let text = path.as_str();
    if let Some(released) = manifest.released().find(|released| &released.path == path) {
        return Err(refuse(
            text,
            format!(
                "was already released to you; its record is projected at {}",
                released.relocated_to
            ),
        ));
    }
    let drift = manifest.drifted(path);
    let owned = manifest.get(path);
    if owned.is_none() {
        return Err(refuse(
            text,
            "is not a generated file the renderer owns; vault conflicts lists the drifted paths",
        ));
    }
    if drift.is_none() && disposition != Disposition::ReleasePath {
        return Err(refuse(
            text,
            "has no recorded drift, so there is nothing to reconcile; vault render diagnoses \
             edits made since the last render, and vault conflicts lists them",
        ));
    }
    let on_disk = at(publisher, path, &split(text).0, split(text).1)?;
    if let Some(drift) = drift {
        let expected = match &drift.observed {
            Some(sha256) => OnDisk::Regular {
                sha256: sha256.clone(),
            },
            None if drift.kind == DriftKind::Edited => OnDisk::Other,
            None => OnDisk::Absent,
        };
        if on_disk != expected {
            return Err(changed(
                text,
                format!("changed since the last render diagnosed it; {RENDER_AGAIN}"),
            ));
        }
        if on_disk == OnDisk::Other && disposition != Disposition::ReleasePath {
            return Err(refuse(
                text,
                "holds a link, folder or special file, which reconcile never moves; move it \
                 yourself, then run vault render",
            ));
        }
    }
    let mut observed = Vec::new();
    if let OnDisk::Regular { sha256 } = &on_disk
        && drift.is_some()
        && !manifest.accepts(path, sha256)
    {
        observed.push((text.to_owned(), Role::Edited, sha256.clone()));
    }
    if let Some(drift) = drift {
        for (role, list) in [
            (Role::ConflictCopy, &drift.copies),
            (Role::Moved, &drift.renamed_to),
        ] {
            for other in list {
                let (parents, name) = split(other);
                match at(publisher, path, &parents, name)? {
                    OnDisk::Absent => {}
                    OnDisk::Regular { sha256 } => observed.push((other.clone(), role, sha256)),
                    OnDisk::Other => {
                        return Err(refuse(
                            text,
                            format!(
                                "{other} is a link, folder or special file, which reconcile \
                                 never moves; move it yourself, then run vault render"
                            ),
                        ));
                    }
                }
            }
        }
    }
    let mut step = PathStep {
        path: path.clone(),
        kind: drift.map(|drift| drift.kind),
        projects: drift
            .and_then(|drift| drift.projects)
            .or_else(|| owned.and_then(|record| record.projects)),
        observed: Vec::with_capacity(observed.len()),
        regenerate: disposition != Disposition::ReleasePath,
        relocated_to: None,
    };
    if disposition == Disposition::ReleasePath {
        step.relocated_to = Some(relocation(publisher, manifest, path, names)?);
    }
    for (file, role, sha256) in observed {
        let human = file.split('/').next().is_some_and(layout::is_human_root);
        let handling = match disposition {
            Disposition::ReleasePath => Handling::Left,
            _ if human => Handling::Left,
            Disposition::PreserveAsNote => Handling::MovedToNotes {
                to: note_name(publisher, path, split(&file).1, &mut names.notes)?,
            },
            Disposition::Discard => Handling::SetAside {
                to: aside(DISCARDED_DIR, &sha256, &file),
            },
            Disposition::ProposeChange => Handling::SetAside {
                to: aside(PROPOSED_DIR, &sha256, &file),
            },
        };
        step.observed.push(Observed {
            path: file,
            role,
            sha256,
            handling,
        });
    }
    Ok(step)
}

/// Parent components and file name of a vault-relative path.
fn split(path: &str) -> (Vec<&str>, &str) {
    let mut components: Vec<&str> = path.split('/').collect();
    let name = components.pop().unwrap_or_default();
    (components, name)
}

/// What is at `parents/name` in the vault, without following links. `label`
/// names the drifted path in errors.
fn at(
    publisher: &Publisher<'_>,
    label: &PlanPath,
    parents: &[&str],
    name: &str,
) -> Result<OnDisk, RenderError> {
    match publisher.walk(label, parents, false)? {
        Walk::Directory(directory) => inspect_named(&directory, label.as_str(), name),
        Walk::Missing => Ok(OnDisk::Absent),
        Walk::Unsafe => Ok(OnDisk::Other),
    }
}

/// `local/conflicts/<discarded|proposed>/<hash prefix>/<vault path>`.
fn aside(area: &str, sha256: &Digest, path: &str) -> String {
    let hex = sha256.as_str().trim_start_matches("sha256:");
    format!("{area}/{}/{path}", &hex[..16.min(hex.len())])
}

/// `name`, then `stem 2.ext`, `stem 3.ext`, …: the first that is free in
/// [`NOTES_FOLDER`] and not chosen earlier in this request.
fn note_candidates(name: &str) -> impl Iterator<Item = String> + '_ {
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, Some(extension)),
        _ => (name, None),
    };
    std::iter::once(name.to_owned()).chain((2..=MAX_CANDIDATES).map(move |n| {
        extension.map_or_else(
            || format!("{stem} {n}"),
            |extension| format!("{stem} {n}.{extension}"),
        )
    }))
}

fn note_name(
    publisher: &Publisher<'_>,
    label: &PlanPath,
    name: &str,
    taken: &mut BTreeSet<String>,
) -> Result<String, RenderError> {
    let parents: Vec<&str> = NOTES_FOLDER.split('/').collect();
    for candidate in note_candidates(name) {
        if taken.contains(&candidate.to_lowercase()) {
            continue;
        }
        if at(publisher, label, &parents, &candidate)? == OnDisk::Absent {
            taken.insert(candidate.to_lowercase());
            return Ok(format!("{NOTES_FOLDER}/{candidate}"));
        }
    }
    Err(refuse(
        label.as_str(),
        format!("{NOTES_FOLDER}/ has no free name for {name}; tidy it and reconcile again"),
    ))
}

/// The path a released note's projection moves to: `<stem>-<n>.md` beside
/// it (beside the original, for a note already relocated), free on disk, in
/// the manifest and in this request.
fn relocation(
    publisher: &Publisher<'_>,
    manifest: &Manifest,
    path: &PlanPath,
    names: &mut Names,
) -> Result<PlanPath, RenderError> {
    let text = path.as_str();
    // A relocated note released in turn counts up from the original name.
    let original = manifest
        .released()
        .find(|released| &released.relocated_to == path)
        .map_or(text, |released| released.path.as_str());
    let markdown_note = original
        .strip_suffix(".md")
        .filter(|_| path.components().next() == Some(layout::CASES));
    let Some(stem_path) = markdown_note else {
        return Err(refuse(
            text,
            "only Markdown notes under Cases/ can be released; use preserve-as-note or discard \
             for this file",
        ));
    };
    let relocated: BTreeSet<String> = manifest
        .released()
        .map(|released| released.relocated_to.as_str().to_lowercase())
        .collect();
    for n in 2..=MAX_CANDIDATES {
        let Ok(candidate) = PlanPath::new(format!("{stem_path}-{n}.md")) else {
            break;
        };
        let folded = candidate.as_str().to_lowercase();
        if manifest.claims(candidate.as_str())
            || relocated.contains(&folded)
            || names.relocated.contains(&folded)
        {
            continue;
        }
        let (parents, name) = split(candidate.as_str());
        if at(publisher, path, &parents, name)? == OnDisk::Absent {
            names.relocated.insert(folded);
            return Ok(candidate);
        }
    }
    Err(refuse(
        text,
        "no free path is left beside it to project its record at",
    ))
}

/// Move or set aside every observed file of one checked step. Returns the
/// step with the destinations actually used.
fn apply(
    publisher: &Publisher<'_>,
    workspace: &Workspace,
    tag: &str,
    mut step: PathStep,
) -> Result<PathStep, RenderError> {
    for observed in &mut step.observed {
        let label = &step.path;
        let (parents, name) = split(&observed.path);
        let Walk::Directory(directory) = publisher.walk(label, &parents, false)? else {
            return Err(changed(
                label.as_str(),
                format!(
                    "{} moved before it could be reconciled; {RENDER_AGAIN}",
                    observed.path
                ),
            ));
        };
        match &mut observed.handling {
            Handling::Left => {}
            Handling::MovedToNotes { to } => {
                let notes: Vec<&str> = NOTES_FOLDER.split('/').collect();
                let Walk::Directory(target) = publisher.walk(label, &notes, true)? else {
                    return Err(RenderError::UnsafeDirectory("vault Notes/Reconciled/"));
                };
                let first = split(to).1.to_owned();
                let mut moved = None;
                for candidate in note_candidates_from(&first, name) {
                    match renameat_with(
                        &directory,
                        name,
                        &target,
                        candidate.as_str(),
                        RenameFlags::NOREPLACE,
                    ) {
                        Ok(()) => {
                            moved = Some(candidate);
                            break;
                        }
                        Err(Errno::EXIST) => {}
                        Err(Errno::NOENT) => {
                            return Err(changed(
                                label.as_str(),
                                format!(
                                    "{} disappeared before it could be moved; {RENDER_AGAIN}",
                                    observed.path
                                ),
                            ));
                        }
                        Err(error) => return Err(RenderError::io(observed.path.as_str(), error)),
                    }
                }
                let Some(moved) = moved else {
                    return Err(refuse(
                        label.as_str(),
                        format!("{NOTES_FOLDER}/ has no free name for {name}"),
                    ));
                };
                fsync(&target).map_err(|error| RenderError::io(NOTES_FOLDER, error))?;
                fsync(&directory)
                    .map_err(|error| RenderError::io(observed.path.as_str(), error))?;
                *to = format!("{NOTES_FOLDER}/{moved}");
            }
            Handling::SetAside { to } => {
                let Some(bytes) = read_regular(&directory, name, &observed.path)? else {
                    return Err(changed(
                        label.as_str(),
                        format!(
                            "{} is gone or too large to set aside; {RENDER_AGAIN}",
                            observed.path
                        ),
                    ));
                };
                let sha256 = hash_bytes(&bytes);
                if sha256 != observed.sha256 {
                    return Err(changed(
                        label.as_str(),
                        format!(
                            "{} changed after it was checked; {RENDER_AGAIN}",
                            observed.path
                        ),
                    ));
                }
                keep(workspace, to, &bytes, &sha256, tag)?;
                match remove_verified(&directory, tag, &observed.path, name, &sha256)? {
                    Removal::Removed | Removal::Vanished => {}
                    Removal::Changed => {
                        return Err(changed(
                            label.as_str(),
                            format!(
                                "{} changed while it was being set aside and is back in place; \
                                 its earlier bytes are kept at {to}; {RENDER_AGAIN}",
                                observed.path
                            ),
                        ));
                    }
                }
            }
        }
    }
    Ok(step)
}

/// The planned note name first, then the usual candidates for `name`.
fn note_candidates_from<'a>(first: &str, name: &'a str) -> impl Iterator<Item = String> + 'a {
    std::iter::once(first.to_owned()).chain(note_candidates(name))
}

/// Write `bytes` to the workspace-relative `to` in the conflicts area,
/// without replacing; an existing file there must already hold them.
fn keep(
    workspace: &Workspace,
    to: &str,
    bytes: &[u8],
    sha256: &Digest,
    tag: &str,
) -> Result<(), RenderError> {
    let local = open_local(workspace.root())?;
    let inside = to
        .strip_prefix("local/")
        .expect("set-aside paths are under local/");
    let (parents, name) = split(inside);
    let directory =
        conflicts_directory(&local, &parents, true)?.expect("created directories exist");
    if !write_bytes_atomically(&directory, to, name, bytes, tag, false)? {
        let existing = read_regular(&directory, name, to)?;
        if existing.as_deref().map(hash_bytes).as_ref() != Some(sha256) {
            return Err(RenderError::io(
                to,
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "a different file is already kept here; nothing was removed from the vault",
                ),
            ));
        }
    }
    Ok(())
}

/// The manifest after one applied step: a regenerated path the step emptied
/// is forgotten, so the render creates it without replacing; one that still
/// holds generated bytes keeps its ownership and loses its drift record; a
/// released path is recorded as released.
fn settle(
    publisher: &Publisher<'_>,
    manifest: Manifest,
    step: &PathStep,
    since: &str,
) -> Result<Manifest, RenderError> {
    // Every file moved into `Notes/Reconciled/` is recorded, so the manifest
    // names every path the renderer wrote; it is the person's from now on.
    let mut manifest = manifest;
    for observed in &step.observed {
        if let Handling::MovedToNotes { to } = &observed.handling {
            manifest = manifest.hand_over(HandedOver {
                path: to.clone(),
                sha256: observed.sha256.clone(),
                from: step.path.clone(),
                since: since.to_owned(),
            });
        }
    }
    if let Some(relocated_to) = &step.relocated_to {
        return Ok(manifest.release(Released {
            path: step.path.clone(),
            relocated_to: relocated_to.clone(),
            projects: step.projects,
            since: since.to_owned(),
        }));
    }
    let (parents, name) = split(step.path.as_str());
    let now = at(publisher, &step.path, &parents, name)?;
    Ok(match now {
        OnDisk::Regular { sha256 } if manifest.accepts(&step.path, &sha256) => {
            manifest.clear_drift(&step.path)
        }
        _ => manifest.forget(&step.path),
    })
}

/// A renderer whose plans publish each released path's projection at its
/// relocated path, with every generated link following it.
pub(crate) struct Relocated<'r, R> {
    inner: &'r R,
    relocations: BTreeMap<PlanPath, PlanPath>,
}

impl<'r, R> Relocated<'r, R> {
    pub(crate) const fn new(inner: &'r R, relocations: BTreeMap<PlanPath, PlanPath>) -> Self {
        Self { inner, relocations }
    }
}

impl<R: CaseRenderer> CaseRenderer for Relocated<'_, R> {
    fn render(
        &self,
        snapshot: &VerifiedSnapshot,
        policy: &ViewPolicy,
    ) -> Result<RenderPlan, RenderError> {
        let _links = layout::LinkRoot::enter(policy.vault_prefix.as_deref());
        relocate(self.inner.render(snapshot, policy)?, &self.relocations)
    }

    fn announce(
        &self,
        plan: &RenderPlan,
        snapshot: &VerifiedSnapshot,
        policy: &ViewPolicy,
        health: &EntryHealth,
    ) -> Result<RenderPlan, RenderError> {
        let _links = layout::LinkRoot::enter(policy.vault_prefix.as_deref());
        relocate(
            self.inner.announce(plan, snapshot, policy, health)?,
            &self.relocations,
        )
    }
}

/// `plan` with each entry at a released path moved to its relocated path,
/// and every wikilink to a released note rewritten to the relocated one.
/// Links are only ever built from plan paths ([`layout::wikilink`]), so a
/// target is always followed by `|`, `\|` (in a table), `#` or `]`.
/// Applying it twice changes nothing more.
pub fn relocate(
    plan: RenderPlan,
    relocations: &BTreeMap<PlanPath, PlanPath>,
) -> Result<RenderPlan, RenderError> {
    if relocations.is_empty() {
        return Ok(plan);
    }
    let links: Vec<(String, String)> = relocations
        .iter()
        .map(|(from, to)| {
            (
                format!("[[{}", layout::link_target(from)),
                format!("[[{}", layout::link_target(to)),
            )
        })
        .collect();
    let mut builder = PlanBuilder::new(plan.generation().clone());
    for entry in plan.entries() {
        let path = relocations
            .get(entry.path())
            .unwrap_or(entry.path())
            .clone();
        let content = match std::str::from_utf8(entry.content()) {
            Ok(text) if text.contains("[[") => rewrite_links(text, &links).into_bytes(),
            _ => entry.content().to_vec(),
        };
        builder.add(entry.phase(), path, content, entry.projects())?;
    }
    Ok(builder.finish())
}

fn rewrite_links(text: &str, links: &[(String, String)]) -> String {
    let mut text = text.to_owned();
    for (from, to) in links {
        if !text.contains(from.as_str()) {
            continue;
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(at) = rest.find(from.as_str()) {
            out.push_str(&rest[..at]);
            let after = &rest[at + from.len()..];
            if after.starts_with(['|', '\\', '#', ']']) {
                out.push_str(to);
            } else {
                out.push_str(from);
            }
            rest = after;
        }
        out.push_str(rest);
        text = out;
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispositions_round_trip_through_their_names() {
        for disposition in Disposition::ALL {
            assert_eq!(Disposition::parse(disposition.as_str()), Some(disposition));
        }
        assert_eq!(Disposition::parse("all"), None);
    }

    #[test]
    fn links_are_rewritten_only_at_a_target_boundary() {
        let links = vec![(
            "[[Cases/c/Questions/q_1".to_owned(),
            "[[Cases/c/Questions/q_1-2".to_owned(),
        )];
        let text = "a [[Cases/c/Questions/q_1|Q]] b [[Cases/c/Questions/q_1\\|Q]] \
                    c [[Cases/c/Questions/q_1#^x|Q]] d \"[[Cases/c/Questions/q_1]]\" \
                    e [[Cases/c/Questions/q_12|other]]";
        let rewritten = rewrite_links(text, &links);
        assert_eq!(
            rewritten,
            "a [[Cases/c/Questions/q_1-2|Q]] b [[Cases/c/Questions/q_1-2\\|Q]] \
             c [[Cases/c/Questions/q_1-2#^x|Q]] d \"[[Cases/c/Questions/q_1-2]]\" \
             e [[Cases/c/Questions/q_12|other]]"
        );
        assert_eq!(rewrite_links(&rewritten, &links), rewritten);
    }

    #[test]
    fn note_names_count_up_beside_the_original() {
        let names: Vec<String> = note_candidates("q_1.md").take(3).collect();
        assert_eq!(names, ["q_1.md", "q_1 2.md", "q_1 3.md"]);
        let names: Vec<String> = note_candidates("q_1.md.conflict").take(2).collect();
        assert_eq!(names, ["q_1.md.conflict", "q_1.md 2.conflict"]);
        let names: Vec<String> = note_candidates("README").take(2).collect();
        assert_eq!(names, ["README", "README 2"]);
    }
}
