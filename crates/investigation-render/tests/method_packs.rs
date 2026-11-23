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
