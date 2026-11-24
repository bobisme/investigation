//! Helpers shared by the archive restore tests: the public binary run with
//! a cleared environment, a workspace archive, `check`, a tree of files,
//! the workspace marker, and every case's replayed state and receipts.
#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use investigation_store::{Store, Workspace, store::ReceiptLookup};
use serde_json::{Value, json};

pub fn run(cwd: &Path, args: &[&str]) -> (i32, Value) {
    run_with(cwd, args, None)
}

/// [`run`] with `stdin` on the binary's standard input.
pub fn run_with(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> (i32, Value) {
    let empty_path = cwd.join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_clear()
        .env("PATH", &empty_path);
    json_output(command, args, stdin)
}

/// Run `command` with `args` and `--format json`, `stdin` (or nothing) on
/// its standard input: the exit code and the one JSON object it printed.
pub fn json_output(mut command: Command, args: &[&str], stdin: Option<&[u8]>) -> (i32, Value) {
    command
        .args(args)
        .args(["--format", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = command.spawn().unwrap();
    if let Some(bytes) = stdin {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
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

pub fn archive(cwd: &Path, workspace: &Path, out: &Path) -> Value {
    let (exit, value) = run(
        cwd,
        &[
            "--workspace",
            workspace.to_str().unwrap(),
            "export",
            "--kind",
            "workspace-archive",
            "--audience",
            "restricted",
            "--out",
            out.to_str().unwrap(),
        ],
    );
    assert_eq!(exit, 0, "{value}");
    value
}

pub fn check(cwd: &Path, workspace: &Path) -> (i32, Value) {
    run(cwd, &["--workspace", workspace.to_str().unwrap(), "check"])
}

/// Every file below `root` by relative path, with its bytes; `skip` names
/// top-level entries left out.
pub fn files(root: &Path, skip: &[&str]) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, skip: &[&str], found: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            if skip.contains(&relative.as_str()) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                found.insert(format!("{relative}/"), Vec::new());
                walk(root, &path, skip, found);
            } else if metadata.is_symlink() {
                found.insert(relative, b"<link>".to_vec());
            } else {
                found.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, root, skip, &mut found);
    found
}

pub fn marker(workspace: &Path) -> Value {
    serde_json::from_slice(&fs::read(workspace.join("state/workspace.json")).unwrap()).unwrap()
}

/// The verified state of every case and the receipt of every command,
/// read through the store. Receipts leave out the index status, which is
/// disposable and differs between a fixture and a rebuilt index.
pub fn replayed(workspace: &Path) -> Vec<(String, Value, Vec<Value>)> {
    replayed_with(workspace, |_, _| true)
}

/// [`replayed`] with the receipts of only the commands `wanted(position,
/// events)` accepts, by position in the case's journal. Each receipt lookup
/// verifies the whole journal again.
pub fn replayed_with(
    workspace: &Path,
    wanted: impl Fn(usize, usize) -> bool,
) -> Vec<(String, Value, Vec<Value>)> {
    let mut store = Store::open(Workspace::open(workspace).unwrap());
    let cases = store.cases().unwrap();
    cases
        .into_iter()
        .map(|(case_id, replay)| {
            let records: Vec<Value> = replay
                .state
                .records()
                .map(|record| serde_json::to_value(record).unwrap())
                .collect();
            let count = replay.events.len();
            let receipts = replay
                .events
                .iter()
                .enumerate()
                .filter(|(position, _)| wanted(*position, count))
                .map(|(_, verified)| {
                    let event = &verified.event;
                    let ReceiptLookup::Found { receipt } =
                        store.receipt(&event.actor, &event.command_id).unwrap()
                    else {
                        panic!("no receipt for {}", event.command_id);
                    };
                    let mut receipt = serde_json::to_value(*receipt).unwrap();
                    receipt.as_object_mut().unwrap().remove("projection");
                    json!([event.actor, event.command_id, receipt])
                })
                .collect();
            (
                format!(
                    "{case_id} {}",
                    serde_json::to_string(&replay.snapshot).unwrap()
                ),
                json!(records),
                receipts,
            )
        })
        .collect()
}
