//! W12.07: the optional viewer gate's adapter (`scripts/obsidian-ui-check.py`)
//! against a fake of the viewer's CLI, through
//! `scripts/test-obsidian-ui-check.py`.
//!
//! No viewer is needed or reachable: the adapter runs with only the fake (or
//! nothing) on PATH. A Base row the viewer does not list, an unresolved link
//! in a generated note and a Canvas the viewer does not know each fail their
//! check; a stale first unresolved-link answer passes only after a reload
//! and two agreeing answers (bn-17wk), and one that never settles fails; an
//! absent CLI skips with exit 0; the fake's argv log shows the
//! exact vault and paths; and removing each check from a copy of the adapter
//! makes the case that relies on it lose its outcome. The real-viewer run is
//! `just obsidian-ui`, never part of `just check`.

use std::{path::Path, process::Command};

fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
}

#[test]
fn the_viewer_gate_adapter_catches_each_fault_of_a_fake_cli() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(repository().join("scripts/test-obsidian-ui-check.py"))
        .arg(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(repository())
        .output()
        .expect("python3 runs the adapter tests");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("cases passed; 8 check removals each fail a case."),
        "{stdout}"
    );
}
