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
