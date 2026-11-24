//! Archive pins at the command line (W11.09 follow-up): doctor lists every
//! pin, live or stale, with its heads, blob count, age and what it holds
//! back from reclamation; `archive pins release` frees stale pins and
//! refuses a live one, releasing nothing.
//!
//! The live pin is held by this test process, as a running archive holds
//! it; the stale one is a pin whose holder let go without releasing.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const ACTOR: &str = "agent:pin-tester";

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

/// A workspace with one case retaining one file.
fn workspace(root: &Path) -> PathBuf {
    let workspace = root.join("workspace");
    Workspace::init(&workspace, false).unwrap();
    let config = workspace.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        config,
        contents.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    fs::write(workspace.join("source.txt"), b"retained evidence\n").unwrap();
    let manifest = json!({
        "schema_version":"inv.ingest.v1", "command_id":"capture-1", "actor":ACTOR,
        "new_case":{"title":"Pinned evidence"},
        "source":{"title":"Log","format":"text/plain"},
        "paths":[workspace.join("source.txt")],
    });
    open(&workspace)
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap();
    workspace
}

fn open(root: &Path) -> Store {
    Store::open(Workspace::open(root).unwrap())
}

fn doctor(cwd: &Path, workspace: &str) -> Value {
    let (exit, value) = run(cwd, &["--workspace", workspace, "doctor"]);
    assert_eq!(exit, 0, "{value}");
    value
}

fn stale_finding(value: &Value) -> Option<&Value> {
    value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["code"] == "ARCHIVE_PIN_STALE")
}

#[test]
fn doctor_lists_pins_and_release_frees_only_stale_ones() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let root = workspace(cwd);
    let shown = root.to_str().unwrap();

    // No pins: no finding and an empty list.
    let healthy = doctor(cwd, shown);
    assert!(stale_finding(&healthy).is_none(), "{healthy}");
    assert_eq!(healthy["workspace"]["archive_pins"]["pins"], json!([]));

    let mut archiver = open(&root);
    let (_state, live) = archiver.read_canonical().unwrap();
    let live_id = live.id().unwrap().to_owned();
    let (_state, dead) = archiver.read_canonical().unwrap();
    let dead_id = dead.id().unwrap().to_owned();
    drop(dead);

    let report = doctor(cwd, shown);
    let pins = &report["workspace"]["archive_pins"];
    assert_eq!(pins["live"], 1, "{pins}");
    assert_eq!(pins["stale"], 1, "{pins}");
    let entry = |id: &str| -> Value {
        pins["pins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pin| pin["id"] == id)
            .cloned()
            .unwrap()
    };
    for (id, state) in [(&live_id, "live"), (&dead_id, "stale")] {
        let pin = entry(id);
        assert_eq!(pin["state"], state, "{pin}");
        assert_eq!(pin["heads"].as_array().unwrap().len(), 1, "{pin}");
        assert!(pin["blobs"].as_u64().unwrap() >= 1, "{pin}");
        assert!(pin["recorded_at_unix_seconds"].is_i64(), "{pin}");
        assert!(pin["age_seconds"].as_i64().unwrap() >= 0, "{pin}");
        // Nothing is withdrawn, so no pin holds anything back yet; a stale
        // pin never does.
        assert_eq!(pin["holds_back"]["blobs"], 0, "{pin}");
    }
    let finding = stale_finding(&report).expect("a stale pin is a finding");
    assert_eq!(finding["severity"], "warning");
    let reason = finding["reason"].as_str().unwrap();
    assert!(
        reason.contains(&dead_id) && !reason.contains(&live_id),
        "{reason}"
    );
    assert!(
        reason.contains("hold nothing back from reclamation"),
        "{reason}"
    );
    let repair: Vec<&str> = finding["repair"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap())
        .collect();
    assert!(
        repair.ends_with(&["archive", "pins", "release", "--stale", "--format", "json"]),
        "{repair:?}"
    );

    // Naming the live pin refuses the whole request; the stale one stays.
    let (exit, refused) = run(
        cwd,
        &[
            "--workspace",
            shown,
            "archive",
            "pins",
            "release",
            &dead_id,
            &live_id,
        ],
    );
    assert_eq!(exit, 5, "{refused}");
    assert_eq!(refused["error"]["code"], "GATE_FAILED");
    assert_eq!(refused["error"]["actual"]["live"], json!([live_id]));
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Nothing was released")
    );
    assert_eq!(archiver.archive_pins().unwrap().len(), 2);

    // --stale frees the stale record and leaves the live pin holding.
    let (exit, released) = run(
        cwd,
        &[
            "--workspace",
            shown,
            "archive",
            "pins",
            "release",
            "--stale",
        ],
    );
    assert_eq!(exit, 0, "{released}");
    assert_eq!(released["released"], json!([dead_id]));
    assert_eq!(released["live"][0]["id"], json!(live_id));
    let after = doctor(cwd, shown);
    assert!(stale_finding(&after).is_none(), "{after}");
    assert_eq!(after["workspace"]["archive_pins"]["live"], 1);

    // A repeated named release reports the record as already gone.
    let (exit, again) = run(
        cwd,
        &["--workspace", shown, "archive", "pins", "release", &dead_id],
    );
    assert_eq!(exit, 0, "{again}");
    assert_eq!(again["already_absent"], json!([dead_id]));

    archiver.release_pin(live).unwrap();
    assert!(open(&root).archive_pins().unwrap().is_empty());
}

#[test]
fn a_name_that_is_not_a_pin_is_invalid_input_and_touches_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let root = workspace(cwd);
    let pins = root.join("local/pins");
    fs::create_dir_all(&pins).unwrap();
    fs::write(pins.join("notes.json"), b"{}\n").unwrap();
    let (exit, value) = run(
        cwd,
        &[
            "--workspace",
            root.to_str().unwrap(),
            "archive",
            "pins",
            "release",
            "notes",
        ],
    );
    assert_eq!(exit, 2, "{value}");
    assert_eq!(value["error"]["code"], "INVALID_INPUT");
    assert!(pins.join("notes.json").is_file());
}
