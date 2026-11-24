//! `export --kind producer-bundle` and its re-import through the public
//! binary (W11.10).
//!
//! - Scenarios A, C and D, each with producer runs added to one case (two
//!   envelopes that declare the same path with different bytes, one with a
//!   missing and a rejected artifact and byte-exact namespaced metadata,
//!   and a captured run with a missing declared artifact): the bundle
//!   validates against its published schema and every envelope against the
//!   producer-run schema, missing artifacts are listed as missing, the same
//!   snapshot gives byte-identical bundles, re-import into a fresh
//!   workspace passes `check` and gives the same facts, and re-import into
//!   the source workspace commits nothing, so it is never a new trial.
//! - Every byte of a bundle, file names included, is scanned for text above
//!   its audience and for the IDs of withheld records, for each audience.
//!
//! Every run clears the environment and puts an empty folder on `PATH`.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

#[path = "support/fixtures.rs"]
mod fixtures;

use fixtures::SCENARIOS;

const ACTOR: &str = "agent:bundle-test";
const BENCH: &[u8] = b"{\"metric\":\"latency.p99\",\"value\":18.7,\"unit\":\"ms\"}\n";
const BENCH_2: &[u8] = b"{\"metric\":\"latency.p99\",\"value\":21.4,\"unit\":\"ms\"}\n";
/// An envelope with namespaced metadata whose bytes (key order, spacing,
/// number spelling) a re-serialization would not keep.
const ENVELOPE_2: &str = r#"{"schema_version":"inv.producer-run.v1",
  "producer":{"name":"example-benchmark","version":"0.1.0"},
  "external_run_id":"bench-run-003",
  "intent":{"label":"repeat with the second setting"},
  "execution":{"state":"completed","exit_code":0,"started_at":"2026-11-02T10:00:00Z","clock_quality":"unverified"},
  "artifacts":[
    {"role":"original-output","path":"./bench-result.json","media_type":"application/json"},
    {"role":"log","path":"./absent.log"},
    {"role":"trace","path":"./trace.json","expected_sha256":"0000000000000000000000000000000000000000000000000000000000000000"}
  ],
  "assertions":[{"kind":"reported_metric","name":"latency.p99","value":21.40,"unit":"ms","metadata":{"example-org.rounding":{"digits":2}}}],
  "controls":[{"name":"steady-load","producer_result":"reported_pass"}],
  "receipts":[{"kind":"signed-statement","reference":"urn:example:statement:2"}],
  "limitations":["Single synthetic workload"],
  "metadata":{"example-org.instrument":{"zeta":1.50,"alpha":[3,"x"],"nested":{"b":true,"a":null}}}}
"#;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_investigation")
}

fn run_with(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> (i32, Value) {
    let empty_path = cwd.join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let mut child = Command::new(binary())
        .current_dir(cwd)
        .env_clear()
        .env("PATH", &empty_path)
        .args(args)
        .args(["--format", "json"])
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(bytes) = stdin {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value = serde_json::from_str(&stdout).unwrap_or_else(|_| {
        panic!(
            "{args:?}: {stdout} {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap(), value)
}

fn run(cwd: &Path, args: &[&str]) -> (i32, Value) {
    run_with(cwd, args, None)
}

/// Run against `workspace` as the test actor, and require success.
fn ok(cwd: &Path, workspace: &Path, args: &[&str]) -> Value {
    ok_with(cwd, workspace, args, None)
}

fn ok_with(cwd: &Path, workspace: &Path, args: &[&str], stdin: Option<&[u8]>) -> Value {
    let mut full = vec!["--workspace", workspace.to_str().unwrap(), "--actor", ACTOR];
    full.extend_from_slice(args);
    let (exit, value) = run_with(cwd, &full, stdin);
    assert_eq!(exit, 0, "{args:?}: {value}");
    value
}

/// Every file below `root` by relative path, with its bytes.
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            if path.is_dir() {
                walk(root, &path, found);
            } else {
                found.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, root, &mut found);
    found
}

fn heads(cwd: &Path, workspace: &Path) -> Value {
    let check = ok(cwd, workspace, &["check"]);
    assert_eq!(check["ok"], true, "{check}");
    check["heads"].clone()
}

fn first_case(cwd: &Path, workspace: &Path) -> String {
    heads(cwd, workspace)[0]["case_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn schema(cwd: &Path, name: &str) -> jsonschema::Validator {
    let (exit, schema) = run(cwd, &["schema", name, "--schema-only"]);
    assert_eq!(exit, 0, "{schema}");
    let published: Value = serde_json::from_slice(
        &fs::read(fixtures::repository().join(format!("schemas/v1/{name}.schema.json"))).unwrap(),
    )
    .unwrap();
    let mut live = schema.clone();
    live["$id"] = published["$id"].clone();
    assert_eq!(
        live, published,
        "regenerate with check-v1-schemas.py --write"
    );
    jsonschema::validator_for(&schema).unwrap()
}

fn assert_valid(validator: &jsonschema::Validator, value: &Value, what: &str) {
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|error| format!("{} at {}", error, error.instance_path))
        .collect();
    assert!(errors.is_empty(), "{what}: {errors:#?}");
}

/// Add producer runs to `case` of a workspace whose import root is its own
/// folder: two envelopes that declare the same path with different bytes,
/// and a captured run with one retained and one missing artifact.
fn add_producer_runs(cwd: &Path, workspace: &Path, case: &str) {
    let fixture = fixtures::repository().join("fixtures/producer-run/v1");
    fs::write(workspace.join("bench-result.json"), BENCH).unwrap();
    fs::copy(
        fixture.join("envelope.json"),
        workspace.join("envelope.json"),
    )
    .unwrap();
    let envelope = workspace.join("envelope.json");
    let first = ok(
        cwd,
        workspace,
        &[
            "--case",
            case,
            "ingest",
            "--input",
            envelope.to_str().unwrap(),
        ],
    );
    assert_eq!(first["run_outcome"], "imported", "{first}");
    fs::write(workspace.join("bench-result.json"), BENCH_2).unwrap();
    fs::write(workspace.join("trace.json"), b"{\"spans\":[]}\n").unwrap();
    fs::write(workspace.join("envelope-2.json"), ENVELOPE_2).unwrap();
    let envelope = workspace.join("envelope-2.json");
    let second = ok(
        cwd,
        workspace,
        &[
            "--case",
            case,
            "ingest",
            "--input",
            envelope.to_str().unwrap(),
        ],
    );
    assert_eq!(second["evidence_completeness"], "partial", "{second}");

    fs::write(workspace.join("captured.json"), b"{\"captured\":true}\n").unwrap();
    let begin = json!({
        "key": "cap-0001/begin", "actor": ACTOR,
        "producer": {"name": "example-producer", "version": "0.2.0"},
        "external_run_id": "cap-0001",
        "intent": {"label": "captured run"},
        "case": {"kind": "existing", "case_id": case},
        "metadata": {"example-org.fixture": {"seed": 7}}
    });
    let begun = ok_with(
        cwd,
        workspace,
        &["capture", "begin", "--input", "-"],
        Some(begin.to_string().as_bytes()),
    );
    let handle = begun["handle"].as_str().unwrap().to_owned();
    let path = workspace.join("captured.json");
    for (operation, request) in [
        (
            "append",
            json!({"handle": handle, "key": "started", "position": 1, "fact": {"kind": "started"}}),
        ),
        (
            "attach",
            json!({"handle": handle, "key": "result", "position": 2, "role": "original-output",
                   "source": {"kind": "file", "path": path}, "media_type": "application/json"}),
        ),
        (
            "finalize",
            json!({"handle": handle, "key": "cap-0001/finalize", "expected_last_position": 2,
                   "completion": "complete", "declared_artifacts": ["result", "summary"],
                   "execution": {"state": "completed", "exit_code": 0},
                   "assertions": [{"kind": "reported_metric", "name": "rows", "value": 3}],
                   "limitations": ["One shard only"]}),
        ),
    ] {
        ok_with(
            cwd,
            workspace,
            &["capture", operation, "--input", "-"],
            Some(request.to_string().as_bytes()),
        );
    }
}

fn export(cwd: &Path, workspace: &Path, case: &str, audience: &str, out: &Path) -> Value {
    let value = ok(
        cwd,
        workspace,
        &[
            "--case",
            case,
            "export",
            "--kind",
            "producer-bundle",
            "--audience",
            audience,
            "--out",
            out.to_str().unwrap(),
        ],
    );
    assert_eq!(value["kind"], "producer-bundle");
    value
}

fn index(bundle: &Path) -> Value {
    serde_json::from_slice(&fs::read(bundle.join("producer-bundle.json")).unwrap()).unwrap()
}

fn run_by<'v>(index: &'v Value, external: &str) -> &'v Value {
    index["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["external_run_id"] == external)
        .unwrap_or_else(|| panic!("no run {external}: {index}"))
}

/// What a re-import must keep of each re-importable run, without the IDs,
/// item names and origin a workspace assigns.
fn facts(index: &Value) -> BTreeMap<String, Value> {
    index["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|run| !run["reingest"].is_null())
        .map(|run| {
            let mut artifacts: Vec<Value> = run["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|artifact| {
                    let retention = match artifact["retention"].as_str().unwrap() {
                        // Rejected bytes were never retained, so a re-import
                        // finds nothing at the path.
                        "rejected" => "missing",
                        other => other,
                    };
                    json!({
                        "role": artifact["role"].as_str().unwrap_or("undeclared"),
                        "media_type": artifact["media_type"],
                        "retention": retention,
                        "digest": if retention == "retained" { artifact["digest"].clone() } else { Value::Null },
                        "size_bytes": if retention == "retained" { artifact["size_bytes"].clone() } else { Value::Null },
                    })
                })
                .collect();
            artifacts.sort_by_key(Value::to_string);
            let receipts: Vec<Value> = run["receipts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|receipt| json!({"kind": receipt["kind"], "reference": receipt["reference"]}))
                .collect();
            let key = format!(
                "{} {}",
                run["producer"]["name"].as_str().unwrap(),
                run["external_run_id"].as_str().unwrap()
            );
            let value = json!({
                "producer": run["producer"],
                "intent": run["intent"],
                "execution": run["execution"],
                "environment": run["environment"],
                "artifacts": artifacts,
                "assertions": run["assertions"],
                "controls": run["controls"],
                "receipts": receipts,
                "limitations": run["limitations"],
                "metadata": run["metadata"],
                "attribution": run["attribution"],
            });
            (key, value)
        })
        .collect()
}

#[test]
fn scenario_bundles_validate_and_round_trip_into_a_fresh_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let bundle_schema = schema(cwd, "producer-bundle");
    let envelope_schema = schema(cwd, "producer-run");
    for scenario in SCENARIOS {
        let root = cwd.join(scenario);
        let workspace = fixtures::workspace(scenario, &root.join("ws"), false);
        let case = first_case(cwd, &workspace);
        let (runs_before, reingestible_before) =
            index_runs(cwd, &workspace, &case, &root.join("before"));
        add_producer_runs(cwd, &workspace, &case);

        // Under exports/, which lies beneath the workspace's import root.
        let bundle = workspace.join("exports/bundle");
        let receipt = export(cwd, &workspace, &case, "internal", &bundle);
        let index = index(&bundle);
        assert_valid(&bundle_schema, &index, scenario);
        let runs = index["runs"].as_array().unwrap();
        assert_eq!(runs.len(), runs_before + 3, "{scenario}");
        for run in runs {
            match &run["reingest"] {
                Value::Null => assert_eq!(run["origin"], "recorded", "{run}"),
                reingest => {
                    let envelope: Value = serde_json::from_slice(
                        &fs::read(bundle.join(reingest["envelope"].as_str().unwrap())).unwrap(),
                    )
                    .unwrap();
                    assert_valid(&envelope_schema, &envelope, &run["folder"].to_string());
                }
            }
            for artifact in run["artifacts"].as_array().unwrap() {
                if let Some(file) = artifact["file"].as_str() {
                    let bytes = fs::read(bundle.join(file)).unwrap();
                    assert_eq!(
                        investigation_store::journal::hash_bytes(&bytes).to_string(),
                        artifact["digest"].as_str().unwrap()
                    );
                }
            }
            assert_eq!(run["attribution"], "producer_reported");
        }

        // Missing artifacts are listed as missing, rejected ones as rejected.
        let partial = run_by(&index, "bench-run-003");
        let retention: Vec<(&str, &str)> = partial["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|artifact| {
                (
                    artifact["role"].as_str().unwrap(),
                    artifact["retention"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            retention,
            [
                ("original-output", "retained"),
                ("log", "missing"),
                ("trace", "rejected")
            ]
        );
        assert_eq!(partial["evidence_completeness"], "partial");
        assert_eq!(partial["reingest"]["verbatim"], true);
        // The original envelope, byte for byte.
        assert_eq!(
            fs::read(bundle.join(partial["original"]["path"].as_str().unwrap())).unwrap(),
            ENVELOPE_2.as_bytes()
        );
        let captured = run_by(&index, "cap-0001");
        assert_eq!(captured["origin"], "capture");
        assert_eq!(captured["finalization"], "finalized");
        let summary = captured["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|artifact| artifact["item"] == "summary")
            .unwrap();
        assert_eq!(summary["retention"], "missing");
        assert!(summary["file"].is_null());
        assert_eq!(captured["metadata"]["example-org.fixture"]["seed"], 7);
        // Two runs declare the same path; each keeps its own bytes.
        let first = run_by(&index, "bench-run-002");
        assert_ne!(first["folder"], partial["folder"]);
        assert_eq!(
            fs::read(bundle.join(first["artifacts"][0]["file"].as_str().unwrap())).unwrap(),
            BENCH
        );
        assert_eq!(
            fs::read(bundle.join(partial["artifacts"][0]["file"].as_str().unwrap())).unwrap(),
            BENCH_2
        );

        // The same snapshot, kind and audience give the same bytes.
        let again = workspace.join("exports/again");
        let second = export(cwd, &workspace, &case, "internal", &again);
        assert_eq!(second["export_id"], receipt["export_id"]);
        assert_eq!(files(&bundle), files(&again), "{scenario}");
        let (exit, verified) = run(
            cwd,
            &[
                "archive",
                "verify",
                bundle.to_str().unwrap(),
                "--scratch",
                cwd.to_str().unwrap(),
            ],
        );
        assert_eq!(exit, 0, "{verified}");

        // Into a fresh workspace: every re-importable run, the same facts.
        let fresh = root.join("fresh");
        let (exit, init) = run(
            cwd,
            &[
                "init",
                fresh.to_str().unwrap(),
                "--no-vault",
                "--import-root",
                workspace.join("exports").to_str().unwrap(),
            ],
        );
        assert_eq!(exit, 0, "{init}");
        let imported = ok(
            cwd,
            &fresh,
            &[
                "ingest",
                "--input",
                bundle.join("producer-bundle.json").to_str().unwrap(),
            ],
        );
        assert_eq!(
            imported["runs"].as_array().unwrap().len(),
            reingestible_before + 3,
            "{imported}"
        );
        assert_eq!(
            imported["skipped"].as_array().unwrap().len(),
            runs_before - reingestible_before,
            "{imported}"
        );
        for run in imported["runs"].as_array().unwrap() {
            assert_eq!(run["run_outcome"], "imported", "{run}");
            assert_eq!(run["measurement_validity"], "unknown", "{run}");
        }
        let fresh_case = imported["case_id"].as_str().unwrap();
        assert_eq!(heads(cwd, &fresh).as_array().unwrap().len(), 1);
        let round = root.join("round");
        export(cwd, &fresh, fresh_case, "internal", &round);
        let round_index = self::index(&round);
        assert_valid(&bundle_schema, &round_index, "re-exported");
        assert_eq!(facts(&round_index), facts(&index), "{scenario}");
        assert_eq!(facts(&index).len(), reingestible_before + 3);
        // A re-imported verbatim envelope keeps its bytes.
        let reimported = run_by(&round_index, "bench-run-003");
        assert_eq!(
            fs::read(round.join(reimported["original"]["path"].as_str().unwrap())).unwrap(),
            ENVELOPE_2.as_bytes()
        );
        // Importing the bundle again adds nothing.
        let fresh_heads = heads(cwd, &fresh);
        let again = ok(
            cwd,
            &fresh,
            &[
                "ingest",
                "--input",
                bundle.join("producer-bundle.json").to_str().unwrap(),
            ],
        );
        for run in again["runs"].as_array().unwrap() {
            assert_eq!(run["run_outcome"], "already_recorded", "{run}");
        }
        assert_eq!(heads(cwd, &fresh), fresh_heads);

        // Into the source workspace: the same attempts, not new trials.
        let source_heads = heads(cwd, &workspace);
        let back = ok(
            cwd,
            &workspace,
            &[
                "ingest",
                "--input",
                bundle.join("producer-bundle.json").to_str().unwrap(),
            ],
        );
        let outcomes: BTreeMap<String, String> = back["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|run| {
                (
                    run["folder"].as_str().unwrap().to_owned(),
                    format!("{} {}", run["run_outcome"], run["result"]),
                )
            })
            .collect();
        for (folder, outcome) in &outcomes {
            assert!(
                outcome.contains("already_recorded") || outcome.contains("already_captured"),
                "{scenario} {folder}: {outcome}"
            );
        }
        assert_eq!(heads(cwd, &workspace), source_heads, "{scenario}");
    }
}

/// Runs the case records before producer runs are added, from a bundle, and
/// how many can be re-imported. A hand-recorded one has no producer
/// identity; one a fixture ingested (Scenario B) can be re-imported.
fn index_runs(cwd: &Path, workspace: &Path, case: &str, out: &Path) -> (usize, usize) {
    export(cwd, workspace, case, "restricted", out);
    let index = index(out);
    for run in index["runs"].as_array().unwrap() {
        if run["origin"] == "recorded" {
            assert_eq!(run["not_reingestible"], "no_producer_identity", "{run}");
        } else {
            assert!(run["reingest"].is_object(), "{run}");
        }
    }
    let runs = index["runs"].as_array().unwrap();
    let reingestible = runs
        .iter()
        .filter(|run| run["reingest"].is_object())
        .count();
    (runs.len(), reingestible)
}

const PUBLIC: &str = "PUFFINLINE";
const INTERNAL: &str = "OTTERNOTE";
const RESTRICTED: &str = "ZEBRACODE";

fn envelope(external: &str, level: &str, marker: &str, file: &str) -> Value {
    json!({
        "schema_version": "inv.producer-run.v1",
        "producer": {"name": "example-benchmark", "instance": format!("{marker}-instance")},
        "external_run_id": format!("{external}-{marker}"),
        "intent": {"label": format!("{marker} intent")},
        "execution": {"state": "completed", "exit_code": 0, "error": format!("{marker} error")},
        "environment": {"workload": format!("{marker}-workload")},
        "artifacts": [{"role": "original-output", "path": file, "classification": level,
                       "metadata": {"example-org.note": format!("{marker} artifact note")}}],
        "assertions": [{"kind": "reported_metric", "name": format!("{marker}.metric"), "value": 1}],
        "controls": [{"name": format!("{marker}-control"), "producer_result": "reported_pass"}],
        "limitations": [format!("{marker} limitation")],
        "classification": level,
        "metadata": {"example-org.instrument": {"label": format!("{marker} metadata")}}
    })
}

#[test]
fn every_byte_of_each_audience_bundle_is_free_of_withheld_material() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let inputs = cwd.join("inputs");
    fs::create_dir_all(&inputs).unwrap();
    let workspace = cwd.join("ws");
    let (exit, init) = run(
        cwd,
        &[
            "init",
            workspace.to_str().unwrap(),
            "--no-vault",
            "--import-root",
            inputs.to_str().unwrap(),
        ],
    );
    assert_eq!(exit, 0, "{init}");
    let mut case: Option<String> = None;
    let mut ingest = |name: &str, envelope: Value| -> Value {
        let path = inputs.join(name);
        fs::write(&path, envelope.to_string()).unwrap();
        let mut args = vec![];
        let case_owned = case.clone();
        if let Some(case) = &case_owned {
            args.extend(["--case", case.as_str()]);
        }
        args.extend(["ingest", "--input", path.to_str().unwrap()]);
        let receipt = ok(cwd, &workspace, &args);
        case.get_or_insert(receipt["case_id"].as_str().unwrap().to_owned());
        receipt
    };
    for (level, marker) in [
        ("public", PUBLIC),
        ("internal", INTERNAL),
        ("restricted", RESTRICTED),
    ] {
        let file = format!("{}.json", level);
        fs::write(
            inputs.join(&file),
            format!("{{\"secret\":\"{marker} bytes\"}}\n"),
        )
        .unwrap();
        ingest(
            &format!("{level}.envelope.json"),
            envelope("run", level, marker, &file),
        );
    }
    // A public run whose second artifact is raised to restricted afterwards:
    // the run stays public, the artifact does not.
    fs::write(
        inputs.join("shared.json"),
        format!("{{\"ok\":\"{PUBLIC} shared\"}}\n"),
    )
    .unwrap();
    fs::write(
        inputs.join("raised.json"),
        format!("{{\"secret\":\"{RESTRICTED} raised bytes\"}}\n"),
    )
    .unwrap();
    let mut mixed = envelope("mixed", "public", PUBLIC, "shared.json");
    mixed["artifacts"].as_array_mut().unwrap().push(json!({
        "role": "log", "path": "raised.json", "classification": "public",
        "metadata": {"example-org.note": format!("{RESTRICTED} declared note")}
    }));
    let receipt = ingest("mixed.envelope.json", mixed);
    let case = case.unwrap();
    let raised = receipt["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["item"] == "artifact:raised.json")
        .unwrap()["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let raised_digest = receipt["artifacts"][1]["digest"]
        .as_str()
        .unwrap()
        .to_owned();
    let shown = ok(
        cwd,
        &workspace,
        &["--case", &case, "show", &raised, "--depth", "full"],
    );
    let revision = shown["record"]["header"]["revision"].as_u64().unwrap();
    let transaction = json!({
        "schema_version": "inv.command.v1", "command_id": "raise-artifact", "case_id": case,
        "actor": ACTOR, "preconditions": [], "operations": [{"op": "classification.raise",
        "payload": {"subject_ref": {"id": raised, "revision": revision}, "level": "restricted",
                    "reason": format!("{RESTRICTED} raise reason")}}]
    });
    ok_with(
        cwd,
        &workspace,
        &["record", "--input", "-"],
        Some(transaction.to_string().as_bytes()),
    );

    let validator = schema(cwd, "producer-bundle");
    for (audience, above, shown_markers) in [
        ("public", vec![INTERNAL, RESTRICTED], vec![PUBLIC]),
        ("internal", vec![RESTRICTED], vec![PUBLIC, INTERNAL]),
        ("restricted", vec![], vec![PUBLIC, INTERNAL, RESTRICTED]),
    ] {
        let out = cwd.join(format!("bundle-{audience}"));
        let receipt = export(cwd, &workspace, &case, audience, &out);
        let bytes = files(&out);
        let index = index(&out);
        assert_valid(&validator, &index, audience);
        let mut withheld: Vec<String> = receipt["publisher_receipt"]["exclusions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|exclusion| exclusion["id"].as_str().unwrap().to_owned())
            .filter(|id| *id != case)
            .collect();
        if audience != "restricted" {
            withheld.push(raised.clone());
            withheld.push(raised_digest.clone());
            withheld.push(raised_digest.trim_start_matches("sha256:").to_owned());
            assert_eq!(receipt["completeness"], "incomplete");
        } else {
            assert_eq!(receipt["completeness"], "complete");
        }
        for needle in above.iter().map(|text| text.to_string()).chain(withheld) {
            let found: Vec<&String> = bytes
                .iter()
                .filter(|(path, content)| {
                    path.contains(&needle)
                        || content
                            .windows(needle.len())
                            .any(|window| window == needle.as_bytes())
                })
                .map(|(path, _)| path)
                .collect();
            assert!(found.is_empty(), "{audience}: {needle} in {found:?}");
        }
        // Not vacuous: what the audience may see is there.
        for marker in shown_markers {
            assert!(
                bytes.values().any(|content| content
                    .windows(marker.len())
                    .any(|w| w == marker.as_bytes())),
                "{audience}: {marker} missing"
            );
        }
        let runs = index["runs"].as_array().unwrap();
        let expected_runs = match audience {
            "public" => 2,
            "internal" => 3,
            _ => 4,
        };
        assert_eq!(runs.len(), expected_runs, "{audience}");
        let mixed = run_by(&index, &format!("mixed-{PUBLIC}"));
        if audience == "restricted" {
            assert_eq!(mixed["reingest"]["verbatim"], true);
        } else if audience == "public" {
            // Public IDs are remapped, which would rewrite UUIDs inside
            // envelopes and retained bytes: the bundle is review only, with
            // the index and its manifest and no envelope or byte member.
            assert_eq!(mixed["artifacts"].as_array().unwrap().len(), 1);
            assert!(mixed["original"].is_null() && mixed["reingest"].is_null());
            for run in runs {
                assert!(
                    ["identifiers_remapped", "no_producer_identity"]
                        .contains(&run["not_reingestible"].as_str().unwrap()),
                    "{run}"
                );
                for artifact in run["artifacts"].as_array().unwrap() {
                    assert!(artifact["file"].is_null(), "{artifact}");
                }
            }
            let mut members: Vec<&String> = bytes.keys().collect();
            members.sort();
            assert_eq!(members, ["export-manifest.json", "producer-bundle.json"]);
        } else {
            // The withheld artifact is left out, and the envelope is rebuilt
            // from what is shown rather than copied.
            assert_eq!(mixed["artifacts"].as_array().unwrap().len(), 1);
            assert_eq!(mixed["reingest"]["verbatim"], false);
            assert!(mixed["original"].is_null());
        }
    }
}

#[test]
fn a_bundle_outside_every_import_root_or_altered_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let workspace = fixtures::workspace("scenario-c", &cwd.join("ws"), false);
    let case = first_case(cwd, &workspace);
    add_producer_runs(cwd, &workspace, &case);
    let bundle = cwd.join("outside");
    export(cwd, &workspace, &case, "internal", &bundle);
    let fresh = cwd.join("fresh");
    let inputs = cwd.join("inputs");
    fs::create_dir_all(&inputs).unwrap();
    let (exit, _) = run(
        cwd,
        &[
            "init",
            fresh.to_str().unwrap(),
            "--no-vault",
            "--import-root",
            inputs.to_str().unwrap(),
        ],
    );
    assert_eq!(exit, 0);
    let index_path: PathBuf = bundle.join("producer-bundle.json");
    let (exit, value) = run(
        cwd,
        &[
            "--workspace",
            fresh.to_str().unwrap(),
            "--actor",
            ACTOR,
            "ingest",
            "--input",
            index_path.to_str().unwrap(),
        ],
    );
    assert_ne!(exit, 0);
    assert_eq!(value["error"]["code"], "INVALID_INPUT", "{value}");

    // Altered after export: refused before anything is read.
    let moved = inputs.join("bundle");
    fs::rename(&bundle, &moved).unwrap();
    let artifact = files(&moved)
        .into_keys()
        .find(|path| path.contains("/files/"))
        .unwrap();
    fs::write(moved.join(artifact), b"altered").unwrap();
    let (exit, value) = run(
        cwd,
        &[
            "--workspace",
            fresh.to_str().unwrap(),
            "--actor",
            ACTOR,
            "ingest",
            "--input",
            moved.join("producer-bundle.json").to_str().unwrap(),
        ],
    );
    assert_ne!(exit, 0);
    assert_eq!(value["error"]["code"], "INTEGRITY_FAILURE", "{value}");
    assert!(heads(cwd, &fresh).as_array().unwrap().is_empty());
}
