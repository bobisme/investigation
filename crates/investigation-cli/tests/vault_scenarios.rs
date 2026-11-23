//! The Scenario A and C proofs, rendered as a vault.
//!
//! Each test runs the repository's own scenario proof against this binary,
//! keeping its workspace, then plans the vault from that workspace: every
//! record renders to its folder, every link resolves, every reference
//! between two notes is linked both ways, and a stale review reads as stale
//! on its own note and on its subject's note. The case Overview opens with
//! the bounded summary, puts the strongest objection above any count, shows
//! the section the case calls for, and agrees with `brief` on the answer,
//! its caveat and the case's freshness for the same snapshot.

use std::{path::Path, process::Command};

use investigation_core::{
    identity::{Id, Kind},
    state::Record,
};
use investigation_render::{RenderPlan, VerifiedSnapshot, ViewPolicy, layout, render};
use investigation_store::{Store, Workspace};
use serde_json::Value;

#[path = "../../investigation-render/tests/support/vault.rs"]
mod vault;

fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
}

/// Run a scenario proof script into `workspace` and plan its vault.
fn prove(script: &str, args: &[&str], workspace: &Path) -> (VerifiedSnapshot, RenderPlan) {
    let output = Command::new("python3")
        .arg(repository().join("scripts").join(script))
        .args(args)
        .current_dir(repository())
        .output()
        .expect("python3 runs the scenario proof");
    assert!(
        output.status.success(),
        "{script}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut store = Store::open(Workspace::open(workspace).unwrap());
    let snapshot = VerifiedSnapshot::read(&mut store).unwrap();
    let plan = render(&snapshot, &ViewPolicy::default()).unwrap();
    (snapshot, plan)
}

fn note(plan: &RenderPlan, case: Id, id: Id) -> &str {
    let path = layout::record_note(case, id).unwrap().unwrap();
    std::str::from_utf8(plan.entry(path.as_str()).unwrap().content()).unwrap()
}

fn overview(plan: &RenderPlan, case: Id) -> &str {
    let path = layout::case_overview(case).unwrap();
    std::str::from_utf8(plan.entry(path.as_str()).unwrap().content()).unwrap()
}

/// `brief` for one case from the same workspace the vault was planned from.
fn brief(workspace: &Path, case: Id) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .arg("--workspace")
        .arg(workspace)
        .args(["--case", &case.to_string(), "brief", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The line of a section that starts with `- {label}: `.
fn line<'a>(section: &'a str, label: &str) -> &'a str {
    section
        .lines()
        .find(|line| line.starts_with(&format!("- {label}: ")))
        .unwrap_or_else(|| panic!("no {label} line in\n{section}"))
}

/// The Overview opens with the bounded summary, names the strongest
/// objection before any count, and tells the same story as `brief` for the
/// same snapshot: the same leading outcome and effective status, the same
/// material caveat and contradiction, and the same freshness.
fn assert_overview_agrees(note: &str, brief: &Value, state: &investigation_core::state::State) {
    let opening = vault::section(note, "Opening summary").unwrap();
    assert!(opening.contains("\nWe are investigating "), "{opening}");
    let objection = note.find("the strongest outstanding objection is").unwrap();
    let counted = note.find("records in evidentiary use").unwrap();
    assert!(objection < counted, "the objection comes before any count");
    assert!(objection < note.find("\n## Records\n").unwrap());

    // The leading outcome is the newest Conclusion or Decision that is not
    // superseded, as in `brief`.
    let live = |record: &&Record| {
        matches!(record.header.id.kind(), Kind::Conclusion | Kind::Decision)
            && record.body["lifecycle"] != "superseded"
            && record.body["status"] != "superseded"
    };
    let primary = state
        .records()
        .filter(live)
        .max_by_key(|record| (record.header.acceptance_sequence, record.header.id))
        .unwrap();
    let id = primary.header.id.to_string();
    let answer = vault::section(note, "Answer and caveat").unwrap();
    let answer_line = line(answer, "Answer");
    assert!(answer_line.contains(&format!("{id}|")), "{answer_line}");
    let outcome = brief["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|outcome| outcome["id"] == id.as_str())
        .unwrap_or_else(|| panic!("{id} not in the brief"));
    let status = outcome["effective_status"]["state"].as_str().unwrap();
    assert!(
        answer_line.contains(&format!("effective `{status}`")),
        "{answer_line} vs {status}"
    );

    let caveat = line(answer, "Material caveat").replace('\\', "");
    let expected = &brief["material_caveat"];
    match (expected["reason"].as_str(), expected["value"].as_str()) {
        (Some(reason), _) => assert!(caveat.contains(reason), "{caveat} vs {reason}"),
        (None, Some(value)) => assert!(caveat.contains(value), "{caveat} vs {value}"),
        (None, None) => {
            assert_eq!(expected["state"], "unknown");
            assert!(caveat.ends_with(": none recorded"), "{caveat}");
        }
    }
    let contradiction = line(answer, "Contradiction");
    match brief["contradiction"]["references"].as_array() {
        Some(references) => {
            for reference in references {
                let target = reference["id"].as_str().unwrap();
                assert!(contradiction.contains(target), "{contradiction}");
            }
        }
        None => {
            let reason = brief["contradiction"]["reason"].as_str();
            assert!(
                reason.map_or(contradiction.ends_with(": none recorded"), |reason| {
                    contradiction.contains(reason)
                }),
                "{contradiction}"
            );
        }
    }
    let freshness = line(answer, "Freshness");
    let word = brief["freshness"]["state"].as_str().unwrap();
    let effective = brief["freshness"]["effective"].as_str().unwrap();
    assert!(
        freshness.starts_with(&format!("- Freshness: `{word}` (effective `{effective}`;")),
        "{freshness} vs {word}/{effective}"
    );
    assert_eq!(
        vault::property(note, "inv_summary").as_deref(),
        Some("summary_current")
    );

    // Exact commands to resume from the same store.
    let resume = vault::section(note, "Resume").unwrap();
    let case = brief["case"]["id"].as_str().unwrap();
    for command in ["brief", "frontier"] {
        assert!(
            resume.contains(&format!(
                "`investigation --case {case} {command} --format json`"
            )),
            "{resume}"
        );
    }
}

fn target(case: Id, id: Id) -> String {
    let path = layout::record_note(case, id).unwrap().unwrap();
    path.as_str().strip_suffix(".md").unwrap().to_owned()
}

/// `Start Here.md` describes every case with its Overview link, and every
/// command it prints runs verbatim in the workspace folder.
fn assert_start_here(workspace: &Path, snapshot: &VerifiedSnapshot, plan: &RenderPlan) -> String {
    let note = std::str::from_utf8(plan.entry("Start Here.md").unwrap().content())
        .unwrap()
        .to_owned();
    for heading in [
        "Projection health",
        "Who owns what",
        "Resume a case",
        "Cases",
    ] {
        assert!(
            vault::section(&note, heading).is_some(),
            "{heading}\n{note}"
        );
    }
    assert!(note.contains("- State: complete."), "{note}");
    let cases = vault::section(&note, "Cases").unwrap();
    for case in snapshot.cases() {
        let overview = layout::case_overview(case.case_id()).unwrap();
        let target = overview.as_str().strip_suffix(".md").unwrap();
        assert!(cases.contains(&format!("- [[{target}|")), "{cases}");
        assert!(
            cases.contains(&format!(
                "Last committed snapshot: sequence {}, head `{}`",
                case.head().sequence,
                case.head().head_hash.as_str()
            )),
            "{cases}"
        );
    }
    let mut commands = 0;
    for span in note.split('`').skip(1).step_by(2) {
        let Some(arguments) = span.strip_prefix("investigation ") else {
            continue;
        };
        commands += 1;
        let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
            .args(arguments.split_whitespace())
            .current_dir(workspace)
            .output()
            .unwrap();
        assert!(output.status.success(), "{span}: {output:?}");
        serde_json::from_slice::<Value>(&output.stdout)
            .unwrap_or_else(|error| panic!("{span}: {error}"));
    }
    // `brief` and `frontier` per case, and the two projection health checks.
    assert_eq!(commands, 2 * snapshot.cases().count() + 2, "{note}");
    let health = vault::section(&note, "Projection health").unwrap();
    for check in ["vault status", "vault conflicts"] {
        assert!(
            health.contains(&format!("`investigation {check} --format json`")),
            "{health}"
        );
    }
    note
}

fn assert_vault(snapshot: &VerifiedSnapshot, plan: &RenderPlan, kinds: &[Kind]) {
    let report = vault::check(plan, snapshot);
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
    for kind in kinds {
        assert!(report.kinds.contains(kind), "no {kind:?} note");
    }
    assert!(
        report.pairs > 50,
        "only {} references checked",
        report.pairs
    );
}

#[test]
fn scenario_a_renders_every_record_type_with_links_both_ways() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("a");
    let handoff = directory.path().join("handoff.json");
    let (snapshot, plan) = prove(
        "scenario_a_proof.py",
        &[
            env!("CARGO_BIN_EXE_investigation"),
            handoff.to_str().unwrap(),
            "--workspace",
            workspace.to_str().unwrap(),
        ],
        &workspace,
    );
    assert_vault(
        &snapshot,
        &plan,
        &[
            Kind::Question,
            Kind::Hypothesis,
            Kind::Inquiry,
            Kind::Source,
            Kind::Artifact,
            Kind::Observation,
            Kind::Run,
            Kind::Conclusion,
            Kind::Review,
            Kind::Retrospective,
        ],
    );

    let start = assert_start_here(&workspace, &snapshot, &plan);
    let cases = vault::section(&start, "Cases").unwrap();
    assert!(
        cases.contains("  - Retrospectives (1):\n    - [["),
        "{cases}"
    );
    assert!(cases.contains("  - Focus: explanation\n"), "{cases}");

    // The validated conclusion and its review agree that the review is
    // current: validation was a lifecycle-only revision of the subject.
    let case = snapshot
        .cases()
        .find(|case| {
            case.state()
                .records()
                .any(|record| record.header.id.kind() == Kind::Conclusion)
        })
        .unwrap();
    let state = case.state();
    let front = overview(&plan, case.case_id());
    assert_overview_agrees(front, &brief(&workspace, case.case_id()), state);
    assert_eq!(
        vault::property(front, "inv_focus").as_deref(),
        Some("explanation")
    );
    let explanation = vault::section(front, "Explanation").unwrap();
    for expected in [
        "### Rival hypotheses",
        "### Decisive and non-diagnostic observations",
        "### Unresolved confounds",
        "### Inquiry timeline",
    ] {
        assert!(explanation.contains(expected), "{expected}\n{explanation}");
    }
    assert!(vault::section(front, "Evaluation").is_none());
    let retrospectives = vault::section(front, "Reviews and retrospectives").unwrap();
    assert!(
        retrospectives.contains("- Most recent retrospective: [[")
            && retrospectives.contains("freshness `current`"),
        "{retrospectives}"
    );
    // The answer is the validated conclusion; the opening sentence names its
    // original evidence and the ladder ends with exact sources.
    let opening = vault::section(front, "Opening summary").unwrap();
    assert!(opening.contains("Our current answer is [["), "{opening}");
    assert!(
        opening.contains("from the original acquisition [["),
        "{opening}"
    );
    let sources = vault::section(front, "Sources").unwrap();
    assert!(
        sources.contains("availability `retained`, original acquisition"),
        "{sources}"
    );
    let review = state
        .records()
        .find(|record| record.header.id.kind() == Kind::Review)
        .unwrap();
    let subject: Id = review.body["subject_ref"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let review_note = note(&plan, case.case_id(), review.header.id);
    assert_eq!(
        vault::property(review_note, "inv_review_kind").as_deref(),
        Some("validation")
    );
    assert_eq!(
        vault::property(review_note, "inv_freshness").as_deref(),
        Some("current")
    );
    assert!(vault::property(review_note, "inv_basis_sequence").is_some());
    assert!(
        !review_note.contains("[!warning] Stale review"),
        "{review_note}"
    );
    let conclusion = note(&plan, case.case_id(), subject);
    assert_eq!(
        vault::property(conclusion, "inv_review_freshness").as_deref(),
        Some("current")
    );
    assert_eq!(
        vault::property(conclusion, "inv_disposition").as_deref(),
        Some("validated")
    );
    let reviews = vault::section(conclusion, "Reviews and objections").unwrap();
    assert!(
        reviews.contains(&format!("[[{}|", target(case.case_id(), review.header.id)))
            && reviews.contains("verdict `supports`, freshness `current`"),
        "{reviews}"
    );
    let disposition = vault::section(conclusion, "Disposition").unwrap();
    for expected in [
        "- Effective disposition: `validated`",
        "- Strongest outstanding objection: none open on this record or its basis",
        "### Contradicting records",
        "- Causal test of `release_before_await`:",
    ] {
        assert!(disposition.contains(expected), "{expected}\n{disposition}");
    }

    // The rival hypothesis lists the conclusion that names it as contradicting.
    let rival: Id = state.current(subject).unwrap().body["contradiction_refs"][0]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let rival_note = note(&plan, case.case_id(), rival);
    let named = vault::section(rival_note, "Standing").unwrap();
    assert!(
        named.contains(&format!("[[{}|", target(case.case_id(), subject)))
            && named.contains(": as a contradicting record"),
        "{named}"
    );
    assert!(named.contains("### Rival explanations"), "{named}");

    // The invalid run is marked invalid and cited by the retrospective.
    let run = state
        .records()
        .find(|record| record.body["evidence_validity"] == "invalid")
        .unwrap();
    let run_note = note(&plan, case.case_id(), run.header.id);
    assert!(
        run_note.contains("- Evidence validity: `invalid`: its results must not ground claims"),
        "{run_note}"
    );
    let retrospective = state
        .records()
        .find(|record| record.header.id.kind() == Kind::Retrospective)
        .unwrap();
    let cited_by = run_note.split("### Cited by").nth(1).unwrap();
    assert!(
        cited_by.contains(&format!(
            "[[{}|",
            target(case.case_id(), retrospective.header.id)
        )),
        "{cited_by}"
    );
    let account = note(&plan, case.case_id(), retrospective.header.id);
    assert_eq!(
        vault::property(account, "inv_freshness").as_deref(),
        Some("current")
    );
    let steps = account
        .split("### Invalid or misleading steps")
        .nth(1)
        .unwrap();
    assert!(
        steps.contains(&format!(
            "(support [[{}|",
            target(case.case_id(), run.header.id)
        )),
        "{steps}"
    );
}

#[test]
fn scenario_c_marks_its_stale_review_on_the_review_and_on_the_decision() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("c");
    let (snapshot, plan) = prove(
        "scenario_c_proof.py",
        &[
            "--bin",
            env!("CARGO_BIN_EXE_investigation"),
            "--workspace",
            workspace.to_str().unwrap(),
        ],
        &workspace,
    );
    assert_vault(
        &snapshot,
        &plan,
        &[
            Kind::Question,
            Kind::Option,
            Kind::Criterion,
            Kind::Comparison,
            Kind::Assumption,
            Kind::Source,
            Kind::Artifact,
            Kind::Observation,
            Kind::Decision,
            Kind::Review,
            Kind::Retrospective,
        ],
    );
    let start = assert_start_here(&workspace, &snapshot, &plan);
    let cases = vault::section(&start, "Cases").unwrap();
    assert!(cases.contains("  - Focus: evaluation\n"), "{cases}");
    let case = snapshot.cases().next().unwrap();
    let state = case.state();
    let front = overview(&plan, case.case_id());
    assert_overview_agrees(front, &brief(&workspace, case.case_id()), state);
    // The question and the decision show the same option-by-criterion matrix,
    // consequences first, with unknowns labelled and no computed score.
    for kind in [Kind::Question, Kind::Decision] {
        let record = state
            .records()
            .find(|r| r.header.id.kind() == kind)
            .unwrap();
        let text = note(&plan, case.case_id(), record.header.id);
        let at = text
            .find("\n### Option-by-criterion comparison\n")
            .unwrap_or_else(|| panic!("no matrix on {kind:?}"));
        let matrix = &text[at..];
        for expected in [
            "| Option |",
            "(hard) |",
            "unknown",
            "outcome `meets`",
            "`110` `seconds`",
            "no overall score and no ranking",
            "Hard failures:",
            "Pareto status",
        ] {
            assert!(
                matrix.contains(expected),
                "{expected} on {kind:?}\n{matrix}"
            );
        }
        assert_eq!(matrix.matches("\n| [[").count(), 3, "{matrix}");
    }
    // A comparison case shows its evaluation, never a failed-looking empty
    // conclusions block.
    assert_eq!(
        vault::property(front, "inv_focus").as_deref(),
        Some("evaluation")
    );
    let evaluation = vault::section(front, "Evaluation").unwrap();
    for expected in [
        "### Options",
        "### Hard constraints and criteria",
        "### Compared evidence",
        "### Decision",
        "### Sensitivity and reversal",
        "outcome `meets`, value `110` `seconds`",
        "- Reversal conditions:",
    ] {
        assert!(evaluation.contains(expected), "{expected}\n{evaluation}");
    }
    assert!(vault::section(front, "Explanation").is_none());
    let opening = vault::section(front, "Opening summary").unwrap();
    assert!(opening.contains("Our current choice is [["), "{opening}");
    for absent in ["Conclusions: 0", "0 conclusions", "no conclusion yet"] {
        assert!(!front.contains(absent), "{absent}\n{front}");
    }
    // The stale comparison basis shows as stale freshness, with the caveat.
    let answer = vault::section(front, "Answer and caveat").unwrap();
    assert!(
        answer.contains("- Freshness: `stale` (effective `needs_review`;"),
        "{answer}"
    );
    // The criterion amendment after acceptance leaves the accepted review
    // and the blindspot review of the accepted decision stale. The unsafe
    // decision's review never read the stricter threshold either, so every
    // review of this case is stale.
    let reviews: Vec<_> = state
        .records()
        .filter(|record| record.header.id.kind() == Kind::Review)
        .collect();
    assert_eq!(reviews.len(), 3);
    assert_eq!(
        reviews
            .iter()
            .filter(|review| review.body["kind"] == "blindspot")
            .count(),
        1
    );
    for review in reviews {
        let review_note = note(&plan, case.case_id(), review.header.id);
        assert_eq!(
            vault::property(review_note, "inv_freshness").as_deref(),
            Some("stale")
        );
        assert_eq!(
            vault::property(review_note, "inv_disposition").as_deref(),
            Some("stale")
        );
        let body = vault::section(review_note, "Review").unwrap();
        assert!(
            body.contains(
                "> [!warning] Stale review\n> This review no longer covers the current basis"
            ) && body.contains("because of `material_revision` at [["),
            "{body}"
        );
        let subject: Id = review.body["subject_ref"]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let decision = note(&plan, case.case_id(), subject);
        assert_eq!(
            vault::property(decision, "inv_review_freshness").as_deref(),
            Some("stale"),
            "{decision}"
        );
        let listed = vault::section(decision, "Reviews and objections").unwrap();
        let line = listed
            .lines()
            .find(|line| line.contains(&target(case.case_id(), review.header.id)))
            .unwrap_or_else(|| panic!("{listed}"));
        assert!(
            line.contains("freshness `stale`: this review no longer covers the current basis"),
            "{line}"
        );
    }

    // The superseded decision shows what it was accepted over, and its
    // successor links back.
    let accepted = state
        .records()
        .find(|record| {
            record.header.id.kind() == Kind::Decision && record.body["lifecycle"] == "superseded"
        })
        .unwrap();
    let accepted_note = note(&plan, case.case_id(), accepted.header.id);
    for expected in [
        "- Effective disposition: `superseded`",
        "- Hard conditions still to verify:",
        "### Accepted over",
        "- Hard criteria accepted as conditions, not as satisfied:",
        "names it in `supersedes`",
    ] {
        assert!(
            accepted_note.contains(expected),
            "{expected}\n{accepted_note}"
        );
    }

    // A comparison names its option, criterion and threshold; the hard
    // criterion's note lists it with the outcome.
    let measured = state
        .records()
        .find(|record| {
            record.header.id.kind() == Kind::Comparison && record.body["outcome"] == "meets"
        })
        .unwrap();
    let comparison = note(&plan, case.case_id(), measured.header.id);
    let measurement = vault::section(comparison, "Measurement").unwrap();
    for expected in [
        "- Effective disposition: `needs_review`",
        "  - Cited criterion: `hard` criterion, threshold `at_most` `300` `seconds`",
        "- Measured value: `110` `seconds`",
        "It is not a universal ranking, and an unknown outcome is not a pass.",
    ] {
        assert!(measurement.contains(expected), "{expected}\n{measurement}");
    }
    let criterion: Id = measured.body["criterion_ref"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let criterion_note = note(&plan, case.case_id(), criterion);
    assert!(criterion_note.contains("- Threshold: `at_most` `60` `seconds`"));
    assert!(criterion_note.contains(&format!(
        "[[{}|",
        target(case.case_id(), measured.header.id)
    )));
}
