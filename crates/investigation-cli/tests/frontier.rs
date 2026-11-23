//! The public `frontier` command: complete inventory, filters, snapshot-bound
//! paging across concurrent writes, and per-gap rule explanations.

use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    case: String,
    question: Value,
    keys: usize,
}

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

/// Run an argv exactly as the CLI returned it, after the program name.
fn follow(cwd: &Path, argv: &Value, exit: i32) -> Value {
    let argv: Vec<&str> = argv
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap())
        .collect();
    assert_eq!(argv[0], "investigation");
    run(cwd, &argv[1..], None, exit)
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
            question: Value::Null,
            keys: 0,
        };
        let case = fixture.submit("case.create", json!({"title":"Frontier","mode":"record"}));
        fixture.case = case["created"][0]["id"].as_str().unwrap().to_owned();
        let question = fixture.submit(
            "question.create",
            json!({"wording":"What explains the recorded difference?","importance":"normal"}),
        );
        fixture.question = created(&question);
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
            "schema_version":"inv.command.v1","command_id":format!("frontier-{}", self.keys),
            "case_id":case,"actor":"agent:frontier","preconditions":[],
            "operations":[{"op":op,"payload":payload}],
        });
        run(
            &self.root,
            &["record", "--input", "-", "--json"],
            Some(&input),
            0,
        )
    }

    fn hypothesis(&mut self, index: usize) -> Value {
        let receipt = self.submit(
            "hypothesis.create",
            json!({
                "question_ref":self.question,"statement":format!("Candidate account {index}."),
                "possible_falsifiers":[],"rival_refs":[],"assumption_refs":[],
            }),
        );
        created(&receipt)
    }

    fn confirmatory_inquiry(&mut self) -> Value {
        let receipt = self.submit(
            "inquiry.create",
            json!({
                "purpose":"Discriminate the candidate accounts.","method":"Compare matched runs.",
                "inquiry":{"mode":"confirmatory","confirmatory":{
                    "predictions":[],"evidence_holdout_boundary":"Runs after the plan is frozen.",
                    "prior_exposure":"None recorded.","stop_analysis_rule":"Stop after ten matched runs.",
                }},"desired_observations":["Matched timing"],
                "controls":[],"limits":["Use a comparable workload."],
            }),
        );
        created(&receipt)
    }

    fn frontier(&self, extra: &[&str], exit: i32) -> Value {
        let mut args = vec!["--case", self.case.as_str(), "frontier"];
        args.extend(extra);
        args.push("--json");
        run(&self.root, &args, None, exit)
    }

    fn walk(&self, extra: &[&str]) -> Vec<Value> {
        let mut pages = vec![self.frontier(extra, 0)];
        while let Some(argv) = pages.last().unwrap().get("next_page_argv").cloned() {
            pages.push(follow(&self.root, &argv, 0));
        }
        pages
    }
}

fn created(receipt: &Value) -> Value {
    json!({"id":receipt["created"][0]["id"],"revision":receipt["created"][0]["revision"]})
}

fn gap_ids(pages: &[Value]) -> Vec<String> {
    pages
        .iter()
        .flat_map(|page| page["gaps"].as_array().unwrap())
        .map(|gap| gap["gap_id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn frontier_is_the_complete_unranked_inventory_behind_next() {
    let mut fixture = Fixture::new();
    for index in 0..5 {
        fixture.hypothesis(index);
    }
    let inquiry = fixture.confirmatory_inquiry();

    let pages = fixture.walk(&["--limit", "2"]);
    assert_eq!(pages.len(), 3);
    let walked = gap_ids(&pages);
    assert_eq!(walked.len(), 6);
    let mut sorted = walked.clone();
    sorted.sort();
    assert_eq!(walked, sorted, "stable gap_id order, unranked");
    assert_eq!(
        walked.iter().collect::<BTreeSet<_>>().len(),
        walked.len(),
        "no gap is listed twice"
    );
    let first = &pages[0];
    assert_eq!(first["schema_version"], "inv.gap_page.v1");
    assert_eq!(first["rule_version"], "slice-gaps.v1");
    assert_eq!(first["inventory_total"], 6);
    assert!(
        pages
            .iter()
            .all(|page| page["snapshot"] == first["snapshot"])
    );
    assert!(first["limits"].as_array().unwrap().iter().any(|limit| {
        limit
            .as_str()
            .unwrap()
            .contains("never creates or dispatches")
    }));

    // `next` ranks a subset of the same snapshot's inventory and counts all of it.
    let next = run(
        &fixture.root,
        &["--case", &fixture.case, "next", "--json"],
        None,
        0,
    );
    assert_eq!(next["snapshot"], first["snapshot"]);
    let counted: u64 = next["counts"]
        .as_object()
        .unwrap()
        .values()
        .map(|count| count.as_u64().unwrap())
        .sum();
    assert_eq!(counted, 6);
    for recommendation in next["recommendations"].as_array().unwrap() {
        assert!(walked.contains(&recommendation["gap_id"].as_str().unwrap().to_owned()));
    }

    for gap in pages
        .iter()
        .flat_map(|page| page["gaps"].as_array().unwrap())
    {
        assert_eq!(gap["rule_version"], "slice-gaps.v1");
        let subjects = gap["subject_refs"].as_array().unwrap();
        let distinct: BTreeSet<_> = subjects
            .iter()
            .map(|subject| subject["id"].as_str().unwrap())
            .collect();
        assert_eq!(distinct.len(), subjects.len(), "subjects are deduplicated");
        assert!(!gap["prerequisites"].as_array().unwrap().is_empty());
        let routes = gap["inspect_argvs"].as_array().unwrap();
        assert_eq!(routes.len(), subjects.len());
        let shown = follow(&fixture.root, &routes[0], 0);
        assert_eq!(shown["result"], "record_shown");
        let schema = follow(&fixture.root, &gap["schema_argv"], 0);
        assert_eq!(schema["name"], gap["suggested_operation"]);
        let explained = follow(&fixture.root, &gap["explain_argv"], 0);
        assert_eq!(explained["rule"]["rule_id"], gap["rule_id"]);
    }
    let subjects = first["subjects"].as_array().unwrap();
    assert_eq!(subjects.len(), first["gaps"].as_array().unwrap().len());

    // Filters: status, record kind by prefix or by name, and rule ID.
    let ready = fixture.walk(&["--status", "ready_to_analyze"]);
    assert_eq!(
        gap_ids(&ready),
        [format!(
            "gap:inquiry.confirmatory_plan_unfrozen:{}",
            inquiry["id"].as_str().unwrap()
        )]
    );
    assert_eq!(ready[0]["matching_total"], 1);
    assert_eq!(ready[0]["inventory_total"], 6);
    let by_prefix = fixture.walk(&["--kind", "hyp"]);
    let by_name = fixture.walk(&["--kind", "Hypothesis", "--status", "requires_source"]);
    assert_eq!(gap_ids(&by_prefix).len(), 5);
    assert_eq!(gap_ids(&by_prefix), gap_ids(&by_name));
    assert_eq!(by_name[0]["filter"]["subject_kinds"], json!(["hyp"]));
    let both = fixture.walk(&["--status", "ready_to_analyze,requires_source"]);
    assert_eq!(gap_ids(&both), walked);
    let by_rule = fixture.walk(&["--rule", "inquiry.confirmatory_plan_unfrozen"]);
    assert_eq!(gap_ids(&by_rule), gap_ids(&ready));
    let none = fixture.walk(&["--status", "deferred"]);
    assert!(gap_ids(&none).is_empty());
    assert!(none[0]["next_cursor"].is_null());
}

#[test]
fn paging_across_a_concurrent_write_stays_on_the_original_snapshot() {
    let mut fixture = Fixture::new();
    let hypotheses: Vec<Value> = (0..4).map(|index| fixture.hypothesis(index)).collect();
    let original = gap_ids(&fixture.walk(&["--limit", "50"]));
    assert_eq!(original.len(), 4);

    let first = fixture.frontier(&["--limit", "2"], 0);
    // A concurrent writer adds one gap and removes another after page one.
    fixture.hypothesis(9);
    let retired = hypotheses.last().unwrap().clone();
    fixture.submit(
        "hypothesis.disposition",
        json!({
            "hypothesis_ref":retired,"lifecycle":"retired",
            "rationale":"The recorded scope excludes this account.","evidence_refs":[],
        }),
    );

    let second = follow(&fixture.root, &first["next_page_argv"], 0);
    assert_eq!(second["snapshot"], first["snapshot"]);
    assert_eq!(second["position"], 2);
    assert!(
        second["newer_head"]["sequence"].as_u64().unwrap()
            > first["snapshot"]["sequence"].as_u64().unwrap()
    );
    assert_eq!(
        gap_ids(&[first.clone(), second.clone()]),
        original,
        "the sequence neither gains the new gap nor loses the retired one"
    );

    // Restarting reads the current head, which differs from the pinned sequence.
    let restarted = follow(&fixture.root, &second["restart_argv"], 0);
    assert_eq!(restarted["snapshot"], second["newer_head"]);
    assert!(restarted.get("newer_head").is_none());
    assert_eq!(restarted["position"], 0);
    let current = gap_ids(&fixture.walk(&[]));
    assert_eq!(current.len(), 4);
    assert_ne!(current, original);

    // A cursor resent with another filter is rejected with a restart route.
    let cursor = first["next_cursor"].as_str().unwrap();
    let mismatch = fixture.frontier(&["--status", "deferred", "--cursor", cursor], 2);
    assert_eq!(mismatch["error"]["code"], "INVALID_INPUT");
    assert_eq!(mismatch["error"]["path"], "/cursor");
    let restart = &mismatch["error"]["repair"]["argv"];
    assert!(
        !restart
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part == "--cursor")
    );
    assert!(
        restart
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part == "deferred")
    );
    let repaired = follow(&fixture.root, restart, 0);
    assert_eq!(repaired["filter"]["statuses"], json!(["deferred"]));

    let damaged = format!("{}x", &cursor[..cursor.len() - 1]);
    let failure = fixture.frontier(&["--cursor", &damaged], 2);
    assert_eq!(failure["error"]["path"], "/cursor");
    follow(&fixture.root, &failure["error"]["repair"]["argv"], 0);

    let limit = fixture.frontier(&["--limit", "0"], 2);
    assert_eq!(limit["error"]["path"], "/limit");
    follow(&fixture.root, &limit["error"]["repair"]["argv"], 0);
}

#[test]
fn explain_names_the_rule_and_version_for_any_gap_id() {
    let mut fixture = Fixture::new();
    let hypothesis = fixture.hypothesis(0);
    let gap_id = format!(
        "gap:hypothesis.no_assessment:{}",
        hypothesis["id"].as_str().unwrap()
    );
    let explained = fixture.frontier(&["--explain", &gap_id], 0);
    assert_eq!(explained["schema_version"], "inv.gap_explanation.v1");
    assert_eq!(explained["emitted"], true);
    assert_eq!(explained["rule"]["rule_id"], "hypothesis.no_assessment");
    assert_eq!(explained["rule"]["rule_version"], "slice-gaps.v1");
    assert_eq!(explained["rule"]["emitted_status"], "requires_source");
    assert_eq!(explained["gap"]["gap_id"], json!(gap_id));
    assert_eq!(explained["subjects"][0]["current_revision"], 1);
    assert_eq!(explained, fixture.frontier(&["--explain", &gap_id], 0));

    // A gap the snapshot does not emit is still explained by its rule.
    let absent = format!(
        "gap:review.stale_read_set:{}",
        "rev_019acd16-0000-7000-8000-000000000001"
    );
    let not_emitted = fixture.frontier(&["--explain", &absent], 0);
    assert_eq!(not_emitted["emitted"], false);
    assert!(not_emitted["gap"].is_null());
    assert_eq!(not_emitted["rule"]["rule_id"], "review.stale_read_set");
    assert_eq!(not_emitted["rule"]["rule_version"], "slice-gaps.v1");
    assert!(not_emitted["subjects"][0]["current_revision"].is_null());

    let unknown = fixture.frontier(
        &["--explain", &gap_id.replace("no_assessment", "renamed")],
        3,
    );
    assert_eq!(unknown["error"]["code"], "UNKNOWN_REFERENCE");
    assert!(
        unknown["error"]["expected"]
            .as_array()
            .unwrap()
            .contains(&json!("hypothesis.no_assessment"))
    );
    follow(&fixture.root, &unknown["error"]["repair"]["argv"], 0);
    let malformed = fixture.frontier(&["--explain", "hypothesis.no_assessment"], 2);
    assert_eq!(malformed["error"]["path"], "/gap_id");
    let combined = fixture.frontier(&["--explain", &gap_id, "--status", "deferred"], 2);
    assert_eq!(combined["error"]["code"], "INVALID_INPUT");
}

#[test]
fn text_output_is_deterministic_and_unknown_filters_are_rejected() {
    let mut fixture = Fixture::new();
    fixture.hypothesis(0);
    let text = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
        let output = command
            .current_dir(&fixture.root)
            .env_remove("AGENT")
            .env_remove("FORMAT")
            .args(["--case", &fixture.case, "frontier"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };
    let rendered = text(&[]);
    assert_eq!(rendered, text(&[]));
    assert!(rendered.contains("hypothesis.no_assessment (slice-gaps.v1)"));
    assert!(rendered.contains("prerequisites: evidence_refs (unknown)"));
    assert!(rendered.contains("does not mean the investigation is complete"));
    assert!(!rendered.contains('\u{1b}'));

    let kind = fixture.frontier(&["--kind", "widget"], 2);
    assert_eq!(kind["error"]["path"], "--kind");
    let rule = fixture.frontier(&["--rule", "hypothesis.no_such_rule"], 3);
    assert_eq!(rule["error"]["code"], "UNKNOWN_REFERENCE");
    let status = fixture.frontier(&["--status", "open"], 2);
    assert_eq!(status["error"]["code"], "INVALID_INPUT");
}
