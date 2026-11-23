//! The static table views under `Cases/<case-id>/Views/` (plan §12.5,
//! W08.07): which views a case gets, bounded rows with an overflow link in
//! a stable order, and a timeline that keeps observed and recorded times
//! apart and shows corrections and retractions.

use investigation_core::{identity::Id, wire::Receipt};
use investigation_render::{RenderPlan, VerifiedSnapshot, ViewPolicy, layout, render};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const OBSERVED: &str = "2026-01-02T03:04:05Z";

fn submit(store: &mut Store, key: &str, case_id: Option<Id>, operation: Value) -> Receipt {
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":key, "case_id":case_id,
        "actor":"agent:views-test", "preconditions":[], "operations":[operation]
    });
    store
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap_or_else(|failure| panic!("{key}: {failure:?}"))
}

fn case(directory: &std::path::Path) -> (Store, Id) {
    let workspace = Workspace::init(&directory.join("workspace"), true).unwrap();
    let mut store = Store::open(workspace);
    let receipt = submit(
        &mut store,
        "case",
        None,
        json!({"op":"case.create", "payload":{"title":"Views fixture", "mode":"record"}}),
    );
    (store, receipt.created[0].id)
}

fn plan(store: &mut Store) -> RenderPlan {
    render(
        &VerifiedSnapshot::read(store).unwrap(),
        &ViewPolicy::default(),
    )
    .unwrap()
}

fn view<'p>(plan: &'p RenderPlan, case: Id, name: &str) -> Option<&'p str> {
    plan.entry(layout::case_view(case, name).unwrap().as_str())
        .map(|entry| std::str::from_utf8(entry.content()).unwrap())
}

/// The body rows of a note's first table.
fn rows(note: &str) -> Vec<&str> {
    note.lines()
        .skip_while(|line| !line.starts_with("| "))
        .take_while(|line| line.starts_with('|'))
        .filter(|line| line.starts_with("| ") && !line.starts_with("| ---"))
        .skip(1)
        .collect()
}

/// The body rows of the table under `## heading`.
fn section_rows<'n>(note: &'n str, heading: &str) -> Vec<&'n str> {
    let start = note
        .find(&format!("\n## {heading}\n"))
        .expect("the section exists");
    rows(&note[start..])
}

#[test]
fn a_case_gets_only_the_views_it_has_rows_for_plus_its_frontier_and_timeline() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = case(directory.path());
    let plan = plan(&mut store);
    let views: Vec<&str> = plan
        .entries()
        .iter()
        .map(|entry| entry.path().as_str())
        .filter(|path| path.contains("/Views/"))
        .collect();
    assert_eq!(
        views,
        [
            format!("Cases/{case_id}/Views/Epistemic Frontier.md"),
            format!("Cases/{case_id}/Views/Timeline.md"),
        ]
    );
    let timeline = view(&plan, case_id, "Timeline").unwrap();
    assert_eq!(rows(timeline).len(), 1, "{timeline}");
    assert!(timeline.contains(" | created | "), "{timeline}");
    // One note so far: the Overview, at its latest revision.
    let latest = section_rows(timeline, "Latest revision of each record");
    assert_eq!(latest.len(), 1, "{timeline}");
    assert!(latest[0].contains("/Overview\\|") && latest[0].contains(" | case | 1 | "));
    // No Base is planned under the default policy.
    assert!(!timeline.contains("## Native Base"));
    let overview = std::str::from_utf8(
        plan.entry(layout::case_overview(case_id).unwrap().as_str())
            .unwrap()
            .content(),
    )
    .unwrap();
    assert!(overview.contains("\n## Views\n"), "{overview}");
    assert!(
        overview.contains("/Views/Timeline|Timeline]]: 1 row\n"),
        "{overview}"
    );
}

#[test]
fn tables_are_bounded_in_record_id_order_with_an_overflow_link() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = case(directory.path());
    let mut ids = Vec::new();
    for n in 0..55 {
        let receipt = submit(
            &mut store,
            &format!("q{n}"),
            Some(case_id),
            json!({"op":"question.create",
                "payload":{"wording":format!("Question number {n}?"), "importance":"normal"}}),
        );
        ids.push(receipt.created[0].id);
    }
    ids.sort();
    let plan = plan(&mut store);
    let questions = view(&plan, case_id, "Questions").unwrap();
    let shown = rows(questions);
    assert_eq!(shown.len(), 50);
    for (row, id) in shown.iter().zip(&ids) {
        assert!(row.contains(&format!("/Questions/{id}\\|")), "{row}");
    }
    assert!(
        questions.contains(&format!(
            "\nShowing 50 of 55 questions. Every record is listed under Records on \
             [[Cases/{case_id}/Overview|Views fixture]].\n"
        )),
        "{questions}"
    );
    assert_eq!(
        questions
            .lines()
            .find_map(|line| line.strip_prefix("inv_rows: ")),
        Some("55")
    );
    // The timeline keeps the latest revisions, in acceptance order.
    let timeline = view(&plan, case_id, "Timeline").unwrap();
    let shown = rows(timeline);
    assert_eq!(shown.len(), 50);
    let sequences: Vec<u64> = shown
        .iter()
        .map(|row| row[2..].split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert!(sequences.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(sequences.last(), Some(&56));
    assert!(
        timeline.contains("\nShowing the latest 50 of 56 revisions."),
        "{timeline}"
    );
    // The note-per-row table is bounded the same way, in record ID order.
    let latest = section_rows(timeline, "Latest revision of each record");
    assert_eq!(latest.len(), 50);
    assert!(
        timeline.contains("\nShowing 50 of 56 notes. Every record is listed under Records on "),
        "{timeline}"
    );
}

#[test]
fn the_timeline_keeps_observed_and_recorded_times_apart_and_shows_corrections() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, case_id) = case(directory.path());
    let observation = |statement: &str| {
        json!({
            "value":{"kind":"text", "statement":statement},
            "basis":{"kind":"assertion", "assertion":"A reviewer read the dial.", "context":null},
            "extraction_method":"direct_read", "validity":"unknown", "quality":[],
            "limitations":[], "evidence_kind":"observational", "variables_changed":[],
            "observed_at":{"timestamp":{"state":"known","value":OBSERVED},
                "offset":{"state":"known","value":"+00:00"}, "clock_quality":"unverified"},
            "run_ref":null
        })
    };
    let added = submit(
        &mut store,
        "observe",
        Some(case_id),
        json!({"op":"observation.add", "payload":observation("The dial read 7.")}),
    );
    let id = added.created[0].id;
    let mut correction = observation("The dial read 1.");
    correction["observation_ref"] = json!({"id":id, "revision":1});
    correction["revision_reason"] = json!("The first reading mistook the needle.");
    submit(
        &mut store,
        "correct",
        Some(case_id),
        json!({"op":"observation.correct", "payload":correction}),
    );
    submit(
        &mut store,
        "retract",
        Some(case_id),
        json!({"op":"observation.retract", "payload":{
            "observation_ref":{"id":id, "revision":2},
            "revision_reason":"The dial is outside the declared scope."}}),
    );
    let plan = plan(&mut store);
    let timeline = view(&plan, case_id, "Timeline").unwrap();
    let rows: Vec<&str> = rows(timeline)
        .into_iter()
        .filter(|row| row.contains(&id.to_string()))
        .collect();
    assert_eq!(rows.len(), 3, "{timeline}");
    assert!(rows[0].contains(" | created | "), "{}", rows[0]);
    assert!(
        rows[1].contains("revised: correction of revision 1; reason: The first reading"),
        "{}",
        rows[1]
    );
    assert!(
        rows[2].contains("revised: `active` to `retracted`; reason: The dial is outside"),
        "{}",
        rows[2]
    );
    for row in &rows {
        // The claimed time is labelled with its clock; the recorded time is
        // the workspace's own and differs from it.
        assert!(
            row.contains(&format!("| observed `{OBSERVED}` (clock `unverified`) | `")),
            "{row}"
        );
        assert!(!row.contains(&format!("| `{OBSERVED}` |")), "{row}");
    }
    assert!(timeline.contains("it alone decides order"), "{timeline}");
}
