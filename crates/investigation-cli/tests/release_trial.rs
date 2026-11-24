//! The release-trial harness (W14.05, plan §20.5): `scripts/release-trial.py
//! self-test`, run against the built binary.
//!
//! The self-test checks the seeded, counterbalanced schedule (every task type,
//! four domains, twins in opposite order, the sealed design's 672
//! assignments from its public commitments alone), the dry-run rubric keys,
//! and that the isolation probe and key scan catch a visible path and a
//! copied key. It checks the command-backend adapter conformance suite
//! against a stub adapter written to a temporary directory (and a broken
//! one, which must fail), and seals copies of development task instances
//! with a throwaway passphrase to exercise the sealed trial-task
//! commitments, source and key roots, and refusals. It then runs a full dry run of the development set offline
//! with the scripted stub participant: both arms of every task type that has
//! both, transcripts, answers, workspace snapshots, complete cost records,
//! blinded grading, mechanics grading, reconciliation, and a frozen,
//! read-only, tamper-evident record. Where bubblewrap or strace is
//! unavailable (some CI hosts) the dry run falls back to an unsandboxed,
//! untraced shell and records that; the sealed trial refuses that mode.

use std::{path::Path, process::Command};

use serde_json::Value;

fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
}

#[test]
fn release_trial_harness_dry_runs_every_task_type_in_both_arms() {
    let output = Command::new("python3")
        .arg(repository().join("scripts/release-trial.py"))
        .arg("self-test")
        .arg("--bin")
        .arg(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(repository())
        .output()
        .expect("python3 runs the release-trial self-test");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "the self-test printed no report ({error}): {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert!(output.status.success(), "{report:#}");
    assert_eq!(report["failed"], serde_json::json!([]), "{report:#}");
    assert!(report["checks"].as_u64().unwrap() >= 250, "{report:#}");
    let covered: Vec<&str> = report["covered"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    for task in ["family", "lookup", "task-5", "task-6", "task-7", "task-8"] {
        for arm in ["inv", "notes"] {
            let pair = format!("{task}/{arm}");
            assert!(
                covered.contains(&pair.as_str()),
                "{pair} missing: {report:#}"
            );
        }
    }
    // Tasks 9 and 10 are graded on journals and define only the CLI arm.
    assert!(covered.contains(&"task-9/inv") && covered.contains(&"task-10/inv"));
}
