//! `vault reconcile`: each of the four dispositions leaves its path without
//! drift, loses no observed bytes without a confirmed discard, never changes
//! canonical state on its own, and refuses a path that changed since the
//! render diagnosed it. `vault clean-conflicts` removes only the set-aside
//! bytes it can identify, and `vault unrelease` returns a released path once
//! the person's file is gone from it.

use std::{
    fs,
    path::{Path, PathBuf},
};

use investigation_core::{identity::Id, wire::Snapshot};
use investigation_render::{
    CaseRenderer, DriftKind, PlanPath, PublishMode, RenderError, RenderReport, VaultRenderer,
    VerifiedSnapshot, ViewPolicy, foreign_files,
    reconcile::{
        DISCARDED_DIR, Disposition, Handling, NOTES_FOLDER, PROPOSED_DIR, ReconcileReport,
        ReconcileRequest, Role, UnreleaseReport, preview, preview_unrelease, reconcile_vault,
        unrelease_vault,
    },
    render_vault_with,
    set_aside::{self, Action, Area, Kept},
    vault_status,
};
use investigation_store::{Store, Workspace, journal::hash_bytes};
use serde_json::{Value, json};

fn submit(store: &mut Store, key: &str, case_id: Option<Id>, operation: Value) -> Id {
    submit_with(store, key, case_id, json!([]), operation);
    store_created(store, key)
}

fn submit_with(
    store: &mut Store,
    key: &str,
    case_id: Option<Id>,
    preconditions: Value,
    operation: Value,
) -> Vec<Id> {
    let input = json!({
        "schema_version":"inv.command.v0-alpha",
        "command_id":key,
        "case_id":case_id,
        "actor":"agent:reconcile-test",
        "preconditions":preconditions,
        "operations":[operation]
    });
    let receipt = store.record(&serde_json::to_vec(&input).unwrap()).unwrap();
    receipt.created.iter().map(|change| change.id).collect()
}

fn store_created(store: &mut Store, key: &str) -> Id {
    let actor = "agent:reconcile-test".parse().unwrap();
    match store.receipt(&actor, key).unwrap() {
        investigation_store::store::ReceiptLookup::Found { receipt } => receipt.created[0].id,
        other => panic!("{other:?}"),
    }
}

fn question(store: &mut Store, key: &str, case_id: Id) -> Id {
    submit(
        store,
        key,
        Some(case_id),
        json!({"op":"question.create", "payload":{"wording":key, "importance":"normal"}}),
    )
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    case_id: Id,
    questions: Vec<Id>,
}

impl Fixture {
    /// A vault-enabled workspace with one case and three questions, rendered.
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::init(&directory.path().join("workspace"), true).unwrap();
        let mut store = Store::open(workspace);
        let case_id = submit(
            &mut store,
            "case",
            None,
            json!({"op":"case.create", "payload":{"title":"Reconcile fixture", "mode":"record"}}),
        );
        let questions = (1..=3)
            .map(|n| question(&mut store, &format!("q{n}"), case_id))
            .collect();
        let mut fixture = Self {
            _directory: directory,
            store,
            case_id,
            questions,
        };
        fixture.render();
        fixture
    }

    fn root(&self) -> PathBuf {
        self.store.workspace().root().to_owned()
    }

    fn vault(&self) -> PathBuf {
        self.root().join("vault")
    }

    fn note(&self, index: usize) -> String {
        format!(
            "Cases/{}/Questions/{}.md",
            self.case_id, self.questions[index]
        )
    }

    fn overview(&self) -> String {
        format!("Cases/{}/Overview.md", self.case_id)
    }

    fn render(&mut self) -> RenderReport {
        self.render_in(PublishMode::Incremental)
    }

    fn render_in(&mut self, mode: PublishMode) -> RenderReport {
        render_vault_with(
            &mut self.store,
            &VaultRenderer,
            &ViewPolicy::default(),
            mode,
            &mut |_| {},
        )
        .unwrap()
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.vault().join(path)).unwrap()
    }

    fn planned_text(&mut self, path: &str) -> String {
        String::from_utf8(self.planned(path)).unwrap()
    }

    fn planned(&mut self, path: &str) -> Vec<u8> {
        let snapshot = VerifiedSnapshot::read(&mut self.store).unwrap();
        VaultRenderer
            .render(&snapshot, &ViewPolicy::default())
            .unwrap()
            .entry(path)
            .unwrap_or_else(|| panic!("{path} is not planned"))
            .content()
            .to_vec()
    }

    fn heads(&mut self) -> Vec<(Id, Snapshot)> {
        VerifiedSnapshot::read(&mut self.store)
            .unwrap()
            .cases()
            .map(|case| (case.case_id(), case.head().clone()))
            .collect()
    }

    fn reconcile(
        &mut self,
        disposition: Disposition,
        paths: &[&str],
        discard_observed: bool,
    ) -> Result<ReconcileReport, RenderError> {
        let request = ReconcileRequest {
            disposition,
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            discard_observed,
        };
        reconcile_vault(
            &mut self.store,
            &VaultRenderer,
            &ViewPolicy::default(),
            &request,
            &mut |_| {},
        )
    }

    fn drifted(&self) -> Vec<String> {
        vault_status(self.store.workspace())
            .unwrap()
            .drift
            .iter()
            .map(|drift| drift.path.as_str().to_owned())
            .collect()
    }
}

/// Every file under `root`, relative to it, with its bytes.
fn files(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        let Ok(listing) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in listing {
            let path = entry.unwrap().path();
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                found.push((
                    relative.to_string_lossy().into_owned(),
                    fs::read(&path).unwrap(),
                ));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn preserve_as_note_moves_the_edit_into_notes_and_regenerates_the_path() {
    let mut fixture = Fixture::new();
    let vault = fixture.vault();
    let path = fixture.note(0);
    let edited = b"a human rewrote this generated note\n".to_vec();
    fs::write(vault.join(&path), &edited).unwrap();
    // A person's own note with the same name is never replaced.
    fs::create_dir_all(vault.join(NOTES_FOLDER)).unwrap();
    let name = format!("{}.md", fixture.questions[0]);
    fs::write(vault.join(NOTES_FOLDER).join(&name), b"mine\n").unwrap();
    let report = fixture.render();
    assert_eq!(report.drift.len(), 1);
    let heads = fixture.heads();

    let preview = preview(
        fixture.store.workspace(),
        &ReconcileRequest {
            disposition: Disposition::PreserveAsNote,
            paths: vec![path.clone()],
            discard_observed: false,
        },
    )
    .unwrap();
    let target = format!("{NOTES_FOLDER}/{} 2.md", fixture.questions[0]);
    assert_eq!(
        preview.steps[0].observed[0].handling,
        Handling::MovedToNotes { to: target.clone() }
    );
    assert_eq!(
        fs::read(vault.join(&path)).unwrap(),
        edited,
        "a preview moves nothing"
    );

    let done = fixture
        .reconcile(Disposition::PreserveAsNote, &[&path], false)
        .unwrap();
    let step = &done.done.steps[0];
    assert_eq!(step.kind, Some(DriftKind::Edited));
    assert_eq!(step.observed[0].role, Role::Edited);
    assert_eq!(step.observed[0].sha256, hash_bytes(&edited));
    assert_eq!(fs::read(vault.join(&target)).unwrap(), edited);
    assert_eq!(
        fs::read(vault.join(NOTES_FOLDER).join(&name)).unwrap(),
        b"mine\n"
    );
    assert_eq!(fixture.read(&path), fixture.planned_text(&path));
    assert!(done.remaining().is_empty());
    assert!(fixture.drifted().is_empty());
    assert_eq!(fixture.heads(), heads, "the journal is unchanged");
    // The path is owned again: a later commit updates it.
    question(&mut fixture.store, "q4", fixture.case_id);
    fixture.render_in(PublishMode::Full);
    assert_eq!(fixture.read(&path), fixture.planned_text(&path));
    assert!(fixture.drifted().is_empty());
}

#[test]
fn preserve_as_note_moves_conflict_copies_and_keeps_a_note_already_moved_into_notes() {
    let mut fixture = Fixture::new();
    let vault = fixture.vault();
    let overview = fixture.overview();
    let copy = format!("Cases/{}/Overview (conflicted copy).md", fixture.case_id);
    fs::write(vault.join(&copy), b"the other device's version\n").unwrap();
    // A person moved a generated note into Notes/.
    let moved = fixture.note(1);
    fs::create_dir_all(vault.join("Notes")).unwrap();
    let kept = vault.join("Notes/my question.md");
    fs::rename(vault.join(&moved), &kept).unwrap();
    let kept_bytes = fs::read(&kept).unwrap();
    let report = fixture.render();
    let kinds: Vec<DriftKind> = report.drift.iter().map(|drift| drift.kind).collect();
    assert_eq!(kinds.len(), 2);
    assert!(kinds.contains(&DriftKind::ConflictCopy));
    assert!(kinds.contains(&DriftKind::Renamed));

    let done = fixture
        .reconcile(Disposition::PreserveAsNote, &[&overview, &moved], false)
        .unwrap();
    let copy_step = &done.done.steps[0];
    assert_eq!(
        copy_step.observed.len(),
        1,
        "the overview itself is generated"
    );
    assert_eq!(copy_step.observed[0].role, Role::ConflictCopy);
    assert_eq!(
        fs::read(
            vault
                .join(NOTES_FOLDER)
                .join("Overview (conflicted copy).md")
        )
        .unwrap(),
        b"the other device's version\n"
    );
    assert!(!vault.join(&copy).exists());
    let moved_step = &done.done.steps[1];
    assert_eq!(moved_step.observed[0].role, Role::Moved);
    assert_eq!(moved_step.observed[0].handling, Handling::Left);
    assert_eq!(
        fs::read(&kept).unwrap(),
        kept_bytes,
        "a human note is left alone"
    );
    assert_eq!(fixture.read(&moved), fixture.planned_text(&moved));
    assert_eq!(fixture.read(&overview), fixture.planned_text(&overview));
    assert!(done.remaining().is_empty());
    assert!(fixture.drifted().is_empty());
}

#[test]
fn discard_needs_its_flag_and_keeps_the_discarded_bytes_in_the_conflicts_area() {
    let mut fixture = Fixture::new();
    let vault = fixture.vault();
    let edited_path = fixture.note(0);
    let deleted_path = fixture.note(1);
    let edited = b"edited\n".to_vec();
    fs::write(vault.join(&edited_path), &edited).unwrap();
    fs::remove_file(vault.join(&deleted_path)).unwrap();
    fixture.render();
    let before = files(&fixture.root());
    let heads = fixture.heads();

    let refused = fixture
        .reconcile(Disposition::Discard, &[&edited_path, &deleted_path], false)
        .unwrap_err();
    assert!(
        refused.to_string().contains("--discard-observed"),
        "{refused}"
    );
    assert_eq!(files(&fixture.root()), before, "a refusal changes nothing");

    let done = fixture
        .reconcile(Disposition::Discard, &[&edited_path, &deleted_path], true)
        .unwrap();
    let Handling::SetAside { to } = &done.done.steps[0].observed[0].handling else {
        panic!("{:?}", done.done.steps[0]);
    };
    assert!(to.starts_with(&format!("{DISCARDED_DIR}/")), "{to}");
    assert!(to.ends_with(&edited_path), "{to}");
    assert_eq!(fs::read(fixture.root().join(to)).unwrap(), edited);
    assert_eq!(done.done.steps[1].kind, Some(DriftKind::Missing));
    assert!(done.done.steps[1].observed.is_empty());
    for path in [&edited_path, &deleted_path] {
        assert_eq!(fixture.read(path), fixture.planned_text(path));
    }
    assert!(done.remaining().is_empty());
    assert!(fixture.drifted().is_empty());
    assert_eq!(fixture.heads(), heads, "the journal is unchanged");
}

#[test]
fn release_path_hands_the_file_over_and_projects_the_record_beside_it() {
    let mut fixture = Fixture::new();
    let vault = fixture.vault();
    let path = fixture.note(0);
    let edited = b"now this is my own note about the question\n".to_vec();
    fs::write(vault.join(&path), &edited).unwrap();
    fixture.render();
    let heads = fixture.heads();

    let done = fixture
        .reconcile(Disposition::ReleasePath, &[&path], false)
        .unwrap();
    let relocated = format!(
        "Cases/{}/Questions/{}-2.md",
        fixture.case_id, fixture.questions[0]
    );
    let step = &done.done.steps[0];
    assert_eq!(
        step.relocated_to.as_ref().map(PlanPath::as_str),
        Some(relocated.as_str())
    );
    assert_eq!(step.observed[0].handling, Handling::Left);
    assert_eq!(fs::read(vault.join(&path)).unwrap(), edited);
    assert!(done.remaining().is_empty());
    assert!(fixture.drifted().is_empty());
    assert_eq!(fixture.heads(), heads, "the journal is unchanged");

    // The record's note is at the relocated path, and generated links follow.
    let note = fs::read_to_string(vault.join(&relocated)).unwrap();
    assert!(
        note.contains(&format!("inv_id: \"{}\"", fixture.questions[0])),
        "{note}"
    );
    let old_target = format!("[[{}", path.strip_suffix(".md").unwrap());
    let new_target = format!("[[{}", relocated.strip_suffix(".md").unwrap());
    let overview = fs::read_to_string(vault.join(fixture.overview())).unwrap();
    for (relative, bytes) in files(&vault) {
        if relative == path {
            continue;
        }
        let text = String::from_utf8(bytes).unwrap();
        for at in text.match_indices(&old_target).map(|(at, _)| at) {
            let next = text[at + old_target.len()..].chars().next();
            assert!(
                next.is_some_and(|c| !matches!(c, '|' | '\\' | '#' | ']')),
                "{relative} still links to the released path"
            );
        }
    }
    let links_before = overview.matches(&new_target).count();
    assert!(links_before > 0, "{overview}");
    let status = vault_status(fixture.store.workspace()).unwrap();
    assert_eq!(status.released.len(), 1);
    assert_eq!(status.released[0].path.as_str(), path);
    assert_eq!(status.released[0].relocated_to.as_str(), relocated);
    assert_eq!(status.released[0].projects, Some(fixture.questions[0]));
    // The released file is the person's: not unknown, never touched again,
    // even when its record changes.
    assert_eq!(
        foreign_files(fixture.store.workspace()).unwrap(),
        Some(Vec::new())
    );
    submit_with(
        &mut fixture.store,
        "case-amend",
        Some(fixture.case_id),
        json!([{"id":fixture.case_id,"revision":1}]),
        json!({"op":"case.amend","payload":{"reason":"retitle","title":"Renamed fixture"}}),
    );
    let report = fixture.render();
    assert!(report.drift.is_empty());
    assert!(report.unknown.is_empty());
    assert_eq!(fs::read(vault.join(&path)).unwrap(), edited);
    assert_eq!(
        fs::read(vault.join(&relocated)).unwrap(),
        fixture_relocated_plan(&mut fixture, &relocated)
    );
    // Releasing it again is refused, and the relocated note can be released
    // in turn; links then follow to the newest path.
    let again = fixture
        .reconcile(Disposition::ReleasePath, &[&path], false)
        .unwrap_err();
    assert!(again.to_string().contains("already released"), "{again}");
    let done = fixture
        .reconcile(Disposition::ReleasePath, &[&relocated], false)
        .unwrap();
    let newest = format!(
        "Cases/{}/Questions/{}-3.md",
        fixture.case_id, fixture.questions[0]
    );
    assert_eq!(
        done.done.steps[0]
            .relocated_to
            .as_ref()
            .map(PlanPath::as_str),
        Some(newest.as_str())
    );
    let overview = fs::read_to_string(vault.join(fixture.overview())).unwrap();
    let newest_target = format!("[[{}", newest.strip_suffix(".md").unwrap());
    assert!(overview.contains(&newest_target), "{overview}");
    for stale in ["|", "\\|", "]"] {
        assert!(
            !overview.contains(&format!("{new_target}{stale}")),
            "{overview}"
        );
    }
    assert!(fixture.drifted().is_empty());
}

/// The relocated note's bytes as the next render plans them: the planned
/// note of the record, with links rewritten. Read back from a fresh render.
fn fixture_relocated_plan(fixture: &mut Fixture, relocated: &str) -> Vec<u8> {
    fixture.render();
    fs::read(fixture.vault().join(relocated)).unwrap()
}

#[test]
fn a_lost_manifest_never_overwrites_a_released_file() {
    let mut fixture = Fixture::new();
    let vault = fixture.vault();
    let path = fixture.note(0);
    let edited = b"mine\n".to_vec();
    fs::write(vault.join(&path), &edited).unwrap();
    fixture.render();
    fixture
        .reconcile(Disposition::ReleasePath, &[&path], false)
        .unwrap();
    fs::remove_file(fixture.root().join("local/render-manifest.json")).unwrap();
    let report = fixture.render();
    assert_eq!(fs::read(vault.join(&path)).unwrap(), edited);
    assert!(
        report
            .publish
            .outcomes
            .iter()
            .any(|outcome| outcome.path.as_str() == path
                && outcome.action
                    == investigation_render::Action::Refused(
                        investigation_render::Refusal::NotProvenOwned
                    ))
    );
}

#[test]
fn a_path_edited_again_after_the_diagnosis_is_refused_and_nothing_moves() {
    let mut fixture = Fixture::new();
    let vault = fixture.vault();
    let path = fixture.note(0);
    let other = fixture.note(1);
    fs::write(vault.join(&path), b"first edit\n").unwrap();
    fs::write(vault.join(&other), b"another edit\n").unwrap();
    fixture.render();
    fs::write(vault.join(&path), b"second edit, not yet diagnosed\n").unwrap();
    let before = files(&fixture.root());
    for disposition in Disposition::ALL {
        let error = fixture
            .reconcile(disposition, &[&other, &path], true)
            .unwrap_err();
        assert!(
            matches!(error, RenderError::Reconcile { changed: true, .. }),
            "{disposition:?}: {error}"
        );
        assert_eq!(
            error.to_failure().error.code,
            investigation_core::wire::ErrorCode::RevisionConflict
        );
        assert_eq!(
            files(&fixture.root()),
            before,
            "{disposition:?}: the whole request is checked before anything moves"
        );
    }
}

#[test]
fn clean_unknown_or_unlisted_paths_are_refused() {
    let mut fixture = Fixture::new();
    let clean = fixture.note(0);
    for (disposition, paths) in [
        (Disposition::PreserveAsNote, vec![clean.as_str()]),
        (Disposition::Discard, vec![clean.as_str()]),
        (Disposition::PreserveAsNote, vec!["Notes/mine.md"]),
        (Disposition::PreserveAsNote, vec!["../escape.md"]),
        (Disposition::PreserveAsNote, vec![]),
        (Disposition::ReleasePath, vec!["Start Here.md"]),
    ] {
        let error = fixture.reconcile(disposition, &paths, true).unwrap_err();
        assert!(
            matches!(error, RenderError::Reconcile { changed: false, .. }),
            "{disposition:?} {paths:?}: {error}"
        );
    }
}

#[test]
fn a_checkbox_or_property_edit_never_changes_the_journal_under_any_file_disposition() {
    for disposition in [
        Disposition::PreserveAsNote,
        Disposition::Discard,
        Disposition::ReleasePath,
        Disposition::ProposeChange,
    ] {
        let mut fixture = Fixture::new();
        let vault = fixture.vault();
        let path = fixture.note(0);
        let original = fs::read_to_string(vault.join(&path)).unwrap();
        let edited = original.replace("inv_status: \"open\"", "inv_status: \"answered\"")
            + "\n- [x] answered\n";
        assert_ne!(edited, original);
        fs::write(vault.join(&path), &edited).unwrap();
        fixture.render();
        let heads = fixture.heads();
        let done = fixture.reconcile(disposition, &[&path], true).unwrap();
        assert!(done.remaining().is_empty(), "{disposition:?}");
        assert_eq!(fixture.heads(), heads, "{disposition:?}");
        // The edit's bytes survive somewhere the report names.
        let kept = match &done.done.steps[0].observed[0].handling {
            Handling::MovedToNotes { to } => fs::read(vault.join(to)).unwrap(),
            Handling::SetAside { to } => {
                if disposition == Disposition::ProposeChange {
                    assert!(to.starts_with(PROPOSED_DIR), "{to}");
                }
                fs::read(fixture.root().join(to)).unwrap()
            }
            Handling::Left => fs::read(vault.join(&path)).unwrap(),
        };
        assert_eq!(kept, edited.as_bytes(), "{disposition:?}");
    }
}

#[test]
fn a_manifest_that_both_owns_and_releases_a_path_is_rebuilt_conservatively() {
    let mut fixture = Fixture::new();
    let path = fixture.note(0);
    fs::write(fixture.vault().join(&path), b"mine\n").unwrap();
    fixture.render();
    fixture
        .reconcile(Disposition::ReleasePath, &[&path], false)
        .unwrap();
    let file = fixture.root().join("local/render-manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(manifest["released"][0]["path"], path.as_str());
    let owned = manifest["owned"][0].clone();
    manifest["released"][0]["path"] = owned["path"].clone();
    fs::write(&file, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let status = vault_status(fixture.store.workspace()).unwrap();
    assert!(
        matches!(
            status.manifest,
            investigation_render::ManifestStatus::Rebuilt { .. }
        ),
        "{:?}",
        status.manifest
    );
    assert!(status.released.is_empty());
}

/// A file's path under the workspace, by its bytes.
fn find(root: &Path, under: &str, bytes: &[u8]) -> String {
    files(&root.join(under))
        .into_iter()
        .find(|(_, found)| found == bytes)
        .map(|(path, _)| format!("{under}/{path}"))
        .unwrap_or_else(|| panic!("no file under {under} holds {bytes:?}"))
}

#[test]
fn clean_conflicts_removes_only_the_set_aside_bytes_it_can_identify() {
    let mut fixture = Fixture::new();
    let vault = fixture.vault();
    let root = fixture.root();
    let (discarded, proposed, changed) = (fixture.note(0), fixture.note(1), fixture.note(2));
    for (path, text) in [
        (&discarded, "discard me\n"),
        (&proposed, "proposed edit\n"),
        (&changed, "discard me too\n"),
    ] {
        fs::write(vault.join(path), text).unwrap();
    }
    fixture.render();
    fixture
        .reconcile(Disposition::Discard, &[&discarded, &changed], true)
        .unwrap();
    fixture
        .reconcile(Disposition::ProposeChange, &[&proposed], false)
        .unwrap();
    let kept_discard = find(&root, DISCARDED_DIR, b"discard me\n");
    let kept_proposal = find(&root, PROPOSED_DIR, b"proposed edit\n");
    // A kept file someone changed since, and entries reconcile never writes.
    let edited_since = find(&root, DISCARDED_DIR, b"discard me too\n");
    fs::write(root.join(&edited_since), b"changed in local/\n").unwrap();
    let area = root.join(DISCARDED_DIR);
    fs::write(area.join("README.txt"), b"mine\n").unwrap();
    fs::create_dir_all(area.join("notes")).unwrap();
    fs::write(area.join("notes/x.md"), b"mine too\n").unwrap();
    let prefix = kept_discard
        .strip_prefix(&format!("{DISCARDED_DIR}/"))
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .to_owned();
    std::os::unix::fs::symlink(
        root.join("investigation.toml"),
        area.join(&prefix).join("link.md"),
    )
    .unwrap();
    // A withheld generated version, which drift records name.
    let withheld = root.join("local/conflicts/gen_withheld/Cases/x.md");
    fs::create_dir_all(withheld.parent().unwrap()).unwrap();
    fs::write(&withheld, b"withheld\n").unwrap();
    let heads = fixture.heads();
    let manifest = fs::read(root.join("local/render-manifest.json")).unwrap();
    let vault_before = files(&vault);
    let before = files(&root);

    let previewed = set_aside::preview(fixture.store.workspace(), &Area::ALL).unwrap();
    assert_eq!(files(&root), before, "a preview writes nothing");
    let action = |report: &set_aside::CleanReport, path: &str| {
        report
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .unwrap_or_else(|| panic!("{path} is not listed: {report:?}"))
            .action
    };
    assert_eq!(action(&previewed, &kept_discard), Action::WouldRemove);
    assert_eq!(action(&previewed, &kept_proposal), Action::WouldRemove);
    assert_eq!(
        action(&previewed, &edited_since),
        Action::Kept(Kept::BytesDiffer)
    );
    assert_eq!(
        action(&previewed, &format!("{DISCARDED_DIR}/README.txt")),
        Action::Kept(Kept::UnexpectedEntry)
    );
    assert_eq!(
        action(&previewed, &format!("{DISCARDED_DIR}/notes")),
        Action::Kept(Kept::UnexpectedEntry)
    );
    assert_eq!(
        action(&previewed, &format!("{DISCARDED_DIR}/{prefix}/link.md")),
        Action::Kept(Kept::NotARegularFile)
    );
    assert!(
        previewed
            .entries
            .iter()
            .all(|entry| !entry.path.contains("gen_withheld")),
        "withheld versions are not looked at"
    );
    let entry = previewed
        .entries
        .iter()
        .find(|entry| entry.path == kept_discard)
        .unwrap();
    assert_eq!(entry.vault_path.as_deref(), Some(discarded.as_str()));
    assert_eq!(entry.area, Area::Discarded);

    // One area at a time.
    let done = set_aside::clean(fixture.store.workspace(), &[Area::Proposed]).unwrap();
    assert_eq!(action(&done, &kept_proposal), Action::Removed);
    assert!(!root.join(&kept_proposal).exists());
    assert!(!root.join(PROPOSED_DIR).exists(), "empty folders go");
    assert!(root.join(&kept_discard).exists());

    let done = set_aside::clean(fixture.store.workspace(), &Area::ALL).unwrap();
    assert_eq!(action(&done, &kept_discard), Action::Removed);
    let mut expected = before.clone();
    expected.retain(|(path, _)| path != &kept_discard && path != &kept_proposal);
    let mut after = files(&root);
    after.retain(|(path, _)| !path.ends_with("render.lock"));
    expected.retain(|(path, _)| !path.ends_with("render.lock"));
    assert_eq!(after, expected, "only the identified files are gone");
    assert!(
        !root
            .join(DISCARDED_DIR)
            .join(
                kept_discard
                    .strip_prefix(&format!("{DISCARDED_DIR}/"))
                    .unwrap()
            )
            .parent()
            .unwrap()
            .exists(),
        "the folders it emptied are removed"
    );
    assert!(fs::symlink_metadata(area.join(&prefix).join("link.md")).is_ok());
    assert_eq!(fs::read(&withheld).unwrap(), b"withheld\n");
    assert_eq!(files(&vault), vault_before, "the vault is untouched");
    assert_eq!(
        fs::read(root.join("local/render-manifest.json")).unwrap(),
        manifest
    );
    assert_eq!(fixture.heads(), heads, "the journal is unchanged");

    // Nothing left to identify: cleaning again removes nothing.
    let again = set_aside::clean(fixture.store.workspace(), &Area::ALL).unwrap();
    assert_eq!(again.count(|action| *action == Action::Removed), 0);
}

impl Fixture {
    fn unrelease(&mut self, paths: &[&str]) -> Result<UnreleaseReport, RenderError> {
        let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
        unrelease_vault(
            &mut self.store,
            &VaultRenderer,
            &ViewPolicy::default(),
            &paths,
            &mut |_| {},
        )
    }

    fn relocated(&self, index: usize, n: usize) -> String {
        format!(
            "Cases/{}/Questions/{}-{n}.md",
            self.case_id, self.questions[index]
        )
    }

    /// Edit `path`, render, and release it.
    fn release(&mut self, path: &str, text: &str) {
        fs::write(self.vault().join(path), text).unwrap();
        self.render();
        self.reconcile(Disposition::ReleasePath, &[path], false)
            .unwrap();
    }
}

/// Whether `text` links to the note at `path`.
fn links_to(text: &str, path: &str) -> bool {
    let target = format!("[[{}", path.strip_suffix(".md").unwrap());
    text.match_indices(&target).any(|(at, _)| {
        text[at + target.len()..]
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '|' | '\\' | '#' | ']'))
    })
}

#[test]
fn unrelease_returns_a_released_path_once_the_persons_file_is_gone() {
    let mut fixture = Fixture::new();
    let vault = fixture.vault();
    let path = fixture.note(0);
    let relocated = fixture.relocated(0, 2);
    fixture.release(&path, "my own note now\n");
    let heads = fixture.heads();
    let before = files(&fixture.root());

    // The person's file is still there: refused, and nothing changes.
    for refused in [
        preview_unrelease(fixture.store.workspace(), std::slice::from_ref(&path)).unwrap_err(),
        fixture.unrelease(&[&path]).unwrap_err(),
    ] {
        assert!(
            refused.to_string().contains("still holds your file"),
            "{refused}"
        );
        assert_eq!(
            refused.to_failure().error.code,
            investigation_core::wire::ErrorCode::InvalidInput
        );
    }
    assert_eq!(files(&fixture.root()), before, "a refusal changes nothing");

    // Owned or never released: refused.
    let owned = fixture.note(1);
    for (paths, expected) in [
        (vec![owned.as_str()], "already owns"),
        (vec![relocated.as_str()], "already owns"),
        (vec!["Notes/mine.md"], "was not released"),
    ] {
        let refused = fixture.unrelease(&paths).unwrap_err();
        assert!(refused.to_string().contains(expected), "{refused}");
    }
    assert_eq!(files(&fixture.root()), before);

    // The person moves their file into Notes/; the path is empty.
    fs::create_dir_all(vault.join("Notes")).unwrap();
    fs::rename(vault.join(&path), vault.join("Notes/mine.md")).unwrap();
    let before = files(&fixture.root());
    let twice = fixture.unrelease(&[&path, &path]).unwrap_err();
    assert!(twice.to_string().contains("listed twice"), "{twice}");
    let previewed =
        preview_unrelease(fixture.store.workspace(), std::slice::from_ref(&path)).unwrap();
    assert_eq!(previewed[0].path.as_str(), path);
    assert_eq!(previewed[0].relocated_from.as_str(), relocated);
    assert_eq!(previewed[0].projects, Some(fixture.questions[0]));
    assert_eq!(files(&fixture.root()), before, "a preview writes nothing");

    let done = fixture.unrelease(&[&path]).unwrap();
    assert_eq!(done.returned, previewed);
    assert!(done.remaining().is_empty(), "{:?}", done.remaining());
    assert_eq!(fixture.read(&path), fixture.planned_text(&path));
    assert!(
        !vault.join(&relocated).exists(),
        "the relocated note is retired"
    );
    assert_eq!(
        fs::read(vault.join("Notes/mine.md")).unwrap(),
        b"my own note now\n"
    );
    let overview = fs::read_to_string(vault.join(fixture.overview())).unwrap();
    assert!(links_to(&overview, &path), "{overview}");
    assert!(!links_to(&overview, &relocated), "{overview}");
    let status = vault_status(fixture.store.workspace()).unwrap();
    assert!(status.released.is_empty());
    assert!(fixture.drifted().is_empty());
    assert_eq!(fixture.heads(), heads, "the journal is unchanged");

    // The path is owned again: a later render keeps it current, and a
    // release undoes the return.
    fixture.render_in(PublishMode::Full);
    assert_eq!(fixture.read(&path), fixture.planned_text(&path));
    fixture.release(&path, "mine again\n");
    assert_eq!(fs::read(vault.join(&path)).unwrap(), b"mine again\n");
    assert!(vault.join(&relocated).exists());
    let refused = fixture
        .reconcile(Disposition::ReleasePath, &[&path], false)
        .unwrap_err();
    assert!(refused.to_string().contains("vault unrelease"), "{refused}");
}

#[test]
fn unrelease_undoes_a_chain_of_releases_one_step_at_a_time() {
    let mut fixture = Fixture::new();
    let vault = fixture.vault();
    let (first, second, third) = (
        fixture.note(0),
        fixture.relocated(0, 2),
        fixture.relocated(0, 3),
    );
    fixture.release(&first, "first\n");
    fixture.release(&second, "second\n");
    assert!(vault.join(&third).exists());

    // Return the second path while the first stays released: the record
    // moves back to it, and the third is retired.
    fs::remove_file(vault.join(&second)).unwrap();
    let done = fixture.unrelease(&[&second]).unwrap();
    assert_eq!(done.returned[0].relocated_from.as_str(), third);
    assert!(done.remaining().is_empty());
    assert_eq!(fs::read(vault.join(&first)).unwrap(), b"first\n");
    assert!(!vault.join(&third).exists());
    let note = fs::read_to_string(vault.join(&second)).unwrap();
    assert!(
        note.contains(&format!("inv_id: \"{}\"", fixture.questions[0])),
        "{note}"
    );
    let overview = fs::read_to_string(vault.join(fixture.overview())).unwrap();
    assert!(links_to(&overview, &second), "{overview}");
    let status = vault_status(fixture.store.workspace()).unwrap();
    assert_eq!(status.released.len(), 1);
    assert_eq!(status.released[0].relocated_to.as_str(), second);

    // Then the first: the record is home, and nothing is released.
    fs::remove_file(vault.join(&first)).unwrap();
    let done = fixture.unrelease(&[&first]).unwrap();
    assert!(done.remaining().is_empty());
    assert_eq!(fixture.read(&first), fixture.planned_text(&first));
    assert!(!vault.join(&second).exists());
    assert!(
        vault_status(fixture.store.workspace())
            .unwrap()
            .released
            .is_empty()
    );
    assert!(fixture.drifted().is_empty());
}

#[test]
fn an_edited_relocated_note_is_kept_as_drift_when_its_path_is_returned() {
    let mut fixture = Fixture::new();
    let vault = fixture.vault();
    let (path, relocated) = (fixture.note(0), fixture.relocated(0, 2));
    fixture.release(&path, "mine\n");
    fs::write(vault.join(&relocated), b"edited the relocated note\n").unwrap();
    fixture.render();
    fs::remove_file(vault.join(&path)).unwrap();

    let done = fixture.unrelease(&[&path]).unwrap();
    assert_eq!(fixture.read(&path), fixture.planned_text(&path));
    assert_eq!(
        fs::read(vault.join(&relocated)).unwrap(),
        b"edited the relocated note\n",
        "an edit is never lost"
    );
    let remaining: Vec<&str> = done
        .remaining()
        .iter()
        .map(|drift| drift.path.as_str())
        .collect();
    assert_eq!(remaining, [relocated.as_str()]);
    // It reconciles like any drift.
    fixture
        .reconcile(Disposition::PreserveAsNote, &[&relocated], false)
        .unwrap();
    assert!(fixture.drifted().is_empty());
    assert!(!vault.join(&relocated).exists());
}
