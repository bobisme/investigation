//! `inv.producer-run.v1` ingestion: one Run per producer attempt, producer
//! statements kept as attributed source content, and validity never inferred.

use std::{fs, path::Path};

use investigation_core::{
    evidence::MeasurementValidity,
    explain::EvidenceCompleteness,
    header::Actor,
    header::ClassificationLevel,
    identity::{Id, Kind},
    producer_run::{ProducerRunRecord, Retention, ValidityClaim},
    wire::{CommitResult, ErrorCode},
};
use investigation_store::{
    Store, Workspace,
    store::ingest::producer_run::{ProducerItemOutcome, ProducerRunReceipt, ProducerRunSubmission},
};
use serde_json::{Value, json};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/producer-run/v1"
);

/// A workspace whose only import root is the checked-in fixture directory, or
/// a scratch directory when `scratch` is set.
fn workspace(scratch: bool) -> (tempfile::TempDir, std::path::PathBuf, Store) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let imports = if scratch {
        let imports = temporary.path().join("imports");
        fs::create_dir(&imports).unwrap();
        for name in ["bench-result.json", "envelope.json"] {
            fs::copy(Path::new(FIXTURE).join(name), imports.join(name)).unwrap();
        }
        imports
    } else {
        Path::new(FIXTURE).canonicalize().unwrap()
    };
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        config,
        contents.replace(
            "import_roots = []",
            &format!("import_roots = [{:?}]", imports.display().to_string()),
        ),
    )
    .unwrap();
    let store = Store::open(Workspace::open(&root).unwrap());
    (temporary, imports, store)
}

fn example() -> Value {
    serde_json::from_slice(&fs::read(Path::new(FIXTURE).join("envelope.json")).unwrap()).unwrap()
}

fn submission(actor: &str) -> ProducerRunSubmission {
    ProducerRunSubmission::new(actor.parse::<Actor>().unwrap())
}

fn ingest(
    store: &mut Store,
    envelope: &Value,
    submission: &ProducerRunSubmission,
) -> ProducerRunReceipt {
    store
        .ingest_producer_run(&serde_json::to_vec(envelope).unwrap(), submission)
        .unwrap()
}

fn kinds(store: &mut Store, case: Id) -> Vec<Kind> {
    let mut kinds: Vec<_> = store
        .case(case)
        .unwrap()
        .state
        .records()
        .map(|record| record.header.id.kind())
        .collect();
    kinds.sort();
    kinds
}

fn run_body(store: &mut Store, receipt: &ProducerRunReceipt) -> Value {
    store
        .case(receipt.case_id)
        .unwrap()
        .state
        .current(receipt.run.id)
        .unwrap()
        .body
        .clone()
}

fn statements(body: &Value) -> ProducerRunRecord {
    serde_json::from_value(body["producer_run"].clone()).unwrap()
}

#[test]
fn plan_example_ingests_as_one_run_with_attributed_assertions() {
    let (_temporary, _imports, mut store) = workspace(false);
    let receipt = ingest(&mut store, &example(), &submission("agent:recorder"));
    assert_eq!(receipt.receipt.result, CommitResult::Committed);
    assert_eq!(receipt.run_outcome, ProducerItemOutcome::Imported);
    assert_eq!(receipt.envelope.outcome, ProducerItemOutcome::Imported);
    assert_eq!(receipt.artifacts.len(), 1);
    assert_eq!(receipt.artifacts[0].outcome, ProducerItemOutcome::Imported);
    assert_eq!(receipt.artifacts[0].item, "artifact:bench-result.json");

    // One record-mode case, one Run, and a Source and Artifact for the envelope
    // and for the declared output. Nothing interpretive.
    let case = store.case(receipt.case_id).unwrap();
    let case_record = case.state.current(receipt.case_id).unwrap();
    assert_eq!(case_record.body["mode"], "record");
    assert_eq!(case_record.body["title"], "compare two queue settings");
    assert_eq!(
        kinds(&mut store, receipt.case_id),
        [
            Kind::Case,
            Kind::Source,
            Kind::Source,
            Kind::Artifact,
            Kind::Artifact,
            Kind::Run
        ]
    );

    let body = run_body(&mut store, &receipt);
    assert_eq!(body["execution_status"], "completed");
    assert_eq!(body["evidence_completeness"], "complete");
    assert_eq!(body["evidence_validity"], "unknown");
    assert_eq!(body["result_assertion"], Value::Null);
    assert_eq!(body["producer"], "agent:recorder");
    assert_eq!(body["tool"], "example-benchmark 0.1.0 (local-a)");
    let recorded = statements(&body);
    assert_eq!(recorded.producer.name, "example-benchmark");
    assert_eq!(recorded.external_run_id, "bench-run-002");
    assert_eq!(recorded.execution.exit_code, Some(0));
    assert_eq!(recorded.assertions.len(), 1);
    assert_eq!(recorded.assertions[0].name, "latency.p99");
    assert_eq!(recorded.assertions[0].value, json!(18.7));
    assert_eq!(
        recorded.assertions[0].validity_claim,
        ValidityClaim::ProducerReported
    );
    assert_eq!(body["producer_run"]["attribution"], "producer_reported");
    assert_eq!(recorded.controls[0].producer_result, "reported_pass");
    assert_eq!(
        recorded.limitations,
        ["Single synthetic workload; representative production profile not established"]
    );
    let Retention::Retained {
        artifact_ref,
        digest,
        ..
    } = &recorded.artifacts[0].retention
    else {
        panic!("the output was retained");
    };
    assert_eq!(Some(artifact_ref.id), receipt.artifacts[0].artifact_id);
    assert_eq!(Some(digest), receipt.artifacts[0].digest.as_ref());
    assert_eq!(
        body["artifact_refs"],
        json!([recorded.envelope.artifact_ref, artifact_ref])
    );

    // The envelope bytes are themselves retained source content, keyed by attempt.
    let envelope_source = case.state.current(recorded.envelope.source_ref.id).unwrap();
    assert_eq!(
        envelope_source.body["acquisition_key"],
        json!({"namespace":"example-benchmark","external_id":"bench-run-002","item":"envelope"})
    );
    assert_eq!(envelope_source.body["acquisition"]["channel"], "producer");
    assert!(
        envelope_source.body["verification_limits"][0]
            .as_str()
            .unwrap()
            .contains("attributed claim")
    );
    let envelope_bytes = serde_json::to_vec(&example()).unwrap();
    assert_eq!(
        receipt.envelope.size_bytes,
        Some(envelope_bytes.len() as u64)
    );
    assert!(store.check().unwrap().issues.is_empty());
}

#[test]
fn exit_code_zero_and_a_reported_pass_set_no_validity_and_promote_nothing() {
    let (_temporary, _imports, mut store) = workspace(false);
    let receipt = ingest(&mut store, &example(), &submission("agent:recorder"));
    assert_eq!(receipt.measurement_validity, MeasurementValidity::Unknown);
    let case = store.case(receipt.case_id).unwrap();
    assert!(!case.state.records().any(|record| matches!(
        record.header.id.kind(),
        Kind::Observation | Kind::Assessment | Kind::Conclusion | Kind::Decision
    )));
    for state in ["failed", "unknown"] {
        let mut envelope = example();
        envelope["external_run_id"] = json!(format!("bench-run-{state}"));
        envelope["execution"]["state"] = json!(state);
        envelope["execution"]["exit_code"] = json!(1);
        let mut other = submission("agent:recorder");
        other.case_id = Some(receipt.case_id);
        let failed = ingest(&mut store, &envelope, &other);
        let body = run_body(&mut store, &failed);
        // Execution, completeness and validity stay independent.
        assert_eq!(body["execution_status"], state);
        assert_eq!(body["evidence_completeness"], "complete");
        assert_eq!(body["evidence_validity"], "unknown");
    }

    // A producer can neither claim its result valid nor add a verdict field.
    let mut claimed = example();
    claimed["assertions"][0]["validity_claim"] = json!("valid");
    let failure = store
        .ingest_producer_run(
            &serde_json::to_vec(&claimed).unwrap(),
            &submission("agent:recorder"),
        )
        .unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);
    assert_eq!(
        failure.error.path.as_deref(),
        Some("/assertions/0/validity_claim")
    );
    let mut verdict = example();
    verdict["verdict"] = json!("pass");
    assert!(
        store
            .ingest_producer_run(
                &serde_json::to_vec(&verdict).unwrap(),
                &submission("agent:recorder")
            )
            .is_err()
    );
}

#[test]
fn identical_content_is_one_attempt_and_a_changed_envelope_revises_it() {
    let (_temporary, _imports, mut store) = workspace(false);
    let first = ingest(&mut store, &example(), &submission("agent:recorder"));
    let sequence = first.receipt.snapshot.sequence;

    // The same content from the same actor replays the command.
    let replay = ingest(&mut store, &example(), &submission("agent:recorder"));
    assert_eq!(replay.receipt.result, CommitResult::AlreadyCommitted);
    assert_eq!(replay.receipt.transaction_id, first.receipt.transaction_id);
    assert_eq!(replay.run, first.run);

    // Another actor delivering the same attempt records nothing new.
    let other = ingest(&mut store, &example(), &submission("agent:relay"));
    assert_eq!(other.receipt.result, CommitResult::AlreadyCommitted);
    assert_eq!(other.run_outcome, ProducerItemOutcome::Deduplicated);
    assert_eq!(other.envelope.outcome, ProducerItemOutcome::Deduplicated);
    assert_eq!(
        other.artifacts[0].outcome,
        ProducerItemOutcome::Deduplicated
    );
    assert_eq!(other.case_id, first.case_id);
    assert!(
        other
            .receipt
            .warnings
            .iter()
            .any(|warning| warning.contains("committed nothing"))
    );
    assert_eq!(
        store.case(first.case_id).unwrap().state.sequence(),
        sequence
    );

    // A changed envelope is a revision of the same Run and Sources.
    let mut changed = example();
    changed["assertions"][0]["value"] = json!(19.2);
    changed["metadata"] = json!({"example-org.instrument": {"sampling": [1, 2, null]}});
    let mut target = submission("agent:recorder");
    target.case_id = Some(first.case_id);
    let revised = ingest(&mut store, &changed, &target);
    assert_eq!(revised.receipt.result, CommitResult::Committed);
    assert_eq!(revised.run.id, first.run.id);
    assert_eq!(revised.run.revision, 2);
    assert_eq!(revised.run_outcome, ProducerItemOutcome::Revised);
    assert_eq!(revised.envelope.outcome, ProducerItemOutcome::Revised);
    assert_eq!(revised.envelope.source_id, first.envelope.source_id);
    assert_ne!(revised.envelope.artifact_id, first.envelope.artifact_id);
    // The output's declaration and bytes did not change.
    assert_eq!(
        revised.artifacts[0].outcome,
        ProducerItemOutcome::Deduplicated
    );
    assert_eq!(
        revised.artifacts[0].artifact_id,
        first.artifacts[0].artifact_id
    );
    let body = run_body(&mut store, &revised);
    assert_eq!(statements(&body).assertions[0].value, json!(19.2));
    assert_eq!(
        body["producer_run"]["metadata"],
        json!({"example-org.instrument": {"sampling": [1, 2, null]}})
    );
    assert_eq!(body["declared_at_sequence"], json!(sequence));
    // The earlier envelope stays referenced from the revised Run.
    let refs = body["artifact_refs"].as_array().unwrap();
    assert!(refs.contains(&json!({"id": first.envelope.artifact_id, "revision": 1})));
    let source = store
        .case(first.case_id)
        .unwrap()
        .state
        .current(revised.envelope.source_id.unwrap())
        .unwrap()
        .clone();
    assert_eq!(source.header.revision.0, 2);
    assert_eq!(source.body["metadata"], changed["metadata"]);

    // A changed envelope without the recorded case cannot open another one.
    let mut fresh_change = changed.clone();
    fresh_change["assertions"][0]["value"] = json!(20.0);
    let refused = store
        .ingest_producer_run(
            &serde_json::to_vec(&fresh_change).unwrap(),
            &submission("agent:recorder"),
        )
        .unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::InvalidInput);
    assert_eq!(refused.error.path.as_deref(), Some("/external_run_id"));
}

#[test]
fn a_digest_mismatch_or_missing_file_affects_only_that_item() {
    let (_temporary, imports, mut store) = workspace(true);
    fs::write(imports.join("trace.log"), "line 1\n").unwrap();
    let mut envelope = example();
    envelope["artifacts"] = json!([
        {"role": "original-output", "path": "./bench-result.json", "expected_sha256": "0".repeat(64), "media_type": "application/json"},
        {"role": "log", "path": "trace.log", "media_type": "text/plain", "classification": "restricted"},
        {"role": "log", "path": "absent/stderr.log"}
    ]);
    let receipt = ingest(&mut store, &envelope, &submission("agent:recorder"));
    let outcomes: Vec<_> = receipt.artifacts.iter().map(|item| item.outcome).collect();
    assert_eq!(
        outcomes,
        [
            ProducerItemOutcome::Rejected,
            ProducerItemOutcome::Imported,
            ProducerItemOutcome::Missing
        ]
    );
    assert_eq!(receipt.evidence_completeness, EvidenceCompleteness::Partial);
    assert!(receipt.artifacts[0].artifact_id.is_none());
    assert_eq!(receipt.receipt.warnings.len(), 2);
    let body = run_body(&mut store, &receipt);
    assert_eq!(body["evidence_completeness"], "partial");
    assert_eq!(body["evidence_validity"], "unknown");
    let recorded = statements(&body);
    assert!(matches!(
        recorded.artifacts[0].retention,
        Retention::Rejected { .. }
    ));
    assert!(matches!(
        recorded.artifacts[2].retention,
        Retention::Missing
    ));
    // The rejected bytes never reach the blob store; everything stored verifies.
    let report = store.check().unwrap();
    assert!(report.issues.is_empty(), "{:?}", report.issues);
    assert_eq!(report.verified_blobs, 2);
    // Restricted artifacts raise the Run's level and their own sensitivity.
    let case = store.case(receipt.case_id).unwrap();
    let run = case.state.current(receipt.run.id).unwrap();
    assert_eq!(
        run.header.classification.level,
        ClassificationLevel::Restricted
    );
    let log = case
        .state
        .current(receipt.artifacts[1].artifact_id.unwrap())
        .unwrap();
    assert_eq!(log.body["sensitivity"], "restricted");

    // Fixing the file records the item and completes the inventory.
    fs::create_dir(imports.join("absent")).unwrap();
    fs::write(imports.join("absent/stderr.log"), "stderr\n").unwrap();
    let mut target = submission("agent:recorder");
    target.case_id = Some(receipt.case_id);
    let fixed = ingest(&mut store, &envelope, &target);
    assert_eq!(fixed.artifacts[2].outcome, ProducerItemOutcome::Imported);
    assert_eq!(
        fixed.artifacts[1].outcome,
        ProducerItemOutcome::Deduplicated
    );
    assert_eq!(fixed.run.revision, 2);

    // Deleting a retained file later does not drop its retained entry.
    fs::remove_file(imports.join("trace.log")).unwrap();
    let later = ingest(&mut store, &envelope, &target);
    assert_eq!(later.artifacts[1].outcome, ProducerItemOutcome::Missing);
    assert!(
        later.artifacts[1]
            .diagnostics
            .iter()
            .any(|note| note.contains("earlier import"))
    );
    let body = run_body(&mut store, &later);
    assert!(statements(&body).artifacts[1].retention.is_retained());
}

#[test]
fn unsupported_versions_and_unsafe_paths_fail_before_anything_is_recorded() {
    let (_temporary, imports, mut store) = workspace(true);
    let mut future = example();
    future["schema_version"] = json!("inv.producer-run.v9");
    let failure = store
        .ingest_producer_run(
            &serde_json::to_vec(&future).unwrap(),
            &submission("agent:recorder"),
        )
        .unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);
    assert_eq!(failure.error.path.as_deref(), Some("/schema_version"));
    assert!(failure.error.message.contains("opaque"));

    for path in ["../escape.json", "/etc/hostname"] {
        let mut escaping = example();
        escaping["artifacts"][0]["path"] = json!(path);
        let failure = store
            .ingest_producer_run(
                &serde_json::to_vec(&escaping).unwrap(),
                &submission("agent:recorder"),
            )
            .unwrap_err();
        assert_eq!(failure.error.path.as_deref(), Some("/artifacts/0/path"));
    }
    std::os::unix::fs::symlink(imports.join("bench-result.json"), imports.join("link.json"))
        .unwrap();
    let mut linked = example();
    linked["artifacts"][0]["path"] = json!("link.json");
    let failure = store
        .ingest_producer_run(
            &serde_json::to_vec(&linked).unwrap(),
            &submission("agent:recorder"),
        )
        .unwrap_err();
    assert_eq!(failure.error.path.as_deref(), Some("/artifacts/0/path"));
    let mut oversized = example();
    oversized["metadata"] = json!({"example-org.blob": "x".repeat(70 * 1024)});
    assert!(
        store
            .ingest_producer_run(
                &serde_json::to_vec(&oversized).unwrap(),
                &submission("agent:recorder")
            )
            .is_err()
    );
    assert!(store.list_cases().unwrap().is_empty());
}

#[test]
fn an_ingested_run_reconciles_without_losing_producer_statements() {
    let (_temporary, _imports, mut store) = workspace(false);
    // The producer could not tell how execution ended.
    let mut envelope = example();
    envelope["execution"] = json!({"state": "unknown", "exit_code": null});
    let receipt = ingest(&mut store, &envelope, &submission("agent:recorder"));
    let body = run_body(&mut store, &receipt);
    let mut outcome = body.clone();
    for key in [
        "declared_at_sequence",
        "chronology",
        "evidence_validity",
        "external_ref",
        "ordering_receipt",
        "producer_run",
    ] {
        outcome.as_object_mut().unwrap().remove(key);
    }
    outcome["execution_status"] = json!("failed");
    outcome["result_assertion"] = json!("The investigator found the action failed.");
    let command = json!({
        "schema_version": "inv.command.v1", "command_id": "reconcile-run",
        "case_id": receipt.case_id, "actor": "agent:investigator",
        "preconditions": [receipt.run],
        "operations": [{"op": "run.reconcile", "payload": {
            "run_ref": receipt.run, "outcome": outcome,
            "rationale": "Annotate the producer run."
        }}]
    });
    store
        .record(&serde_json::to_vec(&command).unwrap())
        .unwrap();
    let reconciled = run_body(&mut store, &receipt);
    assert_eq!(reconciled["producer_run"], body["producer_run"]);
    assert_eq!(reconciled["execution_status"], "failed");
    // The producer's own report is unchanged beside the reconciled status.
    assert_eq!(reconciled["producer_run"]["execution"]["state"], "unknown");

    // Redelivering the unchanged envelope does not undo the reconciliation.
    let mut relay = submission("agent:relay");
    relay.case_id = Some(receipt.case_id);
    let again = ingest(&mut store, &envelope, &relay);
    assert_eq!(again.run_outcome, ProducerItemOutcome::Deduplicated);
    assert_eq!(run_body(&mut store, &receipt)["execution_status"], "failed");
    assert_eq!(reconciled["evidence_validity"], "unknown");
}

/// A receipt with a declared format is structure checked from its retained
/// bytes. An envelope Run pins no frozen plan, so nothing is compared and its
/// chronology stays `order_unknown`, shown with its verification level.
#[test]
fn declared_format_receipts_are_structure_checked_and_never_upgrade_an_envelope_run() {
    let (_temporary, imports, mut store) = workspace(true);
    let token =
        include_bytes!("../../investigation-core/src/receipt_check/testdata/timestamp-token.der");
    fs::write(imports.join("plan.tst"), token).unwrap();
    fs::write(imports.join("garbled.tst"), b"not DER").unwrap();
    let mut envelope = example();
    for path in ["plan.tst", "garbled.tst"] {
        envelope["artifacts"].as_array_mut().unwrap().push(json!({
            "role": "receipt", "path": path, "media_type": "application/timestamp-reply",
        }));
    }
    envelope["receipts"] = json!([
        {"kind": "timestamp", "format": "rfc3161", "artifact": "plan.tst"},
        {"kind": "timestamp", "format": "rfc3161", "artifact": "garbled.tst"},
        {"kind": "statement", "reference": "urn:example:statement:1"},
    ]);
    let receipt = ingest(&mut store, &envelope, &submission("agent:ingester"));
    assert!(
        receipt
            .receipt
            .warnings
            .iter()
            .any(|warning| warning.starts_with("Receipt 1 (timestamp): Not a well-formed rfc3161")),
        "{:?}",
        receipt.receipt.warnings
    );
    let case = store.case(receipt.case_id).unwrap();
    let run = case.state.revision(receipt.run).unwrap();
    assert_eq!(run.body["chronology"], "order_unknown");
    assert!(run.body.get("chronology_verification").is_none());
    let checks = run.body["receipt_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 3);
    assert_eq!(checks[0]["verification"], "structure_checked");
    assert_eq!(checks[0]["binding"], "no_frozen_plan");
    assert_eq!(checks[0]["format"], "rfc3161");
    assert!(checks[0]["artifact_ref"]["id"].is_string());
    assert_eq!(
        checks[0]["subject_digests"][0],
        "sha256:01ed774d4686cf4d74a26d8dfd351cf4b4f856deebc2a0285aaa5ed6ae657464"
    );
    assert_eq!(checks[1]["verification"], "structure_invalid");
    assert_eq!(checks[2]["verification"], "not_checked");
    // The statements themselves keep the declared format.
    let statements: ProducerRunRecord =
        serde_json::from_value(run.body["producer_run"].clone()).unwrap();
    assert_eq!(statements.receipts.len(), 3);
}
