//! Render generations: every note names the snapshot it reflects, the
//! manifest advertises `updating`/`partial`/`complete`, and a render killed
//! at any file leaves a state from which the next render completes.

#[path = "support/human.rs"]
mod human;

#[path = "support/vault.rs"]
mod checks;

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::Once,
    time::Duration,
};

use investigation_core::identity::Id;
use investigation_render::{
    Action, CaseRenderer, EntryHealth, Generation, Manifest, NoteMarker, Phase, PlanBuilder,
    PlanPath, PublishMode, Publisher, RENDERER_VERSION, Refusal, RenderError, RenderLock,
    RenderPlan, RenderReport, RenderState, SweptAction, VaultRenderer, VaultState,
    VerifiedSnapshot, ViewPolicy, incremental::rebase, layout, render_vault, render_vault_with,
    vault_status,
};
use investigation_store::{Store, Workspace, journal::hash_bytes};
use serde_json::{Value, json};

fn submit(store: &mut Store, key: &str, case_id: Option<Id>, operation: Value) -> Id {
    let input = json!({
        "schema_version":"inv.command.v0-alpha",
        "command_id":key,
        "case_id":case_id,
        "actor":"agent:generation-test",
        "preconditions":[],
        "operations":[operation]
    });
    let receipt = store.record(&serde_json::to_vec(&input).unwrap()).unwrap();
    receipt.created[0].id
}

fn case(store: &mut Store, key: &str) -> Id {
    submit(
        store,
        key,
        None,
        json!({"op":"case.create", "payload":{"title":key, "mode":"record"}}),
    )
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
    let case_id = case(&mut store, "generation fixture");
    question(&mut store, "q1", case_id);
    question(&mut store, "q2", case_id);
    (store, case_id)
}

fn vault(store: &Store) -> PathBuf {
    store.workspace().root().join("vault")
}

const RETIRED: &str = "Retired/old.md";

/// The vault layout, optionally with one extra file so that the next plan
/// without it has a retired path to remove.
struct Vault {
    extra: bool,
}

impl CaseRenderer for Vault {
    fn render(
        &self,
        snapshot: &VerifiedSnapshot,
        policy: &ViewPolicy,
    ) -> Result<RenderPlan, RenderError> {
        let plan = VaultRenderer.render(snapshot, policy)?;
        if !self.extra {
            return Ok(plan);
        }
        let mut builder = PlanBuilder::new(plan.generation().clone());
        for entry in plan.entries() {
            builder.add(
                entry.phase(),
                entry.path().clone(),
                entry.content(),
                entry.projects(),
            )?;
        }
        builder.add(Phase::Views, PlanPath::new(RETIRED)?, "retired\n", None)?;
        Ok(builder.finish())
    }

    fn announce(
        &self,
        plan: &RenderPlan,
        snapshot: &VerifiedSnapshot,
        policy: &ViewPolicy,
        health: &EntryHealth,
    ) -> Result<RenderPlan, RenderError> {
        VaultRenderer.announce(plan, snapshot, policy, health)
    }
}

fn render(store: &mut Store, extra: bool) -> Result<RenderReport, RenderError> {
    render_vault(store, &Vault { extra }, &ViewPolicy::default(), &mut |_| {})
}

/// A render in which every note is restamped with its case head, so a commit
/// rewrites every note of its case.
fn render_full(store: &mut Store, extra: bool) -> Result<RenderReport, RenderError> {
    render_vault_with(
        store,
        &Vault { extra },
        &ViewPolicy::default(),
        PublishMode::Full,
        &mut |_| {},
    )
}

fn current_plan(store: &mut Store) -> RenderPlan {
    let snapshot = VerifiedSnapshot::read(store).unwrap();
    VaultRenderer
        .render(&snapshot, &ViewPolicy::default())
        .unwrap()
}

fn current_generation(store: &mut Store) -> Generation {
    let snapshot = VerifiedSnapshot::read(store).unwrap();
    Generation::new(RENDERER_VERSION, &snapshot, &ViewPolicy::default())
}

fn manifest(store: &Store) -> Manifest {
    let lock = RenderLock::acquire(store.workspace(), Duration::ZERO).unwrap();
    Manifest::load(store.workspace(), &lock).unwrap().manifest
}

/// Every file under `root` relative to it, hidden ones included.
fn files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                pending.push(path);
            } else {
                found.push(path.strip_prefix(root).unwrap().to_owned());
            }
        }
    }
    found.sort();
    found
}

/// Whether a vault-relative file lies in a human-owned folder. A top-level
/// hidden file is not in a folder: it may be a leftover temporary.
fn in_human_area(path: &Path) -> bool {
    let mut components = path.components();
    let first = components.next().unwrap().as_os_str();
    components.next().is_some() && layout::is_human_root(first.to_str().unwrap())
}

/// The vault is exactly today's plan, up to notes that keep the earlier head
/// their unchanged bytes were rendered at; the manifest is complete and
/// current, owns every file at its bytes and basis; and nothing a crash left
/// behind remains.
fn assert_complete(store: &mut Store, report: &RenderReport) {
    assert_eq!(report.publish.refused(), 0, "{:?}", report.publish.outcomes);
    assert!(report.unknown.is_empty(), "{:?}", report.unknown);
    let plan = current_plan(store);
    assert_eq!(report.publish.generation.id(), plan.generation().id());
    // Human-owned folders are checked by fingerprint where they are seeded.
    let on_disk: Vec<PathBuf> = files(&vault(store))
        .into_iter()
        .filter(|path| !in_human_area(path))
        .collect();
    let planned: Vec<PathBuf> = plan
        .entries()
        .iter()
        .map(|entry| PathBuf::from(entry.path().as_str()))
        .collect();
    let mut sorted = planned.clone();
    sorted.sort();
    assert_eq!(on_disk, sorted, "only planned files, no leftovers");
    let snapshot = VerifiedSnapshot::read(store).unwrap();
    let manifest = manifest(store);
    for entry in plan.entries() {
        let bytes = fs::read(vault(store).join(entry.path().as_str())).unwrap();
        let kept = checks::up_to_basis(entry, &bytes, &snapshot).unwrap();
        let record = manifest.get(entry.path()).unwrap();
        assert_eq!(record.sha256, hash_bytes(&bytes), "{}", entry.path());
        if kept.is_some() {
            assert_eq!(record.basis, kept, "{}", entry.path());
        }
    }
    assert_eq!(manifest.state(), &RenderState::Complete);
    assert_eq!(manifest.pending().count(), 0);
    let status = vault_status(store.workspace()).unwrap();
    assert!(status.is_current(&current_generation(store)), "{status:?}");
    let local = store.workspace().root().join("local");
    assert!(
        files(&local)
            .iter()
            .all(|path| !path.to_string_lossy().contains(".inv-")),
        "{:?}",
        files(&local)
    );
}

struct Killed;

fn quiet_kills() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.payload().downcast_ref::<Killed>().is_none() {
                default(info);
            }
        }));
    });
}

/// Run a render and stop it dead right after publish step `at`, as a kill
/// would: nothing after that step runs. `false` when the render finished
/// before reaching that step.
fn render_killed_after(store: &mut Store, at: usize) -> bool {
    quiet_kills();
    let result = catch_unwind(AssertUnwindSafe(|| {
        render_vault(
            store,
            &Vault { extra: false },
            &ViewPolicy::default(),
            &mut |step| {
                if step.index == at {
                    std::panic::panic_any(Killed);
                }
            },
        )
    }));
    match result {
        Err(payload) => {
            assert!(payload.downcast_ref::<Killed>().is_some());
            true
        }
        Ok(report) => {
            report.unwrap();
            false
        }
    }
}

#[test]
fn every_note_names_its_snapshot_and_other_cases_never_churn() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, first) = workspace(directory.path());
    let second = case(&mut store, "second case");
    question(&mut store, "q3", second);
    let report = render(&mut store, false).unwrap();
    let generation = report.publish.generation.clone();
    let head = |case_id: Id| {
        generation
            .heads()
            .iter()
            .find(|(id, _)| *id == case_id)
            .unwrap()
            .1
            .clone()
    };
    let before = current_plan(&mut store);
    for entry in before.entries() {
        let bytes = fs::read(vault(&store).join(entry.path().as_str())).unwrap();
        let path = entry.path().as_str();
        let expected = if path == "Start Here.md" {
            NoteMarker::Generation(generation.id().to_owned())
        } else if path.starts_with(&format!("Cases/{first}/")) {
            NoteMarker::Case(head(first))
        } else {
            assert!(path.starts_with(&format!("Cases/{second}/")));
            NoteMarker::Case(head(second))
        };
        // Any single file, read on its own, says which snapshot it reflects.
        assert_eq!(NoteMarker::read(&bytes), Some(expected), "{path}");
        if path != "Start Here.md" {
            assert!(!String::from_utf8(bytes).unwrap().contains(generation.id()));
        }
    }

    // A commit to the second case plans new bytes only for its notes and
    // Start Here: each of its notes names the new head.
    question(&mut store, "q4", second);
    let after = current_plan(&mut store);
    assert_ne!(after.generation().id(), before.generation().id());
    let later_head = after.generation().heads()[1].1.clone();
    for entry in before.entries() {
        let path = entry.path().as_str();
        let later = after.entry(path).unwrap();
        let unchanged = path.starts_with(&format!("Cases/{first}/"));
        assert_eq!(unchanged, later.content() == entry.content(), "{path}");
    }
    // The render rewrites only the notes whose content changed. A second-case
    // note that differs from its earlier bytes only in the head it names
    // keeps those bytes; the rest of the second case is rewritten.
    let report = render(&mut store, false).unwrap();
    let mut kept = 0;
    for entry in before.entries() {
        let path = entry.path().as_str();
        let written = report
            .publish
            .outcomes
            .iter()
            .find(|outcome| outcome.path.as_str() == path)
            .unwrap()
            .action;
        let on_disk = fs::read(vault(&store).join(path)).unwrap();
        let planned = after.entry(path).unwrap().content();
        let content_changed = if path.starts_with(&format!("Cases/{second}/")) {
            rebase(planned, &later_head, &head(second)).as_deref() != Some(entry.content())
        } else {
            planned != entry.content()
        };
        if content_changed {
            assert_eq!(written, Action::Written, "{path}");
            assert_eq!(on_disk, planned, "{path}");
        } else {
            assert_eq!(written, Action::Unchanged, "{path}");
            assert_eq!(on_disk, entry.content(), "{path}");
            kept += usize::from(path.starts_with(&format!("Cases/{second}/")));
        }
    }
    assert!(kept > 0, "an unchanged second-case note keeps its bytes");
    for outcome in &report.publish.outcomes {
        if before.entry(outcome.path.as_str()).is_none() {
            assert_eq!(outcome.action, Action::Written, "{}", outcome.path);
        }
    }

    // In full mode the same commit restamps every second-case note.
    question(&mut store, "q5", second);
    let report = render_full(&mut store, false).unwrap();
    for outcome in &report.publish.outcomes {
        let path = outcome.path.as_str();
        let expected = if path.starts_with(&format!("Cases/{first}/")) {
            Action::Unchanged
        } else {
            Action::Written
        };
        assert_eq!(outcome.action, expected, "{path}");
    }
}

/// Kill the render after each file in turn. Whatever is on disk, each note
/// still names the snapshot it reflects, `vault status` reports an
/// interrupted partial render with the last complete generation, and the
/// next render completes: at the same snapshot, and after further commits
/// that change every file the killed render wrote. The human-owned folders
/// are untouched throughout, killed render and recovery included.
#[test]
fn killing_a_render_at_each_file_leaves_a_state_the_next_render_completes() {
    for moved_on in [false, true] {
        let mut at = 0;
        loop {
            let directory = tempfile::tempdir().unwrap();
            let (mut store, case_id) = workspace(directory.path());
            human::seed(&vault(&store));
            let seen = human::fingerprint(&vault(&store));
            let first = render(&mut store, true).unwrap();
            let complete = first.publish.generation.clone();
            question(&mut store, "q3", case_id);
            let target = current_generation(&mut store);
            if !render_killed_after(&mut store, at) {
                assert!(at >= 6, "three notes, overview, Start Here, one retired");
                break;
            }
            assert_eq!(
                human::fingerprint(&vault(&store)),
                seen,
                "killed after {at}"
            );

            let status = vault_status(store.workspace()).unwrap();
            assert_eq!(
                status.state,
                VaultState::Partial {
                    last_complete: Some(complete.id().to_owned()),
                    generation: target.id().to_owned(),
                    reason: "the render was interrupted before it recorded its outcome".to_owned(),
                    interrupted: true,
                },
                "killed after step {at}"
            );
            assert!(!status.is_current(&target));
            let heads = [complete.heads()[0].1.clone(), target.heads()[0].1.clone()];
            for path in files(&vault(&store)) {
                if in_human_area(&path) {
                    continue;
                }
                let bytes = fs::read(vault(&store).join(&path)).unwrap();
                match NoteMarker::read(&bytes) {
                    Some(NoteMarker::Case(head)) => assert!(heads.contains(&head)),
                    Some(NoteMarker::Generation(id)) => {
                        assert!([complete.id(), target.id()].contains(&id.as_str()));
                    }
                    None => assert_eq!(path, Path::new(RETIRED)),
                }
            }

            if moved_on {
                question(&mut store, "q4", case_id);
            }
            let report = render(&mut store, false).unwrap();
            assert_complete(&mut store, &report);
            assert!(
                report.adopted.is_empty(),
                "recovered files are not adoptions"
            );
            assert_eq!(
                human::fingerprint(&vault(&store)),
                seen,
                "recovered from {at}"
            );
            at += 1;
        }
    }
}

/// A render killed after `updating` was stored but before any vault file
/// changed is completed by the next render too.
#[test]
fn a_render_killed_before_its_first_file_is_completed_next_time() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    render(&mut store, true).unwrap();
    question(&mut store, "q3", case_id);
    let plan = current_plan(&mut store);
    {
        let lock = RenderLock::acquire(store.workspace(), Duration::ZERO).unwrap();
        let loaded = Manifest::load(store.workspace(), &lock).unwrap();
        loaded
            .manifest
            .begin(&plan)
            .store(store.workspace(), &lock)
            .unwrap();
    }
    let status = vault_status(store.workspace()).unwrap();
    assert!(matches!(
        status.state,
        VaultState::Partial {
            interrupted: true,
            ..
        }
    ));
    question(&mut store, "q4", case_id);
    let report = render(&mut store, false).unwrap();
    assert_complete(&mut store, &report);
}

#[test]
fn status_reports_updating_while_a_render_runs_and_stale_after_a_commit() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    let unrendered = vault_status(store.workspace()).unwrap();
    assert_eq!(unrendered.state, VaultState::Unrendered);
    let first = render(&mut store, false).unwrap();
    question(&mut store, "q3", case_id);

    // Rendered, then a commit: complete but stale.
    let stale = vault_status(store.workspace()).unwrap();
    assert_eq!(
        stale.state,
        VaultState::Complete {
            generation: first.publish.generation.id().to_owned()
        }
    );
    let target = current_generation(&mut store);
    assert!(!stale.is_current(&target));

    let workspace = store.workspace().clone();
    let mut seen = Vec::new();
    render_vault(
        &mut store,
        &VaultRenderer,
        &ViewPolicy::default(),
        &mut |_| seen.push(vault_status(&workspace).unwrap().state),
    )
    .unwrap();
    assert!(!seen.is_empty());
    for state in seen {
        assert_eq!(
            state,
            VaultState::Updating {
                from: Some(first.publish.generation.id().to_owned()),
                to: target.id().to_owned(),
            }
        );
    }
    assert!(vault_status(&workspace).unwrap().is_current(&target));
}

#[test]
fn a_failed_render_advertises_partial_while_canonical_reads_continue() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    let first = render(&mut store, false).unwrap();
    let added = question(&mut store, "q3", case_id);
    let case_dir = vault(&store).join(format!("Cases/{case_id}"));
    fs::set_permissions(&case_dir, fs::Permissions::from_mode(0o500)).unwrap();
    if fs::write(case_dir.join("probe"), "").is_ok() {
        // Permission bits do not bind this user (for example root); skip.
        fs::remove_file(case_dir.join("probe")).unwrap();
        fs::set_permissions(&case_dir, fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }
    let error = render(&mut store, false).unwrap_err();
    let target = current_generation(&mut store);

    let status = vault_status(store.workspace()).unwrap();
    assert_eq!(
        status.state,
        VaultState::Partial {
            last_complete: Some(first.publish.generation.id().to_owned()),
            generation: target.id().to_owned(),
            reason: error.to_string(),
            interrupted: false,
        }
    );
    assert_eq!(
        status.last_complete.as_ref().unwrap().id,
        first.publish.generation.id()
    );

    // The opening note, written last and still writable, announces it.
    let start = fs::read_to_string(vault(&store).join("Start Here.md")).unwrap();
    assert!(
        start.contains("- State: partial. The render of this generation stopped"),
        "{start}"
    );
    assert!(start.contains("Permission denied"), "{start}");
    assert_eq!(
        NoteMarker::read(start.as_bytes()),
        Some(NoteMarker::Generation(target.id().to_owned()))
    );

    // Canonical state is unaffected and still readable and writable.
    let cases = store.cases().unwrap();
    assert!(cases[&case_id].state.current(added).is_some());
    let later = question(&mut store, "q4", case_id);
    assert!(
        store.cases().unwrap()[&case_id]
            .state
            .current(later)
            .is_some()
    );

    fs::set_permissions(&case_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let report = render(&mut store, false).unwrap();
    assert_complete(&mut store, &report);
}

/// Leftover temporaries are cleaned only when they carry the publisher's
/// exact names beside a managed path; moved-aside files with unproven bytes
/// are put back or kept, never deleted; links and foreign names are ignored.
#[test]
fn leftovers_of_a_killed_render_are_swept_safely() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    render(&mut store, true).unwrap();
    question(&mut store, "q3", case_id);
    let plan = current_plan(&mut store);
    let tag = &plan.generation().id()["gen_".len()..][..12];
    let root = vault(&store);
    let notes = plan
        .entries()
        .iter()
        .filter(|entry| entry.path().as_str().contains("/Questions/"))
        .map(|entry| entry.path().as_str().to_owned())
        .collect::<Vec<_>>();
    let (q1, q2) = (&notes[0], &notes[1]);
    let beside = |path: &str, leftover: &str| {
        let path = Path::new(path);
        let name = path.file_name().unwrap().to_str().unwrap();
        root.join(path.parent().unwrap())
            .join(format!(".{name}.{leftover}"))
    };

    // Eight stale temporaries with this render's own tag would otherwise
    // block every attempt to write q1.
    let stale: Vec<PathBuf> = (0..8)
        .map(|n| beside(q1, &format!("{tag}-{n}.inv-tmp")))
        .collect();
    for path in &stale {
        fs::write(path, "half a note").unwrap();
    }
    // The retired file was moved aside, then the render died before unlinking.
    let retired = root.join(RETIRED);
    let aside_owned = beside(RETIRED, "render-0.inv-del");
    fs::rename(&retired, &aside_owned).unwrap();
    // A moved-aside file with bytes the renderer never generated, target
    // present: kept. Target absent: put back.
    let aside_kept = beside(q2, &format!("{tag}-1.inv-del"));
    fs::write(&aside_kept, "someone's edit\n").unwrap();
    let start = root.join("Start Here.md");
    let aside_back = beside("Start Here.md", &format!("{tag}-0.inv-del"));
    fs::write(&start, "edited start\n").unwrap();
    fs::rename(&start, &aside_back).unwrap();
    // Names the sweep must ignore.
    let outside = directory.path().join("outside.txt");
    fs::write(&outside, "outside\n").unwrap();
    let ignored = [
        beside(q1, "inv-tmp"),
        beside("Cases/unmanaged.md", &format!("{tag}-0.inv-tmp")),
        root.join(format!("Notes/.mine.md.{tag}-0.inv-tmp")),
        beside(q1, &format!("{tag}-0.inv-tmp.bak")),
    ];
    for path in &ignored {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "not ours\n").unwrap();
    }
    let link = beside(q2, &format!("{tag}-2.inv-tmp"));
    symlink(&outside, &link).unwrap();
    let manifest_temp = store
        .workspace()
        .root()
        .join("local/.render-manifest.json.manifest-0.inv-tmp");
    fs::write(&manifest_temp, "{").unwrap();

    // Full mode, so the render must write q1 beside the stale temporaries.
    let report = render_full(&mut store, false).unwrap();
    let action = |path: &Path| {
        let relative = path
            .strip_prefix(store.workspace().root())
            .unwrap()
            .to_str()
            .unwrap();
        report
            .swept
            .iter()
            .find(|swept| swept.path == relative)
            .map(|swept| swept.action)
    };
    for path in &stale {
        assert_eq!(action(path), Some(SweptAction::Removed));
        assert!(!path.exists());
    }
    assert_eq!(action(&aside_owned), Some(SweptAction::Removed));
    assert!(!aside_owned.exists() && !retired.exists());
    assert_eq!(action(&aside_kept), Some(SweptAction::Kept));
    assert_eq!(fs::read_to_string(&aside_kept).unwrap(), "someone's edit\n");
    assert_eq!(action(&aside_back), Some(SweptAction::Restored));
    assert_eq!(action(&manifest_temp), Some(SweptAction::Removed));
    assert!(!manifest_temp.exists());
    assert_eq!(report.swept.len(), stale.len() + 4);
    for path in &ignored {
        assert_eq!(fs::read_to_string(path).unwrap(), "not ours\n");
    }
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside\n");

    // The restored Start Here is someone's edit of an owned file: drift.
    let restored = report
        .publish
        .outcomes
        .iter()
        .find(|outcome| outcome.path.as_str() == "Start Here.md")
        .unwrap();
    assert_eq!(restored.action, Action::Refused(Refusal::Drifted));
    assert_eq!(fs::read_to_string(&start).unwrap(), "edited start\n");
    let written = fs::read(root.join(q1)).unwrap();
    assert_eq!(&written, plan.entry(q1).unwrap().content());
    assert_eq!(
        manifest(&store)
            .get(&PlanPath::new(q1.as_str()).unwrap())
            .unwrap()
            .sha256,
        hash_bytes(&written)
    );
}

/// A render killed after its last vault write, before the manifest records
/// the outcome, is completed by the next one. Two points: inside the
/// publication, right after the last rename and before the final durability
/// barrier; and after the publication returned (every write durable) but
/// before the manifest commit. Either way the manifest still reads
/// `updating`, claims nothing it did not claim before, and the next render
/// converges without adopting anything or touching a human-owned file.
#[test]
fn a_render_killed_between_its_writes_and_the_manifest_commit_is_completed_next_time() {
    for after_barrier in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, case_id) = workspace(directory.path());
        human::seed(&vault(&store));
        let seen = human::fingerprint(&vault(&store));
        let first = render(&mut store, true).unwrap();
        let before = manifest(&store);
        question(&mut store, "q3", case_id);
        let plan = current_plan(&mut store);
        if after_barrier {
            let lock = RenderLock::acquire(store.workspace(), Duration::ZERO).unwrap();
            let loaded = Manifest::load(store.workspace(), &lock).unwrap().manifest;
            loaded.begin(&plan).store(store.workspace(), &lock).unwrap();
            let report = Publisher::open(store.workspace(), &lock)
                .unwrap()
                .publish(&plan, &loaded, &mut |_| {})
                .unwrap();
            assert!(report.written() > 0);
            // Killed here: the manifest commit never happens.
        } else {
            let total = plan.entries().len() + 1; // one retired path
            assert!(render_killed_after(&mut store, total - 1));
        }
        // Every planned file is on disk, and the record owns only what the
        // completed render owned, at the same hashes: whatever the killed
        // render wrote is accepted through its write-ahead intent only.
        let killed = manifest(&store);
        assert!(matches!(killed.state(), RenderState::Updating { .. }));
        assert_eq!(
            killed.owned().collect::<Vec<_>>(),
            before.owned().collect::<Vec<_>>()
        );
        assert_eq!(killed.last_complete(), before.last_complete());
        let mut written = 0;
        for entry in plan.entries() {
            let bytes = fs::read(vault(&store).join(entry.path().as_str())).unwrap();
            let sha256 = hash_bytes(&bytes);
            assert!(killed.accepts(entry.path(), &sha256), "{}", entry.path());
            if before
                .get(entry.path())
                .is_none_or(|record| record.sha256 != sha256)
            {
                written += 1;
            }
        }
        assert!(written > 0, "the killed render wrote files");
        assert_eq!(
            vault_status(store.workspace()).unwrap().state,
            VaultState::Partial {
                last_complete: Some(first.publish.generation.id().to_owned()),
                generation: plan.generation().id().to_owned(),
                reason: "the render was interrupted before it recorded its outcome".to_owned(),
                interrupted: true,
            }
        );

        let report = render(&mut store, false).unwrap();
        assert_complete(&mut store, &report);
        assert!(
            report.adopted.is_empty(),
            "recovered files are not adoptions"
        );
        assert!(report.drift.is_empty(), "{:?}", report.drift);
        assert_eq!(human::fingerprint(&vault(&store)), seen);
    }
}

/// Writes are staged in batches: a render killed right after its first
/// rename leaves the rest of its batch as hidden temporaries, never as
/// half-written notes. Each path holds its earlier bytes or the planned
/// ones, and the next render sweeps the temporaries and completes.
#[test]
fn a_render_killed_mid_batch_leaves_only_temporaries_the_next_render_sweeps() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    let earlier = current_plan(&mut store);
    render(&mut store, false).unwrap();
    question(&mut store, "q3", case_id);
    let plan = current_plan(&mut store);
    assert!(render_killed_after(&mut store, 0));
    let temporaries: Vec<PathBuf> = files(&vault(&store))
        .into_iter()
        .filter(|path| path.to_string_lossy().ends_with(".inv-tmp"))
        .collect();
    assert!(!temporaries.is_empty(), "the rest of the batch was staged");
    for entry in plan.entries() {
        let Ok(bytes) = fs::read(vault(&store).join(entry.path().as_str())) else {
            continue;
        };
        let earlier = earlier
            .entry(entry.path().as_str())
            .map(|earlier| earlier.content());
        assert!(
            bytes == entry.content() || Some(bytes.as_slice()) == earlier,
            "{} holds neither its earlier nor its planned bytes",
            entry.path()
        );
    }
    let report = render(&mut store, false).unwrap();
    assert_eq!(report.swept.len(), temporaries.len(), "{:?}", report.swept);
    assert!(
        report
            .swept
            .iter()
            .all(|swept| swept.action == SweptAction::Removed)
    );
    assert_complete(&mut store, &report);
}

/// What a power loss may leave of a killed batched publication (ADR 0012),
/// as this test models it.
#[derive(Clone, Copy, Debug)]
enum PowerLoss {
    /// Every change the killed render made survived.
    Kept,
    /// Every change after the batch barrier was lost: the renames into
    /// place and the removal of the retired file. Each temporary the
    /// barrier made durable is still there, whole.
    RenamesLost,
    /// Only some of those changes were lost.
    SomeRenamesLost,
    /// The power went before the barrier: no rename or removal survived,
    /// and every temporary holds a torn prefix of its bytes.
    TornTemporaries,
}

/// A power loss after each publish step, in each way [`PowerLoss`] models,
/// leaves a state from which the next render completes: no path holds
/// bytes that are neither its earlier nor its planned ones, the manifest
/// claims nothing a lost change undid, no hand edit is reported, the
/// leftover temporaries are swept and the human-owned folders are
/// untouched.
///
/// This is a model, not a filesystem fault injector: the lost changes are
/// undone on disk by the test after a kill, as ADR 0012's ordering allows
/// (a barrier makes everything before it durable; anything after it may be
/// lost, and data written before it was not yet durable may be torn). It
/// qualifies recovery from those states. It does not show that `syncfs`
/// gives that ordering on a given filesystem and device; that needs a
/// fault-injecting filesystem such as LazyFS, or a device mapper target.
#[test]
fn a_power_loss_at_each_publish_step_leaves_a_state_the_next_render_completes() {
    for loss in [
        PowerLoss::Kept,
        PowerLoss::RenamesLost,
        PowerLoss::SomeRenamesLost,
        PowerLoss::TornTemporaries,
    ] {
        let mut at = 0;
        loop {
            let directory = tempfile::tempdir().unwrap();
            let (mut store, case_id) = workspace(directory.path());
            human::seed(&vault(&store));
            let seen = human::fingerprint(&vault(&store));
            render(&mut store, true).unwrap();
            let earlier = owned_files(&vault(&store));
            question(&mut store, "q3", case_id);
            let plan = current_plan(&mut store);
            // Temporaries carry a tag from the generation they were written for.
            let tag = plan.generation().id()["gen_".len()..][..12].to_owned();
            if !render_killed_after(&mut store, at) {
                assert!(at >= 6, "three notes, overview, Start Here, one retired");
                break;
            }
            let root = vault(&store);
            let killed = owned_files(&root);
            // The changes after the barrier: paths whose bytes differ from
            // the earlier render's, and paths it held that are gone.
            let mut changed: Vec<&PathBuf> = killed
                .iter()
                .filter(|(path, bytes)| earlier.get(*path) != Some(*bytes))
                .map(|(path, _)| path)
                .chain(earlier.keys().filter(|path| !killed.contains_key(*path)))
                .collect();
            changed.sort();
            changed.dedup();
            let undo: Vec<&PathBuf> = match loss {
                PowerLoss::Kept => Vec::new(),
                PowerLoss::RenamesLost | PowerLoss::TornTemporaries => changed.clone(),
                PowerLoss::SomeRenamesLost => changed.iter().copied().step_by(2).collect(),
            };
            for path in &undo {
                let target = root.join(path);
                // A lost rename leaves the planned bytes in a temporary.
                if let Some(entry) = plan.entry(path.to_str().unwrap()) {
                    let name = target.file_name().unwrap().to_str().unwrap();
                    let temporary = target.with_file_name(format!(".{name}.{tag}-0.inv-tmp"));
                    fs::write(&temporary, entry.content()).unwrap();
                }
                match earlier.get(*path) {
                    Some(bytes) => fs::write(&target, bytes).unwrap(),
                    None => fs::remove_file(&target).unwrap(),
                }
            }
            // A removal moved the retired file aside first; when the removal
            // is lost, so is the move.
            for path in &undo {
                if killed.contains_key(*path) {
                    continue;
                }
                let target = root.join(path);
                let name = target.file_name().unwrap().to_str().unwrap().to_owned();
                for entry in fs::read_dir(target.parent().unwrap()).unwrap() {
                    let aside = entry.unwrap().path();
                    let file = aside.file_name().unwrap().to_string_lossy().into_owned();
                    if file.starts_with(&format!(".{name}.")) && file.ends_with(".inv-del") {
                        fs::remove_file(aside).unwrap();
                    }
                }
            }
            if matches!(loss, PowerLoss::TornTemporaries) {
                for path in files(&root) {
                    if !in_human_area(&path) && path.to_string_lossy().ends_with(".inv-tmp") {
                        let bytes = fs::read(root.join(&path)).unwrap();
                        fs::write(root.join(&path), &bytes[..bytes.len() / 2]).unwrap();
                    }
                }
            }
            for (path, bytes) in owned_files(&root) {
                let planned = plan
                    .entry(path.to_str().unwrap())
                    .map(|entry| entry.content());
                assert!(
                    Some(bytes.as_slice()) == planned || earlier.get(&path) == Some(&bytes),
                    "{loss:?} after {at}: {} holds neither its earlier nor its planned bytes",
                    path.display()
                );
            }

            let report = render(&mut store, false).unwrap();
            assert_complete(&mut store, &report);
            assert!(
                report.drift.is_empty(),
                "{loss:?} after {at}: {:?}",
                report.drift
            );
            assert!(report.adopted.is_empty(), "{loss:?} after {at}");
            assert_eq!(
                human::fingerprint(&vault(&store)),
                seen,
                "{loss:?} after {at}"
            );
            at += 1;
        }
    }
}

/// The generated files under `root` (not in the human-owned folders, not
/// hidden leftovers), by vault-relative path.
fn owned_files(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    files(root)
        .into_iter()
        .filter(|path| !in_human_area(path))
        .filter(|path| {
            !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .map(|path| {
            let bytes = fs::read(root.join(&path)).unwrap();
            (path, bytes)
        })
        .collect()
}
