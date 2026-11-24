//! `producer-bundle`: a case's recorded Runs as structured records another
//! tool can read and re-import (plan §15.4, §11.2, §11.4). Pure.
//!
//! ```text
//! DIR/
//!   producer-bundle.json                    index (inv.producer-bundle.v1): every run
//!   runs/0001/producer-run.json             inv.producer-run.v1 envelope to re-import
//!   runs/0001/capture-manifest.json         a captured run's finalized manifest, verbatim
//!   runs/0001/files/<path>                  the run's retained artifact bytes
//!   export-manifest.json                    every file's digest, written last
//! ```
//!
//! The format is Investigation's own published producer contract: the index
//! follows `schemas/v1/producer-bundle.schema.json` and every envelope
//! follows `producer-run.schema.json`, so a reader needs neither the journal
//! format nor this program. Per Run the index holds its origin, the
//! producer's identity, intent, execution, environment, assertions,
//! controls, receipts, limitations and namespaced metadata (all labelled
//! `producer_reported`), per-artifact roles, digests and retention, the
//! per-channel completeness of a captured run, the lineage edges between the
//! records it names, and the mapping from the producer's namespace, external
//! run ID and item names to canonical IDs.
//!
//! Re-import. Ingesting a run's envelope with its own `files` folder as the
//! artifact folder (`ingest --input DIR/producer-bundle.json` does every run)
//! follows the acquisition rules of W06b.03: the producer name and external
//! run ID identify the attempt, so the same envelope maps onto the recorded
//! Run and is never a new trial, and a changed one revises it. A run recorded
//! from an envelope is re-imported from the envelope's original bytes, so
//! opaque metadata is preserved byte for byte and a re-import into the source
//! workspace deduplicates. A captured run's envelope is built from its
//! finalization. A run recorded by hand has no producer acquisition identity
//! and is exported as a record only. Nor does a run get an envelope when it
//! is recorded less complete than its artifact inventory shows (an
//! interrupted capture's missing positions): ingest computes completeness
//! from the inventory alone, so the re-imported run would read stronger.
//!
//! Audience policy. A run appears only when [`ExportInput::includes`] admits
//! the Run, with its [`ExportInput::caveats`]. Each artifact it names (an
//! original, the envelope, the finalized manifest) appears with its
//! declaration, digest, IDs, lineage and bytes only when the artifact record
//! is included too; a missing or rejected declaration has no record and is
//! part of the Run. An original envelope or manifest is copied only when
//! every artifact it declares is included, so it never names a withheld one;
//! otherwise the envelope is rebuilt from what is shown. Nothing is said
//! about what was left out: the manifest's completeness says whether
//! anything was. Measurement validity is the exporting case's recorded
//! judgment and is never part of an envelope.
//!
//! Identifiers. Re-import is keyed on the producer namespace and external
//! run ID, never on a record ID. But a remapped export (public always,
//! internal or restricted with `--remap-ids`) rewrites every UUIDv7 in every
//! file, retained bytes included: a UUID external run ID would re-import as
//! a new trial, and rewritten artifact bytes would no longer match their
//! recorded digests. So the audiences differ:
//!
//! | Audience | IDs | Bundle |
//! |---|---|---|
//! | `internal`, `restricted` | canonical | re-importable: envelopes and retained bytes |
//! | `public`, or `--remap-ids` | remapped | review only: the index, no envelope, no bytes |
//!
//! A remapped bundle still lists every shown artifact with its retention,
//! digest and size, with `file` null, and every run says
//! `identifiers_remapped` (unless a stronger reason applies). The importer
//! refuses an envelope whose bytes no longer match `reingest.sha256`.

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::{
    classification,
    command::RevisionRef,
    derivation,
    evidence::{ArtifactAvailability, ArtifactBody},
    explain::{EvidenceCompleteness, ExecutionStatus},
    header::{ClassificationLevel, ClockQuality, Knowledge, SourceTime},
    identity::{Id, Kind},
    input,
    producer_run::{
        Attribution, PRODUCER_RUN_KEY, ProducerAssertion, ProducerControl, ProducerDescriptor,
        ProducerExecution, ProducerRun, ProducerRunRecord, ProducerRunVersion, Retention,
        normalized_path,
    },
    state::{Record, State},
    wire::{Digest, ErrorCode, Failure},
};
use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ExportInput, ExportRenderer, dependent_rule};
use crate::{Phase, PlanBuilder, PlanPath, RenderError};

/// The kind's registry name.
pub const KIND: &str = "producer-bundle";
/// Version of the bundle layout, its index and the envelopes it builds.
/// Bump it when the bytes planned for the same input change.
pub const BUNDLE_FORMAT: &str = "inv.producer-bundle.v1";
/// The bundle index, beside the manifest.
pub const INDEX_FILE: &str = "producer-bundle.json";
/// The folder holding one folder per run.
pub const RUNS_ROOT: &str = "runs";
/// A run's envelope to re-import, inside its folder.
pub const ENVELOPE_FILE: &str = "producer-run.json";
/// A captured run's finalized manifest, verbatim, inside its folder.
pub const CAPTURE_MANIFEST_FILE: &str = "capture-manifest.json";
/// The folder of a run's artifact bytes, inside its folder; the artifact
/// folder to re-import its envelope with.
pub const FILES_FOLDER: &str = "files";
/// Role given in a rebuilt envelope to a captured artifact that was
/// declared without one.
pub const UNDECLARED_ROLE: &str = "undeclared";

/// The bundle index version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum BundleVersion {
    #[serde(rename = "inv.producer-bundle.v1")]
    V1,
}

/// `producer-bundle.json`: one case's recorded Runs for another tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "ProducerBundle")]
pub struct ProducerBundle {
    pub schema_version: BundleVersion,
    /// The case the runs were exported from.
    pub case_id: Id,
    /// The audience the bundle was made for: `public`, `internal` or
    /// `restricted`.
    pub audience: String,
    /// The envelope version every `reingest.envelope` file follows.
    pub envelope_version: ProducerRunVersion,
    /// The runs the audience may see, in ID order.
    pub runs: Vec<BundleRun>,
    /// Fixed statements on how to read the bundle.
    pub statements: BundleStatements,
}

/// Fixed statements, compiled into the program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BundleStatements {
    pub attribution: String,
    pub validity: String,
    pub completeness: String,
    pub reingest: String,
    pub withheld: String,
    pub identifiers: String,
}

impl BundleStatements {
    fn fixed(input: &ExportInput<'_>) -> Self {
        Self {
            attribution: "Every producer statement here (identity, intent, execution, \
                environment, assertions, controls, receipts, limitations and metadata) is the \
                producer's own claim, recorded as attributed source content. None of it is an \
                investigative finding, and a reported pass is not proof of method."
                .to_owned(),
            validity: "evidence_validity is the exporting case's recorded judgment of \
                measurement validity. It is not part of any envelope: a producer statement never \
                sets it, and a re-imported run starts at unknown."
                .to_owned(),
            completeness: "Each run's evidence_completeness is as recorded. An artifact with \
                retention missing or rejected has no bytes in the bundle or in the case; one with \
                retention retained has its bytes at file. A rejected artifact's bytes were never \
                retained, so a re-imported run lists it as missing."
                .to_owned(),
            reingest:
                "Re-import a run by ingesting reingest.envelope with reingest.artifact_folder \
                as its artifact folder, or ingest producer-bundle.json to re-import every run. \
                The producer name and external run ID identify the attempt: the same envelope maps \
                onto the recorded Run and is never a new trial, and a changed one revises it."
                    .to_owned(),
            withheld: dependent_rule(input.audience).to_owned(),
            identifiers: if input.remapped() {
                "Record IDs in this bundle are remapped for this export only and match no \
                    workspace. Remapping would rewrite identifiers inside envelopes and retained \
                    bytes, so this bundle carries neither: it is for review, each artifact's \
                    digest and size are as recorded, and no run can be re-imported from it. Export \
                    internal or restricted without --remap-ids to re-import."
            } else {
                "Record IDs are canonical and embed their creation time. Re-import keys on the \
                    producer name and external run ID, not on record IDs."
            }
            .to_owned(),
        }
    }
}

/// Where a Run's record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunOrigin {
    /// Ingested from an `inv.producer-run.v1` envelope.
    ProducerRun,
    /// Recorded through the capture protocol (`inv.capture.v1`).
    Capture,
    /// Recorded by an investigator, without a producer acquisition identity.
    Recorded,
}

/// Why a run has no envelope to re-import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotReingestible {
    /// The Run was recorded without a producer namespace and external run ID.
    NoProducerIdentity,
    /// The captured run is not finalized.
    NotFinalized,
    /// The statements shown do not form a valid envelope.
    EnvelopeInvalid,
    /// The run is recorded less complete than its artifacts alone show
    /// (missing stream positions or channel gaps). An envelope carries only
    /// the artifact inventory, so a re-imported run would read stronger.
    IncompleteBeyondInventory,
    /// The export's IDs are remapped, which would rewrite UUIDs inside the
    /// envelope and the retained bytes; a remapped bundle is review only.
    IdentifiersRemapped,
}

/// One Run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BundleRun {
    /// The canonical Run revision.
    pub run: RevisionRef,
    /// The run's folder in the bundle, such as `runs/0001`.
    pub folder: String,
    pub origin: RunOrigin,
    /// Fixed caveats of the audience policy, word for word.
    pub caveats: Vec<String>,
    /// Who produced the statements below: always `producer_reported`.
    pub attribution: Attribution,
    pub producer: Option<ProducerDescriptor>,
    pub external_run_id: Option<String>,
    pub intent: Option<String>,
    /// Execution as reported.
    pub execution: ProducerExecution,
    /// Reported environment facts, by name.
    pub environment: BTreeMap<String, String>,
    /// A recorded run's free-text environment.
    pub environment_text: Option<String>,
    /// Retention completeness as recorded on the Run.
    pub evidence_completeness: EvidenceCompleteness,
    /// The exporting case's recorded judgment; see `statements.validity`.
    pub evidence_validity: String,
    /// A captured run's finalization: `finalized`, `partial`, or
    /// `not_finalized`.
    pub finalization: Option<String>,
    /// A captured run's per-channel completeness, as finalized.
    #[schemars(with = "Vec<BTreeMap<String, Value>>")]
    pub channels: Vec<Value>,
    pub artifacts: Vec<BundleArtifact>,
    #[schemars(with = "Vec<ProducerAssertion>")]
    pub assertions: Vec<Value>,
    #[schemars(with = "Vec<ProducerControl>")]
    pub controls: Vec<Value>,
    /// Receipts as recorded: an envelope receipt names an artifact path, a
    /// captured one an item key.
    #[schemars(with = "Vec<BTreeMap<String, Value>>")]
    pub receipts: Vec<Value>,
    pub limitations: Vec<String>,
    /// Namespaced inert metadata, as recorded.
    pub metadata: BTreeMap<String, Value>,
    /// The original description, byte for byte: an ingested envelope or a
    /// captured run's finalized manifest.
    pub original: Option<BundleFile>,
    pub reingest: Option<Reingest>,
    pub not_reingestible: Option<NotReingestible>,
    pub mapping: Mapping,
    pub lineage: Vec<LineageEdge>,
}

/// One file of the bundle that holds retained bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BundleFile {
    /// Relative to the bundle.
    pub path: String,
    pub sha256: Digest,
    pub bytes: u64,
    /// The Artifact that retains these bytes.
    pub artifact: RevisionRef,
}

/// How to re-import a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reingest {
    /// The `inv.producer-run.v1` envelope, relative to the bundle.
    pub envelope: String,
    /// The folder, relative to the bundle, its artifact paths resolve in.
    pub artifact_folder: String,
    /// Whether the envelope is the original bytes the run was ingested from.
    pub verbatim: bool,
    pub sha256: Digest,
}

/// One artifact a run names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BundleArtifact {
    /// The acquisition item: `artifact:` plus the declared path for an
    /// envelope, the item key for a captured run, `recorded:` plus a number
    /// for a recorded run.
    pub item: String,
    pub role: Option<String>,
    /// The path the envelope declared.
    pub declared_path: Option<String>,
    pub media_type: Option<String>,
    /// The capture channel.
    pub channel: Option<String>,
    pub classification: Option<ClassificationLevel>,
    /// `retained`, `missing`, `rejected`, `external`, `partial` or
    /// `intentionally_removed`.
    pub retention: String,
    /// Retained bytes; for `rejected`, the bytes that were refused.
    pub digest: Option<Digest>,
    pub size_bytes: Option<u64>,
    /// The digest the producer expected.
    pub expected_sha256: Option<String>,
    /// Why the producer says it is missing.
    pub reason: Option<String>,
    pub source: Option<RevisionRef>,
    pub artifact: Option<RevisionRef>,
    /// The bundle file holding its bytes.
    pub file: Option<String>,
    pub caveats: Vec<String>,
    pub metadata: BTreeMap<String, Value>,
}

/// Producer identifiers to canonical IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    /// The producer namespace of the acquisition.
    pub namespace: Option<String>,
    pub external_run_id: Option<String>,
    pub run: RevisionRef,
    pub items: Vec<MappedItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MappedItem {
    /// `envelope`, `manifest`, or an artifact's item.
    pub item: String,
    pub source: Option<RevisionRef>,
    pub artifact: RevisionRef,
}

/// One edge between two records the bundle shows.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LineageEdge {
    pub from: RevisionRef,
    /// `describes` (an envelope or manifest describes the Run), `records`
    /// (the Run records an artifact), `retained_from` (an artifact's Source)
    /// or `derived_from` (a derivative's input).
    pub relation: String,
    pub to: RevisionRef,
    pub transformation: Option<String>,
    pub version: Option<String>,
}

/// The JSON Schema of `producer-bundle.json`.
pub fn bundle_schema() -> Schema {
    schema_for!(ProducerBundle)
}

/// The renderer row of the registry.
pub struct ProducerBundleRenderer;

impl ExportRenderer for ProducerBundleRenderer {
    fn version(&self) -> &'static str {
        BUNDLE_FORMAT
    }

    fn plan(&self, input: &ExportInput<'_>, files: &mut PlanBuilder) -> Result<(), RenderError> {
        let state = input.case.state();
        let mut runs = Vec::new();
        for record in state.records_of_kind(Kind::Run) {
            if !input.includes(record) {
                continue;
            }
            let folder = format!("{RUNS_ROOT}/{:04}", runs.len() + 1);
            runs.push(plan_run(input, record, folder, files)?);
        }
        let bundle = ProducerBundle {
            schema_version: BundleVersion::V1,
            case_id: input.case.case_id(),
            audience: input.audience.as_str().to_owned(),
            envelope_version: ProducerRunVersion::V1,
            runs,
            statements: BundleStatements::fixed(input),
        };
        files.add(
            Phase::Views,
            PlanPath::new(INDEX_FILE)?,
            pretty(&bundle),
            None,
        )
    }
}

/// Every retained digest a bundle of `state` may copy: the artifacts each
/// Run names. The caller reads them with
/// [`super::source::read_retained`] before planning.
pub fn required_digests(state: &State) -> Vec<Digest> {
    let mut digests = BTreeSet::new();
    for run in state.records_of_kind(Kind::Run) {
        for (_, reference) in named_artifacts(&run.body) {
            if let Some(body) = current_artifact(state, reference)
                && body.availability == ArtifactAvailability::Retained
                && let Some(digest) = body.digest
            {
                digests.insert(digest);
            }
        }
    }
    digests.into_iter().collect()
}

fn pretty(value: &impl Serialize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("bundle JSON serializes");
    bytes.push(b'\n');
    bytes
}

fn revision_ref(value: &Value) -> Option<RevisionRef> {
    serde_json::from_value(value.clone()).ok()
}

/// Every artifact revision a Run body names, with the item it is known by.
fn named_artifacts(body: &Value) -> Vec<(String, RevisionRef)> {
    let mut named = Vec::new();
    if let Some(record) = body.get(PRODUCER_RUN_KEY) {
        if let Some(reference) = revision_ref(&record["envelope"]["artifact_ref"]) {
            named.push(("envelope".to_owned(), reference));
        }
        for entry in record["artifacts"].as_array().into_iter().flatten() {
            if let Some(reference) = revision_ref(&entry["retention"]["artifact_ref"]) {
                let path = entry["declared"]["path"].as_str().unwrap_or_default();
                let item = normalized_path(path).unwrap_or_else(|| path.to_owned());
                named.push((format!("artifact:{item}"), reference));
            }
        }
    }
    if let Some(finalization) = body.get("capture_finalization") {
        if let Some(reference) = revision_ref(&finalization["manifest"]["artifact_ref"]) {
            named.push(("manifest".to_owned(), reference));
        }
        for entry in finalization["artifacts"].as_array().into_iter().flatten() {
            if let Some(reference) = revision_ref(&entry["retention"]["artifact_ref"]) {
                let key = entry["key"].as_str().unwrap_or_default();
                named.push((key.to_owned(), reference));
            }
        }
    }
    for (index, value) in body["artifact_refs"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if let Some(reference) = revision_ref(value) {
            named.push((format!("recorded:{}", index + 1), reference));
        }
    }
    named
}

fn current_artifact(state: &State, reference: RevisionRef) -> Option<ArtifactBody> {
    derivation::artifact_body(state, reference)
}

/// What one run's plan needs to know about the artifacts it names.
struct Shown<'a> {
    input: &'a ExportInput<'a>,
    state: &'a State,
}

impl Shown<'_> {
    /// The current record of `id` when the audience may see it.
    fn record(&self, id: Id) -> Option<&Record> {
        self.state
            .current(id)
            .filter(|record| self.input.includes(record))
    }

    /// Whether an artifact and its Source may both appear.
    fn artifact(&self, reference: RevisionRef) -> bool {
        self.record(reference.id).is_some()
            && current_artifact(self.state, reference)
                .is_none_or(|body| self.record(body.source.id).is_some())
    }

    fn caveats(&self, id: Id) -> Vec<String> {
        self.record(id)
            .map(|record| {
                self.input
                    .caveats(record)
                    .iter()
                    .map(|caveat| caveat.text().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn level(&self, id: Id) -> Option<ClassificationLevel> {
        self.record(id)
            .map(|record| classification::effective_classification(self.state, record).level)
    }

    /// Retention now: an artifact recorded as retained whose bytes are no
    /// longer held says how they are held instead.
    fn availability(&self, reference: RevisionRef) -> String {
        current_artifact(self.state, reference)
            .map(|body| text_of(&body.availability))
            .unwrap_or_else(|| "missing".to_owned())
    }

    /// The bytes of an included, retained artifact.
    /// None when IDs are remapped, which would rewrite them.
    fn bytes(&self, reference: RevisionRef) -> Result<Option<(Digest, &[u8])>, RenderError> {
        if self.input.remapped() {
            return Ok(None);
        }
        let Some(body) = current_artifact(self.state, reference) else {
            return Ok(None);
        };
        if body.availability != ArtifactAvailability::Retained {
            return Ok(None);
        }
        let Some(digest) = body.digest else {
            return Ok(None);
        };
        let bytes = self.input.retained.get(&digest).ok_or_else(|| {
            RenderError::Snapshot(Failure::new(
                ErrorCode::IntegrityFailure,
                format!(
                    "The retained bytes of {digest} were not read before planning. Run \
                     investigation check."
                ),
            ))
        })?;
        Ok(Some((digest, bytes)))
    }
}

fn text_of(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Paths of one run's files folder, unique even when compared without case.
#[derive(Default)]
struct Layout {
    used: BTreeSet<String>,
}

impl Layout {
    /// `wanted` when it is a safe, unused path; `None` otherwise.
    fn claim(&mut self, wanted: &str) -> Option<String> {
        let normalized = normalized_path(wanted)?;
        PlanPath::new(normalized.clone()).ok()?;
        self.used
            .insert(normalized.to_lowercase())
            .then_some(normalized)
    }

    /// A fresh `artifact-N` path.
    fn fresh(&mut self) -> String {
        let mut number = self.used.len() + 1;
        loop {
            let path = format!("artifact-{number}");
            if let Some(path) = self.claim(&path) {
                return path;
            }
            number += 1;
        }
    }
}

/// Plan one included Run: its files, and its index entry.
fn plan_run(
    input: &ExportInput<'_>,
    record: &Record,
    folder: String,
    files: &mut PlanBuilder,
) -> Result<BundleRun, RenderError> {
    let state = input.case.state();
    let shown = Shown { input, state };
    let body = &record.body;
    let producer_run = body
        .get(PRODUCER_RUN_KEY)
        .and_then(|value| input::decode::<ProducerRunRecord>(value).ok());
    let origin = if producer_run.is_some() {
        RunOrigin::ProducerRun
    } else if body.get("capture").is_some_and(Value::is_object) {
        RunOrigin::Capture
    } else {
        RunOrigin::Recorded
    };
    let mut run = BundleRun {
        run: record.reference(),
        folder: folder.clone(),
        origin,
        caveats: shown.caveats(record.header.id),
        attribution: Attribution::ProducerReported,
        producer: None,
        external_run_id: None,
        intent: body["intent"]["intent"].as_str().map(str::to_owned),
        execution: recorded_execution(body),
        environment: BTreeMap::new(),
        environment_text: None,
        evidence_completeness: serde_json::from_value(body["evidence_completeness"].clone())
            .unwrap_or(EvidenceCompleteness::Unknown),
        evidence_validity: body["evidence_validity"]
            .as_str()
            .unwrap_or("unknown")
            .to_owned(),
        finalization: None,
        channels: Vec::new(),
        artifacts: Vec::new(),
        assertions: Vec::new(),
        controls: Vec::new(),
        receipts: Vec::new(),
        limitations: Vec::new(),
        metadata: BTreeMap::new(),
        original: None,
        reingest: None,
        not_reingestible: None,
        mapping: Mapping {
            namespace: None,
            external_run_id: None,
            run: record.reference(),
            items: Vec::new(),
        },
        lineage: Vec::new(),
    };
    let mut layout = Layout::default();
    let mut planned = Planned {
        folder: &folder,
        files,
    };
    match (origin, producer_run) {
        (RunOrigin::ProducerRun, Some(producer_run)) => {
            plan_envelope_run(&shown, &mut run, &producer_run, &mut layout, &mut planned)?;
        }
        (RunOrigin::Capture, _) => {
            plan_captured_run(&shown, &mut run, body, &mut layout, &mut planned)?;
        }
        _ => plan_recorded_run(&shown, &mut run, body, &mut layout, &mut planned)?,
    }
    run.lineage = lineage(&shown, &run);
    Ok(run)
}

/// The run folder and the plan its files go into.
struct Planned<'f> {
    folder: &'f str,
    files: &'f mut PlanBuilder,
}

impl Planned<'_> {
    fn add(&mut self, relative: &str, bytes: &[u8]) -> Result<String, RenderError> {
        let path = format!("{}/{relative}", self.folder);
        self.files
            .add(Phase::Records, PlanPath::new(path.clone())?, bytes, None)?;
        Ok(path)
    }

    fn files_folder(&self) -> String {
        format!("{}/{FILES_FOLDER}", self.folder)
    }
}

/// Copy an artifact's bytes into the run's files folder at `path`.
fn copy_artifact(
    shown: &Shown<'_>,
    planned: &mut Planned<'_>,
    reference: RevisionRef,
    path: &str,
) -> Result<Option<String>, RenderError> {
    match shown.bytes(reference)? {
        Some((_, bytes)) => Ok(Some(planned.add(&format!("{FILES_FOLDER}/{path}"), bytes)?)),
        None => Ok(None),
    }
}

fn source_of(state: &State, reference: RevisionRef) -> Option<RevisionRef> {
    current_artifact(state, reference).map(|body| body.source)
}

/// A run ingested from an `inv.producer-run.v1` envelope.
fn plan_envelope_run(
    shown: &Shown<'_>,
    run: &mut BundleRun,
    record: &ProducerRunRecord,
    layout: &mut Layout,
    planned: &mut Planned<'_>,
) -> Result<(), RenderError> {
    run.producer = Some(record.producer.clone());
    run.external_run_id = Some(record.external_run_id.clone());
    run.intent = Some(record.intent.label.clone());
    run.execution = record.execution.clone();
    run.environment = record.environment.clone();
    run.assertions = values(&record.assertions);
    run.controls = values(&record.controls);
    run.receipts = values(&record.receipts);
    run.limitations = record.limitations.clone();
    run.metadata = record.metadata.clone();
    run.mapping.namespace = Some(record.producer.name.clone());
    run.mapping.external_run_id = Some(record.external_run_id.clone());

    // The original envelope may be copied only when it names nothing withheld.
    let mut verbatim = shown.artifact(record.envelope.artifact_ref);
    let mut declared = Vec::new();
    for entry in &record.artifacts {
        let path = normalized_path(&entry.declared.path).unwrap_or(entry.declared.path.clone());
        let item = format!("artifact:{path}");
        let mut artifact = BundleArtifact {
            item: item.clone(),
            role: Some(entry.declared.role.clone()),
            declared_path: Some(entry.declared.path.clone()),
            media_type: entry.declared.media_type.clone(),
            channel: None,
            classification: Some(entry.declared.classification),
            retention: String::new(),
            digest: None,
            size_bytes: None,
            expected_sha256: entry.declared.expected_sha256.clone(),
            reason: None,
            source: None,
            artifact: None,
            file: None,
            caveats: Vec::new(),
            metadata: entry.declared.metadata.clone(),
        };
        let mut file_path = None;
        match &entry.retention {
            Retention::Retained {
                source_ref,
                artifact_ref,
                digest,
                size_bytes,
            } => {
                if !shown.artifact(*artifact_ref) {
                    verbatim = false;
                    continue;
                }
                let placed = layout.claim(&path);
                if placed.is_none() {
                    verbatim = false;
                }
                let placed = placed.unwrap_or_else(|| layout.fresh());
                artifact.retention = shown.availability(*artifact_ref);
                artifact.digest = Some(digest.clone());
                artifact.size_bytes = Some(*size_bytes);
                artifact.source = Some(*source_ref);
                artifact.artifact = Some(*artifact_ref);
                artifact.classification = shown.level(artifact_ref.id);
                artifact.caveats = shown.caveats(artifact_ref.id);
                artifact.file = copy_artifact(shown, planned, *artifact_ref, &placed)?;
                if artifact.file.is_some() {
                    file_path = Some(placed);
                }
                run.mapping.items.push(MappedItem {
                    item,
                    source: Some(*source_ref),
                    artifact: *artifact_ref,
                });
            }
            Retention::Missing => artifact.retention = "missing".to_owned(),
            Retention::Rejected { actual } => {
                artifact.retention = "rejected".to_owned();
                artifact.digest = Some(actual.clone());
            }
        }
        declared.push((entry.declared.clone(), file_path));
        run.artifacts.push(artifact);
    }

    let envelope_ref = record.envelope.artifact_ref;
    if shown.artifact(envelope_ref) {
        run.mapping.items.insert(
            0,
            MappedItem {
                item: "envelope".to_owned(),
                source: Some(record.envelope.source_ref),
                artifact: envelope_ref,
            },
        );
    }
    let original = if verbatim {
        shown.bytes(envelope_ref)?
    } else {
        None
    };
    if let Some((digest, bytes)) = original {
        let path = planned.add(ENVELOPE_FILE, bytes)?;
        run.original = Some(BundleFile {
            path: path.clone(),
            sha256: digest.clone(),
            bytes: bytes.len() as u64,
            artifact: envelope_ref,
        });
        run.reingest = Some(Reingest {
            envelope: path,
            artifact_folder: planned.files_folder(),
            verbatim: true,
            sha256: digest,
        });
        return Ok(());
    }
    // Rebuilt from what is shown, with the paths the files were placed at.
    let mut paths = BTreeMap::new();
    let artifacts: Vec<Value> = declared
        .into_iter()
        .map(|(mut artifact, placed)| {
            let item = format!(
                "artifact:{}",
                normalized_path(&artifact.path).unwrap_or(artifact.path.clone())
            );
            if let Some(placed) = placed {
                artifact.path = placed;
            }
            paths.insert(item, artifact.path.clone());
            json!(artifact)
        })
        .collect();
    let receipts = receipts_for(&run.receipts, &paths, |receipt| {
        receipt["artifact"]
            .as_str()
            .and_then(normalized_path)
            .map(|path| format!("artifact:{path}"))
    });
    let mut envelope = json!({
        "schema_version": ProducerRunVersion::V1,
        "producer": record.producer,
        "external_run_id": record.external_run_id,
        "intent": record.intent,
        "execution": record.execution,
        "environment": record.environment,
        "artifacts": artifacts,
        "assertions": record.assertions,
        "controls": record.controls,
        "receipts": receipts,
        "limitations": record.limitations,
        "classification": shown.level(run.run.id).unwrap_or_default(),
    });
    if !record.metadata.is_empty() {
        envelope["metadata"] = json!(record.metadata);
    }
    finish_rebuilt(shown, run, envelope, planned)
}

fn values<T: Serialize>(items: &[T]) -> Vec<Value> {
    items.iter().map(|item| json!(item)).collect()
}

/// Receipts of a rebuilt envelope: one naming an artifact points at the
/// path that artifact has in the envelope (`paths`, by item), and loses that
/// pointer when the artifact is not in it; one left with neither pointer nor
/// reference is dropped, as the envelope requires one of them.
fn receipts_for(
    receipts: &[Value],
    paths: &BTreeMap<String, String>,
    item_of: impl Fn(&Value) -> Option<String>,
) -> Vec<Value> {
    receipts
        .iter()
        .filter_map(|receipt| {
            let mut receipt = receipt.clone();
            if let Some(item) = item_of(&receipt) {
                match paths.get(&item) {
                    Some(path) => receipt["artifact"] = json!(path),
                    None => {
                        receipt.as_object_mut()?.remove("artifact");
                    }
                }
            }
            let object = receipt.as_object()?;
            (object.contains_key("artifact") || object.contains_key("reference")).then_some(receipt)
        })
        .collect()
}

/// Decode a rebuilt envelope as the ingest path would, then plan it.
fn finish_rebuilt(
    shown: &Shown<'_>,
    run: &mut BundleRun,
    envelope: Value,
    planned: &mut Planned<'_>,
) -> Result<(), RenderError> {
    if shown.input.remapped() {
        run.not_reingestible = Some(NotReingestible::IdentifiersRemapped);
        return Ok(());
    }
    let valid = serde_json::from_value::<ProducerRun>(envelope.clone())
        .ok()
        .filter(|decoded| decoded.validate().is_ok());
    let Some(decoded) = valid else {
        run.not_reingestible = Some(NotReingestible::EnvelopeInvalid);
        return Ok(());
    };
    // Ingest computes completeness from the artifact inventory alone. A run
    // recorded weaker than that (missing stream positions, channel gaps)
    // would read stronger once re-imported, so it gets no envelope.
    let retained = run
        .artifacts
        .iter()
        .filter(|artifact| artifact.file.is_some())
        .count();
    let inventory = if retained == run.artifacts.len() {
        EvidenceCompleteness::Complete
    } else if retained == 0 {
        EvidenceCompleteness::Missing
    } else {
        EvidenceCompleteness::Partial
    };
    if strength(inventory) > strength(run.evidence_completeness) {
        run.not_reingestible = Some(NotReingestible::IncompleteBeyondInventory);
        return Ok(());
    }
    let bytes = pretty(&decoded);
    let digest = investigation_store::journal::hash_bytes(&bytes);
    let path = planned.add(ENVELOPE_FILE, &bytes)?;
    run.reingest = Some(Reingest {
        envelope: path,
        artifact_folder: planned.files_folder(),
        verbatim: false,
        sha256: digest,
    });
    Ok(())
}

/// How much evidence a completeness claims, for comparison.
const fn strength(completeness: EvidenceCompleteness) -> u8 {
    match completeness {
        EvidenceCompleteness::Complete => 3,
        EvidenceCompleteness::Partial => 2,
        EvidenceCompleteness::Missing => 1,
        EvidenceCompleteness::Unknown => 0,
    }
}

/// A source time as the envelope's verbatim text.
fn reported(value: &Value) -> (Option<String>, Option<ClockQuality>) {
    match serde_json::from_value::<SourceTime>(value.clone()) {
        Ok(time) => (
            match time.timestamp {
                Knowledge::Known(text) => Some(text),
                Knowledge::Unknown => None,
            },
            Some(time.clock_quality),
        ),
        Err(_) => (value.as_str().map(str::to_owned), None),
    }
}

/// Execution of a Run recorded without an envelope.
fn recorded_execution(body: &Value) -> ProducerExecution {
    let (started_at, started_clock) = reported(&body["started_at"]);
    let (ended_at, ended_clock) = reported(&body["finished_at"]);
    ProducerExecution {
        state: serde_json::from_value(body["execution_status"].clone())
            .unwrap_or(ExecutionStatus::Unknown),
        exit_code: None,
        started_at,
        ended_at,
        clock_quality: started_clock
            .or(ended_clock)
            .unwrap_or(ClockQuality::Unknown),
        error: None,
    }
}

/// A run recorded through the capture protocol.
fn plan_captured_run(
    shown: &Shown<'_>,
    run: &mut BundleRun,
    body: &Value,
    layout: &mut Layout,
    planned: &mut Planned<'_>,
) -> Result<(), RenderError> {
    let capture = &body["capture"];
    let name = capture["producer"].as_str().map(str::to_owned);
    run.external_run_id = capture["external_run_id"].as_str().map(str::to_owned);
    run.mapping.namespace.clone_from(&name);
    run.mapping.external_run_id.clone_from(&run.external_run_id);
    let mut metadata: BTreeMap<String, Value> =
        serde_json::from_value(capture["metadata"].clone()).unwrap_or_default();
    run.producer = name.as_ref().map(|name| ProducerDescriptor {
        name: name.clone(),
        version: None,
        instance: None,
    });
    let Some(finalization) = body.get("capture_finalization").filter(|v| v.is_object()) else {
        run.finalization = Some("not_finalized".to_owned());
        run.metadata = metadata;
        run.not_reingestible = Some(NotReingestible::NotFinalized);
        return Ok(());
    };
    if let Ok(producer) =
        serde_json::from_value::<ProducerDescriptor>(finalization["producer"].clone())
    {
        run.producer = Some(producer);
    }
    run.finalization = finalization["finalization"].as_str().map(str::to_owned);
    run.channels = list(&finalization["channels"]);
    run.assertions = list(&finalization["assertions"]);
    run.controls = list(&finalization["controls"]);
    run.receipts = list(&finalization["receipts"]);
    run.limitations =
        serde_json::from_value(finalization["limitations"].clone()).unwrap_or_default();
    let finalized: BTreeMap<String, Value> =
        serde_json::from_value(finalization["metadata"].clone()).unwrap_or_default();
    metadata.extend(finalized);
    run.metadata = metadata;
    let execution = &finalization["execution"];
    let (started_at, started_clock) = reported(&execution["started_at"]);
    let (ended_at, ended_clock) = reported(&execution["ended_at"]);
    run.execution = ProducerExecution {
        state: serde_json::from_value(execution["state"].clone())
            .unwrap_or(ExecutionStatus::Unknown),
        exit_code: execution["exit_code"].as_i64(),
        started_at,
        ended_at,
        clock_quality: started_clock
            .or(ended_clock)
            .unwrap_or(ClockQuality::Unknown),
        error: execution["error"].as_str().map(str::to_owned),
    };

    let manifest_ref = revision_ref(&finalization["manifest"]["artifact_ref"]);
    let mut every_artifact_shown = true;
    let mut declared = Vec::new();
    for entry in finalization["artifacts"].as_array().into_iter().flatten() {
        let key = entry["key"].as_str().unwrap_or_default().to_owned();
        let retention = &entry["retention"];
        let state = retention["state"].as_str().unwrap_or("missing");
        let mut artifact = BundleArtifact {
            item: key.clone(),
            role: entry["role"].as_str().map(str::to_owned),
            declared_path: None,
            media_type: entry["media_type"].as_str().map(str::to_owned),
            channel: entry["channel"].as_str().map(str::to_owned),
            classification: None,
            retention: state.to_owned(),
            digest: None,
            size_bytes: None,
            expected_sha256: None,
            reason: retention["reason"].as_str().map(str::to_owned),
            source: None,
            artifact: None,
            file: None,
            caveats: Vec::new(),
            metadata: serde_json::from_value(entry["metadata"].clone()).unwrap_or_default(),
        };
        let placed;
        if let Some(artifact_ref) = revision_ref(&retention["artifact_ref"]) {
            if !shown.artifact(artifact_ref) {
                every_artifact_shown = false;
                continue;
            }
            let path = layout.claim(&key).unwrap_or_else(|| layout.fresh());
            artifact.retention = shown.availability(artifact_ref);
            artifact.digest = serde_json::from_value(retention["digest"].clone()).ok();
            artifact.size_bytes = retention["size_bytes"].as_u64();
            artifact.source = revision_ref(&retention["source_ref"]);
            artifact.artifact = Some(artifact_ref);
            artifact.classification = shown.level(artifact_ref.id);
            artifact.caveats = shown.caveats(artifact_ref.id);
            artifact.file = copy_artifact(shown, planned, artifact_ref, &path)?;
            placed = path;
            run.mapping.items.push(MappedItem {
                item: key.clone(),
                source: artifact.source,
                artifact: artifact_ref,
            });
        } else {
            placed = layout.claim(&key).unwrap_or_else(|| layout.fresh());
        }
        declared.push((placed, artifact.clone()));
        run.artifacts.push(artifact);
    }
    if let Some(manifest_ref) = manifest_ref.filter(|reference| shown.artifact(*reference)) {
        run.mapping.items.insert(
            0,
            MappedItem {
                item: "manifest".to_owned(),
                source: source_of(shown.state, manifest_ref),
                artifact: manifest_ref,
            },
        );
        if every_artifact_shown && let Some((digest, bytes)) = shown.bytes(manifest_ref)? {
            let path = planned.add(CAPTURE_MANIFEST_FILE, bytes)?;
            run.original = Some(BundleFile {
                path,
                sha256: digest,
                bytes: bytes.len() as u64,
                artifact: manifest_ref,
            });
        }
    }

    let Some(producer) = &run.producer else {
        run.not_reingestible = Some(NotReingestible::NoProducerIdentity);
        return Ok(());
    };
    let mut paths = BTreeMap::new();
    let artifacts: Vec<Value> = declared
        .into_iter()
        .map(|(placed, artifact)| {
            paths.insert(artifact.item.clone(), placed.clone());
            let mut entry = json!({
                "role": artifact.role.clone().unwrap_or_else(|| UNDECLARED_ROLE.to_owned()),
                "path": placed,
                "classification": artifact.classification.unwrap_or_else(|| shown.level(run.run.id).unwrap_or_default()),
            });
            if artifact.file.is_some()
                && let Some(digest) = &artifact.digest
            {
                entry["expected_sha256"] = json!(digest.as_str().trim_start_matches("sha256:"));
            }
            if let Some(media_type) = &artifact.media_type {
                entry["media_type"] = json!(media_type);
            }
            if !artifact.metadata.is_empty() {
                entry["metadata"] = json!(artifact.metadata);
            }
            entry
        })
        .collect();
    // A receipt names a captured item by key; in the envelope it names the
    // artifact's path.
    let receipts = receipts_for(&run.receipts, &paths, |receipt| {
        receipt["artifact"].as_str().map(str::to_owned)
    });
    let mut envelope = json!({
        "schema_version": ProducerRunVersion::V1,
        "producer": producer,
        "external_run_id": run.external_run_id,
        "intent": {"label": run.intent.clone().unwrap_or_else(|| "captured run".to_owned())},
        "execution": run.execution,
        "artifacts": artifacts,
        "assertions": run.assertions,
        "controls": run.controls,
        "receipts": receipts,
        "limitations": run.limitations,
        "classification": shown.level(run.run.id).unwrap_or_default(),
    });
    if !run.metadata.is_empty() {
        envelope["metadata"] = json!(run.metadata);
    }
    finish_rebuilt(shown, run, envelope, planned)
}

fn list(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}

/// A run recorded by an investigator: a record with its artifacts, and no
/// envelope, since it has no producer acquisition identity.
fn plan_recorded_run(
    shown: &Shown<'_>,
    run: &mut BundleRun,
    body: &Value,
    layout: &mut Layout,
    planned: &mut Planned<'_>,
) -> Result<(), RenderError> {
    run.environment_text = body["environment"].as_str().map(str::to_owned);
    run.not_reingestible = Some(NotReingestible::NoProducerIdentity);
    for (item, reference) in named_artifacts(body) {
        if !shown.artifact(reference) {
            continue;
        }
        let artifact_body = current_artifact(shown.state, reference);
        let path = layout.fresh();
        let file = copy_artifact(shown, planned, reference, &path)?;
        let source = artifact_body.as_ref().map(|body| body.source);
        run.artifacts.push(BundleArtifact {
            item: item.clone(),
            role: artifact_body.as_ref().map(|body| text_of(&body.role())),
            declared_path: None,
            media_type: artifact_body.as_ref().and_then(|body| body.format.clone()),
            channel: None,
            classification: shown.level(reference.id),
            retention: shown.availability(reference),
            digest: artifact_body.as_ref().and_then(|body| body.digest.clone()),
            size_bytes: artifact_body.as_ref().and_then(|body| body.size_bytes),
            expected_sha256: None,
            reason: None,
            source,
            artifact: Some(reference),
            file,
            caveats: shown.caveats(reference.id),
            metadata: BTreeMap::new(),
        });
        run.mapping.items.push(MappedItem {
            item,
            source,
            artifact: reference,
        });
    }
    Ok(())
}

/// Edges between records the run entry shows: the description to the Run,
/// the Run to each artifact, each artifact to its Source, and each
/// derivation step to its inputs, when both ends are shown.
fn lineage(shown: &Shown<'_>, run: &BundleRun) -> Vec<LineageEdge> {
    let visible = |reference: RevisionRef| shown.record(reference.id).is_some();
    let mut edges = BTreeSet::new();
    let edge = |from, relation: &str, to| LineageEdge {
        from,
        relation: relation.to_owned(),
        to,
        transformation: None,
        version: None,
    };
    for item in &run.mapping.items {
        if item.item == "envelope" || item.item == "manifest" {
            edges.insert(edge(item.artifact, "describes", run.run));
        } else {
            edges.insert(edge(run.run, "records", item.artifact));
        }
        if let Some(source) = item.source.filter(|source| visible(*source)) {
            edges.insert(edge(item.artifact, "retained_from", source));
        }
        let Some(lineage) = derivation::lineage(shown.state, item.artifact) else {
            continue;
        };
        for step in lineage.steps {
            if !visible(step.artifact_ref) {
                continue;
            }
            for input in step.inputs.into_iter().filter(|input| visible(*input)) {
                edges.insert(LineageEdge {
                    from: step.artifact_ref,
                    relation: "derived_from".to_owned(),
                    to: input,
                    transformation: Some(step.transformation.clone()),
                    version: Some(step.version.clone()),
                });
            }
        }
    }
    edges.into_iter().collect()
}
