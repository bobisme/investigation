//! A case pinned to a pack whose bytes are unavailable still renders.

use std::fs;

use investigation_render::{VerifiedSnapshot, ViewPolicy, render};
use investigation_store::{Store, Workspace};
use serde_json::json;

const PACK: &str = "schema_version = \"inv.method-pack.v1\"\nid = \"field-notes\"\n\
                    version = \"1.0.0\"\ntitle = \"Field notes\"\nsummary = \"Optional guidance.\"\n\
                    modes = [\"explore\"]\n";

#[test]
fn a_case_with_an_unavailable_pinned_pack_still_renders() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        contents.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let command = |key: &str, case: Option<&str>, revision: u64, op: serde_json::Value| {
        let preconditions = case
            .map(|id| vec![json!({"id": id, "revision": revision})])
            .unwrap_or_default();
        serde_json::to_vec(&json!({
            "schema_version": "inv.command.v1", "command_id": key, "actor": "agent:render-pack",
            "case_id": case, "preconditions": preconditions, "operations": [op],
        }))
        .unwrap()
    };
    let created = store
        .record(&command(
            "case",
            None,
            0,
            json!({"op":"case.create","payload":{"title":"Pinned","mode":"explore"}}),
        ))
        .unwrap();
    let case = created.created[0].id.to_string();
    fs::write(root.join("pack.toml"), PACK).unwrap();
    let retained = store
        .ingest(
            &serde_json::to_vec(&json!({
                "schema_version": "inv.ingest.v1", "command_id": "retain", "actor": "agent:render-pack",
                "case_id": case, "source": {"title": "Pack"}, "paths": [root.join("pack.toml")],
            }))
            .unwrap(),
            None,
        )
        .unwrap();
    let item = &retained.items[0];
    store
        .record(&command(
            "adopt",
            Some(&case),
            1,
            json!({"op":"method_pack.adopt","payload":{
                "artifact_ref": {"id": item.artifact_id, "revision": 1}, "pack_id": "field-notes",
                "version": "1.0.0", "digest": item.digest, "reason": "Pin the guidance."}}),
        ))
        .unwrap();
    let hex = item.digest.as_str().strip_prefix("sha256:").unwrap();
    fs::remove_file(root.join("state/blobs/sha256").join(&hex[..2]).join(hex)).unwrap();

    let plan = render(
        &VerifiedSnapshot::read(&mut store).unwrap(),
        &ViewPolicy::default(),
    )
    .unwrap();
    assert!(
        plan.entries()
            .iter()
            .any(|entry| entry.path().as_str().contains(&case))
    );
}

/// Fields a pinned pack declared render as a labeled `## Pack fields`
/// section of the record's note, with authored text escaped.
#[test]
fn pack_fields_render_as_labeled_data_on_the_record_note() {
    const BENCH: &str = include_str!("../../../fixtures/method-packs/bench-notes.toml");
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        contents.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let command = |key: &str, case: Option<&str>, op: serde_json::Value| {
        // Adoption revises the case, so it names the case revision it saw.
        let preconditions = match (case, op["op"].as_str()) {
            (Some(id), Some("method_pack.adopt")) => json!([{"id": id, "revision": 1}]),
            _ => json!([]),
        };
        serde_json::to_vec(&json!({
            "schema_version": "inv.command.v1", "command_id": key, "actor": "agent:render-pack",
            "case_id": case, "preconditions": preconditions, "operations": [op],
        }))
        .unwrap()
    };
    let created = store
        .record(&command(
            "case",
            None,
            json!({"op":"case.create","payload":{"title":"Bench","mode":"measure"}}),
        ))
        .unwrap();
    let case = created.created[0].id.to_string();
    fs::write(root.join("bench-notes.toml"), BENCH).unwrap();
    let retained = store
        .ingest(
            &serde_json::to_vec(&json!({
                "schema_version": "inv.ingest.v1", "command_id": "retain", "actor": "agent:render-pack",
                "case_id": case, "source": {"title": "Pack"}, "paths": [root.join("bench-notes.toml")],
            }))
            .unwrap(),
            None,
        )
        .unwrap();
    let item = &retained.items[0];
    store
        .record(&command(
            "adopt",
            Some(&case),
            json!({"op":"method_pack.adopt","payload":{
                "artifact_ref": {"id": item.artifact_id, "revision": 1}, "pack_id": "bench-notes",
                "version": "1.0.0", "digest": item.digest, "reason": "Bench bookkeeping."}}),
        ))
        .unwrap();
    let question = store
        .record(&command(
            "question",
            Some(&case),
            json!({"op":"question.create","payload":{"importance":"normal","wording":"Does warm-up matter?"}}),
        ))
        .unwrap();
    let hypothesis = store
        .record(&command(
            "hypothesis",
            Some(&case),
            json!({"op":"hypothesis.create","payload":{
                "question_ref": {"id": question.created[0].id, "revision": 1},
                "statement": "Warm-up inflates the first reading.",
                "possible_falsifiers": [], "rival_refs": [], "assumption_refs": []},
              "extensions": {"bench-notes": {"phases": ["warm", "steady"]}}}),
        ))
        .unwrap();
    let id = hypothesis.created[0].id.to_string();

    let plan = render(
        &VerifiedSnapshot::read(&mut store).unwrap(),
        &ViewPolicy::default(),
    )
    .unwrap();
    let note = plan
        .entries()
        .iter()
        .find(|entry| entry.path().as_str().contains(&id))
        .expect("the hypothesis has a note");
    let text = std::str::from_utf8(note.content()).unwrap();
    assert!(text.contains("\n## Pack fields\n"), "{text}");
    assert!(
        text.contains(r#"- `bench-notes` `phases`: `["warm","steady"]`"#),
        "{text}"
    );
    assert!(text.contains("never change a gate"), "{text}");
}
