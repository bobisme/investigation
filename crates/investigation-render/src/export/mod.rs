//! Exports: immutable files rendered from one exact, verified case snapshot.
//!
//! An export is where a case leaves the tool's control, so the properties that
//! matter (determinism, positive inclusion, a truthful manifest, a fresh
//! destination) belong to this one pipeline rather than to each kind:
//!
//! ```text
//!  journal ──ExportSnapshot::read(store, case, pin)──▶ pinned case snapshot + today's state
//!                                   │
//!            plan_export(kind, &snapshot, audience)   pure: no filesystem, clock, randomness, network or model
//!                                   ▼
//!            ExportPlan { files, manifest, exclusions }
//!                                   │
//!            write_export(&plan, out)                 only into a new or empty directory; never overwrites
//!                                   ▼
//!            DIR/<kind files> … DIR/export-manifest.json (written last)
//! ```
//!
//! - [`KINDS`] is the one registry of the §15.4 kinds: name, product
//!   ([`Product`]), purpose, required content and, once implemented, its
//!   [`ExportRenderer`]. A kind without a renderer is listed and refused with
//!   [`ExportError::KindNotAvailable`]; it is never approximated by another.
//! - [`ExportSnapshot`] pins one case to a verified head (the current head
//!   unless `--snapshot` names an earlier one) and keeps today's state beside
//!   it: disclosure always follows today's classification.
//! - [`Inclusion`] is positive inclusion under the strict audience policy
//!   (no ID-only placeholders, [`POLICY_VERSION`]): a record appears only
//!   when both its pinned and its current revision may be shown to the
//!   audience, and when nothing that challenges it or that it rests on is
//!   withheld, unless the audience may be told so by a fixed [`Caveat`]
//!   (internal and restricted may; public may not, and gets the record
//!   withheld instead). Effective state and the material bearing on each
//!   record are computed on the full pinned snapshot before anything is
//!   filtered ([`ExportInput::notes`]), so withholding a record never makes
//!   another one read stronger. Every withheld record of the case is an
//!   [`Exclusion`]; any exclusion makes the export `incomplete`, and the
//!   exact exclusions and caveated records go only to the publisher, never
//!   into the export. The table of what each audience gets is in the
//!   `inclusion` module documentation.
//! - [`ExportManifest`] (`inv.export-manifest.v1`) names the snapshot, the
//!   framework, renderer and schema versions, the audience and policy
//!   version, completeness, included source digests, supersession links and
//!   every file with its digest. It holds no time, path or other case, so the
//!   same snapshot, kind and policy give byte-identical files and manifest.
//! - [`write_export`] writes a plan into a directory that does not exist or
//!   is empty, creating every file without replacing anything, and the
//!   manifest last. A failed write removes what it created.
//!
//! Adding a kind: write a unit type implementing [`ExportRenderer`] in a new
//! module here (pure: the planner purity test lists it), set it as the
//! `renderer` of its [`KINDS`] row, and add the module to the purity test.
//! The kind's whole contract with the audience policy is two calls, asked
//! on every channel it emits (bodies, titles, links, file names, tables,
//! counts, previews, summaries, search entries):
//!
//! - [`ExportInput::includes`] before a record, or anything derived from it,
//!   appears at all. Count only included records.
//! - [`ExportInput::caveats`] for each included record it shows: render
//!   every returned [`Caveat::text`] beside the record wherever its standing
//!   (title with a state, summary, table row) appears. Never paraphrase or
//!   drop it, and never add your own words about withheld material.
//!
//! Kinds that need more than records (retained bytes for attachments or an
//! archive, selected human notes) take them as explicit, digest-checked
//! inputs on [`ExportInput`], never by reading the workspace while planning.

mod case_report;
mod decision_record;
mod inclusion;
mod manifest;
mod snapshot;
mod write;

use std::fmt;

use investigation_core::{
    PROGRAM_NAME,
    command::RevisionRef,
    derivation,
    identity::Kind,
    state::Record,
    wire::{ErrorCode, Failure},
};
use investigation_store::journal::hash_bytes;
use serde_json::Value;

use crate::{
    Audience, CaseSnapshot, Generation, PlanBuilder, RenderError, RenderPlan, ViewPolicy,
    note::NoteContext,
};

pub use inclusion::{Caveat, Caveated, Exclusion, Inclusion, caveats_approved, dependent_rule};
pub use manifest::{
    Completeness, ExportManifest, FileDigest, MANIFEST_SCHEMA, ManifestPolicy, ManifestSnapshot,
    ManifestVersions, SourceDigest, Statements, Supersession, manifest_schema,
};
pub use snapshot::{ExportSnapshot, Pin};
pub use write::{Written, write_export};

/// Version of the shared pipeline: snapshot pinning, manifest and
/// destination rules. Bump it when any of them changes the bytes written;
/// the audience policy has its own [`POLICY_VERSION`].
pub const FRAMEWORK_VERSION: &str = "inv.export.v1";

/// Version of the export audience policy. v1: strict positive inclusion by
/// effective classification, judged at the pinned and the current revision,
/// with no placeholders for withheld records. v2 (W11.02): also the
/// dependent-record layer, so a record that rests on or is challenged by
/// withheld material carries a fixed caveat (internal, restricted) or is
/// withheld itself (public).
pub const POLICY_VERSION: &str = "inv.export-policy.v2";

/// The manifest's file name inside every export. It is written last.
pub const MANIFEST_FILE: &str = "export-manifest.json";

/// What an export is for. Review exports and recovery archives have
/// different completeness and security guarantees (§16.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Product {
    /// A rendered document about the case for a reader.
    Report,
    /// A curated, browsable copy of the case for reviewers; not resumable.
    Review,
    /// The canonical state needed to recover the workspace; resumable.
    Archive,
    /// Structured records for another tool to read.
    Bundle,
}

impl Product {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::Review => "review",
            Self::Archive => "archive",
            Self::Bundle => "bundle",
        }
    }

    /// Whether an export of this product can restore canonical state.
    pub const fn resumable(self) -> bool {
        matches!(self, Self::Archive)
    }
}

/// Plans one kind's files. Implementations must be pure: no filesystem,
/// clock, randomness, network or model call, and no hash-order iteration.
pub trait ExportRenderer: Sync {
    /// Identifies this kind's output format. Bump it whenever the bytes
    /// planned for the same input change.
    fn version(&self) -> &'static str;

    /// Add this kind's files to `files`. Show a record's content only when
    /// [`ExportInput::includes`] admits it, and always with its
    /// [`ExportInput::caveats`]. The manifest file name is reserved.
    fn plan(&self, input: &ExportInput<'_>, files: &mut PlanBuilder) -> Result<(), RenderError>;
}

/// One row of the kind registry.
pub struct KindSpec {
    /// The `--kind` value.
    pub name: &'static str,
    pub product: Product,
    /// Intended use, one line.
    pub purpose: &'static str,
    /// What the kind must contain (§15.4).
    pub contains: &'static str,
    renderer: Option<&'static dyn ExportRenderer>,
}

impl KindSpec {
    /// Whether this build can produce the kind.
    pub const fn available(&self) -> bool {
        self.renderer.is_some()
    }
}

impl fmt::Debug for KindSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KindSpec")
            .field("name", &self.name)
            .field("product", &self.product)
            .field("available", &self.available())
            .finish_non_exhaustive()
    }
}

/// Every export kind, in the order §15.4 lists them.
pub const KINDS: &[KindSpec] = &[
    KindSpec {
        name: "handoff",
        product: Product::Report,
        purpose: "Continue the case as a fresh agent or person.",
        contains: "Snapshot, question, knowns and unknowns, strongest counterargument, next investigation step.",
        renderer: None,
    },
    KindSpec {
        name: "case-report",
        product: Product::Report,
        purpose: "Report a general inquiry.",
        contains: "Objective, methods, original observations, alternatives, supported conclusions, limits, open issues, retrospective highlights.",
        renderer: Some(&case_report::CaseReport),
    },
    KindSpec {
        name: "analysis-report",
        product: Product::Report,
        purpose: "Report a causal or empirical case.",
        contains: "Reproduction conditions, baselines, runs and controls, uncertainty, root-cause or measurement assessment.",
        renderer: None,
    },
    KindSpec {
        name: "decision-record",
        product: Product::Report,
        purpose: "Record an evaluative decision, ADR-like.",
        contains: "Context, options, hard constraints, criteria, evidence, choice, tradeoffs, consequences, revisit trigger, unresolved warnings.",
        renderer: Some(&decision_record::DecisionRecord),
    },
    KindSpec {
        name: "method-retrospective",
        product: Product::Report,
        purpose: "Learn from a completed or paused inquiry.",
        contains: "Approaches attempted, effective and invalid routes, constraints, limits, proposed lessons and reviewer status.",
        renderer: None,
    },
    KindSpec {
        name: "review-vault",
        product: Product::Review,
        purpose: "Share an approved, browsable casebook.",
        contains: "Curated notes, views, explicitly allowed attachments and an omitted-data manifest.",
        renderer: None,
    },
    KindSpec {
        name: "workspace-archive",
        product: Product::Archive,
        purpose: "Recover canonical state in full.",
        contains: "Journal, retained blobs, schemas, owner configuration and selected human notes, with an integrity manifest.",
        renderer: None,
    },
    KindSpec {
        name: "producer-bundle",
        product: Product::Bundle,
        purpose: "Hand recorded runs to another tool.",
        contains: "Versioned structured records, mappings, original lineage, completeness.",
        renderer: None,
    },
];

/// The registry row for `name`.
pub fn kind(name: &str) -> Option<&'static KindSpec> {
    KINDS.iter().find(|spec| spec.name == name)
}

/// Every kind name, in registry order.
pub fn kind_names() -> Vec<&'static str> {
    KINDS.iter().map(|spec| spec.name).collect()
}

/// Names of the kinds this build can produce.
pub fn available_kinds() -> Vec<&'static str> {
    KINDS
        .iter()
        .filter(|spec| spec.available())
        .map(|spec| spec.name)
        .collect()
}

/// What a kind renderer plans from. Everything is in memory.
pub struct ExportInput<'a> {
    pub kind: &'static KindSpec,
    pub audience: Audience,
    /// The pinned case snapshot.
    pub case: &'a CaseSnapshot,
    /// Titles, freshness and effective states computed on the full pinned
    /// snapshot, before inclusion. Its own visibility answers use the
    /// working-vault policy; ask [`Self::includes`] before showing content.
    pub notes: &'a NoteContext<'a>,
    pub inclusion: &'a Inclusion<'a>,
}

impl ExportInput<'_> {
    /// Whether the record's title, summary and content may appear, and be
    /// counted. Ask it on every channel before a record, or anything derived
    /// from it, appears.
    pub fn includes(&self, record: &Record) -> bool {
        self.inclusion.includes(record)
    }

    /// The fixed caveats to show beside an included record wherever its
    /// standing appears, in a fixed order; empty when it needs none. Render
    /// each [`Caveat::text`] as given.
    pub fn caveats(&self, record: &Record) -> &[Caveat] {
        self.inclusion.caveats(record)
    }

    /// The record's safe display title when it is included.
    pub fn title(&self, record: &Record) -> Option<String> {
        if self.includes(record) {
            self.notes.title(record)
        } else {
            None
        }
    }
}

/// A planned export: the kind's files, its manifest, and the exact
/// exclusions and caveated records, which only the publisher sees.
#[derive(Debug, Clone)]
pub struct ExportPlan {
    kind: &'static KindSpec,
    files: RenderPlan,
    manifest: ExportManifest,
    manifest_bytes: Vec<u8>,
    exclusions: Vec<Exclusion>,
    caveated: Vec<Caveated>,
}

impl ExportPlan {
    pub const fn kind(&self) -> &'static KindSpec {
        self.kind
    }

    /// The kind's files in publish order, without the manifest.
    pub fn files(&self) -> &RenderPlan {
        &self.files
    }

    pub const fn manifest(&self) -> &ExportManifest {
        &self.manifest
    }

    /// The exact bytes of [`MANIFEST_FILE`].
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// Records the audience policy withheld, in ID order. Private to the
    /// publisher: they are never written into the export.
    pub fn exclusions(&self) -> &[Exclusion] {
        &self.exclusions
    }

    /// Records shown with caveats, in ID order. Private to the publisher.
    pub fn caveated(&self) -> &[Caveated] {
        &self.caveated
    }

    pub fn complete(&self) -> bool {
        self.exclusions.is_empty()
    }
}

/// Why an export was not produced. Nothing is written in any of these cases
/// except [`ExportError::Destination`] and [`ExportError::Render`] raised
/// while writing, after which the files written are removed again.
#[derive(Debug)]
pub enum ExportError {
    /// The kind is registered but this build has no renderer for it.
    KindNotAvailable { kind: &'static str },
    /// The plan was refused or could not be built.
    Render(RenderError),
    /// The destination exists and is not an empty directory.
    DestinationNotEmpty,
    /// The destination is a link, not a directory, or not a single new name.
    DestinationUnsafe(&'static str),
    /// A destination file operation failed.
    Destination { path: String, error: std::io::Error },
    /// `--require-complete` was given and the policy withheld records.
    Incomplete { exclusions: Vec<Exclusion> },
}

impl From<RenderError> for ExportError {
    fn from(error: RenderError) -> Self {
        Self::Render(error)
    }
}

impl ExportError {
    pub fn to_failure(&self) -> Failure {
        let code = match self {
            Self::KindNotAvailable { .. }
            | Self::DestinationUnsafe(_)
            | Self::DestinationNotEmpty => ErrorCode::InvalidInput,
            Self::Incomplete { .. } => ErrorCode::GateFailed,
            Self::Render(error) => return error.to_failure(),
            Self::Destination { .. } => ErrorCode::IoFailure,
        };
        Failure::new(code, self.to_string())
    }
}

impl fmt::Display for ExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KindNotAvailable { kind } => write!(
                formatter,
                "The export kind {kind} is planned but this build cannot produce it yet. \
                 Available kinds: {}.",
                available_kinds().join(", ")
            ),
            Self::Render(error) => write!(formatter, "{error}"),
            Self::DestinationNotEmpty => formatter.write_str(
                "The destination already exists and is not empty. An export never \
                 overwrites or merges; name a new directory with --out.",
            ),
            Self::DestinationUnsafe(reason) => {
                write!(formatter, "The destination is refused: {reason}.")
            }
            Self::Destination { path, error } => write!(
                formatter,
                "{path}: {error}. The files this export created were removed."
            ),
            Self::Incomplete { exclusions } => write!(
                formatter,
                "The audience policy withholds {} record(s) of this case, so the export \
                 would be incomplete, and --require-complete was given. Nothing was written.",
                exclusions.len()
            ),
        }
    }
}

impl std::error::Error for ExportError {}

/// Plan `kind` for `snapshot` and `audience`. Pure: the same snapshot, kind
/// and policy give a byte-identical plan and manifest.
pub fn plan_export(
    kind: &'static KindSpec,
    snapshot: &ExportSnapshot,
    audience: Audience,
) -> Result<ExportPlan, ExportError> {
    let renderer = kind
        .renderer
        .ok_or(ExportError::KindNotAvailable { kind: kind.name })?;
    let policy = ViewPolicy::for_audience(audience);
    let pinned = snapshot.pinned();
    let case = snapshot.case();
    let generation = Generation::new(renderer.version(), pinned, &policy);
    let notes = NoteContext::new(case, &policy, &generation);
    let inclusion = Inclusion::new(case.state(), snapshot.today(), audience);
    let input = ExportInput {
        kind,
        audience,
        case,
        notes: &notes,
        inclusion: &inclusion,
    };
    let mut files = PlanBuilder::new(generation.clone());
    renderer.plan(&input, &mut files)?;
    let files = files.finish();
    if files.entry(MANIFEST_FILE).is_some() {
        return Err(RenderError::DuplicatePath(MANIFEST_FILE.to_owned()).into());
    }
    let exclusions = inclusion.exclusions();
    let caveated = inclusion.caveated();
    let manifest = build_manifest(kind, renderer, &input, &files, exclusions.is_empty());
    let mut manifest_bytes =
        serde_json::to_vec_pretty(&manifest).expect("export manifest serializes");
    manifest_bytes.push(b'\n');
    Ok(ExportPlan {
        kind,
        files,
        manifest,
        manifest_bytes,
        exclusions,
        caveated,
    })
}

fn build_manifest(
    kind: &'static KindSpec,
    renderer: &dyn ExportRenderer,
    input: &ExportInput<'_>,
    files: &RenderPlan,
    complete: bool,
) -> ExportManifest {
    let case = input.case;
    let state = case.state();
    let included = || state.records().filter(|record| input.includes(record));
    let sources = included()
        .filter(|record| record.header.id.kind() == Kind::Artifact)
        .filter_map(|record| {
            let digest = derivation::artifact_body(state, record.reference())?.digest?;
            Some(SourceDigest {
                artifact: record.reference(),
                sha256: digest,
            })
        })
        .collect();
    let supersession = included()
        .filter_map(|record| {
            let visible = |reference: Option<RevisionRef>| {
                reference.filter(|reference| {
                    state
                        .current(reference.id)
                        .is_some_and(|target| input.includes(target))
                })
            };
            let supersedes = visible(revision_ref(&record.body["supersedes"]));
            let superseded_by = visible(revision_ref(&record.body["superseded_by"]));
            (supersedes.is_some() || superseded_by.is_some()).then(|| Supersession {
                record: record.reference(),
                supersedes,
                superseded_by,
            })
        })
        .collect();
    let file_digests: Vec<FileDigest> = files
        .entries()
        .iter()
        .map(|entry| FileDigest {
            path: entry.path().as_str().to_owned(),
            sha256: entry.sha256().clone(),
            bytes: entry.content().len() as u64,
        })
        .collect();
    let snapshot = ManifestSnapshot {
        case_id: case.case_id(),
        sequence: case.head().sequence,
        head_hash: case.head().head_hash.clone(),
    };
    let completeness = if complete {
        Completeness::Complete
    } else {
        Completeness::Incomplete
    };
    let export_id = export_id(
        kind,
        renderer,
        input.audience,
        &snapshot,
        &file_digests,
        completeness,
    );
    ExportManifest {
        schema_version: MANIFEST_SCHEMA.to_owned(),
        export_id,
        kind: kind.name.to_owned(),
        product: kind.product,
        resumable: kind.product.resumable(),
        generator: PROGRAM_NAME.to_owned(),
        versions: ManifestVersions {
            framework: FRAMEWORK_VERSION.to_owned(),
            renderer: renderer.version().to_owned(),
            manifest_schema: MANIFEST_SCHEMA.to_owned(),
            freshness_rules: investigation_core::freshness::FRESHNESS_RULES_VERSION,
        },
        snapshot,
        policy: ManifestPolicy {
            version: POLICY_VERSION.to_owned(),
            audience: input.audience.as_str().to_owned(),
            placeholders: false,
        },
        completeness,
        sources,
        supersession,
        files: file_digests,
        statements: Statements::fixed(completeness, input.audience),
    }
}

fn revision_ref(value: &Value) -> Option<RevisionRef> {
    serde_json::from_value(value.clone()).ok()
}

/// `exp_<sha256 hex>` over everything the manifest attests: equal exports
/// share it, and any change to their bytes or completeness changes it.
fn export_id(
    kind: &KindSpec,
    renderer: &dyn ExportRenderer,
    audience: Audience,
    snapshot: &ManifestSnapshot,
    files: &[FileDigest],
    completeness: Completeness,
) -> String {
    let mut text = format!(
        "inv.export-id.v1\nkind {}\nframework {FRAMEWORK_VERSION}\nrenderer {}\npolicy {POLICY_VERSION} {}\ncase {} {} {}\ncompleteness {}\n",
        kind.name,
        renderer.version(),
        audience.as_str(),
        snapshot.case_id,
        snapshot.sequence,
        snapshot.head_hash,
        completeness.as_str(),
    );
    for file in files {
        text.push_str(&format!("file {} {}\n", file.sha256, file.path));
    }
    let digest = hash_bytes(text.as_bytes());
    format!("exp_{}", digest.as_str().trim_start_matches("sha256:"))
}

#[cfg(test)]
mod tests;
