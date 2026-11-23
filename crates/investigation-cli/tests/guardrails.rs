//! Opt-in write-side guardrails (W10.19) through the public CLI: a pinned
//! pack can require a distinct actor, a distinct context or a recorded human
//! confirmation for conclusion.validate, decision.accept, source.withdraw,
//! observation.retract and classification.downgrade. A refusal writes nothing
//! and names the route to satisfy it; with no guardrail, nothing changes.
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:guardrail-test";

const PACK: &str = r#"schema_version = "inv.method-pack.v1"
id = "write-guardrails"
version = "1.0.0"
title = "Write guardrails"
summary = "Fixture pack that separates promotion and suppression from the reading context."
modes = ["explain", "measure", "evaluate", "explore", "record"]

[[guardrails]]
name = "separate-validator"
operation = "conclusion.validate"
requirement = "distinct_context"
rationale = "The context that read raw evidence does not validate what it supports."

[[guardrails]]
name = "separate-withdrawal"
operation = "source.withdraw"
requirement = "distinct_context"
rationale = "The context that read raw evidence does not withdraw sources."

[[guardrails]]
name = "confirm-retraction"
operation = "observation.retract"
requirement = "human_confirmation"
rationale = "A person confirms every retraction."

[[guardrails]]
name = "confirm-declassification"
operation = "classification.downgrade"
requirement = "human_confirmation"
rationale = "A person confirms every downgrade."

[[guardrails]]
name = "second-decider"
operation = "decision.accept"
requirement = "distinct_actor"
rationale = "The proposer does not accept their own decision."
"#;

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_SESSION")
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
    serde_json::from_str::<Value>(&stdout).expect(&stdout)
}

struct Case<'a> {
    root: &'a Path,
    id: String,
    /// `items[0]` of the `start` receipt: the attached reading.
    attachment: Value,
}

/// Optional envelope fields of one submission.
#[derive(Default)]
struct Envelope<'a> {
    actor: Option<&'a str>,
    session: Option<&'a str>,
    confirmation: Option<Value>,
    pre: Option<Value>,
}

impl Case<'_> {
    fn submit(&self, key: &str, op: &str, payload: Value, envelope: Envelope, exit: i32) -> Value {
        let mut command = json!({
            "schema_version":"inv.command.v1", "command_id":format!("{}-{key}", self.id), "case_id":self.id,
            "actor":envelope.actor.unwrap_or(ACTOR), "preconditions":envelope.pre.unwrap_or(json!([])),
            "operations":[{"op":op,"payload":payload}]
        });
        if let Some(session) = envelope.session {
            command["session"] = json!(session);
        }
        if let Some(confirmation) = envelope.confirmation {
            command["human_confirmation"] = confirmation;
        }
        run(
            self.root,
            &["record", "--input", "-", "--json"],
            Some(&command),
            exit,
        )
    }

    fn one(&self, key: &str, op: &str, payload: Value) -> Value {
        let receipt = self.submit(key, op, payload, Envelope::default(), 0);
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

    /// The latest journaled after-image of `id`.
    fn journaled(&self, id: &Value) -> Value {
        let events = self.root.join("state/cases").join(&self.id).join("events");
        let mut paths: Vec<_> = fs::read_dir(&events)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        paths.sort();
        paths
            .iter()
            .filter_map(|path| serde_json::from_slice::<Value>(&fs::read(path).unwrap()).ok())
            .flat_map(|event| {
                event["accepted_operations"]["records"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .rfind(|record| &record["header"]["id"] == id)
            .expect("the record is journaled")
    }

    fn revision(&self, id: &Value) -> Value {
        self.show(id)["record"]["header"]["revision"].clone()
    }

    /// Read the attached Artifact through `evidence read` under `session`.
    fn read_evidence(&self, session: &str) {
        let read = run(
            self.root,
            &[
                "--case",
                &self.id,
                "--actor",
                ACTOR,
                "--session",
                session,
                "evidence",
                "read",
                self.attachment["artifact_id"].as_str().unwrap(),
                "--selector-file",
                "selector.json",
                "--json",
            ],
            None,
            0,
        );
        assert_eq!(read["session_read"]["recorded"], true, "{read}");
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

    /// A Conclusion on one assessed Observation, requested for review and
    /// with a supporting validation review: ready to validate.
    fn ready_conclusion(&self) -> (Value, Value) {
        let observation = self.observe("observe");
        let observation = self.one("assess", "measurement.assess", json!({
            "observation_ref":observation,"channel":null,"validity":"valid",
            "verification":[{"kind":"equivalent","justification":"The setup reproduced a known reference value."}],
            "rationale":"Judged from the recorded setup.","limits":[]
        }));
        let candidate = self.one(
            "claim",
            "conclusion.create",
            json!({
                "statement":"The reading is 12.5 units.","scope":"This recorded reading.",
                "significance":"Check the setup before reuse.","support_refs":[observation],
                "contradiction_refs":[],"caveats":["One reading."],"confidence":null
            }),
        );
        let basis = self.show(&candidate)["review_basis"].clone();
        let mut pre = basis["read_set"].as_array().cloned().unwrap();
        pre.push(basis["subject_ref"].clone());
        self.submit(
            "ready",
            "conclusion.request_review",
            json!({"conclusion_ref":candidate}),
            Envelope {
                pre: Some(json!(pre)),
                ..Envelope::default()
            },
            0,
        );
        let ready = json!({"id":candidate["id"],"revision":2});
        let basis = self.show(&ready)["review_basis"].clone();
        let review = self.one("review", "review.submit", json!({
            "kind":"validation","subject_ref":basis["subject_ref"],"basis_digest":basis["basis_digest"],
            "read_set":basis["read_set"],"method":"Read the recorded basis.","materials_examined":[],
            "shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":["One reading."],
            "context_isolation":"fresh_context_unblinded","context":review_context()
        }));
        (ready, review)
    }

    /// Submit conclusion.validate for `ready` with its full preconditions.
    fn validate(
        &self,
        key: &str,
        ready: &Value,
        review: &Value,
        session: Option<&str>,
        exit: i32,
    ) -> Value {
        let basis = self.show(ready)["review_basis"].clone();
        let mut pre = basis["read_set"].as_array().cloned().unwrap();
        pre.extend([ready.clone(), review.clone()]);
        self.submit(
            key,
            "conclusion.validate",
            json!({"conclusion_ref":ready,"review_ref":review}),
            Envelope {
                session,
                pre: Some(json!(pre)),
                ..Envelope::default()
            },
            exit,
        )
    }
}

fn review_context() -> Value {
    json!({
        "saw_original_conclusion":{"state":"known","value":true},
        "gathered_new_evidence":{"state":"known","value":false},
        "method_or_source_changes":[],"activity":"Read the recorded basis.",
        "dependencies":"shared","dependence_basis":"Same recorded data."
    })
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
    let attachment = started["items"][0].clone();
    fs::write(
        root.join("selector.json"),
        serde_json::to_vec(&json!({"schema_version":"inv.selector.v1",
            "artifact_digest":attachment["digest"],"selector":{"type":"whole"}}))
        .unwrap(),
    )
    .unwrap();
    Case {
        root,
        id: started["created"][0]["id"].as_str().unwrap().to_owned(),
        attachment,
    }
}

/// Retain the guardrail pack through ingest and pin it; returns its digest.
fn adopt(case: &Case<'_>) -> String {
    fs::write(case.root.join("write-guardrails.toml"), PACK).unwrap();
    let retained = run(
        case.root,
        &["ingest", "--input", "-", "--json"],
        Some(&json!({
            "schema_version": "inv.ingest.v1", "command_id": format!("retain-pack-{}", case.id),
            "actor": ACTOR, "case_id": case.id,
            "source": {"title": "Write guardrails pack", "format": "application/toml"},
            "paths": ["write-guardrails.toml"]
        })),
        0,
    );
    let item = &retained["items"][0];
    let digest = item["digest"].as_str().unwrap().to_owned();
    let revision = case.revision(&json!({"id": case.id}));
    case.submit(
        &format!("adopt-{}", case.id),
        "method_pack.adopt",
        json!({"artifact_ref": {"id": item["artifact_id"], "revision": 1},
            "pack_id": "write-guardrails", "version": "1.0.0", "digest": digest,
            "reason": "Separate promotion and suppression from the reading context."}),
        Envelope {
            pre: Some(json!([{"id": case.id, "revision": revision}])),
            ..Envelope::default()
        },
        0,
    );
    digest
}

/// The single guardrail blocker of a refusal, after checking the shape every
/// refusal shares.
fn blocker(refused: &Value, guardrail: &str, requirement: &str) -> Value {
    assert_eq!(refused["error"]["code"], "GATE_FAILED", "{refused:#}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Nothing was written"),
        "{refused:#}"
    );
    let expected = &refused["error"]["expected"];
    assert!(
        expected["limits"]
            .as_str()
            .unwrap()
            .contains("self-reported")
    );
    let blockers = expected["guardrail_blockers"].as_array().unwrap();
    assert_eq!(blockers.len(), 1, "{refused:#}");
    let blocker = blockers[0].clone();
    assert_eq!(blocker["pack_id"], "write-guardrails");
    assert_eq!(blocker["guardrail"], guardrail);
    assert_eq!(blocker["requirement"], requirement);
    assert!(blocker["repair"]["route"].as_str().is_some());
    blocker
}

#[test]
fn the_session_that_read_untrusted_evidence_cannot_validate_and_solo_work_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init(root);

    // Without a guardrail, solo validation works as before, even from a
    // session that read the evidence, and stores no guardrail.
    let solo = start(root, "solo-case");
    let (ready, review) = solo.ready_conclusion();
    solo.read_evidence("solo-reader");
    let validated = solo.validate("validate", &ready, &review, Some("solo-reader"), 0);
    assert_eq!(validated["result"], "committed");
    let body = solo.body(&json!({"id":ready["id"],"revision":3}));
    assert_eq!(body["status"], "validated");
    assert_eq!(body["gate_policy"]["packs"], json!([]));
    assert!(body["gate_policy"].get("guardrail").is_none(), "{body:#}");

    let case = start(root, "guarded-case");
    let digest = adopt(&case);
    let (ready, review) = case.ready_conclusion();
    case.read_evidence("reader");

    // The session that read the untrusted Artifact cannot validate the
    // conclusion it supports, and nothing is written.
    let refused = case.validate("validate", &ready, &review, Some("reader"), 5);
    let unmet = blocker(&refused, "separate-validator", "distinct_context");
    assert_eq!(unmet["operation"], "conclusion.validate");
    assert_eq!(unmet["digest"], json!(digest));
    assert_eq!(unmet["unmet"]["missing"], "distinct_context");
    assert_eq!(unmet["unmet"]["session"], "reader");
    assert_eq!(
        unmet["unmet"]["untrusted_reads"],
        json!([{"id":case.attachment["artifact_id"],"revision":1}])
    );
    assert_eq!(unmet["repair"]["change"], "session");
    assert_eq!(case.revision(&ready), 2);
    assert_eq!(case.body(&ready)["status"], "review_ready");

    // A write that declares no session cannot show a distinct context.
    let refused = case.validate("validate-undeclared", &ready, &review, None, 5);
    let unmet = blocker(&refused, "separate-validator", "distinct_context");
    assert_eq!(unmet["unmet"], json!({"missing":"declared_session"}));

    // A separate session that read nothing through the tool may validate; the
    // revision records the guardrail, the declared session and the limit.
    // The refused command ID was never committed, so it can be reused.
    let validated = case.validate("validate", &ready, &review, Some("validator"), 0);
    assert_eq!(validated["result"], "committed");
    let body = case.body(&json!({"id":ready["id"],"revision":3}));
    assert_eq!(body["status"], "validated");
    let guardrail = &body["gate_policy"]["guardrail"];
    assert_eq!(guardrail["operation"], "conclusion.validate");
    assert_eq!(guardrail["rule_version"], "write-guardrails.v1");
    assert_eq!(guardrail["session"], "validator");
    assert_eq!(
        guardrail["packs"],
        json!([{"pack_id":"write-guardrails","version":"1.0.0","digest":digest,"status":"available"}])
    );
    assert_eq!(
        guardrail["rules"],
        json!([{"pack_id":"write-guardrails","version":"1.0.0","digest":digest,
            "guardrail":"separate-validator","requirement":"distinct_context"}])
    );
    assert!(guardrail.get("human_confirmation").is_none());
    assert!(
        guardrail["limits"]
            .as_str()
            .unwrap()
            .contains("self-reported")
    );

    // Withdrawal: the reading session is refused; a clean one is recorded
    // with the guardrail inside the withdrawal entry.
    let source = json!({"id":case.attachment["source_id"],"revision":1});
    let withdraw = |key: &str, session: &str, exit| {
        case.submit(
            key,
            "source.withdraw",
            json!({"source_ref":source,"reason":"superseded","scope":{"extent":"entire"},
                "rationale":"A corrected reading replaces it.","evidence_refs":[]}),
            Envelope {
                session: Some(session),
                ..Envelope::default()
            },
            exit,
        )
    };
    let refused = withdraw("withdraw", "reader", 5);
    blocker(&refused, "separate-withdrawal", "distinct_context");
    assert_eq!(case.revision(&source), 1);
    withdraw("withdraw", "withdrawer", 0);
    let body = case.body(&json!({"id":source["id"],"revision":2}));
    assert_eq!(body["status"], "withdrawn");
    assert_eq!(
        body["withdrawals"][0]["guardrail"]["rules"][0]["guardrail"],
        "separate-withdrawal"
    );
}

#[test]
fn suppression_and_declassification_need_the_required_confirmation_or_actor() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init(root);
    let case = start(root, "confirm-case");
    adopt(&case);

    // Recording is never guarded.
    let observation = case.observe("observe");
    let question = case.one(
        "question",
        "question.create",
        json!({"importance":"normal","wording":"Which setup should be reused?"}),
    );

    // A classification downgrade without the required confirmation is
    // rejected and nothing is written.
    case.one(
        "raise",
        "classification.raise",
        json!({"subject_ref":question,"level":"restricted","reason":"Names a customer."}),
    );
    let downgrade = json!({
        "subject_ref":question,"level":"internal","reason":"Customer name removed.",
        "transformation":{"inputs":[{"input_ref":question,"digest":null}],"transformation":"manual_redaction",
            "version":"1","redaction_decisions":[{"scope":"Customer name","action":"removed","rationale":"Not needed."}],
            "output_digest":null},
        "review":{"method":"Read the whole question.","coverage":"whole_output","examined":[question]}
    });
    // The question's content state and the case head: neither moves on a
    // refusal.
    let level = |case: &Case<'_>| {
        let shown = case.show(&question);
        json!([shown["content_state"], shown["snapshot"]])
    };
    let before = level(&case);
    assert_eq!(before[0], "restricted");
    let refused = case.submit(
        "downgrade",
        "classification.downgrade",
        downgrade.clone(),
        Envelope::default(),
        5,
    );
    let unmet = blocker(&refused, "confirm-declassification", "human_confirmation");
    assert_eq!(unmet["unmet"], json!({"missing":"human_confirmation"}));
    assert_eq!(
        unmet["repair"]["shape"]["confirms"],
        json!([{"op":"classification.downgrade","subject":question["id"]}])
    );
    assert_eq!(level(&case), before);

    // A confirmation of another write does not count.
    let confirmation = |op: &str, subject: &Value| {
        json!({"confirmed_by":"human:reviewer","method":"Approved at an interactive prompt.",
            "confirms":[{"op":op,"subject":subject["id"]}]})
    };
    let refused = case.submit(
        "downgrade",
        "classification.downgrade",
        downgrade.clone(),
        Envelope {
            confirmation: Some(confirmation("observation.retract", &question)),
            ..Envelope::default()
        },
        5,
    );
    let unmet = blocker(&refused, "confirm-declassification", "human_confirmation");
    assert_eq!(
        unmet["unmet"]["missing"],
        "human_confirmation_for_this_write"
    );
    assert_eq!(level(&case), before);

    // With it, the downgrade is recorded with the confirmation as a claim.
    let accepted = case.submit(
        "downgrade",
        "classification.downgrade",
        downgrade,
        Envelope {
            confirmation: Some(confirmation("classification.downgrade", &question)),
            ..Envelope::default()
        },
        0,
    );
    let change = json!({"id":accepted["created"][0]["id"],"revision":1});
    // The change is labelled restricted, so `show` withholds its body; the
    // journal keeps the accepted after-image.
    let journaled = case.journaled(&change["id"]);
    let recorded = &journaled["body"]["classification_change"]["guardrail"];
    assert!(recorded.is_object(), "{journaled:#}");
    assert_eq!(
        recorded["human_confirmation"]["confirmed_by"],
        "human:reviewer"
    );
    assert_eq!(
        recorded["human_confirmation"]["method"],
        "Approved at an interactive prompt."
    );
    let after = level(&case);
    assert_ne!(after[0], "restricted", "{after}");
    assert_ne!(after[1], before[1]);

    // Retraction needs a confirmation too.
    let retract = |key: &str, confirmation: Option<Value>, exit| {
        case.submit(
            key,
            "observation.retract",
            json!({"observation_ref":observation,"revision_reason":"Scope error."}),
            Envelope {
                confirmation,
                ..Envelope::default()
            },
            exit,
        )
    };
    let refused = retract("retract", None, 5);
    blocker(&refused, "confirm-retraction", "human_confirmation");
    assert_eq!(case.revision(&observation), 1);
    retract(
        "retract",
        Some(confirmation("observation.retract", &observation)),
        0,
    );
    let body = case.body(&json!({"id":observation["id"],"revision":2}));
    assert_eq!(body["status"], "retracted");
    assert_eq!(
        body["guardrail"]["human_confirmation"]["confirms"],
        json!([{"op":"observation.retract","subject":observation["id"]}])
    );

    // Decision acceptance: the proposer is refused, another actor is not.
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
        "evidence_refs":[],"assumption_refs":[],"tradeoffs":[],"consequences":["Reuse the setup."],
        "reopen_trigger":"A reading disagrees.","expected_outcome":null,"reversal_conditions":[]
    }));
    let basis = case.show(&proposed)["review_basis"].clone();
    let review = case.one("decision-review", "review.submit", json!({
        "kind":"validation","subject_ref":basis["subject_ref"],"basis_digest":basis["basis_digest"],
        "read_set":basis["read_set"],"method":"Read the recorded basis.","materials_examined":[],
        "shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":["One reading."],
        "context_isolation":"fresh_context_unblinded","context":review_context()
    }));
    let accept = |key: &str, actor: &str, exit| {
        case.submit(
            key,
            "decision.accept",
            json!({"decision_ref":proposed,"review_ref":review,"acknowledgments":[]}),
            Envelope {
                actor: Some(actor),
                ..Envelope::default()
            },
            exit,
        )
    };
    let refused = accept("accept", ACTOR, 5);
    let unmet = blocker(&refused, "second-decider", "distinct_actor");
    assert_eq!(unmet["unmet"]["authors"], json!([ACTOR]));
    assert_eq!(unmet["repair"]["change"], "actor");
    accept("accept-decider", "agent:decider", 0);
    let body = case.body(&json!({"id":proposed["id"],"revision":2}));
    assert_eq!(
        body["gate_policy"]["guardrail"]["rules"][0]["guardrail"],
        "second-decider"
    );
}

#[test]
fn a_malformed_human_confirmation_is_rejected_at_the_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init(root);
    let case = start(root, "envelope-case");
    let refused = case.submit(
        "note",
        "question.create",
        json!({"importance":"normal","wording":"Is the setup reusable?"}),
        Envelope {
            confirmation: Some(json!({"confirmed_by":"human:reviewer","method":" ","confirms":[]})),
            ..Envelope::default()
        },
        2,
    );
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");
    assert_eq!(refused["error"]["path"], "/human_confirmation/method");
}
