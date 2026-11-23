//! `capture.append`: add one bounded lifecycle or collection fact.

use investigation_core::{
    header::{SourceTime, UtcTimestamp},
    identity::Id,
    wire::Digest,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::key::{CaptureHandle, IdempotencyKey, Metadata, Position, ReceiptResult};

/// One typed fact at the next position of a handle.
///
/// High-frequency samples belong in an attached artifact, not in individual facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CaptureEvent {
    /// Handle returned by `begin`.
    pub handle: CaptureHandle,
    /// Producer event ID, unique within the handle.
    pub key: IdempotencyKey,
    /// The position the producer expects this item to take.
    pub position: Position,
    /// The fact itself.
    pub fact: LifecycleFact,
    /// Time reported by the producer. It is a claim; the receipt carries the
    /// separately recorded acceptance time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_at: Option<SourceTime>,
    /// Inert namespaced producer metadata.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub metadata: Metadata,
}

impl CaptureEvent {
    /// A fact with no reported time or metadata.
    pub fn new(
        handle: CaptureHandle,
        key: IdempotencyKey,
        position: Position,
        fact: LifecycleFact,
    ) -> Self {
        Self {
            handle,
            key,
            position,
            fact,
            reported_at: None,
            metadata: Metadata::new(),
        }
    }
}

/// Typed lifecycle or collection fact reported by a producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum LifecycleFact {
    /// The producer reports that its external action started.
    Started,
    /// The producer reports a named phase change.
    PhaseChanged {
        /// Phase name.
        phase: String,
    },
    /// The producer reports an error, preserved verbatim.
    Error {
        /// Error text as reported.
        message: String,
        /// The producer's own error code, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_code: Option<String>,
    },
    /// The producer reports cancellation of its action.
    Cancelled {
        /// Reason as reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// A change in one collection channel, such as a log or trace stream.
    Channel {
        /// Channel name, matched against the inventory at finalization.
        channel: String,
        /// What happened to the channel.
        state: ChannelEvent,
    },
}

/// A collection channel change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum ChannelEvent {
    /// Collection started.
    Opened,
    /// Collection ended normally.
    Closed,
    /// The producer knows collection lost data.
    Gap {
        /// What was lost, as reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// Durable receipt for one accepted item on a handle.
///
/// The recorder persists the item, this receipt and the handle cursor before
/// returning it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ItemReceipt {
    /// First acceptance or replay.
    pub result: ReceiptResult,
    /// Handle holding the item.
    pub handle: CaptureHandle,
    /// Run declared by the handle.
    pub run_id: Id,
    /// Accepted position.
    pub position: Position,
    /// Producer item key.
    pub key: IdempotencyKey,
    /// Digest of the parsed request, used to detect conflicting retries.
    pub request_digest: Digest,
    /// Local acceptance time, distinct from any producer-reported time.
    pub accepted_at: UtcTimestamp,
}
