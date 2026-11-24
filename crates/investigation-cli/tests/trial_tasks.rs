//! Release-trial tasks 5 to 7 (plan §20.5): the scripted rehearsal in
//! `scripts/trial-tasks.py`, run against the built binary.
//!
//! The rehearsal sets up every task deterministically, then replays a correct
//! path and at least one trap path through the same proxy a trial harness
//! installs. It asserts that the journal-based checks pass the correct path
//! and fail the trap, that the injected faults fire (a lost response, a
//! concurrent amendment, refused raw reads), that the rubric keys score a
//! warranted answer fully and flag an overclaiming one, and that no prompt
//! names the trap. The cold-agent runs themselves are W14.08.

use std::{path::Path, process::Command};

use serde_json::Value;

fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
}

#[test]
fn trial_tasks_5_to_7_set_up_inject_and_grade_on_this_binary() {
    let output = Command::new("python3")
        .arg(repository().join("scripts/trial-tasks.py"))
        .arg("rehearse")
        .arg("--bin")
        .arg(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(repository())
        .output()
        .expect("python3 runs the trial-task rehearsal");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "the rehearsal printed no report ({error}): {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert!(output.status.success(), "{report:#}");
    assert_eq!(report["failed"], serde_json::json!([]), "{report:#}");
    // Every task contributes: prompts, rubrics, setup, faults and grading.
    assert!(report["checks"].as_u64().unwrap() >= 90, "{report:#}");
}

/// `check` runs the content-independent rehearsal (prompts, rubric answers,
/// setup and grading in every arm, stage and condition) on instances whose
/// sources and keys come from explicit roots, as sealed instances do from an
/// opened bundle. A root without the sources fails, so the root is honoured.
#[test]
fn trial_task_check_reads_sources_and_keys_from_the_given_roots() {
    let run = |source_root: &Path| {
        let output = Command::new("python3")
            .arg(repository().join("scripts/trial-tasks.py"))
            .arg("check")
            .args(
                [
                    "task-5-stale-resume.json",
                    "task-6-lost-response-conflict.json",
                    "task-7-two-stage-injection.json",
                ]
                .map(|task| repository().join("training/trial-tasks/tasks").join(task)),
            )
            .arg("--bin")
            .arg(env!("CARGO_BIN_EXE_investigation"))
            .arg("--source-root")
            .arg(source_root)
            .arg("--key-root")
            .arg(repository())
            .current_dir(repository())
            .output()
            .expect("python3 runs the trial-task check");
        let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "the check printed no report ({error}): {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.success(), report)
    };
    let (ok, report) = run(&repository().join("fixtures/development/trial-tasks"));
    assert!(ok, "{report:#}");
    assert_eq!(report["failed"], serde_json::json!([]), "{report:#}");
    assert!(report["checks"].as_u64().unwrap() >= 30, "{report:#}");
    let (ok, report) = run(&repository().join("fixtures/development"));
    assert!(
        !ok && !report["failed"].as_array().unwrap().is_empty(),
        "{report:#}"
    );
}
