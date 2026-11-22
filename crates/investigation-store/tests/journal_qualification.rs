use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use investigation_store::{Store, Workspace, journal::hash_bytes};
use serde_json::{Value, json};

fn submit(
    store: &mut Store,
    key: &str,
    case_id: Option<investigation_core::identity::Id>,
    operation: Value,
) -> investigation_core::wire::Receipt {
    let input = serde_json::to_vec(&json!({
        "schema_version":"inv.command.v0-alpha","command_id":key,"case_id":case_id,
        "actor":"agent:integrity-fixture","preconditions":[],"operations":[operation],
    }))
    .unwrap();
    store.record(&input).unwrap()
}

fn persisted_files(root: &Path) -> BTreeMap<PathBuf, investigation_core::wire::Digest> {
    fn collect(
        root: &Path,
        at: &Path,
        files: &mut BTreeMap<PathBuf, investigation_core::wire::Digest>,
    ) {
        for entry in fs::read_dir(at).unwrap().map(Result::unwrap) {
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                collect(root, &entry.path(), files);
            } else if kind.is_file() {
                let path = entry.path();
                files.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    hash_bytes(&fs::read(&path).unwrap()),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    collect(root, root, &mut files);
    files
}

#[test]
fn check_reports_unverified_tail_without_rewriting_entries_or_projection() {
    for kind in [
        "CORRUPT_EVENT",
        "EVENT_PARENT_HASH_MISMATCH",
        "DUPLICATE_EVENT_SEQUENCE",
    ] {
        let scratch = tempfile::tempdir().unwrap();
        let workspace = Workspace::init(scratch.path(), false).unwrap();
        let mut store = Store::open(workspace);
        let case = submit(&mut store, "create", None, json!({
            "op":"case.create","payload":{"title":"Read-only integrity fixture","mode":"record"},
        })).created[0].id;
        for key in ["question-a", "question-b"] {
            submit(
                &mut store,
                key,
                Some(case),
                json!({
                    "op":"question.create","payload":{"wording":format!("Fixture {key}?"),"importance":"normal"},
                }),
            );
        }
        let directory = scratch
            .path()
            .join("state/cases")
            .join(case.to_string())
            .join("events");
        let tail = directory.join("000000000003.json");
        let bytes = fs::read(&tail).unwrap();
        match kind {
            "CORRUPT_EVENT" => {
                fs::remove_file(&tail).unwrap();
                fs::write(&tail, &bytes[..bytes.len() / 2]).unwrap();
            }
            "EVENT_PARENT_HASH_MISMATCH" => {
                let mut event: Value = serde_json::from_slice(&bytes).unwrap();
                let mut digest = event["previous_hash"].as_str().unwrap().to_owned();
                let offset = "sha256:".len();
                let replacement = if digest.as_bytes()[offset] == b'0' {
                    "1"
                } else {
                    "0"
                };
                digest.replace_range(offset..offset + 1, replacement);
                event["previous_hash"] = json!(digest);
                fs::remove_file(&tail).unwrap();
                fs::write(&tail, serde_json::to_vec(&event).unwrap()).unwrap();
            }
            _ => fs::write(directory.join("3.json"), bytes).unwrap(),
        }
        let before = persisted_files(scratch.path());
        let report = store.check().unwrap();
        assert!(!report.integrity_ok());
        assert!(!report.structurally_valid);
        assert!(report.heads.is_empty());
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].code, kind);
        assert!(report.issues[0].repair.is_none());
        assert_eq!(persisted_files(scratch.path()), before);
    }
}
