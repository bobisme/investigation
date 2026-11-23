//! `docs/obsidian-sync.md` states which paths never to synchronize and who
//! owns each top-level name of the vault. Both tables mirror code, so a test
//! compares them: the classifier `investigation exposure` uses, and the vault
//! ownership table the renderer enforces.

use investigation_render::layout::{AreaOwner, Location, VAULT_AREAS};
use investigation_store::store::exposure::area;

const DOC: &str = include_str!("../../../docs/obsidian-sync.md");

fn section(heading: &str) -> &'static str {
    let rest = DOC
        .split(&format!("## {heading}"))
        .nth(1)
        .unwrap_or_else(|| panic!("missing section {heading}"));
    rest.split("\n## ").next().unwrap()
}

fn rows(section: &str) -> Vec<Vec<String>> {
    section
        .lines()
        .filter(|line| line.starts_with('|') && !line.starts_with("|---"))
        .skip(1)
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_owned())
                .collect()
        })
        .collect()
}

fn backticked(cell: &str) -> Vec<&str> {
    cell.split('`').skip(1).step_by(2).collect()
}

/// Every path in the never-sync table has the area the table gives it.
#[test]
fn never_sync_table_matches_the_exposure_classifier() {
    let table = rows(section("What must never be synced"));
    assert!(table.len() >= 10, "{table:?}");
    let mut checked = 0;
    for row in &table {
        let expected = row[1].trim_matches('`');
        for shown in backticked(&row[0]) {
            // Concrete example paths: fill the placeholders.
            let path = shown
                .replace("<digest>", "ab/abcdef")
                .replace("<handle>", "h1");
            let found = area(std::path::Path::new(&path)).as_str();
            assert_eq!(
                found, expected,
                "{shown} is {found}, the table says {expected}"
            );
            checked += 1;
        }
    }
    assert!(checked >= 10, "{checked}");
}

/// The doc covers every top-level name of the vault ownership table, with
/// the owner the renderer enforces.
#[test]
fn vault_table_matches_the_ownership_table() {
    let table = rows(section("What may be in the synced vault"));
    for area in VAULT_AREAS {
        let owner = match area.owner {
            AreaOwner::Investigation => "Investigation",
            AreaOwner::Person => "You",
            AreaOwner::ListedDerivatives => "Investigation (previews only)",
        };
        for location in area.locations {
            let name = match location {
                Location::File(name) | Location::Folder(name) => (*name).to_owned(),
                Location::HiddenFolders => "Hidden folders".to_owned(),
            };
            let row = table
                .iter()
                .find(|row| row[0].trim_matches('`').trim_end_matches('/') == name)
                .unwrap_or_else(|| panic!("no row for {name}"));
            assert_eq!(row[1], owner, "{name}");
        }
    }
    assert_eq!(
        table.len(),
        VAULT_AREAS
            .iter()
            .map(|area| area.locations.len())
            .sum::<usize>()
    );
}

/// The statements the bone requires are present in plain words.
#[test]
fn the_doc_says_a_synced_vault_is_not_a_resumable_investigation() {
    assert!(DOC.contains("It is not a resumable investigation"));
    assert!(DOC.contains("does not synchronize Investigation transactions"));
    assert!(DOC.contains("does not retract copies already made"));
}

/// A rendered vault copied away alone is not a workspace: no command that
/// reads the journal runs against it.
#[test]
fn a_vault_alone_cannot_be_opened_as_an_investigation() {
    use std::process::Command;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("casework");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_investigation"))
            .current_dir(directory.path())
            .env_remove("AGENT")
            .args(args)
            .args(["--format", "json"])
            .output()
            .unwrap()
            .status
            .code()
            .unwrap()
    };
    assert_eq!(run(&["init", root.to_str().unwrap()]), 0);
    assert_eq!(
        run(&["--workspace", root.to_str().unwrap(), "vault", "render"]),
        0
    );
    assert!(root.join("vault/Start Here.md").exists());
    let copy = directory.path().join("synced-vault");
    std::fs::create_dir(&copy).unwrap();
    std::fs::copy(root.join("vault/Start Here.md"), copy.join("Start Here.md")).unwrap();
    for command in ["check", "next", "brief"] {
        assert_ne!(
            run(&["--workspace", copy.to_str().unwrap(), command]),
            0,
            "{command} ran against a vault alone"
        );
    }
}
