//! Vault notes show an `external_selector` as `unavailable`, and show an
//! attributed attestation as attested, never as the core's own check.

use std::fs;

use investigation_core::identity::{Id, Kind};
use investigation_render::{Audience, VerifiedSnapshot, ViewPolicy, layout, render};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const ACTOR: &str = "agent:selector-notes-test";

fn submit(store: &mut Store, key: &str, case: Id, op: &str, payload: Value) -> Id {
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":key, "case_id":case,
        "actor":ACTOR, "preconditions":[], "operations":[{"op":op, "payload":payload}]
    });
    store
        .record(&serde_json::to_vec(&input).unwrap())
        .unwrap_or_else(|failure| panic!("{key}: {failure:?}"))
        .created[0]
        .id
}

fn note(store: &mut Store, case: Id, id: Id) -> String {
    let snapshot = VerifiedSnapshot::read(store).unwrap();
    let plan = render(&snapshot, &ViewPolicy::for_audience(Audience::Internal)).unwrap();
    let path = layout::record_note(case, id).unwrap().unwrap().to_string();
    String::from_utf8(plan.entry(&path).unwrap().content().to_vec()).unwrap()
}

#[test]
fn an_external_selector_reads_unavailable_until_an_attestation_is_labelled_attested() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    Workspace::init(&root, true).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        contents.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    fs::write(root.join("capture.bin"), [0u8, 1, 2, 0xff]).unwrap();
    let mut store = Store::open(Workspace::open(&root).unwrap());
    let manifest = json!({
        "schema_version":"inv.ingest.v1", "command_id":"capture", "actor":ACTOR,
        "new_case":{"title":"Which frame dropped?"}, "source":{"title":"Capture"},
        "paths":[root.join("capture.bin")],
    });
    let receipt = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap_or_else(|failure| panic!("{failure:?}"));
    let case = receipt
        .receipt
        .created
        .iter()
        .find(|created| created.id.kind() == Kind::Case)
        .unwrap()
        .id;
    let item = &receipt.items[0];
    let selector = json!({
        "schema_version":"inv.selector.v1", "artifact_digest":item.digest,
        "selector":{"type":"external_selector", "namespace":"example.frames",
            "schema_uri":"urn:example:frame-index", "schema_version":"1",
            "value":{"frame":3}, "validator":null}
    });
    let observation = submit(
        &mut store,
        "observe",
        case,
        "observation.add",
        json!({
            "value":{"kind":"measurement", "value":"3", "unit":"frames"},
            "basis":{"kind":"source", "artifact_ref":{"id":item.artifact_id, "revision":1},
                "selector":selector, "text_quote":null},
            "validity":"valid", "quality":[], "limitations":[],
            "evidence_kind":"observational", "variables_changed":[]
        }),
    );
    let before = note(&mut store, case, observation);
    assert!(
        before.contains("- Selector: `external_selector`, validation `unavailable`"),
        "{before}"
    );
    assert!(before.contains("It is not verified."), "{before}");
    assert!(!before.contains("Attested selector state"), "{before}");

    let annotation = submit(
        &mut store,
        "attest",
        case,
        "selector.attest",
        json!({
            "observation_ref":{"id":observation, "revision":1}, "selector":selector,
            "outcome":"verified", "validator":{"name":"frame-reader `x`", "version":"2"},
            "rationale":"Decoded frame 3."
        }),
    );
    let after = note(&mut store, case, observation);
    // The recorded validation is unchanged and the attestation is labelled.
    assert!(
        after.contains("- Selector: `external_selector`, validation `unavailable`"),
        "{after}"
    );
    assert!(
        after.contains("- Attested selector state: `verified`, not checked by the core"),
        "{after}"
    );
    assert!(after.contains("direct support still needs"), "{after}");
    let attestation = note(&mut store, case, annotation);
    assert!(
        attestation.contains("not a check by the core"),
        "{attestation}"
    );
}
