//! `exposure`: the secret-exposure diagnosis lists every local file that
//! holds the exposed text, never prints the text, writes nothing, and keeps
//! restricted retained bytes closed unless asked (plan §16.5, W11.12).

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:exposure-test";
/// Punctuation makes the Markdown and JSON forms differ from the raw bytes.
const SECRET: &str = "sk_live.4f9Q-zz71\"x";

struct Run {
    stdout: String,
    exit: i32,
}

fn invoke(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> Run {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_SESSION")
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = command.spawn().unwrap();
    if let Some(bytes) = stdin {
        process.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    let output = process.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    for form in forms() {
        assert!(!stdout.contains(&form), "stdout repeats the text: {stdout}");
        assert!(!stderr.contains(&form), "stderr repeats the text: {stderr}");
    }
    Run {
        stdout,
        exit: output.status.code().unwrap(),
    }
}

fn json_run(cwd: &Path, args: &[&str], stdin: Option<&[u8]>, exit: i32) -> Value {
    let mut argv = args.to_vec();
    argv.extend(["--format", "json"]);
    let run = invoke(cwd, &argv, stdin);
    assert_eq!(run.exit, exit, "{}", run.stdout);
    serde_json::from_str(&run.stdout).expect(&run.stdout)
}

/// The forms the tool can write the secret in, derived here independently:
/// raw, JSON string escaping and Markdown prose escaping.
fn forms() -> Vec<String> {
    let json = serde_json::to_string(SECRET).unwrap();
    let markdown: String = SECRET
        .chars()
        .flat_map(|c| {
            if c.is_ascii_punctuation() {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect();
    vec![
        SECRET.to_owned(),
        json[1..json.len() - 1].to_owned(),
        markdown,
    ]
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Every file below `root`, by relative path, with its bytes.
fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(root.join(dir)).unwrap() {
            let entry = entry.unwrap();
            let relative = dir.join(entry.file_name());
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                walk(root, &relative, out);
            } else if kind.is_file() {
                out.insert(relative.clone(), fs::read(root.join(&relative)).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, Path::new(""), &mut out);
    out
}

/// Ground truth: every workspace file whose bytes hold a form of the secret.
fn holding(workspace: &Path) -> BTreeSet<String> {
    let forms = forms();
    files(workspace)
        .into_iter()
        .filter(|(_, bytes)| forms.iter().any(|form| contains(bytes, form.as_bytes())))
        .map(|(path, _)| path.to_string_lossy().into_owned())
        .collect()
}

fn located(report: &Value, evidence: &str) -> BTreeMap<String, String> {
    report["locations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|location| location["evidence"] == evidence)
        .map(|location| {
            (
                location["path"].as_str().unwrap().to_owned(),
                location["area"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    case: String,
    source: String,
    observation: String,
    observe_command: String,
}

impl Fixture {
    fn workspace(&self) -> PathBuf {
        self.root.join("ws")
    }
}

/// A workspace where the secret reached a retained file, an observation's
/// statement and quote, the index, the rendered vault and capture staging.
fn fixture() -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().to_owned();
    fs::create_dir(root.join("inputs")).unwrap();
    fs::write(
        root.join("inputs/service.log"),
        format!("connect ok\nAuthorization: {SECRET}\nretry 3\n"),
    )
    .unwrap();
    fs::write(root.join("secret.txt"), format!("{SECRET}\n")).unwrap();
    json_run(&root, &["init", "ws", "--import-root", "inputs"], None, 0);
    let started = json_run(
        &root,
        &[
            "--workspace",
            "ws",
            "--actor",
            ACTOR,
            "start",
            "Why did the service retry?",
            "--attach",
            "inputs/service.log",
        ],
        None,
        0,
    );
    let created = started["created"].as_array().unwrap();
    let id_of = |reference: &str| {
        created
            .iter()
            .find(|c| c["client_ref"] == reference)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let case = id_of("case");
    let source = id_of("source:0");
    let quote = format!("Authorization: {SECRET}");
    let statement = format!("The request carried the credential {SECRET}");
    let observed = json_run(
        &root,
        &[
            "--workspace",
            "ws",
            "--actor",
            ACTOR,
            "--case",
            &case,
            "observe",
            &statement,
            "--source",
            &source,
            "--quote",
            &quote,
            "--limitations",
            "[\"one log\"]",
            "--command-id",
            "observe-credential",
        ],
        None,
        0,
    );
    let observation = observed["created"][0]["id"].as_str().unwrap().to_owned();
    json_run(&root, &["--workspace", "ws", "vault", "render"], None, 0);

    // A producer's staged error message quotes the secret too.
    let begun = json_run(
        &root,
        &["--workspace", "ws", "capture", "begin", "--input", "-"],
        Some(
            &serde_json::to_vec(&json!({
                "key": "run-0001/begin",
                "actor": "agent:example-producer",
                "producer": {"name": "example-producer", "version": "0.1.0"},
                "external_run_id": "run-0001",
                "intent": {"label": "synthetic run"},
                "case": {"kind": "new_record_case", "title": "Synthetic runs"}
            }))
            .unwrap(),
        ),
        0,
    );
    json_run(
        &root,
        &["--workspace", "ws", "capture", "append", "--input", "-"],
        Some(
            &serde_json::to_vec(&json!({
                "handle": begun["handle"], "key": "failed", "position": 1,
                "fact": {"kind": "error", "message": format!("auth failed for {SECRET}")}
            }))
            .unwrap(),
        ),
        0,
    );

    // Copies the tool did not make: a sync client's conflict copy beside the
    // generated note, an export and a leftover temporary.
    let workspace = root.join("ws");
    let note = workspace.join(format!("vault/Cases/{case}/Observations/{observation}.md"));
    fs::copy(
        &note,
        note.with_file_name(format!("{observation} (conflicted copy).md")),
    )
    .unwrap();
    fs::write(
        workspace.join("exports/handoff.md"),
        format!("credential {SECRET}"),
    )
    .unwrap();
    fs::write(workspace.join("state/tmp/leftover"), SECRET).unwrap();
    Fixture {
        _temporary: temporary,
        root,
        case,
        source,
        observation,
        observe_command: "observe-credential".to_owned(),
    }
}

#[test]
fn the_diagnosis_lists_every_local_file_holding_the_text_and_writes_nothing() {
    let fixture = fixture();
    let workspace = fixture.workspace();
    let truth = holding(&workspace);
    let before = files(&workspace);

    let report = json_run(
        &fixture.root,
        &["--workspace", "ws", "exposure", "--text-file", "secret.txt"],
        None,
        0,
    );
    assert_eq!(report["result"], "exposure_diagnosed");
    assert_eq!(report["writes"], "nothing");
    assert_eq!(report["subject"]["text"]["bytes"], SECRET.len());
    let found = located(&report, "found");
    let paths: BTreeSet<String> = found.keys().cloned().collect();
    assert_eq!(paths, truth, "{report:#}");

    // Every kind of place the fixture put the text is named by its area.
    let areas: BTreeSet<&str> = found.values().map(String::as_str).collect();
    for area in [
        "journal_event",
        "blob",
        "staging",
        "temporary",
        "index",
        "vault_note",
        "vault_conflict_copy",
        "export",
    ] {
        assert!(areas.contains(area), "{area} missing: {areas:?}");
    }
    // The verdict on the exposed blob names its digest but holds no text.
    let digests = located(&report, "digest_only");
    assert!(
        digests
            .iter()
            .any(|(path, area)| area == "blob_verdict" && path.starts_with("cache/blob-verdicts/")),
        "{digests:?}"
    );
    // The event holding the observation names it; nothing was opened late.
    let event = report["locations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| {
            l["area"] == "journal_event" && l["records"].to_string().contains(&fixture.observation)
        })
        .unwrap();
    assert_eq!(event["canonical"], true);
    assert_eq!(report["not_opened"], json!([]));
    assert!(
        report["areas"]["journal_event"]
            .as_str()
            .unwrap()
            .contains("append-only")
    );
    // Beyond the workspace: the import root, and copies that cannot be recalled.
    let outside = report["outside"].to_string();
    assert!(outside.contains("import_root") && outside.contains("cannot be recalled"));
    assert_eq!(
        report["next"][0]["argv"],
        json!(["investigation", "docs", "exposure"])
    );

    // The same diagnosis from stdin, in text form, still names every file.
    let text = invoke(
        &fixture.root,
        &["--workspace", "ws", "exposure", "--text-file", "-"],
        Some(SECRET.as_bytes()),
    );
    assert_eq!(text.exit, 0);
    for path in &truth {
        assert!(
            text.stdout.contains(path.as_str()),
            "{path}\n{}",
            text.stdout
        );
    }
    assert!(text.stdout.contains("nothing was removed or changed"));
    assert!(text.stdout.contains("Next: investigation docs exposure"));

    // Read-only mode accepts it, and nothing in the workspace changed.
    json_run(
        &fixture.root,
        &[
            "--workspace",
            "ws",
            "--read-only",
            "exposure",
            "--text-file",
            "secret.txt",
        ],
        None,
        0,
    );
    assert_eq!(files(&workspace), before);
}

#[test]
fn a_record_or_event_id_traces_its_own_locations() {
    let fixture = fixture();
    let workspace = fixture.workspace();

    let observation = json_run(
        &fixture.root,
        &["--workspace", "ws", "exposure", &fixture.observation],
        None,
        0,
    );
    assert_eq!(observation["subject"]["case_id"], fixture.case.as_str());
    let holds = located(&observation, "holds");
    assert!(holds.values().any(|area| area == "journal_event"));
    assert!(holds.values().any(|area| area == "index"));
    let notes = located(&observation, "may_hold");
    assert!(notes.contains_key(&format!(
        "vault/Cases/{}/Observations/{}.md",
        fixture.case, fixture.observation
    )));
    assert!(
        observation["limitations"]
            .to_string()
            .contains("Without --text-file")
    );

    // The Source traces to its Artifacts' retained bytes and their verdicts.
    let source = json_run(
        &fixture.root,
        &["--workspace", "ws", "exposure", &fixture.source],
        None,
        0,
    );
    let holds = located(&source, "holds");
    let blobs: Vec<_> = holds
        .iter()
        .filter(|(_, area)| *area == "blob")
        .map(|(path, _)| path)
        .collect();
    assert!(!blobs.is_empty());
    for blob in blobs {
        assert!(workspace.join(blob).is_file());
    }
    assert!(
        located(&source, "digest_only")
            .values()
            .all(|area| area == "blob_verdict")
    );

    // A command ID names its event.
    let event = json_run(
        &fixture.root,
        &["--workspace", "ws", "exposure", &fixture.observe_command],
        None,
        0,
    );
    assert_eq!(
        event["subject"]["records"],
        json!([fixture.observation.as_str()])
    );
    assert!(
        located(&event, "holds")
            .values()
            .any(|area| area == "journal_event")
    );

    // Combined: the record's own locations and every file holding the text.
    let both = json_run(
        &fixture.root,
        &[
            "--workspace",
            "ws",
            "exposure",
            &fixture.observation,
            "--text-file",
            "secret.txt",
        ],
        None,
        0,
    );
    let found: BTreeSet<String> = located(&both, "found").keys().cloned().collect();
    assert_eq!(found, holding(&workspace));
}

#[test]
fn bad_subjects_are_refused_without_echoing_the_text() {
    let fixture = fixture();
    let unknown = json_run(
        &fixture.root,
        &["--workspace", "ws", "exposure", "obs_nothing"],
        None,
        3,
    );
    assert_eq!(unknown["error"]["code"], "UNKNOWN_REFERENCE");
    let missing = invoke(&fixture.root, &["--workspace", "ws", "exposure"], None);
    assert_eq!(missing.exit, 2);
    fs::write(fixture.root.join("short.txt"), "abc\n").unwrap();
    let short = json_run(
        &fixture.root,
        &["--workspace", "ws", "exposure", "--text-file", "short.txt"],
        None,
        2,
    );
    assert_eq!(short["error"]["code"], "INVALID_INPUT");
    assert!(!short.to_string().contains("abc"));
    let long = "x".repeat(4097);
    let refused = json_run(
        &fixture.root,
        &["--workspace", "ws", "exposure", "--text-file", "-"],
        Some(long.as_bytes()),
        2,
    );
    assert_eq!(refused["error"]["path"], "--text-file");
}

#[test]
fn restricted_retained_bytes_are_listed_but_opened_only_on_request() {
    let fixture = fixture();
    let workspace = fixture.workspace();
    // Classify the Source restricted, the usual first containment. Its
    // Artifacts and their derivatives inherit the level.
    let operations = [json!({"op": "classification.raise", "payload": {
        "level": "restricted", "reason": "Holds an exposed credential.",
        "subject_ref": {"id": fixture.source, "revision": 1}, "tags": ["exposure"]
    }})];
    let command = json!({
        "schema_version": "inv.command.v1", "command_id": "raise-exposed",
        "actor": ACTOR, "case_id": fixture.case, "preconditions": [],
        "operations": operations,
    });
    json_run(
        &fixture.root,
        &["--workspace", "ws", "record", "--input", "-"],
        Some(&serde_json::to_vec(&command).unwrap()),
        0,
    );

    let closed = json_run(
        &fixture.root,
        &["--workspace", "ws", "exposure", "--text-file", "secret.txt"],
        None,
        0,
    );
    let not_opened = closed["not_opened"].as_array().unwrap();
    assert!(!not_opened.is_empty());
    for item in not_opened {
        assert_eq!(item["area"], "blob");
        assert!(
            item["reason"]
                .as_str()
                .unwrap()
                .contains("--include-restricted")
        );
    }
    let found = located(&closed, "found");
    assert!(found.values().all(|area| area != "blob"), "{found:?}");
    // Everything else holding the text is still found.
    let blobs: BTreeSet<String> = not_opened
        .iter()
        .map(|item| item["path"].as_str().unwrap().to_owned())
        .collect();
    let expected: BTreeSet<String> = holding(&workspace).difference(&blobs).cloned().collect();
    let paths: BTreeSet<String> = found.keys().cloned().collect();
    assert!(paths.is_superset(&expected), "{paths:?}\n{expected:?}");

    let opened = json_run(
        &fixture.root,
        &[
            "--workspace",
            "ws",
            "exposure",
            "--text-file",
            "secret.txt",
            "--include-restricted",
        ],
        None,
        0,
    );
    assert_eq!(opened["not_opened"], json!([]));
    let paths: BTreeSet<String> = located(&opened, "found").keys().cloned().collect();
    assert_eq!(paths, holding(&workspace));
}
