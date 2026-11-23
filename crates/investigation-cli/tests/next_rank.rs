//! `next` ranking through the public binary: grouped cell gaps, snapshot-bound
//! frontier query paths, not-ranked counts, and rank factors with sources.

use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

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
    assert_eq!(output.status.code(), Some(exit), "{args:?}\n{stdout}");
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    serde_json::from_str(&stdout).expect(&stdout)
}

fn follow(cwd: &Path, argv: &Value) -> Value {
    let argv: Vec<&str> = argv
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap())
        .collect();
    assert_eq!(argv[0], "investigation");
    run(cwd, &argv[1..], None, 0)
}

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    case: String,
    keys: usize,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().to_path_buf();
        run(&root, &["init", ".", "--no-vault", "--json"], None, 0);
        let mut fixture = Self {
            _temporary: temporary,
            root,
            case: String::new(),
            keys: 0,
        };
        let case = fixture.submit("case.create", json!({"title":"Ranking","mode":"evaluate"}));
        fixture.case = case["created"][0]["id"].as_str().unwrap().to_owned();
        fixture
    }

    fn submit(&mut self, op: &str, payload: Value) -> Value {
        self.keys += 1;
        let case = if self.case.is_empty() {
            Value::Null
        } else {
            json!(self.case)
        };
        let input = json!({
            "schema_version":"inv.command.v1","command_id":format!("rank-{}", self.keys),
            "case_id":case,"actor":"agent:rank","preconditions":[],
            "operations":[{"op":op,"payload":payload}],
        });
        run(
            &self.root,
            &["record", "--input", "-", "--json"],
            Some(&input),
            0,
        )
    }

    fn created(&mut self, op: &str, payload: Value) -> Value {
        let receipt = self.submit(op, payload);
        json!({"id":receipt["created"][0]["id"],"revision":receipt["created"][0]["revision"]})
    }

    fn next(&self) -> Value {
        run(
            &self.root,
            &["--case", &self.case, "next", "--json"],
            None,
            0,
        )
    }
}

fn gap_ids(page: &Value) -> BTreeSet<String> {
    page["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|gap| gap["gap_id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn next_groups_repeated_cells_and_every_query_path_stays_on_its_snapshot() {
    let mut fixture = Fixture::new();
    let question = fixture.created(
        "question.create",
        json!({"wording":"Which queue should carry the jobs?","importance":"high"}),
    );
    let criterion = fixture.created(
        "criterion.create",
        json!({"name":"Operating cost","kind":"soft",
            "scope":{"kind":"all_options","question_ref":question},
            "scale":{"unit":"USD/month","description":"Operating cost","anchors":[]},
            "preferred_direction":"lower","threshold":null,"weighting_rationale":null}),
    );
    for name in ["Queue A", "Queue B", "Queue C"] {
        let option = fixture.created(
            "option.create",
            json!({"question_ref":question,"description":name,"dependencies":[],"tradeoff_notes":[]}),
        );
        fixture.submit(
            "option.disposition",
            json!({"option_ref":option,"lifecycle":"eligible","reason":"Plausible candidate"}),
        );
    }
    let next = fixture.next();
    assert_eq!(next["rule_version"], "slice-next.v2");
    assert!(next.get("stop_or_defer").is_none(), "{next}");

    // Three missing cells on one criterion are one grouped recommendation.
    let recommendations = next["recommendations"].as_array().unwrap();
    let grouped: Vec<_> = recommendations
        .iter()
        .filter(|item| item["rule_id"] == "decision.option_criterion_cell_missing")
        .collect();
    assert_eq!(grouped.len(), 1, "{next}");
    let item = grouped[0];
    let group = &item["group"];
    assert_eq!(group["criterion_ref"], criterion);
    assert_eq!(group["question_ref"]["id"], question["id"]);
    assert_eq!(group["gap_count"], 3);
    assert_eq!(group["ranked_count"], 3);
    assert_eq!(
        item["decision_value"]["decision_effect"],
        "could_discriminate"
    );
    assert_eq!(
        item["decision_value"]["rule_decision_effect"],
        "context_only"
    );
    assert_eq!(
        item["decision_value"]["pareto_status"],
        "uncertain_frontier"
    );

    // Every recommendation exposes all eight factors with a source.
    for recommendation in recommendations {
        let reasons = recommendation["rank_reasons"].as_array().unwrap();
        assert_eq!(reasons.len(), 8);
        assert!(reasons.iter().all(|reason| {
            !reason["source"].as_str().unwrap().is_empty() && reason["source_refs"].is_array()
        }));
        assert!(recommendation["decision_value"]["basis"].is_string());
    }

    // A later write does not move any of next's query paths off its snapshot.
    fixture.created(
        "hypothesis.create",
        json!({"question_ref":question,"statement":"Cost dominates the choice.",
            "possible_falsifiers":[],"rival_refs":[],"assumption_refs":[]}),
    );
    let drill = follow(&fixture.root, &group["frontier_argv"]);
    assert_eq!(drill["snapshot"], next["snapshot"]);
    assert!(drill["newer_head"].is_object());
    assert_eq!(drill["matching_total"], 3);
    assert!(gap_ids(&drill).contains(item["gap_id"].as_str().unwrap()));
    assert_eq!(drill["filter"]["read_ids"], json!([criterion["id"]]));

    let everything = follow(&fixture.root, &next["frontier_argv"]);
    assert_eq!(everything["snapshot"], next["snapshot"]);
    let inventory = gap_ids(&everything);
    assert_eq!(
        everything["inventory_total"].as_u64().unwrap() as usize,
        inventory.len()
    );
    for recommendation in recommendations {
        assert!(inventory.contains(recommendation["gap_id"].as_str().unwrap()));
    }
    let statuses: Vec<_> = next["not_ranked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["blocked_by_quality", "deferred", "resolved"]);
    for entry in next["not_ranked"].as_array().unwrap() {
        let page = follow(&fixture.root, &entry["frontier_argv"]);
        assert_eq!(page["snapshot"], next["snapshot"]);
        assert_eq!(page["matching_total"], entry["count"]);
    }
    assert_eq!(next["counts"]["deferred"], next["not_ranked"][1]["count"]);

    // The fresh read sees the new hypothesis; the old drill-down did not.
    let fresh = fixture.next();
    assert_ne!(fresh["snapshot"], next["snapshot"]);

    // Text output names the group, its drill-down and the frontier path.
    let text = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(&fixture.root)
        .args(["--case", &fixture.case, "next", "--format", "text"])
        .output()
        .unwrap();
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("Group: 3 gaps"), "{text}");
    assert!(text.contains("Value: could_discriminate"), "{text}");
    assert!(text.contains("Frontier at this snapshot:"), "{text}");
}

#[test]
fn frontier_reads_filter_and_rejects_malformed_ids() {
    let mut fixture = Fixture::new();
    let question = fixture.created(
        "question.create",
        json!({"wording":"What explains the stall?","importance":"normal"}),
    );
    let first = fixture.created(
        "hypothesis.create",
        json!({"question_ref":question,"statement":"Lock contention.",
            "possible_falsifiers":[],"rival_refs":[],"assumption_refs":[]}),
    );
    fixture.created(
        "hypothesis.create",
        json!({"question_ref":question,"statement":"Disk saturation.",
            "possible_falsifiers":[],"rival_refs":[],"assumption_refs":[]}),
    );
    let id = first["id"].as_str().unwrap();
    let page = run(
        &fixture.root,
        &["--case", &fixture.case, "frontier", "--reads", id, "--json"],
        None,
        0,
    );
    assert_eq!(page["matching_total"], 1);
    assert_eq!(page["gaps"][0]["subject_refs"][0], first);
    let rejected = run(
        &fixture.root,
        &[
            "--case",
            &fixture.case,
            "frontier",
            "--reads",
            "not-an-id",
            "--json",
        ],
        None,
        2,
    );
    assert_eq!(rejected["error"]["code"], "INVALID_INPUT");

    // Two live rivals on one question make each assessment discriminating.
    let next = fixture.next();
    for item in next["recommendations"].as_array().unwrap() {
        assert_eq!(item["rule_id"], "hypothesis.no_assessment");
        assert_eq!(
            item["decision_value"]["decision_effect"],
            "could_discriminate"
        );
        assert_eq!(item["rank_reasons"][4]["priority"], "medium");
    }
}

#[test]
fn a_lone_option_cell_is_ranked_by_its_narrower_value_with_a_stop_offer() {
    let mut fixture = Fixture::new();
    let question = fixture.created(
        "question.create",
        json!({"wording":"Is the current queue acceptable?","importance":"normal"}),
    );
    fixture.created(
        "criterion.create",
        json!({"name":"Operating cost","kind":"soft",
            "scope":{"kind":"all_options","question_ref":question},
            "scale":{"unit":"USD/month","description":"Operating cost","anchors":[]},
            "preferred_direction":"lower","threshold":null,"weighting_rationale":null}),
    );
    let option = fixture.created(
        "option.create",
        json!({"question_ref":question,"description":"Keep the current queue","dependencies":[],"tradeoff_notes":[]}),
    );
    fixture.submit(
        "option.disposition",
        json!({"option_ref":option,"lifecycle":"eligible","reason":"Retain current design"}),
    );
    let next = fixture.next();
    let item = &next["recommendations"][0];
    assert_eq!(item["rule_id"], "decision.option_criterion_cell_missing");
    let value = &item["decision_value"];
    assert_eq!(value["decision_effect"], "context_only");
    assert_eq!(value["every_outcome_unchanged"], true);
    assert!(value["narrower_value"].is_string());
    assert!(
        item["rank_reasons"][3]["source"]
            .as_str()
            .unwrap()
            .contains("narrower value")
    );
    let stop = &next["stop_or_defer"];
    let alternatives = stop["alternatives"].as_array().unwrap();
    assert!(!alternatives.is_empty());
    for alternative in alternatives {
        let schema = follow(&fixture.root, &alternative["schema_argv"]);
        assert_eq!(schema["name"], alternative["operation"]);
    }
}
