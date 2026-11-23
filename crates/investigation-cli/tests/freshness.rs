//! `show`, `brief` and `frontier` read effective freshness from the dependency
//! index: a real `source.withdraw` reaches every layer with a reason chain, and
//! a judgment that raises no concern changes nothing.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:freshness-test";

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
        "{stdout}\n{:?}",
        output.stderr
    );
    serde_json::from_str(&stdout).expect(&stdout)
}

fn example_payload(root: &Path, operation: &str) -> Value {
    run(root, &["schema", operation, "--json"], None, 0)["transaction_example"]["input"]
        ["operations"][0]["payload"]
        .clone()
}

fn exact(id: &str, revision: u64) -> Value {
    json!({"id":id,"revision":revision})
}

#[test]
fn a_real_source_withdrawal_shows_needs_review_with_a_reason_chain_at_every_layer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    run(root, &["init", ".", "--no-vault", "--json"], None, 0);
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
        None,
        0,
    );
    let case = start["created"][0]["id"].as_str().unwrap().to_owned();
    let item = &start["items"][0];
    let source = item["source_id"].as_str().unwrap().to_owned();
    let artifact = item["artifact_id"].as_str().unwrap().to_owned();
    let mut sequence = 0;
    let mut record = |op: &str, payload: Value| -> Value {
        sequence += 1;
        run(
            root,
            &["record", "--input", "-", "--json"],
            Some(&json!({
                "schema_version":"inv.command.v1","command_id":format!("step-{sequence}"),
                "case_id":case,"actor":ACTOR,"preconditions":[],
                "operations":[{"op":op,"payload":payload}]
            })),
            0,
        )
    };
    let created = |receipt: &Value| receipt["created"][0]["id"].as_str().unwrap().to_owned();
    let observation = created(&record(
        "observation.add",
        json!({
            "value":{"kind":"measurement","value":"18.7","unit":"ms"},
            "basis":{"kind":"source","artifact_ref":exact(&artifact, 1),
                "selector":{"schema_version":"inv.selector.v1","artifact_digest":item["digest"],
                    "selector":{"type":"whole"}},"text_quote":null},
            "validity":"valid","quality":[],"limitations":[],"evidence_kind":"observational","variables_changed":[]
        }),
    ));
    let mut question = example_payload(root, "question.create");
    question["wording"] = json!("Which queue meets the latency limit?");
    let question = created(&record("question.create", question));
    let mut hypothesis = example_payload(root, "hypothesis.create");
    hypothesis["question_ref"] = exact(&question, 1);
    let hypothesis = created(&record("hypothesis.create", hypothesis));
    let mut assessment = example_payload(root, "assessment.add");
    assessment["subject_ref"] = exact(&hypothesis, 1);
    assessment["evidence_refs"] = json!([exact(&observation, 1)]);
    let assessment = created(&record("assessment.add", assessment));
    let conclusion = created(&record(
        "conclusion.create",
        json!({
            "statement":"The queue met the latency limit in the benchmark.","scope":"The recorded benchmark.",
            "significance":"Informs the queue choice.","support_refs":[exact(&observation, 1)],
            "contradiction_refs":[],"caveats":["One benchmark."]
        }),
    ));
    let layers = [&artifact, &observation, &assessment, &conclusion];
    let show = |id: &str| run(root, &["show", id, "--json"], None, 0);
    for id in layers {
        let shown = show(id);
        assert_eq!(shown["freshness"]["state"], "current", "{shown}");
        assert_eq!(shown["freshness"]["effective"], "current", "{shown}");
        assert_eq!(shown["freshness"]["reasons"], json!([]), "{shown}");
    }

    // A judgment that raises no concern is not a reason to review anything.
    record(
        "source.assess",
        json!({"source_ref":exact(&source, 1),
            "currency":{"judgment":"current","rationale":"No later edition found."}}),
    );
    for id in layers {
        let shown = show(id);
        assert_eq!(shown["freshness"]["effective"], "current", "{shown}");
    }
    assert_eq!(show(&conclusion)["effective_status"]["state"], "candidate");
    let frontier = |root: &Path| {
        run(root, &["frontier", "--json"], None, 0)["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|gap| gap["rule_id"] == "freshness.needs_review")
            .cloned()
            .collect::<Vec<_>>()
    };
    assert!(frontier(root).is_empty());

    let withdrawn = record(
        "source.withdraw",
        json!({"source_ref":exact(&source, 2),"reason":"retracted_by_origin","scope":{"extent":"entire"},
            "rationale":"The publisher retracted the benchmark report.","evidence_refs":[]}),
    );
    assert_eq!(withdrawn["updated"][0]["revision"], 3);
    for id in layers {
        let shown = show(id);
        let freshness = &shown["freshness"];
        assert_eq!(freshness["state"], "stale", "{shown}");
        assert_eq!(freshness["effective"], "needs_review", "{shown}");
        let reason = &freshness["reasons"][0];
        assert_eq!(reason["trigger"]["kind"], "withdrawn", "{shown}");
        assert_eq!(reason["cause"], exact(&source, 3), "{shown}");
        assert_eq!(reason["path"][0]["record"], id.as_str(), "{shown}");
        let path = reason["path"].as_array().unwrap();
        assert_eq!(path.last().unwrap()["cites"]["id"], source.as_str());
    }
    let shown = show(&observation);
    assert_eq!(
        shown["freshness"]["reasons"][0]["path"][0]["field"],
        "basis.artifact_ref"
    );
    for id in [&assessment, &conclusion] {
        assert_eq!(show(id)["effective_status"]["state"], "needs_review");
    }
    // The withdrawn Source keeps its record and is not counted as affected.
    let shown = show(&source);
    assert_eq!(shown["effective_status"]["recorded"], "withdrawn");

    let case_view = show(&case);
    let overview = &case_view["freshness"]["overview"];
    assert_eq!(case_view["freshness"]["effective"], "needs_review");
    assert_eq!(case_view["freshness"]["label"], "active / needs_review");
    let affected: Vec<&str> = overview["affected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reference| reference["id"].as_str().unwrap())
        .collect();
    // Every artifact `start` retained derives from the Source, plus the layers.
    assert_eq!(overview["needs_review"], affected.len());
    assert!(layers.iter().all(|id| affected.contains(&id.as_str())));
    assert!(!affected.contains(&source.as_str()));

    let brief = run(root, &["brief", "--level", "standard", "--json"], None, 0);
    assert_eq!(brief["freshness"]["overview"], *overview);
    let outcome = &brief["outcomes"][0];
    assert_eq!(outcome["id"], conclusion.as_str());
    assert_eq!(outcome["freshness"]["effective"], "needs_review");
    assert_eq!(
        outcome["freshness"]["reasons"][0]["trigger"]["kind"],
        "withdrawn"
    );
    let evidence = &brief["evidence"][0];
    assert_eq!(evidence["id"], observation.as_str());
    assert_eq!(
        evidence["freshness"]["reasons"][0]["cause"],
        exact(&source, 3)
    );

    let stale = frontier(root);
    let subjects: Vec<_> = stale
        .iter()
        .map(|gap| gap["subject_refs"][0]["id"].as_str().unwrap())
        .collect();
    assert_eq!(subjects.len(), 2, "{stale:?}");
    assert!(subjects.contains(&assessment.as_str()) && subjects.contains(&conclusion.as_str()));
    let gap_id = stale[0]["gap_id"].as_str().unwrap();
    let explained = run(root, &["frontier", "--explain", gap_id, "--json"], None, 0);
    assert_eq!(explained["emitted"], true);
    assert!(
        explained["gap"]["reason"]
            .as_str()
            .unwrap()
            .contains(&format!("withdrawn: {source} r3 via "))
    );
}
