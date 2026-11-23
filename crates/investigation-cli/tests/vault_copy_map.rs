//! W12.04: `vault copy-map` through the public binary, over the checked-in
//! Scenario A (a stated viewer release, so it has case maps) and Scenario D
//! (none, so it has none).
//!
//! | Acceptance criterion | Test |
//! |---|---|
//! | The copy opens in Obsidian and survives later renders byte for byte | `the_copy_is_a_canvas_whose_nodes_resolve_and_survives_renders_and_edits` |
//! | An existing destination is never overwritten | `an_existing_or_linked_destination_is_never_replaced` |
//! | Editing the copy changes no canonical record and produces no drift report | `the_copy_is_a_canvas_whose_nodes_resolve_and_survives_renders_and_edits` |
//!
//! Also: `only_new_canvas_files_under_boards_are_destinations`,
//! `an_unplanned_or_unrendered_map_is_refused_with_its_repair`,
//! `copy_map_is_rejected_in_read_only_mode`.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

#[path = "support/fixtures.rs"]
mod fixtures;

struct Vault {
    _directory: tempfile::TempDir,
    root: PathBuf,
}

impl Vault {
    fn new(scenario: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = fixtures::workspace(scenario, &directory.path().join("workspace"), false);
        Self {
            _directory: directory,
            root,
        }
    }

    fn run(&self, args: &[&str]) -> (i32, Value) {
        let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
            .env_clear()
            .arg("--workspace")
            .arg(&self.root)
            .args(args)
            .args(["--format", "json"])
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let value = serde_json::from_str(&stdout)
            .unwrap_or_else(|error| panic!("{error}: {stdout}\n{:?}", output.stderr));
        (output.status.code().unwrap(), value)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, value) = self.run(args);
        assert_eq!(code, 0, "{args:?}: {value}");
        value
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    /// The case that has a generated map.
    fn case(&self) -> String {
        let cases = self.vault().join("Cases");
        let mut found: Vec<String> = fs::read_dir(&cases)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|case| cases.join(case).join("Map.canvas").is_file())
            .collect();
        found.sort();
        found.remove(0)
    }
}

/// Every file under `at` by relative path.
fn tree(at: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(at) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let kind = fs::symlink_metadata(&path).unwrap().file_type();
            if kind.is_dir() {
                walk(root, &path, found);
            } else if kind.is_symlink() {
                let target = fs::read_link(&path).unwrap();
                let relative = path.strip_prefix(root).unwrap().to_str().unwrap();
                found.insert(relative.to_owned(), target.to_str().unwrap().into());
            } else {
                let relative = path.strip_prefix(root).unwrap().to_str().unwrap();
                found.insert(relative.to_owned(), fs::read(&path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(at, at, &mut found);
    found
}

#[test]
fn the_copy_is_a_canvas_whose_nodes_resolve_and_survives_renders_and_edits() {
    let vault = Vault::new("scenario-a");
    vault.ok(&["vault", "render"]);
    let case = vault.case();
    let state_before = tree(&vault.root.join("state"));
    let manifest_before = fs::read(vault.root.join("local/render-manifest.json")).unwrap();
    let generated = fs::read(vault.vault().join(format!("Cases/{case}/Map.canvas"))).unwrap();

    let copied = vault.ok(&[
        "vault",
        "copy-map",
        "--case",
        &case,
        "--to",
        "Boards/map copy.canvas",
    ]);
    assert_eq!(copied["to"], "Boards/map copy.canvas");
    assert_eq!(copied["owner"], "human");
    assert_eq!(copied["recorded"], false);

    // Vault-only: no journal or other canonical byte, and no manifest entry.
    assert_eq!(tree(&vault.root.join("state")), state_before);
    assert_eq!(
        fs::read(vault.root.join("local/render-manifest.json")).unwrap(),
        manifest_before
    );
    assert!(
        !String::from_utf8(manifest_before)
            .unwrap()
            .contains("Boards/")
    );

    let path = vault.vault().join("Boards/map copy.canvas");
    let bytes = fs::read(&path).unwrap();
    let canvas: Value = serde_json::from_slice(&bytes).unwrap();
    let source: Value = serde_json::from_slice(&generated).unwrap();
    let nodes = canvas["nodes"].as_array().unwrap();
    let originals = source["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), originals.len() + 1);
    assert_eq!(canvas["edges"], source["edges"]);
    // The generated nodes are kept as they were; one text node names the snapshot.
    assert_eq!(&nodes[1..], originals.as_slice());
    let text: Vec<&Value> = nodes.iter().filter(|n| n["type"] == "text").collect();
    assert_eq!(text.len(), 1);
    let note = text[0]["text"].as_str().unwrap();
    assert!(note.contains(&case) && note.contains(copied["generation"].as_str().unwrap()));
    assert!(note.contains(&format!("Cases/{case}/Map.canvas")));
    // Every file node resolves to a generated note in the vault.
    for node in nodes.iter().filter(|n| n["type"] == "file") {
        let file = node["file"].as_str().unwrap();
        assert!(vault.vault().join(file).is_file(), "{file}");
    }

    // Renders neither touch, adopt nor report the copy, edited or not.
    vault.ok(&["vault", "render"]);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let edited = bytes
        .iter()
        .copied()
        .chain(b"\n".iter().copied())
        .collect::<Vec<u8>>();
    fs::write(&path, &edited).unwrap();
    let rendered = vault.ok(&["vault", "render"]);
    assert_eq!(rendered["counts"]["drifted"], 0);
    assert_eq!(rendered["counts"]["unknown"], 0);
    assert_eq!(fs::read(&path).unwrap(), edited);
    let conflicts = vault.ok(&["vault", "conflicts"]);
    assert_eq!(conflicts["count"], 0);
    let status = vault.ok(&["vault", "status"]);
    assert_eq!(status["freshness"], "current");
    // The generated map is unchanged by all of it.
    assert_eq!(
        fs::read(vault.vault().join(format!("Cases/{case}/Map.canvas"))).unwrap(),
        generated
    );
    assert_eq!(tree(&vault.root.join("state")), state_before);
}

#[test]
fn an_existing_or_linked_destination_is_never_replaced() {
    let vault = Vault::new("scenario-a");
    vault.ok(&["vault", "render"]);
    let case = vault.case();
    let boards = vault.vault().join("Boards");
    fs::write(boards.join("mine.canvas"), b"{\"nodes\":[],\"edges\":[]}").unwrap();
    std::os::unix::fs::symlink("elsewhere.canvas", boards.join("dangling.canvas")).unwrap();
    fs::write(vault.root.join("outside.txt"), b"outside").unwrap();
    std::os::unix::fs::symlink(vault.root.join("outside.txt"), boards.join("link.canvas")).unwrap();
    fs::create_dir(vault.root.join("outside-dir")).unwrap();
    std::os::unix::fs::symlink(vault.root.join("outside-dir"), boards.join("folder")).unwrap();
    let before = tree(&vault.root);
    for to in [
        "Boards/mine.canvas",
        "Boards/dangling.canvas",
        "Boards/link.canvas",
        "Boards/folder/new.canvas",
    ] {
        let (code, value) = vault.run(&["vault", "copy-map", "--case", &case, "--to", to]);
        assert_eq!(code, 2, "{to}: {value}");
    }
    assert_eq!(tree(&vault.root), before);
    assert!(!vault.root.join("outside-dir/new.canvas").exists());
    assert_eq!(
        fs::read(vault.root.join("outside.txt")).unwrap(),
        b"outside"
    );
}

#[test]
fn only_new_canvas_files_under_boards_are_destinations() {
    let vault = Vault::new("scenario-a");
    vault.ok(&["vault", "render"]);
    let case = vault.case();
    let before = tree(&vault.root);
    for to in [
        "Notes/map.canvas",
        "Cases/x/Map.canvas",
        "map.canvas",
        "Boards/map.md",
        "Boards/../Notes/map.canvas",
        "../outside.canvas",
        "/tmp/outside.canvas",
        "Boards",
        "boards/map.canvas",
        "Boards/.hidden/map.canvas",
    ] {
        let (code, value) = vault.run(&["vault", "copy-map", "--case", &case, "--to", to]);
        assert_eq!(code, 2, "{to}: {value}");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Boards/"),
            "{to}: {value}"
        );
    }
    assert_eq!(tree(&vault.root), before);
    // A nested folder is created under Boards/ and nowhere else.
    vault.ok(&[
        "vault",
        "copy-map",
        "--case",
        &case,
        "--to",
        "Boards/a/b.canvas",
    ]);
    assert!(vault.vault().join("Boards/a/b.canvas").is_file());
}

#[test]
fn an_unplanned_or_unrendered_map_is_refused_with_its_repair() {
    // No stated viewer release: the vault plans no map.
    let plain = Vault::new("scenario-d");
    plain.ok(&["vault", "render"]);
    let (code, value) = plain.run(&["vault", "copy-map", "--to", "Boards/map.canvas"]);
    assert_eq!(code, 2, "{value}");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("viewer_release") && message.contains("[vault_policy]"),
        "{message}"
    );
    assert!(!plain.vault().join("Boards/map.canvas").exists());

    // A map the vault has not rendered yet: the nodes would not resolve.
    let rendered = Vault::new("scenario-a");
    rendered.ok(&["vault", "render"]);
    let case = rendered.case();
    let unrendered = Vault::new("scenario-a");
    let (code, value) = unrendered.run(&[
        "vault",
        "copy-map",
        "--case",
        &case,
        "--to",
        "Boards/map.canvas",
    ]);
    assert_eq!(code, 2, "{value}");
    assert_eq!(value["error"]["repair"]["argv"][3], "vault");
    assert_eq!(value["error"]["repair"]["argv"][4], "render");
    assert!(!unrendered.vault().join("Boards/map.canvas").exists());
}

#[test]
fn copy_map_is_rejected_in_read_only_mode() {
    let vault = Vault::new("scenario-a");
    vault.ok(&["vault", "render"]);
    let (code, _) = vault.run(&[
        "--read-only",
        "vault",
        "copy-map",
        "--to",
        "Boards/m.canvas",
    ]);
    assert_ne!(code, 0);
    assert!(!vault.vault().join("Boards/m.canvas").exists());
}
