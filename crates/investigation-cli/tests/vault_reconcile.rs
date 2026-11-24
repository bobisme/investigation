//! `vault reconcile` through the CLI, with the environment cleared and an
//! empty folder on `PATH`: no viewer application or other program is
//! needed. A dry run writes nothing; `propose-change` records only a stated,
//! supported change with a fresh precondition, and a retry under the same
//! key returns the original receipt; read-only mode refuses
//! everything but the dry run; the help teaches the command and its next
//! step.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

struct Casework {
    _directory: tempfile::TempDir,
    root: PathBuf,
    empty_path: PathBuf,
    case: String,
}

impl Casework {
    /// A workspace with one case, rendered.
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let empty_path = directory.path().join("empty-path");
        fs::create_dir(&empty_path).unwrap();
        let log = directory.path().join("results.log");
        fs::write(&log, "waited 28 seconds\n").unwrap();
        let parent = directory.path().to_owned();
        let mut casework = Self {
            root: parent.join("casework"),
            empty_path,
            _directory: directory,
            case: String::new(),
        };
        let root = casework.root.to_str().unwrap().to_owned();
        casework.run_in(
            &parent,
            &["init", &root, "--import-root", parent.to_str().unwrap()],
            0,
        );
        let receipt = casework.run(
            &[
                "--actor",
                "agent:reconcile-test",
                "start",
                "Why did requests time out?",
                "--attach",
                log.to_str().unwrap(),
                "--command-id",
                "start-1",
            ],
            0,
        );
        casework.case = receipt["created"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|change| change["id"].as_str().filter(|id| id.starts_with("case_")))
            .unwrap()
            .to_owned();
        casework.run(&["vault", "render"], 0);
        casework
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
        command
            .current_dir(cwd)
            .env_clear()
            .env("PATH", &self.empty_path)
            .args(args);
        command
    }

    fn run_in(&self, cwd: &Path, args: &[&str], exit: i32) -> Value {
        let mut args = args.to_vec();
        args.extend(["--format", "json"]);
        let output = self.command(cwd, &args).output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{args:?}\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("{error}: {stdout}"))
    }

    fn run(&self, args: &[&str], exit: i32) -> Value {
        self.run_in(&self.root, args, exit)
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn overview(&self) -> String {
        format!("Cases/{}/Overview.md", self.case)
    }

    /// Append to a generated note and render, so the edit is diagnosed.
    fn edit(&self, path: &str, line: &str) {
        let target = self.vault().join(path);
        let mut text = fs::read_to_string(&target).unwrap();
        text.push_str(line);
        fs::write(&target, text).unwrap();
        self.run(&["vault", "render"], 0);
    }

    fn title(&self) -> String {
        let shown = self.run(&["show", &self.case], 0);
        shown["title"].as_str().unwrap().to_owned()
    }

    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut found = BTreeMap::new();
        let mut pending = vec![self.root.clone()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.file_name().is_some_and(|name| name != "render.lock") {
                    found.insert(
                        path.strip_prefix(&self.root).unwrap().to_owned(),
                        fs::read(&path).unwrap(),
                    );
                }
            }
        }
        found
    }
}

fn argv(value: &Value) -> Vec<String> {
    value["next"][0]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn propose_change_records_the_stated_title_and_clears_the_drift() {
    let casework = Casework::new();
    let overview = casework.overview();
    let title = casework.title();
    casework.edit(
        &overview,
        "\nA better title would be: Why do lookups stall?\n",
    );

    // vault conflicts names the reconcile command to try first.
    let conflicts = casework.run(&["vault", "conflicts"], 0);
    let next = argv(&conflicts);
    assert!(next.ends_with(&[
        "vault".to_owned(),
        "reconcile".to_owned(),
        overview.clone(),
        "--disposition".to_owned(),
        "preserve-as-note".to_owned(),
        "--dry-run".to_owned(),
        "--format".to_owned(),
        "json".to_owned(),
    ]));

    let args = [
        "--actor",
        "agent:reconcile-test",
        "vault",
        "reconcile",
        overview.as_str(),
        "--disposition",
        "propose-change",
        "--field",
        "title",
        "--value",
        "Why do lookups stall?",
        "--command-id",
        "retitle-1",
    ];
    let before = casework.files();
    let mut dry = args.to_vec();
    dry.push("--dry-run");
    let preview = casework.run(&dry, 0);
    assert_eq!(preview["result"], "vault_reconcile_preview");
    assert_eq!(preview["dry_run"], true);
    let envelope = &preview["proposal"]["envelope"];
    assert_eq!(envelope["case_id"], casework.case.as_str());
    assert_eq!(envelope["preconditions"][0]["id"], casework.case.as_str());
    assert!(envelope["preconditions"][0]["revision"].as_u64().is_some());
    assert_eq!(envelope["operations"][0]["op"], "case.amend");
    assert_eq!(
        envelope["operations"][0]["payload"]["title"],
        "Why do lookups stall?"
    );
    assert_eq!(casework.files(), before, "a dry run writes nothing");
    assert_eq!(casework.title(), title);
    // The dry run's next step is the same command without --dry-run.
    let next = argv(&preview);
    assert!(next.contains(&"propose-change".to_owned()));
    assert!(!next.contains(&"--dry-run".to_owned()));

    let done = casework.run(&args, 0);
    assert_eq!(done["result"], "vault_reconciled");
    assert_eq!(done["clear"], true, "{done}");
    assert!(done["proposal"]["transaction_id"].is_string(), "{done}");
    assert_eq!(casework.title(), "Why do lookups stall?");
    let note = fs::read_to_string(casework.vault().join(&overview)).unwrap();
    assert!(note.contains("# Why do lookups stall?\n"), "{note}");
    let kept = done["paths"][0]["observed"][0]["to"].as_str().unwrap();
    assert!(kept.starts_with("local/conflicts/proposed/"), "{kept}");
    let kept = fs::read_to_string(casework.root.join(kept)).unwrap();
    assert!(kept.contains("A better title would be"));
    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["drift_count"], 0);
    assert_eq!(status["freshness"], "current");
}

/// A retry under the same `--command-id` returns the original receipt, as
/// `record` does, whether or not the first call reconciled the note; a
/// different change under the same key is refused and changes nothing.
#[test]
fn a_retry_under_the_same_command_id_returns_the_original_receipt() {
    let casework = Casework::new();
    let overview = casework.overview();
    casework.edit(&overview, "\nRetitle this.\n");
    let reconcile = |value: &str| {
        vec![
            "--actor",
            "agent:reconcile-test",
            "vault",
            "reconcile",
            overview.as_str(),
            "--disposition",
            "propose-change",
            "--field",
            "title",
            "--value",
            value,
            "--command-id",
            "retitle-retry",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>()
    };
    let args = reconcile("Why do lookups stall?");
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    // The change was recorded but the call stopped before the vault was
    // reconciled: record the exact envelope the command would submit.
    let mut dry = args.clone();
    dry.push("--dry-run");
    let envelope = casework.run(&dry, 0)["proposal"]["envelope"].clone();
    let input = casework.root.join("envelope.json");
    fs::write(&input, envelope.to_string()).unwrap();
    let first = casework.run(
        &[
            "--actor",
            "agent:reconcile-test",
            "record",
            "--input",
            input.to_str().unwrap(),
        ],
        0,
    );
    fs::remove_file(&input).unwrap();
    let transaction = first["transaction_id"].as_str().unwrap().to_owned();

    // The retry finds the note still drifted: it records nothing new and
    // reconciles the note.
    let events = || {
        fs::read_dir(
            casework
                .root
                .join("state/cases")
                .join(&casework.case)
                .join("events"),
        )
        .unwrap()
        .count()
    };
    let recorded = events();
    let finished = casework.run(&args, 0);
    assert_eq!(
        finished["proposal"]["result"], "already_committed",
        "{finished}"
    );
    assert_eq!(finished["proposal"]["transaction_id"], transaction.as_str());
    assert_eq!(finished["clear"], true, "{finished}");
    assert_eq!(events(), recorded);
    assert_eq!(casework.title(), "Why do lookups stall?");

    // Its response is lost too: the next retry finds nothing to reconcile and
    // returns the same receipt without changing a file.
    let before = casework.files();
    let again = casework.run(&args, 0);
    assert_eq!(again["result"], "vault_reconciled");
    assert_eq!(again["proposal"]["result"], "already_committed", "{again}");
    assert_eq!(again["proposal"]["transaction_id"], transaction.as_str());
    assert_eq!(again["paths"], serde_json::json!([]));
    assert_eq!(casework.files(), before);

    // A different change under the same key is refused.
    let changed = reconcile("Another title");
    let changed: Vec<&str> = changed.iter().map(String::as_str).collect();
    let refused = casework.run(&changed, 4);
    assert_eq!(
        refused["error"]["code"], "IDEMPOTENCY_KEY_REUSED",
        "{refused}"
    );
    assert_eq!(casework.files(), before);
    assert_eq!(casework.title(), "Why do lookups stall?");
}

#[test]
fn propose_change_refuses_unsupported_fields_and_records_nothing() {
    let casework = Casework::new();
    let overview = casework.overview();
    let source = fs::read_dir(
        casework
            .vault()
            .join(format!("Cases/{}/Sources", casework.case)),
    )
    .unwrap()
    .next()
    .unwrap()
    .unwrap()
    .file_name();
    let source = format!(
        "Cases/{}/Sources/{}",
        casework.case,
        source.to_str().unwrap()
    );
    casework.edit(&overview, "\n- [x] decided\n");
    casework.edit(&source, "\ninv_status: \"accepted\"\n");
    let before = casework.files();
    for (path, field) in [
        (&source, "title"),
        (&overview, "status"),
        (&overview, "mode"),
    ] {
        let refused = casework.run(
            &[
                "vault",
                "reconcile",
                path,
                "--disposition",
                "propose-change",
                "--field",
                field,
                "--value",
                "anything",
            ],
            2,
        );
        assert_eq!(refused["error"]["code"], "INVALID_INPUT");
        assert_eq!(refused["commit_status"], "not_committed");
        let supported: Vec<&str> = refused["error"]["actual"]["supported"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["field"].as_str().unwrap())
            .collect();
        assert_eq!(supported, ["title", "objective", "scope"]);
    }
    // The value is never read from the note: without --value it is refused.
    let refused = casework.run(
        &[
            "vault",
            "reconcile",
            &overview,
            "--disposition",
            "propose-change",
            "--field",
            "title",
        ],
        2,
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nothing is read from the edited note")
    );
    // Flags of one disposition are refused on another.
    casework.run(
        &[
            "vault",
            "reconcile",
            &overview,
            "--disposition",
            "preserve-as-note",
            "--field",
            "title",
        ],
        2,
    );
    assert_eq!(casework.files(), before);
}

#[test]
fn read_only_allows_only_the_dry_run() {
    let casework = Casework::new();
    let overview = casework.overview();
    casework.edit(&overview, "\nedited\n");
    let before = casework.files();
    let refused = casework.run(
        &[
            "--read-only",
            "vault",
            "reconcile",
            &overview,
            "--disposition",
            "preserve-as-note",
        ],
        2,
    );
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");
    let preview = casework.run(
        &[
            "--read-only",
            "vault",
            "reconcile",
            &overview,
            "--disposition",
            "preserve-as-note",
            "--dry-run",
        ],
        0,
    );
    assert_eq!(
        preview["paths"][0]["observed"][0]["handling"],
        "moved_to_notes"
    );
    assert_eq!(casework.files(), before);

    let done = casework.run(
        &[
            "vault",
            "reconcile",
            &overview,
            "--disposition",
            "preserve-as-note",
        ],
        0,
    );
    assert_eq!(done["clear"], true);
    let moved = done["paths"][0]["observed"][0]["to"].as_str().unwrap();
    assert_eq!(moved, "Notes/Reconciled/Overview.md");
    assert_eq!(
        fs::read(casework.vault().join(moved)).unwrap(),
        before[&PathBuf::from("vault").join(&overview)]
    );
}

#[test]
fn help_teaches_the_dispositions_with_examples_and_the_next_step() {
    let casework = Casework::new();
    let output = casework
        .command(&casework.root, &["vault", "reconcile", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for needle in [
        "preserve-as-note",
        "propose-change",
        "discard",
        "release-path",
        "--discard-observed",
        "--dry-run",
        "Examples:",
        "investigation vault reconcile Cases/",
        "Next: investigation vault status",
    ] {
        assert!(help.contains(needle), "{needle}\n{help}");
    }
}

/// V10 (bn-dla3): a proposal from a note rendered before another writer
/// changed the case is built and checked against the case as it is now.
/// The note shows an open case at revision 1; the case was closed at
/// revision 2 and the vault not rendered since. The proposal carries the
/// fresh revision and only the stated value, and the store checks it
/// against the closed case: refused, nothing committed, no file changed.
#[test]
fn a_proposal_from_a_stale_note_is_checked_against_the_fresh_case() {
    let casework = Casework::new();
    let overview = casework.overview();
    let title = casework.title();
    casework.edit(
        &overview,
        "\nA better title would be: Why do lookups stall?\n",
    );
    let rendered = casework.run(&["show", &casework.case], 0)["basis_revision"].clone();
    assert_eq!(rendered, 1);

    // Another writer closes the case; nothing renders the vault again.
    let close = serde_json::json!({"schema_version":"inv.command.v1","command_id":"close",
        "case_id":casework.case,"actor":"agent:other-writer",
        "preconditions":[{"id":casework.case,"revision":1}],
        "operations":[{"op":"case.close","payload":{"outcome":"inconclusive",
            "reason":"Nothing more to learn.","reopen_condition":"A new timeout.",
            "retrospective_waived":"No material work occurred.","unresolved_questions":[]}}]});
    let path = casework.root.join("local/close.json");
    fs::write(&path, serde_json::to_vec(&close).unwrap()).unwrap();
    casework.run(&["record", "--input", path.to_str().unwrap()], 0);
    fs::remove_file(&path).unwrap();
    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["freshness"], "stale", "{status}");
    assert_eq!(status["cases"][0]["rendered"]["sequence"], 1, "{status}");
    assert_eq!(status["cases"][0]["current"]["sequence"], 2, "{status}");

    let args = [
        "--actor",
        "agent:reconcile-test",
        "vault",
        "reconcile",
        overview.as_str(),
        "--disposition",
        "propose-change",
        "--field",
        "title",
        "--value",
        "Why do lookups stall?",
        "--command-id",
        "retitle-stale",
    ];
    let before = casework.files();
    let mut dry = args.to_vec();
    dry.push("--dry-run");
    let preview = casework.run(&dry, 0);
    let envelope = &preview["proposal"]["envelope"];
    assert_eq!(
        envelope["preconditions"],
        serde_json::json!([{"id":casework.case,"revision":2}]),
        "the fresh revision, not the note's: {preview}"
    );
    let payload = &envelope["operations"][0]["payload"];
    assert_eq!(payload["title"], "Why do lookups stall?");
    assert!(
        !payload.to_string().contains("A better title"),
        "nothing is read from the note: {payload}"
    );

    let refused = casework.run(&args, 5);
    assert_eq!(refused["commit_status"], "not_committed", "{refused}");
    assert_eq!(refused["error"]["code"], "GATE_FAILED", "{refused}");
    assert_eq!(casework.title(), title);
    assert_eq!(
        casework.files(),
        before,
        "a refused proposal changes no file"
    );
}
