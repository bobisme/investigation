//! `vault reconcile` through the CLI, with the environment cleared and an
//! empty folder on `PATH`: no viewer application or other program is
//! needed. A dry run writes nothing and refuses what the real run refuses;
//! `propose-change` records only a stated, supported change, against the
//! case revision the note was rendered from (a stale note conflicts), and a
//! retry under the same key returns the original receipt; read-only mode refuses
//! everything but the dry run; the help teaches the command and its next
//! step. `vault clean-conflicts` previews, then with `--apply` removes, the
//! bytes a discard set aside; `vault unrelease` returns a released path once
//! the person's file is gone from it.

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
    assert_eq!(envelope["preconditions"][0]["revision"], 1, "{preview}");
    assert_eq!(
        preview["proposal"]["would_result"], "committed",
        "{preview}"
    );
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
    // A proposal's dry run goes through the store's dry run, which writes
    // nothing either.
    let proposed = casework.run(
        &[
            "--read-only",
            "--actor",
            "agent:reconcile-test",
            "vault",
            "reconcile",
            &overview,
            "--disposition",
            "propose-change",
            "--field",
            "title",
            "--value",
            "Why do lookups stall?",
            "--dry-run",
        ],
        0,
    );
    assert_eq!(
        proposed["proposal"]["would_result"], "committed",
        "{proposed}"
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

/// The arguments of a `propose-change` of the case title from `path`.
fn retitle(path: &str, value: &str, key: &str) -> Vec<String> {
    [
        "--actor",
        "agent:reconcile-test",
        "vault",
        "reconcile",
        path,
        "--disposition",
        "propose-change",
        "--field",
        "title",
        "--value",
        value,
        "--command-id",
        key,
    ]
    .map(str::to_owned)
    .to_vec()
}

impl Casework {
    /// Run `args` with `--format json`: the exit code and the response.
    fn attempt(&self, args: &[String]) -> (i32, Value) {
        let mut args: Vec<&str> = args.iter().map(String::as_str).collect();
        args.extend(["--format", "json"]);
        let output = self.command(&self.root, &args).output().unwrap();
        let value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {args:?}"));
        (output.status.code().unwrap(), value)
    }

    /// Run `args` as a dry run and then for real. Both must be refused
    /// alike, with the same exit code and the same error, and neither may
    /// change a file. Returns the error.
    fn refused_alike(&self, args: &[String]) -> Value {
        let before = self.files();
        let mut dry = args.to_vec();
        dry.push("--dry-run".to_owned());
        let (dry_exit, dry_refusal) = self.attempt(&dry);
        assert_eq!(self.files(), before, "{dry:?}: the dry run wrote");
        let (exit, refusal) = self.attempt(args);
        assert_eq!(self.files(), before, "{args:?}: the refusal wrote");
        assert_ne!(exit, 0, "{args:?}: {refusal}");
        assert_eq!(
            dry_exit, exit,
            "{args:?}\ndry: {dry_refusal}\nreal: {refusal}"
        );
        assert_eq!(
            dry_refusal["commit_status"], "not_committed",
            "{dry_refusal}"
        );
        assert_eq!(
            dry_refusal["error"], refusal["error"],
            "{args:?}: the dry run and the real run refuse differently"
        );
        refusal["error"].clone()
    }

    /// Commit one operation to the case as another writer.
    fn other_writer(&self, key: &str, op: &str, payload: Value, revision: Option<u64>) {
        let preconditions = revision.map_or_else(
            || serde_json::json!([]),
            |revision| serde_json::json!([{"id":self.case,"revision":revision}]),
        );
        let envelope = serde_json::json!({"schema_version":"inv.command.v1","command_id":key,
            "case_id":self.case,"actor":"agent:other-writer","preconditions":preconditions,
            "operations":[{"op":op,"payload":payload}]});
        let path = self.root.join("local/other-writer.json");
        fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
        self.run(&["record", "--input", path.to_str().unwrap()], 0);
        fs::remove_file(&path).unwrap();
    }

    /// The case record's revision now.
    fn revision(&self) -> u64 {
        self.run(&["show", &self.case], 0)["basis_revision"]
            .as_u64()
            .unwrap()
    }
}

/// V10, bn-3tpv: a note edited after a render that another writer's change
/// then made stale. The proposal's precondition is the revision the note
/// was rendered from, so the change is refused with `REVISION_CONFLICT`,
/// in the dry run as in the real run, and the other writer's title stays.
/// Rendering again does not freshen the edited note: its bytes still show
/// the older case. The repair works: keep the edit as a note, which renders
/// the path from the current case, and propose from there.
#[test]
fn a_proposal_from_a_note_rendered_before_the_case_changed_conflicts() {
    let casework = Casework::new();
    let overview = casework.overview();
    casework.edit(
        &overview,
        "\nA better title would be: Why do lookups stall?\n",
    );
    assert_eq!(casework.revision(), 1);
    casework.other_writer(
        "other-title",
        "case.amend",
        serde_json::json!({"title":"Why did the cache stall?","reason":"The stall is in the cache."}),
        Some(1),
    );
    assert_eq!(casework.revision(), 2);
    let args = retitle(&overview, "Why do lookups stall?", "retitle-stale");
    for render in [false, true] {
        if render {
            let rendered = casework.run(&["vault", "render"], 0);
            assert_eq!(rendered["counts"]["drifted"], 1, "{rendered}");
        }
        let error = casework.refused_alike(&args);
        assert_eq!(error["code"], "REVISION_CONFLICT", "{error}");
        assert_eq!(error["path"], "PATH", "{error}");
        assert_eq!(
            error["expected"],
            serde_json::json!({"id":casework.case,"revision":1})
        );
        assert_eq!(
            error["actual"],
            serde_json::json!({"id":casework.case,"revision":2})
        );
        let repair: Vec<&str> = error["repair"]["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| part.as_str().unwrap())
            .collect();
        assert!(
            repair.ends_with(&["show", casework.case.as_str(), "--format", "json"]),
            "{repair:?}"
        );
        let message = error["message"].as_str().unwrap();
        assert!(message.contains("rendered from revision 1"), "{message}");
        assert!(message.contains("preserve-as-note"), "{message}");
        assert_eq!(casework.title(), "Why did the cache stall?");
    }

    // The repair: keep the edit, then propose from the current note.
    casework.run(
        &[
            "vault",
            "reconcile",
            overview.as_str(),
            "--disposition",
            "preserve-as-note",
        ],
        0,
    );
    casework.edit(&overview, "\nStill: Why do lookups stall?\n");
    let args = retitle(&overview, "Why do lookups stall?", "retitle-fresh");
    let mut dry = args.clone();
    dry.push("--dry-run".to_owned());
    let (exit, preview) = casework.attempt(&dry);
    assert_eq!(exit, 0, "{preview}");
    assert_eq!(
        preview["proposal"]["envelope"]["preconditions"],
        serde_json::json!([{"id":casework.case,"revision":2}])
    );
    assert_eq!(preview["proposal"]["would_result"], "committed");
    let (exit, done) = casework.attempt(&args);
    assert_eq!(exit, 0, "{done}");
    assert_eq!(done["proposal"]["result"], "committed", "{done}");
    assert_eq!(casework.title(), "Why do lookups stall?");
}

/// A change to the case that leaves the case record as the note showed it
/// does not make the proposal stale: the precondition is the case record's
/// rendered revision, and it still holds.
#[test]
fn a_change_elsewhere_in_the_case_leaves_the_proposal_valid() {
    let casework = Casework::new();
    let overview = casework.overview();
    casework.edit(&overview, "\nRetitle this.\n");
    casework.other_writer(
        "question",
        "question.create",
        serde_json::json!({"importance":"normal","wording":"Which phase stalls?"}),
        None,
    );
    assert_eq!(casework.revision(), 1);
    let (exit, done) = casework.attempt(&retitle(&overview, "Why do lookups stall?", "retitle"));
    assert_eq!(exit, 0, "{done}");
    assert_eq!(done["proposal"]["result"], "committed", "{done}");
    assert_eq!(casework.title(), "Why do lookups stall?");
}

/// The dry run runs the store's gates: a proposal from a note rendered after
/// the case closed (so not stale) is refused by the closed-case gate in the
/// dry run exactly as in the real run. Before bn-3tpv the preview passed.
#[test]
fn the_dry_run_refuses_a_gated_proposal_as_the_real_run_does() {
    let casework = Casework::new();
    let overview = casework.overview();
    casework.other_writer(
        "close",
        "case.close",
        serde_json::json!({"outcome":"inconclusive","reason":"Nothing more to learn.",
            "reopen_condition":"A new timeout.","retrospective_waived":"No material work occurred.",
            "unresolved_questions":[]}),
        Some(1),
    );
    casework.run(&["vault", "render"], 0);
    let title = casework.title();
    casework.edit(&overview, "\nRetitle this.\n");
    let error = casework.refused_alike(&retitle(&overview, "Why do lookups stall?", "closed"));
    assert_eq!(error["code"], "GATE_FAILED", "{error}");
    assert_eq!(casework.title(), title);
}

/// Every refusal of every disposition is the same in the dry run as in the
/// real run, and neither changes a file.
#[test]
fn every_disposition_refuses_alike_in_the_dry_run_and_the_real_run() {
    let casework = Casework::new();
    let overview = casework.overview();
    let sources = casework
        .vault()
        .join(format!("Cases/{}/Sources", casework.case));
    let source = fs::read_dir(sources)
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
    casework.edit(&overview, "\nRetitle this.\n");
    // A command this actor already recorded under the key "taken".
    let question = serde_json::json!({"schema_version":"inv.command.v1","command_id":"taken",
        "case_id":casework.case,"actor":"agent:reconcile-test","preconditions":[],
        "operations":[{"op":"question.create","payload":{"importance":"normal","wording":"Which phase?"}}]});
    let input = casework.root.join("local/taken.json");
    fs::write(&input, serde_json::to_vec(&question).unwrap()).unwrap();
    casework.run(&["record", "--input", input.to_str().unwrap()], 0);
    fs::remove_file(&input).unwrap();

    let reconcile = |path: &str, disposition: &str, extra: &[&str]| {
        let mut args: Vec<String> = [
            "--actor",
            "agent:reconcile-test",
            "vault",
            "reconcile",
            path,
            "--disposition",
            disposition,
        ]
        .map(str::to_owned)
        .to_vec();
        args.extend(extra.iter().map(|part| (*part).to_owned()));
        args
    };
    let missing = format!("Cases/{}/Nothing.md", casework.case);
    let cases: Vec<(&str, Vec<String>, &str)> = vec![
        (
            "a clean path",
            reconcile(&source, "preserve-as-note", &[]),
            "INVALID_INPUT",
        ),
        (
            "an unowned path",
            reconcile(&missing, "preserve-as-note", &[]),
            "",
        ),
        (
            "a field without propose-change",
            reconcile(&overview, "preserve-as-note", &["--field", "title"]),
            "INVALID_INPUT",
        ),
        (
            "discard without confirmation",
            reconcile(&overview, "discard", &[]),
            "",
        ),
        (
            "discard of a clean path",
            reconcile(&source, "discard", &["--discard-observed"]),
            "",
        ),
        (
            "confirmation without discard",
            reconcile(&overview, "release-path", &["--discard-observed"]),
            "INVALID_INPUT",
        ),
        (
            "release of an unowned path",
            reconcile(&missing, "release-path", &[]),
            "",
        ),
        (
            "an unsupported field",
            reconcile(
                &overview,
                "propose-change",
                &["--field", "status", "--value", "closed"],
            ),
            "INVALID_INPUT",
        ),
        (
            "no value",
            reconcile(&overview, "propose-change", &["--field", "title"]),
            "INVALID_INPUT",
        ),
        (
            "an empty value",
            reconcile(
                &overview,
                "propose-change",
                &["--field", "title", "--value", " "],
            ),
            "INVALID_INPUT",
        ),
        (
            "a clean path proposed",
            reconcile(
                &source,
                "propose-change",
                &["--field", "title", "--value", "T"],
            ),
            "",
        ),
        (
            "another case named",
            {
                let mut args = reconcile(
                    &overview,
                    "propose-change",
                    &["--field", "title", "--value", "T"],
                );
                args.splice(
                    0..0,
                    [
                        "--case".to_owned(),
                        "case_01a20f59-805c-77a0-b8b5-398371a7cb63".to_owned(),
                    ],
                );
                args
            },
            "",
        ),
        (
            "two paths proposed",
            {
                let mut args = reconcile(
                    &overview,
                    "propose-change",
                    &["--field", "title", "--value", "T"],
                );
                args.insert(5, source.clone());
                args
            },
            "",
        ),
        (
            "a key already used for another command",
            reconcile(
                &overview,
                "propose-change",
                &["--field", "title", "--value", "T", "--command-id", "taken"],
            ),
            "IDEMPOTENCY_KEY_REUSED",
        ),
    ];
    let title = casework.title();
    for (name, args, code) in cases {
        let error = casework.refused_alike(&args);
        if !code.is_empty() {
            assert_eq!(error["code"], code, "{name}: {error}");
        }
    }
    assert_eq!(casework.title(), title);
    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["drift_count"], 1, "{status}");
}

impl Casework {
    /// Run a full argv a response named (`next` or `repair`), as given.
    fn run_argv(&self, argv: &[String], exit: i32) -> Value {
        let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        let output = self.command(&self.root, &args).output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(exit), "{argv:?}\n{stdout}");
        serde_json::from_str(&stdout).unwrap()
    }

    fn help(&self, args: &[&str]) -> String {
        let output = self.command(&self.root, args).output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }
}

#[test]
fn clean_conflicts_previews_then_with_apply_removes_the_discarded_bytes() {
    let casework = Casework::new();
    let overview = casework.overview();
    casework.edit(&overview, "\nan edit to throw away\n");
    let done = casework.run(
        &[
            "vault",
            "reconcile",
            &overview,
            "--disposition",
            "discard",
            "--discard-observed",
        ],
        0,
    );
    let kept = done["paths"][0]["observed"][0]["to"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(casework.root.join(&kept).is_file());
    let before = casework.files();

    for read_only in [false, true] {
        let mut args = vec!["vault", "clean-conflicts"];
        if read_only {
            args.insert(0, "--read-only");
        }
        let preview = casework.run(&args, 0);
        assert_eq!(preview["result"], "vault_clean_conflicts_preview");
        assert_eq!(preview["apply"], false);
        // The discarded edit, and the generated version the render withheld
        // while the path was drifted, which the manifest still records.
        assert_eq!(preview["counts"]["would_remove"], 2, "{preview}");
        assert_eq!(preview["would_remove"][0]["path"], kept.as_str());
        assert_eq!(preview["would_remove"][0]["vault_path"], overview.as_str());
        assert_eq!(preview["would_remove"][0]["area"], "discarded");
        assert_eq!(preview["would_remove"][1]["area"], "withheld", "{preview}");
        assert_eq!(preview["would_remove"][1]["vault_path"], overview.as_str());
        assert_eq!(casework.files(), before, "a preview writes nothing");
    }
    let refused = casework.run(&["--read-only", "vault", "clean-conflicts", "--apply"], 2);
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");
    // The proposed area alone holds nothing.
    let proposed = casework.run(
        &["vault", "clean-conflicts", "--area", "proposed", "--apply"],
        0,
    );
    assert_eq!(proposed["counts"]["removed"], 0);
    assert_eq!(casework.files(), before);
    let wrong = casework.run(&["vault", "clean-conflicts", "--area", "all"], 2);
    assert_eq!(wrong["error"]["code"], "INVALID_INPUT");

    // The preview's next step is the removal.
    let preview = casework.run(&["vault", "clean-conflicts"], 0);
    let next = argv(&preview);
    assert!(next.ends_with(&[
        "--apply".to_owned(),
        "--format".to_owned(),
        "json".to_owned()
    ]));
    let withheld = preview["would_remove"][1]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    let cleaned = casework.run_argv(&next, 0);
    assert_eq!(cleaned["result"], "vault_conflicts_cleaned");
    assert_eq!(cleaned["counts"]["removed"], 2, "{cleaned}");
    assert!(!casework.root.join(&kept).exists());
    assert!(!casework.root.join(&withheld).exists());
    assert!(!casework.root.join("local/conflicts/discarded").exists());
    let manifest = casework.root.join("local/render-manifest.json");
    let mut expected = before.clone();
    expected.remove(&PathBuf::from(&kept));
    expected.remove(&PathBuf::from(&withheld));
    let mut after = casework.files();
    let recorded = after
        .remove(Path::new("local/render-manifest.json"))
        .unwrap();
    expected.remove(Path::new("local/render-manifest.json"));
    assert_eq!(after, expected, "only the identified files are gone");
    assert!(
        !String::from_utf8_lossy(&recorded).contains(&withheld),
        "the manifest forgets the removed withheld version"
    );
    assert!(manifest.is_file());
    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["drift_count"], 0);
    assert_eq!(argv(&cleaned)[3..5], ["vault", "status"]);
}

#[test]
fn unrelease_returns_a_released_note_once_the_persons_file_is_moved() {
    let casework = Casework::new();
    let overview = casework.overview();
    casework.edit(&overview, "\nthis is my note now\n");
    let released = casework.run(
        &[
            "vault",
            "reconcile",
            &overview,
            "--disposition",
            "release-path",
        ],
        0,
    );
    let relocated = released["paths"][0]["relocated_to"]
        .as_str()
        .unwrap()
        .to_owned();
    let mine = fs::read(casework.vault().join(&overview)).unwrap();
    let before = casework.files();

    // The person's file is still there: refused, with a repair that works.
    let refused = casework.run(&["vault", "unrelease", &overview], 2);
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains("still holds your file"), "{message}");
    let repair: Vec<String> = refused["error"]["repair"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap().to_owned())
        .collect();
    let status = casework.run_argv(&repair, 0);
    assert_eq!(status["released"][0]["path"], overview.as_str());
    assert_eq!(casework.files(), before, "a refusal changes nothing");

    fs::create_dir_all(casework.vault().join("Notes")).unwrap();
    fs::rename(
        casework.vault().join(&overview),
        casework.vault().join("Notes/mine.md"),
    )
    .unwrap();
    let before = casework.files();
    for read_only in [false, true] {
        let mut args = vec!["vault", "unrelease", overview.as_str(), "--dry-run"];
        if read_only {
            args.insert(0, "--read-only");
        }
        let preview = casework.run(&args, 0);
        assert_eq!(preview["result"], "vault_unrelease_preview");
        assert_eq!(preview["paths"][0]["path"], overview.as_str());
        assert_eq!(preview["paths"][0]["relocated_from"], relocated.as_str());
        assert_eq!(casework.files(), before, "a dry run writes nothing");
    }
    let refused = casework.run(&["--read-only", "vault", "unrelease", &overview], 2);
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");

    let preview = casework.run(&["vault", "unrelease", &overview, "--dry-run"], 0);
    let done = casework.run_argv(&argv(&preview), 0);
    assert_eq!(done["result"], "vault_unreleased");
    assert_eq!(done["clear"], true, "{done}");
    let note = fs::read_to_string(casework.vault().join(&overview)).unwrap();
    assert!(
        note.contains(&format!("inv_id: \"{}\"", casework.case)),
        "{note}"
    );
    assert!(!casework.vault().join(&relocated).exists());
    assert_eq!(
        fs::read(casework.vault().join("Notes/mine.md")).unwrap(),
        mine
    );
    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["released_count"], 0, "{status}");
    assert_eq!(status["drift_count"], 0);
    // Returned once; a second return has nothing to do.
    let again = casework.run(&["vault", "unrelease", &overview], 2);
    let message = again["error"]["message"].as_str().unwrap();
    assert!(message.contains("already owns"), "{message}");
}

#[test]
fn help_teaches_clean_conflicts_and_unrelease_with_examples_and_the_next_step() {
    let casework = Casework::new();
    for (args, needles) in [
        (
            &["vault", "clean-conflicts", "--help"][..],
            &[
                "local/conflicts/discarded/",
                "local/conflicts/proposed/",
                "local/conflicts/<generation>/",
                "vault clean-conflicts --area withheld",
                "--apply",
                "--area",
                "Examples:",
                "vault clean-conflicts --area discarded --apply",
                "Next: investigation --workspace ./casework vault clean-conflicts --apply",
            ][..],
        ),
        (
            &["vault", "unrelease", "--help"],
            &[
                "release-path",
                "--dry-run",
                "Examples:",
                "vault unrelease Cases/",
                "Next: investigation --workspace ./casework vault status",
            ],
        ),
        (
            &["vault", "reconcile", "--help"],
            &["vault unrelease", "vault clean-conflicts"],
        ),
        (&["vault", "--help"], &["unrelease", "clean-conflicts"]),
    ] {
        let help = casework.help(args);
        for needle in needles {
            assert!(help.contains(needle), "{args:?}: {needle}\n{help}");
        }
        assert!(!help.to_lowercase().contains("obsidian"), "{help}");
    }
}
