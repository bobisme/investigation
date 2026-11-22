#![no_main]

use investigation_core::{
    catalog::{Catalog, standard_catalog},
    command::Command,
    header::{Clock, UtcTimestamp},
    identity::IdSource,
    state::{Acceptance, State, validate_and_apply},
    wire::{CommitStatus, ErrorCode},
};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use uuid::Uuid;

const HANDOFF: &str = include_str!("../../fixtures/development/w01-handoff/transactions.json");

struct SequentialIds(u64);

impl IdSource for SequentialIds {
    fn next_uuid(&mut self) -> Uuid {
        self.0 += 1;
        format!("018d87fc-5234-7000-8000-{:012x}", self.0)
            .parse()
            .expect("fixture ID is UUIDv7")
    }
}

struct FixedClock;

impl Clock for FixedClock {
    fn now_utc(&mut self) -> UtcTimestamp {
        "2026-11-22T12:00:00.000Z"
            .parse()
            .expect("valid fixture timestamp")
    }
}

struct Fixture {
    state: State,
    catalog: Catalog,
    last_id: u64,
}

impl Fixture {
    fn from_handoff() -> Self {
        let mut state = State::default();
        let catalog = standard_catalog();
        let mut ids = SequentialIds(0);
        let handoff: Vec<Value> = serde_json::from_str(HANDOFF).expect("handoff command array");
        for value in handoff {
            let bytes = serde_json::to_vec(&value).expect("handoff encodes");
            let (command, _) = Command::parse(&bytes).expect("handoff command decodes");
            let accepted = validate_and_apply(
                &state,
                &command,
                &catalog,
                Acceptance {
                    sequence: state.sequence() + 1,
                    clock: &mut FixedClock,
                    ids: &mut ids,
                },
            )
            .expect("handoff transaction validates");
            state.apply(&accepted).expect("handoff transaction replays");
        }
        Self {
            state,
            catalog,
            last_id: ids.0,
        }
    }
}

thread_local! {
    static FIXTURE: Fixture = Fixture::from_handoff();
}

fuzz_target!(|bytes: &[u8]| {
    let Ok((command, _)) = Command::parse(bytes) else {
        return;
    };
    FIXTURE.with(|fixture| {
        let before = fixture.state.clone();
        let sequence = before.sequence() + 1;
        let result = validate_and_apply(
            &fixture.state,
            &command,
            &fixture.catalog,
            Acceptance {
                sequence,
                clock: &mut FixedClock,
                ids: &mut SequentialIds(fixture.last_id),
            },
        );

        // Validation cannot publish any of a batch, regardless of its result.
        assert_eq!(fixture.state, before);
        match result {
            Err(failure) => {
                assert_eq!(failure.commit_status, CommitStatus::NotCommitted);
                assert_ne!(failure.error.code, ErrorCode::IntegrityFailure);
                let path = failure
                    .error
                    .path
                    .as_deref()
                    .expect("typed rejection has a location");
                assert!(path.is_empty() || path.starts_with('/'));
            }
            Ok(accepted) => {
                let mut replay = before;
                assert!(!accepted.records.is_empty());
                replay
                    .apply(&accepted)
                    .expect("accepted transaction replays");
                assert_eq!(replay.sequence(), sequence);
                for record in &accepted.records {
                    assert_eq!(replay.revision(record.reference()), Some(record));
                }
            }
        }
    });
});
