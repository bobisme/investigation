//! The option-by-criterion matrix on question and decision notes: every
//! cell at one snapshot, a missing cell labelled, authored table content
//! escaped, a large matrix capped with its count, and no computed score.

mod support;

use investigation_core::{identity::Id, wire::Receipt};
use investigation_render::{RenderPlan, VerifiedSnapshot, ViewPolicy, layout, render};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const ACTOR: &str = "agent:matrix-test";
const HOSTILE: &str = "cheap | fast ]] [[Other]] <b>x</b> `tick`";

fn reference(id: Id, revision: u64) -> Value {
    json!({"id":id, "revision":revision})
}

fn create(store: &mut Store, key: &str, case: Option<Id>, op: &str, payload: Value) -> Id {
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":key, "case_id":case,
        "actor":ACTOR, "preconditions":[], "operations":[{"op":op, "payload":payload}]
    });
    let receipt: Receipt = store
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap_or_else(|failure| panic!("{key}: {failure:?}"));
    receipt.created[0].id
}

fn option(store: &mut Store, case: Id, question: Id, key: &str, description: &str) -> Id {
    let id = create(
        store,
        key,
        Some(case),
        "option.create",
        json!({"question_ref":reference(question, 1), "description":description,
            "dependencies":[], "tradeoff_notes":[]}),
    );
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":format!("{key}-eligible"),
        "case_id":case, "actor":ACTOR, "preconditions":[],
        "operations":[{"op":"option.disposition", "payload":{
            "option_ref":reference(id, 1), "lifecycle":"eligible", "reason":"A candidate"}}]
    });
    store.record(&serde_json::to_vec(&input).unwrap()).unwrap();
    id
}

fn criterion(store: &mut Store, case: Id, question: Id, key: &str, name: &str, kind: &str) -> Id {
    let (unit, threshold) = if kind == "hard" {
        (
            json!("seconds"),
            json!({"kind":"measured", "operator":"at_most",
                "value":{"decimal":"300", "unit":"seconds"}}),
        )
    } else {
        (Value::Null, Value::Null)
    };
    create(
        store,
        key,
        Some(case),
        "criterion.create",
        json!({"name":name, "kind":kind,
            "scope":{"kind":"all_options", "question_ref":reference(question, 1)},
            "scale":{"unit":unit, "description":"A judgment", "anchors":[]},
            "preferred_direction":"higher", "threshold":threshold, "weighting_rationale":null}),
    )
}

fn comparison(store: &mut Store, case: Id, key: &str, option: Id, criterion: Id, rest: Value) {
    let assumption = create(
        store,
        &format!("{key}-assumption"),
        Some(case),
        "assumption.create",
        json!({"claim":"The stated judgment holds", "consequence":"The cell would change",
            "materiality":"material", "affected_refs":[]}),
    );
    let mut payload = json!({
        "option_ref":reference(option, 2), "criterion_ref":reference(criterion, 1),
        "method":"inspection", "evidence_refs":[], "assumption_refs":[assumption],
        "uncertainty":"Judgment only", "rationale":"Recorded as stated"
    });
    for (name, value) in rest.as_object().unwrap() {
        payload[name] = value.clone();
    }
    create(store, key, Some(case), "comparison.add", payload);
}

fn note(plan: &RenderPlan, case: Id, id: Id) -> String {
    let path = layout::record_note(case, id).unwrap().unwrap();
    String::from_utf8(plan.entry(path.as_str()).unwrap().content().to_vec()).unwrap()
}

fn matrix(note: &str) -> &str {
    let start = note
        .find("\n### Option-by-criterion comparison\n")
        .unwrap_or_else(|| panic!("no matrix in\n{note}"))
        + 1;
    let rest = &note[start..];
    let end = rest.find("\n## ").map_or(rest.len(), |at| at + 1);
    &rest[..end]
}

struct Built {
    _directory: tempfile::TempDir,
    store: Store,
    case: Id,
    question: Id,
    option_b: Id,
    soft: Id,
}

fn built(extra_options: usize) -> Built {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    Workspace::init(&root, true).unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let case = create(
        &mut store,
        "case",
        None,
        "case.create",
        json!({"title":"Choose", "mode":"evaluate", "objective":"Choose one",
            "decision_relevance":"Pick a design"}),
    );
    let question = create(
        &mut store,
        "question",
        Some(case),
        "question.create",
        json!({"wording":"Which design?", "importance":"normal"}),
    );
    let a = option(&mut store, case, question, "a", "Design A");
    let b = option(&mut store, case, question, "b", "SECRET-OPTION-TITLE");
    for n in 0..extra_options {
        option(
            &mut store,
            case,
            question,
            &format!("x{n}"),
            &format!("Extra {n}"),
        );
    }
    let hard = criterion(&mut store, case, question, "hard", "Must recover", "hard");
    let soft = criterion(&mut store, case, question, "soft", HOSTILE, "soft");
    comparison(
        &mut store,
        case,
        "a-soft",
        a,
        soft,
        json!({"outcome":"partially_meets",
            "basis":{"kind":"preference", "statement":HOSTILE}}),
    );
    comparison(
        &mut store,
        case,
        "b-hard",
        b,
        hard,
        json!({"outcome":"fails", "value":{"decimal":"900", "unit":"seconds"}}),
    );
    comparison(
        &mut store,
        case,
        "b-soft",
        b,
        soft,
        json!({"outcome":"unknown", "basis":{"kind":"unknown", "reason":"SECRETREASONTEXT"}}),
    );
    Built {
        _directory: directory,
        store,
        case,
        question,
        option_b: b,
        soft,
    }
}

fn plan(built: &mut Built) -> RenderPlan {
    let snapshot = VerifiedSnapshot::read(&mut built.store).unwrap();
    render(&snapshot, &ViewPolicy::default()).unwrap()
}

#[test]
fn every_cell_is_stated_and_a_missing_cell_is_labelled() {
    let mut built = built(0);
    let plan = plan(&mut built);
    let text = note(&plan, built.case, built.question);
    let section = matrix(&text);
    for expected in [
        "unknown: no comparison recorded",
        "preference, not a measurement",
        "outcome `partially_meets`",
        "unknown",
        "because \"SECRETREASONTEXT\"",
        "no overall score and no ranking",
        "(hard)",
        "(soft)",
        "Hard failures:",
        "recorded as failing, but its evidence is not verified",
        "Pareto status",
    ] {
        assert!(section.contains(expected), "{expected}\n{section}");
    }
    for forbidden in ["total", "weighted score", "rank 1", "winner"] {
        assert!(!section.contains(forbidden), "{forbidden}\n{section}");
    }
    // The case's Comparison Matrix view shows the same table, the missing
    // cell as unknown and every recorded cell linked to its comparison.
    let view = std::str::from_utf8(
        plan.entry(
            layout::case_view(built.case, "Comparison Matrix")
                .unwrap()
                .as_str(),
        )
        .expect("a Comparison Matrix view")
        .content(),
    )
    .unwrap();
    let view_matrix = matrix(view);
    assert_eq!(
        view_matrix.trim_end(),
        section.trim_end(),
        "the view and the question note differ"
    );
    assert!(!view.contains("missing: no comparison recorded"), "{view}");
}

#[test]
fn authored_table_content_stays_inside_its_cell() {
    let mut built = built(0);
    let plan = plan(&mut built);
    let text = note(&plan, built.case, built.question);
    let section = matrix(&text);
    assert!(!section.contains("<b>"), "{section}");
    let header = section
        .lines()
        .find(|l| l.starts_with("| Option |"))
        .unwrap();
    let columns = |line: &str| {
        let bytes = line.as_bytes();
        let mut count = 0;
        for (at, byte) in bytes.iter().enumerate() {
            if *byte == b'|' && (at == 0 || bytes[at - 1] != b'\\') {
                count += 1;
            }
        }
        count
    };
    let rows: Vec<&str> = section.lines().filter(|l| l.starts_with("| ")).collect();
    assert_eq!(rows.len(), 3, "{section}");
    for row in rows {
        assert_eq!(columns(row), columns(header), "{row}");
    }
}

#[test]
fn the_matrix_is_stable_across_renders() {
    let mut built = built(0);
    let first = plan(&mut built);
    let second = plan(&mut built);
    let text = note(&first, built.case, built.question);
    assert_eq!(text, note(&second, built.case, built.question), "stable");
}

#[test]
fn a_large_matrix_is_capped_with_its_count() {
    let mut built = built(12);
    let plan = plan(&mut built);
    let text = note(&plan, built.case, built.question);
    let section = matrix(&text);
    assert!(
        section.contains("Showing 12 of 14 options and 2 of 2 criteria"),
        "{section}"
    );
    let rows = section.lines().filter(|l| l.starts_with("| [[")).count();
    assert_eq!(rows, 12, "{section}");
}

#[test]
fn withheld_options_and_criteria_show_by_id_only() {
    let mut built = built(0);
    for (key, subject) in [
        ("raise-option", reference(built.option_b, 2)),
        ("raise-criterion", reference(built.soft, 1)),
    ] {
        create_raise(&mut built, key, subject);
    }
    let plan = plan(&mut built);
    let text = note(&plan, built.case, built.question);
    let section = matrix(&text);
    for secret in ["SECRET-OPTION-TITLE", "SECRETREASONTEXT", "cheap"] {
        assert!(!text.contains(secret), "{secret} leaked\n{text}");
    }
    assert!(section.contains(&built.option_b.to_string()), "{section}");
    assert!(section.contains(&built.soft.to_string()), "{section}");
    // The withheld option's row is withheld in every cell; the withheld
    // criterion's column is withheld for the visible option too.
    let withheld_row = section
        .lines()
        .find(|l| l.contains(&built.option_b.to_string()) && l.starts_with("| "))
        .unwrap();
    assert_eq!(
        withheld_row.matches("withheld").count(),
        2,
        "{withheld_row}"
    );
    let visible_row = section
        .lines()
        .find(|l| l.starts_with("| [[") && l.contains("Design A"))
        .unwrap();
    assert_eq!(visible_row.matches("withheld").count(), 1, "{visible_row}");
}

fn create_raise(built: &mut Built, key: &str, subject: Value) {
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":key, "case_id":built.case,
        "actor":ACTOR, "preconditions":[],
        "operations":[{"op":"classification.raise", "payload":{
            "subject_ref":subject, "level":"restricted", "reason":"Sensitive"}}]
    });
    built
        .store
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap_or_else(|failure| panic!("{key}: {failure:?}"));
}
