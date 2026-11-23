//! The planner's only input from disk: one verified, immutable workspace snapshot.

use std::collections::BTreeMap;

use investigation_core::{
    identity::Id,
    inherited_trust::SessionLedger,
    state::State,
    wire::{Digest, Snapshot},
};
use investigation_store::{Store, durable::DurableFs, journal::CaseReplay};

use crate::RenderError;

/// Every case's verified state at its journal head, read in one pass.
///
/// The value holds no file handle or path, so planning from it cannot reach
/// the filesystem. It can only be built from a verified store read.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedSnapshot {
    workspace_id: Id,
    cases: BTreeMap<Id, CaseSnapshot>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaseSnapshot {
    case_id: Id,
    head: Snapshot,
    state: State,
    /// Evidence reads stamped on the case's journal events, for inherited trust.
    sessions: SessionLedger,
    /// The verified hash of each journal event, by sequence from 1: the head
    /// the case had at that sequence.
    chain: Vec<Digest>,
}

impl VerifiedSnapshot {
    /// Verify the journal and keep the replayed state. The store's commit lock
    /// is held only for the duration of this call.
    pub fn read<F: DurableFs>(store: &mut Store<F>) -> Result<Self, RenderError> {
        let workspace_id = store.workspace().id();
        let cases = store
            .cases()
            .map_err(RenderError::Snapshot)?
            .into_iter()
            .map(|(case_id, replay)| (case_id, CaseSnapshot::from_replay(case_id, replay)))
            .collect();
        Ok(Self {
            workspace_id,
            cases,
        })
    }

    /// A snapshot holding one case: `replay`, which the caller read from a
    /// verified store and may have pinned to an earlier verified head.
    pub(crate) fn single_case(workspace_id: Id, case_id: Id, replay: CaseReplay) -> Self {
        Self {
            workspace_id,
            cases: BTreeMap::from([(case_id, CaseSnapshot::from_replay(case_id, replay))]),
        }
    }

    pub const fn workspace_id(&self) -> Id {
        self.workspace_id
    }

    /// Cases in ID order.
    pub fn cases(&self) -> impl Iterator<Item = &CaseSnapshot> {
        self.cases.values()
    }

    pub fn case(&self, case_id: Id) -> Option<&CaseSnapshot> {
        self.cases.get(&case_id)
    }
}

impl CaseSnapshot {
    fn from_replay(case_id: Id, replay: CaseReplay) -> Self {
        Self {
            case_id,
            sessions: replay.session_ledger(),
            chain: replay
                .events
                .iter()
                .map(|event| event.hash.clone())
                .collect(),
            head: replay.snapshot,
            state: replay.state,
        }
    }

    pub const fn case_id(&self) -> Id {
        self.case_id
    }

    /// The verified journal head this state was replayed to.
    pub const fn head(&self) -> &Snapshot {
        &self.head
    }

    pub const fn state(&self) -> &State {
        &self.state
    }

    /// Session reads by acceptance sequence; a sequence without one declared
    /// no session.
    pub const fn sessions(&self) -> &SessionLedger {
        &self.sessions
    }

    /// The head this case had at `sequence`, from 1 up to its current head;
    /// `None` outside that range. A snapshot equal to it lies on this case's
    /// verified chain.
    pub fn head_at(&self, sequence: u64) -> Option<Snapshot> {
        let offset = usize::try_from(sequence.checked_sub(1)?).ok()?;
        self.chain.get(offset).map(|head_hash| Snapshot {
            sequence,
            head_hash: head_hash.clone(),
        })
    }
}

#[cfg(test)]
impl VerifiedSnapshot {
    /// Unit tests build snapshots from synthetic states; production code can
    /// only read verified ones from a store.
    pub(crate) fn from_states(workspace_id: Id, cases: Vec<(Snapshot, State)>) -> Self {
        let cases = cases
            .into_iter()
            .map(|(head, state)| {
                let case_id = state
                    .records()
                    .find(|record| {
                        record.header.id.kind() == investigation_core::identity::Kind::Case
                    })
                    .expect("a case record")
                    .header
                    .id;
                (
                    case_id,
                    CaseSnapshot {
                        case_id,
                        head,
                        state,
                        sessions: SessionLedger::new(),
                        chain: Vec::new(),
                    },
                )
            })
            .collect();
        Self {
            workspace_id,
            cases,
        }
    }
}
