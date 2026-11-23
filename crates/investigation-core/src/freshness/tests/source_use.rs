//! `source.assess`, `source.withdraw` and `artifact.withdraw` through the real
//! operations, observed through the freshness index and the evidence gates.

use super::*;
use crate::{
    decision_ops::{EffectiveDecision, effective_decision},
    review::EffectiveDisposition,
    review_ops::{effective_disposition, validation_review_current},
    source_ops::{UseState, evidentiary_use, standing, withdrawn_lineage},
};

pub(super) fn apply_as(
    case: &mut Case,
    actor: &str,
    ops: Vec<Value>,
) -> Result<RevisionRef, Failure> {
    let sequence = case.state.sequence() + 1;
    let command: Command = serde_json::from_value(json!({
        "schema_version":"inv.command.v1","case_id":case.case,"actor":actor,
        "command_id":format!("source-use-{sequence}"),"preconditions":[],"operations":ops,
    }))
    .unwrap();
    let accepted = validate_and_apply(
        &case.state,
        &command,
        &standard_catalog(),
        Acceptance {
            sequence,
            clock: &mut SystemClock,
            ids: &mut SystemIdSource,
        },
    )?;
    case.state.apply(&accepted)?;
    case.settle();
    Ok(accepted.records[0].reference())
}

/// `freshness.needs_review` gaps at the case's current snapshot.
fn freshness_gaps(case: &Case) -> Vec<crate::gap_rules::Gap> {
    let snapshot = crate::wire::Snapshot {
        sequence: case.state.sequence(),
        head_hash: format!("sha256:{}", "0".repeat(64)).parse().unwrap(),
    };
    crate::gap_rules::enumerate_gaps(&case.state, &snapshot, case.case)
        .unwrap()
        .gaps
        .into_iter()
        .filter(|gap| gap.rule_id == "freshness.needs_review")
        .collect()
}

fn try_one(case: &mut Case, name: &str, payload: Value) -> Result<RevisionRef, Failure> {
    apply_as(case, "agent:test", vec![op(name, payload)])
}

fn withdraw_source(source: RevisionRef, scope: Value) -> Value {
    json!({
        "source_ref": source, "reason": "retracted_by_origin", "scope": scope,
        "rationale": "The publisher retracted the benchmark report.", "evidence_refs": []
    })
}

fn diagnostic_assessment(f: &Fixture) -> Value {
    json!({
        "subject_ref": f.hypothesis, "evidence_refs": [f.observation], "relation": "supports",
        "rationale": "The retained p99 is below the limit."
    })
}

/// A second, independently collected Source, recorded the way ingest records one.
fn erratum(case: &mut Case) -> RevisionRef {
    case.image(Kind::Source, json!({
        "producer":null,"publisher":"Queue vendor","identity":"erratum.txt",
        "acquisition":{"channel":"local_file","acquired_at":null,"method":null,"context":null,"environment":null},
        "access":"local_only","verification_limits":[]
    }), None)
}

const LAYERS: usize = 9;

fn layers(f: &Fixture) -> [RevisionRef; LAYERS] {
    [
        f.artifact,
        f.observation,
        f.assessment,
        f.conclusion,
        f.conclusion_review,
        f.note,
        f.comparison,
        f.decision,
        f.decision_review,
    ]
}

#[test]
fn real_source_withdrawal_reaches_every_dependent_layer_without_erasure() {
    let mut case = Case::new();
    let f = case.fixture();
    let original = case.state.revision(f.source).unwrap().body.clone();
    let withdrawn = case.one(
        "source.withdraw",
        withdraw_source(f.source, json!({"extent":"entire"})),
    );
    assert_eq!(withdrawn.revision, 2);

    for reference in layers(&f) {
        let result = case.freshness(reference);
        assert_eq!(result.freshness, Freshness::NeedsReview, "{result:#?}");
        let reason = only_reason(result);
        assert_eq!(reason.trigger, Trigger::Withdrawn);
        assert_eq!(reason.cause, withdrawn);
        assert_eq!(reason.path[0].record, reference.id);
        assert_eq!(reason.path.last().unwrap().cites, f.source);
    }
    assert_eq!(
        case.freshness(f.conclusion).label(),
        "validated / needs_review"
    );
    assert_eq!(
        case.freshness(f.decision).label(),
        "accepted / needs_review"
    );
    for unaffected in [f.question, f.hypothesis, f.option, f.criterion] {
        assert_eq!(case.freshness(unaffected).freshness, Freshness::Current);
    }
    let overview = case.index.overview();
    assert_eq!(overview.needs_review, LAYERS);
    assert!(!overview.affected.iter().any(|r| r.id == f.source.id));
    assert_eq!(
        case.freshness(f.source).lifecycle.as_deref(),
        Some("withdrawn")
    );

    // The gap inventory surfaces each stale judgment with its reason chain.
    let stale = freshness_gaps(&case);
    assert_eq!(
        stale
            .iter()
            .map(|gap| gap.subject_refs[0].id)
            .collect::<BTreeSet<_>>(),
        [f.assessment, f.comparison, f.conclusion, f.decision]
            .iter()
            .map(|reference| reference.id)
            .collect()
    );
    let conclusion_gap = stale
        .iter()
        .find(|gap| gap.subject_refs[0].id == f.conclusion.id)
        .unwrap();
    assert!(
        conclusion_gap.reason.starts_with(&format!(
            "Effective freshness is validated / needs_review: withdrawn: {} r2 via {} ",
            f.source.id, f.conclusion.id
        )),
        "{}",
        conclusion_gap.reason
    );
    assert!(conclusion_gap.read_set.contains(&withdrawn));
    assert!(conclusion_gap.read_set.contains(&f.conclusion_review));

    // Not erasure: the earlier revision is unchanged, and so is availability.
    assert_eq!(case.state.revision(f.source).unwrap().body, original);
    let current = case.state.current(f.source.id).unwrap();
    assert_eq!(current.body["status"], "withdrawn");
    assert_eq!(current.body["withdrawals"][0]["actor"], "agent:test");
    assert_eq!(current.body["withdrawals"][0]["revision"], 1);
    assert_eq!(current.body["identity"], original["identity"]);
    let observation = case.state.current(f.observation.id).unwrap();
    assert_eq!(observation.reference(), f.observation);
    let label = evidentiary_use(&case.state, observation);
    assert_eq!(label.state, UseState::Withdrawn);
    assert_eq!(
        label.availability,
        Some(crate::evidence::ArtifactAvailability::Retained)
    );
    assert!(!label.erasure);
    assert_eq!(label.withdrawals[0].record, withdrawn);
    assert_eq!(withdrawn_lineage(&case.state, observation), Some(withdrawn));

    // Withdrawn evidence grounds no new diagnostic judgment or promotion.
    let refused = try_one(&mut case, "assessment.add", diagnostic_assessment(&f)).unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::GateFailed, "{refused:?}");
    let conclusion = case.state.current(f.conclusion.id).unwrap();
    let refused = required_read_set(&case.state, conclusion).unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::GateFailed, "{refused:?}");
    let again = try_one(
        &mut case,
        "source.withdraw",
        withdraw_source(withdrawn, json!({"extent":"entire"})),
    )
    .unwrap_err();
    assert_eq!(again.error.code, ErrorCode::GateFailed);
    // A non-diagnostic record of the withdrawn evidence is still possible.
    let mut payload = diagnostic_assessment(&f);
    payload["relation"] = json!("non_diagnostic");
    try_one(&mut case, "assessment.add", payload).unwrap();
}

#[test]
fn partial_withdrawal_marks_dependents_and_keeps_the_rest_usable() {
    let mut case = Case::new();
    let f = case.fixture();
    let withdrawn = case.one(
        "source.withdraw",
        withdraw_source(
            f.source,
            json!({"extent":"partial","statement":"The second benchmark table only."}),
        ),
    );
    for reference in layers(&f) {
        let reason = only_reason(case.freshness(reference));
        assert_eq!(reason.trigger, Trigger::Withdrawn);
        assert_eq!(reason.cause, withdrawn);
    }
    let source = case.state.current(f.source.id).unwrap();
    assert!(source.body.get("status").is_none());
    assert_eq!(case.freshness(f.source).freshness, Freshness::Current);
    let observation = case.state.current(f.observation.id).unwrap();
    assert_eq!(
        evidentiary_use(&case.state, observation).state,
        UseState::PartiallyWithdrawn
    );
    // The rest stays usable: the promotion closure reads the current Source.
    let conclusion = case.state.current(f.conclusion.id).unwrap();
    let reads = required_read_set(&case.state, conclusion).unwrap();
    assert!(reads.contains(&withdrawn), "{reads:?}");
    try_one(&mut case, "assessment.add", diagnostic_assessment(&f)).unwrap();
    // A later entire withdrawal is its own trigger.
    let entire = case.one(
        "source.withdraw",
        withdraw_source(withdrawn, json!({"extent":"entire"})),
    );
    let reason = only_reason(case.freshness(f.conclusion));
    assert_eq!(
        (reason.trigger.clone(), reason.cause),
        (Trigger::Withdrawn, entire)
    );
}

#[test]
fn artifact_withdrawal_reaches_derived_observations() {
    let mut case = Case::new();
    let f = case.fixture();
    let withdrawn = case.one(
        "artifact.withdraw",
        json!({
            "artifact_ref": f.artifact, "reason": "collection_error", "scope": {"extent":"entire"},
            "rationale": "The capture recorded the wrong environment.", "evidence_refs": []
        }),
    );
    for reference in [f.observation, f.assessment, f.conclusion, f.decision] {
        let reason = only_reason(case.freshness(reference));
        assert_eq!(reason.trigger, Trigger::Withdrawn);
        assert_eq!(reason.cause, withdrawn);
    }
    let artifact = case.state.current(f.artifact.id).unwrap();
    assert_eq!(artifact.body["availability"], "retained");
    assert_eq!(
        artifact.body["digest"],
        case.state.revision(f.artifact).unwrap().body["digest"]
    );
    let refused = try_one(&mut case, "assessment.add", diagnostic_assessment(&f)).unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::GateFailed);
}

#[test]
fn primary_source_erratum_invalidates_a_prior_judgment_without_deleting_it() {
    let mut case = Case::new();
    let f = case.fixture();
    let original = case.state.revision(f.source).unwrap().body.clone();
    let erratum = erratum(&mut case);
    let assessed = case.one(
        "source.assess",
        json!({
            "source_ref": f.source,
            "origin": {"judgment":"primary","rationale":"The vendor published the benchmark."},
            "currency": {"judgment":"superseded","rationale":"The vendor erratum replaces the p99 table."},
            "limitations": ["The erratum names one edition."],
            "evidence_refs": [erratum]
        }),
    );
    for reference in [
        f.observation,
        f.assessment,
        f.conclusion,
        f.comparison,
        f.decision,
    ] {
        let result = case.freshness(reference);
        assert_eq!(result.freshness, Freshness::NeedsReview, "{result:#?}");
        let reason = only_reason(result);
        assert_eq!(
            reason.trigger,
            Trigger::SourceConcern {
                dimensions: vec!["currency".into()]
            }
        );
        assert_eq!(reason.cause, assessed);
    }
    assert_eq!(case.freshness(erratum).freshness, Freshness::Current);
    // The judged record and every record built on it keep their history.
    assert_eq!(case.state.revision(f.source).unwrap().body, original);
    assert_eq!(
        case.state.current(f.observation.id).unwrap().reference(),
        f.observation
    );
    assert_eq!(
        case.state.current(f.assessment.id).unwrap().reference(),
        f.assessment
    );
    let shown = standing(case.state.current(f.source.id).unwrap()).unwrap();
    let currency = &shown.dimensions[3];
    assert!(currency.concern);
    assert_eq!(currency.current[0].judgment, "superseded");
    assert!(
        shown.dimensions[0].current.is_empty(),
        "authenticity not judged"
    );
    assert_eq!(shown.judgments_kept, 1);
}

#[test]
fn benign_judgments_keep_dependents_current_and_promotable() {
    let mut case = Case::new();
    let f = case.fixture();
    let assessed = case.one(
        "source.assess",
        json!({
            "source_ref": f.source,
            "authenticity": {"judgment":"authentic","rationale":"Retrieved from the vendor archive."},
            "reliability": {"judgment":"cannot_judge","grounds":["track_record"],"rationale":"No earlier reports examined."},
            "currency": {"judgment":"current","rationale":"No later edition found."}
        }),
    );
    for result in case.index.records() {
        assert_eq!(result.freshness, Freshness::Current, "{result:?}");
    }
    assert!(freshness_gaps(&case).is_empty());
    let conclusion = case.state.current(f.conclusion.id).unwrap();
    assert!(
        required_read_set(&case.state, conclusion)
            .unwrap()
            .contains(&assessed)
    );
    try_one(&mut case, "assessment.add", diagnostic_assessment(&f)).unwrap();
    // Read-time gates agree: the reviewed basis still covers the judged Source.
    let gates = |case: &Case| {
        let conclusion = case.state.current(f.conclusion.id).unwrap();
        let decision = case.state.current(f.decision.id).unwrap();
        (
            effective_disposition(&case.state, conclusion),
            effective_disposition(&case.state, decision),
            effective_decision(&case.state, decision).unwrap(),
        )
    };
    assert_eq!(
        gates(&case),
        (
            EffectiveDisposition::Current,
            EffectiveDisposition::Current,
            EffectiveDecision::Accepted
        )
    );
    let review = case.state.current(f.conclusion_review.id).unwrap();
    let review: crate::review_ops::ValidationReviewBody =
        serde_json::from_value(review.body.clone()).unwrap();
    let conclusion = case.state.current(f.conclusion.id).unwrap();
    validation_review_current(&case.state, conclusion, &review).unwrap();

    // A second assessor disagrees; the concern is raised, not averaged away.
    let disputed = apply_as(
        &mut case,
        "agent:second-reader",
        vec![op(
            "source.assess",
            json!({
                "source_ref": assessed,
                "currency": {"judgment":"outdated","rationale":"A newer release changed the queue."}
            }),
        )],
    )
    .unwrap();
    let reason = only_reason(case.freshness(f.conclusion));
    assert_eq!(reason.cause, disputed);
    assert_eq!(
        gates(&case),
        (
            EffectiveDisposition::NeedsReview,
            EffectiveDisposition::NeedsReview,
            EffectiveDecision::NeedsReview
        )
    );
    let conclusion = case.state.current(f.conclusion.id).unwrap();
    let refused = validation_review_current(&case.state, conclusion, &review).unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::GateFailed);
    let shown = standing(case.state.current(f.source.id).unwrap()).unwrap();
    assert!(shown.dimensions[3].disagreement);
    assert!(shown.dimensions[3].concern);
    assert_eq!(shown.dimensions[3].current.len(), 2);
}

#[test]
fn malformed_requests_are_refused_before_anything_changes() {
    let mut case = Case::new();
    let f = case.fixture();
    let cases = [
        (
            "source.assess",
            json!({"source_ref": f.source, "limitations": []}),
            ErrorCode::InvalidInput,
        ),
        (
            "source.assess",
            json!({"source_ref": f.artifact,
                "currency": {"judgment":"current","rationale":"Checked."}}),
            ErrorCode::InvalidInput,
        ),
        (
            "source.assess",
            json!({"source_ref": f.source, "evidence_refs": [f.source],
                "currency": {"judgment":"current","rationale":"Checked."}}),
            ErrorCode::InvalidInput,
        ),
        (
            "source.withdraw",
            withdraw_source(f.source, json!({"extent":"partial","statement":" "})),
            ErrorCode::InvalidInput,
        ),
        (
            "artifact.withdraw",
            json!({"artifact_ref": f.artifact, "reason": "other", "scope": {"extent":"entire"},
                "rationale": "Stale.", "evidence_refs": [f.assessment]}),
            ErrorCode::InvalidInput,
        ),
        (
            "source.withdraw",
            withdraw_source(
                RevisionRef {
                    id: f.source.id,
                    revision: 9,
                },
                json!({"extent":"entire"}),
            ),
            ErrorCode::RevisionConflict,
        ),
    ];
    let before = case.state.sequence();
    for (name, payload, code) in cases {
        let refused = try_one(&mut case, name, payload.clone()).unwrap_err();
        assert_eq!(refused.error.code, code, "{name} {payload}: {refused:?}");
    }
    assert_eq!(case.state.sequence(), before);
}

/// A new Observation of the fixture Artifact, recorded the way the fixture
/// records one: it reads the Artifact and its Source as they are now.
fn observe_again(case: &mut Case, f: &Fixture) -> RevisionRef {
    let body = case.state.revision(f.observation).unwrap().body.clone();
    case.image(Kind::Observation, body, None)
}

/// Review the current conclusion again and validate it with that review.
fn revalidate(case: &mut Case, f: &Fixture) -> (RevisionRef, RevisionRef) {
    let review = case.validate(case.current(f.conclusion));
    let current = case.current(f.conclusion);
    let mut reads =
        required_read_set(&case.state, case.state.current(f.conclusion.id).unwrap()).unwrap();
    reads.extend([current, review]);
    let conclusion = case
        .run(
            vec![op(
                "conclusion.validate",
                json!({"conclusion_ref": current, "review_ref": review}),
            )],
            &reads,
        )
        .records[0]
        .reference();
    (conclusion, review)
}

fn gap_subjects(case: &Case, rule: &str) -> BTreeSet<Id> {
    let snapshot = crate::wire::Snapshot {
        sequence: case.state.sequence(),
        head_hash: format!("sha256:{}", "0".repeat(64)).parse().unwrap(),
    };
    crate::gap_rules::enumerate_gaps(&case.state, &snapshot, case.case)
        .unwrap()
        .gaps
        .into_iter()
        .filter(|gap| gap.rule_id == rule)
        .map(|gap| gap.subject_refs[0].id)
        .collect()
}

#[test]
fn records_read_after_a_partial_withdrawal_stay_current_until_the_source_changes_again() {
    let mut case = Case::new();
    let f = case.fixture();
    let withdrawn = case.one(
        "source.withdraw",
        withdraw_source(
            f.source,
            json!({"extent":"partial","statement":"The second benchmark table only."}),
        ),
    );
    // Created after the withdrawal: read with it in effect.
    let fresh = case.one("hypothesis.create", json!({
        "question_ref":f.question,"statement":"The broker meets the latency limit.",
        "applicability":"Projected load","assumption_refs":[],"possible_falsifiers":["p99 above 25 ms"],"rival_refs":[]
    }));
    let observation = observe_again(&mut case, &f);
    let assessment = case.one(
        "assessment.add",
        json!({
            "subject_ref": fresh, "evidence_refs": [observation], "relation": "supports",
            "rationale": "The retained p99 is below the limit."
        }),
    );
    assert_eq!(
        evidentiary_use(&case.state, case.state.current(observation.id).unwrap()).state,
        UseState::PartiallyWithdrawn
    );
    for reference in [observation, assessment] {
        let result = case.freshness(reference);
        assert_eq!(result.freshness, Freshness::Current, "{result:#?}");
    }
    // Revalidated after it: the conclusion and its new review are current and
    // every gate agrees; the review that read the old revision does not.
    let (conclusion, review) = revalidate(&mut case, &f);
    for reference in [conclusion, review] {
        let result = case.freshness(reference);
        assert_eq!(result.freshness, Freshness::Current, "{result:#?}");
    }
    let record = case.state.current(f.conclusion.id).unwrap();
    assert_eq!(
        effective_disposition(&case.state, record),
        EffectiveDisposition::Current
    );
    let body: crate::review_ops::ValidationReviewBody =
        serde_json::from_value(case.state.current(review.id).unwrap().body.clone()).unwrap();
    validation_review_current(&case.state, record, &body).unwrap();
    assert!(!gap_subjects(&case, "freshness.needs_review").contains(&conclusion.id));
    let stale_reads = gap_subjects(&case, "review.stale_read_set");
    assert!(!stale_reads.contains(&review.id));
    assert!(stale_reads.contains(&f.conclusion_review.id));
    // Records that read the pre-withdrawal revision still need review.
    for reference in [f.artifact, f.observation, f.assessment, f.conclusion_review] {
        let reason = only_reason(case.freshness(reference));
        assert_eq!(
            (reason.trigger.clone(), reason.cause),
            (Trigger::Withdrawn, withdrawn)
        );
    }

    // A later source change is new to all of them.
    let assessed = case.one(
        "source.assess",
        json!({
            "source_ref": withdrawn,
            "currency": {"judgment":"outdated","rationale":"A newer release changed the queue."}
        }),
    );
    let concern = Trigger::SourceConcern {
        dimensions: vec!["currency".into()],
    };
    for reference in [observation, assessment, conclusion, review, f.assessment] {
        let reason = only_reason(case.freshness(reference));
        assert_eq!(
            (reason.trigger.clone(), reason.cause),
            (concern.clone(), assessed)
        );
    }
    let record = case.state.current(f.conclusion.id).unwrap();
    assert_eq!(
        effective_disposition(&case.state, record),
        EffectiveDisposition::NeedsReview
    );
    // Read after the concern too: current again.
    let again = observe_again(&mut case, &f);
    assert_eq!(case.freshness(again).freshness, Freshness::Current);
    // An entire withdrawal is new to every reader, whenever it read.
    let entire = case.one(
        "source.withdraw",
        withdraw_source(assessed, json!({"extent":"entire"})),
    );
    for reference in [again, observation, conclusion] {
        let reason = only_reason(case.freshness(reference));
        assert_eq!(
            (reason.trigger.clone(), reason.cause),
            (Trigger::Withdrawn, entire)
        );
    }
}

/// A partial withdrawal names its part in words, not by selector, so the core
/// cannot tell that an Observation quotes another part. Such an Observation
/// needs review until a later revision of it is accepted with the withdrawal
/// in effect; the attributed measurement judgment is that acknowledgement.
/// Records that read the Observation before the withdrawal still need review.
#[test]
fn a_later_judgment_acknowledges_a_partial_withdrawal_for_that_observation_only() {
    let mut case = Case::new();
    let f = case.fixture();
    let withdrawn = case.one(
        "source.withdraw",
        withdraw_source(
            f.source,
            json!({"extent":"partial","statement":"The second benchmark table only."}),
        ),
    );
    assert_eq!(
        case.freshness(f.observation).freshness,
        Freshness::NeedsReview
    );
    let judged = apply_as(
        &mut case,
        "agent:reviewer",
        vec![op(
            "measurement.assess",
            json!({
                "observation_ref": f.observation, "channel": null, "validity": "valid",
                "verification": [{"kind":"equivalent","justification":"The quoted table is not the withdrawn one."}],
                "rationale": "The observation reads the first table.", "limits": []
            }),
        )],
    )
    .unwrap();
    assert_eq!(judged.id, f.observation.id);
    assert_eq!(case.freshness(judged).freshness, Freshness::Current);
    for reference in [f.assessment, f.conclusion, f.comparison, f.decision] {
        let reason = only_reason(case.freshness(reference));
        assert_eq!(
            (reason.trigger.clone(), reason.cause),
            (Trigger::Withdrawn, withdrawn)
        );
    }
}
