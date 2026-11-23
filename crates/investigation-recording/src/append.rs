//! `capture.append`: add one bounded lifecycle or collection fact.

use investigation_core::{
    header::{SourceTime, SystemClock, UtcTimestamp},
    identity::Id,
    wire::{Digest, ErrorCode, Failure},
};
use investigation_store::{
    Store, Workspace,
    store::capture::items::{ItemAcceptance, ItemOperation, ItemResult, ItemSubmission},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    error::RecordingError,
    key::{CaptureHandle, IdempotencyKey, Metadata, Position, ReceiptResult},
};

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

const MAX_NAME_CHARS: usize = 256;
const MAX_CODE_CHARS: usize = 128;
const MAX_DETAIL_CHARS: usize = 4096;
const MAX_MESSAGE_CHARS: usize = 16 * 1024;

/// Stage one fact at the next position of its handle.
pub(crate) fn append(
    workspace: &Workspace,
    event: &CaptureEvent,
) -> Result<ItemReceipt, RecordingError> {
    validate(event)?;
    // The normalized form, so an identical retry compares equal however its
    // JSON was spelled.
    let request = serde_json::to_value(event).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Could not encode the append request.",
        )
    })?;
    let accepted = Store::open(workspace.clone()).stage_capture_item(
        &ItemSubmission {
            operation: ItemOperation::Append,
            handle: event.handle.id(),
            key: event.key.as_str().to_owned(),
            position: event.position.get(),
            request,
            outcome: None,
        },
        &mut SystemClock,
    )?;
    Ok(item_receipt(&accepted)?)
}

/// The receipt of a staged item, for `append` and `attach` alike.
pub(crate) fn item_receipt(accepted: &ItemAcceptance) -> Result<ItemReceipt, Failure> {
    let item = &accepted.item;
    let invalid = || {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "A staged capture item does not hold a valid receipt.",
        )
    };
    Ok(ItemReceipt {
        result: match accepted.result {
            ItemResult::Accepted => ReceiptResult::Accepted,
            ItemResult::AlreadyAccepted => ReceiptResult::AlreadyAccepted,
        },
        handle: CaptureHandle::from_id(item.handle).map_err(|_| invalid())?,
        run_id: item.run_id,
        position: Position::new(item.position).ok_or_else(invalid)?,
        key: IdempotencyKey::new(item.key.clone()).map_err(|_| invalid())?,
        request_digest: item.request_digest.clone(),
        accepted_at: item.accepted_at.clone(),
    })
}

fn validate(event: &CaptureEvent) -> Result<(), Failure> {
    let invalid = |path: &str, message: String| {
        Failure::new(ErrorCode::InvalidInput, message).at(path.to_owned())
    };
    let name = |path: &str, value: &str| {
        if value.trim().is_empty()
            || value.chars().count() > MAX_NAME_CHARS
            || value.chars().any(char::is_control)
        {
            Err(invalid(
                path,
                format!("Use 1 to {MAX_NAME_CHARS} characters without control characters."),
            ))
        } else {
            Ok(())
        }
    };
    let text = |path: &str, value: &str, max: usize| {
        if value.trim().is_empty() || value.chars().count() > max {
            Err(invalid(
                path,
                format!("Use 1 to {max} characters, or omit the field."),
            ))
        } else {
            Ok(())
        }
    };
    match &event.fact {
        LifecycleFact::Started => {}
        LifecycleFact::PhaseChanged { phase } => name("/fact/phase", phase)?,
        LifecycleFact::Error {
            message,
            external_code,
        } => {
            text("/fact/message", message, MAX_MESSAGE_CHARS)?;
            if let Some(code) = external_code {
                if code.chars().count() > MAX_CODE_CHARS {
                    return Err(invalid(
                        "/fact/external_code",
                        format!("Use at most {MAX_CODE_CHARS} characters."),
                    ));
                }
                name("/fact/external_code", code)?;
            }
        }
        LifecycleFact::Cancelled { reason } => {
            if let Some(reason) = reason {
                text("/fact/reason", reason, MAX_DETAIL_CHARS)?;
            }
        }
        LifecycleFact::Channel { channel, state } => {
            name("/fact/channel", channel)?;
            if let ChannelEvent::Gap {
                detail: Some(detail),
            } = state
            {
                text("/fact/state/detail", detail, MAX_DETAIL_CHARS)?;
            }
        }
    }
    investigation_store::opaque::validate_metadata(&event.metadata)
        .map_err(|(message, key)| invalid(&format!("/metadata{key}"), message))
}
