//! `capture.begin`: declare one Run and obtain a capture handle.

use std::path::PathBuf;

use investigation_core::{
    command::RevisionRef,
    header::{Actor, ClassificationLevel},
    identity::Id,
    wire::{Digest, Receipt},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::key::{
    CaptureHandle, CaptureVersion, ExternalRunId, IdempotencyKey, Metadata, ProducerIdentity,
};

/// Request to declare one Run before the producer acts.
///
/// Idempotent on the producer name, external run ID and key: a retry after a lost
/// response returns the same Run and handle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BeginCapture {
    /// Journal command ID for the declaring transaction.
    pub key: IdempotencyKey,
    /// Actor attributed with the declaration and every later call on the handle.
    pub actor: Actor,
    /// Self-reported producer identity.
    pub producer: ProducerIdentity,
    /// The producer's identifier for this logical attempt.
    pub external_run_id: ExternalRunId,
    /// What the producer intends the Run to do.
    pub intent: CaptureIntent,
    /// Case that receives the Run.
    pub case: CaseTarget,
    /// Default classification for retained items; an item may request a stricter one.
    #[serde(default)]
    pub classification: ClassificationLevel,
    /// Inert namespaced producer metadata.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub metadata: Metadata,
}

impl BeginCapture {
    /// A request with `internal` classification and no metadata.
    pub fn new(
        key: IdempotencyKey,
        actor: Actor,
        producer: ProducerIdentity,
        external_run_id: ExternalRunId,
        intent: CaptureIntent,
        case: CaseTarget,
    ) -> Self {
        Self {
            key,
            actor,
            producer,
            external_run_id,
            intent,
            case,
            classification: ClassificationLevel::default(),
            metadata: Metadata::new(),
        }
    }
}

/// The producer's stated intent. It is recorded, never executed or approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CaptureIntent {
    /// Short human-readable purpose.
    pub label: String,
    /// Exact revision of an InquiryPlan in the target case, when the Run follows one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inquiry_plan: Option<RevisionRef>,
    /// Digest of a plan held outside Investigation, when the producer has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_plan_digest: Option<Digest>,
}

impl CaptureIntent {
    /// An exploratory intent with no plan reference.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            inquiry_plan: None,
            external_plan_digest: None,
        }
    }
}

/// Case that receives the Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum CaseTarget {
    /// Record into an existing case.
    Existing {
        /// The case identifier.
        case_id: Id,
    },
    /// Create a minimal `record`-mode case in the same transaction as the Run.
    ///
    /// The case has no question, hypothesis or decision, and needs no vault.
    NewRecordCase {
        /// Case title.
        title: String,
    },
}

/// Limits the recorder enforces on one handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CaptureLimits {
    /// Largest accepted serialized `append` request, in bytes.
    pub max_event_bytes: u64,
    /// Largest accepted attachment, in bytes.
    pub max_attachment_bytes: u64,
    /// Largest accepted position on one handle.
    pub max_items: u64,
}

impl CaptureLimits {
    /// Limits with the given values.
    pub const fn new(max_event_bytes: u64, max_attachment_bytes: u64, max_items: u64) -> Self {
        Self {
            max_event_bytes,
            max_attachment_bytes,
            max_items,
        }
    }
}

/// Durable receipt for a declared Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BeginReceipt {
    /// Handle for every later call on this capture.
    pub handle: CaptureHandle,
    /// The declared Run.
    pub run_id: Id,
    /// The case holding the Run.
    pub case_id: Id,
    /// Negotiated protocol version.
    pub protocol: CaptureVersion,
    /// Limits for this handle.
    pub limits: CaptureLimits,
    /// Private durable staging directory owned by the recorder for this handle.
    ///
    /// The recorder copies attachments into it. Producers must not write into it or
    /// link files from it; it is recovery material, not a cache.
    pub staging_dir: PathBuf,
    /// Journal receipt of the declaring transaction. On a retry its result is
    /// `already_committed` and every identifier is unchanged.
    pub commit: Receipt,
}
