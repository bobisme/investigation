//! Existing-vault mode and `vault remove` through the CLI, with the
//! environment cleared and an empty folder on `PATH`: the casebook renders
//! into the managed prefix of a person's vault, `vault status` names the
//! mode, a dry run and read-only mode remove nothing, and `vault remove`
//! deletes only the generated files, leaving every other file of the vault
//! as it was. `init --existing-vault` writes the setting, refuses an unsafe
//! one with a repair, and changes the prefix only after `vault remove`; a
//! note moved out of the prefix is never looked for there.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

struct Casework {
    _directory: tempfile::TempDir,
    root: PathBuf,
    vault: PathBuf,
    empty_path: PathBuf,
}

impl Casework {
    /// A workspace with one case, configured to render into
    /// `<vault>/Investigation`, beside a vault with notes and settings.
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let parent = fs::canonicalize(directory.path()).unwrap();
        let empty_path = parent.join("empty-path");
        fs::create_dir(&empty_path).unwrap();
        let vault = parent.join("My Vault");
        for (path, bytes) in [
            (".obsidian/app.json", "{}\n"),
            (".obsidian/plugins/x/data.json", "{\"a\":1}\n"),
            ("Start Here.md", "# My own start page\n"),
            ("Projects/plan.md", "a plan\n"),
        ] {
            let path = vault.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        let log = parent.join("results.log");
        fs::write(&log, "waited 28 seconds\n").unwrap();
        let casework = Self {
            root: parent.join("casework"),
            vault,
            empty_path,
            _directory: directory,
        };
        let root = casework.root.to_str().unwrap().to_owned();
        let init = casework.run_in(
            &parent,
            &[
                "init",
                &root,
                "--existing-vault",
                "My Vault",
                "--import-root",
                parent.to_str().unwrap(),
            ],
            0,
        );
        assert_eq!(init["projection"]["mode"], "existing_vault", "{init}");
        assert_eq!(init["projection"]["managed_prefix"], "Investigation");
        assert_eq!(init["layout"]["projection"], Value::Null);
        // The same settings a person would hand-edit, with the canonical path.
        let config = fs::read_to_string(casework.root.join("investigation.toml")).unwrap();
        assert!(
            config.contains(&format!(
                "enabled = true\ninclude_approved_derivatives = false\npreserve_human_folders = \
                 true\nexisting_vault = {:?}\nmanaged_prefix = \"Investigation\"\n",
                casework.vault.to_str().unwrap()
            )),
            "{config}"
        );
        casework.run(
            &[
                "--actor",
                "agent:existing-vault-test",
                "start",
                "Why did requests time out?",
                "--attach",
                log.to_str().unwrap(),
                "--command-id",
                "start-1",
            ],
            0,
        );
        casework
    }

    fn run_in(&self, cwd: &Path, args: &[&str], exit: i32) -> Value {
        let mut args = args.to_vec();
        args.extend(["--format", "json"]);
        let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
            .current_dir(cwd)
            .env_clear()
            .env("PATH", &self.empty_path)
            .args(&args)
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{args:?}\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("{error}: {stdout}"))
    }

    fn run(&self, args: &[&str], exit: i32) -> Value {
        self.run_in(&self.root, args, exit)
    }

    /// Every file of the vault, relative to it, with its bytes.
    fn files(&self) -> BTreeMap<String, Vec<u8>> {
        let mut found = BTreeMap::new();
        let mut pending = vec![self.vault.clone()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let relative = path.strip_prefix(&self.vault).unwrap();
                    found.insert(relative.display().to_string(), fs::read(&path).unwrap());
                }
            }
        }
        found
    }
}

fn outside(files: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    files
        .iter()
        .filter(|(path, _)| !path.starts_with("Investigation/"))
        .map(|(path, bytes)| (path.clone(), bytes.clone()))
        .collect()
}

#[test]
fn render_and_remove_touch_only_the_managed_prefix() {
    let casework = Casework::new();
    assert!(!casework.root.join("vault").exists());
    let before = casework.files();

    let setting = casework.run(&["config", "get", "vault_policy.managed_prefix"], 0);
    assert!(
        setting.to_string().contains("\"Investigation\""),
        "{setting}"
    );
    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["projection"]["mode"], "existing_vault");
    assert_eq!(status["projection"]["managed_prefix"], "Investigation");
    assert_eq!(status["state"], "unrendered");
    assert_eq!(status["foreign"], serde_json::json!([]));

    let rendered = casework.run(&["vault", "render"], 0);
    assert_eq!(rendered["complete"], true, "{rendered}");
    let after = casework.files();
    assert_eq!(outside(&after), before);
    assert!(after.contains_key("Investigation/Start Here.md"));
    let start = String::from_utf8(after["Investigation/Start Here.md"].clone()).unwrap();
    assert!(start.contains("[[Investigation/Cases/case_"), "{start}");

    // A dry run, and read-only mode, remove nothing.
    let preview = casework.run(&["vault", "remove", "--dry-run"], 0);
    assert_eq!(preview["dry_run"], true);
    assert!(
        preview["counts"]["removed"].as_u64().unwrap() > 3,
        "{preview}"
    );
    let refused = casework.run(&["--read-only", "vault", "remove"], 2);
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");
    casework.run(&["--read-only", "vault", "remove", "--dry-run"], 0);
    assert_eq!(casework.files(), after);

    let removed = casework.run(&["vault", "remove"], 0);
    assert_eq!(removed["dry_run"], false);
    assert_eq!(removed["counts"]["removed"], preview["counts"]["removed"]);
    assert_eq!(removed["removed_prefix_folder"], true);
    assert_eq!(casework.files(), before);
    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["state"], "unrendered");
}

#[test]
fn init_refuses_an_unsafe_setting_with_a_repair_and_writes_nothing() {
    let casework = Casework::new();
    let parent = casework.root.parent().unwrap().to_owned();
    let before = casework.files();
    let fresh = parent.join("fresh");
    let init = |vault: &str, prefix: &str| {
        casework.run_in(
            &parent,
            &[
                "init",
                fresh.to_str().unwrap(),
                "--existing-vault",
                vault,
                "--managed-prefix",
                prefix,
            ],
            2,
        )
    };
    for (prefix, says, repaired) in [
        (".obsidian/plugins", "hidden", "obsidian/plugins"),
        ("Cases/[x]", "ASCII", "Cases/x"),
        ("a/b/c/d/e", "one to four", "b/c/d/e"),
        ("Missing/Investigation", "does not exist", "Investigation"),
        ("Start Here.md", "not a folder", "Start Here.md Casebook"),
    ] {
        let refused = init("My Vault", prefix);
        assert_eq!(refused["error"]["code"], "INVALID_INPUT", "{refused}");
        assert_eq!(refused["error"]["path"], "--managed-prefix");
        let message = refused["error"]["message"].as_str().unwrap();
        assert!(message.contains(says), "{prefix}: {message}");
        let argv: Vec<&str> = refused["error"]["repair"]["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|word| word.as_str().unwrap())
            .collect();
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--managed-prefix", repaired]),
            "{argv:?}"
        );
    }
    // The vault must exist, and neither may hold the other.
    let refused = init("No Such Vault", "Investigation");
    assert_eq!(refused["error"]["path"], "--existing-vault");
    let inside = casework.root.parent().unwrap();
    let refused = casework.run_in(
        &parent,
        &[
            "init",
            casework.vault.join("ws").to_str().unwrap(),
            "--existing-vault",
            inside.to_str().unwrap(),
        ],
        2,
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("must be apart"),
        "{refused}"
    );
    assert!(!fresh.exists());
    assert_eq!(casework.files(), before);
}

#[test]
fn a_prefix_change_goes_through_vault_remove_and_never_moves_notes() {
    let casework = Casework::new();
    let vault = casework.vault.to_str().unwrap().to_owned();
    let root = casework.root.to_str().unwrap().to_owned();
    // The same setting again changes nothing.
    let again = casework.run(&["init", &root, "--existing-vault", &vault], 0);
    assert_eq!(again["result"], "already_initialized", "{again}");
    assert_eq!(again["projection"]["setting"], "unchanged");

    casework.run(&["vault", "render"], 0);
    fs::create_dir_all(casework.vault.join("Investigation/Notes")).unwrap();
    fs::write(casework.vault.join("Investigation/Notes/mine.md"), "mine\n").unwrap();
    let config = fs::read(casework.root.join("investigation.toml")).unwrap();
    let rendered = casework.files();

    // While generated files are owned, the change is refused with a repair.
    let move_to = [
        "init",
        &root,
        "--existing-vault",
        &vault,
        "--managed-prefix",
        "Projects/Cases",
    ];
    let refused = casework.run(&move_to, 2);
    assert_eq!(refused["error"]["path"], "--existing-vault", "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("vault remove"),
        "{refused}"
    );
    let repair = refused["error"]["repair"]["argv"].to_string();
    assert!(repair.contains("\"remove\",\"--dry-run\""), "{repair}");
    assert_eq!(
        fs::read(casework.root.join("investigation.toml")).unwrap(),
        config
    );
    assert_eq!(casework.files(), rendered);

    // The route: remove, init with the new prefix, render.
    casework.run(&["vault", "remove"], 0);
    let moved = casework.run(&move_to, 0);
    assert_eq!(moved["result"], "existing_vault_set", "{moved}");
    assert_eq!(moved["projection"]["managed_prefix"], "Projects/Cases");
    assert_eq!(moved["projection"]["setting"], "written");
    casework.run(&["vault", "render"], 0);
    let after = casework.files();
    assert!(after.contains_key("Projects/Cases/Start Here.md"));
    assert_eq!(after["Investigation/Notes/mine.md"], b"mine\n");
    assert!(
        after.keys().all(|path| path.starts_with("Projects/Cases/")
            || rendered.contains_key(path) && !path.starts_with("Investigation/Cases/")),
        "{:?}",
        after.keys().collect::<Vec<_>>()
    );
}

#[test]
fn a_note_moved_out_of_the_prefix_is_not_looked_for_there() {
    let casework = Casework::new();
    casework.run(&["vault", "render"], 0);
    let files = casework.files();
    let note = files
        .keys()
        .find(|path| path.starts_with("Investigation/Cases/") && path.ends_with("Overview.md"))
        .unwrap()
        .clone();
    let inside = "Investigation/Notes/moved.md";
    fs::create_dir_all(casework.vault.join("Investigation/Notes")).unwrap();
    fs::rename(casework.vault.join(&note), casework.vault.join(inside)).unwrap();
    casework.run(&["vault", "render"], 0);
    let conflicts = casework.run(&["vault", "conflicts"], 0);
    assert_eq!(conflicts["conflicts"][0]["kind"], "renamed", "{conflicts}");
    assert_eq!(
        conflicts["conflicts"][0]["renamed_to"][0]["path"],
        "Notes/moved.md"
    );

    // Out of the prefix: the rest of the vault is never read, so it is missing.
    fs::rename(casework.vault.join(inside), casework.vault.join("moved.md")).unwrap();
    casework.run(&["vault", "render"], 0);
    let conflicts = casework.run(&["vault", "conflicts"], 0);
    assert_eq!(conflicts["conflicts"][0]["kind"], "missing", "{conflicts}");
    assert!(
        conflicts["conflicts"][0]["restore"]
            .as_str()
            .unwrap_or_default()
            .contains("only inside the projection folder")
            || conflicts
                .to_string()
                .contains("only inside the projection folder"),
        "{conflicts}"
    );
}

#[test]
fn an_edited_note_left_by_removal_is_resolved_before_the_prefix_changes() {
    let casework = Casework::new();
    let vault = casework.vault.to_str().unwrap().to_owned();
    let root = casework.root.to_str().unwrap().to_owned();
    casework.run(&["vault", "render"], 0);
    let note = casework
        .files()
        .into_keys()
        .find(|path| path.starts_with("Investigation/Cases/") && path.ends_with("Overview.md"))
        .unwrap();
    let mut text = fs::read_to_string(casework.vault.join(&note)).unwrap();
    text.push_str("\nMy edit.\n");
    fs::write(casework.vault.join(&note), &text).unwrap();
    casework.run(&["vault", "render"], 0);
    casework.run(&["vault", "remove"], 0);

    let move_to = [
        "init",
        &root,
        "--existing-vault",
        &vault,
        "--managed-prefix",
        "Elsewhere",
    ];
    let refused = casework.run(&move_to, 2);
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains("release-path"), "{refused}");
    assert!(
        refused["error"]["repair"]["argv"]
            .to_string()
            .contains("\"conflicts\""),
        "{refused}"
    );

    let relative = note.strip_prefix("Investigation/").unwrap();
    casework.run(
        &[
            "vault",
            "reconcile",
            relative,
            "--disposition",
            "release-path",
        ],
        0,
    );
    casework.run(&["vault", "remove"], 0);
    casework.run(&move_to, 0);
    casework.run(&["vault", "render"], 0);
    assert_eq!(
        fs::read_to_string(casework.vault.join(&note)).unwrap(),
        text
    );
    assert!(casework.vault.join("Elsewhere/Start Here.md").is_file());
}

#[test]
fn a_prefix_change_waits_for_the_render_lock_and_writes_nothing_while_it_is_held() {
    let casework = Casework::new();
    let vault = casework.vault.to_str().unwrap().to_owned();
    let root = casework.root.to_str().unwrap().to_owned();
    let config = fs::read(casework.root.join("investigation.toml")).unwrap();
    // Stand in for a render in progress: hold the renderer's advisory lock.
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(casework.root.join("local/render.lock"))
        .unwrap();
    lock.lock().unwrap();
    let move_to = [
        "init",
        &root,
        "--existing-vault",
        &vault,
        "--managed-prefix",
        "Elsewhere",
    ];
    let refused = casework.run(&move_to, 7);
    assert_eq!(refused["error"]["code"], "WORKSPACE_BUSY", "{refused}");
    assert_eq!(refused["error"]["path"], "--existing-vault");
    assert_eq!(
        fs::read(casework.root.join("investigation.toml")).unwrap(),
        config
    );
    lock.unlock().unwrap();
    let moved = casework.run(&move_to, 0);
    assert_eq!(moved["result"], "existing_vault_set", "{moved}");
}
