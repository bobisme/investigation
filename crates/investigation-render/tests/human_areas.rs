//! Human-owned areas (`Notes/`, `Boards/`, `Inbox/`, hidden folders) are
//! preserved byte for byte, untouched even briefly, by every render path:
//! first render, re-render, drift and its conflicts, a lost or rejected
//! manifest, a generation switch, and a plan that names human paths.
//! `Attachments/` keeps files nobody approved and reports them.

#[path = "support/human.rs"]
mod human;

use std::{
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    time::Duration,
};

use investigation_core::identity::Id;
use investigation_render::{
    Action, Audience, CONFLICTS_DIR, CaseRenderer, DriftKind, Manifest, ManifestStatus, Phase,
    PlanBuilder, PlanPath, RENDER_MANIFEST, Refusal, RenderError, RenderLock, RenderPlan,
    RenderReport, VaultRenderer, VerifiedSnapshot, ViewPolicy, render_vault,
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

fn submit(store: &mut Store, key: &str, case_id: Option<Id>, operation: Value) -> Id {
    let input = json!({
        "schema_version":"inv.command.v0-alpha", "command_id":key, "case_id":case_id,
        "actor":"agent:human-areas-test", "preconditions":[], "operations":[operation]
    });
    store
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap()
        .created[0]
        .id
}

fn question(store: &mut Store, key: &str, case_id: Id) -> Id {
    submit(
        store,
        key,
        Some(case_id),
        json!({"op":"question.create", "payload":{"wording":key, "importance":"normal"}}),
    )
}

fn workspace(directory: &Path) -> (Store, Id) {
    let workspace = Workspace::init(&directory.join("workspace"), true).unwrap();
    let mut store = Store::open(workspace);
    let case_id = submit(
        &mut store,
        "case",
        None,
        json!({"op":"case.create", "payload":{"title":"Human areas", "mode":"record"}}),
    );
    question(&mut store, "q1", case_id);
    question(&mut store, "q2", case_id);
    (store, case_id)
}

fn vault(store: &Store) -> PathBuf {
    store.workspace().root().join("vault")
}

fn render_with(store: &mut Store, policy: &ViewPolicy) -> RenderReport {
    render_vault(store, &VaultRenderer, policy, &mut |_| {}).unwrap()
}

fn render(store: &mut Store) -> RenderReport {
    render_with(store, &ViewPolicy::default())
}

fn load(store: &Store) -> (Manifest, ManifestStatus) {
    let lock = RenderLock::acquire(store.workspace(), Duration::ZERO).unwrap();
    let loaded = Manifest::load(store.workspace(), &lock).unwrap();
    (loaded.manifest, loaded.status)
}

fn refusals(report: &RenderReport) -> Vec<(&str, Refusal)> {
    report
        .publish
        .outcomes
        .iter()
        .filter_map(|outcome| match outcome.action {
            Action::Refused(refusal) => Some((outcome.path.as_str(), refusal)),
            _ => None,
        })
        .collect()
}

/// A fixed plan: today's vault plus extra entries.
struct WithExtra(Vec<(String, Vec<u8>)>);

impl CaseRenderer for WithExtra {
    fn render(
        &self,
        snapshot: &VerifiedSnapshot,
        policy: &ViewPolicy,
    ) -> Result<RenderPlan, RenderError> {
        let plan = VaultRenderer.render(snapshot, policy)?;
        let mut builder = PlanBuilder::new(plan.generation().clone());
        for entry in plan.entries() {
            builder.add(
                entry.phase(),
                entry.path().clone(),
                entry.content(),
                entry.projects(),
            )?;
        }
        for (path, bytes) in &self.0 {
            builder.add(Phase::Records, PlanPath::new(path)?, bytes.clone(), None)?;
        }
        Ok(builder.finish())
    }
}

#[test]
fn human_areas_are_untouched_by_render_rerender_drift_manifest_loss_and_generation_switch() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    let vault = vault(&store);
    human::seed(&vault);
    fs::create_dir_all(vault.join("Attachments")).unwrap();
    fs::write(vault.join("Attachments/pasted.png"), b"not approved").unwrap();
    let mut seen = human::fingerprint(&vault);
    let unchanged = |seen: &_, step: &str| {
        assert_eq!(&human::fingerprint(&vault), seen, "after {step}");
    };

    // First render, without a manifest.
    let first = render(&mut store);
    assert_eq!(first.manifest, ManifestStatus::Missing);
    assert!(refusals(&first).is_empty(), "{:?}", refusals(&first));
    unchanged(&seen, "the first render");
    assert_eq!(first.unknown, ["Attachments/pasted.png"]);
    assert_eq!(
        fs::read(vault.join("Attachments/pasted.png")).unwrap(),
        b"not approved"
    );

    // Copies of generated notes, at mirrored paths, are never adopted or
    // retired.
    let overview = format!("Cases/{case_id}/Overview.md");
    let overview_bytes = fs::read(vault.join(&overview)).unwrap();
    human::seed_mirror(&vault, &overview, &overview_bytes);
    seen = human::fingerprint(&vault);

    // Re-render after a commit.
    question(&mut store, "q3", case_id);
    let again = render(&mut store);
    assert!(again.adopted.is_empty());
    assert!(refusals(&again).is_empty(), "{:?}", refusals(&again));
    unchanged(&seen, "a re-render");
    assert_eq!(again.unknown, ["Attachments/pasted.png"]);

    // Drift: one note edited, one deleted, the overview moved into `Notes/`
    // (by the person, so it is theirs now), and a conflict copy beside a
    // question note. The would-be content goes to the conflicts area.
    let notes: Vec<String> = fs::read_dir(vault.join(format!("Cases/{case_id}/Questions")))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    let questions = format!("Cases/{case_id}/Questions");
    fs::write(vault.join(&questions).join(&notes[0]), "edited\n").unwrap();
    fs::remove_file(vault.join(&questions).join(&notes[1])).unwrap();
    fs::rename(vault.join(&overview), vault.join("Notes/moved overview.md")).unwrap();
    let stem = notes[2].trim_end_matches(".md");
    fs::write(
        vault
            .join(&questions)
            .join(format!("{stem} (conflicted copy).md")),
        "copy\n",
    )
    .unwrap();
    seen = human::fingerprint(&vault);
    question(&mut store, "q4", case_id);
    let drifted = render(&mut store);
    let kinds: Vec<DriftKind> = drifted.drift.iter().map(|drift| drift.kind).collect();
    assert_eq!(drifted.drift.len(), 4, "{:?}", drifted.drift);
    for kind in [
        DriftKind::Edited,
        DriftKind::Missing,
        DriftKind::Renamed,
        DriftKind::ConflictCopy,
    ] {
        assert!(kinds.contains(&kind), "{kinds:?}");
    }
    let renamed = drifted
        .drift
        .iter()
        .find(|drift| drift.kind == DriftKind::Renamed)
        .unwrap();
    // Every human file that is the same note is named (the mirrored copies
    // carry its `inv_id`); finding them only reads the human areas.
    assert!(
        renamed
            .renamed_to
            .contains(&"Notes/moved overview.md".to_owned()),
        "{:?}",
        renamed.renamed_to
    );
    assert!(
        store
            .workspace()
            .root()
            .join(CONFLICTS_DIR)
            .read_dir()
            .unwrap()
            .next()
            .is_some()
    );
    unchanged(&seen, "a render that found drift");
    let held = render(&mut store);
    assert_eq!(held.drift.len(), 4);
    unchanged(&seen, "a render holding drift");

    // A lost manifest, a corrupt one, and one that claims a human path.
    let manifest_path = store.workspace().root().join(RENDER_MANIFEST);
    fs::remove_file(&manifest_path).unwrap();
    let lost = render(&mut store);
    assert_eq!(lost.manifest, ManifestStatus::Missing);
    assert!(
        lost.adopted
            .iter()
            .all(|path| !path.as_str().starts_with("Notes"))
    );
    unchanged(&seen, "a render without a manifest");
    fs::write(&manifest_path, "{ not json").unwrap();
    let corrupt = render(&mut store);
    assert!(matches!(corrupt.manifest, ManifestStatus::Rebuilt { .. }));
    unchanged(&seen, "a render with a corrupt manifest");
    let mut value: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    value["owned"][0]["path"] = json!("Notes/thinking.md");
    fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();
    let claimed = render(&mut store);
    assert!(matches!(claimed.manifest, ManifestStatus::Rebuilt { .. }));
    unchanged(&seen, "a render with a manifest claiming a human path");

    // Generation switches: another audience, previews allowed.
    for policy in [
        ViewPolicy::for_audience(Audience::Restricted),
        ViewPolicy::for_audience(Audience::Public).with_derivative_previews(true),
        ViewPolicy::default(),
    ] {
        let report = render_with(&mut store, &policy);
        // The lost manifest left the edited notes unproven; the plan never
        // names a human path.
        assert!(
            refusals(&report)
                .iter()
                .all(|(_, refusal)| matches!(refusal, Refusal::Drifted | Refusal::NotProvenOwned)),
            "{:?}",
            refusals(&report)
        );
        unchanged(&seen, policy.fingerprint().as_str());
    }
    assert_eq!(
        fs::read(vault.join("Attachments/pasted.png")).unwrap(),
        b"not approved"
    );
}

/// A plan that names human paths is refused path by path before anything is
/// inspected; nothing there is swept, intended, owned or retired, even when
/// the render is killed between files.
#[test]
fn a_plan_naming_human_paths_is_refused_and_never_recorded() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, _) = workspace(directory.path());
    let vault = vault(&store);
    human::seed(&vault);
    render(&mut store);
    let seen = human::fingerprint(&vault);

    let faulty = WithExtra(
        [
            "Notes/thinking.md",
            "Notes/idea.md",
            "notes/new.md",
            "Notes",
            "Boards/plan.canvas",
            "INBOX/clipping.txt",
        ]
        .into_iter()
        .map(|path| (path.to_owned(), b"generated over yours\n".to_vec()))
        .collect(),
    );

    // Killed after its first file: the stored write-ahead intent names no
    // human path, so the manifest still loads.
    let killed = catch_unwind(AssertUnwindSafe(|| {
        render_vault(&mut store, &faulty, &ViewPolicy::default(), &mut |step| {
            if step.index == 0 {
                std::panic::panic_any("killed");
            }
        })
    }));
    assert!(killed.is_err());
    let (manifest, status) = load(&store);
    assert_eq!(status, ManifestStatus::Loaded);
    assert!(
        manifest
            .known_paths()
            .iter()
            .all(|path| !investigation_render::layout::is_human_owned(path))
    );
    assert_eq!(human::fingerprint(&vault), seen);

    let report = render_vault(&mut store, &faulty, &ViewPolicy::default(), &mut |_| {}).unwrap();
    let human_refusals: Vec<_> = refusals(&report)
        .into_iter()
        .filter(|(_, refusal)| *refusal == Refusal::HumanOwned)
        .collect();
    assert_eq!(human_refusals.len(), faulty.0.len(), "{human_refusals:?}");
    assert_eq!(refusals(&report).len(), faulty.0.len());
    for outcome in &report.publish.outcomes {
        if outcome.action == Action::Refused(Refusal::HumanOwned) {
            assert_eq!(outcome.previous, None, "not even inspected");
        }
    }
    assert!(
        report
            .swept
            .iter()
            .all(|swept| !swept.path.contains("Notes"))
    );
    let (manifest, status) = load(&store);
    assert_eq!(status, ManifestStatus::Loaded);
    assert!(
        manifest
            .known_paths()
            .iter()
            .all(|path| !investigation_render::layout::is_human_owned(path))
    );
    assert_eq!(human::fingerprint(&vault), seen);

    // Back to the real plan: nothing under the human roots is retired.
    let report = render(&mut store);
    assert_eq!(report.publish.removed(), 0);
    assert_eq!(human::fingerprint(&vault), seen);
}

/// `Attachments/` holds only approved derivative previews. Without approval
/// (the default ownership, and every working-vault render whose plan names
/// something that is not a reviewed derivative), an attachment is refused
/// before inspection and a file already there is kept and reported.
#[test]
fn unapproved_attachments_are_refused_and_unknown_ones_kept_and_reported() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    let vault = vault(&store);
    render(&mut store);
    let artifact = "art_01890a5d-ac96-774b-bcce-b302099a8057";
    fs::create_dir_all(vault.join("Attachments/sub")).unwrap();
    fs::write(vault.join(format!("Attachments/{artifact}.png")), b"mine").unwrap();
    fs::write(vault.join("Attachments/sub/diagram.svg"), b"<svg/>").unwrap();

    let plan = WithExtra(vec![
        (format!("Attachments/{artifact}.png"), b"generated".to_vec()),
        (format!("Attachments/{case_id}.png"), b"generated".to_vec()),
        ("Attachments/picture.png".to_owned(), b"generated".to_vec()),
        (
            "Attachments/sub/diagram.svg".to_owned(),
            b"generated".to_vec(),
        ),
    ]);
    for policy in [
        ViewPolicy::default(),
        ViewPolicy::for_audience(Audience::Restricted).with_derivative_previews(true),
    ] {
        let report = render_vault(&mut store, &plan, &policy, &mut |_| {}).unwrap();
        let refused = refusals(&report);
        assert_eq!(refused.len(), plan.0.len(), "{refused:?}");
        assert!(
            refused
                .iter()
                .all(|(_, refusal)| *refusal == Refusal::UnlistedAttachment)
        );
        assert_eq!(
            report.unknown,
            [
                format!("Attachments/{artifact}.png"),
                "Attachments/sub/diagram.svg".to_owned()
            ]
        );
    }
    assert_eq!(
        fs::read(vault.join(format!("Attachments/{artifact}.png"))).unwrap(),
        b"mine"
    );
    assert_eq!(
        fs::read(vault.join("Attachments/sub/diagram.svg")).unwrap(),
        b"<svg/>"
    );
    assert!(!vault.join("Attachments/picture.png").exists());
}
