//! Review independence (W10.15) through the public CLI: declarations are
//! read back as recorded, same-family reviews form one dependence group, a
//! judgment recorded after reading another is never shown as recorded
//! independently, self-review is flagged as low-independence, and a blinded
//! packet carries no conclusion or confidence and lists what it omitted.
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const AUTHOR: &str = "agent:independence-author";
const STATEMENT: &str = "The gauge drifted by exactly 12.5 units overnight.";
const SIGNIFICANCE: &str = "Recalibrate before every shift.";

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

fn agent(family: &str) -> Value {
    json!({"kind":"agent","model_family":family,"model_version":null,"harness":null})
}

impl Case<'_> {
    fn submit(
        &self,
        key: &str,
        actor: &str,
        descriptor: Option<Value>,
        op: &str,
        payload: Value,
        pre: Value,
    ) -> Value {
        let mut command = json!({
            "schema_version":"inv.command.v1", "command_id":key, "case_id":self.id,
            "actor":actor, "preconditions":pre, "operations":[{"op":op,"payload":payload}]
        });
        if let Some(descriptor) = descriptor {
            command["actor_descriptor"] = descriptor;
        }
        run(
            self.root,
            &["record", "--input", "-", "--json"],
            Some(&command),
            0,
        )
    }

    fn one(&self, key: &str, op: &str, payload: Value) -> Value {
        reference(&self.submit(key, AUTHOR, None, op, payload, json!([])))
    }

    fn read(&self, args: &[&str]) -> Value {
        let mut all = vec!["--case", self.id.as_str()];
        all.extend(args);
        all.push("--json");
        run(self.root, &all, None, 0)
    }

    fn show(&self, id: &Value, depth: &str) -> Value {
        self.read(&["show", id["id"].as_str().unwrap(), "--depth", depth])
    }
}

fn reference(receipt: &Value) -> Value {
    let change = receipt["created"]
        .get(0)
        .or_else(|| receipt["updated"].get(0))
        .unwrap();
    json!({"id":change["id"],"revision":change["revision"]})
}

fn context(read_other_reviews: Value) -> Value {
    json!({
        "saw_original_conclusion":{"state":"known","value":true},
        "gathered_new_evidence":{"state":"known","value":false},
        "method_or_source_changes":[],"activity":"Read the recorded basis.",
        "dependencies":"shared","dependence_basis":"Same recorded data.",
        "work_kind":{"state":"known","value":"independent_interpretation"},
        "prior_involvement":{"state":"known","value":[]},
        "read_other_reviews":read_other_reviews
    })
}

fn validation(basis: &Value, extra_reads: &[&Value], isolation: &str, context: Value) -> Value {
    let mut read_set = basis["read_set"].as_array().cloned().unwrap();
    read_set.extend(extra_reads.iter().map(|value| (*value).clone()));
    json!({
        "kind":"validation","subject_ref":basis["subject_ref"],"basis_digest":basis["basis_digest"],
        "read_set":read_set,"method":"Read the recorded basis.","materials_examined":[],
        "shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":["One reading."],
        "context_isolation":isolation,"context":context
    })
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
    fs::write(root.join("gauge.txt"), "Gauge 12.5 units\n").unwrap();
    let started = run(
        root,
        &[
            "start",
            "Did the gauge drift?",
            "--attach",
            "gauge.txt",
            "--command-id",
            "start",
            "--actor",
            AUTHOR,
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

/// A conclusion with a confidence, ready for review.
fn ready_conclusion(case: &Case<'_>) -> Value {
    let selector = json!({"schema_version":"inv.selector.v1","artifact_digest":case.attachment["digest"],
        "selector":{"type":"text_lines","start_line":1,"end_line":1,"encoding":"utf-8","parser_version":"inv.text.utf8.v1"}});
    let observation = case.one("observe", "observation.add", json!({
        "value":{"kind":"text","statement":"The gauge log reads 12.5 units."},
        "basis":{"kind":"source","artifact_ref":{"id":case.attachment["derivative_id"],"revision":1},
            "selector":selector,"text_quote":{"exact":"Gauge 12.5 units"}},
        "validity":"unknown","quality":[],"limitations":[],"evidence_kind":"observational","variables_changed":[]
    }));
    let observation = case.one("assess", "measurement.assess", json!({
        "observation_ref":observation,"channel":null,"validity":"valid",
        "verification":[{"kind":"equivalent","justification":"The gauge matched a reference weight."}],
        "rationale":"Judged from the recorded setup.","limits":[]
    }));
    let candidate = case.one("claim", "conclusion.create", json!({
        "statement":STATEMENT,"scope":"The overnight log.","significance":SIGNIFICANCE,
        "support_refs":[observation],"contradiction_refs":[],"caveats":["One log."],
        "confidence":{"level":"low","rationale":"Single uncalibrated log.","limitations":["One instrument."],
            "what_would_change_it":"A second gauge.","downgrade_reasons":null}
    }));
    let basis = case.show(&candidate, "full")["review_basis"].clone();
    let mut pre = basis["read_set"].as_array().cloned().unwrap();
    pre.push(basis["subject_ref"].clone());
    reference(&case.submit(
        "ready",
        AUTHOR,
        None,
        "conclusion.request_review",
        json!({"conclusion_ref":candidate}),
        json!(pre),
    ))
}

#[test]
fn same_family_reviews_form_one_group_and_a_read_judgment_is_not_independent() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(dir.path());
    let ready = ready_conclusion(&case);
    let basis = case.show(&ready, "full")["review_basis"].clone();

    // Three reviewers declare one model family, spelled differently.
    let first = reference(&case.submit(
        "review-1",
        "agent:reviewer-one",
        Some(agent("family-x")),
        "review.submit",
        validation(
            &basis,
            &[],
            "fresh_context_unblinded",
            context(json!({"state":"known","value":false})),
        ),
        json!([]),
    ));
    // The second reviewer read the first review: its read set names it.
    let second = case.submit(
        "review-2",
        "agent:reviewer-two",
        Some(agent(" Family-X ")),
        "review.submit",
        validation(
            &basis,
            &[&first],
            "fresh_context_unblinded",
            context(json!({"state":"unknown"})),
        ),
        json!([]),
    );
    assert!(
        second["warnings"].as_array().unwrap().is_empty(),
        "{second:#}"
    );
    let second = reference(&second);
    // The author reviews its own claim in the same session, undeclared.
    let third = case.submit(
        "review-3",
        AUTHOR,
        Some(agent("family-x")),
        "review.submit",
        validation(
            &basis,
            &[],
            "same_session_self_review",
            json!({
                "saw_original_conclusion":{"state":"known","value":true},
                "gathered_new_evidence":{"state":"known","value":false},
                "method_or_source_changes":[],"activity":"Reread my own claim.",
                "dependencies":"shared","dependence_basis":"I wrote it."
            }),
        ),
        json!([]),
    );
    let warnings = third["warnings"].to_string();
    assert!(warnings.contains("same_session_self_review"), "{warnings}");
    assert!(
        warnings.contains("work_kind, prior_involvement, read_other_reviews"),
        "{warnings}"
    );
    let third = reference(&third);

    // Summary: one dependence group beside three reviews; one low-independence.
    let shown = case.show(&ready, "summary");
    let summary = &shown["review_independence"]["summary"];
    assert_eq!(summary["review_count"], 3, "{shown:#}");
    assert_eq!(summary["dependence_group_count"], 1);
    assert_eq!(summary["groups_without_declared_family"], 0);
    assert_eq!(summary["low_independence_count"], 1);
    assert_eq!(summary["low_independence_reviews"], json!([third]));
    assert_eq!(summary["reviews_with_undeclared_attributes"], 1);
    assert_eq!(summary["pairs_recorded_independently"], 0);
    assert!(shown["review_independence"].get("pairs").is_none());
    let text = output(
        dir.path(),
        &[
            "--case",
            &case.id,
            "show",
            ready["id"].as_str().unwrap(),
            "--format",
            "text",
        ],
        None,
        0,
    );
    assert!(
        text.contains("Reviews: 3 in 1 dependence group(s)"),
        "{text}"
    );
    assert!(text.contains("1 low-independence"), "{text}");

    // Full: the group, the self-review reasons and the pair order.
    let full = &case.show(&ready, "full")["review_independence"];
    assert_eq!(
        full["dependence_groups"],
        json!([{"group":"model_family:family-x","basis":"model_family","reviews":[first, second, third]}])
    );
    let reviews = full["reviews"].as_array().unwrap();
    assert_eq!(
        reviews[2]["low_independence_reasons"],
        json!(["same_session_self_review", "reviewer_recorded_subject"])
    );
    assert_eq!(
        reviews[2]["declarations"]["work_kind"],
        json!({"state":"undeclared"})
    );
    assert_eq!(
        reviews[1]["declarations"]["read_other_reviews"],
        json!({"state":"unknown"})
    );
    assert_eq!(reviews[1]["reviews_read"], json!([first]));
    assert_eq!(reviews[0]["low_independence"], false);
    let pair = |earlier: &Value, later: &Value| {
        full["pairs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pair| pair["earlier"] == *earlier && pair["later"] == *later)
            .cloned()
            .unwrap()
    };
    let read = pair(&first, &second);
    assert_eq!(read["order"], "later_read_earlier");
    assert_eq!(read["recorded_independently"], false);
    assert_eq!(read["same_dependence_group"], true);
    assert_eq!(
        pair(&first, &third)["order"],
        "later_could_have_read_earlier"
    );
    assert!(!full.to_string().contains("\"independent\""));

    // The review itself reads back its own declarations and flag.
    let own = &case.show(&third, "summary")["review_independence"];
    assert_eq!(own["low_independence"], true);
    assert_eq!(own["dependence_group"], "model_family:family-x");

    // Briefs carry the same summary on the outcome.
    let brief = case.read(&["brief"]);
    let outcome = brief["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == ready["id"])
        .unwrap();
    assert_eq!(
        outcome["review_independence"]["summary"]["low_independence_count"],
        1
    );
    assert_eq!(
        outcome["review_independence"]["summary"]["dependence_group_count"],
        1
    );
}

#[test]
fn a_blindspot_review_of_an_earlier_snapshot_is_shown_as_recorded_before_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(dir.path());
    let ready = ready_conclusion(&case);
    let audited = case.read(&["blindspots"])["snapshot"].clone();
    let basis = case.show(&ready, "full")["review_basis"].clone();
    let validation_review = reference(&case.submit(
        "validate",
        "agent:reviewer-one",
        Some(agent("family-x")),
        "review.submit",
        validation(
            &basis,
            &[],
            "fresh_context_unblinded",
            context(json!({"state":"known","value":false})),
        ),
        json!([]),
    ));
    let blindspot = reference(&case.submit(
        "challenge", "agent:reviewer-two", Some(agent("family-y")), "review.submit",
        json!({
            "kind":"blindspot","subject_ref":ready,"audited_snapshot":audited,"read_set":basis["read_set"],
            "method":"Look for unrecorded mechanisms.","scope_examined":"The conclusion and its log.",
            "checks_considered":["instrument drift"],"gaps":["No second gauge."],"no_gap_reason":null,
            "evidence_refs":[],"assumption_refs":[],"limitations":["Recorded material only."],
            "verdict":"gaps_identified","context_isolation":"fresh_context_blinded",
            "context":context(json!({"state":"known","value":false}))
        }),
        json!([]),
    ));
    let full = &case.show(&ready, "full")["review_independence"];
    assert_eq!(full["summary"]["dependence_group_count"], 2);
    let pair = &full["pairs"][0];
    assert_eq!(pair["earlier"], validation_review);
    assert_eq!(pair["later"], blindspot);
    assert_eq!(pair["order"], "later_basis_predates_earlier");
    assert_eq!(pair["recorded_independently"], true);
    assert_eq!(pair["same_dependence_group"], false);
}

#[test]
fn a_blinded_packet_has_no_conclusion_or_confidence_and_lists_its_omissions() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(dir.path());
    let ready = ready_conclusion(&case);
    let basis = case.show(&ready, "full")["review_basis"].clone();
    let prior = reference(&case.submit(
        "review-1",
        "agent:reviewer-one",
        Some(agent("family-x")),
        "review.submit",
        validation(
            &basis,
            &[],
            "fresh_context_unblinded",
            context(json!({"state":"known","value":false})),
        ),
        json!([]),
    ));

    let packet = case.read(&["review-packet", ready["id"].as_str().unwrap()]);
    assert_eq!(packet["result"], "review_packet");
    assert_eq!(packet["blinded"], true);
    let bytes = packet.to_string();
    for withheld in [
        STATEMENT,
        SIGNIFICANCE,
        "Single uncalibrated log.",
        "One log.",
        AUTHOR,
        "\"low\"",
    ] {
        assert!(!bytes.contains(withheld), "{withheld} leaked: {packet:#}");
    }
    let subject = &packet["subject"];
    assert_eq!(subject["fields"], json!({"scope":"The overnight log."}));
    let omitted: Vec<&str> = subject["omitted_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field.as_str().unwrap())
        .collect();
    for field in [
        "statement",
        "significance",
        "confidence",
        "caveats",
        "status",
    ] {
        assert!(omitted.contains(&field), "{field}: {omitted:?}");
    }
    let categories: Vec<&str> = packet["omissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|omission| omission["what"].as_str().unwrap())
        .collect();
    for category in ["originating_conclusion", "confidence", "author_identities"] {
        assert!(categories.contains(&category), "{categories:?}");
    }
    assert_eq!(
        packet["omitted_records"],
        json!([{"ref":prior,"type":"rev","reason":"review_of_subject"}])
    );
    // The question and the evidence stay.
    assert!(
        packet["question"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["fields"]["title"] == "Did the gauge drift?"),
        "{packet:#}"
    );
    let observation = packet["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "obs")
        .unwrap();
    assert_eq!(
        observation["fields"]["value"]["statement"],
        "The gauge log reads 12.5 units."
    );
    assert_eq!(
        packet["review_basis"]["basis_digest"],
        basis["basis_digest"]
    );

    // Deterministic, and pinned to a snapshot like other reads.
    let again = case.read(&["review-packet", ready["id"].as_str().unwrap()]);
    assert_eq!(again["packet_digest"], packet["packet_digest"]);
    let sequence = packet["snapshot"]["sequence"].as_u64().unwrap();
    case.one(
        "later",
        "question.create",
        json!({"importance":"normal","wording":"Was the room heated?"}),
    );
    let pinned = case.read(&[
        "review-packet",
        ready["id"].as_str().unwrap(),
        "--snapshot",
        &sequence.to_string(),
    ]);
    assert_eq!(pinned["historical"], true);
    assert_eq!(pinned["packet_digest"], packet["packet_digest"]);
    let head = case.read(&["review-packet", ready["id"].as_str().unwrap()]);
    assert_ne!(head["packet_digest"], packet["packet_digest"]);

    // A blinded review cites the packet's basis directly.
    let blinded = case.submit(
        "review-blind", "agent:reviewer-two", Some(agent("family-y")), "review.submit",
        json!({
            "kind":"validation","subject_ref":packet["review_basis"]["subject_ref"],
            "basis_digest":packet["review_basis"]["basis_digest"],"read_set":packet["review_basis"]["read_set"],
            "method":format!("Judged from blinded packet {}.", packet["packet_digest"].as_str().unwrap()),
            "materials_examined":[],"shared_dependencies":[],"objection_refs":[],"verdict":"inconclusive",
            "caveats":["Blinded."],"context_isolation":"fresh_context_blinded",
            "context":{
                "saw_original_conclusion":{"state":"known","value":false},
                "gathered_new_evidence":{"state":"known","value":false},
                "method_or_source_changes":[],"activity":"Judge the evidence from the packet.",
                "dependencies":"partly_shared","dependence_basis":"Same recorded log.",
                "work_kind":{"state":"known","value":"independent_interpretation"},
                "prior_involvement":{"state":"known","value":[]},
                "read_other_reviews":{"state":"known","value":false}
            }
        }),
        json!([]),
    );
    assert!(
        blinded["warnings"].as_array().unwrap().is_empty(),
        "{blinded:#}"
    );

    // Only validation subjects can be packed.
    let refused = run(
        dir.path(),
        &["--case", &case.id, "review-packet", &case.id, "--json"],
        None,
        2,
    );
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");
}

const PACK: &str = r#"schema_version = "inv.method-pack.v1"
id = "isolated-review-gate"
version = "1.0.0"
title = "Isolated review gate"
summary = "Fixture pack that requires a review with no low-independence flag."
modes = ["explain", "measure", "evaluate", "explore", "record"]

[[policies]]
name = "not-only-self-review"
input = "isolated_review"
gate = "conclusion.validate"
rationale = "A claim is not validated on self-review alone."
"#;

#[test]
fn a_pinned_pack_can_require_a_review_with_no_low_independence_flag() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let case = start(root);
    fs::write(root.join("gate.toml"), PACK).unwrap();
    let retained = run(
        root,
        &["ingest", "--input", "-", "--json"],
        Some(&json!({
            "schema_version":"inv.ingest.v1","command_id":"retain-pack","actor":AUTHOR,"case_id":case.id,
            "source":{"title":"Isolated review gate pack","format":"application/toml"},"paths":["gate.toml"]
        })),
        0,
    );
    let item = &retained["items"][0];
    let revision =
        case.show(&json!({"id":case.id}), "full")["record"]["header"]["revision"].clone();
    case.submit(
        "adopt", AUTHOR, None, "method_pack.adopt",
        json!({"artifact_ref":{"id":item["artifact_id"],"revision":1},"pack_id":"isolated-review-gate",
            "version":"1.0.0","digest":item["digest"],"reason":"Require a review beyond self-review."}),
        json!([{"id":case.id,"revision":revision}]),
    );
    let ready = ready_conclusion(&case);
    let basis = case.show(&ready, "full")["review_basis"].clone();
    let validate = |key: &str, review: &Value, exit: i32| {
        let mut pre = basis["read_set"].as_array().cloned().unwrap();
        pre.extend([ready.clone(), review.clone()]);
        let command = json!({
            "schema_version":"inv.command.v1","command_id":key,"case_id":case.id,"actor":AUTHOR,
            "preconditions":pre,"operations":[{"op":"conclusion.validate","payload":{"conclusion_ref":ready,"review_ref":review}}]
        });
        run(
            root,
            &["record", "--input", "-", "--json"],
            Some(&command),
            exit,
        )
    };

    // The author's own review is recorded, but does not satisfy the gate.
    let own = reference(&case.submit(
        "self-review",
        AUTHOR,
        None,
        "review.submit",
        validation(
            &basis,
            &[],
            "fresh_context_unblinded",
            context(json!({"state":"known","value":false})),
        ),
        json!([]),
    ));
    let refused = validate("validate-self", &own, 5);
    let blocker = &refused["error"]["expected"]["policy_blockers"][0];
    assert_eq!(blocker["input"], "isolated_review", "{refused:#}");
    assert_eq!(blocker["unmet"]["missing"], "isolated_review");
    assert_eq!(blocker["repair_schema"], "review.submit");

    let other = reference(&case.submit(
        "other-review",
        "agent:reviewer-two",
        Some(agent("family-y")),
        "review.submit",
        validation(
            &basis,
            &[],
            "fresh_context_unblinded",
            context(json!({"state":"known","value":false})),
        ),
        json!([]),
    ));
    validate("validate", &other, 0);
    let validated = case.show(&json!({"id":ready["id"],"revision":3}), "full");
    assert_eq!(
        validated["record"]["body"]["gate_policy"]["rules"][0]["satisfied_by"],
        json!([other]),
        "{validated:#}"
    );
}
