//! The path ownership manifest, `local/render-manifest.json`.
//!
//! The renderer manages the casebook, not the whole vault. A vault path is
//! owned only while the manifest records it together with the SHA-256 of the
//! bytes last generated there. Every publisher decision checks that record:
//!
//! | On disk                         | Owned, hash matches | Owned, hash differs | Not owned           |
//! |---------------------------------|---------------------|---------------------|---------------------|
//! | absent                          | create              | create              | create (no-replace) |
//! | bytes equal the plan            | keep                | keep                | keep, then adopt    |
//! | other bytes                     | replace             | refuse: drifted     | refuse: foreign     |
//! | retired path (not in the plan)  | delete              | refuse: drifted     | never considered    |
//! | link, folder or special file    | refuse              | refuse              | refuse              |
//!
//! Adopting a file whose bytes already equal the plan is how a lost or
//! unreadable manifest is rebuilt: nothing can be destroyed by claiming bytes
//! the renderer would write anyway, and every adoption is reported. Any other
//! file stays foreign until a human reconciles it, so deleting the manifest
//! and rendering again destroys no file.
//!
//! The manifest is written atomically and only after every file operation of
//! a render, so it never claims a file before that file exists with the
//! recorded bytes. Render generations (`updating`, `partial`, last complete)
//! extend [`Manifest::updated`] and [`Manifest::store`]; the manifest stays
//! the last file a render publishes.

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
    Action, Decision, Generation, OnDisk, Ownership, PlanEntry, PlanPath, PublishReport, Refusal,
    RenderError, RenderLock, RenderPlan, layout, lock::open_local, publish::write_bytes_atomically,
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
    /// The generation that last wrote or adopted these bytes.
    pub generation: String,
    /// The record the file projects, when it projects exactly one.
    pub projects: Option<Id>,
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

/// Owned paths with their last generated hashes. Also the [`Ownership`]
/// every publication of the working vault is checked against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    workspace_id: Option<Id>,
    generation: Option<CompletedGeneration>,
    owned: BTreeMap<PlanPath, OwnedPath>,
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

    /// The manifest after a publication, and the paths it adopted.
    ///
    /// Written files are recorded with their new hash. A file that already
    /// held the plan's bytes is recorded too; when it was not owned before it
    /// is adopted and listed. Removed retired paths, and retired paths found
    /// empty, are released. Every other outcome (refused, drifted, unsafe)
    /// leaves the previous record as it was, so drift stays visible and
    /// nothing becomes owned without proof. `report` may be partial, from a
    /// render that stopped early; only a report covering every plan entry
    /// moves the last complete generation.
    pub fn updated(&self, plan: &RenderPlan, report: &PublishReport) -> (Self, Vec<PlanPath>) {
        let generation = plan.generation();
        let entries: BTreeMap<&PlanPath, &PlanEntry> = plan
            .entries()
            .iter()
            .map(|entry| (entry.path(), entry))
            .collect();
        let mut owned = self.owned.clone();
        let mut adopted = Vec::new();
        let mut planned = 0;
        for outcome in &report.outcomes {
            let path = &outcome.path;
            let Some(entry) = entries.get(path) else {
                if matches!(outcome.action, Action::Removed | Action::Unchanged) {
                    owned.remove(path);
                }
                continue;
            };
            planned += 1;
            let record = || OwnedPath {
                path: path.clone(),
                sha256: entry.sha256().clone(),
                generation: generation.id().to_owned(),
                projects: entry.projects(),
            };
            match outcome.action {
                Action::Written => {
                    owned.insert(path.clone(), record());
                }
                Action::Unchanged => match owned.get(path) {
                    Some(previous) if &previous.sha256 == entry.sha256() => {}
                    Some(_) => {
                        owned.insert(path.clone(), record());
                    }
                    None => {
                        owned.insert(path.clone(), record());
                        adopted.push(path.clone());
                    }
                },
                Action::Removed | Action::Refused(_) => {}
            }
        }
        let complete = planned == plan.entries().len();
        let manifest = Self {
            workspace_id: Some(generation.workspace_id()),
            generation: if complete {
                Some(CompletedGeneration::from(generation))
            } else {
                self.generation.clone()
            },
            owned,
        };
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
        let wire = Wire {
            schema_version: MANIFEST_VERSION.to_owned(),
            workspace_id: self.workspace_id,
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
        let mut owned = BTreeMap::new();
        let mut folded = BTreeSet::new();
        for record in wire.owned {
            let path = PlanPath::new(record.path).map_err(|error| {
                Problem::Invalid(format!("the manifest lists an invalid path: {error}"))
            })?;
            if layout::is_human_owned(&path) {
                return Err(Problem::Invalid(format!(
                    "the manifest claims the human-owned path {path:?}"
                )));
            }
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
                },
            );
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
            owned,
        })
    }
}

impl Ownership for Manifest {
    fn decide(&self, entry: &PlanEntry, on_disk: &OnDisk) -> Decision {
        match on_disk {
            OnDisk::Absent => Decision::Write,
            OnDisk::Regular { sha256 } if sha256 == entry.sha256() => Decision::Keep,
            OnDisk::Regular { sha256 } => match self.owned.get(entry.path()) {
                Some(record) if &record.sha256 == sha256 => Decision::Write,
                Some(_) => Decision::Refuse(Refusal::Drifted),
                None => Decision::Refuse(Refusal::NotProvenOwned),
            },
            OnDisk::Other => Decision::Refuse(Refusal::NotARegularFile),
        }
    }

    fn retired(&self, plan: &RenderPlan) -> Vec<PlanPath> {
        let planned: BTreeSet<&PlanPath> = plan.entries().iter().map(PlanEntry::path).collect();
        self.owned
            .keys()
            .filter(|path| !planned.contains(path))
            .cloned()
            .collect()
    }

    fn decide_removal(&self, path: &PlanPath, on_disk: &OnDisk) -> Decision {
        match (self.owned.get(path), on_disk) {
            (None, _) => Decision::Refuse(Refusal::NotProvenOwned),
            (Some(_), OnDisk::Absent) => Decision::Keep,
            (Some(record), OnDisk::Regular { sha256 }) if &record.sha256 == sha256 => {
                Decision::Remove
            }
            (Some(_), OnDisk::Regular { .. }) => Decision::Refuse(Refusal::Drifted),
            (Some(_), OnDisk::Other) => Decision::Refuse(Refusal::NotARegularFile),
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
    generation: Option<WireGeneration>,
    owned: Vec<WireOwned>,
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
}
