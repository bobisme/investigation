//! W17.09: `init` inside an existing workspace names that workspace instead
//! of nesting a second one. A W17.03 rehearsal successor ran `cd casework`,
//! then `init ./casework`, and got a second workspace and case inside the
//! first.

use std::{
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::Value;

fn invoke(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn json(cwd: &Path, args: &[&str], exit: i32) -> Value {
    let output = invoke(cwd, &[&["--format", "json"], args].concat());
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// `work/casework` as a workspace; returns `work` and the workspace root.
fn workspace(temporary: &Path) -> (std::path::PathBuf, String) {
    let work = temporary.canonicalize().unwrap().join("work");
    std::fs::create_dir(&work).unwrap();
    let init = json(
        &work,
        &["init", "casework", "--import-root", ".", "--no-vault"],
        0,
    );
    assert_eq!(init["result"], "initialized");
    let root = init["workspace"].as_str().unwrap().to_owned();
    (work, root)
}

#[test]
fn init_inside_a_workspace_names_it_and_creates_nothing() {
    let temporary = tempfile::tempdir().unwrap();
    let (work, root) = workspace(temporary.path());
    let inside = work.join("casework");
    // The rehearsal: cd into the workspace, then init ./casework.
    for (cwd, args, at) in [
        (&inside, vec!["init", "./casework"], "PATH"),
        (&inside, vec!["init", "notes/deeper"], "PATH"),
        (&work, vec!["init", "casework/state"], "PATH"),
        (
            &inside,
            vec!["--workspace", "./casework", "init"],
            "--workspace",
        ),
    ] {
        let failure = json(cwd, &args, 2);
        assert_eq!(failure["error"]["code"], "INVALID_INPUT", "{failure}");
        assert_eq!(failure["error"]["path"], at, "{failure}");
        let message = failure["error"]["message"].as_str().unwrap();
        assert!(
            message.contains(&format!(
                "lies inside the workspace {root}, so init would nest a second workspace in it. Use that workspace: pass --workspace {root}."
            )),
            "{message}"
        );
        assert!(message.contains("add --nested"), "{message}");
        assert_eq!(
            failure["error"]["repair"]["argv"],
            serde_json::json!([
                "investigation",
                "--workspace",
                root,
                "onboard",
                "--format",
                "json"
            ])
        );
    }
    assert!(!inside.join("casework").exists());
    assert!(!inside.join("notes").exists());

    // The repair runs against the enclosing workspace.
    let onboard = json(&inside, &["--workspace", &root, "onboard"], 0);
    assert_eq!(onboard["ok"], true);
}

#[test]
fn init_on_the_workspace_itself_and_on_a_new_path_are_unchanged() {
    let temporary = tempfile::tempdir().unwrap();
    let (work, root) = workspace(temporary.path());
    let again = json(&work.join("casework"), &["init", "."], 0);
    assert_eq!(again["result"], "already_initialized");
    assert_eq!(again["workspace"], root.as_str());
    let beside = json(&work, &["init", "second", "--no-vault"], 0);
    assert_eq!(beside["result"], "initialized");
}

#[test]
fn nested_asks_for_a_workspace_inside_another() {
    let temporary = tempfile::tempdir().unwrap();
    let (work, _) = workspace(temporary.path());
    let inside = work.join("casework");
    let nested = json(&inside, &["init", "inner", "--nested", "--no-vault"], 0);
    assert_eq!(nested["result"], "initialized");
    assert!(inside.join("inner/investigation.toml").is_file());
}
