//! Absolute local paths never reach recorded identities, views or the vault.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use investigation_render::{VerifiedSnapshot, ViewPolicy, render};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const ACTOR: &str = "agent:paths-test";

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>) -> Value {
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
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{stdout}\n{:?}",
        output.stderr
    );
    serde_json::from_str(&stdout).expect(&stdout)
}

/// The JSON as text, without `argv` arrays: replay commands name the caller's
/// own `--workspace`, which is an argument they supplied, not a recorded label.
fn recorded_text(value: &Value) -> String {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.remove("argv");
                map.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut value = value.clone();
    strip(&mut value);
    value.to_string()
}

#[test]
fn ingest_by_absolute_path_records_no_local_path_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let prefix = root.to_string_lossy().into_owned();
    run(&root, &["init", ".", "--json"], None);
    let config = root.join("investigation.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    fs::write(root.join("seed.json"), r#"{"host":"db-7"}"#).unwrap();
    let start = run(
        &root,
        &[
            "start",
            "Where does the latency come from?",
            "--attach",
            "seed.json",
            "--command-id",
            "capture",
            "--actor",
            ACTOR,
            "--json",
        ],
        None,
    );
    let case = start["created"][0]["id"].as_str().unwrap().to_owned();
    fs::write(root.join("spec.txt"), "the spec").unwrap();
    fs::write(root.join("legacy.txt"), "older record").unwrap();
    let absolute = root.join("spec.txt");
    let receipt = run(
        &root,
        &["ingest", "--input", "-", "--json"],
        Some(&json!({
            "schema_version":"inv.ingest.v1","command_id":"abs","actor":ACTOR,"case_id":case,
            "source":{"title":"spec","format":"text/plain"},"paths":[absolute],
        })),
    );
    let source = receipt["items"][0]["source_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // A record written before labels were relativised: the identity carries
    // the absolute path verbatim. It is not rewritten, only never shown.
    let legacy = run(
        &root,
        &["ingest", "--input", "-", "--json"],
        Some(&json!({
            "schema_version":"inv.ingest.v1","command_id":"old","actor":ACTOR,"case_id":case,
            "source":{"title":"legacy","format":"text/plain",
                "identity":format!("legacy / {}", root.join("legacy.txt").display())},
            "paths":["legacy.txt"],
        })),
    );
    let legacy_source = legacy["items"][0]["source_id"].as_str().unwrap().to_owned();

    let shown = run(&root, &["show", &source, "--json"], None);
    assert!(shown.to_string().contains("spec / spec.txt"), "{shown}");
    for id in [&source, &legacy_source] {
        let text = recorded_text(&run(&root, &["show", id, "--json"], None));
        assert!(!text.contains(&prefix), "{text}");
    }
    let brief = recorded_text(&run(&root, &["brief", "--json"], None));
    assert!(!brief.contains(&prefix), "{brief}");

    let mut store = Store::open(Workspace::open(&root).unwrap());
    let snapshot = VerifiedSnapshot::read(&mut store).unwrap();
    let plan = render(&snapshot, &ViewPolicy::default()).unwrap();
    let mut elided = false;
    for entry in plan.entries() {
        let text = String::from_utf8_lossy(entry.content());
        assert!(
            !text.contains(&prefix),
            "{} leaks the temp dir",
            entry.path().as_str()
        );
        elided |= text.contains("local path elided");
    }
    assert!(
        elided,
        "the legacy absolute identity is marked, not dropped"
    );
}

#[test]
fn checked_in_goldens_and_fixtures_hold_no_local_path() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    let text =
        fs::read_to_string(repository.join("fixtures/development/cli-goldens.json")).unwrap();
    for needle in ["/tmp/", "/home/", "/Users/", "/var/folders/"] {
        assert!(!text.contains(needle), "goldens contain {needle}");
    }
}
