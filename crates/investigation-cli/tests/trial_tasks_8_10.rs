//! Release-trial tasks 8-10 (plan §20.5) rehearsed on the built binary
//! through `scripts/trial-tasks-8-10.py self-check`.
//!
//! For each development instance under `training/trial-tasks/tasks-8-10/development/`
//! the script builds the participant workspace from the drill fixtures it
//! reuses, plays the scripted correct path and requires every deterministic
//! check to pass, then plays each scripted fault and requires exactly the
//! checks it names to fail. So a correct path exists on this binary and the
//! graders tell it apart from the failures the tasks exist to catch. The task
//! 8 setup is also checked against drill 1's pinned cold start.

use std::{path::Path, process::Command};

fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
}

#[test]
fn every_release_task_instance_has_a_passing_path_and_caught_faults() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(repository().join("scripts/trial-tasks-8-10.py"))
        .arg("self-check")
        .arg("--bin")
        .arg(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(repository())
        .output()
        .expect("python3 runs the release-task rehearsal");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("every correct path passes and every fault is caught"),
        "{stdout}"
    );
}
