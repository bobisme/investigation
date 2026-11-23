//! Acquisition identity: blob bytes deduplicate, acquisition identities do not;
//! reimport maps onto the recorded Source and a changed manifest revises it.

use std::{fs, path::Path};

use investigation_core::{
    evidence::{ArtifactBody, SourceBody},
    identity::{Id, Kind},
    wire::{CommitResult, ErrorCode},
};
use investigation_store::{
    Store, Workspace,
    store::ingest::{IngestReceipt, acquisition::ItemOutcome},
};
use serde_json::{Value, json};

const NAMESPACE: &str = "example-org.bench";

fn workspace() -> (tempfile::TempDir, std::path::PathBuf, Store) {
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
    let store = Store::open(Workspace::open(&root).unwrap());
    (temporary, root, store)
}

fn manifest(command: &str, case: Option<Id>, paths: &[&Path], key: Option<&str>) -> Value {
    let mut manifest = json!({
        "schema_version":"inv.ingest.v1",
        "command_id":command,
        "actor":"agent:acquisition-test",
        "source":{"title":"Benchmark output","format":"application/json"},
        "paths":paths,
        "extract_text":true,
    });
    match case {
        Some(case) => manifest["case_id"] = json!(case),
        None => manifest["new_case"] = json!({"title":"Compare two queue settings"}),
    }
    if let Some(external_id) = key {
        manifest["source"]["acquisition_key"] =
            json!({"namespace":NAMESPACE,"external_id":external_id});
    }
    manifest
}

fn ingest(store: &mut Store, manifest: &Value) -> IngestReceipt {
    store
        .ingest(&serde_json::to_vec(manifest).unwrap(), None)
        .unwrap()
}

fn case_of(receipt: &IngestReceipt) -> Id {
    receipt
        .receipt
        .created
        .iter()
        .find(|change| change.id.kind() == Kind::Case)
        .unwrap()
        .id
}

fn sources(store: &mut Store, case: Id) -> Vec<(Id, SourceBody)> {
    store
        .case(case)
        .unwrap()
        .state
        .records()
        .filter(|record| record.header.id.kind() == Kind::Source)
        .map(|record| {
            (
                record.header.id,
                serde_json::from_value(record.body.clone()).unwrap(),
            )
        })
        .collect()
}

fn blob_files(root: &Path) -> usize {
    fs::read_dir(root.join("state/blobs/sha256"))
        .unwrap()
        .map(|shard| fs::read_dir(shard.unwrap().path()).unwrap().count())
        .sum()
}

#[test]
fn identical_bytes_from_distinct_acquisitions_share_one_blob() {
    let (_temporary, root, mut store) = workspace();
    let path = root.join("bench-result.json");
    fs::write(&path, br#"{"latency.p99": 18.7}"#).unwrap();
    let first = ingest(
        &mut store,
        &manifest("run-1", None, &[&path], Some("run-001")),
    );
    let case = case_of(&first);
    let blobs = blob_files(&root);
    let second = ingest(
        &mut store,
        &manifest("run-2", Some(case), &[&path], Some("run-002")),
    );
    let unkeyed = ingest(&mut store, &manifest("run-3", Some(case), &[&path], None));

    let items = [&first.items[0], &second.items[0], &unkeyed.items[0]];
    assert!(
        items
            .iter()
            .all(|item| item.outcome == Some(ItemOutcome::Imported))
    );
    assert!(items.iter().all(|item| item.prior.is_none()));
    assert!(
        items
            .iter()
            .all(|item| item.digest == first.items[0].digest)
    );
    let mut identities: Vec<_> = items.iter().map(|item| item.source_id).collect();
    identities.dedup();
    assert_eq!(identities.len(), 3, "each acquisition keeps its own Source");
    assert_ne!(first.items[0].artifact_id, second.items[0].artifact_id);
    assert_eq!(
        second.items[0]
            .acquisition_key
            .as_ref()
            .unwrap()
            .external_id,
        "run-002"
    );
    assert!(unkeyed.items[0].acquisition_key.is_none());
    // Only each new manifest adds a blob; the retained source bytes are stored once.
    assert_eq!(blob_files(&root), blobs + 2);
    let recorded = sources(&mut store, case);
    assert_eq!(recorded.len(), 3);
    assert_eq!(
        recorded
            .iter()
            .filter(|(_, body)| body.acquisition_key.is_none())
            .count(),
        1
    );
    assert!(store.check().unwrap().integrity_ok());
}

#[test]
fn reimport_of_a_known_acquisition_is_idempotent_and_reports_the_prior_link() {
    let (_temporary, root, mut store) = workspace();
    let path = root.join("bench-result.json");
    fs::write(&path, b"latency.p99 18.7 ms\n").unwrap();
    let original = manifest("capture-1", None, &[&path], Some("bench-run-002"));
    let first = ingest(&mut store, &original);
    let case = case_of(&first);
    let item = &first.items[0];
    assert_eq!(item.outcome, Some(ItemOutcome::Imported));
    assert!(item.derivative_id.is_some());

    // Same command: the ordinary idempotent retry, with the original outcome.
    let retry = ingest(&mut store, &original);
    assert_eq!(retry.receipt.result, CommitResult::AlreadyCommitted);
    assert_eq!(retry.items[0].outcome, Some(ItemOutcome::Imported));
    assert_eq!(retry.items[0].source_id, item.source_id);

    // A historical manifest replayed under new command IDs, into the case or
    // as the original new-case manifest, records nothing new.
    for reimport in [
        manifest("replay-1", Some(case), &[&path], Some("bench-run-002")),
        manifest("replay-2", None, &[&path], Some("bench-run-002")),
    ] {
        let again = ingest(&mut store, &reimport);
        assert_eq!(again.receipt.result, CommitResult::AlreadyCommitted);
        assert_eq!(again.receipt.transaction_id, first.receipt.transaction_id);
        assert!(again.manifest_artifact_id.is_none());
        assert!(
            again
                .receipt
                .warnings
                .iter()
                .any(|warning| warning.contains("committed nothing"))
        );
        let reported = &again.items[0];
        assert_eq!(reported.outcome, Some(ItemOutcome::Deduplicated));
        assert_eq!(reported.source_id, item.source_id);
        assert_eq!(reported.artifact_id, item.artifact_id);
        assert_eq!(reported.derivative_id, item.derivative_id);
        let prior = reported.prior.as_ref().unwrap();
        assert_eq!(prior.case_id, case);
        assert_eq!(prior.source.id, item.source_id);
        assert_eq!(prior.source.revision, 1);
        assert_eq!(prior.artifact_id, Some(item.artifact_id));
        assert_eq!(prior.acceptance_sequence, 1);
    }
    assert_eq!(store.list_cases().unwrap().len(), 1);
    assert_eq!(store.case(case).unwrap().snapshot.sequence, 1);
    // A reimported request that committed nothing leaves its command ID free.
    let lookup = store
        .receipt(&"agent:acquisition-test".parse().unwrap(), "replay-1")
        .unwrap();
    assert!(matches!(
        lookup,
        investigation_store::store::ReceiptLookup::NotFoundAtSnapshot { .. }
    ));
    assert!(store.check().unwrap().integrity_ok());
}

#[test]
fn a_changed_manifest_revises_the_known_acquisition_instead_of_adding_a_trial() {
    let (_temporary, root, mut store) = workspace();
    let path = root.join("bench-result.json");
    fs::write(&path, b"latency.p99 18.7 ms\n").unwrap();
    let first = ingest(
        &mut store,
        &manifest("capture-1", None, &[&path], Some("bench-run-002")),
    );
    let case = case_of(&first);
    let item = first.items[0].clone();

    let mut described = manifest("capture-2", Some(case), &[&path], Some("bench-run-002"));
    described["source"]["verification_limits"] = json!(["Single synthetic workload"]);
    let revised = ingest(&mut store, &described);
    assert_eq!(revised.receipt.result, CommitResult::Committed);
    let reported = &revised.items[0];
    assert_eq!(reported.outcome, Some(ItemOutcome::Revised));
    assert_eq!(reported.source_id, item.source_id);
    assert_eq!(
        reported.artifact_id, item.artifact_id,
        "same bytes, same artifact"
    );
    assert_eq!(reported.derivative_id, item.derivative_id);
    assert_eq!(reported.prior.as_ref().unwrap().source.revision, 1);
    assert!(
        revised
            .receipt
            .created
            .iter()
            .all(|change| change.id.kind() != Kind::Source)
    );
    assert_eq!(revised.receipt.updated.len(), 1);
    assert_eq!(revised.receipt.updated[0].id, item.source_id);
    assert_eq!(revised.receipt.updated[0].revision, 2);

    let replay = store.case(case).unwrap();
    let history: Vec<_> = replay.state.history(item.source_id).collect();
    assert_eq!(history.len(), 2, "the revision is explicit lineage");
    let before: SourceBody = serde_json::from_value(history[0].body.clone()).unwrap();
    let after: SourceBody = serde_json::from_value(history[1].body.clone()).unwrap();
    assert_ne!(before.description_digest, after.description_digest);
    assert_eq!(after.verification_limits, ["Single synthetic workload"]);
    assert_eq!(
        before.acquisition.acquired_at, after.acquisition.acquired_at,
        "a revision describes the same acquisition"
    );
    assert_eq!(sources(&mut store, case).len(), 1);

    // Different bytes for a known item: another retained version of the same Source.
    fs::write(&path, b"latency.p99 19.2 ms\n").unwrap();
    described["command_id"] = json!("capture-3");
    let rebytes = ingest(&mut store, &described);
    let reported = rebytes.items[0].clone();
    assert_eq!(reported.outcome, Some(ItemOutcome::Revised));
    assert_eq!(reported.source_id, item.source_id);
    assert_ne!(reported.artifact_id, item.artifact_id);
    assert_ne!(reported.digest, item.digest);
    assert!(rebytes.receipt.updated.is_empty());
    let prior = reported.prior.clone().unwrap();
    assert_eq!(prior.source.revision, 2);
    assert_eq!(prior.artifact_id, Some(item.artifact_id));
    let artifact: ArtifactBody = serde_json::from_value(
        store
            .case(case)
            .unwrap()
            .state
            .current(reported.artifact_id)
            .unwrap()
            .body
            .clone(),
    )
    .unwrap();
    assert_eq!(artifact.source.id, item.source_id);
    assert_eq!(sources(&mut store, case).len(), 1);

    // A lost acknowledgement retried with the same command reconstructs the outcome.
    let retry = ingest(&mut store, &described);
    assert_eq!(retry.receipt.result, CommitResult::AlreadyCommitted);
    assert_eq!(retry.items[0].outcome, Some(ItemOutcome::Revised));
    assert_eq!(retry.items[0].artifact_id, reported.artifact_id);
    assert_eq!(retry.items[0].derivative_id, reported.derivative_id);
    assert_eq!(retry.items[0].prior, reported.prior);
    assert!(retry.receipt.warnings.is_empty());

    // A new item joins the known attempt; known items stay deduplicated.
    let extra = root.join("bench-log.txt");
    fs::write(&extra, b"steady-load reported_pass\n").unwrap();
    described["command_id"] = json!("capture-4");
    described["paths"] = json!([&path, &extra]);
    let widened = ingest(&mut store, &described);
    assert_eq!(widened.items[0].outcome, Some(ItemOutcome::Deduplicated));
    assert_eq!(widened.items[0].source_id, item.source_id);
    assert_eq!(widened.items[1].outcome, Some(ItemOutcome::Imported));
    assert!(widened.manifest_artifact_id.is_some());
    assert_eq!(sources(&mut store, case).len(), 2);

    let cursors = store.acquisition_cursors(NAMESPACE).unwrap();
    assert_eq!(cursors.len(), 2);
    let cursor = cursors
        .iter()
        .find(|cursor| cursor.source.id == item.source_id)
        .unwrap();
    assert_eq!(cursor.source.revision, 2);
    assert_eq!(cursor.case_id, case);
    assert_eq!(cursor.key.external_id, "bench-run-002");
    assert!(
        store
            .acquisition_cursors("another-namespace")
            .unwrap()
            .is_empty()
    );
    assert!(store.check().unwrap().integrity_ok());
}

#[test]
fn an_acquisition_belongs_to_one_case_and_keeps_its_classification() {
    let (_temporary, root, mut store) = workspace();
    let path = root.join("trace.txt");
    fs::write(&path, b"span checkout 41 ms\n").unwrap();
    let mut restricted = manifest("capture-1", None, &[&path], Some("trace-7"));
    restricted["classification"] = json!("restricted");
    let first = ingest(&mut store, &restricted);
    let home = case_of(&first);
    let other = case_of(&ingest(
        &mut store,
        &manifest("other", None, &[&path], None),
    ));

    let elsewhere = store
        .ingest(
            &serde_json::to_vec(&manifest(
                "capture-2",
                Some(other),
                &[&path],
                Some("trace-7"),
            ))
            .unwrap(),
            None,
        )
        .unwrap_err();
    assert_eq!(elsewhere.error.code, ErrorCode::InvalidInput);
    assert_eq!(
        elsewhere.error.path.as_deref(),
        Some("/source/acquisition_key")
    );
    assert_eq!(
        elsewhere.error.actual.as_ref().unwrap()["case_id"],
        json!(home)
    );

    // A changed description under a new case would silently fork the acquisition.
    let mut forked = restricted.clone();
    forked["command_id"] = json!("capture-3");
    forked["source"]["title"] = json!("Renamed trace");
    let failure = store
        .ingest(&serde_json::to_vec(&forked).unwrap(), None)
        .unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);

    let downgrade = store
        .ingest(
            &serde_json::to_vec(&manifest(
                "capture-4",
                Some(home),
                &[&path],
                Some("trace-7"),
            ))
            .unwrap(),
            None,
        )
        .unwrap_err();
    assert_eq!(downgrade.error.path.as_deref(), Some("/classification"));
    assert_eq!(store.list_cases().unwrap().len(), 2);
    assert_eq!(store.case(home).unwrap().snapshot.sequence, 1);

    let mut duplicate = manifest("capture-5", Some(home), &[&path, &path], Some("trace-7"));
    duplicate["classification"] = json!("restricted");
    let failure = store
        .ingest(&serde_json::to_vec(&duplicate).unwrap(), None)
        .unwrap_err();
    assert_eq!(failure.error.path.as_deref(), Some("/paths/1"));
    let mut bad = manifest("capture-6", Some(home), &[&path], Some("trace-7"));
    bad["source"]["acquisition_key"]["namespace"] = json!("../escape");
    let failure = store
        .ingest(&serde_json::to_vec(&bad).unwrap(), None)
        .unwrap_err();
    assert_eq!(
        failure.error.path.as_deref(),
        Some("/source/acquisition_key/namespace")
    );
}
