//! `show` reports derivative lineage, shared origins and lost inspectability.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:lineage-test";

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
fn show_traces_derivatives_to_their_origin_and_reports_lost_inspectability() {
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
    fs::write(root.join("trace.json"), r#"{"host":"db-7","p99_ms":18.7}"#).unwrap();
    let start = run(
        root,
        &[
            "start",
            "Where does the latency come from?",
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
    let derive = |key: &str, input: &str, input_digest: &Value, output: &str| {
        fs::write(root.join(format!("{key}.txt")), output).unwrap();
        run(
            root,
            &["ingest", "--input", "-", "--json"],
            Some(&json!({
                "schema_version":"inv.ingest.v1","command_id":key,"actor":ACTOR,"case_id":case,
                "source":{"title":key,"format":"text/plain"},"paths":[format!("{key}.txt")],
                "derived_from":{"inputs":[{"artifact_ref":{"id":input,"revision":1},"digest":input_digest}],
                    "transformation":format!("example-org.{key}"),"version":"1"},
            })),
            0,
        )
    };
    let redacted = derive("redact", &original, &digest, "host [removed], p99 18.7 ms");
    assert_eq!(redacted["items"][0]["outcome"], "derived");
    let redacted_id = redacted["items"][0]["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let summary = derive(
        "summarize",
        &redacted_id,
        &redacted["items"][0]["digest"],
        "p99 18.7 ms",
    );
    let summary_id = summary["items"][0]["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let plot = derive("plot", &original, &digest, "plot of p99");
    let plot_id = plot["items"][0]["artifact_id"].as_str().unwrap().to_owned();

    let shown = run(root, &["show", &summary_id, "--json"], None, 0);
    let lineage = &shown["lineage"];
    assert_eq!(lineage["role"], "derivative");
    assert_eq!(lineage["depth"], 2);
    assert_eq!(
        lineage["origins"][0]["artifact_ref"]["id"],
        original.as_str()
    );
    assert_eq!(
        lineage["origins"][0]["source_ref"],
        start["items"][0]["source_id"]
            .as_str()
            .map(|id| json!({"id":id,"revision":1}))
            .unwrap()
    );
    assert_eq!(lineage["inspectability"]["state"], "inspectable");
    let sharing: Vec<&str> = lineage["sharing_origin"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reference| reference["id"].as_str().unwrap())
        .collect();
    assert!(sharing.contains(&plot_id.as_str()), "{lineage}");
    assert!(sharing.contains(&original.as_str()), "{lineage}");
    assert!(!sharing.contains(&summary_id.as_str()));
    let original_view = run(root, &["show", &original, "--json"], None, 0);
    assert_eq!(original_view["lineage"]["role"], "original");
    assert_eq!(original_view["lineage"]["depth"], 0);

    // An observation of the summary, and an assessment citing it.
    let record = |key: &str, op: &str, payload: Value| {
        run(
            root,
            &["record", "--input", "-", "--json"],
            Some(
                &json!({"schema_version":"inv.command.v1","command_id":key,"case_id":case,
                "actor":ACTOR,"preconditions":[],"operations":[{"op":op,"payload":payload}]}),
            ),
            0,
        )["created"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let observation = record(
        "observe",
        "observation.add",
        json!({
            "value":{"kind":"measurement","value":"18.7","unit":"ms"},
            "basis":{"kind":"source","artifact_ref":{"id":summary_id,"revision":1},
                "selector":{"schema_version":"inv.selector.v1","artifact_digest":summary["items"][0]["digest"],
                    "selector":{"type":"whole"}},"text_quote":null},
            "validity":"unknown","quality":[],"limitations":[],"evidence_kind":"observational","variables_changed":[]
        }),
    );
    let mut question = example_payload(root, "question.create");
    question["wording"] = json!("Where does the latency come from?");
    let question = record("question", "question.create", question);
    let mut hypothesis = example_payload(root, "hypothesis.create");
    hypothesis["question_ref"] = json!({"id":question,"revision":1});
    let hypothesis = record("hypothesis", "hypothesis.create", hypothesis);
    let mut assessment = example_payload(root, "assessment.add");
    assessment["subject_ref"] = json!({"id":hypothesis,"revision":1});
    assessment["evidence_refs"] = json!([{"id":observation,"revision":1}]);
    let assessment = record("assess", "assessment.add", assessment);
    let observed = run(root, &["show", &observation, "--json"], None, 0);
    assert_eq!(
        observed["lineage"]["origins"][0]["artifact_ref"]["id"],
        original.as_str()
    );
    let assessed = run(root, &["show", &assessment, "--json"], None, 0);
    assert_eq!(assessed["freshness"]["state"], "current");

    // Losing the original's bytes leaves the derivatives readable but no
    // longer re-inspectable against their origin.
    let hex = digest.as_str().unwrap().strip_prefix("sha256:").unwrap();
    fs::remove_file(root.join("state/blobs/sha256").join(&hex[..2]).join(hex)).unwrap();
    let shown = run(root, &["show", &summary_id, "--json"], None, 0);
    let inspectability = &shown["lineage"]["inspectability"];
    assert_eq!(inspectability["state"], "original_unavailable");
    assert_eq!(
        inspectability["unavailable_originals"][0]["id"],
        original.as_str()
    );
    assert_eq!(shown["freshness"]["state"], "current");
    // Records citing the derivative read through its lineage: the lost
    // original is in their read set, so they need review.
    let assessed = run(root, &["show", &assessment, "--json"], None, 0);
    assert_eq!(assessed["effective_status"]["state"], "needs_review");
    assert_eq!(
        assessed["material_caveat"]["unavailable_artifacts"][0]["id"],
        original.as_str()
    );
}
