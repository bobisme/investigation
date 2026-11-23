//! `integration show/apply/remove/inspect` through the public binary: apply is
//! explicit, bounded to its managed block or owned file, idempotent and
//! reversible byte for byte; links and workspace paths are refused; nothing
//! else ever installs the instructions.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

fn run(cwd: &Path, args: &[&str]) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_READ_ONLY")
        .args(args)
        .args(["--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    (
        output.status.code().unwrap(),
        serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("{args:?}: {stdout}")),
    )
}

fn ok(cwd: &Path, args: &[&str]) -> Value {
    let (code, value) = run(cwd, args);
    assert_eq!(code, 0, "{args:?}: {value}");
    value
}

fn refused(cwd: &Path, args: &[&str], reason: &str) {
    let (code, value) = run(cwd, args);
    assert_ne!(code, 0, "{args:?} was accepted: {value}");
    assert_eq!(value["commit_status"], "not_committed", "{value}");
    assert_eq!(value["error"]["expected"]["reason"], reason, "{value}");
}

/// Every file beneath `root` with its bytes; directories as `None`.
fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let key = path.strip_prefix(root).unwrap().to_path_buf();
            if meta.is_dir() {
                pending.push(path);
                out.insert(key, None);
            } else if meta.file_type().is_symlink() {
                out.insert(
                    key,
                    Some(
                        fs::read_link(&path)
                            .unwrap()
                            .into_os_string()
                            .into_encoded_bytes(),
                    ),
                );
            } else {
                out.insert(key, Some(fs::read(&path).unwrap()));
            }
        }
    }
    out
}

const ORIGINAL: &str = "# Project rules\n\nRun the tests before you commit.\n<!-- an unrelated comment -->\nNo trailing newline here";

#[test]
fn apply_twice_is_a_no_op_and_remove_restores_the_original_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let file = cwd.join("AGENTS.md");
    fs::write(&file, ORIGINAL).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
    let args = [
        "integration",
        "apply",
        "--target",
        "agents-block",
        "--path",
        "AGENTS.md",
    ];

    let shown = ok(cwd, &["integration", "show", "--target", "agents-block"]);
    let block = shown["content"].as_str().unwrap().to_owned();
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        ORIGINAL,
        "show writes nothing"
    );

    let first = ok(cwd, &args);
    assert_eq!(first["action"], "appended");
    assert_eq!(first["written"], true);
    let installed = fs::read_to_string(&file).unwrap();
    assert_eq!(
        installed,
        format!("{ORIGINAL}\n{block}"),
        "only the block is added"
    );
    let diff = first["diff"].as_str().unwrap();
    assert!(
        diff.starts_with("--- AGENTS.md\n+++ AGENTS.md\n@@ "),
        "{diff}"
    );
    assert!(
        diff.lines()
            .filter(|l| l.starts_with('-') && !l.starts_with("---"))
            .count()
            == 1
    );
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o640,
        "the file keeps its permissions"
    );

    let second = ok(cwd, &args);
    assert_eq!(second["action"], "unchanged");
    assert_eq!(second["changed"], false);
    assert_eq!(second["written"], false);
    assert_eq!(second["diff"], "");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        installed,
        "apply twice is a no-op"
    );

    // Text the user adds around the block survives a re-apply byte for byte.
    let edited = format!("Intro line\n{installed}\n## Added after\n");
    fs::write(&file, &edited).unwrap();
    assert_eq!(ok(cwd, &args)["action"], "unchanged");
    assert_eq!(fs::read_to_string(&file).unwrap(), edited);
    fs::write(&file, &installed).unwrap();

    let removed = ok(
        cwd,
        &[
            "integration",
            "remove",
            "--target",
            "agents-block",
            "--path",
            "AGENTS.md",
        ],
    );
    assert_eq!(removed["action"], "removed");
    assert_eq!(removed["reverse_argv"][2], "apply");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        ORIGINAL,
        "remove restores the original"
    );
    let again = ok(
        cwd,
        &[
            "integration",
            "remove",
            "--target",
            "agents-block",
            "--path",
            "AGENTS.md",
        ],
    );
    assert_eq!(again["action"], "not_installed");
    assert_eq!(fs::read_to_string(&file).unwrap(), ORIGINAL);

    // A file apply created is deleted again by remove.
    let new = ok(
        cwd,
        &[
            "integration",
            "apply",
            "--target",
            "agents-block",
            "--path",
            "NEW.md",
        ],
    );
    assert_eq!(new["action"], "created");
    assert_eq!(fs::read_to_string(cwd.join("NEW.md")).unwrap(), block);
    ok(
        cwd,
        &[
            "integration",
            "remove",
            "--target",
            "agents-block",
            "--path",
            "NEW.md",
        ],
    );
    assert!(!cwd.join("NEW.md").exists());
    assert_eq!(
        tree(cwd).keys().cloned().collect::<Vec<_>>(),
        [PathBuf::from("AGENTS.md")],
        "no temporary or stray file is left"
    );
}

#[test]
fn a_skill_file_is_written_only_when_absent_or_already_the_skill() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    fs::create_dir_all(cwd.join("skills/investigation")).unwrap();
    let path = "skills/investigation/SKILL.md";
    let args = [
        "integration",
        "apply",
        "--target",
        "skill-file",
        "--path",
        path,
    ];
    let shown = ok(cwd, &["integration", "show", "--target", "skill-file"]);
    assert_eq!(ok(cwd, &args)["action"], "created");
    assert_eq!(
        fs::read_to_string(cwd.join(path)).unwrap(),
        shown["content"].as_str().unwrap()
    );
    assert_eq!(ok(cwd, &args)["action"], "unchanged");
    ok(
        cwd,
        &[
            "integration",
            "remove",
            "--target",
            "skill-file",
            "--path",
            path,
        ],
    );
    assert!(!cwd.join(path).exists());

    fs::write(cwd.join("notes.md"), "my own notes\n").unwrap();
    refused(
        cwd,
        &[
            "integration",
            "apply",
            "--target",
            "skill-file",
            "--path",
            "notes.md",
        ],
        "NOT_OWNED",
    );
    refused(
        cwd,
        &[
            "integration",
            "remove",
            "--target",
            "skill-file",
            "--path",
            "notes.md",
        ],
        "NOT_OWNED",
    );
    assert_eq!(
        fs::read_to_string(cwd.join("notes.md")).unwrap(),
        "my own notes\n"
    );
    refused(
        cwd,
        &[
            "integration",
            "apply",
            "--target",
            "skill-file",
            "--path",
            "missing/SKILL.md",
        ],
        "DIRECTORY_MISSING",
    );
}

#[test]
fn links_and_workspace_paths_are_refused_and_left_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    fs::create_dir(cwd.join("real")).unwrap();
    fs::write(cwd.join("real/AGENTS.md"), ORIGINAL).unwrap();
    symlink("real/AGENTS.md", cwd.join("AGENTS.md")).unwrap();
    symlink("real", cwd.join("linked")).unwrap();
    ok(cwd, &["init", "casework", "--import-root", "."]);
    fs::write(cwd.join("casework/vault/Inbox/AGENTS.md"), "evidence\n").unwrap();
    let before = tree(cwd);

    for (path, reason) in [
        ("AGENTS.md", "SYMLINK_REFUSED"),
        ("linked/AGENTS.md", "SYMLINK_REFUSED"),
        ("casework/AGENTS.md", "INSIDE_WORKSPACE"),
        ("casework/vault/Inbox/AGENTS.md", "INSIDE_WORKSPACE"),
        ("casework/investigation.toml", "INSIDE_WORKSPACE"),
        ("real/", "INVALID_PATH"),
        ("real", "NOT_REGULAR_FILE"),
    ] {
        for command in ["apply", "remove"] {
            for target in ["agents-block", "skill-file"] {
                refused(
                    cwd,
                    &["integration", command, "--target", target, "--path", path],
                    reason,
                );
            }
        }
    }
    let (code, value) = run(
        cwd,
        &[
            "--read-only",
            "integration",
            "apply",
            "--target",
            "agents-block",
            "--path",
            "real/AGENTS.md",
        ],
    );
    assert_eq!(code, 2);
    assert_eq!(value["error"]["expected"]["reason"], "READ_ONLY_REFUSED");
    let preview = ok(
        cwd,
        &[
            "--read-only",
            "integration",
            "apply",
            "--target",
            "agents-block",
            "--path",
            "real/AGENTS.md",
            "--dry-run",
        ],
    );
    assert_eq!(preview["changed"], true);
    assert_eq!(preview["written"], false);
    assert_eq!(tree(cwd), before, "nothing was written anywhere");
}

#[test]
fn nothing_installs_without_an_explicit_apply() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    fs::write(cwd.join("AGENTS.md"), ORIGINAL).unwrap();
    fs::create_dir(cwd.join("skills")).unwrap();
    ok(cwd, &["init", "casework", "--import-root", "."]);
    let before: BTreeMap<_, _> = tree(cwd)
        .into_iter()
        .filter(|(path, _)| !path.starts_with("casework"))
        .collect();
    for args in [
        &["onboard"][..],
        &["--workspace", "casework", "onboard"],
        &["docs", "agents"],
        &["docs", "skill"],
        &["doctor"],
        &["integration", "inspect"],
        &["integration", "show", "--target", "agents-block"],
        &["integration", "show", "--target", "skill-file"],
        &[
            "integration",
            "apply",
            "--target",
            "agents-block",
            "--path",
            "AGENTS.md",
            "--dry-run",
        ],
        &[
            "integration",
            "apply",
            "--target",
            "skill-file",
            "--path",
            "skills/SKILL.md",
            "--dry-run",
        ],
        &[
            "integration",
            "remove",
            "--target",
            "agents-block",
            "--path",
            "AGENTS.md",
        ],
    ] {
        ok(cwd, args);
    }
    let after: BTreeMap<_, _> = tree(cwd)
        .into_iter()
        .filter(|(path, _)| !path.starts_with("casework"))
        .collect();
    assert_eq!(after, before, "only an explicit apply writes instructions");
    for file in tree(&cwd.join("casework")).into_values().flatten() {
        let text = String::from_utf8_lossy(&file);
        assert!(
            !text.contains("investigation:begin"),
            "the workspace holds no instructions block"
        );
    }
}

#[test]
fn inspect_reports_the_doctor_build_data_and_the_targets() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let inspect = ok(cwd, &["integration", "inspect"]);
    let doctor = ok(cwd, &["doctor"]);
    for key in ["versions", "capabilities", "limits"] {
        assert_eq!(inspect[key], doctor[key], "{key} is shared with doctor");
    }
    assert!(
        inspect["limits"]["enforced"]
            .as_array()
            .unwrap()
            .iter()
            .any(|limit| limit["name"] == "integration.file_bytes")
    );
    let targets: Vec<_> = inspect["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|target| target["target"].as_str().unwrap())
        .collect();
    assert_eq!(targets, ["agents-block", "skill-file"]);
    let docs = ok(cwd, &["docs", "agents"]);
    assert_eq!(inspect["agent_instructions"]["version"], docs["version"]);
    assert_eq!(
        fs::read_dir(cwd).unwrap().count(),
        0,
        "inspect wrote nothing"
    );
}
