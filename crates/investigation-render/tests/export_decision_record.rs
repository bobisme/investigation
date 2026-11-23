//! The decision-record export (W11.05) on one small evaluative case built
//! through real operations, with every record cleared for the public except
//! where a test leaves it internal or raises it to restricted:
//!
//! - an option or a criterion that is withheld has no row or column and no
//!   trace;
//! - a cell whose comparison is withheld reads "not shown" for an audience
//!   that approves caveats and drops its option row for one that does not;
//! - Pareto status is listed only when the whole matrix is shown;
//! - every byte of every file is scanned for any withheld ID or marker.

use investigation_core::{
    identity::Id,
    wire::{CommitResult, Receipt},
};
use investigation_render::{
    Audience,
    export::{Caveat, ExportPlan, ExportSnapshot, Pin, kind, plan_export},
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const ACTOR: &str = "agent:export-policy-test";
const REVIEWER: &str = "human:export-reviewer";
const RESTRICTED: &str = "ZEBRACODE";
const INTERNAL: &str = "OTTERNOTE";
const PUBLIC: &str = "PUFFINLINE";
const B_MARK: &str = "BADGERCODE";

fn reference(id: Id, revision: u64) -> Value {
    json!({"id":id, "revision":revision})
}

fn submit_as(
    store: &mut Store,
    key: &str,
    actor: &str,
    case: Id,
    op: &str,
    payload: Value,
) -> Receipt {
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":key, "case_id":case,
        "actor":actor, "preconditions":[], "operations":[{"op":op, "payload":payload}]
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

fn create(store: &mut Store, key: &str, case: Id, op: &str, payload: Value) -> Id {
    submit_as(store, key, ACTOR, case, op, payload).created[0].id
}

fn current(store: &mut Store, case: Id, id: Id) -> Value {
    let replay = store.case(case).unwrap();
    let revision = replay.state.current(id).unwrap().header.revision.0;
    reference(id, revision)
}

fn raise(store: &mut Store, key: &str, case: Id, id: Id) {
    let subject = current(store, case, id);
    submit_as(
        store,
        key,
        ACTOR,
        case,
        "classification.raise",
        json!({"subject_ref":subject, "level":"restricted",
            "reason":format!("{RESTRICTED} raise reason {key}")}),
    );
}

/// Clear a record for the public audience by a reviewed downgrade.
fn clear(store: &mut Store, key: &str, case: Id, id: Id) {
    let subject = current(store, case, id);
    submit_as(
        store,
        key,
        REVIEWER,
        case,
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

fn observation(store: &mut Store, key: &str, case: Id, statement: String) -> Id {
    create(
        store,
        key,
        case,
        "observation.add",
        json!({
            "value":{"kind":"text", "statement":statement},
            "basis":{"kind":"assertion", "assertion":format!("{statement} assertion"), "context":null},
            "extraction_method":null, "validity":"valid", "quality":[], "limitations":[],
            "evidence_kind":"observational", "variables_changed":[], "observed_at":null
        }),
    )
}

/// What a test leaves above the public audience.
#[derive(Clone, Copy, Default)]
struct Hold {
    /// Option B stays internal (with its comparisons).
    option_b: bool,
    /// Only the comparison of A against the soft criterion stays internal.
    cell_a_soft: bool,
    /// Raise option B to restricted too.
    restrict_b: bool,
    /// Raise the comparison of A against the soft criterion to restricted.
    restrict_cell: bool,
    /// Raise the evidence every comparison cites to restricted.
    restrict_evidence: bool,
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: Store,
    case: Id,
    decision: Id,
    option_a: Id,
    option_b: Id,
    hard: Id,
    soft: Id,
    cells: Vec<(Id, Id, Id)>,
}

impl Fixture {
    fn cell(&self, option: Id, criterion: Id) -> Id {
        self.cells
            .iter()
            .find(|(o, c, _)| *o == option && *c == criterion)
            .unwrap()
            .2
    }
}

fn eligible(store: &mut Store, key: &str, case: Id, option: Id) {
    let option_ref = current(store, case, option);
    submit_as(
        store,
        key,
        ACTOR,
        case,
        "option.disposition",
        json!({"option_ref":option_ref, "lifecycle":"eligible", "reason":"Plausible.",
            "criterion_ref":null}),
    );
}

fn fixture(hold: Hold) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let s = &mut store;
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":"case", "case_id":null,
        "actor":ACTOR, "preconditions":[], "operations":[{"op":"case.create",
            "payload":{"title":format!("{INTERNAL} case title"), "mode":"evaluate"}}]
    });
    let case = s
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap()
        .created[0]
        .id;
    let question = create(
        s,
        "question",
        case,
        "question.create",
        json!({"wording":format!("{PUBLIC} question wording"), "importance":"normal"}),
    );
    clear(s, "clear-question", case, question);
    let question_ref = current(s, case, question);
    let option_a = create(
        s,
        "option-a",
        case,
        "option.create",
        json!({"question_ref":question_ref, "description":format!("{PUBLIC} option A"),
            "dependencies":[], "tradeoff_notes":[]}),
    );
    eligible(s, "eligible-a", case, option_a);
    clear(s, "clear-a", case, option_a);
    let option_b = create(
        s,
        "option-b",
        case,
        "option.create",
        json!({"question_ref":question_ref, "description":format!("{B_MARK} option B"),
            "dependencies":[], "tradeoff_notes":[]}),
    );
    eligible(s, "eligible-b", case, option_b);
    if !hold.option_b {
        clear(s, "clear-b", case, option_b);
    }
    if hold.restrict_b {
        raise(s, "raise-b", case, option_b);
    }
    let hard = create(
        s,
        "hard",
        case,
        "criterion.create",
        json!({"name":format!("{PUBLIC} hard criterion"), "kind":"hard",
            "scope":{"kind":"all_options", "question_ref":question_ref},
            "scale":{"unit":"ms", "description":"p99", "anchors":[]},
            "preferred_direction":"lower",
            "threshold":{"kind":"measured", "operator":"at_most",
                "value":{"decimal":"25", "unit":"ms"}}}),
    );
    clear(s, "clear-hard", case, hard);
    let soft = create(
        s,
        "soft",
        case,
        "criterion.create",
        json!({"name":format!("{PUBLIC} soft criterion"), "kind":"soft",
            "scope":{"kind":"all_options", "question_ref":question_ref},
            "scale":{"unit":"points", "description":"preference", "anchors":[]},
            "preferred_direction":"higher", "threshold":null}),
    );
    clear(s, "clear-soft", case, soft);
    let evidence = observation(s, "evidence", case, format!("{PUBLIC} evidence"));
    clear(s, "clear-evidence", case, evidence);
    let mut cells = Vec::new();
    for (option, option_name) in [(option_a, "a"), (option_b, "b")] {
        for (criterion, criterion_name) in [(hard, "hard"), (soft, "soft")] {
            let option_ref = current(s, case, option);
            let criterion_ref = current(s, case, criterion);
            let key = format!("cmp-{option_name}-{criterion_name}");
            let comparison = create(
                s,
                &key,
                case,
                "comparison.add",
                json!({"option_ref":option_ref, "criterion_ref":criterion_ref,
                    "outcome":"unknown", "method":"inspection",
                    "basis":{"kind":"unknown", "reason":format!("{PUBLIC} not measured")},
                    "evidence_refs":[reference(evidence, 1)], "assumption_refs":[],
                    "uncertainty":"No measurement yet", "rationale":"Await measurement"}),
            );
            let held = (option == option_b && hold.option_b)
                || (option == option_a && criterion == soft && hold.cell_a_soft);
            if !held {
                clear(s, &format!("clear-{key}"), case, comparison);
            }
            if option == option_a && criterion == soft && hold.restrict_cell {
                raise(s, "raise-cell", case, comparison);
            }
            cells.push((option, criterion, comparison));
        }
    }
    if hold.restrict_evidence {
        raise(s, "raise-evidence", case, evidence);
    }
    let question_ref = current(s, case, question);
    let option_ref = current(s, case, option_a);
    let decision = create(
        s,
        "decision",
        case,
        "decision.propose",
        json!({
            "question_ref":question_ref,
            "choice":{"kind":"select", "option_ref":option_ref},
            "compared_alternatives":[option_ref],
            "comparison_refs":[], "hard_conditions":[], "evidence_refs":[],
            "assumption_refs":[], "tradeoffs":[format!("{PUBLIC} tradeoff")],
            "consequences":[format!("{PUBLIC} consequence")],
            "reopen_trigger":format!("{PUBLIC} reopen trigger"), "reversal_conditions":[]
        }),
    );
    clear(s, "clear-decision", case, decision);
    Fixture {
        _directory: directory,
        store,
        case,
        decision,
        option_a,
        option_b,
        hard,
        soft,
        cells,
    }
}

fn plan(fixture: &mut Fixture, audience: Audience) -> ExportPlan {
    let snapshot = ExportSnapshot::read(&mut fixture.store, fixture.case, &Pin::default()).unwrap();
    plan_export(kind("decision-record").unwrap(), &snapshot, audience).unwrap()
}

fn record(plan: &ExportPlan) -> String {
    String::from_utf8(
        plan.files()
            .entry("decision-record.md")
            .unwrap()
            .content()
            .to_vec(),
    )
    .unwrap()
}

/// Every byte of every file, the manifest and the names, as one string.
fn everything(plan: &ExportPlan) -> String {
    let mut out = String::new();
    for entry in plan.files().entries() {
        out.push_str(entry.path().as_str());
        out.push_str(std::str::from_utf8(entry.content()).unwrap());
    }
    out.push_str(std::str::from_utf8(plan.manifest_bytes()).unwrap());
    out
}

fn matrix_rows(record: &str) -> Vec<&str> {
    let start = record.find("### Comparison matrix").unwrap();
    record[start..]
        .lines()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .skip(2)
        .collect()
}

#[test]
fn every_audience_gets_the_adr_sections_in_order_and_the_same_bytes_twice() {
    for audience in [Audience::Restricted, Audience::Internal, Audience::Public] {
        let mut fixture = fixture(Hold::default());
        let first = plan(&mut fixture, audience);
        let second = plan(&mut fixture, audience);
        assert_eq!(first.manifest_bytes(), second.manifest_bytes());
        assert_eq!(record(&first), record(&second));
        let text = record(&first);
        let mut at = 0;
        for heading in [
            "# Decision record: ",
            "## Basis",
            "## Context",
            "## Decision 1: ",
            "### Status",
            "### Options considered",
            "### Hard constraints",
            "### Criteria",
            "### Hard conditions to verify",
            "### Comparison matrix",
            "### Choice",
            "### Expected outcome and reopening",
            "### Evidence and assumptions",
            "### Warnings",
            "## Limits",
        ] {
            let found = text[at..].find(heading);
            assert!(
                found.is_some(),
                "{audience:?}: {heading} after {at}\n{text}"
            );
            at += found.unwrap();
        }
        assert!(text.contains(&format!("{PUBLIC} reopen trigger")), "{text}");
        assert!(text.contains("preference"), "soft criteria are labelled");
        assert!(!text.contains("overall score:"), "no score is computed");
        let rows = matrix_rows(&text);
        assert_eq!(
            rows.len(),
            2,
            "{audience:?}: both options are shown\n{text}"
        );
        assert!(
            text.contains("Pareto status"),
            "{audience:?}: whole matrix shown"
        );
    }
}

#[test]
fn a_withheld_option_has_no_row_for_the_public_and_no_pareto_status() {
    let mut fixture = fixture(Hold {
        option_b: true,
        ..Hold::default()
    });
    let plan = plan(&mut fixture, Audience::Public);
    let text = record(&plan);
    assert!(!plan.complete());
    assert_eq!(matrix_rows(&text).len(), 1, "{text}");
    assert!(!text.contains("Pareto status ("), "{text}");
    assert!(!text.contains("Caveat"), "{text}");
    let everything = everything(&plan);
    for withheld in [
        fixture.option_b.to_string(),
        fixture.cell(fixture.option_b, fixture.hard).to_string(),
        fixture.cell(fixture.option_b, fixture.soft).to_string(),
        B_MARK.to_owned(),
        INTERNAL.to_owned(),
    ] {
        assert!(!everything.contains(&withheld), "{withheld} leaked");
    }
    assert!(text.contains(&fixture.decision.to_string()));
    assert!(text.contains(&format!("{PUBLIC} option A")));
}

#[test]
fn a_withheld_comparison_drops_its_row_for_the_public_and_reads_not_shown_for_internal() {
    let hold = Hold {
        cell_a_soft: true,
        ..Hold::default()
    };
    let mut fixture = fixture(hold);
    let hidden = fixture.cell(fixture.option_a, fixture.soft).to_string();
    let public = plan(&mut fixture, Audience::Public);
    let text = record(&public);
    let rows = matrix_rows(&text);
    assert_eq!(rows.len(), 1, "cascading omission of the row\n{text}");
    assert!(rows[0].contains(B_MARK), "{text}");
    assert!(!everything(&public).contains(&hidden));
    assert!(rows.iter().all(|row| !row.contains("not shown")), "{text}");
    assert!(!text.contains("Pareto status ("));

    // Raised above internal, the cell is withheld from the internal audience
    // too; it stays in the row as "not shown" and nothing names it.
    let mut fixture = self::fixture(Hold {
        restrict_cell: true,
        ..hold
    });
    let hidden = fixture.cell(fixture.option_a, fixture.soft).to_string();
    let internal = plan(&mut fixture, Audience::Internal);
    let text = record(&internal);
    let rows = matrix_rows(&text);
    assert_eq!(rows.len(), 2, "{text}");
    let row_a = rows.iter().find(|row| row.contains(PUBLIC)).unwrap();
    assert!(row_a.contains("not shown"), "{row_a}");
    assert!(!everything(&internal).contains(&hidden));
    assert!(!text.contains("Pareto status ("));
    // The restricted audience sees the cell.
    let restricted = plan(&mut fixture, Audience::Restricted);
    let text = record(&restricted);
    assert!(text.contains(&hidden), "{text}");
    assert!(!text.contains("not shown"));
    assert!(text.contains("Pareto status ("));
}

#[test]
fn a_restricted_option_has_no_row_for_internal_and_leaves_the_decision_uncaveated() {
    let mut fixture = fixture(Hold {
        option_b: true,
        restrict_b: true,
        ..Hold::default()
    });
    let internal = plan(&mut fixture, Audience::Internal);
    let text = record(&internal);
    assert_eq!(matrix_rows(&text).len(), 1, "{text}");
    let everything = everything(&internal);
    assert!(!everything.contains(&fixture.option_b.to_string()));
    assert!(!everything.contains(B_MARK));
    assert!(!everything.contains("ZEBRACODE"));
    assert!(!text.contains("Pareto status ("));
    assert!(!internal.complete());
    assert!(
        internal
            .caveated()
            .iter()
            .all(|caveated| caveated.id != fixture.decision),
        "the decision does not rest on B: {:?}",
        internal.caveated()
    );
    // B's comparisons rest on B: they carry the caveat wherever they show,
    // here in the notes under the matrix, while B's row is not shown.
    assert!(!text.contains("- Caveat: "), "{text}");
}

#[test]
fn comparisons_resting_on_withheld_evidence_carry_the_caveat_or_vanish() {
    let hold = Hold {
        restrict_evidence: true,
        ..Hold::default()
    };
    let mut fixture = fixture(hold);
    let internal = plan(&mut fixture, Audience::Internal);
    let text = record(&internal);
    assert_eq!(matrix_rows(&text).len(), 2, "{text}");
    let notes = text.split("Caveats on cells:").nth(1).unwrap();
    let notes = notes.split("Hard failures:").next().unwrap();
    assert_eq!(notes.matches("  - Caveat: ").count(), 4, "{notes}");
    assert!(notes.contains(Caveat::BasisWithheld.text()), "{notes}");
    assert!(
        matrix_rows(&text)
            .iter()
            .all(|row| !row.contains(" evidence")),
        "withheld evidence is not counted\n{text}"
    );

    // The public audience gets no caveat: the comparisons are withheld, so
    // the matrix keeps no row, and the decision itself is untouched.
    let public = plan(&mut fixture, Audience::Public);
    let text = record(&public);
    assert!(matrix_rows(&text).is_empty(), "{text}");
    assert!(!text.contains("Caveat"), "{text}");
    for (_, _, comparison) in &fixture.cells {
        assert!(!everything(&public).contains(&comparison.to_string()));
    }
    assert!(text.contains(&fixture.decision.to_string()));
}
