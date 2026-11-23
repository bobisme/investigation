//! Method pack adoption through the public record and ingest contracts.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:cli-pack-test";

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
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
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{stdout}\n{:?}",
        output.stderr
    );
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    serde_json::from_str(&stdout).expect(&stdout)
}

fn record(root: &Path, key: &str, case: &str, revision: u64, operation: Value, exit: i32) -> Value {
    let command = json!({
        "schema_version": "inv.command.v1", "command_id": key, "actor": ACTOR,
        "case_id": case, "preconditions": [{"id": case, "revision": revision}],
        "operations": [operation],
    });
    run(
        root,
        &["record", "--input", "-", "--json"],
        Some(&command),
        exit,
    )
}

#[test]
fn an_adopted_pack_is_shown_with_the_case_and_never_blocks_reads() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    run(root, &["init", ".", "--no-vault", "--json"], None, 0);
    let config = root.join("investigation.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    let case = run(
        root,
        &[
            "case",
            "new",
            "Pinned guidance",
            "--actor",
            ACTOR,
            "--command-id",
            "case",
            "--json",
        ],
        None,
        0,
    );
    let case = case["created"][0]["id"].as_str().unwrap().to_owned();

    let pack = "schema_version = \"inv.method-pack.v1\"\nid = \"field-notes\"\nversion = \"1.0.0\"\n\
                title = \"Field notes\"\nsummary = \"Optional guidance.\"\nmodes = [\"explore\"]\n";
    fs::write(root.join("field-notes.toml"), pack).unwrap();
    let manifest = json!({
        "schema_version": "inv.ingest.v1", "command_id": "retain-pack", "actor": ACTOR,
        "case_id": case, "source": {"title": "Field notes pack", "format": "application/toml"},
        "paths": ["field-notes.toml"]
    });
    let retained = run(
        root,
        &["ingest", "--input", "-", "--json"],
        Some(&manifest),
        0,
    );
    let artifact = retained["items"][0]["artifact_id"].clone();
    let digest = retained["items"][0]["digest"].as_str().unwrap().to_owned();

    let schema = run(
        root,
        &["schema", "method_pack.adopt", "--schema-only", "--json"],
        None,
        0,
    );
    assert!(schema.to_string().contains("replaces"));
    let adopt = json!({"op": "method_pack.adopt", "payload": {
        "artifact_ref": {"id": artifact, "revision": 1}, "pack_id": "field-notes",
        "version": "1.0.0", "digest": digest, "reason": "Use shared field-note guidance."
    }});
    let adopted = record(root, "adopt", &case, 1, adopt.clone(), 0);
    assert_eq!(adopted["updated"][0]["id"], json!(case));

    fs::write(
        root.join("field-notes.toml"),
        pack.replace("Field notes", "Edited"),
    )
    .unwrap();
    let shown = run(root, &["show", &case, "--depth", "full", "--json"], None, 0);
    let packs = shown["method_packs"].as_array().unwrap();
    assert_eq!(packs[0]["pack_id"], "general");
    assert_eq!(packs[0]["origin"], "built_in");
    assert_eq!(packs[1]["status"], "available");
    assert_eq!(packs[1]["digest"], json!(digest));
    assert_eq!(packs[1]["pin"]["reason"], "Use shared field-note guidance.");

    let hex = digest.strip_prefix("sha256:").unwrap();
    fs::remove_file(root.join("state/blobs/sha256").join(&hex[..2]).join(hex)).unwrap();
    let shown = run(root, &["show", &case, "--depth", "full", "--json"], None, 0);
    assert_eq!(shown["method_packs"][1]["status"], "unavailable");
    assert_eq!(
        shown["method_packs"][1]["unavailable_reason"],
        "bytes_unavailable"
    );
    record(
        root,
        "question",
        &case,
        2,
        json!({"op": "question.create", "payload": {"wording": "What changed first?", "importance": "normal"}}),
        0,
    );
    assert!(
        run(root, &["brief", "--json"], None, 0)["ok"]
            .as_bool()
            .unwrap()
    );
    run(root, &["next", "--json"], None, 0);
}
