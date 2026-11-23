//! `guide NAME`, `method list` and the single contextual card in `next`.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:guide-test";

const LOCAL_PACK: &str = r#"schema_version = "inv.method-pack.v1"
id = "local-notes"
version = "1.0.0"
title = "Local notes"
summary = "Optional guidance for one team."
modes = ["explain"]

[[cards]]
name = "weigh"
title = "Weigh the open account"
stage = "assess"
summary = "An open account has no recorded assessment."
triggers = [{ gap_rule = "hypothesis.no_assessment" }]

[cards.body]
when = "An account is open and nothing interprets evidence against it."
record = "An assessment that cites the exact observations."
skip = "The account was withdrawn."
"#;

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

fn submit(
    root: &Path,
    key: &str,
    case: Value,
    preconditions: Value,
    op: &str,
    payload: Value,
) -> Value {
    let input = json!({
        "schema_version": "inv.command.v1", "command_id": key, "case_id": case,
        "actor": ACTOR, "preconditions": preconditions,
        "operations": [{"op": op, "payload": payload}],
    });
    run(root, &["record", "--input", "-", "--json"], Some(&input), 0)
}

fn created(receipt: &Value) -> Value {
    json!({"id": receipt["created"][0]["id"], "revision": 1})
}

fn card_names(list: &Value) -> Vec<(String, String)> {
    list["stages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|stage| stage["cards"].as_array().unwrap())
        .map(|card| {
            (
                card["pack_id"].as_str().unwrap().to_owned(),
                card["name"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// The guide attached to the recommendation from `hypothesis.no_assessment`.
fn hypothesis_guide(root: &Path) -> (Value, Value) {
    let next = run(root, &["next", "--json"], None, 0);
    for recommendation in next["recommendations"].as_array().unwrap() {
        if let Some(guide) = recommendation.get("guide") {
            assert!(guide.is_object(), "one card, not a list: {guide}");
            assert_eq!(guide["content_trust"], "method_guidance");
        }
    }
    let guide = next["recommendations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["rule_id"] == "hypothesis.no_assessment")
        .expect("the open hypothesis is recommended")["guide"]
        .clone();
    (next, guide)
}

#[test]
fn guide_and_method_list_work_outside_a_workspace() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = temporary.path();
    let guide = run(cwd, &["guide", "blindspot", "--json"], None, 0);
    assert_eq!(guide["result"], "guide_shown");
    assert_eq!(guide["case_id"], Value::Null);
    assert_eq!(guide["guide"]["content_trust"], "method_guidance");
    assert_eq!(guide["guide"]["pack"]["pack_id"], "general");
    assert_eq!(guide["guide"]["pack"]["origin"], "built_in");
    assert!(guide["guide"]["pack"]["version"].is_string());
    assert!(
        guide["guide"]["pack"]["digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(guide["guide"]["card"]["name"], "blindspot");
    assert!(guide["guide"]["card"]["body"]["record"].is_string());

    let missing = run(cwd, &["guide", "blindspt", "--json"], None, 3);
    assert_eq!(missing["error"]["code"], "UNKNOWN_REFERENCE");
    assert_eq!(missing["error"]["actual"]["close_matches"][0], "blindspot");
    follow(cwd, &missing["error"]["repair"]["argv"], 0);

    let list = run(cwd, &["method", "list", "--json"], None, 0);
    assert_eq!(list["packs"].as_array().unwrap().len(), 1);
    let names = card_names(&list);
    assert_eq!(names.len(), 23);
    assert!(names.iter().all(|(pack, _)| pack == "general"));
    let handoff = list["stages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|stage| stage["cards"].as_array().unwrap())
        .find(|card| card["name"] == "handoff")
        .unwrap();
    assert_eq!(handoff["on_request_only"], true);
    assert_eq!(handoff["triggers"], "on request only");

    // A case selection needs a workspace; the built-in fallback is only for
    // the unselected, undiscovered case.
    let failure = run(
        cwd,
        &["--case", "case_0", "guide", "blindspot", "--json"],
        None,
        3,
    );
    assert_eq!(failure["error"]["code"], "WORKSPACE_REQUIRED");
}

#[test]
fn next_carries_one_card_and_only_pinned_packs_supply_guidance() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    run(root, &["init", ".", "--no-vault", "--json"], None, 0);
    let config = root.join("investigation.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    let case = submit(
        root,
        "case",
        Value::Null,
        json!([]),
        "case.create",
        json!({"title": "Guided case", "mode": "explain"}),
    );
    let case = case["created"][0]["id"].as_str().unwrap().to_owned();
    let question = submit(
        root,
        "question",
        json!(case),
        json!([]),
        "question.create",
        json!({"wording": "What explains the recorded difference?", "importance": "normal"}),
    );
    submit(
        root,
        "hypothesis",
        json!(case),
        json!([]),
        "hypothesis.create",
        json!({
            "question_ref": created(&question), "statement": "The first account.",
            "possible_falsifiers": [], "rival_refs": [], "assumption_refs": [],
        }),
    );

    let (next, guide) = hypothesis_guide(root);
    assert_eq!(next["guidance_rule_version"], "guidance.v1");
    assert!(next.get("unavailable_method_packs").is_none());
    assert_eq!(guide["card"], "discriminate");
    assert_eq!(guide["pack_id"], "general");
    assert_eq!(
        guide["matched_trigger"]["gap_rule"],
        "hypothesis.no_assessment"
    );
    let shown = follow(root, &guide["argv"], 0);
    assert_eq!(shown["guide"]["card"]["name"], "discriminate");
    assert_eq!(shown["case_id"], json!(case));
    assert_eq!(shown["guide"]["applies_to_case_mode"], true);

    // A pack file that is present and even retained, but not adopted, is
    // never guidance.
    fs::write(root.join("local-notes.toml"), LOCAL_PACK).unwrap();
    let manifest = json!({
        "schema_version": "inv.ingest.v1", "command_id": "retain-pack", "actor": ACTOR,
        "case_id": case, "source": {"title": "Local notes pack", "format": "application/toml"},
        "paths": ["local-notes.toml"]
    });
    let retained = run(
        root,
        &["ingest", "--input", "-", "--json"],
        Some(&manifest),
        0,
    );
    let artifact = retained["items"][0]["artifact_id"].clone();
    let digest = retained["items"][0]["digest"].as_str().unwrap().to_owned();
    let (_, guide) = hypothesis_guide(root);
    assert_eq!(
        (guide["card"].as_str(), guide["pack_id"].as_str()),
        (Some("discriminate"), Some("general"))
    );
    let missing = run(root, &["guide", "weigh", "--json"], None, 3);
    assert_eq!(missing["error"]["code"], "UNKNOWN_REFERENCE");
    run(
        root,
        &["guide", "weigh", "--pack", "local-notes", "--json"],
        None,
        3,
    );
    let list = run(root, &["method", "list", "--json"], None, 0);
    assert_eq!(list["packs"].as_array().unwrap().len(), 1);
    assert!(card_names(&list).iter().all(|(pack, _)| pack == "general"));

    // Once pinned, its card takes precedence for the same trigger.
    submit(
        root,
        "adopt",
        json!(case),
        json!([{"id": case, "revision": 1}]),
        "method_pack.adopt",
        json!({
            "artifact_ref": {"id": artifact, "revision": 1}, "pack_id": "local-notes",
            "version": "1.0.0", "digest": digest, "reason": "Use the team guidance.",
        }),
    );
    let (_, guide) = hypothesis_guide(root);
    assert_eq!(guide["card"], "weigh");
    assert_eq!(guide["pack_id"], "local-notes");
    assert_eq!(guide["origin"], "pinned");
    assert_eq!(guide["pack_digest"], json!(digest));
    let shown = follow(root, &guide["argv"], 0);
    assert_eq!(shown["guide"]["pack"]["digest"], json!(digest));
    let shown = run(root, &["guide", "weigh", "--json"], None, 0);
    assert_eq!(shown["guide"]["pack"]["pack_id"], "local-notes");
    let general = run(root, &["guide", "discriminate", "--json"], None, 0);
    assert_eq!(general["guide"]["pack"]["pack_id"], "general");
    let list = run(root, &["method", "list", "--json"], None, 0);
    assert_eq!(list["packs"][1]["pack_id"], "local-notes");
    assert!(card_names(&list).contains(&("local-notes".into(), "weigh".into())));
    let weigh = list["stages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|stage| stage["cards"].as_array().unwrap())
        .find(|card| card["name"] == "weigh")
        .unwrap();
    assert_eq!(weigh["triggered_now"], true);

    // Lost bytes make the pin unavailable: reported, never an error, and
    // never guidance.
    let hex = digest.strip_prefix("sha256:").unwrap();
    fs::remove_file(root.join("state/blobs/sha256").join(&hex[..2]).join(hex)).unwrap();
    let (next, guide) = hypothesis_guide(root);
    assert_eq!(guide["pack_id"], "general");
    assert_eq!(
        next["unavailable_method_packs"][0]["unavailable_reason"],
        "bytes_unavailable"
    );
    let list = run(root, &["method", "list", "--json"], None, 0);
    assert_eq!(list["unavailable_packs"][0]["pack_id"], "local-notes");
    assert!(card_names(&list).iter().all(|(pack, _)| pack == "general"));
    let refused = run(
        root,
        &["guide", "weigh", "--pack", "local-notes", "--json"],
        None,
        3,
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unavailable")
    );
    let text = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(root)
        .args(["next", "--format", "text"])
        .output()
        .unwrap();
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("Guide: discriminate (general)"), "{text}");
    assert!(
        text.contains("local-notes 1.0.0 is pinned but unavailable"),
        "{text}"
    );
}
