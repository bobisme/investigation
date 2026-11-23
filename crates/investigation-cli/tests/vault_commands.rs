//! `vault render`, `vault status` and `vault conflicts` through the CLI.
//!
//! Every run clears the environment and puts an empty folder on `PATH`, so
//! rendering and inspecting the vault need no viewer application or any
//! other program. Status reports stale after a commit and current after a
//! render; a render with drift present succeeds and lists it; files in the
//! human-owned folders keep their bytes through every command.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

struct Casework {
    _directory: tempfile::TempDir,
    root: PathBuf,
    empty_path: PathBuf,
}

impl Casework {
    fn new(vault: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let empty_path = directory.path().join("empty-path");
        fs::create_dir(&empty_path).unwrap();
        fs::write(directory.path().join("results.log"), "waited 28 seconds\n").unwrap();
        let parent = directory.path().to_owned();
        let casework = Self {
            root: parent.join("casework"),
            empty_path,
            _directory: directory,
        };
        let root = casework.root.to_str().unwrap();
        let mut args = vec!["init", root, "--import-root", parent.to_str().unwrap()];
        if !vault {
            args.push("--no-vault");
        }
        casework.run_in(&parent, &args, 0);
        casework
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
        command
            .current_dir(cwd)
            .env_clear()
            .env("PATH", &self.empty_path)
            .args(args);
        command
    }

    fn run_in(&self, cwd: &Path, args: &[&str], exit: i32) -> Value {
        let mut args = args.to_vec();
        args.extend(["--format", "json"]);
        let output = self.command(cwd, &args).output().unwrap();
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

    fn text(&self, args: &[&str]) -> String {
        let output = self.command(&self.root, args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    /// Start a case from the log; returns its ID.
    fn start(&self) -> String {
        let log = self.root.parent().unwrap().join("results.log");
        let receipt = self.run(
            &[
                "--actor",
                "agent:vault-test",
                "start",
                "Why did requests time out?",
                "--attach",
                log.to_str().unwrap(),
                "--command-id",
                "start-1",
            ],
            0,
        );
        let case = receipt["created"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|change| {
                change["id"]
                    .as_str()
                    .filter(|id| id.starts_with("case_"))
                    .map(str::to_owned)
            });
        case.unwrap_or_else(|| panic!("{receipt}"))
    }

    fn note(&self, on: &str, key: &str) {
        self.run(
            &[
                "--actor",
                "agent:vault-test",
                "note",
                "Check the clock source",
                "--on",
                on,
                "--command-id",
                key,
            ],
            0,
        );
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }
}

fn next_argv(value: &Value) -> Vec<&str> {
    value["next"][0]["argv"]
        .as_array()
        .map(|argv| argv.iter().map(|part| part.as_str().unwrap()).collect())
        .unwrap_or_default()
}

#[test]
fn status_is_stale_after_a_commit_and_current_after_a_render() {
    let casework = Casework::new(true);
    let case = casework.start();

    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["result"], "vault_status");
    assert_eq!(status["freshness"], "unrendered", "{status}");
    assert_eq!(status["current"], false);
    assert_eq!(
        next_argv(&status)[3..],
        ["vault", "render", "--format", "json"]
    );

    let rendered = casework.run(&["vault", "render"], 0);
    assert_eq!(rendered["result"], "vault_rendered");
    assert_eq!(rendered["complete"], true, "{rendered}");
    assert_eq!(rendered["state"], "complete");
    assert_eq!(rendered["counts"]["drifted"], 0);
    assert!(rendered["counts"]["written"].as_u64().unwrap() >= 3);
    assert!(casework.vault().join("Start Here.md").is_file());

    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["freshness"], "current", "{status}");
    assert_eq!(status["current"], true);
    assert_eq!(status["state"], "complete");
    assert_eq!(
        status["last_complete"]["generation"],
        rendered["generation"]
    );
    assert_eq!(status["drift_count"], 0);
    assert_eq!(status["foreign_count"], 0);
    assert_eq!(status["next"], serde_json::json!([]));

    // A commit makes the vault stale, and says which case moved.
    casework.note(&case, "note-1");
    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["freshness"], "stale", "{status}");
    assert_eq!(status["current"], false);
    assert_eq!(status["state"], "complete");
    assert_eq!(status["cases"][0]["up_to_date"], false);
    assert_eq!(status["cases"][0]["rendered"]["sequence"], 1);
    assert_eq!(status["cases"][0]["current"]["sequence"], 2);
    let reasons = status["stale_reasons"].to_string();
    assert!(
        reasons.contains(&format!("case {case} advanced from sequence 1 to 2")),
        "{reasons}"
    );
    let text = casework.text(&["vault", "status"]);
    assert!(text.starts_with("vault  stale  state complete"), "{text}");
    assert!(
        text.contains("\nNext: investigation --workspace "),
        "{text}"
    );

    // Rendering again makes it current; a second render changes nothing.
    let rendered = casework.run(&["vault", "render"], 0);
    assert_ne!(
        rendered["generation"],
        status["last_complete"]["generation"]
    );
    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["freshness"], "current", "{status}");
    let again = casework.run(&["vault", "render"], 0);
    assert_eq!(again["generation"], rendered["generation"]);
    assert_eq!(again["counts"]["written"], 0, "{again}");
    assert_eq!(again["counts"]["unchanged"], again["counts"]["planned"]);
}

#[test]
fn render_succeeds_with_drift_present_and_lists_it() {
    let casework = Casework::new(true);
    let case = casework.start();
    casework.run(&["vault", "render"], 0);
    let vault = casework.vault();
    let overview = vault.join(format!("Cases/{case}/Overview.md"));
    let source = fs::read_dir(vault.join(format!("Cases/{case}/Sources")))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let source_name = source.file_name().unwrap().to_str().unwrap().to_owned();

    // An edit to a generated note, a generated note moved into Notes/, a
    // person's own note and a file nobody owns.
    let mut edited = fs::read(&overview).unwrap();
    edited.extend_from_slice(b"\nMy edit.\n");
    fs::write(&overview, &edited).unwrap();
    fs::create_dir_all(vault.join("Notes")).unwrap();
    let moved = vault.join("Notes").join(&source_name);
    fs::rename(&source, &moved).unwrap();
    let moved_bytes = fs::read(&moved).unwrap();
    fs::write(vault.join("Notes/mine.md"), b"# Mine\n").unwrap();
    fs::write(vault.join("stray.md"), b"stray\n").unwrap();
    casework.note(&case, "note-1");

    let rendered = casework.run(&["vault", "render"], 0);
    assert_eq!(rendered["complete"], true, "{rendered}");
    assert_eq!(rendered["counts"]["drifted"], 2, "{rendered}");
    assert_eq!(rendered["counts"]["unknown"], 1);
    assert_eq!(rendered["unknown"], serde_json::json!(["stray.md"]));
    let drift = rendered["drift"].as_array().unwrap();
    let by_kind = |kind: &str| {
        drift
            .iter()
            .find(|entry| entry["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind}: {rendered}"))
    };
    let edit = by_kind("edited");
    assert_eq!(edit["path"], format!("Cases/{case}/Overview.md"));
    let retained = casework.root.join(edit["retained"].as_str().unwrap());
    assert!(
        edit["retained"]
            .as_str()
            .unwrap()
            .starts_with("local/conflicts/")
    );
    assert!(retained.is_file(), "{retained:?}");
    let rename = by_kind("renamed");
    assert_eq!(
        rename["renamed_to"],
        serde_json::json!([{"path":format!("Notes/{source_name}"),"human_owned":true}])
    );
    assert_eq!(
        next_argv(&rendered)[3..],
        ["vault", "conflicts", "--format", "json"]
    );
    let text = casework.text(&["vault", "render"]);
    assert!(text.contains("  drifted 2  "), "{text}");
    assert!(
        text.contains(&format!(
            "drifted  Cases/{case}/Overview.md  edited  retained local/conflicts/"
        )),
        "{text}"
    );

    // Left exactly as found.
    assert_eq!(fs::read(&overview).unwrap(), edited);
    assert_eq!(fs::read(&moved).unwrap(), moved_bytes);
    assert!(!source.exists());
    assert_eq!(fs::read(vault.join("Notes/mine.md")).unwrap(), b"# Mine\n");

    let conflicts = casework.run(&["vault", "conflicts"], 0);
    assert_eq!(conflicts["result"], "vault_conflicts");
    assert_eq!(conflicts["count"], 2);
    assert_eq!(conflicts["conflicts"], rendered["drift"]);
    assert_eq!(
        conflicts["kinds"],
        serde_json::json!({"edited":1,"renamed":1})
    );
    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["freshness"], "current");
    assert_eq!(status["drift_count"], 2);
    assert_eq!(status["foreign_count"], 1);
    assert_eq!(
        next_argv(&status)[3..],
        ["vault", "conflicts", "--format", "json"]
    );

    // Narrowed to the case: the foreign file at the vault root is outside it.
    let scoped = casework.run(&["--case", &case, "vault", "status"], 0);
    assert_eq!(scoped["scope"]["kind"], "case");
    assert_eq!(scoped["drift_count"], 2);
    assert_eq!(scoped["foreign_count"], 0);

    // Putting the retained version back clears that path on the next render.
    fs::copy(&retained, &overview).unwrap();
    let rendered = casework.run(&["vault", "render"], 0);
    assert_eq!(rendered["counts"]["drifted"], 1, "{rendered}");
    assert_eq!(rendered["drift"][0]["kind"], "renamed");
    assert_eq!(fs::read(&moved).unwrap(), moved_bytes);
    assert_eq!(fs::read(vault.join("Notes/mine.md")).unwrap(), b"# Mine\n");
}

#[test]
fn read_only_rejects_render_and_allows_status_and_conflicts() {
    let casework = Casework::new(true);
    let case = casework.start();
    let rejected = casework.run(&["--read-only", "vault", "render"], 2);
    assert_eq!(rejected["error"]["code"], "INVALID_INPUT");
    assert_eq!(rejected["commit_status"], "not_committed");
    assert!(!casework.vault().join("Start Here.md").exists());
    // Neither inspection creates the render lock or the manifest.
    let status = casework.run(&["--read-only", "vault", "status"], 0);
    assert_eq!(status["freshness"], "unrendered");
    let conflicts = casework.run(&["--read-only", "vault", "conflicts"], 0);
    assert_eq!(conflicts["count"], 0);
    assert!(!casework.root.join("local/render.lock").exists());
    assert!(!casework.root.join("local/render-manifest.json").exists());

    // After a render and a commit, a refused render leaves the vault and
    // the render record byte for byte as they were.
    casework.run(&["vault", "render"], 0);
    casework.note(&case, "note-1");
    let before = files(&casework.root);
    let rejected = casework.run(&["--read-only", "vault", "render"], 2);
    assert_eq!(rejected["error"]["code"], "INVALID_INPUT");
    for args in [
        ["--read-only", "vault", "status"],
        ["--read-only", "vault", "conflicts"],
    ] {
        casework.run(&args, 0);
    }
    assert_eq!(files(&casework.root), before);
    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["freshness"], "stale");
}

/// Every file under `vault/` and `local/` with its bytes, by relative path.
fn files(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut found = std::collections::BTreeMap::new();
    let mut pending = vec![root.join("vault"), root.join("local")];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = fs::read(&path).unwrap();
                found.insert(path.strip_prefix(root).unwrap().to_owned(), bytes);
            }
        }
    }
    found
}

#[test]
fn a_workspace_without_a_vault_reports_it_and_refuses_to_render() {
    let casework = Casework::new(false);
    casework.start();
    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["vault_enabled"], false, "{status}");
    assert_eq!(status["foreign_count"], Value::Null);
    assert_eq!(status["next"], serde_json::json!([]));
    let text = casework.text(&["vault", "status"]);
    assert!(text.starts_with("vault  disabled"), "{text}");
    let refused = casework.run(&["vault", "render"], 2);
    assert_eq!(refused["error"]["code"], "INVALID_INPUT", "{refused}");
    assert!(!casework.root.join("vault").exists());
}

#[test]
fn a_render_that_stops_reports_partial_and_the_next_render_completes_it() {
    use std::os::unix::fs::PermissionsExt;

    let casework = Casework::new(true);
    let case = casework.start();
    casework.run(&["vault", "render"], 0);
    let folder = casework.vault().join(format!("Cases/{case}"));
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o500)).unwrap();
    casework.note(&case, "note-1");
    let failed = casework.run(&["vault", "render"], 7);
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(failed["error"]["code"], "IO_FAILURE", "{failed}");
    let message = failed["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("The render stopped: ")
            && message.contains("Nothing in the workspace journal changed."),
        "{message}"
    );
    assert_eq!(
        failed["error"]["repair"]["argv"].as_array().unwrap()[3..],
        ["vault", "status", "--format", "json"]
    );

    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["state"], "partial", "{status}");
    assert_eq!(status["freshness"], "stale");
    assert_eq!(status["state_detail"]["interrupted"], false);
    assert!(status["state_detail"]["reason"].as_str().unwrap().len() > 3);
    let text = casework.text(&["vault", "status"]);
    assert!(text.contains("\npartial: "), "{text}");
    assert_eq!(
        next_argv(&status)[3..],
        ["vault", "render", "--format", "json"]
    );

    casework.run(&["vault", "render"], 0);
    let status = casework.run(&["vault", "status"], 0);
    assert_eq!(status["freshness"], "current", "{status}");
    assert_eq!(status["state"], "complete");
}
