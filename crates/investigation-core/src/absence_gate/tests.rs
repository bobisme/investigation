use super::*;
use crate::{
    catalog::standard_catalog,
    command::Command,
    header::SystemClock,
    identity::{IdSource, SystemIdSource},
    review_ops::required_read_set,
    state::{Acceptance, AcceptedTransaction, validate_and_apply},
    wire::RecordChange,
};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/development/scenario-b-latency-regression"
);

fn fixture(path: &str) -> Value {
    let text = std::fs::read_to_string(format!("{FIXTURE}/{path}")).unwrap();
    serde_json::from_str(&text).unwrap()
}

struct Scenario {
    state: State,
    case: Option<Id>,
    case_ref: Option<RevisionRef>,
}

impl Scenario {
    fn new() -> Self {
        let mut scenario = Self {
            state: State::default(),
            case: None,
            case_ref: None,
        };
        let case = scenario.one(
            "case.create",
            json!({"title":"Latency regression study", "mode":"record"}),
        );
        scenario.case = Some(case.id);
        scenario.case_ref = Some(case);
        scenario
    }

    fn transact(
        &mut self,
        ops: Vec<Value>,
        preconditions: &[RevisionRef],
    ) -> Result<AcceptedTransaction, Failure> {
        let sequence = self.state.sequence() + 1;
        let command: Command = serde_json::from_value(json!({
            "schema_version":"inv.command.v0-alpha","case_id":self.case,"actor":"agent:investigator",
            "command_id":format!("absence-{sequence}"),"preconditions":preconditions,"operations":ops,
        }))
        .unwrap();
        let receipt = validate_and_apply(
            &self.state,
            &command,
            &standard_catalog(),
            Acceptance {
                sequence,
                clock: &mut SystemClock,
                ids: &mut SystemIdSource,
            },
        )?;
        self.state.apply(&receipt)?;
        Ok(receipt)
    }

    fn submit(&mut self, name: &str, payload: Value) -> Result<AcceptedTransaction, Failure> {
        self.transact(vec![json!({"op":name,"payload":payload})], &[])
    }

    fn one(&mut self, name: &str, payload: Value) -> RevisionRef {
        self.submit(name, payload).unwrap().records[0].reference()
    }

    fn image(&mut self, kind: Kind, body: Value) -> RevisionRef {
        let case_id = self.case.unwrap();
        let sequence = self.state.sequence() + 1;
        let mut header = self.state.current(case_id).unwrap().header.clone();
        header.acceptance_sequence = sequence;
        header.id = SystemIdSource.next_id(kind).unwrap();
        let record = Record { header, body };
        let reference = record.reference();
        self.state
            .apply(&AcceptedTransaction {
                case_id,
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
        reference
    }

    /// A Run imaged from a fixture producer envelope: execution state,
    /// producer assertions and controls exactly as the envelope states them.
    fn run(&mut self, envelope: &Value, measurement: Option<Value>) -> RevisionRef {
        let mut body = json!({
            "intent":{"kind":"exploration","intent":envelope["intent"]["label"]},
            "producer":"agent:producer","tool":null,"environment":"marlin-1.9",
            "started_at":null,"finished_at":null,"result_assertion":null,"artifact_refs":[],
            "manifest_digest":null,"execution_status":envelope["execution"]["state"],
            "evidence_completeness":"complete","evidence_kind":"observational",
            "variables_changed":[],"search_coverage":null,"evidence_validity":"unknown",
            "producer_run":{"assertions":envelope["assertions"],"controls":envelope["controls"]},
        });
        if let Some(measurement) = measurement {
            body["measurement"] = measurement;
        }
        self.image(Kind::Run, body)
    }

    fn observation(
        &mut self,
        value: &str,
        validity: &str,
        run: Option<RevisionRef>,
        measurement: Option<Value>,
    ) -> RevisionRef {
        let source = self.image(Kind::Source, json!({"access":"local_only"}));
        let hash = format!("sha256:{}", "2".repeat(64));
        let artifact = self.image(Kind::Artifact, json!({
            "digest":hash,"size_bytes":64,"format":"application/json","source":source,
            "transforms":[],"availability":"retained","integrity":"verified","sensitivity":"internal"
        }));
        let mut body = json!({
            "value":{"kind":"measurement","value":value,"unit":"ms"},
            "basis":{"kind":"source","artifact_ref":artifact,
                "selector":{"schema_version":"inv.selector.v1","artifact_digest":hash,"selector":{"type":"whole"},"validation":"verified"},
                "text_quote":{"state":"not_applicable"}},
            "extraction_method":"Nearest-rank p99 of the retained capture",
            "validity":validity,"quality":[],"limitations":[],
            "evidence_kind":"observational","variables_changed":[],
            "status":"active","run_ref":run,
        });
        if let Some(measurement) = measurement {
            body["measurement"] = measurement;
        }
        self.image(Kind::Observation, body)
    }

    fn frozen_inquiry(&mut self, equivalence: Value, freeze: bool) -> RevisionRef {
        let created = self.one(
            "inquiry.create",
            json!({
                "purpose":"Bound the p99 difference between optimization A and the treatment.",
                "method":"Compare an externally computed interval with the margin.",
                "controls":[],"limits":["Stop after the two supplied intervals."],
                "desired_observations":["Interval for the p99 difference."],
                "inquiry":{"mode":"exploratory"},"equivalence":equivalence,
            }),
        );
        if freeze {
            self.one("inquiry.freeze", json!({"inquiry_ref":created}))
        } else {
            created
        }
    }

    fn record(&self, reference: RevisionRef) -> &Record {
        self.state.revision(reference).unwrap()
    }

    fn request_review(&mut self, conclusion: RevisionRef) -> Result<AcceptedTransaction, Failure> {
        let mut reads = required_read_set(&self.state, self.record(conclusion)).unwrap_or_default();
        reads.push(conclusion);
        self.transact(
            vec![json!({"op":"conclusion.request_review","payload":{"conclusion_ref":conclusion}})],
            &reads,
        )
    }
}

fn plan() -> Value {
    let frozen = &fixture("practical-equivalence/manifest.json")["frozen_inquiry"];
    json!({
        "population": frozen["population"],
        "unit": frozen["unit"],
        "aggregation": frozen["aggregation"],
        "replication_unit": frozen["replication_unit"],
        "timeout_exclusion_policy": frozen["timeout_exclusion_policy"],
        "margin": {
            "lower": frozen["margin"]["lower_ms"].to_string(),
            "upper": frozen["margin"]["upper_ms"].to_string(),
        },
    })
}

fn assessment(s: &Scenario, evidence: &[RevisionRef], relation: &str, extra: Value) -> Value {
    let mut payload = json!({
        "subject_ref": s.case_ref.unwrap(), "evidence_refs": evidence, "relation": relation,
        "rationale": "Read from the retained capture.", "confidence": null, "likelihood": null,
    });
    for (key, value) in extra.as_object().unwrap() {
        payload[key] = value.clone();
    }
    payload
}

fn conclusion(statement: &str, support: &[RevisionRef], claim_form: Option<&str>) -> Value {
    json!({
        "statement": statement,
        "scope": "ledgerd mixed requests on fixture-host-1 at steady load.",
        "significance": "Decides whether optimization A is worth shipping.",
        "support_refs": support, "contradiction_refs": [],
        "caveats": ["Synthetic fixture captures only."],
        "claim_form": claim_form,
    })
}

fn codes(failure: &Failure) -> Vec<String> {
    let actual = failure.error.actual.as_ref().unwrap();
    let mut codes: Vec<String> = actual["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|reason| reason["missing"].as_array().unwrap().iter())
        .map(|item| item["code"].as_str().unwrap().to_owned())
        .collect();
    codes.sort();
    codes.dedup();
    codes
}

fn equivalence_payload(inquiry: Option<RevisionRef>, run: RevisionRef) -> Value {
    let mut block = json!({"interval":{"run_ref":run,"assertion":"latency.p99.difference"}});
    if let Some(inquiry) = inquiry {
        block["inquiry_ref"] = json!(inquiry);
    }
    json!({"equivalence": block})
}

/// Records the negative-short-window variant: both captures ran 8 s of a
/// planned 300 s and the monitor self-check failed, so the investigator
/// judged both measurements invalid.
fn short_window(s: &mut Scenario, validity: &str) -> (Vec<RevisionRef>, Vec<RevisionRef>) {
    let mut runs = Vec::new();
    let mut observations = Vec::new();
    for name in ["control-short", "optimization-a-short"] {
        let envelope = fixture(&format!("negative-short-window/runs/{name}/envelope.json"));
        let measurement = fixture(&format!(
            "negative-short-window/runs/{name}/measurement.json"
        ));
        let run = s.run(&envelope, Some(measurement.clone()));
        let value = envelope["assertions"][0]["value"]
            .as_str()
            .unwrap()
            .to_owned();
        observations.push(s.observation(&value, validity, Some(run), Some(measurement)));
        runs.push(run);
    }
    (runs, observations)
}

fn short_window_absence(runs: &[RevisionRef], justified: Option<&str>) -> Value {
    json!({"absence": {
        "expected_signal": "A p99 at least 1 ms lower under optimization A than under the control.",
        "opportunity": {
            "description": "One control and one optimization A capture at steady load.",
            "refs": runs,
            "minimum_observation_window": {"value": "300", "unit": "s"},
        },
        "justified_equivalent": justified,
        "detection_limit": "A 1 ms p99 difference.",
    }})
}

#[test]
fn zero_difference_under_broken_instrumentation_is_inconclusive_not_equality() {
    let mut s = Scenario::new();
    let (runs, observations) = short_window(&mut s, "invalid");
    // The absence is recordable; a supportive relation on invalid
    // measurements is already refused, so it is recorded as inconclusive.
    let supportive = assessment(
        &s,
        &observations,
        "supports",
        short_window_absence(&runs, None),
    );
    assert_eq!(
        s.submit("assessment.add", supportive)
            .unwrap_err()
            .error
            .code,
        ErrorCode::GateFailed
    );
    let payload = assessment(
        &s,
        &observations,
        "inconclusive",
        short_window_absence(&runs, Some("The producer's own timer.")),
    );
    let receipt = s.submit("assessment.add", payload).unwrap();
    let recorded = receipt.records[0].reference();
    assert!(
        receipt
            .warnings
            .iter()
            .any(|warning| warning.starts_with("absence.basis_incomplete:")),
        "{:?}",
        receipt.warnings
    );
    let reading = evaluate_absence(
        &s.state,
        s.case.unwrap(),
        &decode_block(&s.record(recorded).body, "absence").unwrap(),
        &observations,
    );
    assert!(!reading.satisfied);
    assert_eq!(reading.ceiling.as_deref(), Some("inconclusive"));
    assert_eq!(reading.human_question, HUMAN_QUESTION);
    let found: BTreeSet<&str> = reading
        .missing
        .iter()
        .map(|item| item.code.as_str())
        .collect();
    // A justified equivalent never overrides an invalid judgment.
    assert!(found.contains("absence.instrument_invalid"));
    assert!(found.contains("absence.observation_window_below_minimum"));
    assert!(
        reading.missing.iter().any(
            |item| item.message == "The recorded window 8 s is below the stated minimum 300 s."
        ),
        "{reading:?}"
    );

    let claim = s.one(
        "conclusion.create",
        conclusion(
            "Optimization A does not change p99 latency.",
            &[recorded],
            Some("absence"),
        ),
    );
    let failure = s.request_review(claim).unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::GateFailed);
    let actual = failure.error.actual.as_ref().unwrap();
    assert_eq!(actual["ceiling"], "inconclusive");
    assert_eq!(actual["human_question"], HUMAN_QUESTION);
    let codes = codes(&failure);
    for code in [
        "absence.claim_without_basis",
        "absence.instrument_invalid",
        "absence.observation_window_below_minimum",
    ] {
        assert!(codes.contains(&code.to_owned()), "{codes:?}");
    }
    // Nothing in the refusal is a computed statistic.
    let text = serde_json::to_string(&failure).unwrap();
    for key in ["p_value", "power", "confidence_interval"] {
        assert!(!text.contains(key));
    }
    assert_eq!(
        s.record(claim).body["status"],
        "candidate",
        "the refusal commits nothing"
    );
}

#[test]
fn a_complete_absence_basis_lets_the_absence_conclusion_through() {
    let mut s = Scenario::new();
    let measurement = json!({
        "unit":{"state":"known","value":"ms"},
        "observation_window":{"state":"known","value":{"value":"300","unit":"s"}},
        "sample_count":{"state":"known","value":3000},
    });
    let envelope = fixture("negative-short-window/runs/control-short/envelope.json");
    let mut runs = Vec::new();
    let mut observations = Vec::new();
    for value in ["22.6", "22.5"] {
        let run = s.run(&envelope, Some(measurement.clone()));
        observations.push(s.observation(value, "valid", Some(run), None));
        runs.push(run);
    }
    let mut block = short_window_absence(&runs, None);
    block["absence"]["opportunity"]["minimum_sample_count"] = json!(2995);
    let recorded = s.one(
        "assessment.add",
        assessment(&s, &observations, "supports", block),
    );
    let claim = s.one(
        "conclusion.create",
        conclusion(
            "No p99 improvement of 1 ms or more was detected.",
            &[recorded],
            Some("absence"),
        ),
    );
    let receipt = s.request_review(claim).unwrap();
    assert_eq!(receipt.records[0].body["status"], "review_ready");

    // Missing the expected signal and the standard blocks the same claim.
    let bare = s.one(
        "assessment.add",
        assessment(
            &s,
            &observations,
            "supports",
            json!({"absence":{"opportunity":{"description":"Two captures.","refs":runs}}}),
        ),
    );
    let claim = s.one(
        "conclusion.create",
        conclusion("No improvement.", &[bare], Some("absence")),
    );
    let codes = codes(&s.request_review(claim).unwrap_err());
    assert!(codes.contains(&"absence.expected_signal_missing".to_owned()));
    assert!(codes.contains(&"absence.adequacy_standard_missing".to_owned()));
}

#[test]
fn a_failed_or_blocked_run_is_not_negative_evidence() {
    let mut s = Scenario::new();
    let mut envelope = fixture("negative-short-window/runs/control-short/envelope.json");
    envelope["execution"]["state"] = json!("failed");
    let run = s.run(&envelope, None);
    let basis: AbsenceBasis = serde_json::from_value(json!({
        "expected_signal": "An error line for each rejected request.",
        "opportunity": {"description":"The failed capture.","refs":[run],"minimum_sample_count":1},
        "justified_equivalent": "The error log is the instrument.",
    }))
    .unwrap();
    let reading = evaluate_absence(&s.state, s.case.unwrap(), &basis, &[]);
    let found: Vec<&str> = reading
        .missing
        .iter()
        .map(|item| item.code.as_str())
        .collect();
    assert!(found.contains(&"absence.opportunity_run_not_usable"));
    assert!(found.contains(&"absence.sample_count_unrecorded"));
}

#[test]
fn a_bounded_empty_search_is_an_adequacy_standard_but_not_an_instrument() {
    let mut s = Scenario::new();
    let mut envelope = fixture("negative-short-window/runs/control-short/envelope.json");
    envelope["controls"] = json!([]);
    let run = s.run(&envelope, None);
    let mut body = s.record(run).body.clone();
    body["search_coverage"] = json!({
        "corpus":"Project errata pages","revision_bounds":"Editions 1 to 3","queries":["erratum allocator"],
        "filters":[],"inclusion_rules":[],"exclusion_rules":[],"total_hits":0,"examined_hits":0,"empty_hits":null,
    });
    let search = s.image(Kind::Run, body);
    let mut basis: AbsenceBasis = serde_json::from_value(json!({
        "expected_signal": "An erratum naming the allocator.",
        "opportunity": {"description":"Search of every published edition.","refs":[search]},
    }))
    .unwrap();
    let case = s.case.unwrap();
    let reading = evaluate_absence(&s.state, case, &basis, &[]);
    let found: Vec<&str> = reading
        .missing
        .iter()
        .map(|item| item.code.as_str())
        .collect();
    assert_eq!(found, ["absence.instrument_missing"]);
    basis.justified_equivalent = Some("The search log with its bounds is the instrument.".into());
    assert!(evaluate_absence(&s.state, case, &basis, &[]).satisfied);
    // A search without bounds or a stated empty result sets no standard.
    let plain = AbsenceBasis {
        opportunity: Some(AbsenceOpportunity {
            description: "An unbounded look.".into(),
            refs: vec![Reference::Existing(run)],
            minimum_observation_window: None,
            minimum_sample_count: None,
        }),
        ..basis
    };
    let found: Vec<String> = evaluate_absence(&s.state, case, &plain, &[])
        .missing
        .into_iter()
        .map(|item| item.code)
        .collect();
    assert_eq!(found, ["absence.adequacy_standard_missing"]);
}

#[test]
fn prespecified_band_supports_inside_interval_and_leaves_crossing_one_unresolved() {
    let mut s = Scenario::new();
    let inquiry = s.frozen_inquiry(plan(), true);
    let mut assessments = BTreeMap::new();
    for name in ["interval-inside-band", "interval-crosses-band"] {
        let envelope = fixture(&format!("practical-equivalence/runs/{name}/envelope.json"));
        let run = s.run(&envelope, None);
        let observation = s.observation("0", "valid", Some(run), None);
        let receipt = s
            .submit(
                "assessment.add",
                assessment(
                    &s,
                    &[observation],
                    "supports",
                    equivalence_payload(Some(inquiry), run),
                ),
            )
            .unwrap();
        assessments.insert(name, (receipt.records[0].reference(), receipt.warnings));
    }
    let (inside, warnings) = &assessments["interval-inside-band"];
    assert!(warnings.is_empty(), "{warnings:?}");
    let stored: EquivalenceEvaluation =
        serde_json::from_value(s.record(*inside).body[EQUIVALENCE_EVALUATION_KEY].clone()).unwrap();
    assert_eq!(
        stored.outcome,
        EquivalenceOutcome::BoundedEquivalenceSupported
    );
    assert_eq!(stored.margin_timing, MarginTiming::Prespecified);
    let interval = stored.interval.unwrap();
    assert_eq!(
        (
            interval.lower.as_str(),
            interval.upper.as_str(),
            interval.unit.as_str()
        ),
        ("-2", "3", "ms")
    );

    let (crossing, warnings) = &assessments["interval-crosses-band"];
    assert!(warnings[0].starts_with("equivalence.not_supported:"));
    let stored = &s.record(*crossing).body[EQUIVALENCE_EVALUATION_KEY];
    assert_eq!(stored["outcome"], "equivalence_unresolved");
    assert_eq!(stored["margin_timing"], "prespecified");

    let claim = s.one(
        "conclusion.create",
        conclusion(
            "Optimization A and the treatment are practically equivalent in p99 within 5 ms.",
            &[*inside],
            Some("equivalence"),
        ),
    );
    assert_eq!(
        s.request_review(claim).unwrap().records[0].body["status"],
        "review_ready"
    );
    let claim = s.one(
        "conclusion.create",
        conclusion(
            "Optimization A and the treatment are practically equivalent in p99 within 5 ms.",
            &[*crossing],
            Some("equivalence"),
        ),
    );
    let failure = s.request_review(claim).unwrap_err();
    let codes = codes(&failure);
    assert!(codes.contains(&"equivalence.interval_crosses_margin".to_owned()));
    assert!(codes.contains(&"equivalence.claim_without_basis".to_owned()));
    assert_eq!(
        failure.error.actual.as_ref().unwrap()["reasons"][0]["outcome"],
        "equivalence_unresolved"
    );
}

#[test]
fn a_margin_stated_after_results_is_recorded_as_such_and_not_prespecified() {
    let mut s = Scenario::new();
    let envelope = fixture("practical-equivalence/runs/interval-inside-band/envelope.json");
    let run = s.run(&envelope, None);
    let observation = s.observation("0", "valid", Some(run), None);
    // Frozen only after the interval was recorded.
    let late = s.frozen_inquiry(plan(), true);
    // A margin stated with the assessment itself.
    let mut stated = equivalence_payload(None, run);
    stated["equivalence"]["stated_margin"] = json!({"unit":"ms","lower":"-5","upper":"5"});
    // A draft that was never frozen.
    let draft = s.frozen_inquiry(plan(), false);
    for (payload, timing) in [
        (equivalence_payload(Some(late), run), "stated_after_results"),
        (stated, "stated_after_results"),
        (equivalence_payload(Some(draft), run), "not_frozen"),
    ] {
        let receipt = s
            .submit(
                "assessment.add",
                assessment(&s, &[observation], "supports", payload),
            )
            .unwrap();
        let recorded = receipt.records[0].reference();
        let stored = &s.record(recorded).body[EQUIVALENCE_EVALUATION_KEY];
        assert_eq!(stored["margin_timing"], timing);
        // The arithmetic is still reported: the interval is inside the band.
        assert_eq!(stored["outcome"], "bounded_equivalence_supported");
        let claim = s.one(
            "conclusion.create",
            conclusion("Practically equivalent within 5 ms.", &[recorded], None),
        );
        let codes = codes(&s.request_review(claim).unwrap_err());
        let expected = if timing == "not_frozen" {
            "equivalence.margin_not_frozen"
        } else {
            "equivalence.margin_not_prespecified"
        };
        assert!(codes.contains(&expected.to_owned()), "{codes:?}");
    }
}

#[test]
fn intervals_come_only_from_recorded_producer_statements() {
    let mut s = Scenario::new();
    let inquiry = s.frozen_inquiry(plan(), true);
    let case = s.case.unwrap();
    let mut envelope = fixture("practical-equivalence/runs/interval-inside-band/envelope.json");
    let read = |s: &Scenario, run: RevisionRef, assertion: &str| {
        let basis: EquivalenceBasis = serde_json::from_value(json!({
            "inquiry_ref": inquiry,
            "interval": {"run_ref": run, "assertion": assertion},
        }))
        .unwrap();
        evaluate_equivalence(&s.state, case, &basis, None)
    };
    let run = s.run(&envelope, None);
    let missing = read(&s, run, "latency.p99.not-reported");
    assert_eq!(missing.outcome, EquivalenceOutcome::NotEvaluable);
    assert_eq!(
        missing.missing[0].code,
        "equivalence.interval_assertion_missing"
    );
    // Decimal strings are read exactly; units are never converted.
    envelope["assertions"][0]["value"] = json!({"lower": "-0.002", "upper": "0.003"});
    envelope["assertions"][0]["unit"] = json!("s");
    let seconds = s.run(&envelope, None);
    let differs = read(&s, seconds, "latency.p99.difference");
    assert_eq!(differs.outcome, EquivalenceOutcome::NotEvaluable);
    assert_eq!(differs.missing[0].code, "equivalence.unit_differs");
    // An interval entirely outside the band excludes equivalence.
    envelope["assertions"][0]["value"] = json!({"lower": 6, "upper": 9});
    envelope["assertions"][0]["unit"] = json!("ms");
    let outside = s.run(&envelope, None);
    assert_eq!(
        read(&s, outside, "latency.p99.difference").outcome,
        EquivalenceOutcome::EquivalenceExcluded
    );
    // Endpoints on the bounds are inside.
    envelope["assertions"][0]["value"] = json!({"lower": "-5.0", "upper": "5"});
    let edge = s.run(&envelope, None);
    let edge = read(&s, edge, "latency.p99.difference");
    assert!(edge.holds(), "{edge:?}");
}

#[test]
fn shapes_reject_ambiguous_margins_and_empty_bands() {
    let run = json!({"client_ref":"run"});
    let basis: EquivalenceBasis = serde_json::from_value(json!({
        "inquiry_ref": {"client_ref":"plan"},
        "stated_margin": {"unit":"ms","lower":"-5","upper":"5"},
        "interval": {"run_ref": run, "assertion": "difference"},
    }))
    .unwrap();
    assert!(equivalence_shape(&basis, "/equivalence").is_err());
    let mut plan: EquivalencePlan = serde_json::from_value(plan()).unwrap();
    assert!(plan_shape(&plan, "/equivalence").is_ok());
    plan.margin.lower = DecimalString::try_from("5".to_owned()).unwrap();
    assert_eq!(
        plan_shape(&plan, "/equivalence")
            .unwrap_err()
            .error
            .path
            .as_deref(),
        Some("/equivalence/margin")
    );
    let blank: AbsenceBasis = serde_json::from_value(json!({"expected_signal":"  "})).unwrap();
    assert!(absence_shape(&blank, "/absence").is_err());
    // An empty block is recordable; the gate reports what is missing.
    assert!(absence_shape(&AbsenceBasis::default(), "/absence").is_ok());
}

#[test]
fn a_comparison_resting_on_an_incomplete_absence_reads_as_unknown() {
    let s = Scenario::new();
    let case = s.case.unwrap();
    let mut cell: ComparisonBody = serde_json::from_value(json!({
        "option_ref": s.case_ref, "criterion_ref": s.case_ref, "outcome": "meets",
        "method": "Search the errata.", "value": null, "basis": {"kind":"qualitative","statement":"No erratum found."},
        "evidence_refs": [], "assumption_refs": [], "limitations": null,
        "uncertainty": "Search bounds unrecorded.", "rationale": "Nothing found.",
        "retracted": false, "change_reason": null,
    }))
    .unwrap();
    assert!(comparison_basis_holds(&s.state, case, &cell));
    cell.absence = Some(AbsenceBasis {
        expected_signal: Some("An erratum naming the option.".into()),
        ..AbsenceBasis::default()
    });
    assert!(!comparison_basis_holds(&s.state, case, &cell));
}

use std::collections::BTreeMap;
