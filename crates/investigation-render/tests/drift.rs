//! Drift: generated files that were edited, had a property changed, were
//! deleted, moved or copied by a sync client are left exactly as found,
//! diagnosed, and surfaced; the would-be content is retained in the
//! conflicts area; the rest of the case keeps rendering.

use std::{
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    time::Duration,
};

use investigation_core::identity::Id;
use investigation_render::{
    Action, CONFLICTS_DIR, CaseRenderer, Drift, DriftKind, Generation, Manifest, ManifestStatus,
    Phase, PlanBuilder, PlanPath, RENDER_MANIFEST, Refusal, RenderError, RenderLock, RenderPlan,
    RenderReport, RenderState, VaultRenderer, VerifiedSnapshot, ViewPolicy, render_vault,
    vault_status,
};
use investigation_store::{Store, Workspace, journal::hash_bytes};
use serde_json::{Value, json};

fn submit(store: &mut Store, key: &str, case_id: Option<Id>, operation: Value) -> Id {
    let input = json!({
        "schema_version":"inv.command.v0-alpha",
        "command_id":key,
        "case_id":case_id,
        "actor":"agent:drift-test",
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

/// A vault-enabled workspace with one case and three questions, rendered.
fn workspace(directory: &Path) -> (Store, Id, Vec<Id>) {
    let workspace = Workspace::init(&directory.join("workspace"), true).unwrap();
    let mut store = Store::open(workspace);
    let case_id = submit(
        &mut store,
        "case",
        None,
        json!({"op":"case.create", "payload":{"title":"Drift fixture", "mode":"record"}}),
    );
    let questions = (1..=3)
        .map(|n| question(&mut store, &format!("q{n}"), case_id))
        .collect();
    render(&mut store);
    (store, case_id, questions)
}

fn root(store: &Store) -> PathBuf {
    store.workspace().root().to_owned()
}

fn vault(store: &Store) -> PathBuf {
    root(store).join("vault")
}

fn note(case_id: Id, id: Id) -> String {
    format!("Cases/{case_id}/Questions/{id}.md")
}

fn render(store: &mut Store) -> RenderReport {
    render_vault(store, &VaultRenderer, &ViewPolicy::default(), &mut |_| {}).unwrap()
}

fn plan(store: &mut Store) -> RenderPlan {
    let snapshot = VerifiedSnapshot::read(store).unwrap();
    VaultRenderer
        .render(&snapshot, &ViewPolicy::default())
        .unwrap()
}

/// The bytes today's plan would put at `path`.
fn planned(store: &mut Store, path: &str) -> Vec<u8> {
    plan(store)
        .entries()
        .iter()
        .find(|entry| entry.path().as_str() == path)
        .unwrap()
        .content()
        .to_vec()
}

fn manifest(store: &Store) -> (Manifest, ManifestStatus) {
    let lock = RenderLock::acquire(store.workspace(), Duration::ZERO).unwrap();
    let loaded = Manifest::load(store.workspace(), &lock).unwrap();
    (loaded.manifest, loaded.status)
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

fn drift<'a>(report: &'a RenderReport, path: &str) -> &'a Drift {
    report
        .drift
        .iter()
        .find(|drift| drift.path.as_str() == path)
        .unwrap_or_else(|| panic!("no drift for {path}: {:?}", report.drift))
}

/// The retained generated version named by a drift record.
fn retained(store: &Store, drift: &Drift) -> Vec<u8> {
    let conflict = drift.conflict.as_deref().expect("a retained version");
    assert!(
        conflict.starts_with(&format!("{CONFLICTS_DIR}/gen_")),
        "{conflict}"
    );
    assert!(conflict.ends_with(drift.path.as_str()), "{conflict}");
    fs::read(root(store).join(conflict)).unwrap()
}

/// Every file under `root` relative to it.
fn files(root: &Path) -> Vec<String> {
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
                found.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    found
}

#[test]
fn edited_notes_and_property_edits_are_kept_and_the_generated_version_is_retained() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id, questions) = workspace(directory.path());
    let vault = vault(&store);
    let body = note(case_id, questions[0]);
    let property = note(case_id, questions[1]);
    let untouched = note(case_id, questions[2]);

    // A body edit, and a property edit as a viewer's table or board makes
    // it: the same note with one frontmatter value changed.
    let edited_body = b"a human rewrote this generated note\n".to_vec();
    fs::write(vault.join(&body), &edited_body).unwrap();
    let original = fs::read_to_string(vault.join(&property)).unwrap();
    assert!(original.contains("inv_status: \"open\"\n"), "{original}");
    let edited_property = original.replace("inv_status: \"open\"", "inv_status: \"answered\"");
    fs::write(vault.join(&property), &edited_property).unwrap();

    // A commit changes every note of the case; the drifted ones are withheld.
    question(&mut store, "q4", case_id);
    let report = render(&mut store);
    let edits = [
        (&body, questions[0], edited_body.clone()),
        (
            &property,
            questions[1],
            edited_property.clone().into_bytes(),
        ),
    ];
    for (path, id, bytes) in &edits {
        let (path, bytes) = (path.as_str(), bytes);
        assert_eq!(action(&report, path), Action::Refused(Refusal::Drifted));
        assert_eq!(
            &fs::read(vault.join(path)).unwrap(),
            bytes,
            "left exactly as found"
        );
        let drift = drift(&report, path);
        assert_eq!(drift.kind, DriftKind::Edited);
        assert_eq!(drift.observed.as_ref(), Some(&hash_bytes(bytes)));
        assert_eq!(drift.projects, Some(*id));
        assert_eq!(drift.since, report.publish.generation.id());
        // The generated version is retrievable from the conflicts area.
        let generated = planned(&mut store, path);
        assert_eq!(retained(&store, drift), generated);
        assert_eq!(drift.generated.as_ref(), Some(&hash_bytes(&generated)));
    }
    // Drift on two notes does not stop the rest of the case rendering.
    assert_eq!(report.publish.refused(), 2);
    assert_eq!(action(&report, &untouched), Action::Written);
    assert_eq!(
        fs::read(vault.join(&untouched)).unwrap(),
        planned(&mut store, &untouched)
    );
    assert_eq!(report.drift.len(), 2);
    let (stored, _) = manifest(&store);
    assert_eq!(stored.state(), &RenderState::Complete);
    assert_eq!(stored.drift().cloned().collect::<Vec<_>>(), report.drift);
    // Status reports the drift from the manifest alone.
    let status = vault_status(store.workspace()).unwrap();
    assert_eq!(status.drift, report.drift);

    // Drift stays across renders. A render with the same would-be content
    // reuses the retained file; a new one is retained per generation.
    let again = render(&mut store);
    assert_eq!(again.drift.len(), 2);
    for path in [&body, &property] {
        let (first, second) = (drift(&report, path), drift(&again, path));
        assert_eq!(second.conflict, first.conflict);
        assert_eq!(second.since, first.since);
    }
    question(&mut store, "q5", case_id);
    let later = render(&mut store);
    let moved_on = drift(&later, &body);
    assert_ne!(moved_on.conflict, drift(&report, &body).conflict);
    assert_eq!(moved_on.since, drift(&report, &body).since);
    assert_eq!(retained(&store, moved_on), planned(&mut store, &body));
    assert_eq!(fs::read(vault.join(&body)).unwrap(), edited_body);
    // Earlier retained versions are kept; nothing in the conflicts area is
    // deleted by a render.
    assert_eq!(files(&root(&store).join(CONFLICTS_DIR)).len(), 4);

    // Restoring the generated bytes reconciles the path.
    let (stored, _) = manifest(&store);
    let expected = &stored
        .get(&PlanPath::new(property.clone()).unwrap())
        .unwrap()
        .sha256;
    assert_eq!(&hash_bytes(original.as_bytes()), expected);
    fs::write(vault.join(&property), &original).unwrap();
    let reconciled = render(&mut store);
    assert_eq!(action(&reconciled, &property), Action::Written);
    assert_eq!(reconciled.drift.len(), 1);
    assert_eq!(
        vault_status(store.workspace()).unwrap().drift.len(),
        1,
        "only the body edit is left"
    );
}

#[test]
fn a_deleted_note_is_not_recreated_and_its_generated_version_is_retained() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id, questions) = workspace(directory.path());
    let vault = vault(&store);
    let deleted = note(case_id, questions[0]);
    let overview = format!("Cases/{case_id}/Overview.md");
    let generated = fs::read(vault.join(&deleted)).unwrap();
    fs::remove_file(vault.join(&deleted)).unwrap();
    fs::remove_file(vault.join(&overview)).unwrap();

    for _ in 0..2 {
        let report = render(&mut store);
        for path in [&deleted, &overview] {
            assert_eq!(action(&report, path), Action::Refused(Refusal::Drifted));
            assert!(!vault.join(path).exists(), "not recreated");
            let drift = drift(&report, path);
            assert_eq!(drift.kind, DriftKind::Missing);
            assert_eq!(drift.observed, None);
            assert!(drift.renamed_to.is_empty());
            assert_eq!(retained(&store, drift), planned(&mut store, path));
        }
        assert_eq!(
            drift(&report, &overview).projects,
            Some(case_id),
            "the record the path projects"
        );
        assert_eq!(report.publish.refused(), 2);
    }
    // Deleting a generated note never touches canonical state.
    let snapshot = VerifiedSnapshot::read(&mut store).unwrap();
    assert!(
        snapshot
            .case(case_id)
            .unwrap()
            .state()
            .records()
            .any(|record| record.header.id == questions[0])
    );

    // Putting the file back reconciles it.
    fs::write(vault.join(&deleted), generated).unwrap();
    let report = render(&mut store);
    assert_eq!(action(&report, &deleted), Action::Unchanged);
    assert_eq!(report.drift.len(), 1);
    assert_eq!(report.drift[0].path.as_str(), overview);
}

#[test]
fn a_renamed_note_is_found_by_its_bytes_or_its_record_id() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id, questions) = workspace(directory.path());
    let vault = vault(&store);
    // Moved unchanged into a human-owned folder.
    let kept = note(case_id, questions[0]);
    fs::rename(vault.join(&kept), vault.join("Notes/kept.md")).unwrap();
    // Renamed in place and then edited: found by its `inv_id` property.
    let retitled = note(case_id, questions[1]);
    let text = fs::read_to_string(vault.join(&retitled)).unwrap();
    fs::remove_file(vault.join(&retitled)).unwrap();
    let edited = text.replace("# q2", "# My own title for q2");
    assert_ne!(edited, text);
    let elsewhere = format!("Cases/{case_id}/Questions/My question.md");
    fs::write(vault.join(&elsewhere), &edited).unwrap();

    question(&mut store, "q4", case_id);
    let report = render(&mut store);
    let first = drift(&report, &kept);
    assert_eq!(first.kind, DriftKind::Renamed);
    assert_eq!(first.renamed_to, ["Notes/kept.md"]);
    let second = drift(&report, &retitled);
    assert_eq!(second.kind, DriftKind::Renamed);
    assert_eq!(second.renamed_to, std::slice::from_ref(&elsewhere));
    assert_eq!(retained(&store, second), planned(&mut store, &retitled));
    // Neither original path is recreated, and the moved files are untouched.
    assert!(!vault.join(&kept).exists());
    assert!(!vault.join(&retitled).exists());
    assert_eq!(fs::read_to_string(vault.join(&elsewhere)).unwrap(), edited);
    assert!(vault.join("Notes/kept.md").is_file());
    // The moved file outside the human roots is also an unknown file.
    assert_eq!(report.unknown, [elsewhere]);
    assert_eq!(report.publish.refused(), 2);
}

#[test]
fn a_sync_conflict_copy_holds_its_note_until_it_is_removed() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id, questions) = workspace(directory.path());
    let vault = vault(&store);
    let held = note(case_id, questions[0]);
    let copy = format!(
        "Cases/{case_id}/Questions/{} (conflicted copy 2026-11-22 101500).md",
        questions[0]
    );
    let near_miss = format!("Cases/{case_id}/Questions/{} (1).md", questions[0]);
    let before = fs::read(vault.join(&held)).unwrap();
    fs::write(vault.join(&copy), "the other device's version\n").unwrap();
    fs::write(vault.join(&near_miss), "not named as a conflict\n").unwrap();

    // Nothing changed: the copy is reported even though nothing is withheld.
    let quiet = render(&mut store);
    let found = drift(&quiet, &held);
    assert_eq!(found.kind, DriftKind::ConflictCopy);
    assert_eq!(found.copies, std::slice::from_ref(&copy));
    assert_eq!(found.generated, None);
    assert_eq!(found.conflict, None);
    assert_eq!(action(&quiet, &held), Action::Unchanged);

    // A commit would change the note: it is held, the rest renders.
    question(&mut store, "q4", case_id);
    let report = render(&mut store);
    assert_eq!(action(&report, &held), Action::Refused(Refusal::Drifted));
    assert_eq!(fs::read(vault.join(&held)).unwrap(), before);
    let found = drift(&report, &held);
    assert_eq!(found.kind, DriftKind::ConflictCopy);
    assert_eq!(found.observed.as_ref(), Some(&hash_bytes(&before)));
    assert_eq!(retained(&store, found), planned(&mut store, &held));
    assert_eq!(report.drift.len(), 1, "{:?}", report.drift);
    assert_eq!(report.publish.refused(), 1);
    assert_eq!(
        fs::read_to_string(vault.join(&copy)).unwrap(),
        "the other device's version\n"
    );
    let mut unknown = vec![copy.clone(), near_miss.clone()];
    unknown.sort();
    assert_eq!(report.unknown, unknown);

    // Removing the copy reconciles the path.
    fs::remove_file(vault.join(&copy)).unwrap();
    let report = render(&mut store);
    assert_eq!(action(&report, &held), Action::Written);
    assert!(report.drift.is_empty());
    assert!(vault.join(&near_miss).exists());
}

/// Publishes a fixed plan, for retired paths and non-note files.
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
            .add(Phase::Views, PlanPath::new(*path).unwrap(), *content, None)
            .unwrap();
    }
    Fixed(builder.finish())
}

fn render_fixed(store: &mut Store, plan: &Fixed) -> RenderReport {
    render_vault(store, plan, &ViewPolicy::default(), &mut |_| {}).unwrap()
}

#[test]
fn edited_view_files_and_retired_paths_drift_without_a_retained_version() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = Workspace::init(&directory.path().join("workspace"), true).unwrap();
    let mut store = Store::open(workspace);
    let vault = vault(&store);
    let first = fixed(
        &mut store,
        &[
            ("Views/Open.base", "filters: open\n"),
            ("Views/Gone.base", "x\n"),
        ],
    );
    render_fixed(&mut store, &first);
    fs::write(vault.join("Views/Open.base"), "filters: mine\n").unwrap();
    fs::write(vault.join("Views/Gone.base"), "edited\n").unwrap();

    let second = fixed(
        &mut store,
        &[("Views/Open.base", "filters: open, sorted\n")],
    );
    let report = render_fixed(&mut store, &second);
    let view = drift(&report, "Views/Open.base");
    assert_eq!(view.kind, DriftKind::Edited);
    assert_eq!(retained(&store, view), b"filters: open, sorted\n");
    // A retired path withholds a deletion, not content.
    let gone = drift(&report, "Views/Gone.base");
    assert_eq!(gone.kind, DriftKind::Edited);
    assert_eq!(gone.generated, None);
    assert_eq!(gone.conflict, None);
    assert_eq!(
        fs::read_to_string(vault.join("Views/Gone.base")).unwrap(),
        "edited\n"
    );
    assert_eq!(
        fs::read_to_string(vault.join("Views/Open.base")).unwrap(),
        "filters: mine\n"
    );

    // A deleted retired file is simply released: it was going away.
    fs::remove_file(vault.join("Views/Gone.base")).unwrap();
    let report = render_fixed(&mut store, &second);
    assert_eq!(action(&report, "Views/Gone.base"), Action::Unchanged);
    assert_eq!(report.drift.len(), 1);
    let (stored, _) = manifest(&store);
    assert!(!stored.owns("Views/Gone.base"));
}

#[test]
fn after_a_killed_render_a_missing_file_is_recreated_unless_already_drifted() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id, questions) = workspace(directory.path());
    let vault = vault(&store);
    let known = note(case_id, questions[0]);
    let fresh = note(case_id, questions[1]);
    fs::remove_file(vault.join(&known)).unwrap();
    assert_eq!(render(&mut store).drift.len(), 1);

    // Kill a render after its first step: the manifest says `updating`.
    question(&mut store, "q4", case_id);
    let killed = catch_unwind(AssertUnwindSafe(|| {
        render_vault(
            &mut store,
            &VaultRenderer,
            &ViewPolicy::default(),
            &mut |_| panic!("killed"),
        )
    }));
    assert!(killed.is_err());
    assert!(matches!(
        manifest(&store).0.state(),
        RenderState::Updating { .. }
    ));

    // That render may have removed files itself, so a file missing now is
    // recreated; one already known drifted stays drifted.
    fs::remove_file(vault.join(&fresh)).unwrap();
    let report = render(&mut store);
    assert_eq!(action(&report, &fresh), Action::Written);
    assert_eq!(action(&report, &known), Action::Refused(Refusal::Drifted));
    assert!(!vault.join(&known).exists());
    assert_eq!(report.drift.len(), 1);
    assert_eq!(manifest(&store).0.state(), &RenderState::Complete);
}

#[test]
fn drift_records_round_trip_and_tampered_ones_only_make_the_manifest_more_cautious() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id, questions) = workspace(directory.path());
    let vault = vault(&store);
    fs::write(vault.join(note(case_id, questions[0])), "edited\n").unwrap();
    fs::remove_file(vault.join(note(case_id, questions[1]))).unwrap();
    question(&mut store, "q4", case_id);
    let report = render(&mut store);
    assert_eq!(report.drift.len(), 2);
    let (stored, status) = manifest(&store);
    assert_eq!(status, ManifestStatus::Loaded);
    assert_eq!(stored.drift().cloned().collect::<Vec<_>>(), report.drift);
    let bytes = fs::read(root(&store).join(RENDER_MANIFEST)).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let kinds: Vec<&str> = value["drifted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["kind"].as_str().unwrap())
        .collect();
    let mut sorted = kinds.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, ["edited", "missing"]);

    let tamper = |field: &str, replacement: Value| {
        let mut value = value.clone();
        value["drifted"][0][field] = replacement;
        fs::write(
            root(&store).join(RENDER_MANIFEST),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let (manifest, status) = manifest(&store);
        assert!(
            matches!(status, ManifestStatus::Rebuilt { .. }),
            "{field}: {status:?}"
        );
        assert_eq!(manifest.owned().count(), 0);
    };
    tamper("path", json!("Cases/elsewhere.md"));
    tamper("path", json!("Notes/mine.md"));
    tamper("kind", json!("vanished"));
    tamper("conflict", json!("vault/Start Here.md"));
    tamper("conflict", json!("local/conflicts/../render-manifest.json"));
    tamper("copies", json!(["../outside.md"]));
    tamper("renamed_to", json!(["/etc/passwd"]));

    // A manifest from before drift records were stored still loads.
    let mut older = value.clone();
    older.as_object_mut().unwrap().remove("drifted");
    fs::write(
        root(&store).join(RENDER_MANIFEST),
        serde_json::to_vec(&older).unwrap(),
    )
    .unwrap();
    let (manifest, status) = manifest(&store);
    assert_eq!(status, ManifestStatus::Loaded);
    assert_eq!(manifest.drift().count(), 0);
}
