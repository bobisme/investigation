use serde_json::{Value, json};

use super::*;
use crate::{
    blindspot::{AuditRequest, audit},
    catalog::standard_catalog,
    command::Command,
    gap_explain::gap_rule,
    gap_rules::{GapInventory, GapStatus, enumerate_gaps},
    guidance::{STALL_GAP_RULES, TriggerContext, card_for_rule},
    header::SystemClock,
    identity::{IdSource, SystemIdSource},
    method_pack::{CaseFact, GAP_RULE_TRIGGERS},
    method_pack_ops::resolve,
    next_rank::rank_next,
    state::{Acceptance, AcceptedTransaction, validate_and_apply},
    wire::{RecordChange, Snapshot},
};

const DIGEST: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

/// Operations that would create a hypothesis, a plan, a run, an observation
/// or another new line of work. No stall move may name one.
const FABRICATING: &[&str] = &[
    "hypothesis.create",
    "hypothesis.amend",
    "inquiry.create",
    "inquiry.amend",
    "inquiry.freeze",
    "run.start",
    "run.record",
    "observation.add",
    "observation.correct",
    "comparison.add",
    "option.create",
    "criterion.create",
];

struct Case {
    state: State,
    case: Option<Id>,
}

impl Case {
    fn new(mode: &str) -> Self {
        let mut case = Self {
            state: State::default(),
            case: None,
        };
        case.one(
            "case.create",
            json!({"title": "Slow request", "mode": mode}),
        );
        case
    }

    fn one(&mut self, op: &str, payload: Value) -> RevisionRef {
        self.with(op, payload, vec![])
    }

    fn with(&mut self, op: &str, payload: Value, preconditions: Vec<RevisionRef>) -> RevisionRef {
        let sequence = self.state.sequence() + 1;
        let command: Command = serde_json::from_value(json!({
            "schema_version": "inv.command.v1", "command_id": format!("stall-{sequence}"),
            "case_id": self.case, "actor": "agent:test", "preconditions": preconditions,
            "operations": [{"op": op, "payload": payload}],
        }))
        .unwrap();
        let accepted = validate_and_apply(
            &self.state,
            &command,
            &standard_catalog(),
            Acceptance {
                sequence,
                clock: &mut SystemClock,
                ids: &mut SystemIdSource,
            },
        )
        .unwrap_or_else(|failure| panic!("{op}: {failure:?}"));
        self.state.apply(&accepted).unwrap();
        self.state.note_verified_snapshot(self.snapshot()).unwrap();
        self.case.get_or_insert(accepted.case_id);
        accepted.records.last().unwrap().reference()
    }

    /// Store a record image directly, for kinds whose write path needs the
    /// blob store.
    fn image(&mut self, kind: Kind, body: Value) -> RevisionRef {
        let case = self.case.unwrap();
        let sequence = self.state.sequence() + 1;
        let mut header = self.state.current(case).unwrap().header.clone();
        header.acceptance_sequence = sequence;
        header.id = SystemIdSource.next_id(kind).unwrap();
        header.revision.0 = 1;
        header.history.clear();
        let record = Record { header, body };
        let reference = record.reference();
        self.state
            .apply(&AcceptedTransaction {
                case_id: case,
                sequence,
                records: vec![record],
                created: vec![RecordChange {
                    client_ref: None,
                    id: reference.id,
                    revision: reference.revision,
                }],
                updated: vec![],
                warnings: vec![],
            })
            .unwrap();
        self.state.note_verified_snapshot(self.snapshot()).unwrap();
        reference
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            sequence: self.state.sequence(),
            head_hash: format!("sha256:{:064x}", self.state.sequence())
                .parse()
                .unwrap(),
        }
    }

    fn case_ref(&self) -> RevisionRef {
        self.state.current(self.case.unwrap()).unwrap().reference()
    }

    fn sequence(&self, reference: RevisionRef) -> u64 {
        self.state
            .revision(reference)
            .unwrap()
            .header
            .acceptance_sequence
    }

    fn inventory(&self) -> GapInventory {
        enumerate_gaps(&self.state, &self.snapshot(), self.case.unwrap()).unwrap()
    }

    fn stalls(&self, rule: &str) -> Vec<Gap> {
        self.inventory()
            .gaps
            .into_iter()
            .filter(|gap| gap.rule_id == rule)
            .collect()
    }

    fn question(&mut self) -> RevisionRef {
        self.one(
            "question.create",
            json!({"wording": "Why is the request slow?", "importance": "high"}),
        )
    }

    fn hypothesis(&mut self, question: RevisionRef, statement: &str) -> RevisionRef {
        self.one(
            "hypothesis.create",
            json!({"question_ref": question, "statement": statement,
                "possible_falsifiers": ["Warm requests are as slow."],
                "rival_refs": [], "assumption_refs": []}),
        )
    }

    /// A frozen exploratory plan for `question`.
    fn plan(&mut self, question: RevisionRef, purpose: &str) -> RevisionRef {
        let draft = self.one(
            "inquiry.create",
            json!({"purpose": purpose, "method": "Replay the captured request.",
                "controls": ["Same build."], "limits": ["Three attempts."],
                "desired_observations": ["Request latency."],
                "inquiry": {"mode": "exploratory"}}),
        );
        self.one(
            "inquiry.freeze",
            json!({"inquiry_ref": draft, "question_ref": question}),
        )
    }

    fn run(&mut self, plan: RevisionRef, status: &str) -> RevisionRef {
        self.one(
            "run.record",
            json!({"intent": {"kind": "plan", "plan_ref": plan}, "producer": "agent:fixture",
                "artifact_refs": [], "execution_status": status,
                "evidence_completeness": "complete", "evidence_kind": "interventional",
                "variables_changed": ["request replay"]}),
        )
    }

    fn observe(&mut self, value: &str, validity: &str) -> RevisionRef {
        self.one(
            "observation.add",
            json!({
                "value": {"kind": "text", "statement": format!("Reading {value} was reported.")},
                "basis": {"kind": "assertion", "assertion": "Reported by the recorder.", "context": null},
                "validity": validity, "quality": [], "limitations": [],
                "evidence_kind": "observational", "variables_changed": []
            }),
        )
    }
}

fn assert_non_fabricating(gap: &Gap) {
    let stall = gap.stall.as_ref().expect("a stall gap carries its detail");
    assert_eq!(gap.suggested_operation, None, "{}", gap.rule_id);
    assert_eq!(gap.status, GapStatus::ReadyToAnalyze);
    for stall_move in &stall.moves {
        if let Some(operation) = &stall_move.operation {
            assert!(
                STALL_MOVE_OPERATIONS.contains(&operation.as_str()),
                "{}: {operation}",
                gap.rule_id
            );
            assert!(!FABRICATING.contains(&operation.as_str()));
        }
    }
    // A justified stop is always offered, as a recordable operation.
    assert!(stall.moves.iter().any(|stall_move| {
        stall_move.kind == StallMoveKind::StopInconclusive
            && stall_move.operation.as_deref() == Some("case.pause")
    }));
    assert_eq!(stall.stall_rule_version, STALL_RULE_VERSION);
    assert!(!stall.thresholds.is_empty());
    assert!(!stall.journal_items.is_empty());
    // Every cited journal item is in the read set.
    for item in &stall.journal_items {
        for reference in &item.refs {
            assert!(gap.read_set.contains(reference), "{reference:?}");
        }
    }
}

#[test]
fn the_stall_rule_table_is_registered_everywhere_a_rule_must_be() {
    let ids: Vec<&str> = StallPattern::ALL
        .iter()
        .map(|pattern| pattern.rule_id())
        .collect();
    assert_eq!(ids, STALL_RULES);
    assert_eq!(STALL_GAP_RULES, STALL_RULES);
    for rule in STALL_RULES {
        let described = gap_rule(rule).unwrap_or_else(|| panic!("{rule} is described"));
        assert_eq!(described.suggested_operation, None);
        assert!(described.condition.contains("(stall.v1)"), "{rule}");
        assert!(GAP_RULE_TRIGGERS.contains(rule), "{rule}");
    }
    for operation in STALL_MOVE_OPERATIONS {
        assert!(standard_catalog().get(operation).is_some(), "{operation}");
        assert!(!FABRICATING.contains(operation), "{operation}");
    }
}

/// The acceptance fixture: a plan whose Runs keep ending blocked.
#[test]
fn repeated_blocked_runs_of_one_plan_report_a_stall_citing_each_run() {
    let mut case = Case::new("explain");
    let question = case.question();
    case.hypothesis(question, "The cache is cold.");
    let plan = case.plan(question, "Replay the slow request.");
    let mut runs = Vec::new();
    for _ in 1..STALL_RUN_FAILURES {
        runs.push(case.run(plan, "blocked"));
    }
    assert!(case.stalls("run.repeated_same_failure").is_empty());
    runs.push(case.run(plan, "blocked"));

    let stalls = case.stalls("run.repeated_same_failure");
    assert_eq!(stalls.len(), 1, "{stalls:#?}");
    let gap = &stalls[0];
    assert_eq!(gap.subject_refs, vec![plan]);
    assert_eq!(
        gap.gap_id,
        format!("gap:run.repeated_same_failure:{}", plan.id)
    );
    assert_non_fabricating(gap);
    let stall = gap.stall.as_ref().unwrap();
    assert_eq!(stall.pattern, "repeated_same_failure");
    assert_eq!(
        stall.thresholds,
        vec![StallThreshold {
            name: "consecutive_failed_runs".into(),
            value: STALL_RUN_FAILURES as u64,
        }]
    );
    // One journal item per counted Run, in journal order, by sequence.
    let cited: Vec<(u64, Vec<RevisionRef>)> = stall
        .journal_items
        .iter()
        .map(|item| (item.sequence, item.refs.clone()))
        .collect();
    let expected: Vec<(u64, Vec<RevisionRef>)> = runs
        .iter()
        .map(|run| (case.sequence(*run), vec![*run]))
        .collect();
    assert_eq!(cited, expected);
    assert!(
        stall
            .journal_items
            .iter()
            .all(|item| item.fact == "run_failed:execution_status:blocked")
    );
    for run in &runs {
        assert!(gap.reason.contains(&run.id.to_string()), "{}", gap.reason);
    }
    assert!(gap.reason.contains("execution_status:blocked"));
    // Moves name the plan, the open question and the runs exactly.
    let kinds: Vec<StallMoveKind> = stall.moves.iter().map(|m| m.kind).collect();
    for kind in [
        StallMoveKind::VerifyInstrument,
        StallMoveKind::InspectRawData,
        StallMoveKind::NarrowQuestion,
        StallMoveKind::ChangeObservationChannel,
        StallMoveKind::StopInconclusive,
    ] {
        assert!(kinds.contains(&kind), "{kind:?}");
    }
    let operation = |name: &str| {
        stall
            .moves
            .iter()
            .find(|stall_move| stall_move.operation.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("{name}"))
    };
    assert_eq!(operation("inquiry.retire").target_refs, vec![plan]);
    assert_eq!(operation("question.resolve").target_refs, vec![question]);
    assert_eq!(operation("case.pause").target_refs, vec![case.case_ref()]);

    // The stalled fact holds and `next` ranks the stall with its card and a
    // justified stop.
    let inventory = case.inventory();
    let context = TriggerContext::evaluate(&case.state, case.case.unwrap(), &inventory);
    assert!(context.case_facts.contains(&CaseFact::Stalled));
    let packs = resolve(&[], |_| None);
    let card = card_for_rule(&packs, context.mode, "run.repeated_same_failure").unwrap();
    assert_eq!(
        (card.pack_id.as_str(), card.card.as_str()),
        ("general", "stalled")
    );
    let next = rank_next(&case.state, &inventory, 3);
    let recommendation = next
        .recommendations
        .iter()
        .find(|recommendation| recommendation.rule_id == "run.repeated_same_failure")
        .expect("the stall is among the top recommendations");
    assert_eq!(recommendation.stall.as_ref(), gap.stall.as_ref());
    assert_eq!(recommendation.schema_operation, None);
    let stop = next.stop_or_defer.expect("a stall offers a justified stop");
    assert!(stop.basis[0].contains(&gap.gap_id), "{:?}", stop.basis);
    assert!(
        stop.alternatives
            .iter()
            .any(|alternative| alternative.operation == "case.pause")
    );

    // A different failure starts a new streak; it does not extend this one.
    case.run(plan, "failed");
    assert!(case.stalls("run.repeated_same_failure").is_empty());
    for _ in 1..STALL_RUN_FAILURES {
        case.run(plan, "failed");
    }
    let gap = &case.stalls("run.repeated_same_failure")[0];
    assert_eq!(
        gap.stall.as_ref().unwrap().journal_items.len(),
        STALL_RUN_FAILURES
    );
    assert!(gap.reason.contains("execution_status:failed"));

    // Retiring the plan is a stop: the stall is no longer reported.
    let current_plan = case.state.current(plan.id).unwrap().reference();
    case.one(
        "inquiry.retire",
        json!({"inquiry_ref": current_plan, "rationale": "Blocked by the environment every time."}),
    );
    assert!(case.stalls("run.repeated_same_failure").is_empty());
}

#[test]
fn a_consequence_after_the_failures_ends_the_stall() {
    let mut case = Case::new("explain");
    let question = case.question();
    let hypothesis = case.hypothesis(question, "The cache is cold.");
    let plan = case.plan(question, "Replay the slow request.");
    let mut last = None;
    for _ in 0..STALL_RUN_FAILURES {
        last = Some(case.run(plan, "cancelled"));
    }
    assert_eq!(case.stalls("run.repeated_same_failure").len(), 1);
    // An inconclusive assessment is not a consequence.
    let observation = case.observe("1", "unknown");
    case.one(
        "assessment.add",
        json!({"subject_ref": hypothesis, "evidence_refs": [observation],
            "relation": "inconclusive", "rationale": "Fixture."}),
    );
    assert_eq!(case.stalls("run.repeated_same_failure").len(), 1);
    assert!(last.is_some());
    // Retiring a hypothesis is a consequence: the stall is no longer reported.
    case.one(
        "hypothesis.disposition",
        json!({"hypothesis_ref": hypothesis, "lifecycle": "retired",
            "rationale": "No longer useful.", "evidence_refs": []}),
    );
    assert!(case.stalls("run.repeated_same_failure").is_empty());
}

#[test]
fn completed_runs_assessed_only_inconclusive_share_a_failed_prerequisite() {
    let mut case = Case::new("explain");
    let question = case.question();
    let hypothesis = case.hypothesis(question, "The cache is cold.");
    let plan = case.plan(question, "Replay the slow request.");
    for index in 0..STALL_RUN_FAILURES {
        let run = case.run(plan, "completed");
        assert!(
            case.stalls("run.repeated_same_failure").is_empty(),
            "{index}"
        );
        case.one(
            "assessment.add",
            json!({"subject_ref": hypothesis, "evidence_refs": [run],
                "relation": "non_diagnostic", "rationale": "Fixture."}),
        );
    }
    let gap = &case.stalls("run.repeated_same_failure")[0];
    let stall = gap.stall.as_ref().unwrap();
    assert!(stall.journal_items.iter().all(|item| item.fact
        == "run_failed:assessment_relation:inconclusive"
        && item.refs.len() == 2));
}

#[test]
fn collection_without_consequence_is_one_definition_for_the_gap_and_the_check() {
    let mut case = Case::new("explore");
    let mut collected = Vec::new();
    for value in 1..STALL_COLLECTION_TRANSACTIONS {
        collected.push(case.observe(&value.to_string(), "unknown"));
    }
    assert!(
        case.stalls("case.collection_without_consequence")
            .is_empty()
    );
    collected.push(case.observe("4", "unknown"));
    let gap = &case.stalls("case.collection_without_consequence")[0];
    assert_eq!(gap.subject_refs, vec![case.case_ref()]);
    assert_non_fabricating(gap);
    let stall = gap.stall.as_ref().unwrap();
    let expected: Vec<(u64, Vec<RevisionRef>)> = collected
        .iter()
        .map(|observation| (case.sequence(*observation), vec![*observation]))
        .collect();
    let cited: Vec<(u64, Vec<RevisionRef>)> = stall
        .journal_items
        .iter()
        .map(|item| (item.sequence, item.refs.clone()))
        .collect();
    assert_eq!(cited, expected);

    // The blindspot check counts exactly the same transactions.
    let response = audit(
        &case.state,
        &case.snapshot(),
        case.case.unwrap(),
        &AuditRequest::default(),
    )
    .unwrap();
    let finding = response
        .checks
        .iter()
        .find(|finding| finding.check_id == "case.collection_without_consequence")
        .unwrap();
    for (sequence, refs) in &expected {
        let fact = format!("collection_transaction:{sequence}");
        assert!(
            finding
                .facts
                .iter()
                .any(|found| found.fact == fact && &found.refs == refs),
            "{fact}"
        );
    }

    // A recording session does not stall; the blindspot check still reports.
    let mut record = Case::new("record");
    for value in 0..STALL_COLLECTION_TRANSACTIONS {
        record.observe(&value.to_string(), "unknown");
    }
    assert!(
        record
            .inventory()
            .gaps
            .iter()
            .all(|gap| gap.stall.is_none())
    );

    // Pausing the case is the stop: nothing is reported afterwards.
    let current = case.case_ref();
    case.with(
        "case.pause",
        json!({"reason": "No new evidence is expected."}),
        vec![current],
    );
    assert!(case.inventory().gaps.iter().all(|gap| gap.stall.is_none()));
}

#[test]
fn resolving_every_question_ends_every_stall() {
    let mut case = Case::new("explore");
    let question = case.question();
    for value in 0..STALL_COLLECTION_TRANSACTIONS {
        case.observe(&value.to_string(), "unknown");
    }
    assert_eq!(case.stalls("case.collection_without_consequence").len(), 1);
    case.one(
        "question.resolve",
        json!({"question_ref": question, "disposition": "inconclusive",
            "reason": "The readings do not separate the explanations.", "answer": null,
            "evidence_refs": []}),
    );
    assert!(case.inventory().gaps.iter().all(|gap| gap.stall.is_none()));
}

#[test]
fn identical_bytes_acquired_again_and_again_are_reported_on_the_first_artifact() {
    let mut case = Case::new("explore");
    let mut artifacts = Vec::new();
    for _ in 0..STALL_IDENTICAL_ACQUISITIONS {
        let source = case.image(Kind::Source, json!({"access": "local_only"}));
        artifacts.push(case.image(
            Kind::Artifact,
            json!({"digest": DIGEST, "size_bytes": 64, "format": "text/plain",
                "source": source, "transforms": [], "availability": "retained",
                "integrity": "verified", "sensitivity": "internal", "role": "original"}),
        ));
    }
    let stalls = case.stalls("artifact.repeated_identical_bytes");
    assert_eq!(stalls.len(), 1, "{stalls:#?}");
    let gap = &stalls[0];
    assert_eq!(gap.subject_refs, vec![artifacts[0]]);
    assert_non_fabricating(gap);
    let stall = gap.stall.as_ref().unwrap();
    assert_eq!(stall.journal_items.len(), artifacts.len());
    for (item, artifact) in stall.journal_items.iter().zip(&artifacts) {
        assert_eq!(item.sequence, case.sequence(*artifact));
        assert!(item.refs.contains(artifact));
    }
    let inspect = stall
        .moves
        .iter()
        .find(|stall_move| stall_move.kind == StallMoveKind::InspectRawData)
        .unwrap();
    assert_eq!(inspect.target_refs, vec![artifacts[0]]);
    assert_eq!(inspect.operation, None);

    // Repeat outputs that a Run lists are not import attempts.
    let mut listed = Case::new("explore");
    let mut refs = Vec::new();
    for _ in 0..STALL_IDENTICAL_ACQUISITIONS {
        let source = listed.image(Kind::Source, json!({"access": "local_only"}));
        refs.push(listed.image(
            Kind::Artifact,
            json!({"digest": DIGEST, "size_bytes": 64, "source": source,
                "availability": "retained", "integrity": "verified"}),
        ));
    }
    listed.one(
        "run.record",
        json!({"intent": {"kind": "exploration", "intent": "Repeat the capture."},
            "producer": "agent:fixture", "artifact_refs": refs,
            "execution_status": "completed", "evidence_completeness": "complete",
            "evidence_kind": "observational", "variables_changed": []}),
    );
    assert!(
        listed
            .stalls("artifact.repeated_identical_bytes")
            .is_empty()
    );
}

#[test]
fn an_invalid_observation_left_unchanged_is_an_unchanged_blocker() {
    let mut case = Case::new("explain");
    let question = case.question();
    let invalid = case.observe("broken", "invalid");
    for index in 1..STALL_BLOCKER_TRANSACTIONS {
        case.hypothesis(question, &format!("Account {index}."));
    }
    assert!(case.stalls("case.blockers_unchanged").is_empty());
    case.hypothesis(question, "One more account.");
    let gap = &case.stalls("case.blockers_unchanged")[0];
    assert_non_fabricating(gap);
    let stall = gap.stall.as_ref().unwrap();
    assert_eq!(stall.journal_items.len(), 1);
    assert_eq!(stall.journal_items[0].refs, vec![invalid]);
    assert_eq!(stall.journal_items[0].sequence, case.sequence(invalid));
    assert_eq!(
        stall.journal_items[0].fact,
        format!(
            "blocker_unchanged:observation_invalid:transactions_since:{STALL_BLOCKER_TRANSACTIONS}"
        )
    );
    let verify = stall
        .moves
        .iter()
        .find(|stall_move| stall_move.kind == StallMoveKind::VerifyInstrument)
        .unwrap();
    assert_eq!(verify.operation.as_deref(), Some("measurement.assess"));
    assert_eq!(verify.target_refs, vec![invalid]);
    // Retracting the blocker changes it, so it is no longer unchanged.
    case.one(
        "observation.retract",
        json!({"observation_ref": invalid, "revision_reason": "The reading was transcribed wrongly."}),
    );
    assert!(case.stalls("case.blockers_unchanged").is_empty());
}

#[test]
fn every_plan_retired_or_inconclusive_exhausts_the_candidate_tests() {
    let mut case = Case::new("explain");
    let question = case.question();
    let assumption = case.one(
        "assumption.create",
        json!({"claim": "The capture reflects production load.",
            "consequence": "Replays would show the same latency.",
            "materiality": "material", "affected_refs": [question]}),
    );
    let first = case.plan(question, "Replay the slow request.");
    let second = case.plan(question, "Profile the cache.");
    case.run(first, "failed");
    assert!(case.stalls("case.planned_inquiries_exhausted").is_empty());
    case.one(
        "inquiry.retire",
        json!({"inquiry_ref": second, "rationale": "The profiler is unavailable."}),
    );
    let gap = &case.stalls("case.planned_inquiries_exhausted")[0];
    assert_eq!(gap.subject_refs, vec![case.case_ref()]);
    assert_non_fabricating(gap);
    let stall = gap.stall.as_ref().unwrap();
    let facts: Vec<&str> = stall
        .journal_items
        .iter()
        .map(|item| item.fact.as_str())
        .collect();
    assert_eq!(
        facts,
        vec!["plan_inconclusive:execution_status:failed", "plan_retired"]
    );
    let revisit = stall
        .moves
        .iter()
        .find(|stall_move| stall_move.kind == StallMoveKind::RevisitAssumption)
        .unwrap();
    assert_eq!(revisit.operation.as_deref(), Some("assumption.assess"));
    assert_eq!(revisit.target_refs, vec![assumption]);
    assert!(stall.moves.iter().any(|stall_move| stall_move.kind
        == StallMoveKind::RequestMissingEvidence
        && stall_move.operation.is_none()
        && stall_move.external_effects == "consumer_managed"));
    // A plan with no Run yet is still a candidate test.
    case.plan(question, "Compare cold and warm requests.");
    assert!(case.stalls("case.planned_inquiries_exhausted").is_empty());
}

#[test]
fn stall_detection_is_deterministic() {
    let mut case = Case::new("explain");
    let question = case.question();
    case.hypothesis(question, "The cache is cold.");
    let plan = case.plan(question, "Replay the slow request.");
    for _ in 0..STALL_RUN_FAILURES {
        case.run(plan, "blocked");
        case.observe("x", "unknown");
    }
    let first = serde_json::to_vec(&case.inventory()).unwrap();
    let second = serde_json::to_vec(&case.inventory()).unwrap();
    assert_eq!(first, second);
    let rules: Vec<String> = case
        .inventory()
        .gaps
        .into_iter()
        .filter(|gap| gap.stall.is_some())
        .map(|gap| gap.rule_id)
        .collect();
    assert_eq!(
        rules,
        vec![
            // The first blocked Run has stayed unchanged for 5 transactions.
            "case.blockers_unchanged".to_owned(),
            "case.collection_without_consequence".to_owned(),
            "run.repeated_same_failure".to_owned(),
        ]
    );
}
