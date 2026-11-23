//! Record notes beyond the evidence family: assessments, objections,
//! annotations, validation and blindspot reviews, and review freshness on
//! both ends.
//!
//! One explanatory case holds two rival hypotheses, a quoted observation, an
//! assessment that weighs the rival, a conclusion with a validation review,
//! a material objection raised after that review, a critical objection on a
//! hypothesis, a blindspot review, an annotation and a restricted
//! acquisition. Every record is made through the store's real operations.

mod support;

use std::fs;

use investigation_core::{
    basis::basis_digest,
    command::RevisionRef,
    identity::{Id, Kind},
    review_ops::required_read_set,
    wire::Receipt,
};
use investigation_render::{Audience, RenderPlan, VerifiedSnapshot, ViewPolicy, layout, render};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};
use support::vault;

const ACTOR: &str = "agent:record-render-test";
/// Restricted bytes: no note may ever contain them.
const RESTRICTED: &str = "KESTREL-PLAN rollout begins on the ninth.";
const QUOTE: &str = "pool exhausted after 30 s";

fn reference(id: Id, revision: u64) -> Value {
    json!({"id":id, "revision":revision})
}

fn submit(
    store: &mut Store,
    key: &str,
    case: Option<Id>,
    op: &str,
    payload: Value,
    preconditions: Value,
) -> Receipt {
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":key, "case_id":case,
        "actor":ACTOR, "preconditions":preconditions, "operations":[{"op":op, "payload":payload}]
    });
    store
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap_or_else(|failure| panic!("{key}: {failure:?}"))
}

fn create(store: &mut Store, key: &str, case: Id, op: &str, payload: Value) -> Id {
    submit(store, key, Some(case), op, payload, json!([])).created[0].id
}

fn context() -> Value {
    json!({
        "saw_original_conclusion":{"state":"known","value":true},
        "gathered_new_evidence":{"state":"known","value":false},
        "method_or_source_changes":[], "activity":"Read the recorded basis.",
        "dependencies":"shared", "dependence_basis":"Same recorded capture."
    })
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    case: Id,
    factor: Id,
    rival: Id,
    observation: Id,
    assessment: Id,
    conclusion: Id,
    validation: Id,
    blindspot: Id,
    audited: u64,
    material: Id,
    critical: Id,
    annotation: Id,
    restricted: Id,
}

fn ingest(
    store: &mut Store,
    root: &std::path::Path,
    key: &str,
    case: Id,
    text: &str,
    level: &str,
) -> Value {
    let path = root.join(format!("{key}.txt"));
    fs::write(&path, text).unwrap();
    let manifest = json!({
        "schema_version":"inv.ingest.v1", "command_id":key, "actor":ACTOR, "case_id":case,
        "source":{"title":key, "format":"text/plain"},
        "paths":[path], "classification":level, "extract_text":true
    });
    let receipt = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap_or_else(|failure| panic!("{failure:?}"));
    serde_json::to_value(&receipt.items[0]).unwrap()
}

fn current(
    store: &mut Store,
    case: Id,
    id: Id,
) -> (
    investigation_core::state::Record,
    Vec<RevisionRef>,
    u64,
    Value,
) {
    let snapshot = VerifiedSnapshot::read(store).unwrap();
    let case = snapshot.case(case).unwrap();
    let record = case.state().current(id).unwrap().clone();
    let read_set = required_read_set(case.state(), &record).unwrap();
    let head = case.head();
    (record, read_set, head.sequence, json!(head))
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    Workspace::init(&root, true).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        contents.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let case = submit(
        &mut store,
        "case",
        None,
        "case.create",
        json!({"title":"Why does the pool stall?", "mode":"explain"}),
        json!([]),
    )
    .created[0]
        .id;
    let question = create(
        &mut store,
        "question",
        case,
        "question.create",
        json!({"wording":"What exhausts the pool?", "importance":"normal"}),
    );
    let hypothesis = |statement: &str, rivals: Vec<Value>| {
        json!({"question_ref":reference(question, 1), "statement":statement,
            "assumption_refs":[], "rival_refs":rivals, "applicability":"The sampled burst",
            "possible_falsifiers":["The stall persists with connections released early."]})
    };
    let rival = create(
        &mut store,
        "rival",
        case,
        "hypothesis.create",
        hypothesis("Scheduler delay alone explains the stall.", vec![]),
    );
    let factor = create(
        &mut store,
        "factor",
        case,
        "hypothesis.create",
        hypothesis(
            "Held connections exhaust the pool ]] [[Other]] <b>x</b>.",
            vec![reference(rival, 1)],
        ),
    );
    let capture = ingest(
        &mut store,
        &root,
        "capture",
        case,
        &format!("start\n{QUOTE}\n"),
        "internal",
    );
    let text: Id = capture["derivative_id"].as_str().unwrap().parse().unwrap();
    let observation = create(
        &mut store,
        "observation",
        case,
        "observation.add",
        json!({
            "value":{"kind":"text", "statement":"The pool ran out after 30 s."},
            "basis":{"kind":"source", "artifact_ref":reference(text, 1),
                "selector":{"schema_version":"inv.selector.v1", "artifact_digest":capture["digest"],
                    "selector":{"type":"text_lines", "start_line":2, "end_line":2,
                        "encoding":"utf-8", "parser_version":"inv.text.utf8.v1"}},
                "text_quote":{"exact":QUOTE}},
            "validity":"valid", "quality":[], "limitations":["One capture."],
            "evidence_kind":"observational", "variables_changed":[]
        }),
    );
    let restricted = ingest(&mut store, &root, "plan", case, RESTRICTED, "restricted");
    let restricted: Id = restricted["artifact_id"].as_str().unwrap().parse().unwrap();
    let assessment = create(
        &mut store,
        "assessment",
        case,
        "assessment.add",
        json!({
            "subject_ref":reference(factor, 1), "evidence_refs":[reference(observation, 1)],
            "relation":"supports", "rationale":"Exhaustion follows the hold ==hl==.",
            "rival_expectations":[{"rival_ref":reference(rival, 1), "expectation":"less_expected",
                "rationale":"A scheduler delay would not drain the pool."}],
            "assumption_refs":[],
            "confidence":{"level":"moderate", "rationale":"One capture.",
                "limitations":["No control run."], "what_would_change_it":"A control capture.",
                "downgrade_reasons":null},
            "likelihood":null
        }),
    );
    let conclusion = create(
        &mut store,
        "conclusion",
        case,
        "conclusion.create",
        json!({
            "statement":"Held connections exhaust the pool in this burst.",
            "scope":"The sampled burst only.", "significance":"Release before waiting.",
            "support_refs":[reference(observation, 1)], "contradiction_refs":[reference(rival, 1)],
            "caveats":["One capture."], "causal_factors":[], "confidence":null, "likelihood":null
        }),
    );
    let (record, read_set, _, _) = current(&mut store, case, conclusion);
    let mut preconditions = vec![json!(record.reference())];
    preconditions.extend(read_set.iter().map(|reference| json!(reference)));
    submit(
        &mut store,
        "request-review",
        Some(case),
        "conclusion.request_review",
        json!({"conclusion_ref":reference(conclusion, 1)}),
        json!(preconditions),
    );
    let (record, read_set, _, _) = current(&mut store, case, conclusion);
    let validation = create(
        &mut store,
        "validation",
        case,
        "review.submit",
        json!({
            "kind":"validation", "subject_ref":record.reference(),
            "basis_digest":basis_digest(&record).unwrap(), "read_set":read_set,
            "method":"Read the capture and the rival.", "materials_examined":[reference(observation, 1)],
            "shared_dependencies":[], "objection_refs":[], "verdict":"supports",
            "caveats":["One capture."], "context_isolation":"fresh_context_unblinded",
            "context":context()
        }),
    );
    let (_, _, audited, head) = current(&mut store, case, factor);
    let blindspot = create(
        &mut store,
        "blindspot",
        case,
        "review.submit",
        json!({
            "kind":"blindspot", "subject_ref":reference(factor, 1), "audited_snapshot":head,
            "read_set":[], "method":"Walk the rival list.", "scope_examined":"Rivals of the factor.",
            "checks_considered":["Rival coverage"], "gaps":["No control capture [[Other]]."],
            "no_gap_reason":null, "evidence_refs":[], "assumption_refs":[],
            "limitations":["Recorded state only."], "verdict":"gaps_identified",
            "context_isolation":"same_session_self_review", "context":context()
        }),
    );
    let objection = |severity: &str, subject: Id, revision: u64| {
        json!({"subject_ref":reference(subject, revision), "statement":format!("A {severity} concern."),
            "scope":"The single capture.", "severity":severity, "evidence_refs":{"state":"unknown"}})
    };
    let material = create(
        &mut store,
        "material",
        case,
        "objection.raise",
        objection("material", conclusion, 2),
    );
    let critical = create(
        &mut store,
        "critical",
        case,
        "objection.raise",
        objection("critical", rival, 1),
    );
    let annotation = create(
        &mut store,
        "annotation",
        case,
        "annotation.add",
        json!({"subject_refs":[reference(factor, 1)],
            "content":{"kind":"text", "text":"Check the pool size %%c%% next."}}),
    );
    Fixture {
        _directory: directory,
        store,
        case,
        factor,
        rival,
        observation,
        assessment,
        conclusion,
        validation,
        blindspot,
        audited,
        material,
        critical,
        annotation,
        restricted,
    }
}

fn plan(fixture: &mut Fixture, audience: Audience) -> (VerifiedSnapshot, RenderPlan) {
    let snapshot = VerifiedSnapshot::read(&mut fixture.store).unwrap();
    let plan = render(&snapshot, &ViewPolicy::for_audience(audience)).unwrap();
    (snapshot, plan)
}

fn note(plan: &RenderPlan, case: Id, id: Id) -> String {
    let path = layout::record_note(case, id).unwrap().unwrap();
    String::from_utf8(plan.entry(path.as_str()).unwrap().content().to_vec()).unwrap()
}

fn target(case: Id, id: Id) -> String {
    let path = layout::record_note(case, id).unwrap().unwrap();
    path.as_str().strip_suffix(".md").unwrap().to_owned()
}

fn has(text: &str, expected: &str) {
    assert!(text.contains(expected), "{expected}\n---\n{text}");
}

#[test]
fn every_kind_renders_with_typed_sections_and_links_both_ways() {
    let mut fixture = fixture();
    let case = fixture.case;
    let (snapshot, plan) = plan(&mut fixture, Audience::Internal);
    let report = vault::check(&plan, &snapshot);
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
    for kind in [
        Kind::Question,
        Kind::Hypothesis,
        Kind::Observation,
        Kind::Assessment,
        Kind::Conclusion,
        Kind::Review,
        Kind::Objection,
        Kind::Annotation,
    ] {
        assert!(report.kinds.contains(&kind), "{kind:?}");
    }

    // The supported hypothesis: assessments by relation, its rival named
    // both ways, the critical objection and the blindspot review on it.
    let factor = note(&plan, case, fixture.factor);
    let standing = vault::section(&factor, "Standing").unwrap();
    has(standing, "### Rival explanations\n\n- [[");
    has(standing, &target(case, fixture.rival));
    has(
        standing,
        "### Evidence assessed against it\n\nSupporting:\n\n- [[",
    );
    has(
        standing,
        &format!("[[{}|", target(case, fixture.observation)),
    );
    has(standing, "Exhaustion follows the hold \\=\\=hl\\=\\=\\.");
    let reviews = vault::section(&factor, "Reviews and objections").unwrap();
    has(
        reviews,
        "`blindspot` review of revision 1, verdict `gaps_identified`",
    );
    let rival = note(&plan, case, fixture.rival);
    assert_eq!(
        vault::property(&rival, "inv_effective").as_deref(),
        Some("disputed")
    );
    let reviews = vault::section(&rival, "Reviews and objections").unwrap();
    has(
        reviews,
        &format!(
            "- [[{}|A critical concern.]]: `critical` objection, `open`",
            target(case, fixture.critical)
        ),
    );
    let standing = vault::section(&rival, "Standing").unwrap();
    has(standing, &target(case, fixture.factor));
    has(
        standing,
        "Assessments that weigh this hypothesis as a rival:",
    );
    has(
        standing,
        "evidence `less_expected` under this hypothesis. A scheduler delay would not drain the pool\\.",
    );
    has(standing, ": as a contradicting record");

    // The assessment: relation, rival expectations, confidence.
    let assessment = note(&plan, case, fixture.assessment);
    let body = vault::section(&assessment, "Assessment").unwrap();
    has(
        body,
        "- Relation: `supports`: the evidence is more expected if the subject holds",
    );
    has(body, "- Rival expectations:\n  - [[");
    has(
        body,
        "- Confidence: `moderate`. One capture\\.\n  - Limitation: No control run\\.",
    );
    assert!(vault::property(&assessment, "inv_disposition").is_some());

    // The conclusion: disputed through the critical objection on its basis,
    // which is named first; its validation review is disputed by the
    // material objection raised after it.
    let conclusion = note(&plan, case, fixture.conclusion);
    assert_eq!(
        vault::property(&conclusion, "inv_disposition").as_deref(),
        Some("disputed")
    );
    assert_eq!(
        vault::property(&conclusion, "inv_review_freshness").as_deref(),
        Some("disputed")
    );
    let disposition = vault::section(&conclusion, "Disposition").unwrap();
    has(disposition, "- Effective disposition: `disputed`");
    has(
        disposition,
        &format!(
            "- Strongest outstanding objection: [[{}|A critical concern.]], `critical` against [[{}|",
            target(case, fixture.critical),
            target(case, fixture.rival)
        ),
    );
    has(
        disposition,
        "  - 1 more open objection on this record or its basis",
    );
    let reviews = vault::section(&conclusion, "Reviews and objections").unwrap();
    has(
        reviews,
        "`validation` review of revision 2, verdict `supports`, freshness `disputed`",
    );
    has(reviews, "`objection_raised_after_review`");
    has(
        reviews,
        &format!(
            "[[{}|A material concern.]]: `material` objection, `open`",
            target(case, fixture.material)
        ),
    );

    let validation = note(&plan, case, fixture.validation);
    assert_eq!(
        vault::property(&validation, "inv_freshness").as_deref(),
        Some("disputed")
    );
    assert_eq!(
        vault::property(&validation, "inv_review_kind").as_deref(),
        Some("validation")
    );
    has(&validation, "> [!warning] Disputed review\n");

    // The blindspot review: same note kind, its own basis and gaps.
    let blindspot = note(&plan, case, fixture.blindspot);
    assert_eq!(
        vault::property(&blindspot, "inv_kind").as_deref(),
        Some("review")
    );
    assert_eq!(
        vault::property(&blindspot, "inv_review_kind").as_deref(),
        Some("blindspot")
    );
    assert_eq!(
        vault::property(&blindspot, "inv_basis_sequence"),
        Some(fixture.audited.to_string())
    );
    let body = vault::section(&blindspot, "Review").unwrap();
    has(
        body,
        &format!(
            "- Audited snapshot: sequence {}, head `sha256:",
            fixture.audited
        ),
    );
    has(
        body,
        "- Gaps identified:\n  - No control capture \\[\\[Other\\]\\]\\.",
    );
    has(
        body,
        "Zero gaps means none was identified within the examined scope",
    );
    has(
        body,
        "- Low independence: same-session self-review, declared by the reviewer",
    );
    has(
        body,
        "- Work kind: undeclared; read other reviews first: undeclared\n- Prior involvement: undeclared",
    );

    // The objection and the annotation.
    let critical = note(&plan, case, fixture.critical);
    assert_eq!(
        vault::property(&critical, "inv_severity").as_deref(),
        Some("critical")
    );
    has(
        &critical,
        "- Severity: `critical`: while open it blocks validation and disputes its subject",
    );
    has(&critical, "- Evidence: unknown; none was cited");
    let annotation = note(&plan, case, fixture.annotation);
    has(&annotation, "an annotation is commentary, not evidence");
    has(
        &annotation,
        "- Text: Check the pool size \\%\\%c\\%\\% next\\.",
    );
    let cited_by = factor.split("### Cited by").nth(1).unwrap();
    has(
        cited_by,
        &format!("[[{}|", target(case, fixture.annotation)),
    );

    // Nothing restricted reaches a note at this audience.
    for entry in plan.entries() {
        let text = std::str::from_utf8(entry.content()).unwrap();
        assert!(!text.contains("KESTREL"), "{}", entry.path());
    }
    let restricted = note(&plan, case, fixture.restricted);
    assert_eq!(
        vault::property(&restricted, "inv_withheld").as_deref(),
        Some("true")
    );
}

#[test]
fn no_note_for_a_restricted_artifact_contains_its_bytes() {
    let mut fixture = fixture();
    let case = fixture.case;
    let (snapshot, plan) = plan(&mut fixture, Audience::Restricted);
    assert!(vault::check(&plan, &snapshot).problems.is_empty());
    let restricted = note(&plan, case, fixture.restricted);
    assert!(vault::property(&restricted, "inv_withheld").is_none());
    has(&restricted, "| Role | `original`");
    has(
        &restricted,
        "The retained bytes are not copied into the vault.",
    );
    for entry in plan.entries() {
        let text = std::str::from_utf8(entry.content()).unwrap();
        assert!(!text.contains("KESTREL"), "{}", entry.path());
        assert!(!text.contains("ninth"), "{}", entry.path());
    }
}
