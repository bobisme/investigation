use serde_json::{Value, json};

use super::*;
use crate::{
    catalog::standard_catalog,
    command::Command,
    header::{RecordHeader, Revision, SchemaVersion, SystemClock},
    identity::SystemIdSource,
    state::{Acceptance, AcceptedTransaction, validate_and_apply},
    wire::RecordChange,
};

fn reference(id: Id) -> Value {
    json!({"id":id,"revision":1})
}

/// A hand-built decision table whose measured cells carry retained, verified
/// evidence, so the analysis can score them.
struct Table {
    state: State,
    case: Id,
    question: Id,
    artifact: Id,
    digest: String,
    counter: u64,
}

impl Table {
    fn new() -> Self {
        let mut table = Self {
            state: State::default(),
            case: "case_018d87fc-5234-7000-8000-000000000001".parse().unwrap(),
            question: "q_018d87fc-5234-7000-8000-000000000002".parse().unwrap(),
            artifact: "art_018d87fc-5234-7000-8000-000000000003".parse().unwrap(),
            digest: format!("sha256:{}", "a".repeat(64)),
            counter: 16,
        };
        let source: Id = "src_018d87fc-5234-7000-8000-000000000004".parse().unwrap();
        table.put(table.case, json!({}));
        table.put(table.question, json!({}));
        table.put(source, json!({}));
        let artifact = json!({"source":reference(source),"digest":table.digest,"size_bytes":12,
            "format":"application/json","transforms":[],"availability":"retained",
            "integrity":"verified","sensitivity":"internal"});
        table.put(table.artifact, artifact);
        table
    }

    fn id(&mut self, prefix: &str) -> Id {
        self.counter += 1;
        format!("{prefix}_018d87fc-5234-7000-8000-{:012x}", self.counter)
            .parse()
            .unwrap()
    }

    fn put(&mut self, id: Id, body: Value) {
        let sequence = self.state.sequence() + 1;
        let record = Record {
            header: RecordHeader {
                id,
                case_id: self.case,
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
                history: vec![],
                classification: Default::default(),
                provenance: Default::default(),
                extensions: Default::default(),
            },
            body,
        };
        self.state
            .apply(&AcceptedTransaction {
                case_id: self.case,
                sequence,
                records: vec![record],
                created: vec![RecordChange {
                    client_ref: None,
                    id,
                    revision: 1,
                }],
                updated: vec![],
                warnings: vec![],
            })
            .unwrap();
    }

    fn revise(&mut self, id: Id, body: Value) {
        let mut record = self.state.current(id).unwrap().clone();
        record.header.acceptance_sequence = self.state.sequence() + 1;
        record.header.history.push(record.header.revision);
        record.header.revision.0 += 1;
        record.body = body;
        let change = RecordChange {
            client_ref: None,
            id,
            revision: record.header.revision.0,
        };
        self.state
            .apply(&AcceptedTransaction {
                case_id: self.case,
                sequence: record.header.acceptance_sequence,
                records: vec![record],
                created: vec![],
                updated: vec![change],
                warnings: vec![],
            })
            .unwrap();
    }

    fn option(&mut self) -> Id {
        let id = self.id("opt");
        let body = json!({"question_ref":reference(self.question),"description":format!("Option {id}"),
            "context":null,"feasibility":null,"dependencies":[],"tradeoff_notes":[],"lifecycle":"eligible",
            "disposition_reason":null,"amendment_reason":null,"criterion_ref":null});
        self.put(id, body);
        id
    }

    fn criterion(&mut self, kind: &str, unit: &str, weight: Value) -> Id {
        let id = self.id("crit");
        let threshold = if kind == "hard" {
            json!({"kind":"measured","operator":"at_most","value":{"decimal":"25","unit":unit}})
        } else {
            Value::Null
        };
        let mut body = json!({"name":format!("Criterion {id}"),"kind":kind,"question_ref":reference(self.question),
            "option_refs":null,"scale":{"unit":unit,"description":"Lower is better","anchors":[]},
            "preferred_direction":"lower","threshold":threshold,"weighting_rationale":null,
            "lifecycle":"active","change_reason":null,"material_change":false});
        if !weight.is_null() {
            body["weight"] = weight;
        }
        self.put(id, body);
        id
    }

    /// A comparison backed by a retained, verified measurement.
    fn measured(&mut self, option: Id, criterion: Id, value: &str, outcome: &str) -> Id {
        let unit = self.state.current(criterion).unwrap().body["scale"]["unit"].clone();
        let observation = self.id("obs");
        let body = json!({
            "value":{"kind":"measurement","value":value,"unit":unit},
            "basis":{"kind":"source","artifact_ref":reference(self.artifact),"selector":{"schema_version":"inv.selector.v1",
                "artifact_digest":self.digest,"selector":{"type":"whole"},"validation":"verified"},
                "text_quote":{"exact":null,"prefix":null,"suffix":null,"state":"not_applicable"}},
            "validity":"valid","quality":[],"limitations":[],"variables_changed":[],
        });
        self.put(observation, body);
        self.cell(
            option,
            criterion,
            json!({"outcome":outcome,"value":{"decimal":value,"unit":unit},"basis":null,
                "evidence_refs":[reference(observation)]}),
        )
    }

    fn cell(&mut self, option: Id, criterion: Id, fields: Value) -> Id {
        let id = self.id("cmp");
        let mut body = json!({"option_ref":reference(option),"criterion_ref":reference(criterion),
            "method":"fixture","value":null,"basis":null,"evidence_refs":[],"assumption_refs":[],
            "limitations":null,"uncertainty":"Fixture","rationale":"Fixture","retracted":false,"change_reason":null});
        for (key, value) in fields.as_object().unwrap() {
            body[key] = value.clone();
        }
        self.put(id, body);
        id
    }

    fn analysis(&self) -> WeightedAnalysis {
        weighted_analysis(&self.state, self.case, self.question)
            .unwrap()
            .expect("a weighted question")
    }
}

fn ranged(value: &str, worst: &str, best: &str) -> Value {
    json!({"value":value,"range":{"worst":worst,"best":best},"rationale":"Stated swing","status":"ranged"})
}

fn score(analysis: &WeightedAnalysis, option: Id) -> &OptionScore {
    analysis
        .scores
        .iter()
        .find(|score| score.option_ref.id == option)
        .unwrap()
}

fn total(analysis: &WeightedAnalysis, option: Id) -> f64 {
    score(analysis, option).total.unwrap()
}

#[test]
fn an_unweighted_question_has_no_weighted_analysis() {
    let mut table = Table::new();
    let option = table.option();
    let criterion = table.criterion("soft", "ms", Value::Null);
    table.measured(option, criterion, "20", "meets");
    assert!(
        weighted_analysis(&table.state, table.case, table.question)
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_weight_without_a_range_never_enters_a_score() {
    let mut table = Table::new();
    let latency = table.criterion("soft", "ms", ranged("2", "50", "10"));
    let preference = json!({"value":"5","rationale":"The team cares most about cost",
        "stated_by":"planning interview","status":"stated_preference"});
    let cost = table.criterion("soft", "USD", preference.clone());
    let (a, b) = (table.option(), table.option());
    table.measured(a, latency, "30", "meets");
    table.measured(b, latency, "20", "meets");
    table.measured(a, cost, "100", "meets");
    table.measured(b, cost, "900", "meets");
    let analysis = table.analysis();
    // Only the ranged weight counts: 2 * (50 - 30) / 40 and 2 * (50 - 20) / 40.
    assert_eq!(total(&analysis, a), 1.0);
    assert_eq!(total(&analysis, b), 1.5);
    assert!(analysis.scores.iter().all(|score| {
        score
            .contributions
            .iter()
            .all(|c| c.criterion_ref.id == latency)
    }));
    let entry = analysis
        .weights
        .iter()
        .find(|entry| entry.criterion_ref.id == cost)
        .unwrap();
    assert_eq!(entry.weight.status, WeightStatus::StatedPreference);
    assert!(!entry.enters_score);
    assert_eq!(
        entry.weight.stated_by.as_deref(),
        Some("planning interview")
    );
    assert_eq!(
        analysis.unweighted_criteria,
        vec![RevisionRef {
            id: cost,
            revision: 1
        }]
    );
    // Any value of the unranged weight leaves every total unchanged.
    let mut body = table.state.current(cost).unwrap().body.clone();
    body["weight"]["value"] = json!("500");
    table.revise(cost, body);
    let again = table.analysis();
    assert_eq!(total(&again, a), 1.0);
    assert_eq!(total(&again, b), 1.5);
}

#[test]
fn utilities_use_the_stated_range_so_added_or_duplicate_options_never_reorder_the_others() {
    for seed in 0..32u64 {
        let mut random = seed.wrapping_add(7);
        let mut next = || {
            random = random
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            10 + (random >> 33) % 41
        };
        let rows: Vec<[u64; 2]> = (0..3).map(|_| [next(), next()]).collect();
        let build = |extra: &[[u64; 2]], reverse: bool| {
            let mut table = Table::new();
            let first = table.criterion("soft", "ms", ranged("3", "50", "10"));
            let second = table.criterion("soft", "s", ranged("1", "50", "10"));
            let mut all: Vec<[u64; 2]> = rows.clone();
            all.extend_from_slice(extra);
            let mut order: Vec<usize> = (0..all.len()).collect();
            if reverse {
                order.reverse();
            }
            let mut ids = vec![None; all.len()];
            for index in order {
                let option = table.option();
                table.measured(option, first, &all[index][0].to_string(), "meets");
                table.measured(option, second, &all[index][1].to_string(), "meets");
                ids[index] = Some(option);
            }
            let analysis = table.analysis();
            let totals: Vec<f64> = ids[..rows.len()]
                .iter()
                .map(|id| total(&analysis, id.unwrap()))
                .collect();
            (totals, analysis, ids)
        };
        let (base, analysis, _) = build(&[], false);
        // Anchored to the stated range, never to the options present.
        for (row, value) in rows.iter().zip(&base) {
            let expected = 3.0 * (50.0 - row[0] as f64) / 40.0 + (50.0 - row[1] as f64) / 40.0;
            assert!((value - expected).abs() < 1e-9, "seed {seed}");
        }
        assert_eq!(analysis.sensitivity.len(), 3, "seed {seed}");
        let (reordered, _, _) = build(&[], true);
        assert_eq!(base, reordered, "seed {seed}");
        let (with_duplicate, _, _) = build(&[rows[0]], false);
        assert_eq!(base, with_duplicate, "seed {seed}");
        let (with_extreme, extended, ids) = build(&[[10, 10], [50, 50]], true);
        assert_eq!(base, with_extreme, "seed {seed}");
        // Pairwise verdicts among the original rows are unchanged too.
        let verdicts = |analysis: &WeightedAnalysis, ids: &[Option<Id>]| {
            let row = |id: Id| ids.iter().position(|x| *x == Some(id)).unwrap();
            analysis
                .sensitivity
                .iter()
                .filter_map(|pair| {
                    let (mut a, mut b) = (row(pair.option_refs[0].id), row(pair.option_refs[1].id));
                    let mut difference = pair.difference;
                    if a > b {
                        (a, b) = (b, a);
                        difference = -difference;
                    }
                    (b < rows.len()).then_some(((a, b), (difference, pair.order_can_flip)))
                })
                .collect::<BTreeMap<_, _>>()
        };
        let (_, original, original_ids) = build(&[], false);
        let expected = verdicts(&original, &original_ids);
        assert_eq!(expected.len(), 3, "seed {seed}");
        assert_eq!(verdicts(&extended, &ids), expected, "seed {seed}");
    }
}

#[test]
fn a_hard_failure_or_unknown_is_excluded_and_never_traded_against_soft_weights() {
    let mut table = Table::new();
    let hard = table.criterion("hard", "s", Value::Null);
    let soft = table.criterion("soft", "ms", ranged("100", "50", "10"));
    let (unknown, meets, fails, qualitative) = (
        table.option(),
        table.option(),
        table.option(),
        table.option(),
    );
    table.cell(
        unknown,
        hard,
        json!({"outcome":"unknown","basis":{"kind":"unknown","reason":"Not measured"}}),
    );
    table.measured(meets, hard, "20", "meets");
    table.measured(fails, hard, "40", "fails");
    // A recorded pass without verified evidence is not a pass.
    table.cell(
        qualitative,
        hard,
        json!({"outcome":"meets","basis":{"kind":"qualitative","statement":"Looks fine"}}),
    );
    // The excluded options have the best soft values by far.
    for option in [unknown, fails, qualitative] {
        table.measured(option, soft, "10", "meets");
    }
    table.measured(meets, soft, "49", "meets");
    let analysis = table.analysis();
    let reason = |option: Id| {
        analysis
            .excluded
            .iter()
            .find(|e| e.option_ref.id == option)
            .map(|e| e.reason)
    };
    assert_eq!(reason(unknown), Some(ExclusionReason::HardUnresolved));
    assert_eq!(reason(qualitative), Some(ExclusionReason::HardUnresolved));
    assert_eq!(reason(fails), Some(ExclusionReason::HardFailed));
    assert_eq!(reason(meets), None);
    assert_eq!(analysis.scores.len(), 1);
    assert_eq!(analysis.scores[0].option_ref.id, meets);
    assert!(analysis.sensitivity.is_empty());
    // The excluded options stay in the consequences table with their values.
    assert_eq!(analysis.consequences.rows.len(), 4);
    // A hard criterion takes no weight and is never an unweighted soft one.
    assert!(!analysis.unweighted_criteria.iter().any(|r| r.id == hard));
}

#[test]
fn preferences_stay_labelled_preferences_and_never_score() {
    let mut table = Table::new();
    let soft = table.criterion("soft", "ms", ranged("1", "50", "10"));
    let (a, b) = (table.option(), table.option());
    table.cell(
        a,
        soft,
        json!({"outcome":"meets","basis":{"kind":"preference","statement":"The vendor's documentation says it feels fast"}}),
    );
    table.measured(b, soft, "30", "meets");
    let analysis = table.analysis();
    let cell = &analysis.consequences.rows[0].cells[0];
    assert_eq!(analysis.consequences.rows[0].option_ref.id, a);
    assert_eq!(cell.state, CellState::Preference);
    assert!(cell.value.is_none());
    assert!(matches!(
        cell.basis,
        Some(ComparisonBasis::Preference { .. })
    ));
    let a_score = score(&analysis, a);
    assert_eq!(a_score.status, ScoreStatus::Incomplete);
    assert!(a_score.total.is_none());
    assert_eq!(a_score.missing[0].reason, MissingReason::NotMeasured);
    assert_eq!(score(&analysis, b).status, ScoreStatus::Scored);
    // An incomplete option is never ranked against a scored one.
    assert!(analysis.sensitivity.is_empty());
    // The weight itself is a preference with its attribution.
    let serialized = serde_json::to_value(&analysis).unwrap();
    let keys: Vec<&String> = serialized.as_object().unwrap().keys().collect();
    assert_eq!(keys[0], "consequences", "{keys:?}");
}

#[test]
fn sensitivity_reports_whether_an_order_holds_across_stated_weight_uncertainty() {
    let build = |low: &str, high: &str| {
        let mut table = Table::new();
        let mut speed = ranged("1", "50", "10");
        speed["uncertainty"] = json!({"low":low,"high":high});
        let speed = table.criterion("soft", "ms", speed);
        let cost = table.criterion("soft", "USD", ranged("1", "50", "10"));
        let (fast, cheap) = (table.option(), table.option());
        table.measured(fast, speed, "10", "meets");
        table.measured(fast, cost, "40", "meets");
        table.measured(cheap, speed, "40", "meets");
        table.measured(cheap, cost, "10", "meets");
        (table.analysis(), fast, cheap)
    };
    // At the stated weights the options tie; any uncertainty can flip them.
    let (analysis, fast, cheap) = build("0.5", "2");
    let pair = &analysis.sensitivity[0];
    assert_eq!(pair.preferred_at_stated_weights, None);
    assert!(pair.order_can_flip);
    // 0.5 * 1 + 1 * 0.25 and 0.5 * 0.25 + 1 * 1.
    assert_eq!(score(&analysis, fast).total_low, Some(0.75));
    assert_eq!(score(&analysis, cheap).total_low, Some(1.125));
    assert_eq!(score(&analysis, fast).total, Some(1.25));
    // Stated exactly, the tie stays a tie and is reported as unstable.
    let (analysis, _, _) = build("1", "1");
    assert!(analysis.sensitivity[0].order_can_flip);
    // A heavier speed weight with a narrow interval holds the order.
    let mut table = Table::new();
    let mut speed = ranged("3", "50", "10");
    speed["uncertainty"] = json!({"low":"2","high":"4"});
    let speed = table.criterion("soft", "ms", speed);
    let mut cost = ranged("1", "50", "10");
    cost["uncertainty"] = json!({"low":"0.5","high":"1.5"});
    let cost = table.criterion("soft", "USD", cost);
    let (fast, cheap) = (table.option(), table.option());
    table.measured(fast, speed, "10", "meets");
    table.measured(fast, cost, "40", "meets");
    table.measured(cheap, speed, "40", "meets");
    table.measured(cheap, cost, "10", "meets");
    let analysis = table.analysis();
    let pair = &analysis.sensitivity[0];
    assert_eq!(pair.preferred_at_stated_weights.map(|r| r.id), Some(fast));
    assert!(!pair.order_can_flip);
    // Widen the cost uncertainty until it can outweigh speed.
    let mut body = table.state.current(cost).unwrap().body.clone();
    body["weight"]["uncertainty"]["high"] = json!("9");
    table.revise(cost, body);
    for option in [fast, cheap] {
        let cost_cell = table
            .state
            .records()
            .find(|r| {
                r.header.id.kind() == Kind::Comparison
                    && r.body["option_ref"]["id"] == json!(option)
                    && r.body["criterion_ref"]["id"] == json!(cost)
            })
            .unwrap()
            .header
            .id;
        let mut body = table.state.current(cost_cell).unwrap().body.clone();
        body["criterion_ref"]["revision"] = json!(2);
        table.revise(cost_cell, body);
    }
    let analysis = table.analysis();
    assert!(analysis.sensitivity[0].order_can_flip);
}

#[test]
fn a_changed_weight_puts_its_cells_and_their_decisions_under_review() {
    let mut table = Table::new();
    let soft = table.criterion("soft", "ms", ranged("1", "50", "10"));
    let option = table.option();
    let cell = table.measured(option, soft, "30", "meets");
    assert_eq!(table.analysis().scores[0].status, ScoreStatus::Scored);
    let mut body = table.state.current(soft).unwrap().body.clone();
    body["weight"]["value"] = json!("4");
    table.revise(soft, body);
    let record = table.state.current(cell).unwrap();
    assert_eq!(
        crate::decision_ops::effective_comparison(&table.state, record).unwrap(),
        EffectiveComparison::NeedsReview
    );
    let analysis = table.analysis();
    assert_eq!(
        analysis.consequences.rows[0].cells[0].state,
        CellState::NeedsReview
    );
    assert_eq!(analysis.scores[0].status, ScoreStatus::Incomplete);
}

#[test]
fn values_outside_the_stated_range_are_not_scored() {
    let mut table = Table::new();
    let soft = table.criterion("soft", "ms", ranged("1", "50", "10"));
    let option = table.option();
    table.measured(option, soft, "60", "meets");
    let analysis = table.analysis();
    assert_eq!(
        analysis.scores[0].missing[0].reason,
        MissingReason::OutsideRange
    );
    assert!(analysis.scores[0].total.is_none());
}

#[test]
fn tables_for_a_whole_case_equal_each_question_tabulated_alone() {
    let mut table = Table::new();
    let hard = table.criterion("hard", "ms", Value::Null);
    let soft = table.criterion("soft", "ms", ranged("1", "50", "10"));
    let first = table.option();
    let second = table.option();
    table.measured(first, hard, "20", "meets");
    table.measured(first, soft, "30", "meets");
    table.measured(second, soft, "40", "meets");
    table.cell(second, hard, json!({"outcome":"unknown"}));
    let retracted = table.cell(second, soft, json!({"outcome":"meets"}));
    let mut body = table.state.current(retracted).unwrap().body.clone();
    body["retracted"] = json!(true);
    table.revise(retracted, body);
    let mut body = table.state.current(soft).unwrap().body.clone();
    body["weight"]["value"] = json!("4");
    table.revise(soft, body);
    let untabulated = table.id("q");
    table.put(untabulated, json!({}));

    let index = FreshnessIndex::build(&table.state, table.case);
    let tables = consequences_tables_with(&table.state, &index, table.case).unwrap();
    assert_eq!(tables.keys().copied().collect::<Vec<_>>(), [table.question]);
    let alone = consequences_table(&table.state, table.case, table.question).unwrap();
    let together = tables[&table.question].as_ref().unwrap();
    assert_eq!(
        serde_json::to_value(together).unwrap(),
        serde_json::to_value(&alone).unwrap()
    );
    assert_eq!(alone.rows.len(), 2);
    assert_eq!(alone.criteria.len(), 2);
    assert!(
        consequences_table(&table.state, table.case, untabulated)
            .unwrap()
            .criteria
            .is_empty()
    );
}

/// Operations through the standard catalog.
struct Ops {
    state: State,
    case: Option<Id>,
}

impl Ops {
    fn new() -> (Self, RevisionRef, RevisionRef) {
        let mut ops = Self {
            state: State::default(),
            case: None,
        };
        ops.run("case.create", json!({"title":"Weights","mode":"evaluate"}))
            .unwrap();
        let question = ops
            .run(
                "question.create",
                json!({"importance":"high","wording":"Which design?"}),
            )
            .unwrap()
            .0;
        let other = ops
            .run(
                "question.create",
                json!({"importance":"high","wording":"Which vendor?"}),
            )
            .unwrap()
            .0;
        (ops, question, other)
    }

    fn run(&mut self, op: &str, payload: Value) -> Result<(RevisionRef, Vec<String>), Failure> {
        let sequence = self.state.sequence() + 1;
        let command: Command = serde_json::from_value(json!({
            "schema_version":"inv.command.v1","command_id":format!("weights-{sequence}"),
            "case_id":self.case,"actor":"agent:test","preconditions":[],
            "operations":[{"op":op,"payload":payload}],
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
        self.case.get_or_insert(accepted.case_id);
        Ok((
            accepted.records.last().unwrap().reference(),
            accepted.warnings.clone(),
        ))
    }
}

fn criterion_input(question: RevisionRef, kind: &str, direction: &str, weight: Value) -> Value {
    let threshold = if kind == "hard" {
        json!({"kind":"measured","operator":"at_most","value":{"decimal":"25","unit":"ms"}})
    } else {
        Value::Null
    };
    json!({"name":"Latency","kind":kind,"scope":{"kind":"all_options","question_ref":question},
        "scale":{"unit":"ms","description":"p99 latency","anchors":[]},"preferred_direction":direction,
        "threshold":threshold,"weight":weight})
}

#[test]
fn weights_are_validated_and_a_rangeless_weight_is_stored_as_a_stated_preference() {
    let (mut ops, question, other_question) = Ops::new();
    let preference = json!({"value":"2","rationale":"Latency matters to the team"});
    let (created, warnings) = ops
        .run(
            "criterion.create",
            criterion_input(question, "soft", "lower", preference),
        )
        .unwrap();
    let body: CriterionBody =
        serde_json::from_value(ops.state.current(created.id).unwrap().body.clone()).unwrap();
    let weight = body.weight.unwrap();
    assert_eq!(weight.status, WeightStatus::StatedPreference);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("criterion.weight_without_range")),
        "{warnings:?}"
    );
    let ranged = json!({"value":"2","range":{"worst":"40","best":"15"},"rationale":"The swing from 40 to 15 ms"});
    let (ranged_ref, warnings) = ops
        .run(
            "criterion.create",
            criterion_input(question, "soft", "lower", ranged.clone()),
        )
        .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let stored = &ops.state.current(ranged_ref.id).unwrap().body["weight"];
    assert_eq!(stored["status"], "ranged");
    for (kind, direction, weight, path) in [
        ("hard", "lower", ranged.clone(), "/weight"),
        ("soft", "higher", ranged.clone(), "/weight/range"),
        ("soft", "target", ranged.clone(), "/weight/range"),
        (
            "soft",
            "lower",
            json!({"value":"0","rationale":"Zero"}),
            "/weight/value",
        ),
        (
            "soft",
            "lower",
            json!({"value":"2","range":{"worst":"40","best":"15"},"uncertainty":{"low":"3","high":"4"},"rationale":"Bad bracket"}),
            "/weight/uncertainty",
        ),
    ] {
        let failure = ops
            .run(
                "criterion.create",
                criterion_input(question, kind, direction, weight),
            )
            .unwrap_err();
        assert_eq!(
            failure.error.path.as_deref(),
            Some(format!("/operations/0/payload{path}").as_str()),
            "{failure:?}"
        );
    }
    let mut unitless = criterion_input(question, "soft", "lower", ranged);
    unitless["scale"]["unit"] = Value::Null;
    let failure = ops.run("criterion.create", unitless).unwrap_err();
    assert_eq!(
        failure.error.path.as_deref(),
        Some("/operations/0/payload/weight/range")
    );
    // Overlap declarations name an active criterion of the same question.
    let mut overlapping = criterion_input(question, "soft", "lower", Value::Null);
    overlapping["name"] = json!("Queue depth");
    overlapping["overlaps"] = json!([{"criterion_ref":ranged_ref,"relation":"proxy_for",
        "statement":"Queue depth stands in for latency under load"}]);
    let (overlap_ref, _) = ops.run("criterion.create", overlapping.clone()).unwrap();
    let stored = &ops.state.current(overlap_ref.id).unwrap().body["overlaps"];
    assert_eq!(stored[0]["criterion_id"], json!(ranged_ref.id));
    let elsewhere = ops
        .run(
            "criterion.create",
            criterion_input(other_question, "soft", "lower", Value::Null),
        )
        .unwrap()
        .0;
    overlapping["overlaps"][0]["criterion_ref"] = json!(elsewhere);
    let failure = ops
        .run("criterion.create", overlapping.clone())
        .unwrap_err();
    assert_eq!(
        failure.error.path.as_deref(),
        Some("/operations/0/payload/overlaps/0/criterion_ref")
    );
    overlapping["overlaps"][0]["criterion_ref"] = json!(overlap_ref);
    let failure = ops
        .run(
            "criterion.amend",
            json!({"criterion_ref":overlap_ref,"reason":"Self","replacement":overlapping}),
        )
        .unwrap_err();
    assert_eq!(
        failure.error.path.as_deref(),
        Some("/operations/0/payload/replacement/overlaps")
    );
}
