//! Pure planning: a verified snapshot and a view policy in, a path/hash/content
//! plan out.
//!
//! Nothing in this module (or in [`crate::layout`], [`crate::templates`],
//! [`crate::note`], [`crate::text`], [`crate::yaml`], [`crate::policy`] and [`crate::map`]) may touch the filesystem, read a clock, or use
//! randomness or hash-order iteration. A test scans these sources to keep it
//! that way. Identical inputs therefore give byte-identical plans.

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::{
    identity::{Id, Kind},
    wire::{Digest, Snapshot},
};
use investigation_store::journal::hash_bytes;

use crate::{
    RenderError, VerifiedSnapshot, ViewPolicy, layout, marker, note::NoteContext,
    start_here::EntryHealth, templates,
};

/// Identifies the vault renderer's output format. Bump it whenever the bytes
/// produced for the same snapshot and policy change.
///
/// v7: the full `Start Here.md` (W08.06), and local absolute paths elided
/// from display text (W06b.15, which changed bytes without its own bump).
/// v8: human-owned folder enforcement and the Attachments/hidden-folder rows
/// in `Start Here.md` (W08.08).
/// v9: a Decision without a title of its own is titled `Chosen option: …`
/// rather than an imperative sentence around authored text (W05b.13).
/// v10: the option-by-criterion comparison matrix on question and decision
/// notes (W08.17).
/// v11: withheld objections, reviews and assessments of a visible record stay
/// as ID-only placeholders, measurement flags that read a withheld record
/// name only their rule, and `Start Here.md` counts withheld records (W08.13).
/// v12: projection health in `Start Here.md` names the `vault status` and
/// `vault conflicts` commands (W08.11).
/// v13: authored record notes show their inherited trust level and its
/// stated limit in the basis table (W10.18).
/// v14: run notes show the verification level beside the chronology (W07.10).
/// v15: static summary table views under `Cases/<case-id>/Views/` (the
/// comparison matrix, reviews, epistemic frontier, timeline and the other
/// §12.5 views), linked from a `## Views` section of the Overview, and a
/// matrix cell with no comparison reads `unknown` (W08.07).
/// v16: `Start Here.md` says a case note names the head its content was
/// rendered at and keeps it while unchanged (W08.15).
/// v17: a code span whose text starts with `=` or `$=` (after any whitespace,
/// which Dataview trims) shows that first character as `\u{3d}` or `\u{24}`,
/// so it never runs as a Dataview inline query or script (W08.18).
/// v18: freshness rules version 7 (W10.27): a question answered with a record
/// that rests on its reader no longer makes that reader `needs_review`, so
/// effective states, caveats and gaps in case notes change for the same
/// snapshot.
/// v19: a closed case's Overview has a `## Closure` section (outcome, reason,
/// unresolved questions, reopen condition, retrospective or waiver); run
/// notes show reported start and finish times as claims with their clock;
/// observation notes label `Observed at (claimed)`; the timeline's claimed
/// time column also shows a Run's reported start (MB.02).
/// v20: `Start Here.md` announcing drift says how to resolve it with
/// `vault reconcile` and its four dispositions (W12.05).
/// v21: native Base files beside the static table views when the view
/// policy plans them (a supported viewer release is stated); every record
/// note and Overview carries `inv_open_gaps`, observation notes carry
/// `inv_value_kind`, and the frontier and timeline notes gain a table with
/// one row per note, the rows their Bases list (W12.01).
/// v22: when the view policy allows the `canvas.*` viewer features, each
/// case gains `Map.canvas` with `Map Legend.md` and `Map Left Out.md`, and
/// its Overview a `## Map` section (W12.03). The policy fingerprint names
/// the stated viewer release (`;viewer=<release>`) instead of `;bases`.
pub const RENDERER_VERSION: &str = "inv.vault-renderer.v22";

const MAX_PATH_BYTES: usize = 1024;
const MAX_COMPONENT_BYTES: usize = 255;
/// Characters that break links or are rejected by common filesystems and sync clients.
const FORBIDDEN: &[char] = &['\\', ':', '*', '?', '"', '<', '>', '|', '#', '^', '[', ']'];

/// A validated relative path inside a plan destination, `/`-separated.
///
/// Components are non-empty, never `.` or `..`, never start with a dot (so a
/// plan cannot reach hidden configuration folders), and contain no control or
/// link-breaking characters. Absolute paths are impossible by construction.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlanPath(String);

impl PlanPath {
    pub fn new(path: impl Into<String>) -> Result<Self, RenderError> {
        let path = path.into();
        let invalid = |reason| RenderError::InvalidPath {
            path: path.clone(),
            reason,
        };
        if path.is_empty() || path.len() > MAX_PATH_BYTES {
            return Err(invalid("length out of range"));
        }
        for component in path.split('/') {
            if component.is_empty() {
                return Err(invalid("empty component or absolute path"));
            }
            if component.len() > MAX_COMPONENT_BYTES {
                return Err(invalid("component too long"));
            }
            if component.starts_with('.') {
                return Err(invalid("hidden or relative component"));
            }
            if component.ends_with(' ') || component.ends_with('.') {
                return Err(invalid("component ends with a space or dot"));
            }
            if component
                .chars()
                .any(|c| c.is_control() || FORBIDDEN.contains(&c))
            {
                return Err(invalid("forbidden character"));
            }
        }
        Ok(Self(path))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// The final component.
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }
}

impl std::fmt::Display for PlanPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Publish order. A reader that sees a newer entry note can rely on the views
/// and records it links to having been published first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// One note per record.
    Records,
    /// Case overviews, tables and other views over records.
    Views,
    /// The vault's entry note, published last.
    Entry,
}

/// One file in a plan. The digest always matches the content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    path: PlanPath,
    phase: Phase,
    content: Vec<u8>,
    sha256: Digest,
    projects: Option<Id>,
}

impl PlanEntry {
    pub fn path(&self) -> &PlanPath {
        &self.path
    }

    pub const fn phase(&self) -> Phase {
        self.phase
    }

    pub fn content(&self) -> &[u8] {
        &self.content
    }

    pub fn sha256(&self) -> &Digest {
        &self.sha256
    }

    /// The record this file projects, when it projects exactly one.
    pub const fn projects(&self) -> Option<Id> {
        self.projects
    }
}

/// Binds a plan to the exact inputs that produced it: renderer version, view
/// policy and every case head. Its ID is a digest of those inputs, so equal
/// inputs give equal generations and nothing depends on wall-clock time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    id: String,
    renderer: &'static str,
    policy: String,
    workspace_id: Id,
    heads: Vec<(Id, Snapshot)>,
}

impl Generation {
    pub fn new(renderer: &'static str, snapshot: &VerifiedSnapshot, policy: &ViewPolicy) -> Self {
        let policy = policy.fingerprint();
        let heads: Vec<_> = snapshot
            .cases()
            .map(|case| (case.case_id(), case.head().clone()))
            .collect();
        let mut text = format!(
            "inv.render-generation.v1\nrenderer {renderer}\npolicy {policy}\nworkspace {}\n",
            snapshot.workspace_id()
        );
        for (case_id, head) in &heads {
            text.push_str(&format!(
                "case {case_id} {} {}\n",
                head.sequence, head.head_hash
            ));
        }
        let digest = hash_bytes(text.as_bytes());
        let hex = digest.as_str().trim_start_matches("sha256:");
        Self {
            id: format!("gen_{hex}"),
            renderer,
            policy,
            workspace_id: snapshot.workspace_id(),
            heads,
        }
    }

    /// `gen_<sha256 hex>`; safe as a file or directory name.
    pub fn id(&self) -> &str {
        &self.id
    }

    pub const fn renderer(&self) -> &'static str {
        self.renderer
    }

    pub fn policy(&self) -> &str {
        &self.policy
    }

    pub const fn workspace_id(&self) -> Id {
        self.workspace_id
    }

    /// Case heads in case ID order.
    pub fn heads(&self) -> &[(Id, Snapshot)] {
        &self.heads
    }
}

/// A complete, ordered set of files for one generation. Entries are sorted by
/// [`Phase`], then by path, and paths are unique even ignoring case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderPlan {
    generation: Generation,
    entries: Vec<PlanEntry>,
}

impl RenderPlan {
    pub fn generation(&self) -> &Generation {
        &self.generation
    }

    pub fn entries(&self) -> &[PlanEntry] {
        &self.entries
    }

    pub fn entry(&self, path: &str) -> Option<&PlanEntry> {
        self.entries
            .iter()
            .find(|entry| entry.path.as_str() == path)
    }
}

/// Collects entries in any order and produces a plan in the canonical order.
pub struct PlanBuilder {
    generation: Generation,
    entries: BTreeMap<(Phase, PlanPath), PlanEntry>,
    folded: BTreeSet<String>,
}

impl PlanBuilder {
    pub fn new(generation: Generation) -> Self {
        Self {
            generation,
            entries: BTreeMap::new(),
            folded: BTreeSet::new(),
        }
    }

    pub fn generation(&self) -> &Generation {
        &self.generation
    }

    pub fn add(
        &mut self,
        phase: Phase,
        path: PlanPath,
        content: impl Into<Vec<u8>>,
        projects: Option<Id>,
    ) -> Result<(), RenderError> {
        // Case-insensitive filesystems would merge two such paths into one file.
        if !self.folded.insert(path.as_str().to_lowercase()) {
            return Err(RenderError::DuplicatePath(path.0));
        }
        let content = content.into();
        let entry = PlanEntry {
            sha256: hash_bytes(&content),
            path: path.clone(),
            phase,
            content,
            projects,
        };
        self.entries.insert((phase, path), entry);
        Ok(())
    }

    pub fn finish(self) -> RenderPlan {
        RenderPlan {
            generation: self.generation,
            entries: self.entries.into_values().collect(),
        }
    }
}

/// Plans one destination's files from a verified snapshot.
///
/// Implementations must be pure: the same snapshot and policy must give a
/// byte-identical plan. The vault uses [`VaultRenderer`]; exports implement
/// this trait for their own layouts and reuse the publisher pattern.
pub trait CaseRenderer {
    fn render(
        &self,
        snapshot: &VerifiedSnapshot,
        policy: &ViewPolicy,
    ) -> Result<RenderPlan, RenderError>;

    /// `plan` (planned from `snapshot` and `policy`) with its entry note
    /// re-planned to announce `health`: a partial render or recorded drift.
    /// The generation and every other entry are unchanged. The default, for
    /// layouts without an entry note, returns the plan as it is.
    fn announce(
        &self,
        plan: &RenderPlan,
        _snapshot: &VerifiedSnapshot,
        _policy: &ViewPolicy,
        _health: &EntryHealth,
    ) -> Result<RenderPlan, RenderError> {
        Ok(plan.clone())
    }
}

/// The working vault layout: one note per record, one overview and a set of
/// static table views per case, and `Start Here.md`.
#[derive(Debug, Clone, Copy, Default)]
pub struct VaultRenderer;

impl CaseRenderer for VaultRenderer {
    fn render(
        &self,
        snapshot: &VerifiedSnapshot,
        policy: &ViewPolicy,
    ) -> Result<RenderPlan, RenderError> {
        let generation = Generation::new(RENDERER_VERSION, snapshot, policy);
        let mut plan = PlanBuilder::new(generation.clone());
        let contexts: Vec<NoteContext<'_>> = snapshot
            .cases()
            .map(|case| NoteContext::new(case, policy, &generation))
            .collect();
        for notes in &contexts {
            let case = notes.case();
            for record in case.state().records() {
                let id = record.header.id;
                if id.kind() == Kind::Case {
                    continue;
                }
                let Some(path) = layout::record_note(case.case_id(), id)? else {
                    continue;
                };
                let content = templates::record_note(notes, record)?;
                plan.add(Phase::Records, path, content, Some(id))?;
            }
            let views = crate::records::views::plan(notes)?;
            let map = crate::map::plan(notes, &views)?;
            let content = templates::case_overview(notes, &views, map.as_ref())?;
            for view in views {
                plan.add(Phase::Views, view.path, view.content, None)?;
                if let Some((path, content)) = view.base {
                    plan.add(Phase::Views, path, content, None)?;
                }
            }
            for (path, content) in map.into_iter().flat_map(|map| map.files) {
                plan.add(Phase::Views, path, content, None)?;
            }
            plan.add(
                Phase::Views,
                layout::case_overview(case.case_id())?,
                content,
                Some(case.case_id()),
            )?;
        }
        let content = templates::start_here(
            snapshot,
            &contexts,
            policy,
            &generation,
            &EntryHealth::default(),
        )?;
        plan.add(Phase::Entry, layout::start_here()?, content, None)?;
        marker::stamp_plan(plan.finish(), snapshot)
    }

    fn announce(
        &self,
        plan: &RenderPlan,
        snapshot: &VerifiedSnapshot,
        policy: &ViewPolicy,
        health: &EntryHealth,
    ) -> Result<RenderPlan, RenderError> {
        let generation = plan.generation().clone();
        let contexts: Vec<NoteContext<'_>> = snapshot
            .cases()
            .take(crate::start_here::MAX_CASES)
            .map(|case| NoteContext::new(case, policy, &generation))
            .collect();
        let content = templates::start_here(snapshot, &contexts, policy, &generation, health)?;
        let path = layout::start_here()?;
        let mut builder = PlanBuilder::new(generation.clone());
        for entry in plan.entries() {
            let content = if entry.path() == &path {
                content.clone().into_bytes()
            } else {
                entry.content().to_vec()
            };
            builder.add(
                entry.phase(),
                entry.path().clone(),
                content,
                entry.projects(),
            )?;
        }
        if plan.entry(path.as_str()).is_none() {
            builder.add(Phase::Entry, path, content, None)?;
        }
        // Stamping verifies the markers already present and adds the new
        // note's generation marker.
        marker::stamp_plan(builder.finish(), snapshot)
    }
}

/// Plan the working vault for `snapshot` under `policy`.
pub fn render(snapshot: &VerifiedSnapshot, policy: &ViewPolicy) -> Result<RenderPlan, RenderError> {
    VaultRenderer.render(snapshot, policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_paths_reject_escapes_and_hidden_or_unsafe_names() {
        for bad in [
            "",
            "/abs.md",
            "a//b.md",
            "../x.md",
            "a/../x.md",
            "./x.md",
            ".obsidian/app.json",
            "a/.hidden.md",
            "a\\b.md",
            "C:x.md",
            "a/b\0.md",
            "a/b\n.md",
            "trailing /x.md",
            "x.",
            "[[link]].md",
            "a|b.md",
        ] {
            assert!(PlanPath::new(bad).is_err(), "{bad:?} should be rejected");
        }
        for good in [
            "Start Here.md",
            "Cases/case_018d87fc-5234-7000-8000-000000000001/Overview.md",
        ] {
            assert_eq!(PlanPath::new(good).unwrap().as_str(), good);
        }
        assert_eq!(
            PlanPath::new("Cases/c/Overview.md").unwrap().file_name(),
            "Overview.md"
        );
    }

    /// The planner modules must stay free of side effects. This is a coarse
    /// textual guard; the type-level guard is that planning takes only
    /// in-memory values and the publisher requires a [`crate::RenderLock`].
    #[test]
    fn planner_sources_have_no_filesystem_clock_or_randomness() {
        let sources = [
            ("plan.rs", include_str!("plan.rs")),
            ("layout.rs", include_str!("layout.rs")),
            ("templates.rs", include_str!("templates.rs")),
            ("policy.rs", include_str!("policy.rs")),
            ("note.rs", include_str!("note.rs")),
            ("evidence.rs", include_str!("evidence.rs")),
            ("text.rs", include_str!("text.rs")),
            ("yaml.rs", include_str!("yaml.rs")),
            ("marker.rs", include_str!("marker.rs")),
            ("start_here.rs", include_str!("start_here.rs")),
            ("records/views.rs", include_str!("records/views.rs")),
            ("records/matrix.rs", include_str!("records/matrix.rs")),
            ("bases.rs", include_str!("bases.rs")),
            ("map.rs", include_str!("map.rs")),
            ("compat.rs", include_str!("compat.rs")),
        ];
        let forbidden = [
            ["std", "::fs"].concat(),
            ["std", "::env"].concat(),
            ["std", "::time"].concat(),
            ["std", "::process"].concat(),
            ["std", "::net"].concat(),
            ["Hash", "Map"].concat(),
            ["Hash", "Set"].concat(),
            ["rust", "ix"].concat(),
            ["System", "Time"].concat(),
            ["Instant", "::"].concat(),
            ["Clock"].concat(),
            ["rand", "::"].concat(),
            ["crate::", "publish"].concat(),
            ["crate::", "lock"].concat(),
            ["Store", "<"].concat(),
            ["Workspace"].concat(),
        ];
        for (name, source) in sources {
            // Exclude this guard's own test module from the scan.
            let body = source.split("#[cfg(test)]").next().unwrap();
            for term in &forbidden {
                assert!(
                    !body.contains(term.as_str()),
                    "{name} must stay pure but mentions {term}"
                );
            }
        }
    }
}
