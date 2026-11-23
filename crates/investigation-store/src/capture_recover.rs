//! Recovery reads of capture handles (`capture.recover`).
//!
//! Recovery reports, for each declared handle, what the recorder durably
//! accepted, so a producer that crashed or lost a response can continue its
//! recording at the right position or settle it. It is a read:
//!
//! - It takes no handle lock and creates no lock file, seal, item, key entry or
//!   directory. It removes nothing: no staged blob, no temporary.
//! - It refreshes no index. The original receipt of a committed finalization
//!   is returned as an identical retry returns it, `already_committed`, and
//!   reports the index as it finds it, like `receipt` does.
//! - It runs, schedules and repeats nothing. A handle with no record of an
//!   outcome is reported as unsettled, never as "did not happen".
//!
//! The order of the reads keeps each report consistent: the journal first, then
//! the seal, then the items. A sealed handle accepts no item, so its items are
//! final once the seal is read. An open handle's items are a durable prefix; an
//! item accepted after the read is reported by the next one. Once staging is
//! retired, the finalized manifest Artifact supplies the items.

use super::{
    finalize::{CaptureSeal, FinalizedCapture, finalization_of},
    items::StagedItem,
    *,
};

/// What a handle's staging holds, as recovery found it.
#[derive(Debug, Clone, PartialEq)]
pub enum RecoveredStaging {
    /// Staging holds the handle's binding.
    Present {
        /// Every staged item in position order: a durable prefix.
        items: Vec<StagedItem>,
        /// The seal, once finalize has sealed the handle.
        seal: Option<CaptureSeal>,
    },
    /// Declared, but a crash left the staging unallocated. Resending the
    /// identical begin allocates it.
    Unallocated,
    /// Finalized, and the staging directory is gone. The manifest Artifact
    /// holds every item; these are its items, as they were staged.
    Retired { items: Vec<StagedItem> },
}

/// One declared handle and what is durable about it.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveredHandle {
    pub declared: DeclaredHandle,
    pub staging: RecoveredStaging,
    /// The committed finalization, with its original receipt.
    pub finalized: Option<FinalizedCapture>,
}

impl<F: DurableFs> Store<F> {
    /// Report one handle in any state, or with `None` every handle that is not
    /// finalized, in handle order. Never writes.
    pub fn capture_recovery(
        &mut self,
        handle: Option<Id>,
    ) -> Result<Vec<RecoveredHandle>, Failure> {
        let replay = {
            let _lock = self.coordinate()?;
            self.load_verified()?
        };
        let mut declared = self.declared_handles(&replay)?;
        if let Some(handle) = handle {
            declared.retain(|declared| declared.binding.handle == handle);
            if declared.is_empty() {
                return Err(Failure::new(
                    ErrorCode::UnknownReference,
                    format!(
                        "No capture handle {handle} is declared in this workspace. Use the handle from the begin receipt, or recover without a handle to list unsettled ones."
                    ),
                )
                .at("/handle"));
            }
        }
        let mut recovered = Vec::with_capacity(declared.len());
        for declared in declared {
            let case = replay
                .cases
                .get(&declared.case_id)
                .ok_or_else(|| integrity("A declared handle names a case that is missing."))?;
            let finalizing = case.events.iter().find_map(|verified| {
                finalization_of(&verified.event, declared.run_id).map(|section| (verified, section))
            });
            if handle.is_none() && finalizing.is_some() {
                continue;
            }
            let finalized = finalizing.map(|(verified, section)| FinalizedCapture {
                receipt: self.recorded_receipt(&replay, verified),
                case_id: declared.case_id,
                run_id: declared.run_id,
                handle: declared.binding.handle,
                section,
            });
            let staging = match declared.staging {
                StagingState::Missing if finalized.is_some() => RecoveredStaging::Retired {
                    items: self
                        .finalized_manifest(&case.state, declared.run_id, declared.binding.handle)?
                        .ok_or_else(|| integrity("The finalized Run has no manifest."))?
                        .items,
                },
                StagingState::Missing => RecoveredStaging::Unallocated,
                StagingState::Present => {
                    let resolved = self.resolve_declared(&declared, &case.state)?;
                    let seal = self.capture_seal(&resolved)?;
                    let items = self.staged_items(&resolved)?;
                    if let Some(seal) = &seal
                        && seal.sealed_position != items.last().map(|item| item.position)
                    {
                        return Err(integrity(
                            "The capture seal disagrees with the staged items.",
                        ));
                    }
                    RecoveredStaging::Present { items, seal }
                }
            };
            recovered.push(RecoveredHandle {
                declared,
                staging,
                finalized,
            });
        }
        Ok(recovered)
    }

    /// The original receipt of a committed event, as an identical retry
    /// returns it (`already_committed`), with the index health as it is now.
    /// Refreshes nothing.
    fn recorded_receipt(&self, replay: &WorkspaceReplay, verified: &VerifiedEvent) -> Receipt {
        let mut receipt = receipt_for(verified).as_retry();
        let expected: Vec<_> = replay
            .heads()
            .into_iter()
            .map(|head| (head.case_id, head.snapshot))
            .collect();
        receipt.projection.index = if Projection::inspect(&self.index_path(), self.workspace.id())
            .as_ref()
            == Ok(&expected)
        {
            ProjectionStatus::Current
        } else {
            ProjectionStatus::Degraded
        };
        receipt
    }
}

#[cfg(test)]
#[path = "capture_recover_tests.rs"]
mod tests;
