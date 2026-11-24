//! A check that the indexes [`State`] derives from its history answer as the
//! walks over every record they replaced did (bn-9muo, bn-3hr2).
//!
//! Each index is built once per snapshot and dropped whenever the history
//! changes. This module puts each answer beside the walk it replaced:
//! - the classification changes of every subject, read by
//!   [`crate::classification::Classifier`] and so by every read path that
//!   decides disclosure or blob availability;
//! - the selector attestations of every Observation;
//! - the submitted Reviews of every subject (review independence).
//!
//! Tests call it on scenario fixtures, generated bench fixtures and hand-made
//! states, and again after a later transaction and on a changed clone. It is
//! a test aid: no command calls it.

use crate::{
    classification::{self, Classifier},
    identity::Kind,
    review_independence, selector_attest,
    state::State,
};

/// What [`check`] compared: how many items each index returned across every
/// key, and every key where an index and its walk disagreed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Checked {
    /// Records whose ID was used as a key.
    pub keys: usize,
    pub classification_changes: usize,
    pub attestations: usize,
    pub reviews: usize,
    pub mismatches: Vec<String>,
}

/// Compare every derived index of `state` with the walk it replaced, for
/// every current record as the key.
pub fn check(state: &State) -> Checked {
    let mut checked = Checked::default();
    let classifier = Classifier::new(state);
    for record in state.records() {
        let id = record.header.id;
        checked.keys += 1;

        let indexed: Vec<_> = classifier
            .changes(id)
            .map(|(change_ref, change)| (change_ref, change.clone()))
            .collect();
        let scanned = classification::changes_by_scan(state, id);
        checked.classification_changes += indexed.len();
        if indexed != scanned {
            checked.mismatches.push(format!(
                "classification changes of {id}: index {indexed:?}, scan {scanned:?}"
            ));
        }

        if id.kind() == Kind::Observation {
            let indexed: Vec<_> = selector_attest::attestations(state, record).collect();
            let scanned = selector_attest::attestations_by_scan(state, record);
            checked.attestations += indexed.len();
            if indexed != scanned {
                checked.mismatches.push(format!(
                    "selector attestations of {id}: index {indexed:?}, scan {scanned:?}"
                ));
            }
        }

        let indexed = review_independence::review_ids_of(state, id);
        let scanned = review_independence::reviews_of_by_scan(state, id);
        checked.reviews += indexed.len();
        if indexed != scanned {
            checked.mismatches.push(format!(
                "reviews of {id}: index {indexed:?}, scan {scanned:?}"
            ));
        }
    }
    checked
}

#[cfg(test)]
#[path = "derived_check_tests.rs"]
mod tests;
