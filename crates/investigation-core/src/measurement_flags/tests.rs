use super::*;
use crate::{
    catalog::standard_catalog,
    command::Command,
    evidence::MeasurementValidity,
    header::SystemClock,
    identity::SystemIdSource,
    measurement_ops::effective_validity,
    state::{Acceptance, AcceptedTransaction, validate_and_apply},
    wire::Failure,
};
use serde_json::json;

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
                json!({"title": "Measurement flags", "mode": "record"}),
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

    fn create(&mut self, op: &str, payload: Value) -> (RevisionRef, Vec<String>) {
        let accepted = self.submit(op, payload).unwrap();
        (
            accepted.records.last().unwrap().reference(),
            accepted.warnings,
        )
    }

    fn flags(&self, reference: RevisionRef) -> FlagReport {
        record_flags(&self.state, self.state.revision(reference).unwrap()).unwrap()
    }

    fn ids(&self, reference: RevisionRef) -> Vec<String> {
        self.flags(reference)
            .flags
            .into_iter()
            .map(|flag| flag.flag_id)
            .collect()
    }
}

fn run(measurement: Value) -> Value {
    json!({
        "intent": {"kind": "exploration", "intent": "Measure the procedure."},
        "producer": "agent:producer",
        "artifact_refs": [],
        "execution_status": "completed",
        "evidence_completeness": "complete",
        "evidence_kind": "observational",
        "variables_changed": [],
        "measurement": measurement,
    })
}

fn observation(value: &str, measurement: Option<Value>, run: Option<RevisionRef>) -> Value {
    let mut payload = json!({
        "value": {"kind": "measurement", "value": value, "unit": "ms"},
        "basis": {"kind": "assertion", "assertion": "Reported by the recorder.", "context": null},
        "extraction_method": null,
        "validity": "valid",
        "quality": [],
        "limitations": [],
        "evidence_kind": "observational",
        "variables_changed": [],
        "observed_at": null,
        "run_ref": run,
    });
    if let Some(measurement) = measurement {
        payload["measurement"] = measurement;
    }
    payload
}

fn known(value: Value) -> Value {
    json!({"state": "known", "value": value})
}

fn quantile(level: &str) -> Value {
    known(json!({"kind": "quantile", "quantile": level}))
}

fn count(value: u64, definition: &str) -> Value {
    json!({"count": known(json!(value)), "definition": known(json!(definition))})
}

#[test]
fn order_statistic_minimum_matches_the_stated_bounds() {
    assert_eq!(order_statistic_minimum(0.99), 299);
    assert_eq!(order_statistic_minimum(0.999), 2995);
    assert_eq!(order_statistic_minimum(0.5), 5);
    assert_eq!(order_statistic_minimum(0.0), 0);
    assert_eq!(order_statistic_minimum(1.0), 0);
}

#[test]
fn scenario_b_sequential_and_closed_loop_seeds_are_flagged_without_blocking() {
    let mut case = Case::new();
    // A-then-B comparison: the trial order is recorded on the Run.
    let (sequential_run, _) = case.create(
        "run.record",
        run(json!({"trial_order": known(json!("sequential"))})),
    );
    let (sequential, warnings) = case.create(
        "observation.add",
        observation("12.5", None, Some(sequential_run)),
    );
    let report = case.flags(sequential);
    assert_eq!(report.flags.len(), 1);
    let flag = &report.flags[0];
    assert_eq!(flag.flag_id, flag_id::SEQUENTIAL_TRIAL_ORDER);
    assert!(flag.advisory);
    assert_eq!(flag.subject_ref, sequential);
    assert_eq!(flag.fields[0].record_ref, sequential_run);
    assert_eq!(flag.fields[0].path, "/measurement/trial_order");
    assert_eq!(report.notice.as_deref(), Some(ADVISORY_NOTICE));
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(flag_id::SEQUENTIAL_TRIAL_ORDER));
    assert!(warnings[0].contains("does not block recording"));

    // Closed-loop capture timed from the actual send time.
    let (closed, warnings) = case.create(
        "observation.add",
        observation(
            "48.0",
            Some(json!({
                "statistic": quantile("0.99"),
                "sample_count": known(json!(5000)),
                "load_model": known(json!({
                    "loop": known(json!("closed")),
                    "timed_from_intended_send": known(json!(false))
                }))
            })),
            None,
        ),
    );
    assert_eq!(
        case.ids(closed),
        vec![flag_id::CLOSED_LOOP_WITHOUT_INTENDED_SEND_TIMING]
    );
    assert!(
        case.flags(closed).flags[0]
            .reason
            .contains("coordinated omission")
    );
    assert_eq!(warnings.len(), 1);

    // Unknown timing is not treated as corrected either; open loop is not flagged.
    for (loop_kind, timed, flagged) in [
        ("closed", json!({"state": "unknown"}), true),
        ("closed", known(json!(true)), false),
        ("open", known(json!(false)), false),
    ] {
        let (reference, _) = case.create(
            "observation.add",
            observation(
                "1",
                Some(json!({"load_model": known(json!({
                    "loop": known(json!(loop_kind)), "timed_from_intended_send": timed
                }))})),
                None,
            ),
        );
        assert_eq!(!case.ids(reference).is_empty(), flagged, "{loop_kind}");
    }

    // Flags never change validity.
    for reference in [sequential, closed] {
        let record = case.state.current(reference.id).unwrap();
        assert_eq!(
            effective_validity(&case.state, record),
            MeasurementValidity::Valid
        );
    }
}

fn missing_outcome_case(population: &str) -> (Case, RevisionRef, RevisionRef, RevisionRef) {
    let mut case = Case::new();
    let (question, _) = case.create(
        "question.create",
        json!({"importance": "normal", "wording": "Does the option meet the limit?"}),
    );
    let (option, _) = case.create(
        "option.create",
        json!({"question_ref": question, "description": "Candidate", "dependencies": [], "tradeoff_notes": []}),
    );
    let (criterion, _) = case.create(
        "criterion.create",
        json!({"name": "99% within 25 ms", "kind": "hard",
               "scope": {"kind": "all_options", "question_ref": question},
               "scale": {"unit": "ms", "description": "0.99 quantile of all started attempts", "anchors": []},
               "preferred_direction": "lower",
               "threshold": {"kind": "measured", "operator": "at_most", "value": {"decimal": "25", "unit": "ms"}}}),
    );
    let (observed, _) = case.create(
        "observation.add",
        observation(
            "10",
            Some(json!({
                "statistic": known(json!({
                    "kind": "quantile", "quantile": "0.99",
                    "population": known(json!(population))
                })),
                "sample_count": known(json!(95)),
                "attempts": {
                    "started": count(100, "Attempts started."),
                    "observed": count(95, "Attempts with an observed outcome."),
                    "timed_out": count(5, "No outcome within 1 s.")
                }
            })),
            None,
        ),
    );
    (case, option, criterion, observed)
}

fn meets(option: RevisionRef, criterion: RevisionRef, evidence: RevisionRef) -> Value {
    json!({"option_ref": option, "criterion_ref": criterion, "outcome": "meets",
           "method": "Recorded quantile", "value": {"decimal": "10", "unit": "ms"},
           "evidence_refs": [evidence.id], "assumption_refs": [],
           "uncertainty": "One run.", "rationale": "Below the limit."})
}

#[test]
fn missing_outcome_variant_flags_meets_with_the_denominators() {
    let (mut case, option, criterion, observed) = missing_outcome_case("completed_attempts");
    let observation_ids = case.ids(observed);
    assert!(observation_ids.contains(&flag_id::COMPLETION_ONLY_STATISTIC.to_owned()));
    assert!(observation_ids.contains(&flag_id::QUANTILE_BELOW_ORDER_STATISTIC_BOUND.to_owned()));

    // Recording `meets` is not blocked, but it is flagged on the receipt and
    // on every read of the Comparison, naming the denominators.
    let (comparison, warnings) = case.create("comparison.add", meets(option, criterion, observed));
    let outcome_warning = warnings
        .iter()
        .find(|warning| warning.contains(flag_id::OUTCOME_FROM_COMPLETION_ONLY_STATISTIC))
        .expect("outcome flag on the receipt");
    assert!(outcome_warning.contains("started 100, observed 95, timed_out 5"));
    let report = case.flags(comparison);
    let flag = report
        .flags
        .iter()
        .find(|flag| flag.flag_id == flag_id::OUTCOME_FROM_COMPLETION_ONLY_STATISTIC)
        .unwrap();
    assert_eq!(flag.subject_ref, comparison);
    assert_eq!(flag.related_refs, vec![observed]);
    assert!(
        flag.reason
            .contains("started 100, observed 95, timed_out 5")
    );
    assert!(flag.reason.contains("completed attempts only"));
    assert!(
        flag.fields
            .iter()
            .any(|field| field.path == "/measurement/attempts")
    );
    // The cited Observation's own flags travel with the Comparison.
    assert!(
        report
            .flags
            .iter()
            .any(|flag| flag.flag_id == flag_id::COMPLETION_ONLY_STATISTIC
                && flag.subject_ref == observed)
    );

    // Case-wide reads list the Comparison's flag once, with its own subject.
    let all = case_flags(&case.state, case.case.unwrap());
    assert_eq!(
        all.iter()
            .filter(|flag| flag.flag_id == flag_id::OUTCOME_FROM_COMPLETION_ONLY_STATISTIC)
            .count(),
        1
    );
}

#[test]
fn an_unrecorded_population_with_unfinished_attempts_is_flagged_but_all_attempts_is_not() {
    let mut case = Case::new();
    let attempts = json!({
        "started": count(100, "Attempts started."),
        "observed": count(95, "Attempts with an observed outcome.")
    });
    let (unrecorded, _) = case.create(
        "observation.add",
        observation(
            "10",
            Some(json!({"statistic": known(json!({"kind": "mean"})), "attempts": attempts})),
            None,
        ),
    );
    let report = case.flags(unrecorded);
    assert_eq!(report.flags.len(), 1);
    assert!(
        report.flags[0]
            .reason
            .contains("does not record which attempts")
    );

    let (all, _, _, _) = missing_outcome_case("all_attempts");
    let recorded = all
        .state
        .records()
        .find(|record| record.header.id.kind() == Kind::Observation)
        .unwrap()
        .reference();
    assert!(
        !all.ids(recorded)
            .contains(&flag_id::COMPLETION_ONLY_STATISTIC.to_owned())
    );

    // Every attempt observed: completion-only is then the whole population.
    let mut case = Case::new();
    let (complete, _) = case.create(
        "observation.add",
        observation(
            "10",
            Some(json!({
                "statistic": known(json!({"kind": "mean", "population": known(json!("completed_attempts"))})),
                "attempts": {"started": count(40, "Started."), "observed": count(40, "Observed.")}
            })),
            None,
        ),
    );
    assert!(case.ids(complete).is_empty());
}

#[test]
fn a_quantile_needs_a_known_count_at_the_order_statistic_bound() {
    let mut case = Case::new();
    for (level, samples, expected) in [
        (
            "0.99",
            Some(298),
            Some(flag_id::QUANTILE_BELOW_ORDER_STATISTIC_BOUND),
        ),
        ("0.99", Some(299), None),
        (
            "0.999",
            Some(2994),
            Some(flag_id::QUANTILE_BELOW_ORDER_STATISTIC_BOUND),
        ),
        ("0.999", Some(2995), None),
        (
            "0.99",
            None,
            Some(flag_id::QUANTILE_SAMPLE_COUNT_UNRECORDED),
        ),
    ] {
        let mut fields = json!({"statistic": quantile(level)});
        if let Some(samples) = samples {
            fields["sample_count"] = known(json!(samples));
        }
        let (reference, _) = case.create("observation.add", observation("1", Some(fields), None));
        let ids = case.ids(reference);
        assert_eq!(
            ids,
            expected.into_iter().collect::<Vec<_>>(),
            "{level} {samples:?}"
        );
    }
    let (unknown, _) = case.create(
        "observation.add",
        observation(
            "1",
            Some(json!({"statistic": quantile("0.99"), "sample_count": {"state": "unknown"}})),
            None,
        ),
    );
    assert_eq!(
        case.ids(unknown),
        vec![flag_id::QUANTILE_SAMPLE_COUNT_UNRECORDED]
    );
    let (small, _) = case.create(
        "observation.add",
        observation(
            "1",
            Some(json!({"statistic": quantile("0.999"), "sample_count": known(json!(120))})),
            None,
        ),
    );
    let reason = &case.flags(small).flags[0].reason;
    assert!(reason.contains("at least 2995 samples"), "{reason}");
    assert!(reason.contains("not a universal sample floor"));
}

#[test]
fn flat_samples_averaged_quantiles_and_changed_instrumentation_are_flagged() {
    let mut case = Case::new();
    let (reference, warnings) = case.create(
        "observation.add",
        observation(
            "1",
            Some(json!({
                "statistic": known(json!({"kind": "quantile", "quantile": "0.5",
                    "combination": known(json!("averaged_run_values"))})),
                "sample_count": known(json!(3)),
                "samples": {"shape": "flat", "unit": "ms", "values": ["1", "2", "3"]},
                "instrumentation_changed": known(json!(true))
            })),
            None,
        ),
    );
    let ids = case.ids(reference);
    assert_eq!(
        ids,
        vec![
            flag_id::QUANTILE_BELOW_ORDER_STATISTIC_BOUND,
            flag_id::QUANTILES_COMBINED_ACROSS_RUNS,
            flag_id::FLAT_SAMPLES,
            flag_id::INSTRUMENTATION_CHANGED,
        ]
    );
    assert_eq!(warnings.len(), 4);
    let flat = &case.flags(reference).flags[2];
    assert!(
        flat.reason
            .contains("unable to support a variance analysis")
    );
    assert_eq!(
        flat.fields[0].recorded,
        json!({"shape": "flat", "values": 3})
    );
}

#[test]
fn compared_measurements_show_differing_conditions_as_possible_confounders() {
    let mut case = Case::new();
    let fields = |workload: &str, warmup: &str| {
        json!({
            "workload": known(json!(workload)),
            "warmup": known(json!({"value": warmup, "unit": "s"}))
        })
    };
    let (control, _) = case.create(
        "observation.add",
        observation("10", Some(fields("mix-a", "30")), None),
    );
    let (treatment, _) = case.create(
        "observation.add",
        observation("12", Some(fields("mix-b", "0")), None),
    );
    let (same, _) = case.create(
        "observation.add",
        observation("11", Some(fields("mix-a", "30")), None),
    );
    // A control recorded by measurement.assess is compared with its target.
    let accepted = case
        .submit(
            "measurement.assess",
            json!({
                "observation_ref": treatment, "channel": null, "validity": "partial",
                "verification": [{"kind": "equivalent", "justification": "Checked by hand.", "evidence_refs": []},
                                 {"kind": "control", "observation_ref": control,
                                  "expected_environment": null, "rationale": "Same setup."}],
                "rationale": "Judged.", "limits": []
            }),
        )
        .unwrap();
    let assessed = accepted.records[0].reference();
    let report = case.flags(assessed);
    let ids: Vec<_> = report
        .flags
        .iter()
        .map(|flag| flag.flag_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            flag_id::COMPARED_WORKLOAD_DIFFERS,
            flag_id::COMPARED_WARMUP_DIFFERS
        ]
    );
    assert_eq!(report.flags[0].related_refs, vec![assessed, control]);
    assert!(
        report.flags[0]
            .reason
            .contains("differ in workload identity")
    );

    // Matching conditions raise nothing.
    let accepted = case
        .submit(
            "measurement.assess",
            json!({
                "observation_ref": same, "channel": null, "validity": "partial",
                "verification": [{"kind": "equivalent", "justification": "Checked by hand.", "evidence_refs": []},
                                 {"kind": "control", "observation_ref": control,
                                  "expected_environment": null, "rationale": "Same setup."}],
                "rationale": "Judged.", "limits": []
            }),
        )
        .unwrap();
    assert!(case.ids(accepted.records[0].reference()).is_empty());
}

#[test]
fn measurements_without_flagged_fields_raise_nothing() {
    let mut case = Case::new();
    let (plain, warnings) = case.create("observation.add", observation("1", None, None));
    assert!(warnings.is_empty());
    let report = case.flags(plain);
    assert!(report.flags.is_empty());
    assert_eq!(report.notice, None);
    assert_eq!(report.rule_version, FLAG_RULE_VERSION);

    // A non-timing measurement with its conditions recorded.
    let (counted, warnings) = case.create(
        "observation.add",
        observation(
            "1",
            Some(json!({
                "unit": known(json!("ms")),
                "workload": known(json!("sample-set-4")),
                "statistic": known(json!({"kind": "mean", "population": known(json!("all_attempts"))})),
                "sample_count": known(json!(12)),
                "trial_order": known(json!("randomized")),
                "instrumentation_changed": known(json!(false)),
                "samples": {"shape": "leveled", "unit": "ms", "samples": [
                    {"value": "1", "order": known(json!(0))}
                ]}
            })),
            None,
        ),
    );
    assert!(warnings.is_empty());
    assert!(case.ids(counted).is_empty());

    // Kinds that cite no measurement carry no report.
    let case_record = case.state.current(case.case.unwrap()).unwrap();
    assert!(record_flags(&case.state, case_record).is_none());
}

#[test]
fn reports_are_bounded_and_deduplicated() {
    let flags: Vec<_> = (0..40)
        .map(|index| {
            flag(
                flag_id::FLAT_SAMPLES,
                RevisionRef {
                    id: "obs_019acd16-0000-7000-8000-000000000001".parse().unwrap(),
                    revision: index + 1,
                },
                vec![],
                vec![],
                "Reason.".into(),
            )
        })
        .chain(std::iter::once(flag(
            flag_id::FLAT_SAMPLES,
            RevisionRef {
                id: "obs_019acd16-0000-7000-8000-000000000001".parse().unwrap(),
                revision: 1,
            },
            vec![],
            vec![],
            "Reason.".into(),
        )))
        .collect();
    let report = FlagReport::new(flags);
    assert_eq!(report.flags.len(), MAX_FLAGS_PER_RECORD);
    assert_eq!(report.omitted, 40 - MAX_FLAGS_PER_RECORD);
}

#[test]
fn flags_surface_as_advisory_gaps_for_next() {
    let (mut case, option, criterion, observed) = missing_outcome_case("completed_attempts");
    let (comparison, _) = case.create("comparison.add", meets(option, criterion, observed));
    let snapshot = crate::wire::Snapshot {
        sequence: case.state.sequence(),
        head_hash: format!("sha256:{}", "a".repeat(64)).parse().unwrap(),
    };
    let inventory =
        crate::gap_rules::enumerate_gaps(&case.state, &snapshot, case.case.unwrap()).unwrap();
    let gap = inventory
        .gaps
        .iter()
        .find(|gap| gap.rule_id == flag_id::OUTCOME_FROM_COMPLETION_ONLY_STATISTIC)
        .expect("outcome flag gap");
    assert_eq!(gap.subject_refs.len(), 2);
    assert!(gap.subject_refs.contains(&comparison));
    assert_eq!(
        gap.suggested_operation.as_deref(),
        Some("comparison.revise")
    );
    assert_eq!(gap.decision_effect, "could_flip_choice");
    assert!(gap.reason.contains("started 100, observed 95, timed_out 5"));
    assert_eq!(gap.prerequisites[0].reason, ADVISORY_NOTICE);
    assert!(
        inventory
            .gaps
            .iter()
            .any(|gap| gap.rule_id == flag_id::COMPLETION_ONLY_STATISTIC
                && gap.subject_refs == vec![observed])
    );
}
