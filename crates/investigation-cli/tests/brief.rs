//! `brief` levels and budgets, semantic deltas since a transaction, and reads
//! pinned to a historical snapshot, through the public binary.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:brief-test";
const DEFAULT_BUDGET: usize = 16_384;
const WORKSPACE_SENTINEL: &str = "<WORKSPACE>";

/// Run the binary; return the parsed JSON and the exact stdout byte count.
fn call(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> (Value, usize) {
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
        output.stderr
    );
    (serde_json::from_str(&stdout).expect(&stdout), stdout.len())
}

fn run(cwd: &Path, args: &[&str], exit: i32) -> Value {
    call(cwd, args, None, exit).0
}

/// Follow an argv a response returned, from the program name on.
fn follow(cwd: &Path, argv: &Value) -> (Value, usize) {
    let argv: Vec<&str> = argv
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|part| part.as_str().unwrap())
        .collect();
    call(cwd, &argv, None, 0)
}

fn exact(id: &str, revision: u64) -> Value {
    json!({"id":id,"revision":revision})
}

fn example_payload(root: &Path, operation: &str) -> Value {
    run(root, &["schema", operation, "--json"], 0)["transaction_example"]["input"]["operations"][0]
        ["payload"]
        .clone()
}

struct Casework {
    _dir: tempfile::TempDir,
    case: String,
    source: String,
    artifact: String,
    digest: Value,
    created: Vec<String>,
    step: usize,
}

impl Casework {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run(root, &["init", ".", "--no-vault", "--json"], 0);
        let config = root.join("investigation.toml");
        fs::write(
            &config,
            fs::read_to_string(&config)
                .unwrap()
                .replace("import_roots = []", "import_roots = [\".\"]"),
        )
        .unwrap();
        fs::write(root.join("bench.txt"), "p99 18.7 ms").unwrap();
        let start = run(
            root,
            &[
                "start",
                "Does the queue meet the latency limit?",
                "--attach",
                "bench.txt",
                "--command-id",
                "capture",
                "--actor",
                ACTOR,
                "--json",
            ],
            0,
        );
        let item = &start["items"][0];
        Self {
            case: start["created"][0]["id"].as_str().unwrap().to_owned(),
            source: item["source_id"].as_str().unwrap().to_owned(),
            artifact: item["artifact_id"].as_str().unwrap().to_owned(),
            digest: item["digest"].clone(),
            created: start["created"]
                .as_array()
                .unwrap()
                .iter()
                .map(|change| change["id"].as_str().unwrap().to_owned())
                .collect(),
            _dir: dir,
            step: 0,
        }
    }

    fn root(&self) -> &Path {
        self._dir.path()
    }

    /// Record one transaction; return its receipt.
    fn record(&mut self, operations: Vec<(&str, Value)>) -> Value {
        self.step += 1;
        let operations: Vec<Value> = operations
            .into_iter()
            .map(|(op, payload)| json!({"op":op,"payload":payload}))
            .collect();
        let receipt = call(
            self.root(),
            &["record", "--input", "-", "--json"],
            Some(&json!({
                "schema_version":"inv.command.v1","command_id":format!("step-{}", self.step),
                "case_id":self.case,"actor":ACTOR,"preconditions":[],"operations":operations
            })),
            0,
        )
        .0;
        for change in receipt["created"].as_array().unwrap() {
            self.created.push(change["id"].as_str().unwrap().to_owned());
        }
        receipt
    }

    fn observation(&self, value: &str) -> Value {
        json!({
            "value":{"kind":"measurement","value":value,"unit":"ms"},
            "basis":{"kind":"source","artifact_ref":exact(&self.artifact, 1),
                "selector":{"schema_version":"inv.selector.v1","artifact_digest":self.digest,
                    "selector":{"type":"whole"}},"text_quote":null},
            "validity":"valid","quality":[],"limitations":[],"evidence_kind":"observational","variables_changed":[]
        })
    }

    fn observations(&mut self, count: usize) -> Vec<String> {
        let operations = (0..count)
            .map(|index| {
                (
                    "observation.add",
                    self.observation(&format!("4217.{index}")),
                )
            })
            .collect();
        ids(&self.record(operations))
    }

    fn conclusion(statement: &str, support: &[String], caveat: &str) -> Value {
        json!({
            "statement":statement,"scope":"The recorded benchmark.","significance":"Informs the queue choice.",
            "support_refs":support.iter().map(|id| exact(id, 1)).collect::<Vec<_>>(),
            "contradiction_refs":[],"caveats":[caveat]
        })
    }

    fn brief(&self, args: &[&str]) -> (Value, usize) {
        let mut argv = vec!["brief"];
        argv.extend(args);
        argv.push("--json");
        call(self.root(), &argv, None, 0)
    }
}

fn ids(receipt: &Value) -> Vec<String> {
    receipt["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["id"].as_str().unwrap().to_owned())
        .collect()
}

fn omitted<'a>(brief: &'a Value, section: &str) -> Option<&'a Value> {
    brief["omitted"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["section"] == section)
}

/// Measure an envelope without making its invariant depend on the host path.
///
/// Production argv stay exact and runnable. This clone is only for the fixed
/// envelope-size assertion below, where every `--workspace` value represents
/// the same field regardless of how long the test runner's `TMPDIR` is.
fn path_independent_envelope_bytes(value: &Value) -> usize {
    let mut normalized = value.clone();
    normalize_workspace_argv(&mut normalized);
    serde_json::to_vec(&normalized).unwrap().len() + 1
}

fn normalize_workspace_argv(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "argv" || key.ends_with("_argv") {
                    normalize_workspace_arg_values(value);
                } else {
                    normalize_workspace_argv(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                normalize_workspace_argv(value);
            }
        }
        _ => {}
    }
}

fn normalize_workspace_arg_values(value: &mut Value) {
    let Some(values) = value.as_array_mut() else {
        return;
    };
    let mut index = 0;
    while index + 1 < values.len() {
        if values[index].as_str() == Some("--workspace") && values[index + 1].is_string() {
            values[index + 1] = json!(WORKSPACE_SENTINEL);
        }
        index += 1;
    }
}

#[test]
fn envelope_measurement_normalizes_only_argv_fields_across_paths() {
    fn envelope(workspace: &str) -> Value {
        json!({
            "omitted": [{
                "section": "freshness",
                "argv": ["investigation", "--workspace", workspace, "show", "case_1"]
            }],
            "complete_read_argv": [
                "investigation", "--workspace", workspace, "brief", "--max-bytes", "8192"
            ],
            "error": {"repair": {
                "argv": ["investigation", "--workspace", workspace, "brief"]
            }},
            "data": ["--workspace", "not-an-argv-value", 7],
            "typed_argv": ["investigation", "--workspace", 7, "brief"]
        })
    }

    let short_path = "/tmp/investigation";
    let long_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/test-tmp/investigation-workspace"
    );
    let short = envelope(short_path);
    let long = envelope(long_path);
    assert!(serde_json::to_vec(&short).unwrap().len() < serde_json::to_vec(&long).unwrap().len());
    assert_eq!(
        path_independent_envelope_bytes(&short),
        path_independent_envelope_bytes(&long)
    );

    let mut normalized = long.clone();
    normalize_workspace_argv(&mut normalized);
    assert_eq!(
        normalized["data"],
        json!(["--workspace", "not-an-argv-value", 7])
    );
    assert_eq!(normalized["omitted"][0]["argv"][2], WORKSPACE_SENTINEL);
    assert_eq!(normalized["complete_read_argv"][2], WORKSPACE_SENTINEL);
    assert_eq!(normalized["error"]["repair"]["argv"][2], WORKSPACE_SENTINEL);
    assert_eq!(normalized["typed_argv"][2], 7);

    // Measurement works on a clone: the real serialized argv remain runnable.
    assert_eq!(short["omitted"][0]["argv"][2], short_path);
    assert_eq!(short["complete_read_argv"][2], short_path);
    assert_eq!(long["omitted"][0]["argv"][2], long_path);
    assert_eq!(long["complete_read_argv"][2], long_path);
    assert_eq!(long["error"]["repair"]["argv"][2], long_path);
    assert_eq!(long["data"][1], "not-an-argv-value");
    assert_eq!(long["typed_argv"][2], 7);
}

#[test]
fn the_default_brief_of_a_large_stale_case_stays_within_budget_and_reports_omissions() {
    let mut work = Casework::start();
    let root = work.root().to_path_buf();
    let observations = work.observations(60);
    let conclusions: Vec<(&str, Value)> = (0..14)
        .map(|index| {
            (
                "conclusion.create",
                Casework::conclusion(
                    &format!("Finding {index}: the queue met the latency limit."),
                    &observations[index..index + 20],
                    &format!("Caveat {index}: one benchmark run."),
                ),
            )
        })
        .collect();
    let conclusions = ids(&work.record(conclusions));
    let mut questions = Vec::new();
    for index in 0..20 {
        let mut question = example_payload(&root, "question.create");
        question["wording"] = json!(format!("Open question {index} about the latency limit?"));
        questions.push(("question.create", question));
    }
    work.record(questions);
    work.record(vec![(
        "source.withdraw",
        json!({"source_ref":exact(&work.source, 1),"reason":"retracted_by_origin","scope":{"extent":"entire"},
            "rationale":"The publisher retracted the benchmark report.","evidence_refs":[]}),
    )]);

    // The case view lists every stale record; the brief caps that list.
    let case = run(&root, &["show", &work.case, "--depth", "full", "--json"], 0);
    let affected = case["freshness"]["overview"]["affected"]
        .as_array()
        .unwrap()
        .len();
    assert!(affected > 70, "{affected}");

    let (brief, bytes) = work.brief(&[]);
    assert!(bytes <= DEFAULT_BUDGET, "{bytes}");
    assert_eq!(brief["result"], "brief");
    assert_eq!(brief["freshness"]["effective"], "needs_review");
    let overview = &brief["freshness"]["overview"];
    assert_eq!(overview["affected"].as_array().unwrap().len(), 8);
    assert_eq!(overview["affected_omitted"], affected - 8);
    assert_eq!(overview["needs_review"], affected);
    // The strongest caveat of the newest live outcome is never cut.
    let primary = conclusions.last().unwrap();
    assert_eq!(brief["material_caveat"]["state"], "known");
    assert_eq!(brief["outcomes"][0]["id"], primary.as_str());
    assert_eq!(
        brief["outcomes"][0]["material_caveat"],
        brief["material_caveat"]
    );

    // The capped list names its complete read, pinned to the same snapshot.
    let cap = omitted(&brief, "freshness").expect("freshness cap is reported");
    assert_eq!(cap["cause"], "list_cap");
    let argv = cap["argv"].to_string();
    assert!(
        argv.contains("--snapshot") && argv.contains("--head-hash"),
        "{argv}"
    );
    let (complete_case, _) = follow(&root, &cap["argv"]);
    assert_eq!(
        complete_case["freshness"]["overview"]["affected"]
            .as_array()
            .unwrap()
            .len(),
        affected
    );

    // Sections that do not fit are dropped whole and counted, not failed.
    let outcomes = omitted(&brief, "outcomes").expect("some outcomes are omitted");
    assert_eq!(outcomes["cause"], "budget");
    assert_eq!(outcomes["total"], conclusions.len());
    assert_eq!(
        brief["outcomes"].as_array().unwrap().len()
            + outcomes["omitted"].as_u64().unwrap() as usize,
        conclusions.len()
    );
    assert_eq!(brief["omitted_items"]["outcomes"], outcomes["omitted"]);
    let (complete, complete_bytes) = follow(&root, &brief["complete_read_argv"]);
    assert_eq!(complete["snapshot"], brief["snapshot"]);
    assert_eq!(
        complete["outcomes"].as_array().unwrap().len(),
        conclusions.len()
    );
    assert!(complete.get("complete_read_argv").is_none());
    assert!(
        complete["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["cause"] != "budget"),
        "{}",
        complete["omitted"]
    );
    assert!(complete_bytes > DEFAULT_BUDGET);

    // The minimum envelope does not grow with the case.
    for level in ["compact", "standard", "deep"] {
        let error = run(
            &root,
            &["brief", "--level", level, "--max-bytes", "1", "--json"],
            2,
        );
        assert_eq!(error["error"]["code"], "BUDGET_TOO_SMALL");
        let minimum = error["error"]["expected"]["minimum_bytes"]
            .as_u64()
            .unwrap() as usize;
        let (bounded, bytes) = follow(&root, &error["error"]["repair"]["argv"]);
        assert!(bytes <= minimum, "{level} {bytes} {minimum}");
        let envelope_bytes = path_independent_envelope_bytes(&bounded);
        assert!(
            envelope_bytes < DEFAULT_BUDGET / 2,
            "{level} {envelope_bytes}"
        );
        assert_eq!(bounded["material_caveat"], brief["material_caveat"]);
        assert_eq!(bounded["contradiction"], brief["contradiction"]);
        assert_eq!(bounded["freshness"], brief["freshness"]);
        assert_eq!(bounded["omitted_items"]["outcomes"], conclusions.len());
    }
}

#[test]
fn a_compact_brief_of_a_forty_record_case_halves_a_full_traversal_and_keeps_every_caveat() {
    let mut work = Casework::start();
    let root = work.root().to_path_buf();
    let observations = work.observations(22);
    let mut question = example_payload(&root, "question.create");
    question["wording"] = json!("Which queue meets the latency limit?");
    let question = ids(&work.record(vec![("question.create", question)]))[0].clone();
    let mut hypotheses = Vec::new();
    for statement in [
        "The disk queue meets the limit.",
        "The memory queue meets the limit.",
    ] {
        let mut hypothesis = example_payload(&root, "hypothesis.create");
        hypothesis["question_ref"] = exact(&question, 1);
        hypothesis["statement"] = json!(statement);
        hypotheses.push(("hypothesis.create", hypothesis));
    }
    let hypotheses = ids(&work.record(hypotheses));
    let mut assessments = Vec::new();
    for (index, hypothesis) in hypotheses.iter().cycle().take(4).enumerate() {
        let mut assessment = example_payload(&root, "assessment.add");
        assessment["subject_ref"] = exact(hypothesis, 1);
        assessment["evidence_refs"] = json!([exact(&observations[index], 1)]);
        assessments.push(("assessment.add", assessment));
    }
    work.record(assessments);
    let mut conclusions = Vec::new();
    for index in 0..6 {
        let mut conclusion = Casework::conclusion(
            &format!("Finding {index}: the disk queue met the limit in run {index}."),
            &observations[index * 3..index * 3 + 3],
            &format!("Mandatory caveat {index}: only one synthetic workload."),
        );
        if index % 2 == 0 {
            conclusion["contradiction_refs"] = json!([exact(&observations[20 + index / 4], 1)]);
        }
        conclusions.push(("conclusion.create", conclusion));
    }
    let conclusions = ids(&work.record(conclusions));
    work.record(vec![(
        "inquiry.create",
        example_payload(&root, "inquiry.create"),
    )]);
    assert!(work.created.len() >= 40, "{}", work.created.len());

    let traversal: usize = work
        .created
        .iter()
        .map(|id| call(&root, &["show", id, "--depth", "full", "--json"], None, 0).1)
        .sum();
    let (compact, compact_bytes) = work.brief(&["--level", "compact"]);
    assert!(
        compact_bytes * 2 <= traversal,
        "compact {compact_bytes} vs full traversal {traversal}"
    );
    assert_eq!(compact["level"], "compact");
    assert_eq!(compact["omitted_items"]["outcomes"], 0);
    assert_eq!(compact["omitted_items"]["evidence"], 0);
    assert_eq!(omitted(&compact, "inquiries").unwrap()["cause"], "level");

    // Every caveat, contradiction and status the standard brief shows is kept.
    let (standard, _) = work.brief(&["--level", "standard", "--max-bytes", "1000000"]);
    for key in [
        "material_caveat",
        "contradiction",
        "effective_status",
        "freshness",
    ] {
        assert_eq!(compact[key], standard[key], "{key}");
    }
    let outcomes = standard["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), conclusions.len());
    for outcome in outcomes {
        let entry = compact["outcomes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == outcome["id"])
            .expect("every outcome is indexed");
        for key in [
            "material_caveat",
            "contradiction",
            "effective_status",
            "evidence_refs",
        ] {
            assert_eq!(entry[key], outcome[key], "{key}");
        }
        assert_eq!(entry["freshness"]["label"], outcome["freshness"]["label"]);
        assert!(entry.get("summary").is_none());
        assert_eq!(entry["material_caveat"]["state"], "known");
    }
    let contradicted = compact["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["contradiction"]["state"] == "known")
        .count();
    assert_eq!(contradicted, 3);
    let evidence = compact["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), observations.len());
    assert!(
        evidence
            .iter()
            .all(|entry| entry["evidentiary_use"]["state"] == "in_use")
    );

    // Depth and budget are orthogonal: deep adds full records when they fit
    // and never displaces the mandatory caveat.
    let (deep, deep_bytes) = work.brief(&["--level", "deep"]);
    assert!(deep_bytes <= DEFAULT_BUDGET);
    assert_eq!(deep["material_caveat"], compact["material_caveat"]);
    assert_eq!(
        omitted(&deep, "full_outcomes").unwrap()["total"],
        conclusions.len()
    );
    let (deep, _) = work.brief(&["--level", "deep", "--max-bytes", "1000000"]);
    assert_eq!(
        deep["full_outcomes"].as_array().unwrap().len(),
        conclusions.len()
    );
    assert!(deep["full_outcomes"][0]["record"]["body"].is_object());
}

#[test]
fn a_delta_since_a_transaction_reports_the_retraction_and_the_newly_stale_records() {
    let mut work = Casework::start();
    let root = work.root().to_path_buf();
    let observations = work.observations(2);
    let mut question = example_payload(&root, "question.create");
    question["wording"] = json!("Which queue meets the latency limit?");
    let question = ids(&work.record(vec![("question.create", question)]))[0].clone();
    let mut hypothesis = example_payload(&root, "hypothesis.create");
    hypothesis["question_ref"] = exact(&question, 1);
    let hypothesis = ids(&work.record(vec![("hypothesis.create", hypothesis)]))[0].clone();
    let mut assessment = example_payload(&root, "assessment.add");
    assessment["subject_ref"] = exact(&hypothesis, 1);
    assessment["evidence_refs"] = json!([exact(&observations[0], 1)]);
    let assessment = ids(&work.record(vec![("assessment.add", assessment)]))[0].clone();
    let checkpoint = work.record(vec![(
        "conclusion.create",
        Casework::conclusion(
            "The queue met the latency limit.",
            &observations[..1],
            "One benchmark.",
        ),
    )]);
    let conclusion = ids(&checkpoint)[0].clone();
    let checkpoint = work.record(vec![(
        "annotation.add",
        json!({"subject_refs":[exact(&conclusion, 1)],"role":"synopsis",
            "content":{"kind":"text","text":"Authored: the benchmark settles it."}}),
    )]);
    let synopsis = ids(&checkpoint)[0].clone();
    let since = checkpoint["transaction_id"].as_str().unwrap().to_owned();

    // Nothing has changed since the latest transaction.
    let (quiet, _) = work.brief(&["--since", &since]);
    assert_eq!(quiet["result"], "brief_delta");
    assert_eq!(quiet["transactions"], 0);
    for section in [
        "newly_stale",
        "retracted_evidence",
        "disposition_changes",
        "new_evidence",
    ] {
        assert_eq!(quiet[section], json!([]), "{section}");
    }

    let retraction = work.record(vec![(
        "observation.retract",
        json!({"observation_ref":exact(&observations[0], 1),
            "revision_reason":"The capture recorded the wrong environment."}),
    )]);
    let observation_ids = work.observations(1);
    let (delta, bytes) = work.brief(&["--since", &since]);
    assert!(bytes <= DEFAULT_BUDGET);
    assert_eq!(delta["since"]["transaction_id"], since.as_str());
    assert_eq!(delta["since"]["snapshot"], checkpoint["snapshot"]);
    assert_eq!(delta["transactions"], 2);
    assert_ne!(delta["snapshot"], retraction["snapshot"]);

    let retracted = &delta["retracted_evidence"][0];
    assert_eq!(retracted["id"], observations[0].as_str());
    assert_ne!(retracted["before"]["status"], retracted["after"]["status"]);
    assert_eq!(retracted["after"]["revision"], 2);

    let stale: Vec<&str> = delta["newly_stale"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert!(stale.contains(&conclusion.as_str()), "{stale:?}");
    assert!(stale.contains(&assessment.as_str()), "{stale:?}");
    let item = delta["newly_stale"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == conclusion.as_str())
        .unwrap();
    assert_eq!(item["before"], "current");
    assert_eq!(item["freshness"]["effective"], "needs_review");
    assert_eq!(
        item["freshness"]["reasons"][0]["cause"]["id"],
        observations[0].as_str()
    );
    let changed: Vec<&str> = delta["disposition_changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert!(changed.contains(&conclusion.as_str()), "{changed:?}");
    assert_eq!(delta["new_evidence"][0]["id"], observation_ids[0].as_str());
    // The authored synopsis no longer describes its subject; its text stays out.
    let stale_synopsis = &delta["stale_synopses"][0];
    assert_eq!(stale_synopsis["id"], synopsis.as_str());
    assert_eq!(stale_synopsis["state"], "stale");
    assert_eq!(stale_synopsis["before"], "current");
    assert_eq!(stale_synopsis["subject_ref"], exact(&conclusion, 1));
    assert!(!stale.contains(&synopsis.as_str()));
    assert!(!delta.to_string().contains("settles it"), "{delta}");
    let (full, _) = work.brief(&["--level", "deep", "--max-bytes", "1000000"]);
    assert!(!full.to_string().contains("settles it"));
    let omission = full["omissions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "authored_synopses")
        .expect("synopses are counted");
    assert_eq!(omission["stale"], 1);
    assert!(omission["argv"].to_string().contains("--snapshot"));
    // The mandatory case caveat is carried even in a delta.
    assert_eq!(delta["material_caveat"]["state"], "known");
    assert_eq!(
        delta["freshness"]["overview"]["needs_review"],
        run(&root, &["show", &work.case, "--json"], 0)["freshness"]["overview"]["needs_review"]
    );

    // A delta is budgeted like a brief.
    let error = run(
        &root,
        &["brief", "--since", &since, "--max-bytes", "1", "--json"],
        2,
    );
    assert_eq!(error["error"]["code"], "BUDGET_TOO_SMALL");
    let (bounded, bytes) = follow(&root, &error["error"]["repair"]["argv"]);
    assert_eq!(bounded["result"], "brief_delta");
    assert!(
        bytes as u64
            <= error["error"]["expected"]["minimum_bytes"]
                .as_u64()
                .unwrap()
    );
    assert!(omitted(&bounded, "newly_stale").is_some());

    let unknown = run(
        &root,
        &[
            "brief",
            "--since",
            "tx_01a206cb-d4be-7c32-9f50-36dc1032f557",
            "--json",
        ],
        3,
    );
    assert_eq!(unknown["error"]["code"], "UNKNOWN_REFERENCE");
    assert_eq!(unknown["error"]["path"], "--since");
}

#[test]
fn a_historical_read_of_a_since_withdrawn_artifact_reports_it_unavailable() {
    let mut work = Casework::start();
    let root = work.root().to_path_buf();
    let observation = work.observations(1)[0].clone();
    let point = work.record(vec![(
        "conclusion.create",
        Casework::conclusion(
            "The queue met the latency limit.",
            std::slice::from_ref(&observation),
            "One run.",
        ),
    )]);
    let snapshot = &point["snapshot"];
    let sequence = snapshot["sequence"].to_string();
    let hash = snapshot["head_hash"].as_str().unwrap().to_owned();
    let pinned = |id: &str| {
        run(
            &root,
            &[
                "show",
                id,
                "--snapshot",
                &sequence,
                "--head-hash",
                &hash,
                "--json",
            ],
            0,
        )
    };
    let before = pinned(&work.artifact);
    assert_eq!(before["historical"], false);
    assert!(before.get("historical_read").is_none());
    assert_eq!(before["effective_status"]["state"], "recorded");

    work.record(vec![(
        "artifact.withdraw",
        json!({"artifact_ref":exact(&work.artifact, 1),"reason":"collection_error",
            "scope":{"extent":"entire"},"rationale":"The capture recorded the wrong environment.",
            "evidence_refs":[]}),
    )]);
    let artifact = pinned(&work.artifact);
    assert_eq!(artifact["historical"], true);
    assert_eq!(artifact["snapshot"], *snapshot);
    assert_eq!(artifact["basis_revision"], 1);
    assert_eq!(artifact["historical_read"]["snapshot"], *snapshot);
    assert_ne!(artifact["historical_read"]["latest_snapshot"], *snapshot);
    assert_eq!(
        artifact["effective_status"]["state"],
        "evidence_unavailable"
    );
    assert_eq!(artifact["freshness"]["state"], "evidence_unavailable");
    assert_eq!(
        artifact["material_caveat"]["withdrawn_since_snapshot"][0],
        exact(&work.artifact, 1)
    );
    assert_eq!(artifact["evidentiary_use"]["state"], "in_use");
    assert_eq!(artifact["current_evidentiary_use"]["state"], "withdrawn");
    let shown = pinned(&observation);
    assert_eq!(shown["effective_status"]["state"], "evidence_unavailable");

    // The brief at that snapshot says the same, and its links stay pinned.
    let (brief, _) = work.brief(&["--snapshot", &sequence, "--head-hash", &hash]);
    assert_eq!(brief["historical"], true);
    assert_eq!(brief["snapshot"], *snapshot);
    assert_eq!(
        brief["evidence"][0]["effective_status"]["state"],
        "evidence_unavailable"
    );
    assert_eq!(
        brief["outcomes"][0]["effective_status"]["state"],
        "needs_review"
    );
    let (case, _) = follow(&root, &brief["deeper_read_commands"][0]["argv"]);
    assert_eq!(case["snapshot"], *snapshot);
    assert_eq!(case["historical"], true);
    let (_, sequence_only) = call(
        &root,
        &["brief", "--snapshot", &sequence, "--json"],
        None,
        0,
    );
    assert!(sequence_only > 0);

    // Disclosure follows today's classification, even for an earlier snapshot.
    work.record(vec![(
        "classification.raise",
        json!({"subject_ref":exact(&work.artifact, 2),"level":"restricted","reason":"Unannounced plan."}),
    )]);
    let restricted = pinned(&observation);
    assert_eq!(restricted["content_state"], "restricted");
    let (brief, _) = work.brief(&["--snapshot", &sequence, "--level", "deep"]);
    assert!(!brief.to_string().contains("4217.0"), "{brief}");

    let mismatch = run(
        &root,
        &[
            "show",
            &work.artifact,
            "--snapshot",
            &sequence,
            "--head-hash",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "--json",
        ],
        3,
    );
    assert_eq!(mismatch["error"]["code"], "UNKNOWN_REFERENCE");
    assert!(mismatch["error"]["repair"]["argv"].is_array());
}
