use std::{
    cell::Cell,
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    time::Duration,
};

use investigation_core::{
    identity::Id,
    wire::{CommitResult, Receipt},
};
use investigation_render::{
    Action, Audience, CaseRenderer, Generation, NoManifest, Phase, PlanBuilder, PlanPath,
    Publisher, RENDERER_VERSION, Refusal, RenderError, RenderLock, RenderPlan, VaultRenderer,
    VerifiedSnapshot, ViewPolicy, render, render_vault,
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

fn submit(store: &mut Store, key: &str, case_id: Option<Id>, operation: Value) -> Receipt {
    let input = json!({
        "schema_version":"inv.command.v0-alpha",
        "command_id":key,
        "case_id":case_id,
        "actor":"agent:render-test",
        "preconditions":[],
        "operations":[operation]
    });
    store.record(&serde_json::to_vec(&input).unwrap()).unwrap()
}

fn question(store: &mut Store, key: &str, case_id: Id, wording: &str) -> Receipt {
    submit(
        store,
        key,
        Some(case_id),
        json!({"op":"question.create", "payload":{"wording":wording, "importance":"normal"}}),
    )
}

/// A vault-enabled workspace with one case and two questions.
fn workspace(directory: &Path) -> (Store, Id) {
    let workspace = Workspace::init(&directory.join("workspace"), true).unwrap();
    let mut store = Store::open(workspace);
    let genesis = submit(
        &mut store,
        "case",
        None,
        json!({"op":"case.create", "payload":{"title":"Render fixture", "mode":"record"}}),
    );
    let case_id = genesis.created[0].id;
    question(&mut store, "q1", case_id, "What changed first?");
    question(&mut store, "q2", case_id, "Which caveat remains open?");
    (store, case_id)
}

fn reopen(store: &Store) -> Store {
    Store::open(Workspace::open(store.workspace().root()).unwrap())
}

fn vault(store: &Store) -> PathBuf {
    store.workspace().root().join("vault")
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn publish(store: &Store, plan: &RenderPlan) -> investigation_render::PublishReport {
    let lock = RenderLock::acquire(store.workspace(), Duration::ZERO).unwrap();
    Publisher::open(store.workspace(), &lock)
        .unwrap()
        .publish(plan, &NoManifest, &mut |_| {})
        .unwrap()
}

#[test]
fn same_snapshot_and_policy_yield_a_byte_identical_plan() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    let policy = ViewPolicy::default();

    let first = render(&VerifiedSnapshot::read(&mut store).unwrap(), &policy).unwrap();
    let mut other = reopen(&store);
    let snapshot = VerifiedSnapshot::read(&mut other).unwrap();
    let second = VaultRenderer.render(&snapshot, &policy).unwrap();
    let third = render(&snapshot, &policy).unwrap();
    assert_eq!(first, second);
    assert_eq!(second, third);
    for (left, right) in first.entries().iter().zip(third.entries()) {
        assert_eq!(left.path(), right.path());
        assert_eq!(left.content(), right.content());
    }

    // Stable order: records, then views, then the entry note; paths sorted within a phase.
    let entries = first.entries();
    assert_eq!(entries.len(), 4, "two questions, one overview, Start Here");
    assert!(
        entries
            .windows(2)
            .all(|pair| { (pair[0].phase(), pair[0].path()) < (pair[1].phase(), pair[1].path()) })
    );
    assert_eq!(entries.last().unwrap().path().as_str(), "Start Here.md");
    assert_eq!(entries.last().unwrap().phase(), Phase::Entry);
    let overview = format!("Cases/{case_id}/Overview.md");
    assert_eq!(first.entry(&overview).unwrap().projects(), Some(case_id));
    for entry in entries {
        assert_eq!(
            entry.sha256(),
            &investigation_store::journal::hash_bytes(entry.content())
        );
    }
    assert_eq!(first.generation().renderer(), RENDERER_VERSION);
    assert_eq!(first.generation().heads().len(), 1);

    // Authored text never reaches the skeleton templates.
    for entry in entries {
        let text = std::str::from_utf8(entry.content()).unwrap();
        assert!(!text.contains("Render fixture") && !text.contains("What changed first"));
        assert!(text.contains("tool-owned projection"));
        assert!(text.contains(first.generation().id()));
    }

    // A different policy or a new commit is a different generation.
    let public = render(&snapshot, &ViewPolicy::for_audience(Audience::Public)).unwrap();
    assert_ne!(public.generation().id(), first.generation().id());
    question(&mut store, "q3", case_id, "What would refute it?");
    let later = render(&VerifiedSnapshot::read(&mut store).unwrap(), &policy).unwrap();
    assert_ne!(later.generation().id(), first.generation().id());
    assert_eq!(later.entries().len(), 5);
}

#[test]
fn planner_has_no_access_to_the_workspace() {
    // Planning takes only an in-memory snapshot and policy: there is no handle
    // through which it could write. Prove it with the workspace gone entirely.
    let directory = tempfile::tempdir().unwrap();
    let (mut store, _) = workspace(directory.path());
    let snapshot = VerifiedSnapshot::read(&mut store).unwrap();
    let policy = ViewPolicy::default();
    let before = render(&snapshot, &policy).unwrap();
    drop(store);
    let root = directory.path().join("workspace");
    fs::remove_dir_all(&root).unwrap();
    let after = render(&snapshot, &policy).unwrap();
    assert_eq!(before, after);
    assert!(!root.exists(), "planning recreated nothing on disk");
}

#[test]
fn a_commit_succeeds_while_a_render_is_in_progress() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    let mut writer = reopen(&store);
    let committed = Cell::new(None);
    let mut observe = |step: &investigation_render::PublishStep<'_>| {
        if step.index != 0 {
            return;
        }
        // Mid-render: a second render is refused, a commit is not.
        let workspace = Workspace::open(writer.workspace().root()).unwrap();
        assert!(matches!(
            RenderLock::acquire(&workspace, Duration::ZERO),
            Err(RenderError::Busy { .. })
        ));
        let receipt = question(&mut writer, "during-render", case_id, "Asked mid-render?");
        assert_eq!(receipt.result, CommitResult::Committed);
        committed.set(Some(receipt.snapshot.sequence));
    };
    let report = render_vault(
        &mut store,
        &VaultRenderer,
        &ViewPolicy::default(),
        &mut observe,
    )
    .unwrap()
    .publish;
    assert_eq!(committed.get(), Some(4));
    assert_eq!(report.written(), 4);
    assert_eq!(report.refused(), 0);
    // The render published the snapshot it read; the next one sees the commit.
    assert_eq!(report.generation.heads()[0].1.sequence, 3);
    let next = render(
        &VerifiedSnapshot::read(&mut store).unwrap(),
        &ViewPolicy::default(),
    )
    .unwrap();
    assert_eq!(next.generation().heads()[0].1.sequence, 4);
    // The lock is released once the render returns.
    drop(RenderLock::acquire(store.workspace(), Duration::ZERO).unwrap());
}

#[test]
fn publishing_writes_the_plan_and_rerendering_changes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, _) = workspace(directory.path());
    let human = vault(&store).join("Notes/mine.md");
    fs::write(&human, "my own words\n").unwrap();
    let plan = render(
        &VerifiedSnapshot::read(&mut store).unwrap(),
        &ViewPolicy::default(),
    )
    .unwrap();

    let first = publish(&store, &plan);
    assert_eq!(first.written(), plan.entries().len());
    for entry in plan.entries() {
        let bytes = fs::read(vault(&store).join(entry.path().as_str())).unwrap();
        assert_eq!(bytes, entry.content());
    }
    let second = publish(&store, &plan);
    assert_eq!(second.written(), 0);
    assert_eq!(second.unchanged(), plan.entries().len());

    assert_eq!(fs::read_to_string(&human).unwrap(), "my own words\n");
    let leftovers: Vec<_> = files_under(&vault(&store))
        .into_iter()
        .filter(|path| path.to_string_lossy().contains("inv-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn publisher_never_overwrites_unowned_files_or_follows_symlinks() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = workspace(directory.path());
    let outside = directory.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let outside_file = directory.path().join("outside.md");
    fs::write(&outside_file, "outside\n").unwrap();

    // A foreign file at a planned path, a symlink at another, and a symlinked
    // parent directory that points out of the vault.
    let vault = vault(&store);
    fs::write(vault.join("Start Here.md"), "a human wrote this\n").unwrap();
    symlink(&outside, vault.join("Cases")).unwrap();
    let plan = render(
        &VerifiedSnapshot::read(&mut store).unwrap(),
        &ViewPolicy::default(),
    )
    .unwrap();
    let report = publish(&store, &plan);
    assert_eq!(report.written(), 0);
    for outcome in &report.outcomes {
        let expected = if outcome.path.as_str() == "Start Here.md" {
            Refusal::NotProvenOwned
        } else {
            Refusal::UnsafeParent
        };
        assert_eq!(
            outcome.action,
            Action::Refused(expected),
            "{}",
            outcome.path
        );
    }
    assert_eq!(
        fs::read_to_string(vault.join("Start Here.md")).unwrap(),
        "a human wrote this\n"
    );
    assert!(fs::read_dir(&outside).unwrap().next().is_none());

    // A symlink at the file path itself is never written through.
    fs::remove_file(vault.join("Cases")).unwrap();
    fs::create_dir_all(vault.join(format!("Cases/{case_id}"))).unwrap();
    let overview = format!("Cases/{case_id}/Overview.md");
    symlink(&outside_file, vault.join(&overview)).unwrap();
    let report = publish(&store, &plan);
    let outcome = report
        .outcomes
        .iter()
        .find(|outcome| outcome.path.as_str() == overview)
        .unwrap();
    assert_eq!(outcome.action, Action::Refused(Refusal::NotARegularFile));
    assert_eq!(fs::read_to_string(&outside_file).unwrap(), "outside\n");
    assert!(
        fs::symlink_metadata(vault.join(&overview))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(report.written(), 2, "the two question notes");
}

#[test]
fn publisher_refuses_human_owned_roots_and_unsafe_vaults() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, _) = workspace(directory.path());
    let snapshot = VerifiedSnapshot::read(&mut store).unwrap();
    let policy = ViewPolicy::default();
    let mut builder = PlanBuilder::new(Generation::new("test", &snapshot, &policy));
    builder
        .add(
            Phase::Records,
            PlanPath::new("Notes/generated.md").unwrap(),
            "x",
            None,
        )
        .unwrap();
    assert!(matches!(
        builder.add(
            Phase::Views,
            PlanPath::new("notes/GENERATED.md").unwrap(),
            "y",
            None
        ),
        Err(RenderError::DuplicatePath(_))
    ));
    let report = publish(&store, &builder.finish());
    assert_eq!(
        report.outcomes[0].action,
        Action::Refused(Refusal::HumanOwned)
    );
    assert!(!vault(&store).join("Notes/generated.md").exists());

    // A lock from another workspace cannot authorize publication.
    let (other, _) = workspace(&directory.path().join("other"));
    let lock = RenderLock::acquire(other.workspace(), Duration::ZERO).unwrap();
    assert!(matches!(
        Publisher::open(store.workspace(), &lock),
        Err(RenderError::WrongWorkspace)
    ));
    drop(lock);

    // The vault itself must be a real directory.
    let vault = vault(&store);
    fs::rename(&vault, directory.path().join("moved-vault")).unwrap();
    symlink(directory.path().join("moved-vault"), &vault).unwrap();
    let lock = RenderLock::acquire(store.workspace(), Duration::ZERO).unwrap();
    assert!(matches!(
        Publisher::open(store.workspace(), &lock),
        Err(RenderError::UnsafeDirectory("vault"))
    ));

    // No vault, no render.
    let plain = Workspace::init(&directory.path().join("plain"), false).unwrap();
    let lock = RenderLock::acquire(&plain, Duration::ZERO).unwrap();
    assert!(matches!(
        Publisher::open(&plain, &lock),
        Err(RenderError::VaultDisabled)
    ));
}

/// Writes the target itself between the publisher's inspection and its write,
/// like an editor or sync client racing the render.
struct RacingWriter(PathBuf);

impl investigation_render::Ownership for RacingWriter {
    fn decide(
        &self,
        entry: &investigation_render::PlanEntry,
        on_disk: &investigation_render::OnDisk,
    ) -> investigation_render::Decision {
        assert_eq!(on_disk, &investigation_render::OnDisk::Absent);
        let target = self.0.join(entry.path().as_str());
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, "written by a racing editor\n").unwrap();
        investigation_render::Decision::Write
    }
}

#[test]
fn a_file_created_during_publication_is_not_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, _) = workspace(directory.path());
    let plan = render(
        &VerifiedSnapshot::read(&mut store).unwrap(),
        &ViewPolicy::default(),
    )
    .unwrap();
    let lock = RenderLock::acquire(store.workspace(), Duration::ZERO).unwrap();
    let report = Publisher::open(store.workspace(), &lock)
        .unwrap()
        .publish(&plan, &RacingWriter(vault(&store)), &mut |_| {})
        .unwrap();
    assert_eq!(report.refused(), plan.entries().len());
    for entry in plan.entries() {
        assert_eq!(
            fs::read_to_string(vault(&store).join(entry.path().as_str())).unwrap(),
            "written by a racing editor\n"
        );
    }
    assert!(
        files_under(&vault(&store))
            .iter()
            .all(|path| !path.to_string_lossy().contains("inv-tmp"))
    );
}
