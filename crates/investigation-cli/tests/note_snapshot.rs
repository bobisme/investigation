//! W11.14: snapshot human notes and boards by hash when a record relies on
//! them, through the public binary with the environment cleared.
//!
//! | Acceptance criterion | Test |
//! |---|---|
//! | Editing a note after snapshot changes no record; the difference is reported | `a_record_keeps_the_cited_bytes_after_the_note_is_edited` |
//! | An export lists each included human file with its hash and role | `an_export_shows_the_captured_version_not_the_live_one` |
//! | Human files are byte-identical before and after snapshot and export | both tests above |
//!
//! The working vault's annotation note shows the captured file and whether
//! it still matches: `the_working_vault_note_shows_the_captured_file_and_whether_it_still_matches`.
//!
//! Also: `restricted_note_bytes_never_reach_a_non_restricted_export`,
//! `links_special_files_and_paths_outside_the_human_folders_are_refused`,
//! `existing_vault_mode_reads_only_inside_the_managed_prefix`,
//! `a_retry_with_the_same_command_id_records_nothing_twice`.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

const ACTOR: &str = "agent:note-snapshot-test";

struct Casework {
    _directory: tempfile::TempDir,
    parent: PathBuf,
    root: PathBuf,
    /// The projection root: `vault/`, or the managed prefix.
    projection: PathBuf,
    case: String,
}

impl Casework {
    /// A workspace with one case and a dedicated vault.
    fn dedicated() -> Self {
        Self::new(None)
    }

    /// A workspace rendering into `<vault>/Investigation` of a person's vault.
    fn existing() -> Self {
        Self::new(Some("Investigation"))
    }

    fn new(prefix: Option<&str>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let parent = fs::canonicalize(directory.path()).unwrap();
        let root = parent.join("casework");
        let log = parent.join("results.log");
        fs::write(&log, "waited 28 seconds\n").unwrap();
        let init: Vec<&str> = match prefix {
            None => vec!["init", root.to_str().unwrap()],
            Some(_) => vec!["init", root.to_str().unwrap(), "--no-vault"],
        };
        let mut init = init;
        init.extend(["--import-root", parent.to_str().unwrap()]);
        run_in(&parent, &init, 0);
        let projection = match prefix {
            None => root.join("vault"),
            Some(prefix) => {
                let vault = parent.join("My Vault");
                fs::create_dir_all(vault.join(".obsidian")).unwrap();
                fs::write(vault.join(".obsidian/app.json"), "{}\n").unwrap();
                let config = root.join("investigation.toml");
                let text = fs::read_to_string(&config).unwrap().replace(
                    "enabled = false\n",
                    &format!(
                        "enabled = true\nexisting_vault = {:?}\nmanaged_prefix = \"{prefix}\"\n",
                        vault.to_str().unwrap()
                    ),
                );
                fs::write(&config, text).unwrap();
                vault.join(prefix)
            }
        };
        let started = run_in(
            &root,
            &[
                "--actor",
                ACTOR,
                "start",
                "Why did requests time out?",
                "--attach",
                log.to_str().unwrap(),
                "--command-id",
                "start-1",
            ],
            0,
        );
        let case = started["created"][0]["id"].as_str().unwrap().to_owned();
        Self {
            _directory: directory,
            parent,
            root,
            projection,
            case,
        }
    }

    fn run(&self, args: &[&str], exit: i32) -> Value {
        let mut full = vec!["--actor", ACTOR, "--case", self.case.as_str()];
        full.extend(args);
        run_in(&self.root, &full, exit)
    }

    /// Write a person-owned file below the projection root.
    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.projection.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn snapshot(&self, path: &str, on: &str, extra: &[&str]) -> Value {
        let mut args = vec!["vault", "snapshot-note", path, "--on", on];
        args.extend(extra);
        self.run(&args, 0)
    }

    fn head(&self) -> Value {
        self.run(&["show", self.case.as_str()], 0)["snapshot"].clone()
    }

    fn show(&self, id: &str) -> Value {
        self.run(&["show", id, "--depth", "full"], 0)
    }

    fn listing(&self) -> Value {
        self.run(&["vault", "note-snapshots"], 0)
    }

    fn export(&self, kind: &str, audience: &str, name: &str) -> PathBuf {
        let out = self.parent.join(name);
        let mut args = vec!["--actor", ACTOR];
        // A workspace archive covers every case and takes no --case.
        if kind != "workspace-archive" {
            args.extend(["--case", self.case.as_str()]);
        }
        args.extend([
            "export",
            "--kind",
            kind,
            "--audience",
            audience,
            "--out",
            out.to_str().unwrap(),
        ]);
        run_in(&self.root, &args, 0);
        out
    }
}

fn run_in(cwd: &Path, args: &[&str], exit: i32) -> Value {
    let mut args = args.to_vec();
    args.extend(["--format", "json"]);
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_clear()
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

/// Every file below `at`, relative to it, with its bytes.
fn files(at: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![at.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let kind = fs::symlink_metadata(&path).unwrap().file_type();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                found.insert(
                    path.strip_prefix(at).unwrap().display().to_string(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    found
}

fn sha256_hex(value: &Value) -> String {
    value
        .as_str()
        .unwrap()
        .trim_start_matches("sha256:")
        .to_owned()
}

#[test]
fn a_record_keeps_the_cited_bytes_after_the_note_is_edited() {
    let casework = Casework::dedicated();
    let original = b"# Plan\n\nThe proxy timeout is 30 s, so check it first.\n";
    casework.write("Notes/plan.md", original);
    let before = files(&casework.projection);

    let snapshot = casework.snapshot("Notes/plan.md", &casework.case, &["--command-id", "snap-1"]);
    assert_eq!(snapshot["result"], "note_snapshot_recorded");
    assert_eq!(snapshot["role"], "human_note_snapshot");
    assert_eq!(snapshot["owner"], "human");
    assert_eq!(snapshot["file_modified"], false);
    assert_eq!(snapshot["bytes"], original.len());
    // The human file is byte-identical after the snapshot.
    assert_eq!(files(&casework.projection), before);

    let annotation_id = snapshot["annotation_id"].as_str().unwrap().to_owned();
    let annotation = casework.show(&annotation_id);
    let body = &annotation["record"]["body"];
    assert_eq!(body["role"], "note", "{annotation}");
    assert_eq!(body["evidentiary_status"], "none");
    assert_eq!(
        body["human_file"],
        serde_json::json!({"path":"Notes/plan.md","folder":"Notes","owner":"human"})
    );
    assert_eq!(body["content"]["kind"], "artifact");
    assert_eq!(body["content"]["digest"], snapshot["sha256"]);
    let source = casework.show(snapshot["source_id"].as_str().unwrap());
    let acquisition = &source["record"]["body"]["acquisition"];
    assert_eq!(acquisition["context"], "human note snapshot", "{source}");
    assert_eq!(acquisition["channel"], "local_file");
    let head = casework.head();

    // The retained blob holds the captured bytes.
    let hex = sha256_hex(&snapshot["sha256"]);
    let blob = casework
        .root
        .join(format!("state/blobs/sha256/{}/{hex}", &hex[..2]));
    assert_eq!(fs::read(&blob).unwrap(), original);
    assert_eq!(casework.listing()["counts"]["unchanged"], 1);

    // Edit the note: no record changes, and the difference is reported.
    let edited = b"# Plan\n\nActually the timeout is 60 s.\n";
    casework.write("Notes/plan.md", edited);
    assert_eq!(casework.head(), head, "an edit to a note committed nothing");
    assert_eq!(
        casework.show(&annotation_id)["record"],
        annotation["record"]
    );
    assert_eq!(fs::read(&blob).unwrap(), original);
    let listing = casework.listing();
    assert_eq!(listing["records_changed"], false);
    assert_eq!(listing["counts"]["changed"], 1, "{listing}");
    let item = &listing["snapshots"][0];
    assert_eq!(item["live"], "changed");
    assert_eq!(item["path"], "Notes/plan.md");
    assert_eq!(item["captured"]["sha256"], snapshot["sha256"]);
    assert_ne!(item["live_file"]["sha256"], snapshot["sha256"]);
    assert_eq!(item["live_file"]["bytes"], edited.len());
    let next = listing["next"][0]["argv"].as_array().unwrap();
    assert!(
        next.iter()
            .any(|part| part == &Value::from(annotation_id.clone()))
    );

    // A removed note is reported as missing; still nothing is recorded.
    fs::remove_file(casework.projection.join("Notes/plan.md")).unwrap();
    assert_eq!(casework.listing()["counts"]["missing"], 1);
    assert_eq!(casework.head(), head);
    assert_eq!(casework.run(&["check"], 0)["ok"], true);
}

/// The working vault's note of `annotation`, below the projection root.
fn annotation_note(casework: &Casework, annotation: &str) -> String {
    let (path, bytes) = files(&casework.projection)
        .into_iter()
        .find(|(path, _)| path.ends_with(&format!("{annotation}.md")))
        .unwrap_or_else(|| panic!("no note for {annotation}"));
    assert!(path.contains("Annotations/"), "{path}");
    String::from_utf8(bytes).unwrap()
}

/// The `### Human file` section of a rendered annotation note.
fn human_file_section(note: &str) -> &str {
    let start = note
        .find("### Human file")
        .unwrap_or_else(|| panic!("no human file section in {note}"));
    let rest = &note[start..];
    let end = rest[1..].find("\n#").map_or(rest.len(), |at| at + 1);
    &rest[..end]
}

#[test]
fn the_working_vault_note_shows_the_captured_file_and_whether_it_still_matches() {
    let casework = Casework::dedicated();
    let original = b"# Plan\n\nCheck the proxy timeout first.\n";
    casework.write("Notes/plan.md", original);
    let snapshot = casework.snapshot("Notes/plan.md", &casework.case, &[]);
    let annotation = snapshot["annotation_id"].as_str().unwrap().to_owned();
    let captured = snapshot["sha256"].as_str().unwrap().to_owned();
    let render = || casework.run(&["vault", "render"], 0);

    render();
    let note = annotation_note(&casework, &annotation);
    let section = human_file_section(&note);
    assert!(
        section.contains("[[Notes/plan|Notes/plan.md]]"),
        "{section}"
    );
    assert!(section.contains(&format!("`{captured}`")), "{section}");
    assert!(
        section.contains(&format!("`{}` bytes", original.len())),
        "{section}"
    );
    assert!(section.contains("rendered: `unchanged`"), "{section}");
    assert!(section.contains("vault note-snapshots"), "{section}");

    // An edit is shown as changed with the live digest; rendering never
    // writes the person's file and records nothing.
    let head = casework.head();
    let edited = b"# Plan\n\nThe timeout is 60 s.\n";
    casework.write("Notes/plan.md", edited);
    render();
    assert_eq!(
        fs::read(casework.projection.join("Notes/plan.md")).unwrap(),
        edited
    );
    let note = annotation_note(&casework, &annotation);
    let section = human_file_section(&note);
    assert!(section.contains("rendered: `changed`"), "{section}");
    assert!(section.contains(&format!("`{captured}`")), "{section}");
    let listing = casework.listing();
    let live = listing["snapshots"][0]["live_file"]["sha256"]
        .as_str()
        .unwrap();
    assert!(section.contains(&format!("`{live}`")), "{section}");
    assert_eq!(listing["snapshots"][0]["live"], "changed");
    assert_eq!(casework.head(), head);
    assert_eq!(casework.run(&["vault", "status"], 0)["drift_count"], 0);

    // A removed file is missing.
    fs::remove_file(casework.projection.join("Notes/plan.md")).unwrap();
    render();
    let note = annotation_note(&casework, &annotation);
    assert!(
        human_file_section(&note).contains("rendered: `missing`"),
        "{note}"
    );

    // A link to a file outside the projection root is refused, never read.
    let outside = casework.parent.join("outside.md");
    fs::write(&outside, original).unwrap();
    symlink(&outside, casework.projection.join("Notes/plan.md")).unwrap();
    render();
    let note = annotation_note(&casework, &annotation);
    let section = human_file_section(&note);
    assert!(section.contains("rendered: `refused`"), "{section}");
    assert!(section.contains("symbolic link"), "{section}");
    assert_eq!(listing_word(&casework), "refused");

    // A review export shows the annotation but never the live comparison.
    let out = casework.export("review-vault", "internal", "review");
    for (path, bytes) in files(&out) {
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("### Human file"), "{path}");
        assert!(
            !text.contains("Live file when this note was rendered"),
            "{path}"
        );
        assert!(!text.contains("vault note-snapshots"), "{path}");
    }
}

fn listing_word(casework: &Casework) -> String {
    casework.listing()["snapshots"][0]["live"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn an_export_shows_the_captured_version_not_the_live_one() {
    let casework = Casework::dedicated();
    let note = b"# Reading\n\nThe queue drains in 3 s.\n";
    let board = br#"{"nodes":[{"id":"a","type":"text","text":"queue","x":0,"y":0,"width":10,"height":10}],"edges":[]}"#;
    casework.write("Notes/reading.md", note);
    casework.write("Boards/sketch.canvas", board);
    let note_snapshot = casework.snapshot("Notes/reading.md", &casework.case, &[]);
    let board_snapshot = casework.snapshot("Boards/sketch.canvas", &casework.case, &[]);
    casework.write("Notes/reading.md", b"# Reading\n\nRewritten later.\n");
    let live = files(&casework.projection);

    for (kind, file) in [
        ("case-report", "case-report.md"),
        ("decision-record", "decision-record.md"),
    ] {
        let out = casework.export(kind, "internal", kind);
        let report = fs::read_to_string(out.join(file)).unwrap();
        assert!(report.contains("## Human files relied on"), "{report}");
        for (snapshot, path, role) in [
            (&note_snapshot, "Notes/reading.md", "human_note_snapshot"),
            (
                &board_snapshot,
                "Boards/sketch.canvas",
                "human_board_snapshot",
            ),
        ] {
            let hex = sha256_hex(&snapshot["sha256"]);
            let line = report
                .lines()
                .find(|line| line.contains(&format!("`{path}`")))
                .unwrap_or_else(|| panic!("{path} missing from {report}"));
            assert!(line.contains(&format!("`{role}`")), "{line}");
            assert!(line.contains("`human`"), "{line}");
            assert!(
                line.contains(snapshot["sha256"].as_str().unwrap()),
                "{line}"
            );
            assert!(line.contains(&format!("`human/sha256/{hex}`")), "{line}");
        }
        // The copies are the captured bytes, listed in the manifest.
        let copied = fs::read(out.join(format!(
            "human/sha256/{}",
            sha256_hex(&note_snapshot["sha256"])
        )))
        .unwrap();
        assert_eq!(copied, note);
        let manifest: Value =
            serde_json::from_slice(&fs::read(out.join("export-manifest.json")).unwrap()).unwrap();
        let listed: Vec<&str> = manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["path"].as_str().unwrap())
            .collect();
        assert!(
            listed.contains(
                &format!("human/sha256/{}", sha256_hex(&board_snapshot["sha256"])).as_str()
            )
        );
        assert!(!report.contains("Rewritten later"));
    }
    // Exporting read the vault not at all, and changed nothing in it.
    assert_eq!(files(&casework.projection), live);

    // The same snapshot exports byte for byte the same.
    let again = casework.export("case-report", "internal", "again");
    assert_eq!(files(&again), files(&casework.parent.join("case-report")));
}

#[test]
fn restricted_note_bytes_never_reach_a_non_restricted_export() {
    let casework = Casework::dedicated();
    let canary = "CANARY-restricted-note-7f3a";
    casework.write("Notes/secret.md", format!("Private: {canary}\n").as_bytes());
    casework.write("Notes/open.md", b"Shareable note\n");
    let secret = casework.snapshot(
        "Notes/secret.md",
        &casework.case,
        &["--classification", "restricted"],
    );
    assert_eq!(secret["classification"], "restricted");
    let open = casework.snapshot("Notes/open.md", &casework.case, &[]);
    assert_eq!(open["classification"], "internal");
    let secret_hex = sha256_hex(&secret["sha256"]);

    for audience in ["internal", "public"] {
        let out = casework.export("case-report", audience, &format!("report-{audience}"));
        for (name, bytes) in files(&out) {
            for needle in [canary, "Notes/secret.md", secret_hex.as_str()] {
                assert!(
                    !String::from_utf8_lossy(&bytes).contains(needle) && !name.contains(needle),
                    "{audience}: {needle} leaked in {name}"
                );
            }
        }
    }
    let internal =
        fs::read_to_string(casework.parent.join("report-internal/case-report.md")).unwrap();
    assert!(internal.contains("Notes/open.md"), "{internal}");

    // The restricted audience sees the captured bytes.
    let out = casework.export("case-report", "restricted", "report-restricted");
    assert_eq!(
        fs::read_to_string(out.join(format!("human/sha256/{secret_hex}"))).unwrap(),
        format!("Private: {canary}\n")
    );

    // The working read shows the restricted snapshot without its path.
    let listing = casework.listing();
    assert_eq!(listing["counts"]["restricted"], 1, "{listing}");
    assert!(!listing.to_string().contains("Notes/secret.md"));
    assert!(!listing.to_string().contains(&secret_hex));

    // A capture never sits below the classification of its records, and an
    // annotation may not lower the classification of its note artifact.
    let lowered = casework.run(
        &[
            "record",
            "--input",
            write_command(
                &casework,
                &serde_json::json!([{
                    "op":"annotation.add",
                    "payload":{
                        "subject_refs":[{"id":casework.case,"revision":1}],
                        "content":{"kind":"artifact","artifact_ref":secret["artifact"],"digest":secret["sha256"]},
                        "classification":{"level":"internal","reason":null,"tags":[]},
                        "human_file":{"path":"Notes/secret.md"},
                    }
                }]),
            )
            .to_str()
            .unwrap(),
        ],
        5,
    );
    assert_eq!(lowered["error"]["code"], "GATE_FAILED", "{lowered}");
}

fn write_command(casework: &Casework, operations: &Value) -> PathBuf {
    let path = casework.parent.join("command.json");
    let command = serde_json::json!({
        "schema_version":"inv.command.v1",
        "command_id":"lower-1",
        "actor":ACTOR,
        "case_id":casework.case,
        "preconditions":[],
        "operations":operations,
    });
    fs::write(&path, serde_json::to_vec(&command).unwrap()).unwrap();
    path
}

#[test]
fn links_special_files_and_paths_outside_the_human_folders_are_refused() {
    let casework = Casework::dedicated();
    let outside = casework.parent.join("outside.md");
    fs::write(&outside, "not a vault file\n").unwrap();
    casework.write("Notes/real.md", b"real\n");
    symlink(&outside, casework.projection.join("Notes/link.md")).unwrap();
    symlink(
        &casework.parent,
        casework.projection.join("Notes/linked-folder"),
    )
    .unwrap();
    fs::create_dir_all(casework.projection.join("Notes/folder.md")).unwrap();
    let head = casework.head();
    let before = files(&casework.projection);

    for (path, code, message) in [
        ("Notes/link.md", "INVALID_INPUT", "symbolic link"),
        (
            "Notes/linked-folder/outside.md",
            "INVALID_INPUT",
            "symbolic link",
        ),
        ("Notes/folder.md", "INVALID_INPUT", "not a regular file"),
        ("Notes/absent.md", "UNKNOWN_REFERENCE", "No file"),
        (
            "../outside.md",
            "INVALID_INPUT",
            "plain vault-relative path",
        ),
        (
            "Notes/../../outside.md",
            "INVALID_INPUT",
            "plain vault-relative path",
        ),
        (
            "/etc/hostname",
            "INVALID_INPUT",
            "plain vault-relative path",
        ),
        (
            "Cases/x/Overview.md",
            "INVALID_INPUT",
            "outside the person-owned folders",
        ),
        (
            ".obsidian/app.json",
            "INVALID_INPUT",
            "plain vault-relative path",
        ),
        (
            "Start Here.md",
            "INVALID_INPUT",
            "outside the person-owned folders",
        ),
        ("Notes", "INVALID_INPUT", "names a folder"),
    ] {
        let exit = if code == "UNKNOWN_REFERENCE" { 3 } else { 2 };
        let refused = casework.run(
            &["vault", "snapshot-note", path, "--on", &casework.case],
            exit,
        );
        assert_eq!(refused["error"]["code"], code, "{path}: {refused}");
        assert_eq!(refused["commit_status"], "not_committed");
        let text = refused["error"]["message"].as_str().unwrap();
        assert!(text.contains(message), "{path}: {text}");
    }
    assert_eq!(casework.head(), head, "a refusal commits nothing");
    assert_eq!(files(&casework.projection), before);

    // A note that became a link after its snapshot is reported, not followed.
    casework.snapshot("Notes/real.md", &casework.case, &[]);
    fs::remove_file(casework.projection.join("Notes/real.md")).unwrap();
    symlink(&outside, casework.projection.join("Notes/real.md")).unwrap();
    let listing = casework.listing();
    assert_eq!(listing["counts"]["refused"], 1, "{listing}");
    assert_eq!(
        listing["snapshots"][0]["live_file"]["reason"],
        "symbolic link"
    );

    // The archive lists links as skipped, the same way.
    let archive = casework.export("workspace-archive", "restricted", "archive");
    let index: Value =
        serde_json::from_slice(&fs::read(archive.join("archive.json")).unwrap()).unwrap();
    let skipped = index["human_notes_skipped"].to_string();
    assert!(
        skipped.contains("Notes/link.md") && skipped.contains("symbolic link"),
        "{skipped}"
    );
}

#[test]
fn existing_vault_mode_reads_only_inside_the_managed_prefix() {
    let casework = Casework::existing();
    let vault = casework.projection.parent().unwrap().to_path_buf();
    // A person's note outside the prefix, with the same relative path.
    fs::create_dir_all(vault.join("Notes")).unwrap();
    fs::write(vault.join("Notes/plan.md"), "outside the prefix\n").unwrap();
    casework.run(&["vault", "render"], 0);
    let refused = casework.run(
        &[
            "vault",
            "snapshot-note",
            "Notes/plan.md",
            "--on",
            &casework.case,
        ],
        3,
    );
    assert_eq!(refused["error"]["code"], "UNKNOWN_REFERENCE", "{refused}");

    // A link inside the prefix that points out of it is refused.
    fs::create_dir_all(casework.projection.join("Notes")).unwrap();
    symlink(
        vault.join("Notes/plan.md"),
        casework.projection.join("Notes/escape.md"),
    )
    .unwrap();
    let refused = casework.run(
        &[
            "vault",
            "snapshot-note",
            "Notes/escape.md",
            "--on",
            &casework.case,
        ],
        2,
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("symbolic link")
    );

    // So is a human folder of the prefix that is itself a link.
    fs::create_dir_all(vault.join("Elsewhere")).unwrap();
    fs::write(vault.join("Elsewhere/map.canvas"), "{}\n").unwrap();
    symlink(vault.join("Elsewhere"), casework.projection.join("Boards")).unwrap();
    let refused = casework.run(
        &[
            "vault",
            "snapshot-note",
            "Boards/map.canvas",
            "--on",
            &casework.case,
        ],
        2,
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("symbolic link")
    );

    casework.write("Notes/plan.md", b"inside the prefix\n");
    let vault_before = files(&vault);
    let snapshot = casework.snapshot("Notes/plan.md", &casework.case, &[]);
    assert_eq!(snapshot["bytes"], "inside the prefix\n".len());
    assert_eq!(
        files(&vault),
        vault_before,
        "the snapshot changed nothing in the vault"
    );
    assert_eq!(casework.listing()["counts"]["unchanged"], 1);
    // The annotation note links inside the prefix and compared the file
    // there, not the person's note at the same path outside it.
    casework.run(&["vault", "render"], 0);
    let annotation = snapshot["annotation_id"].as_str().unwrap();
    let note = annotation_note(&casework, annotation);
    let section = human_file_section(&note);
    assert!(
        section.contains("[[Investigation/Notes/plan|Notes/plan.md]]"),
        "{section}"
    );
    assert!(section.contains("rendered: `unchanged`"), "{section}");
    let vault_before = files(&vault);

    // The archive reads the human folders through the prefix too.
    let archive = casework.export("workspace-archive", "restricted", "archive");
    let index: Value =
        serde_json::from_slice(&fs::read(archive.join("archive.json")).unwrap()).unwrap();
    let notes = index["human_notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "{index}");
    assert_eq!(notes[0]["path"], "Notes/plan.md");
    assert_eq!(notes[0]["sha256"], snapshot["sha256"]);
    let skipped = index["human_notes_skipped"].to_string();
    assert!(skipped.contains("Notes/escape.md"), "{skipped}");
    assert!(skipped.contains("\"Boards\""), "{skipped}");
    assert!(!index.to_string().contains("outside the prefix"));
    assert_eq!(files(&vault), vault_before);
}

#[test]
fn a_retry_with_the_same_command_id_records_nothing_twice() {
    let casework = Casework::dedicated();
    casework.write("Notes/plan.md", b"one\n");
    let first = casework.snapshot("Notes/plan.md", &casework.case, &["--command-id", "same"]);
    let head = casework.head();
    let again = casework.snapshot("Notes/plan.md", &casework.case, &["--command-id", "same"]);
    assert_eq!(again["annotation_id"], first["annotation_id"]);
    assert_eq!(again["already_committed"], serde_json::json!([true, true]));
    assert_eq!(casework.head(), head);
    assert_eq!(casework.listing()["snapshots"].as_array().unwrap().len(), 1);

    // The same key for different bytes is refused, never silently reused.
    casework.write("Notes/plan.md", b"two\n");
    let reused = casework.run(
        &[
            "vault",
            "snapshot-note",
            "Notes/plan.md",
            "--on",
            &casework.case,
            "--command-id",
            "same",
        ],
        4,
    );
    assert_eq!(
        reused["error"]["code"], "IDEMPOTENCY_KEY_REUSED",
        "{reused}"
    );
    assert_eq!(casework.head(), head);
}

#[test]
fn a_remapped_export_copies_captured_bytes_only_when_remapping_leaves_them_intact() {
    let casework = Casework::dedicated();
    let plain_bytes = b"# Plain\n\nNo identifiers here.\n";
    casework.write("Notes/plain.md", plain_bytes);
    let quoting = format!("# Quoting\n\nSee {} for the run.\n", casework.case);
    casework.write("Notes/quoting.md", quoting.as_bytes());
    let plain = casework.snapshot("Notes/plain.md", &casework.case, &[]);
    let quoted = casework.snapshot("Notes/quoting.md", &casework.case, &[]);
    let out = casework.parent.join("remapped");
    casework.run(
        &[
            "export",
            "--kind",
            "case-report",
            "--audience",
            "internal",
            "--remap-ids",
            "--out",
            out.to_str().unwrap(),
        ],
        0,
    );
    let report = fs::read_to_string(out.join("case-report.md")).unwrap();
    assert!(
        !report.contains(&casework.case),
        "IDs are remapped: {report}"
    );
    let copies: BTreeMap<String, Vec<u8>> = files(&out)
        .into_iter()
        .filter(|(name, _)| name.starts_with("human/sha256/"))
        .collect();
    // The plain note is copied byte for byte under its own digest.
    let member = format!("human/sha256/{}", sha256_hex(&plain["sha256"]));
    assert_eq!(
        copies.get(&member).map(Vec::as_slice),
        Some(&plain_bytes[..]),
        "{report}"
    );
    assert_eq!(copies.len(), 1, "{:?}", copies.keys());
    // The note quoting a canonical ID is listed, never copied altered.
    let line = report
        .lines()
        .find(|line| line.contains("Notes/quoting.md"))
        .unwrap_or_else(|| panic!("{report}"));
    assert!(line.contains("not copied"), "{line}");
    assert!(line.contains(quoted["sha256"].as_str().unwrap()), "{line}");
}
