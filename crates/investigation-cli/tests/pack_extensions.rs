//! Namespaced extension properties declared by a pinned method pack (W09.06)
//! through the public CLI: a third-party pack adds a card and typed optional
//! fields with no core change; values are schema-checked when written, shown
//! as labeled pack data, and survive removal of the pack file.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:extension-test";
const PACK: &str = include_str!("../../../fixtures/method-packs/bench-notes.toml");

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
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{args:?}\n{stdout}\n{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&stdout).expect(&stdout)
}

struct Case<'a> {
    root: &'a Path,
    id: String,
    attachment: Value,
}

impl Case<'_> {
    fn record(&self, key: &str, operation: Value, preconditions: Value, exit: i32) -> Value {
        let command = json!({
            "schema_version": "inv.command.v1", "command_id": key, "actor": ACTOR,
            "case_id": self.id, "preconditions": preconditions, "operations": [operation],
        });
        run(
            self.root,
            &["record", "--input", "-", "--json"],
            Some(&command),
            exit,
        )
    }

    fn show(&self, id: &str) -> Value {
        run(
            self.root,
            &["--case", &self.id, "show", id, "--depth", "full", "--json"],
            None,
            0,
        )
    }

    fn observe(&self, key: &str, extensions: Option<Value>, exit: i32) -> Value {
        let selector = json!({"schema_version":"inv.selector.v1","artifact_digest":self.attachment["digest"],
            "selector":{"type":"text_lines","start_line":1,"end_line":1,"encoding":"utf-8","parser_version":"inv.text.utf8.v1"}});
        let mut operation = json!({"op": "observation.add", "payload": {
            "value":{"kind":"text","statement":"The median reading is 12.5 ms."},
            "basis":{"kind":"source","artifact_ref":{"id":self.attachment["derivative_id"],"revision":1},
                "selector":selector,"text_quote":{"exact":"median 12.5 ms"}},
            "validity":"unknown","quality":[],"limitations":[],"evidence_kind":"observational","variables_changed":[]
        }});
        if let Some(extensions) = extensions {
            operation["extensions"] = extensions;
        }
        self.record(key, operation, json!([]), exit)
    }
}

fn start(root: &Path) -> Case<'_> {
    run(root, &["init", ".", "--no-vault", "--json"], None, 0);
    let config = root.join("investigation.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    fs::write(root.join("reading.txt"), "median 12.5 ms over 30 runs\n").unwrap();
    let started = run(
        root,
        &[
            "start",
            "Does warm-up change the reading?",
            "--attach",
            "reading.txt",
            "--command-id",
            "start",
            "--actor",
            ACTOR,
            "--json",
        ],
        None,
        0,
    );
    Case {
        root,
        id: started["created"][0]["id"].as_str().unwrap().to_owned(),
        attachment: started["items"][0].clone(),
    }
}

/// Retain and adopt the bench-notes pack; returns its digest.
fn adopt(case: &Case<'_>) -> String {
    fs::write(case.root.join("bench-notes.toml"), PACK).unwrap();
    let manifest = json!({
        "schema_version": "inv.ingest.v1", "command_id": "retain-pack", "actor": ACTOR,
        "case_id": case.id, "source": {"title": "Bench notes pack", "format": "application/toml"},
        "paths": ["bench-notes.toml"]
    });
    let retained = run(
        case.root,
        &["ingest", "--input", "-", "--json"],
        Some(&manifest),
        0,
    );
    let item = &retained["items"][0];
    let digest = item["digest"].as_str().unwrap().to_owned();
    let revision = case.show(&case.id)["basis_revision"].clone();
    case.record(
        "adopt",
        json!({"op": "method_pack.adopt", "payload": {
            "artifact_ref": {"id": item["artifact_id"], "revision": 1}, "pack_id": "bench-notes",
            "version": "1.0.0", "digest": digest, "reason": "Use the bench bookkeeping fields."
        }}),
        json!([{"id": case.id, "revision": revision}]),
        0,
    );
    digest
}

fn extensions() -> Value {
    json!({"bench-notes": {"warmup_runs": 5, "machine": {"cpu": "x86-64", "cores": 16, "clock_ghz": "3.20"}}})
}

#[test]
fn a_third_party_pack_adds_a_card_and_typed_fields_that_survive_its_removal() {
    let dir = tempfile::tempdir().unwrap();
    let case = start(dir.path());
    let digest = adopt(&case);

    // The pack's card is guidance for this case, read from the pinned bytes.
    let guide = run(
        case.root,
        &[
            "--case",
            &case.id,
            "guide",
            "warm-up",
            "--pack",
            "bench-notes",
            "--json",
        ],
        None,
        0,
    );
    assert_eq!(
        guide["guide"]["pack"]["pack_id"], "bench-notes",
        "{guide:#}"
    );

    // A declared value is accepted and shown as labeled, uninterpreted data.
    let accepted = case.observe("observe", Some(extensions()), 0);
    let observation = accepted["created"][0]["id"].as_str().unwrap().to_owned();
    let shown = case.show(&observation);
    assert_eq!(shown["record"]["header"]["extensions"], extensions());
    assert!(shown["record"]["body"].get("extensions").is_none());
    assert_eq!(
        shown["pack_fields"]["interpretation"],
        "pack_data_uninterpreted"
    );
    assert_eq!(
        shown["pack_fields"]["fields"],
        json!([
            {"pack_id": "bench-notes", "field": "machine",
             "value": {"cpu": "x86-64", "cores": 16, "clock_ghz": "3.20"}},
            {"pack_id": "bench-notes", "field": "warmup_runs", "value": 5},
        ])
    );
    // The journal keeps the values as part of the accepted after-image.
    let events = dir.path().join("state/cases").join(&case.id).join("events");
    let journaled = fs::read_dir(&events)
        .unwrap()
        .map(|entry| fs::read(entry.unwrap().path()).unwrap())
        .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .flat_map(|event| {
            event["accepted_operations"]["records"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .find(|record| record["header"]["id"] == observation)
        .expect("the observation is journaled");
    assert_eq!(journaled["header"]["extensions"], extensions());

    // An invalid value is refused with the JSON Pointer of the failing field.
    let mut invalid = extensions();
    invalid["bench-notes"]["machine"]["cores"] = json!("sixteen");
    let refused = case.observe("observe-invalid", Some(invalid), 2);
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/extensions/bench-notes/machine/cores"
    );
    // A pack the case has not adopted is refused with a repair naming adoption.
    let refused = case.observe(
        "observe-unadopted",
        Some(json!({"source-research": {"edition": "2nd"}})),
        3,
    );
    assert_eq!(refused["error"]["code"], "UNKNOWN_REFERENCE");
    assert_eq!(
        refused["error"]["path"],
        "/operations/0/extensions/source-research"
    );
    assert_eq!(refused["error"]["repair"]["schema"], "method_pack.adopt");

    // Remove the pack file from disk and rebuild the index from the journal:
    // the journal verifies and the record replays with its fields.
    fs::remove_file(case.root.join("bench-notes.toml")).unwrap();
    run(case.root, &["check", "--json"], None, 0);
    run(case.root, &["repair", "index", "--json"], None, 0);
    let shown = case.show(&observation);
    assert_eq!(shown["record"]["header"]["extensions"], extensions());
    assert_eq!(shown["pack_fields"]["fields"][1]["value"], 5);

    // Without the retained bytes too, reading and recording continue; only
    // new values cannot be checked against the pack's schema.
    let hex = digest.strip_prefix("sha256:").unwrap();
    fs::remove_file(
        case.root
            .join("state/blobs/sha256")
            .join(&hex[..2])
            .join(hex),
    )
    .unwrap();
    assert_eq!(
        case.show(&observation)["pack_fields"]["fields"][1]["value"],
        5
    );
    case.observe("observe-plain", None, 0);
    let refused = case.observe("observe-unavailable", Some(extensions()), 5);
    assert_eq!(refused["error"]["code"], "GATE_FAILED");
    assert_eq!(
        refused["error"]["actual"]["unavailable_reason"],
        "bytes_unavailable"
    );
    run(case.root, &["--case", &case.id, "next", "--json"], None, 0);
}
