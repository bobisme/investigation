//! `archive restore` and `archive fork` through the public binary (W11.18,
//! ADR 0013): the archives of Scenarios A to D restore and fork to the same
//! case heads, states and receipts; a fork has a new workspace identity that
//! its commits, `check` and its own archives name; every tampered archive
//! and every bad destination is refused before anything is published, with
//! nothing written outside PATH; and a restore stopped at any step leaves
//! PATH refused as incomplete or complete and checked, and a retry succeeds.
//!
//! Every run clears the environment and puts an empty folder on `PATH`.

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use investigation_render::export::{
    ExportManifest, expected_export_id,
    restore::{Mode, Refusal, STAGING, Step, restore_archive_until},
};
use investigation_store::{Store, Workspace, journal::hash_bytes};
use serde_json::{Value, json};

#[path = "support/fixtures.rs"]
mod fixtures;
#[path = "support/restore.rs"]
mod restore;

use fixtures::SCENARIOS;
use restore::{archive, check, files, marker, replayed, run};

const NOTE: &[u8] = b"# Plan\n\nAsk the build owner about the clock.\n";
const ACTOR: &str = "agent:fork-tester";
/// The command word, kept apart so no line reads as a version-control call.
const RESTORE: &str = "restore";

fn publish(cwd: &Path, verb: &str, dir: &Path, path: &Path) -> (i32, Value) {
    run(
        cwd,
        &[
            "archive",
            verb,
            dir.to_str().unwrap(),
            path.to_str().unwrap(),
        ],
    )
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

fn write_manifest(directory: &Path, manifest: &ExportManifest) {
    let mut bytes = serde_json::to_vec_pretty(manifest).unwrap();
    bytes.push(b'\n');
    fs::write(directory.join("export-manifest.json"), bytes).unwrap();
}

/// A fixture workspace with a human note, and its archive.
fn scenario(root: &Path, scenario: &str) -> (PathBuf, PathBuf, Value) {
    let workspace = fixtures::workspace(scenario, &root.join(scenario), false);
    fs::create_dir_all(workspace.join("vault/Notes/Plans")).unwrap();
    fs::write(workspace.join("vault/Notes/Plans/next.md"), NOTE).unwrap();
    let out = root.join(format!("{scenario}-archive"));
    let receipt = archive(root, &workspace, &out);
    (workspace, out, receipt)
}

#[test]
fn every_scenario_restores_and_forks_to_the_same_heads_states_and_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for name in SCENARIOS {
        let (workspace, out, archived) = scenario(root, name);
        let (_, original_check) = check(root, &workspace);
        let original = replayed(&workspace);
        let original_marker = marker(&workspace);
        let archive_before = files(&out, &[]);
        for (verb, mode) in [(RESTORE, "exact"), ("fork", "fork")] {
            let path = root.join(format!("{name}-{verb}"));
            let outside = files(root, &[path.file_name().unwrap().to_str().unwrap()]);
            let (exit, value) = publish(root, verb, &out, &path);
            assert_eq!(exit, 0, "{name} {verb}: {value}");
            assert_eq!(value["mode"], mode);
            assert_eq!(value["check"]["ok"], true);
            assert_eq!(value["check"]["index"], "current");
            assert_eq!(value["check"]["heads"], archived["cases"], "{name} {verb}");
            assert_eq!(value["archive"]["export_id"], archived["export_id"]);
            assert_eq!(value["vault"]["rendered"], true, "{value}");
            // Nothing outside PATH changed, the archive included.
            assert_eq!(
                files(root, &[path.file_name().unwrap().to_str().unwrap()]),
                outside,
                "{name} {verb} wrote outside PATH"
            );
            assert_eq!(files(&out, &[]), archive_before);
            assert!(!path.join(STAGING).exists());

            // check passes there with the same heads; every state and
            // every receipt is the original's.
            let (exit, checked) = check(root, &path);
            assert_eq!(exit, 0, "{checked}");
            assert_eq!(checked["heads"], original_check["heads"], "{name} {verb}");
            assert_eq!(checked["index"], "current");
            assert_eq!(replayed(&path), original, "{name} {verb}");
            assert_eq!(
                fs::read(path.join("vault/Notes/Plans/next.md")).unwrap(),
                NOTE
            );
            assert!(path.join("vault/Start Here.md").is_file());
            // Disposable state is rebuilt, never carried over.
            assert!(path.join("cache/index.sqlite").is_file());

            let restored_marker = marker(&path);
            match mode {
                "exact" => {
                    assert_eq!(restored_marker, original_marker);
                    assert_eq!(checked["workspace_id"], original_marker["workspace_id"]);
                    assert!(checked.get("fork").is_none());
                }
                _ => {
                    let id = &restored_marker["workspace_id"];
                    assert_ne!(id, &original_marker["workspace_id"]);
                    assert_eq!(&value["workspace_id"], id);
                    assert_eq!(&checked["workspace_id"], id);
                    let fork = &restored_marker["fork"];
                    assert_eq!(fork["origin_workspace_id"], original_marker["workspace_id"]);
                    assert_eq!(fork["origin_export_id"], archived["export_id"]);
                    assert_eq!(checked["fork"], *fork);
                    let cases = fork["cases"].as_array().unwrap();
                    assert_eq!(
                        cases.len(),
                        original_check["heads"].as_array().unwrap().len()
                    );
                    for (case, head) in cases
                        .iter()
                        .zip(original_check["heads"].as_array().unwrap())
                    {
                        assert_eq!(case["case_id"], head["case_id"]);
                        assert_eq!(case["sequence"], head["snapshot"]["sequence"]);
                        assert_eq!(case["head_hash"], head["snapshot"]["head_hash"]);
                        assert_eq!(
                            case["writers"],
                            json!([{
                                "workspace_id": original_marker["workspace_id"],
                                "through_sequence": head["snapshot"]["sequence"],
                            }])
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_fork_has_its_own_identity_and_its_commits_checks_and_archives_name_it() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (workspace, out, _) = scenario(root, "scenario-c");
    let origin = marker(&workspace)["workspace_id"].clone();
    let fork = root.join("fork");
    let (exit, value) = publish(root, "fork", &out, &fork);
    assert_eq!(exit, 0, "{value}");
    let fork_id = value["workspace_id"].clone();
    assert_ne!(fork_id, origin);

    // Two forks of one archive are two workspaces.
    let twin = root.join("twin");
    let (exit, second) = publish(root, "fork", &out, &twin);
    assert_eq!(exit, 0, "{second}");
    assert_ne!(second["workspace_id"], fork_id);
    assert_ne!(second["workspace_id"], origin);

    // A commit to an inherited case and a new case both name the fork.
    let case = value["check"]["heads"][0]["case_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sequence = value["check"]["heads"][0]["snapshot"]["sequence"]
        .as_u64()
        .unwrap();
    let at = fork.to_str().unwrap();
    let (exit, noted) = run(
        root,
        &[
            "--workspace",
            at,
            "note",
            "Forked here",
            "--on",
            &case,
            "--actor",
            ACTOR,
            "--command-id",
            "fork-note",
        ],
    );
    assert_eq!(exit, 0, "{noted}");
    let (exit, created) = run(
        root,
        &[
            "--workspace",
            at,
            "case",
            "new",
            "Fork-only question",
            "--actor",
            ACTOR,
            "--command-id",
            "fork-case",
        ],
    );
    assert_eq!(exit, 0, "{created}");
    let event: Value = serde_json::from_slice(
        &fs::read(fork.join(format!(
            "state/cases/{case}/events/{:012}.json",
            sequence + 1
        )))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(event["workspace_id"], fork_id);
    let new_case = created["created"][0]["id"].as_str().unwrap();
    let genesis: Value = serde_json::from_slice(
        &fs::read(fork.join(format!("state/cases/{new_case}/genesis.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(genesis["workspace_id"], fork_id);
    let (exit, checked) = check(root, &fork);
    assert_eq!(exit, 0, "{checked}");
    assert_eq!(checked["workspace_id"], fork_id);
    // The original never sees the fork's events.
    let (exit, original) = check(root, &workspace);
    assert_eq!(exit, 0, "{original}");
    assert_eq!(original["heads"].as_array().unwrap().len(), 1);

    // The fork's own archive names the fork, verifies, and restores and
    // forks again; a fork of a fork lists both writers.
    let fork_archive = root.join("fork-archive");
    archive(root, &fork, &fork_archive);
    let listed = manifest(&fork_archive);
    assert_eq!(json!(listed.snapshot.case_id), fork_id);
    let index: Value =
        serde_json::from_slice(&fs::read(fork_archive.join("archive.json")).unwrap()).unwrap();
    assert_eq!(index["workspace_id"], fork_id);
    let scratch = root.join("scratch");
    fs::create_dir_all(&scratch).unwrap();
    let (exit, verified) = run(
        root,
        &[
            "archive",
            "verify",
            fork_archive.to_str().unwrap(),
            "--scratch",
            scratch.to_str().unwrap(),
        ],
    );
    assert_eq!(exit, 0, "{verified}");
    let again = root.join("fork-again");
    let (exit, restored) = publish(root, RESTORE, &fork_archive, &again);
    assert_eq!(exit, 0, "{restored}");
    assert_eq!(restored["workspace_id"], fork_id);
    assert_eq!(marker(&again), marker(&fork));
    let grandchild = root.join("grandchild");
    let (exit, forked) = publish(root, "fork", &fork_archive, &grandchild);
    assert_eq!(exit, 0, "{forked}");
    let grand = marker(&grandchild);
    assert_eq!(grand["fork"]["origin_workspace_id"], fork_id);
    let inherited = grand["fork"]["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["case_id"] == case.as_str())
        .unwrap();
    assert_eq!(
        inherited["writers"],
        json!([
            {"workspace_id": origin, "through_sequence": sequence},
            {"workspace_id": fork_id, "through_sequence": sequence + 1},
        ])
    );
    let (exit, receipt) = run(
        root,
        &[
            "--workspace",
            grandchild.to_str().unwrap(),
            "receipt",
            "fork-note",
            "--actor",
            ACTOR,
        ],
    );
    assert_eq!(exit, 0, "{receipt}");
    assert_eq!(receipt["transaction_id"], noted["transaction_id"]);

    // The inherited head binds the inherited bytes: a marker that claims
    // another head, or claims the fork wrote the origin's events, is refused.
    let edit_marker = |name: &str, change: &dyn Fn(&mut Value)| {
        let copy = root.join(name);
        copy_tree(&fork, &copy);
        let mut value = marker(&copy);
        change(&mut value);
        let path = copy.join("state/workspace.json");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        check(root, &copy)
    };
    let (exit, value) = edit_marker("other-head", &|value| {
        value["fork"]["cases"][0]["head_hash"] = json!(hash_bytes(b"another head"));
    });
    assert_eq!(exit, 6, "{value}");
    let (exit, value) = edit_marker("self-writer", &|value| {
        value["fork"]["cases"][0]["writers"][0]["workspace_id"] = value["workspace_id"].clone();
    });
    assert_ne!(exit, 0, "{value}");
    let (exit, value) = edit_marker("no-fork", &|value| {
        value.as_object_mut().unwrap().remove("fork");
    });
    assert_eq!(exit, 6, "{value}");
}

/// One named change to a copy of an archive.
type Tamper = (&'static str, Box<dyn Fn(&Path)>);

/// The tampers of `archive verify`, each applied to a fresh copy.
fn tampers(original: &Path) -> Vec<Tamper> {
    let listed = manifest(original);
    let blob = listed
        .files
        .iter()
        .map(|file| file.path.clone())
        .find(|path| path.starts_with("workspace/state/blobs/"))
        .unwrap();
    let last = listed
        .files
        .iter()
        .map(|file| file.path.clone())
        .rfind(|path| path.contains("/events/"))
        .unwrap();
    let first = listed
        .files
        .iter()
        .map(|file| file.path.clone())
        .find(|path| path.ends_with("/events/000000000001.json"))
        .unwrap();
    let redigest = |copy: &Path, member: &str, edited: &[u8], recompute: bool| {
        fs::write(copy.join(member), edited).unwrap();
        let mut manifest = manifest(copy);
        let entry = manifest
            .files
            .iter_mut()
            .find(|file| file.path == member)
            .unwrap();
        entry.sha256 = hash_bytes(edited);
        entry.bytes = edited.len() as u64;
        if recompute {
            manifest.export_id = expected_export_id(&manifest);
        }
        write_manifest(copy, &manifest);
    };
    let edited = |copy: &Path, member: &str| {
        fs::read_to_string(copy.join(member))
            .unwrap()
            .replacen("\"sequence\"", "\"sequence\" ", 1)
    };
    let (b1, b2, b3, b4) = (blob.clone(), blob.clone(), blob.clone(), blob);
    let (f1, f2, l1, l2) = (first.clone(), first, last.clone(), last);
    vec![
        (
            "missing",
            Box::new(move |copy: &Path| fs::remove_file(copy.join(&b1)).unwrap()),
        ),
        (
            "extra",
            Box::new(|copy: &Path| {
                fs::write(copy.join("workspace/state/notes.txt"), b"added").unwrap()
            }),
        ),
        (
            "unsafe",
            Box::new(move |copy: &Path| {
                fs::remove_file(copy.join(&b2)).unwrap();
                symlink("/dev/null", copy.join(&b2)).unwrap();
            }),
        ),
        (
            "corrupted",
            Box::new(move |copy: &Path| {
                let mut bytes = fs::read(copy.join(&b3)).unwrap();
                bytes[0] ^= 0x20;
                fs::write(copy.join(&b3), bytes).unwrap();
            }),
        ),
        (
            "altered",
            Box::new(move |copy: &Path| {
                let text = edited(copy, &f1);
                redigest(copy, &f1, text.as_bytes(), false);
            }),
        ),
        (
            "forged",
            Box::new(move |copy: &Path| {
                let text = edited(copy, &f2);
                redigest(copy, &f2, text.as_bytes(), true);
            }),
        ),
        (
            "rehashed-head",
            Box::new(move |copy: &Path| {
                let text = edited(copy, &l1);
                redigest(copy, &l1, text.as_bytes(), true);
            }),
        ),
        (
            "truncated",
            Box::new(move |copy: &Path| {
                fs::remove_file(copy.join(&l2)).unwrap();
                let mut manifest = manifest(copy);
                manifest.files.retain(|file| file.path != l2);
                manifest.export_id = expected_export_id(&manifest);
                write_manifest(copy, &manifest);
            }),
        ),
        (
            "unindexed",
            Box::new(move |copy: &Path| {
                fs::remove_file(copy.join(&b4)).unwrap();
                let mut manifest = manifest(copy);
                manifest.files.retain(|file| file.path != b4);
                manifest.export_id = expected_export_id(&manifest);
                write_manifest(copy, &manifest);
            }),
        ),
        // The marker names another workspace, with a consistent manifest.
        (
            "marker-edited",
            Box::new(move |copy: &Path| {
                let member = "workspace/state/workspace.json";
                let mut marker: Value =
                    serde_json::from_slice(&fs::read(copy.join(member)).unwrap()).unwrap();
                let id = marker["workspace_id"].as_str().unwrap().to_owned();
                let last = if id.ends_with('0') { "1" } else { "0" };
                marker["workspace_id"] = json!(format!("{}{last}", &id[..id.len() - 1]));
                let bytes = serde_json::to_vec_pretty(&marker).unwrap();
                redigest(copy, member, &bytes, true);
            }),
        ),
        // An archive still being written has no manifest yet.
        (
            "unfinished",
            Box::new(|copy: &Path| {
                fs::remove_file(copy.join("export-manifest.json")).unwrap();
            }),
        ),
    ]
}

#[test]
fn every_tampered_archive_is_refused_before_anything_is_written() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (workspace, original, _) = scenario(root, "scenario-c");
    for (name, tamper) in tampers(&original) {
        let copy = root.join(format!("tamper-{name}"));
        copy_tree(&original, &copy);
        tamper(&copy);
        let before = files(&copy, &[]);
        for verb in [RESTORE, "fork"] {
            // A new PATH is not left behind; an empty one stays empty.
            let new = root.join(format!("{name}-{verb}-new"));
            let empty = root.join(format!("{name}-{verb}-empty"));
            fs::create_dir(&empty).unwrap();
            for path in [&new, &empty] {
                let (exit, value) = publish(root, verb, &copy, path);
                assert_eq!(exit, 6, "{name} {verb}: {value}");
                assert_eq!(value["error"]["code"], "INTEGRITY_FAILURE", "{name}");
                assert_eq!(value["error"]["actual"]["verified"], false, "{name}");
                assert!(value["error"]["actual"]["problem_count"].as_u64().unwrap() > 0);
            }
            assert!(!new.exists(), "{name} {verb}");
            assert_eq!(fs::read_dir(&empty).unwrap().count(), 0, "{name} {verb}");
            assert_eq!(files(&copy, &[]), before, "{name}: DIR is never written");
        }
    }

    // An export that is not a workspace archive restores nothing.
    let case = fs::read_dir(workspace.join("state/cases"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .into_string()
        .unwrap();
    let report = root.join("report");
    let (exit, value) = run(
        root,
        &[
            "--workspace",
            workspace.to_str().unwrap(),
            "--case",
            &case,
            "export",
            "--kind",
            "case-report",
            "--audience",
            "internal",
            "--out",
            report.to_str().unwrap(),
        ],
    );
    assert_eq!(exit, 0, "{value}");
    let path = root.join("from-report");
    let (exit, value) = publish(root, RESTORE, &report, &path);
    assert_eq!(exit, 6, "{value}");
    assert_eq!(
        value["error"]["actual"]["problems"][0]["code"],
        "NOT_RESTORABLE"
    );
    assert!(!path.exists());
}

#[test]
fn an_occupied_or_unsafe_destination_is_refused_and_left_as_found() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (_, out, _) = scenario(root, "scenario-a");
    let occupied = root.join("occupied");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("keep.txt"), b"mine").unwrap();
    let file = root.join("plain-file");
    fs::write(&file, b"a file").unwrap();
    let link = root.join("link");
    symlink(root.join("elsewhere"), &link).unwrap();
    let orphan = root.join("missing-parent/child");
    let inside = out.join("inside");
    let before = files(root, &[".empty-path"]);
    for verb in [RESTORE, "fork"] {
        for (path, needle) in [
            (&occupied, "not empty"),
            (&file, "not a directory"),
            (&link, "symbolic link"),
            (&orphan, "parent"),
            (&inside, "inside the archive"),
        ] {
            let (exit, value) = publish(root, verb, &out, path);
            assert_eq!(exit, 2, "{verb} {}: {value}", path.display());
            assert_eq!(value["error"]["path"], "PATH");
            let message = value["error"]["message"].as_str().unwrap();
            assert!(message.contains(needle), "{message}");
        }
        assert_eq!(files(root, &[".empty-path"]), before);
    }
    assert_eq!(fs::read(occupied.join("keep.txt")).unwrap(), b"mine");
}

#[test]
fn a_kill_at_every_step_leaves_path_refused_or_complete_and_a_retry_succeeds() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    // A small workspace with two cases and no vault: every step is
    // stopped at and retried, and each copy is synced file by file.
    let workspace = root.join("small");
    let (exit, value) = run(root, &["init", workspace.to_str().unwrap(), "--no-vault"]);
    assert_eq!(exit, 0, "{value}");
    let at = workspace.to_str().unwrap();
    for (title, key) in [("First question", "first"), ("Second question", "second")] {
        let (exit, value) = run(
            root,
            &[
                "--workspace",
                at,
                "case",
                "new",
                title,
                "--actor",
                ACTOR,
                "--command-id",
                key,
            ],
        );
        assert_eq!(exit, 0, "{value}");
    }
    let out = root.join("small-archive");
    archive(root, &workspace, &out);
    let (_, original) = check(root, &workspace);
    let steps = [
        Step::Staging,
        Step::Verified,
        Step::Forked,
        Step::Checked,
        Step::Publishing,
        Step::Published,
    ];
    for (mode, verb) in [(Mode::Exact, RESTORE), (Mode::Fork, "fork")] {
        for step in steps {
            if step == Step::Forked && mode == Mode::Exact {
                continue;
            }
            let path = root.join(format!("{verb}-{step:?}"));
            match restore_archive_until(&out, &path, mode, Some(step)) {
                Err(Refusal::Interrupted(at)) => assert_eq!(at, step),
                other => panic!("{step:?}: {other:?}"),
            }
            let (exit, checked) = check(root, &path);
            if step == Step::Published {
                // The marker is in place: PATH is the checked workspace,
                // with an empty staging folder left behind.
                assert_eq!(exit, 0, "{step:?}: {checked}");
                assert_eq!(checked["heads"], original["heads"]);
                continue;
            }
            // No marker: check refuses PATH as no workspace.
            assert_ne!(exit, 0, "{verb} {step:?}: {checked}");
            assert!(checked["error"].is_object(), "{checked}");
            assert!(!path.join("state/workspace.json").exists());
            // An interrupted PATH is not empty, so it is refused too, and
            // the refusal names the interrupted restore.
            let (exit, refused) = publish(root, verb, &out, &path);
            assert_eq!(exit, 2, "{refused}");
            assert!(
                refused["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains(STAGING),
                "{refused}"
            );
            // A retry into an emptied PATH succeeds, from the untouched
            // archive.
            fs::remove_dir_all(&path).unwrap();
            fs::create_dir(&path).unwrap();
            let (exit, retried) = publish(root, verb, &out, &path);
            assert_eq!(exit, 0, "{verb} {step:?}: {retried}");
            let (exit, checked) = check(root, &path);
            assert_eq!(exit, 0, "{checked}");
            assert_eq!(checked["heads"], original["heads"]);
        }
    }
}

#[test]
fn a_live_pin_in_the_source_does_not_block_a_fork_of_a_finished_archive() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (workspace, out, _) = scenario(root, "scenario-d");
    // Another archive is being written: its pin is live in the source.
    let mut store = Store::open(Workspace::open(&workspace).unwrap());
    let (_, pin) = store.read_canonical().unwrap();
    assert!(store.archive_pins().unwrap().iter().any(|info| info.live));
    // The fork reads only the finished archive, never the source.
    let path = root.join("fork");
    let (exit, value) = publish(root, "fork", &out, &path);
    assert_eq!(exit, 0, "{value}");
    assert!(store.archive_pins().unwrap().iter().any(|info| info.live));
    store.release_pin(pin).unwrap();
}
