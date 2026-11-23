use serde_json::{Value, json};

use super::*;
use crate::{
    catalog::standard_catalog,
    command::Command,
    header::SystemClock,
    identity::{Id, SystemIdSource},
    state::{Acceptance, AcceptedTransaction, validate_and_apply},
};

struct Case {
    state: State,
    case: Option<Id>,
}

impl Case {
    fn new() -> Self {
        let mut test = Self {
            state: State::default(),
            case: None,
        };
        let accepted = test
            .submit(
                "case.create",
                json!({"title": "Coverage study", "mode": "record"}),
            )
            .unwrap();
        test.case = Some(accepted.case_id);
        test
    }

    fn submit(&mut self, op: &str, payload: Value) -> Result<AcceptedTransaction, Failure> {
        let sequence = self.state.sequence() + 1;
        let command: Command = serde_json::from_value(json!({
            "schema_version": "inv.command.v1",
            "command_id": format!("command-{sequence}"),
            "case_id": self.case,
            "actor": "agent:test",
            "preconditions": [],
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
        )?;
        self.state.apply(&accepted)?;
        Ok(accepted)
    }

    fn create(&mut self, op: &str, payload: Value) -> RevisionRef {
        self.submit(op, payload)
            .unwrap()
            .records
            .last()
            .unwrap()
            .reference()
    }

    fn run(&mut self, status: &str) -> RevisionRef {
        self.create(
            "run.record",
            json!({
                "intent": {"kind": "exploration", "intent": "Try the cell."},
                "producer": "agent:producer",
                "environment": null,
                "artifact_refs": [],
                "execution_status": status,
                "evidence_completeness": "complete",
                "evidence_kind": "observational",
                "variables_changed": [],
            }),
        )
    }

    fn observation(&mut self) -> RevisionRef {
        self.create(
            "observation.add",
            json!({
                "value": {"kind": "measurement", "value": "12.5", "unit": "unit"},
                "basis": {"kind": "assertion", "assertion": "Reported by the recorder.", "context": null},
                "extraction_method": null,
                "validity": "unknown",
                "quality": [],
                "limitations": [],
                "evidence_kind": "observational",
                "variables_changed": [],
                "observed_at": null,
                "run_ref": null,
            }),
        )
    }

    fn plan(&self, reference: RevisionRef) -> PlanEvaluation {
        let record = self.state.current(reference.id).unwrap();
        evaluate(&self.state, record).unwrap()
    }

    fn current(&self, reference: RevisionRef) -> RevisionRef {
        self.state.current(reference.id).unwrap().reference()
    }

    fn link(
        &mut self,
        plan: RevisionRef,
        cell: Value,
        state: &str,
        refs: Value,
    ) -> Result<AcceptedTransaction, Failure> {
        let plan = self.current(plan);
        self.submit(
            "coverage.link",
            json!({"plan_ref": plan, "links": [{
                "cell": cell, "state": state, "record_refs": refs,
                "rationale": "Recorded attempt in this cell."
            }]}),
        )
    }
}

fn general() -> Value {
    json!({
        "title": "Conditions by component",
        "plan_kind": "general",
        "applicability": "Current branch on the recorded hosts.",
        "dimensions": [
            {"name": "condition", "values": [{"key": "cold"}, {"key": "warm"}]},
            {"name": "component", "values": [{"key": "parser"}, {"key": "cache"}, {"key": "writer"}]}
        ],
        "predicate": "The failure occurs under the condition in the component.",
        "polarity": {"positive": "The failure occurred.", "negative": "The failure did not occur within scope."},
        "exclusions": [{"matching": {"condition": "cold", "component": "writer"}, "reason": "Not loaded cold."}]
    })
}

fn code(failure: &Failure) -> ErrorCode {
    failure.error.code
}

fn cell_state(evaluation: &PlanEvaluation, condition: &str, component: &str) -> CellView {
    evaluation
        .cells
        .iter()
        .find(|cell| cell.at["condition"] == condition && cell.at["component"] == component)
        .cloned()
        .unwrap_or_else(|| panic!("no listed cell {condition}/{component}: {evaluation:?}"))
}

#[test]
fn a_percentage_carries_its_plan_revision_and_denominator() {
    let mut case = Case::new();
    let plan = case.create("coverage.define", general());
    let fresh = case.plan(plan);
    assert_eq!(
        (fresh.cartesian_size, fresh.excluded, fresh.denominator),
        (6, 1, 5)
    );
    assert_eq!(fresh.counts["untested"], 5);
    assert_eq!(fresh.counts["not_applicable_with_reason"], 1);
    assert_eq!(fresh.counts.len(), CellState::ALL.len());
    assert_eq!(fresh.denominator_history.len(), 1);

    let run = case.run("completed");
    case.link(
        plan,
        json!({"condition": "warm", "component": "cache"}),
        "observed_positive",
        json!([run]),
    )
    .unwrap();
    // A blocked cell may cite nothing; it is activity, not coverage.
    case.link(
        plan,
        json!({"condition": "cold", "component": "parser"}),
        "blocked",
        json!([]),
    )
    .unwrap();
    let evaluation = case.plan(plan);
    assert_eq!(evaluation.assessed, 1);
    assert_eq!(evaluation.plan_revision, 1);
    let percentage = json!(evaluation.percentage.as_ref().unwrap());
    assert_eq!(percentage["percent"], "20.0");
    assert_eq!(percentage["denominator"], 5);
    assert_eq!(percentage["plan_revision"], 1);
    assert_eq!(percentage["plan_ref"]["id"], json!(plan.id));
    let statement = percentage["statement"].as_str().unwrap();
    assert!(statement.contains("1 of 5") && statement.contains("plan revision 1"));
    assert_eq!(evaluation.activity.links_recorded, 2);
    let warm = cell_state(&evaluation, "warm", "cache");
    assert_eq!(warm.state, Some(CellState::ObservedPositive));
    assert_eq!(warm.result.as_deref(), Some("The failure occurred."));
    assert_eq!(
        cell_state(&evaluation, "cold", "parser").state,
        Some(CellState::Blocked)
    );

    // Everywhere a percentage is serialized, its revision and denominator
    // sit beside it.
    let record = case.state.current(plan.id).unwrap();
    let case_record = case.state.current(case.case.unwrap()).unwrap();
    for view in [
        projection(&case.state, record, true, &|_| true).unwrap(),
        projection(&case.state, case_record, false, &|_| true).unwrap(),
    ] {
        assert_percentages_are_qualified(&view);
        assert!(text(&json!({"coverage": view})).contains("1 of 5"));
    }
}

fn assert_percentages_are_qualified(value: &Value) {
    match value {
        Value::Object(map) => {
            if map.contains_key("percent") {
                for key in ["plan_revision", "denominator", "plan_ref", "statement"] {
                    assert!(
                        map.contains_key(key),
                        "{key} missing beside percent: {value}"
                    );
                }
            }
            map.values().for_each(assert_percentages_are_qualified);
        }
        Value::Array(items) => items.iter().for_each(assert_percentages_are_qualified),
        _ => {}
    }
}

#[test]
fn invalidating_a_linked_measurement_moves_its_cell_to_needs_retest() {
    let mut case = Case::new();
    let plan = case.create("coverage.define", general());
    let observation = case.observation();
    case.link(
        plan,
        json!({"condition": "cold", "component": "cache"}),
        "observed_negative_within_scope",
        json!([observation]),
    )
    .unwrap();
    assert_eq!(case.plan(plan).assessed, 1);
    case.submit(
        "measurement.assess",
        json!({
            "observation_ref": observation, "channel": null, "validity": "invalid",
            "verification": [{"kind": "equivalent", "justification": "Dropped samples in the window."}],
            "rationale": "The value cannot be interpreted.", "limits": []
        }),
    )
    .unwrap();
    let evaluation = case.plan(plan);
    assert_eq!(evaluation.assessed, 0);
    assert_eq!(evaluation.counts["needs_retest"], 1);
    let cell = cell_state(&evaluation, "cold", "cache");
    assert_eq!(cell.state, Some(CellState::NeedsRetest));
    assert_eq!(
        cell.recorded_state,
        Some(LinkState::ObservedNegativeWithinScope)
    );
    assert_eq!(
        cell.retest,
        vec![RetestReason::LinkedRecordChanged {
            record_ref: observation,
            trigger: Trigger::InvalidMeasurement,
        }]
    );
    assert_eq!(json!(evaluation.percentage.unwrap())["percent"], "0.0");

    // An invalid measurement cannot newly assess a cell.
    let current = case.current(observation);
    let failure = case
        .link(
            plan,
            json!({"condition": "warm", "component": "cache"}),
            "observed_positive",
            json!([current]),
        )
        .unwrap_err();
    assert_eq!(code(&failure), ErrorCode::GateFailed);
}

#[test]
fn a_revised_cell_definition_needs_retest_and_the_denominator_history_is_kept() {
    let mut case = Case::new();
    let plan = case.create("coverage.define", general());
    let run = case.run("completed");
    case.link(
        plan,
        json!({"condition": "warm", "component": "parser"}),
        "observed_positive",
        json!([run]),
    )
    .unwrap();
    case.link(
        plan,
        json!({"condition": "warm", "component": "cache"}),
        "observed_positive",
        json!([run]),
    )
    .unwrap();
    let mut revised = general();
    revised["cells"] = json!([{
        "at": {"condition": "warm", "component": "parser"},
        "predicate": "The failure occurs in the parser after a warm restart with a full cache."
    }]);
    revised["exclusions"] = json!([]);
    let current = case.current(plan);
    let accepted = case
        .submit(
            "coverage.revise",
            json!({"plan_ref": current, "definition": revised, "reason": "Sharpen the parser cell."}),
        )
        .unwrap();
    assert!(
        accepted
            .warnings
            .iter()
            .any(|warning| warning.contains("coverage.cells_need_retest: 1")),
        "{:?}",
        accepted.warnings
    );
    let evaluation = case.plan(plan);
    assert_eq!(evaluation.plan_revision, 2);
    assert_eq!(evaluation.denominator, 6);
    let history: Vec<_> = evaluation
        .denominator_history
        .iter()
        .map(|entry| (entry.plan_revision, entry.denominator))
        .collect();
    assert_eq!(history, vec![(1, 5), (2, 6)]);
    let parser = cell_state(&evaluation, "warm", "parser");
    assert_eq!(parser.state, Some(CellState::NeedsRetest));
    assert_eq!(
        parser.retest,
        vec![RetestReason::CellDefinitionChanged {
            linked_plan_revision: 1,
            plan_revision: 2
        }]
    );
    // An untouched cell keeps its result across the revision.
    assert_eq!(
        cell_state(&evaluation, "warm", "cache").state,
        Some(CellState::ObservedPositive)
    );
    let percentage = json!(evaluation.percentage.unwrap());
    assert_eq!(
        (&percentage["plan_revision"], &percentage["denominator"]),
        (&json!(2), &json!(6))
    );

    // Revising to the same definition is refused.
    let current = case.current(plan);
    let stored = stored(case.state.current(plan.id).unwrap()).unwrap();
    let failure = case
        .submit(
            "coverage.revise",
            json!({"plan_ref": current, "definition": stored.definition, "reason": "Again."}),
        )
        .unwrap_err();
    assert_eq!(code(&failure), ErrorCode::InvalidInput);
}

#[test]
fn an_oversized_plan_is_rejected_with_advice_to_narrow() {
    let mut case = Case::new();
    let values = |n: usize| -> Value {
        (0..n)
            .map(|index| json!({"key": format!("v{index}")}))
            .collect()
    };
    let mut definition = general();
    definition["exclusions"] = json!([]);
    definition["dimensions"] = json!([
        {"name": "a", "values": values(64)},
        {"name": "b", "values": values(65)}
    ]);
    let failure = case
        .submit("coverage.define", definition.clone())
        .unwrap_err();
    assert_eq!(code(&failure), ErrorCode::InvalidInput);
    assert_eq!(
        failure.error.path.as_deref(),
        Some("/operations/0/payload/dimensions")
    );
    assert!(failure.error.message.contains("4160"));
    assert!(failure.error.message.contains("Narrow it"));
    assert_eq!(
        failure.error.actual.as_ref().unwrap()["cartesian_size"],
        4160
    );

    definition["dimensions"][1]["values"] = values(64);
    case.submit("coverage.define", definition).unwrap();

    // A plan whose every cell is excluded has no denominator.
    let mut empty = general();
    empty["exclusions"] = json!([{"matching": {"condition": "cold"}, "reason": "Out."},
        {"matching": {"condition": "warm"}, "reason": "Out."}]);
    assert_eq!(
        code(&case.submit("coverage.define", empty).unwrap_err()),
        ErrorCode::InvalidInput
    );
}

#[test]
fn links_are_checked_against_the_plan_and_the_records() {
    let mut case = Case::new();
    let plan = case.create("coverage.define", general());
    let failed = case.run("failed");
    let completed = case.run("completed");
    for (cell, state, refs, expected) in [
        (
            json!({"condition": "cold", "component": "writer"}),
            "observed_positive",
            json!([completed]),
            ErrorCode::GateFailed,
        ),
        (
            json!({"condition": "hot", "component": "cache"}),
            "observed_positive",
            json!([completed]),
            ErrorCode::InvalidInput,
        ),
        (
            json!({"condition": "warm"}),
            "observed_positive",
            json!([completed]),
            ErrorCode::InvalidInput,
        ),
        (
            json!({"condition": "warm", "component": "cache"}),
            "observed_positive",
            json!([failed]),
            ErrorCode::GateFailed,
        ),
        (
            json!({"condition": "warm", "component": "cache"}),
            "attempted_inconclusive",
            json!([]),
            ErrorCode::InvalidInput,
        ),
        (
            json!({"condition": "warm", "component": "cache"}),
            "observed_positive",
            json!([plan]),
            ErrorCode::InvalidInput,
        ),
    ] {
        let failure = case.link(plan, cell.clone(), state, refs).unwrap_err();
        assert_eq!(code(&failure), expected, "{cell} {state}: {failure:?}");
    }
    // A failed run is an inconclusive attempt, not an observation.
    case.link(
        plan,
        json!({"condition": "warm", "component": "cache"}),
        "attempted_inconclusive",
        json!([failed]),
    )
    .unwrap();
    let evaluation = case.plan(plan);
    assert_eq!(evaluation.assessed, 0);
    assert_eq!(evaluation.counts["attempted_inconclusive"], 1);
    // A stale plan revision is a conflict.
    let failure = case
        .submit(
            "coverage.link",
            json!({"plan_ref": plan, "links": [{
                "cell": {"condition": "warm", "component": "parser"}, "state": "blocked",
                "record_refs": [], "rationale": "Host unavailable."
            }]}),
        )
        .unwrap_err();
    assert_eq!(code(&failure), ErrorCode::RevisionConflict);
}

fn criterion(question: RevisionRef, limit: &str) -> Value {
    json!({
        "name": "Latency", "kind": "hard", "preferred_direction": "lower",
        "scale": {"anchors": [], "description": "p99 latency", "unit": "ms"},
        "scope": {"kind": "all_options", "question_ref": question},
        "threshold": {"kind": "measured", "operator": "at_most", "value": {"decimal": limit, "unit": "ms"}}
    })
}

#[test]
fn an_evaluation_plan_reads_as_labels_and_a_changed_criterion_makes_cells_stale() {
    let mut case = Case::new();
    let question = case.create(
        "question.create",
        json!({"importance": "high", "wording": "Which queue?"}),
    );
    let mut options = Vec::new();
    for name in ["Managed queue", "Broker"] {
        let option = case.create(
            "option.create",
            json!({"question_ref": question, "description": name, "dependencies": [], "tradeoff_notes": []}),
        );
        options.push(case.create(
            "option.disposition",
            json!({"option_ref": option, "lifecycle": "eligible", "reason": "Feasible"}),
        ));
    }
    let latency = case.create("criterion.create", criterion(question, "25"));
    let definition = |latency: RevisionRef| {
        json!({
            "title": "Queue options by criterion",
            "plan_kind": "evaluation",
            "question_ref": question,
            "applicability": "The two shortlisted queues under the recorded load.",
            "dimensions": [
                {"name": "option", "values": [
                    {"key": "managed", "record_ref": options[0]},
                    {"key": "broker", "record_ref": options[1]}
                ]},
                {"name": "criterion", "values": [{"key": "latency", "record_ref": latency}]}
            ],
            "predicate": "The option meets the criterion.",
            "polarity": {"positive": "Meets the criterion.", "negative": "Does not meet the criterion."}
        })
    };
    // An evaluation plan needs an option and a criterion dimension.
    let mut lopsided = definition(latency);
    lopsided["dimensions"][1]["values"][0]
        .as_object_mut()
        .unwrap()
        .remove("record_ref");
    assert_eq!(
        code(&case.submit("coverage.define", lopsided).unwrap_err()),
        ErrorCode::InvalidInput
    );
    let plan = case.create("coverage.define", definition(latency));
    assert_eq!(
        case.state.current(plan.id).unwrap().body["subject_refs"],
        json!([options[0], options[1], latency])
    );
    let observation = case.observation();
    let comparison = case.create(
        "comparison.add",
        json!({
            "option_ref": options[0], "criterion_ref": latency, "outcome": "meets",
            "method": "Benchmark", "value": {"decimal": "20", "unit": "ms"},
            "evidence_refs": [observation], "assumption_refs": [],
            "uncertainty": "One run.", "rationale": "Measured p99 is below the limit."
        }),
    );
    // A comparison of another option does not assess this cell.
    let failure = case
        .link(
            plan,
            json!({"option": "broker", "criterion": "latency"}),
            "observed_positive",
            json!([comparison]),
        )
        .unwrap_err();
    assert_eq!(code(&failure), ErrorCode::GateFailed);
    case.link(
        plan,
        json!({"option": "managed", "criterion": "latency"}),
        "observed_positive",
        json!([comparison]),
    )
    .unwrap();
    let evaluation = case.plan(plan);
    assert_eq!(
        json!(evaluation.counts),
        json!({"assessed": 1, "excluded": 0, "stale": 0, "unknown": 1})
    );
    assert_eq!(
        json!(evaluation.unknown_because),
        json!({"attempted_inconclusive": 0, "blocked": 0, "untested": 1})
    );
    let view = json!(evaluation);
    let rendered = view.to_string();
    assert!(!rendered.contains("observed_positive"), "{rendered}");
    assert!(!rendered.contains("observed_negative"), "{rendered}");
    assert_eq!(view["cells"][0]["label"], "assessed");
    assert_eq!(view["cells"][0]["result"], "Meets the criterion.");

    // A material criterion revision makes the dependent cell stale.
    let amended = case.create(
        "criterion.amend",
        json!({"criterion_ref": latency, "reason": "Tighter limit.", "replacement": criterion(question, "15")}),
    );
    let evaluation = case.plan(plan);
    assert_eq!(evaluation.counts["stale"], 1);
    assert_eq!(evaluation.assessed, 0);
    assert_eq!(evaluation.stale_coordinates.len(), 1);
    assert_eq!(evaluation.stale_coordinates[0].record_ref, latency);
    assert!(matches!(
        evaluation.cells[0].retest.as_slice(),
        [RetestReason::CoordinateChanged { record_ref, .. }] if *record_ref == latency
    ));
    // Linking against the stale coordinate is refused until the plan pins it.
    let failure = case
        .link(
            plan,
            json!({"option": "broker", "criterion": "latency"}),
            "blocked",
            json!([]),
        )
        .unwrap_err();
    assert_eq!(code(&failure), ErrorCode::GateFailed);
    let current = case.current(plan);
    case.submit(
        "coverage.revise",
        json!({"plan_ref": current, "definition": definition(amended), "reason": "Pin the amended criterion."}),
    )
    .unwrap();
    let evaluation = case.plan(plan);
    assert!(evaluation.stale_coordinates.is_empty());
    assert_eq!(evaluation.counts["stale"], 1);
    assert!(matches!(
        evaluation.cells[0].retest.as_slice(),
        [RetestReason::CellDefinitionChanged { .. }]
    ));
}

#[test]
fn a_case_without_a_plan_has_no_coverage_projection() {
    let mut case = Case::new();
    let question = case.create(
        "question.create",
        json!({"importance": "high", "wording": "Why?"}),
    );
    let case_record = case.state.current(case.case.unwrap()).unwrap();
    assert!(projection(&case.state, case_record, true, &|_| true).is_none());
    let question = case.state.current(question.id).unwrap();
    assert!(projection(&case.state, question, true, &|_| true).is_none());
    assert_eq!(text(&json!({"title": "x"})), "");

    let plan = case.create("coverage.define", general());
    let case_record = case.state.current(case.case.unwrap()).unwrap();
    assert!(projection(&case.state, case_record, false, &|_| true).is_some());
    // A plan the audience may not see is not listed, so no section appears.
    assert!(
        projection(&case.state, case_record, false, &|record| record.header.id
            != plan.id)
        .is_none()
    );
}
