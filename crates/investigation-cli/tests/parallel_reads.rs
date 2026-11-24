//! Read-only commands share the coordination lock (ADR 0011, "Reads share
//! the coordination lock"; bn-2vek). The trial_tasks_8_10 rehearsal runs 4
//! read-only `show` calls side by side; each took the lock exclusively for
//! its whole verification, so under load the last one could wait past the
//! fixed 2 s and refuse with WORKSPACE_BUSY. Here another reader holds the
//! lock for longer than that wait while 4 workers run every read command.
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
};

use serde_json::Value;

fn run(root: &Path, args: &[&str]) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_READ_ONLY")
        .arg("--workspace")
        .arg(root)
        .args(args)
        .args(["--format", "json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    (output.status.code().unwrap(), value)
}

fn lock(root: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("state/locks/commit.lock"))
        .unwrap()
}

#[test]
fn parallel_reads_answer_while_another_read_holds_the_lock_past_the_wait() {
    let directory = tempfile::tempdir().unwrap();
    let root: PathBuf = directory.path().join("workspace");
    let root_arg = root.to_str().unwrap();
    assert_eq!(run(&root, &["init", root_arg, "--no-vault"]).0, 0);
    let (exit, created) = run(
        &root,
        &["--actor", "agent:t", "case", "new", "Parallel reads"],
    );
    assert_eq!(exit, 0, "{created}");
    let case = created["created"][0]["id"].as_str().unwrap().to_owned();
    let key = created["command_id"].as_str().unwrap().to_owned();

    // A slow read: a shared hold of the coordination lock, as a reading
    // process takes it, kept until every worker has finished, so far past
    // the 2 s each command waits. Before bn-2vek every read here refused.
    let slow = lock(&root);
    slow.lock_shared().unwrap();
    let reads: Vec<Vec<String>> = [
        vec!["show", &case, "--depth", "title"],
        vec!["show", &case, "--depth", "full"],
        vec!["brief"],
        vec!["next"],
        vec!["frontier"],
        vec!["receipt", &key, "--actor", "agent:t"],
    ]
    .into_iter()
    .map(|args| {
        let mut argv = vec!["--read-only".to_owned(), "--case".to_owned(), case.clone()];
        argv.extend(args.into_iter().map(str::to_owned));
        argv
    })
    .collect();
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let (root, reads) = (root.clone(), reads.clone());
            thread::spawn(move || {
                reads
                    .iter()
                    .map(|argv| {
                        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
                        (argv.join(" "), run(&root, &argv))
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let answered: Vec<_> = workers
        .into_iter()
        .flat_map(|worker| worker.join().unwrap())
        .collect();
    for (argv, (exit, value)) in &answered {
        assert_eq!(*exit, 0, "{argv}: {value}");
    }
    assert_eq!(answered.len(), 4 * reads.len());

    // A commit waits for the read and refuses past the wait, as it did.
    let (exit, refused) = run(
        &root,
        &[
            "--actor",
            "agent:t",
            "case",
            "new",
            "Refused while a read holds the lock",
        ],
    );
    assert_eq!(exit, 7, "{refused}");
    assert_eq!(refused["error"]["code"], "WORKSPACE_BUSY");
    slow.unlock().unwrap();

    // A writer holding the lock still keeps reads out: the read refuses with
    // the refusal it always printed, and nothing changes.
    let writer = lock(&root);
    writer.lock().unwrap();
    let (exit, refused) = run(&root, &["--read-only", "--case", &case, "show", &case]);
    assert_eq!(exit, 7, "{refused}");
    assert_eq!(refused["error"]["code"], "WORKSPACE_BUSY");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Retry the identical request"),
        "{refused}"
    );
    writer.unlock().unwrap();
    assert_eq!(
        run(&root, &["--read-only", "--case", &case, "show", &case]).0,
        0
    );
}
