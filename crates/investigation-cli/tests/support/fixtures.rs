//! The checked-in Scenario A, B, C and D workspaces under
//! `fixtures/vault-goldens/` and the golden vaults rendered from them.
//!
//! Tests only read the fixtures: they copy a workspace into a temporary
//! folder before opening it, and never write a golden.
//! `scripts/vault-goldens.py` regenerates both, and only with an explicit
//! flag.
#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use investigation_render::{RenderPlan, VerifiedSnapshot, layout, render, working_policy};
use investigation_store::{Store, Workspace};

pub const SCENARIOS: [&str; 4] = ["scenario-a", "scenario-b", "scenario-c", "scenario-d"];

/// Directories a workspace must have; git keeps no empty directory.
const REQUIRED: [&str; 13] = [
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
    "vault/Notes",
    "vault/Boards",
    "vault/Inbox",
];

pub const REGENERATE: &str = "python3 scripts/vault-goldens.py target/debug/investigation --write";

pub fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
}

fn golden_root(scenario: &str) -> PathBuf {
    repository().join("fixtures/vault-goldens").join(scenario)
}

/// Copy one fixture workspace to `target`, creating entries in sorted order,
/// or in reverse order so a file system that lists a folder in creation
/// order lists every folder the other way round.
pub fn workspace(scenario: &str, target: &Path, reverse: bool) -> PathBuf {
    fn copy(from: &Path, to: &Path, reverse: bool) {
        fs::create_dir_all(to).unwrap();
        let mut entries: Vec<_> = fs::read_dir(from)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        if reverse {
            entries.reverse();
        }
        for path in entries {
            let destination = to.join(path.file_name().unwrap());
            if path.is_dir() {
                copy(&path, &destination, reverse);
            } else {
                fs::copy(&path, &destination).unwrap();
            }
        }
    }
    copy(&golden_root(scenario).join("workspace"), target, reverse);
    let mut required = REQUIRED;
    if reverse {
        required.reverse();
    }
    for directory in required {
        fs::create_dir_all(target.join(directory)).unwrap();
    }
    target.to_owned()
}

pub fn open(workspace: &Path) -> Store {
    Store::open(Workspace::open(workspace).unwrap())
}

/// The verified snapshot of a workspace and its vault plan under the
/// workspace's working view policy, the one `vault render` uses (Scenarios
/// A and C state a supported viewer release, so their vaults carry native
/// Bases and case maps; Scenario D states none and stays the baseline).
pub fn plan(workspace: &Path) -> (VerifiedSnapshot, RenderPlan) {
    let mut store = open(workspace);
    let policy = working_policy(store.workspace());
    let snapshot = VerifiedSnapshot::read(&mut store).unwrap();
    let plan = render(&snapshot, &policy).unwrap();
    (snapshot, plan)
}

/// Every generated file under a vault folder by vault-relative path: the
/// human-owned folders and hidden folders hold no generated file.
pub fn owned(vault: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            if at == root {
                let name = relative.as_str();
                if layout::is_human_root(name) || name.starts_with('.') {
                    continue;
                }
            }
            if path.is_dir() {
                walk(root, &path, found);
            } else {
                found.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(vault, vault, &mut found);
    found
}

/// The checked-in golden vault of one scenario.
pub fn golden(scenario: &str) -> BTreeMap<String, Vec<u8>> {
    owned(&golden_root(scenario).join("vault"))
}

/// A plan's files by path.
pub fn files(plan: &RenderPlan) -> BTreeMap<String, Vec<u8>> {
    plan.entries()
        .iter()
        .map(|entry| (entry.path().to_string(), entry.content().to_vec()))
        .collect()
}

/// Paths whose bytes differ between two vaults, or that only one has.
pub fn differences(
    expected: &BTreeMap<String, Vec<u8>>,
    found: &BTreeMap<String, Vec<u8>>,
) -> Vec<String> {
    let mut paths: Vec<&String> = expected.keys().chain(found.keys()).collect();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter(|path| expected.get(*path) != found.get(*path))
        .cloned()
        .collect()
}
