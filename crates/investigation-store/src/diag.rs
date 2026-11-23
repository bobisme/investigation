//! Diagnostic logging for the commit path.
//!
//! Events go through `tracing` and reach stderr only when the CLI installs a
//! subscriber. Fields are limited to opaque identifiers, stage names, counts,
//! timings and error classes. Never log payload text, titles, excerpts,
//! credentials or filesystem paths here: the default log must be safe to share.

use std::time::Instant;

use investigation_core::{
    identity::{Id, Kind},
    state::AcceptedTransaction,
    wire::{ErrorCode, Failure},
};

/// Log target for every commit-path event.
pub const TARGET: &str = "investigation::commit";

/// Persistence stage, named after the numbered steps of the commit algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    /// Step 1: hash retained promotion evidence before the lock is retaken.
    VerifyArtifacts,
    /// Step 2: coordination and case locks.
    Lock,
    /// Step 3: replay and verify the journal tail.
    VerifyTail,
    /// Step 4: actor and command ID lookup.
    Idempotency,
    /// Step 5: validate against a temporary state and assign IDs.
    Validate,
    /// Step 6: publish immutable blobs.
    BlobPublish,
    /// Step 7: write and fsync the staged event.
    JournalWrite,
    /// Step 7: exclusive publication of the event.
    JournalPublish,
    /// Step 7: directory fsync after publication.
    DirectorySync,
    /// Step 8: disposable index update after the commit boundary.
    IndexUpdate,
    /// Step 10: receipt returned.
    Receipt,
}

impl Stage {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::VerifyArtifacts => "verify_artifacts",
            Self::Lock => "lock",
            Self::VerifyTail => "verify_tail",
            Self::Idempotency => "idempotency",
            Self::Validate => "validate",
            Self::BlobPublish => "blob_publish",
            Self::JournalWrite => "journal_write",
            Self::JournalPublish => "journal_publish",
            Self::DirectorySync => "directory_sync",
            Self::IndexUpdate => "index_update",
            Self::Receipt => "receipt",
        }
    }

    pub(crate) const fn step(self) -> u8 {
        match self {
            Self::VerifyArtifacts => 1,
            Self::Lock => 2,
            Self::VerifyTail => 3,
            Self::Idempotency => 4,
            Self::Validate => 5,
            Self::BlobPublish => 6,
            Self::JournalWrite | Self::JournalPublish | Self::DirectorySync => 7,
            Self::IndexUpdate => 8,
            Self::Receipt => 10,
        }
    }
}

/// Tracks how far one commit got, for the failure event.
pub(crate) struct CommitTrace {
    command_id: String,
    stage: Stage,
    started: Instant,
    transaction_id: Option<Id>,
    run_id: Option<Id>,
    sequence: Option<u64>,
}

impl CommitTrace {
    pub(crate) fn new(command_id: &str) -> Self {
        Self {
            command_id: command_id.to_owned(),
            stage: Stage::Lock,
            started: Instant::now(),
            transaction_id: None,
            run_id: None,
            sequence: None,
        }
    }

    pub(crate) fn enter(&mut self, stage: Stage) {
        self.stage = stage;
        tracing::debug!(
            target: TARGET,
            command_id = self.command_id.as_str(),
            transaction_id = self.transaction_id.map(|id| id.to_string()).as_deref().unwrap_or("-"),
            stage = stage.name(),
            step = stage.step(),
            elapsed_ms = self.elapsed_ms(),
            "commit stage"
        );
    }

    pub(crate) fn sequence(&mut self, sequence: u64) {
        self.sequence = Some(sequence);
    }

    pub(crate) fn accepted(&mut self, accepted: &AcceptedTransaction, transaction_id: Id) {
        self.transaction_id = Some(transaction_id);
        self.run_id = accepted
            .created
            .iter()
            .map(|change| change.id)
            .find(|id| id.kind() == Kind::Run);
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub(crate) fn finish(&self, command_id: &str, case_id: Option<Id>, outcome: Outcome<'_>) {
        let transaction = self.transaction_id.map(|id| id.to_string());
        let run = self.run_id.map(|id| id.to_string());
        let case = case_id.map(|id| id.to_string());
        let (transaction, run, case) = (
            transaction.as_deref().unwrap_or("-"),
            run.as_deref().unwrap_or("-"),
            case.as_deref().unwrap_or("-"),
        );
        let sequence = self.sequence.unwrap_or(0);
        match outcome {
            Outcome::Committed => tracing::info!(
                target: TARGET,
                command_id, case_id = case, transaction_id = transaction, run_id = run,
                sequence, stage = Stage::Receipt.name(), elapsed_ms = self.elapsed_ms(),
                "commit complete"
            ),
            Outcome::Failed(failure) => {
                let class = serde_json::to_value(failure.error.code)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let status = serde_json::to_value(failure.commit_status)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default();
                // Storage and integrity trouble in the persistence stages is for
                // operators. A request rejected while validating is an ordinary
                // outcome the receipt already reports.
                let operational = !matches!(self.stage, Stage::Validate | Stage::Idempotency)
                    && matches!(
                        failure.error.code,
                        ErrorCode::IoFailure
                            | ErrorCode::IntegrityFailure
                            | ErrorCode::DurabilityUnknown
                            | ErrorCode::WorkspaceBusy
                    );
                macro_rules! failed {
                    ($level:ident) => {
                        tracing::$level!(
                            target: TARGET,
                            command_id, case_id = case, transaction_id = transaction,
                            run_id = run, sequence, stage = self.stage.name(),
                            step = self.stage.step(), error_class = class,
                            commit_status = status, elapsed_ms = self.elapsed_ms(),
                            "commit failed; recover with the same actor and command ID"
                        )
                    };
                }
                if operational {
                    failed!(warn);
                } else {
                    failed!(info);
                }
            }
        }
    }
}

pub(crate) enum Outcome<'a> {
    Committed,
    Failed(&'a Failure),
}
