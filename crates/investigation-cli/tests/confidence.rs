//! Confidence with named downgrade reasons (W10.07) through the public CLI:
//! promotion requires named reasons for moderate or high confidence, concerns
//! are visible in the summary view beside computed ones, and no output turns
//! confidence into a number.
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:confidence-test";

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

struct Case<'a> {
    root: &'a Path,
    id: String,
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

    /// Request review with the current read set as preconditions.
    fn request_review(&self, key: &str, candidate: &Value, exit: i32) -> Value {
        let basis = self.show(candidate, "full")["review_basis"].clone();
        let mut pre = basis["read_set"].as_array().cloned().unwrap();
        pre.push(basis["subject_ref"].clone());
        self.submit(
            key,
            ACTOR,
            "conclusion.request_review",
            json!({"conclusion_ref":candidate}),
            json!(pre),
            exit,
        )
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
            "Can the reading be interpreted?",
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

fn confidence(level: &str, reasons: Value) -> Value {
    json!({"level":level,"rationale":"One calibrated reading.","limitations":["One instrument."],
        "what_would_change_it":"A second instrument disagrees.","downgrade_reasons":reasons})
}

fn reasons(consistency: &str) -> Value {
    json!({"validity":"ok","consistency":consistency,"directness":"ok",
        "precision":"not_assessed","completeness":"not_assessed"})
}

/// Every number under a key that mentions confidence, other than an exact
/// record revision, with its path.
fn confidence_numbers(value: &Value, path: &str, inside: bool, found: &mut Vec<String>) {
    match value {
        Value::Number(_) if inside && !path.ends_with("/revision") => found.push(path.to_owned()),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                confidence_numbers(item, &format!("{path}/{index}"), inside, found);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                let lower = key.to_lowercase();
                assert!(
                    !(lower.contains("confidence")
                        && (lower.contains("score")
                            || lower.contains("percent")
                            || lower.contains("probability")
                            || lower.contains("average"))),
                    "{path}/{key}"
                );
                confidence_numbers(
                    item,
                    &format!("{path}/{key}"),
                    inside || lower.contains("confidence"),
                    found,
                );
            }
        }
        _ => {}
    }
}

#[test]
fn promotion_requires_named_reasons_and_summary_shows_concerns_without_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let case = start(root);
    let observation = case.observe("observe");
    let observation = case.one("assess", "measurement.assess", json!({
        "observation_ref":observation,"channel":null,"validity":"valid",
        "verification":[{"kind":"equivalent","justification":"The setup reproduced a known reference value."}],
        "rationale":"Judged from the recorded setup.","limits":[]
    }));
    let claim = |confidence: Value| {
        json!({
            "statement":"The reading is 12.5 units.","scope":"This recorded reading.",
            "significance":"Check the setup before reuse.","support_refs":[observation],
            "contradiction_refs":[],"caveats":["One reading."],"confidence":confidence
        })
    };

    // A numeric confidence is rejected at recording with a pointer to likelihood.
    let refused = case.submit(
        "numeric",
        ACTOR,
        "conclusion.create",
        claim(json!(0.9)),
        json!([]),
        2,
    );
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("never a number or probability"),
        "{refused:#}"
    );

    // High confidence without reasons records fine but cannot be promoted.
    let candidate = case.one(
        "claim",
        "conclusion.create",
        claim(confidence("high", Value::Null)),
    );
    let shown = case.show(&candidate, "summary");
    assert_eq!(shown["confidence"]["level"], "high");
    assert_eq!(
        shown["confidence"]["promotion_requirement"],
        "missing_downgrade_reasons"
    );
    let refused = case.request_review("ready-unnamed", &candidate, 5);
    assert_eq!(refused["error"]["code"], "GATE_FAILED");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("names the downgrade reasons considered"),
        "{refused:#}"
    );
    assert_eq!(
        refused["error"]["expected"]["repair_schema"],
        "conclusion.amend"
    );
    assert_eq!(
        refused["error"]["expected"]["confidence"]["downgrade_reasons"]["validity"],
        "ok | concern | not_assessed"
    );
    assert_eq!(refused["error"]["actual"]["confidence"]["level"], "high");

    // Naming the reasons repairs it; a recorded concern is shown in the
    // summary view beside the computed facts, and the label stays as recorded.
    let amended = case.one(
        "amend",
        "conclusion.amend",
        json!({"conclusion_ref":candidate,"replacement":claim(confidence("high", reasons("concern"))),
            "reason":"Name the downgrade reasons."}),
    );
    case.request_review("ready", &amended, 0);
    let ready = json!({"id":candidate["id"],"revision":3});
    let shown = case.show(&ready, "summary");
    let view = &shown["confidence"];
    assert_eq!(view["level"], "high");
    assert_eq!(view["promotion_requirement"], "met");
    assert_eq!(
        view["concerns"],
        json!([{"reason":"consistency","recorded":"concern","computed":["single_origin"]}])
    );
    assert_eq!(
        view["advisories"],
        json!([{"code":"label_above_named_concerns","ceiling":"moderate"}])
    );
    assert!(view.get("rationale").is_none());
    let full = case.show(&ready, "full");
    let detail = &full["confidence"];
    assert_eq!(detail["rationale"], "One calibrated reading.");
    assert_eq!(detail["reasons"][1]["reason"], "consistency");
    assert_eq!(detail["reasons"][0]["recorded"], "ok");
    assert_eq!(detail["computed_concerns"][0]["source"], "claim_origin");
    assert_eq!(detail["computed_concerns"][0]["refs"], json!([observation]));
    assert!(
        detail["advisories"][0]["message"]
            .as_str()
            .unwrap()
            .contains("recorded label is shown unchanged")
    );
    assert_eq!(full["record"]["body"]["confidence"]["level"], "high");
    let text = case.show_text(&ready);
    assert!(
        text.contains("Confidence: high; consistency concern (recorded; computed: single_origin)"),
        "{text}"
    );
    assert!(
        text.contains("Confidence advisory label_above_named_concerns (ceiling moderate)"),
        "{text}"
    );

    // Validation passes with named reasons.
    let review = case.review("review", &ready);
    let basis = case.show(&ready, "full")["review_basis"].clone();
    let mut pre = basis["read_set"].as_array().cloned().unwrap();
    pre.extend([ready.clone(), review.clone()]);
    case.submit(
        "validate",
        ACTOR,
        "conclusion.validate",
        json!({"conclusion_ref":ready,"review_ref":review}),
        json!(pre),
        0,
    );

    // A low or unassessed confidence never needs reasons.
    let low = case.one(
        "claim-low",
        "conclusion.create",
        claim(confidence("low", Value::Null)),
    );
    case.request_review("ready-low", &low, 0);
    let bare = case.one("claim-bare", "conclusion.create", claim(Value::Null));
    case.request_review("ready-bare", &bare, 0);
    let shown = case.show(&json!({"id":bare["id"]}), "summary");
    assert_eq!(shown["confidence"]["level"], "not_assessed");
    assert_eq!(shown["confidence"]["recorded"], false);
    assert!(case.show_text(&bare).contains("Confidence: not_assessed"));

    // No output combines confidence into a number: not the record views and
    // not the case brief, which lists every outcome separately.
    for subject in [&ready, &low, &bare] {
        for depth in ["summary", "full"] {
            let mut found = Vec::new();
            confidence_numbers(&case.show(subject, depth), "", false, &mut found);
            assert!(found.is_empty(), "{found:?}");
        }
        let text = case.show_text(subject);
        for line in text.lines().filter(|line| line.contains("Confidence")) {
            assert!(
                !line.chars().any(|c| c.is_ascii_digit() || c == '%'),
                "{line}"
            );
        }
    }
    let brief = run(root, &["--case", &case.id, "brief", "--json"], None, 0);
    let mut found = Vec::new();
    confidence_numbers(&brief, "", false, &mut found);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn decision_acceptance_requires_named_reasons_on_cited_assessments() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let case = start(root);
    let observation = case.observe("observe");
    let question = case.one(
        "question",
        "question.create",
        json!({"importance":"normal","wording":"Which setup should be reused?"}),
    );
    let hypothesis = case.one("hypothesis", "hypothesis.create", json!({
        "question_ref":question,"statement":"The bench setup is calibrated.",
        "possible_falsifiers":["A reference reading disagrees."],"rival_refs":[],"assumption_refs":[]
    }));
    let assessment = |confidence: Value| {
        json!({"subject_ref":hypothesis,"evidence_refs":[observation],"relation":"inconclusive",
            "rationale":"One reading of the setup.","confidence":confidence})
    };
    let unnamed = case.one(
        "assessment",
        "assessment.add",
        assessment(confidence("moderate", Value::Null)),
    );
    let option = case.one("option", "option.create", json!({
        "question_ref":question,"description":"Reuse the bench setup","dependencies":[],"tradeoff_notes":[]
    }));
    let option = case.one(
        "eligible",
        "option.disposition",
        json!({"option_ref":option,"lifecycle":"eligible","reason":"Feasible"}),
    );
    let propose = |key: &str, evidence: &Value| {
        case.one(key, "decision.propose", json!({
            "question_ref":question,"choice":{"kind":"select","option_ref":option},
            "compared_alternatives":[option],"comparison_refs":[],"hard_conditions":[],
            "evidence_refs":[evidence],"assumption_refs":[],"tradeoffs":[],"consequences":["Reuse the setup."],
            "reopen_trigger":"A reading disagrees.","reversal_conditions":[]
        }))
    };
    let accept = |key: &str, decision: &Value, exit| {
        let review = case.review(&format!("{key}-review"), decision);
        case.submit(
            key,
            "agent:decider",
            "decision.accept",
            json!({"decision_ref":decision,"review_ref":review}),
            json!([]),
            exit,
        )
    };

    let proposed = propose("propose", &unnamed);
    let refused = accept("accept-unnamed", &proposed, 5);
    assert_eq!(refused["error"]["code"], "GATE_FAILED");
    assert_eq!(refused["error"]["actual"], json!([unnamed]));
    assert_eq!(
        refused["error"]["expected"]["repair_schema"],
        "assessment.supersede"
    );

    let named = case.submit(
        "supersede",
        ACTOR,
        "assessment.supersede",
        json!({"assessment_ref":unnamed,"replacement":assessment(confidence("moderate", reasons("ok"))),
            "rationale":"Name the downgrade reasons."}),
        json!([]),
        0,
    );
    let named = named["created"][0].clone();
    let named = json!({"id":named["id"],"revision":named["revision"]});
    let shown = case.show(&named, "summary");
    assert_eq!(shown["confidence"]["level"], "moderate");
    assert_eq!(shown["confidence"]["downgrade_reasons_named"], true);
    // A recorded `ok` that recorded facts contradict is flagged, not rewritten;
    // one concern leaves moderate within its ceiling.
    assert_eq!(
        shown["confidence"]["advisories"],
        json!([{"code":"recorded_ok_with_computed_concern","reason":"consistency"}])
    );
    assert_eq!(
        shown["confidence"]["concerns"],
        json!([{"reason":"consistency","recorded":"ok","computed":["single_origin"]}])
    );
    let proposed = propose("propose-named", &named);
    assert_eq!(accept("accept", &proposed, 0)["result"], "committed");

    // A later invalid measurement judgment shows as a computed validity
    // concern; the recorded judgment and label are left as recorded.
    case.one(
        "invalid",
        "measurement.assess",
        json!({
            "observation_ref":observation,"channel":null,"validity":"invalid",
            "verification":[],"rationale":"The reference reading disagreed.","limits":[]
        }),
    );
    let view = case.show(&named, "summary")["confidence"].clone();
    assert_eq!(view["level"], "moderate");
    assert_eq!(
        view["concerns"],
        json!([
            {"reason":"validity","recorded":"ok","computed":["measurement_invalid"]},
            {"reason":"consistency","recorded":"ok","computed":["single_origin"]}
        ])
    );
    let codes: Vec<_> = view["advisories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|advisory| advisory["code"].as_str().unwrap())
        .collect();
    assert_eq!(
        codes,
        [
            "recorded_ok_with_computed_concern",
            "recorded_ok_with_computed_concern",
            "label_above_named_concerns"
        ]
    );
    assert_eq!(view["advisories"][2]["ceiling"], "low");
}
