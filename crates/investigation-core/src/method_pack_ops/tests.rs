use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::{
    catalog::{Catalog, standard_catalog},
    command::Command,
    header::{Clock, UtcTimestamp},
    identity::IdSource,
    state::{Acceptance, AcceptedTransaction, Record, validate_and_apply},
    wire::RecordChange,
};

const PACK_V1: &str = r#"schema_version = "inv.method-pack.v1"
id = "field-notes"
version = "1.0.0"
title = "Field notes"
summary = "Optional note-taking guidance."
modes = ["explore"]
"#;

#[derive(Default)]
struct Ids(u64);

impl IdSource for Ids {
    fn next_uuid(&mut self) -> Uuid {
        self.0 += 1;
        format!("018d87fc-5234-7000-8000-{:012x}", self.0)
            .parse()
            .unwrap()
    }
}

struct FixedTime;

impl Clock for FixedTime {
    fn now_utc(&mut self) -> UtcTimestamp {
        "2026-11-22T08:12:44.000Z".parse().unwrap()
    }
}

fn digest(bytes: &[u8]) -> Digest {
    format!("sha256:{:x}", Sha256::digest(bytes))
        .parse()
        .unwrap()
}

fn v2() -> String {
    PACK_V1.replace("1.0.0", "1.1.0")
}

struct Fixture {
    catalog: Catalog,
    ids: Ids,
    state: State,
    case_id: Id,
    commands: u32,
}

impl Fixture {
    fn new() -> Self {
        let mut fixture = Self {
            catalog: standard_catalog(),
            ids: Ids::default(),
            state: State::default(),
            case_id: "case_018d87fc-5234-7000-8000-000000000001".parse().unwrap(),
            commands: 0,
        };
        let accepted = fixture
            .submit(
                None,
                json!([{"op":"case.create","payload":{"title":"Pinned guidance","mode":"explore"}}]),
            )
            .unwrap();
        fixture.case_id = accepted.case_id;
        fixture
    }

    fn case_revision(&self) -> u64 {
        self.state.current(self.case_id).unwrap().header.revision.0
    }

    fn submit(
        &mut self,
        case_id: Option<Id>,
        operations: Value,
    ) -> Result<AcceptedTransaction, Failure> {
        self.commands += 1;
        let preconditions = case_id
            .map(|id| vec![json!({"id": id, "revision": self.case_revision()})])
            .unwrap_or_default();
        let command: Command = serde_json::from_value(json!({
            "schema_version": "inv.command.v1",
            "command_id": format!("pack-test-{}", self.commands),
            "actor": "agent:pack-test",
            "case_id": case_id,
            "operations": operations,
            "preconditions": preconditions,
        }))
        .unwrap();
        let accepted = validate_and_apply(
            &self.state,
            &command,
            &self.catalog,
            Acceptance {
                sequence: self.state.sequence() + 1,
                clock: &mut FixedTime,
                ids: &mut self.ids,
            },
        )?;
        self.state.apply(&accepted).unwrap();
        Ok(accepted)
    }

    /// Mimic a retained ingest artifact for the given bytes.
    fn artifact(&mut self, bytes: &[u8]) -> RevisionRef {
        let case = self.state.current(self.case_id).unwrap();
        let sequence = self.state.sequence() + 1;
        let mut header = case.header.clone();
        header.acceptance_sequence = sequence;
        header.revision = crate::header::Revision(1);
        header.history.clear();
        let mut records = Vec::new();
        let mut created = Vec::new();
        let source = self.ids.next_id(Kind::Source).unwrap();
        let artifact = self.ids.next_id(Kind::Artifact).unwrap();
        for (id, body) in [
            (
                source,
                json!({
                    "producer": null, "publisher": null, "identity": null,
                    "acquisition": {"channel":"local_file","acquired_at":null,"method":null,"context":null,"environment":null},
                    "access":"local_only","verification_limits":[]
                }),
            ),
            (
                artifact,
                json!({
                    "digest": digest(bytes), "size_bytes": bytes.len(), "format": null,
                    "source": {"id": source, "revision": 1}, "transforms": [],
                    "availability": "retained", "integrity": "verified", "sensitivity": "internal"
                }),
            ),
        ] {
            records.push(Record {
                header: crate::header::RecordHeader {
                    id,
                    ..header.clone()
                },
                body,
            });
            created.push(RecordChange {
                id,
                revision: 1,
                client_ref: None,
            });
        }
        self.state
            .apply(&AcceptedTransaction {
                case_id: self.case_id,
                sequence,
                records,
                created,
                updated: Vec::new(),
                warnings: Vec::new(),
            })
            .unwrap();
        RevisionRef {
            id: artifact,
            revision: 1,
        }
    }

    fn adopt(&mut self, payload: Value) -> Result<AcceptedTransaction, Failure> {
        self.submit(
            Some(self.case_id),
            json!([{"op": ADOPT, "payload": payload}]),
        )
    }
}

fn payload(artifact: RevisionRef, bytes: &[u8], version: &str) -> Value {
    json!({
        "artifact_ref": artifact,
        "pack_id": "field-notes",
        "version": version,
        "digest": digest(bytes),
        "reason": "Use shared field-note guidance.",
    })
}

#[test]
fn adoption_pins_digest_and_reason_on_a_new_case_revision() {
    let mut fixture = Fixture::new();
    let artifact = fixture.artifact(PACK_V1.as_bytes());
    let accepted = fixture
        .adopt(payload(artifact, PACK_V1.as_bytes(), "1.0.0"))
        .unwrap();
    assert_eq!(accepted.updated[0].id, fixture.case_id);
    let case = &accepted.records[0];
    assert_eq!(json!(case.header.recorded_by), json!("agent:pack-test"));
    let pins = adopted_packs(&fixture.state, fixture.case_id, None).unwrap();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].digest, digest(PACK_V1.as_bytes()));
    assert_eq!(pins[0].reason, "Use shared field-note guidance.");
    assert_eq!(pins[0].artifact_ref, artifact);
    assert_eq!(pins[0].adopted_at_sequence, accepted.sequence);
    assert_eq!(pins[0].replaces, None);
    // The case body still decodes for ordinary case operations.
    fixture
        .submit(
            Some(fixture.case_id),
            json!([{"op":"case.amend","payload":{"reason":"Narrow the title","title":"Pinned"}}]),
        )
        .unwrap();
    assert_eq!(
        adopted_packs(&fixture.state, fixture.case_id, None).unwrap(),
        pins
    );
}

#[test]
fn a_newer_revision_needs_an_explicit_replacement_and_keeps_history() {
    let mut fixture = Fixture::new();
    let first = fixture.artifact(PACK_V1.as_bytes());
    let adopted = fixture
        .adopt(payload(first, PACK_V1.as_bytes(), "1.0.0"))
        .unwrap()
        .sequence;
    let newer = v2();
    let second = fixture.artifact(newer.as_bytes());

    let silent = fixture
        .adopt(payload(second, newer.as_bytes(), "1.1.0"))
        .unwrap_err();
    assert_eq!(silent.error.code, ErrorCode::GateFailed);
    assert_eq!(
        silent.error.path.as_deref(),
        Some("/operations/0/payload/replaces")
    );
    assert_eq!(
        silent.error.expected,
        Some(json!(digest(PACK_V1.as_bytes())))
    );

    let mut wrong = payload(second, newer.as_bytes(), "1.1.0");
    wrong["replaces"] = json!(digest(b"something else"));
    assert_eq!(
        fixture.adopt(wrong).unwrap_err().error.code,
        ErrorCode::GateFailed
    );

    let same = fixture
        .adopt(payload(first, PACK_V1.as_bytes(), "1.0.0"))
        .unwrap_err();
    assert_eq!(same.error.code, ErrorCode::GateFailed);

    let mut explicit = payload(second, newer.as_bytes(), "1.1.0");
    explicit["replaces"] = json!(digest(PACK_V1.as_bytes()));
    explicit["reason"] = json!("Adopt the clarified revision.");
    let upgraded = fixture.adopt(explicit).unwrap().sequence;

    let current = adopted_packs(&fixture.state, fixture.case_id, None).unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].version, "1.1.0");
    assert_eq!(current[0].reason, "Adopt the clarified revision.");
    assert_eq!(
        current[0].replaces,
        Some(PinnedRevision {
            version: "1.0.0".into(),
            digest: digest(PACK_V1.as_bytes())
        })
    );
    // Work recorded before the upgrade resolves the policy it was made under.
    for sequence in adopted..upgraded {
        let earlier = adopted_packs(&fixture.state, fixture.case_id, Some(sequence)).unwrap();
        assert_eq!(earlier[0].digest, digest(PACK_V1.as_bytes()), "{sequence}");
    }
    assert!(
        adopted_packs(&fixture.state, fixture.case_id, Some(adopted - 1))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn adoption_rejects_unbound_or_mismatched_artifacts_and_the_builtin_id() {
    let mut fixture = Fixture::new();
    let artifact = fixture.artifact(PACK_V1.as_bytes());

    let mut other = payload(artifact, b"other bytes", "1.0.0");
    assert_eq!(
        fixture
            .adopt(other.clone())
            .unwrap_err()
            .error
            .path
            .as_deref(),
        Some("/operations/0/payload/digest")
    );
    other["artifact_ref"] = json!({"id": fixture.case_id, "revision": 1});
    other["digest"] = json!(digest(PACK_V1.as_bytes()));
    assert_eq!(
        fixture.adopt(other).unwrap_err().error.code,
        ErrorCode::UnknownReference
    );

    let mut builtin = payload(artifact, PACK_V1.as_bytes(), "1.0.0");
    builtin["pack_id"] = json!("general");
    assert_eq!(
        fixture.adopt(builtin).unwrap_err().error.path.as_deref(),
        Some("/operations/0/payload/pack_id")
    );

    let mut premature = payload(artifact, PACK_V1.as_bytes(), "1.0.0");
    premature["replaces"] = json!(digest(b"earlier"));
    assert_eq!(
        fixture.adopt(premature).unwrap_err().error.code,
        ErrorCode::InvalidInput
    );

    // The case revision is a required precondition, like other case revisions.
    let command = fixture.submit(
        Some(fixture.case_id),
        json!([{"op": ADOPT, "payload": payload(artifact, PACK_V1.as_bytes(), "1.0.0")},
               {"op": ADOPT, "payload": payload(artifact, PACK_V1.as_bytes(), "1.0.0")}]),
    );
    assert!(command.is_err());
    assert!(
        adopted_packs(&fixture.state, fixture.case_id, None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn closed_cases_cannot_adopt() {
    let mut fixture = Fixture::new();
    let artifact = fixture.artifact(PACK_V1.as_bytes());
    fixture
        .submit(
            Some(fixture.case_id),
            json!([{"op":"case.close","payload":{
                "outcome":"abandoned","reason":"Out of scope.","unresolved_questions":[],
                "retrospective_waived":"No material work occurred."}}]),
        )
        .unwrap();
    assert_eq!(
        fixture
            .adopt(payload(artifact, PACK_V1.as_bytes(), "1.0.0"))
            .unwrap_err()
            .error
            .code,
        ErrorCode::GateFailed
    );
}

#[test]
fn resolution_uses_only_pinned_bytes_and_reports_unavailable_packs() {
    let pin = MethodPackPin {
        pack_id: "field-notes".into(),
        version: "1.0.0".into(),
        digest: digest(PACK_V1.as_bytes()),
        artifact_ref: RevisionRef {
            id: "art_018d87fc-5234-7000-8000-000000000009".parse().unwrap(),
            revision: 1,
        },
        reason: "r".into(),
        adopted_at_sequence: 2,
        replaces: None,
    };
    let resolved = resolve(std::slice::from_ref(&pin), |_| {
        Some(PACK_V1.as_bytes().to_vec())
    });
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].pack_id, BUILTIN_PACK_ID);
    assert_eq!(resolved[0].origin, PackOrigin::BuiltIn);
    assert!(resolved[0].is_available());
    assert!(resolved[1].is_available());
    assert_eq!(
        resolved[1].loaded.as_ref().unwrap().pack().title,
        "Field notes"
    );

    let edited = PACK_V1.replace("Field notes", "Edited on disk");
    let cases = [
        (None, UnavailableReason::BytesUnavailable),
        (Some(edited.as_bytes()), UnavailableReason::DigestMismatch),
    ];
    for (bytes, reason) in cases {
        let resolved = resolve_pin(&pin, bytes);
        assert_eq!(resolved.status, PackStatus::Unavailable);
        assert_eq!(resolved.unavailable_reason, Some(reason));
        assert!(resolved.loaded.is_none());
    }

    let future = PACK_V1.replace("inv.method-pack.v1", "inv.method-pack.v9");
    let unknown = MethodPackPin {
        digest: digest(future.as_bytes()),
        ..pin.clone()
    };
    assert_eq!(
        resolve_pin(&unknown, Some(future.as_bytes())).unavailable_reason,
        Some(UnavailableReason::InvalidPack)
    );

    let renamed = PACK_V1.replace("1.0.0", "2.0.0");
    let mislabeled = MethodPackPin {
        digest: digest(renamed.as_bytes()),
        ..pin
    };
    assert_eq!(
        resolve_pin(&mislabeled, Some(renamed.as_bytes())).unavailable_reason,
        Some(UnavailableReason::IdentityMismatch)
    );
}
