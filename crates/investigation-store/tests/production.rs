use std::{
    fs::{self, OpenOptions},
    path::Path,
    process::{Child, Command, Stdio},
};

use investigation_core::{
    identity::Id,
    wire::{CommitResult, ErrorCode, ProjectionStatus, Receipt},
};
use investigation_store::{Store, Workspace, store::ReceiptLookup};
use serde_json::{Value, json};

fn create(key: &str, title: &str) -> Value {
    command(
        key,
        None,
        json!({"op":"case.create", "payload":{"title":title,"mode":"record"}}),
    )
}

fn command(key: &str, case_id: Option<Id>, operation: Value) -> Value {
    json!({
        "schema_version":"inv.command.v0-alpha",
        "command_id":key,
        "case_id":case_id,
        "actor":"agent:integration",
        "preconditions":[],
        "operations":[operation]
    })
}

fn submit(store: &mut Store, input: &Value) -> Result<Receipt, investigation_core::wire::Failure> {
    store.record(&serde_json::to_vec(input).unwrap())
}

fn child(root: &Path, name: &str, input: &Value) -> Child {
    let request = root.join(format!("{name}.request.json"));
    let response = root.join(format!("{name}.response.json"));
    fs::write(&request, serde_json::to_vec_pretty(input).unwrap()).unwrap();
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "record_worker", "--nocapture"])
        .env("INV_TEST_WORKSPACE", root)
        .env("INV_TEST_STORE_INPUT", request)
        .env("INV_TEST_STORE_OUTPUT", response)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn finish(child: Child, root: &Path, name: &str) -> Value {
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&fs::read(root.join(format!("{name}.response.json"))).unwrap()).unwrap()
}

#[test]
fn record_worker() {
    let Some(root) = std::env::var_os("INV_TEST_WORKSPACE") else {
        return;
    };
    let root = Path::new(&root);
    let mut store = Store::open(Workspace::open(root).unwrap());
    let input = fs::read(std::env::var_os("INV_TEST_STORE_INPUT").unwrap()).unwrap();
    let reply = match store.record(&input) {
        Ok(receipt) => serde_json::to_vec(&receipt).unwrap(),
        Err(failure) => serde_json::to_vec(&failure).unwrap(),
    };
    fs::write(std::env::var_os("INV_TEST_STORE_OUTPUT").unwrap(), reply).unwrap();
}

#[test]
fn processes_serialize_additions_and_recover_the_original_receipt() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let workspace = Workspace::init(&root, false).unwrap();
    let first = create("case-1", "Production path fixture");
    let child_result = finish(child(&root, "initial", &first), &root, "initial");
    let receipt: Receipt = serde_json::from_value(child_result).unwrap();
    let id = receipt.created[0].id;
    let mut store = Store::open(workspace);

    let retry = submit(&mut store, &first).unwrap();
    assert_eq!(retry.result, CommitResult::AlreadyCommitted);
    assert_eq!(retry.transaction_id, receipt.transaction_id);
    assert_eq!(retry.created, receipt.created);
    assert_eq!(retry.snapshot, receipt.snapshot);

    let add = |key: &str, wording: &str| {
        command(
            key,
            Some(id),
            json!({
                "op":"question.create","payload":{"wording":wording,"importance":"normal"}
            }),
        )
    };
    let input_a = add("add-a", "Which observation was retained?");
    let input_b = add("add-b", "What remains undecided?");
    let a = child(&root, "a", &input_a);
    let b = child(&root, "b", &input_b);
    let a = finish(a, &root, "a");
    let b = finish(b, &root, "b");
    assert_eq!(a["ok"], true);
    assert_eq!(b["ok"], true);
    let mut sequences = [
        a["snapshot"]["sequence"].as_u64().unwrap(),
        b["snapshot"]["sequence"].as_u64().unwrap(),
    ];
    sequences.sort();
    assert_eq!(sequences, [2, 3]);
    assert_eq!(store.case(id).unwrap().state.records().count(), 3);

    let mut stale = add("stale", "Must fail at the earlier case revision");
    stale["preconditions"] = json!([{"id":id,"revision":2}]);
    let error = submit(&mut store, &stale).unwrap_err();
    assert_eq!(error.error.code, ErrorCode::RevisionConflict);
    assert_eq!(error.error.actual.unwrap()["revision"], 1);
    let mismatch = submit(&mut store, &create("case-1", "Different payload")).unwrap_err();
    assert_eq!(mismatch.error.code, ErrorCode::IdempotencyKeyReused);
    assert_eq!(store.case(id).unwrap().snapshot.sequence, 3);

    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("state/locks/commit.lock"))
        .unwrap();
    lock.lock().unwrap();
    let result = finish(
        child(
            &root,
            "blocked",
            &add("blocked", "Wait for the existing commit lock"),
        ),
        &root,
        "blocked",
    );
    assert_eq!(result["error"]["code"], "WORKSPACE_BUSY");
    drop(lock);
    assert_eq!(store.case(id).unwrap().snapshot.sequence, 3);
}

#[test]
fn competing_processes_return_the_winning_revision_and_bounded_diff() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let mut store = Store::open(Workspace::init(&root, false).unwrap());
    let case = submit(&mut store, &create("initial", "Original scope"))
        .unwrap()
        .created[0]
        .id;
    let amend = |key: &str, title: &str| {
        let mut request = command(
            key,
            Some(case),
            json!({"op":"case.amend","payload":{"title":title,"reason":"New workload description"}}),
        );
        request["preconditions"] = json!([{"id":case,"revision":1}]);
        request
    };
    let a = child(&root, "amend-a", &amend("amend-a", "Battery tests"));
    let b = child(&root, "amend-b", &amend("amend-b", "Power supply tests"));
    let during = store.case(case).unwrap();
    assert_eq!(during.events.len() as u64, during.snapshot.sequence);
    assert_eq!(
        during.state.current(case).unwrap().header.revision.0,
        during.snapshot.sequence
    );
    assert_eq!(
        during.events.last().unwrap().hash,
        during.snapshot.head_hash
    );
    let [a, b] = [finish(a, &root, "amend-a"), finish(b, &root, "amend-b")];
    let (winner, loser) = if a["ok"] == true { (a, b) } else { (b, a) };
    assert_eq!(winner["updated"][0]["revision"], 2);
    assert_eq!(loser["commit_status"], "not_committed");
    let conflict = &loser["error"];
    assert_eq!(conflict["code"], "REVISION_CONFLICT");
    assert_eq!(conflict["expected"], json!({"id":case,"revision":1}));
    assert_eq!(conflict["actual"]["id"], json!(case));
    assert_eq!(conflict["actual"]["revision"], 2);
    assert!(
        conflict["actual"]["changed_fields_sample"]
            .as_array()
            .unwrap()
            .contains(&json!("title"))
    );
    assert_eq!(conflict["retry"], "reassess");
    assert_eq!(conflict["repair"]["argv"][1], "show");
    assert_eq!(store.case(case).unwrap().snapshot.sequence, 2);
}

#[test]
fn index_failure_after_publication_preserves_the_commit() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let mut store = Store::open(Workspace::init(&root, false).unwrap());
    let database = root.join("cache/index.sqlite");
    fs::create_dir(&database).unwrap();
    let receipt = submit(&mut store, &create("commit-with-warning", "Index recovery")).unwrap();
    assert_eq!(receipt.projection.index, ProjectionStatus::Degraded);
    assert!(!receipt.warnings.is_empty());
    assert_eq!(store.list_cases().unwrap()[0].snapshot, receipt.snapshot);
    let report = store.check().unwrap();
    assert!(report.integrity_ok());
    assert_eq!(report.index, ProjectionStatus::Degraded);

    fs::remove_dir(&database).unwrap();
    store.repair_index().unwrap();
    let ReceiptLookup::Found { receipt: recovered } = store
        .receipt(&"agent:integration".parse().unwrap(), "commit-with-warning")
        .unwrap()
    else {
        panic!("verified receipt should survive deleting the index");
    };
    assert_eq!(recovered.snapshot, receipt.snapshot);
    assert_eq!(recovered.created, receipt.created);
    assert_eq!(recovered.transaction_id, receipt.transaction_id);
    assert_eq!(store.check().unwrap().index, ProjectionStatus::Current);
}

#[test]
fn replay_reports_duplicate_sequences_and_unknown_versions_in_place() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let mut store = Store::open(Workspace::init(&root, false).unwrap());
    let receipt = submit(&mut store, &create("original", "Integrity check fixture")).unwrap();
    let id = receipt.created[0].id;
    let events = root.join("state/cases").join(id.to_string()).join("events");
    let original_path = events.join("000000000001.json");
    let original = fs::read(&original_path).unwrap();
    let duplicate_path = events.join("1.json");
    fs::copy(&original_path, &duplicate_path).unwrap();
    let report = store.check().unwrap();
    assert!(!report.integrity_ok());
    assert_eq!(report.issues[0].code, "DUPLICATE_EVENT_SEQUENCE");
    fs::remove_file(duplicate_path).unwrap();
    assert_eq!(fs::read(&original_path).unwrap(), original);

    let question = command(
        "next",
        Some(id),
        json!({
            "op":"question.create",
            "payload":{"wording":"Which source is available?","importance":"normal"}
        }),
    );
    submit(&mut store, &question).unwrap();
    assert_eq!(store.case(id).unwrap(), store.case(id).unwrap());
    let second_path = events.join("000000000002.json");
    let second = fs::read(&second_path).unwrap();

    fs::remove_file(&original_path).unwrap();
    assert_eq!(store.check().unwrap().issues[0].code, "MISSING_EVENT");
    assert!(!original_path.exists());
    fs::write(&original_path, &original).unwrap();

    let replace_second = |bytes: &[u8]| {
        fs::remove_file(&second_path).unwrap();
        fs::write(&second_path, bytes).unwrap();
    };
    let mut mismatched: Value = serde_json::from_slice(&second).unwrap();
    mismatched["previous_hash"] = json!(format!("sha256:{}", "a".repeat(64)));
    replace_second(&serde_json::to_vec(&mismatched).unwrap());
    assert_eq!(
        store.check().unwrap().issues[0].code,
        "EVENT_PARENT_HASH_MISMATCH"
    );
    replace_second(b"{");
    assert_eq!(store.check().unwrap().issues[0].code, "CORRUPT_EVENT");
    assert_eq!(fs::read(&second_path).unwrap(), b"{");
    replace_second(&second);

    // This is a private test workspace. Replace its test event with a future
    // format to confirm readers refuse it without editing or migrating bytes.
    let mut event: Value = serde_json::from_slice(&original).unwrap();
    event["schema_version"] = json!("inv.event.future");
    let advanced = serde_json::to_vec(&event).unwrap();
    fs::remove_file(&original_path).unwrap();
    fs::write(&original_path, &advanced).unwrap();
    let error = submit(&mut store, &create("blocked", "Do not proceed")).unwrap_err();
    assert_eq!(error.exit_code(), 6);
    assert!(error.error.message.contains("inv.event.future"));
    assert_eq!(fs::read(&original_path).unwrap(), advanced);
    assert!(!store.check().unwrap().integrity_ok());
}

#[test]
fn legacy_event_upcasts_without_rewriting_exact_bytes_before_append() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let mut store = Store::open(Workspace::init(&root, false).unwrap());
    let original = create("legacy", "Read a legacy envelope");
    let receipt = submit(&mut store, &original).unwrap();
    let id = receipt.created[0].id;
    let events = root.join("state/cases").join(id.to_string()).join("events");
    let first = events.join("000000000001.json");
    let mut legacy: Value = serde_json::from_slice(&fs::read(&first).unwrap()).unwrap();
    legacy["schema_version"] = json!("inv.event.v0-alpha");
    legacy.as_object_mut().unwrap().remove("manifest_digest");
    legacy["accepted_operations"]
        .as_object_mut()
        .unwrap()
        .remove("warnings");
    let mut canonical_legacy = serde_json::to_vec_pretty(&legacy).unwrap();
    canonical_legacy.push(b'\n');
    fs::remove_file(&first).unwrap();
    fs::write(&first, &canonical_legacy).unwrap();
    let expected_hash = investigation_store::journal::hash_bytes(&canonical_legacy);
    assert_eq!(store.case(id).unwrap().snapshot.head_hash, expected_hash);
    let retry = submit(&mut store, &original).unwrap();
    assert_eq!(retry.transaction_id, receipt.transaction_id);
    assert_eq!(retry.snapshot.head_hash, expected_hash);

    submit(&mut store, &command("append", Some(id), json!({"op":"question.create","payload":{"wording":"Can the new event follow a legacy hash?","importance":"normal"}}))).unwrap();
    let second: Value =
        serde_json::from_slice(&fs::read(events.join("000000000002.json")).unwrap()).unwrap();
    assert_eq!(second["previous_hash"], json!(expected_hash));
    assert_eq!(fs::read(first).unwrap(), canonical_legacy);
    assert!(store.check().unwrap().integrity_ok());
}
