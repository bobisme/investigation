//! Advice feedback (plan §7.5) through the public CLI: `annotation.add` with
//! `role: advice_feedback` defers one emitted gap. `next` stops ranking it,
//! `frontier` keeps listing it, a material change or a resumption brings it
//! back, and a deferred blocker still blocks its promotion.
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:cli-test";
const RULE_VERSION: &str = "slice-gaps.v2";

fn output(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.stdin(if stdin.is_some() {
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
}

impl Case<'_> {
    fn start(root: &Path) -> (Case<'_>, Value) {
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
                "Can the reading be interpreted?",
                "--attach",
                "reading.txt",
                "--command-id",
                "advice-case",
                "--actor",
                ACTOR,
                "--json",
            ],
            None,
            0,
        );
        let id = started["created"][0]["id"].as_str().unwrap().to_owned();
        (Case { root, id }, started)
    }

    fn submit(&self, key: &str, op: &str, payload: Value, pre: Value, exit: i32) -> Value {
        let command = json!({
            "schema_version":"inv.command.v1", "command_id":key, "case_id":self.id,
            "actor":ACTOR, "preconditions":pre, "operations":[{"op":op,"payload":payload}]
        });
        run(
            self.root,
            &["record", "--input", "-", "--json"],
            Some(&command),
            exit,
        )
    }

    fn one(&self, key: &str, op: &str, payload: Value) -> Value {
        let receipt = self.submit(key, op, payload, json!([]), 0);
        let change = receipt["created"]
            .get(0)
            .or_else(|| receipt["updated"].get(0))
            .unwrap();
        json!({"id":change["id"],"revision":change["revision"]})
    }

    fn feedback(&self, key: &str, gap: &Value, disposition: &str, exit: i32) -> Value {
        self.submit(
            key,
            "annotation.add",
            json!({
                "subject_refs":gap["subject_refs"],"role":"advice_feedback",
                "content":{"kind":"text","text":"Not useful until the controlled build exists."},
                "advice_feedback":{"rule_id":gap["rule_id"],"rule_version":gap["rule_version"],
                    "disposition":disposition,"revisit_condition":"The controlled build is available."}
            }),
            json!([]),
            exit,
        )
    }

    fn next(&self) -> Value {
        run(self.root, &["--case", &self.id, "next", "--json"], None, 0)
    }

    fn frontier(&self, args: &[&str]) -> Value {
        let mut argv = vec!["--case", &self.id, "frontier", "--json"];
        argv.extend(args);
        run(self.root, &argv, None, 0)
    }

    /// The one frontier gap with this rule ID about `subject`.
    fn gap(&self, rule: &str, subject: &Value) -> Value {
        let page = self.frontier(&["--rule", rule, "--reads", subject["id"].as_str().unwrap()]);
        let gaps = page["gaps"].as_array().unwrap();
        assert_eq!(gaps.len(), 1, "{page:#}");
        gaps[0].clone()
    }

    fn show(&self, id: &Value, depth: &str) -> Value {
        run(
            self.root,
            &[
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
}

fn ranked(next: &Value) -> Vec<String> {
    next["recommendations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["gap_id"].as_str().unwrap().to_owned())
        .collect()
}

fn hypothesis(question: &Value, statement: &str) -> Value {
    json!({
        "question_ref":question,"statement":statement,"applicability":"The recorded reading.",
        "assumption_refs":[],"possible_falsifiers":["A second reading differs."],"rival_refs":[]
    })
}

#[test]
fn deferral_leaves_next_but_not_frontier_and_a_material_change_brings_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let (case, _) = Case::start(dir.path());
    let question = case.one(
        "question",
        "question.create",
        json!({"wording":"Is the reading trustworthy?","importance":"normal"}),
    );
    let hyp = case.one(
        "hypothesis",
        "hypothesis.create",
        hypothesis(&question, "The instrument drifted."),
    );
    let gap = case.gap("hypothesis.no_assessment", &hyp);
    let gap_id = gap["gap_id"].as_str().unwrap().to_owned();
    let rule_status = gap["status"].clone();
    assert_ne!(rule_status, "deferred");
    assert!(ranked(&case.next()).contains(&gap_id));

    // Refusals name the exact repair and commit nothing.
    let mut stale = gap.clone();
    stale["rule_version"] = json!("slice-gaps.v0");
    let refused = case.feedback("defer-old-rule", &stale, "defer", 2);
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/payload/advice_feedback/rule_version"
    );
    assert_eq!(refused["error"]["expected"], RULE_VERSION);
    let mut other = gap.clone();
    other["rule_id"] = json!("conclusion.disputed");
    let refused = case.feedback("defer-not-emitted", &other, "defer", 2);
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/payload/advice_feedback/rule_id"
    );
    assert!(
        refused["error"]["expected"]
            .as_array()
            .unwrap()
            .contains(&json!(gap_id)),
        "{refused:#}"
    );
    let refused = case.feedback("resume-nothing", &gap, "resume", 2);
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/payload/advice_feedback/disposition"
    );

    // Deferring removes the item from next but not from frontier.
    let receipt = case.feedback("defer", &gap, "defer", 0);
    let feedback = json!({"id":receipt["created"][0]["id"],"revision":1});
    let next = case.next();
    assert!(!ranked(&next).contains(&gap_id), "{next:#}");
    assert_eq!(next["counts"]["deferred"], 1);
    let deferred = next["not_ranked"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["status"] == "deferred")
        .unwrap();
    assert_eq!(deferred["count"], 1);
    let listed = case.gap("hypothesis.no_assessment", &hyp);
    assert_eq!(listed["status"], "deferred");
    assert_eq!(listed["advice"]["state"], "deferred");
    assert_eq!(listed["advice"]["rule_status"], rule_status);
    assert_eq!(listed["advice"]["feedback_ref"], feedback);
    assert_eq!(listed["advice"]["recorded_by"], ACTOR);
    assert_eq!(listed["advice"]["has_revisit_condition"], true);
    let only_deferred = case.frontier(&["--status", "deferred"]);
    assert_eq!(only_deferred["gaps"][0]["gap_id"], gap_id);
    let text = output(
        dir.path(),
        &["--case", &case.id, "frontier", "--status", "deferred"],
        None,
        0,
    );
    assert!(
        text.contains(&format!("advice: deferred by {ACTOR} in")),
        "{text}"
    );
    // The reason stays in the attributed annotation; the gap never copies it.
    assert!(!text.contains("controlled build"), "{text}");
    let annotation = case.show(&feedback, "full");
    assert_eq!(annotation["record"]["body"]["role"], "advice_feedback");
    assert_eq!(
        annotation["record"]["body"]["advice_feedback"]["gap_id"],
        gap_id
    );
    assert_eq!(
        annotation["record"]["body"]["advice_feedback"]["material_basis"],
        gap["read_set"]
    );

    // A material change to the target re-surfaces it, with the reason.
    let amended = case.one(
        "amend",
        "hypothesis.amend",
        json!({"hypothesis_ref":hyp,"rationale":"Narrow to thermal drift.",
            "replacement":hypothesis(&question, "The instrument drifted with temperature.")}),
    );
    let back = case.gap("hypothesis.no_assessment", &amended);
    assert_eq!(back["status"], rule_status);
    assert_eq!(back["advice"]["state"], "resurfaced");
    let because = &back["advice"]["resurfaced_because"][0];
    assert_eq!(because["kind"], "basis_changed");
    assert_eq!(because["record_ref"], hyp);
    assert_eq!(because["trigger"]["kind"], "material_revision");
    let next = case.next();
    let item = next["recommendations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["gap_id"] == gap_id)
        .expect("the re-surfaced gap is ranked again");
    assert_eq!(item["advice"]["state"], "resurfaced");
    assert_eq!(next["counts"]["deferred"], 0);

    // A fresh deferral of the new revision holds; an explicit resume ends it.
    case.feedback("defer-again", &back, "defer", 0);
    assert!(!ranked(&case.next()).contains(&gap_id));
    case.feedback("resume", &back, "resume", 0);
    let resumed = case.gap("hypothesis.no_assessment", &amended);
    assert_eq!(resumed["status"], rule_status);
    assert_eq!(resumed["advice"]["state"], "resumed");
    assert!(ranked(&case.next()).contains(&gap_id));
}

#[test]
fn a_deferred_blocker_still_blocks_its_promotion_and_keeps_its_caveat() {
    let dir = tempfile::tempdir().unwrap();
    let (case, started) = Case::start(dir.path());
    let artifact = started["items"][0]["derivative_id"].as_str().unwrap();
    let selector = json!({"schema_version":"inv.selector.v1","artifact_digest":started["items"][0]["digest"],"selector":{"type":"text_lines","start_line":1,"end_line":1,"encoding":"utf-8","parser_version":"inv.text.utf8.v1"}});
    let observation = case.one(
        "observe",
        "observation.add",
        json!({
            "value":{"kind":"text","statement":"The recorded reading is 12.5 units."},
            "basis":{"kind":"source","artifact_ref":{"id":artifact,"revision":1},"selector":selector,
                "text_quote":{"exact":"Reading 12.5 units"}},
            "validity":"unknown","quality":[],"limitations":[],"evidence_kind":"observational","variables_changed":[]
        }),
    );
    let observation = case.one("assess", "measurement.assess", json!({
        "observation_ref":observation,"channel":null,"validity":"valid",
        "verification":[{"kind":"equivalent","justification":"The setup reproduced a known reference value."}],
        "rationale":"Judged from the recorded setup.","limits":[]
    }));
    let candidate = case.one(
        "claim",
        "conclusion.create",
        json!({
            "statement":"The reading is 12.5 units.","scope":"This recorded reading.",
            "significance":"Check the setup before reuse.","support_refs":[observation],
            "contradiction_refs":[],"caveats":["One reading."]
        }),
    );
    let objection = |key: &str, subject: &Value, severity: &str| {
        case.one(
            key,
            "objection.raise",
            json!({
                "subject_ref":subject,"statement":format!("A {severity} concern."),
                "scope":"The recorded reading.","severity":severity,"evidence_refs":{"state":"unknown"}
            }),
        )
    };
    let material = objection("material", &candidate, "material");

    // Defer the disputed-conclusion suggestion.
    let gap = case.gap("conclusion.disputed", &candidate);
    let gap_id = gap["gap_id"].as_str().unwrap().to_owned();
    case.feedback("defer", &gap, "defer", 0);
    assert!(!ranked(&case.next()).contains(&gap_id));

    // A lifecycle-only revision of the target is benign: the deferral holds.
    let basis = case.show(&candidate, "full")["review_basis"].clone();
    let mut pre = basis["read_set"].as_array().cloned().unwrap();
    pre.push(basis["subject_ref"].clone());
    case.submit(
        "ready",
        "conclusion.request_review",
        json!({"conclusion_ref":candidate}),
        json!(pre),
        0,
    );
    let ready = json!({"id":candidate["id"],"revision":2});
    let held = case.gap("conclusion.disputed", &ready);
    assert_eq!(held["status"], "deferred");
    assert_eq!(held["subject_refs"][0], ready);

    // The promotion gate never reads advice feedback.
    let critical = objection("critical", &ready, "critical");
    let shown = case.show(&ready, "full");
    let basis = shown["review_basis"].clone();
    // A supporting review answers each open objection on its subject by
    // citing it (ADR 0017).
    let answered: Vec<Value> = shown["challenges"]["open_objections"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|objection| objection["subject_ref"]["id"] == ready["id"])
        .map(|objection| objection["objection_ref"].clone())
        .collect();
    let mut read_set = basis["read_set"].as_array().cloned().unwrap();
    read_set.extend(answered.iter().cloned());
    let review = case.one(
        "review",
        "review.submit",
        json!({
            "kind":"validation","subject_ref":basis["subject_ref"],"basis_digest":basis["basis_digest"],
            "read_set":read_set,"method":"Read the recorded basis.","materials_examined":[],
            "shared_dependencies":[],"objection_refs":answered,"verdict":"supports","caveats":["One reading."],
            "context_isolation":"fresh_context_unblinded","context":{
                "saw_original_conclusion":{"state":"known","value":true},
                "gathered_new_evidence":{"state":"known","value":false},
                "method_or_source_changes":[],"activity":"Read the recorded basis.",
                "dependencies":"shared","dependence_basis":"Same recorded data."}
        }),
    );
    let mut pre = read_set.clone();
    pre.extend([ready.clone(), review.clone()]);
    let blocked = case.submit(
        "validate",
        "conclusion.validate",
        json!({"conclusion_ref":ready,"review_ref":review}),
        json!(pre),
        5,
    );
    assert_eq!(blocked["error"]["code"], "GATE_FAILED");
    assert_eq!(blocked["error"]["actual"], json!([critical]));
    assert_eq!(
        case.gap("conclusion.disputed", &ready)["status"],
        "deferred"
    );

    // The deferral hides no caveat in show or brief.
    let shown = case.show(&ready, "summary");
    let open = shown["challenges"]["open_objections"].as_array().unwrap();
    for objection in [&material, &critical] {
        assert!(
            open.iter()
                .any(|entry| entry["objection_ref"]["id"] == objection["id"]),
            "{shown:#}"
        );
    }
    let brief = output(
        dir.path(),
        &["--case", &case.id, "brief", "--json"],
        None,
        0,
    );
    assert!(brief.contains(critical["id"].as_str().unwrap()), "{brief}");
}
