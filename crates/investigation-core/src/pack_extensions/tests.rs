use std::collections::BTreeMap;

use serde_json::json;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use super::*;
use crate::{
    basis::basis_digest,
    blindspot::{AuditRequest, audit},
    catalog::{Catalog, standard_catalog},
    command::{Command, RevisionRef},
    freshness::FreshnessIndex,
    gap_rules::enumerate_gaps,
    header::{Clock, Revision, UtcTimestamp},
    identity::IdSource,
    method_pack,
    next_rank::rank_next,
    review_ops::required_read_set,
    state::{Acceptance, AcceptedTransaction, validate_and_apply_with_packs},
    wire::{Digest, RecordChange, Snapshot},
};

const PACK: &str = include_str!("../../../../fixtures/method-packs/bench-notes.toml");
const PACK_ID: &str = "bench-notes";

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

/// Retained pack bytes by digest, as the store supplies them.
#[derive(Default)]
struct Packs(BTreeMap<String, Vec<u8>>);

impl PackSource for Packs {
    fn pack_bytes(&self, digest: &Digest) -> Option<Vec<u8>> {
        self.0.get(digest.as_str()).cloned()
    }
}

fn digest(bytes: &[u8]) -> Digest {
    format!("sha256:{:x}", Sha256::digest(bytes))
        .parse()
        .unwrap()
}

struct Fixture {
    catalog: Catalog,
    ids: Ids,
    state: State,
    case_id: Id,
    commands: u32,
    packs: Packs,
    /// The outcome of a promotion gate run by [`twin`].
    gate: Option<Result<Value, Value>>,
}

impl Fixture {
    fn new() -> Self {
        let mut fixture = Self {
            catalog: standard_catalog(),
            ids: Ids::default(),
            state: State::default(),
            case_id: "case_018d87fc-5234-7000-8000-000000000001".parse().unwrap(),
            commands: 0,
            packs: Packs::default(),
            gate: None,
        };
        let accepted = fixture
            .command(
                None,
                vec![],
                json!([{"op":"case.create","payload":{"title":"Bench","mode":"measure"}}]),
            )
            .unwrap();
        fixture.case_id = accepted.case_id;
        fixture
    }

    /// A case that pins the bench-notes pack from retained bytes.
    fn adopted() -> Self {
        let mut fixture = Self::new();
        let artifact = fixture.artifact(PACK.as_bytes());
        fixture
            .packs
            .0
            .insert(digest(PACK.as_bytes()).as_str().to_owned(), PACK.into());
        fixture
            .submit(json!([{"op":"method_pack.adopt","payload":{
                "artifact_ref": artifact, "pack_id": PACK_ID, "version": "1.0.0",
                "digest": digest(PACK.as_bytes()), "reason": "Use bench bookkeeping."
            }}]))
            .unwrap();
        fixture
    }

    fn case_revision(&self) -> u64 {
        self.state.current(self.case_id).unwrap().header.revision.0
    }

    fn command(
        &mut self,
        case_id: Option<Id>,
        preconditions: Vec<RevisionRef>,
        operations: Value,
    ) -> Result<AcceptedTransaction, Failure> {
        self.commands += 1;
        let command: Command = serde_json::from_value(json!({
            "schema_version": "inv.command.v1",
            "command_id": format!("extension-test-{}", self.commands),
            "actor": "agent:extension-test",
            "case_id": case_id,
            "operations": operations,
            "preconditions": preconditions,
        }))
        .unwrap();
        let accepted = validate_and_apply_with_packs(
            &self.state,
            &command,
            &self.catalog,
            Acceptance {
                sequence: self.state.sequence() + 1,
                clock: &mut FixedTime,
                ids: &mut self.ids,
            },
            &BTreeMap::new(),
            &self.packs,
        )?;
        self.state.apply(&accepted).unwrap();
        Ok(accepted)
    }

    fn submit(&mut self, operations: Value) -> Result<AcceptedTransaction, Failure> {
        let case = RevisionRef {
            id: self.case_id,
            revision: self.case_revision(),
        };
        self.command(Some(self.case_id), vec![case], operations)
    }

    /// Submit one operation and return the revision it created or revised.
    fn one(&mut self, op: &str, payload: Value, extensions: Option<Value>) -> RevisionRef {
        let mut operation = json!({"op": op, "payload": payload});
        if let Some(extensions) = extensions {
            operation["extensions"] = extensions;
        }
        let accepted = self.submit(json!([operation])).unwrap();
        let change = accepted
            .created
            .first()
            .or_else(|| accepted.updated.first())
            .unwrap();
        RevisionRef {
            id: change.id,
            revision: change.revision,
        }
    }

    fn try_one(&mut self, op: &str, payload: Value, extensions: Value) -> Failure {
        self.submit(json!([{"op": op, "payload": payload, "extensions": extensions}]))
            .unwrap_err()
    }

    fn question(&mut self) -> RevisionRef {
        self.one(
            "question.create",
            json!({"importance":"normal","wording":"Does the warm-up change the reading?"}),
            None,
        )
    }

    fn hypothesis(&mut self, question: RevisionRef, extensions: Option<Value>) -> RevisionRef {
        self.one("hypothesis.create", hypothesis_body(question), extensions)
    }

    /// Mimic a retained ingest artifact for the given bytes.
    fn artifact(&mut self, bytes: &[u8]) -> RevisionRef {
        let case = self.state.current(self.case_id).unwrap();
        let sequence = self.state.sequence() + 1;
        let mut header = case.header.clone();
        header.acceptance_sequence = sequence;
        header.revision = Revision(1);
        header.history.clear();
        let source = self.ids.next_id(Kind::Source).unwrap();
        let artifact = self.ids.next_id(Kind::Artifact).unwrap();
        let mut records = Vec::new();
        let mut created = Vec::new();
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

    /// Mimic an accepted, valid measured Observation of `artifact`, as an
    /// ingest pipeline with a verified selector would record it, carrying
    /// `extensions` on its header.
    fn observation(&mut self, artifact: RevisionRef, extensions: Option<Value>) -> RevisionRef {
        let mut header = self.state.current(self.case_id).unwrap().header.clone();
        header.acceptance_sequence = self.state.sequence() + 1;
        header.revision = Revision(1);
        header.history.clear();
        header.id = self.ids.next_id(Kind::Observation).unwrap();
        header.extensions = extensions
            .map(|value| serde_json::from_value(value).unwrap())
            .unwrap_or_default();
        let hash = self.state.revision(artifact).unwrap().body["digest"].clone();
        let record = Record {
            header,
            body: json!({
                "value":{"kind":"measurement","value":"12.5","unit":"ms"},
                "basis":{"kind":"source","artifact_ref":artifact,
                    "selector":{"schema_version":"inv.selector.v1","artifact_digest":hash,"selector":{"type":"whole"},"validation":"verified"},
                    "text_quote":{"state":"not_applicable"}},
                "extraction_method":"Median of 30 timed runs",
                "validity":"valid","quality":[],"limitations":[],
                "evidence_kind":"observational","variables_changed":[],
                "status":"active","run_ref":null,
            }),
        };
        let reference = record.reference();
        self.state
            .apply(&AcceptedTransaction {
                case_id: self.case_id,
                sequence: record.header.acceptance_sequence,
                created: vec![RecordChange {
                    id: reference.id,
                    revision: 1,
                    client_ref: None,
                }],
                records: vec![record],
                updated: Vec::new(),
                warnings: Vec::new(),
            })
            .unwrap();
        reference
    }
}

fn hypothesis_body(question: RevisionRef) -> Value {
    json!({
        "question_ref": question, "statement": "Warm-up runs inflate the first reading.",
        "possible_falsifiers": ["Discarding no runs gives the same reading."],
        "rival_refs": [], "assumption_refs": []
    })
}

fn phases(values: Value) -> Value {
    json!({PACK_ID: {"phases": values}})
}

#[test]
fn the_fixture_pack_adds_a_card_and_properties_without_core_changes() {
    let loaded = method_pack::load(PACK.as_bytes()).unwrap();
    assert_eq!(loaded.id(), PACK_ID);
    assert_eq!(loaded.pack().cards.len(), 1);
    let names: Vec<&str> = loaded
        .pack()
        .properties
        .iter()
        .map(|property| property.name.as_str())
        .collect();
    assert_eq!(names, ["warmup_runs", "machine", "phases", "edition"]);
}

#[test]
fn a_declared_value_is_stored_on_the_record_header_under_the_pack_id() {
    let mut fixture = Fixture::adopted();
    let question = fixture.question();
    let hypothesis = fixture.hypothesis(question, Some(phases(json!(["warm", "steady"]))));
    let record = fixture.state.revision(hypothesis).unwrap().clone();
    assert_eq!(
        json!(record.header.extensions),
        json!({PACK_ID: {"phases": ["warm", "steady"]}})
    );
    // The body is untouched: extensions are never record payload fields.
    assert!(record.body.get("extensions").is_none());
    assert!(record.body.get("phases").is_none());

    // A later revision by an operation without extensions carries them
    // forward; an operation with extensions replaces that pack's object.
    let amended = fixture.one(
        "hypothesis.amend",
        json!({"hypothesis_ref": hypothesis, "rationale": "Narrow.", "replacement": hypothesis_body(question)}),
        None,
    );
    assert_eq!(
        fixture.state.revision(amended).unwrap().header.extensions,
        record.header.extensions
    );
    let replaced = fixture.one(
        "hypothesis.amend",
        json!({"hypothesis_ref": amended, "rationale": "Narrow again.", "replacement": hypothesis_body(question)}),
        Some(phases(json!(["cold"]))),
    );
    assert_eq!(
        json!(fixture.state.revision(replaced).unwrap().header.extensions),
        json!({PACK_ID: {"phases": ["cold"]}})
    );
    // `null` removes the pack's fields.
    let removed = fixture.one(
        "hypothesis.amend",
        json!({"hypothesis_ref": replaced, "rationale": "Drop phases.", "replacement": hypothesis_body(question)}),
        Some(json!({PACK_ID: null})),
    );
    assert!(
        fixture
            .state
            .revision(removed)
            .unwrap()
            .header
            .extensions
            .is_empty()
    );
    // Earlier revisions keep what they were recorded with.
    assert_eq!(
        json!(
            fixture
                .state
                .revision(hypothesis)
                .unwrap()
                .header
                .extensions
        ),
        json!({PACK_ID: {"phases": ["warm", "steady"]}})
    );
    let shown = projection(fixture.state.revision(replaced).unwrap()).unwrap();
    assert_eq!(shown["interpretation"], INTERPRETATION);
    assert_eq!(
        shown["fields"],
        json!([{"pack_id": PACK_ID, "field": "phases", "value": ["cold"]}])
    );
}

#[test]
fn an_invalid_value_is_rejected_with_its_field_path() {
    let mut fixture = Fixture::adopted();
    let question = fixture.question();
    let body = hypothesis_body(question);
    let cases = [
        (
            phases(json!(["warm", "lukewarm"])),
            "/operations/0/extensions/bench-notes/phases/1",
            "enum",
        ),
        (
            phases(json!(["cold", "warm", "steady", "cold", "warm"])),
            "/operations/0/extensions/bench-notes/phases",
            "max_items",
        ),
        (
            phases(json!("warm")),
            "/operations/0/extensions/bench-notes/phases",
            "Expected an array",
        ),
        (
            json!({PACK_ID: {"phase": ["warm"]}}),
            "/operations/0/extensions/bench-notes/phase",
            "declares no such property",
        ),
        (
            json!({PACK_ID: {"warmup_runs": 3}}),
            "/operations/0/extensions/bench-notes/warmup_runs",
            "other record kinds",
        ),
        (
            json!({PACK_ID: {}}),
            "/operations/0/extensions/bench-notes",
            "Supply an object",
        ),
        (
            json!({PACK_ID: ["warm"]}),
            "/operations/0/extensions/bench-notes",
            "Supply an object",
        ),
    ];
    for (extensions, path, message) in cases {
        let refused = fixture.try_one("hypothesis.create", body.clone(), extensions.clone());
        assert_eq!(refused.error.code, ErrorCode::InvalidInput, "{extensions}");
        assert_eq!(refused.error.path.as_deref(), Some(path), "{extensions}");
        assert!(
            refused.error.message.contains(message),
            "{extensions}: {}",
            refused.error.message
        );
    }
    // Nothing was recorded by the refused operations.
    assert_eq!(
        fixture
            .state
            .records()
            .filter(|record| record.header.id.kind() == Kind::Hypothesis)
            .count(),
        0
    );
}

#[test]
fn nested_object_and_scalar_values_are_checked_against_the_closed_subset() {
    let machine = |value: Value| json!({PACK_ID: {"machine": value}});
    let cases = [
        (
            machine(json!({"cores": 8})),
            "/bench-notes/machine/cpu",
            "Required field is missing",
        ),
        (
            machine(json!({"cpu": "x", "socket": 1})),
            "/bench-notes/machine/socket",
            "not declared",
        ),
        (
            machine(json!({"cpu": "x", "cores": 0})),
            "/bench-notes/machine/cores",
            "outside the property's range",
        ),
        (
            machine(json!({"cpu": "x", "cores": 2.5})),
            "/bench-notes/machine/cores",
            "Expected an integer",
        ),
        (
            machine(json!({"cpu": "x", "clock_ghz": 3.2})),
            "/bench-notes/machine/clock_ghz",
            "decimal string",
        ),
        (
            machine(json!({"cpu": "x", "clock_ghz": "3.2e0"})),
            "/bench-notes/machine/clock_ghz",
            "decimal string",
        ),
        (
            machine(json!({"cpu": "x".repeat(65)})),
            "/bench-notes/machine/cpu",
            "max_length",
        ),
        (
            json!({PACK_ID: {"warmup_runs": -1}}),
            "/bench-notes/warmup_runs",
            "outside the property's range",
        ),
        (
            json!({PACK_ID: {"warmup_runs": null}}),
            "/bench-notes/warmup_runs",
            "Expected an integer",
        ),
    ];
    let loaded = method_pack::load(PACK.as_bytes()).unwrap();
    let schema = |name: &str| {
        &loaded
            .pack()
            .properties
            .iter()
            .find(|property| property.name == name)
            .unwrap()
            .schema
    };
    for (extensions, path, message) in cases {
        let (name, value) = extensions[PACK_ID]
            .as_object()
            .unwrap()
            .iter()
            .next()
            .unwrap();
        let mut segments = vec![PACK_ID.to_owned(), name.clone()];
        let refused = check_value(schema(name), value, &mut segments).unwrap_err();
        assert_eq!(refused.error.path.as_deref(), Some(path), "{extensions}");
        assert!(
            refused.error.message.contains(message),
            "{extensions}: {}",
            refused.error.message
        );
        assert!(refused.error.expected.is_some());
    }
    let mut segments = vec![PACK_ID.to_owned(), "machine".into()];
    check_value(
        schema("machine"),
        &json!({"cpu": "x86-64", "cores": 16, "clock_ghz": "3.20"}),
        &mut segments,
    )
    .unwrap();
    for good in ["0", "-1.5", "18.70"] {
        assert!(decimal(good), "{good}");
    }
    for bad in ["", "-", ".5", "5.", "1.2.3", "+1", " 1", "1e3"] {
        assert!(!decimal(bad), "{bad}");
    }
}

#[test]
fn an_unadopted_namespace_is_refused_with_a_repair_that_names_adoption() {
    let mut fixture = Fixture::new();
    let question = fixture.question();
    let refused = fixture.try_one(
        "hypothesis.create",
        hypothesis_body(question),
        phases(json!(["warm"])),
    );
    assert_eq!(refused.error.code, ErrorCode::UnknownReference);
    assert_eq!(
        refused.error.path.as_deref(),
        Some("/operations/0/extensions/bench-notes")
    );
    assert!(refused.error.message.contains("method_pack.adopt"));
    assert!(refused.error.message.contains("opaque metadata"));
    assert_eq!(
        refused.error.repair.as_ref().unwrap().schema.as_deref(),
        Some("method_pack.adopt")
    );
    // Core-looking keys are not namespaces either, so no extension can reach
    // the `inv.*` header keys the gap rules read.
    for key in ["inv.time_sensitive_preservation", "inv", "general"] {
        let refused = fixture.try_one(
            "hypothesis.create",
            hypothesis_body(question),
            json!({key: {"x": true}}),
        );
        assert!(
            matches!(
                refused.error.code,
                ErrorCode::UnknownReference | ErrorCode::InvalidInput
            ),
            "{key}"
        );
    }
}

#[test]
fn a_pack_adopted_earlier_in_the_same_transaction_applies() {
    let mut fixture = Fixture::new();
    let artifact = fixture.artifact(PACK.as_bytes());
    fixture
        .packs
        .0
        .insert(digest(PACK.as_bytes()).as_str().to_owned(), PACK.into());
    let question = fixture.question();
    let accepted = fixture
        .submit(json!([
            {"op":"method_pack.adopt","payload":{
                "artifact_ref": artifact, "pack_id": PACK_ID, "version": "1.0.0",
                "digest": digest(PACK.as_bytes()), "reason": "Use bench bookkeeping."}},
            {"op":"hypothesis.create","payload": hypothesis_body(question),
             "extensions": phases(json!(["steady"]))}
        ]))
        .unwrap();
    let hypothesis = accepted
        .records
        .iter()
        .find(|record| record.header.id.kind() == Kind::Hypothesis)
        .unwrap();
    assert_eq!(
        json!(hypothesis.header.extensions),
        json!({PACK_ID: {"phases": ["steady"]}})
    );
}

#[test]
fn unavailable_pack_bytes_refuse_new_values_but_never_recording_or_replay() {
    let mut fixture = Fixture::adopted();
    let question = fixture.question();
    let hypothesis = fixture.hypothesis(question, Some(phases(json!(["warm"]))));
    fixture.packs.0.clear();
    let refused = fixture.try_one(
        "hypothesis.create",
        hypothesis_body(question),
        phases(json!(["cold"])),
    );
    assert_eq!(refused.error.code, ErrorCode::GateFailed);
    assert_eq!(
        refused.error.path.as_deref(),
        Some("/operations/0/extensions/bench-notes")
    );
    assert_eq!(
        refused.error.actual.as_ref().unwrap()["unavailable_reason"],
        "bytes_unavailable"
    );
    // Recording without extensions continues, and revising the record keeps
    // its stored values without consulting the pack.
    let amended = fixture.one(
        "hypothesis.amend",
        json!({"hypothesis_ref": hypothesis, "rationale": "Narrow.", "replacement": hypothesis_body(question)}),
        None,
    );
    assert_eq!(
        json!(fixture.state.revision(amended).unwrap().header.extensions),
        json!({PACK_ID: {"phases": ["warm"]}})
    );
}

#[test]
fn extensions_need_one_target_record() {
    let image = |id: &str| Record {
        header: crate::header::RecordHeader {
            id: id.parse().unwrap(),
            ..fixture_header()
        },
        body: json!({}),
    };
    let one = "hyp_018d87fc-5234-7000-8000-000000000010";
    let other = "hyp_018d87fc-5234-7000-8000-000000000011";
    assert_eq!(
        target(None, &[image(one), image(one)]).unwrap(),
        one.parse().unwrap()
    );
    assert_eq!(
        target(Some(other.parse().unwrap()), &[image(one), image(other)]).unwrap(),
        other.parse().unwrap()
    );
    let refused = target(None, &[image(one), image(other)]).unwrap_err();
    assert_eq!(refused.error.code, ErrorCode::InvalidInput);
}

fn fixture_header() -> crate::header::RecordHeader {
    let mut fixture = Fixture::new();
    let question = fixture.question();
    fixture.state.revision(question).unwrap().header.clone()
}

/// Replay is a pure application of accepted images: it never loads a pack,
/// so a record keeps its extensions with no pack bytes anywhere.
#[test]
fn accepted_extensions_replay_without_any_pack() {
    let mut fixture = Fixture::adopted();
    let question = fixture.question();
    let hypothesis = fixture.hypothesis(question, Some(phases(json!(["warm"]))));
    let mut replayed = State::default();
    // Rebuild through `State::apply` from the transactions' after-images.
    for transaction in transactions(&fixture.state) {
        replayed.apply(&transaction).unwrap();
    }
    assert_eq!(
        replayed.revision(hypothesis).unwrap().header.extensions,
        fixture
            .state
            .revision(hypothesis)
            .unwrap()
            .header
            .extensions
    );
}

/// The accepted transactions of a state, reconstructed from its history.
fn transactions(state: &State) -> Vec<AcceptedTransaction> {
    let mut by_sequence: BTreeMap<u64, Vec<Record>> = BTreeMap::new();
    let mut ids: Vec<Id> = state.records().map(|record| record.header.id).collect();
    ids.sort();
    for id in ids {
        for record in state.history(id) {
            by_sequence
                .entry(record.header.acceptance_sequence)
                .or_default()
                .push(record.clone());
        }
    }
    by_sequence
        .into_iter()
        .map(|(sequence, mut records)| {
            // The case record leads a transaction that creates it.
            records.sort_by_key(|record| (record.header.id.kind() != Kind::Case, record.header.id));
            let case_id = records[0].header.case_id;
            let change = |record: &Record| RecordChange {
                client_ref: None,
                id: record.header.id,
                revision: record.header.revision.0,
            };
            AcceptedTransaction {
                case_id,
                sequence,
                created: records
                    .iter()
                    .filter(|record| record.header.revision.0 == 1)
                    .map(change)
                    .collect(),
                updated: records
                    .iter()
                    .filter(|record| record.header.revision.0 > 1)
                    .map(change)
                    .collect(),
                records,
                warnings: Vec::new(),
            }
        })
        .collect()
}

/// Run the same work twice, once with pack extensions on every record that
/// can carry them, and return the states.
fn twin(with_extensions: bool) -> Fixture {
    let ext = |value: Value| with_extensions.then_some(value);
    let mut fixture = Fixture::adopted();
    let question = fixture.question();
    let hypothesis = fixture.hypothesis(question, ext(phases(json!(["warm"]))));
    let rival_body = json!({
        "question_ref": question, "statement": "The first reading is cold-cache noise.",
        "possible_falsifiers": ["A warm cache reproduces it."], "rival_refs": [hypothesis],
        "assumption_refs": []
    });
    let rival = fixture.one(
        "hypothesis.create",
        rival_body.clone(),
        ext(phases(json!(["cold"]))),
    );
    fixture.one(
        "assumption.create",
        json!({"affected_refs":[hypothesis],"claim":"The timer is monotonic.",
               "consequence":"Readings compare.","materiality":"material"}),
        None,
    );
    let artifact = fixture.artifact(b"median 12.5 ms over 30 runs\n");
    let observation = fixture.observation(
        artifact,
        ext(json!({PACK_ID: {"warmup_runs": 5, "machine": {"cpu": "x86-64", "cores": 16}}})),
    );
    // An assessment of the rival that its amendment below leaves needing review.
    fixture.one(
        "assessment.add",
        json!({"subject_ref": rival, "evidence_refs": [observation], "relation": "inconclusive",
               "rationale": "One reading cannot rule out cold-cache noise."}),
        None,
    );
    let mut renamed = rival_body;
    renamed["statement"] = json!("The first reading is page-cache noise.");
    let rival = fixture.one(
        "hypothesis.amend",
        json!({"hypothesis_ref": rival, "rationale": "Name the cache.", "replacement": renamed}),
        ext(phases(json!(["cold", "warm"]))),
    );
    let hypothesis = fixture.one(
        "hypothesis.amend",
        json!({"hypothesis_ref": hypothesis, "rationale": "Narrow.", "replacement": hypothesis_body(question)}),
        ext(phases(json!(["warm", "steady"]))),
    );
    let assessment = fixture.one(
        "assessment.add",
        json!({"subject_ref": hypothesis, "evidence_refs": [observation], "relation": "inconclusive",
               "rationale": "One reading cannot separate the accounts."}),
        None,
    );
    let conclusion = fixture.one(
        "conclusion.create",
        json!({"statement":"Warm-up has not been shown to matter.","scope":"This bench.",
               "significance":"Keep discarding runs.","support_refs":[observation, assessment],
               "contradiction_refs":[rival],"caveats":["No readings."],"confidence":null}),
        ext(json!({PACK_ID: {"phases": ["steady"]}})),
    );
    // A promotion gate: its outcome must not depend on the extensions.
    let record = fixture.state.current(conclusion.id).unwrap().clone();
    let mut preconditions = required_read_set(&fixture.state, &record).unwrap();
    preconditions.push(conclusion);
    let gate = fixture.command(
        Some(fixture.case_id),
        preconditions,
        json!([{"op":"conclusion.request_review","payload":{"conclusion_ref":conclusion}}]),
    );
    fixture.gate = Some(
        gate.map(|accepted| json!(accepted.records))
            .map_err(|f| json!(f)),
    );
    fixture
}

#[test]
fn extensions_never_change_gates_freshness_ranking_blindspots_or_basis_digests() {
    let plain = twin(false);
    let extended = twin(true);
    assert!(
        extended
            .state
            .records()
            .filter(|record| !record.header.extensions.is_empty())
            .count()
            >= 3
    );
    let stripped = |records: Value| {
        let mut records = records;
        for record in records.as_array_mut().into_iter().flatten() {
            record["header"]["extensions"] = json!({});
        }
        records
    };
    let gate = |fixture: &Fixture| match fixture.gate.clone().unwrap() {
        Ok(records) => Ok(stripped(records)),
        Err(failure) => Err(failure),
    };
    assert_eq!(gate(&plain), gate(&extended));
    assert!(plain.gate.as_ref().unwrap().is_ok(), "{:?}", plain.gate);

    let snapshot = Snapshot {
        sequence: plain.state.sequence(),
        head_hash: format!("sha256:{}", "a".repeat(64)).parse().unwrap(),
    };
    let views = |fixture: &Fixture| {
        let case = fixture.case_id;
        let state = &fixture.state;
        let gaps = enumerate_gaps(state, &snapshot, case).unwrap();
        let next = rank_next(state, &gaps, 10);
        let freshness: Vec<Value> = FreshnessIndex::build(state, case)
            .records()
            .map(|record| json!(record))
            .collect();
        let audit = audit(state, &snapshot, case, &AuditRequest::default()).unwrap();
        let digests: Vec<Value> = state
            .records()
            .flat_map(|record| state.history(record.header.id))
            .map(|record| json!([record.reference(), basis_digest(record).ok()]))
            .collect();
        let bodies: Vec<Value> = state
            .records()
            .flat_map(|record| state.history(record.header.id))
            .map(|record| json!([record.reference(), record.body]))
            .collect();
        json!({"gaps": gaps, "next": next, "freshness": freshness, "audit": audit,
               "basis_digests": digests, "bodies": bodies})
    };
    let (plain_views, extended_views) = (views(&plain), views(&extended));
    for key in [
        "gaps",
        "next",
        "freshness",
        "audit",
        "basis_digests",
        "bodies",
    ] {
        assert_eq!(plain_views[key], extended_views[key], "{key}");
    }
    assert!(!plain_views["gaps"]["gaps"].as_array().unwrap().is_empty());
    assert!(
        plain_views["freshness"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record.to_string().contains("needs_review")
                || record.to_string().contains("stale")),
        "{:#}",
        plain_views["freshness"]
    );
}
