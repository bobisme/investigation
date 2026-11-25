use std::collections::BTreeSet;

use serde_json::json;

use super::*;
use crate::{
    catalog::{Status, standard_catalog},
    header::{RecordHeader, Revision, SchemaVersion},
    inherited_trust::SessionRead,
    method_pack::{GUARDED_OPERATIONS, PackErrorKind, load},
    state::AcceptedTransaction,
    wire::RecordChange,
};

fn pack(operation: &str, requirement: &str) -> String {
    format!(
        r#"schema_version = "inv.method-pack.v1"
id = "guarded"
version = "1.0.0"
title = "Guarded"
summary = "Fixture pack with one guardrail."
modes = ["explain"]

[[guardrails]]
name = "g"
operation = "{operation}"
requirement = "{requirement}"
rationale = "r"
"#
    )
}

const REQUIREMENTS: [&str; 3] = ["distinct_actor", "distinct_context", "human_confirmation"];

/// Exactly the seven named operations can be guarded. A pack that declares a
/// guardrail on any other catalog operation is rejected when it loads, so
/// recording, ingest and capture can never be refused by a guardrail.
#[test]
fn only_the_seven_named_operations_can_be_guarded() {
    let named: BTreeSet<&str> = GuardedOperation::ALL
        .iter()
        .map(|op| op.operation())
        .collect();
    assert_eq!(
        named,
        BTreeSet::from([
            "conclusion.validate",
            "decision.accept",
            "source.withdraw",
            "artifact.withdraw",
            "artifact.expire",
            "observation.retract",
            "classification.downgrade",
        ])
    );
    assert_eq!(
        named,
        GUARDED_OPERATIONS.iter().copied().collect::<BTreeSet<_>>()
    );
    for op in GuardedOperation::ALL {
        assert_eq!(GuardedOperation::from_operation(op.operation()), Some(op));
        assert_eq!(serde_json::to_value(op).unwrap(), json!(op.operation()));
    }
    let catalog = standard_catalog();
    let mut refused = 0;
    for operation in catalog.operations() {
        let name = operation.info().name;
        for requirement in REQUIREMENTS {
            let loaded = load(pack(name, requirement).as_bytes());
            if named.contains(name) {
                assert_eq!(operation.status(), Status::Active, "{name}");
                assert!(loaded.is_ok(), "{name} {requirement}: {loaded:?}");
            } else {
                let error = loaded.expect_err(name);
                assert_eq!(error.kind, PackErrorKind::UnknownReference, "{name}");
                assert_eq!(GuardedOperation::from_operation(name), None);
                refused += 1;
            }
        }
    }
    assert!(refused > 100, "{refused}");
    // Recording operations in particular.
    for name in ["observation.add", "source.add", "case.close"] {
        assert!(load(pack(name, "human_confirmation").as_bytes()).is_err());
    }
}

/// Only the handlers of the seven operations call the guardrail check; the
/// two withdrawals share one handler in source_ops.rs, each named once, and
/// `artifact.expire` has its own in retention_ops.rs.
#[test]
fn only_the_guarded_handlers_enforce_guardrails() {
    let handlers: [(&str, &str, &[&str]); 6] = [
        (
            "review_ops.rs",
            include_str!("../review_ops.rs"),
            &["ConclusionValidate"],
        ),
        (
            "decision_ops.rs",
            include_str!("../decision_ops.rs"),
            &["DecisionAccept"],
        ),
        (
            "source_ops.rs",
            include_str!("../source_ops.rs"),
            &["SourceWithdraw", "ArtifactWithdraw"],
        ),
        (
            "retention_ops.rs",
            include_str!("../retention_ops.rs"),
            &["ArtifactExpire"],
        ),
        (
            "observation_ops.rs",
            include_str!("../observation_ops.rs"),
            &["ObservationRetract"],
        ),
        (
            "classification_ops.rs",
            include_str!("../classification_ops.rs"),
            &["ClassificationDowngrade"],
        ),
    ];
    for (name, source, operations) in handlers {
        assert!(source.contains("guardrails::enforce("), "{name}");
        for operation in operations {
            assert_eq!(
                source
                    .matches(&format!("GuardedOperation::{operation}"))
                    .count(),
                1,
                "{name}"
            );
        }
        for other in GuardedOperation::ALL {
            let variant = format!("GuardedOperation::{other:?}");
            if !operations.contains(&format!("{other:?}").as_str()) {
                assert!(!source.contains(&variant), "{name} guards {variant}");
            }
        }
    }
    for (name, source) in [
        ("case_ops.rs", include_str!("../case_ops.rs")),
        ("measurement_ops.rs", include_str!("../measurement_ops.rs")),
        ("evaluate_ops.rs", include_str!("../evaluate_ops.rs")),
        ("explain_ops.rs", include_str!("../explain_ops.rs")),
        (
            "retrospective_ops.rs",
            include_str!("../retrospective_ops.rs"),
        ),
        ("producer_run.rs", include_str!("../producer_run.rs")),
        ("method_pack_ops.rs", include_str!("../method_pack_ops.rs")),
        ("coverage_ops.rs", include_str!("../coverage_ops.rs")),
        ("lessons_ops.rs", include_str!("../lessons_ops.rs")),
        ("origin_ops.rs", include_str!("../origin_ops.rs")),
    ] {
        assert!(!source.contains("guardrails::"), "{name}");
    }
}

#[test]
fn a_human_confirmation_names_each_confirmed_write_once() {
    let subject = id("concl", 9);
    let confirmation = |method: &str, confirms: Value| -> HumanConfirmation {
        serde_json::from_value(json!({
            "confirmed_by": "human:reviewer", "method": method, "confirms": confirms,
        }))
        .unwrap()
    };
    let one = json!([{"op": "conclusion.validate", "subject": subject}]);
    let valid = confirmation("Approved in an interactive prompt.", one.clone());
    validate_confirmation(&valid).unwrap();
    assert!(valid.covers(GuardedOperation::ConclusionValidate, subject));
    assert!(!valid.covers(GuardedOperation::DecisionAccept, subject));
    assert!(!valid.covers(GuardedOperation::ConclusionValidate, id("concl", 8)));

    for (bad, path) in [
        (confirmation(" ", one.clone()), "/human_confirmation/method"),
        (
            confirmation("a\u{7}b", one.clone()),
            "/human_confirmation/method",
        ),
        (
            confirmation(&"x".repeat(501), one.clone()),
            "/human_confirmation/method",
        ),
        (
            confirmation("ok", json!([])),
            "/human_confirmation/confirms",
        ),
        (
            confirmation(
                "ok",
                json!([{"op": "conclusion.validate", "subject": subject},
                        {"op": "conclusion.validate", "subject": subject}]),
            ),
            "/human_confirmation/confirms/1",
        ),
    ] {
        let failure = validate_confirmation(&bad).unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::InvalidInput);
        assert_eq!(failure.error.path.as_deref(), Some(path));
    }
    // Only guarded operations can be confirmed.
    assert!(
        serde_json::from_value::<HumanConfirmation>(json!({
            "confirmed_by": "human:reviewer", "method": "ok",
            "confirms": [{"op": "observation.add", "subject": subject}],
        }))
        .is_err()
    );
}

fn id(prefix: &str, n: u64) -> Id {
    format!("{prefix}_018d87fc-5234-7000-8000-{n:012x}")
        .parse()
        .unwrap()
}

/// Append a revision of `id` recorded by `actor` at the next sequence.
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
            header: RecordHeader {
                id,
                case_id: case,
                schema_version: SchemaVersion::RecordV0Alpha,
                created_by: actor.parse().unwrap(),
                actor_descriptor_self_reported: None,
                created_at: "2026-11-23T08:12:44.000Z".parse().unwrap(),
                recorded_at: "2026-11-23T08:12:44.000Z".parse().unwrap(),
                recorded_by: actor.parse().unwrap(),
                observed_at: None,
                acquired_at: None,
                acceptance_sequence: sequence,
                revision: Revision(1),
                history: vec![],
                classification: Default::default(),
                provenance: Default::default(),
                extensions: Default::default(),
            },
            body,
        },
    };
    let reference = record.reference();
    let change = RecordChange {
        client_ref: None,
        id,
        revision: reference.revision,
    };
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

fn reads(session: &Session, reads: Vec<SessionRead>, unlisted: u64) -> SessionReads {
    SessionReads::new(session.clone(), reads, unlisted)
}

/// An Observation recorded by `agent:author` under session `a`, then retracted
/// or withdrawn-in-use by others: what each requirement sees.
#[test]
fn requirements_read_authors_sessions_and_reads() {
    let case = id("case", 1);
    let other_case = id("case", 2);
    let mut state = State::default();
    write(&mut state, case, case, "agent:author", json!({}));
    let observation = id("obs", 3);
    write(
        &mut state,
        observation,
        case,
        "agent:author",
        json!({"value": "12.5", "status": "active"}),
    );
    // A metadata-only revision by another actor is not content.
    write(
        &mut state,
        observation,
        case,
        "agent:assessor",
        json!({"value": "12.5", "status": "active", "measurement_assessment": {"x": 1}}),
    );
    let subject = state.current(observation).unwrap().clone();
    let session_a: Session = "a".parse().unwrap();
    let session_b: Session = "b".parse().unwrap();
    let artifact = RevisionRef {
        id: id("art", 4),
        revision: 1,
    };
    let elsewhere = RevisionRef {
        id: id("art", 5),
        revision: 1,
    };
    let mut evidence = SessionEvidence::default();
    // The Observation's revision 1 was committed under session `a`.
    evidence.ledger.insert(2, reads(&session_a, Vec::new(), 0));

    let assessor: Actor = "agent:assessor".parse().unwrap();
    let author: Actor = "agent:author".parse().unwrap();
    let probe = retract(&state, &subject, case, &author, None, None);
    let content = probe.content_revisions().unwrap();
    assert_eq!(content.len(), 1);

    // distinct_actor: the assessor recorded no content.
    assert!(probe.distinct_actor().unwrap().is_some());
    assert!(
        retract(&state, &subject, case, &assessor, None, None)
            .distinct_actor()
            .unwrap()
            .is_none()
    );

    // distinct_context: a session is required, and session evidence.
    let (unmet, repair) = probe.distinct_context(&content).unwrap();
    assert_eq!(unmet, json!({"missing": "declared_session"}));
    assert_eq!(repair["change"], "session");
    let (unmet, _) = retract(&state, &subject, case, &assessor, Some(&session_b), None)
        .distinct_context(&content)
        .unwrap();
    assert_eq!(unmet["session_reads"], "unavailable");

    // The authoring session cannot retract its own Observation.
    let (unmet, _) = retract(
        &state,
        &subject,
        case,
        &assessor,
        Some(&session_a),
        Some(&evidence),
    )
    .distinct_context(&content)
    .unwrap();
    assert_eq!(
        unmet["authored_revisions"],
        json!([subject_ref(&state, observation, 1)])
    );
    assert_eq!(unmet["untrusted_reads_in_this_case"], 0);

    // A clean session passes.
    assert!(
        retract(
            &state,
            &subject,
            case,
            &assessor,
            Some(&session_b),
            Some(&evidence)
        )
        .distinct_context(&content)
        .is_none()
    );

    // A session that read untrusted evidence, now or on an earlier event of
    // this case, does not; reads in other cases are counted, not named.
    evidence.current = Some(reads(
        &session_b,
        vec![
            SessionRead {
                case_id: case,
                artifact,
            },
            SessionRead {
                case_id: other_case,
                artifact: elsewhere,
            },
        ],
        0,
    ));
    let (unmet, _) = retract(
        &state,
        &subject,
        case,
        &assessor,
        Some(&session_b),
        Some(&evidence),
    )
    .distinct_context(&content)
    .unwrap();
    assert_eq!(unmet["untrusted_reads"], json!([artifact]));
    assert_eq!(unmet["untrusted_reads_in_other_cases"], 1);
    assert_eq!(unmet["authored_revisions"], json!([]));
    assert!(unmet.get("unlisted_reads").is_none());

    evidence.current = None;
    evidence.ledger.insert(3, reads(&session_b, Vec::new(), 2));
    let (unmet, _) = retract(
        &state,
        &subject,
        case,
        &assessor,
        Some(&session_b),
        Some(&evidence),
    )
    .distinct_context(&content)
    .unwrap();
    assert_eq!(unmet["unlisted_reads"], true);

    // A record revised in the same transaction was authored by this session.
    let session_c: Session = "c".parse().unwrap();
    let mut same = retract(
        &state,
        &subject,
        case,
        &assessor,
        Some(&session_c),
        Some(&evidence),
    );
    same.sequence = 2;
    assert!(same.distinct_context(&content).is_some());
}

fn retract<'a>(
    state: &'a State,
    subject: &'a Record,
    case: Id,
    actor: &'a Actor,
    session: Option<&'a Session>,
    evidence: Option<&'a SessionEvidence>,
) -> Write<'a> {
    Write {
        operation: GuardedOperation::ObservationRetract,
        state,
        case,
        sequence: 10,
        actor,
        session,
        confirmation: None,
        evidence,
        subject,
    }
}

fn subject_ref(state: &State, id: Id, revision: u64) -> RevisionRef {
    state
        .revision(RevisionRef { id, revision })
        .unwrap()
        .reference()
}

#[test]
fn a_human_confirmation_must_name_this_write() {
    let case = id("case", 1);
    let mut state = State::default();
    write(&mut state, case, case, "agent:a", json!({}));
    let source = id("src", 2);
    write(&mut state, source, case, "agent:a", json!({"title": "S"}));
    let subject = state.current(source).unwrap().clone();
    let actor: Actor = "agent:a".parse().unwrap();
    let confirmation: HumanConfirmation = serde_json::from_value(json!({
        "confirmed_by": "human:reviewer", "method": "Approved in chat.",
        "confirms": [{"op": "observation.retract", "subject": source}],
    }))
    .unwrap();
    let write_with = |confirmation| Write {
        operation: GuardedOperation::SourceWithdraw,
        state: &state,
        case,
        sequence: 5,
        actor: &actor,
        session: None,
        confirmation,
        evidence: None,
        subject: &subject,
    };
    let (unmet, repair) = write_with(None).human_confirmation().unwrap().unwrap();
    assert_eq!(unmet, json!({"missing": "human_confirmation"}));
    assert_eq!(
        repair["shape"]["confirms"],
        json!([{"op": "source.withdraw", "subject": source}])
    );
    let (unmet, _) = write_with(Some(&confirmation))
        .human_confirmation()
        .unwrap()
        .unwrap();
    assert_eq!(unmet["missing"], "human_confirmation_for_this_write");
    let mut right = confirmation.clone();
    right.confirms[0].op = GuardedOperation::SourceWithdraw;
    assert!(
        write_with(Some(&right))
            .human_confirmation()
            .unwrap()
            .is_none()
    );
}

/// The confirming person is someone other than the writer and other than
/// whoever recorded the subject's current content: the content authors that
/// `distinct_actor` and review independence use. Actor strings are compared,
/// a procedural check that authenticates nobody.
#[test]
fn a_human_confirmation_by_the_writer_or_a_content_author_is_refused() {
    let case = id("case", 1);
    let mut state = State::default();
    write(&mut state, case, case, "agent:author", json!({}));
    let observation = id("obs", 3);
    write(
        &mut state,
        observation,
        case,
        "agent:author",
        json!({"value": "12.5", "status": "active"}),
    );
    // A metadata-only revision by another actor records no content.
    write(
        &mut state,
        observation,
        case,
        "agent:assessor",
        json!({"value": "12.5", "status": "active", "measurement_assessment": {"x": 1}}),
    );
    let subject = state.current(observation).unwrap().clone();
    let confirmed_by = |actor: &str| -> HumanConfirmation {
        serde_json::from_value(json!({
            "confirmed_by": actor, "method": "Approved in chat.",
            "confirms": [{"op": "observation.retract", "subject": observation}],
        }))
        .unwrap()
    };
    let check = |writer: &str, confirmer: &str| {
        let actor: Actor = writer.parse().unwrap();
        let confirmation = confirmed_by(confirmer);
        Write {
            operation: GuardedOperation::ObservationRetract,
            state: &state,
            case,
            sequence: 10,
            actor: &actor,
            session: None,
            confirmation: Some(&confirmation),
            evidence: None,
            subject: &subject,
        }
        .human_confirmation()
        .unwrap()
    };
    for (writer, confirmer) in [
        ("agent:author", "agent:author"),
        ("agent:writer", "agent:writer"),
        ("agent:writer", "agent:author"),
    ] {
        let (unmet, repair) = check(writer, confirmer)
            .unwrap_or_else(|| panic!("{writer} confirmed by {confirmer} was accepted"));
        assert_eq!(
            unmet,
            json!({
                "missing": "human_confirmation_by_another_actor",
                "confirmed_by": confirmer, "actor": writer, "authors": ["agent:author"],
            }),
            "{writer} {confirmer}"
        );
        assert_eq!(repair["change"], "human_confirmation");
        let route = repair["route"].as_str().unwrap();
        assert!(route.contains("other than the writer"), "{route}");
        assert!(route.contains("not authenticate"), "{route}");
    }
    // Someone else confirms: a person who recorded no content, even one who
    // revised the record's metadata.
    assert!(check("agent:writer", "human:reviewer").is_none());
    assert!(check("agent:writer", "agent:assessor").is_none());
    assert!(check("agent:author", "human:reviewer").is_none());
    // The route without a confirmation also names someone other than the writer.
    let actor: Actor = "agent:writer".parse().unwrap();
    let (_, repair) = Write {
        operation: GuardedOperation::ObservationRetract,
        state: &state,
        case,
        sequence: 10,
        actor: &actor,
        session: None,
        confirmation: None,
        evidence: None,
        subject: &subject,
    }
    .human_confirmation()
    .unwrap()
    .unwrap();
    assert!(
        repair["route"]
            .as_str()
            .unwrap()
            .contains("other than the writer")
    );
}
