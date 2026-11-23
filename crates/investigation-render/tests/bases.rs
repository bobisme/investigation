//! Native Base files beside the static table views (plan §12.5, W12.01):
//! optional and off by default, planned only for a supported stated viewer
//! release, byte-stable across renders and commits, owned like every
//! generated file so that an edit is drift, and retired when switched off.
//! `investigation-cli/tests/vault_fixtures.rs` checks over the golden vaults
//! that each Base selects exactly its static table's rows, all in its case.

use std::fs;

use investigation_core::{identity::Id, wire::Receipt};
use investigation_render::{
    Action, DriftKind, PublishMode, RenderPlan, RenderReport, VaultRenderer, VerifiedSnapshot,
    ViewPolicy, layout, render, render_vault_with, working_policy,
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

fn submit(store: &mut Store, key: &str, case_id: Option<Id>, operation: Value) -> Receipt {
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":key, "case_id":case_id,
        "actor":"agent:bases-test", "preconditions":[], "operations":[operation]
    });
    store
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap_or_else(|failure| panic!("{key}: {failure:?}"))
}

fn question(store: &mut Store, key: &str, case_id: Id) -> Id {
    submit(
        store,
        key,
        Some(case_id),
        json!({"op":"question.create",
            "payload":{"wording":format!("Question {key}?"), "importance":"normal"}}),
    )
    .created[0]
        .id
}

fn case(directory: &std::path::Path) -> (Store, Id) {
    let workspace = Workspace::init(&directory.join("workspace"), true).unwrap();
    let mut store = Store::open(workspace);
    let receipt = submit(
        &mut store,
        "case",
        None,
        json!({"op":"case.create", "payload":{"title":"Bases fixture", "mode":"record"}}),
    );
    (store, receipt.created[0].id)
}

fn plan(store: &mut Store, policy: &ViewPolicy) -> RenderPlan {
    render(&VerifiedSnapshot::read(store).unwrap(), policy).unwrap()
}

fn bases(plan: &RenderPlan) -> Vec<&str> {
    plan.entries()
        .iter()
        .map(|entry| entry.path().as_str())
        .filter(|path| path.ends_with(".base"))
        .collect()
}

fn publish(store: &mut Store, policy: &ViewPolicy) -> RenderReport {
    render_vault_with(
        store,
        &VaultRenderer,
        policy,
        PublishMode::Full,
        &mut |_| {},
    )
    .unwrap()
}

/// A policy that allows the Base features and nothing else, so these tests
/// see Bases alone (the case map has its own tests).
fn on() -> ViewPolicy {
    let profile = investigation_render::compat::Profile::bundled();
    let mut viewer = profile.resolve(Some(&profile.pinned_release.version));
    viewer
        .optional
        .retain(|feature| investigation_render::bases::FEATURES.contains(&feature.as_str()));
    ViewPolicy::default().with_viewer(viewer)
}

#[test]
fn bases_are_off_by_default_and_the_markdown_vault_is_the_same_without_them() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = case(directory.path());
    question(&mut store, "q1", case_id);
    let without = plan(&mut store, &ViewPolicy::default());
    let with = plan(&mut store, &on());
    assert!(bases(&without).is_empty());
    assert_eq!(
        bases(&with),
        [
            format!("Cases/{case_id}/Views/Epistemic Frontier.base"),
            format!("Cases/{case_id}/Views/Questions.base"),
            format!("Cases/{case_id}/Views/Timeline.base"),
        ]
    );
    assert_ne!(without.generation().id(), with.generation().id());
    // Every Markdown file is planned either way; only the view notes differ,
    // by the paragraph that names their Base.
    for entry in without.entries() {
        let path = entry.path().as_str();
        let other = with.entry(path).unwrap();
        let (a, b) = (
            std::str::from_utf8(entry.content()).unwrap(),
            std::str::from_utf8(other.content()).unwrap(),
        );
        if path.contains("/Views/") {
            let (kept, added) = b.split_once("\n## Native Base\n").unwrap();
            assert_eq!(kept, a, "{path}");
            assert!(added.contains(
                "Editing a cell in the Base changes that note's properties, which is drift"
            ));
            assert!(!a.contains("Native Base"));
        } else if path != layout::START_HERE {
            assert_eq!(a, b, "{path}");
        }
    }
}

#[test]
fn the_working_policy_plans_bases_only_for_a_supported_stated_release() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    Workspace::init(&root, true).unwrap();
    let config = root.join("investigation.toml");
    let original = fs::read_to_string(&config).unwrap();
    assert!(!original.contains("viewer_release"));
    let stated = |release: Option<&str>| {
        let text = match release {
            Some(release) => original.replace(
                "preserve_human_folders = true\n",
                &format!("preserve_human_folders = true\nviewer_release = \"{release}\"\n"),
            ),
            None => original.clone(),
        };
        fs::write(&config, text).unwrap();
        working_policy(&Workspace::open(&root).unwrap())
            .allows_all(investigation_render::bases::FEATURES)
    };
    assert!(!stated(None));
    for supported in ["1.12.7", "1.15.1", "v1.14"] {
        assert!(stated(Some(supported)), "{supported}");
    }
    for unsupported in ["1.12.6", "1.15.2", "2.0.0", "latest", ""] {
        assert!(!stated(Some(unsupported)), "{unsupported:?}");
    }
}

#[test]
fn base_bytes_are_identical_across_renders_and_later_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = case(directory.path());
    question(&mut store, "q1", case_id);
    let first = plan(&mut store, &on());
    assert_eq!(first, plan(&mut store, &on()));
    question(&mut store, "q2", case_id);
    let later = plan(&mut store, &on());
    assert_ne!(first.generation().id(), later.generation().id());
    for path in bases(&first) {
        assert_eq!(
            first.entry(path).unwrap().content(),
            later.entry(path).unwrap().content(),
            "{path}"
        );
    }
}

#[test]
fn an_edited_base_or_a_cell_edit_is_drift_and_switching_off_retires_the_bases() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = case(directory.path());
    let id = question(&mut store, "q1", case_id);
    let report = publish(&mut store, &on());
    assert_eq!(report.publish.refused(), 0);
    let vault = store.workspace().root().join("vault");
    let base = format!("Cases/{case_id}/Views/Questions.base");
    let timeline = format!("Cases/{case_id}/Views/Timeline.base");
    let note = layout::record_note(case_id, id).unwrap().unwrap();
    let note = note.as_str();
    assert!(vault.join(&base).is_file());

    // A viewer saving a changed Base, and a cell edit in it, which rewrites
    // the note's property.
    let edited_base = "filters:\n  and:\n    - 'inv_kind == \"question\"'\n";
    fs::write(vault.join(&base), edited_base).unwrap();
    let text = fs::read_to_string(vault.join(note)).unwrap();
    assert!(text.contains("\ninv_status: \"open\"\n"), "{text}");
    fs::write(
        vault.join(note),
        text.replace("\ninv_status: \"open\"\n", "\ninv_status: \"answered\"\n"),
    )
    .unwrap();
    question(&mut store, "q2", case_id);
    let report = publish(&mut store, &on());
    let drifted: Vec<(&str, DriftKind)> = report
        .drift
        .iter()
        .map(|drift| (drift.path.as_str(), drift.kind))
        .collect();
    assert!(
        drifted.contains(&(base.as_str(), DriftKind::Edited)),
        "{drifted:?}"
    );
    assert!(drifted.contains(&(note, DriftKind::Edited)), "{drifted:?}");
    assert_eq!(fs::read_to_string(vault.join(&base)).unwrap(), edited_base);

    // Switching Bases off retires the unedited ones and leaves the edited
    // one in place as drift; the Markdown vault renders complete.
    let report = publish(&mut store, &ViewPolicy::default());
    assert!(!vault.join(&timeline).exists());
    assert_eq!(fs::read_to_string(vault.join(&base)).unwrap(), edited_base);
    assert!(report.drift.iter().any(|drift| drift.path.as_str() == base));
    assert!(
        report
            .publish
            .outcomes
            .iter()
            .filter(|outcome| outcome.path.as_str().ends_with(".md"))
            .all(|outcome| !matches!(outcome.action, Action::Refused(_))
                || outcome.path.as_str() == note)
    );
}
