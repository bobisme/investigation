//! Render generations: every note names the snapshot it reflects, the
//! manifest advertises `updating`/`partial`/`complete`, and a render killed
//! at any file leaves a state from which the next render completes.

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
    PlanPath, RENDERER_VERSION, Refusal, RenderError, RenderLock, RenderPlan, RenderReport,
    RenderState, SweptAction, VaultRenderer, VaultState, VerifiedSnapshot, ViewPolicy,
    render_vault, vault_status,
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

/// The vault is exactly today's plan, the manifest is complete and current,
/// and nothing a crash left behind remains.
fn assert_complete(store: &mut Store, report: &RenderReport) {
    assert_eq!(report.publish.refused(), 0, "{:?}", report.publish.outcomes);
    assert!(report.unknown.is_empty(), "{:?}", report.unknown);
    let plan = current_plan(store);
    assert_eq!(report.publish.generation.id(), plan.generation().id());
    let on_disk = files(&vault(store));
    let planned: Vec<PathBuf> = plan
        .entries()
        .iter()
        .map(|entry| PathBuf::from(entry.path().as_str()))
        .collect();
    let mut sorted = planned.clone();
    sorted.sort();
    assert_eq!(on_disk, sorted, "only planned files, no leftovers");
    for entry in plan.entries() {
        let bytes = fs::read(vault(store).join(entry.path().as_str())).unwrap();
        assert_eq!(bytes, entry.content(), "{}", entry.path());
    }
    let manifest = manifest(store);
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

    // A commit to the second case changes only its notes and Start Here.
    question(&mut store, "q4", second);
    let after = current_plan(&mut store);
    assert_ne!(after.generation().id(), before.generation().id());
    for entry in before.entries() {
        let path = entry.path().as_str();
        let later = after.entry(path).unwrap();
        let unchanged = path.starts_with(&format!("Cases/{first}/"));
        assert_eq!(unchanged, later.content() == entry.content(), "{path}");
    }
    let report = render(&mut store, false).unwrap();
    for entry in before.entries() {
        let path = entry.path().as_str();
        let written = report
            .publish
            .outcomes
            .iter()
            .find(|outcome| outcome.path.as_str() == path)
            .unwrap()
            .action;
        let expected = if path.starts_with(&format!("Cases/{first}/")) {
            Action::Unchanged
        } else {
            Action::Written
        };
        assert_eq!(written, expected, "{path}");
    }
}

/// Kill the render after each file in turn. Whatever is on disk, each note
/// still names the snapshot it reflects, `vault status` reports an
/// interrupted partial render with the last complete generation, and the
/// next render completes: at the same snapshot, and after further commits
/// that change every file the killed render wrote.
#[test]
fn killing_a_render_at_each_file_leaves_a_state_the_next_render_completes() {
    for moved_on in [false, true] {
        let mut at = 0;
        loop {
            let directory = tempfile::tempdir().unwrap();
            let (mut store, case_id) = workspace(directory.path());
            let first = render(&mut store, true).unwrap();
            let complete = first.publish.generation.clone();
            question(&mut store, "q3", case_id);
            let target = current_generation(&mut store);
            if !render_killed_after(&mut store, at) {
                assert!(at >= 6, "three notes, overview, Start Here, one retired");
                break;
            }

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

    let report = render(&mut store, false).unwrap();
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
