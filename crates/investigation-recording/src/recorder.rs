//! The [`Recorder`] contract and its local-workspace implementation.

use std::{io, path::Path, sync::Arc};

use investigation_core::{
    PROGRAM_NAME,
    wire::{ErrorCode, Failure},
};
use investigation_store::{Workspace, workspace::WorkspaceError};

use crate::{
    append::{CaptureEvent, ItemReceipt},
    attach::{ArtifactRef, StableArtifact},
    begin::{BeginCapture, BeginReceipt},
    error::RecordingError,
    finalize::{FinalizeCapture, FinalizeReceipt},
    key::Operation,
    recover::{RecoverScope, RecoveryState},
};

/// Records what an external producer did, through the same validation and
/// durable transactions as every other Investigation write.
///
/// The producer owns execution: its permissions, scheduling, limits,
/// cancellation and cleanup. A recorder only receives facts and bytes about a run
/// and returns durable receipts. No call authorizes, starts, schedules or reruns
/// anything, and no call invokes code supplied by the caller.
///
/// # Idempotency and retries
///
/// Every mutating call carries an [`IdempotencyKey`](crate::IdempotencyKey).
/// Requests are taken by reference so a producer can resend the identical value
/// after a lost response. An identical retry returns the original receipt; the
/// same key with different content is rejected as a conflict.
///
/// # Implementing
///
/// Producers normally use [`LocalRecorder`]. The trait is public and dyn-compatible
/// so a producer can wrap a recorder, for example in a bounded local spool that
/// reconciles later, or test its own control flow. Receipts come from a real
/// recorder; a wrapper passes them through unchanged.
pub trait Recorder {
    /// Declare one Run and allocate private durable staging for it.
    ///
    /// Commits one transaction that creates the Run in the `declared` state, and a
    /// minimal `record`-mode case when [`CaseTarget::NewRecordCase`](crate::CaseTarget)
    /// is requested. Returns the handle, Run and case IDs, negotiated protocol,
    /// limits and staging directory. A retry with the same producer, external run
    /// ID and key returns the same Run and handle.
    ///
    /// # Does not
    ///
    /// - Authorize, approve or start the producer's action. The producer decides
    ///   whether and how to execute, under its own policy.
    /// - Check that the producer is permitted to act, or verify its identity.
    /// - Open a hypothesis, question or decision, or require a vault.
    fn begin(&self, request: &BeginCapture) -> Result<BeginReceipt, RecordingError>;

    /// Accept one bounded lifecycle or collection fact at the next position.
    ///
    /// Only the next expected position is accepted. The item, its receipt and the
    /// handle cursor are durable before the receipt is returned. A fact is staged
    /// and summarized at finalization; it is not one journal transaction.
    ///
    /// # Does not
    ///
    /// - Authorize, start, pause, cancel or rerun the action it describes. A
    ///   `cancelled` fact records a report; it cancels nothing.
    /// - Reorder or fill gaps. A missing position stays missing.
    /// - Treat the producer's reported time as verified time or as ordering.
    fn append(&self, event: &CaptureEvent) -> Result<ItemReceipt, RecordingError>;

    /// Copy one completed artifact into staging and verify its digest and size,
    /// or record it as missing.
    ///
    /// The copy happens without the canonical commit lock. A file that changes
    /// during the copy, or a digest or size mismatch, rejects the item and leaves
    /// the handle usable.
    ///
    /// # Does not
    ///
    /// - Authorize, start or rerun the action that produced the bytes, or run any
    ///   parser, extractor or command named by the artifact or its media type.
    /// - Accept a mutable reference as finalized bytes, or hardlink a
    ///   producer-writable path into the store.
    /// - Replace an absent artifact with an empty one.
    fn attach(&self, artifact: &StableArtifact) -> Result<ArtifactRef, RecordingError>;

    /// Seal the handle and publish the Run's immutable manifest.
    ///
    /// Seals staging durably, publishes retained bytes, then commits one
    /// transaction binding the manifest digest to the Run. The manifest records
    /// retained and missing artifacts, execution state, producer assertions,
    /// controls, receipts, per-channel completeness and the item-key, digest and
    /// receipt map. Later items on the handle are refused as closed.
    ///
    /// # Does not
    ///
    /// - Authorize, start or rerun anything, or wait for the producer's action.
    /// - Create an Assessment or a validated Conclusion, or treat an exit code or
    ///   a `reported_pass` control as proof of a sound result.
    /// - Infer `execution_completed` from `finalized`, or the reverse.
    fn finalize(&self, summary: &FinalizeCapture) -> Result<FinalizeReceipt, RecordingError>;

    /// Report accepted positions, receipts and seal state so a producer can
    /// resume at the right position after a crash or lost response.
    ///
    /// This is a read. It needs no idempotency key and records nothing; the
    /// receipts it returns are the durable receipts of earlier calls. To settle an
    /// interrupted capture, call [`Recorder::finalize`] with
    /// [`CaptureCompletion::Interrupted`](crate::CaptureCompletion).
    ///
    /// # Does not
    ///
    /// - Rerun, resume or schedule any external action. A missing record is not
    ///   proof that an external effect did not happen.
    /// - Delete, compact or retire staged bytes.
    /// - Finalize a handle or change its state.
    fn recover(&self, scope: &RecoverScope) -> Result<RecoveryState, RecordingError>;
}

macro_rules! forward_recorder {
    ($($pointer:ty),*) => {$(
        impl<R: Recorder + ?Sized> Recorder for $pointer {
            fn begin(&self, request: &BeginCapture) -> Result<BeginReceipt, RecordingError> {
                (**self).begin(request)
            }
            fn append(&self, event: &CaptureEvent) -> Result<ItemReceipt, RecordingError> {
                (**self).append(event)
            }
            fn attach(&self, artifact: &StableArtifact) -> Result<ArtifactRef, RecordingError> {
                (**self).attach(artifact)
            }
            fn finalize(
                &self,
                summary: &FinalizeCapture,
            ) -> Result<FinalizeReceipt, RecordingError> {
                (**self).finalize(summary)
            }
            fn recover(&self, scope: &RecoverScope) -> Result<RecoveryState, RecordingError> {
                (**self).recover(scope)
            }
        }
    )*};
}

forward_recorder!(&R, Box<R>, Arc<R>);

/// A [`Recorder`] over an initialized local Investigation workspace.
///
/// It shares the store's journal, blob and lock implementation with the CLI; it
/// has no separate evidence store. It never spawns a process and never holds the
/// commit lock while caller code runs.
#[derive(Debug, Clone)]
pub struct LocalRecorder {
    workspace: Workspace,
}

impl LocalRecorder {
    /// Open the workspace whose root contains `investigation.toml`.
    ///
    /// Opening reads the workspace marker and configuration only; it records
    /// nothing and takes no lock.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, RecordingError> {
        let root = root.as_ref();
        Workspace::open(root)
            .map(|workspace| Self { workspace })
            .map_err(|error| workspace_failure(root, error).into())
    }

    /// The workspace root.
    pub fn workspace_root(&self) -> &Path {
        self.workspace.root()
    }
}

// Each operation lands with its own durable implementation. Until then the
// call reports `Unsupported` and records nothing, rather than pretending.
impl Recorder for LocalRecorder {
    fn begin(&self, _request: &BeginCapture) -> Result<BeginReceipt, RecordingError> {
        Err(unsupported(Operation::Begin))
    }

    fn append(&self, _event: &CaptureEvent) -> Result<ItemReceipt, RecordingError> {
        Err(unsupported(Operation::Append))
    }

    fn attach(&self, _artifact: &StableArtifact) -> Result<ArtifactRef, RecordingError> {
        Err(unsupported(Operation::Attach))
    }

    fn finalize(&self, _summary: &FinalizeCapture) -> Result<FinalizeReceipt, RecordingError> {
        Err(unsupported(Operation::Finalize))
    }

    fn recover(&self, _scope: &RecoverScope) -> Result<RecoveryState, RecordingError> {
        Err(unsupported(Operation::Recover))
    }
}

const fn unsupported(operation: Operation) -> RecordingError {
    RecordingError::Unsupported { operation }
}

fn workspace_failure(root: &Path, error: WorkspaceError) -> Failure {
    let missing = matches!(&error, WorkspaceError::WorkspaceNotFound(_))
        || matches!(&error, WorkspaceError::Io { source, .. }
            if source.kind() == io::ErrorKind::NotFound && !root.join("investigation.toml").is_file());
    let code = match &error {
        _ if missing => ErrorCode::WorkspaceRequired,
        WorkspaceError::Io { .. }
        | WorkspaceError::FilesystemDetection { .. }
        | WorkspaceError::Serialization(_)
        | WorkspaceError::Identity(_) => ErrorCode::IoFailure,
        WorkspaceError::InvalidRoot(_)
        | WorkspaceError::SymlinkRejected(_)
        | WorkspaceError::UnsupportedFilesystem { .. } => ErrorCode::InvalidInput,
        _ => ErrorCode::IntegrityFailure,
    };
    let failure = Failure::new(code, error.to_string()).at("/workspace");
    if missing {
        failure.with_argv(
            [PROGRAM_NAME, "onboard", "--format", "json"]
                .map(str::to_owned)
                .to_vec(),
        )
    } else {
        failure
    }
}
