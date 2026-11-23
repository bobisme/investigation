//! The optional example packs under `methods/` (W09.08) through the public
//! CLI. Each loads, pins by digest, drives a `next` card and accepts its
//! extension values in its own small case, using the same public schema as
//! any third-party pack. Nothing here is built in: the pack files are
//! retained and adopted like any other.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:example-packs-test";
const DEBUGGING: &str = include_str!("../../../methods/debugging.toml");
const PERFORMANCE: &str = include_str!("../../../methods/performance.toml");

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
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
        "{args:?}\n{stdout}\n{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&stdout).expect(&stdout)
}

struct Case<'a> {
    root: &'a Path,
    id: String,
    /// The retained reading: `derivative_id` and `digest`.
    attachment: Value,
}

impl Case<'_> {
    fn submit(&self, key: &str, op: Value, pre: Value, exit: i32) -> Value {
        let command = json!({
            "schema_version": "inv.command.v1", "command_id": key, "case_id": self.id,
            "actor": ACTOR, "preconditions": pre, "operations": [op],
        });
        run(
            self.root,
            &["record", "--input", "-", "--json"],
            Some(&command),
            exit,
        )
    }

    fn one(&self, key: &str, op: &str, payload: Value) -> Value {
        let receipt = self.submit(key, json!({"op": op, "payload": payload}), json!([]), 0);
        let change = receipt["created"]
            .get(0)
            .or_else(|| receipt["updated"].get(0))
            .unwrap();
        json!({"id": change["id"], "revision": change["revision"]})
    }

    fn show(&self, id: &Value) -> Value {
        let id = id["id"].as_str().or(id.as_str()).unwrap();
        run(
            self.root,
            &["--case", &self.id, "show", id, "--depth", "full", "--json"],
            None,
            0,
        )
    }

    fn body(&self, id: &Value) -> Value {
        self.show(id)["record"]["body"].clone()
    }

    fn observation_op(&self, extensions: Option<Value>) -> Value {
        let selector = json!({"schema_version":"inv.selector.v1","artifact_digest":self.attachment["digest"],
            "selector":{"type":"text_lines","start_line":1,"end_line":1,"encoding":"utf-8","parser_version":"inv.text.utf8.v1"}});
        let mut operation = json!({"op": "observation.add", "payload": {
            "value":{"kind":"text","statement":"The recorded reading is 12.5 units."},
            "basis":{"kind":"source","artifact_ref":{"id":self.attachment["derivative_id"],"revision":1},
                "selector":selector,"text_quote":{"exact":"Reading 12.5 units"}},
            "validity":"unknown","quality":[],"limitations":[],"evidence_kind":"observational","variables_changed":[]
        }});
        if let Some(extensions) = extensions {
            operation["extensions"] = extensions;
        }
        operation
    }

    fn observe(&self, key: &str, extensions: Option<Value>, exit: i32) -> Value {
        self.submit(key, self.observation_op(extensions), json!([]), exit)
    }

    /// Retain the pack file through ingest and pin it; returns its digest.
    fn adopt(&self, id: &str, bytes: &str) -> String {
        let file = format!("{id}.toml");
        fs::write(self.root.join(&file), bytes).unwrap();
        let retained = run(
            self.root,
            &["ingest", "--input", "-", "--json"],
            Some(&json!({
                "schema_version": "inv.ingest.v1", "command_id": format!("retain-{id}"),
                "actor": ACTOR, "case_id": self.id,
                "source": {"title": format!("{id} pack"), "format": "application/toml"},
                "paths": [file]
            })),
            0,
        );
        let item = &retained["items"][0];
        let digest = item["digest"].as_str().unwrap().to_owned();
        let revision = self.show(&json!({"id": self.id}))["record"]["header"]["revision"].clone();
        self.submit(
            &format!("adopt-{id}"),
            json!({"op": "method_pack.adopt", "payload": {
                "artifact_ref": {"id": item["artifact_id"], "revision": 1}, "pack_id": id,
                "version": "1.0.0", "digest": digest, "reason": "Use the example pack."
            }}),
            json!([{"id": self.id, "revision": revision}]),
            0,
        );
        digest
    }

    fn next(&self) -> Value {
        run(self.root, &["--case", &self.id, "next", "--json"], None, 0)
    }

    /// The card attached to the recommendation from `rule_id`.
    fn guide_for(&self, rule_id: &str) -> Value {
        let next = self.next();
        next["recommendations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["rule_id"] == rule_id)
            .unwrap_or_else(|| panic!("no recommendation for {rule_id}: {next:#}"))["guide"]
            .clone()
    }

    fn guide(&self, card: &str, pack: &str) -> Value {
        run(
            self.root,
            &["--case", &self.id, "guide", card, "--pack", pack, "--json"],
            None,
            0,
        )
    }

    fn pack_policy_blockers(refused: &Value) -> Vec<String> {
        refused["error"]["expected"]["policy_blockers"]
            .as_array()
            .unwrap_or_else(|| panic!("{refused:#}"))
            .iter()
            .map(|blocker| blocker["policy"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Create a conclusion that cites an assessed observation, carrying
    /// `extensions` when given, and ask for its review.
    fn conclusion(&self, key: &str, observation: &Value, extensions: Option<Value>) -> Value {
        let mut create = json!({"op": "conclusion.create", "payload": {
            "statement":"The reading is 12.5 units.","scope":"This recorded reading.",
            "significance":"Check the setup before reuse.","support_refs":[observation],
            "contradiction_refs":[],"caveats":["One reading."],"confidence":{
                "level":"moderate","rationale":"One calibrated reading.","limitations":[],
                "what_would_change_it":null,
                "downgrade_reasons":{"validity":"ok","consistency":"concern","directness":"ok",
                    "precision":"not_assessed","completeness":"not_assessed"}}
        }});
        if let Some(extensions) = extensions {
            create["extensions"] = extensions;
        }
        let created = self.submit(key, create, json!([]), 0);
        let candidate = json!({"id": created["created"][0]["id"], "revision": 1});
        let basis = self.show(&candidate)["review_basis"].clone();
        let mut pre = basis["read_set"].as_array().cloned().unwrap();
        pre.push(basis["subject_ref"].clone());
        self.submit(
            &format!("{key}-ready"),
            json!({"op": "conclusion.request_review", "payload": {"conclusion_ref": candidate}}),
            json!(pre),
            0,
        );
        json!({"id": candidate["id"], "revision": 2})
    }

    fn review(&self, key: &str, subject: &Value) -> Value {
        let basis = self.show(subject)["review_basis"].clone();
        self.one(
            key,
            "review.submit",
            json!({
                "kind":"validation","subject_ref":basis["subject_ref"],"basis_digest":basis["basis_digest"],
                "read_set":basis["read_set"],"method":"Read the recorded basis.","materials_examined":[],
                "shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":["One reading."],
                "context_isolation":"fresh_context_unblinded","context":{
                    "saw_original_conclusion":{"state":"known","value":true},
                    "gathered_new_evidence":{"state":"known","value":false},
                    "method_or_source_changes":[],"activity":"Read the recorded basis.",
                    "dependencies":"shared","dependence_basis":"Same recorded data."}
            }),
        )
    }

    fn validate(&self, key: &str, subject: &Value, review: &Value, exit: i32) -> Value {
        let basis = self.show(subject)["review_basis"].clone();
        let mut pre = basis["read_set"].as_array().cloned().unwrap();
        pre.extend([subject.clone(), review.clone()]);
        self.submit(
            key,
            json!({"op": "conclusion.validate", "payload": {"conclusion_ref": subject, "review_ref": review}}),
            json!(pre),
            exit,
        )
    }

    /// Record an assessed observation to support a conclusion.
    fn assessed_observation(&self, key: &str) -> Value {
        let created = self.observe(key, None, 0);
        let observation = json!({"id": created["created"][0]["id"], "revision": 1});
        self.one(
            &format!("{key}-assess"),
            "measurement.assess",
            json!({
                "observation_ref":observation,"channel":null,"validity":"valid",
                "verification":[{"kind":"equivalent","justification":"The setup reproduced a known reference value."}],
                "rationale":"Judged from the recorded setup.","limits":[]
            }),
        )
    }
}

fn start<'a>(root: &'a Path, mode: &str, question: &str) -> Case<'a> {
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
    let command = json!({
        "schema_version": "inv.command.v1", "command_id": "case", "case_id": Value::Null,
        "actor": ACTOR, "preconditions": [],
        "operations": [{"op": "case.create", "payload": {"title": question, "mode": mode}}],
    });
    let created = run(
        root,
        &["record", "--input", "-", "--json"],
        Some(&command),
        0,
    );
    let id = created["created"][0]["id"].as_str().unwrap().to_owned();
    let retained = run(
        root,
        &["ingest", "--input", "-", "--json"],
        Some(&json!({
            "schema_version": "inv.ingest.v1", "command_id": "retain-reading", "actor": ACTOR,
            "case_id": id, "source": {"title": "Reading", "format": "text/plain"},
            "paths": ["reading.txt"], "extract_text": true
        })),
        0,
    );
    Case {
        root,
        id,
        attachment: retained["items"][0].clone(),
    }
}

fn card_names(list: &Value, pack: &str) -> Vec<String> {
    let mut names: Vec<String> = list["stages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|stage| stage["cards"].as_array().unwrap())
        .filter(|card| card["pack_id"] == pack)
        .map(|card| card["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn the_debugging_pack_pins_drives_a_card_and_gates_causal_conclusions() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(
        dir.path(),
        "explain",
        "Why does the build fail intermittently?",
    );
    let question = case.one(
        "question",
        "question.create",
        json!({"wording": "What makes the build fail?", "importance": "normal"}),
    );
    case.one(
        "hypothesis",
        "hypothesis.create",
        json!({"question_ref": question, "statement": "A race in the cache step.",
            "possible_falsifiers": [], "rival_refs": [], "assumption_refs": []}),
    );

    // Before adoption the general card answers the open hypothesis.
    let guide = case.guide_for("hypothesis.no_assessment");
    assert_eq!(
        (guide["pack_id"].as_str(), guide["card"].as_str()),
        (Some("general"), Some("discriminate"))
    );

    let digest = case.adopt("debugging", DEBUGGING);
    let guide = case.guide_for("hypothesis.no_assessment");
    assert_eq!(guide["pack_id"], "debugging");
    assert_eq!(guide["card"], "discriminate-by-reproduction");
    assert_eq!(guide["origin"], "pinned");
    assert_eq!(guide["pack_digest"], json!(digest));
    for card in ["bisect-bookkeeping", "intervene-and-revert"] {
        let shown = case.guide(card, "debugging");
        assert_eq!(shown["guide"]["pack"]["digest"], json!(digest));
    }
    let list = run(
        case.root,
        &["--case", &case.id, "method", "list", "--json"],
        None,
        0,
    );
    assert_eq!(
        card_names(&list, "debugging"),
        [
            "bisect-bookkeeping",
            "discriminate-by-reproduction",
            "intervene-and-revert"
        ]
    );

    // Reproduction rate as k of n, and bisection bookkeeping, are typed and
    // checked; a bad value names its JSON Pointer.
    let rate = json!({"debugging": {"reproduction": {"failures": 3, "runs": 10}}});
    let accepted = case.observe("rate", Some(rate.clone()), 0);
    let id = json!({"id": accepted["created"][0]["id"], "revision": 1});
    assert_eq!(case.show(&id)["record"]["header"]["extensions"], rate);
    let refused = case.observe(
        "rate-bad",
        Some(json!({"debugging": {"reproduction": {"failures": 3, "runs": 0}}})),
        2,
    );
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/extensions/debugging/reproduction/runs"
    );
    let refused = case.observe(
        "ledger-wrong-kind",
        Some(json!({"debugging": {"bisection": {"steps_run": 6}}})),
        2,
    );
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");

    // Policy: validation of a conclusion needs an intervention or a reason.
    let observation = case.assessed_observation("support");
    let bare = case.conclusion("claim-bare", &observation, None);
    let review = case.review("review-bare", &bare);
    let refused = case.validate("validate-bare", &bare, &review, 5);
    assert_eq!(refused["error"]["code"], "GATE_FAILED");
    assert_eq!(
        Case::pack_policy_blockers(&refused),
        ["intervention-or-reason"]
    );
    assert_eq!(
        refused["error"]["expected"]["policy_blockers"][0]["pack_id"],
        "debugging"
    );

    let reasoned = case.conclusion(
        "claim-reasoned",
        &observation,
        Some(json!({"debugging": {"intervention_basis": "Disabled the cache step: 0 of 20 runs failed, 7 of 20 with it."}})),
    );
    let review = case.review("review-reasoned", &reasoned);
    case.validate("validate-reasoned", &reasoned, &review, 0);
    let validated = case.body(&json!({"id": reasoned["id"], "revision": 3}));
    assert_eq!(validated["gate_policy"]["packs"][0]["pack_id"], "debugging");
    assert_eq!(
        validated["gate_policy"]["rules"][0]["policy"],
        "intervention-or-reason"
    );
}

#[test]
fn the_performance_pack_pins_drives_cards_and_requires_comparability_fields() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(dir.path(), "measure", "Did the change alter the timing?");
    let digest = case.adopt("performance", PERFORMANCE);
    let list = run(
        case.root,
        &["--case", &case.id, "method", "list", "--json"],
        None,
        0,
    );
    assert_eq!(
        card_names(&list, "performance"),
        [
            "baseline-comparable",
            "count-every-attempt",
            "measure-with-workload"
        ]
    );

    // A timing with no validity context is answered by the pack's card, which
    // points at the core's measurement flags rather than repeating them.
    let fields = json!({"performance": {
        "workload_id": "checkout-flow", "environment_id": "bench-host-a", "sample_count": 300}});
    let accepted = case.observe("timing", Some(fields.clone()), 0);
    let id = json!({"id": accepted["created"][0]["id"], "revision": 1});
    assert_eq!(case.show(&id)["record"]["header"]["extensions"], fields);
    // The card is triggered by the unassessed measurement. That gap is
    // blocked by quality, so `next` does not rank it; the method list says so.
    let list = run(
        case.root,
        &["--case", &case.id, "method", "list", "--json"],
        None,
        0,
    );
    let triggered: Vec<_> = list["stages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|stage| stage["cards"].as_array().unwrap())
        .filter(|card| card["pack_id"] == "performance" && card["triggered_now"] == true)
        .map(|card| card["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(triggered, ["measure-with-workload"]);

    // A run whose outcome is not known drives a ranked `next` card.
    let begun = run(
        case.root,
        &["capture", "begin", "--input", "-", "--format", "json"],
        Some(&json!({
            "key": "run-0001/begin", "actor": ACTOR,
            "producer": {"name": "fixture-producer", "version": "0.1.0"},
            "external_run_id": "run-0001", "intent": {"label": "synthetic timed run"},
            "case": {"kind": "existing", "case_id": case.id}
        })),
        0,
    );
    assert_eq!(begun["commit"]["result"], "committed");
    let guide = case.guide_for("run.outcome_incomplete_or_unknown");
    assert_eq!(guide["pack_id"], "performance");
    assert_eq!(guide["card"], "count-every-attempt");
    assert_eq!(guide["origin"], "pinned");
    assert_eq!(guide["pack_digest"], json!(digest));
    let shown = case.guide("baseline-comparable", "performance");
    assert_eq!(shown["guide"]["pack"]["digest"], json!(digest));
    let refused = case.observe(
        "timing-bad",
        Some(json!({"performance": {"sample_count": -1}})),
        2,
    );
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/extensions/performance/sample_count"
    );

    // Policy: promotion needs the comparability fields.
    let observation = case.assessed_observation("support");
    let bare = case.conclusion("claim-bare", &observation, None);
    let review = case.review("review-bare", &bare);
    let refused = case.validate("validate-bare", &bare, &review, 5);
    assert_eq!(
        Case::pack_policy_blockers(&refused),
        ["baseline-comparability"]
    );
    let partial = case.conclusion(
        "claim-partial",
        &observation,
        Some(json!({"performance": {"workload_id": "checkout-flow"}})),
    );
    let review = case.review("review-partial", &partial);
    let refused = case.validate("validate-partial", &partial, &review, 5);
    assert_eq!(
        Case::pack_policy_blockers(&refused),
        ["baseline-comparability"]
    );
    let complete = case.conclusion("claim-complete", &observation, Some(fields));
    let review = case.review("review-complete", &complete);
    case.validate("validate-complete", &complete, &review, 0);
}

#[test]
fn neither_pack_is_built_in_or_pinned_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(dir.path(), "explain", "Does anything apply by default?");
    let list = run(
        case.root,
        &["--case", &case.id, "method", "list", "--json"],
        None,
        0,
    );
    assert_eq!(list["packs"].as_array().unwrap().len(), 1);
    assert!(card_names(&list, "debugging").is_empty());
    assert!(card_names(&list, "performance").is_empty());
    run(
        case.root,
        &["guide", "bisect-bookkeeping", "--json"],
        None,
        3,
    );
    run(
        case.root,
        &["guide", "measure-with-workload", "--json"],
        None,
        3,
    );
}
