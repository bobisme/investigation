//! Cross-case check of lesson consultations at commit (§8.6, §16.1).
//!
//! One case's validation cannot read another case, so the core records a
//! consultation as a pointer and the store checks it here, under the commit
//! lock, against the verified source case: the lesson must carry a current
//! admission with the same content digest. An unadmitted lesson therefore
//! cannot be recorded as read from another case.

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::{
    identity::Id,
    lessons::{consultation_body, verify_pointer},
    state::AcceptedTransaction,
    wire::Failure,
};

use crate::journal::CaseReplay;

/// The cases whose lessons the consultations created by `accepted` read.
pub(super) fn source_cases(accepted: &AcceptedTransaction) -> BTreeSet<Id> {
    accepted
        .records
        .iter()
        .filter(|record| record.header.revision.0 == 1)
        .filter_map(consultation_body)
        .map(|body| body.lesson.source_case_id)
        .collect()
}

/// Check every consultation created by `accepted` against `others`, the
/// verified cases other than the target. `others` must hold every case
/// [`source_cases`] names.
pub(super) fn verify_consultations(
    others: &BTreeMap<Id, CaseReplay>,
    accepted: &AcceptedTransaction,
) -> Result<(), Failure> {
    for record in accepted
        .records
        .iter()
        .filter(|record| record.header.revision.0 == 1)
    {
        let Some(body) = consultation_body(record) else {
            continue;
        };
        let source = others
            .get(&body.lesson.source_case_id)
            .map(|replay| &replay.state);
        verify_pointer(source, &body.lesson)?;
    }
    Ok(())
}
