//! `capture.recover`: report what the recorder accepted, so a producer can resume.

use std::path::PathBuf;

use investigation_core::identity::Id;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    append::ItemReceipt,
    attach::ArtifactRef,
    finalize::FinalizeReceipt,
    key::{CaptureHandle, ExternalRunId, Position, ProducerIdentity},
};

/// Which handles to report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum RecoverScope {
    /// Every handle that is not finalized.
    Unsettled,
    /// One handle, in any state, including finalized.
    Handle {
        /// The handle.
        handle: CaptureHandle,
    },
}

/// Seal state of a handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SealState {
    /// Accepting items.
    Open,
    /// Sealed; the manifest is not yet published.
    Sealed,
    /// The manifest is published.
    Finalized,
}

/// Accepted state of the requested handles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RecoveryState {
    /// One entry per reported handle.
    pub handles: Vec<HandleRecovery>,
}

/// What one handle accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct HandleRecovery {
    /// The handle.
    pub handle: CaptureHandle,
    /// The Run it declared.
    pub run_id: Id,
    /// The case holding the Run.
    pub case_id: Id,
    /// Producer identity from `begin`.
    pub producer: ProducerIdentity,
    /// External run ID from `begin`.
    pub external_run_id: ExternalRunId,
    /// Seal state.
    pub seal: SealState,
    /// Last accepted position; the next item takes the following one.
    pub last_position: Option<Position>,
    /// Receipts of accepted facts, in position order.
    pub items: Vec<ItemReceipt>,
    /// Receipts of accepted artifacts, in position order.
    pub artifacts: Vec<ArtifactRef>,
    /// The finalize receipt, once finalized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalized: Option<FinalizeReceipt>,
    /// Staging directory, until it is retired after finalization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging_dir: Option<PathBuf>,
}
