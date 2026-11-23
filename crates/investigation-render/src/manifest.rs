//! The path ownership manifest, `local/render-manifest.json`.
//!
//! The renderer manages the casebook, not the whole vault. A vault path is
//! owned only while the manifest records it together with the SHA-256 of the
//! bytes last generated there. Every publisher decision checks that record:
//!
//! | On disk                         | Owned, hash matches | Owned, hash differs | Not owned           |
//! |---------------------------------|---------------------|---------------------|---------------------|
//! | absent                          | refuse: drifted     | refuse: drifted     | create (no-replace) |
//! | bytes equal the plan            | keep                | keep                | keep, then adopt    |
//! | other bytes                     | replace             | refuse: drifted     | refuse: foreign     |
//! | retired path (not in the plan)  | delete              | refuse: drifted     | never considered    |
//! | link, folder or special file    | refuse              | refuse              | refuse              |
//!
//! An owned file that is absent was deleted or moved by someone else; it is
//! drift (see [`crate::drift`]) and is not recreated, except right after a
//! killed render (state `updating`), which may have removed it itself.
//! Drifted paths stay owned at their last generated hash, and the manifest
//! keeps one [`Drift`] record for each, so drift is visible without a render.
//!
//! Adopting a file whose bytes already equal the plan is how a lost or
//! unreadable manifest is rebuilt: nothing can be destroyed by claiming bytes
//! the renderer would write anyway, and every adoption is reported. A case
//! note an earlier render left unchanged, naming an earlier head of its
//! case, is adopted the same way ([`crate::incremental`]).
//!
//! For a case note, the record also keeps the basis its bytes name and the
//! generation that last confirmed them. A render lets an unchanged note keep
//! that basis only while the last complete generation used the same renderer
//! version and view policy; bases recorded under other rules are dropped. Any other
//! file stays foreign until a human reconciles it, so deleting the manifest
//! and rendering again destroys no file.
//!
//! The manifest is written atomically and only after every file operation of
//! a render, so it never claims a file before that file exists with the
//! recorded bytes.
//!
//! ## Render generations
//!
//! The manifest also carries the render state ([`RenderState`]): `complete`,
//! `updating` or `partial`, beside the last complete generation. A render
//! writes it twice:
//!
//! 1. Before any vault file changes, [`Manifest::begin`] records `updating`
//!    and, as a write-ahead intent, the hash of every planned file whose
//!    bytes would change ([`Manifest::pending`]). Owned paths are not touched.
//! 2. After every file operation, [`Manifest::updated`] records the outcomes:
//!    `complete` when every entry was processed (the intent is then
//!    discarded), `partial` otherwise (the intent is kept for the paths not
//!    reached). This second write is the last file a render publishes.
//!
//! A render killed between the two leaves `updating` with its intent. The
//! intent is what makes the next render complete: a file whose bytes equal a
//! pending hash was written by the interrupted render, so it is treated as
//! owned at that hash (replaced or removed like any owned file) even when the
//! snapshot moved on meanwhile. Pending hashes accumulate across consecutive
//! unfinished renders and are dropped per path once a render processes it.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
};

use investigation_core::{
    identity::Id,
    wire::{Digest, Snapshot},
};
use investigation_store::Workspace;
use rustix::{
    fs::{FileType, Mode, OFlags, fstat, openat},
    io::Errno,
};
use serde::{Deserialize, Serialize};

use crate::{
    Action, Decision, Drift, DriftKind, Generation, OnDisk, Ownership, PlanEntry, PlanPath,
    PublishReport, Refusal, RenderError, RenderLock, RenderPlan, drift::CONFLICTS_DIR, layout,
    lock::open_local, publish::write_bytes_atomically,
};

/// Workspace-relative path of the manifest.
pub const RENDER_MANIFEST: &str = "local/render-manifest.json";
/// The manifest format this build reads and writes.
pub const MANIFEST_VERSION: &str = "inv.render-manifest.v0";

const FILE_NAME: &str = "render-manifest.json";
const MAX_MANIFEST_BYTES: u64 = 256 << 20;

/// One owned vault path and the bytes last generated there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedPath {
    pub path: PlanPath,
    pub sha256: Digest,
    /// The generation that last confirmed these bytes: it wrote or adopted
    /// them, or found them already holding its planned bytes.
    pub generation: String,
    /// The record the file projects, when it projects exactly one.
    pub projects: Option<Id>,
    /// For a case note, the case head its bytes name as their basis
    /// ([`crate::NoteMarker::Case`]). It can be older than the confirming
    /// generation's head when the note did not change (see
    /// [`crate::incremental`]).
    pub basis: Option<Snapshot>,
}

/// The case head a note's bytes name, when they name one.
fn basis_of(entry: &PlanEntry) -> Option<Snapshot> {
    match crate::NoteMarker::read(entry.content())? {
        crate::NoteMarker::Case(head) => Some(head),
        crate::NoteMarker::Generation(_) => None,
    }
}

/// The last generation whose every plan entry was processed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedGeneration {
    pub id: String,
    pub renderer: String,
    pub policy: String,
    /// Case heads in case ID order.
    pub heads: Vec<(Id, Snapshot)>,
}

/// Whether the files on disk are one generation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RenderState {
    /// No render has run, or the last one processed every plan entry.
    #[default]
    Complete,
    /// A render of `generation` started and has not recorded its outcome:
    /// it is running, or it was killed.
    Updating { generation: String },
    /// A render of `generation` stopped on an error before every entry was
    /// processed. The vault mixes generations.
    Partial { generation: String, reason: String },
}

impl RenderState {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Updating { .. } => "updating",
            Self::Partial { .. } => "partial",
        }
    }
}

/// Owned paths with their last generated hashes. Also the [`Ownership`]
/// every publication of the working vault is checked against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    workspace_id: Option<Id>,
    generation: Option<CompletedGeneration>,
    state: RenderState,
    owned: BTreeMap<PlanPath, OwnedPath>,
    /// Hashes an unfinished render may have written, per path.
    pending: BTreeMap<PlanPath, BTreeSet<Digest>>,
    /// Owned paths diagnosed as drifted by the last render that reached them.
    drifted: BTreeMap<PlanPath, Drift>,
}

/// How [`Manifest::load`] found the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestStatus {
    Loaded,
    /// No manifest yet: every existing file is foreign.
    Missing,
    /// The manifest was unreadable or belonged to another workspace. It is
    /// rebuilt conservatively from an empty record, as if missing.
    Rebuilt {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedManifest {
    pub manifest: Manifest,
    pub status: ManifestStatus,
}

impl Manifest {
    /// An empty record: nothing is owned.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, path: &PlanPath) -> Option<&OwnedPath> {
        self.owned.get(path)
    }

    /// Whether the vault-relative path is owned.
    pub fn owns(&self, path: &str) -> bool {
        PlanPath::new(path).is_ok_and(|path| self.owned.contains_key(&path))
    }

    /// Owned paths in path order.
    pub fn owned(&self) -> impl Iterator<Item = &OwnedPath> {
        self.owned.values()
    }

    pub const fn workspace_id(&self) -> Option<Id> {
        self.workspace_id
    }

    pub fn last_complete(&self) -> Option<&CompletedGeneration> {
        self.generation.as_ref()
    }

    pub const fn state(&self) -> &RenderState {
        &self.state
    }

    /// Drifted owned paths in path order, as the last render that reached
    /// each one diagnosed it.
    pub fn drift(&self) -> impl Iterator<Item = &Drift> {
        self.drifted.values()
    }

    /// The drift record for `path`, when it is drifted.
    pub fn drifted(&self, path: &PlanPath) -> Option<&Drift> {
        self.drifted.get(path)
    }

    /// The same record with the drift `drifts` diagnosed for the paths
    /// `report` reached. Records for paths the report did not reach are
    /// kept; a reached path without a new record is no longer drifted.
    /// Records for paths that are not owned are dropped.
    pub(crate) fn with_drift(mut self, report: &PublishReport, drifts: Vec<Drift>) -> Self {
        for outcome in &report.outcomes {
            self.drifted.remove(&outcome.path);
        }
        for drift in drifts {
            self.drifted.insert(drift.path.clone(), drift);
        }
        let owned = &self.owned;
        self.drifted.retain(|path, _| owned.contains_key(path));
        self
    }

    /// Paths an unfinished render may have written, with the hashes it
    /// intended. Empty after a complete render.
    pub fn pending(&self) -> impl Iterator<Item = (&PlanPath, &BTreeSet<Digest>)> {
        self.pending.iter()
    }

    /// Whether `sha256` at `path` is bytes this renderer generated: the last
    /// hash recorded as owned, or a hash an unfinished render intended.
    pub fn accepts(&self, path: &PlanPath, sha256: &Digest) -> bool {
        self.owned
            .get(path)
            .is_some_and(|record| &record.sha256 == sha256)
            || self
                .pending
                .get(path)
                .is_some_and(|hashes| hashes.contains(sha256))
    }

    /// Every path the manifest knows: owned or pending.
    pub fn known_paths(&self) -> BTreeSet<PlanPath> {
        self.owned
            .keys()
            .chain(self.pending.keys())
            .cloned()
            .collect()
    }

    /// The record to store before publishing `plan`: state `updating`, and
    /// the hash of every planned file whose bytes would change added to the
    /// write-ahead intent. Ownership and the last complete generation are
    /// unchanged.
    pub fn begin(&self, plan: &RenderPlan) -> Self {
        let generation = plan.generation();
        let mut next = self.clone();
        next.workspace_id = Some(generation.workspace_id());
        next.state = RenderState::Updating {
            generation: generation.id().to_owned(),
        };
        next.add_intent(plan, &BTreeSet::new());
        next
    }

    /// Entries in a human-owned area are never intended: the publisher
    /// refuses them, and a manifest naming one would be rejected on load.
    fn add_intent(&mut self, plan: &RenderPlan, reached: &BTreeSet<&PlanPath>) {
        for entry in plan.entries() {
            let path = entry.path();
            if layout::is_human_owned(path) {
                continue;
            }
            let owned_here = self
                .owned
                .get(path)
                .is_some_and(|record| &record.sha256 == entry.sha256());
            if !owned_here && !reached.contains(path) {
                self.pending
                    .entry(path.clone())
                    .or_default()
                    .insert(entry.sha256().clone());
            }
        }
    }

    /// The same record marked `partial` with `reason`, for a render that
    /// stopped on an error.
    pub fn failed(mut self, generation: &Generation, reason: impl Into<String>) -> Self {
        self.state = RenderState::Partial {
            generation: generation.id().to_owned(),
            reason: reason.into(),
        };
        self
    }

    /// The write-ahead record for one extra write of `entry` after the plan
    /// was published (the entry note announcing projection health): its hash
    /// is added to the intent, and a `complete` state reads `updating` until
    /// the write is recorded, so a render killed during it is completed by
    /// the next one. A `partial` state is kept.
    pub(crate) fn intending(&self, entry: &PlanEntry, generation: &Generation) -> Self {
        let mut next = self.clone();
        next.pending
            .entry(entry.path().clone())
            .or_default()
            .insert(entry.sha256().clone());
        if next.state == RenderState::Complete {
            next.state = RenderState::Updating {
                generation: generation.id().to_owned(),
            };
        }
        next
    }

    /// The same record with `entry`'s path owned at its hash, after the
    /// write [`Self::intending`] announced. Drift and pending hashes are
    /// unchanged.
    pub(crate) fn owning(mut self, entry: &PlanEntry, generation: &Generation) -> Self {
        self.owned.insert(
            entry.path().clone(),
            OwnedPath {
                path: entry.path().clone(),
                sha256: entry.sha256().clone(),
                generation: generation.id().to_owned(),
                projects: entry.projects(),
                basis: basis_of(entry),
            },
        );
        self
    }

    /// The manifest after a publication, and the paths it adopted.
    ///
    /// Written files are recorded with their new hash. A file that already
    /// held the plan's bytes is recorded too, as confirmed by this
    /// generation; when it was not owned before it is adopted and listed.
    /// Each record keeps the basis a case note names. Removed retired paths,
    /// and retired paths found empty, are released. Every other outcome (refused, drifted, unsafe)
    /// leaves the previous record as it was, so drift stays visible and
    /// nothing becomes owned without proof. `report` may be partial, from a
    /// render that stopped early; only a report covering every plan entry
    /// moves the last complete generation and sets the state `complete`.
    /// Otherwise the state is `partial` and the write-ahead intent is kept
    /// for every path the report did not reach. A file found holding bytes
    /// an unfinished render intended is recovered, not reported as adopted.
    pub fn updated(&self, plan: &RenderPlan, report: &PublishReport) -> (Self, Vec<PlanPath>) {
        let generation = plan.generation();
        let entries: BTreeMap<&PlanPath, &PlanEntry> = plan
            .entries()
            .iter()
            .map(|entry| (entry.path(), entry))
            .collect();
        let mut owned = self.owned.clone();
        let mut adopted = Vec::new();
        let complete = report
            .outcomes
            .iter()
            .filter(|outcome| entries.contains_key(&outcome.path))
            .count()
            == plan.entries().len();
        // A recorded basis vouches that the owned bytes are what the last
        // complete generation's renderer and policy planned at that basis
        // ([`crate::incremental`]). Bases recorded under any other renderer
        // or policy are dropped, so such notes are restamped, never kept.
        let same_rules = self.generation.as_ref().is_some_and(|last| {
            last.renderer == generation.renderer() && last.policy == generation.policy()
        });
        for outcome in &report.outcomes {
            let path = &outcome.path;
            let Some(entry) = entries.get(path) else {
                if matches!(outcome.action, Action::Removed | Action::Unchanged) {
                    owned.remove(path);
                }
                continue;
            };
            let record = || OwnedPath {
                path: path.clone(),
                sha256: entry.sha256().clone(),
                generation: generation.id().to_owned(),
                projects: entry.projects(),
                basis: basis_of(entry).filter(|_| complete || same_rules),
            };
            match outcome.action {
                Action::Written => {
                    owned.insert(path.clone(), record());
                }
                Action::Unchanged => match owned.get(path) {
                    Some(_) => {
                        owned.insert(path.clone(), record());
                    }
                    None => {
                        owned.insert(path.clone(), record());
                        if !self
                            .pending
                            .get(path)
                            .is_some_and(|hashes| hashes.contains(entry.sha256()))
                        {
                            adopted.push(path.clone());
                        }
                    }
                },
                Action::Removed | Action::Refused(_) => {}
            }
        }
        if complete && !same_rules {
            for record in owned.values_mut() {
                if record.generation != generation.id() {
                    record.basis = None;
                }
            }
        }
        // Drift stays recorded while its path is still refused; the render
        // replaces the records of the paths it reached ([`Self::with_drift`]).
        let reached: BTreeMap<&PlanPath, &Action> = report
            .outcomes
            .iter()
            .map(|outcome| (&outcome.path, &outcome.action))
            .collect();
        let drifted = self
            .drifted
            .iter()
            .filter(|(path, _)| owned.contains_key(*path))
            .filter(|(path, _)| {
                reached
                    .get(path)
                    .is_none_or(|action| matches!(action, Action::Refused(_)))
            })
            .map(|(path, drift)| (path.clone(), drift.clone()))
            .collect();
        let mut manifest = Self {
            workspace_id: Some(generation.workspace_id()),
            generation: if complete {
                Some(CompletedGeneration::from(generation))
            } else {
                self.generation.clone()
            },
            state: RenderState::Complete,
            owned,
            pending: BTreeMap::new(),
            drifted,
        };
        if !complete {
            let reached: BTreeSet<&PlanPath> = report
                .outcomes
                .iter()
                .map(|outcome| &outcome.path)
                .collect();
            manifest.pending = self
                .pending
                .iter()
                .filter(|(path, _)| !reached.contains(path))
                .map(|(path, hashes)| (path.clone(), hashes.clone()))
                .collect();
            manifest.add_intent(plan, &reached);
            manifest.state = RenderState::Partial {
                generation: generation.id().to_owned(),
                reason: "the render stopped before every plan entry was processed".to_owned(),
            };
        }
        (manifest, adopted)
    }

    /// Read `local/render-manifest.json` without following links.
    ///
    /// A missing, unreadable or foreign manifest yields an empty record and a
    /// status saying why, never an error: an empty record owns nothing, so it
    /// can only make the publisher more cautious. A manifest in an unknown
    /// format is an error instead, so a newer tool's record is never
    /// overwritten by an older one.
    pub fn load(workspace: &Workspace, lock: &RenderLock) -> Result<LoadedManifest, RenderError> {
        if lock.workspace_root() != workspace.root() {
            return Err(RenderError::WrongWorkspace);
        }
        Self::read(workspace)
    }

    /// [`Manifest::load`] without the render lock, for status reads. The
    /// manifest is replaced atomically, so an unlocked read sees one whole
    /// version.
    pub(crate) fn read(workspace: &Workspace) -> Result<LoadedManifest, RenderError> {
        let rebuilt = |reason: String| {
            Ok(LoadedManifest {
                manifest: Self::new(),
                status: ManifestStatus::Rebuilt { reason },
            })
        };
        let local = open_local(workspace.root())?;
        let fd = match openat(
            &local,
            FILE_NAME,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => {
                return Ok(LoadedManifest {
                    manifest: Self::new(),
                    status: ManifestStatus::Missing,
                });
            }
            Err(Errno::LOOP) => return rebuilt("the manifest is a symbolic link".to_owned()),
            Err(error) => return Err(RenderError::io(RENDER_MANIFEST, error)),
        };
        let stat = fstat(&fd).map_err(|error| RenderError::io(RENDER_MANIFEST, error))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return rebuilt("the manifest is not a regular file".to_owned());
        }
        let mut bytes = Vec::new();
        File::from(fd)
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| RenderError::io(RENDER_MANIFEST, error))?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return rebuilt("the manifest is too large".to_owned());
        }
        match Self::from_json(&bytes) {
            Ok(manifest) if manifest.workspace_id != Some(workspace.id()) => {
                rebuilt("the manifest belongs to a different workspace".to_owned())
            }
            Ok(manifest) => Ok(LoadedManifest {
                manifest,
                status: ManifestStatus::Loaded,
            }),
            Err(Problem::Unsupported(version)) => Err(RenderError::UnsupportedManifest { version }),
            Err(Problem::Invalid(reason)) => rebuilt(reason),
        }
    }

    /// Replace `local/render-manifest.json` atomically. Call it only after
    /// every file operation of the render, so the manifest is published last.
    pub fn store(&self, workspace: &Workspace, lock: &RenderLock) -> Result<(), RenderError> {
        if lock.workspace_root() != workspace.root() {
            return Err(RenderError::WrongWorkspace);
        }
        let local = open_local(workspace.root())?;
        let tag = self
            .generation
            .as_ref()
            .and_then(|generation| generation.id.trim_start_matches("gen_").get(..12))
            .unwrap_or("manifest");
        write_bytes_atomically(
            &local,
            RENDER_MANIFEST,
            FILE_NAME,
            &self.to_json(),
            tag,
            true,
        )
        .map(|_| ())
    }

    /// Canonical JSON: fixed field order, owned paths sorted, trailing newline.
    pub fn to_json(&self) -> Vec<u8> {
        let (state, target, reason) = match &self.state {
            RenderState::Complete => (WireState::Complete, None, None),
            RenderState::Updating { generation } => {
                (WireState::Updating, Some(generation.clone()), None)
            }
            RenderState::Partial { generation, reason } => (
                WireState::Partial,
                Some(generation.clone()),
                Some(reason.clone()),
            ),
        };
        let wire = Wire {
            schema_version: MANIFEST_VERSION.to_owned(),
            workspace_id: self.workspace_id,
            state,
            target,
            reason,
            generation: self.generation.as_ref().map(|generation| WireGeneration {
                id: generation.id.clone(),
                renderer: generation.renderer.clone(),
                policy: generation.policy.clone(),
                heads: generation
                    .heads
                    .iter()
                    .map(|(case_id, head)| WireHead {
                        case_id: *case_id,
                        sequence: head.sequence,
                        head_hash: head.head_hash.clone(),
                    })
                    .collect(),
            }),
            owned: self
                .owned
                .values()
                .map(|record| WireOwned {
                    path: record.path.as_str().to_owned(),
                    sha256: record.sha256.clone(),
                    generation: record.generation.clone(),
                    projects: record.projects,
                    basis: record.basis.as_ref().map(|head| WireBasis {
                        sequence: head.sequence,
                        head_hash: head.head_hash.clone(),
                    }),
                })
                .collect(),
            pending: self
                .pending
                .iter()
                .map(|(path, hashes)| WirePending {
                    path: path.as_str().to_owned(),
                    sha256: hashes.iter().cloned().collect(),
                })
                .collect(),
            drifted: self
                .drifted
                .values()
                .map(|drift| WireDrift {
                    path: drift.path.as_str().to_owned(),
                    kind: drift.kind.as_str().to_owned(),
                    expected: drift.expected.clone(),
                    observed: drift.observed.clone(),
                    projects: drift.projects,
                    generated: drift.generated.clone(),
                    conflict: drift.conflict.clone(),
                    renamed_to: drift.renamed_to.clone(),
                    copies: drift.copies.clone(),
                    since: drift.since.clone(),
                })
                .collect(),
        };
        let mut bytes = serde_json::to_vec_pretty(&wire).expect("manifest serializes");
        bytes.push(b'\n');
        bytes
    }

    /// Parse and validate a manifest. Every path must be a valid plan path
    /// outside the human-owned roots, and no two may fold to the same name.
    fn from_json(bytes: &[u8]) -> Result<Self, Problem> {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|error| Problem::Invalid(format!("the manifest is not JSON: {error}")))?;
        match value
            .get("schema_version")
            .and_then(|version| version.as_str())
        {
            Some(MANIFEST_VERSION) => {}
            Some(version) => return Err(Problem::Unsupported(version.to_owned())),
            None => {
                return Err(Problem::Invalid(
                    "the manifest has no schema_version".to_owned(),
                ));
            }
        }
        let wire: Wire = serde_json::from_value(value)
            .map_err(|error| Problem::Invalid(format!("the manifest is malformed: {error}")))?;
        let checked = |path: String| {
            let path = PlanPath::new(path).map_err(|error| {
                Problem::Invalid(format!("the manifest lists an invalid path: {error}"))
            })?;
            if layout::is_human_owned(&path) {
                return Err(Problem::Invalid(format!(
                    "the manifest claims the human-owned path {path:?}"
                )));
            }
            Ok(path)
        };
        let state = match (wire.state, wire.target, wire.reason) {
            (WireState::Complete, None, None) => RenderState::Complete,
            (WireState::Updating, Some(generation), None) => RenderState::Updating { generation },
            (WireState::Partial, Some(generation), Some(reason)) => {
                RenderState::Partial { generation, reason }
            }
            _ => {
                return Err(Problem::Invalid(
                    "the manifest's render state is inconsistent".to_owned(),
                ));
            }
        };
        let mut pending = BTreeMap::new();
        for record in wire.pending {
            let path = checked(record.path)?;
            if record.sha256.is_empty() || pending.contains_key(&path) {
                return Err(Problem::Invalid(format!(
                    "the manifest lists a malformed pending path {path:?}"
                )));
            }
            pending.insert(path, record.sha256.into_iter().collect());
        }
        let mut owned = BTreeMap::new();
        let mut folded = BTreeSet::new();
        for record in wire.owned {
            let path = checked(record.path)?;
            if !folded.insert(path.as_str().to_lowercase()) {
                return Err(Problem::Invalid(format!(
                    "the manifest lists {path:?} twice"
                )));
            }
            owned.insert(
                path.clone(),
                OwnedPath {
                    path,
                    sha256: record.sha256,
                    generation: record.generation,
                    projects: record.projects,
                    basis: record.basis.map(|head| Snapshot {
                        sequence: head.sequence,
                        head_hash: head.head_hash,
                    }),
                },
            );
        }
        let mut drifted = BTreeMap::new();
        for record in wire.drifted {
            let drift = checked_drift(record, &owned)?;
            if drifted.insert(drift.path.clone(), drift).is_some() {
                return Err(Problem::Invalid(
                    "the manifest lists a drifted path twice".to_owned(),
                ));
            }
        }
        Ok(Self {
            workspace_id: wire.workspace_id,
            generation: wire.generation.map(|generation| CompletedGeneration {
                id: generation.id,
                renderer: generation.renderer,
                policy: generation.policy,
                heads: generation
                    .heads
                    .into_iter()
                    .map(|head| {
                        let snapshot = Snapshot {
                            sequence: head.sequence,
                            head_hash: head.head_hash,
                        };
                        (head.case_id, snapshot)
                    })
                    .collect(),
            }),
            state,
            owned,
            pending,
            drifted,
        })
    }
}

/// A drift record from the wire: an owned path, a known kind, a conflict file
/// inside the conflicts area, and relative vault paths.
fn checked_drift(
    record: WireDrift,
    owned: &BTreeMap<PlanPath, OwnedPath>,
) -> Result<Drift, Problem> {
    let invalid = |what: &str| Problem::Invalid(format!("the manifest lists {what}"));
    let path = PlanPath::new(record.path).map_err(|_| invalid("an invalid drifted path"))?;
    if !owned.contains_key(&path) {
        return Err(invalid("a drifted path it does not own"));
    }
    let kind = DriftKind::parse(&record.kind).ok_or_else(|| invalid("an unknown drift kind"))?;
    if let Some(conflict) = &record.conflict {
        let inside = conflict
            .strip_prefix(CONFLICTS_DIR)
            .and_then(|rest| rest.strip_prefix('/'))
            .and_then(|rest| rest.split_once('/'));
        let valid = inside.is_some_and(|(generation, rest)| {
            PlanPath::new(generation).is_ok() && PlanPath::new(rest).is_ok()
        });
        if !valid {
            return Err(invalid("a conflict file outside the conflicts area"));
        }
    }
    let relative = |path: &String| {
        !path.is_empty()
            && !path.starts_with('/')
            && path
                .split('/')
                .all(|component| !component.is_empty() && component != "." && component != "..")
    };
    if !record.renamed_to.iter().all(relative) || !record.copies.iter().all(relative) {
        return Err(invalid("a drift location outside the vault"));
    }
    Ok(Drift {
        path,
        kind,
        expected: record.expected,
        observed: record.observed,
        projects: record.projects,
        generated: record.generated,
        conflict: record.conflict,
        renamed_to: record.renamed_to,
        copies: record.copies,
        since: record.since,
    })
}

impl Ownership for Manifest {
    fn decide(&self, entry: &PlanEntry, on_disk: &OnDisk) -> Decision {
        match on_disk {
            // An owned file someone deleted or moved is drift; it is not
            // recreated. A killed render may have removed it itself, so
            // after one it is, unless it was already known drifted.
            OnDisk::Absent
                if self.owned.contains_key(entry.path())
                    && (self.drifted.contains_key(entry.path())
                        || !matches!(self.state, RenderState::Updating { .. })) =>
            {
                Decision::Refuse(Refusal::Drifted)
            }
            OnDisk::Absent => Decision::Write,
            OnDisk::Regular { sha256 } if sha256 == entry.sha256() => Decision::Keep,
            OnDisk::Regular { sha256 } if self.accepts(entry.path(), sha256) => Decision::Write,
            OnDisk::Regular { .. } if self.owned.contains_key(entry.path()) => {
                Decision::Refuse(Refusal::Drifted)
            }
            OnDisk::Regular { .. } => Decision::Refuse(Refusal::NotProvenOwned),
            OnDisk::Other => Decision::Refuse(Refusal::NotARegularFile),
        }
    }

    fn retired(&self, plan: &RenderPlan) -> Vec<PlanPath> {
        let planned: BTreeSet<&PlanPath> = plan.entries().iter().map(PlanEntry::path).collect();
        self.known_paths()
            .into_iter()
            .filter(|path| !planned.contains(path))
            .collect()
    }

    fn decide_removal(&self, path: &PlanPath, on_disk: &OnDisk) -> Decision {
        let owned = self.owned.contains_key(path);
        if !owned && !self.pending.contains_key(path) {
            return Decision::Refuse(Refusal::NotProvenOwned);
        }
        match on_disk {
            OnDisk::Absent => Decision::Keep,
            OnDisk::Regular { sha256 } if self.accepts(path, sha256) => Decision::Remove,
            OnDisk::Regular { .. } if owned => Decision::Refuse(Refusal::Drifted),
            OnDisk::Regular { .. } => Decision::Refuse(Refusal::NotProvenOwned),
            OnDisk::Other => Decision::Refuse(Refusal::NotARegularFile),
        }
    }
}

impl From<&Generation> for CompletedGeneration {
    fn from(generation: &Generation) -> Self {
        Self {
            id: generation.id().to_owned(),
            renderer: generation.renderer().to_owned(),
            policy: generation.policy().to_owned(),
            heads: generation.heads().to_vec(),
        }
    }
}

enum Problem {
    Unsupported(String),
    Invalid(String),
}

#[derive(Serialize, Deserialize)]
struct Wire {
    schema_version: String,
    workspace_id: Option<Id>,
    /// Absent in manifests written before render states existed.
    #[serde(default)]
    state: WireState,
    /// The generation being (or last) attempted while not complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    generation: Option<WireGeneration>,
    owned: Vec<WireOwned>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending: Vec<WirePending>,
    /// Absent in manifests written before drift was recorded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    drifted: Vec<WireDrift>,
}

#[derive(Serialize, Deserialize)]
struct WireDrift {
    path: String,
    kind: String,
    expected: Digest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed: Option<Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    projects: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generated: Option<Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    conflict: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    renamed_to: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    copies: Vec<String>,
    since: String,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum WireState {
    #[default]
    Complete,
    Updating,
    Partial,
}

#[derive(Serialize, Deserialize)]
struct WirePending {
    path: String,
    sha256: Vec<Digest>,
}

#[derive(Serialize, Deserialize)]
struct WireGeneration {
    id: String,
    renderer: String,
    policy: String,
    heads: Vec<WireHead>,
}

#[derive(Serialize, Deserialize)]
struct WireHead {
    case_id: Id,
    sequence: u64,
    head_hash: Digest,
}

#[derive(Serialize, Deserialize)]
struct WireOwned {
    path: String,
    sha256: Digest,
    generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    projects: Option<Id>,
    /// Absent in manifests written before incremental rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    basis: Option<WireBasis>,
}

/// A case note's basis; its path names the case.
#[derive(Serialize, Deserialize)]
struct WireBasis {
    sequence: u64,
    head_hash: Digest,
}
