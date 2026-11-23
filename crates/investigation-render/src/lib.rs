//! Projection of verified canonical snapshots into files.
//!
//! The vault is a view of the journal, never a second source of truth. This
//! crate splits that view into three steps, each with one job:
//!
//! ```text
//!  journal ──Store::cases()──▶ VerifiedSnapshot          commit lock held only while reading
//!                                   │
//!            CaseRenderer::render(&snapshot, &ViewPolicy)  pure: no filesystem, clock or randomness
//!                                   ▼
//!                       RenderPlan { generation, entries: [{path, content, sha256}] }
//!                                   │
//!            Publisher::publish(&plan, &dyn Ownership, observe)   holds a RenderLock (local/render.lock)
//!                                   ▼
//!                       vault/ files written by temp + atomic rename, never through symlinks
//! ```
//!
//! - [`snapshot`]: [`VerifiedSnapshot`] is the only planner input from disk. It
//!   is read once under the store's commit lock, which is released before any
//!   planning or publishing starts. A render therefore never blocks a commit.
//! - [`plan`]: [`RenderPlan`] is plain data with a stable order. The same
//!   snapshot and policy yield a byte-identical plan, so a plan can be diffed,
//!   tested and reused by exports without touching a working vault.
//!   [`CaseRenderer`] is the planner interface; [`VaultRenderer`] is the vault
//!   implementation and [`render`] its entry point.
//! - [`layout`] and [`templates`]: identity-based paths and the note bodies.
//!   The current templates are a deliberately small skeleton that shows IDs,
//!   kinds, revisions and snapshot markers only, never authored text.
//! - [`policy`]: [`ViewPolicy`] carries the audience the plan is built for.
//! - [`publish`]: [`Publisher`] applies a plan. Each path is checked against an
//!   [`Ownership`] decision before any write. [`NoManifest`] is the
//!   conservative default: it creates absent files and keeps identical ones,
//!   and refuses everything else.
//! - [`manifest`]: [`Manifest`] is the path ownership record in
//!   `local/render-manifest.json` and the [`Ownership`] the working vault is
//!   published under. A path is owned only with the hash of the bytes last
//!   generated there; replacement and deletion require those bytes. Retired
//!   paths are computed from the manifest and the plan, never from directory
//!   listings. A lost manifest is rebuilt conservatively.
//! - [`lock`]: [`RenderLock`] serializes renders. It is a separate file from
//!   the commit lock, so commits continue while a render runs.
//!
//! Seams for later work:
//!
//! - Entity notes, Overview and `Start Here.md`: replace the bodies in
//!   [`templates`]; paths stay in [`layout`]. Bump [`RENDERER_VERSION`]
//!   whenever output bytes change for the same snapshot.
//! - Render generations: [`Manifest::store`] is the last write of a render;
//!   [`Manifest::updated`] already keeps the last complete generation and
//!   accepts a partial report. [`Generation`] binds a plan to its snapshot heads,
//!   renderer version and policy. [`Phase`] gives the publish order (records,
//!   then views, then the entry note); the observer passed to
//!   [`Publisher::publish`] sees every step, for progress state and for
//!   crash-at-each-file tests.
//! - Drift: [`Refusal::Drifted`] marks an owned file whose bytes differ from
//!   the manifest hash; it is left in place and stays owned at the old hash,
//!   so it is refused again on every render until reconciled. Drift handling
//!   records the would-be content from the plan entry.
//! - Classification-aware projection: extend [`ViewPolicy`] and consult it in
//!   templates for every emitted field.
//! - Incremental render: the planner still produces the full plan;
//!   [`Ownership`] and the on-disk hash decide what to skip.
//! - Exports: implement [`CaseRenderer`] for another layout and publish into a
//!   fresh destination.
//!
//! Rendering works with any viewer application absent or closed. The
//! filesystem is the only interface.

pub mod error;
pub mod layout;
pub mod lock;
pub mod manifest;
pub mod plan;
pub mod policy;
pub mod publish;
pub mod snapshot;
pub mod templates;

use std::time::Duration;

use investigation_store::{Store, durable::DurableFs};

pub use error::RenderError;
pub use lock::RenderLock;
pub use manifest::{
    CompletedGeneration, LoadedManifest, MANIFEST_VERSION, Manifest, ManifestStatus, OwnedPath,
    RENDER_MANIFEST,
};
pub use plan::{
    CaseRenderer, Generation, Phase, PlanBuilder, PlanEntry, PlanPath, RENDERER_VERSION,
    RenderPlan, VaultRenderer, render,
};
pub use policy::{Audience, ViewPolicy};
pub use publish::{
    Action, Decision, NoManifest, OnDisk, Outcome, Ownership, PublishReport, PublishStep,
    Publisher, Refusal,
};
pub use snapshot::{CaseSnapshot, VerifiedSnapshot};

/// Default wait for another render to finish before reporting busy.
pub const RENDER_LOCK_WAIT: Duration = Duration::from_secs(2);

/// What one working-vault render did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderReport {
    pub publish: PublishReport,
    /// How the ownership manifest was found before the render.
    pub manifest: ManifestStatus,
    /// Files that already held the planned bytes and became owned.
    pub adopted: Vec<PlanPath>,
    /// Files outside the human-owned roots that the manifest does not own.
    /// They are preserved and never adopted silently.
    pub unknown: Vec<String>,
}

/// Lock, read one verified snapshot, plan it and publish it into `vault/`
/// under the path ownership manifest, then store the manifest.
///
/// The render lock is taken first, so two renders never interleave and a
/// later render never publishes an older snapshot over a newer one. The
/// commit lock is held only inside [`VerifiedSnapshot::read`]. The manifest
/// is written after every file operation; when a file operation fails, the
/// outcomes reached so far are still recorded before the error is returned,
/// so files already written stay owned.
pub fn render_vault<F: DurableFs>(
    store: &mut Store<F>,
    renderer: &impl CaseRenderer,
    policy: &ViewPolicy,
    observe: &mut dyn FnMut(&PublishStep<'_>),
) -> Result<RenderReport, RenderError> {
    let lock = RenderLock::acquire(store.workspace(), RENDER_LOCK_WAIT)?;
    let snapshot = VerifiedSnapshot::read(store)?;
    let plan = renderer.render(&snapshot, policy)?;
    let workspace = store.workspace();
    let loaded = Manifest::load(workspace, &lock)?;
    let publisher = Publisher::open(workspace, &lock)?;
    let mut reached = Vec::new();
    let published = publisher.publish(&plan, &loaded.manifest, &mut |step| {
        reached.push(step.outcome.clone());
        observe(step);
    });
    let publish = match published {
        Ok(report) => report,
        Err(error) => {
            let partial = PublishReport {
                generation: plan.generation().clone(),
                outcomes: reached,
            };
            let (manifest, _) = loaded.manifest.updated(&plan, &partial);
            // The render error is the one to report; a failed manifest write
            // only leaves files it would have claimed foreign, never lost.
            let _ = manifest.store(workspace, &lock);
            return Err(error);
        }
    };
    let (manifest, adopted) = loaded.manifest.updated(&plan, &publish);
    manifest.store(workspace, &lock)?;
    let unknown = publisher.unknown_files(&|path| manifest.owns(path))?;
    Ok(RenderReport {
        publish,
        manifest: loaded.status,
        adopted,
        unknown,
    })
}
