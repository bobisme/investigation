//! The Scenario A and C proofs, rendered as a vault.
//!
//! Each test runs the repository's own scenario proof against this binary,
//! keeping its workspace, then plans the vault from that workspace: every
//! record renders to its folder, every link resolves, every reference
//! between two notes is linked both ways, and a stale review reads as stale
//! on its own note and on its subject's note.

use std::{path::Path, process::Command};

use investigation_core::identity::{Id, Kind};
use investigation_render::{RenderPlan, VerifiedSnapshot, ViewPolicy, layout, render};
use investigation_store::{Store, Workspace};

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

fn target(case: Id, id: Id) -> String {
    let path = layout::record_note(case, id).unwrap().unwrap();
    path.as_str().strip_suffix(".md").unwrap().to_owned()
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
    let case = snapshot.cases().next().unwrap();
    let state = case.state();
    // The criterion amendment after acceptance leaves the accepted review
    // stale. The unsafe decision's review never read the stricter threshold
    // either, so every review of this case is stale.
    let reviews: Vec<_> = state
        .records()
        .filter(|record| record.header.id.kind() == Kind::Review)
        .collect();
    assert_eq!(reviews.len(), 2);
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
