//! One schema, four producer kinds: a benchmark, a file analysis, a simulation
//! and a trace capture are recorded through the same envelope and the same
//! capture lifecycle, each in a complete and an incomplete variant. The recorded
//! Runs differ only in data. Nothing in the crates names a kind.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:kinds-test";
const KINDS: [&str; 4] = ["benchmark", "file-analysis", "simulation", "trace-capture"];
const VARIANTS: [&str; 2] = ["envelope", "envelope-incomplete"];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/producer-kinds/v1")
        .canonicalize()
        .unwrap()
}

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .args(["--format", "json"])
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = command.spawn().unwrap();
    if let Some(value) = stdin {
        let mut pipe = process.stdin.take().unwrap();
        pipe.write_all(&serde_json::to_vec(value).unwrap()).unwrap();
    }
    let output = process.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(exit), "{stdout}");
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    serde_json::from_str(&stdout).expect(&stdout)
}

/// A fresh workspace that may read the four fixture directories and nothing else.
fn workspace(root: &Path) {
    run(root, &["init", "ws", "--no-vault"], None, 0);
    let roots: Vec<String> = KINDS
        .iter()
        .map(|kind| fixtures().join(kind).display().to_string())
        .collect();
    let config = root.join("ws/investigation.toml");
    let text = fs::read_to_string(&config)
        .unwrap()
        .replace("import_roots = []", &format!("import_roots = {roots:?}"));
    fs::write(config, text).unwrap();
}

fn envelope(kind: &str, variant: &str) -> Value {
    serde_json::from_slice(
        &fs::read(fixtures().join(kind).join(format!("{variant}.json"))).unwrap(),
    )
    .unwrap()
}

fn keys(value: &Value) -> BTreeSet<String> {
    value.as_object().unwrap().keys().cloned().collect()
}

/// Every key path in a value, array positions elided, so two records have the
/// same shape exactly when they have the same paths.
fn shape(value: &Value, prefix: &str, into: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = format!("{prefix}/{key}");
                into.insert(path.clone());
                shape(child, &path, into);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| shape(item, prefix, into)),
        _ => {}
    }
}

#[test]
fn four_producer_kinds_record_through_one_envelope_and_differ_only_in_data() {
    let mut receipt_keys: Vec<BTreeSet<String>> = Vec::new();
    let mut body_keys: Vec<BTreeSet<String>> = Vec::new();
    for kind in KINDS {
        for variant in VARIANTS {
            let source = envelope(kind, variant);
            // Each Run opens its own case in its own workspace.
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path();
            workspace(root);
            let receipt = run(
                root.join("ws").as_path(),
                &[
                    "ingest",
                    "--input",
                    fixtures()
                        .join(kind)
                        .join(format!("{variant}.json"))
                        .to_str()
                        .unwrap(),
                    "--actor",
                    ACTOR,
                ],
                None,
                0,
            );
            assert_eq!(receipt["result"], "committed", "{kind} {variant}");
            // Recording a producer's statements never validates a measurement.
            assert_eq!(receipt["measurement_validity"], "unknown");
            assert_eq!(receipt["execution"], source["execution"]["state"]);
            let declared = source["artifacts"].as_array().unwrap().len();
            assert_eq!(receipt["artifacts"].as_array().unwrap().len(), declared);
            let imported = receipt["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|artifact| artifact["outcome"] == "imported")
                .count();
            let complete = variant == "envelope";
            assert_eq!(imported == declared, complete, "{kind} {variant}");
            assert_eq!(
                receipt["evidence_completeness"],
                match (complete, imported) {
                    (true, _) => "complete",
                    (false, 0) => "missing",
                    (false, _) => "partial",
                },
                "{kind} {variant}"
            );
            if !complete {
                // The shortfall is named, never dropped.
                let outcomes: Vec<_> = receipt["artifacts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|artifact| artifact["outcome"] != "imported")
                    .map(|artifact| artifact["outcome"].clone())
                    .collect();
                assert!(!outcomes.is_empty(), "{kind}");
                assert!(!source["limitations"].as_array().unwrap().is_empty());
            }

            let shown = run(
                root.join("ws").as_path(),
                &[
                    "show",
                    receipt["run"]["id"].as_str().unwrap(),
                    "--depth",
                    "full",
                ],
                None,
                0,
            );
            let body = &shown["record"]["body"];
            assert_eq!(body["evidence_validity"], "unknown");
            let recorded = &body["producer_run"];
            assert_eq!(
                recorded["assertions"],
                source
                    .get("assertions")
                    .cloned()
                    .unwrap_or(json!([]))
                    .tap_default()
            );
            for assertion in recorded["assertions"].as_array().unwrap() {
                assert_eq!(assertion["validity_claim"], "producer_reported");
            }
            assert_eq!(recorded["limitations"], source["limitations"]);

            let mut set = BTreeSet::new();
            shape(&receipt, "", &mut set);
            receipt_keys.push(set);
            // Assertion values are data: compare the Run's shape beneath them.
            let mut body_shape = BTreeSet::new();
            let mut stripped = body.clone();
            for assertion in stripped["producer_run"]["assertions"]
                .as_array_mut()
                .into_iter()
                .flatten()
            {
                assertion["value"] = Value::Null;
            }
            shape(&stripped, "", &mut body_shape);
            body_keys.push(body_shape);
        }
    }

    // Outside the per-artifact outcome items, every receipt has one shape.
    let top = |set: &BTreeSet<String>| -> BTreeSet<String> {
        set.iter()
            .filter(|path| !path.starts_with("/artifacts/"))
            .cloned()
            .collect()
    };
    for set in &receipt_keys {
        assert_eq!(top(set), top(&receipt_keys[0]));
    }
    // The recorded Run has one shape as well, whatever the assertion values hold.
    for set in &body_keys {
        let kept = |set: &BTreeSet<String>| -> BTreeSet<String> {
            set.iter()
                .filter(|path| {
                    // A reported time expands to its parts only when one is sent.
                    !path.starts_with("/started_at/") && !path.starts_with("/finished_at/")
                })
                .filter(|path| {
                    // Optional lists, environment entries and the error text are
                    // data: a producer may send none.
                    ![
                        "artifacts",
                        "assertions",
                        "controls",
                        "environment/",
                        "execution/error",
                        "producer/version",
                    ]
                    .iter()
                    .any(|part| path.starts_with(&format!("/producer_run/{part}")))
                })
                .cloned()
                .collect()
        };
        assert_eq!(kept(set), kept(&body_keys[0]));
    }
}

trait TapDefault {
    fn tap_default(self) -> Value;
}

impl TapDefault for Value {
    /// Assertions are recorded with the one defaulted field made explicit.
    fn tap_default(mut self) -> Value {
        for assertion in self.as_array_mut().into_iter().flatten() {
            assertion["validity_claim"] = json!("producer_reported");
        }
        self
    }
}

/// Drive the capture lifecycle from an envelope. The driver reads only the
/// envelope's generic fields; it has no branch on the kind of producer.
fn capture(root: &Path, source: &Value, complete: bool) -> Value {
    let ws = root.join("ws");
    let begun = run(
        &ws,
        &["capture", "begin", "--input", "-"],
        Some(&json!({
            "key": format!("{}/begin", source["external_run_id"].as_str().unwrap()),
            "actor": ACTOR,
            "producer": source["producer"],
            "external_run_id": source["external_run_id"],
            "intent": {"label": source["intent"]["label"]},
            "case": {"kind": "new_record_case", "title": "Producer kinds"},
        })),
        0,
    );
    let handle = begun["handle"].clone();
    let mut position = 1;
    run(
        &ws,
        &["capture", "append", "--input", "-"],
        Some(
            &json!({"handle": handle, "key": "started", "position": position, "fact": {"kind": "started"}}),
        ),
        0,
    );
    let mut declared = Vec::new();
    for artifact in source["artifacts"].as_array().unwrap() {
        let path = artifact["path"].as_str().unwrap().trim_start_matches("./");
        let mut request = json!({
            "handle": handle, "key": path, "position": position + 1,
            "role": artifact["role"], "source": {"kind": "file", "path": path},
        });
        if let Some(media) = artifact.get("media_type") {
            request["media_type"] = media.clone();
        }
        if let Some(digest) = artifact["expected_sha256"].as_str() {
            request["expected_digest"] = json!(format!("sha256:{digest}"));
            // A mismatch is refused and consumes no position.
            run(
                &ws,
                &["capture", "attach", "--input", "-"],
                Some(&request),
                2,
            );
        } else {
            run(
                &ws,
                &["capture", "attach", "--input", "-"],
                Some(&request),
                0,
            );
            position += 1;
        }
        declared.push(json!(path));
    }
    let execution = &source["execution"];
    let mut report = json!({"state": execution["state"]});
    for field in ["exit_code", "error"] {
        if !execution[field].is_null() {
            report[field] = execution[field].clone();
        }
    }
    let mut summary = json!({
        "handle": handle,
        "key": format!("{}/finalize", source["external_run_id"].as_str().unwrap()),
        "expected_last_position": position,
        "completion": if complete { "complete" } else { "partial" },
        "execution": report,
        "declared_artifacts": declared,
    });
    for field in ["assertions", "controls", "limitations"] {
        if let Some(items) = source.get(field) {
            summary[field] = items.clone();
        }
    }
    run(
        &ws,
        &["capture", "finalize", "--input", "-"],
        Some(&summary),
        0,
    )
}

#[test]
fn the_same_capture_lifecycle_records_all_four_kinds_and_names_what_is_missing() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    workspace(root);
    let mut receipts = Vec::new();
    for kind in KINDS {
        for variant in VARIANTS {
            let source = envelope(kind, variant);
            let complete = variant == "envelope";
            let receipt = capture(root, &source, complete);
            assert_eq!(
                receipt["finalization"],
                if complete {
                    "finalized"
                } else {
                    "finalized_partial"
                },
                "{kind} {variant}"
            );
            assert_eq!(receipt["execution"], source["execution"]["state"]);
            assert_eq!(receipt["commit"]["result"], "committed");
            receipts.push(receipt);
        }
    }
    // The finalize receipt has one shape for every kind, complete or not.
    let first = keys(&receipts[0]);
    for receipt in &receipts {
        assert_eq!(keys(receipt), first);
    }
    // The workspace stays consistent after eight different runs.
    let check = run(root.join("ws").as_path(), &["check"], None, 0);
    assert_eq!(check["ok"], true);
}

#[test]
fn the_crates_hold_no_branch_on_a_fixture_producer_or_its_assertion_kinds() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut vocabulary: BTreeSet<String> = BTreeSet::new();
    for kind in KINDS {
        for variant in VARIANTS {
            let source = envelope(kind, variant);
            vocabulary.insert(source["producer"]["name"].as_str().unwrap().to_owned());
            vocabulary.insert(source["external_run_id"].as_str().unwrap().to_owned());
            for assertion in source["assertions"].as_array().into_iter().flatten() {
                let name = assertion["kind"].as_str().unwrap();
                // `reported_metric` is the documented example; the rest are
                // this fixture's own vocabulary.
                if name != "reported_metric" {
                    vocabulary.insert(name.to_owned());
                }
            }
        }
    }
    assert!(vocabulary.len() >= 12, "{vocabulary:?}");
    let mut stack = vec![
        crates.join("investigation-core/src"),
        crates.join("investigation-store/src"),
        crates.join("investigation-recording/src"),
    ];
    let mut scanned = 0;
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap().path();
            if entry.is_dir() {
                stack.push(entry);
            } else if entry.extension().is_some_and(|ext| ext == "rs") {
                let text = fs::read_to_string(&entry).unwrap();
                scanned += 1;
                for term in &vocabulary {
                    assert!(
                        !text.contains(term.as_str()),
                        "{} names fixture term {term}",
                        entry.display()
                    );
                }
            }
        }
    }
    assert!(scanned > 20, "{scanned}");
}
