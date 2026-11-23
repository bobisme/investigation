use std::path::Path;

use serde_json::{Value, json};

use crate::{
    blindspot::{AuditRequest, AuditResponse, Finding, audit, audit_schema},
    catalog::standard_catalog,
    command::{Command, RevisionRef},
    header::SystemClock,
    identity::{Id, IdSource, Kind, SystemIdSource},
    state::{Acceptance, AcceptedTransaction, Record, State, validate_and_apply},
    wire::{Failure, RecordChange, Snapshot},
};

const DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

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
        test.create(
            "case.create",
            json!({"title": "Causal blindspots", "mode": "record"}),
        );
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
        self.state.apply(&accepted).unwrap();
        self.state.note_verified_snapshot(self.snapshot()).unwrap();
        self.case.get_or_insert(accepted.case_id);
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

    /// Store a record image directly, for kinds whose write path needs the
    /// blob store, and for states the write gates refuse.
    fn image(&mut self, kind: Kind, body: Value, replaces: Option<RevisionRef>) -> RevisionRef {
        let case = self.case.unwrap();
        let sequence = self.state.sequence() + 1;
        let mut header = self
            .state
            .current(replaces.map_or(case, |reference| reference.id))
            .unwrap()
            .header
            .clone();
        header.acceptance_sequence = sequence;
        if replaces.is_some() {
            header.history.push(header.revision);
            header.revision.0 += 1;
        } else {
            header.id = SystemIdSource.next_id(kind).unwrap();
            header.revision.0 = 1;
            header.history.clear();
        }
        let record = Record { header, body };
        let reference = record.reference();
        let change = RecordChange {
            client_ref: None,
            id: reference.id,
            revision: reference.revision,
        };
        let (created, updated) = if replaces.is_some() {
            (vec![], vec![change])
        } else {
            (vec![change], vec![])
        };
        self.state
            .apply(&AcceptedTransaction {
                case_id: case,
                sequence,
                records: vec![record],
                created,
                updated,
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

    fn audit(&self) -> AuditResponse {
        let response = audit(
            &self.state,
            &self.snapshot(),
            self.case.unwrap(),
            &AuditRequest::default(),
        )
        .unwrap();
        let schema = serde_json::to_value(audit_schema()).unwrap();
        let instance = serde_json::to_value(&response).unwrap();
        assert!(jsonschema::is_valid(&schema, &instance), "{instance:#}");
        response
    }

    fn body(&self, reference: RevisionRef) -> Value {
        self.state.current(reference.id).unwrap().body.clone()
    }

    fn question(&mut self) -> RevisionRef {
        self.create(
            "question.create",
            json!({"wording": "Why does the request stall?", "importance": "normal"}),
        )
    }

    fn hypothesis(&mut self, question: RevisionRef, statement: &str) -> RevisionRef {
        self.rival(question, statement, &[])
    }

    fn rival(&mut self, question: RevisionRef, statement: &str, of: &[RevisionRef]) -> RevisionRef {
        self.create(
            "hypothesis.create",
            json!({"question_ref": question, "statement": statement,
                "possible_falsifiers": ["The stall persists without it."],
                "rival_refs": of, "assumption_refs": []}),
        )
    }

    fn run(&mut self, environment: Option<&str>, measurement: Value) -> RevisionRef {
        let mut body = json!({
            "intent": {"kind": "exploration", "intent": "Read a capture."},
            "producer": "agent:fixture", "tool": null, "environment": environment,
            "started_at": null, "finished_at": null, "result_assertion": null,
            "artifact_refs": [], "manifest_digest": null, "execution_status": "completed",
            "evidence_completeness": "complete", "evidence_kind": "observational",
            "variables_changed": [], "search_coverage": null,
        });
        if !measurement.is_null() {
            body["measurement"] = measurement;
        }
        self.image(Kind::Run, body, None)
    }

    /// A measurement Observation over retained, verified source bytes.
    fn observation(
        &mut self,
        run: Option<RevisionRef>,
        kind: &str,
        measurement: Value,
        variables: &[&str],
    ) -> RevisionRef {
        let source = self.image(Kind::Source, json!({"access": "local_only"}), None);
        let artifact = self.image(
            Kind::Artifact,
            json!({
                "digest": DIGEST, "size_bytes": 64, "format": "application/json", "source": source,
                "transforms": [], "availability": "retained", "integrity": "verified",
                "sensitivity": "internal", "role": "original",
            }),
            None,
        );
        let mut body = json!({
            "status": "active",
            "value": {"kind": "measurement", "value": "28000", "unit": "ms"},
            "basis": {"kind": "source", "artifact_ref": artifact,
                "selector": {"schema_version": "inv.selector.v1", "artifact_digest": DIGEST,
                    "selector": {"type": "whole"}, "validation": "verified"},
                "text_quote": {"state": "not_applicable"}},
            "extraction_method": "Read the hold interval.",
            "validity": "valid", "quality": [], "limitations": [],
            "evidence_kind": kind, "variables_changed": variables, "observed_at": null,
            "run_ref": run,
        });
        if !measurement.is_null() {
            body["measurement"] = measurement;
        }
        self.image(Kind::Observation, body, None)
    }

    fn conclusion(&mut self, payload: Value) -> RevisionRef {
        let mut claim = json!({
            "statement": "Holding the connection across the wait causes the stall.",
            "scope": "The captured workload only.",
            "significance": "Release the connection first.",
            "support_refs": [], "contradiction_refs": [],
            "caveats": ["Synthetic captures only."],
        });
        for (key, value) in payload.as_object().unwrap() {
            claim[key] = value.clone();
        }
        self.create("conclusion.create", claim)
    }
}

fn findings<'a>(response: &'a AuditResponse, check_id: &str) -> Vec<&'a Finding> {
    response
        .checks
        .iter()
        .filter(|finding| finding.check_id == check_id)
        .collect()
}

fn only<'a>(response: &'a AuditResponse, check_id: &str) -> &'a Finding {
    let found = findings(response, check_id);
    assert_eq!(found.len(), 1, "{check_id}: {:#?}", response.checks);
    found[0]
}

fn facts(finding: &Finding) -> Vec<&str> {
    finding
        .facts
        .iter()
        .map(|fact| fact.fact.as_str())
        .collect()
}

fn fact_refs<'a>(finding: &'a Finding, name: &str) -> &'a [RevisionRef] {
    &finding
        .facts
        .iter()
        .find(|fact| fact.fact == name)
        .unwrap_or_else(|| panic!("no {name} in {finding:#?}"))
        .refs
}

fn known(value: Value) -> Value {
    json!({"state": "known", "value": value})
}

#[test]
fn a_confirmatory_plan_with_a_shared_prediction_is_flagged_before_any_run() {
    let mut case = Case::new();
    let question = case.question();
    let held = case.hypothesis(question, "A connection is held across the wait.");
    let scheduler = case.hypothesis(question, "Scheduler delay alone explains it.");
    let plan = |first: &str, second: &str| {
        json!({
            "purpose": "Compare minimal reproduction variants.",
            "method": "Toggle the release point and compare hold times.",
            "controls": ["Same build, workload and data set."],
            "limits": ["Twenty attempts."],
            "desired_observations": ["Connection hold time per attempt."],
            "inquiry": {"mode": "confirmatory", "confirmatory": {
                "predictions": [
                    {"rival_ref": held, "predicted_outcome": first},
                    {"rival_ref": scheduler, "predicted_outcome": second},
                ],
                "evidence_holdout_boundary": "Attempts 11 to 20 are held out.",
                "prior_exposure": "unseen_declared",
                "stop_analysis_rule": "Compare after twenty attempts.",
            }},
        })
    };
    let draft = case.create(
        "inquiry.create",
        plan("The stall persists.", "The stall persists. "),
    );
    let draft_audit = case.audit();
    let finding = only(&draft_audit, "inquiry.non_discriminating_plan");
    assert_eq!(finding.subject_refs, [draft]);
    assert!(facts(finding).contains(&"plan_lifecycle:draft"));

    let frozen = case
        .submit(
            "inquiry.freeze",
            json!({"inquiry_ref": draft, "question_ref": question}),
        )
        .unwrap();
    assert!(frozen.warnings[0].contains("inquiry.non_discriminating_plan"));
    let frozen = frozen.records[0].reference();
    let response = case.audit();
    let finding = only(&response, "inquiry.non_discriminating_plan");
    assert_eq!(finding.subject_refs, [frozen]);
    assert_eq!(
        finding.gap_id,
        format!("blindspot:inquiry.non_discriminating_plan:{}", frozen.id)
    );
    assert_eq!(finding.suggested_schema.as_deref(), Some("inquiry.amend"));
    assert_eq!(finding.guide.as_deref(), Some("discriminate"));
    assert!(facts(finding).contains(&"plan_lifecycle:frozen"));
    // Raised from the plan alone: no Run cites it yet.
    assert_eq!(fact_refs(finding, "runs_citing_plan:none"), [frozen]);
    assert_eq!(
        fact_refs(finding, "same_predicted_outcome"),
        [held, scheduler]
    );
    // The message is fixed and quotes no prediction text.
    assert!(
        !serde_json::to_string(&response)
            .unwrap()
            .contains("stall persists")
    );

    // A Run that executes the plan is disclosed; the gap_id stays stable.
    let run = case.create(
        "run.record",
        json!({"intent": {"kind": "plan", "plan_ref": frozen}, "producer": "agent:fixture",
            "artifact_refs": [], "execution_status": "completed", "evidence_completeness": "complete",
            "evidence_kind": "interventional", "variables_changed": ["release point"]}),
    );
    let after = case.audit();
    let finding = only(&after, "inquiry.non_discriminating_plan");
    assert_eq!(fact_refs(finding, "runs_citing_plan:recorded"), [run]);
    assert_eq!(
        finding.gap_id,
        format!("blindspot:inquiry.non_discriminating_plan:{}", frozen.id)
    );

    // A successor with distinct predictions remediates; the frozen one is kept.
    let successor = case.create(
        "inquiry.amend",
        json!({"inquiry_ref": frozen, "replacement": plan("Hold time falls.", "Hold time unchanged."),
            "rationale": "Make the predictions discriminate."}),
    );
    assert_ne!(successor.id, frozen.id);
    assert!(findings(&case.audit(), "inquiry.non_discriminating_plan").is_empty());
}

#[test]
fn a_rival_ruled_out_leaves_no_shared_live_prediction() {
    let mut case = Case::new();
    let question = case.question();
    let first = case.hypothesis(question, "First account.");
    let second = case.hypothesis(question, "Second account.");
    case.create(
        "inquiry.create",
        json!({
            "purpose": "Test.", "method": "Compare.", "controls": ["Same load."], "limits": [],
            "desired_observations": [],
            "inquiry": {"mode": "confirmatory", "confirmatory": {
                "predictions": [
                    {"rival_ref": first, "predicted_outcome": "Rises."},
                    {"rival_ref": second, "predicted_outcome": "Rises."},
                ],
                "evidence_holdout_boundary": "None held.", "prior_exposure": "seen",
                "stop_analysis_rule": "Stop at ten.",
            }},
        }),
    );
    assert_eq!(
        findings(&case.audit(), "inquiry.non_discriminating_plan").len(),
        1
    );
    case.create(
        "hypothesis.disposition",
        json!({"hypothesis_ref": second, "lifecycle": "retired",
            "rationale": "Ruled out by a separate check.", "evidence_refs": []}),
    );
    assert!(findings(&case.audit(), "inquiry.non_discriminating_plan").is_empty());
}

#[test]
fn a_causal_conclusion_with_only_observational_support_stays_candidate() {
    let mut case = Case::new();
    let question = case.question();
    let factor = case.hypothesis(question, "A connection is held across the wait.");
    let rival = case.rival(question, "Scheduler delay alone explains it.", &[factor]);
    let trace = case.observation(None, "observational", Value::Null, &[]);
    let candidate = case.conclusion(json!({
        "support_refs": [trace], "contradiction_refs": [rival],
        "causal_factors": [{"hypothesis_ref": factor, "role": "trigger"}],
    }));

    // The promotion gate refuses: there is no intervention, so it stays a candidate.
    let refused = case
        .submit(
            "conclusion.request_review",
            json!({"conclusion_ref": candidate}),
        )
        .unwrap_err();
    assert_eq!(
        refused
            .error
            .path
            .as_deref()
            .map(|path| path.ends_with("/causal_test")),
        Some(true),
        "{refused:?}"
    );
    assert_eq!(case.body(candidate)["status"], "candidate");

    let response = case.audit();
    let finding = only(&response, "causal.observational_only_support");
    assert_eq!(finding.subject_refs, [candidate]);
    assert_eq!(
        fact_refs(finding, "evidence_kind:observational_only"),
        [trace]
    );
    assert!(facts(finding).contains(&"conclusion_status:candidate"));
    assert!(facts(finding).contains(&"causal_standing:candidate"));
    assert!(
        !facts(finding)
            .iter()
            .any(|fact| fact.starts_with("status_exceeds_evidence"))
    );
    assert_eq!(finding.guide.as_deref(), Some("intervene"));

    // The live rival was never assessed against the support.
    let rivals = only(&response, "conclusion.support_not_assessed_against_rival");
    assert_eq!(fact_refs(rivals, "rival_not_assessed"), [rival]);

    // A record that claims validation anyway is shown as exceeding its evidence.
    let mut forged = case.body(candidate);
    forged["status"] = json!("validated");
    let validated = case.image(Kind::Conclusion, forged, Some(candidate));
    let response = case.audit();
    let finding = only(&response, "causal.observational_only_support");
    assert_eq!(finding.subject_refs, [validated]);
    assert!(facts(finding).contains(&"causal_standing:candidate"));
    assert_eq!(
        fact_refs(finding, "status_exceeds_evidence:validated"),
        [validated]
    );
    // Its measurement rests on the recorder's word and no sample or window.
    let basis = only(&response, "empirical.confirmatory_basis_incomplete");
    assert_eq!(basis.subject_refs, [validated]);
    for name in [
        "validity_basis:recorded_only",
        "sample_count:unrecorded",
        "observation_window:unrecorded",
    ] {
        assert_eq!(fact_refs(basis, name), [trace], "{name}");
    }
}

#[test]
fn an_interventional_support_clears_the_observational_check() {
    let mut case = Case::new();
    let question = case.question();
    let factor = case.hypothesis(question, "A connection is held across the wait.");
    let trace = case.observation(None, "observational", Value::Null, &[]);
    let toggle = case.observation(None, "interventional", Value::Null, &["release point"]);
    case.conclusion(json!({
        "support_refs": [trace, toggle],
        "causal_factors": [{"hypothesis_ref": factor, "role": "trigger"}],
    }));
    assert!(findings(&case.audit(), "causal.observational_only_support").is_empty());
}

#[test]
fn support_equally_expected_under_a_rival_is_reported() {
    let mut case = Case::new();
    let question = case.question();
    let factor = case.hypothesis(question, "A connection is held across the wait.");
    let rival = case.rival(question, "Scheduler delay alone explains it.", &[factor]);
    let trace = case.observation(None, "observational", Value::Null, &[]);
    let assess = |expectation: &str| {
        json!({
            "subject_ref": factor, "evidence_refs": [trace], "relation": "supports",
            "rival_expectations": [{"rival_ref": rival, "expectation": expectation,
                "rationale": "Both accounts predict a long hold."}],
            "rationale": "The hold coincides with the wait.",
        })
    };
    let equal = case.create("assessment.add", assess("equally_expected"));
    let conclusion = case.conclusion(json!({"support_refs": [equal]}));
    let response = case.audit();
    let finding = only(&response, "conclusion.support_not_assessed_against_rival");
    assert_eq!(finding.subject_refs, [conclusion]);
    let mut cited = vec![equal, rival];
    cited.sort_by_key(|reference| reference.id);
    assert_eq!(
        fact_refs(finding, "support_equally_expected_under_rival"),
        cited
    );

    // An assessment under which the support is less expected for the rival discriminates.
    let less = case.create("assessment.add", assess("less_expected"));
    case.conclusion(json!({"support_refs": [less]}));
    let response = case.audit();
    assert_eq!(
        findings(&response, "conclusion.support_not_assessed_against_rival").len(),
        1,
        "only the equally-expected conclusion remains"
    );
}

#[test]
fn failure_filtered_support_without_a_baseline_is_flagged() {
    let mut case = Case::new();
    let question = case.question();
    let factor = case.hypothesis(question, "A connection is held across the wait.");
    let attempts = |observed: u64, timed_out: u64| {
        json!({"attempts": {
            "observed": {"count": known(json!(observed)), "definition": known(json!("requests with a response or timeout"))},
            "timed_out": {"count": known(json!(timed_out)), "definition": known(json!("requests over 25 s"))},
        }})
    };
    let failures = case.observation(None, "observational", attempts(12, 12), &[]);
    let conclusion = case.conclusion(json!({
        "support_refs": [failures],
        "causal_factors": [{"hypothesis_ref": factor, "role": "trigger"}],
    }));
    let response = case.audit();
    let finding = only(&response, "causal.failure_only_selection");
    assert_eq!(finding.subject_refs, [conclusion]);
    assert_eq!(
        fact_refs(finding, "selection:failure_only_attempts"),
        [failures]
    );
    assert_eq!(fact_refs(finding, "baseline:none_recorded"), [failures]);
    assert_eq!(finding.guide.as_deref(), Some("causal-structure"));

    // Citing a non-failure baseline clears it.
    let baseline = case.observation(None, "observational", attempts(40, 1), &[]);
    let mut amended = case.body(conclusion);
    amended["support_refs"] = json!([failures, baseline]);
    case.image(Kind::Conclusion, amended, Some(conclusion));
    assert!(findings(&case.audit(), "causal.failure_only_selection").is_empty());
}

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/development/scenario-a-timeout/different-build-trap/sources")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn scenario_a_refutation_on_a_different_build_yields_the_comparability_gap() {
    let slow = fixture("trace-slow.json");
    let trap = fixture("scheduler-only-test.json");
    let build = |capture: &Value| capture["build_id"].as_str().unwrap().to_owned();
    assert_ne!(build(&slow), build(&trap));

    let mut case = Case::new();
    let question = case.question();
    let factor = case.hypothesis(question, "A connection is held across the wait.");
    let scheduler = case.hypothesis(question, "Scheduler delay alone explains it.");
    let slow_run = case.run(Some(&build(&slow)), Value::Null);
    let trap_run = case.run(Some(&build(&trap)), Value::Null);
    let held = case.observation(Some(slow_run), "observational", Value::Null, &[]);
    let refuting = case.observation(
        Some(trap_run),
        "interventional",
        Value::Null,
        &["callback queue bypassed"],
    );
    // The only apparent refutation of the scheduler account came from another build.
    let refutation = case.create(
        "assessment.add",
        json!({
            "subject_ref": scheduler, "evidence_refs": [refuting], "relation": "contradicts",
            "rationale": "No timeout once the queue was bypassed.",
        }),
    );
    let conclusion = case.conclusion(json!({
        "support_refs": [held], "contradiction_refs": [refutation],
        "causal_factors": [{"hypothesis_ref": factor, "role": "trigger"}],
    }));
    let response = case.audit();
    let finding = only(&response, "comparability.environment_differs");
    assert_eq!(finding.subject_refs, [conclusion]);
    let mut pair = vec![held, refuting];
    pair.sort_by_key(|reference| reference.id);
    assert_eq!(fact_refs(finding, "run_environment_differs"), pair);
    assert_eq!(finding.guide.as_deref(), Some("baseline"));
    // No build identifier is quoted.
    let text = serde_json::to_string(&response).unwrap();
    assert!(!text.contains(&build(&trap)), "{text}");

    // An Assessment that cites both is reported on itself too.
    let both = case.create(
        "assessment.add",
        json!({
            "subject_ref": scheduler, "evidence_refs": [held, refuting], "relation": "contradicts",
            "rationale": "Compared across the two captures.",
        }),
    );
    let response = case.audit();
    let subjects: Vec<_> = findings(&response, "comparability.environment_differs")
        .into_iter()
        .map(|finding| finding.subject_refs[0])
        .collect();
    assert!(subjects.contains(&both) && subjects.contains(&conclusion));
}

#[test]
fn identity_differences_named_as_the_changed_variable_are_not_confounders() {
    let mut case = Case::new();
    let question = case.question();
    let subject = case.hypothesis(question, "The upgrade slowed the allocator.");
    let identity = |build: &str| {
        json!({"environment_identity": known(json!({
            "build": known(json!(build)), "host": known(json!("bench-1")),
        }))})
    };
    let control = case.observation(None, "observational", identity("a"), &[]);
    let upgrade = case.observation(None, "interventional", identity("b"), &["build"]);
    let assessment = case.create(
        "assessment.add",
        json!({"subject_ref": subject, "evidence_refs": [control, upgrade], "relation": "supports",
            "rationale": "Only the build changed."}),
    );
    assert!(findings(&case.audit(), "comparability.environment_differs").is_empty());

    // The same difference, not named as the changed variable, is a confounder.
    let unnamed = case.observation(None, "observational", identity("c"), &[]);
    let mut body = case.body(assessment);
    body["evidence_refs"] = json!([control, unnamed]);
    let revised = case.image(Kind::Assessment, body, Some(assessment));
    let response = case.audit();
    let finding = only(&response, "comparability.environment_differs");
    assert_eq!(finding.subject_refs, [revised]);
    assert!(facts(finding).contains(&"environment_identity_differs"));
}

#[test]
fn a_control_recorded_under_another_build_is_reported_on_the_measurement() {
    let mut case = Case::new();
    let treated_run = case.run(Some("build-7c2a"), Value::Null);
    let control_run = case.run(Some("build-7c2b"), Value::Null);
    let treated = case.observation(Some(treated_run), "observational", Value::Null, &[]);
    let control = case.observation(Some(control_run), "observational", Value::Null, &[]);
    let judged = case.create(
        "measurement.assess",
        json!({"observation_ref": treated, "channel": null, "validity": "invalid",
            "verification": [{"kind": "control", "observation_ref": control,
                "expected_environment": null, "rationale": "Paired control."}],
            "rationale": "The only control ran elsewhere.", "limits": []}),
    );
    let response = case.audit();
    let finding = only(&response, "comparability.environment_differs");
    assert_eq!(finding.subject_refs, [judged]);
    assert!(facts(finding).contains(&"control_run_environment_differs"));
    assert_eq!(finding.related_refs, [control]);
}

#[test]
fn a_confirmatory_test_on_known_data_is_not_presented_as_prospective() {
    let mut case = Case::new();
    let question = case.question();
    let subject = case.hypothesis(question, "The local queue grows.");
    let rival = case.rival(question, "The downstream queue grows.", &[subject]);
    let source = case.image(Kind::Source, json!({"access": "local_only"}), None);
    let artifact = case.image(
        Kind::Artifact,
        json!({"digest": DIGEST, "size_bytes": 24, "format": "text/plain", "source": source,
            "transforms": [], "availability": "retained", "integrity": "verified",
            "sensitivity": "internal", "role": "original"}),
        None,
    );
    // Read before the plan is frozen.
    let inspected = case.image(
        Kind::Observation,
        json!({"status": "active",
            "value": {"kind": "text", "statement": "Stage three dominates the trace."},
            "basis": {"kind": "source", "artifact_ref": artifact,
                "selector": {"schema_version": "inv.selector.v1", "artifact_digest": DIGEST,
                    "selector": {"type": "whole"}, "validation": "verified"},
                "text_quote": {"state": "not_applicable"}},
            "validity": "valid", "quality": [], "limitations": [], "variables_changed": [],
            "run_ref": null}),
        None,
    );
    let draft = case.create(
        "inquiry.create",
        json!({"purpose": "Discriminate.", "method": "Compare stage timings.",
            "controls": ["Same load."], "limits": ["Ten traces."], "desired_observations": ["Stage timings."],
            "inquiry": {"mode": "confirmatory", "confirmatory": {
                "predictions": [
                    {"rival_ref": subject, "predicted_outcome": "Local stage time rises."},
                    {"rival_ref": rival, "predicted_outcome": "Downstream stage time rises."},
                ],
                "evidence_holdout_boundary": "Traces 11 to 20.", "prior_exposure": "unseen_declared",
                "stop_analysis_rule": "Compare after ten traces.",
            }}}),
    );
    let plan = case.create(
        "inquiry.freeze",
        json!({"inquiry_ref": draft, "question_ref": question}),
    );
    let reused = case.create(
        "assessment.add",
        json!({"subject_ref": subject, "evidence_refs": [inspected], "relation": "supports",
            "rival_expectations": [{"rival_ref": rival, "expectation": "less_expected",
                "rationale": "Downstream stayed flat."}],
            "rationale": "As predicted.",
            "frozen_basis": {"plan_ref": plan, "actual_inputs": [{"input_ref": inspected}],
                "stop_rule_applied": "Stopped after the first clear trace."}}),
    );
    let conclusion = case.conclusion(json!({"support_refs": [reused]}));
    let response = case.audit();
    let finding = only(&response, "conclusion.known_outcome_reuse_as_prospective");
    assert_eq!(finding.subject_refs, [conclusion]);
    assert_eq!(
        fact_refs(finding, "evidence_reuse:known_outcome_reuse"),
        [reused]
    );
    assert_eq!(
        fact_refs(finding, "stop_rule_differs_without_deviation"),
        [reused]
    );
    assert!(finding.related_refs.contains(&plan));
    // A text observation carries no measurement basis to check.
    assert!(findings(&response, "empirical.confirmatory_basis_incomplete").is_empty());

    // Data acquired after the freeze, analysed under the frozen stop rule, is a
    // fresh test and is not reported.
    let fresh_source = case.image(Kind::Source, json!({"access": "local_only"}), None);
    let digest = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    let fresh_artifact = case.image(
        Kind::Artifact,
        json!({"digest": digest, "size_bytes": 24, "format": "text/plain", "source": fresh_source,
            "transforms": [], "availability": "retained", "integrity": "verified",
            "sensitivity": "internal", "role": "original"}),
        None,
    );
    let mut body = case.body(inspected);
    body["basis"]["artifact_ref"] = json!(fresh_artifact);
    body["basis"]["selector"]["artifact_digest"] = json!(digest);
    let fresh = case.image(Kind::Observation, body, None);
    let tested = case.create(
        "assessment.add",
        json!({"subject_ref": subject, "evidence_refs": [fresh], "relation": "supports",
            "rival_expectations": [{"rival_ref": rival, "expectation": "less_expected",
                "rationale": "Downstream stayed flat."}],
            "rationale": "As predicted.",
            "frozen_basis": {"plan_ref": plan, "actual_inputs": [{"input_ref": fresh}],
                "stop_rule_applied": "Compare after ten traces."}}),
    );
    case.conclusion(json!({"support_refs": [tested]}));
    let response = case.audit();
    assert_eq!(
        only(&response, "conclusion.known_outcome_reuse_as_prospective").subject_refs,
        [conclusion]
    );
}
