//! `criterion.hard_unmeasured` on the checked-in Scenario C workspace
//! (W10.12b). After the recovery-time threshold changed, every recovery cell
//! still cites the revision before the change, so the audit reports that hard
//! criterion as unmeasured, and frontier keeps reporting each stale
//! Comparison. A benign later revision of a criterion, a restatement without
//! change, keeps its cells as they were measured. A `decision.propose` that
//! ignores such a criterion and states no hard condition is recorded with a
//! warning.
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use investigation_render::VerifiedSnapshot;
use serde_json::{Value, json};

#[path = "support/fixtures.rs"]
mod fixtures;

fn run(root: &Path, args: &[&str], stdin: Option<&Value>) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(root)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_SESSION")
        .args(args)
        .args(["--format", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut process = command.spawn().unwrap();
    if let Some(stdin) = stdin {
        let mut pipe = process.stdin.take().unwrap();
        pipe.write_all(&serde_json::to_vec(stdin).unwrap()).unwrap();
    }
    let output = process.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{args:?}\n{stdout}");
    serde_json::from_str(&stdout).unwrap()
}

/// Current records of the one case: (exact reference, body), in ID order.
fn records(root: &Path) -> (String, Vec<(Value, Value)>) {
    let snapshot = VerifiedSnapshot::read(&mut fixtures::open(root)).unwrap();
    let cases: Vec<_> = snapshot.cases().collect();
    assert_eq!(cases.len(), 1);
    let mut found: Vec<(Value, Value)> = cases[0]
        .state()
        .records()
        .map(|record| {
            (
                serde_json::to_value(record.reference()).unwrap(),
                record.body.clone(),
            )
        })
        .collect();
    found.sort_by(|left, right| left.0["id"].as_str().cmp(&right.0["id"].as_str()));
    (cases[0].case_id().to_string(), found)
}

fn criterion(records: &[(Value, Value)], name: &str) -> (Value, Value) {
    records
        .iter()
        .find(|(reference, body)| {
            reference["id"].as_str().unwrap().starts_with("crit_") && body["name"] == name
        })
        .cloned()
        .unwrap()
}

/// Every Comparison of a criterion, in ID order.
fn cells(records: &[(Value, Value)], criterion: &Value) -> Vec<Value> {
    records
        .iter()
        .filter(|(reference, body)| {
            reference["id"].as_str().unwrap().starts_with("cmp_")
                && body["criterion_ref"]["id"] == criterion["id"]
        })
        .map(|(reference, _)| reference.clone())
        .collect()
}

fn hard_unmeasured(root: &Path, case: &str, criterion: &Value) -> Value {
    let audit = run(root, &["--case", case, "blindspots"], None);
    let found: Vec<&Value> = audit["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| {
            finding["check_id"] == "criterion.hard_unmeasured"
                && finding["subject_refs"][0]["id"] == criterion["id"]
        })
        .collect();
    assert_eq!(found.len(), 1, "{audit:#}");
    found[0].clone()
}

fn fact(finding: &Value, name: &str) -> Value {
    finding["facts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|fact| fact["fact"] == name)
        .map_or(Value::Null, |fact| fact["refs"].clone())
}

#[test]
fn scenario_c_reports_the_hard_criterion_whose_cells_cite_its_superseded_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixtures::workspace("scenario-c", &dir.path().join("workspace"), false);
    let (case, records) = records(&root);
    let (recovery, recovery_body) = criterion(&records, "Recovery time after acknowledged enqueue");
    let (loss, _) = criterion(&records, "No acknowledged job is lost");
    assert_eq!(recovery["revision"], 2, "the threshold changed once");
    assert_eq!(recovery_body["kind"], "hard");
    let recovery_cells = cells(&records, &recovery);
    assert_eq!(recovery_cells.len(), 3);
    let cited = json!({"id": recovery["id"], "revision": 1});

    let finding = hard_unmeasured(&root, &case, &recovery);
    assert_eq!(finding["check_version"], "v2");
    assert_eq!(finding["subject_refs"], json!([recovery]));
    assert_eq!(
        fact(&finding, "hard_cell:criterion_superseded"),
        json!(recovery_cells)
    );
    assert_eq!(
        fact(&finding, "freshness_trigger:material_revision"),
        json!([cited, recovery])
    );
    assert_eq!(fact(&finding, "hard_cell:unknown"), Value::Null);
    // Frontier reports each of those cells as a stale Comparison, and the
    // finding names each of those gaps.
    let frontier = run(&root, &["--case", &case, "frontier"], None);
    for cell in &recovery_cells {
        let gap = frontier["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|gap| {
                gap["rule_id"] == "comparison.stale_dependency_revision"
                    && gap["subject_refs"] == json!([cell])
            })
            .unwrap_or_else(|| panic!("{cell} in {frontier:#}"));
        let name = format!("frontier_gap:{}", gap["gap_id"].as_str().unwrap());
        assert_eq!(fact(&finding, &name), json!([cell]));
    }
    // The unknown loss requirement is reported as before.
    let unknown = hard_unmeasured(&root, &case, &loss);
    assert_eq!(
        fact(&unknown, "hard_cell:unknown"),
        json!(cells(&records, &loss))
    );
    assert_eq!(
        fact(&unknown, "hard_cell:criterion_superseded"),
        Value::Null
    );

    // Restating the loss requirement without change is benign: its cells keep
    // their recorded (unknown) outcomes and none is reported as superseded,
    // although frontier now reports each as citing an earlier revision.
    let (_, loss_body) = criterion(&records, "No acknowledged job is lost");
    let mut replacement = json!({
        "name": loss_body["name"], "kind": loss_body["kind"],
        "scope": {"kind": "all_options", "question_ref": loss_body["question_ref"]},
        "scale": loss_body["scale"], "preferred_direction": loss_body["preferred_direction"],
        "threshold": loss_body["threshold"], "weighting_rationale": loss_body["weighting_rationale"],
    });
    if let Some(weight) = loss_body.get("weight") {
        replacement["weight"] = weight.clone();
    }
    let case_ref = records
        .iter()
        .find(|(reference, _)| reference["id"] == case.as_str())
        .unwrap()
        .0
        .clone();
    let command = json!({
    "schema_version": "inv.command.v1", "command_id": "hard-unmeasured-restate",
    "case_id": case, "actor": "agent:cli-test", "preconditions": [case_ref],
    "operations": [
        {"op": "case.reopen", "payload": {"reason": "Restate the loss requirement for a review."}},
        {"op": "criterion.amend", "payload": {"criterion_ref": loss,
            "reason": "Restated for a review, unchanged.", "replacement": replacement}}
    ]});
    let receipt = run(&root, &["record", "--input", "-"], Some(&command));
    assert_eq!(receipt["result"], "committed", "{receipt:#}");
    let (_, after) = self::records(&root);
    let (restated, _) = criterion(&after, "No acknowledged job is lost");
    assert_eq!(restated["revision"], loss["revision"].as_u64().unwrap() + 1);
    let unknown = hard_unmeasured(&root, &case, &restated);
    assert_eq!(
        fact(&unknown, "hard_cell:unknown"),
        json!(cells(&after, &loss))
    );
    assert_eq!(
        fact(&unknown, "hard_cell:criterion_superseded"),
        Value::Null
    );
    assert!(
        !unknown.to_string().contains("freshness_trigger:"),
        "{unknown:#}"
    );
    // The recovery-time finding is unchanged by the unrelated restatement.
    let finding = hard_unmeasured(&root, &case, &recovery);
    assert_eq!(
        fact(&finding, "hard_cell:criterion_superseded"),
        json!(recovery_cells)
    );

    // A proposal that ignores both hard criteria is recorded with a warning
    // for each; one that states a hard condition for each has none.
    let option = |description: &str| {
        after
            .iter()
            .find(|(reference, body)| {
                reference["id"].as_str().unwrap().starts_with("opt_")
                    && body["description"] == description
            })
            .unwrap()
            .0
            .clone()
    };
    let broker = option("Run dedicated broker");
    let question = recovery_body["question_ref"].clone();
    let propose = |id: &str, conditions: Value| {
        let command = json!({
            "schema_version": "inv.command.v1", "command_id": id, "case_id": case,
            "actor": "agent:cli-test", "preconditions": [],
            "operations": [{"op": "decision.propose", "payload": {
                "question_ref": question, "choice": {"kind": "select", "option_ref": broker},
                "compared_alternatives": [broker], "comparison_refs": [],
                "hard_conditions": conditions, "evidence_refs": [], "assumption_refs": [],
                "tradeoffs": [], "consequences": ["Run a dedicated broker."],
                "reopen_trigger": "A recovery test loses an acknowledged job.",
                "reversal_conditions": []}}]});
        let receipt = run(&root, &["record", "--input", "-"], Some(&command));
        assert_eq!(receipt["result"], "committed", "{receipt:#}");
        receipt["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|warning| warning.as_str())
            .filter(|warning| warning.starts_with("Hard criterion "))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let warnings = propose("hard-unmeasured-ignored", json!([]));
    assert_eq!(warnings.len(), 2, "{warnings:#?}");
    for criterion in [&recovery, &restated] {
        let id = criterion["id"].as_str().unwrap();
        assert!(
            warnings.iter().any(
                |warning| warning.contains(id) && warning.contains("criterion.hard_unmeasured")
            ),
            "{id} in {warnings:#?}"
        );
    }
    let conditioned = propose(
        "hard-unmeasured-conditioned",
        json!([
            {"criterion_ref": recovery, "reason": "Accept only after a recovery test against the 60-second limit."},
            {"criterion_ref": restated, "reason": "Accept only after a loss test under the failure model."}
        ]),
    );
    assert!(conditioned.is_empty(), "{conditioned:#?}");
}
