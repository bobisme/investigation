//! Objection policy (§14.3) through the public CLI: critical objections block
//! validation, decisions record and show what they were accepted over, and a
//! resolution or withdrawal is a visible, attributed revision.
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:cli-test";

fn invocation(cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT");
    command
}

fn output(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> String {
    let mut command = invocation(cwd);
    command
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

    /// A validation review over the exact current basis. `answered` are the
    /// open objections on the subject that the review cites (ADR 0017).
    fn review_payload(&self, subject: &Value, verdict: &str, answered: &[Value]) -> Value {
        let basis = self.show(subject, "full")["review_basis"].clone();
        let mut read_set = basis["read_set"].as_array().cloned().unwrap();
        read_set.extend(answered.iter().cloned());
        json!({
            "kind":"validation","subject_ref":basis["subject_ref"],"basis_digest":basis["basis_digest"],
            "read_set":read_set,"method":"Read the recorded basis.","materials_examined":[],
            "shared_dependencies":[],"objection_refs":answered,"verdict":verdict,"caveats":["One reading."],
            "context_isolation":"fresh_context_unblinded","context":{
                "saw_original_conclusion":{"state":"known","value":true},
                "gathered_new_evidence":{"state":"known","value":false},
                "method_or_source_changes":[],"activity":"Read the recorded basis.",
                "dependencies":"shared","dependence_basis":"Same recorded data."}
        })
    }

    /// The open objections naming `subject` itself, as `show` lists them.
    fn open_on(&self, subject: &Value) -> Vec<Value> {
        self.show(subject, "full")["challenges"]["open_objections"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|objection| objection["subject_ref"]["id"] == subject["id"])
            .map(|objection| objection["objection_ref"].clone())
            .collect()
    }

    /// Submit a supportive validation review over the exact current basis
    /// that answers every open objection on the subject by citing it.
    fn review(&self, key: &str, subject: &Value) -> Value {
        let answered = self.open_on(subject);
        self.one(
            key,
            "review.submit",
            self.review_payload(subject, "supports", &answered),
        )
    }

    fn objection(&self, key: &str, subject: &Value, severity: &str) -> Value {
        self.one(
            key,
            "objection.raise",
            json!({
                "subject_ref":subject,"statement":format!("A {severity} concern."),
                "scope":"The recorded reading.","severity":severity,"evidence_refs":{"state":"unknown"}
            }),
        )
    }
}

fn start(root: &Path) -> (String, Value) {
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
    let start = run(
        root,
        &[
            "start",
            "Can the reading be interpreted?",
            "--attach",
            "reading.txt",
            "--command-id",
            "objection-case",
            "--actor",
            ACTOR,
            "--json",
        ],
        None,
        0,
    );
    (
        start["created"][0]["id"].as_str().unwrap().to_owned(),
        start,
    )
}

fn ids(items: &Value, field: &str) -> Vec<String> {
    items
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| item[field]["id"].as_str().unwrap().to_owned())
        .collect()
}

fn warnings(receipt: &Value) -> String {
    receipt["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|warning| warning.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn critical_objections_block_validation_and_material_ones_stay_visible() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (id, started) = start(root);
    let case = Case { root, id };
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
    let material = case.objection("material", &candidate, "material");

    // The material objection is visible before promotion, in the read and in
    // the promotion receipt; it does not block review readiness.
    let shown = case.show(&candidate, "summary");
    assert_eq!(
        ids(&shown["challenges"]["open_objections"], "objection_ref"),
        [material["id"].as_str().unwrap()]
    );
    assert_eq!(shown["material_caveat"]["open_objections"][0], material);
    let basis = case.show(&candidate, "full")["review_basis"].clone();
    let mut pre = basis["read_set"].as_array().cloned().unwrap();
    pre.push(basis["subject_ref"].clone());
    let ready = case.submit(
        "ready",
        ACTOR,
        "conclusion.request_review",
        json!({"conclusion_ref":candidate}),
        json!(pre),
        0,
    );
    assert!(
        warnings(&ready).contains(&format!(
            "open material objection {}",
            material["id"].as_str().unwrap()
        )),
        "{ready:#}"
    );
    let ready = json!({"id":candidate["id"],"revision":2});

    // A critical objection blocks validation and the blocker names it.
    let critical = case.objection("critical", &ready, "critical");

    // A supporting review that does not cite the open objections on its
    // subject is refused and names them (ADR 0017); one that answers only
    // some of them is refused for the rest. A disputing review is recorded.
    for (key, answered) in [("unanswered", vec![]), ("half", vec![material.clone()])] {
        let refused = case.submit(
            key,
            ACTOR,
            "review.submit",
            case.review_payload(&ready, "supports", &answered),
            json!([]),
            5,
        );
        assert_eq!(refused["error"]["code"], "GATE_FAILED", "{refused:#}");
        assert_eq!(
            refused["error"]["path"],
            "/operations/0/payload/objection_refs"
        );
        let open: Vec<_> = refused["error"]["actual"]["open_objections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|objection| objection["objection_ref"].clone())
            .collect();
        let mut wanted = vec![material.clone(), critical.clone()];
        wanted.retain(|objection| !answered.contains(objection));
        wanted.sort_by_key(|objection| objection["id"].as_str().unwrap().to_owned());
        assert_eq!(open, wanted);
        let argv = refused["error"]["repair"]["argv"].as_array().unwrap();
        assert!(argv.contains(&wanted[0]["id"]), "{argv:?}");
    }
    case.submit(
        "objects",
        ACTOR,
        "review.submit",
        case.review_payload(&ready, "objects", &[]),
        json!([]),
        0,
    );
    let review = case.review("review-1", &ready);
    let validate = |key: &str, review: &Value, exit| {
        let basis = case.show(&ready, "full")["review_basis"].clone();
        let mut pre = basis["read_set"].as_array().cloned().unwrap();
        // The review's read set holds the objections it answered.
        pre.extend(case.open_on(&ready));
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
    let blocked = validate("validate-blocked", &review, 5);
    assert_eq!(blocked["error"]["code"], "GATE_FAILED");
    assert_eq!(blocked["error"]["actual"], json!([critical]));

    // Resolving cites evidence and a fresh review; the objection is kept.
    let resolved = case.submit(
        "resolve",
        "agent:resolver",
        "objection.resolve",
        json!({"objection_ref":critical,"reason":"The setup was independently checked.",
            "evidence_refs":[observation],"review_ref":review}),
        json!([]),
        0,
    );
    assert_eq!(resolved["updated"][0]["revision"], 2);
    let kept = case.show(&critical, "summary");
    assert_eq!(kept["objection"]["status"], "resolved");
    assert_eq!(kept["objection"]["severity"], "critical");
    assert_eq!(
        kept["objection"]["disposition"]["recorded_by"],
        "agent:resolver"
    );
    assert_eq!(kept["objection"]["disposition"]["review_ref"], review);
    assert_eq!(
        kept["objection"]["disposition"]["reason"],
        "The setup was independently checked."
    );
    let original = run(
        root,
        &[
            "show",
            critical["id"].as_str().unwrap(),
            "--at",
            "1",
            "--depth",
            "full",
            "--json",
        ],
        None,
        0,
    );
    assert_eq!(original["record"]["body"]["status"], "open");
    assert_eq!(
        original["record"]["body"]["statement"],
        "A critical concern."
    );

    // The old review captured the open critical objection; a fresh one is due.
    validate("validate-stale", &review, 5);
    let review = case.review("review-2", &ready);
    let validated = validate("validate", &review, 0);
    assert!(
        warnings(&validated).contains(material["id"].as_str().unwrap()),
        "{validated:#}"
    );
    let shown = case.show(&ready, "summary");
    assert_eq!(shown["effective_status"]["state"], "validated");
    assert_eq!(
        ids(&shown["challenges"]["open_objections"], "objection_ref"),
        [material["id"].as_str().unwrap()]
    );
    let text = output(root, &["show", ready["id"].as_str().unwrap()], None, 0);
    assert!(
        text.contains(&format!(
            "Open objections: {} (material)",
            material["id"].as_str().unwrap()
        )),
        "{text}"
    );

    // An objection raised after validation disputes the conclusion.
    let validated = json!({"id":ready["id"],"revision":3});
    let late = case.objection("late", &validated, "material");
    let shown = case.show(&validated, "summary");
    assert_eq!(shown["effective_status"]["state"], "disputed");
    assert_eq!(shown["effective_status"]["recorded"], "validated");
    assert!(
        shown["contradiction"]["open_objections"]
            .as_array()
            .unwrap()
            .contains(&late),
        "{shown:#}"
    );

    // Withdrawal is attributed and visible; nothing flips a flag in place.
    case.submit(
        "withdraw",
        "agent:raiser",
        "objection.withdraw",
        json!({"objection_ref":late,"reason":"Raised against the wrong reading.","evidence_refs":[]}),
        json!([]),
        0,
    );
    let withdrawn = case.show(&late, "summary");
    assert_eq!(withdrawn["objection"]["status"], "withdrawn");
    assert_eq!(
        withdrawn["objection"]["disposition"]["recorded_by"],
        "agent:raiser"
    );
    assert_eq!(withdrawn["basis_revision"], 2);

    // The disputed gap follows open objections only: once the last one is
    // withdrawn it clears, while every objection stays on record.
    let disputed = || {
        run(
            root,
            &[
                "--case",
                &case.id,
                "frontier",
                "--rule",
                "conclusion.disputed",
                "--json",
            ],
            None,
            0,
        )["gaps"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(disputed(), 1);
    case.submit(
        "withdraw-material",
        ACTOR,
        "objection.withdraw",
        json!({"objection_ref":material,"reason":"Addressed by the caveat.","evidence_refs":[]}),
        json!([]),
        0,
    );
    assert_eq!(disputed(), 0);
    for objection in [&material, &critical, &late] {
        assert_ne!(
            case.show(objection, "summary")["objection"]["status"],
            "open"
        );
    }
}

#[test]
fn decisions_record_and_show_the_objections_they_were_accepted_over() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (id, _) = start(root);
    let case = Case { root, id };
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
    let proposed = case.one("propose", "decision.propose", json!({
        "question_ref":question,"choice":{"kind":"select","option_ref":option},
        "compared_alternatives":[option],"comparison_refs":[],"hard_conditions":[],
        "evidence_refs":[],"assumption_refs":[],"tradeoffs":[],"consequences":["Reuse the setup."],
        "reopen_trigger":"A reading disagrees.","reversal_conditions":[]
    }));
    let material = case.objection("material", &proposed, "material");
    let critical = case.objection("critical", &proposed, "critical");
    let other = case.one(
        "other-question",
        "question.create",
        json!({"importance":"normal","wording":"Where should results be kept?"}),
    );
    let unrelated = case.objection("unrelated", &other, "material");
    let review = case.review("review", &proposed);
    let accept = |key: &str, acknowledgments: Value, exit| {
        case.submit(
            key,
            "agent:decider",
            "decision.accept",
            json!({"decision_ref":proposed,"review_ref":review,"acknowledgments":acknowledgments}),
            json!([]),
            exit,
        )
    };

    // The default policy demands an explicit acknowledgment of a critical objection.
    let refused = accept("accept-silent", json!([]), 5);
    assert_eq!(refused["error"]["code"], "GATE_FAILED");
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/payload/acknowledgments"
    );
    assert_eq!(refused["error"]["expected"], json!([critical]));
    // Acknowledgments name only open objections on the decision or its basis.
    let refused = accept(
        "accept-unrelated",
        json!([{"objection_ref":critical,"rationale":"Accepted risk."},
               {"objection_ref":unrelated,"rationale":"Not about this choice."}]),
        5,
    );
    assert_eq!(refused["error"]["code"], "GATE_FAILED");

    let accepted = accept(
        "accept",
        json!([{"objection_ref":critical,"rationale":"Reversible within a day; accepted risk."}]),
        0,
    );
    let notes = warnings(&accepted);
    for objection in [&material, &critical] {
        assert!(notes.contains(objection["id"].as_str().unwrap()), "{notes}");
    }
    let decision = json!({"id":proposed["id"],"revision":2});
    let stored = case.show(&decision, "full")["record"]["body"]["acceptance_challenges"].clone();
    assert_eq!(stored["policy"], "critical");
    assert_eq!(stored["open_objections"].as_array().unwrap().len(), 2);

    // Every default read shows what the decision was accepted over.
    let shown = case.show(&decision, "summary");
    let over = &shown["challenges"]["accepted_over"]["open_objections"];
    let mut expected = vec![
        material["id"].as_str().unwrap().to_owned(),
        critical["id"].as_str().unwrap().to_owned(),
    ];
    expected.sort();
    assert_eq!(ids(over, "objection_ref"), expected);
    let acknowledged: Vec<_> = over
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["severity"].clone(), item["acknowledgment"].clone()))
        .collect();
    assert!(acknowledged.contains(&(
        json!("critical"),
        json!("Reversible within a day; accepted risk.")
    )));
    assert!(acknowledged.contains(&(json!("material"), Value::Null)));
    assert_eq!(
        ids(&shown["challenges"]["open_objections"], "objection_ref"),
        expected
    );
    assert_eq!(
        shown["material_caveat"]["reason"],
        "Open objections apply to the record or its basis."
    );
    let text = output(root, &["show", decision["id"].as_str().unwrap()], None, 0);
    assert!(text.contains("Accepted over objections:"), "{text}");
    assert!(
        text.contains(&format!("{} (material)", material["id"].as_str().unwrap())),
        "{text}"
    );
    let brief = run(root, &["brief", "--json"], None, 0);
    let outcome = brief["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == decision["id"])
        .unwrap();
    assert_eq!(
        ids(
            &outcome["challenges"]["accepted_over"]["open_objections"],
            "objection_ref"
        ),
        expected
    );

    // Resolving keeps the objection and the acceptance record; only the live
    // status changes.
    case.submit(
        "resolve",
        ACTOR,
        "objection.resolve",
        json!({"objection_ref":material,"reason":"A second reading agreed.","evidence_refs":[]}),
        json!([]),
        5,
    );
    case.submit(
        "withdraw",
        ACTOR,
        "objection.withdraw",
        json!({"objection_ref":material,"reason":"Superseded by the critical objection.","evidence_refs":[]}),
        json!([]),
        0,
    );
    let shown = case.show(&decision, "summary");
    let over = shown["challenges"]["accepted_over"]["open_objections"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(over.len(), 2);
    let material_entry = over
        .iter()
        .find(|item| item["objection_ref"]["id"] == material["id"])
        .unwrap();
    assert_eq!(material_entry["current_status"], "withdrawn");
    assert_eq!(
        ids(&shown["challenges"]["open_objections"], "objection_ref"),
        [critical["id"].as_str().unwrap()]
    );
    assert!(case.show(&material, "summary")["objection"]["status"] == "withdrawn");
}
