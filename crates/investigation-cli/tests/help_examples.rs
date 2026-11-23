//! The core examples that `--help` prints run as written in a fixture
//! workspace: init, start, record, brief, next and show. The lines are read
//! from the help text itself, so an example that stops working fails here.
//! The lint in `src/help_lint.rs` checks that every example parses.

use std::{fs, path::Path, process::Command};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_investigation");

fn run(cwd: &Path, args: &[String]) -> (i32, String) {
    let output = Command::new(BIN)
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

fn args(line: &str) -> Vec<String> {
    shell_words(line).into_iter().skip(1).collect()
}

/// Split a help example into words; the examples use only plain quoting.
fn shell_words(line: &str) -> Vec<String> {
    let (mut words, mut word, mut quote) = (Vec::new(), String::new(), None);
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            (None, c) => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// The example line in `help` that starts with `investigation <prefix>`.
fn example(help: &[String], prefix: &str) -> String {
    let (_, text) = run(Path::new("."), help);
    let start = format!("investigation {prefix}");
    text.lines()
        .map(str::trim)
        .map(|line| line.strip_prefix("Example:").map_or(line, str::trim))
        .find(|line| line.starts_with(&start))
        .unwrap_or_else(|| panic!("no example starting `{start}` in {help:?}"))
        .to_owned()
}

fn ok(cwd: &Path, line: &str) -> Value {
    let (code, stdout) = run(cwd, &args(line));
    assert_eq!(code, 0, "`{line}` failed: {stdout}");
    serde_json::from_str(&stdout).unwrap_or(Value::Null)
}

fn root_help() -> Vec<String> {
    vec!["--help".to_owned()]
}

#[test]
fn the_core_examples_in_help_run_in_a_fixture_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    fs::write(cwd.join("results.log"), "request waited 28 seconds\n").unwrap();

    // init, then start, exactly as the top-level help shows them.
    ok(cwd, &example(&root_help(), "init"));
    let started = ok(
        cwd,
        &example(&root_help(), "--workspace ./casework --actor"),
    );
    let case = started["created"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["client_ref"] == "case")
        .and_then(|item| item["id"].as_str())
        .unwrap()
        .to_owned();

    // brief and next from the top-level help.
    ok(cwd, &example(&root_help(), "--workspace ./casework brief"));
    ok(cwd, &example(&root_help(), "--workspace ./casework next"));

    // show: the example's placeholder is the case ID the start receipt gave.
    let show = example(&["show".to_owned(), "--help".to_owned()], "--workspace");
    let shown = ok(cwd, &show.replace("<ID>", &case));
    assert_eq!(shown["ok"], true, "{shown}");

    // record: the transaction example printed by `schema case.create`.
    let (_, schema) = run(cwd, &args("investigation schema case.create --format json"));
    let schema: Value = serde_json::from_str(&schema).unwrap();
    let mut transaction = schema["transaction_example"]["input"].clone();
    transaction["actor"] = "agent:investigator".into();
    fs::write(cwd.join("case.json"), transaction.to_string()).unwrap();
    let record = example(&root_help(), "--workspace ./casework record");
    let (code, stdout) = run(cwd, &args(&record));
    // A second case makes later reads ambiguous, so record runs last.
    assert_eq!(code, 0, "`{record}` failed: {stdout}");
}
