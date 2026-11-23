//! Per-case method pack adoption: digest pins, explicit revision changes and
//! resolution from retained bytes only.

use std::{fs, path::Path};

use investigation_core::{
    identity::{Id, Kind},
    method_pack_ops::{BUILTIN_PACK_ID, PackOrigin, ResolvedPack, UnavailableReason},
    wire::{ErrorCode, Failure, Receipt},
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const PACK: &str = r#"schema_version = "inv.method-pack.v1"
id = "field-notes"
version = "1.0.0"
title = "Field notes"
summary = "Optional note-taking guidance."
modes = ["explore"]

[[cards]]
name = "frame"
title = "Frame the question"
stage = "frame"
summary = "The question is still vague."
operations = ["question.create"]

[cards.body]
when = "Use when the question is not yet answerable."
record = "Record one answerable question."
skip = "Skip when the case already has one."
"#;

struct Harness {
    root: std::path::PathBuf,
    store: Store,
    case_id: Id,
    commands: u32,
    _temporary: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        Workspace::init(&root, false).unwrap();
        let config = root.join("investigation.toml");
        let contents = fs::read_to_string(&config).unwrap();
        fs::write(
            &config,
            contents.replace("import_roots = []", "import_roots = [\".\"]"),
        )
        .unwrap();
        let store = Store::open(Workspace::open(&root).unwrap());
        let mut harness = Self {
            root,
            store,
            case_id: "case_018d87fc-5234-7000-8000-000000000001".parse().unwrap(),
            commands: 0,
            _temporary: temporary,
        };
        let receipt = harness
            .record(
                None,
                json!({"op":"case.create","payload":{"title":"Pinned guidance","mode":"explore"}}),
            )
            .unwrap();
        harness.case_id = receipt.created[0].id;
        harness
    }

    fn case_revision(&mut self) -> u64 {
        let replay = self.store.case(self.case_id).unwrap();
        replay
            .state
            .current(self.case_id)
            .unwrap()
            .header
            .revision
            .0
    }

    fn record(&mut self, case_id: Option<Id>, operation: Value) -> Result<Receipt, Failure> {
        self.commands += 1;
        let preconditions = match case_id {
            Some(id) => vec![json!({"id": id, "revision": self.case_revision()})],
            None => Vec::new(),
        };
        let command = json!({
            "schema_version":"inv.command.v1",
            "command_id": format!("pack-{}", self.commands),
            "actor":"agent:pack-adopter",
            "case_id": case_id,
            "preconditions": preconditions,
            "operations":[operation],
        });
        self.store.record(&serde_json::to_vec(&command).unwrap())
    }

    /// Retain a file through the standard ingest path; return (artifact, digest).
    fn retain(&mut self, name: &str, contents: &str) -> (Id, String) {
        self.commands += 1;
        let path = self.root.join(name);
        fs::write(&path, contents).unwrap();
        let manifest = json!({
            "schema_version":"inv.ingest.v1",
            "command_id": format!("retain-{}", self.commands),
            "actor":"agent:pack-adopter",
            "case_id": self.case_id,
            "source":{"title": name, "format":"application/toml"},
            "paths":[path],
        });
        let receipt = self
            .store
            .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
            .unwrap();
        let item = &receipt.items[0];
        (item.artifact_id, item.digest.to_string())
    }

    fn adopt(
        &mut self,
        artifact: Id,
        digest: &str,
        version: &str,
        replaces: Option<&str>,
    ) -> Result<Receipt, Failure> {
        let mut payload = json!({
            "artifact_ref": {"id": artifact, "revision": 1},
            "pack_id": "field-notes",
            "version": version,
            "digest": digest,
            "reason": format!("Adopt field notes {version} for this case."),
        });
        if let Some(replaces) = replaces {
            payload["replaces"] = json!(replaces);
        }
        self.record(
            Some(self.case_id),
            json!({"op":"method_pack.adopt","payload":payload}),
        )
    }

    fn resolve(&mut self, at_sequence: Option<u64>) -> Vec<ResolvedPack> {
        let replay = self.store.case(self.case_id).unwrap();
        self.store
            .method_packs(&replay.state, self.case_id, at_sequence)
            .unwrap()
    }

    fn blob(&self, digest: &str) -> std::path::PathBuf {
        let hex = digest.strip_prefix("sha256:").unwrap();
        self.root
            .join("state/blobs/sha256")
            .join(&hex[..2])
            .join(hex)
    }
}

fn title(pack: &ResolvedPack) -> &str {
    &pack.loaded.as_ref().unwrap().pack().title
}

#[test]
fn editing_the_pack_file_on_disk_does_not_change_an_adopted_case() {
    let mut harness = Harness::new();
    let (artifact, digest) = harness.retain("field-notes.toml", PACK);
    let adopted = harness.adopt(artifact, &digest, "1.0.0", None).unwrap();

    let before = harness.resolve(None);
    assert_eq!(before.len(), 2);
    assert_eq!(before[0].pack_id, BUILTIN_PACK_ID);
    assert_eq!(before[0].origin, PackOrigin::BuiltIn);
    assert!(before[1].is_available());
    assert_eq!(before[1].digest.as_str(), digest);
    assert_eq!(title(&before[1]), "Field notes");

    // Rewrite the original file in place, same id and version.
    let edited = PACK.replace("Field notes", "Rewritten on disk");
    fs::write(harness.root.join("field-notes.toml"), &edited).unwrap();
    let after = harness.resolve(None);
    assert!(after[1].is_available());
    assert_eq!(after[1].digest.as_str(), digest);
    assert_eq!(title(&after[1]), "Field notes");

    // The adoption is a journaled, attributed event with digest and reason.
    let replay = harness.store.case(harness.case_id).unwrap();
    let event = &replay
        .events
        .iter()
        .find(|event| event.event.sequence == adopted.snapshot.sequence)
        .unwrap()
        .event;
    assert_eq!(json!(event.actor), json!("agent:pack-adopter"));
    let payload = &event.request.input["operations"][0]["payload"];
    assert_eq!(payload["digest"], json!(digest));
    assert_eq!(
        payload["reason"],
        json!("Adopt field notes 1.0.0 for this case.")
    );
    let case = &event.accepted.records[0];
    assert_eq!(case.header.id, harness.case_id);
    assert_eq!(json!(case.header.recorded_by), json!("agent:pack-adopter"));
    let pin = &case.body["adopted_method_packs"][0];
    assert_eq!(pin["digest"], json!(digest));
    assert_eq!(
        pin["reason"],
        json!("Adopt field notes 1.0.0 for this case.")
    );
    assert_eq!(pin["artifact_ref"]["id"], json!(artifact));

    // Retaining and adopting the edited bytes is a separate explicit step.
    let (newer, newer_digest) = harness.retain("field-notes.toml", &edited);
    let silent = harness
        .adopt(newer, &newer_digest, "1.0.0", None)
        .unwrap_err();
    assert_eq!(silent.error.code, ErrorCode::GateFailed);
    let upgraded = harness
        .adopt(newer, &newer_digest, "1.0.0", Some(&digest))
        .unwrap();
    assert_eq!(title(&harness.resolve(None)[1]), "Rewritten on disk");
    let earlier = harness.resolve(Some(upgraded.snapshot.sequence - 1));
    assert_eq!(earlier[1].digest.as_str(), digest);
    assert_eq!(title(&earlier[1]), "Field notes");
    assert!(
        harness.resolve(Some(adopted.snapshot.sequence - 1))[1..].is_empty(),
        "no pack applied before adoption"
    );
    assert!(harness.store.check().unwrap().integrity_ok());
}

#[test]
fn adoption_requires_loadable_bytes_that_match_the_declaration() {
    let mut harness = Harness::new();
    let (notes, notes_digest) = harness.retain("notes.txt", "Plain notes, not a pack.\n");
    let failure = harness
        .adopt(notes, &notes_digest, "1.0.0", None)
        .unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);
    assert!(failure.error.actual.unwrap()["pack_error"].is_string());

    let (pack, digest) = harness.retain("field-notes.toml", PACK);
    let failure = harness.adopt(pack, &digest, "9.9.9", None).unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::InvalidInput);
    assert_eq!(failure.error.actual.unwrap()["version"], json!("1.0.0"));

    let hook = PACK.replace(
        "modes = [\"explore\"]",
        "modes = [\"explore\"]\ncommand = \"run\"",
    );
    let (hooked, hooked_digest) = harness.retain("hooked.toml", &hook);
    assert_eq!(
        harness
            .adopt(hooked, &hooked_digest, "1.0.0", None)
            .unwrap_err()
            .error
            .code,
        ErrorCode::InvalidInput
    );

    // None of the rejected adoptions changed the case.
    assert_eq!(harness.resolve(None).len(), 1);
    assert_eq!(harness.case_revision(), 1);
}

#[test]
fn a_pinned_pack_without_usable_bytes_does_not_block_recording_or_reading() {
    let mut harness = Harness::new();
    let (artifact, digest) = harness.retain("field-notes.toml", PACK);
    harness.adopt(artifact, &digest, "1.0.0", None).unwrap();
    fs::remove_file(harness.blob(&digest)).unwrap();

    let resolved = harness.resolve(None);
    assert!(resolved[0].is_available());
    assert!(!resolved[1].is_available());
    assert_eq!(
        resolved[1].unavailable_reason,
        Some(UnavailableReason::BytesUnavailable)
    );
    let shown = serde_json::to_value(&resolved[1]).unwrap();
    assert_eq!(shown["status"], json!("unavailable"));
    assert_eq!(shown["pin"]["digest"], json!(digest));

    // Generic recording and reading continue.
    let question = harness
        .record(
            Some(harness.case_id),
            json!({"op":"question.create","payload":{"wording":"What changed first?","importance":"normal"}}),
        )
        .unwrap();
    let replay = harness.store.case(harness.case_id).unwrap();
    assert!(replay.state.current(question.created[0].id).is_some());
    assert!(
        replay
            .state
            .records()
            .any(|record| record.header.id.kind() == Kind::Artifact)
    );
    // Integrity checks still report the missing blob; the pin does not hide it.
    let check = harness.store.check().unwrap();
    assert!(
        check
            .issues
            .iter()
            .any(|issue| issue.code == "BLOB_MISSING")
    );

    // Corrupted bytes at the pinned digest are reported the same way.
    fs::write(harness.blob(&digest), PACK.replace("1.0.0", "1.0.1")).unwrap();
    assert_eq!(
        harness.resolve(None)[1].unavailable_reason,
        Some(UnavailableReason::DigestMismatch)
    );
    assert!(Path::new(&harness.blob(&digest)).exists());
}
