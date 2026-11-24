//! `Store::receipts` answers each command as `Store::receipt` does, from one
//! verified replay: found and missing commands, repeats, an empty request,
//! a current and a degraded index, and a journal that fails verification,
//! which both refuse with the same error.

use std::fs;

use investigation_core::{header::Actor, wire::ProjectionStatus};
use investigation_store::{Store, Workspace, store::ReceiptLookup};
use serde_json::json;

const ACTOR: &str = "agent:receipt-lookup";

fn create(store: &mut Store, key: &str) {
    let request = json!({
        "schema_version": "inv.command.v1", "command_id": key, "case_id": null,
        "actor": ACTOR, "preconditions": [],
        "operations": [{"op": "case.create", "payload": {"title": key, "mode": "record"}}]
    });
    store
        .record(&serde_json::to_vec(&request).unwrap())
        .unwrap();
}

/// Each lookup one at a time and all of them at once, as comparable text.
fn both(store: &mut Store, commands: &[(&Actor, &str)]) -> (Vec<String>, Vec<String>) {
    let single = commands
        .iter()
        .map(|(actor, key)| format!("{:?}", store.receipt(actor, key).unwrap()))
        .collect();
    let bulk = store
        .receipts(commands)
        .unwrap()
        .iter()
        .map(|lookup| format!("{lookup:?}"))
        .collect();
    (single, bulk)
}

fn index(lookup: &ReceiptLookup) -> ProjectionStatus {
    match lookup {
        ReceiptLookup::Found { receipt } => receipt.projection.index,
        ReceiptLookup::NotFoundAtSnapshot { .. } => panic!("not found"),
    }
}

#[test]
fn a_bulk_lookup_answers_each_command_as_a_single_lookup_does() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let mut store = Store::open(Workspace::init(&root, false).unwrap());
    create(&mut store, "first");
    create(&mut store, "second");
    let actor: Actor = ACTOR.parse().unwrap();
    let other: Actor = "agent:someone-else".parse().unwrap();
    let commands = [
        (&actor, "first"),
        (&actor, "missing"),
        (&other, "first"),
        (&actor, "second"),
        (&actor, "first"),
    ];

    let (single, bulk) = both(&mut store, &commands);
    assert_eq!(bulk, single);
    let found = store.receipts(&commands).unwrap();
    assert!(matches!(found[1], ReceiptLookup::NotFoundAtSnapshot { .. }));
    assert!(matches!(found[2], ReceiptLookup::NotFoundAtSnapshot { .. }));
    assert_eq!(index(&found[0]), ProjectionStatus::Current);
    assert!(store.receipts(&[]).unwrap().is_empty());

    // Without the disposable index every found receipt says so, in both.
    fs::remove_file(root.join("cache/index.sqlite")).unwrap();
    let (single, bulk) = both(&mut store, &commands);
    assert_eq!(bulk, single);
    let found = store.receipts(&commands).unwrap();
    assert_eq!(index(&found[0]), ProjectionStatus::Degraded);
    assert_eq!(index(&found[3]), ProjectionStatus::Degraded);

    // A journal that fails verification refuses both, with the same error.
    let case = fs::read_dir(root.join("state/cases"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let event = case.join("events/000000000001.json");
    let mut permissions = fs::metadata(&event).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&event, permissions).unwrap();
    let bytes = fs::read(&event).unwrap();
    fs::write(&event, &bytes[..bytes.len() / 2]).unwrap();
    let single = store.receipt(&actor, "first").unwrap_err();
    let bulk = store.receipts(&commands).unwrap_err();
    assert_eq!(bulk.error.code, single.error.code);
    assert_eq!(bulk.error.message, single.error.message);
}
