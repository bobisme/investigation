use std::io::Read;

use investigation_core::{
    case::{CaseCreate, CaseMode},
    evidence::{
        AcquisitionChannel, ArtifactAvailability, ArtifactBody, ArtifactIntegrity, ArtifactRole,
        ArtifactTransform, SourceAccess, SourceAcquisition, SourceBody,
    },
    header::{
        ActorDescriptor, Classification, ClassificationLevel, ClockQuality, Knowledge,
        RecordHeader, Revision, SchemaVersion, SourceTime, UtcTimestamp,
    },
    input,
    state::Record,
    wire::{Digest, RecordChange},
};
use schemars::{JsonSchema, Schema, schema_for};
use serde::Deserialize;

use super::*;
use crate::{blob::stage, import_paths::ImportRoot};

#[path = "acquisition.rs"]
pub mod acquisition;
#[path = "producer_run.rs"]
pub mod producer_run;
use acquisition::{AcquisitionKeyRequest, ItemFacts, ItemOutcome, ItemPlan, PriorAcquisition};

#[path = "derivation.rs"]
pub mod derivation;
use derivation::{DerivedFromRequest, ResolvedDerivation};

#[path = "reference.rs"]
pub mod reference;
use reference::{ExternalRefRequest, PreparedReference, RetentionMode, ReverifyRequest};

#[cfg(test)]
#[path = "ingest_streaming_tests.rs"]
pub(crate) mod streaming_tests;

/// Largest copied import: a local file, standard input or a producer-run
/// artifact. Every copy streams through one fixed buffer, so this bounds the
/// disk a copy may take, not memory. A larger local file can be hashed where it
/// lies with `retention: reference`.
pub const MAX_IMPORT_BYTES: u64 = 64 << 30;
/// Bytes streamed between two progress reports of one item on the log.
pub const PROGRESS_INTERVAL_BYTES: u64 = 64 << 20;
/// Log target of streaming progress. Events carry byte counts only.
pub const PROGRESS_TARGET: &str = "investigation::ingest";
pub const MAX_ITEMS: usize = 32;
pub const TEXT_PARSER_VERSION: &str = "inv.text.utf8.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum IngestVersion {
    #[serde(rename = "inv.ingest.v1", alias = "inv.ingest.v0-alpha")]
    V1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: IngestVersion,
    #[schemars(length(min = 1, max = 128))]
    pub command_id: String,
    pub actor: Actor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_descriptor: Option<ActorDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case_id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_case: Option<CaseCreate>,
    pub source: SourceManifest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<StreamContent>,
    #[serde(default)]
    pub extract_text: bool,
    #[serde(default)]
    pub classification: ClassificationLevel,
    /// The items are outputs of a transformation of recorded Artifacts. They are
    /// recorded as derivatives of those inputs: no Source or acquisition is
    /// created, `source.title` labels the output and `source.format` gives its
    /// format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<DerivedFromRequest>,
    /// Reference-only items: bytes held at an external location, named by an
    /// opaque locator with their expected digest and size. Nothing is read or
    /// copied; each Artifact is `external` and unverified until `reverify`.
    /// Exclusive with `paths` and `content`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_refs: Vec<ExternalRefRequest>,
    /// `copy` (default) snapshots `paths` into the workspace. `reference`
    /// hashes them without copying, for large files: the Artifacts are
    /// `external` and cannot satisfy an offline-completeness guarantee. A
    /// failed copy is a failed ingest, never a fallback to `reference`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<RetentionMode>,
    /// Compare the supplied bytes (one path or stdin) with an exact
    /// reference-only Artifact revision. A match records an attributed
    /// verification in a new revision and, for copied bytes, retains them. A
    /// mismatch records a failed verification; the recorded digest never
    /// changes and records citing the Artifact need review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverify: Option<ReverifyRequest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceManifest {
    pub title: String,
    pub producer: Option<String>,
    pub publisher: Option<String>,
    pub identity: Option<String>,
    pub format: Option<String>,
    pub context: Option<String>,
    #[serde(default)]
    pub verification_limits: Vec<String>,
    /// Opaque producer metadata retained with the resulting Source.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub metadata: std::collections::BTreeMap<String, Value>,
    /// Stable identity of the logical acquisition. Without it every ingest is
    /// a separate acquisition, even when its bytes match an earlier one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquisition_key: Option<AcquisitionKeyRequest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamContent {
    Stdin { name: String },
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ItemReceipt {
    pub source_id: Id,
    pub artifact_id: Id,
    pub digest: Digest,
    pub size_bytes: u64,
    pub derivative_id: Option<Id>,
    pub text_status: TextStatus,
    // Diagnostics about content the core retained but did not interpret.
    // Published unconstrained: frozen v1 only allows additive unconstrained properties.
    #[schemars(schema_with = "unconstrained")]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    // Acquisition identity outcome, always present in new receipts: `imported`,
    // `deduplicated` (a known acquisition; nothing new recorded) or `revised`
    // (a known acquisition with a changed description or bytes). Published
    // unconstrained like `diagnostics`, as are the key and prior link.
    #[schemars(schema_with = "unconstrained")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ItemOutcome>,
    #[schemars(schema_with = "unconstrained")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquisition_key: Option<investigation_core::evidence::AcquisitionKey>,
    // Records of the earlier import of this acquisition item.
    #[schemars(schema_with = "unconstrained")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior: Option<PriorAcquisition>,
    // For a derivative: its role, origins and derivation steps, traced to the
    // original acquisitions. Published unconstrained like `outcome`.
    #[schemars(schema_with = "unconstrained")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<investigation_core::derivation::Lineage>,
    // For an Artifact whose bytes are not retained, or that was reverified:
    // `state` (`source_unavailable` or `retained`), locator, expected digest
    // and size, and the verification standing. Published unconstrained.
    #[schemars(schema_with = "unconstrained")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<investigation_core::external_ref::Retention>,
}

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextStatus {
    NotRequested,
    Decoded,
    InvalidUtf8,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IngestReceipt {
    #[serde(flatten)]
    pub receipt: Receipt,
    pub items: Vec<ItemReceipt>,
    pub manifest_artifact_id: Option<Id>,
}

struct PreparedItem {
    label: String,
    /// Label safe to record in identities and titles: never an absolute local path.
    display: String,
    digest: Digest,
    size: u64,
    channel: AcquisitionChannel,
    text: TextStatus,
    diagnostics: Vec<String>,
    /// Set when the bytes are not retained.
    reference: Option<PreparedReference>,
}

fn unconstrained(_: &mut schemars::SchemaGenerator) -> Schema {
    true.into()
}

pub fn manifest_schema() -> Schema {
    schema_for!(Manifest)
}

pub fn receipt_schema() -> Schema {
    schema_for!(IngestReceipt)
}

pub fn parse_manifest(bytes: &[u8]) -> Result<(Manifest, Value), Failure> {
    let raw = input::parse_request(bytes).map_err(|error| invalid(error.message, &error.path))?;
    let manifest: Manifest =
        input::decode(&raw).map_err(|error| invalid(error.message, &error.path))?;
    if manifest.command_id.is_empty()
        || manifest.command_id.chars().count() > 128
        || manifest.command_id.chars().any(char::is_control)
    {
        return Err(invalid(
            "Provide a command ID with 1–128 characters.",
            "/command_id",
        ));
    }
    match (&manifest.new_case, manifest.case_id) {
        (Some(case), None)
            if case.mode == CaseMode::Explore
                && !case.title.trim().is_empty()
                && case.title.chars().count() <= 256 => {}
        (None, Some(id)) if id.kind() == Kind::Case => {}
        _ => {
            return Err(invalid(
                "Choose one existing case_id or an explore new_case with a title.",
                "/case_id",
            ));
        }
    }
    let external = !manifest.external_refs.is_empty();
    if (!external && manifest.paths.is_empty() == manifest.content.is_none())
        || manifest.paths.len() > MAX_ITEMS
    {
        return Err(invalid(
            "Supply a bounded nonempty paths array or one stdin content descriptor.",
            "/paths",
        ));
    }
    if manifest.source.title.trim().is_empty() || manifest.source.title.chars().count() > 256 {
        return Err(invalid(
            "Provide a source title of 1–256 characters.",
            "/source/title",
        ));
    }
    if let Some(key) = &manifest.source.acquisition_key {
        key.validate()?;
        let mut labels = std::collections::BTreeSet::new();
        for (index, path) in manifest.paths.iter().enumerate() {
            if !labels.insert(path.to_string_lossy()) {
                return Err(invalid(
                    "Each item of a keyed acquisition needs a distinct path.",
                    &format!("/paths/{index}"),
                ));
            }
        }
    }
    crate::opaque::validate_metadata(&manifest.source.metadata)
        .map_err(|(message, key)| invalid(&message, &format!("/source/metadata{key}")))?;
    if let Some(derived_from) = &manifest.derived_from {
        derived_from.validate(&manifest)?;
    }
    reference::validate(&manifest)?;
    Ok((manifest, raw))
}

impl<F: DurableFs> Store<F> {
    /// Snapshot local inputs before acquiring the canonical commit lock.
    pub fn ingest(
        &mut self,
        bytes: &[u8],
        stream: Option<&mut dyn Read>,
    ) -> Result<IngestReceipt, Failure> {
        self.ingest_observed(bytes, stream, &mut |_| {})
    }

    /// [`Self::ingest`], calling `during` with the bytes read so far of the
    /// item being copied or hashed, after each read of it. Every item streams
    /// through one fixed buffer and no lock is held while it does.
    pub fn ingest_observed(
        &mut self,
        bytes: &[u8],
        stream: Option<&mut dyn Read>,
        during: &mut dyn FnMut(u64),
    ) -> Result<IngestReceipt, Failure> {
        let (manifest, raw) = parse_manifest(bytes)?;
        let mut stages = Vec::new();
        let result = self.ingest_inner(bytes, manifest, raw, stream, &mut stages, during);
        if result
            .as_ref()
            .is_err_and(|error| error.error.code == ErrorCode::DurabilityUnknown)
        {
            self.note_stopped();
        }
        for staged in &stages {
            let _ = self.fs.remove(&staged.temp);
        }
        result
    }

    fn ingest_inner(
        &mut self,
        bytes: &[u8],
        manifest: Manifest,
        raw: Value,
        stream: Option<&mut dyn Read>,
        stages: &mut Vec<StagedBlob>,
        during: &mut dyn FnMut(u64),
    ) -> Result<IngestReceipt, Failure> {
        if manifest.reverify.is_some() {
            return self.reverify(bytes, &manifest, raw, stream, stages);
        }
        let mut prepared = Vec::new();
        for (index, path) in manifest.paths.iter().enumerate() {
            if reference::reference_mode(&manifest) {
                // Hashed where it lies; never copied, so never retained.
                let (digest, size) = self.read_reference(path, index, during)?;
                let label = path.to_string_lossy().into_owned();
                prepared.push(PreparedItem {
                    reference: Some(PreparedReference {
                        locator: label.clone(),
                        basis: investigation_core::external_ref::DigestBasis::ReadAtIngest,
                    }),
                    display: self.display_label(path),
                    label,
                    digest,
                    size,
                    channel: AcquisitionChannel::LocalFile,
                    text: TextStatus::NotRequested,
                    diagnostics: Vec::new(),
                });
                continue;
            }
            let mut opened = self.open_import(path, index, MAX_IMPORT_BYTES)?;
            let staged = stage_watched(
                &mut self.fs,
                &self.gate,
                Limited::new(&mut opened.file),
                during,
            )?;
            stages.push(staged);
            if !opened
                .witness
                .verify_unchanged(&opened.file)
                .map_err(io_failure)?
            {
                return Err(Failure::new(
                    ErrorCode::IoFailure,
                    "The source changed while it was read. Retry with a stable snapshot.",
                )
                .at(format!("/paths/{index}")));
            }
            prepared.push(self.prepare_item(
                stages.last().expect("just staged"),
                path.to_string_lossy().into_owned(),
                self.display_label(path),
                AcquisitionChannel::LocalFile,
                manifest.extract_text,
            )?);
        }
        if let Some(StreamContent::Stdin { name }) = &manifest.content {
            if name.trim().is_empty() || name.chars().count() > 256 {
                return Err(invalid(
                    "Name the stdin source with 1–256 characters.",
                    "/content/name",
                ));
            }
            let stream = stream.ok_or_else(|| {
                invalid(
                    "Supply original bytes on stdin using a manifest file.",
                    "/content",
                )
            })?;
            stages.push(stage_watched(
                &mut self.fs,
                &self.gate,
                Limited::new(stream),
                during,
            )?);
            prepared.push(self.prepare_item(
                stages.last().expect("just staged"),
                name.clone(),
                name.clone(),
                AcquisitionChannel::Producer,
                manifest.extract_text,
            )?);
        }
        for item in &manifest.external_refs {
            prepared.push(PreparedItem {
                label: item.locator.clone(),
                display: item.locator.clone(),
                digest: item.digest.clone(),
                size: item.size_bytes,
                channel: AcquisitionChannel::ExternalReference,
                text: TextStatus::NotRequested,
                diagnostics: Vec::new(),
                reference: Some(PreparedReference {
                    locator: item.locator.clone(),
                    basis: investigation_core::external_ref::DigestBasis::Declared,
                }),
            });
        }
        // Only retained bytes are attachments; a reference has no blob.
        let attachments = prepared
            .iter()
            .filter(|item| item.reference.is_none())
            .map(|item| item.digest.clone())
            .collect();
        let staged_manifest = stage_input(&mut self.fs, &self.gate, bytes)?;
        let manifest_digest = digest_for(&staged_manifest)?;
        let manifest_size = staged_manifest.size;
        stages.push(staged_manifest);
        let description = description_digest(&manifest)?;
        let facts: Vec<_> = prepared
            .iter()
            .map(|item| ItemFacts {
                label: &item.label,
                digest: &item.digest,
                decoded: matches!(item.text, TextStatus::Decoded),
                retained: item.reference.is_none(),
            })
            .collect();
        let mut plans = None;
        let receipt = self.commit_plan(
            CommitPlan {
                case_id: manifest.case_id,
                event: EventInput {
                    actor: &manifest.actor,
                    command_id: &manifest.command_id,
                    preconditions: &[],
                    manifest_digest: Some(manifest_digest.clone()),
                    session_reads: None,
                },
                request: SemanticRequest {
                    input: raw,
                    attachments,
                },
                blobs: stages,
            },
            &mut SystemClock,
            &mut SystemIdSource,
            |_, _, acceptance, visible| {
                let derivation = match (&manifest.derived_from, visible.target) {
                    (Some(request), Some(case)) => {
                        Some(derivation::resolve(request, &manifest, case)?)
                    }
                    (Some(_), None) => {
                        return Err(invalid(
                            "A derivative needs the existing case that holds its inputs.",
                            "/case_id",
                        ));
                    }
                    (None, _) => None,
                };
                let resolved = resolve_acquisition(&manifest, &description, &facts, visible)?;
                let plans = plans.insert(resolved);
                if let Some((case_id, sequence)) = existing_transaction(plans) {
                    return Ok(Prepared::Existing { case_id, sequence });
                }
                create_records(
                    &manifest,
                    &prepared,
                    plans,
                    derivation.as_ref(),
                    &description,
                    &manifest_digest,
                    manifest_size,
                    acceptance,
                )
                .map(Prepared::Accept)
            },
        )?;
        let plans = match plans {
            Some(plans) => plans,
            // An identical earlier request answered before planning: resolve
            // its items against the case as it stood before that transaction.
            None => self.replan(&manifest, &description, &facts, &receipt)?,
        };
        // A derivative's receipt reports its lineage from the committed state, so
        // an identical retry answers the same way.
        let derived = match (&manifest.derived_from, manifest.case_id) {
            (Some(_), Some(case_id)) => Some(self.state_after_write(case_id)?),
            _ => None,
        };
        let references = prepared.iter().any(|item| item.reference.is_some());
        let mut receipt = ingest_receipt(receipt, prepared, plans, derived.as_ref());
        if references {
            self.label_retention(&manifest, &mut receipt)?;
        }
        if receipt
            .items
            .iter()
            .all(|item| item.outcome == Some(ItemOutcome::Deduplicated))
            && receipt.receipt.command_id != manifest.command_id
        {
            receipt.receipt.warnings.push(format!(
                "Every item is already recorded for this acquisition, so command {} committed nothing. The receipt describes the transaction that recorded the current revision.",
                manifest.command_id
            ));
        }
        Ok(receipt)
    }

    fn replan(
        &mut self,
        manifest: &Manifest,
        description: &Digest,
        facts: &[ItemFacts<'_>],
        receipt: &Receipt,
    ) -> Result<Vec<ItemPlan>, Failure> {
        let Some(request) = &manifest.source.acquisition_key else {
            return Ok(acquisition::unkeyed(facts.len()));
        };
        let case_id = manifest.case_id.or_else(|| {
            receipt
                .created
                .iter()
                .find(|change| change.id.kind() == Kind::Case)
                .map(|change| change.id)
        });
        let Some(case_id) = case_id else {
            return Ok(acquisition::fresh(request, facts));
        };
        let replay = self.case(case_id)?;
        acquisition::resolve(
            case_id,
            &replay,
            receipt.snapshot.sequence.saturating_sub(1),
            request,
            description,
            manifest.classification,
            facts,
        )
    }

    /// Report the retention of each item whose bytes are not retained, read
    /// from the committed state so an identical retry answers the same way.
    fn label_retention(
        &mut self,
        manifest: &Manifest,
        receipt: &mut IngestReceipt,
    ) -> Result<(), Failure> {
        let created_case = receipt
            .receipt
            .created
            .iter()
            .find(|change| change.id.kind() == Kind::Case)
            .map(|change| change.id);
        let Some(first) = receipt.items.first() else {
            return Ok(());
        };
        let state = match manifest.case_id.or(created_case) {
            Some(case_id) => self.state_after_write(case_id)?,
            // A deduplicated new_case manifest answers from the acquisition's home case.
            None => match self
                .cases()?
                .into_values()
                .find(|case| case.state.current(first.artifact_id).is_some())
            {
                Some(case) => case.state,
                None => return Ok(()),
            },
        };
        for item in &mut receipt.items {
            item.retention = state
                .current(item.artifact_id)
                .and_then(investigation_core::external_ref::artifact_retention);
        }
        Ok(())
    }

    fn open_import(
        &self,
        path: &Path,
        index: usize,
        limit: u64,
    ) -> Result<crate::import_paths::OpenedImport, Failure> {
        let mut failure = None;
        for configured in &self.workspace.config().import_roots {
            let root = if configured.is_absolute() {
                configured.clone()
            } else {
                self.workspace.root().join(configured)
            };
            let relative = if path.is_absolute() {
                let Ok(relative) = path.strip_prefix(&root) else {
                    continue;
                };
                relative
            } else {
                path
            };
            match ImportRoot::open(&root).and_then(|dir| dir.open_file(relative, limit)) {
                Ok(opened) => return Ok(opened),
                Err(error) => failure = Some(error.to_string()),
            }
        }
        let root = self.workspace.root().to_string_lossy().into_owned();
        let Some(failure) = failure else {
            let configured = &self.workspace.config().import_roots;
            // An absolute path names its directory; a relative one does not.
            let directory = path
                .is_absolute()
                .then(|| path.parent())
                .flatten()
                .and_then(Path::to_str)
                .unwrap_or("DIR");
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                format!(
                    "Path {} is outside configured import roots{}. Add the file's directory with {PROGRAM_NAME} init {root} --import-root {directory}.",
                    path.display(),
                    if configured.is_empty() {
                        " (none are configured)"
                    } else {
                        ""
                    },
                ),
            )
            .at(format!("/paths/{index}"))
            .with_schema("ingest")
            .with_argv(
                [PROGRAM_NAME, "init", &root, "--import-root", directory, "--format", "json"]
                    .map(str::to_owned)
                    .to_vec(),
            ));
        };
        Err(Failure::new(ErrorCode::InvalidInput, failure)
            .at(format!("/paths/{index}"))
            .with_schema("ingest"))
    }

    /// A label for recorded identities: relative to the matching import root,
    /// else the basename. A relative input path is already local-neutral.
    fn display_label(&self, path: &Path) -> String {
        if !path.is_absolute() {
            return path.to_string_lossy().into_owned();
        }
        for configured in &self.workspace.config().import_roots {
            let root = if configured.is_absolute() {
                configured.clone()
            } else {
                self.workspace.root().join(configured)
            };
            if let Ok(relative) = path.strip_prefix(&root)
                && !relative.as_os_str().is_empty()
            {
                return relative.to_string_lossy().into_owned();
            }
        }
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into())
    }

    fn prepare_item(
        &self,
        staged: &StagedBlob,
        label: String,
        display: String,
        channel: AcquisitionChannel,
        extract: bool,
    ) -> Result<PreparedItem, Failure> {
        let text = if extract {
            // Streamed: memory stays one buffer whatever the artifact's size.
            let reader = self
                .fs
                .read(&staged.temp)
                .map_err(io_failure)?
                .take(staged.size);
            if crate::utf8::is_valid(reader).map_err(io_failure)? {
                TextStatus::Decoded
            } else {
                TextStatus::InvalidUtf8
            }
        } else {
            TextStatus::NotRequested
        };
        let mut diagnostics = Vec::new();
        if staged.size <= crate::opaque::MAX_PROBE_BYTES {
            let mut data = Vec::new();
            self.fs
                .read(&staged.temp)
                .map_err(io_failure)?
                .take(crate::opaque::MAX_PROBE_BYTES)
                .read_to_end(&mut data)
                .map_err(io_failure)?;
            if let crate::opaque::EnvelopeProbe::UnsupportedVersion(version) =
                crate::opaque::probe_envelope(&data)
            {
                diagnostics.push(crate::opaque::unsupported_version_diagnostic(&version));
            }
        }
        Ok(PreparedItem {
            label,
            display,
            diagnostics,
            reference: None,
            digest: digest_for(staged)?,
            size: staged.size,
            channel,
            text,
        })
    }
}

/// Digest of what the manifest says about its acquisition, apart from who
/// submitted it, under which command, into which case. Equal descriptions of a
/// known acquisition are a reimport; a different one is a revision.
fn description_digest(manifest: &Manifest) -> Result<Digest, Failure> {
    let description = serde_json::json!({
        "source": manifest.source,
        "classification": manifest.classification,
        "extract_text": manifest.extract_text,
    });
    let bytes = serde_json::to_vec(&acquisition::canonical(description))
        .map_err(|_| integrity("Could not encode the acquisition description."))?;
    Ok(crate::journal::hash_bytes(&bytes))
}

/// Find where the manifest's acquisition is already recorded and plan each item.
/// Runs under the commit lock against verified canonical state.
fn resolve_acquisition(
    manifest: &Manifest,
    description: &Digest,
    facts: &[ItemFacts<'_>],
    visible: Visible<'_>,
) -> Result<Vec<ItemPlan>, Failure> {
    let Some(request) = &manifest.source.acquisition_key else {
        return Ok(acquisition::unkeyed(facts.len()));
    };
    let Some((home, home_case, source)) = home_of(manifest.case_id, request, &visible)? else {
        return Ok(acquisition::fresh(request, facts));
    };
    let plans = acquisition::resolve(
        home,
        home_case,
        home_case.state.sequence(),
        request,
        description,
        manifest.classification,
        facts,
    )?;
    if manifest.case_id.is_none() && existing_transaction(&plans).is_none() {
        return Err(acquisition::other_case(home, source));
    }
    Ok(plans)
}

/// The one case that records this acquisition attempt, with the Source through
/// which it does. Refuses a target case other than that one.
fn home_of<'v>(
    case_id: Option<Id>,
    request: &AcquisitionKeyRequest,
    visible: &Visible<'v>,
) -> Result<Option<(Id, &'v CaseReplay, Id)>, Failure> {
    let target = case_id.zip(visible.target);
    let homes: Vec<(Id, &CaseReplay, Id)> = visible
        .others
        .cases
        .iter()
        .map(|(case_id, case)| (*case_id, case))
        .chain(target)
        .filter_map(|(case_id, case)| {
            acquisition::records_attempt(&case.state, request).map(|source| (case_id, case, source))
        })
        .collect();
    let home = match homes.as_slice() {
        [] => return Ok(None),
        [only] => *only,
        _ => {
            return Err(integrity(
                "One acquisition is recorded in more than one case.",
            ));
        }
    };
    if case_id.is_some_and(|target| target != home.0) {
        return Err(acquisition::other_case(home.0, home.2));
    }
    Ok(Some(home))
}

/// When nothing is new, the transaction whose receipt answers the request.
fn existing_transaction(plans: &[ItemPlan]) -> Option<(Id, u64)> {
    if plans.is_empty()
        || plans
            .iter()
            .any(|plan| plan.outcome != ItemOutcome::Deduplicated)
    {
        return None;
    }
    let known = plans.iter().filter_map(|plan| plan.known.as_ref());
    let case_id = known.clone().next()?.case_id;
    let sequence = known.map(|known| known.prior_sequence).max()?;
    Some((case_id, sequence))
}

#[allow(clippy::too_many_arguments)]
fn create_records(
    manifest: &Manifest,
    items: &[PreparedItem],
    plans: &[ItemPlan],
    derivation: Option<&ResolvedDerivation>,
    description: &Digest,
    manifest_digest: &Digest,
    manifest_size: u64,
    acceptance: Acceptance<'_>,
) -> Result<AcceptedTransaction, Failure> {
    let case_id = match manifest.case_id {
        Some(case_id) => case_id,
        None => new_id(acceptance.ids, Kind::Case)?,
    };
    let mut builder = IngestRecords {
        actor: &manifest.actor,
        descriptor: manifest.actor_descriptor.as_ref(),
        classification: manifest.classification,
        case_id,
        timestamp: acceptance.clock.now_utc(),
        sequence: acceptance.sequence,
        ids: acceptance.ids,
        records: Vec::new(),
        created: Vec::new(),
        updated: Vec::new(),
    };
    if let Some(create) = &manifest.new_case {
        builder.add(
            case_id,
            "case".into(),
            create.clone().into_body(),
            Vec::new(),
        )?;
    }
    let acquired_at = Some(SourceTime {
        timestamp: Knowledge::Known(builder.timestamp.to_string()),
        offset: Knowledge::Known("+00:00".into()),
        clock_quality: ClockQuality::Unverified,
    });
    let mut first_source = None;
    for (index, (item, plan)) in items.iter().zip(plans).enumerate() {
        if let Some(derivation) = derivation {
            // The output of a transformation: a derivative of the exact inputs,
            // tracing to their acquisitions. No Source is created for it.
            let body = ArtifactBody {
                digest: Some(item.digest.clone()),
                size_bytes: Some(item.size),
                format: manifest.source.format.clone(),
                source: derivation.source,
                transforms: derivation.transforms.clone(),
                availability: ArtifactAvailability::Retained,
                integrity: ArtifactIntegrity::Verified,
                sensitivity: sensitivity(manifest.classification),
                role: Some(ArtifactRole::Derivative),
                derivation: Some(derivation.record.clone()),
                external: None,
                status: None,
                withdrawals: Vec::new(),
            };
            body.validate_shape()
                .and_then(|()| body.validate_references())
                .map_err(|blocker| integrity(blocker.code()))?;
            let artifact_id = new_id(builder.ids, Kind::Artifact)?;
            builder.add(
                artifact_id,
                format!("artifact:{index}"),
                body.clone(),
                derivation.provenance.clone(),
            )?;
            first_source.get_or_insert(derivation.source);
            if matches!(item.text, TextStatus::Decoded) {
                add_text_derivative(&mut builder, index, first_revision(artifact_id), &body)?;
            }
            continue;
        }
        let keyed = plan.key.is_some();
        let mut source = SourceBody {
            producer: manifest.source.producer.clone(),
            publisher: manifest.source.publisher.clone(),
            identity: manifest
                .source
                .identity
                .clone()
                .or_else(|| Some(format!("{} / {}", manifest.source.title, item.display))),
            acquisition: SourceAcquisition {
                channel: item.channel,
                acquired_at: acquired_at.clone(),
                method: Some(
                    match item.reference.as_ref().map(|reference| reference.basis) {
                        None => "local byte snapshot",
                        Some(investigation_core::external_ref::DigestBasis::Declared) => {
                            "external reference; bytes not retained"
                        }
                        Some(investigation_core::external_ref::DigestBasis::ReadAtIngest) => {
                            "local file hashed in reference mode; bytes not retained"
                        }
                    }
                    .into(),
                ),
                context: manifest.source.context.clone(),
                environment: None,
            },
            access: if manifest.classification == ClassificationLevel::Restricted {
                SourceAccess::Restricted
            } else {
                SourceAccess::LocalOnly
            },
            verification_limits: manifest
                .source
                .verification_limits
                .iter()
                .chain(&item.diagnostics)
                .cloned()
                .chain(item.reference.is_some().then(|| {
                    "Reference only: the bytes are not retained and cannot satisfy an offline-completeness guarantee.".to_owned()
                }))
                .collect(),
            metadata: manifest.source.metadata.clone(),
            acquisition_key: plan.key.clone(),
            description_digest: keyed.then(|| description.clone()),
            status: None,
            withdrawals: Vec::new(),
            source_assessment: None,
        };
        let source_ref = match &plan.known {
            // A known acquisition keeps its Source ID; a changed description
            // becomes the next revision of that Source, not a new acquisition.
            Some(known) if known.revise => {
                source.acquisition.acquired_at = known.body.acquisition.acquired_at.clone();
                // A new description never lifts a withdrawal or drops a judgment.
                source.status = known.body.status;
                source.withdrawals = known.body.withdrawals.clone();
                source.source_assessment = known.body.source_assessment.clone();
                builder.revise(&known.source, source)?
            }
            Some(known) => known.source.reference(),
            None => {
                let source_id = new_id(builder.ids, Kind::Source)?;
                builder.add(source_id, format!("source:{index}"), source, Vec::new())?;
                RevisionRef {
                    id: source_id,
                    revision: 1,
                }
            }
        };
        let source_id = source_ref.id;
        first_source.get_or_insert(source_ref);

        let original = ArtifactBody {
            digest: Some(item.digest.clone()),
            size_bytes: Some(item.size),
            format: manifest.source.format.clone(),
            source: source_ref,
            transforms: Vec::new(),
            availability: if item.reference.is_some() {
                ArtifactAvailability::External
            } else {
                ArtifactAvailability::Retained
            },
            integrity: item
                .reference
                .as_ref()
                .map_or(ArtifactIntegrity::Verified, reference::reference_integrity),
            sensitivity: sensitivity(manifest.classification),
            status: None,
            withdrawals: Vec::new(),
            role: Some(ArtifactRole::Original),
            derivation: None,
            external: item.reference.as_ref().map(|reference| {
                reference::external_block(
                    reference,
                    &item.digest,
                    item.size,
                    manifest,
                    &builder.timestamp,
                )
            }),
        };
        let known_artifact = plan.known.as_ref().and_then(|known| known.artifact);
        let artifact_id = match known_artifact {
            Some(artifact_id) => artifact_id,
            None => {
                let artifact_id = new_id(builder.ids, Kind::Artifact)?;
                builder.add(
                    artifact_id,
                    format!("artifact:{index}"),
                    original.clone(),
                    vec![source_id],
                )?;
                artifact_id
            }
        };
        let known_derivative = known_artifact
            .and(plan.known.as_ref())
            .and_then(|known| known.derivative);
        if matches!(item.text, TextStatus::Decoded) && known_derivative.is_none() {
            add_text_derivative(&mut builder, index, first_revision(artifact_id), &original)?;
        }
    }
    let source = first_source.ok_or_else(|| integrity("Ingest has no source acquisition."))?;
    let id = new_id(builder.ids, Kind::Artifact)?;
    builder.add(
        id,
        "manifest".into(),
        ArtifactBody {
            digest: Some(manifest_digest.clone()),
            size_bytes: Some(manifest_size),
            format: Some("application/json".into()),
            source,
            transforms: Vec::new(),
            availability: ArtifactAvailability::Retained,
            integrity: ArtifactIntegrity::Verified,
            sensitivity: sensitivity(manifest.classification),
            status: None,
            withdrawals: Vec::new(),
            role: Some(ArtifactRole::Manifest),
            derivation: None,
            external: None,
        },
        vec![source.id],
    )?;
    Ok(AcceptedTransaction {
        case_id,
        sequence: builder.sequence,
        records: builder.records,
        created: builder.created,
        updated: builder.updated,
        warnings: Vec::new(),
    })
}

/// Strict UTF-8 decoding with unchanged newline and character bytes. The text
/// derivative gets its own record even when its blob deduplicates.
fn add_text_derivative(
    builder: &mut IngestRecords<'_>,
    index: usize,
    input_ref: RevisionRef,
    input: &ArtifactBody,
) -> Result<(), Failure> {
    let text_id = new_id(builder.ids, Kind::Artifact)?;
    let input_id = input_ref.id;
    let derivative = ArtifactBody {
        format: Some("text/plain; charset=utf-8".into()),
        transforms: vec![ArtifactTransform {
            input: input_ref,
            transformation: "decode_utf8".into(),
            version: TEXT_PARSER_VERSION.into(),
            input_digest: input.digest.clone(),
        }],
        role: Some(ArtifactRole::Derivative),
        derivation: None,
        external: None,
        ..input.clone()
    };
    builder.add(
        text_id,
        format!("text:{index}"),
        derivative,
        vec![input.source.id, input_id],
    )
}

pub(crate) struct IngestRecords<'a> {
    pub(crate) actor: &'a Actor,
    pub(crate) descriptor: Option<&'a ActorDescriptor>,
    /// Level of the records added or revised next.
    pub(crate) classification: ClassificationLevel,
    pub(crate) case_id: Id,
    pub(crate) timestamp: UtcTimestamp,
    pub(crate) sequence: u64,
    pub(crate) ids: &'a mut dyn IdSource,
    pub(crate) records: Vec<Record>,
    pub(crate) created: Vec<RecordChange>,
    pub(crate) updated: Vec<RecordChange>,
}

impl IngestRecords<'_> {
    pub(crate) fn add(
        &mut self,
        id: Id,
        client_ref: String,
        body: impl Serialize,
        refs: Vec<Id>,
    ) -> Result<(), Failure> {
        let actor = self.actor.clone();
        let kind = id.kind();
        let level = if kind == Kind::Case {
            ClassificationLevel::Internal
        } else {
            self.classification
        };
        let acquired_at = matches!(kind, Kind::Source | Kind::Artifact).then(|| SourceTime {
            timestamp: Knowledge::Known(self.timestamp.to_string()),
            offset: Knowledge::Known("+00:00".into()),
            clock_quality: ClockQuality::Unverified,
        });
        self.records.push(Record {
            header: RecordHeader {
                id,
                case_id: self.case_id,
                schema_version: SchemaVersion::RecordV0Alpha,
                created_by: actor.clone(),
                actor_descriptor_self_reported: self.descriptor.cloned(),
                created_at: self.timestamp.clone(),
                recorded_at: self.timestamp.clone(),
                recorded_by: actor,
                observed_at: None,
                acquired_at,
                acceptance_sequence: self.sequence,
                revision: Revision(1),
                history: Vec::new(),
                classification: Classification {
                    level,
                    ..Classification::default()
                },
                provenance: investigation_core::header::Provenance { references: refs },
                extensions: BTreeMap::new(),
            },
            body: serde_json::to_value(body)
                .map_err(|_| integrity("Could not encode the retained source record."))?,
        });
        self.created.push(RecordChange {
            id,
            revision: 1,
            client_ref: Some(client_ref),
        });
        Ok(())
    }
}

impl IngestRecords<'_> {
    /// Record the next revision of a known record, keeping its creation identity.
    pub(crate) fn revise(
        &mut self,
        old: &Record,
        body: impl Serialize,
    ) -> Result<RevisionRef, Failure> {
        let mut header = old.header.clone();
        let revision = header
            .revision
            .0
            .checked_add(1)
            .ok_or_else(|| integrity("Record revision is exhausted."))?;
        header.history.push(header.revision);
        header.revision = Revision(revision);
        header.acceptance_sequence = self.sequence;
        header.recorded_at = self.timestamp.clone();
        header.recorded_by = self.actor.clone();
        header.classification.level = self.classification;
        let reference = RevisionRef {
            id: header.id,
            revision,
        };
        self.records.push(Record {
            header,
            body: serde_json::to_value(body)
                .map_err(|_| integrity("Could not encode the revised source record."))?,
        });
        self.updated.push(RecordChange {
            id: reference.id,
            revision,
            client_ref: None,
        });
        Ok(reference)
    }
}

pub(crate) fn sensitivity(
    level: ClassificationLevel,
) -> investigation_core::evidence::ArtifactSensitivity {
    use investigation_core::evidence::ArtifactSensitivity;
    match level {
        ClassificationLevel::Public => ArtifactSensitivity::Public,
        ClassificationLevel::Internal => ArtifactSensitivity::Internal,
        ClassificationLevel::Restricted => ArtifactSensitivity::Restricted,
    }
}

fn ingest_receipt(
    mut receipt: Receipt,
    items: Vec<PreparedItem>,
    plans: Vec<ItemPlan>,
    derived: Option<&investigation_core::state::State>,
) -> IngestReceipt {
    let id = |client_ref: &str| {
        receipt.created.iter().find_map(|change| {
            (change.client_ref.as_deref() == Some(client_ref)).then_some(change.id)
        })
    };
    let expected_count = items.len();
    let complete = plans.len() == expected_count;
    // With nothing new, the receipt is an earlier transaction's and this
    // manifest's bytes were not retained.
    let nothing_new = existing_transaction(&plans).is_some();
    let manifest_artifact_id = id("manifest").filter(|_| !nothing_new);
    let items = items
        .into_iter()
        .zip(plans)
        .enumerate()
        .filter_map(|(index, (item, plan))| {
            let known = plan.known.as_ref();
            let decoded = matches!(item.text, TextStatus::Decoded);
            if let Some(state) = derived {
                let artifact_id = id(&format!("artifact:{index}"))?;
                let lineage = investigation_core::derivation::lineage(
                    state,
                    RevisionRef {
                        id: artifact_id,
                        revision: 1,
                    },
                )?;
                let source =
                    investigation_core::derivation::artifact_body(state, lineage.artifact_ref)?
                        .source;
                return Some(ItemReceipt {
                    source_id: source.id,
                    artifact_id,
                    digest: item.digest,
                    size_bytes: item.size,
                    derivative_id: id(&format!("text:{index}")),
                    text_status: item.text,
                    diagnostics: item.diagnostics,
                    outcome: Some(ItemOutcome::Derived),
                    prior: None,
                    acquisition_key: None,
                    lineage: Some(lineage),
                    retention: None,
                });
            }
            Some(ItemReceipt {
                source_id: known
                    .map(|known| known.source.header.id)
                    .or_else(|| id(&format!("source:{index}")))?,
                artifact_id: known
                    .and_then(|known| known.artifact)
                    .or_else(|| id(&format!("artifact:{index}")))?,
                digest: item.digest,
                size_bytes: item.size,
                derivative_id: known
                    .filter(|_| decoded)
                    .and_then(|known| known.artifact.and(known.derivative))
                    .or_else(|| id(&format!("text:{index}"))),
                text_status: item.text,
                diagnostics: item.diagnostics,
                outcome: Some(plan.outcome),
                prior: plan.prior(),
                acquisition_key: plan.key,
                lineage: None,
                retention: None,
            })
        })
        .collect::<Vec<_>>();
    if !complete
        || items.len() != expected_count
        || (manifest_artifact_id.is_none() && !nothing_new)
    {
        receipt.warnings.push("The canonical receipt is available. Some import item details could not be reconstructed.".into());
    }
    IngestReceipt {
        receipt,
        items,
        manifest_artifact_id,
    }
}

fn first_revision(id: Id) -> RevisionRef {
    RevisionRef { id, revision: 1 }
}

pub(crate) fn new_id(ids: &mut dyn IdSource, kind: Kind) -> Result<Id, Failure> {
    ids.next_id(kind)
        .map_err(|_| integrity("Could not allocate an import record ID."))
}

fn digest_for(staged: &StagedBlob) -> Result<Digest, Failure> {
    format!("sha256:{}", staged.digest)
        .parse()
        .map_err(|_| integrity("Staged blob has an invalid digest."))
}

fn stage_input(
    fs: &mut impl DurableFs,
    gate: &WriteGate,
    input: impl Read,
) -> Result<StagedBlob, Failure> {
    stage_watched(fs, gate, input, &mut |_| {})
}

/// Copy `input` into a private temporary under `state/tmp` while hashing it,
/// through one fixed buffer, reporting progress to `during` and the log. A
/// failed copy, including a full disk, removes the partial temporary.
fn stage_watched(
    fs: &mut impl DurableFs,
    gate: &WriteGate,
    input: impl Read,
    during: &mut dyn FnMut(u64),
) -> Result<StagedBlob, Failure> {
    let id = new_id(&mut SystemIdSource, Kind::Import)?;
    let path = Path::new(TMP).join(format!("import-{id}.tmp"));
    let input = Progress::new(input, during);
    stage(fs, gate, &path, input).map_err(|error| {
        // The partial copy holds no accepted bytes.
        let _ = fs.remove(&path);
        match error {
            crate::blob::BlobError::Io(error) if error.kind() == io::ErrorKind::FileTooLarge => {
                Failure::new(
                    ErrorCode::InvalidInput,
                    format!(
                        "The input exceeds the {} import limit, so nothing was retained. Ingest a larger local file with retention reference.",
                        readable_bytes(MAX_IMPORT_BYTES)
                    ),
                )
                .with_schema("ingest")
            }
            error => blob_failure(error),
        }
    })
}

fn invalid(message: &str, path: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message)
        .at(path)
        .with_schema("ingest")
}

/// A byte count in the largest binary unit that divides it, as `64 GiB`.
fn readable_bytes(bytes: u64) -> String {
    let mut value = bytes;
    let mut unit = "bytes";
    for next in ["KiB", "MiB", "GiB", "TiB"] {
        if value < 1024 || !value.is_multiple_of(1024) {
            break;
        }
        value /= 1024;
        unit = next;
    }
    format!("{value} {unit}")
}

struct Limited<R> {
    input: R,
    remaining: u64,
    limit: u64,
}

impl<R> Limited<R> {
    fn new(input: R) -> Self {
        Self::with_limit(input, MAX_IMPORT_BYTES)
    }

    fn with_limit(input: R, limit: u64) -> Self {
        Self {
            input,
            remaining: limit,
            limit,
        }
    }
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 && !buffer.is_empty() {
            if self.input.read(&mut [0; 1])? == 0 {
                return Ok(0);
            }
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!("Local import exceeds {}.", readable_bytes(self.limit)),
            ));
        }
        let limit = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = self.input.read(&mut buffer[..limit])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

/// Counts the bytes read through it. Reports the running total to `during`
/// after every read and to the log every [`PROGRESS_INTERVAL_BYTES`]; the log
/// carries counts only, never content, names or paths, and reaches stderr only.
pub(crate) struct Progress<'a, R> {
    input: R,
    read: u64,
    logged: u64,
    during: &'a mut dyn FnMut(u64),
}

impl<'a, R> Progress<'a, R> {
    pub(crate) fn new(input: R, during: &'a mut dyn FnMut(u64)) -> Self {
        Self {
            input,
            read: 0,
            logged: 0,
            during,
        }
    }
}

impl<R: Read> Read for Progress<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.input.read(buffer)?;
        if read == 0 {
            // A stream long enough to report progress also reports its end.
            if self.read > self.logged && self.logged > 0 {
                tracing::info!(target: PROGRESS_TARGET, bytes = self.read, "stream complete");
                self.logged = self.read;
            }
            return Ok(0);
        }
        self.read = self.read.saturating_add(read as u64);
        (self.during)(self.read);
        if self.read - self.logged >= PROGRESS_INTERVAL_BYTES {
            tracing::info!(target: PROGRESS_TARGET, bytes = self.read, "streaming");
            self.logged = self.read;
        }
        Ok(read)
    }
}
