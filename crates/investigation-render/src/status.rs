//! Whether the vault on disk is one complete generation, read without
//! rendering.
//!
//! The answer comes from `local/render-manifest.json` and the render lock
//! only; no vault file is read and nothing is written. Canonical reads
//! (`brief`, `show`, `next`) never consult either, so a broken projection
//! never changes what the CLI reports about canonical state.
//!
//! [`foreign_files`] adds the one fact the manifest cannot hold: which files
//! in the vault nobody owns. It lists directories, reads no file content and
//! writes nothing. [`working_policy`] is the view policy a working-vault
//! render uses, so a frontend can compute the generation that
//! [`VaultStatus::is_current`] compares with.

use investigation_store::Workspace;

use crate::{
    CompletedGeneration, Drift, Generation, Manifest, ManifestStatus, Publisher, Released,
    RenderError, RenderState, ViewPolicy,
    compat::Profile,
    lock::{self, Probe},
};

/// The projection's generation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultState {
    /// No render has completed or started.
    Unrendered,
    /// The last render processed every entry of `generation`.
    Complete { generation: String },
    /// A render of `to` is running now.
    Updating { from: Option<String>, to: String },
    /// The vault mixes generations: a render of `generation` stopped on an
    /// error, or was killed (`interrupted`). The next render completes it.
    Partial {
        last_complete: Option<String>,
        generation: String,
        reason: String,
        interrupted: bool,
    },
}

impl VaultState {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Unrendered => "unrendered",
            Self::Complete { .. } => "complete",
            Self::Updating { .. } => "updating",
            Self::Partial { .. } => "partial",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultStatus {
    pub state: VaultState,
    pub last_complete: Option<CompletedGeneration>,
    /// How the manifest was found. A rebuilt manifest reports `Unrendered`.
    pub manifest: ManifestStatus,
    /// Drifted owned paths as the last render diagnosed them, in path order.
    /// Read from the manifest; the vault is not inspected, so drift that
    /// happened since the last render shows up after the next one.
    pub drift: Vec<Drift>,
    /// Paths released to a person (`vault reconcile --disposition
    /// release-path`), with where their projections moved, in path order.
    pub released: Vec<Released>,
}

impl VaultStatus {
    /// Whether the vault is complete at exactly `current`, the generation a
    /// render of today's snapshot would produce. `false` means stale or
    /// incomplete; render again.
    pub fn is_current(&self, current: &Generation) -> bool {
        matches!(&self.state, VaultState::Complete { generation } if generation == current.id())
    }
}

/// Read the projection state. A manifest saying `updating` while no render
/// holds the lock is reported as an interrupted `partial`.
pub fn vault_status(workspace: &Workspace) -> Result<VaultStatus, RenderError> {
    // Held (when free) until the manifest is read, so a render that starts
    // meanwhile cannot make a running render look interrupted.
    let probe = lock::probe(workspace)?;
    let loaded = Manifest::read(workspace)?;
    let running = matches!(probe, Probe::Running);
    drop(probe);
    let manifest = loaded.manifest;
    let last_complete = manifest.last_complete().cloned();
    let from = last_complete
        .as_ref()
        .map(|generation| generation.id.clone());
    let state = match manifest.state().clone() {
        RenderState::Complete => match &from {
            Some(generation) => VaultState::Complete {
                generation: generation.clone(),
            },
            None => VaultState::Unrendered,
        },
        RenderState::Updating { generation } if running => VaultState::Updating {
            from,
            to: generation,
        },
        RenderState::Updating { generation } => VaultState::Partial {
            last_complete: from,
            generation,
            reason: "the render was interrupted before it recorded its outcome".to_owned(),
            interrupted: true,
        },
        RenderState::Partial { generation, reason } => VaultState::Partial {
            last_complete: from,
            generation,
            reason,
            interrupted: false,
        },
    };
    Ok(VaultStatus {
        state,
        last_complete,
        drift: manifest.drift().cloned().collect(),
        released: manifest.released().cloned().collect(),
        manifest: loaded.status,
    })
}

/// The view policy of the working vault: the default audience, with
/// derivative previews when the workspace's vault policy includes approved
/// derivatives, and the optional viewer features (native Bases, the case
/// map) that the bundled compatibility profile allows for its stated
/// `viewer_release`. An absent, unknown or out-of-range release gets plain
/// Markdown only. In existing-vault mode it carries the managed prefix, so
/// every link starts there.
pub fn working_policy(workspace: &Workspace) -> ViewPolicy {
    let vault = &workspace.config().vault_policy;
    let resolved = Profile::bundled().resolve(vault.viewer_release.as_deref());
    ViewPolicy::default()
        .with_derivative_previews(vault.include_approved_derivatives)
        .with_viewer(resolved)
        .with_vault_prefix(vault.existing().map(|(_, prefix)| prefix))
}

/// Vault-relative paths of files outside the human-owned areas that the
/// manifest does not own, sorted: what a render would preserve and report as
/// unknown. `None` while a render runs, since its files are not owned yet.
/// Hidden entries are skipped and links are listed, never followed. Reads
/// directory listings only and writes nothing, not even the lock file.
pub fn foreign_files(workspace: &Workspace) -> Result<Option<Vec<String>>, RenderError> {
    // Held (when free) for the walk, so no render starts meanwhile.
    let probe = lock::probe(workspace)?;
    let Some(publisher) = Publisher::inspect(workspace, &probe)? else {
        return Ok(None);
    };
    let manifest = Manifest::read(workspace)?.manifest;
    publisher
        .unknown_files(&|path| manifest.claims(path))
        .map(Some)
}
