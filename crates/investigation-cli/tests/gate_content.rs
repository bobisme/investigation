//! Gate-content grading (W14.06, plan §20.5): the scripted rehearsal in
//! `scripts/grade-gate-content.py`, run against the built binary.
//!
//! The rehearsal writes good and strawman runs per metric through the public binary, reads each journal
//! after `investigation check`, and asserts that the strawman-rival rate and the checkable-retrospective
//! rate separate them. It also removes one clause at a time (a shared prediction, a paraphrase padding
//! one rival, a generic trigger, a vague action, praise) and asserts that the matching clause alone fails,
//! so neither metric is vacuous. Only development families and synthetic wording are used; the sealed
//! set is never opened.

use std::{path::Path, process::Command};

use serde_json::Value;

fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
}

#[test]
fn gate_content_metrics_separate_good_runs_from_strawman_runs_on_this_binary() {
    let output = Command::new("python3")
        .arg(repository().join("scripts/grade-gate-content.py"))
        .arg("rehearse")
        .arg("--bin")
        .arg(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(repository())
        .output()
        .expect("python3 runs the gate-content rehearsal");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "the rehearsal printed no report ({error}): {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert!(output.status.success(), "{report:#}");
    assert_eq!(report["failed"], serde_json::json!([]), "{report:#}");
    assert!(report["checks"].as_u64().unwrap() >= 45, "{report:#}");
    // Every development family contributes a good run with no strawman and a strawman run with all.
    for family in [
        "tide-gauge-dropout",
        "door-alarm-delay",
        "proofer-temperature-drift",
    ] {
        assert_eq!(
            report[family]["good"]["strawman"],
            serde_json::json!([0, 3]),
            "{family}"
        );
        assert_eq!(
            report[family]["strawman"]["strawman"],
            serde_json::json!([3, 3]),
            "{family}"
        );
        assert_eq!(
            report[family]["good"]["lessons"],
            serde_json::json!([2, 2]),
            "{family}"
        );
        assert_eq!(
            report[family]["strawman"]["lessons"],
            serde_json::json!([0, 3]),
            "{family}"
        );
    }
}
