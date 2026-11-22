//! Development handoff: only case and question are active runtime operations.
use investigation_core::{
    catalog::{standard_catalog, wire_schema},
    command::Command,
    evidence::{ObservationAdd, SelectorKind, SourceBody},
    explain::HypothesisCreate,
    header::{Clock, UtcTimestamp},
    identity::IdSource,
    review::{AssessmentAdd, ConclusionCreate},
    state::{Acceptance, State, validate_and_apply},
};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const TRANSACTIONS: &str =
    include_str!("../../../fixtures/development/w01-handoff/transactions.json");
const DRAFTS: &str = include_str!("../../../fixtures/development/w01-handoff/drafts.json");
const EXPECTED: &str = include_str!("../../../fixtures/development/w01-handoff/expected.json");
const SOURCE: &str = include_str!("../../../fixtures/development/w01-handoff/source.txt");
const PROVENANCE: &str = include_str!("../../../fixtures/development/w01-handoff/provenance.json");

#[derive(Default)]
struct FixtureIds(u64);
impl IdSource for FixtureIds {
    fn next_uuid(&mut self) -> Uuid {
        self.0 += 1;
        format!("018d87fc-5234-7000-8000-{:012x}", self.0)
            .parse()
            .unwrap()
    }
}
struct FixtureClock;
impl Clock for FixtureClock {
    fn now_utc(&mut self) -> UtcTimestamp {
        "2026-11-22T08:12:44.000Z".parse().unwrap()
    }
}

fn typed_and_schema<T: DeserializeOwned + JsonSchema>(value: &Value) {
    let _: T = serde_json::from_value(value.clone()).unwrap();
    let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(value)
        .unwrap();
}

#[test]
fn handoff_replays_active_commands_and_checks_deferred_drafts() {
    let raw_commands: Vec<Value> = serde_json::from_str(TRANSACTIONS).unwrap();
    let schema = serde_json::to_value(wire_schema("record").unwrap()).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let commands = raw_commands
        .iter()
        .map(|raw| {
            validator.validate(raw).unwrap();
            Command::parse(&serde_json::to_vec(raw).unwrap()).unwrap().0
        })
        .collect::<Vec<_>>();
    let catalog = standard_catalog();
    let mut state = State::default();
    let mut ids = FixtureIds::default();
    let mut clock = FixtureClock;
    let mut images = Vec::new();
    for command in &commands {
        let accepted = validate_and_apply(
            &state,
            command,
            &catalog,
            Acceptance {
                sequence: state.sequence() + 1,
                clock: &mut clock,
                ids: &mut ids,
            },
        )
        .unwrap();
        state.apply(&accepted).unwrap();
        images.push(accepted);
    }
    let mut replay = State::default();
    for accepted in &images {
        replay.apply(accepted).unwrap();
    }
    assert_eq!(state, replay);
    assert_eq!(state.records().count(), 2);
    assert_eq!(
        images[0].case_id.to_string(),
        "case_018d87fc-5234-7000-8000-000000000001"
    );
    assert_eq!(
        images[1].created[0].id.to_string(),
        "q_018d87fc-5234-7000-8000-000000000002"
    );

    let draft: Value = serde_json::from_str(DRAFTS).unwrap();
    let expected: Value = serde_json::from_str(EXPECTED).unwrap();
    let provenance: Value = serde_json::from_str(PROVENANCE).unwrap();
    typed_and_schema::<SourceBody>(&draft["source"]);
    typed_and_schema::<ObservationAdd>(&draft["observation"]);
    typed_and_schema::<HypothesisCreate>(&draft["hypothesis"]);
    typed_and_schema::<AssessmentAdd>(&draft["assessment"]);
    typed_and_schema::<ConclusionCreate>(&draft["conclusion"]);
    let quote = draft["observation"]["basis"]["text_quote"]["exact"]
        .as_str()
        .unwrap();
    assert_eq!(SOURCE.matches(quote).count(), 1);
    let observation: ObservationAdd = serde_json::from_value(draft["observation"].clone()).unwrap();
    observation.validate_shape().unwrap();
    let selector: SelectorKind =
        serde_json::from_value(draft["observation"]["basis"]["selector"]["selector"].clone())
            .unwrap();
    let SelectorKind::ByteRange { start, end } = selector else {
        panic!("the fixture points at the exact quoted bytes");
    };
    assert_eq!(
        &SOURCE.as_bytes()[start as usize..end as usize],
        quote.as_bytes()
    );
    assert_eq!(
        SOURCE,
        "At 10:00 UTC, the application log reported a database connection wait of 900 ms.\nNo database-side measurements were collected.\n"
    );
    assert_eq!(
        draft["observation"]["basis"]["selector"]["artifact_digest"],
        format!("sha256:{:x}", Sha256::digest(SOURCE.as_bytes()))
    );
    assert_eq!(
        draft["observation"]["basis"]["artifact_ref"]["id"],
        provenance["artifact_id"]
    );
    assert_eq!(provenance["source_path"], expected["source"]);
    assert_eq!(
        expected["question"],
        commands[1].operations[0].payload["wording"]
    );
    assert_eq!(
        expected["observation"],
        draft["observation"]["value"]["statement"]
    );
    assert_eq!(expected["candidate"], draft["conclusion"]["statement"]);
    assert_eq!(expected["limitation"], draft["conclusion"]["caveats"][0]);
    assert_eq!(expected["disposition"], "candidate; not validated");
}
