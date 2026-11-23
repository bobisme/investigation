//! Full-text navigation over the disposable index.
//!
//! The index only narrows the search. Callers receive the verified journal
//! replay read under the same lock and must re-check every candidate against
//! canonical records, classification included, before showing anything.

use std::collections::BTreeMap;

use investigation_core::{identity::Id, wire::Failure};

use super::Store;
use crate::{
    durable::DurableFs,
    journal::CaseReplay,
    projection::{Projection, SearchCandidate},
};

/// Index candidates together with the verified replay they were read against.
pub struct SearchSnapshot {
    /// Verified cases in scope: the selected case, or every case.
    pub cases: BTreeMap<Id, CaseReplay>,
    /// Matching fields, best first. Unverified pointers into `cases`.
    pub candidates: Vec<SearchCandidate>,
    /// More index rows matched than were read.
    pub truncated: bool,
}

impl<F: DurableFs> Store<F> {
    /// Bring the index up to the verified journal, then read candidates for
    /// `terms` in one case, or in every case when `case_id` is `None`.
    pub fn search(
        &mut self,
        terms: &[String],
        case_id: Option<Id>,
    ) -> Result<SearchSnapshot, Failure> {
        let _lock = self.coordinate()?;
        let mut workspace = self.load_verified()?;
        self.refresh_index(&workspace)?;
        let (candidates, truncated) =
            Projection::open(&self.index_path(), self.workspace.id())?.search(terms, case_id)?;
        let cases = match case_id {
            Some(id) => {
                let replay = workspace
                    .cases
                    .remove(&id)
                    .ok_or_else(|| super::missing_case(id))?;
                BTreeMap::from([(id, replay)])
            }
            None => workspace.cases,
        };
        Ok(SearchSnapshot {
            cases,
            candidates,
            truncated,
        })
    }
}
