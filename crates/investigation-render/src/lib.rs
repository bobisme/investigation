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
//! - [`lock`]: [`RenderLock`] serializes renders. It is a separate file from
//!   the commit lock, so commits continue while a render runs.
//!
//! Seams for later work:
//!
//! - Path ownership manifest: implement [`Ownership`] over
//!   `local/render-manifest.json` and update it from [`PublishReport`]. Owned
//!   deletions are computed from the manifest and the plan, not from
//!   directory listings.
//! - Entity notes, Overview and `Start Here.md`: replace the bodies in
//!   [`templates`]; paths stay in [`layout`]. Bump [`RENDERER_VERSION`]
//!   whenever output bytes change for the same snapshot.
//! - Render generations: [`Generation`] binds a plan to its snapshot heads,
//!   renderer version and policy. [`Phase`] gives the publish order (records,
//!   then views, then the entry note); the observer passed to
//!   [`Publisher::publish`] sees every step, for progress state and for
//!   crash-at-each-file tests.
//! - Drift: a [`Refusal`] leaves the file in place; drift handling records the
//!   would-be content from the plan entry.
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
pub mod plan;
pub mod policy;
pub mod publish;
pub mod snapshot;
pub mod templates;

use std::time::Duration;

use investigation_store::{Store, durable::DurableFs};

pub use error::RenderError;
pub use lock::RenderLock;
pub use plan::{
    CaseRenderer, Generation, Phase, PlanBuilder, PlanEntry, PlanPath, RENDERER_VERSION,
    RenderPlan, VaultRenderer, render,
};
pub use policy::{Audience, ViewPolicy};
pub use publish::{
    Action, Decision, NoManifest, OnDisk, Ownership, PublishReport, PublishStep, Publisher, Refusal,
};
pub use snapshot::{CaseSnapshot, VerifiedSnapshot};

/// Default wait for another render to finish before reporting busy.
pub const RENDER_LOCK_WAIT: Duration = Duration::from_secs(2);

/// Lock, read one verified snapshot, plan it and publish it into `vault/`.
///
/// The render lock is taken first, so two renders never interleave and a
/// later render never publishes an older snapshot over a newer one. The
/// commit lock is held only inside [`VerifiedSnapshot::read`].
pub fn render_vault<F: DurableFs>(
    store: &mut Store<F>,
    renderer: &impl CaseRenderer,
    policy: &ViewPolicy,
    ownership: &dyn Ownership,
    observe: &mut dyn FnMut(&PublishStep<'_>),
) -> Result<PublishReport, RenderError> {
    let lock = RenderLock::acquire(store.workspace(), RENDER_LOCK_WAIT)?;
    let snapshot = VerifiedSnapshot::read(store)?;
    let plan = renderer.render(&snapshot, policy)?;
    Publisher::open(store.workspace(), &lock)?.publish(&plan, ownership, observe)
}
