//! `obsidian setup` through the public binary: the preview writes nothing,
//! apply changes only the previewed keys of `app.json`, nothing else in the
//! vault is touched, and links, missing folders and ambiguity are refused.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

fn run(args: &[&str]) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
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

fn ok(args: &[&str]) -> Value {
    let (code, value) = run(args);
    assert_eq!(code, 0, "{args:?}: {value}");
    value
}

fn refused(args: &[&str], reason: &str) {
    let (code, value) = run(args);
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
            let key = path.strip_prefix(root).unwrap().to_path_buf();
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                pending.push(path);
                out.insert(key, None);
            } else {
                out.insert(key, Some(fs::read(&path).unwrap()));
            }
        }
    }
    out
}

const APP_JSON: &str = "{\n\t\"showLineNumber\": true,\n\t\"newLinkFormat\": \"shortest\",\n\t\"nested\": {\"useMarkdownLinks\": 1}\n}";

/// A vault with a configuration folder full of unrelated settings.
fn vault(config_dir: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join(config_dir);
    fs::create_dir_all(config.join("plugins/some-plugin")).unwrap();
    fs::write(config.join("app.json"), APP_JSON).unwrap();
    fs::write(config.join("community-plugins.json"), "[\"some-plugin\"]\n").unwrap();
    fs::write(config.join("hotkeys.json"), "{}").unwrap();
    fs::write(config.join("plugins/some-plugin/main.js"), "// code\n").unwrap();
    fs::create_dir(directory.path().join("Notes")).unwrap();
    fs::write(directory.path().join("Notes/mine.md"), "my note\n").unwrap();
    directory
}

fn path(directory: &tempfile::TempDir) -> &str {
    directory.path().to_str().unwrap()
}

#[test]
fn preview_writes_nothing_and_shows_the_exact_diff() {
    let directory = vault(".obsidian");
    let before = tree(directory.path());
    let value = ok(&["obsidian", "setup", "--vault", path(&directory)]);
    assert_eq!(tree(directory.path()), before);
    assert_eq!(value["written"], false, "{value}");
    assert_eq!(value["changed"], true);
    assert_eq!(value["config_dir"], ".obsidian");
    assert_eq!(value["config_dir_source"], "default");
    let actions: Vec<(&str, &str)> = value["settings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["key"].as_str().unwrap(),
                row["action"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        actions,
        [("useMarkdownLinks", "add"), ("newLinkFormat", "change")]
    );
    let diff = value["diff"].as_str().unwrap();
    assert!(
        diff.contains("-\t\"newLinkFormat\": \"shortest\","),
        "{diff}"
    );
    assert!(
        diff.contains("+\t\"newLinkFormat\": \"absolute\","),
        "{diff}"
    );
    assert_eq!(value["next"][0]["kind"], "local_write");
    let next: Vec<&str> = value["next"][0]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap())
        .collect();
    assert_eq!(next.last(), Some(&"--apply"));
}

#[test]
fn apply_changes_only_the_previewed_keys_and_nothing_else() {
    let directory = vault(".obsidian");
    let before = tree(directory.path());
    let preview = ok(&["obsidian", "setup", "--vault", path(&directory)]);
    let value = ok(&["obsidian", "setup", "--vault", path(&directory), "--apply"]);
    assert_eq!(value["written"], true, "{value}");
    assert_eq!(value["diff"], preview["diff"]);
    let after = tree(directory.path());
    let changed: Vec<&PathBuf> = before
        .keys()
        .filter(|key| before[*key] != after[*key])
        .collect();
    assert_eq!(changed, [&PathBuf::from(".obsidian/app.json")]);
    assert_eq!(before.len(), after.len(), "no file was created or removed");
    let text =
        String::from_utf8(after[&PathBuf::from(".obsidian/app.json")].clone().unwrap()).unwrap();
    assert_eq!(
        text,
        "{\n\t\"showLineNumber\": true,\n\t\"newLinkFormat\": \"absolute\",\n\t\"nested\": {\"useMarkdownLinks\": 1},\n\t\"useMarkdownLinks\": false\n}"
    );
    // A second apply changes nothing.
    let again = ok(&["obsidian", "setup", "--vault", path(&directory), "--apply"]);
    assert_eq!(again["changed"], false);
    assert_eq!(again["written"], false);
    assert_eq!(tree(directory.path()), after);
}

#[test]
fn setting_limits_the_keys_that_change() {
    let directory = vault(".obsidian");
    ok(&[
        "obsidian",
        "setup",
        "--vault",
        path(&directory),
        "--setting",
        "useMarkdownLinks",
        "--apply",
    ]);
    let text = fs::read_to_string(directory.path().join(".obsidian/app.json")).unwrap();
    assert!(text.contains("\"newLinkFormat\": \"shortest\""), "{text}");
    assert!(text.contains("\"useMarkdownLinks\": false"), "{text}");
    refused(
        &[
            "obsidian",
            "setup",
            "--vault",
            path(&directory),
            "--setting",
            "enablePlugins",
        ],
        "UNKNOWN_SETTING",
    );
}

#[test]
fn a_custom_configuration_folder_is_detected_or_named() {
    let directory = vault(".config-custom");
    let before = tree(directory.path());
    let value = ok(&["obsidian", "setup", "--vault", path(&directory)]);
    assert_eq!(value["config_dir"], ".config-custom");
    assert_eq!(value["config_dir_source"], "detected");
    let named = ok(&[
        "obsidian",
        "setup",
        "--vault",
        path(&directory),
        "--config-dir",
        ".config-custom",
    ]);
    assert_eq!(named["config_dir_source"], "flag");
    assert_eq!(tree(directory.path()), before);

    // Two hidden folders that each hold an app.json are ambiguous.
    let second = directory.path().join(".other");
    fs::create_dir(&second).unwrap();
    fs::write(second.join("app.json"), "{}").unwrap();
    refused(
        &["obsidian", "setup", "--vault", path(&directory)],
        "CONFIG_DIR_AMBIGUOUS",
    );
    refused(
        &[
            "obsidian",
            "setup",
            "--vault",
            path(&directory),
            "--config-dir",
            "../x",
        ],
        "INVALID_CONFIG_DIR",
    );
}

#[test]
fn no_folder_or_file_is_created() {
    let directory = tempfile::tempdir().unwrap();
    refused(
        &["obsidian", "setup", "--vault", path(&directory), "--apply"],
        "CONFIG_DIR_MISSING",
    );
    refused(
        &[
            "obsidian",
            "setup",
            "--vault",
            path(&directory),
            "--config-dir",
            ".obsidian",
            "--apply",
        ],
        "CONFIG_DIR_MISSING",
    );
    assert!(tree(directory.path()).is_empty());

    // An existing folder without app.json gets only that file.
    fs::create_dir(directory.path().join(".obsidian")).unwrap();
    ok(&["obsidian", "setup", "--vault", path(&directory), "--apply"]);
    let after = tree(directory.path());
    assert_eq!(
        after.keys().cloned().collect::<Vec<_>>(),
        [
            PathBuf::from(".obsidian"),
            PathBuf::from(".obsidian/app.json")
        ]
    );
}

#[test]
fn links_and_malformed_files_are_refused_untouched() {
    let directory = vault(".obsidian");
    let real = tempfile::tempdir().unwrap();
    let link = real.path().join("link");
    symlink(directory.path(), &link).unwrap();
    refused(
        &[
            "obsidian",
            "setup",
            "--vault",
            link.to_str().unwrap(),
            "--apply",
        ],
        "VAULT_NOT_A_FOLDER",
    );
    let config = directory.path().join(".obsidian");
    fs::rename(&config, directory.path().join("moved")).unwrap();
    symlink(directory.path().join("moved"), &config).unwrap();
    refused(
        &["obsidian", "setup", "--vault", path(&directory), "--apply"],
        "SYMLINK_REFUSED",
    );
    fs::remove_file(&config).unwrap();
    fs::rename(directory.path().join("moved"), &config).unwrap();

    fs::write(config.join("app.json"), "[1, 2]").unwrap();
    let before = tree(directory.path());
    refused(
        &["obsidian", "setup", "--vault", path(&directory), "--apply"],
        "APP_JSON_INVALID",
    );
    assert_eq!(tree(directory.path()), before);
}

#[test]
fn read_only_mode_allows_the_preview_and_refuses_apply() {
    let directory = vault(".obsidian");
    let before = tree(directory.path());
    ok(&[
        "obsidian",
        "setup",
        "--vault",
        path(&directory),
        "--read-only",
    ]);
    let (code, _) = run(&[
        "obsidian",
        "setup",
        "--vault",
        path(&directory),
        "--apply",
        "--read-only",
    ]);
    assert_ne!(code, 0);
    assert_eq!(tree(directory.path()), before);
}
