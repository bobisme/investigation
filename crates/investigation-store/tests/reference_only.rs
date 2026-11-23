//! Reference-only imports: an explicit external, unverified state; quotes and
//! reads that never treat a reference as retained bytes; reverification that
//! upgrades through a new revision or records a mismatch without replacing
//! the recorded digest.

use std::{fs, path::Path};

use investigation_core::{
    command::RevisionRef,
    evidence::{ArtifactAvailability, ArtifactBody, ArtifactIntegrity, SelectorRequest},
    external_ref::{DigestBasis, RetentionState, VerificationOutcome, VerificationState},
    freshness::{Freshness, FreshnessIndex, Trigger},
    identity::{Id, Kind},
    input,
    state::State,
    wire::{ErrorCode, Failure},
};
use investigation_store::{
    Store, Workspace,
    store::ingest::{IngestReceipt, acquisition::ItemOutcome},
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const ACTOR: &str = "agent:reference-test";
const TEXT: &str = "The relay holds at most 32 unacknowledged frames.\n";

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

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn ingest(store: &mut Store, manifest: &Value) -> Result<IngestReceipt, Failure> {
    store.ingest(&serde_json::to_vec(manifest).unwrap(), None)
}

fn external_manifest(command: &str, case: Option<Id>, locator: &str, bytes: &[u8]) -> Value {
    let mut manifest = json!({
        "schema_version":"inv.ingest.v1",
        "command_id":command,
        "actor":ACTOR,
        "source":{"title":"Archived specification","format":"text/plain"},
        "external_refs":[{"locator":locator,"digest":digest(bytes),"size_bytes":bytes.len()}],
    });
    match case {
        Some(case) => manifest["case_id"] = json!(case),
        None => manifest["new_case"] = json!({"title":"What window does the relay allow?"}),
    }
    manifest
}

fn reverify_manifest(command: &str, case: Id, artifact: RevisionRef, path: &Path) -> Value {
    json!({
        "schema_version":"inv.ingest.v1",
        "command_id":command,
        "actor":ACTOR,
        "case_id":case,
        "source":{"title":"Supplied copy"},
        "paths":[path],
        "reverify":{"artifact_ref":artifact},
    })
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

fn state(store: &mut Store, case: Id) -> State {
    store.case(case).unwrap().state
}

fn artifact(state: &State, reference: RevisionRef) -> ArtifactBody {
    input::decode(&state.revision(reference).unwrap().body).unwrap()
}

fn first(id: Id) -> RevisionRef {
    RevisionRef { id, revision: 1 }
}

fn blob_exists(root: &Path, bytes: &[u8]) -> bool {
    let hex = &digest(bytes)["sha256:".len()..];
    root.join("state/blobs/sha256")
        .join(&hex[..2])
        .join(hex)
        .exists()
}

fn selector(bytes: &[u8], end: usize) -> Value {
    json!({"schema_version":"inv.selector.v1","artifact_digest":digest(bytes),
        "selector":{"type":"byte_range","start":0,"end":end}})
}

/// Record a quoted Observation on the Artifact and return its revision and
/// the receipt's selector and quote state.
fn observe(
    store: &mut Store,
    case: Id,
    artifact: RevisionRef,
    bytes: &[u8],
) -> (RevisionRef, Value) {
    let quote = "The relay holds at most 32 unacknowledged frames.";
    let command = json!({
        "schema_version":"inv.command.v1","command_id":format!("observe-{}", artifact.revision),
        "case_id":case,"actor":ACTOR,"preconditions":[],
        "operations":[{"op":"observation.add","payload":{
            "value":{"kind":"text","statement":quote},
            "basis":{"kind":"source","artifact_ref":artifact,"selector":selector(bytes, quote.len()),
                "text_quote":{"exact":quote}},
            "extraction_method":"direct_read","validity":"unknown","quality":[],"limitations":[],
            "evidence_kind":"observational","variables_changed":[]}}],
    });
    let receipt = store
        .record(&serde_json::to_vec(&command).unwrap())
        .unwrap();
    let change = receipt.created.first().unwrap();
    let checks = json!(receipt.observation_checks);
    (
        RevisionRef {
            id: change.id,
            revision: change.revision,
        },
        checks[0].clone(),
    )
}

#[test]
fn an_external_reference_is_explicitly_unverified_and_never_read_as_retained_bytes() {
    let (_temporary, root, mut store) = workspace();
    let bytes = TEXT.as_bytes();
    let receipt = ingest(
        &mut store,
        &external_manifest("refer", None, "archive://shelf-4/spec-rev-c", bytes),
    )
    .unwrap();
    let case = case_of(&receipt);
    let item = &receipt.items[0];
    assert_eq!(item.outcome, Some(ItemOutcome::Imported));
    let retention = item
        .retention
        .as_ref()
        .expect("a reference reports its retention");
    assert_eq!(retention.state, RetentionState::SourceUnavailable);
    assert_eq!(retention.label, "external_unverified");
    assert!(!retention.offline_complete);
    // The receipt lists only retained bytes, and no blob was written.
    assert!(
        receipt
            .receipt
            .artifacts
            .iter()
            .all(|listed| listed.artifact.id != item.artifact_id)
    );
    assert!(!blob_exists(&root, bytes));

    let state = state(&mut store, case);
    let body = artifact(&state, first(item.artifact_id));
    assert_eq!(body.availability, ArtifactAvailability::External);
    assert_eq!(body.integrity, ArtifactIntegrity::NotChecked);
    let external = body.external.as_ref().unwrap();
    assert_eq!(external.locator, "archive://shelf-4/spec-rev-c");
    assert_eq!(external.digest_basis, DigestBasis::Declared);
    assert!(external.verifications.is_empty());
    let source = state.current(item.source_id).unwrap();
    assert_eq!(source.body["acquisition"]["channel"], "external_reference");

    // A read produces source_unavailable, not fabricated bytes.
    let replay = store.case(case).unwrap();
    let request: SelectorRequest = input::decode(&selector(bytes, 9)).unwrap();
    let failure = store
        .evidence_read(
            &replay,
            replay.state.current(item.artifact_id).unwrap(),
            request,
        )
        .unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::GateFailed);
    assert_eq!(
        failure.error.actual.as_ref().unwrap()["state"],
        "source_unavailable"
    );

    // A quote against the reference stays unavailable, never verified.
    let (_, checks) = observe(&mut store, case, first(item.artifact_id), bytes);
    assert_eq!(checks["selector_validation"], "unavailable");
    assert_eq!(checks["quote_state"], "unavailable");

    // check counts the reference without treating it as a verified blob.
    let report = store.check().unwrap();
    assert!(report.integrity_ok());
    assert_eq!(report.reference_only_artifacts, 1);
    assert_eq!(report.unavailable_artifacts, 1);
}

#[test]
fn matching_bytes_upgrade_the_reference_and_keep_dependents_current() {
    let (_temporary, root, mut store) = workspace();
    let bytes = TEXT.as_bytes();
    let receipt = ingest(
        &mut store,
        &external_manifest("refer", None, "archive://shelf-4/spec-rev-c", bytes),
    )
    .unwrap();
    let case = case_of(&receipt);
    let artifact_id = receipt.items[0].artifact_id;
    let (observation, _) = observe(&mut store, case, first(artifact_id), bytes);

    let path = root.join("supplied.txt");
    fs::write(&path, bytes).unwrap();
    let manifest = reverify_manifest("verify", case, first(artifact_id), &path);
    let verified = ingest(&mut store, &manifest).unwrap();
    let item = &verified.items[0];
    assert_eq!(item.outcome, Some(ItemOutcome::Reverified));
    assert!(verified.receipt.warnings.is_empty());
    let retention = item.retention.as_ref().unwrap();
    assert_eq!(retention.state, RetentionState::Retained);
    assert_eq!(retention.verification, Some(VerificationState::Verified));
    assert!(blob_exists(&root, bytes));

    let state = state(&mut store, case);
    let current = state.current(artifact_id).unwrap().reference();
    assert_eq!(current.revision, 2);
    let body = artifact(&state, current);
    assert_eq!(body.availability, ArtifactAvailability::Retained);
    assert_eq!(body.integrity, ArtifactIntegrity::Verified);
    assert_eq!(body.digest.unwrap().as_str(), digest(bytes));
    let entry = &body.external.as_ref().unwrap().verifications[0];
    assert_eq!(entry.outcome, VerificationOutcome::Verified);
    assert_eq!(entry.verified_by.to_string(), ACTOR);
    assert_eq!(entry.command_id, "verify");
    // The earlier revision is history, unchanged.
    assert_eq!(
        artifact(&state, first(artifact_id)).availability,
        ArtifactAvailability::External
    );
    // Recording that the cited bytes match is not a change to them.
    let index = FreshnessIndex::build(&state, case);
    assert_eq!(
        index.get(observation.id).unwrap().freshness,
        Freshness::Current
    );

    // Retained now: a read returns the bytes, and a new quote verifies.
    let replay = store.case(case).unwrap();
    let request: SelectorRequest = input::decode(&selector(bytes, 9)).unwrap();
    store
        .evidence_read(&replay, replay.state.current(artifact_id).unwrap(), request)
        .unwrap();
    let (_, checks) = observe(&mut store, case, current, bytes);
    assert_eq!(checks["selector_validation"], "verified");

    // An identical retry answers with the same outcome and records nothing.
    let retried = ingest(&mut store, &manifest).unwrap();
    assert_eq!(retried.items[0].outcome, Some(ItemOutcome::Reverified));
    assert_eq!(
        retried.receipt.transaction_id,
        verified.receipt.transaction_id
    );

    // The upgraded revision is retained: there is nothing left to reverify.
    let again = ingest(
        &mut store,
        &reverify_manifest("verify-again", case, current, &path),
    )
    .unwrap_err();
    assert_eq!(again.error.code, ErrorCode::InvalidInput);
    assert_eq!(again.error.path.as_deref(), Some("/reverify/artifact_ref"));
}

#[test]
fn a_changed_local_file_fails_reverification_and_marks_dependents() {
    let (_temporary, root, mut store) = workspace();
    let bytes = TEXT.as_bytes();
    let path = root.join("large-capture.txt");
    fs::write(&path, bytes).unwrap();
    let receipt = ingest(
        &mut store,
        &json!({
            "schema_version":"inv.ingest.v1","command_id":"refer-local","actor":ACTOR,
            "new_case":{"title":"What window does the relay allow?"},
            "source":{"title":"Large capture"},
            "paths":[path],"retention":"reference",
        }),
    )
    .unwrap();
    let case = case_of(&receipt);
    let item = &receipt.items[0];
    assert_eq!(item.digest.as_str(), digest(bytes));
    let retention = item.retention.as_ref().unwrap();
    assert_eq!(retention.state, RetentionState::SourceUnavailable);
    assert_eq!(retention.label, "external_verified");
    assert_eq!(retention.digest_basis, Some(DigestBasis::ReadAtIngest));
    // Hashed, not copied.
    assert!(!blob_exists(&root, bytes));
    let artifact_id = item.artifact_id;
    let (observation, checks) = observe(&mut store, case, first(artifact_id), bytes);
    assert_eq!(checks["quote_state"], "unavailable");

    // The external file changes; reverification reads it again.
    let changed = b"The relay holds at most 64 unacknowledged frames.\n";
    fs::write(&path, changed).unwrap();
    let manifest = json!({
        "schema_version":"inv.ingest.v1","command_id":"reverify-local","actor":ACTOR,
        "case_id":case,"source":{"title":"Large capture"},
        "paths":[path],"retention":"reference",
        "reverify":{"artifact_ref":first(artifact_id)},
    });
    let failed = ingest(&mut store, &manifest).unwrap();
    let item = &failed.items[0];
    assert_eq!(item.outcome, Some(ItemOutcome::ReverificationFailed));
    assert_eq!(item.digest.as_str(), digest(changed));
    assert_eq!(failed.receipt.warnings.len(), 1);
    let retention = item.retention.as_ref().unwrap();
    assert_eq!(retention.label, "external_mismatch");
    assert_eq!(retention.state, RetentionState::SourceUnavailable);

    let state = state(&mut store, case);
    let body = artifact(&state, state.current(artifact_id).unwrap().reference());
    // The recorded digest never changes, and nothing is retained in its place.
    assert_eq!(body.digest.unwrap().as_str(), digest(bytes));
    assert_eq!(body.size_bytes, Some(bytes.len() as u64));
    assert_eq!(body.availability, ArtifactAvailability::External);
    assert_eq!(body.integrity, ArtifactIntegrity::Mismatch);
    let log = &body.external.unwrap().verifications;
    assert_eq!(log.len(), 2);
    assert_eq!(log[1].outcome, VerificationOutcome::Mismatch);
    assert_eq!(log[1].observed_digest.as_str(), digest(changed));
    assert!(!blob_exists(&root, changed));

    // The Observation that cites the reference needs review, and says why.
    let index = FreshnessIndex::build(&state, case);
    let freshness = index.get(observation.id).unwrap();
    assert_eq!(freshness.freshness, Freshness::NeedsReview);
    assert_eq!(freshness.reasons[0].trigger, Trigger::ExternalMismatch);

    // A retry answers the same way without recording a second verification.
    let retried = ingest(&mut store, &manifest).unwrap();
    assert_eq!(
        retried.items[0].outcome,
        Some(ItemOutcome::ReverificationFailed)
    );
    assert_eq!(
        retried.receipt.transaction_id,
        failed.receipt.transaction_id
    );
}

#[test]
fn mismatching_copied_bytes_are_not_retained_and_a_stale_revision_conflicts() {
    let (_temporary, root, mut store) = workspace();
    let bytes = TEXT.as_bytes();
    let receipt = ingest(
        &mut store,
        &external_manifest("refer", None, "archive://shelf-4/spec-rev-c", bytes),
    )
    .unwrap();
    let case = case_of(&receipt);
    let artifact_id = receipt.items[0].artifact_id;
    let other = b"Not the archived specification.\n";
    let path = root.join("other.txt");
    fs::write(&path, other).unwrap();
    let failed = ingest(
        &mut store,
        &reverify_manifest("verify", case, first(artifact_id), &path),
    )
    .unwrap();
    assert_eq!(
        failed.items[0].outcome,
        Some(ItemOutcome::ReverificationFailed)
    );
    assert!(!blob_exists(&root, other));
    assert!(store.check().unwrap().integrity_ok());

    // Revision 1 is no longer current.
    fs::write(&path, TEXT).unwrap();
    let stale = ingest(
        &mut store,
        &reverify_manifest("verify-stale", case, first(artifact_id), &path),
    )
    .unwrap_err();
    assert_eq!(stale.error.code, ErrorCode::RevisionConflict);
    // The current revision can still be verified with the right bytes.
    let current = RevisionRef {
        id: artifact_id,
        revision: 2,
    };
    let verified = ingest(
        &mut store,
        &reverify_manifest("verify-current", case, current, &path),
    )
    .unwrap();
    assert_eq!(verified.items[0].outcome, Some(ItemOutcome::Reverified));
}

#[test]
fn retained_bytes_never_deduplicate_into_a_recorded_reference() {
    let (_temporary, root, mut store) = workspace();
    let bytes = TEXT.as_bytes();
    let path = root.join("spec.txt");
    fs::write(&path, bytes).unwrap();
    let key = json!({"namespace":"example-archive","external_id":"spec-rev-c"});
    let mut manifest = external_manifest("refer", None, &path.to_string_lossy(), bytes);
    manifest["source"]["acquisition_key"] = key.clone();
    let referenced = ingest(&mut store, &manifest).unwrap();
    let case = case_of(&referenced);
    // The same acquisition item, now with bytes: a new retained Artifact, not
    // a silent "deduplicated" answer pointing at the unretained reference.
    let retained = ingest(
        &mut store,
        &json!({
            "schema_version":"inv.ingest.v1","command_id":"retain","actor":ACTOR,"case_id":case,
            "source":{"title":"Archived specification","format":"text/plain","acquisition_key":key},
            "paths":[path],
        }),
    )
    .unwrap();
    let item = &retained.items[0];
    assert_ne!(item.outcome, Some(ItemOutcome::Deduplicated));
    assert_ne!(item.artifact_id, referenced.items[0].artifact_id);
    assert!(blob_exists(&root, bytes));
}

#[test]
fn reference_manifests_reject_ambiguous_or_misleading_requests() {
    let (_temporary, root, mut store) = workspace();
    let bytes = TEXT.as_bytes();
    let path = root.join("spec.txt");
    fs::write(&path, bytes).unwrap();
    let base = external_manifest("refer", None, "archive://a", bytes);
    let cases = [
        ("extract_text", json!(true), "/extract_text"),
        ("paths", json!([path]), "/external_refs"),
        ("retention", json!("reference"), "/retention"),
    ];
    for (field, value, at) in cases {
        let mut manifest = base.clone();
        manifest[field] = value;
        let failure = ingest(&mut store, &manifest).unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::InvalidInput, "{field}");
        assert_eq!(failure.error.path.as_deref(), Some(at), "{field}");
    }
    let mut duplicate = base.clone();
    duplicate["external_refs"] = json!([base["external_refs"][0], base["external_refs"][0]]);
    assert!(ingest(&mut store, &duplicate).is_err());
    // Reference mode needs local paths, and reverification needs a case.
    let mut stdin_reference = json!({
        "schema_version":"inv.ingest.v1","command_id":"ref-stdin","actor":ACTOR,
        "new_case":{"title":"Q"},"source":{"title":"S"},
        "content":{"kind":"stdin","name":"stream"},"retention":"reference",
    });
    assert_eq!(
        ingest(&mut store, &stdin_reference)
            .unwrap_err()
            .error
            .path
            .as_deref(),
        Some("/retention")
    );
    stdin_reference["retention"] = Value::Null;
    stdin_reference["reverify"] =
        json!({"artifact_ref":{"id":"art_018d87fc-5234-7000-8000-000000000003","revision":1}});
    assert_eq!(
        ingest(&mut store, &stdin_reference)
            .unwrap_err()
            .error
            .path
            .as_deref(),
        Some("/case_id")
    );
}
