//! `demo PATH --scenario NAME` and the shipped tutorial path (plan §17.5).
//!
//! - Each scenario, generated twice from the same embedded fixture, gives the
//!   same canonical bytes (journal, retained blobs, workspace marker), the
//!   same vault and export bytes, and the same replayed state as read back
//!   through the disposable index (`brief`, `check`); machine-local lock,
//!   temporary and cache bytes are not compared.
//! - The §17.5 command path runs as written in the plan, with only `inv`
//!   replaced by the binary, and ends with a non-empty decision record.
//! - No tutorial step records anything: the journal is byte-identical before
//!   and after every step of every scenario's tutorial, so no step can
//!   validate or promote a result.
//! - `demo` refuses a non-empty PATH and a missing parent, and writes nothing.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_investigation");

fn run(cwd: &Path, args: &[String]) -> (i32, String, String) {
    let output = Command::new(BIN)
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_READ_ONLY")
        .env("TZ", "UTC")
        .args(args)
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn json(cwd: &Path, args: &[&str]) -> Value {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let (code, stdout, stderr) = run(cwd, &args);
    assert_eq!(code, 0, "{args:?}\n{stdout}\n{stderr}");
    serde_json::from_str(&stdout).unwrap()
}

/// Every file under `root/relative`, by path, skipping machine-local lock and
/// temporary files and the disposable cache.
fn files(root: &Path, relative: &str) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let name = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let skipped = name
                .split('/')
                .any(|part| matches!(part, "locks" | "tmp" | "cache") || part.ends_with(".lock"));
            if skipped {
                continue;
            }
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.insert(name, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, &root.join(relative), &mut out);
    out
}

fn same_files(
    scenario: &str,
    what: &str,
    a: &BTreeMap<String, Vec<u8>>,
    b: &BTreeMap<String, Vec<u8>>,
) {
    assert!(!a.is_empty(), "{scenario}: no {what} files");
    assert_eq!(
        a.keys().collect::<Vec<_>>(),
        b.keys().collect::<Vec<_>>(),
        "{scenario}: {what} file sets differ"
    );
    for (path, bytes) in a {
        assert!(bytes == &b[path], "{scenario}: {what} {path} differs");
    }
}

/// The JSON a read prints, with the workspace's own path taken out.
fn read(cwd: &Path, workspace: &Path, args: &[&str]) -> String {
    let mut full = vec!["--workspace", workspace.to_str().unwrap()];
    full.extend(args);
    json(cwd, &full)
        .to_string()
        .replace(workspace.to_str().unwrap(), "<WORKSPACE>")
}

fn demo(cwd: &Path, path: &str, scenario: &str) -> Value {
    let receipt = json(
        cwd,
        &["demo", path, "--scenario", scenario, "--format", "json"],
    );
    assert_eq!(receipt["result"], "demo_created", "{receipt}");
    receipt
}

/// The tutorial steps the demo printed, without the program name.
fn tutorial(receipt: &Value) -> Vec<Vec<String>> {
    receipt["tutorial"]
        .as_array()
        .unwrap()
        .iter()
        .map(|argv| {
            let argv: Vec<String> = serde_json::from_value(argv.clone()).unwrap();
            assert_eq!(argv[0], "investigation");
            argv[1..].to_vec()
        })
        .collect()
}

fn reproduces(scenario: &str) {
    let base = tempfile::tempdir().unwrap();
    let (one, two) = (base.path().join("one"), base.path().join("two"));
    fs::create_dir_all(&one).unwrap();
    fs::create_dir_all(&two).unwrap();
    let first = demo(&one, "./demo-case", scenario);
    let second = demo(&two, "./demo-case", scenario);
    let (a, b) = (one.join("demo-case"), two.join("demo-case"));
    assert_eq!(first["workspace_id"], second["workspace_id"]);
    assert_eq!(first["case_id"], second["case_id"]);
    assert!(first["transactions"].as_u64().unwrap() > 20, "{first}");

    // Canonical event bytes, retained digests and the demo marker.
    let canonical = files(&a, "state");
    same_files(scenario, "canonical", &canonical, &files(&b, "state"));
    assert!(canonical.keys().any(|path| path.contains("/events/")));
    assert!(
        canonical
            .keys()
            .any(|path| path.starts_with("state/blobs/"))
    );
    let marker: Value = serde_json::from_slice(&canonical["state/workspace.json"]).unwrap();
    assert_eq!(marker["demo"]["scenario"], scenario, "{marker}");
    assert_eq!(marker["schema_version"], "inv.workspace.v1");
    // The staged source files were removed again.
    let mut top: Vec<_> = fs::read_dir(&a)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    top.sort();
    assert_eq!(
        top,
        [
            "README.txt",
            "cache",
            "exports",
            "investigation.toml",
            "local",
            "state",
            "vault"
        ]
    );

    // The tutorial: every step succeeds, and none changes canonical state.
    let steps = tutorial(&first);
    assert_eq!(steps, tutorial(&second));
    for (cwd, workspace) in [(&one, &a), (&two, &b)] {
        for step in &steps {
            let (code, stdout, stderr) = run(cwd, step);
            assert_eq!(code, 0, "{scenario}: {step:?}\n{stdout}\n{stderr}");
            if step.contains(&"json".to_owned()) {
                let value: Value = serde_json::from_str(&stdout).unwrap();
                assert_ne!(value["ok"], false, "{scenario}: {step:?}\n{stdout}");
            }
        }
        same_files(
            scenario,
            "canonical after the tutorial",
            &canonical,
            &files(workspace, "state"),
        );
    }

    // Deterministic owned outputs: the vault and the export.
    same_files(scenario, "vault", &files(&a, "vault"), &files(&b, "vault"));
    let exports = files(&a, "exports");
    same_files(scenario, "export", &exports, &files(&b, "exports"));
    assert!(
        exports.values().any(|bytes| bytes.len() > 1000),
        "{scenario}: the export is empty"
    );

    // The disposable index, compared by the replayed state it serves.
    for args in [
        &["brief", "--format", "json"][..],
        &["check", "--format", "json"][..],
    ] {
        assert_eq!(
            read(&one, &a, args),
            read(&two, &b, args),
            "{scenario}: {args:?}"
        );
    }
}

#[test]
fn demo_debug_reproduces_its_bytes_and_tutorial() {
    reproduces("debug");
}

#[test]
fn demo_performance_reproduces_its_bytes_and_tutorial() {
    reproduces("performance");
}

#[test]
fn demo_research_reproduces_its_bytes_and_tutorial() {
    reproduces("research");
}

#[test]
fn demo_choice_reproduces_its_bytes_and_tutorial() {
    reproduces("choice");
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Plan §17.5 exactly as written, `inv` being the binary.
#[test]
fn plan_tutorial_path_runs_as_written_and_ends_with_a_decision_record() {
    let plan = fs::read_to_string(repository().join("notes/plan.md")).unwrap();
    let block = plan
        .split("### 17.5 Shipped tutorial command path")
        .nth(1)
        .and_then(|rest| rest.split("```sh").nth(1))
        .and_then(|rest| rest.split("```").next())
        .unwrap();
    let steps: Vec<Vec<String>> = block
        .lines()
        .filter_map(|line| line.strip_prefix("inv "))
        // A trailing shell comment (an erratum note) is not part of the command.
        .map(|line| {
            line.split(" #")
                .next()
                .unwrap_or(line)
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        })
        .collect();
    assert_eq!(steps.len(), 8, "{block}");
    assert_eq!(steps[0][..2], ["demo".to_owned(), "./demo-case".to_owned()]);
    let cwd = tempfile::tempdir().unwrap();
    let mut journal = None;
    for step in &steps {
        let (code, stdout, stderr) = run(cwd.path(), step);
        assert_eq!(code, 0, "{step:?}\n{stdout}\n{stderr}");
        let state = files(&cwd.path().join("demo-case"), "state");
        assert_eq!(
            *journal.get_or_insert_with(|| state.clone()),
            state,
            "{step:?} changed the journal"
        );
    }
    let exports = files(&cwd.path().join("demo-case"), "exports");
    let (_, record) = exports
        .iter()
        .find(|(path, _)| path.ends_with("/decision-record.md"))
        .expect("a decision record");
    let record = String::from_utf8(record.clone()).unwrap();
    assert!(
        record.starts_with("# Decision record: Queue architecture choice"),
        "{record}"
    );
    assert!(record.contains("## Decision 1: Chosen option:"), "{record}");
    assert!(record.contains("### Comparison matrix"), "{record}");
    // The decision that rests on the changed hard threshold is not presented
    // as accepted and current.
    assert!(
        record.contains("Effective status: `needs_review`"),
        "{record}"
    );
}

#[test]
fn demo_refuses_a_non_empty_path_or_a_missing_parent_and_writes_nothing() {
    let cwd = tempfile::tempdir().unwrap();
    let busy = cwd.path().join("busy");
    fs::create_dir(&busy).unwrap();
    fs::write(busy.join("notes.txt"), b"mine").unwrap();
    let refused = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let (code, stdout, _) = run(cwd.path(), &args);
        assert_ne!(code, 0, "{args:?}\n{stdout}");
        let value: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(value["commit_status"], "not_committed", "{value}");
        value
    };
    let value = refused(&["demo", "./busy", "--scenario", "choice", "--format", "json"]);
    assert_eq!(value["error"]["code"], "INVALID_INPUT", "{value}");
    assert_eq!(fs::read_dir(&busy).unwrap().count(), 1);
    let value = refused(&[
        "demo",
        "./missing/demo-case",
        "--scenario",
        "choice",
        "--format",
        "json",
    ]);
    assert_eq!(value["error"]["code"], "INVALID_INPUT", "{value}");
    assert!(!cwd.path().join("missing").exists());
    refused(&[
        "--read-only",
        "demo",
        "./fresh",
        "--scenario",
        "choice",
        "--format",
        "json",
    ]);
    assert!(!cwd.path().join("fresh").exists());
    refused(&[
        "demo",
        "./fresh",
        "--scenario",
        "nonsense",
        "--format",
        "json",
    ]);
    assert_eq!(fs::read_dir(cwd.path()).unwrap().count(), 1);
}
