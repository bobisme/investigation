use std::fs;

use investigation_core::{
    evidence::ArtifactBody,
    identity::Kind,
    wire::{CommitResult, ErrorCode},
};
use investigation_store::{Store, Workspace};
use serde_json::json;

#[test]
fn ingest_preserves_originals_and_creates_independent_acquisitions() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        config,
        contents.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    let workspace = Workspace::open(&root).unwrap();
    let original = "Observed José\r\nstatus: waiting\r\n".as_bytes();
    fs::write(root.join("source.txt"), original).unwrap();
    let path = root.join("source.txt");
    let mut store = Store::open(workspace);
    let mut manifest = json!({
        "schema_version":"inv.ingest.v0-alpha",
        "command_id":"capture-1",
        "actor":"agent:ingest-test",
        "new_case":{"title":"Inspect the retained trace"},
        "source":{"title":"Service log","format":"text/plain"},
        "paths":[path],
        "extract_text":true,
    });
    let raw_manifest = serde_json::to_vec_pretty(&manifest).unwrap();
    let first = store.ingest(&raw_manifest, None).unwrap();
    assert_eq!(first.receipt.snapshot.sequence, 1);
    let case = first
        .receipt
        .created
        .iter()
        .find(|change| change.id.kind() == Kind::Case)
        .unwrap()
        .id;
    let raw = &first.items[0];
    assert_eq!(raw.size_bytes as usize, original.len());
    let hex = raw.digest.as_str().strip_prefix("sha256:").unwrap();
    let saved = root.join("state/blobs/sha256").join(&hex[..2]).join(hex);
    assert_eq!(fs::read(&saved).unwrap(), original);

    let snapshot = store.case(case).unwrap();
    let derivative = snapshot.state.current(raw.derivative_id.unwrap()).unwrap();
    let derivative: ArtifactBody = serde_json::from_value(derivative.body.clone()).unwrap();
    assert_eq!(derivative.digest.as_ref(), Some(&raw.digest));
    assert_eq!(derivative.transforms[0].input.id, raw.artifact_id);
    let manifest_record = snapshot
        .state
        .current(first.manifest_artifact_id.unwrap())
        .unwrap();
    let artifact: ArtifactBody = serde_json::from_value(manifest_record.body.clone()).unwrap();
    assert_eq!(artifact.size_bytes, Some(raw_manifest.len() as u64));

    let retry = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap();
    assert_eq!(retry.receipt.result, CommitResult::AlreadyCommitted);
    assert_eq!(retry.receipt.created, first.receipt.created);
    assert_eq!(retry.manifest_artifact_id, first.manifest_artifact_id);
    assert_eq!(store.case(case).unwrap().snapshot.sequence, 1);

    manifest["command_id"] = json!("capture-2");
    manifest["case_id"] = json!(case);
    manifest.as_object_mut().unwrap().remove("new_case");
    let second = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap();
    assert_ne!(raw.source_id, second.items[0].source_id);
    assert_eq!(raw.digest, second.items[0].digest);
    assert!(store.check().unwrap().integrity_ok());

    fs::write(&path, b"Different source bytes").unwrap();
    let failure = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::IdempotencyKeyReused);
    assert_eq!(store.case(case).unwrap().snapshot.sequence, 2);
    fs::remove_file(saved).unwrap();
    let check = store.check().unwrap();
    assert!(!check.integrity_ok());
    let issue = check
        .issues
        .iter()
        .find(|issue| issue.code == "BLOB_MISSING")
        .unwrap();
    assert!(issue.objects.contains(&raw.artifact_id));
    assert!(issue.objects.contains(&second.items[0].artifact_id));
}

#[test]
fn failed_path_leaves_no_case_and_binary_stdin_keeps_its_bytes() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let mut store = Store::open(Workspace::init(&root, false).unwrap());
    let mut manifest = json!({
        "schema_version":"inv.ingest.v0-alpha",
        "command_id":"stdin-example",
        "actor":"agent:ingest-test",
        "new_case":{"title":"Inspect the capture"},
        "source":{"title":"Raw snapshot"},
        "paths":["absent.bin"],
        "extract_text":true,
    });
    assert!(
        store
            .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
            .is_err()
    );
    assert!(store.list_cases().unwrap().is_empty());
    manifest.as_object_mut().unwrap().remove("paths");
    manifest["content"] = json!({"kind":"stdin","name":"binary snapshot"});
    let mut binary = &b"\xff\x00"[..];
    let result = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), Some(&mut binary))
        .unwrap();
    assert_eq!(result.items[0].size_bytes, 2);
    assert!(result.items[0].derivative_id.is_none());
    assert!(store.check().unwrap().integrity_ok());
}

fn opaque_workspace() -> (tempfile::TempDir, std::path::PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        config,
        contents.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    (temporary, root)
}

#[test]
fn unknown_envelope_version_is_retained_opaque_with_a_diagnostic() {
    let (_guard, root) = opaque_workspace();
    let envelope =
        br#"{"schema_version":"inv.producer-run.v9","assertions":[{"name":"latency","value":1}]}"#;
    fs::write(root.join("run.json"), envelope).unwrap();
    let metadata = json!({"example-org.instrument": {"b": [1.5, null, "x"], "a": {}}});
    let manifest = json!({
        "schema_version":"inv.ingest.v1", "command_id":"opaque-1", "actor":"agent:test",
        "new_case":{"title":"Opaque"},
        "source":{"title":"Run","metadata":metadata},
        "paths":["run.json"],
    });
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let receipt = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap();
    let item = &receipt.items[0];
    assert_eq!(item.diagnostics.len(), 1);
    assert!(item.diagnostics[0].contains("inv.producer-run.v9"));
    let hex = item.digest.as_str().strip_prefix("sha256:").unwrap();
    let saved = root.join("state/blobs/sha256").join(&hex[..2]).join(hex);
    assert_eq!(fs::read(saved).unwrap(), envelope);
    let case = receipt
        .receipt
        .created
        .iter()
        .find(|change| change.id.kind() == Kind::Case)
        .unwrap()
        .id;
    // Nothing is mapped into typed facts: only the case, source, artifacts.
    assert!(
        receipt
            .receipt
            .created
            .iter()
            .all(|c| matches!(c.id.kind(), Kind::Case | Kind::Source | Kind::Artifact))
    );

    // A fresh replay of the journal returns the metadata exactly as supplied.
    let mut reopened = Store::open(Workspace::open(&root).unwrap());
    let replay = reopened.case(case).unwrap();
    let source = replay.state.current(item.source_id).unwrap();
    assert_eq!(
        serde_json::to_vec(&source.body["metadata"]).unwrap(),
        serde_json::to_vec(&metadata).unwrap()
    );
    assert!(
        source.body["verification_limits"][0]
            .as_str()
            .unwrap()
            .contains("opaque")
    );
}

#[test]
fn oversized_opaque_metadata_is_rejected_before_anything_commits() {
    let (_guard, root) = opaque_workspace();
    fs::write(root.join("a.txt"), "x").unwrap();
    let manifest = json!({
        "schema_version":"inv.ingest.v1", "command_id":"opaque-2", "actor":"agent:test",
        "new_case":{"title":"Opaque"},
        "source":{"title":"Run","metadata":{"example-org.big":"x".repeat(70_000)}},
        "paths":["a.txt"],
    });
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let failure = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);
    assert!(failure.error.message.contains("exceeds"));
}
