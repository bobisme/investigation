//! Reads pinned to an exact verified snapshot of one case journal.
//!
//! A pinned replay is rebuilt from the verified events up to the snapshot, so a
//! read that continues an earlier one sees exactly the state the earlier read
//! saw, however many transactions were committed in between.

use investigation_core::{
    gap_page::{GapCursor, GapPage, GapPageRequest, cursor_failure, page_gaps},
    gap_rules::enumerate_gaps,
    identity::Id,
    state::State,
    wire::{ErrorCode, Failure, Snapshot},
};
use serde_json::json;

use crate::{durable::DurableFs, journal::CaseReplay, store::Store};

impl CaseReplay {
    /// Rebuild the case as it was at `snapshot`, which must be a verified head
    /// of this replay's chain (same sequence and same hash).
    pub fn at_snapshot(&self, snapshot: &Snapshot) -> Result<CaseReplay, Failure> {
        if self.state.verified_snapshot(snapshot.sequence) != Some(&snapshot.head_hash) {
            let mut failure = Failure::new(
                ErrorCode::UnknownReference,
                "The snapshot is not a verified head of this case journal.",
            )
            .at("/snapshot");
            failure.error.actual = Some(json!(snapshot));
            failure.error.expected = Some(json!({"latest": self.snapshot}));
            return Err(failure);
        }
        if snapshot == &self.snapshot {
            return Ok(self.clone());
        }
        let integrity = |message: String| Failure::new(ErrorCode::IntegrityFailure, message);
        let genesis = self
            .state
            .verified_snapshot(0)
            .ok_or_else(|| integrity("The replay has no verified genesis snapshot.".into()))?;
        let mut pinned = CaseReplay {
            state: State::default(),
            snapshot: Snapshot {
                sequence: 0,
                head_hash: genesis.clone(),
            },
            events: Vec::new(),
        };
        pinned
            .state
            .note_verified_snapshot(pinned.snapshot.clone())?;
        for verified in self
            .events
            .iter()
            .take_while(|verified| verified.event.sequence <= snapshot.sequence)
        {
            pinned.state.apply(&verified.event.accepted)?;
            pinned.snapshot = Snapshot {
                sequence: verified.event.sequence,
                head_hash: verified.hash.clone(),
            };
            pinned
                .state
                .note_verified_snapshot(pinned.snapshot.clone())?;
            pinned.events.push(verified.clone());
        }
        if &pinned.snapshot != snapshot {
            return Err(integrity(format!(
                "Rebuilding sequence {} reached sequence {} instead.",
                snapshot.sequence, pinned.snapshot.sequence
            )));
        }
        Ok(pinned)
    }
}

/// Read one page of a case's gap inventory. The first page reads the current
/// head; a continuation reads the snapshot its cursor is bound to, and reports
/// the newer head when one exists.
pub fn read_gap_page<F: DurableFs>(
    store: &mut Store<F>,
    case_id: Id,
    request: &GapPageRequest,
) -> Result<GapPage, Failure> {
    request.filter.validate()?;
    let cursor = request
        .cursor
        .as_deref()
        .map(GapCursor::decode)
        .transpose()?;
    if let Some(cursor) = &cursor {
        cursor.check_request(case_id, &request.filter, request.ordering)?;
    }
    let head = store.case(case_id)?;
    let pinned = match &cursor {
        Some(cursor) if cursor.snapshot != head.snapshot => {
            Some(head.at_snapshot(&cursor.snapshot).map_err(|failure| {
                if failure.error.code == ErrorCode::UnknownReference {
                    cursor_failure(
                        "The cursor's snapshot is not a verified head of this case journal.",
                        Some(json!({"latest": head.snapshot})),
                        Some(json!(cursor.snapshot)),
                    )
                } else {
                    failure
                }
            })?)
        }
        _ => None,
    };
    let replay = pinned.as_ref().unwrap_or(&head);
    let inventory = enumerate_gaps(&replay.state, &replay.snapshot, case_id)?;
    let mut page = page_gaps(
        &inventory,
        &request.filter,
        request.ordering,
        request.limit,
        cursor.as_ref(),
    )?;
    if head.snapshot != page.snapshot {
        page.newer_head = Some(head.snapshot.clone());
    }
    Ok(page)
}
