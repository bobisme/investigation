//! Pack policy inputs (W09.07) through the public CLI: a pinned pack can add
//! requirements at promotion and closure gates, every gate records the policy
//! it consulted, and no policy input can refuse evidence preservation.
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:pack-policy-test";

/// Every policy input the pack format offers, at each gate it applies to.
const PACK: &str = r#"schema_version = "inv.method-pack.v1"
id = "challenge-gates"
version = "1.0.0"
title = "Challenge gates"
summary = "Fixture pack that tightens every promotion gate."
modes = ["explain", "measure", "evaluate", "explore", "record"]

[[policies]]
name = "confidence-before-review"
input = "required_fields"
gate = "conclusion.request_review"
fields = ["confidence"]
rationale = "Reviewers need the stated confidence."

[[policies]]
name = "challenge-before-validation"
input = "blindspot_review"
gate = "conclusion.validate"
rationale = "A claim gets a recorded challenge before validation."

[[policies]]
name = "outcome-before-proposal"
input = "required_fields"
gate = "decision.propose"
fields = ["expected_outcome"]
rationale = "A proposal states what should happen if it is right."

[[policies]]
name = "challenge-before-accept"
input = "blindspot_review"
gate = "decision.accept"
rationale = "Consequential choices get a recorded challenge first."

[[policies]]
name = "second-decider"
input = "distinct_actor"
gate = "decision.accept"
rationale = "The proposer does not accept their own decision."

[[policies]]
name = "acknowledge-everything"
input = "acknowledge_open_objections"
gate = "decision.accept"
rationale = "Every open objection gets an explicit answer."

[[policies]]
name = "account-for-observations"
input = "closure_retrospective"
gate = "case.close"
triggers = [{ record_kind = "observation" }]
rationale = "Any recorded observation makes closure material."
"#;

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
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    stdout
}

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let stdout = output(cwd, args, stdin, exit);
    serde_json::from_str::<Value>(&stdout).expect(&stdout)
}

struct Case<'a> {
    root: &'a Path,
    id: String,
    /// `items[0]` of the `start` receipt: the attached reading.
    attachment: Value,
}

impl Case<'_> {
    fn submit(
        &self,
        key: &str,
        actor: &str,
        op: &str,
        payload: Value,
        pre: Value,
        exit: i32,
    ) -> Value {
        let command = json!({
            "schema_version":"inv.command.v1", "command_id":key, "case_id":self.id,
            "actor":actor, "preconditions":pre, "operations":[{"op":op,"payload":payload}]
        });
        run(
            self.root,
            &["record", "--input", "-", "--json"],
            Some(&command),
            exit,
        )
    }

    fn one(&self, key: &str, op: &str, payload: Value) -> Value {
        let receipt = self.submit(key, ACTOR, op, payload, json!([]), 0);
        let change = receipt["created"]
            .get(0)
            .or_else(|| receipt["updated"].get(0))
            .unwrap();
        json!({"id":change["id"],"revision":change["revision"]})
    }

    fn show(&self, id: &Value) -> Value {
        run(
            self.root,
            &[
                "--case",
                &self.id,
                "show",
                id["id"].as_str().unwrap(),
                "--depth",
                "full",
                "--json",
            ],
            None,
            0,
        )
    }

    fn body(&self, id: &Value) -> Value {
        self.show(id)["record"]["body"].clone()
    }

    fn context() -> Value {
        json!({
            "saw_original_conclusion":{"state":"known","value":true},
            "gathered_new_evidence":{"state":"known","value":false},
            "method_or_source_changes":[],"activity":"Read the recorded basis.",
            "dependencies":"shared","dependence_basis":"Same recorded data."
        })
    }

    fn review(&self, key: &str, subject: &Value) -> Value {
        let basis = self.show(subject)["review_basis"].clone();
        self.one(key, "review.submit", json!({
            "kind":"validation","subject_ref":basis["subject_ref"],"basis_digest":basis["basis_digest"],
            "read_set":basis["read_set"],"method":"Read the recorded basis.","materials_examined":[],
            "shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":["One reading."],
            "context_isolation":"fresh_context_unblinded","context":Self::context()
        }))
    }

    fn blindspot(&self, key: &str, subject: &Value, read_set: Value) -> Value {
        self.one(key, "review.submit", json!({
            "kind":"blindspot","subject_ref":subject,
            "audited_snapshot":{"sequence":1,"head_hash":format!("sha256:{}", "2".repeat(64))},
            "read_set":read_set,"method":"Looked for unconsidered options and assumptions.",
            "scope_examined":"The proposal and its recorded basis.","checks_considered":["missing options"],
            "gaps":[],"no_gap_reason":"Every recorded option was compared.","evidence_refs":[],
            "assumption_refs":[],"limitations":["Same recorded data."],
            "verdict":"no_gap_identified_within_scope","context_isolation":"fresh_context_unblinded",
            "context":Self::context()
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
}

fn init(root: &Path) {
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
}

fn start<'a>(root: &'a Path, key: &str) -> Case<'a> {
    let started = run(
        root,
        &[
            "start",
            "Can the reading be interpreted?",
            "--attach",
            "reading.txt",
            "--command-id",
            key,
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

/// Retain the pack through ingest and pin it; returns its digest.
fn adopt(case: &Case<'_>) -> String {
    fs::write(case.root.join("challenge-gates.toml"), PACK).unwrap();
    let retained = run(
        case.root,
        &["ingest", "--input", "-", "--json"],
        Some(&json!({
            "schema_version": "inv.ingest.v1", "command_id": format!("retain-pack-{}", case.id),
            "actor": ACTOR, "case_id": case.id,
            "source": {"title": "Challenge gates pack", "format": "application/toml"},
            "paths": ["challenge-gates.toml"]
        })),
        0,
    );
    let item = &retained["items"][0];
    let digest = item["digest"].as_str().unwrap().to_owned();
    let revision = case.show(&json!({"id": case.id}))["record"]["header"]["revision"].clone();
    case.submit(
        &format!("adopt-{}", case.id),
        ACTOR,
        "method_pack.adopt",
        json!({"artifact_ref": {"id": item["artifact_id"], "revision": 1},
            "pack_id": "challenge-gates", "version": "1.0.0", "digest": digest,
            "reason": "Tighten promotion for this case."}),
        json!([{"id": case.id, "revision": revision}]),
        0,
    );
    digest
}

fn blockers(failure: &Value) -> Vec<(String, String)> {
    failure["error"]["expected"]["policy_blockers"]
        .as_array()
        .unwrap_or_else(|| panic!("{failure:#}"))
        .iter()
        .map(|blocker| {
            assert_eq!(blocker["pack_id"], "challenge-gates");
            assert_eq!(blocker["version"], "1.0.0");
            assert!(blocker["digest"].as_str().unwrap().starts_with("sha256:"));
            (
                blocker["policy"].as_str().unwrap().to_owned(),
                blocker["input"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pair(policy: &str, input: &str) -> (String, String) {
    (policy.to_owned(), input.to_owned())
}

#[test]
fn a_pinned_pack_gates_decision_acceptance_and_every_gate_records_its_policy() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init(root);
    let case = start(root, "decision-case");
    let digest = adopt(&case);

    let question = case.one(
        "question",
        "question.create",
        json!({"importance":"normal","wording":"Which setup should be reused?"}),
    );
    let option = case.one("option", "option.create", json!({
        "question_ref":question,"description":"Reuse the bench setup","dependencies":[],"tradeoff_notes":[]
    }));
    let option = case.one(
        "eligible",
        "option.disposition",
        json!({"option_ref":option,"lifecycle":"eligible","reason":"Feasible"}),
    );
    let proposal = |expected_outcome: Value| {
        json!({
            "question_ref":question,"choice":{"kind":"select","option_ref":option},
            "compared_alternatives":[option],"comparison_refs":[],"hard_conditions":[],
            "evidence_refs":[],"assumption_refs":[],"tradeoffs":[],"consequences":["Reuse the setup."],
            "reopen_trigger":"A reading disagrees.","expected_outcome":expected_outcome,"reversal_conditions":[]
        })
    };

    // Required fields at decision.propose: the blocker names pack, rule and field.
    let refused = case.submit(
        "propose-bare",
        ACTOR,
        "decision.propose",
        proposal(Value::Null),
        json!([]),
        5,
    );
    assert_eq!(refused["error"]["code"], "GATE_FAILED");
    assert_eq!(
        blockers(&refused),
        [pair("outcome-before-proposal", "required_fields")]
    );
    assert_eq!(
        refused["error"]["expected"]["policy_blockers"][0]["unmet"]["missing_fields"],
        json!(["expected_outcome"])
    );
    let proposed = case.one(
        "propose",
        "decision.propose",
        proposal(json!("Readings agree within 1%.")),
    );
    let stored = case.body(&proposed)["gate_policy"].clone();
    assert_eq!(stored["gate"], "decision.propose");
    assert_eq!(stored["rule_version"], "pack-policy.v1");
    assert_eq!(
        stored["packs"],
        json!([{"pack_id":"challenge-gates","version":"1.0.0","digest":digest,"status":"available"}])
    );
    assert_eq!(stored["rules"][0]["policy"], "outcome-before-proposal");

    let material = case.one("material", "objection.raise", json!({
        "subject_ref":proposed,"statement":"A material concern.","scope":"The recorded reading.",
        "severity":"material","evidence_refs":{"state":"unknown"}
    }));
    let review = case.review("review", &proposed);
    let accept = |key: &str, actor: &str, acknowledgments: Value, exit| {
        case.submit(
            key,
            actor,
            "decision.accept",
            json!({"decision_ref":proposed,"review_ref":review,"acknowledgments":acknowledgments}),
            json!([]),
            exit,
        )
    };
    let acknowledged = json!([{"objection_ref":material,"rationale":"Reversible within a day."}]);

    // acknowledge_open_objections tightens the default: a material objection
    // now needs an explicit acknowledgment too.
    let refused = accept("accept-silent", "agent:decider", json!([]), 5);
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/payload/acknowledgments"
    );
    assert_eq!(refused["error"]["expected"], json!([material]));
    assert_eq!(refused["error"]["actual"]["policy"], "all_open");

    // Without a current blindspot review, and from the proposer, acceptance
    // is blocked with one machine-readable blocker per unmet rule.
    let refused = accept("accept-proposer", ACTOR, acknowledged.clone(), 5);
    assert_eq!(refused["error"]["code"], "GATE_FAILED");
    assert_eq!(
        blockers(&refused),
        [
            pair("challenge-before-accept", "blindspot_review"),
            pair("second-decider", "distinct_actor"),
        ]
    );
    assert_eq!(
        refused["error"]["expected"]["policy_blockers"][0]["repair_schema"],
        "review.submit"
    );
    let refused = accept(
        "accept-unchallenged",
        "agent:decider",
        acknowledged.clone(),
        5,
    );
    assert_eq!(
        blockers(&refused),
        [pair("challenge-before-accept", "blindspot_review")]
    );

    // A blindspot review of another subject does not count; one of this
    // proposal does.
    case.blindspot("blindspot-question", &question, json!([]));
    accept(
        "accept-other-review",
        "agent:decider",
        acknowledged.clone(),
        5,
    );
    let blindspot = case.blindspot("blindspot", &proposed, json!([option]));
    let refused = accept("accept-proposer-again", ACTOR, acknowledged.clone(), 5);
    assert_eq!(
        blockers(&refused),
        [pair("second-decider", "distinct_actor")]
    );
    let accepted = accept("accept", "agent:decider", acknowledged, 0);
    assert_eq!(accepted["result"], "committed");

    let decision = json!({"id":proposed["id"],"revision":2});
    let body = case.body(&decision);
    assert_eq!(body["acceptance_challenges"]["policy"], "all_open");
    let stored = &body["gate_policy"];
    assert_eq!(stored["gate"], "decision.accept");
    assert_eq!(stored["packs"][0]["digest"], json!(digest));
    let rules: Vec<_> = stored["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rule| rule["policy"].as_str().unwrap())
        .collect();
    assert_eq!(
        rules,
        [
            "challenge-before-accept",
            "second-decider",
            "acknowledge-everything"
        ]
    );
    assert_eq!(stored["rules"][0]["satisfied_by"], json!([blindspot]));
}

#[test]
fn pack_policy_gates_conclusion_promotion_and_closure_but_never_recording() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init(root);
    let control = start(root, "control-case");
    let case = start(root, "pinned-case");
    adopt(&case);

    // Recording, ingest and capture are never refused for pack policy, even
    // though this pack declares every policy input.
    let observation = case.observe("observe");
    fs::write(root.join("second.txt"), "Reading 12.6 units\n").unwrap();
    run(
        root,
        &["ingest", "--input", "-", "--json"],
        Some(&json!({
            "schema_version": "inv.ingest.v1", "command_id": "second-reading", "actor": ACTOR,
            "case_id": case.id, "source": {"title": "Second reading", "format": "text/plain"},
            "paths": ["second.txt"]
        })),
        0,
    );
    let capture = json!({
        "key": "run-0001/begin", "actor": ACTOR,
        "producer": {"name": "fixture-producer", "version": "0.1.0"},
        "external_run_id": "run-0001", "intent": {"label": "synthetic run"},
        "case": {"kind": "existing", "case_id": case.id}
    });
    let begun = run(
        root,
        &["capture", "begin", "--input", "-", "--format", "json"],
        Some(&capture),
        0,
    );
    assert_eq!(begun["commit"]["result"], "committed");

    // Required fields at conclusion.request_review.
    let observation = case.one("assess", "measurement.assess", json!({
        "observation_ref":observation,"channel":null,"validity":"valid",
        "verification":[{"kind":"equivalent","justification":"The setup reproduced a known reference value."}],
        "rationale":"Judged from the recorded setup.","limits":[]
    }));
    let conclusion = |key: &str, confidence: Value| {
        case.one(
            key,
            "conclusion.create",
            json!({
                "statement":"The reading is 12.5 units.","scope":"This recorded reading.",
                "significance":"Check the setup before reuse.","support_refs":[observation],
                "contradiction_refs":[],"caveats":["One reading."],"confidence":confidence
            }),
        )
    };
    let request = |candidate: &Value, key: &str, exit| {
        let basis = case.show(candidate)["review_basis"].clone();
        let mut pre = basis["read_set"].as_array().cloned().unwrap();
        pre.push(basis["subject_ref"].clone());
        case.submit(
            key,
            ACTOR,
            "conclusion.request_review",
            json!({"conclusion_ref":candidate}),
            json!(pre),
            exit,
        )
    };
    let bare = conclusion("claim-bare", Value::Null);
    let refused = request(&bare, "ready-bare", 5);
    assert_eq!(
        blockers(&refused),
        [pair("confidence-before-review", "required_fields")]
    );
    let candidate = conclusion(
        "claim",
        json!({"level":"moderate","rationale":"One calibrated reading.","limitations":[],"what_would_change_it":null,"downgrade_reasons":null}),
    );
    request(&candidate, "ready", 0);
    let ready = json!({"id":candidate["id"],"revision":2});
    assert_eq!(
        case.body(&ready)["gate_policy"]["gate"],
        "conclusion.request_review"
    );

    // Blindspot review at conclusion.validate.
    let review = case.review("validation", &ready);
    let validate = |key: &str, exit| {
        let basis = case.show(&ready)["review_basis"].clone();
        let mut pre = basis["read_set"].as_array().cloned().unwrap();
        pre.extend([ready.clone(), review.clone()]);
        case.submit(
            key,
            ACTOR,
            "conclusion.validate",
            json!({"conclusion_ref":ready,"review_ref":review}),
            json!(pre),
            exit,
        )
    };
    let refused = validate("validate-unchallenged", 5);
    assert_eq!(
        blockers(&refused),
        [pair("challenge-before-validation", "blindspot_review")]
    );
    let blindspot = case.blindspot("blindspot", &ready, json!([]));
    validate("validate", 0);
    let validated = case.body(&json!({"id":candidate["id"],"revision":3}));
    assert_eq!(
        validated["gate_policy"]["rules"][0]["satisfied_by"],
        json!([blindspot])
    );

    // A pack closure trigger makes the case material: no waiver. The control
    // case records the same kind of observation and may still waive.
    control.observe("control-observe");
    let close = |target: &Case<'_>, key: &str, exit| {
        let revision =
            target.show(&json!({"id": target.id}))["record"]["header"]["revision"].clone();
        target.submit(
            key,
            ACTOR,
            "case.close",
            json!({"outcome":"inconclusive","reason":"Stopped here.","unresolved_questions":[],
                "reopen_condition":null,"retrospective_ref":null,"retrospective_waived":"Only one reading was taken."}),
            json!([{"id": target.id, "revision": revision}]),
            exit,
        )
    };
    let refused = close(&case, "close-waived", 5);
    assert_eq!(refused["error"]["code"], "RETROSPECTIVE_REQUIRED");
    assert_eq!(
        blockers(&refused),
        [pair("account-for-observations", "closure_retrospective")]
    );
    let closed = close(&control, "close-control", 0);
    assert_eq!(closed["result"], "committed");
    let control_case = control.show(&json!({"id": control.id}))["record"]["body"].clone();
    assert_eq!(control_case["closure"]["gate_policy"]["packs"], json!([]));
}

#[test]
fn an_unavailable_pinned_pack_is_recorded_at_the_gate_and_never_silent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init(root);
    let case = start(root, "unavailable-case");
    let digest = adopt(&case);
    let hex = digest.strip_prefix("sha256:").unwrap();
    fs::remove_file(root.join("state/blobs/sha256").join(&hex[..2]).join(hex)).unwrap();

    let question = case.one(
        "question",
        "question.create",
        json!({"importance":"normal","wording":"Should the setup be reused?"}),
    );
    let receipt = case.submit(
        "propose",
        ACTOR,
        "decision.propose",
        json!({
            "question_ref":question,"choice":{"kind":"defer","reason":"Await data"},
            "compared_alternatives":[],"comparison_refs":[],"hard_conditions":[],"evidence_refs":[],
            "assumption_refs":[],"tradeoffs":[],"consequences":["Choice delayed"],
            "reopen_trigger":"Measurement available","reversal_conditions":[]
        }),
        json!([]),
        0,
    );
    let warnings = receipt["warnings"].to_string();
    assert!(warnings.contains("challenge-gates@1.0.0"), "{warnings}");
    assert!(warnings.contains("is unavailable"), "{warnings}");
    let decision = json!({"id":receipt["created"][0]["id"],"revision":1});
    let stored = case.body(&decision)["gate_policy"].clone();
    assert_eq!(
        stored["packs"],
        json!([{"pack_id":"challenge-gates","version":"1.0.0","digest":digest,
            "status":"unavailable","unavailable_reason":"bytes_unavailable"}])
    );
    assert_eq!(stored["rules"], json!([]));
}
