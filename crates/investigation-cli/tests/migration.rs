//! W11.15, ADR 0014: historical workspaces open, replay, `check` and render
//! under this build; reads on them change no file; a newer required format
//! stops writes and says why; and the migration framework runs dry run
//! first, takes and verifies a backup before any change, reverses exactly
//! from that backup, and finishes after a stop at any step.
//!
//! No migration ships today, so the framework is exercised with a test
//! migration ([`Probe`]) that writes one file under `cache/` and one under
//! `local/` and raises the write requirement to format 2. This build
//! understands format 1, so after the probe it refuses writes, which is the
//! simulated future version the framework must respect.
//!
//! Every binary run clears the environment and puts an empty folder on
//! `PATH`.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
};

use investigation_render::migrate::{self, MigrationPlan, Step, Stopped};
use investigation_store::{
    Store, Workspace,
    journal::hash_bytes,
    migration::{Area, Migration, MigrationError, Registry},
    workspace::WorkspaceFormat,
};
use serde_json::{Value, json};

const ACTOR: &str = "agent:migration-tester";
const HISTORICAL: [&str; 5] = [
    "alpha-pilot-332e69ec",
    "alpha-pilot-37b1a42c",
    "alpha-pilot-bb3ab553",
    "v0-alpha-2026-11-22",
    "v1-2026-11-24",
];
/// Layout directories git does not keep.
const LAYOUT: [&str; 10] = [
    "state/cases",
    "state/events",
    "state/blobs/sha256",
    "state/checkpoints",
    "state/staging",
    "state/locks",
    "state/tmp",
    "cache",
    "local",
    "exports",
];
const HUMAN: [&str; 3] = ["vault/Notes", "vault/Boards", "vault/Inbox"];

fn historical_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/historical")
}

/// Copy a historical workspace to `target` and add the empty layout
/// directories git drops.
fn historical(name: &str, target: &Path) -> PathBuf {
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let path = entry.unwrap().path();
            let destination = to.join(path.file_name().unwrap());
            if path.is_dir() {
                copy(&path, &destination);
            } else {
                fs::copy(&path, &destination).unwrap();
            }
        }
    }
    let source = historical_root().join(name);
    copy(&source, target);
    for directory in LAYOUT {
        fs::create_dir_all(target.join(directory)).unwrap();
    }
    if source.join("vault").is_dir() {
        for directory in HUMAN {
            fs::create_dir_all(target.join(directory)).unwrap();
        }
    }
    target.to_owned()
}

fn run_with(cwd: &Path, args: &[&str], input: Option<&str>) -> (i32, Value) {
    let empty_path = cwd.join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_clear()
        .env("PATH", &empty_path)
        .args(args)
        .args(["--format", "json"])
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
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

fn run(cwd: &Path, workspace: &Path, args: &[&str]) -> (i32, Value) {
    let mut full = vec!["--workspace", workspace.to_str().unwrap()];
    full.extend_from_slice(args);
    run_with(cwd, &full, None)
}

fn case_ids(workspace: &Path) -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(workspace.join("state/cases"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    ids.sort();
    ids
}

/// Every file and directory below `root`: bytes, size, modification time
/// and mode, so a rewrite with equal bytes is seen too.
fn tree(root: &Path) -> BTreeMap<String, (Vec<u8>, i64, i64, u32)> {
    fn walk(root: &Path, at: &Path, found: &mut BTreeMap<String, (Vec<u8>, i64, i64, u32)>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            if relative == ".empty-path" {
                continue;
            }
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                found.insert(
                    format!("{relative}/"),
                    (Vec::new(), 0, metadata.mtime_nsec(), metadata.mode()),
                );
                walk(root, &path, found);
            } else {
                found.insert(
                    relative,
                    (
                        fs::read(&path).unwrap(),
                        metadata.mtime(),
                        metadata.mtime_nsec(),
                        metadata.mode(),
                    ),
                );
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, root, &mut found);
    found
}

type Tree = BTreeMap<String, (Vec<u8>, i64, i64, u32)>;

/// Fail with the paths that differ between two [`tree`]s.
fn assert_unchanged(before: &Tree, after: &Tree, what: &str) {
    let changed: Vec<&String> = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .collect();
    assert!(changed.is_empty(), "{what}: {changed:?}");
}

/// The disposable cache of blob hash verdicts. Each entry names the
/// identity (device, inode, times) of the blob file it hashed, and a copy
/// changes that identity, so the first read of a copied workspace hashes
/// its blobs again and records new verdicts. That is not a migration: a
/// workspace read in place keeps its verdicts, and so does a second read.
const VERDICTS: &str = "cache/blob-verdicts";

/// The first reads of a copied workspace: `brief` on every case, then
/// `check`, which hashes every blob.
fn warm(cwd: &Path, workspace: &Path) {
    for case in case_ids(workspace) {
        let (exit, brief) = run(cwd, workspace, &["--case", &case, "brief"]);
        assert_eq!(exit, 0, "{brief}");
    }
    let (_, check) = run(cwd, workspace, &["check"]);
    assert_eq!(check["structurally_valid"], true, "{check}");
}

/// The canonical files: marker, configuration, case journals and blobs.
fn canonical(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    for name in ["investigation.toml", "state/workspace.json"] {
        found.insert(name.to_owned(), fs::read(root.join(name)).unwrap());
    }
    for (path, (bytes, ..)) in tree(root) {
        let journal = path.starts_with("state/cases/")
            && (path.ends_with("genesis.json") || path.contains("/events/"))
            && !path.ends_with('/');
        let blob = path.starts_with("state/blobs/sha256/") && !path.ends_with('/');
        if journal || blob {
            found.insert(path, bytes);
        }
    }
    found
}

/// The human vault folders, by path and bytes.
fn human(root: &Path) -> BTreeMap<String, Vec<u8>> {
    tree(root)
        .into_iter()
        .filter(|(path, _)| {
            HUMAN
                .iter()
                .any(|folder| path.starts_with(&format!("{folder}/")))
                && !path.ends_with('/')
        })
        .map(|(path, (bytes, ..))| (path, bytes))
        .collect()
}

fn set_format(workspace: &Path, format: Value, extra: Option<(&str, Value)>) {
    let path = workspace.join("state/workspace.json");
    let mut marker: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    marker["format"] = format;
    if let Some((name, value)) = extra {
        marker[name] = value;
    }
    fs::write(&path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();
}

fn note(cwd: &Path, workspace: &Path, case: &str, key: &str) -> (i32, Value) {
    run(
        cwd,
        workspace,
        &[
            "--case",
            case,
            "note",
            "Checked the clock source",
            "--on",
            case,
            "--actor",
            ACTOR,
            "--command-id",
            key,
        ],
    )
}

fn issue_codes(check: &Value) -> Vec<String> {
    check["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["code"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn historical_fixtures_are_kept_byte_for_byte() {
    let root = historical_root();
    let sums = fs::read_to_string(root.join("SHA256SUMS")).unwrap();
    let mut listed = BTreeMap::new();
    for line in sums.lines() {
        let (digest, path) = line.split_once("  ").unwrap();
        listed.insert(path.to_owned(), format!("sha256:{digest}"));
    }
    let mut found = BTreeMap::new();
    for (path, (bytes, ..)) in tree(&root) {
        if path.ends_with('/') || path == "README.md" || path == "SHA256SUMS" {
            continue;
        }
        found.insert(path, hash_bytes(&bytes).to_string());
    }
    assert_eq!(
        found, listed,
        "a historical fixture changed; these are kept forever (fixtures/historical/README.md)"
    );
    for name in HISTORICAL {
        assert!(
            root.join(name).join("state/workspace.json").is_file(),
            "{name}"
        );
    }
}

#[test]
fn every_historical_fixture_opens_replays_checks_and_renders_under_this_build() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    for name in HISTORICAL {
        let workspace = historical(name, &cwd.join(name));
        let before = canonical(&workspace);
        let notes = human(&workspace);

        let (exit, check) = run(cwd, &workspace, &["check"]);
        assert_eq!(exit, 0, "{name}: {check}");
        assert_eq!(check["structurally_valid"], true, "{name}");
        assert_eq!(
            check["format"],
            json!({"version": 1, "read_requires": 1, "write_requires": 1}),
            "{name}: format 1, with no section in the marker"
        );
        assert!(
            !fs::read_to_string(workspace.join("state/workspace.json"))
                .unwrap()
                .contains("\"format\"")
        );

        // Replay: every event's receipt is found, and resubmitting its
        // original request returns that receipt without a new event.
        let mut events = 0;
        for case in case_ids(&workspace) {
            let journal = workspace.join("state/cases").join(&case).join("events");
            let mut files: Vec<PathBuf> = fs::read_dir(&journal)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            files.sort();
            for file in files {
                let raw = fs::read(&file).unwrap();
                let event: Value = serde_json::from_slice(&raw).unwrap();
                let (exit, receipt) = run(
                    cwd,
                    &workspace,
                    &[
                        "receipt",
                        event["command_id"].as_str().unwrap(),
                        "--actor",
                        event["actor"].as_str().unwrap(),
                    ],
                );
                assert_eq!(exit, 0, "{name}: {receipt}");
                assert_eq!(receipt["transaction_id"], event["transaction_id"]);
                assert_eq!(
                    receipt["snapshot"],
                    json!({"sequence": event["sequence"], "head_hash": hash_bytes(&raw)})
                );
                if events < 3 {
                    let request = event["request"]["input"].to_string();
                    let mut args = vec!["--workspace", workspace.to_str().unwrap()];
                    args.extend(["record", "--input", "-"]);
                    let (exit, replay) = run_with(cwd, &args, Some(&request));
                    assert_eq!(exit, 0, "{name}: {replay}");
                    assert_eq!(replay["result"], "already_committed", "{name}");
                    assert_eq!(replay["transaction_id"], event["transaction_id"]);
                }
                events += 1;
            }
            let (exit, brief) = run(cwd, &workspace, &["--case", &case, "brief"]);
            assert_eq!(exit, 0, "{name}: {brief}");
        }

        // Derived state is rebuilt, never migrated by hand: an index of
        // any earlier version is replaced.
        let (exit, repaired) = run(cwd, &workspace, &["repair", "index"]);
        assert_eq!(exit, 0, "{name}: {repaired}");
        let (exit, check) = run(cwd, &workspace, &["check"]);
        assert_eq!(exit, 0, "{name}: {check}");
        assert_eq!(check["index"], "current", "{name}");

        // Render: the vault an older renderer wrote is brought up to date,
        // and the human folders keep their bytes.
        if workspace.join("vault").is_dir() {
            let (exit, rendered) = run(cwd, &workspace, &["vault", "render"]);
            assert_eq!(exit, 0, "{name}: {rendered}");
            assert_eq!(human(&workspace), notes, "{name}: human folders changed");
            assert!(!notes.is_empty(), "{name}: the fixture holds human notes");
            let (exit, status) = run(cwd, &workspace, &["vault", "status"]);
            assert_eq!(exit, 0, "{name}: {status}");
        }
        let (exit, plan) = run(cwd, &workspace, &["migrate"]);
        assert_eq!(exit, 0, "{name}: {plan}");
        assert_eq!(plan["status"], "current", "{name}");
        assert_eq!(plan["plan_id"], json!(null));
        assert_eq!(
            canonical(&workspace),
            before,
            "{name}: canonical bytes changed"
        );
    }
}

#[test]
fn brief_and_every_read_on_a_historical_workspace_change_no_file() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    for name in HISTORICAL {
        let workspace = historical(name, &cwd.join(name));
        let copied = tree(&workspace);
        warm(cwd, &workspace);
        let warmed = tree(&workspace);
        let refreshed: Vec<&String> = copied
            .keys()
            .chain(warmed.keys())
            .filter(|path| copied.get(*path) != warmed.get(*path))
            .collect();
        assert!(
            refreshed.iter().all(|path| path.starts_with(VERDICTS)),
            "{name}: the first brief wrote outside {VERDICTS}: {refreshed:?}"
        );
        let before = warmed;
        for case in case_ids(&workspace) {
            for args in [
                vec!["--case", &case, "brief"],
                vec!["--case", &case, "brief", "--level", "deep"],
                vec!["--case", &case, "show", &case],
                vec!["--case", &case, "next"],
            ] {
                let (exit, value) = run(cwd, &workspace, &args);
                assert_eq!(exit, 0, "{name} {args:?}: {value}");
            }
        }
        let mut workspace_reads = vec![vec!["check"], vec!["migrate"], vec!["doctor"]];
        if workspace.join("vault").is_dir() {
            workspace_reads.push(vec!["vault", "status"]);
        }
        for args in workspace_reads {
            let (exit, value) = run(cwd, &workspace, &args);
            assert_eq!(exit, 0, "{name} {args:?}: {value}");
        }
        assert_unchanged(
            &before,
            &tree(&workspace),
            &format!("{name}: a read changed a file"),
        );
    }
}

#[test]
fn a_newer_required_write_format_blocks_every_write_and_says_why_while_reads_work() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    for name in ["alpha-pilot-332e69ec", "v1-2026-11-24"] {
        let workspace = historical(name, &cwd.join(name));
        // A store opened before the marker changes still refuses: a commit
        // reads the format under the lock.
        let mut early = Store::open(Workspace::open(&workspace).unwrap());
        // A newer program wrote this: format 2, which it needs to write,
        // and a marker field this build does not know.
        set_format(
            &workspace,
            json!({"version": 2, "read_requires": 1, "write_requires": 2}),
            Some(("added_later", json!({"anything": 1}))),
        );
        warm(cwd, &workspace);
        let before = tree(&workspace);
        let case = case_ids(&workspace)[0].clone();

        let (exit, brief) = run(cwd, &workspace, &["--case", &case, "brief"]);
        assert_eq!(exit, 0, "{name}: {brief}");
        let (exit, check) = run(cwd, &workspace, &["check"]);
        assert_eq!(exit, 0, "{name}: {check}");
        assert!(issue_codes(&check).contains(&"WORKSPACE_FORMAT_NEWER".to_owned()));
        assert_eq!(check["format"]["write_requires"], 2);

        let refusals = [
            note(cwd, &workspace, &case, "blocked-note"),
            run(cwd, &workspace, &["repair", "index"]),
            run(cwd, &workspace, &["vault", "render"]),
            run(
                cwd,
                &workspace,
                &["start", "A new question", "--attach", "investigation.toml"],
            ),
        ];
        for (exit, refused) in refusals {
            assert_eq!(exit, 2, "{name}: {refused}");
            let message = refused["error"]["message"].as_str().unwrap();
            assert!(message.starts_with("WORKSPACE_FORMAT_NEWER:"), "{message}");
            assert!(
                message.contains("format 2") && message.contains("up to format 1"),
                "{message}"
            );
            assert!(message.contains("Reads still work"), "{message}");
            assert_eq!(
                refused["error"]["expected"]["reason"],
                "WORKSPACE_FORMAT_NEWER"
            );
        }
        let request = json!({
            "schema_version": "inv.command.v0-alpha",
            "actor": ACTOR,
            "command_id": "library-write",
            "case_id": null,
            "operations": [{"op": "case.create", "payload": {"mode": "explore", "title": "Blocked"}}],
            "preconditions": [],
        });
        let refused = early.record(request.to_string().as_bytes()).unwrap_err();
        assert!(
            refused.error.message.starts_with("WORKSPACE_FORMAT_NEWER:"),
            "{}",
            refused.error.message
        );
        assert_unchanged(
            &before,
            &tree(&workspace),
            &format!("{name}: a refused write changed a file"),
        );

        // A format this build cannot read refuses reads too, and says so.
        set_format(
            &workspace,
            json!({"version": 3, "read_requires": 3, "write_requires": 3}),
            None,
        );
        let before = tree(&workspace);
        for args in [vec!["--case", case.as_str(), "brief"], vec!["check"]] {
            let (exit, refused) = run(cwd, &workspace, &args);
            assert_ne!(exit, 0, "{name}: {refused}");
            let message = refused["error"]["message"].as_str().unwrap();
            assert!(
                message.contains("WORKSPACE_FORMAT_NEWER")
                    && message.contains("format 3")
                    && message.contains("up to format 1"),
                "{message}"
            );
        }
        let (_, doctor) = run(cwd, &workspace, &["doctor"]);
        assert!(
            doctor.to_string().contains("WORKSPACE_FORMAT_NEWER"),
            "{doctor}"
        );
        assert_unchanged(&before, &tree(&workspace), "unchanged");
    }
}

/// A test migration: format 2 writes `cache/format-probe` and then
/// `local/format-probe`, and needs format 2 to write. With `fail_once`, the
/// first run stops between the two files.
struct Probe {
    fail_once: AtomicBool,
}

impl Probe {
    fn registry(fail_once: bool) -> Registry {
        Registry::new(vec![Box::new(Probe {
            fail_once: AtomicBool::new(fail_once),
        })])
        .unwrap()
    }
}

impl Migration for Probe {
    fn version(&self) -> u32 {
        2
    }
    fn name(&self) -> &'static str {
        "0002-test-probe"
    }
    fn summary(&self) -> &'static str {
        "Test migration: write a probe file under cache/ and local/."
    }
    fn touches(&self) -> &'static [Area] {
        &[Area::Cache, Area::Local]
    }
    fn requires(&self) -> (u32, u32) {
        (1, 2)
    }
    fn describe(&self, workspace: &Workspace) -> Result<Vec<String>, MigrationError> {
        Ok(vec![format!(
            "write cache/format-probe and local/format-probe in {}",
            workspace.root().display()
        )])
    }
    fn apply(&self, workspace: &Workspace) -> Result<(), MigrationError> {
        let write = |relative: &str| {
            fs::write(workspace.root().join(relative), b"format 2\n")
                .map_err(|error| MigrationError(error.to_string()))
        };
        write("cache/format-probe")?;
        if self.fail_once.swap(false, Ordering::SeqCst) {
            return Err(MigrationError("the probe failed between its files".into()));
        }
        write("local/format-probe")
    }
}

fn store(workspace: &Path) -> Store {
    Store::open(Workspace::open(workspace).unwrap())
}

fn dry_run(workspace: &Path, registry: &Registry) -> MigrationPlan {
    migrate::plan(&mut store(workspace), registry).unwrap()
}

fn apply_until(
    workspace: &Path,
    registry: &Registry,
    plan_id: &str,
    stop: Option<Step>,
) -> Result<migrate::Applied, Stopped> {
    migrate::apply_until(
        &mut store(workspace),
        registry,
        plan_id,
        None,
        Vec::new(),
        stop,
    )
}

fn marker_format(workspace: &Path) -> Value {
    let marker: Value =
        serde_json::from_slice(&fs::read(workspace.join("state/workspace.json")).unwrap()).unwrap();
    marker["format"].clone()
}

#[test]
fn a_migration_runs_its_dry_run_plan_after_a_verified_backup_and_reverses_exactly() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let registry = Probe::registry(false);
    for name in ["alpha-pilot-bb3ab553", "v1-2026-11-24"] {
        let workspace = historical(name, &cwd.join(name));
        let original = canonical(&workspace);
        let notes = human(&workspace);
        let case = case_ids(&workspace)[0].clone();

        // The dry run writes nothing and names the plan.
        warm(cwd, &workspace);
        let before = tree(&workspace);
        let plan = dry_run(&workspace, &registry);
        assert_unchanged(
            &before,
            &tree(&workspace),
            &format!("{name}: the dry run wrote"),
        );
        assert_eq!(
            (plan.status, plan.from, plan.to),
            ("migration_available", 1, 2)
        );
        assert_eq!(plan.migrations.len(), 1);
        assert_eq!(plan.migrations[0].touches, ["cache", "local"]);
        assert!(
            plan.never_changed
                .iter()
                .any(|line| line.contains("case journals"))
        );
        let plan_id = plan.plan_id.clone().unwrap();
        assert!(plan_id.starts_with("mig_"));
        assert_eq!(dry_run(&workspace, &registry).plan_id.unwrap(), plan_id);

        // Only the plan the dry run named runs.
        match apply_until(&workspace, &registry, "mig_other", None) {
            Err(Stopped::Failed(failure)) => {
                assert!(
                    failure.error.message.contains("plan changed"),
                    "{failure:?}"
                )
            }
            other => panic!("{other:?}"),
        }
        assert_unchanged(
            &before,
            &tree(&workspace),
            &format!("{name}: a refused plan wrote"),
        );

        let applied = apply_until(&workspace, &registry, &plan_id, None).unwrap();
        assert!(!applied.backup_reused);
        assert!(applied.backup.starts_with(workspace.join("exports")));
        assert_eq!(
            marker_format(&workspace),
            json!({"version": 2, "read_requires": 1, "write_requires": 2})
        );
        let mut migrated = canonical(&workspace);
        let marker = migrated.remove("state/workspace.json").unwrap();
        let mut expected = original.clone();
        expected.remove("state/workspace.json");
        assert_eq!(
            migrated, expected,
            "{name}: a journal, blob or the configuration changed"
        );
        assert!(workspace.join("cache/format-probe").is_file());
        assert!(workspace.join("local/format-probe").is_file());
        assert_eq!(human(&workspace).len(), notes.len());
        let (exit, check) = run(cwd, &workspace, &["check"]);
        assert_eq!(exit, 0, "{name}: {check}");
        assert_eq!(check["heads"], json!(applied.check.heads));

        // The backup verifies on its own, and the migrated workspace is at a
        // format this build reads and does not write.
        let backup = applied.backup.to_str().unwrap().to_owned();
        let (exit, verified) = run_with(cwd, &["archive", "verify", &backup], None);
        assert_eq!(exit, 0, "{verified}");
        assert_eq!(verified["export_id"], json!(applied.backup_export_id));
        let (exit, brief) = run(cwd, &workspace, &["--case", &case, "brief"]);
        assert_eq!(exit, 0, "{brief}");
        let (exit, refused) = note(cwd, &workspace, &case, "after-migration");
        assert_eq!(exit, 2, "{refused}");
        assert!(refused.to_string().contains("WORKSPACE_FORMAT_NEWER"));

        // Reverse: the backup restores the workspace as it was, byte for
        // byte in every canonical file, the marker included.
        let reversed = cwd.join(format!("{name}-reversed"));
        let (exit, restored) = run_with(
            cwd,
            &["archive", "restore", &backup, reversed.to_str().unwrap()],
            None,
        );
        assert_eq!(exit, 0, "{restored}");
        assert_eq!(
            canonical(&reversed),
            original,
            "{name}: the reverse is not exact"
        );
        assert_ne!(marker, original["state/workspace.json"]);
        assert_eq!(human(&reversed), notes, "{name}: human notes");
        let (exit, check) = run(cwd, &reversed, &["check"]);
        assert_eq!(exit, 0, "{check}");
        assert_eq!(check["heads"], json!(applied.check.heads));
        let (exit, noted) = note(cwd, &reversed, &case, "after-reverse");
        assert_eq!(exit, 0, "{noted}");

        // Nothing left to do once migrated.
        let again = dry_run(&workspace, &registry);
        assert_eq!((again.status, again.plan_id), ("current", None));
    }
}

#[test]
fn a_migration_stopped_at_any_step_is_accepted_or_refused_by_check_and_a_retry_finishes() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let registry = Probe::registry(false);
    let reference = historical("v1-2026-11-24", &cwd.join("reference"));
    let original = canonical(&reference);
    let plan_id = dry_run(&reference, &registry).plan_id.unwrap();
    apply_until(&reference, &registry, &plan_id, None).unwrap();
    let finished = marker_format(&reference);
    let case = case_ids(&reference)[0].clone();

    let steps = [
        (Step::BackedUp, false),
        (Step::Verified, false),
        (Step::Marked, true),
        (Step::Applied(2), true),
        (Step::Rebuilt, true),
        (Step::Checked, true),
        (Step::Published, false),
    ];
    for (index, (step, marked)) in steps.into_iter().enumerate() {
        let workspace = historical("v1-2026-11-24", &cwd.join(format!("stop-{index}")));
        assert_eq!(dry_run(&workspace, &registry).plan_id.unwrap(), plan_id);
        match apply_until(&workspace, &registry, &plan_id, Some(step)) {
            Err(Stopped::Interrupted(at)) => assert_eq!(at, step),
            other => panic!("{step:?}: {other:?}"),
        }
        let (exit, check) = run(cwd, &workspace, &["check"]);
        if marked {
            // Refused clearly, with the command that finishes it, and every
            // write refused meanwhile.
            assert_eq!(exit, 6, "{step:?}: {check}");
            let issue = check["issues"]
                .as_array()
                .unwrap()
                .iter()
                .find(|issue| issue["code"] == "MIGRATION_INCOMPLETE")
                .unwrap_or_else(|| panic!("{step:?}: {check}"));
            assert!(issue["repair"].to_string().contains("migrate"), "{issue}");
            assert_eq!(check["structurally_valid"], true);
            let (exit, refused) = note(cwd, &workspace, &case, "during");
            assert_eq!(exit, 2, "{step:?}: {refused}");
            assert!(
                refused.to_string().contains("MIGRATION_INCOMPLETE"),
                "{refused}"
            );
            let (exit, plan) = run(cwd, &workspace, &["migrate"]);
            assert_eq!(exit, 0, "{plan}");
            assert_eq!(plan["status"], "migration_incomplete");
            let (exit, brief) = run(cwd, &workspace, &["--case", &case, "brief"]);
            assert_eq!(exit, 0, "{step:?}: reads still work: {brief}");
        } else {
            assert_eq!(exit, 0, "{step:?}: {check}");
            assert!(!issue_codes(&check).contains(&"MIGRATION_INCOMPLETE".to_owned()));
        }
        // A retry with the same plan finishes, reusing the backup.
        if step != Step::Published {
            let plan = dry_run(&workspace, &registry);
            assert_eq!(plan.plan_id.as_deref(), Some(plan_id.as_str()), "{step:?}");
            let applied = apply_until(&workspace, &registry, &plan_id, None)
                .unwrap_or_else(|stopped| panic!("{step:?}: {stopped:?}"));
            assert!(applied.backup_reused, "{step:?}");
        }
        assert_eq!(marker_format(&workspace), finished, "{step:?}");
        let (exit, check) = run(cwd, &workspace, &["check"]);
        assert_eq!(exit, 0, "{step:?}: {check}");
        let mut now = canonical(&workspace);
        now.remove("state/workspace.json");
        let mut expected = original.clone();
        expected.remove("state/workspace.json");
        assert_eq!(now, expected, "{step:?}");
        let backups = fs::read_dir(workspace.join("exports")).unwrap().count();
        assert_eq!(backups, 1, "{step:?}: one backup");
    }
}

#[test]
fn a_migration_that_fails_part_way_is_refused_clearly_and_a_retry_finishes() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let registry = Probe::registry(true);
    let workspace = historical("alpha-pilot-332e69ec", &cwd.join("failing"));
    let case = case_ids(&workspace)[0].clone();
    let plan_id = dry_run(&workspace, &registry).plan_id.unwrap();
    let failure = match apply_until(&workspace, &registry, &plan_id, None) {
        Err(Stopped::Failed(failure)) => failure,
        other => panic!("{other:?}"),
    };
    assert!(
        failure.error.message.contains("the probe failed"),
        "{failure:?}"
    );
    assert!(
        failure.error.message.contains("archive restore"),
        "{failure:?}"
    );
    assert!(workspace.join("cache/format-probe").is_file());
    assert!(!workspace.join("local/format-probe").exists());
    let (exit, check) = run(cwd, &workspace, &["check"]);
    assert_eq!(exit, 6, "{check}");
    assert!(issue_codes(&check).contains(&"MIGRATION_INCOMPLETE".to_owned()));
    let (exit, _) = note(cwd, &workspace, &case, "during-failure");
    assert_eq!(exit, 2);

    let applied = apply_until(&workspace, &registry, &plan_id, None).unwrap();
    assert!(applied.backup_reused);
    assert!(workspace.join("local/format-probe").is_file());
    let (exit, check) = run(cwd, &workspace, &["check"]);
    assert_eq!(exit, 0, "{check}");
}

#[test]
fn a_change_after_the_dry_run_voids_the_plan_and_nothing_is_marked_or_backed_up() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let registry = Probe::registry(false);
    let workspace = historical("v1-2026-11-24", &cwd.join("changed"));
    let case = case_ids(&workspace)[0].clone();
    let plan_id = dry_run(&workspace, &registry).plan_id.unwrap();
    let (exit, noted) = note(cwd, &workspace, &case, "after-dry-run");
    assert_eq!(exit, 0, "{noted}");
    match apply_until(&workspace, &registry, &plan_id, None) {
        Err(Stopped::Failed(failure)) => {
            assert!(
                failure.error.message.contains("plan changed"),
                "{failure:?}"
            );
            assert_eq!(failure.error.expected, Some(json!({"plan_id": plan_id})));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(marker_format(&workspace), json!(null));
    assert_eq!(fs::read_dir(workspace.join("exports")).unwrap().count(), 0);
}

#[test]
fn an_unfinished_backup_is_left_alone_and_the_next_name_is_used() {
    let directory = tempfile::tempdir().unwrap();
    let registry = Probe::registry(false);
    let workspace = historical("alpha-pilot-332e69ec", &directory.path().join("partial"));
    let plan = dry_run(&workspace, &registry);
    // What a kill while the backup was written leaves: files, no manifest.
    fs::create_dir_all(&plan.default_backup).unwrap();
    fs::write(plan.default_backup.join(".partial"), b"x").unwrap();
    let applied = apply_until(
        &workspace,
        &registry,
        plan.plan_id.as_deref().unwrap(),
        None,
    )
    .unwrap();
    let mut expected = plan.default_backup.clone().into_os_string();
    expected.push("-2");
    assert_eq!(applied.backup, PathBuf::from(expected));
    assert_eq!(
        fs::read(plan.default_backup.join(".partial")).unwrap(),
        b"x"
    );

    // A named backup directory that holds something else is refused, and
    // nothing changes.
    let other = historical("alpha-pilot-bb3ab553", &directory.path().join("named"));
    let plan = dry_run(&other, &registry);
    let named = directory.path().join("occupied");
    fs::create_dir_all(&named).unwrap();
    fs::write(named.join("keep.txt"), b"mine").unwrap();
    let before = tree(&other);
    match migrate::apply(
        &mut store(&other),
        &registry,
        plan.plan_id.as_deref().unwrap(),
        Some(&named),
        Vec::new(),
    ) {
        Err(Stopped::Failed(failure)) => {
            assert!(
                failure.error.message.contains("choose another --backup"),
                "{failure:?}"
            )
        }
        other => panic!("{other:?}"),
    }
    assert_unchanged(&before, &tree(&other), "unchanged");
}

#[test]
fn a_migration_this_program_does_not_know_is_refused_clearly() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let workspace = historical("v1-2026-11-24", &cwd.join("unknown"));
    set_format(
        &workspace,
        json!({"version": 1, "read_requires": 1, "write_requires": 1, "migrating": {
            "from": 1, "to": 7, "plan_id": "mig_elsewhere", "backup_export_id": "exp_elsewhere"}}),
        None,
    );
    warm(cwd, &workspace);
    let before = tree(&workspace);
    let (exit, plan) = run(cwd, &workspace, &["migrate"]);
    assert_eq!(exit, 0, "{plan}");
    assert_eq!(plan["status"], "migration_incomplete");
    assert!(
        plan["refusal"].as_str().unwrap().contains("exp_elsewhere"),
        "{plan}"
    );
    assert_eq!(plan["plan_id"], json!(null));
    let (exit, applied) = run(
        cwd,
        &workspace,
        &["migrate", "--apply", "--plan", "mig_elsewhere"],
    );
    assert_eq!(exit, 2, "{applied}");
    let (exit, check) = run(cwd, &workspace, &["check"]);
    assert_eq!(exit, 6, "{check}");
    assert!(issue_codes(&check).contains(&"MIGRATION_INCOMPLETE".to_owned()));
    assert_unchanged(&before, &tree(&workspace), "unchanged");
    // The builtin registry has no migration: format 1 is current.
    assert_eq!(Registry::builtin().latest(), 1);
    assert_eq!(WorkspaceFormat::BASELINE.version, 1);
}
