//! Whether the vault on disk is one complete generation, read without
//! rendering.
//!
//! The answer comes from `local/render-manifest.json` and the render lock
//! only; no vault file is read and nothing is written. Canonical reads
//! (`brief`, `show`, `next`) never consult either, so a broken projection
//! never changes what the CLI reports about canonical state.

use investigation_store::Workspace;

use crate::{
    CompletedGeneration, Drift, Generation, Manifest, ManifestStatus, RenderError, RenderState,
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
        manifest: loaded.status,
    })
}
