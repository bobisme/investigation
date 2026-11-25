//! bn-1lau (DF.08): in a workspace with several cases, a read given a record
//! ID reads the case that holds it; a prefix held by several cases is
//! refused with the retry. A brief level or show depth of the other command
//! is refused with the matching name, and a search for a kind or an ID says
//! what search matches and which read lists it.

use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::Value;

const ACTOR: &str = "agent:multi-case-test";

fn invoke(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn json(cwd: &Path, args: &[&str], exit: i32) -> Value {
    let args = [
        &[
            "--workspace",
            "casework",
            "--actor",
            ACTOR,
            "--format",
            "json",
        ],
        args,
    ]
    .concat();
    let output = invoke(cwd, &args);
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Run a repair argv: its first word is the program.
fn repair(cwd: &Path, failure: &Value) -> Output {
    let argv: Vec<&str> = failure["error"]["repair"]["argv"]
        .as_array()
        .unwrap_or_else(|| panic!("a repair argv: {failure}"))
        .iter()
        .map(|word| word.as_str().unwrap())
        .collect();
    assert_eq!(argv[0], "investigation");
    invoke(cwd, &argv[1..])
}

struct Case {
    id: String,
    artifact: String,
    source: String,
}

/// A started case on its own file, with distinct text.
fn start(cwd: &Path, name: &str) -> Case {
    fs::write(cwd.join(format!("{name}.txt")), format!("{name} line\n")).unwrap();
    let started = json(
        cwd,
        &[
            "start",
            &format!("What does {name} say?"),
            "--attach",
            &format!("./{name}.txt"),
            "--command-id",
            name,
        ],
        0,
    );
    let item = &started["items"][0];
    Case {
        id: started["created"][0]["id"].as_str().unwrap().to_owned(),
        artifact: item["artifact_id"].as_str().unwrap().to_owned(),
        source: item["source_id"].as_str().unwrap().to_owned(),
    }
}

fn two_cases() -> (tempfile::TempDir, Case, Case) {
    let scratch = tempfile::tempdir().unwrap();
    let output = invoke(
        scratch.path(),
        &[
            "init",
            "casework",
            "--actor",
            ACTOR,
            "--no-vault",
            "--import-root",
            ".",
            "--format",
            "json",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let first = start(scratch.path(), "alpha");
    let second = start(scratch.path(), "beta");
    (scratch, first, second)
}

fn common_prefix(left: &str, right: &str) -> String {
    left.chars()
        .zip(right.chars())
        .take_while(|(l, r)| l == r)
        .map(|(l, _)| l)
        .collect()
}

#[test]
fn a_read_given_a_record_id_reads_the_case_that_holds_it() {
    let (scratch, first, second) = two_cases();
    let cwd = scratch.path();
    for case in [&first, &second] {
        let name = if case.id == first.id { "alpha" } else { "beta" };
        let read = json(cwd, &["evidence", "read", &case.artifact, "--whole"], 0);
        assert_eq!(read["excerpt"]["text"], format!("{name} line\n"), "{read}");
        let shown = json(cwd, &["show", &case.source], 0);
        assert_eq!(shown["id"], case.source.as_str());
        let explained = json(cwd, &["explain", &case.artifact], 0);
        assert_eq!(explained["ok"], true, "{explained}");
        let case_shown = json(cwd, &["show", &case.id], 0);
        assert_eq!(case_shown["id"], case.id.as_str());
    }
    // An explicit --case still decides, and must hold the record.
    let elsewhere = json(
        cwd,
        &[
            "--case",
            &second.id,
            "evidence",
            "read",
            &first.artifact,
            "--whole",
        ],
        3,
    );
    assert_eq!(elsewhere["error"]["code"], "UNKNOWN_REFERENCE");

    // Without the index, the case is still found from the verified events.
    fs::remove_file(cwd.join("casework/cache/index.sqlite")).unwrap();
    let read = json(cwd, &["evidence", "read", &second.artifact, "--whole"], 0);
    assert_eq!(read["excerpt"]["text"], "beta line\n");
}

#[test]
fn a_prefix_held_by_several_cases_is_refused_with_the_retry() {
    let (scratch, first, second) = two_cases();
    let cwd = scratch.path();
    let prefix = common_prefix(&first.id, &second.id);
    assert!(prefix.len() > "case_".len(), "{prefix}");
    let refused = json(cwd, &["show", &prefix], 3);
    assert_eq!(refused["error"]["code"], "CASE_REQUIRED", "{refused}");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains("matches records in 2 cases"), "{message}");
    assert!(message.contains(&first.id) && message.contains(&second.id));
    assert!(message.contains("--case ID"), "{message}");
    let holders = refused["error"]["actual"].as_array().unwrap();
    assert_eq!(holders.len(), 2);
    // The retry names the first case and runs.
    let argv = refused["error"]["repair"]["argv"].as_array().unwrap();
    assert_eq!(argv[1], "--case");
    assert_eq!(argv[2], holders[0]);
    let retried = repair(cwd, &refused);
    assert!(retried.status.success(), "{retried:?}");
    let shown: Value = serde_json::from_slice(&retried.stdout).unwrap();
    assert_eq!(shown["id"], holders[0]);

    let artifacts = common_prefix(&first.artifact, &second.artifact);
    let refused = json(cwd, &["evidence", "read", &artifacts, "--whole"], 3);
    assert_eq!(refused["error"]["code"], "CASE_REQUIRED", "{refused}");

    // A typed ID that no case holds is unknown, not a case selection.
    let unknown = json(cwd, &["show", "art_ffffffff"], 3);
    assert_eq!(unknown["error"]["code"], "UNKNOWN_REFERENCE", "{unknown}");
    // Text that is not an ID selects as before.
    let untyped = json(cwd, &["show", "alpha"], 3);
    assert_eq!(untyped["error"]["code"], "CASE_REQUIRED");
}

#[test]
fn a_depth_name_of_the_other_command_is_refused_with_the_matching_one() {
    let (scratch, first, _) = two_cases();
    let cwd = scratch.path();
    let case = first.id.as_str();
    for (args, expected) in [
        (vec!["--case", case, "brief", "--level", "full"], "deep"),
        (
            vec!["--case", case, "brief", "--level=summary"],
            "--level=standard",
        ),
        (
            vec!["--case", case, "show", &first.source, "--depth", "deep"],
            "full",
        ),
    ] {
        let refused = json(cwd, &args, 2);
        assert_eq!(refused["error"]["code"], "INVALID_INPUT");
        let message = refused["error"]["message"].as_str().unwrap();
        for name in ["compact, standard or deep", "title, summary or full"] {
            assert!(message.contains(name), "{message}");
        }
        let argv = refused["error"]["repair"]["argv"].as_array().unwrap();
        assert!(argv.iter().any(|word| word == expected), "{refused}");
        let retried = repair(cwd, &refused);
        assert!(retried.status.success(), "{retried:?}");
    }
    // A name of neither command points at the help.
    let refused = json(cwd, &["--case", case, "brief", "--level", "huge"], 2);
    let argv = refused["error"]["repair"]["argv"].as_array().unwrap();
    assert_eq!(argv[1..3], ["brief", "--help"]);
    let help = invoke(cwd, &["brief", "--help"]);
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(
        help.contains("show --depth (title, summary, full)"),
        "{help}"
    );
    let help = invoke(cwd, &["show", "--help"]);
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(
        help.contains("brief --level (compact, standard, deep)"),
        "{help}"
    );
    assert!(
        help.contains("the case is the one that\nholds the ID"),
        "{help}"
    );
}

#[test]
fn a_search_for_a_kind_or_an_id_says_what_search_matches_and_which_read_lists_it() {
    let (scratch, first, _) = two_cases();
    let cwd = scratch.path();
    let case = first.id.as_str();
    let found = json(cwd, &["--case", case, "search", "observations"], 0);
    assert_eq!(found["counts"]["matching_records"], 0);
    let hint = &found["hint"];
    assert!(
        hint["message"]
            .as_str()
            .unwrap()
            .contains("not record kinds"),
        "{found}"
    );
    assert!(
        hint["message"]
            .as_str()
            .unwrap()
            .contains("frontier --kind observation")
    );
    let argv: Vec<&str> = hint["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|word| word.as_str().unwrap())
        .collect();
    assert!(argv.contains(&"brief"), "{argv:?}");
    let listed = invoke(cwd, &argv[1..]);
    assert!(listed.status.success(), "{listed:?}");

    let by_id = json(cwd, &["--case", case, "search", &first.source], 0);
    let argv: Vec<&str> = by_id["hint"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|word| word.as_str().unwrap())
        .collect();
    assert_eq!(
        argv[argv.len() - 4..],
        ["show", first.source.as_str(), "--format", "json"]
    );
    let shown = invoke(cwd, &argv[1..]);
    assert!(shown.status.success(), "{shown:?}");

    // Every case: the hint names no case, so it has no argv.
    let across = json(cwd, &["search", "hypotheses", "--all-cases"], 0);
    assert!(
        across["hint"]["message"]
            .as_str()
            .unwrap()
            .contains("hypothesis")
    );
    assert!(across["hint"].get("argv").is_none(), "{across}");
    // A search that finds records has no hint.
    let titled = json(cwd, &["--case", case, "search", "alpha"], 0);
    assert!(
        titled["counts"]["matching_records"].as_u64().unwrap() > 0,
        "{titled}"
    );
    assert!(titled.get("hint").is_none());

    let help = invoke(cwd, &["search", "--help"]);
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("Searches recorded text only"), "{help}");
    assert!(help.contains("frontier --kind KIND"), "{help}");
    assert!(help.contains("use brief"), "{help}");
}
