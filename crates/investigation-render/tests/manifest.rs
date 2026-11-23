//! Path ownership manifest: the publisher writes or deletes only files proven
//! owned, and losing the manifest destroys nothing.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    time::Duration,
};

use investigation_core::identity::Id;
use investigation_render::{
    Action, CaseRenderer, Decision, Generation, MANIFEST_VERSION, Manifest, ManifestStatus, OnDisk,
    Ownership, Phase, PlanBuilder, PlanEntry, PlanPath, Publisher, RENDER_MANIFEST, Refusal,
    RenderError, RenderLock, RenderPlan, RenderReport, VaultRenderer, VerifiedSnapshot, ViewPolicy,
    render_vault,
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

fn submit(store: &mut Store, key: &str, case_id: Option<Id>, operation: Value) -> Id {
    let input = json!({
        "schema_version":"inv.command.v0-alpha",
        "command_id":key,
        "case_id":case_id,
        "actor":"agent:manifest-test",
        "preconditions":[],
        "operations":[operation]
    });
    let receipt = store.record(&serde_json::to_vec(&input).unwrap()).unwrap();
    receipt.created[0].id
}

fn question(store: &mut Store, key: &str, case_id: Id) -> Id {
    submit(
        store,
        key,
        Some(case_id),
        json!({"op":"question.create", "payload":{"wording":key, "importance":"normal"}}),
    )
}

/// A vault-enabled workspace with one case and two questions.
fn workspace(directory: &Path) -> (Store, Id) {
    let workspace = Workspace::init(&directory.join("workspace"), true).unwrap();
    let mut store = Store::open(workspace);
    let case_id = submit(
        &mut store,
        "case",
        None,
        json!({"op":"case.create", "payload":{"title":"Manifest fixture", "mode":"record"}}),
    );
    question(&mut store, "q1", case_id);
    question(&mut store, "q2", case_id);
    (store, case_id)
}

fn vault(store: &Store) -> PathBuf {
    store.workspace().root().join("vault")
}

fn manifest_file(store: &Store) -> PathBuf {
    store.workspace().root().join(RENDER_MANIFEST)
}

fn render(store: &mut Store) -> RenderReport {
    render_vault(store, &VaultRenderer, &ViewPolicy::default(), &mut |_| {}).unwrap()
}

/// Publishes a fixed plan, for removal scenarios the vault layout cannot
/// produce yet.
struct Fixed(RenderPlan);

impl CaseRenderer for Fixed {
    fn render(&self, _: &VerifiedSnapshot, _: &ViewPolicy) -> Result<RenderPlan, RenderError> {
        Ok(self.0.clone())
    }
}

fn fixed(store: &mut Store, files: &[(&str, &str)]) -> Fixed {
    let policy = ViewPolicy::default();
    let snapshot = VerifiedSnapshot::read(store).unwrap();
    let mut builder = PlanBuilder::new(Generation::new("test", &snapshot, &policy));
    for (path, content) in files {
        builder
            .add(
                Phase::Records,
                PlanPath::new(*path).unwrap(),
                *content,
                None,
            )
            .unwrap();
    }
    Fixed(builder.finish())
}

fn render_fixed(store: &mut Store, plan: &Fixed) -> RenderReport {
    render_vault(store, plan, &ViewPolicy::default(), &mut |_| {}).unwrap()
}

/// Every file under `root`, including hidden ones and symbolic links, with
/// its bytes (a link's target for links).
fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let relative = path.strip_prefix(root).unwrap().to_owned();
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).unwrap();
                found.insert(relative, target.into_os_string().into_encoded_bytes());
            } else {
                found.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    found
}

fn action(report: &RenderReport, path: &str) -> Action {
    report
        .publish
        .outcomes
        .iter()
        .find(|outcome| outcome.path.as_str() == path)
        .unwrap_or_else(|| panic!("no outcome for {path}"))
        .action
}

fn loaded(store: &Store) -> (Manifest, ManifestStatus) {
    let lock = RenderLock::acquire(store.workspace(), Duration::ZERO).unwrap();
    let loaded = Manifest::load(store.workspace(), &lock).unwrap();
    (loaded.manifest, loaded.status)
}

#[test]
fn a_foreign_file_inside_a_case_folder_survives_any_number_of_renders() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    let first = render(&mut store);
    assert_eq!(first.manifest, ManifestStatus::Missing);
    assert_eq!(first.publish.written(), 4);
    assert!(first.unknown.is_empty(), "{:?}", first.unknown);

    let vault = vault(&store);
    let foreign = [
        "Attachments/unlisted.png".to_owned(),
        format!("Cases/{case_id}/Questions/mine.md"),
        format!("Cases/{case_id}/mine.md"),
    ];
    fs::create_dir_all(vault.join("Attachments")).unwrap();
    for path in &foreign {
        fs::write(vault.join(path), format!("foreign {path}\n")).unwrap();
    }
    fs::write(vault.join("Notes/thinking.md"), "human\n").unwrap();
    fs::write(vault.join(format!("Cases/{case_id}/.hidden")), "x").unwrap();

    for round in 0..5 {
        question(&mut store, &format!("round {round}"), case_id);
        let report = render(&mut store);
        assert_eq!(report.manifest, ManifestStatus::Loaded);
        assert_eq!(report.publish.refused(), 0);
        assert_eq!(report.publish.written(), report.publish.outcomes.len());
        // Unknown files are reported, never adopted; human roots and hidden
        // entries are not reported.
        assert_eq!(report.unknown, foreign.to_vec());
        assert!(report.adopted.is_empty());
    }
    for path in &foreign {
        assert_eq!(
            fs::read_to_string(vault.join(path)).unwrap(),
            format!("foreign {path}\n")
        );
    }

    // Retire every generated file: owned files go, foreign files and the
    // folders that still hold them stay.
    let only_entry = fixed(&mut store, &[("Start Here.md", "only\n")]);
    let report = render_fixed(&mut store, &only_entry);
    assert_eq!(
        report.publish.removed(),
        8,
        "seven questions and the overview"
    );
    assert_eq!(report.publish.refused(), 0);
    let left: Vec<_> = tree(&vault).into_keys().collect();
    let mut expected: Vec<PathBuf> = foreign.iter().map(PathBuf::from).collect();
    expected.push(PathBuf::from(format!("Cases/{case_id}/.hidden")));
    expected.push(PathBuf::from("Notes/thinking.md"));
    expected.push(PathBuf::from("Start Here.md"));
    expected.sort();
    assert_eq!(left, expected);
    for path in &foreign {
        assert_eq!(
            fs::read_to_string(vault.join(path)).unwrap(),
            format!("foreign {path}\n")
        );
    }
    let (manifest, _) = loaded(&store);
    let owned: Vec<_> = manifest
        .owned()
        .map(|record| record.path.as_str())
        .collect();
    assert_eq!(owned, ["Start Here.md"]);
}

#[test]
fn deleting_the_manifest_and_rerendering_destroys_no_file() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    let first = render(&mut store);
    let vault = vault(&store);
    let planned: Vec<PlanPath> = first
        .publish
        .outcomes
        .iter()
        .map(|outcome| outcome.path.clone())
        .collect();

    // Same snapshot: every file is byte-identical to the plan, so the
    // manifest is rebuilt by adoption, and the adoption is reported.
    let before = tree(&vault);
    fs::remove_file(manifest_file(&store)).unwrap();
    let report = render(&mut store);
    assert_eq!(report.manifest, ManifestStatus::Missing);
    assert_eq!(report.publish.written(), 0);
    assert_eq!(report.publish.unchanged(), planned.len());
    assert_eq!(report.adopted, planned);
    assert_eq!(tree(&vault), before);

    // New snapshot with the manifest lost: every existing file now differs
    // from the plan and is foreign; it is refused, never overwritten.
    fs::remove_file(manifest_file(&store)).unwrap();
    let edited = format!("Cases/{case_id}/Overview.md");
    fs::write(vault.join(&edited), "edited by hand\n").unwrap();
    let before = tree(&vault);
    let added = question(&mut store, "q3", case_id);
    let report = render(&mut store);
    assert_eq!(report.publish.written(), 1, "only the new question note");
    assert_eq!(report.publish.removed(), 0);
    assert_eq!(report.publish.refused(), planned.len());
    for path in &planned {
        assert_eq!(
            action(&report, path.as_str()),
            Action::Refused(Refusal::NotProvenOwned)
        );
    }
    let after = tree(&vault);
    for (path, bytes) in &before {
        assert_eq!(after.get(path), Some(bytes), "{path:?} changed");
    }
    assert!(after.contains_key(Path::new(&format!("Cases/{case_id}/Questions/{added}.md"))));
    let mut unknown: Vec<_> = planned.iter().map(|path| path.to_string()).collect();
    unknown.sort();
    assert_eq!(report.unknown, unknown);

    // A corrupt manifest is rebuilt the same conservative way.
    fs::write(manifest_file(&store), "{ not json").unwrap();
    let before = tree(&vault);
    let report = render(&mut store);
    assert!(matches!(report.manifest, ManifestStatus::Rebuilt { .. }));
    assert_eq!(report.publish.written(), 0);
    assert_eq!(tree(&vault), before);

    // Retiring everything with no proof of ownership removes nothing.
    fs::remove_file(manifest_file(&store)).unwrap();
    let before = tree(&vault);
    let empty = fixed(&mut store, &[]);
    let report = render_fixed(&mut store, &empty);
    assert!(report.publish.outcomes.is_empty());
    assert_eq!(tree(&vault), before);
}

#[test]
fn an_edited_owned_file_is_drift_and_is_never_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    render(&mut store);
    let vault = vault(&store);
    let overview = format!("Cases/{case_id}/Overview.md");
    let generated = fs::read(vault.join(&overview)).unwrap();
    let (manifest, _) = loaded(&store);
    let owned_hash = manifest
        .get(&PlanPath::new(overview.clone()).unwrap())
        .unwrap()
        .sha256
        .clone();

    fs::write(vault.join(&overview), "a human edited a generated note\n").unwrap();
    for round in 0..2 {
        question(&mut store, &format!("q{}", round + 3), case_id);
        let report = render(&mut store);
        assert_eq!(
            action(&report, &overview),
            Action::Refused(Refusal::Drifted)
        );
        assert_eq!(report.publish.refused(), 1, "the rest still renders");
        assert_eq!(
            fs::read_to_string(vault.join(&overview)).unwrap(),
            "a human edited a generated note\n"
        );
        // Drift stays owned at the last generated hash, so it is not reported
        // as unknown and is refused again next time.
        assert!(report.unknown.is_empty());
        let (manifest, _) = loaded(&store);
        let record = manifest
            .get(&PlanPath::new(overview.clone()).unwrap())
            .unwrap();
        assert_eq!(record.sha256, owned_hash);
    }

    // Putting back the generated bytes ends the drift.
    fs::write(vault.join(&overview), generated).unwrap();
    let report = render(&mut store);
    assert_eq!(action(&report, &overview), Action::Written);
    assert_eq!(report.publish.refused(), 0);
}

#[test]
fn retired_files_are_removed_only_while_their_bytes_match() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, _) = workspace(directory.path());
    let vault = vault(&store);
    let full = fixed(
        &mut store,
        &[
            ("Cases/c/Extra/a.md", "a\n"),
            ("Cases/c/Extra/b.md", "b\n"),
            ("Cases/c/Solo/only.md", "only\n"),
            ("Cases/c/Kept/x.md", "x\n"),
            ("Cases/c/Linked.md", "linked\n"),
            ("Top.md", "top\n"),
        ],
    );
    assert_eq!(render_fixed(&mut store, &full).publish.written(), 6);
    fs::write(vault.join("Cases/c/Extra/b.md"), "edited\n").unwrap();
    fs::write(vault.join("Cases/c/Kept/.sync-state"), "foreign\n").unwrap();
    let outside = directory.path().join("outside.md");
    fs::write(&outside, "linked\n").unwrap();
    fs::remove_file(vault.join("Cases/c/Linked.md")).unwrap();
    symlink(&outside, vault.join("Cases/c/Linked.md")).unwrap();

    let empty = fixed(&mut store, &[]);
    let report = render_fixed(&mut store, &empty);
    assert_eq!(action(&report, "Cases/c/Extra/a.md"), Action::Removed);
    assert_eq!(action(&report, "Top.md"), Action::Removed);
    assert_eq!(action(&report, "Cases/c/Solo/only.md"), Action::Removed);
    assert_eq!(action(&report, "Cases/c/Kept/x.md"), Action::Removed);
    assert_eq!(
        action(&report, "Cases/c/Extra/b.md"),
        Action::Refused(Refusal::Drifted)
    );
    assert_eq!(
        action(&report, "Cases/c/Linked.md"),
        Action::Refused(Refusal::NotARegularFile)
    );
    assert_eq!(
        fs::read_to_string(vault.join("Cases/c/Extra/b.md")).unwrap(),
        "edited\n"
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "linked\n");
    // An emptied folder goes; a folder with anything left in it stays.
    assert!(!vault.join("Cases/c/Solo").exists());
    assert!(!vault.join("Cases/c/Extra/a.md").exists());
    assert_eq!(
        fs::read_to_string(vault.join("Cases/c/Kept/.sync-state")).unwrap(),
        "foreign\n"
    );

    // Refused paths stay owned; restoring the generated bytes lets the next
    // render retire them, and emptied folders below the top level go too.
    let (manifest, _) = loaded(&store);
    let owned: Vec<_> = manifest
        .owned()
        .map(|record| record.path.as_str())
        .collect();
    assert_eq!(owned, ["Cases/c/Extra/b.md", "Cases/c/Linked.md"]);
    fs::write(vault.join("Cases/c/Extra/b.md"), "b\n").unwrap();
    fs::remove_file(vault.join("Cases/c/Linked.md")).unwrap();
    fs::remove_file(vault.join("Cases/c/Kept/.sync-state")).unwrap();
    // The Kept folder is not tracked: only removals prune folders.
    fs::remove_dir(vault.join("Cases/c/Kept")).unwrap();
    let report = render_fixed(&mut store, &empty);
    assert_eq!(action(&report, "Cases/c/Extra/b.md"), Action::Removed);
    assert_eq!(action(&report, "Cases/c/Linked.md"), Action::Unchanged);
    assert!(!vault.join("Cases/c").exists());
    assert!(vault.join("Cases").is_dir(), "top-level folders are kept");
    let (manifest, _) = loaded(&store);
    assert_eq!(manifest.owned().count(), 0);
}

/// Edits a retired file between the publisher's inspection and its removal.
struct RacingEditor {
    inner: Manifest,
    vault: PathBuf,
}

impl Ownership for RacingEditor {
    fn decide(&self, entry: &PlanEntry, on_disk: &OnDisk) -> Decision {
        self.inner.decide(entry, on_disk)
    }

    fn retired(&self, plan: &RenderPlan) -> Vec<PlanPath> {
        self.inner.retired(plan)
    }

    fn decide_removal(&self, path: &PlanPath, on_disk: &OnDisk) -> Decision {
        let decision = self.inner.decide_removal(path, on_disk);
        if decision == Decision::Remove {
            fs::write(self.vault.join(path.as_str()), "edited mid-removal\n").unwrap();
        }
        decision
    }
}

#[test]
fn a_file_edited_during_removal_is_put_back() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, _) = workspace(directory.path());
    let vault = vault(&store);
    let one = fixed(&mut store, &[("Cases/c/a.md", "a\n")]);
    render_fixed(&mut store, &one);
    let (manifest, _) = loaded(&store);
    let empty = fixed(&mut store, &[]);
    let lock = RenderLock::acquire(store.workspace(), Duration::ZERO).unwrap();
    let racing = RacingEditor {
        inner: manifest,
        vault: vault.clone(),
    };
    let report = Publisher::open(store.workspace(), &lock)
        .unwrap()
        .publish(&empty.0, &racing, &mut |_| {})
        .unwrap();
    assert_eq!(report.outcomes[0].action, Action::Refused(Refusal::Drifted));
    assert_eq!(
        fs::read_to_string(vault.join("Cases/c/a.md")).unwrap(),
        "edited mid-removal\n"
    );
    let names: Vec<_> = tree(&vault).into_keys().collect();
    assert_eq!(names, [PathBuf::from("Cases/c/a.md")]);
}

#[test]
fn the_manifest_round_trips_and_tampering_only_makes_it_more_cautious() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    let first = render(&mut store);
    let (manifest, status) = loaded(&store);
    assert_eq!(status, ManifestStatus::Loaded);
    assert_eq!(manifest.to_json(), fs::read(manifest_file(&store)).unwrap());
    assert_eq!(manifest.workspace_id(), Some(store.workspace().id()));
    let complete = manifest.last_complete().unwrap();
    assert_eq!(complete.id, first.publish.generation.id());
    assert_eq!(complete.heads, first.publish.generation.heads());
    let overview = PlanPath::new(format!("Cases/{case_id}/Overview.md")).unwrap();
    let record = manifest.get(&overview).unwrap();
    assert_eq!(record.projects, Some(case_id));
    assert_eq!(record.generation, first.publish.generation.id());
    let text: Value = serde_json::from_slice(&manifest.to_json()).unwrap();
    assert_eq!(text["schema_version"], MANIFEST_VERSION);
    assert_eq!(text["owned"].as_array().unwrap().len(), 4);

    let other = Workspace::init(&directory.path().join("other"), true)
        .unwrap()
        .id();
    let rewrite = |store: &Store, edit: &dyn Fn(&mut Value)| {
        let mut value = text.clone();
        edit(&mut value);
        fs::write(manifest_file(store), serde_json::to_vec(&value).unwrap()).unwrap();
    };
    // Paths that escape, point into a human root, repeat, or come from another
    // workspace discard the whole record.
    for edit in [
        &(|value: &mut Value| value["owned"][0]["path"] = json!("../outside.md"))
            as &dyn Fn(&mut Value),
        &|value| value["owned"][0]["path"] = json!("Notes/mine.md"),
        &|value| value["owned"][1]["path"] = value["owned"][0]["path"].clone(),
        &|value| value["workspace_id"] = json!(other.to_string()),
        &|value| value["owned"][0]["sha256"] = json!("not-a-digest"),
    ] {
        rewrite(&store, edit);
        let (manifest, status) = loaded(&store);
        assert!(
            matches!(status, ManifestStatus::Rebuilt { .. }),
            "{status:?}"
        );
        assert_eq!(manifest.owned().count(), 0);
    }

    // An unknown format is never overwritten by this build.
    rewrite(&store, &|value| {
        value["schema_version"] = json!("inv.render-manifest.v9")
    });
    let before = fs::read(manifest_file(&store)).unwrap();
    let error = render_vault(
        &mut store,
        &VaultRenderer,
        &ViewPolicy::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(error, RenderError::UnsupportedManifest { .. }));
    assert_eq!(fs::read(manifest_file(&store)).unwrap(), before);

    // A symbolic link in place of the manifest is never followed: it is
    // treated as lost, and storing replaces the link, not its target.
    let elsewhere = directory.path().join("elsewhere.json");
    fs::write(&elsewhere, "not mine\n").unwrap();
    fs::remove_file(manifest_file(&store)).unwrap();
    symlink(&elsewhere, manifest_file(&store)).unwrap();
    let report = render(&mut store);
    assert!(matches!(report.manifest, ManifestStatus::Rebuilt { .. }));
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "not mine\n");
    assert!(
        !fs::symlink_metadata(manifest_file(&store))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_failed_render_still_records_the_files_it_wrote() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    render(&mut store);
    let added = question(&mut store, "q3", case_id);
    // Make the case folder unwritable: the question notes in its subfolder
    // are written, then the overview fails.
    let case_dir = vault(&store).join(format!("Cases/{case_id}"));
    fs::set_permissions(&case_dir, fs::Permissions::from_mode(0o500)).unwrap();
    if fs::write(case_dir.join("probe"), "").is_ok() {
        // Permission bits do not bind this user (for example root); skip.
        fs::remove_file(case_dir.join("probe")).unwrap();
        fs::set_permissions(&case_dir, fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }
    let result = render_vault(
        &mut store,
        &VaultRenderer,
        &ViewPolicy::default(),
        &mut |_| {},
    );
    fs::set_permissions(&case_dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result, Err(RenderError::Io { .. })));

    let (manifest, status) = loaded(&store);
    assert_eq!(status, ManifestStatus::Loaded);
    let note = PlanPath::new(format!("Cases/{case_id}/Questions/{added}.md")).unwrap();
    let bytes = fs::read(vault(&store).join(note.as_str())).unwrap();
    assert_eq!(
        manifest.get(&note).unwrap().sha256,
        investigation_store::journal::hash_bytes(&bytes)
    );
    // The last complete generation is still the earlier one.
    assert_eq!(manifest.last_complete().unwrap().heads[0].1.sequence, 3);

    // The next render completes normally and replaces the stale overview.
    let report = render(&mut store);
    assert_eq!(report.publish.refused(), 0);
    assert_eq!(
        action(&report, &format!("Cases/{case_id}/Overview.md")),
        Action::Written
    );
}

/// No publisher code path performs a recursive delete. The only directory
/// removal is a single `rmdir` of an emptied folder, which the kernel refuses
/// while anything remains in it.
#[test]
fn no_publisher_code_path_deletes_recursively() {
    let sources = [
        include_str!("../src/publish.rs"),
        include_str!("../src/manifest.rs"),
        include_str!("../src/lock.rs"),
        include_str!("../src/lib.rs"),
        include_str!("../src/sweep.rs"),
        include_str!("../src/status.rs"),
        include_str!("../src/drift.rs"),
    ];
    let mut directory_removals = 0;
    for source in sources {
        for forbidden in ["remove_dir_all", "remove_dir(", "fs::remove"] {
            assert!(!source.contains(forbidden), "{forbidden}");
        }
        directory_removals += source.matches("AtFlags::REMOVEDIR").count();
    }
    assert_eq!(directory_removals, 1);
}
