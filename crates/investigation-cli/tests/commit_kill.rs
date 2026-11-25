//! Real committing writer processes killed with SIGKILL at every point of a
//! commit, then recovered (bn-dla3; plan §10.4 crash matrix, §20.2 P1 and
//! P19).
//!
//! The binary is built with debug assertions, so the commit pause of
//! `investigation_store::test_pause` is compiled in: the process marks a
//! ready file when it reaches the named point and waits there until it is
//! killed. The points are the start of each commit stage and
//! `journal_linked`, the event in the journal before its directory is
//! synced. At each one:
//!
//! - `check` finds nothing wrong but, at most, a stale disposable index;
//! - the receipt lookup finds the command exactly when its event reached
//!   the journal, with that event's transaction ID;
//! - the identical retry commits it once (`committed`), or returns the
//!   original receipt (`already_committed`, the same transaction and
//!   created IDs) without a new event;
//! - afterwards `check` reports no issue and the journal holds the command
//!   once.
//!
//! The first matrix creates a case with an attachment (blob publication and
//! the new case directory, the `start` command). The second is drill 6's
//! closure batch, `retrospective.record` plus `case.close` on a material
//! case (P19): at every point the case is closed with its retrospective or
//! open without one.
//!
//! A process kill keeps what the kernel has cached, so the directory sync
//! after `journal_linked` is not tested against a power loss here; the
//! crash-model tests of the store and the render power-loss model cover
//! that. Every run clears the environment and puts an empty folder on
//! `PATH`.

use std::{
    fs,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use investigation_core::identity::{Id, Kind};
use investigation_store::{
    Store, Workspace,
    test_pause::{PAUSE_COMMIT, PAUSE_READY},
};
use serde_json::{Value, json};

#[path = "support/pause.rs"]
mod pause;
#[path = "support/restore.rs"]
mod restore;

use restore::{run, run_with};

const ACTOR: &str = "agent:commit-kill";
const ATTACHMENT: &str = "Reading 12.5 units\n";
const ATTACHMENT_DIGEST: &str = "c74dfcceaf6ee5b53fcd41b902eb73f8ebf1fcf94bd0dd5472df00b7e3da686e";

/// Every point a kill stops the `start` commit at, in order, and whether
/// its event is in the journal there.
const NEW_CASE: [(&str, bool); 11] = [
    ("lock", false),
    ("verify_tail", false),
    ("idempotency", false),
    ("validate", false),
    ("blob_publish", false),
    ("journal_write", false),
    ("directory_sync", false),
    ("journal_publish", false),
    ("journal_linked", true),
    ("index_update", true),
    ("receipt", true),
];

/// The same for an event appended to an existing case: its publication
/// links the event file and syncs the directory in one step.
const APPEND: [(&str, bool); 10] = [
    ("lock", false),
    ("verify_tail", false),
    ("idempotency", false),
    ("validate", false),
    ("blob_publish", false),
    ("journal_write", false),
    ("journal_publish", false),
    ("journal_linked", true),
    ("index_update", true),
    ("receipt", true),
];

/// The binary on `args` (with `--format json`) in `cwd`, `stdin` piped in,
/// with a cleared environment and an empty `PATH`.
fn command(cwd: &Path, args: &[&str]) -> Command {
    let empty_path = cwd.join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_clear()
        .env("PATH", &empty_path)
        .args(on(args))
        .args(["--format", "json"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Run `args` until the commit reaches `point`, then SIGKILL it there.
fn kill_at(cwd: &Path, args: &[&str], stdin: Option<&[u8]>, point: &str) {
    let ready = cwd.join(".commit-paused");
    let _ = fs::remove_file(&ready);
    let mut child = command(cwd, args)
        .env(PAUSE_COMMIT, point)
        .env(PAUSE_READY, &ready)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .spawn()
        .unwrap();
    if let Some(bytes) = stdin {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    pause::wait_for_ready(&mut child, &ready, point, &format!("{args:?}"));
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.signal(), Some(9), "{point}: {status}");
    fs::remove_file(&ready).unwrap();
}

/// A folder `root/name` holding the attachment and a new workspace,
/// `casework`, that imports from the folder; commands run in the folder.
fn workspace(root: &Path, name: &str) -> PathBuf {
    let folder = root.join(name);
    fs::create_dir(&folder).unwrap();
    let (exit, value) = run(
        &folder,
        &["init", "./casework", "--no-vault", "--import-root", "."],
    );
    assert_eq!(exit, 0, "{value}");
    fs::write(folder.join("reading.txt"), ATTACHMENT).unwrap();
    folder
}

/// `args` after `--workspace casework`.
fn on(args: &[&str]) -> Vec<String> {
    ["--workspace", "casework"]
        .iter()
        .chain(args)
        .map(|arg| (*arg).to_owned())
        .collect()
}

/// `run` of `args` on the workspace in `folder`.
fn inv(folder: &Path, args: &[&str]) -> (i32, Value) {
    let args = on(args);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run(folder, &args)
}

/// `run_with` of `args` on the workspace in `folder`.
fn inv_with(folder: &Path, args: &[&str], stdin: &[u8]) -> (i32, Value) {
    let args = on(args);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_with(folder, &args, Some(stdin))
}

/// The events of every case, by file.
fn events(workspace: &Path) -> Vec<Value> {
    let mut found = Vec::new();
    let cases = workspace.join("casework/state/cases");
    for case in fs::read_dir(cases).unwrap() {
        let events = case.unwrap().path().join("events");
        let mut names: Vec<_> = fs::read_dir(&events)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        names.sort();
        for name in names {
            found.push(serde_json::from_slice(&fs::read(name).unwrap()).unwrap());
        }
    }
    found
}

/// `check` passes, with no issue but at most a stale index when `stale` is
/// allowed.
fn assert_checked(workspace: &Path, point: &str, stale: bool) {
    let (exit, report) = inv(workspace, &["check"]);
    assert_eq!(exit, 0, "{point}: {report:#}");
    assert_eq!(report["structurally_valid"], true, "{point}: {report:#}");
    for issue in report["issues"].as_array().unwrap() {
        assert!(
            stale && issue["code"] == "INDEX_STALE" && issue["severity"] == "warning",
            "{point}: {issue:#}"
        );
    }
}

/// The receipt lookup of `key`: the committed receipt, or `None` when the
/// command is not in the journal.
fn lookup(workspace: &Path, key: &str, point: &str) -> Option<Value> {
    let (exit, found) = inv(workspace, &["receipt", key, "--actor", ACTOR]);
    assert_eq!(exit, 0, "{point}: {found:#}");
    match found["result"].as_str().unwrap() {
        "committed" => Some(found),
        "not_found_at_snapshot" => None,
        other => panic!("{point}: {other}: {found:#}"),
    }
}

/// After a kill at `point`: the journal holds `key` exactly when
/// `committed`, recovery is honest, and the identical retry (`retry`)
/// commits it once. Returns the retry's receipt.
fn recover(
    workspace: &Path,
    key: &str,
    point: &str,
    committed: bool,
    before: usize,
    retry: impl Fn() -> (i32, Value),
) -> Value {
    assert_checked(workspace, point, true);
    let journal = events(workspace);
    let found = lookup(workspace, key, point);
    let mine: Vec<&Value> = journal
        .iter()
        .filter(|event| event["command_id"] == key)
        .collect();
    assert_eq!(mine.len(), usize::from(committed), "{point}");
    assert_eq!(journal.len(), before + usize::from(committed), "{point}");
    assert_eq!(found.is_some(), committed, "{point}: {found:?}");
    if let (Some(found), Some(event)) = (&found, mine.first()) {
        assert_eq!(found["transaction_id"], event["transaction_id"], "{point}");
    }

    let (exit, retried) = retry();
    assert_eq!(exit, 0, "{point}: {retried:#}");
    let expected = if committed {
        "already_committed"
    } else {
        "committed"
    };
    assert_eq!(retried["result"], expected, "{point}: {retried:#}");
    if let Some(found) = &found {
        assert_eq!(
            retried["transaction_id"], found["transaction_id"],
            "{point}"
        );
        assert_eq!(retried["created"], found["created"], "{point}");
    }
    let journal = events(workspace);
    assert_eq!(journal.len(), before + 1, "{point}");
    let mine: Vec<&Value> = journal
        .iter()
        .filter(|event| event["command_id"] == key)
        .collect();
    assert_eq!(mine.len(), 1, "{point}");
    assert_eq!(mine[0]["transaction_id"], retried["transaction_id"]);
    let found = lookup(workspace, key, point).expect("committed after the retry");
    assert_eq!(
        found["transaction_id"], retried["transaction_id"],
        "{point}"
    );
    assert_checked(workspace, point, false);
    retried
}

/// P1: `start` with an attachment, a new case and its blobs, killed at
/// every point of its commit.
#[test]
fn a_writer_killed_at_every_commit_point_recovers_its_receipt_or_commits_once_on_retry() {
    let root = tempfile::tempdir().unwrap();
    let args = [
        "start",
        "Can the reading be interpreted?",
        "--attach",
        "reading.txt",
        "--command-id",
        "start",
        "--actor",
        ACTOR,
    ];
    for (point, committed) in NEW_CASE {
        let workspace = workspace(root.path(), point);
        kill_at(&workspace, &args, None, point);
        let blob = workspace
            .join("casework/state/blobs/sha256")
            .join(&ATTACHMENT_DIGEST[..2])
            .join(ATTACHMENT_DIGEST);
        // The attachment is published before the event, never after it.
        if committed {
            assert_eq!(fs::read_to_string(&blob).unwrap(), ATTACHMENT, "{point}");
        }
        let retried = recover(&workspace, "start", point, committed, 0, || {
            inv(&workspace, &args)
        });
        assert_eq!(fs::read_to_string(&blob).unwrap(), ATTACHMENT, "{point}");
        let case = retried["created"][0]["id"].as_str().unwrap();
        let (exit, shown) = inv(&workspace, &["show", case]);
        assert_eq!(exit, 0, "{point}: {shown:#}");
        // A temporary a kill left (the private copy of the import, a staged
        // event or case directory) is not in the journal and never blocks a
        // writer; reclaiming it waits for the grace period.
        for entry in fs::read_dir(workspace.join("casework/state/tmp")).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            assert!(
                ["import-import_", "create-tx_", "event-tx_"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix)),
                "{point}: {name}"
            );
        }
    }
}

/// A measured case with one failed Run, and its closure batch.
fn material_case(workspace: &Path) -> Value {
    let submit = |key: &str, case: Option<&str>, operations: Value, pre: Value| {
        let command = json!({
            "schema_version":"inv.command.v1","command_id":key,"case_id":case,
            "actor":ACTOR,"preconditions":pre,"operations":operations,
        });
        let bytes = serde_json::to_vec(&command).unwrap();
        let (exit, receipt) = inv_with(workspace, &["record", "--input", "-"], &bytes);
        assert_eq!(exit, 0, "{receipt:#}");
        receipt
    };
    let created = submit(
        "case",
        None,
        json!([{"op":"case.create","payload":{"title":"Is the benchmark valid?","mode":"measure"}}]),
        json!([]),
    );
    let case = created["created"][0]["id"].as_str().unwrap().to_owned();
    let ran = submit(
        "run",
        Some(&case),
        json!([{"op":"run.record","payload":{
            "intent":{"kind":"exploration","intent":"Run the benchmark once."},
            "producer":"agent:fixture","execution_status":"failed","evidence_completeness":"complete",
            "evidence_kind":"observational","variables_changed":[],"artifact_refs":[],
        }}]),
        json!([]),
    );
    let run = ran["created"][0]["id"].as_str().unwrap();
    let case_ref = json!({"id":case,"revision":1});
    json!({
        "schema_version":"inv.command.v1","command_id":"close","case_id":case,
        "actor":ACTOR,"preconditions":[case_ref],"operations":[
            {"op":"retrospective.record","client_ref":"account","payload":{
                "case_ref":case_ref,"snapshot":ran["snapshot"],
                "methods_attempted":[{"name":"Single benchmark run"}],"effective_steps":[],
                "invalid_or_misleading_steps":[{"description":"The run failed before measuring.","support_refs":[run]}],
                "costs_or_constraints":"One run.","residual_unknowns":["Whether a rerun succeeds."],
                "lessons":[],"no_reusable_lesson_reason":"One failed run teaches nothing portable.",
            }},
            {"op":"case.close","payload":{
                "outcome":"inconclusive","reason":"The only run failed.","unresolved_questions":[],
                "reopen_condition":"A run completes.","retrospective_ref":{"client_ref":"account"},
            }}
        ],
    })
}

/// Whether the case is closed, which retrospective its closure cites, and
/// every retrospective it holds, read through a verified replay.
fn closure(workspace: &Path, case: &str) -> (bool, Option<Id>, Vec<Id>) {
    let mut store = Store::open(Workspace::open(&workspace.join("casework")).unwrap());
    let id: Id = case.parse().unwrap();
    let replay = store.case(id).unwrap();
    let body = &replay.state.current(id).unwrap().body;
    (
        body["lifecycle"] == "closed",
        body["closure"]["retrospective_ref"]["id"]
            .as_str()
            .map(|id| id.parse().unwrap()),
        replay
            .state
            .records()
            .filter(|record| record.header.id.kind() == Kind::Retrospective)
            .map(|record| record.header.id)
            .collect(),
    )
}

/// P19: the closure batch killed at every point of its commit leaves the
/// case closed with its retrospective or open without one, and the retry
/// closes it once with the acknowledged IDs.
#[test]
fn a_closure_batch_killed_at_every_commit_point_leaves_both_or_neither() {
    let root = tempfile::tempdir().unwrap();
    for (point, committed) in APPEND {
        let workspace = workspace(root.path(), point);
        let batch = material_case(&workspace);
        let case = batch["case_id"].as_str().unwrap().to_owned();
        let bytes = serde_json::to_vec(&batch).unwrap();
        let args = ["record", "--input", "-"];
        kill_at(&workspace, &args, Some(&bytes), point);
        let (closed, cited, retrospectives) = closure(&workspace, &case);
        assert_eq!(closed, committed, "{point}");
        assert_eq!(retrospectives.len(), usize::from(committed), "{point}");
        assert_eq!(cited, retrospectives.first().copied(), "{point}");

        let retried = recover(&workspace, "close", point, committed, 2, || {
            inv_with(&workspace, &args, &bytes)
        });
        let (closed, cited, retrospectives) = closure(&workspace, &case);
        assert!(closed, "{point}");
        assert_eq!(retrospectives.len(), 1, "{point}");
        assert_eq!(cited, Some(retrospectives[0]), "{point}");
        let acknowledged: Vec<&str> = retried["created"]
            .as_array()
            .unwrap()
            .iter()
            .map(|change| change["id"].as_str().unwrap())
            .collect();
        assert!(
            acknowledged.contains(&retrospectives[0].to_string().as_str()),
            "{point}: {retried:#}"
        );
    }
}

/// A child that creates the ready file empty and writes its point later, as
/// a create-then-write child did under load (bn-38ue): the parent waits for
/// the whole point instead of reading the empty file. The child writes only
/// when the test says so, so no step depends on timing (bn-2uoc). Every wait
/// is bounded, every failure says what the files held, and the child is
/// killed and reaped on every path.
#[test]
fn the_ready_handshake_waits_for_the_whole_point_name() {
    let bound = Duration::from_secs(30);
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join(".commit-paused");
    let go = root.path().join("go");
    // Exit codes 3 and 4 name a failed create or write of the ready file, so
    // such a failure ends the wait at once instead of after the child's life.
    let script = r#": > "$1" || exit 3
while [ ! -e "$2" ]; do sleep 0.01; done
printf lock > "$1" || exit 4
exec sleep 300"#;
    let mut child = pause::Reaped(
        Command::new("/bin/sh")
            .args(["-c", script, "sh"])
            .arg(&ready)
            .arg(&go)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // The child has created the file and waits for `go`: it is empty.
    let started = Instant::now();
    while !ready.exists() {
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!("sh exited with {status} before creating the ready file");
        }
        assert!(
            started.elapsed() < bound,
            "sh did not create the ready file within {bound:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fs::read_to_string(&ready).unwrap(), "");

    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| {
            pause::wait_for_ready_within(&mut child.0, &ready, "lock", "sh", bound);
        });
        // The file stays empty until `go`, so the wait cannot have returned.
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !waiting.is_finished(),
            "the wait returned on an empty ready file"
        );
        fs::write(&go, "").unwrap();
        if let Err(panic) = waiting.join() {
            std::panic::resume_unwind(panic);
        }
    });
    assert_eq!(fs::read_to_string(&ready).unwrap(), "lock");
}

/// A paused process marks its ready file in one rename: the file holds the
/// whole point and no temporary file is left beside it.
#[cfg(debug_assertions)]
#[test]
fn a_paused_process_marks_the_ready_file_whole() {
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join(".commit-paused");
    investigation_store::test_pause::mark_ready(&ready, "journal_linked");
    assert_eq!(fs::read_to_string(&ready).unwrap(), "journal_linked");
    let names: Vec<_> = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [".commit-paused"]);
}
