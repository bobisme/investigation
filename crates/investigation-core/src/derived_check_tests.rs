//! Each derived index answers as the walk it replaced did, also after a
//! later transaction and on a clone changed after the index was built.

use serde_json::{Value, json};

use super::check;
use crate::{
    classification::Classifier,
    command::RevisionRef,
    header::{RecordHeader, Revision, SchemaVersion},
    identity::{Id, Kind},
    review_independence, selector_attest,
    state::{AcceptedTransaction, Record, State},
    wire::RecordChange,
};

fn id(kind: Kind, n: u64) -> Id {
    format!("{}_018d87fc-5234-7000-8000-{n:012x}", kind.prefix())
        .parse()
        .unwrap()
}

fn reference(id: Id) -> Value {
    json!({"id": id, "revision": 1})
}

const CASE: u64 = 0;
const OTHER_CASE: u64 = 1;

fn header(id: Id, case_id: Id, sequence: u64) -> RecordHeader {
    RecordHeader {
        id,
        case_id,
        schema_version: SchemaVersion::RecordV0Alpha,
        created_by: "agent:test".parse().unwrap(),
        actor_descriptor_self_reported: None,
        created_at: "2026-11-22T08:12:44.000Z".parse().unwrap(),
        recorded_at: "2026-11-22T08:12:44.000Z".parse().unwrap(),
        recorded_by: "agent:test".parse().unwrap(),
        observed_at: None,
        acquired_at: None,
        acceptance_sequence: sequence,
        revision: Revision(1),
        history: Vec::new(),
        classification: Default::default(),
        provenance: Default::default(),
        extensions: Default::default(),
    }
}

/// Accept `records` as one transaction of the case numbered `case`.
fn add(state: &mut State, case: u64, records: Vec<(Id, Value)>) {
    let case_id = id(Kind::Case, case);
    let sequence = state.sequence() + 1;
    state
        .apply(&AcceptedTransaction {
            case_id,
            sequence,
            created: records
                .iter()
                .map(|(id, _)| RecordChange {
                    id: *id,
                    revision: 1,
                    client_ref: None,
                })
                .collect(),
            updated: Vec::new(),
            records: records
                .into_iter()
                .map(|(id, body)| Record {
                    header: header(id, case_id, sequence),
                    body,
                })
                .collect(),
            warnings: Vec::new(),
        })
        .unwrap();
}

fn change(subject: Id) -> Value {
    json!({"role": "classification", "classification_change": {
        "direction": "raise", "subject_ref": reference(subject), "from": "internal",
        "to": "restricted", "base_level": "internal", "reason": "Codename.", "tags": []
    }})
}

fn attestation(observation: Id, accepted_at_sequence: u64) -> Value {
    json!({"role": "selector_validation", "selector_attestation": {
        "observation_ref": reference(observation),
        "selector": {"schema_version": "inv.selector.v1",
            "artifact_digest": format!("sha256:{}", "ab".repeat(32)),
            "selector": {"type": "whole"}},
        "outcome": "verified", "validator": {"name": "reader", "version": "1"},
        "declared_validator": null, "matches_declared_validator": null,
        "extraction_ref": null, "rationale": "Checked.", "limits": [],
        "attested_by": "agent:test", "accepted_at_sequence": accepted_at_sequence,
        "recorded_validation": "unavailable"
    }})
}

fn review(subject: Id, status: &str) -> Value {
    json!({
        "kind": "blindspot", "status": status, "subject_ref": reference(subject),
        "read_set": [], "verdict": "gaps_identified",
        "context_isolation": "fresh_context_blinded",
        "context": {
            "saw_original_conclusion": {"state": "known", "value": false},
            "gathered_new_evidence": {"state": "unknown"},
            "method_or_source_changes": [], "activity": "Judge the packet.",
            "dependencies": "unknown", "dependence_basis": "Not compared.",
            "work_kind": {"state": "unknown"},
            "prior_involvement": {"state": "known", "value": []},
            "read_other_reviews": {"state": "known", "value": false}
        },
        "audited_snapshot": null
    })
}

/// A case with one subject of each kind, two of each indexed record on it,
/// and records each walk must leave out: a change and an attestation in
/// another case, and a Review that is not submitted.
fn base() -> State {
    let mut state = State::default();
    add(
        &mut state,
        CASE,
        vec![
            (id(Kind::Case, CASE), json!({})),
            (id(Kind::Question, 1), json!({})),
            (id(Kind::Conclusion, 1), json!({})),
            (id(Kind::Observation, 1), json!({})),
            (id(Kind::Observation, 2), json!({})),
        ],
    );
    add(
        &mut state,
        CASE,
        vec![
            (id(Kind::Annotation, 1), change(id(Kind::Question, 1))),
            (
                id(Kind::Annotation, 2),
                attestation(id(Kind::Observation, 1), 2),
            ),
            // A tie on the acceptance sequence keeps annotation ID order.
            (
                id(Kind::Annotation, 3),
                attestation(id(Kind::Observation, 1), 2),
            ),
            (
                id(Kind::Review, 1),
                review(id(Kind::Conclusion, 1), "submitted"),
            ),
            (
                id(Kind::Review, 2),
                review(id(Kind::Conclusion, 1), "draft"),
            ),
        ],
    );
    add(
        &mut state,
        CASE,
        vec![
            (
                id(Kind::Review, 3),
                review(id(Kind::Conclusion, 1), "submitted"),
            ),
            // Records whose ID order is not their acceptance order, so an
            // index that skipped its sort would answer in the wrong order.
            (
                id(Kind::Review, 0),
                review(id(Kind::Conclusion, 1), "submitted"),
            ),
            (id(Kind::Annotation, 0), change(id(Kind::Question, 1))),
            (
                id(Kind::Annotation, 6),
                attestation(id(Kind::Observation, 1), 1),
            ),
        ],
    );
    add(
        &mut state,
        OTHER_CASE,
        vec![
            (id(Kind::Case, OTHER_CASE), json!({})),
            (id(Kind::Annotation, 4), change(id(Kind::Question, 1))),
            (
                id(Kind::Annotation, 5),
                attestation(id(Kind::Observation, 1), 4),
            ),
        ],
    );
    state
}

/// `count` reads one index for one key; `more` adds one indexed record.
/// The index must agree with its walk at every step, see the record added by
/// a later transaction, and on a clone changed after the index was built
/// see the clone's record while the original keeps its own answer.
fn follows(name: &str, count: impl Fn(&State) -> usize, more: impl Fn(u64) -> (Id, Value)) {
    let agree = |state: &State| {
        let checked = check(state);
        assert!(
            checked.mismatches.is_empty(),
            "{name}: {:?}",
            checked.mismatches
        );
        checked
    };
    let mut state = base();
    let checked = agree(&state);
    assert!(
        checked.classification_changes > 0 && checked.attestations > 0 && checked.reviews > 0,
        "{name}: the base state exercises every index: {checked:?}"
    );
    let before = count(&state);
    assert!(before > 0, "{name}");

    add(&mut state, CASE, vec![more(10)]);
    assert_eq!(count(&state), before + 1, "{name}: a later transaction");
    agree(&state);

    let mut scratch = state.clone();
    add(&mut scratch, CASE, vec![more(11)]);
    assert_eq!(count(&scratch), before + 2, "{name}: a changed clone");
    assert_eq!(count(&state), before + 1, "{name}: the original");
    agree(&scratch);
    agree(&state);
}

#[test]
fn the_classification_change_index_answers_as_the_scan_did_and_follows_every_change() {
    let subject = id(Kind::Question, 1);
    follows(
        "classification changes",
        |state| Classifier::new(state).changes(subject).count(),
        |n| (id(Kind::Annotation, n), change(subject)),
    );
}

#[test]
fn the_selector_attestation_index_answers_as_the_scan_did_and_follows_every_change() {
    let observation = id(Kind::Observation, 1);
    follows(
        "selector attestations",
        |state| {
            let record = state.current(observation).unwrap();
            selector_attest::attestations(state, record).count()
        },
        |n| (id(Kind::Annotation, n), attestation(observation, n)),
    );
}

#[test]
fn the_review_index_answers_as_the_scan_did_and_follows_every_change() {
    let subject = id(Kind::Conclusion, 1);
    follows(
        "reviews",
        |state| review_independence::review_ids_of(state, subject).len(),
        |n| (id(Kind::Review, n), review(subject, "submitted")),
    );
}

#[test]
fn each_index_keeps_the_order_and_the_filters_of_its_walk() {
    // The base state's answers, in the order each walk gives them: by
    // acceptance (ties by ID), without other cases' records or drafts.
    let state = base();
    let ids = |found: Vec<RevisionRef>| found.into_iter().map(|r| r.id).collect::<Vec<_>>();
    assert_eq!(
        ids(Classifier::new(&state)
            .changes(id(Kind::Question, 1))
            .map(|(r, _)| r)
            .collect()),
        vec![id(Kind::Annotation, 1), id(Kind::Annotation, 0)]
    );
    let observation = state.current(id(Kind::Observation, 1)).unwrap();
    assert_eq!(
        ids(selector_attest::attestations(&state, observation)
            .map(|(r, _)| r)
            .collect()),
        vec![
            id(Kind::Annotation, 6),
            id(Kind::Annotation, 2),
            id(Kind::Annotation, 3)
        ]
    );
    assert_eq!(
        review_independence::review_ids_of(&state, id(Kind::Conclusion, 1)),
        vec![
            id(Kind::Review, 1),
            id(Kind::Review, 0),
            id(Kind::Review, 3)
        ]
    );
}
