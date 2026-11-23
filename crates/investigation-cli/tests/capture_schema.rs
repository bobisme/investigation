//! Capture discovery: the published schemas, the shipped examples and the
//! reported capabilities match what the binary does.

use std::{fs, path::Path, process::Command};

use serde_json::{Value, json};

fn run(cwd: &Path, args: &[&str], exit: i32) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .args(["--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert_eq!(output.status.code(), Some(exit), "{stdout}");
    serde_json::from_str(&stdout).expect(&stdout)
}

fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/capture/v1")
        .canonicalize()
        .unwrap()
}

fn fixture(name: &str) -> Value {
    serde_json::from_slice(&fs::read(fixtures().join(name)).unwrap()).unwrap()
}

fn valid(schema: &Value, instance: &Value) {
    let compiled = jsonschema::draft202012::new(schema).expect("schema compiles");
    let errors: Vec<_> = compiled
        .iter_errors(instance)
        .map(|error| format!("{} at {}", error, error.instance_path))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn overview_reports_the_real_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    run(root, &["init", "ws", "--no-vault"], 0);
    let overview = run(root, &["schema", "capture"], 0);
    let capabilities = &overview["capabilities"];
    assert_eq!(capabilities["protocols"], json!(["inv.capture.v1"]));
    assert_eq!(
        capabilities["operations"],
        json!([
            "capture.begin",
            "capture.append",
            "capture.attach",
            "capture.finalize",
            "capture.recover"
        ])
    );
    // The capabilities are the limits and protocol a real begin returns.
    let config = root.join("ws/investigation.toml");
    let text = fs::read_to_string(&config).unwrap().replace(
        "import_roots = []",
        &format!("import_roots = [{:?}]", fixtures().display().to_string()),
    );
    fs::write(config, text).unwrap();
    fs::write(root.join("begin.json"), fixture("begin.json").to_string()).unwrap();
    let begun = run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "begin",
            "--input",
            "begin.json",
        ],
        0,
    );
    assert_eq!(begun["limits"], capabilities["limits"]);
    assert_eq!(begun["protocol"], capabilities["default_protocol"]);
    valid(
        &run(root, &["schema", "capture", "--schema-only"], 0),
        capabilities,
    );
    valid(
        &run(
            root,
            &["schema", "capture.begin.receipt", "--schema-only"],
            0,
        ),
        &begun,
    );
}

#[test]
fn shipped_examples_run_through_the_cli_and_validate_against_their_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    run(root, &["init", "ws", "--no-vault"], 0);
    let config = root.join("ws/investigation.toml");
    let text = fs::read_to_string(&config).unwrap().replace(
        "import_roots = []",
        &format!("import_roots = [{:?}]", fixtures().display().to_string()),
    );
    fs::write(config, text).unwrap();

    let schema_of = |name: &str| run(root, &["schema", name, "--schema-only"], 0);
    let example_of = |name: &str| run(root, &["schema", name], 0)["example"].clone();
    let step = |operation: &str, request: &Value, handle: Option<&str>| {
        let mut request = request.clone();
        if let Some(handle) = handle {
            request["handle"] = json!(handle);
        }
        valid(&schema_of(operation), &request);
        fs::write(root.join("request.json"), request.to_string()).unwrap();
        let verb = operation.strip_prefix("capture.").unwrap();
        let receipt = run(
            root,
            &[
                "--workspace",
                "ws",
                "capture",
                verb,
                "--input",
                "request.json",
            ],
            0,
        );
        valid(&schema_of(&format!("{operation}.receipt")), &receipt);
        receipt
    };

    // The published example is the shipped fixture.
    for (operation, file) in [
        ("capture.begin", "begin.json"),
        ("capture.append", "append.json"),
        ("capture.attach", "attach.json"),
        ("capture.finalize", "finalize.json"),
        ("capture.recover", "recover.json"),
    ] {
        assert_eq!(example_of(operation), fixture(file), "{operation}");
    }

    let handle = step("capture.begin", &fixture("begin.json"), None)["handle"]
        .as_str()
        .unwrap()
        .to_owned();
    step("capture.append", &fixture("append.json"), Some(&handle));
    let attached = step("capture.attach", &fixture("attach.json"), Some(&handle));
    assert_eq!(attached["availability"], "staged");
    step("capture.finalize", &fixture("finalize.json"), Some(&handle));
    let mut scope = fixture("recover.json");
    scope["handle"] = json!(handle);
    valid(&schema_of("capture.recover"), &scope);
    let recovered = run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "recover",
            "--handle",
            &handle,
        ],
        0,
    );
    valid(&schema_of("capture.recover.receipt"), &recovered);
}

#[test]
fn an_unsupported_required_protocol_is_refused_with_the_supported_list() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    run(root, &["init", "ws", "--no-vault"], 0);
    let mut request = fixture("begin.json");
    request["protocol"] = json!("inv.capture.v2");
    fs::write(root.join("begin.json"), request.to_string()).unwrap();
    let refused = run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "begin",
            "--input",
            "begin.json",
        ],
        2,
    );
    assert_eq!(refused["error"]["path"], "/protocol");
    assert_eq!(refused["error"]["expected"], json!(["inv.capture.v1"]));
    assert_eq!(refused["commit_status"], "not_committed");
    // Nothing was declared, so the same key still begins with a supported version.
    request["protocol"] = json!("inv.capture.v1");
    fs::write(root.join("begin.json"), request.to_string()).unwrap();
    run(
        root,
        &[
            "--workspace",
            "ws",
            "capture",
            "begin",
            "--input",
            "begin.json",
        ],
        0,
    );
}

#[test]
fn schema_list_names_every_capture_schema_and_reserves_nothing_for_capture() {
    let dir = tempfile::tempdir().unwrap();
    let listing = run(dir.path(), &["schema", "--list", "--include-deferred"], 0);
    let wire: Vec<_> = listing["wire_schemas"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for name in ["capture", "capture.begin", "capture.recover.receipt"] {
        assert!(wire.contains(&name), "{name}");
    }
    assert!(
        !listing["reserved_wire_schemas"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "capture")
    );
}
