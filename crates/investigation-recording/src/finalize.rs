//! `capture.finalize`: seal the handle and publish the Run's manifest.

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::{
    command::RevisionRef,
    explain::{EvidenceCompleteness, ExecutionStatus},
    header::{SourceTime, SystemClock, UtcTimestamp},
    identity::Id,
    producer_run::{self, Attribution},
    wire::{Digest, ErrorCode, Failure, Receipt, Snapshot},
};
use investigation_store::{
    Store, Workspace,
    store::capture::{
        attach::{AttachAvailability, AttachOutcome, MissingCause as StoreMissingCause},
        finalize::{
            FinalizeContext, FinalizePlan, FinalizeSubmission, FinalizedCapture, PlannedArtifact,
            PlannedRole, RecordedRefs,
        },
        items::ItemOperation,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    append::{CaptureEvent, ChannelEvent, LifecycleFact},
    attach::{ArtifactRole, ArtifactSource, StableArtifact},
    begin::BeginCapture,
    error::RecordingError,
    key::{
        CaptureHandle, CaptureVersion, ExternalRunId, IdempotencyKey, Metadata, Position,
        ProducerIdentity,
    },
};

/// Final summary of a capture.
///
/// Recording completeness, execution outcome and the producer's own assertions
/// stay separate: a failed execution can be finalized completely, and a
/// successful one can be finalized as partial.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct FinalizeCapture {
    /// Handle returned by `begin`.
    pub handle: CaptureHandle,
    /// Journal command ID for the finalizing transaction.
    pub key: IdempotencyKey,
    /// Last position the producer believes was accepted; `None` if it sent no items.
    ///
    /// A `complete` claim with a stale cursor is rejected.
    pub expected_last_position: Option<Position>,
    /// The producer's claim about its own stream of items.
    pub completion: CaptureCompletion,
    /// Execution outcome as the producer reports it.
    pub execution: ExecutionReport,
    /// Declared per-channel inventory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<ChannelInventory>,
    /// Item keys of every artifact the producer meant to attach, so absent ones
    /// are recorded as missing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared_artifacts: Vec<IdempotencyKey>,
    /// Producer results, retained as attributed producer assertions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assertions: Vec<ProducerAssertion>,
    /// Controls the producer reports, retained as its claims.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<ProducerControl>,
    /// References to external receipts about the action.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receipts: Vec<ExternalReceipt>,
    /// Limitations the producer states.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
    /// Inert namespaced producer metadata.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub metadata: Metadata,
}

impl FinalizeCapture {
    /// A summary with empty inventories, assertions and metadata.
    pub fn new(
        handle: CaptureHandle,
        key: IdempotencyKey,
        expected_last_position: Option<Position>,
        completion: CaptureCompletion,
        execution: ExecutionReport,
    ) -> Self {
        Self {
            handle,
            key,
            expected_last_position,
            completion,
            execution,
            channels: Vec::new(),
            declared_artifacts: Vec::new(),
            assertions: Vec::new(),
            controls: Vec::new(),
            receipts: Vec::new(),
            limitations: Vec::new(),
            metadata: Metadata::new(),
        }
    }
}

/// The producer's claim about the items it sent on a handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CaptureCompletion {
    /// Every item was sent; the cursor must match.
    Complete,
    /// Some items are known to be absent; the recorder names missing positions.
    Partial,
    /// The producer stopped or restarted and cannot vouch for its stream.
    Interrupted,
}

/// Execution outcome as reported by the producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ExecutionReport {
    /// Reported state; `unknown` when the producer cannot tell what happened.
    pub state: ExecutionStatus,
    /// Exit code, when the action had one. It implies no measurement validity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// Reported start time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<SourceTime>,
    /// Reported end time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<SourceTime>,
    /// Original error text, preserved verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ExecutionReport {
    /// A report with only a state.
    pub fn new(state: ExecutionStatus) -> Self {
        Self {
            state,
            exit_code: None,
            started_at: None,
            ended_at: None,
            error: None,
        }
    }
}

/// Declared completeness of one collection channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ChannelInventory {
    /// Channel name.
    pub channel: String,
    /// Completeness the producer claims for the channel.
    pub completeness: EvidenceCompleteness,
}

impl ChannelInventory {
    /// A channel with a completeness claim.
    pub fn new(channel: impl Into<String>, completeness: EvidenceCompleteness) -> Self {
        Self {
            channel: channel.into(),
            completeness,
        }
    }
}

/// Producer results and controls are the envelope types of
/// `inv.producer-run.v1`, so a producer that records through this API and one
/// that writes a single envelope make identical, attributed statements.
pub use investigation_core::producer_run::{ProducerAssertion, ProducerControl};

/// Reference to an external receipt about the action.
///
/// The capture form of the envelope's `ProducerReceipt`: the same fields, with
/// the receipt bytes named by an attached item key instead of an artifact path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ExternalReceipt {
    /// Receipt format, as named by the producer.
    pub kind: String,
    /// Item key of an attached artifact holding the receipt bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<IdempotencyKey>,
    /// External locator, retained as an unverified reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

impl ExternalReceipt {
    /// A receipt of the given kind with no artifact or reference yet.
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            artifact: None,
            reference: None,
        }
    }
}

/// Finalization status, independent of execution success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Finalization {
    /// Every claimed item is present.
    Finalized,
    /// Finalized with known gaps; see [`FinalizeReceipt::missing_positions`].
    FinalizedPartial,
}

/// Durable receipt for a sealed capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct FinalizeReceipt {
    /// Sealed handle.
    pub handle: CaptureHandle,
    /// The Run.
    pub run_id: Id,
    /// The case holding the Run.
    pub case_id: Id,
    /// Finalization status.
    pub finalization: Finalization,
    /// Execution state as recorded. `completed` here does not mean `finalized`,
    /// and `failed` does not prevent finalization.
    pub execution: ExecutionStatus,
    /// Last accepted position when the handle was sealed.
    pub sealed_position: Option<Position>,
    /// Positions known to be missing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_positions: Vec<Position>,
    /// Digest of the immutable manifest artifact.
    pub manifest_digest: Digest,
    /// Journal receipt of the finalizing transaction.
    pub commit: Receipt,
}

const MAX_REQUEST_BYTES: usize = investigation_core::producer_run::MAX_ENVELOPE_BYTES;
const MAX_ERROR_CHARS: usize = 16 * 1024;
const MAX_NAME_CHARS: usize = 256;
const MAX_LABEL_CHARS: usize = 128;
const MAX_UNIT_CHARS: usize = 64;
const MAX_TEXT_CHARS: usize = 2048;
const MAX_CHANNELS: usize = 256;
const MANIFEST_VERSION: &str = "inv.capture-manifest.v1";

/// Seal the handle and commit its manifest, or return the original receipt.
pub(crate) fn finalize(
    workspace: &Workspace,
    summary: &FinalizeCapture,
) -> Result<FinalizeReceipt, RecordingError> {
    validate(summary)?;
    // The normalized form, so an identical retry compares equal however its
    // JSON was spelled.
    let request = serde_json::to_value(summary).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Could not encode the finalize request.",
        )
    })?;
    let finalized = Store::open(workspace.clone()).finalize_capture(
        &FinalizeSubmission {
            handle: summary.handle.id(),
            command_id: summary.key.as_str().to_owned(),
            request,
        },
        &mut SystemClock,
        |context| plan(summary, context),
    )?;
    Ok(receipt(finalized)?)
}

/// The receipt of a committed finalization, from the Run's section.
fn receipt(finalized: FinalizedCapture) -> Result<FinalizeReceipt, Failure> {
    let record: FinalizationRecord = serde_json::from_value(finalized.section).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "The finalized Run does not hold a valid finalization record.",
        )
    })?;
    let handle = CaptureHandle::from_id(finalized.handle).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "The finalized handle is not a handle identifier.",
        )
    })?;
    Ok(FinalizeReceipt {
        handle,
        run_id: finalized.run_id,
        case_id: finalized.case_id,
        finalization: record.frozen.finalization,
        execution: record.frozen.execution.state,
        sealed_position: record.frozen.sealed_position,
        missing_positions: record.frozen.missing_positions,
        manifest_digest: record.manifest.digest,
        commit: finalized.receipt,
    })
}

/// What `capture.finalize` freezes on the Run under `capture_finalization`.
///
/// The field names follow the `producer_run` section of an envelope Run where
/// the two describe the same thing (`attribution`, `producer`,
/// `external_run_id`, `execution`, `artifacts[].retention`, `assertions`,
/// `controls`, `receipts`, `limitations`, `metadata`), so one reader can treat
/// both as producer data. `manifest` plays the envelope's part: the retained
/// bytes that hold every statement. Unknown fields are refused by `Frozen`;
/// serde cannot refuse them on a struct that flattens another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FinalizationRecord {
    schema_version: CaptureVersion,
    attribution: Attribution,
    command_id: IdempotencyKey,
    producer: ProducerIdentity,
    external_run_id: ExternalRunId,
    manifest: ManifestRef,
    #[serde(flatten)]
    frozen: Frozen,
}

/// The finalized manifest Artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRef {
    source_ref: RevisionRef,
    artifact_ref: RevisionRef,
    digest: Digest,
    size_bytes: u64,
}

/// Everything the manifest and the Run's section share.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frozen {
    /// Case head before the finalizing transaction.
    snapshot: Snapshot,
    /// The producer's claim about its stream.
    completion: CaptureCompletion,
    expected_last_position: Option<Position>,
    sealed_position: Option<Position>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    missing_positions: Vec<Position>,
    finalization: Finalization,
    /// Execution exactly as reported, including the original error text.
    execution: ExecutionReport,
    /// What survived, computed from staging; never from the producer's claim alone.
    evidence_completeness: EvidenceCompleteness,
    artifacts: Vec<InventoryEntry>,
    channels: Vec<ChannelRecord>,
    assertions: Vec<ProducerAssertion>,
    controls: Vec<ProducerControl>,
    receipts: Vec<ExternalReceipt>,
    limitations: Vec<String>,
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    metadata: Metadata,
}

/// One declared or attached artifact and what survived of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryEntry {
    key: IdempotencyKey,
    /// Accepted position; absent for a declared artifact never attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    position: Option<Position>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<ArtifactRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
    /// Whether the producer listed the key in `declared_artifacts`.
    declared: bool,
    retention: Retention,
}

/// Retention of one artifact. `retained` has the shape of the envelope Run's
/// retention, with record references only on the Run (the manifest cannot
/// name records that its own transaction creates).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum Retention {
    Retained {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_ref: Option<RevisionRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_ref: Option<RevisionRef>,
        digest: Digest,
        size_bytes: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        derived_from: Option<IdempotencyKey>,
    },
    Missing {
        cause: MissingCause,
        /// The producer's reason, verbatim, for a declared-missing source.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MissingCause {
    /// Attached as missing by the producer.
    Declared,
    /// Attached by path, and no import root held a file there.
    NotFound,
    /// Listed in `declared_artifacts` but never attached.
    NotAttached,
}

/// Completeness of one collection channel: the producer's claim and what the
/// staged items support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelRecord {
    channel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claimed: Option<EvidenceCompleteness>,
    /// Never better than the claim or than what survived.
    recorded: EvidenceCompleteness,
    retained_artifacts: usize,
    missing_artifacts: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    gaps: Vec<Position>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closed_at: Option<Position>,
}

/// The manifest bytes: the frozen summary plus every staged item, so the item
/// key, digest and receipt map and every fact survive staging retirement.
#[derive(Serialize)]
struct Manifest<'a> {
    schema_version: &'static str,
    protocol: CaptureVersion,
    handle: CaptureHandle,
    run_id: Id,
    case_id: Id,
    command_id: &'a IdempotencyKey,
    producer: &'a ProducerIdentity,
    external_run_id: &'a ExternalRunId,
    #[serde(flatten)]
    frozen: &'a Frozen,
    items: Vec<ManifestItem<'a>>,
}

#[derive(Serialize)]
struct ManifestItem<'a> {
    position: u64,
    key: &'a str,
    operation: ItemOperation,
    request_digest: &'a Digest,
    accepted_at: &'a UtcTimestamp,
    request: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<&'a Value>,
}

enum Item {
    Fact(CaptureEvent),
    Artifact(Box<StableArtifact>, AttachOutcome),
}

/// Decide what the finalization records, under the handle lock.
fn plan(summary: &FinalizeCapture, context: &FinalizeContext<'_>) -> Result<FinalizePlan, Failure> {
    let corrupt = |what: &str| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            format!("A staged {what} on this handle cannot be read as a capture request."),
        )
    };
    let begin: BeginCapture =
        serde_json::from_value(context.declaration.clone()).map_err(|_| corrupt("declaration"))?;
    let mut items = Vec::with_capacity(context.items.len());
    for item in context.items {
        items.push(match item.operation {
            ItemOperation::Append => Item::Fact(
                serde_json::from_value(item.request.clone()).map_err(|_| corrupt("fact"))?,
            ),
            ItemOperation::Attach => Item::Artifact(
                serde_json::from_value(item.request.clone()).map_err(|_| corrupt("attachment"))?,
                AttachOutcome::of(item)?.ok_or_else(|| corrupt("attachment"))?,
            ),
        });
    }
    let last = context
        .items
        .last()
        .and_then(|item| Position::new(item.position));
    let missing_positions = cursor(summary, last, context.binding.limits.max_items)?;

    let invalid =
        |path: String, message: &str| Failure::new(ErrorCode::InvalidInput, message).at(path);
    let mut declared = BTreeSet::new();
    for (index, key) in summary.declared_artifacts.iter().enumerate() {
        if !declared.insert(key.as_str()) {
            return Err(invalid(
                format!("/declared_artifacts/{index}"),
                "Declare each artifact key once.",
            ));
        }
        let fact = context
            .items
            .iter()
            .zip(&items)
            .any(|(staged, item)| staged.key == key.as_str() && matches!(item, Item::Fact(_)));
        if fact {
            return Err(invalid(
                format!("/declared_artifacts/{index}"),
                "This key names an appended fact, not an attached artifact. Declare the keys of attachments only.",
            ));
        }
    }

    // The inventory: every attachment in position order, then every declared
    // key that was never attached.
    let mut inventory = Vec::new();
    let mut planned = Vec::new();
    for item in &items {
        let Item::Artifact(artifact, outcome) = item else {
            continue;
        };
        let retention = match outcome.availability {
            AttachAvailability::Staged => {
                let (Some(digest), Some(size_bytes)) = (outcome.digest.clone(), outcome.size_bytes)
                else {
                    return Err(corrupt("attachment"));
                };
                planned.push(PlannedArtifact {
                    position: artifact.position.get(),
                    role: match (&artifact.derived_from, &outcome.derived_from) {
                        (Some(derivation), Some(original)) => {
                            let (transformation, version) = transform_parts(&derivation.transform);
                            PlannedRole::Derivative {
                                from: original.position,
                                transformation,
                                version,
                            }
                        }
                        _ => PlannedRole::Original,
                    },
                    media_type: artifact.media_type.clone(),
                });
                Retention::Retained {
                    source_ref: None,
                    artifact_ref: None,
                    digest,
                    size_bytes,
                    derived_from: artifact.derived_from.as_ref().map(|d| d.from.clone()),
                }
            }
            _ => Retention::Missing {
                cause: match outcome.missing {
                    Some(StoreMissingCause::Declared) => MissingCause::Declared,
                    _ => MissingCause::NotFound,
                },
                reason: match &artifact.source {
                    ArtifactSource::Missing { reason } => Some(reason.clone()),
                    _ => None,
                },
            },
        };
        inventory.push(InventoryEntry {
            key: artifact.key.clone(),
            position: Some(artifact.position),
            role: Some(artifact.role),
            channel: artifact.channel.clone(),
            media_type: artifact.media_type.clone(),
            declared: declared.contains(artifact.key.as_str()),
            retention,
        });
    }
    for key in &summary.declared_artifacts {
        if !inventory.iter().any(|entry| entry.key == *key) {
            inventory.push(InventoryEntry {
                key: key.clone(),
                position: None,
                role: None,
                channel: None,
                media_type: None,
                declared: true,
                retention: Retention::Missing {
                    cause: MissingCause::NotAttached,
                    reason: None,
                },
            });
        }
    }
    for (index, external) in summary.receipts.iter().enumerate() {
        if let Some(key) = &external.artifact
            && !inventory.iter().any(|entry| entry.key == *key)
        {
            return Err(invalid(
                format!("/receipts/{index}/artifact"),
                "Name the key of an attached or declared artifact.",
            ));
        }
    }

    let unvouched =
        summary.completion != CaptureCompletion::Complete || !missing_positions.is_empty();
    let channels = channels(summary, &items, &inventory, unvouched);
    let completeness = overall(
        summary,
        context.items.is_empty(),
        &inventory,
        &channels,
        unvouched,
    );
    let mut warnings = Vec::new();
    for entry in &inventory {
        if let Retention::Missing { cause, .. } = &entry.retention {
            warnings.push(match cause {
                MissingCause::NotAttached => format!(
                    "Declared artifact {} was never attached; the Run records it as missing.",
                    entry.key
                ),
                _ => format!(
                    "Artifact {} has no retained bytes; the Run records it as missing.",
                    entry.key
                ),
            });
        }
    }
    for channel in &channels {
        if let Some(claimed) = channel.claimed
            && claimed != EvidenceCompleteness::Unknown
            && claimed != channel.recorded
        {
            warnings.push(format!(
                "Channel {} is recorded as {} although the producer claims {}.",
                channel.channel,
                completeness_name(channel.recorded),
                completeness_name(claimed)
            ));
        }
    }
    if !missing_positions.is_empty() {
        warnings.push(format!(
            "Positions {} to {} were never accepted; the finalization is partial.",
            missing_positions[0],
            missing_positions[missing_positions.len() - 1]
        ));
    }

    let frozen = Frozen {
        snapshot: context.snapshot.clone(),
        completion: summary.completion,
        expected_last_position: summary.expected_last_position,
        sealed_position: last,
        missing_positions,
        finalization: if summary.completion == CaptureCompletion::Complete {
            Finalization::Finalized
        } else {
            Finalization::FinalizedPartial
        },
        execution: summary.execution.clone(),
        evidence_completeness: completeness,
        artifacts: inventory,
        channels,
        assertions: summary.assertions.clone(),
        controls: summary.controls.clone(),
        receipts: summary.receipts.clone(),
        limitations: summary.limitations.clone(),
        metadata: summary.metadata.clone(),
    };
    let handle = summary.handle;
    let mut manifest = serde_json::to_vec(&Manifest {
        schema_version: MANIFEST_VERSION,
        protocol: CaptureVersion::V1,
        handle,
        run_id: context.run_id,
        case_id: context.case_id,
        command_id: &summary.key,
        producer: &begin.producer,
        external_run_id: &begin.external_run_id,
        frozen: &frozen,
        items: context
            .items
            .iter()
            .map(|item| ManifestItem {
                position: item.position,
                key: &item.key,
                operation: item.operation,
                request_digest: &item.request_digest,
                accepted_at: &item.accepted_at,
                request: &item.request,
                outcome: item.outcome.as_ref(),
            })
            .collect(),
    })
    .map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Could not encode the manifest.",
        )
    })?;
    manifest.push(b'\n');

    let execution_status = frozen.execution.state;
    let started_at = frozen.execution.started_at.clone();
    let finished_at = frozen.execution.ended_at.clone();
    let limitations = frozen.limitations.clone();
    let metadata = frozen.metadata.clone();
    let positions: Vec<u64> = planned.iter().map(|artifact| artifact.position).collect();
    let command_id = summary.key.clone();
    let section = move |refs: &RecordedRefs| -> Result<Value, Failure> {
        let mut frozen = frozen;
        for entry in &mut frozen.artifacts {
            if let (
                Some(position),
                Retention::Retained {
                    source_ref,
                    artifact_ref,
                    ..
                },
            ) = (entry.position, &mut entry.retention)
                && let Some(index) = positions.iter().position(|p| *p == position.get())
            {
                *source_ref = Some(refs.source);
                *artifact_ref = refs.artifacts.get(index).copied();
            }
        }
        serde_json::to_value(FinalizationRecord {
            schema_version: CaptureVersion::V1,
            attribution: Attribution::ProducerReported,
            command_id,
            producer: begin.producer,
            external_run_id: begin.external_run_id,
            manifest: ManifestRef {
                source_ref: refs.source,
                artifact_ref: refs.manifest,
                digest: refs.manifest_digest.clone(),
                size_bytes: refs.manifest_size,
            },
            frozen,
        })
        .map_err(|_| {
            Failure::new(
                ErrorCode::IntegrityFailure,
                "Could not encode the finalization record.",
            )
        })
    };
    Ok(FinalizePlan {
        manifest,
        artifacts: planned,
        execution_status,
        evidence_completeness: completeness,
        started_at,
        finished_at,
        limitations,
        metadata,
        warnings,
        section: Box::new(section),
    })
}

/// Check the producer's cursor against the sealed position and name the
/// positions it sent that never arrived.
fn cursor(
    summary: &FinalizeCapture,
    last: Option<Position>,
    max_items: u64,
) -> Result<Vec<Position>, Failure> {
    let expected = summary.expected_last_position;
    if let Some(expected) = expected
        && expected.get() > max_items
    {
        let mut failure = Failure::new(
            ErrorCode::InvalidInput,
            format!("This handle accepts at most {max_items} positions."),
        )
        .at("/expected_last_position");
        failure.error.expected = Some(json!({"max_items": max_items}));
        return Err(failure);
    }
    let stale = match summary.completion {
        CaptureCompletion::Complete => expected != last,
        CaptureCompletion::Partial => expected < last,
        _ => false,
    };
    if stale {
        let message = match summary.completion {
            CaptureCompletion::Complete => format!(
                "A complete finalization must name the last accepted position, {}. Call recover, then finalize with that position, or finalize as partial.",
                last.map_or_else(|| "none".to_owned(), |last| last.to_string())
            ),
            _ => "The handle accepted items after expected_last_position. Call recover and finalize with the accepted last position.".to_owned(),
        };
        let mut failure =
            Failure::new(ErrorCode::RevisionConflict, message).at("/expected_last_position");
        failure.error.expected = Some(json!({"last_position": last}));
        failure.error.actual = Some(json!({"expected_last_position": expected}));
        failure.error.retry = Some("finalize with the accepted last position".into());
        return Err(failure);
    }
    let after = last.map_or(0, Position::get);
    Ok(match expected {
        Some(expected) if expected.get() > after => (after + 1..=expected.get())
            .filter_map(Position::new)
            .collect(),
        _ => Vec::new(),
    })
}

/// Per-channel completeness over every declared channel and every channel the
/// staged items name. A channel is recorded no better than its claim, and no
/// better than what survived: a missing artifact or a reported gap makes it
/// partial, an unclosed channel on a stream the producer cannot vouch for is
/// partial, and a channel with nothing staged, or with no retained artifact
/// among its attachments, is missing.
fn channels(
    summary: &FinalizeCapture,
    items: &[Item],
    inventory: &[InventoryEntry],
    unvouched: bool,
) -> Vec<ChannelRecord> {
    let mut records: Vec<ChannelRecord> = summary
        .channels
        .iter()
        .map(|declared| ChannelRecord {
            channel: declared.channel.clone(),
            claimed: Some(declared.completeness),
            recorded: EvidenceCompleteness::Missing,
            retained_artifacts: 0,
            missing_artifacts: 0,
            gaps: Vec::new(),
            closed_at: None,
        })
        .collect();
    let mut staged: BTreeMap<String, usize> = BTreeMap::new();
    let mut entry = |records: &mut Vec<ChannelRecord>, name: &str| -> usize {
        *staged.entry(name.to_owned()).or_default() += 1;
        match records.iter().position(|record| record.channel == name) {
            Some(index) => index,
            None => {
                records.push(ChannelRecord {
                    channel: name.to_owned(),
                    claimed: None,
                    recorded: EvidenceCompleteness::Missing,
                    retained_artifacts: 0,
                    missing_artifacts: 0,
                    gaps: Vec::new(),
                    closed_at: None,
                });
                records.len() - 1
            }
        }
    };
    for item in items {
        match item {
            Item::Fact(event) => {
                if let LifecycleFact::Channel { channel, state } = &event.fact {
                    let index = entry(&mut records, channel);
                    match state {
                        ChannelEvent::Gap { .. } => records[index].gaps.push(event.position),
                        ChannelEvent::Closed => records[index].closed_at = Some(event.position),
                        _ => {}
                    }
                }
            }
            Item::Artifact(artifact, _) => {
                if let Some(channel) = &artifact.channel {
                    let index = entry(&mut records, channel);
                    let retained = inventory.iter().any(|entry| {
                        entry.key == artifact.key
                            && matches!(entry.retention, Retention::Retained { .. })
                    });
                    if retained {
                        records[index].retained_artifacts += 1;
                    } else {
                        records[index].missing_artifacts += 1;
                    }
                }
            }
        }
    }
    for record in &mut records {
        let computed = if !staged.contains_key(&record.channel)
            || (record.retained_artifacts == 0 && record.missing_artifacts > 0)
        {
            EvidenceCompleteness::Missing
        } else if record.missing_artifacts > 0
            || !record.gaps.is_empty()
            || (record.closed_at.is_none() && unvouched)
        {
            EvidenceCompleteness::Partial
        } else {
            EvidenceCompleteness::Complete
        };
        record.recorded = match record.claimed {
            Some(claimed) if claimed != EvidenceCompleteness::Unknown => {
                if rank(claimed) < rank(computed) {
                    claimed
                } else {
                    computed
                }
            }
            _ => computed,
        };
    }
    records
}

/// Evidence completeness of the Run: what survived of the declared inventory,
/// the stream and the channels. Execution success plays no part.
fn overall(
    summary: &FinalizeCapture,
    empty: bool,
    inventory: &[InventoryEntry],
    channels: &[ChannelRecord],
    unvouched: bool,
) -> EvidenceCompleteness {
    let retained = inventory
        .iter()
        .filter(|entry| matches!(entry.retention, Retention::Retained { .. }))
        .count();
    if empty && inventory.is_empty() {
        return if summary.completion == CaptureCompletion::Complete && channels.is_empty() {
            EvidenceCompleteness::Complete
        } else {
            EvidenceCompleteness::Missing
        };
    }
    if !inventory.is_empty() && retained == 0 {
        return EvidenceCompleteness::Missing;
    }
    let shortfall = retained < inventory.len()
        || unvouched
        || channels
            .iter()
            .any(|channel| channel.recorded != EvidenceCompleteness::Complete);
    if shortfall {
        EvidenceCompleteness::Partial
    } else {
        EvidenceCompleteness::Complete
    }
}

const fn rank(completeness: EvidenceCompleteness) -> u8 {
    match completeness {
        EvidenceCompleteness::Missing => 0,
        EvidenceCompleteness::Partial => 1,
        EvidenceCompleteness::Complete => 2,
        _ => 3,
    }
}

const fn completeness_name(completeness: EvidenceCompleteness) -> &'static str {
    match completeness {
        EvidenceCompleteness::Complete => "complete",
        EvidenceCompleteness::Partial => "partial",
        EvidenceCompleteness::Missing => "missing",
        _ => "unknown",
    }
}

/// Split a `name/version` transform label; a label without a version keeps
/// its whole text as the name and records the version as unspecified.
fn transform_parts(transform: &str) -> (String, String) {
    match transform.rsplit_once('/') {
        Some((name, version))
            if !name.is_empty() && !version.is_empty() && version.chars().count() <= 128 =>
        {
            (name.to_owned(), version.to_owned())
        }
        _ => (transform.to_owned(), "unspecified".to_owned()),
    }
}

fn validate(summary: &FinalizeCapture) -> Result<(), Failure> {
    let invalid = |path: &str, message: String| {
        Failure::new(ErrorCode::InvalidInput, message).at(path.to_owned())
    };
    let name = |path: &str, value: &str, max: usize| {
        if value.trim().is_empty()
            || value.chars().count() > max
            || value.chars().any(char::is_control)
        {
            Err(invalid(
                path,
                format!("Use 1 to {max} characters without control characters."),
            ))
        } else {
            Ok(())
        }
    };
    let bounded = |path: &str, count: usize, max: usize| {
        if count > max {
            Err(invalid(path, format!("List at most {max} entries.")))
        } else {
            Ok(())
        }
    };
    let metadata = |path: &str, metadata: &Metadata| {
        investigation_store::opaque::validate_metadata(metadata)
            .map_err(|(message, key)| invalid(&format!("{path}{key}"), message))
    };
    let size = serde_json::to_vec(summary).map_or(usize::MAX, |bytes| bytes.len());
    if size > MAX_REQUEST_BYTES {
        let mut failure = invalid(
            "",
            format!(
                "The finalize request is {size} bytes; at most {MAX_REQUEST_BYTES} are accepted. Retain large results as an attached artifact."
            ),
        );
        failure.error.path = None;
        return Err(failure);
    }
    if let Some(error) = &summary.execution.error
        && (error.is_empty() || error.chars().count() > MAX_ERROR_CHARS)
    {
        return Err(invalid(
            "/execution/error",
            format!(
                "Keep the original error text to 1 to {MAX_ERROR_CHARS} characters; it is preserved verbatim."
            ),
        ));
    }
    bounded("/channels", summary.channels.len(), MAX_CHANNELS)?;
    let mut channels = BTreeSet::new();
    for (index, channel) in summary.channels.iter().enumerate() {
        let path = format!("/channels/{index}/channel");
        name(&path, &channel.channel, MAX_NAME_CHARS)?;
        if !channels.insert(channel.channel.as_str()) {
            return Err(invalid(&path, "Declare each channel once.".into()));
        }
    }
    bounded(
        "/assertions",
        summary.assertions.len(),
        producer_run::MAX_ASSERTIONS,
    )?;
    for (index, assertion) in summary.assertions.iter().enumerate() {
        let at = |field: &str| format!("/assertions/{index}/{field}");
        name(&at("kind"), &assertion.kind, MAX_LABEL_CHARS)?;
        name(&at("name"), &assertion.name, MAX_LABEL_CHARS)?;
        if let Some(unit) = &assertion.unit {
            name(&at("unit"), unit, MAX_UNIT_CHARS)?;
        }
        let size = serde_json::to_vec(&assertion.value).map_or(usize::MAX, |bytes| bytes.len());
        if size > producer_run::MAX_VALUE_BYTES {
            return Err(invalid(
                &at("value"),
                format!(
                    "Keep an assertion value under {} bytes. Retain larger results as an attached artifact.",
                    producer_run::MAX_VALUE_BYTES
                ),
            ));
        }
        metadata(&at("metadata"), &assertion.metadata)?;
    }
    bounded(
        "/controls",
        summary.controls.len(),
        producer_run::MAX_CONTROLS,
    )?;
    for (index, control) in summary.controls.iter().enumerate() {
        let at = |field: &str| format!("/controls/{index}/{field}");
        name(&at("name"), &control.name, MAX_LABEL_CHARS)?;
        name(
            &at("producer_result"),
            &control.producer_result,
            MAX_LABEL_CHARS,
        )?;
        metadata(&at("metadata"), &control.metadata)?;
    }
    bounded(
        "/receipts",
        summary.receipts.len(),
        producer_run::MAX_RECEIPTS,
    )?;
    for (index, receipt) in summary.receipts.iter().enumerate() {
        let at = |field: &str| format!("/receipts/{index}/{field}");
        name(&at("kind"), &receipt.kind, MAX_LABEL_CHARS)?;
        if let Some(reference) = &receipt.reference {
            name(&at("reference"), reference, MAX_TEXT_CHARS)?;
        }
        if receipt.artifact.is_none() && receipt.reference.is_none() {
            return Err(invalid(
                &format!("/receipts/{index}"),
                "Give the receipt the key of an attached artifact, a reference, or both.".into(),
            ));
        }
    }
    bounded(
        "/limitations",
        summary.limitations.len(),
        producer_run::MAX_LIMITATIONS,
    )?;
    for (index, limitation) in summary.limitations.iter().enumerate() {
        name(&format!("/limitations/{index}"), limitation, MAX_TEXT_CHARS)?;
    }
    metadata("/metadata", &summary.metadata)
}
