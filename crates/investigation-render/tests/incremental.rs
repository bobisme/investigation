//! Incremental render (W08.15): a commit rewrites only the files whose
//! content changed, an incrementally maintained vault equals a clean render
//! up to the earlier basis an unchanged note keeps (and is byte-identical to
//! the clean render at that basis), and drift is diagnosed the same way in
//! both publish modes.

#[path = "support/vault.rs"]
mod checks;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    time::Instant,
};

use investigation_core::{identity::Id, wire::Snapshot};
use investigation_render::{
    Action, CaseRenderer, Drift, Generation, ManifestStatus, NoteMarker, PlanBuilder, PublishMode,
    RENDER_MANIFEST, RenderError, RenderPlan, RenderReport, VaultRenderer, VerifiedSnapshot,
    ViewPolicy, incremental::rebase, layout, render_vault, render_vault_with, vault_status,
};
use investigation_store::{Store, Workspace, journal::hash_bytes};
use serde_json::{Value, json};

/// A workspace and the records committed to it so far.
struct Casebook {
    store: Store,
    cases: Vec<Id>,
    questions: Vec<(Id, Id)>,
    /// Case, question and hypothesis IDs.
    hypotheses: Vec<(Id, Id, Id)>,
    commands: usize,
}

impl Casebook {
    fn new(directory: &Path) -> Self {
        let workspace = Workspace::init(&directory.join("workspace"), true).unwrap();
        Self {
            store: Store::open(workspace),
            cases: Vec::new(),
            questions: Vec::new(),
            hypotheses: Vec::new(),
            commands: 0,
        }
    }

    fn submit(&mut self, case_id: Option<Id>, operations: Vec<Value>) -> Vec<Id> {
        self.submit_after(case_id, json!([]), operations)
    }

    fn submit_after(
        &mut self,
        case_id: Option<Id>,
        preconditions: Value,
        operations: Vec<Value>,
    ) -> Vec<Id> {
        self.commands += 1;
        let input = json!({
            "schema_version":"inv.command.v0-alpha",
            "command_id":format!("command-{}", self.commands),
            "case_id":case_id,
            "actor":"agent:incremental-test",
            "preconditions":preconditions,
            "operations":operations
        });
        let receipt = self
            .store
            .record(&serde_json::to_vec(&input).unwrap())
            .unwrap();
        receipt.created.iter().map(|created| created.id).collect()
    }

    fn case(&mut self, title: &str) -> Id {
        let id = self.submit(
            None,
            vec![json!({"op":"case.create", "payload":{"title":title, "mode":"record"}})],
        )[0];
        self.cases.push(id);
        id
    }

    fn question(&mut self, case_id: Id, wording: &str) -> Id {
        let id = self.submit(Some(case_id), vec![question(wording)])[0];
        self.questions.push((case_id, id));
        id
    }

    fn hypothesis(&mut self, case_id: Id, question_id: Id, statement: &str) -> Id {
        let id = self.submit(Some(case_id), vec![hypothesis(question_id, statement)])[0];
        self.hypotheses.push((case_id, question_id, id));
        id
    }

    fn amend_hypothesis(&mut self, index: usize, statement: &str) {
        let (case_id, question_id, hypothesis_id) = self.hypotheses[index];
        let revision = self.store.cases().unwrap()[&case_id]
            .state
            .current(hypothesis_id)
            .unwrap()
            .header
            .revision
            .0;
        self.submit(
            Some(case_id),
            vec![json!({"op":"hypothesis.amend", "payload":{
                "hypothesis_ref":{"id":hypothesis_id, "revision":revision},
                "rationale":"Sharper wording",
                "replacement":{
                    "question_ref":{"id":question_id, "revision":1},
                    "statement":statement,
                    "possible_falsifiers":[], "rival_refs":[], "assumption_refs":[]
                }
            }})],
        );
    }

    fn retitle(&mut self, case_id: Id, title: &str) {
        let revision = self.store.cases().unwrap()[&case_id]
            .state
            .current(case_id)
            .unwrap()
            .header
            .revision
            .0;
        self.submit_after(
            Some(case_id),
            json!([{"id": case_id, "revision": revision}]),
            vec![json!({"op":"case.amend", "payload":{"title":title, "reason":"Sharper title"}})],
        );
    }

    fn root(&self) -> &Path {
        self.store.workspace().root()
    }

    fn snapshot(&mut self) -> VerifiedSnapshot {
        VerifiedSnapshot::read(&mut self.store).unwrap()
    }

    fn plan(&mut self) -> RenderPlan {
        VaultRenderer
            .render(&self.snapshot(), &ViewPolicy::default())
            .unwrap()
    }

    fn render(&mut self, mode: PublishMode) -> RenderReport {
        render_vault_with(
            &mut self.store,
            &VaultRenderer,
            &ViewPolicy::default(),
            mode,
            &mut |_| {},
        )
        .unwrap()
    }

    fn owned(&self) -> BTreeMap<String, Vec<u8>> {
        owned(&self.root().join("vault"))
    }
}

fn question(wording: &str) -> Value {
    json!({"op":"question.create", "payload":{"wording":wording, "importance":"normal"}})
}

fn hypothesis(question_id: Id, statement: &str) -> Value {
    json!({"op":"hypothesis.create", "payload":{
        "question_ref":{"id":question_id, "revision":1},
        "statement":statement,
        "possible_falsifiers":[], "rival_refs":[], "assumption_refs":[]
    }})
}

/// Every generated file under a vault folder by vault-relative path; the
/// human-owned and hidden folders hold none.
fn owned(vault: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let name = path.file_name().unwrap().to_str().unwrap();
            if name.starts_with('.') || (at == root && layout::is_human_root(name)) {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, found);
            } else {
                found.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(vault, vault, &mut found);
    found
}

fn copy_tree(from: &Path, to: &Path, skip: &[&str]) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap();
        if skip.iter().any(|skipped| name == *skipped) {
            continue;
        }
        let destination = to.join(name);
        if path.is_dir() {
            copy_tree(&path, &destination, &[]);
        } else {
            fs::copy(&path, &destination).unwrap();
        }
    }
}

/// The owned tree a render from scratch publishes for this workspace's
/// canonical state: the workspace without its vault or local files, copied
/// and rendered once.
fn clean_render(casebook: &Casebook, scratch: &Path) -> BTreeMap<String, Vec<u8>> {
    let target = scratch.join("clean");
    if target.exists() {
        fs::remove_dir_all(&target).unwrap();
    }
    copy_tree(casebook.root(), &target, &["vault", "local"]);
    for directory in ["vault/Notes", "vault/Boards", "vault/Inbox", "local"] {
        fs::create_dir_all(target.join(directory)).unwrap();
    }
    let mut store = Store::open(Workspace::open(&target).unwrap());
    let report = render_vault(
        &mut store,
        &VaultRenderer,
        &ViewPolicy::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(report.publish.refused(), 0);
    owned(&target.join("vault"))
}

/// The case head a note's bytes name, when they name one.
fn basis(bytes: &[u8]) -> Option<Snapshot> {
    match NoteMarker::read(bytes)? {
        NoteMarker::Case(head) => Some(head),
        NoteMarker::Generation(_) => None,
    }
}

/// The paths of `after` whose content differs from the vault `before`, by an
/// independent reading: absent before, or different bytes once the planned
/// note is restamped to the basis the file on disk names.
fn content_changed(before: &BTreeMap<String, Vec<u8>>, after: &RenderPlan) -> BTreeSet<String> {
    let snapshot_heads: BTreeMap<String, Snapshot> = after
        .generation()
        .heads()
        .iter()
        .map(|(case_id, head)| (case_id.to_string(), head.clone()))
        .collect();
    let mut changed = BTreeSet::new();
    for entry in after.entries() {
        let path = entry.path().as_str();
        let Some(old) = before.get(path) else {
            changed.insert(path.to_owned());
            continue;
        };
        if old.as_slice() == entry.content() {
            continue;
        }
        let head = path
            .strip_prefix("Cases/")
            .and_then(|rest| rest.split('/').next())
            .and_then(|case_id| snapshot_heads.get(case_id));
        let same = match (head, basis(old)) {
            (Some(head), Some(old_basis)) => {
                rebase(entry.content(), head, &old_basis).as_deref() == Some(old.as_slice())
            }
            _ => false,
        };
        if !same {
            changed.insert(path.to_owned());
        }
    }
    changed
}

fn written(report: &RenderReport) -> BTreeSet<String> {
    report
        .publish
        .outcomes
        .iter()
        .filter(|outcome| outcome.action == Action::Written)
        .map(|outcome| outcome.path.as_str().to_owned())
        .collect()
}

/// Render incrementally and check that exactly the files whose content
/// changed were written, every other file kept its bytes, and the vault
/// equals today's plan up to honest kept bases. Returns how many notes kept
/// an earlier basis.
fn render_and_check(casebook: &mut Casebook) -> usize {
    let before = casebook.owned();
    let plan = casebook.plan();
    let expected = content_changed(&before, &plan);
    let report = casebook.render(PublishMode::Incremental);
    assert_eq!(report.publish.refused(), 0);
    assert_eq!(written(&report), expected, "written = content changed");
    assert_eq!(
        report.publish.unchanged(),
        plan.entries().len() - expected.len()
    );
    let after = casebook.owned();
    let snapshot = casebook.snapshot();
    let mut kept = 0;
    for entry in plan.entries() {
        let path = entry.path().as_str();
        let bytes = &after[path];
        if !expected.contains(path) {
            assert_eq!(Some(bytes), before.get(path), "{path} kept its bytes");
        }
        match checks::up_to_basis(entry, bytes, &snapshot) {
            Ok(None) => {}
            Ok(Some(_)) => kept += 1,
            Err(problem) => panic!("{problem}"),
        }
    }
    assert_eq!(after.len(), plan.entries().len(), "only planned files");
    kept
}

#[test]
fn a_one_record_change_rewrites_only_the_files_whose_content_changed() {
    let directory = tempfile::tempdir().unwrap();
    let mut casebook = Casebook::new(directory.path());
    let first = casebook.case("first");
    let second = casebook.case("second");
    for n in 0..3 {
        let question = casebook.question(first, &format!("Question {n}"));
        for m in 0..2 {
            casebook.hypothesis(first, question, &format!("Hypothesis {m} of {n}"));
        }
    }
    casebook.question(second, "Other question");
    let report = casebook.render(PublishMode::Incremental);
    assert_eq!(report.publish.written(), report.publish.outcomes.len());
    // Nothing committed: nothing written.
    assert_eq!(
        casebook.render(PublishMode::Incremental).publish.written(),
        0
    );

    // One new hypothesis: its note, the notes that link to it or list it,
    // the case Overview and Start Here change; the rest keep their bytes.
    let question = casebook.questions[0].1;
    let added = casebook.hypothesis(first, question, "One more");
    let before = casebook.owned();
    let plan = casebook.plan();
    let changed = content_changed(&before, &plan);
    let kept = render_and_check(&mut casebook);
    assert!(changed.contains(&format!("Cases/{first}/Hypotheses/{added}.md")));
    assert!(changed.contains(&format!("Cases/{first}/Overview.md")));
    assert!(changed.contains("Start Here.md"));
    assert!(
        changed
            .iter()
            .all(|path| !path.contains(&second.to_string()))
    );
    let first_case_notes = plan
        .entries()
        .iter()
        .filter(|entry| entry.path().as_str().contains(&first.to_string()))
        .count();
    assert!(kept >= 4, "kept {kept}");
    assert!(changed.len() < first_case_notes, "{changed:?}");

    // Amending one hypothesis rewrites its note and the notes that show it.
    let (_, _, amended) = casebook.hypotheses[5];
    casebook.amend_hypothesis(5, "Sharper hypothesis");
    let before = casebook.owned();
    let changed = content_changed(&before, &casebook.plan());
    render_and_check(&mut casebook);
    assert!(changed.contains(&format!("Cases/{first}/Hypotheses/{amended}.md")));
    assert!(
        !changed.contains(&format!("Cases/{first}/Hypotheses/{added}.md")),
        "{changed:?}"
    );

    // A change every note of the case shows rewrites every one of them.
    casebook.retitle(first, "first, retitled");
    let before = casebook.owned();
    let plan = casebook.plan();
    let changed = content_changed(&before, &plan);
    assert_eq!(render_and_check(&mut casebook), 0, "no note kept its basis");
    assert_eq!(
        changed.len(),
        first_case_notes + 1,
        "every note and Start Here"
    );

    // Full mode restamps every kept note and then equals a clean render.
    let report = casebook.render(PublishMode::Full);
    assert_eq!(report.publish.refused(), 0);
    assert_eq!(
        casebook.owned(),
        clean_render(&casebook, directory.path()),
        "full = clean"
    );
}

/// A lost manifest is rebuilt by adoption: notes that kept an earlier basis
/// are adopted as they stand, like files holding the planned bytes, and
/// nothing is written or refused.
#[test]
fn a_lost_manifest_adopts_notes_that_kept_an_earlier_basis() {
    let directory = tempfile::tempdir().unwrap();
    let mut casebook = Casebook::new(directory.path());
    let case_id = casebook.case("lost");
    for n in 0..3 {
        casebook.question(case_id, &format!("Question {n}"));
    }
    casebook.render(PublishMode::Incremental);
    casebook.question(case_id, "One more");
    assert!(render_and_check(&mut casebook) > 0);
    let before = casebook.owned();
    fs::remove_file(casebook.root().join(RENDER_MANIFEST)).unwrap();
    let report = casebook.render(PublishMode::Incremental);
    assert_eq!(report.manifest, ManifestStatus::Missing);
    assert_eq!(report.publish.refused(), 0, "{:?}", report.publish.outcomes);
    assert_eq!(report.publish.written(), 0);
    assert_eq!(report.adopted.len(), before.len());
    assert_eq!(casebook.owned(), before);
    // The rebuilt record proves the kept bases again on the next commit.
    casebook.question(case_id, "And one more");
    assert!(render_and_check(&mut casebook) > 0);
}

/// The vault layout under another renderer version.
struct OtherRenderer;

impl CaseRenderer for OtherRenderer {
    fn render(
        &self,
        snapshot: &VerifiedSnapshot,
        policy: &ViewPolicy,
    ) -> Result<RenderPlan, RenderError> {
        let plan = VaultRenderer.render(snapshot, policy)?;
        let mut builder = PlanBuilder::new(Generation::new(
            "inv.vault-renderer.other",
            snapshot,
            policy,
        ));
        for entry in plan.entries() {
            builder.add(
                entry.phase(),
                entry.path().clone(),
                entry.content(),
                entry.projects(),
            )?;
        }
        Ok(builder.finish())
    }
}

/// A kept basis vouches only for bytes planned under the same renderer
/// version and view policy: after either changes, a commit restamps every
/// note of its case, and later commits keep bases again.
#[test]
fn a_new_renderer_or_policy_restamps_every_note_once() {
    let directory = tempfile::tempdir().unwrap();
    let mut casebook = Casebook::new(directory.path());
    let case_id = casebook.case("rules");
    for n in 0..3 {
        casebook.question(case_id, &format!("Question {n}"));
    }
    casebook.render(PublishMode::Incremental);
    let notes = casebook.plan().entries().len() - 1;
    let render = |casebook: &mut Casebook, renderer: &dyn Fn(&mut Store) -> RenderReport| {
        casebook.question(case_id, "One more");
        let report = renderer(&mut casebook.store);
        assert_eq!(report.publish.refused(), 0);
        report.publish.written()
    };
    let other_renderer = |store: &mut Store| {
        render_vault(store, &OtherRenderer, &ViewPolicy::default(), &mut |_| {}).unwrap()
    };
    let previews = ViewPolicy {
        derivative_previews: true,
        ..ViewPolicy::default()
    };
    let other_policy =
        |store: &mut Store| render_vault(store, &VaultRenderer, &previews, &mut |_| {}).unwrap();
    let ours = |store: &mut Store| {
        render_vault(store, &VaultRenderer, &ViewPolicy::default(), &mut |_| {}).unwrap()
    };
    // Every existing note, the new one and Start Here.
    assert_eq!(render(&mut casebook, &other_renderer), notes + 2);
    assert!(render(&mut casebook, &other_renderer) < notes);
    assert_eq!(render(&mut casebook, &ours), notes + 4);
    assert_eq!(render(&mut casebook, &other_policy), notes + 5);
    assert!(render(&mut casebook, &other_policy) < notes);
    assert_eq!(render(&mut casebook, &ours), notes + 7);
    assert!(render(&mut casebook, &ours) < notes);
}

/// A small deterministic generator, so every history is reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % bound as u64).unwrap()
    }
}

/// One random commit: a question, a hypothesis, an amendment, a retitled
/// case or, rarely, a new case.
fn random_commit(casebook: &mut Casebook, rng: &mut Rng, step: usize) {
    let case_id = casebook.cases[rng.below(casebook.cases.len())];
    match rng.below(10) {
        0..=2 => {
            casebook.question(case_id, &format!("Question at step {step}"));
        }
        3..=6 if !casebook.questions.is_empty() => {
            let (case_id, question_id) = casebook.questions[rng.below(casebook.questions.len())];
            casebook.hypothesis(case_id, question_id, &format!("Hypothesis at step {step}"));
        }
        7 | 8 if !casebook.hypotheses.is_empty() => {
            let index = rng.below(casebook.hypotheses.len());
            casebook.amend_hypothesis(index, &format!("Amended at step {step}"));
        }
        9 if casebook.cases.len() < 3 && rng.below(2) == 0 => {
            casebook.case(&format!("Case at step {step}"));
        }
        _ => casebook.retitle(case_id, &format!("Retitled at step {step}")),
    }
}

/// For random commit histories with renders at random points, the
/// incremental vault after every render: was written exactly where content
/// changed; equals the clean render of the same snapshot byte for byte,
/// except that a note may name an earlier head of its case; and every such
/// note is byte-identical to the clean render at that head. Rendering it in
/// full mode then makes it byte-identical to a clean render.
#[test]
fn incremental_renders_equal_clean_renders_for_random_histories() {
    let (mut kept_total, mut rewritten_total) = (0, 0);
    for seed in 1..=12_u64 {
        let directory = tempfile::tempdir().unwrap();
        let mut casebook = Casebook::new(directory.path());
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let case_id = casebook.case("history");
        casebook.question(case_id, "Opening question");
        // Clean plans at each render point, by case head, for the basis check.
        let mut clean_at: BTreeMap<(u64, String), BTreeMap<String, Vec<u8>>> = BTreeMap::new();
        for step in 0..10 {
            for _ in 0..=rng.below(2) {
                random_commit(&mut casebook, &mut rng, step);
            }
            if rng.below(4) == 0 {
                continue;
            }
            let before = casebook.owned();
            let plan = casebook.plan();
            rewritten_total += content_changed(&before, &plan).len();
            kept_total += render_and_check(&mut casebook);
            for (_, head) in plan.generation().heads() {
                let files = plan
                    .entries()
                    .iter()
                    .map(|entry| (entry.path().as_str().to_owned(), entry.content().to_vec()))
                    .collect();
                clean_at
                    .entry((head.sequence, head.head_hash.as_str().to_owned()))
                    .or_insert(files);
            }
            // Every note that keeps an earlier basis holds the bytes the
            // clean render at that basis planned.
            for (path, bytes) in casebook.owned() {
                let Some(basis) = basis(&bytes) else { continue };
                let at_basis = clean_at
                    .get(&(basis.sequence, basis.head_hash.as_str().to_owned()))
                    .unwrap_or_else(|| panic!("seed {seed}: {path} names an unseen head"));
                assert_eq!(at_basis.get(&path), Some(&bytes), "seed {seed}: {path}");
            }
            // The clean render is the plan, byte for byte.
            let clean = clean_render(&casebook, directory.path());
            let planned: BTreeMap<String, Vec<u8>> = plan
                .entries()
                .iter()
                .map(|entry| (entry.path().as_str().to_owned(), entry.content().to_vec()))
                .collect();
            assert_eq!(clean, planned, "seed {seed}: clean render = plan");
        }
        casebook.render(PublishMode::Full);
        assert_eq!(
            casebook.owned(),
            clean_render(&casebook, directory.path()),
            "seed {seed}: full = clean"
        );
    }
    // Neither half of the property is vacuous.
    assert!(kept_total > 20, "kept {kept_total}");
    assert!(rewritten_total > 20, "rewritten {rewritten_total}");
}

/// A drift record without the version a render withheld, which depends on
/// what the mode would have written.
fn diagnosis(drift: &Drift) -> String {
    format!(
        "{} {} expected {} observed {:?} projects {:?} renamed to {:?} copies {:?} since {}",
        drift.path,
        drift.kind.as_str(),
        drift.expected.as_str(),
        drift.observed.as_ref().map(|digest| digest.as_str()),
        drift.projects,
        drift.renamed_to,
        drift.copies,
        drift.since
    )
}

/// The same vault with the same edits, rendered in each mode: the same
/// paths drift with the same kind, owned and observed hashes, moves and
/// copies; every edited, moved or copied file is left exactly as found; each
/// withheld version is what that mode would have published; and the rest of
/// the two vaults agree up to kept bases.
#[test]
fn drift_detection_is_the_same_in_both_modes() {
    let directory = tempfile::tempdir().unwrap();
    let mut casebook = Casebook::new(directory.path());
    let case_id = casebook.case("drift");
    let questions: Vec<Id> = (0..4)
        .map(|n| casebook.question(case_id, &format!("Question {n}")))
        .collect();
    let hypotheses: Vec<Id> = questions
        .iter()
        .map(|question| casebook.hypothesis(case_id, *question, "A hypothesis"))
        .collect();
    casebook.render(PublishMode::Incremental);
    // A commit that changes the note of question 0 (it gains a hypothesis)
    // and leaves the notes of questions 1 to 3 unchanged.
    casebook.hypothesis(case_id, questions[0], "Another hypothesis");
    let vault = casebook.root().join("vault");
    let note = |kind: &str, id: Id| format!("Cases/{case_id}/{kind}/{id}.md");
    let changed_note = note("Questions", questions[0]);
    let before = casebook.owned();
    let changed_now = content_changed(&before, &casebook.plan());
    assert!(changed_now.contains(&changed_note));

    // Edits: a body edit to an unchanged note and to the changed one, a
    // property edit, a deletion, a move into Notes/, and conflict copies
    // beside an unchanged and the changed note.
    let edited_unchanged = note("Questions", questions[1]);
    assert!(!changed_now.contains(&edited_unchanged));
    let property = note("Hypotheses", hypotheses[1]);
    let deleted = note("Questions", questions[2]);
    let moved = note("Hypotheses", hypotheses[2]);
    let copied_unchanged = note("Questions", questions[3]);
    let copied_changed = format!("Cases/{case_id}/Overview.md");
    assert!(changed_now.contains(&copied_changed));
    assert!(!changed_now.contains(&copied_unchanged));
    let mut found: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for path in [&edited_unchanged, &changed_note] {
        let mut bytes = fs::read(vault.join(path)).unwrap();
        bytes.extend_from_slice(b"\nMy edit.\n");
        fs::write(vault.join(path), &bytes).unwrap();
        found.insert(path.clone(), bytes);
    }
    let text = fs::read_to_string(vault.join(&property)).unwrap();
    let edited = text.replace("inv_revision: 1", "inv_revision: 7");
    assert_ne!(edited, text);
    fs::write(vault.join(&property), &edited).unwrap();
    found.insert(property.clone(), edited.into_bytes());
    fs::remove_file(vault.join(&deleted)).unwrap();
    fs::rename(vault.join(&moved), vault.join("Notes/moved.md")).unwrap();
    found.insert("Notes/moved.md".to_owned(), before[&moved].clone());
    for path in [&copied_unchanged, &copied_changed] {
        let copy = path.replace(".md", " (conflicted copy).md");
        fs::write(vault.join(&copy), b"the other device\n").unwrap();
        found.insert(copy, b"the other device\n".to_vec());
        found.insert(path.clone(), before[path].clone());
    }

    // Two identical workspaces, one rendered in each mode.
    let twin = directory.path().join("twin");
    copy_tree(casebook.root(), &twin, &[]);
    let mut full_store = Store::open(Workspace::open(&twin).unwrap());
    let incremental = casebook.render(PublishMode::Incremental);
    let full = render_vault_with(
        &mut full_store,
        &VaultRenderer,
        &ViewPolicy::default(),
        PublishMode::Full,
        &mut |_| {},
    )
    .unwrap();

    let diagnosed =
        |report: &RenderReport| -> Vec<_> { report.drift.iter().map(diagnosis).collect() };
    assert_eq!(diagnosed(&incremental), diagnosed(&full));
    let kinds: BTreeSet<&str> = incremental
        .drift
        .iter()
        .map(|drift| drift.kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        BTreeSet::from(["conflict_copy", "edited", "missing", "renamed"])
    );
    assert_eq!(incremental.drift.len(), 7);
    assert_eq!(
        vault_status(casebook.store.workspace())
            .unwrap()
            .drift
            .iter()
            .map(diagnosis)
            .collect::<Vec<_>>(),
        vault_status(full_store.workspace())
            .unwrap()
            .drift
            .iter()
            .map(diagnosis)
            .collect::<Vec<_>>(),
    );

    // Everything a person or a sync client left is exactly as found.
    for root in [casebook.root(), twin.as_path()] {
        for (path, bytes) in &found {
            assert_eq!(
                &fs::read(root.join("vault").join(path)).unwrap(),
                bytes,
                "{path}"
            );
        }
        assert!(!root.join("vault").join(&deleted).exists());
        assert!(!root.join("vault").join(&moved).exists());
    }

    // Each withheld version is what its mode would have published: the plan
    // in full mode, the plan or its kept-basis twin in incremental mode.
    let plan = casebook.plan();
    let snapshot = casebook.snapshot();
    for (root, report, mode) in [
        (casebook.root(), &incremental, PublishMode::Incremental),
        (twin.as_path(), &full, PublishMode::Full),
    ] {
        for drift in &report.drift {
            let entry = plan.entry(drift.path.as_str()).unwrap();
            let Some(conflict) = &drift.conflict else {
                assert_eq!(drift.generated, None);
                continue;
            };
            let retained = fs::read(root.join(conflict)).unwrap();
            assert_eq!(drift.generated.as_ref(), Some(&hash_bytes(&retained)));
            let kept = checks::up_to_basis(entry, &retained, &snapshot).unwrap();
            if mode == PublishMode::Full {
                assert_eq!(kept, None, "{}", drift.path);
            }
        }
    }
    // A conflict copy beside an unchanged note holds nothing back in
    // incremental mode, since nothing would be written there.
    let held = |report: &RenderReport, path: &str| {
        report
            .publish
            .outcomes
            .iter()
            .find(|outcome| outcome.path.as_str() == path)
            .unwrap()
            .action
    };
    assert_eq!(held(&incremental, &copied_unchanged), Action::Unchanged);
    assert_ne!(held(&full, &copied_unchanged), Action::Unchanged);
    for path in [
        &edited_unchanged,
        &changed_note,
        &property,
        &deleted,
        &moved,
        &copied_changed,
    ] {
        assert_eq!(held(&incremental, path), held(&full, path), "{path}");
    }

    // The files that did not drift agree up to kept bases.
    let drifted: BTreeSet<String> = incremental
        .drift
        .iter()
        .map(|drift| drift.path.as_str().to_owned())
        .collect();
    let full_tree = owned(&twin.join("vault"));
    let incremental_tree = casebook.owned();
    // Both announce the same drift on the entry note.
    assert_eq!(
        full_tree["Start Here.md"],
        incremental_tree["Start Here.md"]
    );
    for (path, bytes) in &incremental_tree {
        let Some(entry) = plan.entry(path) else {
            continue;
        };
        if drifted.contains(path) || path == "Start Here.md" {
            continue;
        }
        assert_eq!(full_tree[path], entry.content(), "{path}");
        checks::up_to_basis(entry, bytes, &snapshot).unwrap();
    }
}

/// A one-record commit on a case of 10,001 records, rendered in each mode,
/// with the plan timed on its own. Run by hand:
/// `cargo test --release -p investigation-render --test incremental -- --ignored --nocapture`.
#[test]
#[ignore = "timing; run by hand in release mode"]
fn render_time_on_a_large_case() {
    let directory = tempfile::tempdir().unwrap();
    let mut casebook = Casebook::new(directory.path());
    let case_id = casebook.case("large");
    let started = Instant::now();
    let mut question_ids = Vec::new();
    for start in (0..2_000).step_by(120) {
        let operations = (start..2_000.min(start + 120))
            .map(|n| question(&format!("Question {n}")))
            .collect();
        question_ids.extend(casebook.submit(Some(case_id), operations));
    }
    let pairs: Vec<(Id, usize)> = question_ids
        .iter()
        .flat_map(|id| (0..4).map(move |n| (*id, n)))
        .collect();
    for chunk in pairs.chunks(120) {
        let operations = chunk
            .iter()
            .map(|(id, n)| hypothesis(*id, &format!("Hypothesis {n} of {id}")))
            .collect();
        casebook.submit(Some(case_id), operations);
    }
    eprintln!("built 10,001 records in {:?}", started.elapsed());

    let started = Instant::now();
    let plan = casebook.plan();
    eprintln!(
        "plan alone: {:?} ({} files)",
        started.elapsed(),
        plan.entries().len()
    );
    let timed = |label: &str, casebook: &mut Casebook, mode: PublishMode| {
        let started = Instant::now();
        let report = casebook.render(mode);
        eprintln!(
            "{label}: {:?}, written {}, unchanged {}",
            started.elapsed(),
            report.publish.written(),
            report.publish.unchanged()
        );
    };
    timed("first render", &mut casebook, PublishMode::Incremental);
    timed("no commit", &mut casebook, PublishMode::Incremental);
    casebook.submit(Some(case_id), vec![question("One more question")]);
    timed("one-record commit, full", &mut casebook, PublishMode::Full);
    casebook.submit(Some(case_id), vec![question("And one more")]);
    timed(
        "one-record commit, incremental",
        &mut casebook,
        PublishMode::Incremental,
    );
}
