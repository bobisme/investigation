//! `table_cell`, `time_window`, `code_span` and `external_selector` through
//! the public binary. Each round-trips with the validation the core computed.
//! Native tables and code blobs are checked against retained bytes; a time
//! window and an external selector are stored `unavailable`, shown so by
//! `show`, `explain` and `evidence read`, and an attributed `selector.attest`
//! changes the displayed state without revising the Observation.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:selector-test";
const TABLE: &str = "id,label,ms\r\nr1,\"warm, \"\"steady\"\"\",12\r\nr2,cold,48\r\n";
const CODE: &str = "fn pool() {\n    let limit = 32;\n}\n";

fn call(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
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
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{args:?}\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&stdout).expect(&stdout)
}

struct Case {
    _directory: tempfile::TempDir,
    root: std::path::PathBuf,
    case: String,
    keys: usize,
}

impl Case {
    fn submit(&mut self, op: &str, payload: Value, exit: i32) -> Value {
        self.keys += 1;
        let input = json!({
            "schema_version": "inv.command.v1", "command_id": format!("selectors-{}", self.keys),
            "case_id": self.case, "actor": ACTOR, "preconditions": [],
            "operations": [{"op": op, "payload": payload}],
        });
        call(
            &self.root,
            &["record", "--input", "-", "--json"],
            Some(&input),
            exit,
        )
    }

    fn show(&self, id: &str) -> Value {
        call(
            &self.root,
            &[
                "show", id, "--case", &self.case, "--depth", "full", "--json",
            ],
            None,
            0,
        )
    }

    fn read(&self, artifact: &str, selector: &Value, exit: i32) -> Value {
        call(
            &self.root,
            &[
                "evidence",
                "read",
                artifact,
                "--selector-file",
                "-",
                "--case",
                &self.case,
                "--json",
            ],
            Some(selector),
            exit,
        )
    }

    /// Record an Observation citing `selector` and return its ID.
    fn observe(&mut self, artifact: &str, selector: &Value, quote: Option<&str>) -> String {
        let value = match quote {
            Some(_) => json!({"kind": "text", "statement": "The cited span says so."}),
            None => json!({"kind": "measurement", "value": "12", "unit": "ms"}),
        };
        let receipt = self.submit(
            "observation.add",
            json!({
                "value": value,
                "basis": {"kind": "source", "artifact_ref": {"id": artifact, "revision": 1},
                    "selector": selector,
                    "text_quote": quote.map(|exact| json!({"exact": exact}))},
                "validity": "valid", "quality": [], "limitations": [],
                "evidence_kind": "observational", "variables_changed": [],
            }),
            0,
        );
        receipt["created"][0]["id"].as_str().unwrap().to_owned()
    }
}

fn request(digest: &str, selector: Value) -> Value {
    json!({"schema_version": "inv.selector.v1", "artifact_digest": digest, "selector": selector})
}

fn setup() -> (Case, Vec<(String, String)>) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    call(&root, &["init", ".", "--no-vault", "--json"], None, 0);
    let config = root.join("investigation.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    fs::write(root.join("requests.csv"), TABLE).unwrap();
    fs::write(root.join("pool.rs"), CODE).unwrap();
    fs::write(root.join("capture.bin"), [0u8, 1, 2, 3, 0xff, 0xfe]).unwrap();
    let manifest = json!({
        "schema_version": "inv.ingest.v1", "command_id": "selectors-ingest", "actor": ACTOR,
        "new_case": {"title": "Which pool limit applied?"},
        "source": {"title": "Captured files"},
        "paths": ["requests.csv", "pool.rs", "capture.bin"],
    });
    let receipt = call(
        &root,
        &["ingest", "--input", "-", "--json"],
        Some(&manifest),
        0,
    );
    let case = receipt["created"][0]["id"].as_str().unwrap().to_owned();
    let items = receipt["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["artifact_id"].as_str().unwrap().to_owned(),
                item["digest"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    (
        Case {
            _directory: directory,
            root,
            case,
            keys: 0,
        },
        items,
    )
}

fn stored_selector(case: &Case, observation: &str) -> Value {
    case.show(observation)["record"]["body"]["basis"]["selector"].clone()
}

#[test]
fn each_selector_round_trips_with_the_validation_the_core_computed() {
    let (mut case, items) = setup();
    let (table, table_digest) = &items[0];
    let (code, code_digest) = &items[1];
    let (capture, capture_digest) = &items[2];

    let csv_cell = request(
        table_digest,
        json!({"type": "table_cell", "table": "requests", "row_identity": "r1",
            "row_index": null, "column": "label", "extraction_version": "inv.table.csv.v1",
            "key_column": "id"}),
    );
    let producer_cell = request(
        table_digest,
        json!({"type": "table_cell", "table": "requests", "row_identity": null, "row_index": 1,
            "column": "ms", "extraction_version": "producer-sheet-export-3"}),
    );
    let code_span = request(
        code_digest,
        json!({"type": "code_span", "repository": "example/pool", "commit": "abc123",
            "blob_digest": code_digest, "path": "src/pool.rs", "start_line": 2, "end_line": 2}),
    );
    let unretained_blob = request(
        code_digest,
        json!({"type": "code_span", "repository": "example/pool", "commit": "abc123",
            "blob_digest": table_digest, "path": "src/other.rs", "start_line": 1, "end_line": 1}),
    );
    let window = request(
        capture_digest,
        json!({"type": "time_window", "start": "0.5", "end": "1.25", "unit": "s",
            "coordinate_origin": "capture-start", "channel": "latency"}),
    );
    let external = request(
        capture_digest,
        json!({"type": "external_selector", "namespace": "example.frames",
            "schema_uri": "urn:example:frame-index", "schema_version": "1",
            "value": {"frame": 3}, "validator": null}),
    );

    let cases = [
        (table, &csv_cell, Some("warm, \"\"steady\"\""), "verified"),
        (table, &producer_cell, None, "unavailable"),
        (code, &code_span, Some("let limit = 32;"), "verified"),
        (code, &unretained_blob, None, "unavailable"),
        (capture, &window, None, "unavailable"),
        (capture, &external, None, "unavailable"),
    ];
    for (artifact, selector, quote, expected) in cases {
        let observation = case.observe(artifact, selector, quote);
        let stored = stored_selector(&case, &observation);
        let mut accepted = selector.clone();
        accepted["validation"] = json!(expected);
        assert_eq!(stored, accepted, "{selector}");
        let shown = case.show(&observation);
        assert_eq!(shown["selector_validation"]["state"], expected, "{shown}");
        assert_eq!(shown["selector_validation"]["recorded"], expected);
        assert_eq!(shown["selector_validation"]["basis"], "core");
        let quote_state = &shown["record"]["body"]["basis"]["text_quote"]["state"];
        match quote {
            Some(_) => assert_eq!(quote_state, "verified", "{shown}"),
            None => assert_eq!(quote_state, "not_applicable"),
        }
    }

    // `evidence read` returns the exact cell and code bytes.
    let read = case.read(table, &csv_cell, 0);
    assert_eq!(read["excerpt"]["text"], "warm, \"\"steady\"\"");
    let read = case.read(code, &code_span, 0);
    assert_eq!(read["excerpt"]["text"], "    let limit = 32;\n");

    // What the core cannot check is refused with a repair, never shown.
    for selector in [&producer_cell, &unretained_blob, &window, &external] {
        let artifact = if selector == &producer_cell {
            table
        } else if selector == &unretained_blob {
            code
        } else {
            capture
        };
        let refused = case.read(artifact, selector, 5);
        assert_eq!(refused["error"]["code"], "GATE_FAILED", "{refused}");
        assert_eq!(refused["error"]["actual"]["validation"], "unavailable");
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("selector.attest"),
            "{refused}"
        );
        assert!(refused.get("excerpt").is_none());
    }

    // A cell that does not exist is refused at observation time.
    let missing = request(
        table_digest,
        json!({"type": "table_cell", "table": "requests", "row_index": 9,
            "column": "ms", "extraction_version": "inv.table.csv.v1"}),
    );
    case.submit(
        "observation.add",
        json!({
            "value": {"kind": "measurement", "value": "12", "unit": "ms"},
            "basis": {"kind": "source", "artifact_ref": {"id": table, "revision": 1},
                "selector": missing, "text_quote": null},
            "validity": "valid", "quality": [], "limitations": [],
            "evidence_kind": "observational", "variables_changed": [],
        }),
        2,
    );
    // A row identity without its key column cannot be checked.
    let unkeyed = request(
        table_digest,
        json!({"type": "table_cell", "table": "requests", "row_identity": "r2",
            "column": "ms", "extraction_version": "inv.table.csv.v1"}),
    );
    let observation = case.observe(table, &unkeyed, None);
    assert_eq!(
        stored_selector(&case, &observation)["validation"],
        "unavailable"
    );
}

#[test]
fn an_attestation_changes_the_displayed_state_without_revising_the_observation() {
    let (mut case, items) = setup();
    let (table, table_digest) = &items[0];
    let (capture, capture_digest) = &items[2];
    let external = request(
        capture_digest,
        json!({"type": "external_selector", "namespace": "example.frames",
            "schema_uri": "urn:example:frame-index", "schema_version": "1",
            "value": {"frame": 3}, "validator": "example-frame-reader"}),
    );
    let observation = case.observe(capture, &external, None);
    let before = case.show(&observation);
    assert_eq!(before["selector_validation"]["state"], "unavailable");
    assert_eq!(before["selector_validation"]["basis"], "core");
    let explained = call(
        &case.root,
        &["explain", &observation, "--case", &case.case, "--json"],
        None,
        0,
    );
    assert_eq!(
        explained["subject"]["selector_validation"]["state"], "unavailable",
        "{explained}"
    );

    // An attestation must name the exact selector the Observation records.
    let mut other = external.clone();
    other["selector"]["value"] = json!({"frame": 4});
    let attest = |selector: &Value, outcome: &str| {
        json!({
            "observation_ref": {"id": observation, "revision": 1},
            "selector": selector, "outcome": outcome,
            "validator": {"name": "example-frame-reader", "version": "2.1"},
            "rationale": "The reader decoded frame 3 from the cited bytes.",
        })
    };
    let refused = case.submit("selector.attest", attest(&other, "verified"), 5);
    assert_eq!(
        refused["error"]["path"], "/operations/0/payload/selector",
        "{refused}"
    );

    let receipt = case.submit("selector.attest", attest(&external, "verified"), 0);
    let annotation = receipt["created"][0]["id"].as_str().unwrap().to_owned();
    assert!(annotation.starts_with("ann_"), "{receipt}");
    assert!(receipt["updated"].as_array().unwrap().is_empty());

    let after = case.show(&observation);
    // The Observation is the same revision with the same recorded validation.
    assert_eq!(after["record"], before["record"]);
    let view = &after["selector_validation"];
    assert_eq!(view["state"], "verified", "{view}");
    assert_eq!(view["basis"], "attested");
    assert_eq!(view["recorded"], "unavailable");
    assert_eq!(view["attestations"][0]["attested_by"], ACTOR);
    assert_eq!(view["attestations"][0]["matches_declared_validator"], true);
    assert_eq!(view["disagreement"], false);
    let explained = call(
        &case.root,
        &["explain", &observation, "--case", &case.case, "--json"],
        None,
        0,
    );
    assert_eq!(
        explained["subject"]["selector_validation"]["basis"],
        "attested"
    );
    // The bytes are still not read through the external selector.
    case.read(capture, &external, 5);

    // A later attestation by the same attestor replaces its earlier outcome.
    case.submit("selector.attest", attest(&external, "not_found"), 0);
    let view = case.show(&observation)["selector_validation"].clone();
    assert_eq!(view["state"], "not_found");
    assert_eq!(view["attestations"].as_array().unwrap().len(), 1);
    assert_eq!(view["attestations_kept"], 2);

    // A selector the core verified itself cannot be attested over.
    let cell = request(
        table_digest,
        json!({"type": "table_cell", "table": "requests", "row_index": 1,
            "column": "ms", "extraction_version": "inv.table.csv.v1"}),
    );
    let verified = case.observe(table, &cell, None);
    let refused = case.submit(
        "selector.attest",
        json!({
            "observation_ref": {"id": verified, "revision": 1},
            "selector": cell, "outcome": "not_found",
            "validator": {"name": "another-reader", "version": "1"},
            "rationale": "Disagrees.",
        }),
        5,
    );
    assert_eq!(refused["error"]["actual"]["validation"], "verified");
}

/// Blank descriptor fields are refused for new writes only. A journal that
/// already holds such a selector, as accepted before the refusal existed,
/// still replays, reads, passes `check` and survives `repair index`; an
/// identical retry returns the recorded receipt.
#[test]
fn a_blank_descriptor_accepted_before_the_refusal_still_replays_and_reads() {
    use investigation_store::journal::{Event, hash_bytes};

    let (mut case, items) = setup();
    let (capture, capture_digest) = &items[2];
    let external = request(
        capture_digest,
        json!({"type": "external_selector", "namespace": "example.frames",
            "schema_uri": "urn:example:frame-index", "schema_version": "1",
            "value": {"frame": 3}, "validator": null}),
    );
    let observation = case.observe(capture, &external, None);

    // Rewrite the tail event as an older binary would have accepted it: a
    // blank namespace in the request and in the after-image, hashes intact.
    let events = case
        .root
        .join("state/cases")
        .join(&case.case)
        .join("events");
    let tail = fs::read_dir(&events)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .max()
        .unwrap();
    let mut event: Event = serde_json::from_slice(&fs::read(&tail).unwrap()).unwrap();
    assert_eq!(event.command_id, format!("selectors-{}", case.keys));
    let blank = |selector: &mut Value| {
        assert_eq!(selector["namespace"], "example.frames");
        selector["namespace"] = json!(" ");
    };
    blank(&mut event.request.input["operations"][0]["payload"]["basis"]["selector"]["selector"]);
    let record = event
        .accepted
        .records
        .iter_mut()
        .find(|record| record.header.id.to_string() == observation)
        .unwrap();
    blank(&mut record.body["basis"]["selector"]["selector"]);
    event.request_hash = hash_bytes(&serde_json::to_vec(&event.request).unwrap());
    // Events are written read-only; replace the file as a restore would.
    fs::remove_file(&tail).unwrap();
    fs::write(&tail, event.to_bytes().unwrap()).unwrap();

    call(&case.root, &["repair", "index", "--json"], None, 0);
    let check = call(&case.root, &["check", "--json"], None, 0);
    assert_eq!(check["ok"], true, "{check}");
    let shown = case.show(&observation);
    let stored = &shown["record"]["body"]["basis"]["selector"];
    assert_eq!(stored["selector"]["namespace"], " ");
    assert_eq!(stored["validation"], "unavailable");
    assert_eq!(shown["selector_validation"]["state"], "unavailable");
    call(
        &case.root,
        &["explain", &observation, "--case", &case.case, "--json"],
        None,
        0,
    );

    // The identical retry is answered from the journal, not re-validated.
    let retry = json!({
        "schema_version": "inv.command.v1", "command_id": event.command_id,
        "case_id": case.case, "actor": ACTOR, "preconditions": [],
        "operations": event.request.input["operations"],
    });
    let receipt = call(
        &case.root,
        &["record", "--input", "-", "--json"],
        Some(&retry),
        0,
    );
    assert_eq!(
        receipt["created"][0]["id"],
        observation.as_str(),
        "{receipt}"
    );

    // A new write with the same blank field is refused.
    let mut fresh = external.clone();
    fresh["selector"]["namespace"] = json!(" ");
    let refused = case.submit(
        "observation.add",
        json!({
            "value": {"kind": "measurement", "value": "12", "unit": "ms"},
            "basis": {"kind": "source", "artifact_ref": {"id": capture, "revision": 1},
                "selector": fresh, "text_quote": null},
            "validity": "valid", "quality": [], "limitations": [],
            "evidence_kind": "observational", "variables_changed": [],
        }),
        2,
    );
    assert_eq!(
        refused["error"]["message"], "OBSERVATION_SELECTOR_INVALID",
        "{refused}"
    );
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/payload/basis/selector"
    );
}
