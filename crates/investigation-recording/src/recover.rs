//! `capture.recover`: report what the recorder accepted, so a producer can resume.
//!
//! Recovery is a read. It reports each handle's seal state, the receipts of
//! every accepted item, staged and missing attachments, and the original
//! finalize receipt once the handle is finalized. It returns the exact commands
//! that continue the recording, each of which is an ordinary capture call that
//! only records. It never reruns, resumes or schedules an external action, and
//! never treats a missing record as proof that an action did not happen: until
//! a finalize reports otherwise, a handle's external execution is `unknown`.

use std::path::PathBuf;

use investigation_core::{
    PROGRAM_NAME,
    explain::ExecutionStatus,
    identity::Id,
    wire::{Digest, ErrorCode, Failure},
};
use investigation_store::{
    Store, Workspace,
    store::capture::{
        items::{ItemAcceptance, ItemOperation, ItemResult},
        recover::{RecoveredHandle, RecoveredStaging},
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    append::{ItemReceipt, item_receipt},
    attach::{ArtifactRef, artifact_ref},
    begin::BeginCapture,
    error::RecordingError,
    finalize::{FinalizeReceipt, receipt as finalize_receipt},
    key::{CaptureHandle, ExternalRunId, IdempotencyKey, Position, ProducerIdentity},
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

/// What a handle's staging directory holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StagingState {
    /// Staging holds the accepted items and their bytes.
    Present,
    /// The Run is declared but a crash left its staging unallocated. Resend the
    /// identical `begin` request to allocate it.
    Unallocated,
    /// The handle is finalized and its staging is gone. The manifest Artifact
    /// holds every accepted item, and recovery reads their receipts from it.
    Retired,
}

/// A capture call that continues or settles a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RecoveryAction {
    /// Resend the identical `begin` request under `key`.
    ResendBegin,
    /// Record the next fact at `next_position`.
    Append,
    /// Record the next artifact at `next_position`.
    Attach,
    /// Settle the handle. A producer that cannot vouch for every step finalizes
    /// with completion `interrupted` and execution state `unknown`.
    Finalize,
    /// Resend the identical sealing `finalize` request under `key`; only the
    /// request with `request_digest` completes the handle.
    ResendFinalize,
    /// Read the committed finalize receipt under `key`.
    LookupReceipt,
}

/// One next step for a handle and the exact command that takes it.
///
/// Every step records, reads or retries a recording call. None reruns or
/// resumes the producer's external action; whether that action happened is the
/// producer's knowledge, not the recorder's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RecoveryStep {
    /// What the step does.
    pub action: RecoveryAction,
    /// The command, as an argument array. A request is read from stdin.
    pub argv: Vec<String>,
    /// The key the request must carry, when only one key fits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<IdempotencyKey>,
    /// Digest of the only request that fits, when the handle is sealed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_digest: Option<Digest>,
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
    /// What the staging directory holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging: Option<StagingState>,
    /// The external execution state as recorded: `unknown` until a finalize
    /// commits the producer's report. Recovery never infers it.
    #[serde(default = "unknown_execution")]
    pub execution: ExecutionStatus,
    /// The position the next item must take, while the handle is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_position: Option<Position>,
    /// The calls that continue or settle this recording.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next: Vec<RecoveryStep>,
}

const fn unknown_execution() -> ExecutionStatus {
    ExecutionStatus::Unknown
}

/// Report the requested handles. Writes nothing.
pub(crate) fn recover(
    workspace: &Workspace,
    scope: &RecoverScope,
) -> Result<RecoveryState, RecordingError> {
    let handle = match scope {
        RecoverScope::Unsettled => None,
        RecoverScope::Handle { handle } => Some(handle.id()),
    };
    let recovered = Store::open(workspace.clone()).capture_recovery(handle)?;
    let handles = recovered
        .into_iter()
        .map(handle_recovery)
        .collect::<Result<_, _>>()?;
    Ok(RecoveryState { handles })
}

fn handle_recovery(recovered: RecoveredHandle) -> Result<HandleRecovery, Failure> {
    let invalid = |what: &str| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            format!("A declared capture handle has no valid {what}."),
        )
    };
    let declared = &recovered.declared;
    let begin: BeginCapture =
        serde_json::from_value(declared.request.clone()).map_err(|_| invalid("begin request"))?;
    let handle = CaptureHandle::from_id(declared.binding.handle).map_err(|_| invalid("handle"))?;
    let finalized = recovered.finalized.map(finalize_receipt).transpose()?;

    let mut items = Vec::new();
    let mut artifacts = Vec::new();
    let mut last_position = None;
    let mut sealed_by = None;
    let staged = match &recovered.staging {
        RecoveredStaging::Present { items, seal } => {
            sealed_by = seal.as_ref();
            items.as_slice()
        }
        // The finalized manifest binds every item, so its receipts survive.
        RecoveredStaging::Retired { items } => items.as_slice(),
        _ => &[],
    };
    for item in staged {
        // An earlier call made the item durable; this is that call's receipt.
        let accepted = ItemAcceptance {
            result: ItemResult::AlreadyAccepted,
            case_id: declared.case_id,
            item: item.clone(),
        };
        match item.operation {
            ItemOperation::Append => items.push(item_receipt(&accepted)?),
            ItemOperation::Attach => artifacts.push(artifact_ref(&accepted)?),
        }
        last_position = Position::new(item.position);
    }
    if let Some(finalized) = &finalized {
        // Also after staging is retired, the receipt names the sealed position.
        last_position = finalized.sealed_position;
    }

    let seal = match (&finalized, sealed_by) {
        (Some(_), _) => SealState::Finalized,
        (None, Some(_)) => SealState::Sealed,
        (None, None) => SealState::Open,
    };
    let staging = match &recovered.staging {
        RecoveredStaging::Present { .. } => StagingState::Present,
        RecoveredStaging::Unallocated => StagingState::Unallocated,
        RecoveredStaging::Retired { .. } => StagingState::Retired,
    };
    let mut next_position = None;
    let mut next = Vec::new();
    match (&finalized, sealed_by, staging) {
        (Some(finalized), _, _) => next.push(RecoveryStep {
            action: RecoveryAction::LookupReceipt,
            argv: vec![
                PROGRAM_NAME.into(),
                "receipt".into(),
                finalized.commit.command_id.clone(),
                "--actor".into(),
                declared.actor.to_string(),
                "--format".into(),
                "json".into(),
            ],
            key: IdempotencyKey::new(finalized.commit.command_id.clone()).ok(),
            request_digest: None,
        }),
        (None, Some(seal), _) => next.push(RecoveryStep {
            action: RecoveryAction::ResendFinalize,
            argv: capture_argv("finalize"),
            key: Some(IdempotencyKey::new(seal.command_id.clone()).map_err(|_| invalid("seal"))?),
            request_digest: Some(seal.request_digest.clone()),
        }),
        (None, None, StagingState::Unallocated) => next.push(RecoveryStep {
            action: RecoveryAction::ResendBegin,
            argv: capture_argv("begin"),
            key: Some(
                IdempotencyKey::new(declared.command_id.clone())
                    .map_err(|_| invalid("begin key"))?,
            ),
            request_digest: None,
        }),
        (None, None, _) => {
            let after = last_position.map_or(0, Position::get);
            next_position = Position::new(after + 1)
                .filter(|position| position.get() <= declared.binding.limits.max_items);
            if next_position.is_some() {
                for (action, operation) in [
                    (RecoveryAction::Append, "append"),
                    (RecoveryAction::Attach, "attach"),
                ] {
                    next.push(RecoveryStep {
                        action,
                        argv: capture_argv(operation),
                        key: None,
                        request_digest: None,
                    });
                }
            }
            next.push(RecoveryStep {
                action: RecoveryAction::Finalize,
                argv: capture_argv("finalize"),
                key: None,
                request_digest: None,
            });
        }
    }

    Ok(HandleRecovery {
        handle,
        run_id: declared.run_id,
        case_id: declared.case_id,
        producer: begin.producer,
        external_run_id: begin.external_run_id,
        seal,
        last_position,
        items,
        artifacts,
        execution: finalized
            .as_ref()
            .map_or(ExecutionStatus::Unknown, |finalized| finalized.execution),
        finalized,
        staging_dir: (staging == StagingState::Present)
            .then(|| PathBuf::from(&declared.binding.staging)),
        staging: Some(staging),
        next_position,
        next,
    })
}

fn capture_argv(operation: &str) -> Vec<String> {
    [
        PROGRAM_NAME,
        "capture",
        operation,
        "--input",
        "-",
        "--format",
        "json",
    ]
    .map(str::to_owned)
    .to_vec()
}
