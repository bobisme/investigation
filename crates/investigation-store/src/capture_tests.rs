//! Capture declaration tests. A child of `capture`, so it sees the store's
//! private commit path through the same module tree as production.
use std::{fs, os::unix::fs::PermissionsExt};

use super::*;

const ACTOR: &str = "agent:example-producer";

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    store: Store,
}

fn fixture() -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let store = Store::open(Workspace::init(&root, false).unwrap());
    Fixture {
        _temporary: temporary,
        root,
        store,
    }
}

fn declaration(key: &str, external_run_id: &str) -> CaptureDeclaration {
    CaptureDeclaration {
        command_id: key.into(),
        actor: ACTOR.parse().unwrap(),
        target: DeclarationTarget::NewRecordCase {
            title: "Synthetic runs".into(),
        },
        run: json!({
            "intent": {"kind": "exploration", "intent": "synthetic run"},
            "producer": ACTOR,
            "tool": "example-producer",
            "external_ref": external_run_id,
            "evidence_kind": "observational",
        }),
        producer: "example-producer".into(),
        external_run_id: external_run_id.into(),
        external_plan_digest: None,
        classification: ClassificationLevel::Internal,
        request: json!({"opaque": "recorder request", "external_run_id": external_run_id}),
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn begin_commits_one_declared_run_in_a_new_record_case_and_allocates_staging() {
    let mut fixture = fixture();
    let declared = fixture
        .store
        .declare_capture(&declaration("run-0001/begin", "run-0001"))
        .unwrap();
    assert_eq!(declared.receipt.result, CommitResult::Committed);
    assert_eq!(declared.receipt.snapshot.sequence, 1);
    let created: Vec<_> = declared.receipt.created.iter().map(|c| c.id).collect();
    assert_eq!(created, [declared.case_id, declared.run_id]);
    assert_eq!(declared.binding.handle.kind(), Kind::Handle);
    assert_eq!(
        declared.binding.staging,
        format!("state/staging/{}", declared.run_id)
    );

    let replay = fixture.store.case(declared.case_id).unwrap();
    let case = replay.state.current(declared.case_id).unwrap();
    assert_eq!(case.body["mode"], "record");
    let run = replay.state.current(declared.run_id).unwrap();
    assert_eq!(run.body["execution_status"], "declared");
    assert_eq!(run.body["evidence_completeness"], "unknown");
    assert_eq!(run.body["external_ref"], "run-0001");
    assert_eq!(
        run.body["capture"],
        serde_json::to_value(&declared.binding).unwrap()
    );
    let event = &replay.events[0].event;
    assert_eq!(event.request.input["schema_version"], "inv.capture.v1");
    assert_eq!(event.request.input["operation"], "capture.begin");
    assert_eq!(event.request.input["case_id"], Value::Null);

    let staging = fixture.root.join(&declared.binding.staging);
    assert_eq!(mode(&staging), 0o700);
    let binding = staging.join(BINDING_FILE);
    assert_eq!(mode(&binding), 0o400);
    let written: Value = serde_json::from_slice(&fs::read(&binding).unwrap()).unwrap();
    assert_eq!(written["schema_version"], STAGING_VERSION);
    assert_eq!(written["handle"], json!(declared.binding.handle));
    assert_eq!(written["run_id"], json!(declared.run_id));
    assert_eq!(
        written["transaction_id"],
        json!(declared.receipt.transaction_id)
    );
    assert_eq!(
        fs::read_dir(&staging).unwrap().count(),
        1,
        "no temporary remains"
    );

    // Recovery can see the handle and its staging immediately.
    let handles = fixture.store.capture_handles().unwrap();
    assert_eq!(handles.len(), 1);
    assert_eq!(handles[0].binding, declared.binding);
    assert_eq!(handles[0].staging, StagingState::Present);
    assert_eq!(handles[0].command_id, "run-0001/begin");
    assert_eq!(handles[0].request["opaque"], "recorder request");
    assert!(fixture.store.check().unwrap().integrity_ok());
}

#[test]
fn an_identical_retry_returns_the_same_run_handle_and_staging() {
    let mut fixture = fixture();
    let request = declaration("run-0001/begin", "run-0001");
    let first = fixture.store.declare_capture(&request).unwrap();
    let binding = fixture.root.join(&first.binding.staging).join(BINDING_FILE);
    let bytes = fs::read(&binding).unwrap();

    let retry = fixture.store.declare_capture(&request).unwrap();
    assert_eq!(retry.receipt.result, CommitResult::AlreadyCommitted);
    assert_eq!(retry.receipt.transaction_id, first.receipt.transaction_id);
    assert_eq!(retry.receipt.snapshot, first.receipt.snapshot);
    assert_eq!(
        (retry.case_id, retry.run_id, &retry.binding),
        (first.case_id, first.run_id, &first.binding)
    );
    assert_eq!(fs::read(&binding).unwrap(), bytes);
    assert_eq!(fixture.store.list_cases().unwrap().len(), 1);
    assert_eq!(fixture.store.capture_handles().unwrap().len(), 1);
}

#[test]
fn a_changed_request_or_a_second_key_for_the_same_attempt_conflicts() {
    let mut fixture = fixture();
    let first = fixture
        .store
        .declare_capture(&declaration("run-0001/begin", "run-0001"))
        .unwrap();

    let mut changed = declaration("run-0001/begin", "run-0001");
    changed.request["opaque"] = json!("different content");
    let error = fixture.store.declare_capture(&changed).unwrap_err();
    assert_eq!(error.error.code, ErrorCode::IdempotencyKeyReused);
    assert_eq!(error.error.path.as_deref(), Some("/command_id"));

    let error = fixture
        .store
        .declare_capture(&declaration("another-key", "run-0001"))
        .unwrap_err();
    assert_eq!(error.error.code, ErrorCode::IdempotencyKeyReused);
    assert_eq!(error.error.path.as_deref(), Some("/external_run_id"));
    let expected = error.error.expected.unwrap();
    assert_eq!(expected["command_id"], "run-0001/begin");
    assert_eq!(expected["run_id"], json!(first.run_id));
    assert_eq!(expected["handle"], json!(first.binding.handle));
    assert_eq!(
        error.error.repair.unwrap().argv.unwrap()[1..3],
        ["receipt".to_owned(), "run-0001/begin".to_owned()]
    );

    // Another producer may reuse the external run ID; namespaces do not collide.
    let mut other = declaration("other/begin", "run-0001");
    other.producer = "other-producer".into();
    fixture.store.declare_capture(&other).unwrap();
    assert_eq!(fixture.store.capture_handles().unwrap().len(), 2);
}

#[test]
fn begin_can_record_into_an_existing_case() {
    let mut fixture = fixture();
    let first = fixture
        .store
        .declare_capture(&declaration("run-0001/begin", "run-0001"))
        .unwrap();
    let mut second = declaration("run-0002/begin", "run-0002");
    second.target = DeclarationTarget::Existing(first.case_id);
    second.classification = ClassificationLevel::Restricted;
    let declared = fixture.store.declare_capture(&second).unwrap();
    assert_eq!(declared.case_id, first.case_id);
    assert_eq!(declared.receipt.snapshot.sequence, 2);
    assert_eq!(declared.receipt.created.len(), 1);
    let replay = fixture.store.case(first.case_id).unwrap();
    let run = replay.state.current(declared.run_id).unwrap();
    assert_eq!(
        run.header.classification.level,
        ClassificationLevel::Restricted
    );
    assert_eq!(
        replay.events[1].event.request.input["case_id"],
        json!(first.case_id)
    );
    assert!(fixture.store.check().unwrap().integrity_ok());

    let mut missing = declaration("run-0003/begin", "run-0003");
    missing.target =
        DeclarationTarget::Existing("case_019acd16-0000-7000-8000-000000000009".parse().unwrap());
    let error = fixture.store.declare_capture(&missing).unwrap_err();
    assert_eq!(error.error.code, ErrorCode::CaseRequired);
    assert_eq!(error.error.path.as_deref(), Some("/case_id"));
}

#[test]
fn catalog_failures_name_the_declaration_part_and_commit_nothing() {
    let mut fixture = fixture();
    let mut request = declaration("run-0001/begin", "run-0001");
    request.run["intent"] = json!({"kind": "exploration", "intent": "  "});
    let error = fixture.store.declare_capture(&request).unwrap_err();
    assert_eq!(error.error.code, ErrorCode::InvalidInput);
    assert_eq!(error.error.path.as_deref(), Some("/run/intent/intent"));
    assert!(fixture.store.list_cases().unwrap().is_empty());
    assert_eq!(
        fs::read_dir(fixture.root.join(STAGING_ROOT))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn a_retry_allocates_staging_that_a_crash_left_unallocated() {
    let mut fixture = fixture();
    let request = declaration("run-0001/begin", "run-0001");
    let declared = fixture.store.declare_capture(&request).unwrap();
    let staging = fixture.root.join(&declared.binding.staging);
    let binding = staging.join(BINDING_FILE);
    let bytes = fs::read(&binding).unwrap();

    // Simulate a crash after the commit and before staging was allocated.
    fs::remove_file(&binding).unwrap();
    fs::remove_dir(&staging).unwrap();
    let handles = fixture.store.capture_handles().unwrap();
    assert_eq!(handles[0].staging, StagingState::Missing);

    fixture.store.declare_capture(&request).unwrap();
    assert_eq!(fs::read(&binding).unwrap(), bytes);

    // A crash during publication can leave the temporary; a retry replaces it.
    fs::remove_file(&binding).unwrap();
    fs::write(staging.join("handle.json.tmp"), b"partial").unwrap();
    fixture.store.declare_capture(&request).unwrap();
    assert_eq!(fs::read(&binding).unwrap(), bytes);
    assert!(!staging.join("handle.json.tmp").exists());
    assert_eq!(
        fixture.store.capture_handles().unwrap()[0].staging,
        StagingState::Present
    );
}

#[test]
fn staging_bound_to_another_declaration_is_never_overwritten() {
    let mut fixture = fixture();
    let request = declaration("run-0001/begin", "run-0001");
    let declared = fixture.store.declare_capture(&request).unwrap();
    let binding = fixture
        .root
        .join(&declared.binding.staging)
        .join(BINDING_FILE);
    fs::remove_file(&binding).unwrap();
    fs::write(&binding, b"{\"foreign\": true}\n").unwrap();
    let error = fixture.store.declare_capture(&request).unwrap_err();
    assert_eq!(error.error.code, ErrorCode::IntegrityFailure);
    assert_eq!(fs::read(&binding).unwrap(), b"{\"foreign\": true}\n");
}

#[test]
fn declaration_paths_map_command_positions_to_declaration_parts() {
    let at = |path: &str, new_case| {
        declaration_path(
            Failure::new(ErrorCode::InvalidInput, "x").at(path),
            new_case,
        )
        .error
        .path
        .unwrap()
    };
    assert_eq!(at("/operations/0/payload/title", true), "/case/title");
    assert_eq!(
        at("/operations/1/payload/intent/plan_ref", true),
        "/run/intent/plan_ref"
    );
    assert_eq!(
        at("/operations/0/payload/external_ref", false),
        "/run/external_ref"
    );
    assert_eq!(at("/operations/0/op", false), "/run");
    assert_eq!(at("/case_id", false), "/case_id");
}

#[test]
fn reconciling_a_declared_run_keeps_its_capture_binding() {
    let mut fixture = fixture();
    let declared = fixture
        .store
        .declare_capture(&declaration("run-0001/begin", "run-0001"))
        .unwrap();
    let replay = fixture.store.case(declared.case_id).unwrap();
    let run = replay.state.current(declared.run_id).unwrap();
    let mut outcome = run.body.clone();
    for key in [
        "capture",
        "chronology",
        "declared_at_sequence",
        "external_ref",
        "evidence_validity",
        "ordering_receipt",
    ] {
        outcome.as_object_mut().unwrap().remove(key);
    }
    outcome["execution_status"] = json!("unknown");
    let command = json!({
        "schema_version": "inv.command.v1", "command_id": "reconcile",
        "actor": "agent:reviewer", "case_id": declared.case_id, "preconditions": [],
        "operations": [{"op": "run.reconcile", "payload": {
            "run_ref": {"id": declared.run_id, "revision": 1},
            "outcome": outcome,
            "rationale": "The producer stopped reporting."
        }}],
    });
    fixture
        .store
        .record(&serde_json::to_vec(&command).unwrap())
        .unwrap();
    let replay = fixture.store.case(declared.case_id).unwrap();
    let run = replay.state.current(declared.run_id).unwrap();
    assert_eq!(run.header.revision.0, 2);
    assert_eq!(run.body["execution_status"], "unknown");
    assert_eq!(
        run.body["capture"],
        serde_json::to_value(&declared.binding).unwrap()
    );
    assert_eq!(
        fixture.store.capture_handles().unwrap()[0].binding,
        declared.binding
    );
}
