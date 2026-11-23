use std::collections::BTreeSet;

use serde_json::json;

use super::*;
use crate::{
    catalog::{Status, standard_catalog},
    header::{RecordHeader, Revision, SchemaVersion},
    method_pack::POLICY_GATES,
    state::AcceptedTransaction,
    wire::RecordChange,
};

#[test]
fn gates_are_the_pack_policy_gates_and_none_records_evidence() {
    let gates: BTreeSet<&str> = Gate::ALL.iter().map(|gate| gate.operation()).collect();
    let declared: BTreeSet<&str> = POLICY_GATES.iter().copied().collect();
    assert_eq!(gates, declared);
    for gate in Gate::ALL {
        assert_eq!(gate_for(gate.operation()), Some(gate));
        assert_eq!(serde_json::to_value(gate).unwrap(), json!(gate.operation()));
    }
    let catalog = standard_catalog();
    let mut recording = 0;
    for operation in catalog.operations() {
        let info = operation.info();
        if matches!(info.stage, "start" | "observe" | "collect" | "record")
            || info.name.starts_with("observation.")
            || info.name.starts_with("source.")
            || info.name.starts_with("artifact.")
            || info.name.starts_with("run.")
        {
            recording += 1;
            assert_eq!(gate_for(info.name), None, "{} records evidence", info.name);
        }
    }
    assert!(recording > 0);
    for gate in Gate::ALL {
        let operation = catalog
            .get(gate.operation())
            .expect("gate is in the catalog");
        assert_eq!(operation.status(), Status::Active);
    }
}

/// Policy is consulted only from gate handlers. Recording, ingest and capture
/// modules cannot reach it, so no pack input can refuse preservation.
#[test]
fn only_promotion_and_closure_handlers_consult_pack_policy() {
    let consulting = [
        ("review_ops.rs", include_str!("../review_ops.rs")),
        ("decision_ops.rs", include_str!("../decision_ops.rs")),
        ("case_ops.rs", include_str!("../case_ops.rs")),
    ];
    for (name, source) in consulting {
        assert!(source.contains("pack_policy::consult"), "{name}");
    }
    for (name, source) in [
        ("observation_ops.rs", include_str!("../observation_ops.rs")),
        ("source_ops.rs", include_str!("../source_ops.rs")),
        ("measurement_ops.rs", include_str!("../measurement_ops.rs")),
        ("evaluate_ops.rs", include_str!("../evaluate_ops.rs")),
        ("explain_ops.rs", include_str!("../explain_ops.rs")),
        (
            "retrospective_ops.rs",
            include_str!("../retrospective_ops.rs"),
        ),
        (
            "classification_ops.rs",
            include_str!("../classification_ops.rs"),
        ),
        ("producer_run.rs", include_str!("../producer_run.rs")),
        ("method_pack_ops.rs", include_str!("../method_pack_ops.rs")),
    ] {
        assert!(
            !source.contains("pack_policy"),
            "{name} must not consult pack policy"
        );
    }
}

fn id(prefix: &str, n: u64) -> Id {
    format!("{prefix}_018d87fc-5234-7000-8000-{n:012x}")
        .parse()
        .unwrap()
}

fn header(id: Id, case: Id, sequence: u64, actor: &str) -> RecordHeader {
    RecordHeader {
        id,
        case_id: case,
        schema_version: SchemaVersion::RecordV0Alpha,
        created_by: actor.parse().unwrap(),
        actor_descriptor_self_reported: None,
        created_at: "2026-11-22T08:12:44.000Z".parse().unwrap(),
        recorded_at: "2026-11-22T08:12:44.000Z".parse().unwrap(),
        recorded_by: actor.parse().unwrap(),
        observed_at: None,
        acquired_at: None,
        acceptance_sequence: sequence,
        revision: Revision(1),
        history: vec![],
        classification: Default::default(),
        provenance: Default::default(),
        extensions: Default::default(),
    }
}

/// Append a revision of `id` (creating it at revision 1) recorded by `actor`.
fn write(state: &mut State, id: Id, case: Id, actor: &str, body: Value) -> RevisionRef {
    let sequence = state.sequence() + 1;
    let record = match state.current(id) {
        Some(current) => {
            let mut record = current.clone();
            record.header.history.push(record.header.revision);
            record.header.revision.0 += 1;
            record.header.acceptance_sequence = sequence;
            record.header.recorded_by = actor.parse().unwrap();
            record.body = body;
            record
        }
        None => Record {
            header: header(id, case, sequence, actor),
            body,
        },
    };
    let change = RecordChange {
        client_ref: None,
        id,
        revision: record.header.revision.0,
    };
    let reference = record.reference();
    let (created, updated) = if reference.revision == 1 {
        (vec![change], vec![])
    } else {
        (vec![], vec![change])
    };
    state
        .apply(&AcceptedTransaction {
            case_id: case,
            sequence,
            records: vec![record],
            created,
            updated,
            warnings: vec![],
        })
        .unwrap();
    reference
}

fn decision(question: RevisionRef, reopen: &str, lifecycle: &str) -> Value {
    json!({
        "question_ref": question, "choice": {"kind": "defer", "reason": "Await data"},
        "compared_alternatives": [], "criteria_set_version": "v", "comparison_refs": [],
        "hard_conditions": [], "evidence_refs": [], "assumption_refs": [], "tradeoffs": [],
        "consequences": ["Delay"], "reopen_trigger": reopen, "expected_outcome": null,
        "check_when": null, "reversal_conditions": [], "lifecycle": lifecycle,
        "review_ref": null, "supersedes": null, "superseded_by": null, "change_reason": null
    })
}

fn blindspot(subject: RevisionRef, read_set: Value, status: &str) -> Value {
    json!({"kind": "blindspot", "status": status, "subject_ref": subject, "read_set": read_set})
}

#[test]
fn a_blindspot_review_is_current_only_for_the_same_basis_and_a_current_read_set() {
    let case = id("case", 1);
    let mut state = State::default();
    write(&mut state, case, case, "agent:a", json!({}));
    let question = write(&mut state, id("q", 2), case, "agent:a", json!({}));
    let decision_id = id("dec", 3);
    let proposed = write(
        &mut state,
        decision_id,
        case,
        "agent:a",
        decision(question, "A reading disagrees.", "proposed"),
    );
    let fresh = write(
        &mut state,
        id("rev", 4),
        case,
        "agent:b",
        blindspot(proposed, json!([question]), "submitted"),
    );
    write(
        &mut state,
        id("rev", 5),
        case,
        "agent:b",
        blindspot(proposed, json!([]), "superseded"),
    );
    write(
        &mut state,
        id("rev", 6),
        case,
        "agent:b",
        blindspot(question, json!([]), "submitted"),
    );
    let current = |state: &State| {
        current_blindspot_reviews(state, state.current(decision_id).unwrap()).unwrap()
    };
    assert_eq!(current(&state), [fresh]);

    // A lifecycle-only revision keeps the review current.
    write(
        &mut state,
        decision_id,
        case,
        "agent:c",
        decision(question, "A reading disagrees.", "accepted"),
    );
    assert_eq!(current(&state), [fresh]);
    // A changed read-set member makes it stale.
    write(
        &mut state,
        question.id,
        case,
        "agent:a",
        json!({"wording": "Changed"}),
    );
    assert!(current(&state).is_empty());
    // A changed substantive basis makes it stale, even with a fresh read-set.
    let rereviewed = write(
        &mut state,
        id("rev", 7),
        case,
        "agent:b",
        blindspot(proposed, json!([]), "submitted"),
    );
    assert_eq!(current(&state), [rereviewed]);
    write(
        &mut state,
        decision_id,
        case,
        "agent:a",
        decision(question, "Any reading disagrees.", "proposed"),
    );
    assert!(current(&state).is_empty());
}

#[test]
fn content_authors_are_whoever_recorded_the_current_substantive_basis() {
    let case = id("case", 1);
    let mut state = State::default();
    write(&mut state, case, case, "agent:a", json!({}));
    let question = write(&mut state, id("q", 2), case, "agent:a", json!({}));
    let decision_id = id("dec", 3);
    write(
        &mut state,
        decision_id,
        case,
        "agent:proposer",
        decision(question, "A reading disagrees.", "proposed"),
    );
    // A lifecycle-only revision does not make its actor an author.
    write(
        &mut state,
        decision_id,
        case,
        "agent:decider",
        decision(question, "A reading disagrees.", "accepted"),
    );
    let authors = |state: &State| -> Vec<String> {
        content_authors(state, state.current(decision_id).unwrap())
            .unwrap()
            .iter()
            .map(|actor| actor.to_string())
            .collect()
    };
    assert_eq!(authors(&state), ["agent:proposer"]);
    write(
        &mut state,
        decision_id,
        case,
        "agent:editor",
        decision(question, "Any reading disagrees.", "proposed"),
    );
    assert_eq!(authors(&state), ["agent:editor", "agent:proposer"]);
}

#[test]
fn required_fields_need_a_meaningful_value() {
    let body = json!({
        "statement": "A claim.", "blank": "  ", "none": null, "empty": [], "map": {},
        "unknown": {"state": "unknown"}, "known_blank": {"state": "known", "value": ""},
        "known": {"state": "known", "value": 3}, "nested": {"limit": {"value": false}},
        "list": ["x"]
    });
    let subject = Subject {
        body: &body,
        record: None,
    };
    for present in [
        "statement",
        "known",
        "nested.limit",
        "nested.limit.value",
        "list",
    ] {
        assert!(field_present(&subject, present), "{present}");
    }
    for absent in [
        "missing",
        "blank",
        "none",
        "empty",
        "map",
        "unknown",
        "known_blank",
        "nested.other",
        "statement.deeper",
        "extensions.pack.field",
    ] {
        assert!(!field_present(&subject, absent), "{absent}");
    }

    // Extension paths fall back to the record header's extension map.
    let case = id("case", 1);
    let mut record = Record {
        header: header(id("concl", 2), case, 1, "agent:a"),
        body: body.clone(),
    };
    record.header.extensions.insert(
        "fixture-pack".into(),
        json!({"reproduction": {"k": 3, "n": 5}}),
    );
    let subject = Subject {
        body: &body,
        record: Some(&record),
    };
    assert!(field_present(
        &subject,
        "extensions.fixture-pack.reproduction"
    ));
    assert!(!field_present(&subject, "extensions.fixture-pack.other"));
}
