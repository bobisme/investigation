//! One-shot ingestion of an `inv.producer-run.v1` envelope.
//!
//! A completed producer run becomes, in one transaction:
//!
//! - one Run whose `producer_run` section indexes the producer's statements;
//! - one Source and Artifact for the envelope bytes themselves, so every
//!   assertion, control and receipt is retained source content that a later
//!   Observation can cite with a JSON selector;
//! - one Source and Artifact per retained declared artifact;
//! - a minimal `record`-mode case when the submission names none.
//!
//! The producer name and external run ID form the acquisition attempt
//! (W06b.03). The envelope is item `envelope`; each declared artifact is item
//! `artifact:` plus its normalized path. Ingesting identical content again
//! records nothing; a changed envelope revises the same Sources and the same Run.
//!
//! Nothing here creates an Observation, an Assessment or a Conclusion, and no
//! producer statement sets measurement validity: the Run's `evidence_validity`
//! is `unknown` unless an investigator records otherwise.

use std::collections::BTreeSet;

use investigation_core::{
    attempt::{self, AttemptIdentity, AttemptPath},
    case::CaseCreate,
    command::{Reference, RevisionRef},
    evidence::{
        AcquisitionChannel, ArtifactAvailability, ArtifactBody, ArtifactIntegrity, ArtifactRole,
        EvidenceKind, MeasurementValidity, SourceAccess, SourceAcquisition, SourceBody,
    },
    explain::{EvidenceCompleteness, ExecutionStatus, RunIntent, RunRecord},
    header::{ActorDescriptor, ClassificationLevel, Knowledge, SourceTime},
    producer_run::{
        Attribution, EnvelopeRef, InventoryEntry, MAX_ENVELOPE_BYTES, PRODUCER_RUN_KEY,
        ProducerArtifact, ProducerRun, ProducerRunRecord, ProducerRunVersion, Retention,
        inventory_completeness, normalized_path,
    },
};

use super::*;
use crate::{
    import_paths::{ImportErrorKind, OpenedImport},
    opaque::{EnvelopeProbe, probe_envelope, unsupported_version_diagnostic},
};

/// Acquisition item name of the envelope within its attempt.
pub const ENVELOPE_ITEM: &str = "envelope";
/// Prefix of a declared artifact's acquisition item name.
pub const ARTIFACT_ITEM_PREFIX: &str = "artifact:";
const SCHEMA: &str = "producer-run";

/// Who submits an envelope, into which case, under which command ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerRunSubmission {
    /// Attributed submitter. The envelope's producer identity stays its own claim.
    pub actor: Actor,
    pub actor_descriptor: Option<ActorDescriptor>,
    /// Existing case. `None` creates a minimal `record`-mode case, unless the
    /// attempt is already recorded and nothing changed.
    pub case_id: Option<Id>,
    /// Journal command ID. `None` derives one from the request content, so an
    /// identical resubmission is idempotent.
    pub command_id: Option<String>,
}

impl ProducerRunSubmission {
    pub fn new(actor: Actor) -> Self {
        Self {
            actor,
            actor_descriptor: None,
            case_id: None,
            command_id: None,
        }
    }
}

/// What ingest did with the envelope, the Run or one declared artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProducerItemOutcome {
    /// New records.
    Imported,
    /// Already recorded with the same content; nothing new.
    Deduplicated,
    /// A known item with changed content: a new revision or retained version.
    Revised,
    /// No file at the declared path beneath any import root.
    Missing,
    /// The bytes differ from `expected_sha256`; nothing was retained.
    Rejected,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProducerItemReceipt {
    /// `envelope`, or `artifact:` plus the normalized path.
    pub item: String,
    /// Declared path, for an artifact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub outcome: ProducerItemOutcome,
    pub source_id: Option<Id>,
    pub artifact_id: Option<Id>,
    /// Digest of the retained bytes, or of the rejected bytes.
    pub digest: Option<Digest>,
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

/// Receipt of a producer-run ingest.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProducerRunReceipt {
    #[serde(flatten)]
    pub receipt: Receipt,
    pub case_id: Id,
    /// The Run revision this receipt describes.
    pub run: RevisionRef,
    pub run_outcome: ProducerItemOutcome,
    pub envelope: ProducerItemReceipt,
    pub artifacts: Vec<ProducerItemReceipt>,
    /// Execution state exactly as the producer reported it.
    pub execution: ExecutionStatus,
    /// Retention completeness of the declared artifacts, computed by ingest.
    pub evidence_completeness: EvidenceCompleteness,
    /// The Run's recorded measurement validity. Ingest records `unknown` and
    /// never changes it: no producer statement, exit code or control result
    /// sets measurement validity.
    pub measurement_validity: MeasurementValidity,
}

pub fn envelope_schema() -> Schema {
    schema_for!(ProducerRun)
}

pub fn receipt_schema() -> Schema {
    schema_for!(ProducerRunReceipt)
}

fn invalid(message: &str, path: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message)
        .at(path)
        .with_schema(SCHEMA)
}

/// Parse and check an envelope. An unsupported version of the family is an
/// explicit failure here; ingesting the file as bytes retains it opaquely.
pub fn parse_envelope(bytes: &[u8]) -> Result<(ProducerRun, Value), Failure> {
    if bytes.len() > MAX_ENVELOPE_BYTES {
        return Err(invalid(
            "The envelope exceeds 512 KiB. Retain large producer output as declared artifacts.",
            "",
        ));
    }
    if let EnvelopeProbe::UnsupportedVersion(version) = probe_envelope(bytes) {
        return Err(invalid(
            &format!(
                "{} To keep the bytes anyway, ingest the file with an inv.ingest.v1 manifest.",
                unsupported_version_diagnostic(&version)
            ),
            "/schema_version",
        ));
    }
    let raw = input::parse_request(bytes).map_err(|error| invalid(error.message, &error.path))?;
    let run: ProducerRun =
        input::decode(&raw).map_err(|error| invalid(error.message, &error.path))?;
    run.validate()
        .map_err(|error| invalid(&error.message, &error.path))?;
    for (pointer, metadata) in run.metadata_maps() {
        crate::opaque::validate_metadata(metadata)
            .map_err(|(message, key)| invalid(&message, &format!("{pointer}{key}")))?;
    }
    Ok((run, raw))
}

/// Outcome of reading one declared artifact before the commit lock.
enum Staged {
    Retained(PreparedItem),
    Missing,
    Rejected(PreparedItem),
}

impl Store {
    /// Ingest one completed producer run described by an `inv.producer-run.v1`
    /// envelope. Artifact bytes are snapshotted before the commit lock.
    pub fn ingest_producer_run(
        &mut self,
        bytes: &[u8],
        submission: &ProducerRunSubmission,
    ) -> Result<ProducerRunReceipt, Failure> {
        let (run, raw) = parse_envelope(bytes)?;
        if let Some(command_id) = &submission.command_id
            && (command_id.is_empty()
                || command_id.chars().count() > 128
                || command_id.chars().any(char::is_control))
        {
            return Err(invalid(
                "Provide a command ID with 1–128 characters.",
                "/command_id",
            ));
        }
        if submission
            .case_id
            .is_some_and(|case| case.kind() != Kind::Case)
        {
            return Err(invalid("Choose an existing case ID.", "/case_id"));
        }
        let mut stages = Vec::new();
        let result = self.producer_run_inner(bytes, &run, raw, submission, &mut stages);
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

    fn producer_run_inner(
        &mut self,
        bytes: &[u8],
        run: &ProducerRun,
        raw: Value,
        submission: &ProducerRunSubmission,
        stages: &mut Vec<StagedBlob>,
    ) -> Result<ProducerRunReceipt, Failure> {
        let mut staged = Vec::new();
        let mut retained_blobs = Vec::new();
        for (index, artifact) in run.artifacts.iter().enumerate() {
            let relative = PathBuf::from(normalized_path(&artifact.path).ok_or_else(|| {
                invalid("Use a relative path beneath an import root.", "/artifacts")
            })?);
            let Some(mut opened) = self.open_declared(&relative, index)? else {
                staged.push(Staged::Missing);
                continue;
            };
            let blob = stage_input(&mut self.fs, &self.gate, Limited::new(&mut opened.file))?;
            stages.push(blob.clone());
            if !opened
                .witness
                .verify_unchanged(&opened.file)
                .map_err(io_failure)?
            {
                return Err(Failure::new(
                    ErrorCode::IoFailure,
                    "An artifact changed while it was read. Retry once the producer has closed it.",
                )
                .at(format!("/artifacts/{index}/path")));
            }
            let label = artifact_item(artifact);
            let item = self.prepare_item(
                &blob,
                label.clone(),
                label,
                AcquisitionChannel::LocalFile,
                false,
            )?;
            let matches = artifact.expected_sha256.as_ref().is_none_or(|expected| {
                item.digest.as_str().strip_prefix("sha256:") == Some(expected)
            });
            if matches {
                retained_blobs.push(blob);
                staged.push(Staged::Retained(item));
            } else {
                staged.push(Staged::Rejected(item));
            }
        }
        let envelope_blob = stage_input(&mut self.fs, &self.gate, bytes)?;
        let envelope_digest = digest_for(&envelope_blob)?;
        let envelope_size = envelope_blob.size;
        stages.push(envelope_blob.clone());
        retained_blobs.push(envelope_blob);

        let retention: Vec<Value> = run
            .artifacts
            .iter()
            .zip(&staged)
            .map(|(artifact, staged)| {
                let (state, digest) = match staged {
                    Staged::Retained(item) => ("retained", Some(&item.digest)),
                    Staged::Rejected(item) => ("rejected", Some(&item.digest)),
                    Staged::Missing => ("missing", None),
                };
                json!({"item": artifact_item(artifact), "state": state, "digest": digest})
            })
            .collect();
        let new_case = submission.case_id.is_none().then(|| {
            serde_json::from_value::<CaseCreate>(json!({
                "title": run.intent.label,
                "mode": "record",
            }))
            .expect("a record-mode case from a bounded label decodes")
        });
        let mut request_input = json!({
            "schema_version": IngestVersion::V1,
            "actor": submission.actor,
            "producer_run": raw,
            "retention": retention,
        });
        match (&submission.case_id, &new_case) {
            (Some(case_id), _) => request_input["case_id"] = json!(case_id),
            (None, Some(create)) => request_input["new_case"] = json!(create),
            (None, None) => unreachable!("a missing case yields a new case"),
        }
        let attachments: Vec<Digest> = staged
            .iter()
            .filter_map(|item| match item {
                Staged::Retained(item) => Some(item.digest.clone()),
                _ => None,
            })
            .collect();
        let command_id = match &submission.command_id {
            Some(command_id) => command_id.clone(),
            None => derived_command_id(&request_input, &attachments, &envelope_digest)?,
        };
        request_input["command_id"] = json!(command_id);

        let request = acquisition::AcquisitionKeyRequest {
            namespace: run.producer.name.clone(),
            external_id: run.external_run_id.clone(),
        };
        request.validate().map_err(|_| {
            invalid(
                "Use a producer name and external run ID that form a valid acquisition key.",
                "/producer/name",
            )
        })?;
        let event_actor = submission.actor.clone();
        let mut decided: Option<(Id, bool)> = None;
        let mut warnings = Vec::new();
        for (artifact, staged) in run.artifacts.iter().zip(&staged) {
            match staged {
                Staged::Missing => warnings.push(format!(
                    "Declared artifact {} was not found beneath any import root; the Run records it as missing.",
                    artifact.path
                )),
                Staged::Rejected(item) => warnings.push(format!(
                    "Declared artifact {} has digest {} instead of the expected sha256:{}; its bytes were not retained.",
                    artifact.path,
                    item.digest,
                    artifact.expected_sha256.as_deref().unwrap_or_default()
                )),
                Staged::Retained(_) => {}
            }
        }
        let context = RunContext {
            run,
            submission,
            request: &request,
            staged: &staged,
            new_case: new_case.as_ref(),
            envelope_digest: &envelope_digest,
            envelope_size,
            warnings: &warnings,
        };
        let receipt = self.commit_plan(
            CommitPlan {
                case_id: submission.case_id,
                event: EventInput {
                    actor: &event_actor,
                    command_id: &command_id,
                    preconditions: &[],
                    manifest_digest: Some(envelope_digest.clone()),
                },
                request: SemanticRequest {
                    input: request_input,
                    attachments,
                },
                blobs: &retained_blobs,
            },
            &mut SystemClock,
            &mut SystemIdSource,
            |_, _, acceptance, visible| {
                let prepared = context.prepare(acceptance, &visible)?;
                decided = Some(match &prepared {
                    Prepared::Accept(accepted) => (accepted.case_id, false),
                    Prepared::Existing { case_id, .. } => (*case_id, true),
                });
                Ok(prepared)
            },
        )?;
        let (case_id, deduplicated) = match decided {
            Some(decided) => decided,
            None => (
                submission
                    .case_id
                    .or_else(|| {
                        receipt
                            .created
                            .iter()
                            .find(|change| change.id.kind() == Kind::Case)
                            .map(|change| change.id)
                    })
                    .ok_or_else(|| integrity("The committed producer run has no case."))?,
                false,
            ),
        };
        let replay = self.case(case_id)?;
        let mut receipt = producer_receipt(
            receipt,
            case_id,
            &replay.state,
            run,
            &staged,
            &request,
            deduplicated,
            (&envelope_digest, envelope_size),
        )?;
        if deduplicated && receipt.receipt.command_id != command_id {
            receipt.receipt.warnings.push(format!(
                "This producer run is already recorded with identical content, so command {command_id} committed nothing. The receipt describes the transaction that recorded the current revision."
            ));
        }
        Ok(receipt)
    }

    /// Open a declared artifact beneath the first import root that holds it.
    /// `None` when no root has a file at that path; any other refusal (a
    /// symlink, a hardlinked or oversized file) is an error.
    fn open_declared(
        &self,
        relative: &Path,
        index: usize,
    ) -> Result<Option<OpenedImport>, Failure> {
        let at = format!("/artifacts/{index}/path");
        let mut unusable_root = None;
        let mut refusal = None;
        let mut absent = false;
        for configured in &self.workspace.config().import_roots {
            let root = if configured.is_absolute() {
                configured.clone()
            } else {
                self.workspace.root().join(configured)
            };
            let directory = match ImportRoot::open(&root) {
                Ok(directory) => directory,
                Err(error) => {
                    unusable_root.get_or_insert(error.to_string());
                    continue;
                }
            };
            match directory.open_file(relative, MAX_IMPORT_BYTES) {
                Ok(opened) => return Ok(Some(opened)),
                Err(error) => match &error.reason {
                    ImportErrorKind::Io(io) if io.kind() == io::ErrorKind::NotFound => {
                        absent = true;
                    }
                    _ => {
                        refusal.get_or_insert(error.to_string());
                    }
                },
            }
        }
        // A root that refuses the file outright wins over a root that lacks it.
        let message = match (refusal, absent) {
            (Some(message), _) => message,
            (None, true) => return Ok(None),
            (None, false) => unusable_root.unwrap_or_else(|| {
                "Configure an import root that holds the producer's artifacts.".into()
            }),
        };
        Err(Failure::new(ErrorCode::InvalidInput, message)
            .at(at)
            .with_schema(SCHEMA))
    }
}

fn artifact_item(artifact: &ProducerArtifact) -> String {
    format!(
        "{ARTIFACT_ITEM_PREFIX}{}",
        normalized_path(&artifact.path).unwrap_or_else(|| artifact.path.clone())
    )
}

/// Command ID derived from everything the request says, so the same content
/// under the same actor is one command and changed content is another.
fn derived_command_id(
    input: &Value,
    attachments: &[Digest],
    envelope: &Digest,
) -> Result<String, Failure> {
    let bytes = serde_json::to_vec(&acquisition::canonical(json!({
        "input": input,
        "attachments": attachments,
        "envelope": envelope,
    })))
    .map_err(|_| integrity("Could not encode the producer-run request."))?;
    let digest = crate::journal::hash_bytes(&bytes);
    let hex = digest.as_str().trim_start_matches("sha256:");
    Ok(format!("producer-run/{}", &hex[..48]))
}

fn description(value: Value) -> Result<Digest, Failure> {
    let bytes = serde_json::to_vec(&acquisition::canonical(value))
        .map_err(|_| integrity("Could not encode the acquisition description."))?;
    Ok(crate::journal::hash_bytes(&bytes))
}

fn producer_label(run: &ProducerRun) -> String {
    let mut label = run.producer.name.clone();
    if let Some(version) = &run.producer.version {
        label.push(' ');
        label.push_str(version);
    }
    if let Some(instance) = &run.producer.instance {
        label.push_str(&format!(" ({instance})"));
    }
    label
}

fn environment_text(run: &ProducerRun) -> Option<String> {
    (!run.environment.is_empty())
        .then(|| serde_json::to_string(&run.environment).expect("a string map serializes"))
}

/// A reported time as a source claim; the offset is read from the text only
/// when it ends in `Z` or `±HH:MM`.
fn reported_time(text: Option<&String>, run: &ProducerRun) -> Option<SourceTime> {
    let text = text?;
    let offset = if text.ends_with('Z') || text.ends_with('z') {
        Knowledge::Known("+00:00".to_owned())
    } else {
        let tail: Vec<char> = text.chars().rev().take(6).collect();
        let tail: String = tail.into_iter().rev().collect();
        let bytes = tail.as_bytes();
        if bytes.len() == 6
            && matches!(bytes[0], b'+' | b'-')
            && bytes[1..3].iter().all(u8::is_ascii_digit)
            && bytes[3] == b':'
            && bytes[4..6].iter().all(u8::is_ascii_digit)
        {
            Knowledge::Known(tail)
        } else {
            Knowledge::Unknown
        }
    };
    Some(SourceTime {
        timestamp: Knowledge::Known(text.clone()),
        offset,
        clock_quality: run.execution.clock_quality,
    })
}

fn access(level: ClassificationLevel) -> SourceAccess {
    if level == ClassificationLevel::Restricted {
        SourceAccess::Restricted
    } else {
        SourceAccess::LocalOnly
    }
}

/// The Run that records this attempt in a case, if any.
pub(super) fn recorded_run<'s>(
    state: &'s State,
    request: &acquisition::AcquisitionKeyRequest,
) -> Option<&'s Record> {
    state.records().find(|record| {
        record.header.id.kind() == Kind::Run
            && record.body[PRODUCER_RUN_KEY]["producer"]["name"] == request.namespace.as_str()
            && record.body[PRODUCER_RUN_KEY]["external_run_id"] == request.external_id.as_str()
    })
}

fn decode_record(body: &Value) -> Option<ProducerRunRecord> {
    input::decode(body.get(PRODUCER_RUN_KEY)?).ok()
}

/// Run metadata keys that ingest sets on a new Run and otherwise preserves.
const RUN_META: &[&str] = &[
    "declared_at_sequence",
    "chronology",
    "plan_digest",
    "evidence_validity",
    "prior_exposure",
    "external_ref",
    "retrospective_plan_ref",
    "reconciliation_reason",
    "ordering_receipt",
];

struct RunContext<'a> {
    run: &'a ProducerRun,
    submission: &'a ProducerRunSubmission,
    request: &'a acquisition::AcquisitionKeyRequest,
    staged: &'a [Staged],
    new_case: Option<&'a CaseCreate>,
    envelope_digest: &'a Digest,
    envelope_size: u64,
    warnings: &'a [String],
}

impl RunContext<'_> {
    /// Decide under the commit lock against verified canonical state.
    fn prepare(
        &self,
        acceptance: Acceptance<'_>,
        visible: &Visible<'_>,
    ) -> Result<Prepared, Failure> {
        let run = self.run;
        let level = run.level();
        refuse_captured_attempt(run, visible)?;
        let home = home_of(self.submission.case_id, self.request, visible).map_err(relocate)?;
        let plan = |label: &str, digest: &Digest, described: &Digest, level| {
            let facts = [acquisition::ItemFacts {
                label,
                digest,
                decoded: false,
                retained: true,
            }];
            let mut plans = match home {
                None => acquisition::fresh(self.request, &facts),
                Some((case_id, case, _)) => acquisition::resolve(
                    case_id,
                    case,
                    case.state.sequence(),
                    self.request,
                    described,
                    level,
                    &facts,
                )?,
            };
            plans
                .pop()
                .ok_or_else(|| integrity("Acquisition planning returned no item."))
        };
        let previous = home.and_then(|(_, case, _)| recorded_run(&case.state, self.request));
        let previous_record = previous.and_then(|record| decode_record(&record.body));

        let case_id = match (self.submission.case_id, home) {
            (Some(case_id), _) => case_id,
            (None, Some((home, _, _))) => home,
            (None, None) => new_id(acceptance.ids, Kind::Case)?,
        };
        let mut builder = IngestRecords {
            actor: &self.submission.actor,
            descriptor: self.submission.actor_descriptor.as_ref(),
            classification: level,
            case_id,
            timestamp: acceptance.clock.now_utc(),
            sequence: acceptance.sequence,
            ids: acceptance.ids,
            records: Vec::new(),
            created: Vec::new(),
            updated: Vec::new(),
        };
        if let (Some(create), None) = (self.new_case, home) {
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
            clock_quality: investigation_core::header::ClockQuality::Unverified,
        });
        let producer = Some(producer_label(run));
        let mut known_sequence = previous.map_or(0, |record| record.header.acceptance_sequence);
        let mut all_known = previous.is_some();

        // The envelope itself: the retained source content of every statement.
        let envelope_described = description(
            serde_json::to_value(run)
                .map_err(|_| integrity("Could not encode the producer-run description."))?,
        )?;
        let envelope_plan = plan(
            ENVELOPE_ITEM,
            self.envelope_digest,
            &envelope_described,
            level,
        )?;
        let mut limits = vec![
            "Every producer statement in this envelope is an attributed claim; ingest verified none of them.".to_owned(),
        ];
        limits.extend(run.limitations.iter().cloned());
        let envelope_source = SourceBody {
            producer: producer.clone(),
            publisher: None,
            identity: Some(format!(
                "{} run {} envelope",
                run.producer.name, run.external_run_id
            )),
            acquisition: SourceAcquisition {
                channel: AcquisitionChannel::Producer,
                acquired_at: acquired_at.clone(),
                method: Some("producer-run envelope snapshot".into()),
                context: Some(run.intent.label.clone()),
                environment: environment_text(run),
            },
            access: access(level),
            verification_limits: limits,
            metadata: run.metadata.clone(),
            acquisition_key: envelope_plan.key.clone(),
            description_digest: Some(envelope_described),
            status: None,
            withdrawals: Vec::new(),
            source_assessment: None,
        };
        let (envelope_source_ref, envelope_artifact_ref) = builder.retain(
            &envelope_plan,
            envelope_source,
            |source| ArtifactBody {
                digest: Some(self.envelope_digest.clone()),
                size_bytes: Some(self.envelope_size),
                format: Some("application/json".into()),
                source,
                transforms: Vec::new(),
                availability: ArtifactAvailability::Retained,
                integrity: ArtifactIntegrity::Verified,
                sensitivity: sensitivity(level),
                status: None,
                withdrawals: Vec::new(),
                // The envelope is acquired source content, not an import request.
                role: Some(ArtifactRole::Original),
                derivation: None,
                external: None,
            },
            "envelope",
            level,
        )?;
        all_known &= envelope_plan.outcome == acquisition::ItemOutcome::Deduplicated;
        if let Some(known) = &envelope_plan.known {
            known_sequence = known_sequence.max(known.prior_sequence);
        }

        // Each declared artifact, in declaration order.
        let mut inventory = Vec::new();
        for (index, (artifact, staged)) in run.artifacts.iter().zip(self.staged).enumerate() {
            let label = artifact_item(artifact);
            let retention = match staged {
                Staged::Retained(item) => {
                    let described = description(json!({
                        "producer": run.producer,
                        "external_run_id": run.external_run_id,
                        "artifact": artifact,
                    }))?;
                    let item_plan =
                        plan(&label, &item.digest, &described, artifact.classification)?;
                    let source = SourceBody {
                        producer: producer.clone(),
                        publisher: None,
                        identity: Some(format!(
                            "{} run {} {}",
                            run.producer.name, run.external_run_id, artifact.path
                        )),
                        acquisition: SourceAcquisition {
                            channel: AcquisitionChannel::LocalFile,
                            acquired_at: acquired_at.clone(),
                            method: Some("local byte snapshot".into()),
                            context: Some(format!(
                                "{} declared by the producer run",
                                artifact.role
                            )),
                            environment: None,
                        },
                        access: access(artifact.classification),
                        verification_limits: item.diagnostics.clone(),
                        metadata: artifact.metadata.clone(),
                        acquisition_key: item_plan.key.clone(),
                        description_digest: Some(described),
                        status: None,
                        withdrawals: Vec::new(),
                        source_assessment: None,
                    };
                    let (source_ref, artifact_ref) = builder.retain(
                        &item_plan,
                        source,
                        |source| ArtifactBody {
                            digest: Some(item.digest.clone()),
                            size_bytes: Some(item.size),
                            format: artifact.media_type.clone(),
                            source,
                            transforms: Vec::new(),
                            availability: ArtifactAvailability::Retained,
                            integrity: ArtifactIntegrity::Verified,
                            sensitivity: sensitivity(artifact.classification),
                            status: None,
                            withdrawals: Vec::new(),
                            role: Some(ArtifactRole::Original),
                            derivation: None,
                            external: None,
                        },
                        &format!("artifact:{index}"),
                        artifact.classification,
                    )?;
                    all_known &= item_plan.outcome == acquisition::ItemOutcome::Deduplicated;
                    if let Some(known) = &item_plan.known {
                        known_sequence = known_sequence.max(known.prior_sequence);
                    }
                    Retention::Retained {
                        source_ref,
                        artifact_ref,
                        digest: item.digest.clone(),
                        size_bytes: item.size,
                    }
                }
                Staged::Rejected(item) => Retention::Rejected {
                    actual: item.digest.clone(),
                },
                Staged::Missing => Retention::Missing,
            };
            inventory.push(InventoryEntry {
                declared: artifact.clone(),
                retention: carried_forward(retention, artifact, previous_record.as_ref()),
            });
        }

        let mut artifact_refs = vec![Reference::Existing(envelope_artifact_ref)];
        for entry in &inventory {
            if let Retention::Retained { artifact_ref, .. } = &entry.retention {
                artifact_refs.push(Reference::Existing(*artifact_ref));
            }
        }
        if let Some(previous) = previous {
            let old: Vec<Reference> =
                input::decode(&previous.body["artifact_refs"]).unwrap_or_default();
            for reference in old {
                if !artifact_refs.contains(&reference) {
                    artifact_refs.push(reference);
                }
            }
        }
        let completeness = inventory_completeness(&inventory);
        let outcome = RunRecord {
            intent: RunIntent::Exploration {
                intent: run.intent.label.clone(),
            },
            // The first submitter stays the Run's producer, as run.reconcile requires.
            producer: previous
                .and_then(|record| input::decode(&record.body["producer"]).ok())
                .unwrap_or_else(|| self.submission.actor.clone()),
            tool: producer.clone(),
            environment: environment_text(run),
            started_at: reported_time(run.execution.started_at.as_ref(), run),
            finished_at: reported_time(run.execution.ended_at.as_ref(), run),
            result_assertion: None,
            artifact_refs,
            manifest_digest: Some(self.envelope_digest.clone()),
            execution_status: run.execution.state,
            evidence_completeness: completeness,
            evidence_kind: EvidenceKind::Observational,
            variables_changed: Vec::new(),
            search_coverage: None,
            // Investigator-recorded measurement fields survive a revision.
            measurement: previous
                .and_then(|record| input::decode(&record.body["measurement"]).ok())
                .flatten(),
            repetition: previous
                .and_then(|record| input::decode(&record.body["repetition"]).ok())
                .flatten(),
        };
        let statements = ProducerRunRecord {
            schema_version: ProducerRunVersion::V1,
            attribution: Attribution::ProducerReported,
            producer: run.producer.clone(),
            external_run_id: run.external_run_id.clone(),
            intent: run.intent.clone(),
            execution: run.execution.clone(),
            environment: run.environment.clone(),
            envelope: EnvelopeRef {
                source_ref: envelope_source_ref,
                artifact_ref: envelope_artifact_ref,
                digest: self.envelope_digest.clone(),
            },
            artifacts: inventory,
            assertions: run.assertions.clone(),
            controls: run.controls.clone(),
            receipts: run.receipts.clone(),
            limitations: run.limitations.clone(),
            metadata: run.metadata.clone(),
        };
        let mut body = serde_json::to_value(outcome)
            .map_err(|_| integrity("Could not encode the Run record."))?;
        match previous {
            // Investigator-owned and declaration facts survive a revision.
            Some(previous) => {
                for key in RUN_META {
                    if let Some(value) = previous.body.get(*key) {
                        body[*key] = value.clone();
                    }
                }
            }
            None => {
                body["declared_at_sequence"] = json!(acceptance.sequence);
                body["chronology"] = json!("order_unknown");
                body["external_ref"] = json!(run.external_run_id);
                // No producer statement sets measurement validity.
                body["evidence_validity"] = json!(MeasurementValidity::Unknown);
                body["ordering_receipt"] = Value::Null;
            }
        }
        body[PRODUCER_RUN_KEY] = serde_json::to_value(statements)
            .map_err(|_| integrity("Could not encode the producer statements."))?;
        let mut refs = vec![envelope_source_ref.id, envelope_artifact_ref.id];
        if let Some(previous) = previous {
            // Same producer statements and inventory: nothing new from the
            // producer, whatever an investigator reconciled on the Run since.
            if previous.body[PRODUCER_RUN_KEY] == body[PRODUCER_RUN_KEY] {
                if all_known {
                    return Ok(Prepared::Existing {
                        case_id,
                        sequence: known_sequence,
                    });
                }
            } else {
                keep_terminal_status(previous, run.execution.state)?;
                builder.classification = level.max(previous.header.classification.level);
                builder.revise(previous, body)?;
            }
        } else {
            refs.sort();
            refs.dedup();
            let run_id = new_id(builder.ids, Kind::Run)?;
            builder.classification = level;
            builder.add(run_id, "run".into(), body, refs)?;
        }
        if builder.records.is_empty() {
            return Ok(Prepared::Existing {
                case_id,
                sequence: known_sequence,
            });
        }
        if self.submission.case_id.is_none()
            && let Some((home, _, source)) = home
        {
            return Err(relocate(acquisition::other_case(home, source)));
        }
        Ok(Prepared::Accept(AcceptedTransaction {
            case_id,
            sequence: builder.sequence,
            records: builder.records,
            created: builder.created,
            updated: builder.updated,
            warnings: self.warnings.to_vec(),
        }))
    }
}

/// One attempt has one Run: an attempt that `capture.begin` declared is not
/// recorded again from an envelope. A workspace written before the paths shared
/// an identity may hold an envelope Run as well; that Run stays the envelope's.
fn refuse_captured_attempt(run: &ProducerRun, visible: &Visible<'_>) -> Result<(), Failure> {
    let identity = AttemptIdentity {
        producer: &run.producer.name,
        external_run_id: &run.external_run_id,
    };
    let recorded = capture::recorded_attempts(visible, identity);
    if recorded
        .iter()
        .any(|(_, _, path)| *path == AttemptPath::ProducerRun)
    {
        return Ok(());
    }
    match recorded.first() {
        Some((_, record, path)) => {
            Err(capture::recorded_by_other_path(record, *path).with_schema(SCHEMA))
        }
        None => Ok(()),
    }
}

/// A changed envelope keeps the Run's terminal execution status, by the rule
/// `run.reconcile` applies ([`attempt::keeps_terminal_status`]).
fn keep_terminal_status(previous: &Record, reported: ExecutionStatus) -> Result<(), Failure> {
    let recorded: ExecutionStatus = input::decode(&previous.body["execution_status"])
        .map_err(|_| integrity("The recorded Run has no valid execution status."))?;
    if attempt::keeps_terminal_status(recorded, reported) {
        return Ok(());
    }
    let run = previous.reference();
    let status = |status: ExecutionStatus| match serde_json::to_value(status) {
        Ok(Value::String(name)) => name,
        _ => format!("{status:?}"),
    };
    let mut failure = Failure::new(
        ErrorCode::GateFailed,
        format!(
            "Run {} already records the terminal execution status {}; a changed envelope cannot replace it with {}. Describe a separate attempt with another external run ID.",
            run.id,
            status(recorded),
            status(reported),
        ),
    )
    .at("/execution/state")
    .with_argv(crate::store::show_argv(run.id, previous.header.case_id))
    .with_schema(SCHEMA);
    failure.error.expected = Some(json!({"run": run, "execution_status": status(recorded)}));
    failure.error.actual = Some(json!({"execution_status": status(reported)}));
    Err(failure)
}

/// Point an acquisition conflict at the envelope field that names the attempt.
fn relocate(failure: Failure) -> Failure {
    if failure.error.path.as_deref() == Some("/source/acquisition_key") {
        failure.at("/external_run_id").with_schema(SCHEMA)
    } else {
        failure
    }
}

/// An artifact rejected or absent now that an earlier revision of the Run
/// retained, with bytes that still satisfy the declared digest, keeps its
/// retained entry: retained bytes do not disappear on a reimport.
fn carried_forward(
    retention: Retention,
    artifact: &ProducerArtifact,
    previous: Option<&ProducerRunRecord>,
) -> Retention {
    if retention.is_retained() {
        return retention;
    }
    let path = normalized_path(&artifact.path);
    previous
        .into_iter()
        .flat_map(|record| &record.artifacts)
        .find(|entry| normalized_path(&entry.declared.path) == path)
        .and_then(|entry| match &entry.retention {
            Retention::Retained { digest, .. }
                if artifact.expected_sha256.as_ref().is_none_or(|expected| {
                    digest.as_str().strip_prefix("sha256:") == Some(expected)
                }) =>
            {
                Some(entry.retention.clone())
            }
            _ => None,
        })
        .unwrap_or(retention)
}

impl IngestRecords<'_> {
    /// Record one acquisition item as planned: reuse, revise or add its Source,
    /// and reuse or add the original Artifact for its bytes.
    fn retain(
        &mut self,
        plan: &acquisition::ItemPlan,
        mut source: SourceBody,
        artifact: impl FnOnce(RevisionRef) -> ArtifactBody,
        client_ref: &str,
        level: ClassificationLevel,
    ) -> Result<(RevisionRef, RevisionRef), Failure> {
        self.classification = level;
        let source_ref = match &plan.known {
            Some(known) if known.revise => {
                source.acquisition.acquired_at = known.body.acquisition.acquired_at.clone();
                // A new description never lifts a withdrawal or drops a judgment.
                source.status = known.body.status;
                source.withdrawals = known.body.withdrawals.clone();
                source.source_assessment = known.body.source_assessment.clone();
                self.revise(&known.source, source)?
            }
            Some(known) => known.source.reference(),
            None => {
                let source_id = new_id(self.ids, Kind::Source)?;
                self.add(
                    source_id,
                    format!("{client_ref}.source"),
                    source,
                    Vec::new(),
                )?;
                RevisionRef {
                    id: source_id,
                    revision: 1,
                }
            }
        };
        let artifact_id = match plan.known.as_ref().and_then(|known| known.artifact) {
            Some(artifact_id) => artifact_id,
            None => {
                let artifact = artifact(source_ref);
                let artifact_id = new_id(self.ids, Kind::Artifact)?;
                self.add(
                    artifact_id,
                    client_ref.to_owned(),
                    artifact,
                    vec![source_ref.id],
                )?;
                artifact_id
            }
        };
        Ok((
            source_ref,
            RevisionRef {
                id: artifact_id,
                revision: 1,
            },
        ))
    }
}

/// Describe the Run and each item as recorded at the receipt's snapshot.
#[allow(clippy::too_many_arguments)]
fn producer_receipt(
    receipt: Receipt,
    case_id: Id,
    state: &State,
    run: &ProducerRun,
    staged: &[Staged],
    request: &acquisition::AcquisitionKeyRequest,
    deduplicated: bool,
    envelope: (&Digest, u64),
) -> Result<ProducerRunReceipt, Failure> {
    let missing = || integrity("The recorded producer run could not be read back.");
    let current = recorded_run(state, request).ok_or_else(missing)?;
    let record = state
        .record_at(current.header.id, receipt.snapshot.sequence)
        .ok_or_else(missing)?;
    let statements = decode_record(&record.body).ok_or_else(missing)?;
    let created: BTreeSet<Id> = receipt.created.iter().map(|change| change.id).collect();
    let updated: BTreeSet<Id> = receipt.updated.iter().map(|change| change.id).collect();
    let outcome = |source: Id, artifact: Id| {
        if deduplicated {
            ProducerItemOutcome::Deduplicated
        } else if created.contains(&source) {
            ProducerItemOutcome::Imported
        } else if updated.contains(&source) || created.contains(&artifact) {
            ProducerItemOutcome::Revised
        } else {
            ProducerItemOutcome::Deduplicated
        }
    };
    let run_ref = record.reference();
    let run_outcome = if deduplicated {
        ProducerItemOutcome::Deduplicated
    } else if created.contains(&run_ref.id) {
        ProducerItemOutcome::Imported
    } else if updated.contains(&run_ref.id) {
        ProducerItemOutcome::Revised
    } else {
        ProducerItemOutcome::Deduplicated
    };
    let envelope_ref = &statements.envelope;
    let envelope_receipt = ProducerItemReceipt {
        item: ENVELOPE_ITEM.into(),
        path: None,
        role: None,
        outcome: outcome(envelope_ref.source_ref.id, envelope_ref.artifact_ref.id),
        source_id: Some(envelope_ref.source_ref.id),
        artifact_id: Some(envelope_ref.artifact_ref.id),
        digest: Some(envelope.0.clone()),
        size_bytes: Some(envelope.1),
        expected_sha256: None,
        diagnostics: Vec::new(),
    };
    let artifacts = run
        .artifacts
        .iter()
        .zip(staged)
        .map(|(artifact, staged)| {
            let entry = statements.artifacts.iter().find(|entry| {
                normalized_path(&entry.declared.path) == normalized_path(&artifact.path)
            });
            let mut item = ProducerItemReceipt {
                item: artifact_item(artifact),
                path: Some(artifact.path.clone()),
                role: Some(artifact.role.clone()),
                outcome: ProducerItemOutcome::Missing,
                source_id: None,
                artifact_id: None,
                digest: None,
                size_bytes: None,
                expected_sha256: artifact.expected_sha256.clone(),
                diagnostics: Vec::new(),
            };
            match staged {
                Staged::Retained(prepared) => {
                    item.digest = Some(prepared.digest.clone());
                    item.size_bytes = Some(prepared.size);
                    item.diagnostics = prepared.diagnostics.clone();
                    let Some(Retention::Retained {
                        source_ref,
                        artifact_ref,
                        ..
                    }) = entry.map(|entry| &entry.retention)
                    else {
                        return Err(missing());
                    };
                    item.outcome = outcome(source_ref.id, artifact_ref.id);
                    item.source_id = Some(source_ref.id);
                    item.artifact_id = Some(artifact_ref.id);
                }
                Staged::Rejected(prepared) => {
                    item.outcome = ProducerItemOutcome::Rejected;
                    item.digest = Some(prepared.digest.clone());
                    item.size_bytes = Some(prepared.size);
                    item.diagnostics.push(
                        "The bytes differ from expected_sha256 and were not retained.".into(),
                    );
                }
                Staged::Missing => {
                    item.diagnostics.push(
                        "No file exists at this path beneath any configured import root.".into(),
                    );
                }
            }
            if !matches!(staged, Staged::Retained(_))
                && let Some(Retention::Retained { artifact_ref, .. }) =
                    entry.map(|entry| &entry.retention)
            {
                item.diagnostics.push(format!(
                    "An earlier import retained matching bytes as {}; the Run keeps that entry.",
                    artifact_ref.id
                ));
            }
            Ok(item)
        })
        .collect::<Result<Vec<_>, Failure>>()?;
    let completeness = input::decode::<EvidenceCompleteness>(&record.body["evidence_completeness"])
        .map_err(|_| missing())?;
    Ok(ProducerRunReceipt {
        receipt,
        case_id,
        run: run_ref,
        run_outcome,
        envelope: envelope_receipt,
        artifacts,
        execution: run.execution.state,
        evidence_completeness: completeness,
        measurement_validity: input::decode(&record.body["evidence_validity"])
            .unwrap_or(MeasurementValidity::Unknown),
    })
}
