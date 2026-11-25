//! Rival hypotheses recorded in one transaction (DF.03, bn-78gl).
//!
//! A `client_ref` names only an earlier operation of its transaction (plan
//! §9.3), so four rivals that each list the other three cannot be recorded
//! at once. The rivalry is stored once, on the hypothesis that names the
//! other, and counts in both directions. These tests hold the refusal to the
//! order rule and a repair that runs, the schema to documenting the rule and
//! showing two rivals created in order, and `show` to listing a
//! hypothesis's rivals in both directions.

use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:rival-test";
const ORDER_RULE: &str = "rival_refs may name only earlier operations; a rivalry recorded once counts in both directions";

/// Run the binary in `cwd`; returns the exit code and stdout.
fn invoke(cwd: &Path, args: &[&str], stdin: Option<&Value>) -> (i32, String) {
    let mut process = Command::new(env!("CARGO_BIN_EXE_investigation"))
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
        })
        .spawn()
        .unwrap();
    if let Some(stdin) = stdin {
        let mut pipe = process.stdin.take().unwrap();
        pipe.write_all(&serde_json::to_vec(stdin).unwrap()).unwrap();
    }
    let output = process.wait_with_output().unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

fn json(cwd: &Path, args: &[&str], stdin: Option<&Value>, code: i32) -> Value {
    let mut args = args.to_vec();
    args.extend(["--format", "json"]);
    let (status, stdout) = invoke(cwd, &args, stdin);
    assert_eq!(status, code, "{args:?}\n{stdout}");
    serde_json::from_str(&stdout).expect(&stdout)
}

/// A workspace with one case and one question; returns them.
fn casework(root: &Path) -> (String, Value) {
    json(
        root,
        &["init", "./ws", "--actor", ACTOR, "--no-vault"],
        None,
        0,
    );
    let ws = root.join("ws");
    let started = {
        let mut process = Command::new(env!("CARGO_BIN_EXE_investigation"))
            .current_dir(&ws)
            .env_remove("AGENT")
            .args([
                "--actor",
                ACTOR,
                "start",
                "Why do bursts stall?",
                "--attach",
                "-",
                "--format",
                "json",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        process
            .stdin
            .take()
            .unwrap()
            .write_all(b"burst 1 stalled 40 ms\n")
            .unwrap();
        let output = process.wait_with_output().unwrap();
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let case = started["created"][0]["id"].as_str().unwrap().to_owned();
    assert!(case.starts_with("case_"), "{started}");
    let question = submit(
        &ws,
        &case,
        "question",
        json!([{"op":"question.create",
            "payload":{"wording":"Why do bursts stall?", "importance":"normal"}}]),
        0,
    );
    let question = question["created"][0].clone();
    (
        case,
        json!({"id":question["id"], "revision":question["revision"]}),
    )
}

fn submit(ws: &Path, case: &str, key: &str, operations: Value, code: i32) -> Value {
    json(
        ws,
        &["record", "--input", "-"],
        Some(&json!({
            "schema_version":"inv.command.v1", "command_id":key, "case_id":case,
            "actor":ACTOR, "preconditions":[], "operations":operations
        })),
        code,
    )
}

fn hypothesis(alias: &str, question: &Value, rivals: &[&str]) -> Value {
    json!({"op":"hypothesis.create", "client_ref":alias, "payload":{
        "question_ref":question, "statement":format!("Explanation {alias}."),
        "applicability":null, "possible_falsifiers":[format!("{alias} is ruled out.")],
        "assumption_refs":[], "rival_refs":rivals
            .iter()
            .map(|rival| json!({"client_ref":rival}))
            .collect::<Vec<_>>()}})
}

/// The DF.03 transaction: four rivals, each listing the other three. It is
/// refused at the first forward alias with the order rule, and the repair
/// reads the schema of `hypothesis.create` instead of an unrelated record.
#[test]
fn a_forward_rival_alias_is_refused_with_the_order_rule_and_a_runnable_repair() {
    let directory = tempfile::tempdir().unwrap();
    let (case, question) = casework(directory.path());
    let ws = directory.path().join("ws");
    let names = ["a", "b", "c", "d"];
    let all: Vec<Value> = names
        .iter()
        .map(|name| {
            let rivals: Vec<&str> = names.iter().copied().filter(|n| n != name).collect();
            hypothesis(name, &question, &rivals)
        })
        .collect();
    let refused = submit(&ws, &case, "four-rivals", json!(all), 3);
    let error = &refused["error"];
    assert_eq!(error["code"], "UNKNOWN_REFERENCE", "{refused}");
    assert_eq!(error["path"], "/operations/0/payload/rival_refs/0");
    assert!(
        error["message"].as_str().unwrap().starts_with(ORDER_RULE),
        "{error}"
    );
    assert_eq!(error["repair"]["schema"], "hypothesis.create");
    let argv: Vec<&str> = error["repair"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|word| word.as_str().unwrap())
        .collect();
    assert_eq!(
        &argv[argv.len() - 4..],
        ["schema", "hypothesis.create", "--format", "json"],
        "{argv:?}"
    );
    let (code, stdout) = invoke(&ws, &argv[1..], None);
    assert_eq!(code, 0, "{stdout}");

    // An alias that no operation declares keeps the generic refusal.
    let unknown = submit(
        &ws,
        &case,
        "unknown-rival",
        json!([hypothesis("a", &question, &["nowhere"])]),
        3,
    );
    assert!(
        !unknown["error"]["message"]
            .as_str()
            .unwrap()
            .contains(ORDER_RULE),
        "{unknown}"
    );

    // In order, each naming only those before it, the same four commit.
    let ordered: Vec<Value> = names
        .iter()
        .enumerate()
        .map(|(index, name)| hypothesis(name, &question, &names[..index]))
        .collect();
    let committed = submit(&ws, &case, "four-rivals-in-order", json!(ordered), 0);
    assert_eq!(committed["created"].as_array().unwrap().len(), 4);
}

/// `schema hypothesis.create` documents the rule on rival_refs, and its
/// transaction example creates two rivals in order; filled with the case and
/// question, it commits.
#[test]
fn the_schema_documents_the_order_rule_and_its_example_creates_two_rivals_in_order() {
    let directory = tempfile::tempdir().unwrap();
    let (case, question) = casework(directory.path());
    let ws = directory.path().join("ws");
    let schema = json(&ws, &["schema", "hypothesis.create"], None, 0);
    let description = schema["payload_schema"]["properties"]["rival_refs"]["description"]
        .as_str()
        .unwrap_or_default()
        .replace('\n', " ");
    for phrase in [
        "client_ref of an earlier operation",
        "counts in both directions",
    ] {
        assert!(description.contains(phrase), "{description}");
    }
    let example = &schema["transaction_example"];
    assert!(
        example["guidance"]
            .as_str()
            .unwrap()
            .contains("create them in order"),
        "{example}"
    );
    let mut input = example["input"].clone();
    let operations = input["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 2, "{input}");
    let first = operations[0]["client_ref"].as_str().unwrap().to_owned();
    assert_eq!(operations[0]["payload"]["rival_refs"], json!([]));
    assert_eq!(
        operations[1]["payload"]["rival_refs"],
        json!([{"client_ref":first}])
    );
    input["case_id"] = json!(case);
    input["actor"] = json!(ACTOR);
    for operation in input["operations"].as_array_mut().unwrap() {
        operation["payload"]["question_ref"] = question.clone();
    }
    let committed = json(&ws, &["record", "--input", "-"], Some(&input), 0);
    assert_eq!(committed["created"].as_array().unwrap().len(), 2);
}

/// A rivalry recorded on the later hypothesis is listed by `show` on both,
/// with the side that recorded it, in JSON and in text.
#[test]
fn show_lists_a_hypothesis_rivals_in_both_directions() {
    let directory = tempfile::tempdir().unwrap();
    let (case, question) = casework(directory.path());
    let ws = directory.path().join("ws");
    let committed = submit(
        &ws,
        &case,
        "three-rivals",
        json!([
            hypothesis("a", &question, &[]),
            hypothesis("b", &question, &["a"]),
            hypothesis("c", &question, &["a", "b"]),
        ]),
        0,
    );
    let ids: Vec<String> = committed["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect();
    let rivals = |id: &str| -> Vec<(String, String)> {
        let shown = json(&ws, &["--case", &case, "show", id], None, 0);
        shown["rivals"]
            .as_array()
            .unwrap_or_else(|| panic!("no rivals: {shown}"))
            .iter()
            .map(|rival| {
                assert_eq!(rival["rival_ref"]["revision"], 1, "{rival}");
                (
                    rival["rival_ref"]["id"].as_str().unwrap().to_owned(),
                    rival["recorded_on"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let pair = |index: usize, side: &str| (ids[index].clone(), side.to_owned());
    // The first names nobody, and is still a rival of both later ones.
    assert_eq!(rivals(&ids[0]), [pair(1, "rival"), pair(2, "rival")]);
    assert_eq!(rivals(&ids[1]), [pair(0, "this"), pair(2, "rival")]);
    assert_eq!(rivals(&ids[2]), [pair(0, "this"), pair(1, "this")]);

    let (code, text) = invoke(&ws, &["--case", &case, "show", &ids[0]], None);
    assert_eq!(code, 0, "{text}");
    let line = text
        .lines()
        .find(|line| line.starts_with("Rivals, recorded once and counted both ways: "))
        .unwrap_or_else(|| panic!("{text}"));
    for id in &ids[1..] {
        assert!(line.contains(&format!("{id}@1 (names this one)")), "{line}");
    }
    // A full read states them once.
    let (_, full) = invoke(
        &ws,
        &["--case", &case, "show", &ids[0], "--depth", "full"],
        None,
    );
    assert_eq!(full.matches(&ids[1]).count(), 1, "{full}");
}
