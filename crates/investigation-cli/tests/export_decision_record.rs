//! `export --kind decision-record` through the public binary (W11.05) over
//! Scenarios A, C and D: the ADR sections and the comparison matrix, the
//! same bytes for the same snapshot, and what each audience gets. Every run
//! clears the environment, so nothing outside the workspace can be reached.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use investigation_core::{identity::Id, wire::CommitResult};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

#[path = "support/fixtures.rs"]
mod fixtures;

use fixtures::SCENARIOS;

fn run(cwd: &Path, args: &[&str]) -> (i32, Value) {
    let empty_path = cwd.join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_clear()
        .env("PATH", &empty_path)
        .args(args)
        .args(["--format", "json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value = serde_json::from_str(&stdout).unwrap_or_else(|_| {
        panic!(
            "{args:?}: {stdout} {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap(), value)
}

fn export(root: &Path, workspace: &Path, case: &str, audience: &str, out: &Path) -> (i32, Value) {
    run(
        root,
        &[
            "--workspace",
            workspace.to_str().unwrap(),
            "--case",
            case,
            "export",
            "--kind",
            "decision-record",
            "--audience",
            audience,
            "--out",
            out.to_str().unwrap(),
        ],
    )
}

fn cases(workspace: &Path) -> Vec<String> {
    let mut cases: Vec<String> = fs::read_dir(workspace.join("state/cases"))
        .unwrap()
        .map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.split('.').next().unwrap().to_owned()
        })
        .collect();
    cases.sort();
    cases.dedup();
    cases
}

fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path: PathBuf = entry.unwrap().path();
        found.insert(
            path.file_name().unwrap().to_str().unwrap().to_owned(),
            fs::read(&path).unwrap(),
        );
    }
    found
}

fn record(out: &Path) -> String {
    fs::read_to_string(out.join("decision-record.md")).unwrap()
}

fn listed_ids(receipt: &Value, list: &str) -> Vec<String> {
    receipt[list]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

/// The records a Decision rests on, by ID, from the journal.
fn decision_basis(workspace: &Path, case: Id) -> Option<(Id, Id)> {
    use investigation_core::{identity::Kind, review_ops};
    let mut store = Store::open(Workspace::open(workspace).unwrap());
    let replay = store.case(case).unwrap();
    let state = &replay.state;
    state.records_of_kind(Kind::Decision).find_map(|decision| {
        let reads = review_ops::required_reads(state, decision).ok()?;
        let basis = reads
            .into_iter()
            .map(|reference| reference.id)
            .find(|id| id.kind() == Kind::Observation)?;
        Some((decision.header.id, basis))
    })
}

fn raise_to_restricted(workspace: &Path, case: Id, id: Id) {
    let mut store = Store::open(Workspace::open(workspace).unwrap());
    let revision = store
        .case(case)
        .unwrap()
        .state
        .current(id)
        .unwrap()
        .header
        .revision
        .0;
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":format!("raise-{id}"),
        "case_id":case, "actor":"agent:exporter", "preconditions":[],
        "operations":[{"op":"classification.raise", "payload":{
            "subject_ref":{"id":id, "revision":revision},
            "level":"restricted", "reason":"Restricted for the audience test."}}]
    });
    let receipt = store.record(&serde_json::to_vec(&input).unwrap()).unwrap();
    assert_eq!(receipt.result, CommitResult::Committed);
}

#[test]
fn the_same_snapshot_gives_byte_identical_records_for_every_audience() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for scenario in SCENARIOS {
        let first = fixtures::workspace(scenario, &root.join(scenario).join("a"), false);
        let second = fixtures::workspace(scenario, &root.join(scenario).join("b"), true);
        for case in cases(&first) {
            for audience in ["public", "internal", "restricted"] {
                let mut outputs = Vec::new();
                for (label, workspace) in [("a", &first), ("b", &second)] {
                    let out = root.join(format!("{scenario}-{audience}-{label}"));
                    let (exit, value) = export(root, workspace, &case, audience, &out);
                    assert_eq!(exit, 0, "{scenario} {audience}: {value}");
                    assert_eq!(value["kind"], "decision-record");
                    assert_eq!(
                        files(&out).keys().collect::<Vec<_>>(),
                        ["decision-record.md", "export-manifest.json"]
                    );
                    outputs.push(files(&out));
                    fs::remove_dir_all(&out).unwrap();
                }
                assert_eq!(outputs[0], outputs[1], "{scenario} {audience}");
            }
        }
    }
}

/// Scenario C holds the conditional decision: its record carries the matrix,
/// the hard conditions to verify, the reopen trigger and the review warning
/// that follows the threshold change.
#[test]
fn scenario_c_exports_a_conditional_record_with_its_matrix_and_reopen_conditions() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-c", &root.join("ws"), false);
    let case = cases(&workspace).remove(0);
    let out = root.join("c-internal");
    let (exit, value) = export(root, &workspace, &case, "internal", &out);
    assert_eq!(exit, 0, "{value}");
    assert_eq!(value["completeness"], "complete");
    let text = record(&out);
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
        "## Supersession history",
        "## Limits",
    ] {
        let found = text[at..].find(heading);
        assert!(found.is_some(), "{heading} after {at}\n{text}");
        at += found.unwrap();
    }
    // The matrix: three options, five criteria, unknown cells stay unknown.
    let rows: Vec<&str> = text
        .lines()
        .skip_while(|line| !line.starts_with("| Option |"))
        .take_while(|line| line.starts_with('|'))
        .collect();
    assert_eq!(rows.len(), 2 + 3, "{text}");
    assert!(
        rows[2..].iter().all(|row| row.contains("unknown")),
        "{text}"
    );
    assert!(text.contains("an unknown or missing cell is not a pass"));
    assert!(text.contains("a preference, not a measurement"));
    // Conditions of the decision and the revisit trigger.
    assert!(
        text.contains("Reassess 110 seconds against the stricter 60"),
        "{text}"
    );
    assert!(text.contains("Reopen trigger: Retest recovery if the threshold changes"));
    assert!(text.contains("Reversal conditions:"));
    // The criterion changed after the comparison: the decision needs review.
    assert!(text.contains("Effective status: `needs_review`"), "{text}");
    assert!(text.contains("This decision needs review"), "{text}");
    assert!(text.contains("`material_revision`"), "{text}");
    assert!(text.contains("Stale comparison:"), "{text}");
    // Supersession links both ways between the decisions.
    assert!(text.contains("supersedes `dec_"), "{text}");
    assert!(text.contains("superseded by `dec_"), "{text}");
    assert!(
        !text.contains("Caveat"),
        "nothing is withheld from internal: {text}"
    );
}

/// A case with no decision still exports: a plain statement and the limits.
#[test]
fn a_case_without_a_decision_says_so() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-a", &root.join("ws"), false);
    for case in cases(&workspace) {
        let out = root.join(format!("a-{case}"));
        let (exit, value) = export(root, &workspace, &case, "restricted", &out);
        assert_eq!(exit, 0, "{value}");
        let text = record(&out);
        assert!(text.contains("## Limits"), "{text}");
        if !text.contains("## Decision 1: ") {
            assert!(
                text.contains("No decision is recorded or included"),
                "{text}"
            );
        }
    }
}

/// After raising one record a decision rests on to restricted: internal shows
/// the decision with its fixed basis caveat, public shows nothing of the
/// scenario, and no audience file names a withheld record.
#[test]
fn each_audience_gets_its_policy_over_scenarios_a_c_and_d() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut raised_any = 0;
    for scenario in SCENARIOS {
        let workspace = fixtures::workspace(scenario, &root.join(scenario), false);
        for case in cases(&workspace) {
            let case_id: Id = case.parse().unwrap();
            let raised = decision_basis(&workspace, case_id);
            if let Some((_, basis)) = raised {
                raise_to_restricted(&workspace, case_id, basis);
                raised_any += 1;
            }
            for audience in ["restricted", "internal", "public"] {
                let label = format!("{scenario} {case} {audience}");
                let out = root.join(format!("{scenario}-{audience}-policy"));
                let (exit, value) = export(root, &workspace, &case, audience, &out);
                assert_eq!(exit, 0, "{label}: {value}");
                let receipt = &value["publisher_receipt"];
                let excluded = listed_ids(receipt, "exclusions");
                let caveated = listed_ids(receipt, "caveated");
                let all = files(&out);
                for (path, bytes) in &all {
                    let text = String::from_utf8(bytes.clone()).unwrap();
                    for id in excluded.iter().filter(|id| **id != case) {
                        assert!(!text.contains(id.as_str()), "{label}: {path} names {id}");
                    }
                }
                let text = record(&out);
                match audience {
                    "restricted" => {
                        assert!(excluded.is_empty() && caveated.is_empty(), "{label}");
                        assert!(!text.contains("Caveat:"), "{label}");
                    }
                    "internal" => {
                        if let Some((decision, _)) = raised {
                            assert!(caveated.contains(&decision.to_string()), "{label}");
                            // The decision's own listing carries the caveat.
                            let shown: Vec<&str> = text
                                .lines()
                                .skip_while(|line| !line.contains(&decision.to_string()))
                                .collect();
                            assert!(
                                shown
                                    .iter()
                                    .any(|line| line.contains("- Caveat: ")
                                        && line.contains("rests on")),
                                "{label}: {decision} carries its caveat"
                            );
                        } else {
                            assert!(caveated.is_empty(), "{label}");
                        }
                    }
                    _ => {
                        assert!(caveated.is_empty(), "{label}");
                        assert!(!text.contains("Caveat"), "{label}");
                        // Scenario records are internal or restricted: the
                        // public record shows no decision, option or cell.
                        assert!(!text.contains("## Decision 1: "), "{label}\n{text}");
                        assert!(!text.contains("| Option |"), "{label}");
                    }
                }
                fs::remove_dir_all(&out).unwrap();
            }
        }
    }
    assert!(raised_any >= 1, "scenario C has a decision with a basis");
}
