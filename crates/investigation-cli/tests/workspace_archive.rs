//! `export --kind workspace-archive` and `archive verify` through the public
//! binary (W11.08): the archives of Scenarios A, C and D verify offline,
//! restore to the same case heads, states and receipts, and are
//! byte-identical when made twice; tampering with a member, adding or
//! removing one, or forging a consistent manifest over an edited journal is
//! found; and an archive is refused for audiences below restricted, with a
//! case or snapshot, and while capture staging is unsettled.
//!
//! Every run clears the environment and puts an empty folder on `PATH`.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use investigation_render::export::{
    ExportManifest, archive::ArchiveIndex, expected_export_id, verify::restore_into,
};
use investigation_store::{Store, Workspace, journal::hash_bytes};
use serde_json::{Value, json};

#[path = "support/fixtures.rs"]
mod fixtures;

use fixtures::SCENARIOS;

const NOTE: &[u8] = b"# Plan\n\nAsk the build owner about the clock.\n";

fn run(cwd: &Path, args: &[&str]) -> (i32, Value) {
    let empty_path = cwd.join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_clear()
        .env("PATH", &empty_path)
        .args(args)
        .args(["--format", "json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value = serde_json::from_str(&stdout).unwrap_or_else(|_| {
        panic!(
            "{args:?}: {stdout} {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap(), value)
}

fn archive(cwd: &Path, workspace: &Path, out: &Path, extra: &[&str]) -> (i32, Value) {
    let mut args = vec![
        "--workspace",
        workspace.to_str().unwrap(),
        "export",
        "--kind",
        "workspace-archive",
        "--out",
        out.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    run(cwd, &args)
}

fn verify(cwd: &Path, directory: &Path) -> (i32, Value) {
    let scratch = cwd.join("scratch");
    fs::create_dir_all(&scratch).unwrap();
    run(
        cwd,
        &[
            "archive",
            "verify",
            directory.to_str().unwrap(),
            "--scratch",
            scratch.to_str().unwrap(),
        ],
    )
}

/// Every file below `root` by relative path, with its bytes.
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            if metadata.is_dir() {
                walk(root, &path, found);
            } else if metadata.is_symlink() {
                found.insert(relative, b"<link>".to_vec());
            } else {
                found.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, root, &mut found);
    found
}

/// Pin records left below `local/pins`.
fn pin_records(workspace: &Path) -> usize {
    fs::read_dir(workspace.join("local/pins")).map_or(0, Iterator::count)
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}

fn manifest(directory: &Path) -> ExportManifest {
    serde_json::from_slice(&fs::read(directory.join("export-manifest.json")).unwrap()).unwrap()
}

fn index(directory: &Path) -> ArchiveIndex {
    serde_json::from_slice(&fs::read(directory.join("archive.json")).unwrap()).unwrap()
}

fn write_manifest(directory: &Path, manifest: &ExportManifest) {
    let mut bytes = serde_json::to_vec_pretty(manifest).unwrap();
    bytes.push(b'\n');
    fs::write(directory.join("export-manifest.json"), bytes).unwrap();
}

fn codes(value: &Value) -> Vec<String> {
    value["error"]["actual"]["problems"]
        .as_array()
        .unwrap_or_else(|| panic!("{value}"))
        .iter()
        .map(|problem| problem["code"].as_str().unwrap().to_owned())
        .collect()
}

/// The verified state and every receipt of a workspace, read through the
/// store.
fn replayed(workspace: &Path) -> Vec<(String, Value, String)> {
    let mut store = Store::open(Workspace::open(workspace).unwrap());
    store
        .cases()
        .unwrap()
        .into_iter()
        .map(|(case_id, replay)| {
            let records: Vec<Value> = replay
                .state
                .records()
                .map(|record| serde_json::to_value(record).unwrap())
                .collect();
            let commands: Vec<String> = replay
                .events
                .iter()
                .map(|event| format!("{} {}", event.event.actor, event.event.command_id))
                .collect();
            (
                format!(
                    "{case_id} {}",
                    serde_json::to_string(&replay.snapshot).unwrap()
                ),
                json!(records),
                commands.join("\n"),
            )
        })
        .collect()
}

#[test]
fn scenario_archives_verify_offline_and_restore_to_the_same_state_and_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for scenario in SCENARIOS {
        let workspace = fixtures::workspace(scenario, &root.join(scenario), false);
        fs::create_dir_all(workspace.join("vault/Notes/Plans")).unwrap();
        fs::write(workspace.join("vault/Notes/Plans/next.md"), NOTE).unwrap();
        symlink("next.md", workspace.join("vault/Notes/Plans/link.md")).unwrap();
        let out = root.join(format!("{scenario}-archive"));
        let (exit, receipt) = archive(root, &workspace, &out, &["--audience", "restricted"]);
        assert_eq!(exit, 0, "{receipt}");
        assert_eq!(receipt["kind"], "workspace-archive");
        assert_eq!(receipt["completeness"], "complete");
        // The archive's pin ended with it (W11.09): no record is left behind.
        assert_eq!(pin_records(&workspace), 0);
        assert_eq!(receipt["human_notes"], 1, "{receipt}");
        assert_eq!(
            receipt["human_notes_skipped"],
            json!([{"path": "Notes/Plans/link.md", "reason": "symbolic link"}])
        );

        // Made twice from the same state, the archive is byte-identical.
        let again = root.join(format!("{scenario}-again"));
        let (exit, second) = archive(root, &workspace, &again, &["--audience", "restricted"]);
        assert_eq!(exit, 0, "{second}");
        assert_eq!(second["export_id"], receipt["export_id"]);
        assert_eq!(files(&out), files(&again), "{scenario}");

        // The manifest, the index and the case heads agree.
        let manifest = manifest(&out);
        let index = index(&out);
        assert!(manifest.resumable);
        assert_eq!(manifest.policy.audience, "restricted");
        assert_eq!(manifest.snapshot.case_id, index.workspace_id);
        assert_eq!(manifest.snapshot.sequence, 0);
        assert_eq!(manifest.snapshot.head_hash, index.recovery_head);
        assert_eq!(expected_export_id(&manifest), manifest.export_id);
        assert_eq!(
            json!(index.cases.len()),
            json!(receipt["cases"].as_array().unwrap().len())
        );
        assert!(index.excluded.iter().any(|area| area.path == "cache/"));
        assert!(index.excluded.iter().any(|area| area.path == "local/"));
        let archived = files(&out);
        for member in archived.keys() {
            assert!(
                !member.starts_with("workspace/cache")
                    && !member.starts_with("workspace/local")
                    && !member.starts_with("workspace/vault")
                    && !member.starts_with("workspace/exports")
                    && !member.ends_with("commit.lock"),
                "{member}"
            );
        }

        // The archive carries the published v1 contracts unchanged.
        let published = fixtures::repository().join("schemas/v1");
        let published = files(&published);
        let carried: BTreeMap<&str, &Vec<u8>> = archived
            .iter()
            .filter_map(|(path, bytes)| Some((path.strip_prefix("schemas/v1/")?, bytes)))
            .collect();
        assert_eq!(
            published.keys().map(String::as_str).collect::<Vec<_>>(),
            carried.keys().copied().collect::<Vec<_>>()
        );
        for (name, bytes) in &published {
            let expected: Value = serde_json::from_slice(bytes).unwrap();
            let actual: Value = serde_json::from_slice(carried[name.as_str()]).unwrap();
            assert_eq!(expected, actual, "schemas/v1/{name}");
        }

        // Offline verification restores a private copy and checks it.
        let before = files(&out);
        let (exit, verified) = verify(root, &out);
        assert_eq!(exit, 0, "{verified}");
        assert_eq!(verified["verified"], true);
        let rehearsal = &verified["archive"]["rehearsal"];
        assert_eq!(rehearsal["check_ok"], true, "{verified}");
        assert_eq!(rehearsal["heads"], receipt["cases"]);
        assert_eq!(files(&out), before, "verify never writes into DIR");
        assert_eq!(fs::read_dir(root.join("scratch")).unwrap().count(), 0);

        // A restore gives the same heads, states and command identities;
        // check passes there and an old command finds its receipt.
        let restored = root.join(format!("{scenario}-restored"));
        restore_into(&out, &manifest, &index, &restored, true).unwrap();
        let (exit, checked) = run(root, &["--workspace", restored.to_str().unwrap(), "check"]);
        assert_eq!(exit, 0, "{checked}");
        let (_, original) = run(root, &["--workspace", workspace.to_str().unwrap(), "check"]);
        assert_eq!(checked["heads"], original["heads"]);
        assert_eq!(replayed(&restored), replayed(&workspace));
        assert_eq!(
            fs::read(restored.join("vault/Notes/Plans/next.md")).unwrap(),
            NOTE
        );
        let source = Store::open(Workspace::open(&workspace).unwrap())
            .cases()
            .unwrap();
        let event = &source.values().next().unwrap().events[0].event;
        let lookup = |at: &Path| {
            run(
                root,
                &[
                    "--workspace",
                    at.to_str().unwrap(),
                    "receipt",
                    &event.command_id,
                    "--actor",
                    &event.actor.to_string(),
                ],
            )
        };
        let (exit, old) = lookup(&workspace);
        assert_eq!(exit, 0, "{old}");
        let (exit, new) = lookup(&restored);
        assert_eq!(exit, 0, "{new}");
        for field in [
            "result",
            "transaction_id",
            "command_id",
            "snapshot",
            "created",
            "updated",
        ] {
            assert_eq!(old[field], new[field], "{field}: {new}");
        }
        assert_eq!(new["transaction_id"], json!(event.transaction_id));
    }
}

/// One archive of Scenario C, copied before each tamper.
fn tamper_case(
    root: &Path,
    name: &str,
    original: &Path,
    change: impl FnOnce(&Path),
) -> Vec<String> {
    let copy = root.join(name);
    copy_tree(original, &copy);
    change(&copy);
    let before = files(&copy);
    let (exit, value) = verify(root, &copy);
    assert_eq!(exit, 6, "{name}: {value}");
    assert_eq!(value["error"]["code"], "INTEGRITY_FAILURE");
    assert_eq!(files(&copy), before, "{name}: verify never writes into DIR");
    codes(&value)
}

fn first_member<'a>(manifest: &'a ExportManifest, prefix: &str) -> &'a str {
    manifest
        .files
        .iter()
        .map(|file| file.path.as_str())
        .find(|path| path.starts_with(prefix))
        .unwrap()
}

#[test]
fn verify_finds_missing_extra_unsafe_and_corrupted_members_and_a_forged_journal() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-c", &root.join("ws"), false);
    let original = root.join("archive");
    let (exit, receipt) = archive(root, &workspace, &original, &["--audience", "restricted"]);
    assert_eq!(exit, 0, "{receipt}");
    let listed = manifest(&original);
    let blob = first_member(&listed, "workspace/state/blobs/").to_owned();
    let event = listed
        .files
        .iter()
        .map(|file| file.path.as_str())
        .rfind(|path| path.contains("/events/"))
        .unwrap()
        .to_owned();

    let found = tamper_case(root, "missing", &original, |copy| {
        fs::remove_file(copy.join(&blob)).unwrap();
    });
    assert_eq!(found, ["MEMBER_MISSING"]);

    let found = tamper_case(root, "extra", &original, |copy| {
        fs::write(copy.join("workspace/state/notes.txt"), b"added").unwrap();
    });
    assert_eq!(found, ["MEMBER_EXTRA"]);

    let found = tamper_case(root, "unsafe", &original, |copy| {
        fs::remove_file(copy.join(&blob)).unwrap();
        symlink("/dev/null", copy.join(&blob)).unwrap();
    });
    assert_eq!(found, ["MEMBER_UNSAFE"]);

    let found = tamper_case(root, "corrupted", &original, |copy| {
        let mut bytes = fs::read(copy.join(&blob)).unwrap();
        bytes[0] ^= 0x20;
        fs::write(copy.join(&blob), bytes).unwrap();
    });
    assert_eq!(found, ["MEMBER_CORRUPTED"]);

    // The manifest re-digested over an edited first event, export_id left
    // alone; then with export_id recomputed too.
    let first = listed
        .files
        .iter()
        .map(|file| file.path.as_str())
        .find(|path| path.ends_with("/events/000000000001.json"))
        .unwrap()
        .to_owned();
    let edit = |copy: &Path, recompute: bool| {
        let path = copy.join(&first);
        let text = fs::read_to_string(&path).unwrap();
        let edited = text.replacen("\"sequence\"", "\"sequence\" ", 1);
        assert_ne!(text, edited);
        fs::write(&path, &edited).unwrap();
        let mut manifest = manifest(copy);
        let entry = manifest
            .files
            .iter_mut()
            .find(|file| file.path == first)
            .unwrap();
        entry.sha256 = hash_bytes(edited.as_bytes());
        entry.bytes = edited.len() as u64;
        if recompute {
            manifest.export_id = expected_export_id(&manifest);
        }
        write_manifest(copy, &manifest);
    };
    let found = tamper_case(root, "altered", &original, |copy| edit(copy, false));
    assert_eq!(found, ["MANIFEST_ALTERED"]);

    // A consistent forgery passes the member checks; the chain does not.
    let found = tamper_case(root, "forged", &original, |copy| edit(copy, true));
    assert!(!found.is_empty(), "{found:?}");
    assert!(
        found.iter().all(|code| code.starts_with("CHECK_")),
        "{found:?}"
    );

    // A last event reformatted without a content change, then re-digested:
    // no later file names its hash, so before ADR 0016 only archive.json's
    // head caught it. Now the restored copy's check refuses its bytes, as
    // `check`, every load and `repair journal` do (see canonical_journal.rs).
    for (name, reformat) in [
        (
            "respaced-head",
            (|text: &str| text.replacen("\"sequence\"", "\"sequence\" ", 1)) as fn(&str) -> String,
        ),
        ("newline-head", |text: &str| format!("{text}\n")),
        ("escaped-head", |text: &str| {
            text.replacen("\"schema_version\":\"i", "\"schema_version\":\"\\u0069", 1)
        }),
    ] {
        let found = tamper_case(root, name, &original, |copy| {
            let path = copy.join(&event);
            let text = fs::read_to_string(&path).unwrap();
            let edited = reformat(&text);
            assert_ne!(text, edited, "{name}");
            fs::write(&path, &edited).unwrap();
            let mut manifest = manifest(copy);
            let entry = manifest
                .files
                .iter_mut()
                .find(|file| file.path == event)
                .unwrap();
            entry.sha256 = hash_bytes(edited.as_bytes());
            entry.bytes = edited.len() as u64;
            manifest.export_id = expected_export_id(&manifest);
            write_manifest(copy, &manifest);
        });
        assert_eq!(found, ["CHECK_NON_CANONICAL_JOURNAL_FILE"], "{name}");
    }

    // A journal cut short with a consistent manifest restores to an earlier
    // head than archive.json names.
    let found = tamper_case(root, "truncated", &original, |copy| {
        fs::remove_file(copy.join(&event)).unwrap();
        let mut manifest = manifest(copy);
        manifest.files.retain(|file| file.path != event);
        manifest.export_id = expected_export_id(&manifest);
        write_manifest(copy, &manifest);
    });
    assert_eq!(found, ["HEAD_MISMATCH"]);

    // A blob dropped from a consistent manifest is missing from the index.
    let found = tamper_case(root, "unindexed", &original, |copy| {
        fs::remove_file(copy.join(&blob)).unwrap();
        let mut manifest = manifest(copy);
        manifest.files.retain(|file| file.path != blob);
        manifest.export_id = expected_export_id(&manifest);
        write_manifest(copy, &manifest);
    });
    assert_eq!(found, ["INDEX_MISMATCH"]);

    let found = tamper_case(root, "no-manifest", &original, |copy| {
        fs::remove_file(copy.join("export-manifest.json")).unwrap();
    });
    assert_eq!(found, ["MANIFEST_MISSING"]);

    // The untouched original still verifies.
    let (exit, value) = verify(root, &original);
    assert_eq!(exit, 0, "{value}");
}

#[test]
fn an_archive_is_only_for_the_restricted_audience_covers_every_case_and_waits_for_staging() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-a", &root.join("ws"), false);
    let out: PathBuf = root.join("archive");
    for (extra, path) in [
        (&["--audience", "internal"][..], "--audience"),
        (&["--audience", "public"][..], "--audience"),
        (
            &["--audience", "restricted", "--snapshot", "1"][..],
            "--snapshot",
        ),
    ] {
        let (exit, value) = archive(root, &workspace, &out, extra);
        assert_eq!(exit, 2, "{value}");
        assert_eq!(value["error"]["path"], path, "{value}");
        assert!(!out.exists());
    }
    let case = fs::read_dir(workspace.join("state/cases"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .into_string()
        .unwrap();
    let (exit, value) = run(
        root,
        &[
            "--workspace",
            workspace.to_str().unwrap(),
            "--case",
            &case,
            "export",
            "--kind",
            "workspace-archive",
            "--audience",
            "restricted",
            "--out",
            out.to_str().unwrap(),
        ],
    );
    assert_eq!(exit, 2, "{value}");
    assert_eq!(value["error"]["path"], "--case");
    assert!(!out.exists());

    // An open capture holds staged evidence the journal does not reference.
    let (exit, value) = run(root, &["init", "staging", "--no-vault"]);
    assert_eq!(exit, 0, "{value}");
    let begin = json!({
        "key": "run-0001/begin",
        "actor": "agent:example-producer",
        "producer": {"name": "example-producer", "version": "0.1.0"},
        "external_run_id": "run-0001",
        "intent": {"label": "synthetic run"},
        "case": {"kind": "new_record_case", "title": "Synthetic runs"}
    });
    fs::write(root.join("begin.json"), serde_json::to_vec(&begin).unwrap()).unwrap();
    let (exit, value) = run(
        root,
        &[
            "--workspace",
            "staging",
            "capture",
            "begin",
            "--input",
            "begin.json",
        ],
    );
    assert_eq!(exit, 0, "{value}");
    let (exit, value) = archive(
        root,
        &root.join("staging"),
        &out,
        &["--audience", "restricted"],
    );
    assert_eq!(exit, 5, "{value}");
    assert_eq!(value["error"]["code"], "GATE_FAILED");
    assert_eq!(value["error"]["actual"]["active"], 1, "{value}");
    assert!(!out.exists());
    assert_eq!(pin_records(&root.join("staging")), 0);

    // Verification needs no workspace and refuses what is not a directory.
    let (exit, value) = run(root, &["archive", "verify", "begin.json"]);
    assert_eq!(exit, 2, "{value}");
}
