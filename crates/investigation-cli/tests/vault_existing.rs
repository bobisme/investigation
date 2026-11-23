//! Existing-vault mode and `vault remove` through the CLI, with the
//! environment cleared and an empty folder on `PATH`: the casebook renders
//! into the managed prefix of a person's vault, `vault status` names the
//! mode, a dry run and read-only mode remove nothing, and `vault remove`
//! deletes only the generated files, leaving every other file of the vault
//! as it was.

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
        casework.run_in(
            &parent,
            &[
                "init",
                &root,
                "--no-vault",
                "--import-root",
                parent.to_str().unwrap(),
            ],
            0,
        );
        let config = casework.root.join("investigation.toml");
        let text = fs::read_to_string(&config).unwrap().replace(
            "enabled = false\n",
            &format!(
                "enabled = true\nexisting_vault = {:?}\nmanaged_prefix = \"Investigation\"\n",
                casework.vault.to_str().unwrap()
            ),
        );
        fs::write(&config, text).unwrap();
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
