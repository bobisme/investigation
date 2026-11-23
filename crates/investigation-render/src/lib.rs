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
//! - [`policy`]: [`ViewPolicy`] carries the audience the plan is built for,
//!   whether derivative previews are allowed, and the optional viewer
//!   features [`compat`] resolves for the recorded viewer release.
//! - Who owns what: [`layout::VAULT_AREAS`] is the one ownership table.
//!   `Start Here.md` prints it, and every rule below enforces it through
//!   [`layout::area`]. `Notes/`, `Boards/`, `Inbox/` and hidden folders are a
//!   person's: the publisher refuses plan entries there before inspecting
//!   anything, never removes, sweeps or adopts there, the write-ahead intent
//!   never names them, and a manifest that claims one is rejected. Drift
//!   detection only reads them (to find a moved note). So their bytes are
//!   identical across renders, re-renders, generation switches, killed
//!   renders, drift and a lost manifest. A human note is not evidence until
//!   it is added to a case, which retains its exact bytes by hash.
//! - [`attachments`]: `Attachments/` holds approved previews of reviewed
//!   derivatives only, with the derivative's exact recorded bytes, when the
//!   policy opts in and the audience may see them; never an original.
//! - [`publish`]: [`Publisher`] applies a plan. Each path is checked against an
//!   [`Ownership`] decision before any write. [`NoManifest`] is the
//!   conservative default: it creates absent files and keeps identical ones,
//!   and refuses everything else. [`Ownership::approves_attachment`] gates
//!   every `Attachments/` entry and approves nothing by default.
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
//!   the templates run, and a commit to one case never changes another
//!   case's notes. A working-vault render lets a note whose bytes did not
//!   otherwise change keep the earlier head it names ([`incremental`]).
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
//! - Case Overview: after the shared opening, `records::overview` writes the
//!   bounded opening summary (question, decision, answer or choice, scope,
//!   strongest original evidence, strongest outstanding objection, the step
//!   `next` ranks first), the title/synopsis/full ladder (answer with the
//!   brief's caveat, contradiction and freshness and a current or stale
//!   synopsis; what the answer cites; its exact Sources and Artifacts), the
//!   case's own reviews with the latest blindspot review and retrospective,
//!   one section chosen by what the case holds (`inv_focus`: explanation,
//!   measurement, evaluation or research), and the `brief`/`frontier`
//!   commands to resume. It reuses core rules (freshness overview, objection
//!   policy, synopsis status, `next` ranking) and never the CLI crate.
//! - Table views: `records::views` plans one static Markdown table note per
//!   §12.5 view under `Cases/<case-id>/Views/` (questions through the
//!   comparison matrix, reviews, epistemic frontier and timeline), each with
//!   its default columns, bounded rows with an overflow link, a stable sort
//!   key and cells built only from [`text`] escapers. A view with no rows is
//!   not planned, except the frontier and the timeline. No table computes a
//!   score or rank; the Overview links every planned view under `## Views`.
//! - Native Bases ([`bases`]): when the view policy allows every
//!   [`bases::FEATURES`] ([`ViewPolicy::allows_all`]), one `.base` file
//!   beside each planned table view except the comparison matrix, filtered
//!   by the exact case ID and the view's record kinds, listing the same rows.
//!   [`working_policy`] turns it on only when the workspace states a viewer
//!   release that the compatibility profile ([`compat`]) resolves to every
//!   Base feature; otherwise the vault is plain Markdown.
//! - Case map ([`map`]): `Cases/<case-id>/Map.canvas`, a bounded JSON Canvas
//!   of file nodes and labelled edges with a mode-sensitive focus, planned
//!   only when the view policy allows the `canvas.*` features, with
//!   `Map Legend.md` and `Map Left Out.md` beside it and a `## Map` section
//!   in the Overview.
//! - `Start Here.md` ([`start_here`]): what the vault is, projection
//!   health, who owns which folder, and per case (capped, in case ID order)
//!   its Overview, last committed snapshot, answer, pending questions,
//!   outstanding reviews, unresolved blindspot reviews and retrospectives,
//!   each list bounded with a link to the Overview, and the exact `brief`
//!   and `frontier` commands. Health is an explicit input
//!   ([`EntryHealth`]): the plan renders it healthy, and [`render_vault`]
//!   publishes the variant announcing recorded drift or a partial render
//!   through [`CaseRenderer::announce`] (`entry`). Projection health names
//!   the `vault status` and `vault conflicts` commands that check it. Bump
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
//!   snapshot, built with [`working_policy`]. [`foreign_files`] lists the
//!   unowned files a render would report as unknown, without rendering.
//!   [`RenderReport::swept`] lists cleaned leftovers.
//! - Drift ([`drift`]): an owned file that was edited (its body, or a
//!   property a viewer's table rewrote), deleted, moved, or that has a sync
//!   client's conflict copy beside it is refused ([`Refusal::Drifted`]) and
//!   left exactly as found; nothing is recreated, replaced or deleted. It
//!   stays owned at the old hash, so it is diagnosed again on every render
//!   until reconciled. The would-be content is retained under
//!   `local/conflicts/<generation>/<path>`, and a [`Drift`] record per path
//!   is stored in the manifest and reported in [`RenderReport::drift`] and
//!   [`VaultStatus::drift`] (for `vault status` and `vault conflicts`).
//!   The supported contract stays: do not edit generated notes directly;
//!   use `Notes/` or controlled mutations.
//! - Reconcile ([`reconcile`]): `vault reconcile` resolves listed drifted
//!   paths with one explicit disposition (`preserve-as-note`,
//!   `propose-change`, `discard`, `release-path`), checks each against its
//!   drift record first, keeps every observed byte (a confirmed discard keeps
//!   them under `local/conflicts/discarded/`), and completes with a render
//!   under the same lock. Its one write in a human-owned folder is
//!   `preserve-as-note` adding new files under `Notes/Reconciled/` with a
//!   no-replace rename; it never replaces or removes anything there. A
//!   released path is the person's from then on, and the render projects its
//!   record at a relocated path that every generated link follows.
//! - Classification-aware projection: notes ask the core `Classifier` per
//!   field through [`note::NoteContext`]; extend [`ViewPolicy`] (and its
//!   fingerprint) for further rules. A withheld record appears by ID only:
//!   its note keeps the frame, its file name is its ID, and links to it show
//!   the ID. Effective state is computed on the full snapshot first, so a
//!   visible record never reads stronger than it is: withheld objections,
//!   reviews and assessments of it stay as ID-only placeholders and are
//!   counted, and `Start Here.md` says how many records its lists leave out.
//!   A gap, `next` step or measurement flag whose rule read a withheld record
//!   names only the rule, since its reason can quote that record's fields.
//!   `tests/audience_policy.rs` scans every planned byte per audience.
//! - Incremental render ([`incremental`]): the planner still produces the
//!   full plan, and the publisher skips every file that already holds the
//!   planned bytes. A case note whose bytes are unchanged apart from the
//!   basis it names keeps its earlier basis, so a commit rewrites only the
//!   notes whose content changed. That is proven per note by comparing the
//!   restamped bytes with the owned hash; nothing about what a note reads is
//!   tracked or cached. [`PublishMode::Full`] restamps every note instead.
//! - Exports ([`export`]): `export --kind KIND` pins one case to a verified
//!   head, plans a registered kind's files with the same pure planning and
//!   audience classification (strict, without placeholders, and a record
//!   bearing on withheld material caveated or withheld by audience), and
//!   writes them with an `inv.export-manifest.v1` manifest into a fresh
//!   destination, never over anything. Kinds plug in through
//!   [`export::KINDS`] and ask [`export::ExportInput::includes`] and
//!   [`export::ExportInput::caveats`] on every channel.
//!
//! Rendering works with any viewer application absent or closed. The
//! filesystem is the only interface.

pub mod attachments;
pub mod bases;
pub mod compat;
pub mod drift;
pub(crate) mod entry;
pub mod error;
pub mod evidence;
pub mod export;
pub mod incremental;
pub mod layout;
pub mod lock;
pub mod manifest;
pub mod map;
pub mod marker;
pub mod note;
pub mod plan;
pub mod policy;
pub mod publish;
pub mod reconcile;
pub(crate) mod records;
pub mod snapshot;
pub mod start_here;
pub mod status;
pub mod sweep;
pub mod templates;
pub mod text;
pub mod yaml;

use std::time::Duration;

use investigation_store::{Store, durable::DurableFs};

pub use drift::{CONFLICTS_DIR, Drift, DriftKind};
pub use error::RenderError;
pub use incremental::PublishMode;
pub use lock::RenderLock;
pub use manifest::{
    CompletedGeneration, LoadedManifest, MANIFEST_VERSION, Manifest, ManifestStatus, OwnedPath,
    RENDER_MANIFEST, Released, RenderState,
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
pub use start_here::EntryHealth;
pub use status::{VaultState, VaultStatus, foreign_files, vault_status, working_policy};
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
///    held, and drifted paths are refused; the rest render ([`drift`]). The
///    entry note is planned announcing the drift the manifest recorded;
/// 4. drift is diagnosed, and the content withheld from drifted paths is
///    retained under `local/conflicts/<generation>/`;
/// 5. when the drift found differs from the drift announced, the entry note
///    is written again announcing it, under a write-ahead intent;
/// 6. the manifest is stored with the outcomes and the drift records,
///    `complete` or `partial` ([`Manifest::updated`]). It is the last file
///    the render writes.
///
/// When a file operation fails, the outcomes reached so far are still
/// recorded, with state `partial` and the error as its reason, and the entry
/// note announces the partial render when it can still be written, before
/// the error is returned. A render killed at any point leaves `updating` (or
/// the earlier manifest), from which the next render completes.
///
/// Notes whose content did not change keep the basis they name
/// ([`PublishMode::Incremental`]); [`render_vault_with`] chooses the mode.
pub fn render_vault<F: DurableFs>(
    store: &mut Store<F>,
    renderer: &impl CaseRenderer,
    policy: &ViewPolicy,
    observe: &mut dyn FnMut(&PublishStep<'_>),
) -> Result<RenderReport, RenderError> {
    render_vault_with(store, renderer, policy, PublishMode::Incremental, observe)
}

/// [`render_vault`] in the given [`PublishMode`]. Before step 1, in
/// [`PublishMode::Incremental`], every case note the manifest proves
/// unchanged apart from its basis is restamped to the basis it already names
/// ([`incremental`]); the publisher then finds its bytes in place.
pub fn render_vault_with<F: DurableFs>(
    store: &mut Store<F>,
    renderer: &impl CaseRenderer,
    policy: &ViewPolicy,
    mode: PublishMode,
    observe: &mut dyn FnMut(&PublishStep<'_>),
) -> Result<RenderReport, RenderError> {
    let lock = RenderLock::acquire(store.workspace(), RENDER_LOCK_WAIT)?;
    render_locked(store, &lock, renderer, policy, mode, observe)
}

/// [`render_vault_with`] under a render lock the caller already holds, so a
/// reconcile ([`reconcile`]) and the render that completes it are one step.
pub(crate) fn render_locked<F: DurableFs>(
    store: &mut Store<F>,
    lock: &RenderLock,
    renderer: &impl CaseRenderer,
    policy: &ViewPolicy,
    mode: PublishMode,
    observe: &mut dyn FnMut(&PublishStep<'_>),
) -> Result<RenderReport, RenderError> {
    let snapshot = VerifiedSnapshot::read(store)?;
    let workspace = store.workspace();
    let loaded = Manifest::load(workspace, lock)?;
    // Released paths are a person's; their projections are planned at the
    // paths they were relocated to, and every link follows.
    let renderer = reconcile::Relocated::new(renderer, loaded.manifest.relocations());
    let renderer = &renderer;
    let planned = renderer.render(&snapshot, policy)?;
    let workspace = store.workspace();
    let publisher = Publisher::open(workspace, lock)?;
    // The entry note announces the drift the last render recorded; it is
    // written again below only when this render finds something else.
    let expected = entry::health(None, loaded.manifest.drift());
    let planned = if expected.is_healthy() {
        planned
    } else {
        renderer.announce(&planned, &snapshot, policy, &expected)?
    };
    // Announcing re-plans from `planned`, whose notes name their case heads;
    // only the entry note of an announcement is ever published.
    let plan = match mode {
        PublishMode::Incremental => {
            incremental::carry(&planned, &snapshot, &loaded.manifest, &mut |path| {
                incremental::read_note(&publisher, path)
            })?
        }
        PublishMode::Full => planned.clone(),
    };

    let mut targets = loaded.manifest.known_paths();
    targets.extend(plan.entries().iter().map(|entry| entry.path().clone()));
    let mut swept = sweep::sweep_local(workspace)?;
    swept.extend(publisher.sweep(&targets, &|path, sha256| {
        loaded.manifest.accepts(path, sha256)
    })?);

    let survey = drift::survey(&publisher, &loaded.manifest, &plan)?;
    let guard = drift::Guard {
        manifest: &loaded.manifest,
        survey: &survey,
    };
    let ownership = attachments::Gate {
        inner: &guard,
        snapshot: &snapshot,
        policy,
    };
    // A partial render is announced on the entry note when it can still be
    // written; failing to announce it never hides the render error.
    let announce_partial = |manifest: Manifest, reason: String| {
        let health = entry::health(Some(reason), manifest.drift());
        renderer
            .announce(&planned, &snapshot, policy, &health)
            .and_then(|announced| {
                entry::publish(
                    workspace,
                    lock,
                    &publisher,
                    &survey,
                    manifest.clone(),
                    &announced,
                )
            })
            .map_or(manifest, |(manifest, _)| manifest)
    };
    loaded.manifest.begin(&plan).store(workspace, lock)?;
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
            let manifest = manifest.failed(plan.generation(), error.to_string());
            let _ = manifest.store(workspace, lock);
            let _ = announce_partial(manifest, error.to_string()).store(workspace, lock);
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
            let manifest = manifest.failed(plan.generation(), error.to_string());
            let _ = manifest.store(workspace, lock);
            let _ = announce_partial(manifest, error.to_string()).store(workspace, lock);
            return Err(error);
        }
    };
    let manifest = manifest.with_drift(&publish, drifts);
    let actual = entry::health(None, manifest.drift());
    let manifest = if actual == expected {
        manifest
    } else {
        let announced = renderer.announce(&planned, &snapshot, policy, &actual)?;
        match entry::publish(
            workspace,
            lock,
            &publisher,
            &survey,
            manifest.clone(),
            &announced,
        ) {
            Ok((manifest, _)) => manifest,
            Err(error) => {
                // Every plan entry was processed; only the announcement
                // failed. The plan's entry note stays, as recorded.
                manifest.store(workspace, lock)?;
                return Err(error);
            }
        }
    };
    manifest.store(workspace, lock)?;
    let unknown = publisher.unknown_files(&|path| manifest.claims(path))?;
    Ok(RenderReport {
        publish,
        manifest: loaded.status,
        adopted,
        unknown,
        swept,
        drift: manifest.drift().cloned().collect(),
    })
}
