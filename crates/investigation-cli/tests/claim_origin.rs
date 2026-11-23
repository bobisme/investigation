//! `show` reports claim origins: acquisitions apart from known origins, and
//! `unknown` for every pair of origins nobody judged.

use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:origin-cli-test";

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

#[test]
fn show_reports_claim_origins_and_unknown_dependence() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    run(root, &["init", ".", "--no-vault", "--json"], None, 0);
    let schema = run(
        root,
        &["schema", "origin.judge_dependence", "--json"],
        None,
        0,
    );
    assert_eq!(
        schema["transaction_example"]["input"]["operations"][0]["op"],
        "origin.judge_dependence"
    );
    let mut sequence = 0;
    let mut record = |case: Option<&str>, op: &str, payload: Value| -> Value {
        sequence += 1;
        let receipt = run(
            root,
            &["record", "--input", "-", "--json"],
            Some(
                &json!({"schema_version":"inv.command.v1","command_id":format!("c{sequence}"),
                "case_id":case,"actor":ACTOR,"preconditions":[],
                "operations":[{"op":op,"payload":payload}]}),
            ),
            0,
        );
        receipt["created"]
            .get(0)
            .or(receipt["updated"].get(0))
            .cloned()
            .unwrap()
    };
    let case = record(
        None,
        "case.create",
        json!({"title":"Do the reports agree?","mode":"explore"}),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let case = Some(case.as_str());
    let mut observe = |statement: &str| {
        record(
            case,
            "observation.add",
            json!({
                "value":{"kind":"text","statement":statement},
                "basis":{"kind":"assertion","assertion":statement,"context":null},
                "validity":"unknown","quality":[],"limitations":[],"evidence_kind":"observational","variables_changed":[]
            }),
        )
    };
    let spec = observe("The specification states 64 frames.");
    let article = observe("The article states 64 frames.");
    let bench = observe("The article's bench test stalled above 48 frames.");
    let trace = |record: &mut dyn FnMut(Option<&str>, &str, Value) -> Value,
                 target: &Value,
                 origin: Value| {
        record(
            case,
            "origin.trace",
            json!({"observation_ref":target,"origin":origin,"rationale":"Compared the wording."}),
        )
    };
    let spec = trace(&mut record, &spec, json!({"kind":"original"}));
    trace(
        &mut record,
        &article,
        json!({"kind":"restates","observation_ref":spec}),
    );
    let bench = trace(&mut record, &bench, json!({"kind":"original"}));

    let shown = run(
        root,
        &["show", article["id"].as_str().unwrap(), "--json"],
        None,
        0,
    );
    let origin = &shown["claim_origin"];
    assert_eq!(origin["origin"]["state"], "known");
    assert_eq!(origin["origin"]["root"]["id"], spec["id"]);
    assert_eq!(origin["traces"][0]["assessor"], ACTOR);

    let question = record(
        case,
        "question.create",
        json!({"wording":"What is the frame window?","importance":"normal"}),
    );
    let hypothesis = record(
        case,
        "hypothesis.create",
        json!({"question_ref":question,"statement":"The window is 64 frames.",
            "possible_falsifiers":["A correction changes the value."],"rival_refs":[],"assumption_refs":[]}),
    );
    let assessment = record(
        case,
        "assessment.add",
        json!({"subject_ref":hypothesis,"evidence_refs":[spec,{"id":article["id"],"revision":2},bench],
            "relation":"inconclusive","rationale":"Two reports restate one value; one reports a test."}),
    );
    let shown = run(
        root,
        &["show", assessment["id"].as_str().unwrap(), "--json"],
        None,
        0,
    );
    let origins = &shown["evidence_origins"][0];
    assert_eq!(origins["field"], "evidence_refs");
    let summary = &origins["summary"];
    assert_eq!(summary["observations"], 3);
    assert_eq!(summary["acquisition_count"], 0);
    assert_eq!(summary["known_origin_count"], 2);
    assert_eq!(summary["pairs"][0]["dependence"], "unknown");
    assert_eq!(summary["independent_pairs"], 0);
    assert_eq!(shown["freshness"]["state"], "current");
}
