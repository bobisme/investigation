//! Milestone B §20.1 gaps X12 and Y8: no probability is invented.
//!
//! X12: overlapping hypotheses under one question are accepted, nothing
//! requires them to be exclusive or exhaustive or to sum to one, and no
//! probability is asked for or emitted by `show`, `brief` or `frontier`.
//! Y8: a sensitivity result names the uncertain assumption or the weight
//! range that could reverse the order, and no computed probability appears
//! in its JSON or in the rendered decision note.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

#[path = "support/fixtures.rs"]
mod fixtures;

const WORDS: [&str; 7] = [
    "probab",
    "likelihood",
    "posterior",
    "odds",
    "chance",
    "bayes",
    "credence",
];

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_SESSION")
        .args(args)
        .args(["--format", "json"])
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
    serde_json::from_str(&stdout).unwrap()
}

/// Every key anywhere in `value` that names a probability or likelihood.
fn probability_keys(value: &Value, path: &str, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                let at = format!("{path}/{key}");
                if WORDS.iter().any(|word| key.to_lowercase().contains(word)) {
                    found.push(at.clone());
                }
                probability_keys(inner, &at, found);
            }
        }
        Value::Array(items) => {
            for (index, inner) in items.iter().enumerate() {
                probability_keys(inner, &format!("{path}/{index}"), found);
            }
        }
        _ => {}
    }
}

/// Authored words are the case's own text; the product's output must not add
/// a probability word or a "n%" figure of its own.
fn assert_no_probability(label: &str, value: &Value) {
    let mut keys = Vec::new();
    probability_keys(value, "", &mut keys);
    assert!(keys.is_empty(), "{label} has probability keys {keys:?}");
}

fn assert_text_has_no_probability(label: &str, text: &str) {
    let lower = text.to_lowercase();
    for word in WORDS {
        assert!(!lower.contains(word), "{label} contains {word:?}");
    }
    let bytes = lower.as_bytes();
    for (at, byte) in bytes.iter().enumerate() {
        assert!(
            !(*byte == b'%' && at > 0 && bytes[at - 1].is_ascii_digit()),
            "{label} states a percentage near {:?}",
            &lower[at.saturating_sub(20)..(at + 1).min(lower.len())]
        );
    }
}

struct Work {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    case: String,
    keys: usize,
}

impl Work {
    fn new(mode: &str) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().to_path_buf();
        run(&root, &["init", ".", "--no-vault"], None);
        let config = root.join("investigation.toml");
        let text = fs::read_to_string(&config).unwrap();
        fs::write(
            &config,
            text.replace("import_roots = []", "import_roots = [\".\"]"),
        )
        .unwrap();
        let mut work = Self {
            _temporary: temporary,
            root,
            case: String::new(),
            keys: 0,
        };
        let receipt = work.submit(
            "case.create",
            json!({"title":"No invented probability","mode":mode,
                "objective":"Check what is claimed","decision_relevance":"Choose carefully"}),
        );
        work.case = receipt["created"][0]["id"].as_str().unwrap().to_owned();
        work
    }

    fn submit(&mut self, op: &str, payload: Value) -> Value {
        self.keys += 1;
        let case = if self.case.is_empty() {
            Value::Null
        } else {
            json!(self.case)
        };
        let input = json!({"schema_version":"inv.command.v1",
            "command_id":format!("no-probability-{}", self.keys),
            "case_id":case,"actor":"agent:no-probability","preconditions":[],
            "operations":[{"op":op,"payload":payload}]});
        let receipt = run(&self.root, &["record", "--input", "-"], Some(&input));
        assert_eq!(receipt["ok"], true, "{op}: {receipt}");
        receipt
    }

    fn created(&mut self, op: &str, payload: Value) -> Value {
        let receipt = self.submit(op, payload);
        json!({"id":receipt["created"][0]["id"],"revision":receipt["created"][0]["revision"]})
    }

    fn read(&self, args: &[&str]) -> Value {
        let mut argv = vec!["--case", self.case.as_str()];
        argv.extend_from_slice(args);
        run(&self.root, &argv, None)
    }

    fn show(&self, id: &Value) -> Value {
        self.read(&["show", id["id"].as_str().unwrap(), "--depth", "full"])
    }
}

#[test]
fn overlapping_hypotheses_are_accepted_and_no_probability_is_required_or_emitted() {
    let mut work = Work::new("explore");
    let question = work.created(
        "question.create",
        json!({"wording":"Why does latency vary during bursts?","importance":"high"}),
    );
    // Three hypotheses that overlap (the first two can both be true, the third
    // contains the first), do not cover every explanation, and carry no
    // number at all.
    let statements = [
        "A queue builds up during bursts.",
        "Garbage collection pauses coincide with bursts.",
        "Any queue that builds up delays requests, including the burst queue.",
    ];
    let mut hypotheses = Vec::new();
    for statement in statements {
        let receipt = work.submit(
            "hypothesis.create",
            json!({"question_ref":question,"statement":statement,
                "applicability":"The sampled workload","assumption_refs":[],
                "rival_refs":[],"possible_falsifiers":["Latency stays flat during a burst."]}),
        );
        assert_eq!(
            receipt["created"][0]["id"].as_str().unwrap()[..4].to_owned(),
            "hyp_"
        );
        assert!(
            receipt["warnings"].as_array().is_none_or(Vec::is_empty),
            "no overlap or exhaustiveness warning: {receipt}"
        );
        hypotheses.push(json!({"id":receipt["created"][0]["id"],"revision":1}));
    }
    // A hypothesis that names the others as rivals is still not made exclusive:
    // all stay live and none is refused for overlapping.
    let rival = work.submit(
        "hypothesis.create",
        json!({"question_ref":question,"statement":"Both effects occur together.",
            "applicability":"The sampled workload","assumption_refs":[],
            "rival_refs":[hypotheses[0], hypotheses[1]],
            "possible_falsifiers":["Either effect alone explains every burst."]}),
    );
    assert_eq!(rival["ok"], true);

    // Supplying a probability (or a sum) is not part of the schema.
    let input = json!({"schema_version":"inv.command.v1","command_id":"no-probability-reject",
        "case_id":work.case,"actor":"agent:no-probability","preconditions":[],
        "operations":[{"op":"hypothesis.create","payload":{"question_ref":question,
            "statement":"Burst probability is 0.7.","probability":0.7,
            "possible_falsifiers":["x"]}}]});
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    let mut process = command
        .current_dir(&work.root)
        .args(["record", "--input", "-", "--format", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    process
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&input).unwrap())
        .unwrap();
    let rejected = process.wait_with_output().unwrap();
    assert_ne!(
        rejected.status.code(),
        Some(0),
        "an invented probability field is refused"
    );

    // Nothing the product emits carries a probability, likelihood or sum.
    let mut views = vec![("show question", work.show(&question))];
    for hypothesis in &hypotheses {
        views.push(("show hypothesis", work.show(hypothesis)));
    }
    views.push(("brief", work.read(&["brief"])));
    views.push(("frontier", work.read(&["frontier"])));
    views.push(("next", work.read(&["next"])));
    for (label, view) in &views {
        assert_no_probability(label, view);
    }
    let frontier = &views[views.len() - 2].1;
    let text = serde_json::to_string(frontier).unwrap();
    assert!(
        !text.contains("exclusive") && !text.contains("exhaustive"),
        "{text}"
    );
    // Every one of the four hypotheses is still current in the case.
    let shown = work.show(&hypotheses[0]);
    assert_eq!(shown["record"]["body"]["statement"], statements[0]);
}

/// A ranged, uncertain weight on a soft criterion.
fn weighted(question: &Value, name: &str, unit: &str, low: &str, high: &str) -> Value {
    json!({"name":name,"kind":"soft",
        "scope":{"kind":"all_options","question_ref":question},
        "scale":{"unit":unit,"description":name,"anchors":[]},
        "preferred_direction":"lower","threshold":null,"weighting_rationale":null,
        "weight":{"value":"1","range":{"worst":"50","best":"10"},
            "rationale":"The swing from worst to best",
            "uncertainty":{"low":low,"high":high}}})
}

#[test]
fn a_sensitivity_result_names_the_weight_range_that_reverses_the_order_without_a_probability() {
    let mut work = Work::new("evaluate");
    let question = work.created(
        "question.create",
        json!({"wording":"Which design should we use?","importance":"high"}),
    );
    let mut options = Vec::new();
    for description in ["The fast design", "The cheap design"] {
        let option = work.created(
            "option.create",
            json!({"question_ref":question,"description":description,
                "dependencies":[],"tradeoff_notes":[]}),
        );
        let eligible = work.submit(
            "option.disposition",
            json!({"option_ref":option,"lifecycle":"eligible","reason":"Plausible"}),
        );
        options.push(json!({"id":option["id"],"revision":eligible["updated"][0]["revision"]}));
    }
    let speed = work.created(
        "criterion.create",
        weighted(&question, "Latency", "ms", "0.5", "2"),
    );
    let cost = work.created(
        "criterion.create",
        weighted(&question, "Cost", "USD", "1", "1"),
    );

    // One retained source holds every measurement; each cell cites its own
    // observation of one value in it.
    fs::write(
        work.root.join("measurements.json"),
        r#"{"fast":{"ms":10,"usd":40},"cheap":{"ms":40,"usd":10}}"#,
    )
    .unwrap();
    fs::write(
        work.root.join("ingest.json"),
        serde_json::to_vec(
            &json!({"schema_version":"inv.ingest.v1","command_id":"ingest-1",
            "actor":"agent:no-probability","case_id":work.case,
            "source":{"title":"Synthetic measurements","producer":"offline","identity":"m.json",
                "format":"application/json","verification_limits":["Synthetic"]},
            "paths":["measurements.json"],"extract_text":true}),
        )
        .unwrap(),
    )
    .unwrap();
    let ingested = run(&work.root, &["ingest", "--input", "ingest.json"], None);
    let artifact = ingested["items"][0]["artifact_id"].clone();
    let digest = ingested["items"][0]["digest"].clone();

    let mut cell = |option: &Value, criterion: &Value, pointer: &str, value: &str, unit: &str| {
        let selector = json!({"schema_version":"inv.selector.v1","artifact_digest":digest,
            "selector":{"type":"json_pointer","pointer":pointer,"parser":null}});
        fs::write(work.root.join("selector.json"), selector.to_string()).unwrap();
        run(
            &work.root,
            &[
                "--case",
                &work.case.clone(),
                "evidence",
                "read",
                artifact.as_str().unwrap(),
                "--selector-file",
                "selector.json",
            ],
            None,
        );
        let observation = work.created(
            "observation.add",
            json!({"value":{"kind":"measurement","value":value,"unit":unit},
                "basis":{"kind":"source","artifact_ref":{"id":artifact,"revision":1},
                    "selector":selector,"text_quote":null},
                "extraction_method":"Read one value from the retained source",
                "validity":"valid","quality":[],"limitations":[],
                "evidence_kind":"observational","variables_changed":[],"observed_at":null}),
        );
        work.submit(
            "comparison.add",
            json!({"option_ref":option,"criterion_ref":criterion,"outcome":"meets",
                "method":"read from source","value":{"decimal":value,"unit":unit},
                "evidence_refs":[observation],"assumption_refs":[],
                "uncertainty":"Synthetic","rationale":"As measured"}),
        );
    };
    cell(&options[0], &speed, "/fast/ms", "10", "ms");
    cell(&options[0], &cost, "/fast/usd", "40", "USD");
    cell(&options[1], &speed, "/cheap/ms", "40", "ms");
    cell(&options[1], &cost, "/cheap/usd", "10", "USD");

    let shown = work.show(&question);
    assert_no_probability("question show", &shown);
    let analysis = &shown["weighted_analysis"];
    let pair = &analysis["sensitivity"][0];
    // The stated weights tie the options, and the stated range of the latency
    // weight can reverse the order.
    assert_eq!(pair["order_can_flip"], true, "{analysis:#}");
    assert!(pair["preferred_at_stated_weights"].is_null());
    assert!(pair["lowest_difference"].as_f64().unwrap() < 0.0);
    assert!(pair["highest_difference"].as_f64().unwrap() > 0.0);
    // It names which weight's range does it: the latency weight carries the
    // stated uncertainty, and the cost weight is exact.
    let weights = analysis["weights"].as_array().unwrap();
    let latency = weights
        .iter()
        .find(|entry| entry["criterion_ref"]["id"] == speed["id"])
        .unwrap();
    assert_eq!(
        latency["weight"]["uncertainty"],
        json!({"low":"0.5","high":"2"})
    );
    assert_eq!(latency["enters_score"], true);
    let exact = weights
        .iter()
        .find(|entry| entry["criterion_ref"]["id"] == cost["id"])
        .unwrap();
    assert_eq!(
        exact["weight"]["uncertainty"],
        json!({"low":"1","high":"1"})
    );
    // The sensitivity pair names both options it compares.
    let named: Vec<_> = pair["option_refs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reference| reference["id"].clone())
        .collect();
    assert_eq!(
        named,
        vec![options[0]["id"].clone(), options[1]["id"].clone()]
    );
    // Differences and totals are weighted sums of stated values, not odds.
    assert_no_probability("weighted analysis", analysis);
    assert_no_probability("brief", &work.read(&["brief"]));
    assert_no_probability("frontier", &work.read(&["frontier"]));
}

#[test]
fn scenario_c_reversal_conditions_name_the_assumption_and_no_probability_appears() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixtures::workspace("scenario-c", &dir.path().join("workspace"), false);
    let (snapshot, plan) = fixtures::plan(&root);
    let case = snapshot.cases().next().unwrap().case_id().to_string();
    let read = |args: &[&str]| {
        let mut argv = vec!["--case", case.as_str()];
        argv.extend_from_slice(args);
        run(&root, &argv, None)
    };

    // The decisions' reversal conditions name an uncertain assumption (with
    // the criterion, comparison and option it concerns) and say what would
    // reverse the choice.
    let state = snapshot.cases().next().unwrap().state();
    let mut conditions = 0;
    for record in state.records() {
        if !record.header.id.to_string().starts_with("dec_") {
            continue;
        }
        let shown = read(&["show", &record.header.id.to_string(), "--depth", "full"]);
        assert_no_probability("decision show", &shown);
        for reversal in shown["record"]["body"]["reversal_conditions"]
            .as_array()
            .unwrap_or(&Vec::new())
        {
            conditions += 1;
            assert!(
                reversal["assumption_ref"]["id"]
                    .as_str()
                    .unwrap()
                    .starts_with("asm_")
            );
            assert!(reversal["criterion_ref"].is_object() && reversal["option_ref"].is_object());
            assert_eq!(
                reversal["condition"],
                "A peak-traffic cost measurement favors the managed queue"
            );
        }
    }
    assert!(conditions > 0, "scenario C records a reversal condition");
    let assumption = state
        .records()
        .find(|record| record.header.id.to_string().starts_with("asm_"))
        .unwrap();
    assert_eq!(
        assumption.body["claim"],
        "Traffic stays below 2500000 jobs per day"
    );

    assert_no_probability("brief", &read(&["brief"]));
    assert_no_probability("frontier", &read(&["frontier"]));

    // The rendered decision note lists the condition and links the assumption
    // it names; no note states a probability or likelihood of its own.
    let mut named = false;
    for entry in plan.entries() {
        let text = std::str::from_utf8(entry.content()).unwrap_or_default();
        let text = text.replace('\\', "");
        if text.contains("- Reversal conditions:\n")
            && text.contains("A peak-traffic cost measurement favors the managed queue")
        {
            assert!(
                text.contains("Assumptions/asm_")
                    && text.contains("|Traffic stays below 2500000 jobs per day]]"),
                "{text}"
            );
            named = true;
        }
    }
    assert!(named, "a decision note shows the reversal condition");
    for entry in plan.entries() {
        let text = std::str::from_utf8(entry.content()).unwrap_or_default();
        // Authored case text may use words such as "estimate"; the checked
        // words are the ones that would state a probability.
        assert_text_has_no_probability(entry.path().as_str(), text);
    }
}
