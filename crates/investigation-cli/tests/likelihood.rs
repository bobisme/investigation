//! Likelihood on resolvable claims, outcome recording and Brier calibration
//! by model family (W10.08) through the public CLI.
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:likelihood-test";

fn output(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut process = command.spawn().unwrap();
    if let Some(stdin) = stdin {
        let mut pipe = process.stdin.take().unwrap();
        pipe.write_all(&serde_json::to_vec(stdin).unwrap()).unwrap();
    }
    let output = process.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{stdout}\n{:?}",
        output.stderr
    );
    stdout
}

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let stdout = output(cwd, args, stdin, exit);
    serde_json::from_str::<Value>(&stdout).expect(&stdout)
}

fn agent(family: &str, version: &str) -> Value {
    json!({"kind":"agent","model_family":family,"model_version":version,"harness":null})
}

struct Case<'a> {
    root: &'a Path,
    id: String,
    attachment: Value,
}

impl Case<'_> {
    fn submit_as(
        &self,
        key: &str,
        (actor, descriptor): (&str, Option<&Value>),
        op: &str,
        payload: Value,
        pre: Value,
        exit: i32,
    ) -> Value {
        let mut command = json!({
            "schema_version":"inv.command.v1", "command_id":key, "case_id":self.id,
            "actor":actor, "preconditions":pre, "operations":[{"op":op,"payload":payload}]
        });
        if let Some(descriptor) = descriptor {
            command["actor_descriptor"] = descriptor.clone();
        }
        run(
            self.root,
            &["record", "--input", "-", "--json"],
            Some(&command),
            exit,
        )
    }

    fn submit(&self, key: &str, op: &str, payload: Value, pre: Value, exit: i32) -> Value {
        self.submit_as(key, (ACTOR, None), op, payload, pre, exit)
    }

    fn changed(receipt: &Value) -> Value {
        let change = receipt["created"]
            .get(0)
            .or_else(|| receipt["updated"].get(0))
            .unwrap();
        json!({"id":change["id"],"revision":change["revision"]})
    }

    fn one(&self, key: &str, op: &str, payload: Value) -> Value {
        Self::changed(&self.submit(key, op, payload, json!([]), 0))
    }

    fn show(&self, id: &Value, depth: &str) -> Value {
        run(
            self.root,
            &[
                "--case",
                &self.id,
                "show",
                id["id"].as_str().unwrap(),
                "--depth",
                depth,
                "--json",
            ],
            None,
            0,
        )
    }

    fn show_text(&self, id: &Value) -> String {
        output(
            self.root,
            &[
                "--case",
                &self.id,
                "show",
                id["id"].as_str().unwrap(),
                "--format",
                "text",
            ],
            None,
            0,
        )
    }

    fn calibration(&self) -> Value {
        run(
            self.root,
            &["--case", &self.id, "calibration", "--json"],
            None,
            0,
        )
    }

    fn review(&self, key: &str, subject: &Value) -> Value {
        let basis = self.show(subject, "full")["review_basis"].clone();
        self.one(key, "review.submit", json!({
            "kind":"validation","subject_ref":basis["subject_ref"],"basis_digest":basis["basis_digest"],
            "read_set":basis["read_set"],"method":"Read the recorded basis.","materials_examined":[],
            "shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":["One reading."],
            "context_isolation":"fresh_context_unblinded",
            "context":{
                "saw_original_conclusion":{"state":"known","value":true},
                "gathered_new_evidence":{"state":"known","value":false},
                "method_or_source_changes":[],"activity":"Read the recorded basis.",
                "dependencies":"shared","dependence_basis":"Same recorded data."
            }
        }))
    }

    fn observe(&self, key: &str) -> Value {
        let selector = json!({"schema_version":"inv.selector.v1","artifact_digest":self.attachment["digest"],
            "selector":{"type":"text_lines","start_line":1,"end_line":1,"encoding":"utf-8","parser_version":"inv.text.utf8.v1"}});
        self.one(key, "observation.add", json!({
            "value":{"kind":"text","statement":"The recorded reading is 12.5 units."},
            "basis":{"kind":"source","artifact_ref":{"id":self.attachment["derivative_id"],"revision":1},
                "selector":selector,"text_quote":{"exact":"Reading 12.5 units"}},
            "validity":"unknown","quality":[],"limitations":[],"evidence_kind":"observational","variables_changed":[]
        }))
    }

    fn hypothesis(&self, key: &str) -> (Value, Value) {
        let question = self.one(
            &format!("{key}-question"),
            "question.create",
            json!({"importance":"normal","wording":"Does the reading hold on a rerun?"}),
        );
        let hypothesis = self.one(
            key,
            "hypothesis.create",
            json!({
                "question_ref":question,"statement":"The reading holds on a rerun.",
                "possible_falsifiers":["A rerun disagrees."],"rival_refs":[],"assumption_refs":[]
            }),
        );
        (question, hypothesis)
    }
}

fn start(root: &Path) -> Case<'_> {
    run(root, &["init", ".", "--no-vault", "--json"], None, 0);
    let config = root.join("investigation.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    fs::write(root.join("reading.txt"), "Reading 12.5 units\n").unwrap();
    let started = run(
        root,
        &[
            "start",
            "Does the reading hold?",
            "--attach",
            "reading.txt",
            "--command-id",
            "start",
            "--actor",
            ACTOR,
            "--json",
        ],
        None,
        0,
    );
    Case {
        root,
        id: started["created"][0]["id"].as_str().unwrap().to_owned(),
        attachment: started["items"][0].clone(),
    }
}

fn likelihood(estimate: Value) -> Value {
    json!({"estimate":estimate,"resolution_condition":"The scheduled rerun reports its reading."})
}

#[test]
fn a_likelihood_without_a_resolution_condition_is_rejected_with_a_confidence_repair() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(dir.path());
    let observation = case.observe("observe");
    let claim = |likelihood: Value| {
        json!({
            "statement":"The reading is 12.5 units.","scope":"This recorded reading.",
            "significance":"Check before reuse.","support_refs":[observation],
            "contradiction_refs":[],"caveats":["One reading."],"confidence":null,
            "likelihood":likelihood
        })
    };
    for (key, value) in [
        ("bare-number", json!(0.8)),
        ("bare-term", json!("likely")),
        ("no-condition", json!({"estimate":0.8})),
        (
            "blank-condition",
            json!({"estimate":"likely","resolution_condition":"  "}),
        ),
    ] {
        let refused = case.submit(key, "conclusion.create", claim(value), json!([]), 2);
        assert_eq!(refused["error"]["code"], "INVALID_INPUT", "{refused:#}");
        let message = refused["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("only on a claim with a stated resolution condition")
                && message.contains("state confidence instead"),
            "{refused:#}"
        );
        assert_eq!(
            refused["error"]["path"],
            "/operations/0/payload/likelihood/resolution_condition"
        );
        assert_eq!(
            refused["error"]["expected"]["or_confidence"]["level"],
            "low | moderate | high | not_assessed"
        );
    }
    // The same rule holds when a likelihood is set later.
    let candidate = case.one("claim", "conclusion.create", claim(Value::Null));
    let refused = case.submit(
        "set-no-condition",
        "likelihood.set",
        json!({"claim_ref":candidate,"likelihood":{"estimate":0.4},"reason":"Forecast."}),
        json!([]),
        2,
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("state confidence instead"),
        "{refused:#}"
    );
    // A stated condition is accepted, and only Assessments and Conclusions
    // carry a likelihood.
    let stated = case.one(
        "claim-stated",
        "conclusion.create",
        claim(likelihood(json!("very_likely"))),
    );
    let shown = case.show(&stated, "summary");
    assert_eq!(shown["likelihood"]["resolution_state"], "unresolved");
    assert_eq!(shown["likelihood"]["stated"]["scored_probability"], 0.875);
    assert_eq!(
        shown["likelihood"]["stated"]["mapping_version"],
        "icd203-midpoint.v1"
    );
    assert_eq!(
        shown["likelihood"]["stated"]["term_range"],
        json!([0.8, 0.95])
    );
    let refused = case.submit(
        "set-observation",
        "likelihood.set",
        json!({"claim_ref":observation,"likelihood":likelihood(json!(0.4)),"reason":"Forecast."}),
        json!([]),
        2,
    );
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");
}

/// One forecast: estimate, stating family and version, recorded outcome, and
/// the expected Brier score.
struct Expected {
    estimate: Value,
    family: &'static str,
    version: &'static str,
    outcome: &'static str,
    brier: Option<f64>,
}

#[test]
fn resolving_claims_produces_the_expected_brier_values_grouped_by_model_family() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(dir.path());
    let observation = case.observe("observe");
    let rerun = case.observe("rerun");
    let (_, hypothesis) = case.hypothesis("hypothesis");
    let fixture = |estimate: Value, family, version, outcome, brier| Expected {
        estimate,
        family,
        version,
        outcome,
        brier,
    };
    // Family "Family-A" in two spellings forms one group; 11 scored forecasts
    // and one annulled. Terms score at their ICD 203 range midpoints.
    let expected = [
        fixture(json!(0.9), "Family-A", "1", "holds", Some(0.01)),
        fixture(json!(0.9), "Family-A", "1", "does_not_hold", Some(0.81)),
        fixture(json!(0.7), "Family-A", "1", "holds", Some(0.09)),
        fixture(json!(0.2), "Family-A", "1", "does_not_hold", Some(0.04)),
        fixture(json!("likely"), "Family-A", "1", "holds", Some(0.105625)),
        fixture(
            json!("very_unlikely"),
            "Family-A",
            "1",
            "does_not_hold",
            Some(0.015625),
        ),
        fixture(
            json!("almost_certain"),
            "Family-A",
            "2",
            "holds",
            Some(0.0009),
        ),
        fixture(
            json!("roughly_even_chance"),
            "Family-A",
            "2",
            "holds",
            Some(0.25),
        ),
        fixture(json!(0.5), "Family-A", "2", "does_not_hold", Some(0.25)),
        fixture(json!(1.0), "Family-A", "2", "holds", Some(0.0)),
        fixture(json!(0.0), "FAMILY-A", "2", "holds", Some(1.0)),
        fixture(json!("unlikely"), "Family-A", "2", "annulled", None),
        fixture(json!(0.6), "Family-B", "7", "holds", Some(0.16)),
        fixture(
            json!("almost_no_chance"),
            "Family-B",
            "7",
            "does_not_hold",
            Some(0.0009),
        ),
    ];
    let mut claims = Vec::new();
    for (index, forecast) in expected.iter().enumerate() {
        let descriptor = agent(forecast.family, forecast.version);
        let receipt = case.submit_as(
            &format!("assess-{index}"),
            (&format!("agent:forecaster-{index}"), Some(&descriptor)),
            "assessment.add",
            json!({"subject_ref":hypothesis,"evidence_refs":[observation],"relation":"inconclusive",
                "rationale":"Forecast before the rerun.","confidence":null,
                "likelihood":likelihood(forecast.estimate.clone())}),
            json!([]),
            0,
        );
        let claim = Case::changed(&receipt);
        let resolved = case.submit_as(
            &format!("resolve-{index}"),
            ("agent:resolver", None),
            "likelihood.resolve",
            json!({"claim_ref":claim,"outcome":forecast.outcome,"evidence_refs":[rerun],
                "rationale":"The rerun reported its reading."}),
            json!([]),
            0,
        );
        claims.push(Case::changed(&resolved));
    }
    for (claim, forecast) in claims.iter().zip(&expected) {
        let view = &case.show(claim, "summary")["likelihood"];
        assert_eq!(view["resolution_state"], "resolved", "{view:#}");
        assert_eq!(view["resolution"]["outcome"], forecast.outcome);
        assert_eq!(
            view["resolution"]["brier"],
            json!(forecast.brier),
            "{view:#}"
        );
        assert_eq!(view["resolution"]["resolved_by"], "agent:resolver");
        assert_eq!(view["resolution"]["evidence_refs"], json!([rerun]));
        // The forecaster, not the resolver, is attributed with the forecast.
        assert_eq!(
            view["stated"]["dependence_group"],
            format!("model_family:{}", forecast.family.to_lowercase())
        );
        assert_eq!(view["stated"]["stated_at_revision"], 1);
    }

    let calibration = case.calibration();
    assert_eq!(calibration["rule_version"], "likelihood-calibration.v1");
    assert_eq!(calibration["mapping_version"], "icd203-midpoint.v1");
    assert_eq!(calibration["minimum_scored_for_mean"], 10);
    assert_eq!(
        (
            &calibration["scored"],
            &calibration["annulled"],
            &calibration["unresolved"]
        ),
        (&json!(13), &json!(1), &json!(0))
    );
    let groups = calibration["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    let family_a = &groups[0];
    assert_eq!(family_a["dependence_group"], "model_family:family-a");
    assert_eq!(
        (&family_a["scored"], &family_a["annulled"]),
        (&json!(11), &json!(1))
    );
    assert_eq!(family_a["score_state"], "shown");
    // Sum 2.57215 over 11 scored forecasts.
    assert_eq!(family_a["mean_brier"], 0.233832);
    // Each version has fewer than ten: counts only, no score.
    assert_eq!(
        family_a["versions"],
        json!([
            {"model_version":"1","scored":6,"score_state":"below_minimum","mean_brier":null},
            {"model_version":"2","scored":5,"score_state":"below_minimum","mean_brier":null}
        ])
    );
    let family_b = &groups[1];
    assert_eq!(family_b["dependence_group"], "model_family:family-b");
    assert_eq!(family_b["scored"], 2);
    assert_eq!(family_b["score_state"], "below_minimum");
    assert_eq!(family_b["mean_brier"], Value::Null);
    let text = output(
        dir.path(),
        &["--case", &case.id, "calibration", "--format", "text"],
        None,
        0,
    );
    assert!(
        text.contains("model_family:family-a: n=11, mean Brier 0.233832; 1 annulled"),
        "{text}"
    );
    assert!(
        text.contains("model_family:family-b: n=2, no score below 10"),
        "{text}"
    );

    // A recorded outcome is not overwritten: a second one needs a stated
    // correction, which then counts while the first stays in the log.
    let first = &claims[0];
    let refused = case.submit(
        "resolve-again",
        "likelihood.resolve",
        json!({"claim_ref":first,"outcome":"does_not_hold","evidence_refs":[rerun],
            "rationale":"Read again."}),
        json!([]),
        5,
    );
    assert_eq!(refused["error"]["code"], "GATE_FAILED");
    assert_eq!(refused["error"]["actual"], "holds");
    let corrected = case.one(
        "correct",
        "likelihood.resolve",
        json!({"claim_ref":first,"outcome":"does_not_hold","evidence_refs":[rerun],
            "rationale":"The rerun reading was misread.","correction_reason":"Misread the rerun."}),
    );
    let view = &case.show(&corrected, "full")["likelihood"];
    assert_eq!(view["resolution"]["brier"], 0.81);
    assert_eq!(view["resolution"]["corrected_entries"], 1);
    let record = &case.show(&corrected, "full")["record"]["body"];
    assert_eq!(record["likelihood_resolution"].as_array().unwrap().len(), 2);
    // (2.57215 - 0.01 + 0.81) / 11
    assert_eq!(case.calibration()["groups"][0]["mean_brier"], 0.306559);

    // A resolved forecast stays as stated.
    let refused = case.submit(
        "set-after",
        "likelihood.set",
        json!({"claim_ref":corrected,"likelihood":likelihood(json!(0.1)),"reason":"Hindsight."}),
        json!([]),
        5,
    );
    assert_eq!(refused["error"]["code"], "GATE_FAILED");

    // An outcome cites evidence and needs a stated likelihood.
    let bare = case.one(
        "bare",
        "assessment.add",
        json!({"subject_ref":hypothesis,"evidence_refs":[observation],"relation":"inconclusive",
            "rationale":"No forecast.","confidence":null}),
    );
    let refused = case.submit(
        "resolve-bare",
        "likelihood.resolve",
        json!({"claim_ref":bare,"outcome":"holds","evidence_refs":[rerun],"rationale":"Rerun."}),
        json!([]),
        5,
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no forecast to score")
    );
    let refused = case.submit(
        "resolve-no-evidence",
        "likelihood.resolve",
        json!({"claim_ref":bare,"outcome":"holds","evidence_refs":[],"rationale":"Rerun."}),
        json!([]),
        2,
    );
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");

    // A likelihood stated later is attributed to the actor who stated it, and
    // an unresolved one is counted as unresolved.
    let stated = case.submit_as(
        "set-later",
        ("agent:other", Some(&agent("Family-C", "3"))),
        "likelihood.set",
        json!({"claim_ref":bare,"likelihood":likelihood(json!("likely")),"reason":"Forecast now."}),
        json!([]),
        0,
    );
    let stated = Case::changed(&stated);
    let view = &case.show(&stated, "summary")["likelihood"];
    assert_eq!(view["stated"]["stated_by"], "agent:other");
    assert_eq!(view["stated"]["dependence_group"], "model_family:family-c");
    assert_eq!(view["stated"]["stated_at_revision"], 2);
    assert_eq!(case.calibration()["unresolved"], 1);
    let text = case.show_text(&stated);
    let line = text
        .lines()
        .find(|line| line.starts_with("Likelihood:"))
        .unwrap();
    assert!(
        line.contains("likely (scored as 0.675 under icd203-midpoint.v1)"),
        "{text}"
    );
    // Never in the same sentence as confidence.
    for line in text.lines() {
        assert!(
            !(line.contains("Likelihood") && line.contains("Confidence")),
            "{text}"
        );
    }
}

/// The effective status and freshness of a record.
fn standing(case: &Case<'_>, id: &Value) -> (Value, Value) {
    let shown = case.show(&json!({"id":id["id"]}), "summary");
    (
        shown["effective_status"].clone(),
        shown["freshness"]["state"].clone(),
    )
}

#[test]
fn no_gate_outcome_changes_when_a_likelihood_is_added_changed_or_removed() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(dir.path());
    let observation = case.observe("observe");
    let rerun = case.observe("rerun");
    let observation = case.one("assess", "measurement.assess", json!({
        "observation_ref":observation,"channel":null,"validity":"valid",
        "verification":[{"kind":"equivalent","justification":"The setup reproduced a known reference value."}],
        "rationale":"Judged from the recorded setup.","limits":[]
    }));

    // A conclusion reviewed without a likelihood.
    let candidate = case.one(
        "claim",
        "conclusion.create",
        json!({
            "statement":"The reading is 12.5 units.","scope":"This recorded reading.",
            "significance":"Check before reuse.","support_refs":[observation],
            "contradiction_refs":[],"caveats":["One reading."],"confidence":null
        }),
    );
    let basis = case.show(&candidate, "full")["review_basis"].clone();
    let mut pre = basis["read_set"].as_array().cloned().unwrap();
    pre.push(basis["subject_ref"].clone());
    let ready = Case::changed(&case.submit(
        "ready",
        "conclusion.request_review",
        json!({"conclusion_ref":candidate}),
        json!(pre),
        0,
    ));
    let review = case.review("review", &ready);
    let before = standing(&case, &ready);

    // Add, change and remove a likelihood: each leaves the reviewed basis,
    // the effective status and freshness as they were.
    let mut current = ready.clone();
    for (key, value) in [
        ("add", likelihood(json!(0.8))),
        ("change", likelihood(json!("unlikely"))),
        ("remove", Value::Null),
        ("add-again", likelihood(json!("very_likely"))),
    ] {
        current = case.one(
            key,
            "likelihood.set",
            json!({"claim_ref":current,"likelihood":value,"reason":"Revise the forecast."}),
        );
        assert_eq!(standing(&case, &current), before, "{key}");
        assert_eq!(
            case.show(&current, "full")["review_basis"]["basis_digest"],
            basis["basis_digest"],
            "{key}"
        );
    }

    // The review recorded before any likelihood still validates the claim.
    let basis = case.show(&current, "full")["review_basis"].clone();
    let mut pre = basis["read_set"].as_array().cloned().unwrap();
    pre.extend([current.clone(), review.clone()]);
    let validated = Case::changed(&case.submit(
        "validate",
        "conclusion.validate",
        json!({"conclusion_ref":current,"review_ref":review}),
        json!(pre),
        0,
    ));
    let validated_standing = standing(&case, &validated);

    // Changing, resolving or removing it after validation changes nothing.
    let changed = case.one(
        "change-validated",
        "likelihood.set",
        json!({"claim_ref":validated,"likelihood":likelihood(json!(0.3)),"reason":"Revise."}),
    );
    assert_eq!(standing(&case, &changed), validated_standing);
    let resolved = case.one(
        "resolve-validated",
        "likelihood.resolve",
        json!({"claim_ref":changed,"outcome":"does_not_hold","evidence_refs":[rerun],
            "rationale":"The rerun disagreed with the forecast condition."}),
    );
    assert_eq!(standing(&case, &resolved), validated_standing);
    assert_eq!(
        case.show(&resolved, "summary")["likelihood"]["resolution"]["brier"],
        0.09
    );

    // A decision citing an assessment: adding a likelihood to the cited
    // assessment after the proposal leaves acceptance open.
    let (question, hypothesis) = case.hypothesis("hypothesis");
    let assessment = case.one(
        "assessment",
        "assessment.add",
        json!({"subject_ref":hypothesis,"evidence_refs":[observation],"relation":"inconclusive",
            "rationale":"One reading of the setup.","confidence":null}),
    );
    let option = case.one("option", "option.create", json!({
        "question_ref":question,"description":"Reuse the bench setup","dependencies":[],"tradeoff_notes":[]
    }));
    let option = case.one(
        "eligible",
        "option.disposition",
        json!({"option_ref":option,"lifecycle":"eligible","reason":"Feasible"}),
    );
    let proposed = case.one("propose", "decision.propose", json!({
        "question_ref":question,"choice":{"kind":"select","option_ref":option},
        "compared_alternatives":[option],"comparison_refs":[],"hard_conditions":[],
        "evidence_refs":[assessment],"assumption_refs":[],"tradeoffs":[],"consequences":["Reuse the setup."],
        "reopen_trigger":"A reading disagrees.","reversal_conditions":[]
    }));
    let decision_before = standing(&case, &proposed);
    let decision_review = case.review("decision-review", &proposed);
    let forecast = case.one(
        "assessment-forecast",
        "likelihood.set",
        json!({"claim_ref":assessment,"likelihood":likelihood(json!("likely")),"reason":"Forecast."}),
    );
    assert_eq!(standing(&case, &proposed), decision_before);
    assert_eq!(standing(&case, &forecast).1, "current");
    let accepted = case.submit_as(
        "accept",
        ("agent:decider", None),
        "decision.accept",
        json!({"decision_ref":proposed,"review_ref":decision_review}),
        json!([]),
        0,
    );
    assert_eq!(accepted["result"], "committed");
}
