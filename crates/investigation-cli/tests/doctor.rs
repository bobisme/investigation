//! `doctor` and `doctor bundle --preview` through the CLI.
//!
//! A healthy workspace yields one JSON object and no findings; a stale index
//! yields a specific finding whose repair restores health; the bundle preview
//! carries no evidence content and no local path; and neither command writes
//! anything. Filesystem refusal is covered by the unit tests in `doctor.rs`,
//! which inject the filesystem classification.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

const EVIDENCE: &str = "UNIQUE-EVIDENCE-BYTES-7f3a waited 28 seconds";
const QUESTION: &str = "Why did UNIQUE-QUESTION-TEXT-91c2 time out?";

struct Casework {
    directory: tempfile::TempDir,
    root: PathBuf,
}

impl Casework {
    fn new(vault: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("results.log"), EVIDENCE).unwrap();
        let root = directory.path().join("casework");
        let casework = Self { directory, root };
        let parent = casework.directory.path().to_str().unwrap().to_owned();
        let root = casework.root.to_str().unwrap().to_owned();
        let mut args = vec!["init", &root, "--import-root", &parent];
        if !vault {
            args.push("--no-vault");
        }
        casework.run_in(casework.directory.path(), &args, 0);
        casework
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
        command
            .current_dir(cwd)
            .env_clear()
            .env("HOME", self.directory.path())
            .args(args);
        command
    }

    fn stdout_in(&self, cwd: &Path, args: &[&str], exit: i32) -> String {
        let output = self.command(cwd, args).output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{args:?}\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        stdout
    }

    fn run_in(&self, cwd: &Path, args: &[&str], exit: i32) -> Value {
        let mut args = args.to_vec();
        args.extend(["--format", "json"]);
        let stdout = self.stdout_in(cwd, &args, exit);
        assert_eq!(stdout.lines().count(), 1, "one JSON object: {stdout}");
        let value: Value =
            serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("{error}: {stdout}"));
        assert!(value.is_object());
        value
    }

    fn run(&self, args: &[&str]) -> Value {
        self.run_in(&self.root, args, 0)
    }

    fn start(&self) {
        let log = self.directory.path().join("results.log");
        self.run(&[
            "--actor",
            "agent:doctor-test",
            "start",
            QUESTION,
            "--attach",
            log.to_str().unwrap(),
            "--command-id",
            "start-1",
        ]);
    }

    /// Every file under the workspace with its bytes.
    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    walk(&entry.path(), out);
                } else {
                    out.insert(entry.path(), fs::read(entry.path()).unwrap_or_default());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.root, &mut out);
        out
    }
}

fn codes(report: &Value) -> Vec<&str> {
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["code"].as_str().unwrap())
        .collect()
}

#[test]
fn a_healthy_workspace_is_one_json_object_with_no_findings() {
    let casework = Casework::new(true);
    casework.start();
    casework.run(&["vault", "render"]);
    let report = casework.run(&["doctor"]);
    assert_eq!(report["result"], "doctor");
    assert_eq!(report["ok"], true);
    assert_eq!(report["healthy"], true, "{report:#}");
    assert_eq!(report["findings"], serde_json::json!([]));
    assert_eq!(report["summary"]["warnings"], 0);
    assert_eq!(report["summary"]["errors"], 0);
    assert_eq!(report["workspace"]["opened"], true);
    assert_eq!(report["workspace"]["cases"], 1);
    assert_eq!(report["workspace"]["index"], "current");
    assert_eq!(report["workspace"]["lock"], "free");
    assert_eq!(report["workspace"]["unsettled_capture_handles"], 0);
    assert_eq!(report["workspace"]["vault"]["freshness"], "current");
    assert_eq!(report["filesystem"]["subject"], "workspace");
    assert_ne!(report["filesystem"]["support"], "refused");
    assert_eq!(report["versions"]["program"], "investigation");
    assert!(report["versions"]["vault_renderer"].is_string());
    assert!(report["versions"]["freshness_rules"].is_number());
    assert!(
        report["capabilities"]["capture"]["protocols"]
            .as_array()
            .is_some_and(|protocols| !protocols.is_empty())
    );
    let selectors = report["capabilities"]["selectors"].as_array().unwrap();
    let time_window = selectors
        .iter()
        .find(|row| row["selector"] == "time_window")
        .unwrap();
    assert_eq!(time_window["validation"], "attestation_only");
    let limits = report["limits"]["enforced"].as_array().unwrap();
    assert!(limits.iter().any(|limit| limit["name"] == "request.bytes"));
    assert_eq!(report["limits"]["expansion"]["decompression"], false);
    assert!(!report["not_checked"].as_array().unwrap().is_empty());

    let text = casework.stdout_in(&casework.root, &["doctor", "--format", "text"], 0);
    assert!(text.starts_with("doctor  healthy"), "{text}");

    // Without a vault the vault section is disabled and still healthy.
    let plain = Casework::new(false);
    plain.start();
    let report = plain.run(&["doctor"]);
    assert_eq!(report["healthy"], true, "{report:#}");
    assert_eq!(report["workspace"]["vault"]["enabled"], false);
}

#[test]
fn a_stale_index_is_a_specific_finding_whose_repair_restores_health() {
    let casework = Casework::new(false);
    casework.start();
    fs::remove_file(casework.root.join("cache/index.sqlite")).unwrap();
    let report = casework.run(&["doctor"]);
    assert_eq!(report["healthy"], false);
    assert_eq!(codes(&report), ["INDEX_STALE"], "{report:#}");
    assert_eq!(report["workspace"]["index"], "degraded");
    let finding = &report["findings"][0];
    assert_eq!(finding["severity"], "warning");
    assert!(
        finding["repair"]["step"]
            .as_str()
            .unwrap()
            .contains("index")
    );
    let argv: Vec<&str> = finding["repair"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap())
        .collect();
    assert_eq!(argv[0], "investigation");
    assert!(
        argv.windows(2).any(|pair| pair == ["repair", "index"]),
        "{argv:?}"
    );
    assert_eq!(report["next"][0], finding["repair"]["argv"]);

    // The repair runs as given, from anywhere, and doctor is healthy after it.
    let output = casework
        .command(casework.directory.path(), &argv[1..])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(casework.run(&["doctor"])["healthy"], true);
}

#[test]
fn outside_a_workspace_doctor_still_reports_the_build_and_names_a_next_step() {
    let casework = Casework::new(false);
    let elsewhere = tempfile::tempdir().unwrap();
    let report = casework.run_in(elsewhere.path(), &["doctor"], 0);
    assert_eq!(codes(&report), ["WORKSPACE_NOT_FOUND"]);
    assert_eq!(report["filesystem"]["subject"], "working_directory");
    assert_eq!(report["workspace"]["opened"], false);
    assert!(report["versions"]["binary"].is_string());
    assert!(report["findings"][0]["repair"]["argv"].is_array());

    let missing = elsewhere.path().join("nothing-here");
    let report = casework.run_in(
        elsewhere.path(),
        &["--workspace", missing.to_str().unwrap(), "doctor"],
        0,
    );
    assert_eq!(codes(&report), ["WORKSPACE_NOT_FOUND"]);
    assert_eq!(report["findings"][0]["severity"], "error");
}

#[test]
fn the_bundle_preview_holds_no_evidence_content_and_no_local_path() {
    let casework = Casework::new(true);
    casework.start();
    // Findings whose text could carry paths: a stale index and vault.
    fs::remove_file(casework.root.join("cache/index.sqlite")).unwrap();
    let before = casework.files();
    let json = casework.stdout_in(
        &casework.root,
        &["doctor", "bundle", "--preview", "--format", "json"],
        0,
    );
    let text = casework.stdout_in(&casework.root, &["doctor", "bundle", "--preview"], 0);
    let preview: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(preview["result"], "doctor_bundle_preview");
    assert_eq!(preview["archive"]["written"], false);
    assert_eq!(preview["archive"]["uploaded"], false);
    let excluded: Vec<&str> = preview["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["category"].as_str().unwrap())
        .collect();
    for category in ["evidence_content", "credentials", "local_paths"] {
        assert!(excluded.contains(&category), "{excluded:?}");
    }
    let report = &preview["report"];
    assert!(codes(report).contains(&"INDEX_STALE"), "{report:#}");
    assert_eq!(
        report["findings"][0]["repair"]["argv"][2], "<WORKSPACE>",
        "{report:#}"
    );

    let temp = casework.directory.path().to_str().unwrap();
    let canonical = fs::canonicalize(casework.directory.path()).unwrap();
    // The unredacted report does carry the location, so the scan below is
    // not vacuous.
    let plain = casework.stdout_in(&casework.root, &["doctor", "--format", "json"], 0);
    assert!(plain.contains(canonical.to_str().unwrap()), "{plain}");
    for output in [&json, &text] {
        for forbidden in [
            "UNIQUE-EVIDENCE-BYTES-7f3a",
            "waited 28 seconds",
            "UNIQUE-QUESTION-TEXT-91c2",
            "results.log",
            "agent:doctor-test",
            temp,
            canonical.to_str().unwrap(),
            "/tmp/",
            "/home/",
        ] {
            assert!(
                !output.contains(forbidden),
                "bundle preview contains {forbidden:?}:\n{output}"
            );
        }
        // No string in the preview is an absolute path.
        for word in output.split(|c: char| c.is_whitespace() || c == '"') {
            assert!(
                !(word.starts_with('/') && word.len() > 1),
                "absolute path {word:?} in bundle preview"
            );
        }
    }
    assert_eq!(casework.files(), before, "the preview wrote nothing");
}

#[test]
fn doctor_writes_nothing_and_runs_in_read_only_mode() {
    let casework = Casework::new(true);
    casework.start();
    let before = casework.files();
    let report = casework.run(&["--read-only", "doctor"]);
    assert_eq!(report["result"], "doctor");
    let preview = casework.run(&["--read-only", "doctor", "bundle", "--preview"]);
    assert_eq!(preview["result"], "doctor_bundle_preview");
    assert_eq!(casework.files(), before);

    // --preview is required: this build writes no archive.
    let refused = casework.run_in(&casework.root, &["doctor", "bundle"], 2);
    assert_eq!(refused["error"]["code"], "INVALID_INPUT");
}
