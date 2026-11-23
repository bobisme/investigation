//! `--dry-run` on writes (W17.05): `record`, `ingest`, `start` and the catalog
//! shortcuts validate against the current snapshot exactly as a commit does,
//! return the would-be receipt and the command that commits it, refuse with
//! the commit's own failure, run under `--read-only`, and write nothing: every
//! file and directory of the workspace, with its bytes, mode and modification
//! time, is the same after each dry run.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:dry-run-test";
const BIN: &str = env!("CARGO_BIN_EXE_investigation");

fn run(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> (i32, Value) {
    let mut command = Command::new(BIN);
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_SESSION")
        .env_remove("INVESTIGATION_READ_ONLY")
        .args(args)
        .arg("--json")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    if let Some(bytes) = stdin {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value = serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("{args:?}: {stdout}"));
    (output.status.code().unwrap(), value)
}

fn ok(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> Value {
    let (exit, value) = run(cwd, args, stdin);
    assert_eq!(exit, 0, "{args:?}: {value}");
    value
}

/// What one workspace entry looks like: kind, mode, modification time and,
/// for a file, its bytes.
type Entry = (bool, u32, (i64, i64), Option<Vec<u8>>);

/// Every file and directory beneath `root`. A directory's modification time
/// moves when an entry is created in it or removed from it, so a temporary
/// created and removed again shows here too.
fn tree(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let meta = fs::symlink_metadata(&directory).unwrap();
        found.insert(
            directory.strip_prefix(root).unwrap().to_path_buf(),
            (true, meta.mode(), (meta.mtime(), meta.mtime_nsec()), None),
        );
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                pending.push(path);
            } else {
                found.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    (
                        false,
                        meta.mode(),
                        (meta.mtime(), meta.mtime_nsec()),
                        Some(fs::read(&path).unwrap()),
                    ),
                );
            }
        }
    }
    found
}

fn client_refs(receipt: &Value) -> Vec<Value> {
    receipt["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["client_ref"].clone())
        .collect()
}

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    case: String,
    source: String,
}

impl Fixture {
    /// A workspace with a vault, one started case and the producer-run
    /// fixture directory as a second import root.
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        fs::write(
            root.join("results.log"),
            "request timed out after 28 seconds\n",
        )
        .unwrap();
        fs::write(root.join("other.log"), "a second log line\n").unwrap();
        ok(
            &root,
            &["init", "./ws", "--actor", ACTOR, "--import-root", "."],
            None,
        );
        let producer = producer_fixture();
        ok(
            &root,
            &["init", "./ws", "--import-root", producer.to_str().unwrap()],
            None,
        );
        let started = ok(
            &root,
            &[
                "--workspace",
                "./ws",
                "start",
                "Why did requests time out?",
                "--attach",
                "./results.log",
                "--command-id",
                "start-1",
            ],
            None,
        );
        let created = |prefix: &str| {
            started["created"]
                .as_array()
                .unwrap()
                .iter()
                .map(|change| change["id"].as_str().unwrap().to_owned())
                .find(|id| id.starts_with(prefix))
                .unwrap()
        };
        let (case, source) = (created("case_"), started["items"][0]["artifact_id"].clone());
        Self {
            _directory: directory,
            root,
            case,
            source: source.as_str().unwrap().to_owned(),
        }
    }

    fn workspace(&self) -> PathBuf {
        self.root.join("ws")
    }

    /// `--workspace ./ws --case CASE`, then `args`.
    fn args<'a>(&'a self, args: &[&'a str]) -> Vec<&'a str> {
        let mut all = vec!["--workspace", "./ws", "--case", self.case.as_str()];
        all.extend_from_slice(args);
        all
    }

    fn write(&self, name: &str, value: &Value) -> String {
        fs::write(self.root.join(name), serde_json::to_vec(value).unwrap()).unwrap();
        name.to_owned()
    }

    /// A valid one-note transaction, built by the shortcut's own envelope.
    fn note_transaction(&self, command_id: &str) -> Value {
        ok(
            &self.root,
            &self.args(&[
                "note",
                "Check the clock source",
                "--on",
                &self.case,
                "--command-id",
                command_id,
                "--print-envelope",
            ]),
            None,
        )
    }
}

fn producer_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/producer-run/v1")
        .canonicalize()
        .unwrap()
}

/// Run a dry run and prove it wrote nothing; return its response.
fn dry(fixture: &Fixture, args: &[&str], stdin: Option<&[u8]>, exit: i32) -> Value {
    let before = tree(&fixture.workspace());
    let (code, value) = run(&fixture.root, args, stdin);
    assert_eq!(code, exit, "{args:?}: {value}");
    assert_eq!(
        tree(&fixture.workspace()),
        before,
        "{args:?} changed the workspace"
    );
    value
}

/// The published `dry-run` wire schema, and the receipt schema the
/// response's other fields keep.
fn published(name: &str) -> jsonschema::Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas/v1")
        .join(name);
    let schema: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn assert_dry_response(value: &Value, would: &str, args: &[&str]) {
    assert!(
        published("dry-run.schema.json").is_valid(value),
        "{args:?}: {value}"
    );
    // With the commit's result back, a record's response is its receipt.
    if args.contains(&"record") {
        let mut receipt = value.clone();
        let object = receipt.as_object_mut().unwrap();
        object.insert("result".into(), value["would_result"].clone());
        object.remove("would_result");
        object.remove("next_write");
        assert!(
            published("receipt.schema.json").is_valid(&receipt),
            "{args:?}: {receipt}"
        );
    }
    assert_eq!(value["ok"], true, "{args:?}: {value}");
    assert_eq!(value["result"], "dry_run", "{args:?}: {value}");
    assert_eq!(value["would_result"], would, "{args:?}: {value}");
    assert_eq!(value["next"], json!([]));
    assert_eq!(value["projection"]["index"], "pending");
    assert!(
        value["warnings"][0]
            .as_str()
            .unwrap()
            .starts_with("Dry run: nothing was committed."),
        "{value}"
    );
    let argv: Vec<&str> = value["next_write"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap())
        .collect();
    let expected: Vec<&str> = std::iter::once("investigation")
        .chain(
            args.iter()
                .copied()
                .filter(|arg| *arg != "--dry-run" && *arg != "--read-only"),
        )
        .chain(["--json"])
        .collect();
    assert_eq!(argv, expected, "the commit command is this one, unchecked");
}

/// Arguments, stdin and the `would_result` expected.
type DryCase<'a> = (Vec<&'a str>, Option<&'a [u8]>, &'a str);

#[test]
fn every_dry_run_validates_reports_and_writes_nothing() {
    let fixture = Fixture::new();
    let case = fixture.case.clone();
    let tx = fixture.write("tx.json", &fixture.note_transaction("note-1"));
    let standalone = ok(
        &fixture.root,
        &[
            "--workspace",
            "./ws",
            "case",
            "new",
            "Another question",
            "--command-id",
            "case-2",
            "--print-envelope",
        ],
        None,
    );
    let standalone = fixture.write("case.json", &standalone);
    let manifest = ok(
        &fixture.root,
        &fixture.args(&[
            "source",
            "add",
            "./other.log",
            "--command-id",
            "src-2",
            "--print-envelope",
        ]),
        None,
    );
    let manifest = fixture.write("manifest.json", &manifest);
    let stdin_manifest = fixture.write(
        "stdin-manifest.json",
        &json!({
            "schema_version":"inv.ingest.v1","command_id":"src-3","actor":ACTOR,"case_id":case,
            "source":{"title":"Piped"},"content":{"kind":"stdin","name":"piped.log"},
            "extract_text":true
        }),
    );
    let envelope = producer_fixture().join("envelope.json");
    let envelope = envelope.to_str().unwrap();

    let source = fixture.source.clone();
    let standalone_bytes = fs::read(fixture.root.join(&standalone)).unwrap();
    let cases: Vec<DryCase<'_>> = vec![
        (
            fixture.args(&["record", "--input", &tx, "--dry-run"]),
            None,
            "committed",
        ),
        (
            vec!["--workspace", "./ws", "record", "--input", "-", "--dry-run"],
            Some(standalone_bytes.as_slice()),
            "committed",
        ),
        (
            vec![
                "--workspace",
                "./ws",
                "start",
                "A second question",
                "--attach",
                "./other.log",
                "--dry-run",
            ],
            None,
            "committed",
        ),
        (
            vec![
                "--workspace",
                "./ws",
                "start",
                "Piped question",
                "--attach",
                "-",
                "--dry-run",
            ],
            Some(b"piped bytes\n"),
            "committed",
        ),
        (
            fixture.args(&["ingest", "--input", &manifest, "--dry-run"]),
            None,
            "committed",
        ),
        (
            fixture.args(&["ingest", "--input", &stdin_manifest, "--dry-run"]),
            Some(b"streamed source bytes\n"),
            "committed",
        ),
        (
            fixture.args(&["ingest", "--input", envelope, "--dry-run"]),
            None,
            "committed",
        ),
        (
            fixture.args(&["note", "A note", "--on", &case, "--dry-run"]),
            None,
            "committed",
        ),
        (
            fixture.args(&[
                "observe",
                "The log reports 28 seconds",
                "--source",
                &source,
                "--quote",
                "timed out after 28 seconds",
                "--dry-run",
            ]),
            None,
            "committed",
        ),
        (
            vec!["--workspace", "./ws", "case", "new", "Third", "--dry-run"],
            None,
            "committed",
        ),
        (
            fixture.args(&["source", "add", "./other.log", "--dry-run"]),
            None,
            "committed",
        ),
        // A retry of a committed command ID: the commit would return the
        // earlier receipt.
        (
            vec![
                "--workspace",
                "./ws",
                "start",
                "Why did requests time out?",
                "--attach",
                "./results.log",
                "--command-id",
                "start-1",
                "--dry-run",
            ],
            None,
            "already_committed",
        ),
    ];
    for (args, stdin, would) in &cases {
        for read_only in [false, true] {
            let mut argv = if read_only {
                vec!["--read-only"]
            } else {
                vec![]
            };
            argv.extend(args.iter().copied());
            let value = dry(&fixture, &argv, *stdin, 0);
            assert_dry_response(&value, would, &argv);
        }
    }

    // The producer run's receipt fields come from the planned state.
    let (_, value) = run(
        &fixture.root,
        &fixture.args(&["ingest", "--input", envelope, "--dry-run"]),
        None,
    );
    assert_eq!(value["run_outcome"], "imported", "{value}");
    assert_eq!(value["artifacts"][0]["outcome"], "imported", "{value}");
    // start reports the items it would retain.
    let (_, value) = run(&fixture.root, &cases[2].0, None);
    assert_eq!(value["items"][0]["outcome"], "imported", "{value}");
    assert_eq!(value["items"][0]["text_status"], "decoded", "{value}");
}

#[test]
fn a_valid_dry_run_leaves_the_head_and_its_commit_command_commits_the_plan() {
    let fixture = Fixture::new();
    let tx = fixture.write("tx.json", &fixture.note_transaction("note-1"));
    let args = fixture.args(&["record", "--input", &tx, "--dry-run"]);
    let planned = dry(&fixture, &args, None, 0);
    let again = dry(&fixture, &args, None, 0);
    // The head did not move: a second dry run plans the same next sequence.
    assert_eq!(planned["snapshot"]["sequence"], 2, "{planned}");
    assert_eq!(again["snapshot"]["sequence"], 2);

    let argv: Vec<String> = planned["next_write"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap().to_owned())
        .collect();
    // `run` adds the --json the dry run was given.
    let argv: Vec<&str> = argv[1..argv.len() - 1].iter().map(String::as_str).collect();
    let before = tree(&fixture.workspace());
    let committed = ok(&fixture.root, &argv, None);
    // The control: the snapshot does see a commit.
    assert_ne!(tree(&fixture.workspace()), before);
    assert_eq!(committed["result"], "committed");
    assert_eq!(committed["snapshot"]["sequence"], 2);
    assert_eq!(client_refs(&committed), client_refs(&planned));
    assert_eq!(committed["command_id"], planned["command_id"]);
    // Committed now: a dry run says the commit would be a retry.
    let retry = dry(&fixture, &args, None, 0);
    assert_eq!(retry["would_result"], "already_committed");
    assert_eq!(retry["transaction_id"], committed["transaction_id"]);
}

#[test]
fn an_invalid_dry_run_fails_with_the_commits_code_path_and_exit() {
    let fixture = Fixture::new();
    let mut stale = fixture.note_transaction("stale-1");
    stale["preconditions"] = json!([{"id": fixture.case, "revision": 7}]);
    let mut unknown = fixture.note_transaction("unknown-1");
    unknown["operations"][0]["payload"]["subject_refs"][0]["id"] =
        json!("case_01a00000-0000-7000-8000-000000000000");
    let mut invalid = fixture.note_transaction("invalid-1");
    invalid["operations"][0]["payload"]
        .as_object_mut()
        .unwrap()
        .remove("content");
    let mut reused = fixture.note_transaction("start-1");
    reused["operations"][0]["payload"]["content"]["text"] = json!("A different request");
    let requests = [
        ("stale", stale),
        ("unknown", unknown),
        ("invalid", invalid),
        ("reused", reused),
    ];
    let mut codes = Vec::new();
    for (name, request) in &requests {
        let file = fixture.write(&format!("{name}.json"), request);
        let dry_args = fixture.args(&["record", "--input", &file, "--dry-run"]);
        let (dry_exit, dry_value) = run(&fixture.root, &dry_args, None);
        let before = tree(&fixture.workspace());
        let (exit, value) = run(
            &fixture.root,
            &fixture.args(&["record", "--input", &file]),
            None,
        );
        assert_eq!(
            tree(&fixture.workspace()),
            before,
            "{name}: a refusal wrote"
        );
        assert_ne!(exit, 0, "{name}: {value}");
        assert_eq!(dry_exit, exit, "{name}: {dry_value} vs {value}");
        assert_eq!(dry_value["ok"], false);
        for field in ["code", "path", "repair"] {
            assert_eq!(
                dry_value["error"][field], value["error"][field],
                "{name} {field}: {dry_value} vs {value}"
            );
        }
        assert_eq!(dry_value["commit_status"], value["commit_status"]);
        codes.push(value["error"]["code"].as_str().unwrap().to_owned());
    }
    // Four different refusals: the schema, a gate, the CAS and idempotency.
    codes.dedup();
    assert_eq!(codes.len(), 4, "{codes:?}");

    // A shortcut refuses the same way, and so does start with a bad file.
    for args in [
        fixture.args(&["note", "x", "--on", "case_zzz"]),
        vec![
            "--workspace",
            "./ws",
            "start",
            "Q",
            "--attach",
            "./missing.log",
        ],
    ] {
        let (exit, value) = run(&fixture.root, &args, None);
        let mut dry_args = args.clone();
        dry_args.push("--dry-run");
        let dry_value = dry(&fixture, &dry_args, None, exit);
        assert_eq!(dry_value["error"]["code"], value["error"]["code"]);
        assert_eq!(dry_value["error"]["path"], value["error"]["path"]);
    }
}

#[test]
fn read_only_refuses_the_write_but_runs_its_dry_run() {
    let fixture = Fixture::new();
    let tx = fixture.write("tx.json", &fixture.note_transaction("note-1"));
    let args = fixture.args(&["--read-only", "record", "--input", &tx]);
    let (exit, value) = run(&fixture.root, &args, None);
    assert_eq!(exit, 2);
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("`record` writes unless --dry-run is given"),
        "{value}"
    );
    let mut dry_args = args.clone();
    dry_args.push("--dry-run");
    assert_eq!(dry(&fixture, &dry_args, None, 0)["result"], "dry_run");
}

#[test]
fn help_shows_a_dry_run_example_and_the_response_is_published() {
    let directory = tempfile::tempdir().unwrap();
    let (_, help) = run(directory.path(), &["record", "--help"], None);
    let text = help["help"].as_str().unwrap();
    assert!(
        text.contains("investigation record --input transaction.json --dry-run --format json"),
        "{text}"
    );
    assert!(text.contains("--dry-run"));
    let schema = ok(directory.path(), &["schema", "dry-run"], None);
    assert_eq!(schema["schema"]["properties"]["result"]["const"], "dry_run");
    let list = ok(directory.path(), &["schema", "--list"], None);
    assert!(
        list["wire_schemas"]
            .as_array()
            .unwrap()
            .contains(&json!("dry-run"))
    );
}

#[test]
fn dry_run_and_print_envelope_are_exclusive() {
    let fixture = Fixture::new();
    let (exit, value) = run(
        &fixture.root,
        &fixture.args(&[
            "note",
            "x",
            "--on",
            &fixture.case,
            "--dry-run",
            "--print-envelope",
        ]),
        None,
    );
    assert_eq!(exit, 2, "{value}");
    assert_eq!(value["error"]["code"], "INVALID_INPUT");
}
