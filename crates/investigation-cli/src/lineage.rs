//! Bounded lineage projections for `show`.
//!
//! An Artifact or Observation shows where its bytes come from: its role, the
//! original acquisitions it traces to, the derivation steps in between and the
//! other Artifacts that share those origins. Artifacts that share an origin are
//! views of one acquisition, not independent confirmations. When an original is
//! no longer retained, the lineage reports that the derivation can no longer be
//! re-inspected. Records citing a derivative already see that loss: their
//! review read set follows `transforms[].input` to the original.

use investigation_core::{
    command::RevisionRef,
    derivation::{self, Lineage},
    identity::Kind,
    state::{Record, State},
};
use investigation_store::Store;
use serde_json::{Value, json};

/// Lists in a lineage view are bounded; the omitted count is reported.
const MAX_LISTED: usize = 16;

fn artifact_of(state: &State, reference: RevisionRef) -> Option<RevisionRef> {
    match reference.id.kind() {
        Kind::Artifact => Some(reference),
        Kind::Observation => {
            serde_json::from_value(state.revision(reference)?.body["basis"]["artifact_ref"].clone())
                .ok()
        }
        _ => None,
    }
}

/// Current revisions of the originals a lineage reaches that are no longer
/// retained or fail verification.
fn lost_origins(
    store: &Store,
    state: &State,
    classification: &State,
    lineage: &Lineage,
) -> Vec<RevisionRef> {
    let current: Vec<RevisionRef> = lineage
        .origins
        .iter()
        .filter_map(|origin| state.current(origin.artifact_ref.id))
        .map(Record::reference)
        .collect();
    store.unavailable_artifacts(state, classification, &current)
}

fn bounded(items: Vec<Value>) -> (Vec<Value>, usize) {
    let omitted = items.len().saturating_sub(MAX_LISTED);
    (items.into_iter().take(MAX_LISTED).collect(), omitted)
}

/// Lineage of an Artifact, or of the Artifact an Observation cites.
/// `classification` governs disclosure: the current head, also when `state`
/// is an earlier snapshot.
pub fn lineage_view(
    store: &Store,
    state: &State,
    classification: &State,
    record: &Record,
) -> Option<Value> {
    let artifact = artifact_of(state, record.reference())?;
    let lineage = derivation::lineage(state, artifact)?;
    let lost = lost_origins(store, state, classification, &lineage);
    let (steps, omitted_steps) = bounded(lineage.steps.iter().map(|step| json!(step)).collect());
    let (sharing, omitted_sharing) = bounded(
        derivation::sharing_origin(state, &lineage)
            .into_iter()
            .filter(|other| other.id != record.header.id)
            .map(|other| json!(other))
            .collect(),
    );
    Some(json!({
        "artifact_ref": lineage.artifact_ref,
        "role": lineage.role,
        "origins": lineage.origins,
        "depth": lineage.depth,
        "steps": steps,
        "omitted_steps": omitted_steps,
        "unresolved_inputs": lineage.unresolved,
        "truncated": lineage.truncated,
        "sharing_origin": sharing,
        "omitted_sharing_origin": omitted_sharing,
        "inspectability": inspectability_state(&lost),
    }))
}

fn inspectability_state(lost: &[RevisionRef]) -> Value {
    if lost.is_empty() {
        json!({"state":"inspectable"})
    } else {
        json!({
            "state":"original_unavailable",
            "reason":"An original acquisition this evidence derives from is no longer retained or fails verification. The derivation cannot be re-inspected against it.",
            "unavailable_originals":lost,
        })
    }
}
