//! Derivative artifacts: lineage to the original acquisition, shared origins,
//! and originals that stay byte-identical after any derivation.

use std::{fs, path::Path};

use investigation_core::{
    classification::{ClassificationLevel, Classifier},
    command::RevisionRef,
    derivation::{self, DerivationRecord},
    evidence::{ArtifactBody, ArtifactRole},
    freshness,
    identity::{Id, Kind},
    state::State,
    wire::ErrorCode,
};
use investigation_store::{
    Store, Workspace,
    store::ingest::{IngestReceipt, acquisition::ItemOutcome},
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

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

fn ingest(store: &mut Store, manifest: &Value) -> IngestReceipt {
    store
        .ingest(&serde_json::to_vec(manifest).unwrap(), None)
        .unwrap()
}

fn original(store: &mut Store, root: &Path, command: &str, case: Option<Id>) -> IngestReceipt {
    let path = root.join(format!("{command}.json"));
    fs::write(
        &path,
        format!(r#"{{"trace":"{command}","latency_ms":[3,5,8]}}"#),
    )
    .unwrap();
    let mut manifest = json!({
        "schema_version":"inv.ingest.v1",
        "command_id":command,
        "actor":"agent:derivation-test",
        "source":{"title":"Captured trace","format":"application/json"},
        "paths":[path],
        "classification":"internal",
    });
    match case {
        Some(case) => manifest["case_id"] = json!(case),
        None => manifest["new_case"] = json!({"title":"Where does the latency come from?"}),
    }
    ingest(store, &manifest)
}

fn derive_manifest(
    root: &Path,
    command: &str,
    case: Id,
    inputs: &[(Id, &Value)],
    transformation: &str,
    output: &str,
) -> Value {
    let path = root.join(format!("{command}.out"));
    fs::write(&path, output).unwrap();
    json!({
        "schema_version":"inv.ingest.v1",
        "command_id":command,
        "actor":"agent:derivation-test",
        "case_id":case,
        "source":{"title":format!("{transformation} output"),"format":"text/plain","producer":"example-reader 1.0"},
        "paths":[path],
        "derived_from":{
            "inputs":inputs.iter().map(|(id, digest)| json!({
                "artifact_ref":{"id":id,"revision":1},"digest":digest
            })).collect::<Vec<_>>(),
            "transformation":transformation,
            "version":"1.0.0",
        },
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

fn reference(id: Id) -> RevisionRef {
    RevisionRef { id, revision: 1 }
}

fn body(state: &State, id: Id) -> ArtifactBody {
    derivation::artifact_body(state, reference(id)).unwrap()
}

fn blob_path(root: &Path, digest: &str) -> std::path::PathBuf {
    let hex = digest.strip_prefix("sha256:").unwrap();
    root.join("state/blobs/sha256").join(&hex[..2]).join(hex)
}

#[test]
fn a_derivative_of_a_derivative_traces_to_the_original_acquisition() {
    let (_temporary, root, mut store) = workspace();
    let first = original(&mut store, &root, "capture", None);
    let case = case_of(&first);
    let original_id = first.items[0].artifact_id;
    let source_id = first.items[0].source_id;
    let original_digest = json!(first.items[0].digest);

    let extracted = ingest(
        &mut store,
        &derive_manifest(
            &root,
            "extract",
            case,
            &[(original_id, &original_digest)],
            "example-org.extract-latency",
            "3\n5\n8\n",
        ),
    );
    let extracted_item = &extracted.items[0];
    assert_eq!(extracted_item.outcome, Some(ItemOutcome::Derived));
    assert_eq!(extracted_item.source_id, source_id);
    assert!(extracted_item.prior.is_none() && extracted_item.acquisition_key.is_none());
    let extracted_id = extracted_item.artifact_id;
    // No new acquisition: the only created records are the derivative and the
    // retained manifest of the request.
    assert!(
        extracted
            .receipt
            .created
            .iter()
            .all(|change| change.id.kind() == Kind::Artifact)
    );

    let redacted = ingest(
        &mut store,
        &derive_manifest(
            &root,
            "redact",
            case,
            &[(extracted_id, &json!(extracted_item.digest))],
            "example-org.redact",
            "3\n5\n[removed]\n",
        ),
    );
    let redacted_id = redacted.items[0].artifact_id;
    let lineage = redacted.items[0].lineage.clone().unwrap();
    assert_eq!(lineage.role, ArtifactRole::Derivative);
    assert_eq!(lineage.depth, 2);
    assert_eq!(lineage.origins.len(), 1);
    assert_eq!(lineage.origins[0].source_ref, reference(source_id));
    assert_eq!(lineage.origins[0].artifact_ref, reference(original_id));
    assert_eq!(lineage.origins[0].role, ArtifactRole::Original);
    assert_eq!(
        lineage
            .steps
            .iter()
            .map(|step| (step.artifact_ref.id, step.transformation.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (redacted_id, "example-org.redact"),
            (extracted_id, "example-org.extract-latency"),
        ]
    );

    let state = state(&mut store, case);
    assert_eq!(
        state
            .records()
            .filter(|record| record.header.id.kind() == Kind::Source)
            .count(),
        1,
        "derivations never record a new acquisition"
    );
    let original_body = body(&state, original_id);
    assert_eq!(original_body.role, Some(ArtifactRole::Original));
    let redacted_body = body(&state, redacted_id);
    assert_eq!(redacted_body.role, Some(ArtifactRole::Derivative));
    assert_eq!(redacted_body.source, reference(source_id));
    let transform = &redacted_body.transforms[0];
    assert_eq!(transform.input, reference(extracted_id));
    assert_eq!(transform.version, "1.0.0");
    assert_eq!(
        transform.input_digest.as_ref(),
        Some(&extracted_item.digest)
    );
    assert_eq!(
        redacted_body.derivation,
        Some(DerivationRecord {
            label: Some("example-org.redact output".into()),
            producer: Some("example-reader 1.0".into()),
            ..DerivationRecord::default()
        })
    );
    assert_eq!(
        derivation::lineage(&state, reference(redacted_id)).unwrap(),
        lineage
    );

    // The freshness index and classification follow the same lineage fields.
    let edges = freshness::dependency_edges(state.current(redacted_id).unwrap()).unwrap();
    assert!(
        edges
            .iter()
            .any(|edge| edge.field == "transforms.input" && edge.target == reference(extracted_id))
    );

    // An identical retry answers with the same derived receipt.
    let retry = ingest(
        &mut store,
        &derive_manifest(
            &root,
            "redact",
            case,
            &[(extracted_id, &json!(extracted_item.digest))],
            "example-org.redact",
            "3\n5\n[removed]\n",
        ),
    );
    assert_eq!(retry.items[0].artifact_id, redacted_id);
    assert_eq!(retry.items[0].lineage.as_ref(), Some(&lineage));
}

#[test]
fn two_derivatives_of_one_acquisition_share_its_origin() {
    let (_temporary, root, mut store) = workspace();
    let first = original(&mut store, &root, "capture", None);
    let case = case_of(&first);
    let original_id = first.items[0].artifact_id;
    let digest = json!(first.items[0].digest);
    let screenshot = ingest(
        &mut store,
        &derive_manifest(
            &root,
            "screenshot",
            case,
            &[(original_id, &digest)],
            "example-org.render",
            "rendered trace",
        ),
    )
    .items[0]
        .artifact_id;
    let extraction = ingest(
        &mut store,
        &derive_manifest(
            &root,
            "json-extract",
            case,
            &[(original_id, &digest)],
            "example-org.extract",
            r#"{"p99":8}"#,
        ),
    )
    .items[0]
        .artifact_id;
    // A second acquisition, even of a similar trace, is a separate origin.
    let other = original(&mut store, &root, "second-capture", Some(case));
    let other_extraction = ingest(
        &mut store,
        &derive_manifest(
            &root,
            "other-extract",
            case,
            &[(other.items[0].artifact_id, &json!(other.items[0].digest))],
            "example-org.extract",
            r#"{"p99":8}"#,
        ),
    )
    .items[0]
        .artifact_id;

    let state = state(&mut store, case);
    let lineage = |id| derivation::lineage(&state, reference(id)).unwrap();
    assert_eq!(
        lineage(screenshot).shared_acquisitions(&lineage(extraction)),
        vec![first.items[0].source_id]
    );
    assert!(
        lineage(screenshot)
            .shared_acquisitions(&lineage(other_extraction))
            .is_empty()
    );
    let sharing: Vec<Id> = derivation::sharing_origin(&state, &lineage(screenshot))
        .into_iter()
        .map(|reference| reference.id)
        .collect();
    assert!(sharing.contains(&extraction) && sharing.contains(&original_id));
    assert!(!sharing.contains(&other_extraction));
    assert!(!sharing.contains(&other.items[0].artifact_id));

    // An aggregation of both acquisitions reaches both origins.
    let aggregate = ingest(
        &mut store,
        &derive_manifest(
            &root,
            "aggregate",
            case,
            &[(extraction, &Value::Null), (other_extraction, &Value::Null)],
            "example-org.aggregate",
            "p99 8 / 8",
        ),
    );
    let reached = aggregate.items[0].lineage.as_ref().unwrap().acquisitions();
    assert_eq!(
        reached,
        [first.items[0].source_id, other.items[0].source_id]
            .into_iter()
            .collect()
    );
}

#[test]
fn originals_stay_byte_identical_after_any_derivation() {
    let (_temporary, root, mut store) = workspace();
    let first = original(&mut store, &root, "capture", None);
    let case = case_of(&first);
    let original_id = first.items[0].artifact_id;
    let digest = first.items[0].digest.to_string();
    let blob = blob_path(&root, &digest);
    let before = fs::read(&blob).unwrap();
    let record_before = state(&mut store, case)
        .current(original_id)
        .unwrap()
        .clone();

    let mut manifest = derive_manifest(
        &root,
        "redact",
        case,
        &[(original_id, &json!(digest))],
        "example-org.redact",
        r#"{"trace":"[removed]"}"#,
    );
    manifest["extract_text"] = json!(true);
    manifest["derived_from"]["redaction_decisions"] = json!([
        {"scope":"trace name","action":"masked","rationale":"Names an internal host."}
    ]);
    manifest["derived_from"]["selectors"] = json!([{
        "schema_version":"inv.selector.v1","artifact_digest":digest,
        "selector":{"type":"json_pointer","pointer":"/trace","parser":null}
    }]);
    manifest["derived_from"]["review"] =
        json!({"method":"Read the whole output.","coverage":"whole_output"});
    let redacted = ingest(&mut store, &manifest);
    let item = &redacted.items[0];
    // The text derivative of a derivative also traces to the original.
    let text = item.derivative_id.unwrap();

    // Deriving from the original's exact bytes again records a new derivative
    // whose blob deduplicates with the original; the original is untouched.
    let copy = ingest(
        &mut store,
        &derive_manifest(
            &root,
            "copy",
            case,
            &[(original_id, &json!(digest))],
            "example-org.copy",
            &String::from_utf8(before.clone()).unwrap(),
        ),
    );
    assert_eq!(copy.items[0].digest.to_string(), digest);
    assert_ne!(copy.items[0].artifact_id, original_id);

    let after = fs::read(&blob).unwrap();
    assert_eq!(before, after);
    assert_eq!(
        format!("sha256:{:x}", Sha256::digest(&after)),
        digest,
        "the original blob still hashes to its recorded digest"
    );
    let state = state(&mut store, case);
    assert_eq!(state.current(original_id).unwrap(), &record_before);
    let text_lineage = derivation::lineage(&state, reference(text)).unwrap();
    assert_eq!(text_lineage.depth, 2);
    assert_eq!(text_lineage.origins[0].artifact_ref, reference(original_id));
    let redacted_body = body(&state, item.artifact_id);
    let record = redacted_body.derivation.unwrap();
    assert_eq!(record.redaction_decisions.len(), 1);
    assert_eq!(record.selectors.len(), 1);
    assert!(record.review.is_some());
    let report = store.check().unwrap();
    assert_eq!(report.unavailable_artifacts, 0);
}

#[test]
fn derivatives_inherit_the_strictest_input_classification() {
    let (_temporary, root, mut store) = workspace();
    let path = root.join("secret.json");
    fs::write(&path, r#"{"token":"x"}"#).unwrap();
    let first = ingest(
        &mut store,
        &json!({
            "schema_version":"inv.ingest.v1","command_id":"restricted","actor":"agent:derivation-test",
            "new_case":{"title":"Restricted capture"},
            "source":{"title":"Restricted capture"},"paths":[path],"classification":"restricted",
        }),
    );
    let case = case_of(&first);
    let digest = json!(first.items[0].digest);
    let step = ingest(
        &mut store,
        &derive_manifest(
            &root,
            "step-1",
            case,
            &[(first.items[0].artifact_id, &digest)],
            "example-org.extract",
            "token",
        ),
    );
    let chained = ingest(
        &mut store,
        &derive_manifest(
            &root,
            "step-2",
            case,
            &[(step.items[0].artifact_id, &json!(step.items[0].digest))],
            "example-org.summarize",
            "a token",
        ),
    );
    let state = state(&mut store, case);
    let classifier = Classifier::new(&state);
    let record = state.current(chained.items[0].artifact_id).unwrap();
    assert_eq!(
        record.header.classification.level,
        ClassificationLevel::Internal
    );
    assert_eq!(
        classifier.effective(record).level,
        ClassificationLevel::Restricted
    );
}

#[test]
fn derivation_requests_are_checked_against_recorded_inputs() {
    let (_temporary, root, mut store) = workspace();
    let first = original(&mut store, &root, "capture", None);
    let case = case_of(&first);
    let original_id = first.items[0].artifact_id;
    let digest = json!(first.items[0].digest);
    let manifest_id = first.manifest_artifact_id.unwrap();
    let reject = |store: &mut Store, manifest: Value, path: &str| {
        let failure = store
            .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
            .unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::InvalidInput, "{failure:?}");
        assert_eq!(failure.error.path.as_deref(), Some(path), "{failure:?}");
    };
    let base = || {
        derive_manifest(
            &root,
            "bad",
            case,
            &[(original_id, &digest)],
            "example-org.extract",
            "x",
        )
    };

    let mut wrong_digest = base();
    wrong_digest["derived_from"]["inputs"][0]["digest"] =
        json!(format!("sha256:{}", "0".repeat(64)));
    reject(&mut store, wrong_digest, "/derived_from/inputs/0/digest");

    let mut from_manifest = base();
    from_manifest["derived_from"]["inputs"][0] =
        json!({"artifact_ref":{"id":manifest_id,"revision":1}});
    reject(
        &mut store,
        from_manifest,
        "/derived_from/inputs/0/artifact_ref",
    );

    let mut unknown = base();
    unknown["derived_from"]["inputs"][0]["artifact_ref"]["revision"] = json!(2);
    reject(&mut store, unknown, "/derived_from/inputs/0/artifact_ref");

    let mut new_case = base();
    new_case.as_object_mut().unwrap().remove("case_id");
    new_case["new_case"] = json!({"title":"Elsewhere"});
    reject(&mut store, new_case, "/case_id");

    let mut keyed = base();
    keyed["source"]["acquisition_key"] = json!({"namespace":"example-org","external_id":"x"});
    reject(&mut store, keyed, "/source/acquisition_key");

    let mut stray_selector = base();
    stray_selector["derived_from"]["selectors"] = json!([{
        "schema_version":"inv.selector.v1","artifact_digest":format!("sha256:{}", "1".repeat(64)),
        "selector":{"type":"whole"}
    }]);
    reject(
        &mut store,
        stray_selector,
        "/derived_from/selectors/0/artifact_digest",
    );

    let mut twice = base();
    twice["derived_from"]["inputs"] = json!([
        {"artifact_ref":{"id":original_id,"revision":1}},
        {"artifact_ref":{"id":original_id,"revision":1}},
    ]);
    reject(&mut store, twice, "/derived_from/inputs/1/artifact_ref");

    // An input from another case is not visible to this one.
    let elsewhere = original(&mut store, &root, "elsewhere", None);
    let mut foreign = base();
    foreign["derived_from"]["inputs"][0] =
        json!({"artifact_ref":{"id":elsewhere.items[0].artifact_id,"revision":1}});
    reject(&mut store, foreign, "/derived_from/inputs/0/artifact_ref");

    // Nothing was committed by any rejected request.
    let state = state(&mut store, case);
    assert_eq!(
        state
            .records()
            .filter(|record| record.header.id.kind() == Kind::Artifact)
            .count(),
        2
    );
}
