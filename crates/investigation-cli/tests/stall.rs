//! Stalled-investigation detection through the public binary: a plan whose
//! Runs keep failing on the same prerequisite is reported by `next` and
//! `frontier` with the journal items it counted, the `stalled` card from the
//! built-in pack, specific moves and a justified stop.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const RULE: &str = "run.repeated_same_failure";

/// Operations that would start a new hypothesis, plan or test.
const FABRICATING: &[&str] = &[
    "hypothesis.create",
    "hypothesis.amend",
    "inquiry.create",
    "inquiry.amend",
    "inquiry.freeze",
    "run.start",
    "run.record",
    "observation.add",
];

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>) -> Value {
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
    assert_eq!(output.status.code(), Some(0), "{args:?}\n{stdout}");
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    serde_json::from_str(&stdout).expect(&stdout)
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
        run(&root, &["init", ".", "--no-vault", "--json"], None);
        let mut fixture = Self {
            _temporary: temporary,
            root,
            case: String::new(),
            keys: 0,
        };
        let case = fixture.submit(
            "case.create",
            json!({"title":"Slow request","mode":"explain"}),
        );
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
            "schema_version":"inv.command.v1","command_id":format!("stall-{}", self.keys),
            "case_id":case,"actor":"agent:stall","preconditions":[],
            "operations":[{"op":op,"payload":payload}],
        });
        run(
            &self.root,
            &["record", "--input", "-", "--json"],
            Some(&input),
        )
    }

    /// The last record the operation wrote, with the receipt's sequence.
    fn written(&mut self, op: &str, payload: Value) -> (Value, u64) {
        let receipt = self.submit(op, payload);
        let change = receipt["updated"]
            .as_array()
            .and_then(|updated| updated.last())
            .or_else(|| receipt["created"].as_array().and_then(|c| c.last()))
            .unwrap();
        (
            json!({"id":change["id"],"revision":change["revision"]}),
            receipt["snapshot"]["sequence"].as_u64().unwrap(),
        )
    }
}

#[test]
fn repeated_blocked_runs_surface_a_stall_with_the_stalled_card_and_a_justified_stop() {
    let mut fixture = Fixture::new();
    let (question, _) = fixture.written(
        "question.create",
        json!({"wording":"Why is the request slow?","importance":"high"}),
    );
    fixture.written(
        "hypothesis.create",
        json!({"question_ref":question,"statement":"The cache is cold.",
            "possible_falsifiers":["Warm requests are as slow."],"rival_refs":[],"assumption_refs":[]}),
    );
    let (draft, _) = fixture.written(
        "inquiry.create",
        json!({"purpose":"Replay the slow request.","method":"Replay the capture.",
            "controls":["Same build."],"limits":["Three attempts."],
            "desired_observations":["Request latency."],"inquiry":{"mode":"exploratory"}}),
    );
    let (plan, _) = fixture.written(
        "inquiry.freeze",
        json!({"inquiry_ref":draft,"question_ref":question}),
    );
    let mut runs = Vec::new();
    for _ in 0..3 {
        runs.push(fixture.written(
            "run.record",
            json!({"intent":{"kind":"plan","plan_ref":plan},"producer":"agent:fixture",
                "artifact_refs":[],"execution_status":"blocked","evidence_completeness":"complete",
                "evidence_kind":"interventional","variables_changed":["request replay"]}),
        ));
    }

    let next = run(
        &fixture.root,
        &["--case", &fixture.case, "next", "--json"],
        None,
    );
    let recommendation = next["recommendations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["rule_id"] == RULE)
        .unwrap_or_else(|| panic!("no stall recommendation in {next:#}"));
    let gap_id = format!("gap:{RULE}:{}", plan["id"].as_str().unwrap());
    assert_eq!(recommendation["gap_id"], json!(gap_id));
    assert_eq!(recommendation["subject_refs"], json!([plan]));
    assert!(recommendation["schema_operation"].is_null());

    // The journal items cite each Run by its acceptance sequence.
    let stall = &recommendation["stall"];
    assert_eq!(stall["pattern"], "repeated_same_failure");
    assert_eq!(
        stall["thresholds"],
        json!([{"name":"consecutive_failed_runs","value":3}])
    );
    let items: Vec<(Value, Value)> = stall["journal_items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["sequence"].clone(), item["refs"].clone()))
        .collect();
    let expected: Vec<(Value, Value)> = runs
        .iter()
        .map(|(reference, sequence)| (json!(sequence), json!([reference])))
        .collect();
    assert_eq!(items, expected);
    for item in stall["journal_items"].as_array().unwrap() {
        assert_eq!(item["fact"], "run_failed:execution_status:blocked");
    }

    // The general pack's stalled card is attached through the stalled fact.
    let guide = &recommendation["guide"];
    assert_eq!(guide["card"], "stalled", "{recommendation:#}");
    assert_eq!(guide["pack_id"], "general");
    assert_eq!(guide["matched_trigger"]["case_fact"], "stalled");

    // Every move is specific and none starts a new hypothesis or test.
    let moves = stall["moves"].as_array().unwrap();
    for stall_move in moves {
        if let Some(operation) = stall_move["operation"].as_str() {
            assert!(!FABRICATING.contains(&operation), "{operation}");
            let argv = stall_move["schema_argv"].as_array().unwrap();
            assert!(argv.contains(&json!(operation)), "{stall_move:#}");
        }
    }
    let stop: Vec<&str> = moves
        .iter()
        .filter(|stall_move| stall_move["kind"] == "stop_inconclusive")
        .filter_map(|stall_move| stall_move["operation"].as_str())
        .collect();
    assert_eq!(stop, ["inquiry.retire", "question.resolve", "case.pause"]);
    let inspect = moves
        .iter()
        .find(|stall_move| stall_move["kind"] == "inspect_raw_data")
        .unwrap();
    assert!(
        inspect["inspect_argv"]
            .as_array()
            .unwrap()
            .contains(&json!("show"))
    );

    // `next` offers a justified stop that names the stall.
    let offer = &next["stop_or_defer"];
    assert!(
        offer["basis"][0].as_str().unwrap().contains(&gap_id),
        "{offer:#}"
    );
    assert!(
        offer["alternatives"]
            .as_array()
            .unwrap()
            .iter()
            .any(|alternative| alternative["operation"] == "case.pause")
    );

    // The frontier explains the rule, its threshold and the emitted gap.
    let explained = run(
        &fixture.root,
        &[
            "--case",
            &fixture.case,
            "frontier",
            "--explain",
            &gap_id,
            "--json",
        ],
        None,
    );
    assert_eq!(explained["emitted"], true);
    assert!(
        explained["rule"]["condition"]
            .as_str()
            .unwrap()
            .contains("consecutive_failed_runs = 3")
    );
    assert_eq!(
        explained["gap"]["stall"]["journal_items"],
        stall["journal_items"]
    );

    // Text output names the stall and its moves.
    let text = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(&fixture.root)
        .args(["--case", &fixture.case, "next", "--format", "text"])
        .output()
        .unwrap();
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("Stall: repeated_same_failure"), "{text}");
    assert!(text.contains("Guide: stalled (general)"), "{text}");
    assert!(
        text.contains("Move stop_inconclusive (case.pause)"),
        "{text}"
    );
}
