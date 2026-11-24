//! §20.1 Y28, export half: a restricted lesson never reaches a projection
//! rendered for an audience below it.
//!
//! One case records two Retrospectives through the real operations. The
//! first is cleared for the `public` audience by a reviewed
//! `classification.downgrade`; the second is raised to `restricted` by
//! `classification.raise` after its lesson was admitted and consulted by a
//! second case. Every string of the restricted account carries
//! [`RESTRICTED`] and every string of the public one [`PUBLIC`], so a byte
//! scan of the whole plan (paths and contents) finds any channel that
//! leaks: note bodies, titles, aliases, frontmatter, filenames, link labels,
//! the Views, the Overview and `Start Here.md` (this renderer plans no Canvas
//! or Bases file; `Boards/` is human-owned).
//!
//! The working vault keeps ID-only placeholders for withheld records for
//! every audience; the public review export (W11.16) names none of them.

mod support;

use investigation_core::{
    identity::Id,
    wire::{CommitResult, Receipt},
};
use investigation_render::{
    Audience, RenderPlan, VerifiedSnapshot, ViewPolicy,
    export::{ExportSnapshot, Pin, kind, plan_export},
    layout, render,
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};
use support::vault;

const AUTHOR: &str = "agent:lesson-author";
const REVIEWER: &str = "human:lesson-reviewer";
/// Prefix of every string of the restricted Retrospective.
const RESTRICTED: &str = "ZEBRALESSON";
/// Prefix of every string of the Retrospective cleared for `public`.
const PUBLIC: &str = "PUFFINLESSON";

fn reference(id: Id, revision: u64) -> Value {
    json!({"id":id, "revision":revision})
}

fn submit_as(
    store: &mut Store,
    key: &str,
    actor: &str,
    descriptor: Option<Value>,
    case: Option<Id>,
    op: &str,
    payload: Value,
) -> Receipt {
    let mut input = json!({
        "schema_version":"inv.command.v1", "command_id":key, "case_id":case,
        "actor":actor, "preconditions":[], "operations":[{"op":op, "payload":payload}]
    });
    if let Some(descriptor) = descriptor {
        input["actor_descriptor"] = descriptor;
    }
    let receipt = store
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap_or_else(|failure| panic!("{key}: {failure:?}"));
    assert_eq!(
        receipt.result,
        CommitResult::Committed,
        "{key}: {receipt:?}"
    );
    receipt
}

fn create(store: &mut Store, key: &str, case: Option<Id>, op: &str, payload: Value) -> Id {
    submit_as(store, key, AUTHOR, None, case, op, payload).created[0].id
}

fn current(store: &mut Store, case: Id, id: Id) -> Value {
    let snapshot = VerifiedSnapshot::read(store).unwrap();
    let revision = snapshot
        .case(case)
        .unwrap()
        .state()
        .current(id)
        .unwrap()
        .header
        .revision
        .0;
    reference(id, revision)
}

/// A Retrospective of `case` at its current head whose every string starts
/// with `marker`.
fn retrospective(store: &mut Store, key: &str, case: Id, run: Id, marker: &str) -> Id {
    let replay = store.case(case).unwrap();
    let case_ref = reference(case, replay.state.current(case).unwrap().header.revision.0);
    let text = |field: &str| format!("{marker} {field}");
    create(
        store,
        key,
        Some(case),
        "retrospective.record",
        json!({
            "case_ref":case_ref, "snapshot":replay.snapshot,
            "methods_attempted":[{"name":text("method"), "inquiry_ref":null}],
            "effective_steps":[{"description":text("effective step"), "support_refs":[run]}],
            "invalid_or_misleading_steps":[{"description":text("misleading step"), "support_refs":[run]}],
            "none_observed_reason":null,
            "costs_or_constraints":text("costs"),
            "residual_unknowns":[text("unknown")],
            "lessons":[{"guidance":text("guidance"), "applies_when":text("applies when"),
                "support_refs":[run], "transfer_limits":text("transfer limits")}],
        }),
    )
}

fn admit(store: &mut Store, key: &str, case: Id, retrospective: Id) -> Id {
    submit_as(
        store,
        key,
        REVIEWER,
        Some(json!({"kind":"human"})),
        Some(case),
        "lesson.admit",
        json!({"retrospective_ref":reference(retrospective, 1), "lesson_index":0,
            "basis":{"kind":"human_review", "rationale":"Read the cited run; the limit is stated."}}),
    )
    .created[0]
        .id
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    source: Id,
    reader: Id,
    public: Id,
    /// The restricted Retrospective.
    secret: Id,
    /// Records the restricted lesson produced: the Retrospective, its
    /// admission and its classification change.
    secret_records: Vec<Id>,
    /// The restricted lesson's digest, as its admission bound it.
    secret_digest: String,
    /// The reading case's consultation of the restricted lesson.
    consultation: Id,
}

#[allow(clippy::too_many_lines)]
fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    Workspace::init(&root, true).unwrap();
    let mut owned = Store::open(Workspace::open(&root).unwrap());
    let store = &mut owned;
    let source = create(
        store,
        "source",
        None,
        "case.create",
        json!({"title":"Why does the stage stall?", "mode":"explain"}),
    );
    let reader = create(
        store,
        "reader",
        None,
        "case.create",
        json!({"title":"Why does the batch stall?", "mode":"explain"}),
    );
    let run = create(
        store,
        "run",
        Some(source),
        "run.record",
        json!({"intent":{"kind":"exploration", "intent":"Trace one burst."},
            "producer":"agent:producer", "artifact_refs":[], "execution_status":"failed",
            "evidence_completeness":"missing", "evidence_kind":"observational",
            "variables_changed":[]}),
    );

    // The account cleared for the public audience by a reviewed downgrade.
    let public = retrospective(store, "public-retro", source, run, PUBLIC);
    admit(store, "admit-public", source, public);
    let subject = current(store, source, public);
    submit_as(
        store,
        "clear-public",
        REVIEWER,
        Some(json!({"kind":"human"})),
        Some(source),
        "classification.downgrade",
        json!({
            "subject_ref":subject, "level":"public",
            "reason":"The account names no system, person or figure.",
            "transformation":{
                "inputs":[{"input_ref":subject, "digest":null}],
                "transformation":"manual_review", "version":"1",
                "redaction_decisions":[{"scope":"Whole account", "action":"retained",
                    "rationale":"Nothing in it is sensitive."}],
                "output_digest":null
            },
            "review":{"method":"Read the whole account.", "coverage":"whole_output",
                "examined":[subject]}
        }),
    );

    // The account raised to restricted after its lesson travelled.
    let secret = retrospective(store, "secret-retro", source, run, RESTRICTED);
    let admission = admit(store, "admit-secret", source, secret);
    let secret_digest = {
        let replay = store.case(source).unwrap();
        replay.state.current(admission).unwrap().body["lesson_admission"]["lesson"]["lesson_digest"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let consultation = create(
        store,
        "consult-secret",
        Some(reader),
        "lesson.consult",
        json!({"lesson":{"source_case_id":source, "retrospective_ref":reference(secret, 1),
            "lesson_index":0, "admission_ref":reference(admission, 1),
            "lesson_digest":secret_digest},
            "purpose":"Planning the tracer setup."}),
    );
    let raised = submit_as(
        store,
        "raise-secret",
        AUTHOR,
        None,
        Some(source),
        "classification.raise",
        json!({"subject_ref":reference(secret, 1), "level":"restricted",
            "reason":format!("{RESTRICTED} raise reason")}),
    )
    .created[0]
        .id;
    Fixture {
        _directory: directory,
        store: owned,
        source,
        reader,
        public,
        secret,
        secret_records: vec![secret, admission, raised],
        secret_digest,
        consultation,
    }
}

fn plan(fixture: &mut Fixture, audience: Audience) -> RenderPlan {
    let snapshot = VerifiedSnapshot::read(&mut fixture.store).unwrap();
    render(&snapshot, &ViewPolicy::for_audience(audience)).unwrap()
}

fn text(plan: &RenderPlan, path: &str) -> String {
    String::from_utf8(plan.entry(path).unwrap().content().to_vec()).unwrap()
}

/// Every planned byte that names `needle`, as `path: line`.
fn leaks(plan: &RenderPlan, needle: &str) -> Vec<String> {
    let mut found = Vec::new();
    for entry in plan.entries() {
        if entry.path().as_str().contains(needle) {
            found.push(format!("path {}", entry.path()));
        }
        let content = String::from_utf8_lossy(entry.content());
        for line in content.lines().filter(|line| line.contains(needle)) {
            found.push(format!("{}: {line}", entry.path()));
        }
    }
    found
}

#[test]
fn no_text_title_or_digest_of_a_restricted_lesson_reaches_a_lower_audience() {
    let mut fixture = fixture();
    for audience in [Audience::Public, Audience::Internal, Audience::Restricted] {
        let plan = plan(&mut fixture, audience);
        let restricted = leaks(&plan, RESTRICTED);
        let digest = leaks(&plan, &fixture.secret_digest);
        let public = leaks(&plan, PUBLIC);
        if audience == Audience::Restricted {
            // Not vacuous: the same plan shows the lesson to a cleared reader.
            assert!(!restricted.is_empty());
        } else {
            assert!(restricted.is_empty(), "{audience:?}: {restricted:#?}");
            assert!(digest.is_empty(), "{audience:?}: {digest:#?}");
        }
        // The public lesson is shown to every audience, as its own note.
        assert!(!public.is_empty(), "{audience:?}");
        let note = text(
            &plan,
            layout::record_note(fixture.source, fixture.public)
                .unwrap()
                .unwrap()
                .as_str(),
        );
        assert!(
            note.contains(&format!("- {PUBLIC} guidance (support ")),
            "{audience:?}\n{note}"
        );
        assert!(
            vault::property(&note, "inv_withheld").is_none(),
            "{audience:?}"
        );
        // The restricted account's own note, if any, is an ID-only placeholder.
        if audience != Audience::Restricted {
            for &id in &fixture.secret_records {
                let Some(path) = layout::record_note(fixture.source, id).unwrap() else {
                    continue;
                };
                if let Some(entry) = plan.entry(path.as_str()) {
                    let note = String::from_utf8_lossy(entry.content());
                    assert!(
                        vault::property(&note, "inv_withheld").is_some(),
                        "{id} {audience:?}\n{note}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_internal_working_vault_names_the_withheld_lesson_by_id_only() {
    let mut fixture = fixture();
    let plan = plan(&mut fixture, Audience::Internal);
    // The reading case's consultation, recorded before the raise, keeps its
    // pointer: the withheld records appear as bare IDs, never as links that
    // carry a title, and never with the lesson's digest.
    let path = layout::record_note(fixture.reader, fixture.consultation)
        .unwrap()
        .unwrap();
    let consultation = text(&plan, path.as_str());
    assert!(
        consultation.contains(&format!(
            "- `{}` (absent from this snapshot) in `lesson_consultation.lesson.retrospective_ref`",
            fixture.secret
        )),
        "{consultation}"
    );
    // The source case lists the withheld account by ID, as `withheld`.
    let view = text(
        &plan,
        &format!(
            "Cases/{}/Views/Retrospectives and Lessons.md",
            fixture.source
        ),
    );
    let row = view
        .lines()
        .find(|line| line.contains(&fixture.secret.to_string()))
        .unwrap_or_else(|| panic!("{view}"));
    assert!(row.ends_with("| withheld | withheld | withheld | withheld | withheld |"));
    assert!(view.contains(&format!("{PUBLIC} guidance")), "{view}");
}

/// §16.3: "the existence of an omission needs audience permission". The
/// working vault renders withheld records as ID-only placeholders for every
/// audience (`ViewPolicy::audience_policy` always sets `placeholders`), so a
/// public working vault still has the restricted Retrospective's note file,
/// Overview and Timeline entries and a `withheld` row in the Retrospectives
/// view. The review export is what leaves a case (W11.16): its public copy
/// names none of the restricted lesson's records, in any form, nor its
/// digest, and says nothing is withheld in the Retrospectives view.
#[test]
fn a_public_vault_does_not_reveal_that_a_restricted_lesson_exists() {
    let mut fixture = fixture();
    let snapshot =
        ExportSnapshot::read(&mut fixture.store, fixture.source, &Pin::default()).unwrap();
    let plan = plan_export(kind("review-vault").unwrap(), &snapshot, Audience::Public).unwrap();
    let identifiers = plan.identifiers();
    let files: Vec<(String, String)> = plan
        .files()
        .entries()
        .iter()
        .map(|entry| {
            (
                entry.path().as_str().to_owned(),
                String::from_utf8_lossy(entry.content()).into_owned(),
            )
        })
        .chain([(
            "export-manifest.json".to_owned(),
            String::from_utf8_lossy(plan.manifest_bytes()).into_owned(),
        )])
        .collect();
    let restore = |text: &str| String::from_utf8(identifiers.restore(text.as_bytes())).unwrap();
    for &id in &fixture.secret_records {
        let canonical = id.to_string();
        let uuid = canonical.split_once('_').unwrap().1.to_owned();
        let mut needles = vec![
            uuid.clone(),
            uuid.replace('-', ""),
            uuid.to_ascii_uppercase(),
        ];
        needles.extend(identifiers.exported(&canonical).map(str::to_owned));
        for (path, text) in &files {
            for needle in &needles {
                assert!(!path.contains(needle.as_str()), "{id}: {path}");
                assert!(!text.contains(needle.as_str()), "{id}: {path}");
            }
            assert!(!restore(text).contains(&uuid), "{id}: {path}");
        }
    }
    let digest = fixture.secret_digest.trim_start_matches("sha256:");
    for (path, text) in &files {
        assert!(!text.contains(RESTRICTED), "{path}");
        assert!(!text.contains(digest), "{path}");
    }
    let view = files
        .iter()
        .find(|(path, _)| path.ends_with("/Views/Retrospectives and Lessons.md"))
        .map(|(_, text)| text.as_str())
        .unwrap_or_default();
    assert!(!view.contains("withheld"), "{view}");
    // Not vacuous: the public account is there.
    assert!(view.contains(&format!("{PUBLIC} method")), "{view}");
    // Its steps and lesson cite the run, which is not cleared for the
    // public, so they are left out (bn-91gz).
    for (_, text) in &files {
        assert!(!text.contains(&format!("{PUBLIC} guidance")), "{text}");
        assert!(
            !text.contains(&format!("{PUBLIC} effective step")),
            "{text}"
        );
    }
    assert!(view.contains("none shown to this audience"), "{view}");
}

/// The reading case consulted the lesson before it was raised, so its
/// consultation still points at the restricted Retrospective and binds the
/// lesson's digest. That pointer lives in another case, whose own records
/// are internal: no export of the reading case below `restricted` may name
/// the restricted Retrospective, its admission or its digest.
#[test]
fn no_export_of_a_consulting_case_names_the_restricted_lesson() {
    let mut fixture = fixture();
    let snapshot =
        ExportSnapshot::read(&mut fixture.store, fixture.reader, &Pin::default()).unwrap();
    let digest = fixture
        .secret_digest
        .trim_start_matches("sha256:")
        .to_owned();
    let mut found = Vec::new();
    let mut named = 0;
    for spec in investigation_render::export::KINDS {
        if spec.scope() != investigation_render::export::Scope::Case {
            continue;
        }
        for audience in [Audience::Internal, Audience::Public] {
            let plan = plan_export(spec, &snapshot, audience).unwrap();
            let identifiers = plan.identifiers();
            let files = plan
                .files()
                .entries()
                .iter()
                .map(|entry| (entry.path().as_str().to_owned(), entry.content().to_vec()))
                .chain([(
                    "export-manifest.json".to_owned(),
                    plan.manifest_bytes().to_vec(),
                )]);
            for (path, bytes) in files {
                let text = String::from_utf8(identifiers.restore(&bytes)).unwrap();
                // Not vacuous: the consultation itself is shown.
                named += usize::from(text.contains(&fixture.consultation.to_string()));
                for &id in &fixture.secret_records {
                    let uuid = id.to_string().split_once('_').unwrap().1.to_owned();
                    if text.contains(&uuid) || path.contains(&uuid) {
                        found.push(format!("{} {audience:?} {path}: {id}", spec.name));
                    }
                }
                if text.contains(&digest) || text.contains(RESTRICTED) {
                    found.push(format!("{} {audience:?} {path}: digest or text", spec.name));
                }
            }
        }
    }
    assert!(found.is_empty(), "{found:#?}");
    assert!(
        named > 0,
        "no restricted export shows the consultation's pointer"
    );
}
