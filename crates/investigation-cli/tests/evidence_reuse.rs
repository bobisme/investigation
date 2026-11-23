//! `show` reports evidence reuse and deviations of an assessment of a frozen
//! confirmatory plan beside the chronology of the Runs behind its inputs.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:reuse-test";

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

#[test]
fn show_reports_reuse_deviations_and_chronology_as_separate_axes() {
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
    fs::write(root.join("trace.json"), r#"{"stage":"three","ms":18.7}"#).unwrap();
    let start = run(
        root,
        &[
            "start",
            "Which stage is slow?",
            "--attach",
            "trace.json",
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
    let original = start["items"][0]["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let digest = start["items"][0]["digest"].clone();
    let transaction = |key: &str, op: &str, payload: Value| {
        run(
            root,
            &["record", "--input", "-", "--json"],
            Some(
                &json!({"schema_version":"inv.command.v1","command_id":key,"case_id":case,
                "actor":ACTOR,"preconditions":[],"operations":[{"op":op,"payload":payload}]}),
            ),
            0,
        )
    };
    let record = |key: &str, op: &str, payload: Value| {
        let receipt = transaction(key, op, payload);
        let created = &receipt["created"][0];
        json!({"id": created["id"], "revision": created["revision"]})
    };
    let observe = |key: &str, artifact: &str, digest: &Value, run: Value| {
        record(
            key,
            "observation.add",
            json!({
                "value":{"kind":"measurement","value":"18.7","unit":"ms"},
                "basis":{"kind":"source","artifact_ref":{"id":artifact,"revision":1},
                    "selector":{"schema_version":"inv.selector.v1","artifact_digest":digest,
                        "selector":{"type":"whole"}},"text_quote":null},
                "validity":"unknown","quality":[],"limitations":[],"evidence_kind":"observational",
                "variables_changed":[],"run_ref":run,
            }),
        )
    };
    // The trace is inspected before any plan exists.
    let inspected = observe("inspect", &original, &digest, Value::Null);

    let mut question = example_payload(root, "question.create");
    question["wording"] = json!("Which stage is slow?");
    let question = record("question", "question.create", question);
    let hypothesis = |key: &str, statement: &str, rivals: Value| {
        record(
            key,
            "hypothesis.create",
            json!({"question_ref":question,"statement":statement,"possible_falsifiers":[],
                "rival_refs":rivals,"assumption_refs":[]}),
        )
    };
    let local = hypothesis("local", "The local stage is slow.", json!([]));
    let remote = hypothesis("remote", "The remote stage is slow.", json!([local]));
    let draft = record(
        "plan",
        "inquiry.create",
        json!({
            "purpose":"Discriminate the stage explanations.","method":"Compare stage timings.",
            "controls":["Same synthetic load."],"limits":["Ten traces."],
            "desired_observations":["Stage timings per trace."],
            "inquiry":{"mode":"confirmatory","confirmatory":{
                "predictions":[
                    {"rival_ref":local,"predicted_outcome":"Local stage time rises."},
                    {"rival_ref":remote,"predicted_outcome":"Remote stage time rises."}],
                "evidence_holdout_boundary":"Traces 11 through 20 are held out.",
                "prior_exposure":"unseen_declared",
                "stop_analysis_rule":"Compare after ten complete traces."}},
        }),
    );
    let frozen = transaction(
        "freeze",
        "inquiry.freeze",
        json!({"inquiry_ref":draft,"question_ref":question}),
    );
    let plan =
        json!({"id": frozen["updated"][0]["id"], "revision": frozen["updated"][0]["revision"]});

    // After the freeze the same trace is renamed and re-extracted under a Run
    // bound to the plan.
    fs::write(root.join("renamed.json"), r#"{"stage":"three","ms":18.7} "#).unwrap();
    let renamed = run(
        root,
        &["ingest", "--input", "-", "--json"],
        Some(&json!({
            "schema_version":"inv.ingest.v1","command_id":"rename","actor":ACTOR,"case_id":case,
            "source":{"title":"renamed trace","format":"application/json"},"paths":["renamed.json"],
            "derived_from":{"inputs":[{"artifact_ref":{"id":original,"revision":1},"digest":digest}],
                "transformation":"example-org.rename","version":"1"},
        })),
        0,
    );
    let renamed_id = renamed["items"][0]["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let bound = record(
        "run",
        "run.record",
        json!({
            "intent":{"kind":"plan","plan_ref":plan},"producer":"agent:executor",
            "artifact_refs":[{"id":renamed_id,"revision":1}],"execution_status":"completed",
            "evidence_completeness":"complete","evidence_kind":"observational","variables_changed":[],
        }),
    );
    let reextracted = observe(
        "reextract",
        &renamed_id,
        &renamed["items"][0]["digest"],
        bound.clone(),
    );

    let receipt = transaction(
        "assess",
        "assessment.add",
        json!({
            "subject_ref":local,"evidence_refs":[reextracted],"relation":"inconclusive",
            "rationale":"The re-extracted trace repeats the earlier reading.",
            "frozen_basis":{
                "plan_ref":plan,
                "actual_inputs":[{"input_ref":reextracted,"partition":"Trace 3"}],
                "stop_rule_applied":"Stopped after one trace.",
                "deviations":[{"kind":"early_stop","description":"Stopped after one of ten traces.",
                    "reason":"The first look was favorable."}],
            },
        }),
    );
    let warnings: Vec<&str> = receipt["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.starts_with("assessment.evidence_reuse known_outcome_reuse")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.starts_with("assessment.deviation early_stop")),
        "{warnings:?}"
    );
    let assessment = receipt["created"][0]["id"].as_str().unwrap().to_owned();

    let shown = run(root, &["show", &assessment, "--json"], None, 0);
    let reuse = &shown["evidence_reuse"];
    assert_eq!(reuse["classification"], "known_outcome_reuse", "{reuse}");
    assert_eq!(reuse["counts_as_prospective"], false);
    assert_eq!(
        reuse["inputs"][0]["prior_exposures"][0]["record_ref"],
        inspected
    );
    assert_eq!(
        reuse["inputs"][0]["prior_exposures"][0]["via"],
        original.as_str()
    );
    assert_eq!(reuse["early_stop"], true);
    assert_eq!(reuse["deviations"][0]["kind"], "early_stop");
    assert_eq!(
        reuse["frozen_stop_rule"],
        "Compare after ten complete traces."
    );
    let runs = &reuse["chronology"]["runs"];
    assert_eq!(runs[0]["run_ref"]["id"], bound["id"]);
    assert_eq!(runs[0]["binds_assessed_plan"], true);
    assert_eq!(runs[0]["chronology"], "recorded_before_run_declaration");
    // The axes stay separate: the Run itself is unchanged by the reuse reading.
    let run_view = run(
        root,
        &[
            "show",
            bound["id"].as_str().unwrap(),
            "--depth",
            "full",
            "--json",
        ],
        None,
        0,
    );
    assert!(run_view.get("evidence_reuse").is_none());
    assert_eq!(shown["freshness"]["state"], "current");
}
