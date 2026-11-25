//! The `case-report` and `analysis-report` exports (W11.04).
//!
//! - Scenarios A, C and D: the checked-in workspaces under
//!   `fixtures/vault-goldens/`, copied to a temporary folder and exported for
//!   every audience. Their records are `internal`, so restricted and internal
//!   exports are complete and public withholds every record.
//! - A small causal case built through real operations, in the shape of
//!   Scenario B's measurements: repeated runs with typed measurement fields,
//!   a causal test, a validated conclusion whose support is retracted
//!   afterwards (a stale review basis), a conclusion with confidence and
//!   likelihood, and a conclusion resting on a restricted observation.
//!
//! Every report is checked for its fixed section order, the elements §15.4
//! requires, byte-identical output for the same inputs, and that nothing
//! withheld appears in any byte or file name.

use std::{fs, path::Path};

use investigation_core::{
    basis::basis_digest,
    command::RevisionRef,
    identity::Id,
    measurement_flags,
    review_ops::required_read_set,
    wire::{CommitResult, Receipt},
};
use investigation_render::{
    Audience, VerifiedSnapshot,
    export::{
        Caveat, ExportPlan, ExportSnapshot, IdentifierMode, Pin, Retained, kind, plan_export,
        plan_export_remap,
    },
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const KINDS: [&str; 2] = ["case-report", "analysis-report"];
const AUDIENCES: [Audience; 3] = [Audience::Public, Audience::Internal, Audience::Restricted];
const SCENARIOS: [&str; 4] = ["scenario-a", "scenario-b", "scenario-c", "scenario-d"];

const CASE_SECTIONS: &[&str] = &[
    "Basis",
    "Status and outcome",
    "Objective",
    "Methods",
    "Original observations",
    "Alternatives",
    "Supported conclusions",
    "Decisions",
    "Human files relied on",
    "Limits",
    "Open issues",
    "Methodological retrospective",
];
const ANALYSIS_SECTIONS: &[&str] = &[
    "Basis",
    "Status and outcome",
    "Question",
    "Reproduction conditions",
    "Baselines",
    "Runs and controls",
    "Uncertainty",
    "Root-cause or measurement assessment",
    "Limits",
    "Open issues",
];

// ------------------------------------------------------------- Scenarios

/// Directories a workspace must have; git keeps no empty directory.
const REQUIRED: [&str; 13] = [
    "state/cases",
    "state/events",
    "state/blobs/sha256",
    "state/checkpoints",
    "state/staging",
    "state/locks",
    "state/tmp",
    "cache",
    "local",
    "exports",
    "vault/Notes",
    "vault/Boards",
    "vault/Inbox",
];

fn copy(from: &Path, to: &Path, reverse: bool) {
    fs::create_dir_all(to).unwrap();
    let mut entries: Vec<_> = fs::read_dir(from)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    if reverse {
        entries.reverse();
    }
    for path in entries {
        let destination = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy(&path, &destination, reverse);
        } else {
            fs::copy(&path, &destination).unwrap();
        }
    }
}

/// A copy of one checked-in scenario workspace, opened.
fn scenario(name: &str, target: &Path, reverse: bool) -> Store {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/vault-goldens")
        .join(name)
        .join("workspace");
    copy(&source, target, reverse);
    for directory in REQUIRED {
        fs::create_dir_all(target.join(directory)).unwrap();
    }
    Store::open(Workspace::open(target).unwrap())
}

fn cases(store: &Store) -> Vec<Id> {
    let mut cases: Vec<Id> = fs::read_dir(store.workspace().root().join("state/cases"))
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .file_name()
                .into_string()
                .unwrap()
                .parse()
                .unwrap()
        })
        .collect();
    cases.sort();
    cases
}

fn plan(store: &mut Store, case: Id, name: &str, audience: Audience) -> ExportPlan {
    let snapshot = ExportSnapshot::read(store, case, &Pin::default()).unwrap();
    plan_export(kind(name).unwrap(), &snapshot, audience).unwrap()
}

/// The report's text with canonical IDs: a remapped (public) export is
/// restored through the publisher's mapping, so a check for a canonical ID
/// means the same thing in every audience. [`raw_text`] is what is written.
fn text(plan: &ExportPlan) -> String {
    String::from_utf8(plan.identifiers().restore(raw_text(plan).as_bytes())).unwrap()
}

fn raw_text(plan: &ExportPlan) -> String {
    assert_eq!(plan.files().entries().len(), 1);
    let name = format!("{}.md", plan.kind().name);
    String::from_utf8(plan.files().entry(&name).unwrap().content().to_vec()).unwrap()
}

/// [`bytes`] restored to canonical IDs, for leak checks by canonical ID.
fn restored(plan: &ExportPlan) -> Vec<(String, String)> {
    let identifiers = plan.identifiers();
    let restore = |text: &str| String::from_utf8(identifiers.restore(text.as_bytes())).unwrap();
    bytes(plan)
        .into_iter()
        .map(|(path, body)| (restore(&path), restore(&body)))
        .collect()
}

/// Every file name and its text, the manifest included.
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

/// The `## ` headings of a report, in order.
fn sections(report: &str) -> Vec<&str> {
    report
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .collect()
}

fn assert_shape(plan: &ExportPlan, label: &str) {
    let report = text(plan);
    let expected = match plan.kind().name {
        "case-report" => CASE_SECTIONS,
        _ => ANALYSIS_SECTIONS,
    };
    assert_eq!(sections(&report), expected, "{label}");
    assert!(report.starts_with("# "), "{label}");
    assert_eq!(
        plan.manifest().versions.renderer,
        match plan.kind().name {
            "case-report" => "inv.export.case-report.v6",
            _ => "inv.export.analysis-report.v1",
        }
    );
    // Confidence and likelihood never share a line, let alone a sentence.
    for line in report.lines() {
        assert!(
            !(line.contains("Confidence") && line.contains("Likelihood")),
            "{label}: {line}"
        );
    }
    // A recorded validation or acceptance is never printed on its own when
    // the effective state differs.
    for line in report.lines() {
        if line.contains("(recorded `validated`") || line.contains("(recorded `accepted`") {
            let effective_matches = line.contains("effective `validated` (recorded `validated`)")
                || line.contains("effective `accepted` (recorded `accepted`)");
            assert!(
                effective_matches || line.contains("which does not hold as it stands"),
                "{label}: {line}"
            );
        }
    }
}

/// No withheld record is named anywhere in the export, nor any of `above`.
fn assert_nothing_withheld_leaks(plan: &ExportPlan, case: Id, above: &[&str]) {
    let files = restored(plan);
    for exclusion in plan.exclusions() {
        if exclusion.id == case {
            continue; // The case ID names the snapshot the export is of.
        }
        let id = exclusion.id.to_string();
        for (path, body) in &files {
            assert!(!path.contains(&id), "{path} names withheld {id}");
            assert!(!body.contains(&id), "{path} names withheld {id}");
        }
    }
    for needle in above {
        for (path, body) in &files {
            assert!(
                !path.contains(needle) && !body.contains(needle),
                "{path}: {needle}"
            );
        }
    }
}

fn each_scenario_export(audience: Audience, check: impl Fn(&str, Id, &ExportPlan)) {
    let directory = tempfile::tempdir().unwrap();
    for name in SCENARIOS {
        let mut store = scenario(name, &directory.path().join(name), false);
        for case in cases(&store) {
            for report in KINDS {
                let plan = plan(&mut store, case, report, audience);
                assert_shape(&plan, &format!("{name} {case} {report}"));
                check(name, case, &plan);
            }
        }
    }
}

#[test]
fn restricted_reports_of_scenarios_a_c_d_are_complete_and_carry_no_caveat() {
    each_scenario_export(Audience::Restricted, |name, case, plan| {
        assert!(plan.complete(), "{name} {case}: {:?}", plan.exclusions());
        assert!(plan.caveated().is_empty());
        let report = text(plan);
        assert!(!report.contains("- Caveat: "), "{name} {case}");
        assert!(report.contains("| Audience | `restricted` |"));
    });
}

#[test]
fn internal_reports_of_scenarios_a_c_d_are_complete_and_carry_no_caveat() {
    each_scenario_export(Audience::Internal, |name, case, plan| {
        assert!(plan.complete(), "{name} {case}: {:?}", plan.exclusions());
        assert!(plan.caveated().is_empty());
        let report = text(plan);
        assert!(!report.contains("- Caveat: "), "{name} {case}");
        assert!(!report.contains("Not included for this audience"));
    });
}

#[test]
fn public_reports_of_scenarios_a_c_d_withhold_every_internal_record() {
    let directory = tempfile::tempdir().unwrap();
    for name in SCENARIOS {
        let mut store = scenario(name, &directory.path().join(name), false);
        for case in cases(&store) {
            let replay = store.case(case).unwrap();
            let titles: Vec<String> = replay
                .state
                .records()
                .filter_map(|record| record.body.get("title").and_then(Value::as_str))
                .map(str::to_owned)
                .collect();
            for report in KINDS {
                let plan = plan(&mut store, case, report, Audience::Public);
                let label = format!("{name} {case} {report}");
                assert_shape(&plan, &label);
                assert!(!plan.complete(), "{label}");
                assert!(plan.caveated().is_empty(), "{label}");
                let text = text(&plan);
                assert!(
                    text.contains("- Not included for this audience."),
                    "{label}"
                );
                assert!(!text.contains("- Caveat: "), "{label}");
                // Every record of these scenarios is internal: nothing but the
                // case ID that names the snapshot is shown.
                for record in replay.state.records() {
                    if record.header.id != case {
                        assert!(!text.contains(&record.header.id.to_string()), "{label}");
                    }
                }
                let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
                assert_nothing_withheld_leaks(&plan, case, &titles);
            }
        }
    }
}

#[test]
fn reports_of_scenarios_a_c_d_are_byte_identical_for_the_same_inputs() {
    let directory = tempfile::tempdir().unwrap();
    for name in SCENARIOS {
        let mut first = scenario(name, &directory.path().join(name).join("a"), false);
        let mut second = scenario(name, &directory.path().join(name).join("b"), true);
        for case in cases(&first) {
            for report in KINDS {
                for audience in AUDIENCES {
                    let one = plan(&mut first, case, report, audience);
                    let again = plan(&mut first, case, report, audience);
                    let other = plan(&mut second, case, report, audience);
                    assert_eq!(bytes(&one), bytes(&again), "{name} {case} {report}");
                    assert_eq!(bytes(&one), bytes(&other), "{name} {case} {report}");
                    assert_eq!(one.manifest().export_id, other.manifest().export_id);
                }
            }
        }
    }
}

fn exported(name: &str, report: &str) -> Vec<(Id, String)> {
    let directory = tempfile::tempdir().unwrap();
    let mut store = scenario(name, &directory.path().join(name), false);
    cases(&store)
        .into_iter()
        .map(|case| {
            (
                case,
                text(&plan(&mut store, case, report, Audience::Internal)),
            )
        })
        .collect()
}

/// The line naming `needle` and every line under it that is indented
/// deeper, up to the next line at the same or a shallower depth.
fn block<'r>(report: &'r str, needle: &str) -> String {
    let lines: Vec<&'r str> = report.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} not in report"));
    let depth = |line: &str| line.len() - line.trim_start().len();
    let base = depth(lines[start]);
    let mut out = vec![lines[start]];
    out.extend(
        lines[start + 1..]
            .iter()
            .take_while(|line| line.is_empty() || depth(line) > base)
            .copied(),
    );
    out.join("\n")
}

#[test]
fn scenario_a_case_report_contains_every_required_element() {
    let reports = exported("scenario-a", "case-report");
    let main = &reports
        .iter()
        .find(|(_, text)| text.contains("synthetic lookup stall"))
        .unwrap()
        .1;
    // Objective and methods.
    assert!(main.contains("- Mode: `explain`"));
    assert!(main.contains("`q_01a20751-8e7a-7c71-b14a-90fde312ac50`"));
    let inquiry = block(main, "`inq_01a20751-8ed6-7190-955a-f2935f5f83c6`");
    for element in ["- Purpose: ", "- Method: ", "- Control: ", "- Limit: "] {
        assert!(inquiry.contains(element), "{element}\n{inquiry}");
    }
    assert!(main.contains("`run_01a20751-9162-77d2-8280-ddfec6c3c8df`"));
    // Original observations: source, selector, quote state, inert excerpt.
    let quoted = block(main, "- `obs_01a20751-8ff0-71b3-8533-fa18bea0abb8`");
    assert!(
        quoted.contains(
            "from source `src_01a20751-8f13-7960-9b4f-64bc7c2c8abc` revision 1, selector \
             `byte_range` with validation `verified`, quote state `verified`"
        ),
        "{quoted}"
    );
    assert!(
        quoted.contains("    ```text\n    callback returned after 28 seconds\n    ```"),
        "{quoted}"
    );
    // Alternatives.
    let alternatives = main.split("## Alternatives").nth(1).unwrap();
    for hypothesis in [
        "hyp_01a20751-8e9a-7312-8e8c-2d318d46c4eb",
        "hyp_01a20751-8eb8-7f03-b74b-ad5633c8ef75",
    ] {
        assert!(alternatives.contains(hypothesis));
    }
    // Supported conclusions: every §15.3 element.
    let validated = block(main, "- `concl_01a20751-9308-7c52-b557-693825716707`");
    assert!(validated.contains("effective `validated` (recorded `validated`)"));
    for element in [
        "  - Statement: ",
        "  - Applicability: ",
        "  - Significance: ",
        "  - Evidence:\n",
        "selector `byte_range` on `art_01a20751-8f14-7590-998a-0df43ddaebdb` revision 1, quote `verified`",
        "  - Alternatives addressed:\n    - `hyp_01a20751-8eb8-7f03-b74b-ad5633c8ef75` revision 1",
        "  - Strongest contradiction: the recorded alternative `hyp_01a20751-8eb8-7f03-b74b-ad5633c8ef75`",
        "  - Uncertainty:\n    - Recorded caveat: ",
        "  - Validating review: `rev_01a20751-93b1-7663-9a6a-e16094fc573e` revision 1, review state `current`",
        "  - Dissent and other reviews: ",
        "  - What would invalidate it:\n",
        "  - Causal account:",
        "      - Baseline: `obs_01a20751-9066-7e50-945b-fa7cf8f304ac` revision 1",
        "      - Failure boundary: `at_least` `25000` `ms`",
    ] {
        assert!(validated.contains(element), "{element}\n{validated}");
    }
    assert!(!validated.contains("severity score"));
    let candidate = block(main, "- `concl_01a20751-91b0-7003-9309-18ce4a1e3796`");
    assert!(
        candidate
            .contains("- Validating review: none accepted; the recorded claim is not validated")
    );
    // Limits, open issues and retrospective highlights with exact references.
    assert!(main.contains(
        "- Reopen condition: A comparable scheduler\\-only toggle changes the result\\."
    ));
    let open = main.split("## Open issues").nth(1).unwrap();
    assert!(open.contains("- Open questions:"));
    assert!(
        open.contains(
            "- Residual unknowns recorded by `retro_01a20751-94b8-71f2-bbeb-59aa7355f091`"
        )
    );
    let retrospective = main
        .split("## Methodological retrospective")
        .nth(1)
        .unwrap();
    assert!(retrospective.contains(
        "`retro_01a20751-94b8-71f2-bbeb-59aa7355f091` Retrospective at sequence 28: revision 1, \
         account state `current`, describes the case at sequence 28"
    ));
    assert!(retrospective.contains("  - Effective step: "));
    assert!(retrospective.contains(
        "  - Invalid or misleading step: The unmapped clock could not order events\\. (support \
         `run_01a20751-9162-77d2-8280-ddfec6c3c8df`)"
    ));
    assert!(retrospective.contains("  - No reusable lesson: "));
}

#[test]
fn scenario_a_analysis_report_contains_every_required_element() {
    let reports = exported("scenario-a", "analysis-report");
    let main = &reports
        .iter()
        .find(|(_, text)| text.contains("synthetic lookup stall"))
        .unwrap()
        .1;
    let conditions = main.split("## Reproduction conditions").nth(1).unwrap();
    assert!(conditions.contains("`inq_01a20751-8ed6-7190-955a-f2935f5f83c6`"));
    assert!(conditions.contains(
        "- Run environment: Unaligned wall clock (runs `run_01a20751-9162-77d2-8280-ddfec6c3c8df`)"
    ));
    assert!(conditions.contains("- Variable changed: release\\_before\\_await (runs "));
    let baselines = main.split("## Baselines").nth(1).unwrap();
    assert!(baselines.contains(
        "- `obs_01a20751-9066-7e50-945b-fa7cf8f304ac` Measured 28007 ms: baseline of the causal \
         test of `release_before_await`"
    ));
    assert!(baselines.contains("- Runs that record no changed variable:"));
    let runs = main.split("## Runs and controls").nth(1).unwrap();
    let invalid = block(runs, "- `run_01a20751-9162-77d2-8280-ddfec6c3c8df`");
    for element in [
        "  - Execution status: `completed`",
        "  - Evidence completeness: `complete`",
        "  - Evidence validity: `invalid`; its results must not ground claims",
        "  - Evidence kind: `observational`",
        "  - Observations from this run: none listed",
    ] {
        assert!(invalid.contains(element), "{element}\n{invalid}");
    }
    let treatment = block(runs, "- `run_01a20751-9089-7dc3-a58e-ccac5e838388`");
    assert!(treatment.contains("  - Variables changed: release\\_before\\_await"));
    assert!(treatment.contains("    - `obs_01a20751-90b0-7583-8016-b35360f4629f` Measured 102 ms"));
    let uncertainty = main.split("## Uncertainty").nth(1).unwrap();
    assert!(uncertainty.contains("`run_01a20751-9162-77d2-8280-ddfec6c3c8df`"));
    let assessment = main
        .split("## Root-cause or measurement assessment")
        .nth(1)
        .unwrap();
    assert!(assessment.contains("### Hypotheses"));
    let validated = block(assessment, "- `concl_01a20751-9308-7c52-b557-693825716707`");
    for element in [
        "Baseline: `obs_01a20751-9066-7e50-945b-fa7cf8f304ac`",
        "Treatment: `obs_01a20751-90b0-7583-8016-b35360f4629f`",
        "Reversal: `obs_01a20751-9140-7d72-bbd2-181cd705230d`",
        "- Uncertainty:",
        "- Validating review: `rev_01a20751-93b1-7663-9a6a-e16094fc573e`",
    ] {
        assert!(validated.contains(element), "{element}\n{validated}");
    }
}

#[test]
fn an_inconclusive_case_says_so_with_its_reopen_condition() {
    for report in KINDS {
        let a = exported("scenario-a", report);
        let sample = &a
            .iter()
            .find(|(_, text)| text.contains("matched production sample"))
            .unwrap()
            .1;
        let status = sample.split("## Status and outcome").nth(1).unwrap();
        assert!(status.contains("- Outcome: `inconclusive`. This case is inconclusive"));
        assert!(status.contains("- Reopen condition: A compatible sample is supplied\\."));
        assert!(status.contains("- Retrospective waived: "));

        let c = exported("scenario-c", report);
        let status = c[0].1.split("## Status and outcome").nth(1).unwrap();
        assert!(status.contains("This case is inconclusive"), "{report}");
        assert!(status.contains(
            "- Reopen condition: A reviewed comparison addresses the 60\\-second threshold"
        ));

        // An open case says it is not settled.
        let d = exported("scenario-d", report);
        assert!(
            d[0].1
                .contains("- Outcome: none yet. The case is not closed")
        );
    }
}

#[test]
fn scenario_c_marks_superseded_decisions_and_stale_acceptance() {
    let report = &exported("scenario-c", "case-report")[0].1;
    let superseded = block(report, "- `dec_01a20751-9bab-74e0-aeda-0e5c729cb2bb`");
    assert!(superseded.contains("effective `superseded` (recorded `superseded`)"));
    assert!(superseded.contains("superseded by `dec_01a20751-a224-7070-bc1a-61782ce70dba`"));
    assert!(superseded.contains("- Accepting review: `rev_01a20751-9c0a-7282-a488-cfc335111820` revision 1, review state `stale`"));
    let successor = block(report, "- `dec_01a20751-a224-7070-bc1a-61782ce70dba`");
    assert!(successor.contains("supersedes `dec_01a20751-9bab-74e0-aeda-0e5c729cb2bb`"));
    assert!(report.contains("A decision records a choice."));
    let open = report.split("## Open issues").nth(1).unwrap();
    assert!(open.contains("effective `needs_review`"));
}

#[test]
fn scenario_d_annotates_withdrawn_inputs_where_they_are_cited() {
    let report = &exported("scenario-d", "case-report")[0].1;
    let withdrawn = block(report, "- `obs_01a20751-b947-7f80-831f-df5b9f8ba215`");
    assert!(
        withdrawn
            .contains("annotated: effective `needs_review`, evidentiary use `partially_withdrawn`"),
        "{withdrawn}"
    );
    assert!(withdrawn.contains("```text\n"));
    let validated = block(report, "- `concl_01a20751-c413-7553-ba88-ddc5afdb0ca4`");
    assert!(validated.contains("    - Confidence: `low`."));
    assert!(validated.contains("  - What would invalidate it:\n    - A later erratum"));
    // The stale assessment is annotated in place where the conclusion
    // cites it.
    assert!(
        validated.contains(
            "`assess_01a20751-bd05-7942-ad90-0e3a23510e0b` revision 1: The specification, a trade \
         article and a blog all state 64 frames.; status `current`; annotated: effective \
         `needs_review`"
        ),
        "{validated}"
    );
}

// --------------------------------------------------- A causal case, built

const ACTOR: &str = "agent:export-reports-test";
const RESTRICTED: &str = "HERONMARK";
const QUOTE: &str = "p99 went from 12 ms to 41 ms ```";

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
    submit(store, key, Some(case), op, payload, json!([])).created[0].id
}

fn current(
    store: &mut Store,
    case: Id,
    id: Id,
) -> (investigation_core::state::Record, Vec<RevisionRef>) {
    let snapshot = VerifiedSnapshot::read(store).unwrap();
    let case = snapshot.case(case).unwrap();
    let record = case.state().current(id).unwrap().clone();
    let read_set = required_read_set(case.state(), &record).unwrap();
    (record, read_set)
}

fn run(
    store: &mut Store,
    key: &str,
    case: Id,
    changed: &[&str],
    validity: &str,
    extra: Value,
) -> Id {
    let mut payload = json!({
        "intent":{"kind":"exploration", "intent":format!("{key} intent")},
        "producer":"agent:producer", "artifact_refs":[],
        "environment":"Synthetic build 9 on the warm path",
        "execution_status":"completed", "evidence_completeness":"complete",
        "evidence_validity":validity,
        "evidence_kind":if changed.is_empty() { "observational" } else { "interventional" },
        "variables_changed":changed
    });
    for (key, value) in extra.as_object().unwrap() {
        payload[key] = value.clone();
    }
    create(store, key, case, "run.record", payload)
}

/// A measurement read from one line of the retained capture.
fn measured(
    store: &mut Store,
    key: &str,
    case: Id,
    (line, value): (u64, &str),
    run: Id,
    changed: &[&str],
    capture: &(Id, Value),
) -> Id {
    create(
        store,
        key,
        case,
        "observation.add",
        json!({
            "value":{"kind":"measurement", "value":value, "unit":"ms"},
            "basis":{"kind":"source", "artifact_ref":reference(capture.0, 1),
                "selector":{"schema_version":"inv.selector.v1", "artifact_digest":capture.1["digest"],
                    "selector":{"type":"text_lines", "start_line":line, "end_line":line,
                        "encoding":"utf-8", "parser_version":"inv.text.utf8.v1"}},
                "text_quote":null},
            "extraction_method":"Read the p99 from the capture line.", "validity":"valid",
            "quality":[], "limitations":[format!("{key} covers a synthetic workload only.")],
            "evidence_kind":if changed.is_empty() { "observational" } else { "interventional" },
            "variables_changed":changed, "observed_at":null, "run_ref":reference(run, 1)
        }),
    )
}

fn conclusion(store: &mut Store, key: &str, case: Id, support: &[Id], extra: Value) -> Id {
    let mut payload = json!({
        "statement":format!("{key} statement"), "scope":"The synthetic warm path only.",
        "significance":"Keep the arena on the warm path.",
        "support_refs":support.iter().map(|id| reference(*id, 1)).collect::<Vec<_>>(),
        "contradiction_refs":[], "caveats":[format!("{key} recorded caveat")],
        "causal_factors":[], "confidence":null, "likelihood":null
    });
    for (key, value) in extra.as_object().unwrap() {
        payload[key] = value.clone();
    }
    create(store, key, case, "conclusion.create", payload)
}

struct Causal {
    _directory: tempfile::TempDir,
    store: Store,
    case: Id,
    validated: Id,
    retracted: Id,
    judged: Id,
    resting: Id,
    secret: Id,
}

fn causal() -> Causal {
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
    let s = &mut store;
    let case = submit(
        s,
        "case",
        None,
        "case.create",
        json!({"title":"Why did p99 rise after the allocator change?", "mode":"explain"}),
        json!([]),
    )
    .created[0]
        .id;
    // A quoted capture backs the validated conclusion with verified bytes.
    let path = root.join("capture.txt");
    fs::write(&path, format!("start\n{QUOTE}\n41\n40\n12\n42\n77\n")).unwrap();
    let manifest = json!({
        "schema_version":"inv.ingest.v1", "command_id":"capture", "actor":ACTOR, "case_id":case,
        "source":{"title":"capture", "format":"text/plain"},
        "paths":[path], "classification":"internal", "extract_text":true
    });
    let capture = s
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap_or_else(|failure| panic!("{failure:?}"));
    let capture = serde_json::to_value(&capture.items[0]).unwrap();
    let derivative: Id = capture["derivative_id"].as_str().unwrap().parse().unwrap();
    let capture = (derivative, capture);
    let question = create(
        s,
        "question",
        case,
        "question.create",
        json!({"wording":"Did the allocator change raise p99?", "importance":"normal"}),
    );
    let factor = create(
        s,
        "factor",
        case,
        "hypothesis.create",
        json!({"question_ref":reference(question, 1), "statement":"The allocator change raises p99.",
            "assumption_refs":[], "rival_refs":[], "applicability":"Warm path",
            "possible_falsifiers":["p99 stays high with the old allocator."]}),
    );
    // A repetition series: two repeats sharing one setup run, one of them
    // with incomplete evidence.
    let setup = run(s, "setup", case, &[], "valid", json!({}));
    let repeat = |id: &str| {
        json!({"series":"warm-path", "acquisition":{"namespace":"bench", "external_id":id},
            "setup_refs":[reference(setup, 1)]})
    };
    let fields = json!({
        "workload":{"state":"known", "value":"read90-write10"},
        "sample_count":{"state":"known", "value":95},
        "statistic":{"state":"known", "value":{"kind":"quantile", "quantile":"0.99",
            "population":{"state":"known", "value":"completed_attempts"},
            "combination":{"state":"known", "value":"single_run"}}},
        "attempts":{
            "started":{"count":{"state":"known", "value":100}, "definition":{"state":"known", "value":"Requests sent."}},
            "observed":{"count":{"state":"known", "value":95}, "definition":{"state":"known", "value":"Requests that finished."}}
        },
        "trial_order":{"state":"known", "value":"sequential"}
    });
    let baseline_run = run(
        s,
        "baseline-run",
        case,
        &[],
        "valid",
        json!({"repetition":repeat("attempt-1"), "measurement":fields}),
    );
    let partial_run = run(
        s,
        "partial-run",
        case,
        &[],
        "valid",
        json!({"repetition":repeat("attempt-2"), "evidence_completeness":"partial"}),
    );
    let treatment_run = run(
        s,
        "treatment-run",
        case,
        &["old_allocator"],
        "valid",
        json!({}),
    );
    let reversal_run = run(
        s,
        "reversal-run",
        case,
        &["old_allocator"],
        "valid",
        json!({}),
    );
    let baseline = measured(s, "baseline", case, (3, "41"), baseline_run, &[], &capture);
    let _partial = measured(s, "partial", case, (4, "40"), partial_run, &[], &capture);
    let treatment = measured(
        s,
        "treatment",
        case,
        (5, "12"),
        treatment_run,
        &["old_allocator"],
        &capture,
    );
    let reversal = measured(
        s,
        "reversal",
        case,
        (6, "42"),
        reversal_run,
        &["old_allocator"],
        &capture,
    );

    let quoted = create(
        s,
        "quoted",
        case,
        "observation.add",
        json!({
            "value":{"kind":"text", "statement":"p99 rose after the allocator change."},
            "basis":{"kind":"source", "artifact_ref":reference(capture.0, 1),
                "selector":{"schema_version":"inv.selector.v1", "artifact_digest":capture.1["digest"],
                    "selector":{"type":"text_lines", "start_line":2, "end_line":2,
                        "encoding":"utf-8", "parser_version":"inv.text.utf8.v1"}},
                "text_quote":{"exact":QUOTE}},
            "validity":"valid", "quality":[], "limitations":["One capture."],
            "evidence_kind":"observational", "variables_changed":[]
        }),
    );

    // A validated causal conclusion whose support is retracted afterwards.
    let validated = conclusion(
        s,
        "validated",
        case,
        &[quoted, baseline, treatment, reversal],
        json!({
            "causal_factors":[{"hypothesis_ref":reference(factor, 1), "role":"trigger"}],
            "causal_test":{"variable":"old_allocator", "factor_ref":reference(factor, 1),
                "baseline":reference(baseline, 1), "treatment":reference(treatment, 1),
                "reversal":reference(reversal, 1),
                "failure_boundary":{"operator":"at_least", "value":"30", "unit":"ms"}}
        }),
    );
    let (record, read_set) = current(s, case, validated);
    let mut preconditions = vec![json!(record.reference())];
    preconditions.extend(read_set.iter().map(|reference| json!(reference)));
    submit(
        s,
        "request-review",
        Some(case),
        "conclusion.request_review",
        json!({"conclusion_ref":reference(validated, 1)}),
        json!(preconditions),
    );
    let (record, read_set) = current(s, case, validated);
    let review = create(
        s,
        "review",
        case,
        "review.submit",
        json!({
            "kind":"validation", "subject_ref":record.reference(),
            "basis_digest":basis_digest(&record).unwrap(), "read_set":read_set,
            "method":"Read the captures.", "materials_examined":[reference(quoted, 1)],
            "shared_dependencies":[], "objection_refs":[], "verdict":"supports",
            "caveats":["Synthetic captures only."], "context_isolation":"fresh_context_unblinded",
            "context":{
                "saw_original_conclusion":{"state":"known","value":true},
                "gathered_new_evidence":{"state":"known","value":false},
                "method_or_source_changes":[], "activity":"Read the recorded basis.",
                "dependencies":"shared", "dependence_basis":"Same captures."
            }
        }),
    );
    let (record, read_set) = current(s, case, validated);
    let mut preconditions = vec![json!(record.reference()), reference(review, 1)];
    preconditions.extend(read_set.iter().map(|reference| json!(reference)));
    submit(
        s,
        "validate",
        Some(case),
        "conclusion.validate",
        json!({"conclusion_ref":record.reference(), "review_ref":reference(review, 1)}),
        json!(preconditions),
    );
    submit(
        s,
        "retract",
        Some(case),
        "observation.retract",
        json!({"observation_ref":reference(reversal, 1),
            "revision_reason":"The reversal capture came from another build."}),
        json!([]),
    );

    // Confidence and likelihood, recorded together, rendered apart.
    let judged = conclusion(
        s,
        "judged",
        case,
        &[baseline],
        json!({
            "confidence":{"level":"low", "rationale":"One baseline capture.",
                "limitations":["One build."], "what_would_change_it":"A second build.",
                "downgrade_reasons":{"completeness":"concern", "consistency":"ok",
                    "directness":"ok", "precision":"not_assessed", "validity":"ok"}},
            "likelihood":{"estimate":"likely", "resolution_condition":"The next rerun repeats it."}
        }),
    );

    // A conclusion resting on a restricted measurement.
    let secret = measured(
        s,
        &format!("{RESTRICTED}-secret"),
        case,
        (7, "77"),
        baseline_run,
        &[],
        &capture,
    );
    let resting = conclusion(s, "resting", case, &[secret], json!({}));
    let subject = current(s, case, secret).0.reference();
    submit(
        s,
        "raise",
        Some(case),
        "classification.raise",
        json!({"subject_ref":subject, "level":"restricted", "reason":format!("{RESTRICTED} reason")}),
        json!([]),
    );
    Causal {
        _directory: directory,
        store,
        case,
        validated,
        retracted: reversal,
        judged,
        resting,
        secret,
    }
}

fn causal_plan(fixture: &mut Causal, report: &str, audience: Audience) -> ExportPlan {
    plan(&mut fixture.store, fixture.case, report, audience)
}

#[test]
fn a_stale_reviewed_conclusion_is_never_printed_as_simply_validated() {
    let mut fixture = causal();
    for report in KINDS {
        for audience in [Audience::Internal, Audience::Restricted] {
            let plan = causal_plan(&mut fixture, report, audience);
            assert_shape(&plan, report);
            let text = text(&plan);
            let validated = block(&text, &format!("- `{}`", fixture.validated));
            let first = validated.lines().next().unwrap();
            assert!(
                first.ends_with(
                    "effective `needs_review` (recorded `validated`, which does not hold as it \
                     stands at this snapshot)"
                ),
                "{first}"
            );
            assert!(!text.contains("effective `validated`"), "{report}");
            assert!(validated.contains("review state `stale`"), "{validated}");
            // The retracted support is annotated where it is cited.
            let retracted = fixture.retracted.to_string();
            let cited = validated
                .lines()
                .find(|line| line.contains(&retracted))
                .unwrap();
            assert!(cited.contains("annotated: recorded `retracted`"), "{cited}");
            // It is an open issue too.
            let open = text.split("## Open issues").nth(1).unwrap();
            assert!(open.contains(&fixture.validated.to_string()));
        }
    }
}

#[test]
fn confidence_and_likelihood_are_shown_on_separate_lines() {
    let mut fixture = causal();
    for report in KINDS {
        let text = text(&causal_plan(&mut fixture, report, Audience::Internal));
        let judged = block(&text, &format!("- `{}`", fixture.judged));
        let confidence = judged
            .lines()
            .find(|line| line.contains("- Confidence: `low`."))
            .unwrap();
        let likelihood = judged
            .lines()
            .find(|line| line.contains("- Likelihood: `likely`."))
            .unwrap();
        assert_ne!(confidence, likelihood);
        assert!(!confidence.contains("ikelihood") && !likelihood.contains("onfidence"));
        assert!(judged.contains("  - Resolved when: The next rerun repeats it\\."));
        assert!(judged.contains("- What would invalidate it:\n    - A second build\\."));
    }
}

#[test]
fn the_analysis_report_shows_runs_measurements_flags_and_repetition() {
    let mut fixture = causal();
    let text = text(&causal_plan(
        &mut fixture,
        "analysis-report",
        Audience::Restricted,
    ));
    let runs = text
        .split("## Runs and controls")
        .nth(1)
        .unwrap()
        .split("\n## ")
        .next()
        .unwrap();
    for element in [
        "    - Workload: read90\\-write10",
        "    - Sample count, as stated: `95`",
        "    - Statistic: `quantile` at `0.99`, population `completed_attempts`, combination `single_run`",
        "    - Trial order: `sequential`",
        "    - Attempts `started`: `100` (Requests sent\\.)",
        "    - Attempts `observed`: `95` (Requests that finished\\.)",
        "  - Repetition series: `warm-path`",
        "  - Evidence completeness: `partial`",
    ] {
        assert!(runs.contains(element), "{element}\n{runs}");
    }
    assert!(
        runs.contains(
            "- Series `warm-path`: 1 of 2 repeats completed with complete, valid evidence"
        )
    );
    // Every advisory flag the rules raise on an included record is named.
    let state = fixture.store.case(fixture.case).unwrap().state;
    for flag in measurement_flags::case_flags(&state, fixture.case) {
        if flag.subject_ref.id == fixture.secret {
            continue;
        }
        assert!(
            text.contains(&format!("`{}`", flag.flag_id)),
            "{}",
            flag.flag_id
        );
    }
    let baselines = text.split("## Baselines").nth(1).unwrap();
    assert!(baselines.contains("baseline of the causal test of `old_allocator`"));
    let uncertainty = text.split("## Uncertainty").nth(1).unwrap();
    assert!(uncertainty.contains("evidence completeness `partial`"));
    let assessment = text
        .split("## Root-cause or measurement assessment")
        .nth(1)
        .unwrap();
    assert!(assessment.contains("      - Failure boundary: `at_least` `30` `ms`"));
}

#[test]
fn withheld_material_never_reaches_either_report() {
    let mut fixture = causal();
    for report in KINDS {
        let restricted = causal_plan(&mut fixture, report, Audience::Restricted);
        assert!(restricted.complete());
        assert!(text(&restricted).contains(&fixture.secret.to_string()));
        if report == "case-report" {
            assert!(text(&restricted).contains(RESTRICTED));
        }

        let internal = causal_plan(&mut fixture, report, Audience::Internal);
        assert!(!internal.complete());
        let shown = text(&internal);
        let resting = block(&shown, &format!("- `{}`", fixture.resting));
        assert!(
            resting
                .lines()
                .nth(1)
                .is_some_and(|line| line == format!("  - Caveat: {}", Caveat::BasisWithheld.text())),
            "{resting}"
        );
        assert!(resting.contains("  - Evidence: none listed"), "{resting}");
        // Only the fixed caveat texts appear as caveats.
        for line in shown
            .lines()
            .filter(|line| line.trim_start().starts_with("- Caveat: "))
        {
            let shown = line.trim_start().trim_start_matches("- Caveat: ");
            assert!(
                [
                    Caveat::BasisWithheld.text(),
                    Caveat::ChallengeWithheld.text()
                ]
                .contains(&shown),
                "{line}"
            );
        }
        assert_nothing_withheld_leaks(&internal, fixture.case, &[RESTRICTED]);

        let public = causal_plan(&mut fixture, report, Audience::Public);
        assert!(!public.complete());
        assert!(!text(&public).contains(&fixture.resting.to_string()));
        assert_nothing_withheld_leaks(&public, fixture.case, &[RESTRICTED, "allocator"]);
    }
}

#[test]
fn the_causal_reports_are_byte_identical_for_the_same_inputs() {
    let mut fixture = causal();
    for report in KINDS {
        for audience in AUDIENCES {
            let first = causal_plan(&mut fixture, report, audience);
            let second = causal_plan(&mut fixture, report, audience);
            assert_eq!(bytes(&first), bytes(&second), "{report}");
            assert_eq!(first.manifest().export_id, second.manifest().export_id);
        }
    }
}

/// W11.13: with IDs remapped, both kinds name the bundle instead of the case
/// snapshot and show no journal sequence, head hash or command that needs
/// one, including a retrospective's (Scenario C has one). Planning would
/// fail closed on a leftover canonical head or ID; restoring through the
/// publisher's mapping gives back every canonical record ID shown.
#[test]
fn remapped_reports_name_the_bundle_and_show_no_sequence_or_head() {
    let directory = tempfile::tempdir().unwrap();
    let mut retrospectives = 0;
    for name in SCENARIOS {
        let mut store = scenario(name, &directory.path().join(name), false);
        for case in cases(&store) {
            let snapshot = ExportSnapshot::read(&mut store, case, &Pin::default()).unwrap();
            let head = store.case(case).unwrap().snapshot.head_hash.to_string();
            for report in KINDS {
                let label = format!("{name} {case} {report}");
                let plan = plan_export_remap(
                    kind(report).unwrap(),
                    &snapshot,
                    Audience::Internal,
                    &Retained::none(),
                    true,
                )
                .unwrap_or_else(|error| panic!("{label}: {error}"));
                assert_eq!(plan.identifiers().mode(), IdentifierMode::Remapped);
                let raw = raw_text(&plan);
                assert!(raw.contains("| Bundle | "), "{label}");
                assert!(!raw.contains("| Snapshot | "), "{label}");
                assert!(!raw.contains("brief --snapshot"), "{label}");
                assert!(!raw.contains(" at sequence "), "{label}");
                assert!(!raw.contains(&head), "{label}");
                assert!(raw.contains("ask the publisher"), "{label}");
                assert!(!raw.contains(&case.to_string()), "{label}");
                let restored = text(&plan);
                assert!(restored.contains(&case.to_string()), "{label}");
                if report == "case-report" {
                    let canonical =
                        plan_export(kind(report).unwrap(), &snapshot, Audience::Internal).unwrap();
                    let canonical = text(&canonical);
                    for line in canonical
                        .lines()
                        .filter(|line| line.starts_with("- `retro_"))
                    {
                        retrospectives += 1;
                        let id = &line[3..line[3..].find('`').unwrap() + 3];
                        let shown = restored
                            .lines()
                            .find(|line| line.starts_with(&format!("- `{id}` ")))
                            .unwrap_or_else(|| panic!("{label}: {id}"));
                        assert!(
                            shown.starts_with(&format!("- `{id}` Retrospective: revision ")),
                            "{label}: {shown}"
                        );
                    }
                }
            }
        }
    }
    assert!(retrospectives > 0, "a retrospective was checked");
}

/// Scenario B as recorded (W14.11): the latency-regression case. Its
/// changed variables are recorded on the observations, so the baselines are
/// the control captures alone; measurement fields, advisory flags and every
/// measurement judgment are shown as recorded, and the measurements judged
/// `invalid` or `partial` are listed under uncertainty. B records no
/// repetition series, causal test or retraction; those stay covered by the
/// synthetic [`causal`] fixture.
#[test]
fn scenario_b_analysis_report_shows_its_recorded_measurement_evidence() {
    let reports = exported("scenario-b", "analysis-report");
    assert_eq!(reports.len(), 1);
    let main = &reports[0].1;
    let section = |heading: &str| -> String {
        main.split(&format!("## {heading}\n"))
            .nth(1)
            .unwrap()
            .split("\n## ")
            .next()
            .unwrap()
            .to_owned()
    };
    let control = "`run_01a210c9-be76-7413-86a7-b1564d7d217c` control latency capture";
    let treatment = "`run_01a210c9-bf3a-7dc1-a4c9-f4e76187a026` treatment latency capture";
    let debug = "`run_01a210c9-c05d-7c83-b0ef-19884a0c7d49`";

    let outcome = section("Status and outcome");
    assert!(outcome.contains("- Outcome: `inconclusive`. This case is inconclusive"));
    assert!(outcome.contains(
        "- Reopen condition: Run the frozen confirmatory repeat plan with the allocator reverted \
         alone\\."
    ));

    let conditions = section("Reproduction conditions");
    assert!(conditions.contains("- Variable changed: allocator (runs "));
    assert!(conditions.contains(&format!(
        "- Variable changed: debug\\_logging (runs {debug})"
    )));

    // Baselines: the controls, never a capture whose observations record a
    // changed variable.
    let baselines = section("Baselines");
    assert!(baselines.contains(control), "{baselines}");
    assert!(!baselines.contains(treatment), "{baselines}");
    assert!(!baselines.contains(debug), "{baselines}");

    let runs = section("Runs and controls");
    let treated = block(&runs, treatment);
    assert!(
        treated.contains(
            "  - Variables changed, as its observations record them: allocator; revision; runtime"
        ),
        "{treated}"
    );
    assert!(treated.contains("  - Evidence completeness: `complete`"));
    assert!(treated.contains("  - Evidence validity: `unknown`"));
    // The debug-logging capture: the field as recorded, its judgment and
    // the advisory flag, each under the observation it belongs to.
    let logged = block(&runs, debug);
    for element in [
        "`obs_01a210c9-c0dc-7a01-86cb-246cfbd2a4fd` Measured 79.1 ms",
        "        - Instrumentation changed: `true`",
        "      - Measurement validity: effective `invalid` (recorded with the observation `unknown`)",
        "        - Judgment `invalid` by `agent:scenario-b`",
        "      - Advisory flags, rule version `measurement-flags.v1`; a flag never changes validity:",
        "        - `measurement.instrumentation_changed`: Instrumentation changed relative to the \
         compared runs",
    ] {
        assert!(logged.contains(element), "{element}\n{logged}");
    }
    // Sample-count, order, closed-loop and completion-only flags appear as
    // recorded on the captures that raised them.
    for flag in [
        "measurement.quantile_below_order_statistic_bound",
        "measurement.sequential_trial_order",
        "measurement.closed_loop_without_intended_send_timing",
        "measurement.completion_only_statistic",
    ] {
        assert!(runs.contains(&format!("- `{flag}`: ")), "{flag}");
    }
    assert!(runs.contains("- No repetition series is recorded."));

    let uncertainty = section("Uncertainty");
    let judged = block(
        &uncertainty,
        "- Measurements whose effective measurement validity, from their judgments, is not",
    );
    for (observation, validity) in [
        ("obs_01a210c9-c0dc-7a01-86cb-246cfbd2a4fd", "invalid"),
        ("obs_01a210c9-c2ad-7140-8dea-8ddd28982787", "invalid"),
        ("obs_01a210c9-c32a-7fa1-8c3d-ccdd9be37f0c", "invalid"),
        ("obs_01a210c9-c582-71d1-8610-6daeb8cc856c", "partial"),
    ] {
        assert!(
            judged.lines().any(|line| line.contains(observation)
                && line.contains(&format!("effective measurement validity `{validity}`"))),
            "{observation}\n{judged}"
        );
    }
    // A capture judged valid is not listed as uncertain on that account.
    assert!(!judged.contains("obs_01a210c9-bebe-7110-97ca-38cc352767ba"));

    let assessment = section("Root-cause or measurement assessment");
    assert!(assessment.contains(
        "`concl_01a210c9-d65f-7ce1-8785-70861bfbe6dc` The slab-v2 allocator change causes the p99 \
         regression.: effective `candidate` (recorded `candidate`)"
    ));
}

/// Scenario B's case report: an inconclusive closure with its reopen
/// condition, original observations with their judged validity, the
/// decision as a choice, and the retrospective.
#[test]
fn scenario_b_case_report_contains_every_required_element() {
    let reports = exported("scenario-b", "case-report");
    let main = &reports[0].1;
    assert!(main.contains("- Outcome: `inconclusive`. This case is inconclusive"));
    assert!(main.contains("- Reopen condition: Run the frozen confirmatory repeat plan"));
    let observations = main
        .split("## Original observations\n")
        .nth(1)
        .unwrap()
        .split("\n## ")
        .next()
        .unwrap();
    let logged = block(observations, "`obs_01a210c9-c0dc-7a01-86cb-246cfbd2a4fd`");
    assert!(
        logged.contains("  - Measurement validity: effective `invalid`"),
        "{logged}"
    );
    assert!(
        logged.contains("  - Variables changed: runtime; allocator; revision; debug\\_logging"),
        "{logged}"
    );
    let decisions = main.split("## Decisions\n").nth(1).unwrap();
    assert!(decisions.contains(
        "- `dec_01a210c9-d22c-7f71-8871-edeaff4bf86e` Chosen option: Keep the Marlin 1.9 build \
         with slab-v2: effective `proposed` (recorded `proposed`)"
    ));
    let retrospective = main
        .split("## Methodological retrospective\n")
        .nth(1)
        .unwrap();
    assert!(retrospective.contains("- `retro_01a210c9-e0df-7d52-908e-d02c244d5e79` "));
}
