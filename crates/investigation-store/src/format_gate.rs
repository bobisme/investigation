//! The workspace format at the store: every commit refuses a format this
//! program must not write, and a migration publishes a new format under
//! the coordination lock (plan §10.7, W11.15, ADR 0014).
//!
//! The format is read from the marker on disk under the lock, not from the
//! copy read when the store was opened, so a migration that starts while
//! another process holds an open store still stops that process's next
//! commit.

use investigation_core::wire::{Digest, ErrorCode, Failure};
use serde_json::json;

use super::{DurableFs, Id, Store, integrity};
use crate::{
    migration,
    workspace::{FORMAT_VERSION, FormatRefusal, FormatStatus, WorkspaceError, WorkspaceFormat},
};

/// The failure for a write this program refuses because of the format.
pub fn format_failure(refusal: &FormatRefusal, format: &WorkspaceFormat) -> Failure {
    let mut failure = Failure::new(ErrorCode::InvalidInput, refusal.to_string());
    failure.error.expected = Some(json!({
        "reason": refusal.code,
        "writes_up_to_format": FORMAT_VERSION,
    }));
    failure.error.actual = Some(json!({ "format": format }));
    match &refusal.next {
        Some(argv) => failure.with_argv(argv.clone()),
        None => failure,
    }
}

fn marker_failure(error: WorkspaceError) -> Failure {
    integrity(format!("The workspace marker cannot be read: {error}"))
}

impl<F: DurableFs> Store<F> {
    /// Refuse a write when the marker on disk now holds a format this
    /// program must not write. The caller holds the coordination lock.
    pub(super) fn refuse_unwritable_format(&self) -> Result<(), Failure> {
        let format = self.workspace.current_format().map_err(marker_failure)?;
        match migration::write_refusal(&format) {
            Some(refusal) => Err(format_failure(&refusal, &format)),
            None => Ok(()),
        }
    }

    /// The format on disk now and where it stands for this program.
    pub fn format_status(&self) -> Result<(WorkspaceFormat, FormatStatus), Failure> {
        let format = self.workspace.current_format().map_err(marker_failure)?;
        let status = format.status(FORMAT_VERSION, migration::Registry::builtin().latest());
        Ok((format, status))
    }

    /// Replace the marker's format section `expected` with `next`, under
    /// the coordination lock, only while every case head is exactly
    /// `heads` (case ID, sequence, head hash, in case ID order). The store
    /// reopens its workspace afterwards. A migration's own step; nothing
    /// else changes the format.
    pub fn publish_format(
        &mut self,
        expected: &WorkspaceFormat,
        next: WorkspaceFormat,
        heads: &[(Id, u64, Digest)],
    ) -> Result<(), Failure> {
        let _lock = self.coordinate()?;
        let on_disk = self.workspace.current_format().map_err(marker_failure)?;
        if &on_disk != expected {
            let mut failure = Failure::new(
                ErrorCode::RevisionConflict,
                "The workspace format changed while the migration ran. Nothing was changed. Run \
                 migrate again to see where it stands.",
            );
            failure.error.expected = Some(json!({ "format": expected }));
            failure.error.actual = Some(json!({ "format": on_disk }));
            return Err(failure);
        }
        let verified = self.load_verified()?;
        let current: Vec<(Id, u64, Digest)> = verified
            .heads()
            .into_iter()
            .map(|head| {
                (
                    head.case_id,
                    head.snapshot.sequence,
                    head.snapshot.head_hash,
                )
            })
            .collect();
        if current != heads {
            let mut failure = Failure::new(
                ErrorCode::RevisionConflict,
                "The workspace changed since its backup was taken: a case head differs. Nothing \
                 was changed. Run migrate again for a new plan and a new backup.",
            );
            failure.error.actual = Some(json!({ "heads": verified.heads() }));
            return Err(failure);
        }
        self.workspace = self
            .workspace
            .replace_format(expected, next)
            .map_err(marker_failure)?;
        Ok(())
    }
}
