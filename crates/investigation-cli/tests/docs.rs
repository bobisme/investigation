//! Embedded docs through the public binary: the pages print offline from an
//! empty directory, `onboard` reports the same instructions version, and a
//! cold reader who runs the commands of the agent instructions in order, with
//! only the placeholders filled, gets a successful response from each one.

use std::{fs, path::Path, process::Command};

use serde_json::Value;

const ACTOR: &str = "agent:NAME";

fn investigation(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_READ_ONLY")
        .args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

fn json(cwd: &Path, args: &[&str]) -> (i32, Value) {
    let mut args = args.to_vec();
    args.extend(["--format", "json"]);
    let (code, stdout) = investigation(cwd, &args, &[]);
    (code, serde_json::from_str(&stdout).unwrap())
}

fn page_file(name: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/agent");
    fs::read_to_string(root.join(name)).unwrap()
}

#[test]
fn pages_print_offline_from_an_empty_directory_even_in_read_only_mode() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let (code, list) = json(cwd, &["docs"]);
    assert_eq!(code, 0, "{list}");
    let names: Vec<_> = list["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|page| page["page"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["agents", "skill"]);

    for (name, file) in [("agents", "agents.md"), ("skill", "skill.md")] {
        let expected = page_file(file);
        let (code, text) = investigation(cwd, &["docs", name], &[("INVESTIGATION_READ_ONLY", "1")]);
        assert_eq!(code, 0);
        assert_eq!(text, expected, "text output is the page byte for byte");
        let (code, page) = json(cwd, &["docs", name]);
        assert_eq!(code, 0);
        assert_eq!(page["content"], expected.as_str());
        assert_eq!(page["bytes"], expected.len());
        let version = page["version"].as_str().unwrap();
        assert!(expected.contains(&format!("<!-- {version} -->")));
    }
    assert_eq!(fs::read_dir(cwd).unwrap().count(), 0, "docs wrote nothing");

    let (code, unknown) = json(cwd, &["docs", "nope"]);
    assert_eq!(code, 3);
    assert_eq!(unknown["error"]["code"], "UNKNOWN_REFERENCE");
    assert_eq!(unknown["error"]["expected"]["pages"][0], "agents");
}

#[test]
fn onboard_reports_the_version_and_digest_of_the_agent_instructions() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let (_, agents) = json(cwd, &["docs", "agents"]);
    let (code, cold) = json(cwd, &["onboard"]);
    assert_eq!(code, 0);
    let (code, _) = json(cwd, &["init", "casework", "--no-vault"]);
    assert_eq!(code, 0);
    let (code, empty) = json(cwd, &["--workspace", "casework", "onboard"]);
    assert_eq!(code, 0);
    for onboard in [&cold, &empty] {
        let pointer = &onboard["agent_instructions"];
        assert_eq!(pointer["version"], agents["version"], "{onboard}");
        assert_eq!(pointer["digest"], agents["digest"]);
        assert_eq!(pointer["argv"], agents["argv"]);
    }
    let (_, text) = investigation(cwd, &["onboard"], &[]);
    assert!(text.contains(&format!(
        "Agent instructions ({})",
        agents["version"].as_str().unwrap()
    )));
}

/// Split a shell command line into words, honouring single quotes.
fn words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    for (index, part) in line.split('\'').enumerate() {
        if index % 2 == 1 {
            words.push(part.to_owned());
        } else {
            words.extend(part.split_whitespace().map(str::to_owned));
        }
    }
    words
}

/// The command lines of a page, in order: inline code spans that start with
/// the program name followed by a space.
fn commands(page: &str) -> Vec<Vec<String>> {
    page.split('`')
        .skip(1)
        .step_by(2)
        .map(|span| span.replace('\n', " "))
        .filter(|span| span.starts_with("investigation "))
        .map(|span| words(&span))
        .collect()
}

#[test]
fn a_cold_reader_can_run_every_command_of_the_agent_instructions_in_order() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    fs::write(cwd.join("FILE"), "request waited 28 seconds\n").unwrap();
    let (_, page) = investigation(cwd, &["docs", "agents"], &[]);

    let mut case = String::new();
    let mut source = String::new();
    let mut key = String::new();
    let mut ran = Vec::new();
    let mut skipped = Vec::new();
    for mut argv in commands(&page) {
        let shown = argv.join(" ");
        // Inputs a reader must write first; the schema commands cover them.
        if argv
            .iter()
            .any(|w| w == "--selector-file" || w == "--input")
        {
            skipped.push(shown);
            continue;
        }
        let after_init = cwd.join("casework").exists();
        if after_init && !argv.iter().any(|w| w == "--workspace") {
            argv.splice(1..1, ["--workspace".to_owned(), "./casework".to_owned()]);
        }
        for (index, word) in argv.clone().iter().enumerate() {
            let previous = index.checked_sub(1).map(|i| argv[i].as_str());
            argv[index] = match (previous, word.as_str()) {
                (_, "QUESTION") => "Why did requests time out?".to_owned(),
                (Some("--source"), "ID") => source.clone(),
                (_, "ID") => case.clone(),
                (_, "OP") => "observation.add".to_owned(),
                (_, "TEXT") => "waited 28 seconds".to_owned(),
                (_, "KEY") => key.clone(),
                _ => word.clone(),
            };
        }
        if !argv.iter().any(|w| w == "--format") {
            argv.extend(["--format".to_owned(), "json".to_owned()]);
        }
        let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
        let (code, stdout) = investigation(cwd, &args, &[("AGENT", ACTOR)]);
        assert_eq!(code, 0, "`{shown}` as {argv:?}: {stdout}");
        // Audit responses (inv.audit.v1) carry no `ok`; none may be a failure.
        let value: Value = serde_json::from_str(&stdout).unwrap();
        assert!(
            value["ok"] != false && value.get("error").is_none(),
            "`{shown}`: {value}"
        );
        if argv.iter().any(|w| w == "start") {
            case = value["created"][0]["id"].as_str().unwrap().to_owned();
            source = value["items"][0]["source_id"].as_str().unwrap().to_owned();
            key = value["command_id"].as_str().unwrap().to_owned();
            assert!(case.starts_with("case_") && source.starts_with("src_"));
        }
        ran.push(shown);
    }
    assert!(ran.len() >= 15, "{ran:#?}");
    assert_eq!(skipped.len(), 2, "{skipped:#?}");
    assert!(ran.iter().any(|c| c.contains(" observe ")), "{ran:#?}");
    assert!(ran.iter().any(|c| c.contains(" receipt KEY")), "{ran:#?}");
}
