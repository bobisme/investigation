//! `archive restore` and `archive fork` killed with SIGKILL as real
//! processes at every step, and `--resume` (bn-f97f).
//!
//! The binary is built with debug assertions, so the test pause of
//! `investigation_render::export::restore` is compiled in: the process marks
//! a ready file at the named step and waits there until it is killed. After
//! each kill, PATH holds no workspace marker and nothing the restore does not
//! own (its staging folder and the entries its `moved.json` lists), or it is
//! the complete, checked workspace; a plain run refuses it and points to
//! `--resume`; `--resume` clears it and restores, also when it is itself
//! killed part way. Every refusal of `--resume` removes nothing.
//!
//! Every run clears the environment and puts an empty folder on `PATH`.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::{fs::symlink, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use investigation_render::export::restore::{PAUSE_AT, PAUSE_READY, STAGING};
use serde_json::Value;

#[path = "support/restore.rs"]
mod restore;

use restore::{archive, check, files, run};

const ACTOR: &str = "agent:restore-kill";
/// Every step a kill can stop a restore at, in order, and whether a fork
/// only reaches it.
const STEPS: [(&str, bool); 7] = [
    ("Staging", false),
    ("Verified", false),
    ("Forked", true),
    ("Checked", false),
    ("Moving", false),
    ("Publishing", false),
    ("Published", false),
];

/// A small workspace with two cases, and its archive.
fn archived(root: &Path, name: &str) -> PathBuf {
    let workspace = root.join(name);
    let (exit, value) = run(root, &["init", workspace.to_str().unwrap(), "--no-vault"]);
    assert_eq!(exit, 0, "{value}");
    for key in ["first", "second"] {
        let (exit, value) = run(
            root,
            &[
                "--workspace",
                workspace.to_str().unwrap(),
                "case",
                "new",
                &format!("The {key} question of {name}"),
                "--actor",
                ACTOR,
                "--command-id",
                key,
            ],
        );
        assert_eq!(exit, 0, "{value}");
    }
    let out = root.join(format!("{name}-archive"));
    archive(root, &workspace, &out);
    out
}

fn command(root: &Path, verb: &str, dir: &Path, path: &Path, resume: bool) -> Command {
    let empty_path = root.join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(root)
        .env_clear()
        .env("PATH", &empty_path)
        .args([
            "archive",
            verb,
            dir.to_str().unwrap(),
            path.to_str().unwrap(),
        ])
        .args(resume.then_some("--resume"))
        .args(["--format", "json"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Start the command paused at `step`; return once it is there.
fn paused(root: &Path, verb: &str, dir: &Path, path: &Path, resume: bool, step: &str) -> Child {
    let ready = root.join(format!(
        "ready-{}",
        path.file_name().unwrap().to_str().unwrap()
    ));
    let _ = fs::remove_file(&ready);
    let mut child = command(root, verb, dir, path, resume)
        .env(PAUSE_AT, step)
        .env(PAUSE_READY, &ready)
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !ready.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("{verb} exited with {status} before {step}");
        }
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "{verb} never reached {step}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::remove_file(&ready).unwrap();
    child
}

fn kill(mut child: Child) {
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.signal(), Some(9), "{status}");
}

fn resume(root: &Path, verb: &str, dir: &Path, path: &Path) -> (i32, Value) {
    let mut args = vec![
        "archive",
        verb,
        dir.to_str().unwrap(),
        path.to_str().unwrap(),
    ];
    args.push("--resume");
    run(root, &args)
}

fn publish(root: &Path, verb: &str, dir: &Path, path: &Path) -> (i32, Value) {
    run(
        root,
        &[
            "archive",
            verb,
            dir.to_str().unwrap(),
            path.to_str().unwrap(),
        ],
    )
}

fn top_level(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

/// What PATH holds after a kill, checked against what the restore owns.
fn assert_owned_or_complete(root: &Path, path: &Path, step: &str, heads: &Value) {
    let (exit, checked) = check(root, path);
    if step == "Published" {
        assert_eq!(exit, 0, "{step}: {checked}");
        assert_eq!(&checked["heads"], heads, "{step}");
        return;
    }
    assert_ne!(exit, 0, "{step}: {checked}");
    assert!(!path.join("state/workspace.json").exists(), "{step}");
    let staging = path.join(STAGING);
    let moved: Vec<String> = fs::read(staging.join("moved.json"))
        .ok()
        .map(|bytes| {
            let moved: Value = serde_json::from_slice(&bytes).unwrap();
            moved["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry.as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default();
    for name in top_level(path) {
        assert!(
            name == STAGING || moved.contains(&name),
            "{step}: {name} in PATH is not the restore's"
        );
    }
    let owner: Value =
        serde_json::from_slice(&fs::read(staging.join("owner.json")).unwrap()).unwrap();
    assert_eq!(owner["kind"], "investigation.restore-staging", "{step}");
    if step == "Moving" || step == "Publishing" {
        assert!(top_level(path).len() > 1, "{step}: entries moved");
    }
}

#[test]
fn a_restore_killed_at_every_step_is_resumed_to_the_same_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let out = archived(root, "source");
    let (_, original) = check(root, &root.join("source"));
    for verb in ["restore", "fork"] {
        for (step, fork_only) in STEPS {
            if fork_only && verb == "restore" {
                continue;
            }
            let path = root.join(format!("{verb}-{step}"));
            kill(paused(root, verb, &out, &path, false, step));
            assert_owned_or_complete(root, &path, step, &original["heads"]);

            // A plain run refuses the interrupted PATH and writes nothing.
            let before = files(&path, &[]);
            let (exit, refused) = publish(root, verb, &out, &path);
            assert_eq!(exit, 2, "{verb} {step}: {refused}");
            let message = refused["error"]["message"].as_str().unwrap();
            assert!(message.contains("--resume"), "{message}");
            assert_eq!(files(&path, &[]), before);

            // A resume killed after it cleared PATH's entries resumes too.
            if step == "Moving" || step == "Publishing" {
                kill(paused(root, verb, &out, &path, true, "Cleared"));
                assert_eq!(top_level(&path), [STAGING], "{verb} {step}");
            }

            let (exit, resumed) = resume(root, verb, &out, &path);
            assert_eq!(exit, 0, "{verb} {step}: {resumed}");
            let expected = match (step, verb) {
                ("Published", _) => "already_published",
                (_, "restore") => "restored",
                _ => "forked",
            };
            assert_eq!(resumed["result"], expected, "{verb} {step}");
            assert!(!path.join(STAGING).exists(), "{verb} {step}");
            let (exit, checked) = check(root, &path);
            assert_eq!(exit, 0, "{verb} {step}: {checked}");
            assert_eq!(checked["heads"], original["heads"], "{verb} {step}");
            let marker = restore::marker(&path);
            if verb == "restore" {
                assert_eq!(marker["workspace_id"], original_id(root), "{step}");
                assert!(marker.get("fork").is_none(), "{step}");
            } else {
                assert_ne!(marker["workspace_id"], original_id(root), "{step}");
                assert!(marker["fork"].is_object(), "{step}");
            }
            // Resuming a complete PATH is refused: it is no interrupted run.
            let (exit, refused) = resume(root, verb, &out, &path);
            assert_eq!(exit, 2, "{verb} {step}: {refused}");
        }
    }
}

fn original_id(root: &Path) -> Value {
    restore::marker(&root.join("source"))["workspace_id"].clone()
}

/// An interrupted restore of `out` at `path`, killed at `Checked`.
fn interrupted(root: &Path, verb: &str, out: &Path, path: &Path) {
    kill(paused(root, verb, out, path, false, "Checked"));
}

fn assert_refused(
    root: &Path,
    verb: &str,
    out: &Path,
    path: &Path,
    exit: i32,
    needle: &str,
) -> BTreeMap<String, Vec<u8>> {
    let before = files(path, &[]);
    let (code, refused) = resume(root, verb, out, path);
    assert_eq!(code, exit, "{}: {refused}", path.display());
    assert_eq!(refused["error"]["path"], "PATH", "{refused}");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains(needle), "{needle}: {message}");
    if exit == 2 {
        assert!(message.contains("Nothing was removed"), "{message}");
    }
    assert_eq!(
        files(path, &[]),
        before,
        "{}: a refusal removed something",
        path.display()
    );
    before
}

#[test]
fn resume_refuses_everything_a_restore_of_this_archive_did_not_write() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let out = archived(root, "source");
    let other = archived(root, "other");

    // Not an interrupted restore: a user's folder.
    let occupied = root.join("occupied");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("keep.txt"), b"mine").unwrap();
    assert_refused(
        root,
        "restore",
        &out,
        &occupied,
        2,
        "holds no .restore-staging",
    );

    // A staging name that is a link, or a folder without an ownership
    // marker that holds something.
    let linked = root.join("linked");
    fs::create_dir(&linked).unwrap();
    symlink(root.join("elsewhere"), linked.join(STAGING)).unwrap();
    assert_refused(root, "restore", &out, &linked, 2, "is not a directory");
    let unowned = root.join("unowned");
    fs::create_dir_all(unowned.join(STAGING)).unwrap();
    fs::write(unowned.join(STAGING).join("data.bin"), b"someone's").unwrap();
    assert_refused(
        root,
        "restore",
        &out,
        &unowned,
        2,
        "holds no ownership marker",
    );
    let forged = root.join("forged");
    fs::create_dir_all(forged.join(STAGING)).unwrap();
    fs::write(
        forged.join(STAGING).join("owner.json"),
        b"{\"kind\":\"other\"}",
    )
    .unwrap();
    assert_refused(root, "restore", &out, &forged, 2, "not an ownership marker");

    // An interrupted restore of another archive, or of the other mode.
    let path = root.join("interrupted");
    interrupted(root, "restore", &out, &path);
    assert_refused(root, "restore", &other, &path, 2, "is of archive");
    assert_refused(root, "fork", &out, &path, 2, "was an archive restore");

    // Something added to PATH or to the staging folder.
    fs::write(path.join("notes.txt"), b"mine").unwrap();
    assert_refused(root, "restore", &out, &path, 2, "notes.txt");
    fs::remove_file(path.join("notes.txt")).unwrap();
    fs::write(path.join(STAGING).join("extra"), b"mine").unwrap();
    assert_refused(root, "restore", &out, &path, 2, "extra");
    fs::remove_file(path.join(STAGING).join("extra")).unwrap();

    // A restore still running into PATH holds its staging folder.
    let busy = root.join("busy");
    let running = paused(root, "restore", &out, &busy, false, "Checked");
    assert_refused(root, "restore", &out, &busy, 7, "still running");
    kill(running);

    // After all those refusals both interrupted restores still resume.
    for path in [&path, &busy] {
        let (exit, resumed) = resume(root, "restore", &out, path);
        assert_eq!(exit, 0, "{resumed}");
        assert_eq!(check(root, path).0, 0);
    }
    assert_eq!(fs::read(occupied.join("keep.txt")).unwrap(), b"mine");
    assert_eq!(
        fs::read(unowned.join(STAGING).join("data.bin")).unwrap(),
        b"someone's"
    );
}

#[test]
fn resume_restores_into_a_new_or_empty_path_and_clears_a_staging_folder_with_no_marker_yet() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let out = archived(root, "source");
    let (_, original) = check(root, &root.join("source"));
    let new = root.join("new");
    let empty = root.join("empty");
    fs::create_dir(&empty).unwrap();
    // A restore killed between making its staging folder and renaming its
    // ownership marker.
    let unmarked = root.join("unmarked");
    fs::create_dir_all(unmarked.join(STAGING)).unwrap();
    let staged_marker = root.join("staged-marker");
    fs::create_dir_all(staged_marker.join(STAGING)).unwrap();
    fs::write(staged_marker.join(STAGING).join("owner.json.tmp"), b"{\"ki").unwrap();
    for path in [&new, &empty, &unmarked, &staged_marker] {
        let (exit, resumed) = resume(root, "restore", &out, path);
        assert_eq!(exit, 0, "{}: {resumed}", path.display());
        assert_eq!(resumed["result"], "restored");
        let (exit, checked) = check(root, path);
        assert_eq!(exit, 0, "{checked}");
        assert_eq!(checked["heads"], original["heads"]);
        assert!(!path.join(STAGING).exists());
    }
}
