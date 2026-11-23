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
//!   [`note`] is the shared entity-note skeleton: flat `inv_` frontmatter
//!   ([`yaml`]), display title and aliases, the tool-owned banner, the basis
//!   table and fixed sections (summary and caveat, typed links with reasons,
//!   evidence selectors with excerpt anchors, next action or blocker).
//!   Authored text passes through the context-specific escapers in [`text`]
//!   and [`yaml`]; links come only from [`layout::wikilink`].
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
//! - [`marker`]: every Markdown note names the snapshot it reflects in flat
//!   frontmatter ([`NoteMarker`]): a case note its case head
//!   (`inv_render_sequence`, `inv_snapshot`), `Start Here.md` the generation
//!   (`inv_generation`). [`marker::stamp_plan`] adds or verifies them after
//!   the templates run, so they are the only place a note names its basis
//!   snapshot, and a commit to one case never changes another case's notes.
//! - Generations: the manifest carries the render state
//!   ([`RenderState`]: complete, updating, partial) and a write-ahead intent
//!   of planned hashes, stored before the first vault write and again, last,
//!   after the final one. A render killed at any file leaves a state the next
//!   render completes. [`sweep`] removes the temporaries such a render leaves.
//!
//! Seams for later work:
//!
//! - Per-type notes: `records` holds one module per record kind (evidence
//!   kinds delegate to [`evidence`]), each choosing its headline fields and
//!   typed backlinks between the summary and the typed links. Effective
//!   state comes from the freshness overlay and the objection policy, never
//!   from the recorded lifecycle alone. `## Reviews and objections` marks a
//!   stale or disputed review on the Review note (`inv_freshness`) and on its
//!   subject's note (`inv_review_freshness`). Every reference in a body links
//!   both ways: dependency edges under "Relies on"/"Cited by", other named
//!   records under "Also names" and "names it in".
//! - Overview and `Start Here.md`: extend the bodies in [`templates`] on top
//!   of [`note::NoteContext`]; paths stay in [`layout`]. Reviews whose
//!   subject is the case itself belong on the Overview. Bump
//!   [`RENDERER_VERSION`] whenever output bytes change for the same snapshot.
//! - Evidence-family notes: [`evidence`] adds Source, Artifact and
//!   Observation sections (acquisition, source assessment, role, integrity,
//!   lineage to the original acquisitions, derivation record, quoted evidence
//!   with explicit limits, evidentiary use). Recorded quotes are the only
//!   captured text in the vault, as bounded excerpts in a labelled callout;
//!   retained bytes are never read by the planner.
//! - Safe Markdown rendering: every new authored or captured string goes
//!   through the [`text`] function for its context; block excerpts use
//!   [`text::fenced_block`]. Add hostile cases to
//!   `tests/fixtures/hostile-text.json`.
//! - Vault status (`vault status`): [`vault_status`] reports
//!   [`VaultState`] (unrendered, complete, updating, partial) and the last
//!   complete generation from the manifest and the lock alone;
//!   [`VaultStatus::is_current`] compares it with the generation of today's
//!   snapshot. [`RenderReport::swept`] lists cleaned leftovers.
//! - Drift ([`drift`]): an owned file that was edited (its body, or a
//!   property a viewer's table rewrote), deleted, moved, or that has a sync
//!   client's conflict copy beside it is refused ([`Refusal::Drifted`]) and
//!   left exactly as found; nothing is recreated, replaced or deleted. It
//!   stays owned at the old hash, so it is diagnosed again on every render
//!   until reconciled. The would-be content is retained under
//!   `local/conflicts/<generation>/<path>`, and a [`Drift`] record per path
//!   is stored in the manifest and reported in [`RenderReport::drift`] and
//!   [`VaultStatus::drift`] (for `vault status` and `vault conflicts`).
//!   Reconciling dispositions are a separate seam: they read those records
//!   and act on one path at a time. The supported contract stays: do not edit
//!   generated notes directly; use `Notes/` or controlled mutations.
//! - Classification-aware projection: notes ask the core `Classifier` per
//!   field through [`note::NoteContext`]; extend [`ViewPolicy`] (and its
//!   fingerprint) for further rules.
//! - Incremental render: the planner still produces the full plan;
//!   [`Ownership`] and the on-disk hash decide what to skip.
//! - Exports: implement [`CaseRenderer`] for another layout and publish into a
//!   fresh destination.
//!
//! Rendering works with any viewer application absent or closed. The
//! filesystem is the only interface.

pub mod drift;
pub mod error;
pub mod evidence;
pub mod layout;
pub mod lock;
pub mod manifest;
pub mod marker;
pub mod note;
pub mod plan;
pub mod policy;
pub mod publish;
pub(crate) mod records;
pub mod snapshot;
pub mod status;
pub mod sweep;
pub mod templates;
pub mod text;
pub mod yaml;

use std::time::Duration;

use investigation_store::{Store, durable::DurableFs};

pub use drift::{CONFLICTS_DIR, Drift, DriftKind};
pub use error::RenderError;
pub use lock::RenderLock;
pub use manifest::{
    CompletedGeneration, LoadedManifest, MANIFEST_VERSION, Manifest, ManifestStatus, OwnedPath,
    RENDER_MANIFEST, RenderState,
};
pub use marker::NoteMarker;
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
pub use status::{VaultState, VaultStatus, vault_status};
pub use sweep::{Swept, SweptAction};

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
    /// Temporary files a killed render had left, cleaned before publishing.
    pub swept: Vec<Swept>,
    /// Every drifted owned path after this render, in path order: the ones
    /// it diagnosed and, after an early stop, earlier records for paths it
    /// did not reach. Drift does not fail a render.
    pub drift: Vec<Drift>,
}

/// Lock, read one verified snapshot, plan it and publish it into `vault/`
/// under the path ownership manifest, then store the manifest.
///
/// The render lock is taken first, so two renders never interleave and a
/// later render never publishes an older snapshot over a newer one. The
/// commit lock is held only inside [`VerifiedSnapshot::read`]. Then, in order:
///
/// 1. leftovers of a killed render are swept ([`sweep`]);
/// 2. the manifest is stored as `updating`, with the planned hashes as a
///    write-ahead intent ([`Manifest::begin`]); no vault file has changed yet;
/// 3. the plan is published: records, then views, then the entry note, then
///    retired paths are removed. Paths with a conflict copy beside them are
///    held, and drifted paths are refused; the rest render ([`drift`]);
/// 4. drift is diagnosed, and the content withheld from drifted paths is
///    retained under `local/conflicts/<generation>/`;
/// 5. the manifest is stored with the outcomes and the drift records,
///    `complete` or `partial` ([`Manifest::updated`]). It is the last file
///    the render writes.
///
/// When a file operation fails, the outcomes reached so far are still
/// recorded, with state `partial` and the error as its reason, before the
/// error is returned. A render killed at any point leaves `updating` (or the
/// earlier manifest), from which the next render completes.
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

    let mut targets = loaded.manifest.known_paths();
    targets.extend(plan.entries().iter().map(|entry| entry.path().clone()));
    let mut swept = sweep::sweep_local(workspace)?;
    swept.extend(publisher.sweep(&targets, &|path, sha256| {
        loaded.manifest.accepts(path, sha256)
    })?);

    let survey = drift::survey(&publisher, &loaded.manifest, &plan)?;
    let ownership = drift::Guard {
        manifest: &loaded.manifest,
        survey: &survey,
    };
    loaded.manifest.begin(&plan).store(workspace, &lock)?;
    let mut reached = Vec::new();
    let published = publisher.publish(&plan, &ownership, &mut |step| {
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
            // The render error is the one to report. A failed manifest write
            // leaves the `updating` record, whose intent still covers every
            // file this render may have written.
            let _ = manifest
                .failed(plan.generation(), error.to_string())
                .store(workspace, &lock);
            return Err(error);
        }
    };
    let (manifest, adopted) = loaded.manifest.updated(&plan, &publish);
    let diagnosed = drift::diagnose(&publisher, &loaded.manifest, &plan, &publish, &survey)
        .and_then(|mut drifts| {
            drift::retain(workspace, &loaded.manifest, &plan, &mut drifts)?;
            Ok(drifts)
        });
    let drifts = match diagnosed {
        Ok(drifts) => drifts,
        Err(error) => {
            // Every vault file was processed; only the drift record is
            // incomplete. Earlier drift records are kept.
            let _ = manifest
                .failed(plan.generation(), error.to_string())
                .store(workspace, &lock);
            return Err(error);
        }
    };
    let manifest = manifest.with_drift(&publish, drifts);
    manifest.store(workspace, &lock)?;
    let unknown = publisher.unknown_files(&|path| manifest.owns(path))?;
    Ok(RenderReport {
        publish,
        manifest: loaded.status,
        adopted,
        unknown,
        swept,
        drift: manifest.drift().cloned().collect(),
    })
}
