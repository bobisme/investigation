//! The `method-retrospective` export (W11.06) on one small case built
//! through real operations.
//!
//! One Retrospective with four lessons:
//!
//! - lesson 0 cites a public observation and is admitted on human review by
//!   the Retrospective's own author (self-reported);
//! - lesson 1 cites the same observation and is admitted on human review by
//!   a different actor;
//! - lesson 2 cites an internal observation and has no admission;
//! - lesson 3 cites a restricted observation.
//!
//! Every restricted string carries [`RESTRICTED`] and every internal one
//! [`INTERNAL`], so a byte scan of every planned file, the manifest and the
//! file names finds any channel that leaks.

use investigation_core::{
    identity::Id,
    wire::{CommitResult, Receipt},
};
use investigation_render::{
    Audience,
    export::{ExportPlan, ExportSnapshot, OmissionReason, Omitted, Pin, kind, plan_export},
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const AUTHOR: &str = "agent:retro-author";
const OTHER: &str = "human:other-reviewer";
const REVIEWER: &str = "human:classification-reviewer";
const RESTRICTED: &str = "ZEBRACODE";
const INTERNAL: &str = "OTTERNOTE";
const PUBLIC: &str = "PUFFINLINE";

fn reference(id: Id, revision: u64) -> Value {
    json!({"id":id, "revision":revision})
}

fn submit(
    store: &mut Store,
    key: &str,
    actor: &str,
    descriptor: Option<&str>,
    case: Option<Id>,
    op: &str,
    payload: Value,
) -> Receipt {
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":key, "case_id":case,
        "actor":actor, "actor_descriptor":descriptor.map(|kind| json!({"kind":kind})),
        "preconditions":[], "operations":[{"op":op, "payload":payload}]
    });
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

fn current(store: &mut Store, case: Id, id: Id) -> Value {
    let replay = store.case(case).unwrap();
    reference(id, replay.state.current(id).unwrap().header.revision.0)
}

fn observation(store: &mut Store, key: &str, case: Id, statement: String) -> Id {
    submit(
        store,
        key,
        AUTHOR,
        None,
        Some(case),
        "observation.add",
        json!({
            "value":{"kind":"text", "statement":statement},
            "basis":{"kind":"assertion", "assertion":format!("{statement} assertion"), "context":null},
            "extraction_method":null, "validity":"valid", "quality":[], "limitations":[],
            "evidence_kind":"observational", "variables_changed":[], "observed_at":null
        }),
    )
    .created[0]
    .id
}

fn raise(store: &mut Store, key: &str, case: Id, id: Id) {
    let subject = current(store, case, id);
    submit(
        store,
        key,
        AUTHOR,
        None,
        Some(case),
        "classification.raise",
        json!({"subject_ref":subject, "level":"restricted",
            "reason":format!("{RESTRICTED} raise reason {key}")}),
    );
}

fn clear(store: &mut Store, key: &str, case: Id, id: Id) {
    let subject = current(store, case, id);
    submit(
        store,
        key,
        REVIEWER,
        None,
        Some(case),
        "classification.downgrade",
        json!({
            "subject_ref":subject, "level":"public",
            "reason":"Reviewed for publication.",
            "transformation":{
                "inputs":[{"input_ref":subject, "digest":null}],
                "transformation":"manual_review", "version":"1",
                "redaction_decisions":[{"scope":"Whole record", "action":"retained",
                    "rationale":"Nothing in it is sensitive."}],
                "output_digest":null
            },
            "review":{"method":"Read the whole record.", "coverage":"whole_output",
                "examined":[subject]}
        }),
    );
}

fn create_case(store: &mut Store, title: String) -> Id {
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":"case", "case_id":null,
        "actor":AUTHOR, "preconditions":[], "operations":[{"op":"case.create",
            "payload":{"title":title, "mode":"explain"}}]
    });
    store
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap()
        .created[0]
        .id
}

fn retrospective_payload(store: &mut Store, case: Id, extra: Value) -> Value {
    let replay = store.case(case).unwrap();
    let case_ref = replay.state.current(case).unwrap().reference();
    let mut payload = json!({
        "case_ref":case_ref,
        "snapshot":{"sequence":replay.snapshot.sequence, "head_hash":replay.snapshot.head_hash},
        "methods_attempted":[{"name":format!("{PUBLIC} tracing method")}],
        "effective_steps":[],
        "invalid_or_misleading_steps":[],
        "none_observed_reason":format!("{PUBLIC} nothing misleading was seen"),
        "costs_or_constraints":format!("{PUBLIC} one session"),
        "residual_unknowns":[format!("{PUBLIC} the root cause")],
    });
    for (key, value) in extra.as_object().unwrap() {
        payload[key] = value.clone();
    }
    payload
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    case: Id,
    retrospective: Id,
    admission_self: Id,
    admission_other: Id,
    internal_observation: Id,
    restricted_observation: Id,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let case = create_case(&mut store, format!("{INTERNAL} case title"));
    let public_observation = observation(
        &mut store,
        "public-observation",
        case,
        format!("{PUBLIC} observation"),
    );
    clear(&mut store, "clear-observation", case, public_observation);
    let internal_observation = observation(
        &mut store,
        "internal-observation",
        case,
        format!("{INTERNAL} observation"),
    );
    let restricted_observation = observation(
        &mut store,
        "restricted-observation",
        case,
        format!("{RESTRICTED} observation"),
    );
    raise(
        &mut store,
        "raise-observation",
        case,
        restricted_observation,
    );

    let lesson = |guidance: String, support: Vec<Id>| {
        json!({
            "guidance":guidance, "applies_when":format!("{PUBLIC} short bursts only"),
            "support_refs":support,
            "transfer_limits":format!("{PUBLIC} not for long-running workloads"),
        })
    };
    let payload = retrospective_payload(
        &mut store,
        case,
        json!({
            "effective_steps":[{"description":format!("{PUBLIC} step on public evidence"),
                "support_refs":[public_observation]},
                {"description":format!("{INTERNAL} step on internal evidence"),
                "support_refs":[internal_observation]}],
            "lessons":[
                lesson(format!("{PUBLIC} lesson zero"), vec![public_observation]),
                lesson(format!("{PUBLIC} lesson one"), vec![public_observation]),
                lesson(format!("{INTERNAL} lesson two"), vec![internal_observation]),
                lesson(format!("{RESTRICTED} lesson three"), vec![restricted_observation]),
            ],
        }),
    );
    let retrospective = submit(
        &mut store,
        "retrospective",
        AUTHOR,
        None,
        Some(case),
        "retrospective.record",
        payload,
    )
    .created[0]
        .id;
    let retro_ref = current(&mut store, case, retrospective);
    let admit = |store: &mut Store, key: &str, actor: &str, index: u32| {
        submit(
            store,
            key,
            actor,
            Some("human"),
            Some(case),
            "lesson.admit",
            json!({"retrospective_ref":retro_ref, "lesson_index":index,
                "basis":{"kind":"human_review", "rationale":format!("{PUBLIC} checked it {key}")}}),
        )
        .created[0]
            .id
    };
    let admission_self = admit(&mut store, "admit-self", AUTHOR, 0);
    let admission_other = admit(&mut store, "admit-other", OTHER, 1);
    // Make the lessons' retrospective shareable, as a reviewed downgrade of
    // its text, so the audiences below are decided lesson by lesson.
    for (key, id) in [
        ("clear-retrospective", retrospective),
        ("clear-admission-self", admission_self),
        ("clear-admission-other", admission_other),
    ] {
        clear(&mut store, key, case, id);
    }
    Fixture {
        _directory: directory,
        store,
        case,
        retrospective,
        admission_self,
        admission_other,
        internal_observation,
        restricted_observation,
    }
}

fn export(fixture: &mut Fixture, audience: Audience) -> ExportPlan {
    let snapshot = ExportSnapshot::read(&mut fixture.store, fixture.case, &Pin::default()).unwrap();
    plan_export(kind("method-retrospective").unwrap(), &snapshot, audience).unwrap()
}

fn bytes(plan: &ExportPlan) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = plan
        .files()
        .entries()
        .iter()
        .map(|entry| {
            (
                entry.path().as_str().to_owned(),
                String::from_utf8(entry.content().to_vec()).unwrap(),
            )
        })
        .collect();
    out.push((
        "export-manifest.json".to_owned(),
        String::from_utf8(plan.manifest_bytes().to_vec()).unwrap(),
    ));
    out
}

fn mentions(plan: &ExportPlan, needle: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (path, text) in bytes(plan) {
        if path.contains(needle) {
            found.push(format!("path {path}"));
        }
        found.extend(
            text.lines()
                .filter(|line| line.contains(needle))
                .map(|line| format!("{path}: {line}")),
        );
    }
    found
}

fn document(plan: &ExportPlan) -> String {
    String::from_utf8(
        plan.files()
            .entry("method-retrospective.md")
            .unwrap()
            .content()
            .to_vec(),
    )
    .unwrap()
}

/// The block of one lesson, up to the next lesson heading. The retrospective
/// ID is mapped to what the export shows (identical unless remapped).
fn lesson_block(plan: &ExportPlan, index: u32, retrospective: Id) -> Option<String> {
    let canonical = retrospective.to_string();
    let shown = plan.identifiers().exported(&canonical)?;
    let document = document(plan);
    let heading = format!("### Lesson `lesson:{shown}:{index}`");
    let start = document.find(&heading)?;
    let rest = &document[start + heading.len()..];
    let end = rest
        .find("\n### ")
        .or_else(|| rest.find("\n## "))
        .unwrap_or(rest.len());
    Some(rest[..end].to_owned())
}

#[test]
fn restricted_shows_every_lesson_with_its_exact_reviewer_status() {
    let mut fixture = fixture();
    let plan = export(&mut fixture, Audience::Restricted);
    let text = document(&plan);
    assert!(
        text.contains("Nothing was applied to any method pack"),
        "{text}"
    );
    for index in 0..4 {
        let block = lesson_block(&plan, index, fixture.retrospective)
            .unwrap_or_else(|| panic!("lesson {index} missing:\n{text}"));
        for label in [
            "Applies when:",
            "Do not reuse where (transfer limits):",
            "Reviewer status:",
            "Counterexamples:",
            "Consultation history:",
            "Trust and classification:",
        ] {
            assert!(
                block.contains(label),
                "lesson {index} lacks {label}:\n{block}"
            );
        }
    }
    let zero = lesson_block(&plan, 0, fixture.retrospective).unwrap();
    assert!(zero.contains("SELF-REPORTED"), "{zero}");
    assert!(zero.contains("not an independent review"), "{zero}");
    assert!(zero.contains(AUTHOR), "{zero}");
    assert!(
        !zero.contains("other than the retrospective's author"),
        "{zero}"
    );
    let one = lesson_block(&plan, 1, fixture.retrospective).unwrap();
    assert!(!one.contains("SELF-REPORTED"), "{one}");
    assert!(one.contains(OTHER), "{one}");
    assert!(
        one.contains("an actor other than the retrospective's author"),
        "{one}"
    );
    assert!(one.contains("not authenticated"), "{one}");
    let two = lesson_block(&plan, 2, fixture.retrospective).unwrap();
    assert!(two.contains("Not shown as admitted"), "{two}");
    assert!(two.contains("unreviewed proposal"), "{two}");
    assert!(plan.complete(), "{:?}", plan.exclusions());
    assert!(plan.omissions().is_empty(), "{:?}", plan.omissions());
}

/// The lessons and steps each audience does not get, as the publisher sees
/// them: what, why, the retrospective and which entry (bn-1vg8).
fn left_out(plan: &ExportPlan) -> Vec<(Omitted, OmissionReason, Option<Id>, String)> {
    plan.omissions()
        .iter()
        .map(|omission| {
            (
                omission.content,
                omission.reason,
                omission.record,
                omission.part.clone().unwrap(),
            )
        })
        .collect()
}

#[test]
fn every_lesson_and_step_left_out_is_reported_to_the_publisher_only() {
    let mut fixture = fixture();
    let retro = Some(fixture.retrospective);
    let withheld = OmissionReason::RestsOnWithheld;
    let internal = export(&mut fixture, Audience::Internal);
    assert_eq!(
        left_out(&internal),
        [(Omitted::Lesson, withheld, retro, "lessons[3]".to_owned())]
    );
    let public = export(&mut fixture, Audience::Public);
    assert_eq!(
        left_out(&public),
        [
            (Omitted::Lesson, withheld, retro, "lessons[2]".to_owned()),
            (Omitted::Lesson, withheld, retro, "lessons[3]".to_owned()),
            (
                Omitted::Step,
                withheld,
                retro,
                "effective_steps[1]".to_owned()
            ),
        ]
    );
    for plan in [&internal, &public] {
        assert!(!plan.complete());
        // The export says only that material was withheld, in the words an
        // export with no omission beside its exclusions uses as well.
        let manifest = plan.manifest();
        assert_eq!(manifest.completeness.as_str(), "incomplete");
        assert!(
            manifest
                .statements
                .completeness
                .starts_with("Incomplete: the audience policy withheld material"),
            "{}",
            manifest.statements.completeness
        );
        for word in ["lessons[", "effective_steps[", "rests_on", "omission"] {
            assert!(mentions(plan, word).is_empty(), "{word}");
        }
    }
}

#[test]
fn internal_leaves_out_lessons_that_cite_restricted_evidence() {
    let mut fixture = fixture();
    let plan = export(&mut fixture, Audience::Internal);
    let text = document(&plan);
    assert!(
        lesson_block(&plan, 0, fixture.retrospective).is_some(),
        "{text}"
    );
    assert!(
        lesson_block(&plan, 2, fixture.retrospective).is_some(),
        "{text}"
    );
    assert!(
        lesson_block(&plan, 3, fixture.retrospective).is_none(),
        "a lesson resting on restricted evidence is not shown:\n{text}"
    );
    assert!(!plan.complete());
    for needle in [RESTRICTED, &fixture.restricted_observation.to_string()] {
        assert!(mentions(&plan, needle).is_empty(), "{needle}");
    }
    assert!(
        text.contains(INTERNAL),
        "internal text may appear for internal"
    );
}

#[test]
fn public_shows_only_lessons_whose_support_is_public() {
    let mut fixture = fixture();
    let plan = export(&mut fixture, Audience::Public);
    let text = document(&plan);
    assert!(
        lesson_block(&plan, 0, fixture.retrospective).is_some(),
        "{text}"
    );
    assert!(
        lesson_block(&plan, 1, fixture.retrospective).is_some(),
        "{text}"
    );
    for index in [2, 3] {
        assert!(
            lesson_block(&plan, index, fixture.retrospective).is_none(),
            "lesson {index} rests on non-public evidence:\n{text}"
        );
    }
    // The self-reported label survives into the public export, without names.
    let head = ExportSnapshot::read(&mut fixture.store, fixture.case, &Pin::default())
        .unwrap()
        .case()
        .head()
        .clone();
    assert!(
        mentions(&plan, head.head_hash.as_str()).is_empty(),
        "canonical chain head in a remapped export"
    );
    assert!(mentions(&plan, "not disclosed").len() == 1);
    let zero = lesson_block(&plan, 0, fixture.retrospective).unwrap();
    assert!(zero.contains("SELF-REPORTED") && zero.contains("the author of this retrospective"));
    let one = lesson_block(&plan, 1, fixture.retrospective).unwrap();
    assert!(one.contains("an actor other than the retrospective's author"));
    for actor in [AUTHOR, OTHER, REVIEWER] {
        assert!(
            mentions(&plan, actor).is_empty(),
            "{actor}: {:#?}",
            mentions(&plan, actor)
        );
    }
    assert!(!plan.complete());
}

#[test]
fn no_audience_leaks_withheld_text_names_or_ids() {
    let mut fixture = fixture();
    for (audience, above) in [
        (Audience::Public, vec![RESTRICTED, INTERNAL]),
        (Audience::Internal, vec![RESTRICTED]),
    ] {
        let plan = export(&mut fixture, audience);
        for exclusion in plan.exclusions() {
            if exclusion.id == fixture.case {
                continue;
            }
            let found = mentions(&plan, &exclusion.id.to_string());
            assert!(
                found.is_empty(),
                "{audience:?} withheld {}: {found:#?}",
                exclusion.id
            );
        }
        for needle in above {
            let found = mentions(&plan, needle);
            assert!(found.is_empty(), "{audience:?} {needle}: {found:#?}");
        }
        let ids = [fixture.restricted_observation];
        for id in ids {
            assert!(mentions(&plan, &id.to_string()).is_empty());
        }
    }
    let public = export(&mut fixture, Audience::Public);
    assert!(
        mentions(&public, &fixture.internal_observation.to_string()).is_empty(),
        "internal evidence ID in public export"
    );
    let _ = (fixture.admission_self, fixture.admission_other);
}

#[test]
fn the_same_snapshot_gives_byte_identical_files_and_manifest() {
    let mut fixture = fixture();
    for audience in [Audience::Public, Audience::Internal, Audience::Restricted] {
        let first = bytes(&export(&mut fixture, audience));
        let second = bytes(&export(&mut fixture, audience));
        assert_eq!(first, second, "{audience:?}");
    }
}

#[test]
fn a_case_with_no_reusable_lesson_exports_that_statement_and_its_reason() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let case = create_case(&mut store, format!("{PUBLIC} empty case"));
    clear(&mut store, "clear-case", case, case);
    let payload = retrospective_payload(
        &mut store,
        case,
        json!({"lessons":[],
            "no_reusable_lesson_reason":format!("{PUBLIC} the setup cannot be repeated elsewhere")}),
    );
    let retrospective = submit(
        &mut store,
        "retrospective",
        AUTHOR,
        None,
        Some(case),
        "retrospective.record",
        payload,
    )
    .created[0]
        .id;
    clear(&mut store, "clear-retrospective", case, retrospective);
    let snapshot = ExportSnapshot::read(&mut store, case, &Pin::default()).unwrap();
    for audience in [Audience::Public, Audience::Internal, Audience::Restricted] {
        let plan = plan_export(kind("method-retrospective").unwrap(), &snapshot, audience).unwrap();
        let text = document(&plan);
        assert!(
            text.contains("No reusable lesson identified. Reason:"),
            "{audience:?}\n{text}"
        );
        assert!(
            text.contains("the setup cannot be repeated elsewhere"),
            "{audience:?}"
        );
        assert!(!text.contains("### Lesson"), "{audience:?}");
    }
}

/// A lesson cites a Decision whose derived title would name its chosen
/// option; that option is restricted. No audience below restricted may see
/// the option's text through the cited decision's title (bn-31an).
#[test]
fn a_cited_decision_never_leaks_its_withheld_option_through_a_derived_title() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let case = create_case(&mut store, format!("{PUBLIC} derived case"));
    clear(&mut store, "clear-case", case, case);
    let question = submit(
        &mut store,
        "question",
        AUTHOR,
        None,
        Some(case),
        "question.create",
        json!({"wording":format!("{PUBLIC} question"), "importance":"normal"}),
    )
    .created[0]
        .id;
    let question_ref = current(&mut store, case, question);
    let option = submit(
        &mut store,
        "option",
        AUTHOR,
        None,
        Some(case),
        "option.create",
        json!({"question_ref":question_ref,
            "description":format!("{RESTRICTED} option description"),
            "dependencies":[], "tradeoff_notes":[]}),
    )
    .created[0]
        .id;
    raise(&mut store, "raise-option", case, option);
    let question_ref = current(&mut store, case, question);
    let option_ref = current(&mut store, case, option);
    let decision = submit(
        &mut store,
        "decision",
        AUTHOR,
        None,
        Some(case),
        "decision.propose",
        json!({
            "question_ref":question_ref,
            "choice":{"kind":"select", "option_ref":option_ref},
            "compared_alternatives":[option_ref],
            "comparison_refs":[], "hard_conditions":[], "evidence_refs":[],
            "assumption_refs":[], "tradeoffs":[format!("{PUBLIC} tradeoff")],
            "consequences":[format!("{PUBLIC} consequence")],
            "reopen_trigger":format!("{PUBLIC} reopen"), "reversal_conditions":[]
        }),
    )
    .created[0]
        .id;
    clear(&mut store, "clear-decision", case, decision);
    let payload = retrospective_payload(
        &mut store,
        case,
        json!({
            "effective_steps":[{"description":format!("{PUBLIC} step citing the decision"),
                "support_refs":[decision]}],
            "lessons":[{"guidance":format!("{PUBLIC} lesson on the decision"),
                "applies_when":format!("{PUBLIC} when choosing"),
                "support_refs":[decision],
                "transfer_limits":format!("{PUBLIC} one case only")}],
        }),
    );
    let retrospective = submit(
        &mut store,
        "retrospective",
        AUTHOR,
        None,
        Some(case),
        "retrospective.record",
        payload,
    )
    .created[0]
        .id;
    clear(&mut store, "clear-retrospective", case, retrospective);
    let snapshot = ExportSnapshot::read(&mut store, case, &Pin::default()).unwrap();
    for audience in [Audience::Public, Audience::Internal] {
        let plan = plan_export(kind("method-retrospective").unwrap(), &snapshot, audience).unwrap();
        assert!(
            mentions(&plan, RESTRICTED).is_empty(),
            "{audience:?}: {:#?}",
            mentions(&plan, RESTRICTED)
        );
        assert!(
            mentions(&plan, &option.to_string()).is_empty(),
            "{audience:?} names the withheld option"
        );
    }
}
