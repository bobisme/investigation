//! The one disk read of an export: a case pinned to one verified head.

use investigation_core::{
    identity::Id,
    state::State,
    wire::{Digest, ErrorCode, Failure, Snapshot},
};
use investigation_store::{Store, durable::DurableFs};
use serde_json::json;

use crate::{CaseSnapshot, VerifiedSnapshot};

/// Which head to export: the current one, or an earlier verified one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pin {
    /// A verified case journal sequence; `None` is the current head.
    pub sequence: Option<u64>,
    /// The head hash required at `sequence`.
    pub head_hash: Option<Digest>,
}

/// A case at one verified head, with today's state beside it when the head
/// is an earlier one. Holds no file handle or path.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSnapshot {
    case_id: Id,
    pinned: VerifiedSnapshot,
    latest: Snapshot,
    today: Option<State>,
}

impl ExportSnapshot {
    /// Verify the case journal under the commit lock, then pin it. The lock
    /// is released before this returns, so planning never blocks a commit.
    pub fn read<F: DurableFs>(
        store: &mut Store<F>,
        case_id: Id,
        pin: &Pin,
    ) -> Result<Self, Failure> {
        let workspace_id = store.workspace().id();
        let head = store.case(case_id)?;
        let latest = head.snapshot.clone();
        let Some(sequence) = pin.sequence else {
            return Ok(Self {
                case_id,
                pinned: VerifiedSnapshot::single_case(workspace_id, case_id, head),
                latest,
                today: None,
            });
        };
        let head_hash = match &pin.head_hash {
            Some(hash) => hash.clone(),
            None => head
                .state
                .verified_snapshot(sequence)
                .cloned()
                .ok_or_else(|| {
                    let mut failure = Failure::new(
                        ErrorCode::UnknownReference,
                        "The sequence is not a verified snapshot of this case journal.",
                    )
                    .at("--snapshot");
                    failure.error.expected = Some(json!({"latest": latest}));
                    failure
                })?,
        };
        let snapshot = Snapshot {
            sequence,
            head_hash,
        };
        if snapshot == latest {
            return Ok(Self {
                case_id,
                pinned: VerifiedSnapshot::single_case(workspace_id, case_id, head),
                latest,
                today: None,
            });
        }
        let pinned = head.at_snapshot(&snapshot)?;
        Ok(Self {
            case_id,
            pinned: VerifiedSnapshot::single_case(workspace_id, case_id, pinned),
            latest,
            today: Some(head.state),
        })
    }

    pub const fn case_id(&self) -> Id {
        self.case_id
    }

    /// A snapshot holding only the pinned case.
    pub const fn pinned(&self) -> &VerifiedSnapshot {
        &self.pinned
    }

    /// The pinned case.
    pub fn case(&self) -> &CaseSnapshot {
        self.pinned
            .case(self.case_id)
            .expect("an export snapshot holds its case")
    }

    /// The case's current head when the export was read.
    pub const fn latest(&self) -> &Snapshot {
        &self.latest
    }

    /// Today's state when the pinned head is an earlier one.
    pub const fn today(&self) -> Option<&State> {
        self.today.as_ref()
    }

    /// Whether the export pins an earlier head than the current one.
    pub const fn historical(&self) -> bool {
        self.today.is_some()
    }
}
