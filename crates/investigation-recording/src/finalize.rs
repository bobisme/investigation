//! `capture.finalize`: seal the handle and publish the Run's manifest.

use investigation_core::{
    explain::{EvidenceCompleteness, ExecutionStatus},
    header::SourceTime,
    identity::Id,
    wire::{Digest, Receipt},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::key::{CaptureHandle, IdempotencyKey, Metadata, Position};

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
