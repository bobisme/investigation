//! `repair journal` witnesses beyond the journal and the index (ADR 0016):
//! a fork's inherited case head and a workspace archive's `archive.json`,
//! each tested where it is the only witness, and the archives it refuses.
//!
//! A case head is reformatted (a final newline, content kept) and the
//! disposable index is removed, so neither a later file nor the index
//! records the written bytes.
//!
//! Every run clears the environment and puts an empty folder on `PATH`.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use investigation_render::export::archive::ArchiveIndex;
use serde_json::Value;

#[path = "support/fixtures.rs"]
mod fixtures;

const ACTOR: &str = "agent:journal-repair";

fn run(cwd: &Path, args: &[&str]) -> (i32, Value) {
    let empty_path = cwd.join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_clear()
        .env("PATH", &empty_path)
        .args(args)
        .args(["--format", "json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value = serde_json::from_str(&stdout).unwrap_or_else(|_| {
        panic!(
            "{args:?}: {stdout} {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap(), value)
}

fn at<'a>(workspace: &'a Path, args: &[&'a str]) -> Vec<&'a str> {
    let mut all = vec!["--workspace", workspace.to_str().unwrap()];
    all.extend_from_slice(args);
    all
}

fn archive(cwd: &Path, workspace: &Path, out: &Path) {
    let (exit, value) = run(
        cwd,
        &at(
            workspace,
            &[
                "export",
                "--kind",
                "workspace-archive",
                "--audience",
                "restricted",
                "--out",
                out.to_str().unwrap(),
            ],
        ),
    );
    assert_eq!(exit, 0, "{value}");
}

fn index(out: &Path) -> ArchiveIndex {
    serde_json::from_slice(&fs::read(out.join("archive.json")).unwrap()).unwrap()
}

/// The last event file of `case` and the bytes it was written with, after
/// giving it a final newline; the index is removed.
fn reformat_head(workspace: &Path, case: &str) -> (PathBuf, Vec<u8>) {
    let events = workspace.join("state/cases").join(case).join("events");
    let mut names: Vec<_> = fs::read_dir(&events)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    names.sort();
    let head = names.pop().unwrap();
    let written = fs::read(&head).unwrap();
    fs::set_permissions(&head, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&head, [written.as_slice(), b"\n"].concat()).unwrap();
    match fs::remove_file(workspace.join("cache/index.sqlite")) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => panic!("{error}"),
        _ => {}
    }
    (head, written)
}

fn codes(check: &Value) -> Vec<&str> {
    check["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|issue| issue["severity"] == "error")
        .map(|issue| issue["code"].as_str().unwrap())
        .collect()
}

fn witnesses(repaired: &Value) -> Vec<&str> {
    repaired["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["witness"].as_str().unwrap())
        .collect()
}

#[test]
fn an_archive_of_this_workspace_is_the_only_witness_of_a_reformatted_head() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-c", &root.join("ws"), false);
    let out = root.join("archive");
    archive(root, &workspace, &out);
    let case = index(&out).cases[0].case_id.to_string();
    let (head, written) = reformat_head(&workspace, &case);
    let changed = fs::read(&head).unwrap();

    let (exit, check) = run(root, &at(&workspace, &["check"]));
    assert_eq!(exit, 6, "{check}");
    assert_eq!(codes(&check), ["NON_CANONICAL_JOURNAL_FILE"]);

    // Without the archive nothing records the written bytes.
    let (exit, refused) = run(root, &at(&workspace, &["repair", "journal"]));
    assert_eq!(exit, 6, "{refused}");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&case) && message.contains("--archive"),
        "{message}"
    );
    assert_eq!(fs::read(&head).unwrap(), changed);

    let dir = out.to_str().unwrap();
    let (exit, planned) = run(
        root,
        &at(
            &workspace,
            &["repair", "journal", "--archive", dir, "--dry-run"],
        ),
    );
    assert_eq!(exit, 0, "{planned}");
    assert_eq!(witnesses(&planned), ["archive"]);
    assert_eq!(
        fs::read(&head).unwrap(),
        changed,
        "a dry run writes nothing"
    );

    let (exit, repaired) = run(
        root,
        &at(&workspace, &["repair", "journal", "--archive", dir]),
    );
    assert_eq!(exit, 0, "{repaired}");
    assert_eq!(witnesses(&repaired), ["archive"]);
    assert_eq!(fs::read(&head).unwrap(), written);
    let (exit, check) = run(root, &at(&workspace, &["check"]));
    assert_eq!(exit, 0, "{check}");
    assert!(codes(&check).is_empty());
}

#[test]
fn an_archive_that_cannot_witness_is_refused_and_nothing_is_written() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-c", &root.join("ws"), false);
    let out = root.join("archive");
    archive(root, &workspace, &out);
    let case = index(&out).cases[0].case_id.to_string();

    // An archive of another workspace.
    let other = fixtures::workspace("scenario-a", &root.join("other"), false);
    let foreign = root.join("foreign-archive");
    archive(root, &other, &foreign);
    // An archive whose archive.json is not the file its manifest lists.
    let altered = root.join("altered-archive");
    fs::create_dir(&altered).unwrap();
    for entry in walk(&out) {
        let relative = entry.strip_prefix(&out).unwrap();
        let target = altered.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(&entry, &target).unwrap();
    }
    let edited = fs::read_to_string(altered.join("archive.json")).unwrap();
    fs::set_permissions(
        altered.join("archive.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::write(altered.join("archive.json"), format!("{edited} ")).unwrap();
    // A directory that is no export.
    let empty = root.join("empty");
    fs::create_dir(&empty).unwrap();

    let (head, _) = reformat_head(&workspace, &case);
    let changed = fs::read(&head).unwrap();
    for (label, dir, needle) in [
        ("foreign", &foreign, "not from this workspace"),
        ("altered", &altered, "not the file its manifest lists"),
        ("empty", &empty, "cannot witness"),
    ] {
        let (exit, refused) = run(
            root,
            &at(
                &workspace,
                &["repair", "journal", "--archive", dir.to_str().unwrap()],
            ),
        );
        assert_eq!(exit, 2, "{label}: {refused}");
        assert_eq!(refused["error"]["code"], "INVALID_INPUT", "{label}");
        assert_eq!(refused["error"]["path"], "--archive", "{label}");
        let message = refused["error"]["message"].as_str().unwrap();
        assert!(message.contains(needle), "{label}: {message}");
        assert_eq!(fs::read(&head).unwrap(), changed, "{label}");
    }

    // An archive older than the head records an earlier head: no witness.
    fs::write(&head, &changed[..changed.len() - 1]).unwrap();
    let (exit, value) = run(root, &at(&workspace, &["repair", "index"]));
    assert_eq!(exit, 0, "{value}");
    let (exit, value) = run(
        root,
        &at(
            &workspace,
            &[
                "--case",
                &case,
                "--actor",
                ACTOR,
                "note",
                "A later note.",
                "--command-id",
                "later",
            ],
        ),
    );
    assert_eq!(exit, 0, "{value}");
    let (head, _) = reformat_head(&workspace, &case);
    let changed = fs::read(&head).unwrap();
    let (exit, refused) = run(
        root,
        &at(
            &workspace,
            &["repair", "journal", "--archive", out.to_str().unwrap()],
        ),
    );
    assert_eq!(exit, 6, "{refused}");
    assert_eq!(
        refused["error"]["actual"]["unwitnessed"][0],
        head.strip_prefix(&workspace).unwrap().to_str().unwrap()
    );
    assert_eq!(fs::read(&head).unwrap(), changed);
}

#[test]
fn a_forks_inherited_head_is_the_only_witness_of_its_reformatted_head() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-c", &root.join("ws"), false);
    let out = root.join("archive");
    archive(root, &workspace, &out);
    let fork = root.join("fork");
    let (exit, forked) = run(
        root,
        &[
            "archive",
            "fork",
            out.to_str().unwrap(),
            fork.to_str().unwrap(),
        ],
    );
    assert_eq!(exit, 0, "{forked}");
    let case = index(&out).cases[0].case_id.to_string();
    let (head, written) = reformat_head(&fork, &case);

    // Named as reformatted, not as a fork that differs from its origin.
    let (exit, check) = run(root, &at(&fork, &["check"]));
    assert_eq!(exit, 6, "{check}");
    assert_eq!(codes(&check), ["NON_CANONICAL_JOURNAL_FILE"]);

    let (exit, repaired) = run(root, &at(&fork, &["repair", "journal"]));
    assert_eq!(exit, 0, "{repaired}");
    assert_eq!(witnesses(&repaired), ["inherited_head"]);
    assert_eq!(fs::read(&head).unwrap(), written);
    let (exit, check) = run(root, &at(&fork, &["check"]));
    assert_eq!(exit, 0, "{check}");
    assert!(codes(&check).is_empty());
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}
