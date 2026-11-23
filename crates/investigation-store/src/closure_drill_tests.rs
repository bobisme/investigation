//! Adversarial drill 6 (§17.7, §8.6): the closure transaction under kill and
//! retry. A child of `store`, so it drives the production commit path on the
//! crash-model filesystem of `durable`, like `crash_tests`.
//!
//! The batch is `retrospective.record` plus `case.close` on a material case
//! (one failed Run). A fault is injected at every filesystem call of the
//! commit in turn, then the process crashes and restarts. The walk ends after
//! several consecutive faults land past the last call, so no bound has to be
//! tuned to the commit path's length. At every crash point:
//!
//! - the retrospective and the closure both exist or neither does, and the
//!   closure cites exactly the retrospective that exists;
//! - a committed batch is found by its command ID with the IDs it was
//!   acknowledged with, and the identical retry returns those IDs without a
//!   new event;
//! - CAS survives the crash: a new command ID reusing the old case revision
//!   is a revision conflict, the original command ID with a changed body is
//!   refused, and a batch that never committed cannot be retried past a
//!   concurrent case revision or past new material evidence.
use std::fs;

use investigation_core::wire::CommitResult;

use super::{
    crash_tests::{apply, restart, setup},
    *,
};
use crate::durable::MemoryFs;

const ACTOR: &str = "agent:crash-test";
/// Faults past the commit path's last call acknowledge; this many in a row
/// end the walk.
const TAIL: usize = 5;

fn parse(
    case: Option<Id>,
    key: &str,
    ops: Value,
    preconditions: Value,
) -> (Command, SemanticRequest) {
    Command::parse(
        &serde_json::to_vec(&json!({
            "schema_version":"inv.command.v0-alpha","command_id":key,"case_id":case,"actor":ACTOR,
            "preconditions":preconditions,"operations":ops,
        }))
        .unwrap(),
    )
    .unwrap()
}

fn failed_run(case: Id, key: &str) -> (Command, SemanticRequest) {
    parse(
        Some(case),
        key,
        json!([{"op":"run.record","payload":{
            "intent":{"kind":"exploration","intent":"Run the benchmark once."},
            "producer":"agent:fixture","execution_status":"failed","evidence_completeness":"complete",
            "evidence_kind":"observational","variables_changed":[],"artifact_refs":[],
        }}]),
        json!([]),
    )
}

struct Fixture {
    store: Store<MemoryFs>,
    root: PathBuf,
    case: Id,
    batch: (Command, SemanticRequest),
}

impl Fixture {
    /// A material measurement case and its closure batch, committed up to
    /// but not including the batch.
    fn new(seed: u64) -> Self {
        let (mut store, root) = setup(seed);
        let (command, semantic) = parse(
            None,
            "case",
            json!([{"op":"case.create","payload":{"title":"Is the benchmark valid?","mode":"measure"}}]),
            json!([]),
        );
        let case = apply(&mut store, &command, &semantic, 0).unwrap().created[0].id;
        let (command, semantic) = failed_run(case, "run");
        let run = apply(&mut store, &command, &semantic, 100).unwrap().created[0].id;
        let snapshot = store.case(case).unwrap().snapshot;
        let batch = Self::batch(case, run, snapshot, "close-batch", "The only run failed.");
        Self {
            store,
            root,
            case,
            batch,
        }
    }

    fn batch(
        case: Id,
        run: Id,
        snapshot: Snapshot,
        key: &str,
        reason: &str,
    ) -> (Command, SemanticRequest) {
        let case_ref = json!({"id":case,"revision":1});
        parse(
            Some(case),
            key,
            json!([
                {"op":"retrospective.record","client_ref":"account","payload":{
                    "case_ref":case_ref,"snapshot":snapshot,
                    "methods_attempted":[{"name":"Single benchmark run"}],"effective_steps":[],
                    "invalid_or_misleading_steps":[{"description":"The run failed before measuring.","support_refs":[run]}],
                    "costs_or_constraints":"One run.","residual_unknowns":["Whether a rerun succeeds."],
                    "lessons":[],"no_reusable_lesson_reason":"One failed run teaches nothing portable.",
                }},
                {"op":"case.close","payload":{
                    "outcome":"inconclusive","reason":reason,"unresolved_questions":[],
                    "reopen_condition":"A run completes.","retrospective_ref":{"client_ref":"account"},
                }}
            ]),
            json!([case_ref]),
        )
    }

    fn submit(&mut self, ids: u64) -> Result<Receipt, Failure> {
        let (command, semantic) = self.batch.clone();
        apply(&mut self.store, &command, &semantic, ids)
    }

    fn lookup(&mut self) -> Option<Receipt> {
        let key = self.batch.0.command_id.clone();
        match self.store.receipt(&self.batch.0.actor, &key).unwrap() {
            ReceiptLookup::Found { receipt } => Some(*receipt),
            ReceiptLookup::NotFoundAtSnapshot { .. } => None,
        }
    }

    /// What a verified replay holds of the batch.
    fn outcome(&mut self) -> Outcome {
        let replay = self.store.case(self.case).unwrap();
        let case = replay.state.current(self.case).unwrap();
        Outcome {
            events: replay.events.len(),
            closed: case.body["lifecycle"] == "closed",
            closure_ref: case.body["closure"]["retrospective_ref"]["id"]
                .as_str()
                .map(|id| id.parse().unwrap()),
            retrospectives: replay
                .state
                .records()
                .filter(|record| record.header.id.kind() == Kind::Retrospective)
                .map(|record| record.header.id)
                .collect(),
        }
    }

    fn remove(self) {
        let root = self.root.clone();
        drop(self);
        fs::remove_dir_all(root).unwrap();
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    events: usize,
    closed: bool,
    closure_ref: Option<Id>,
    retrospectives: Vec<Id>,
}

impl Outcome {
    fn neither() -> Self {
        Self {
            events: 2,
            closed: false,
            closure_ref: None,
            retrospectives: vec![],
        }
    }

    fn both(receipt: &Receipt, case: Id) -> Self {
        assert_eq!(receipt.created.len(), 1);
        assert_eq!(receipt.created[0].client_ref.as_deref(), Some("account"));
        assert_eq!(receipt.updated.len(), 1);
        assert_eq!(receipt.updated[0].id, case);
        let retrospective = receipt.created[0].id;
        Self {
            events: 3,
            closed: true,
            closure_ref: Some(retrospective),
            retrospectives: vec![retrospective],
        }
    }
}

fn same_ids(left: &Receipt, right: &Receipt, at: usize) {
    assert_eq!(left.transaction_id, right.transaction_id, "fault {at}");
    assert_eq!(left.created, right.created, "fault {at}");
    assert_eq!(left.updated, right.updated, "fault {at}");
}

#[test]
fn every_crash_point_of_the_closure_batch_leaves_both_or_neither_and_the_retry_keeps_the_ids() {
    let mut not_committed = 0;
    let mut committed_unacknowledged = 0;
    let mut acknowledged = 0;
    let mut unknown = 0;
    let mut consecutive = 0;
    for fault in 1.. {
        if consecutive == TAIL {
            break;
        }
        let mut fixture = Fixture::new(900 + fault as u64);
        fixture.store.fs.fail_at(fault);
        let first = fixture.submit(1_000);
        consecutive = if first.is_ok() { consecutive + 1 } else { 0 };
        if let Err(failure) = &first {
            unknown += usize::from(failure.error.code == ErrorCode::DurabilityUnknown);
        }
        restart(&mut fixture.store);

        // Both or neither, and a committed batch keeps its acknowledged IDs.
        let found = fixture.lookup();
        match (&first, &found) {
            (Ok(first), Some(found)) => {
                acknowledged += 1;
                same_ids(first, found, fault);
            }
            (Ok(_), None) => panic!("fault {fault}: an acknowledged closure was lost"),
            (Err(_), Some(_)) => committed_unacknowledged += 1,
            (Err(_), None) => not_committed += 1,
        }
        let expected = found.as_ref().map_or_else(Outcome::neither, |receipt| {
            Outcome::both(receipt, fixture.case)
        });
        assert_eq!(fixture.outcome(), expected, "fault {fault}");

        // The identical retry settles it, with the original IDs if committed.
        let retried = fixture
            .submit(2_000)
            .unwrap_or_else(|failure| panic!("fault {fault}: {failure:?}"));
        if let Some(found) = &found {
            assert_eq!(
                retried.result,
                CommitResult::AlreadyCommitted,
                "fault {fault}"
            );
            same_ids(found, &retried, fault);
        } else {
            assert_eq!(retried.result, CommitResult::Committed, "fault {fault}");
        }
        let settled = Outcome::both(&retried, fixture.case);
        assert_eq!(fixture.outcome(), settled, "fault {fault}");
        restart(&mut fixture.store);
        let again = fixture.submit(3_000).unwrap();
        assert_eq!(again.result, CommitResult::AlreadyCommitted);
        same_ids(&retried, &again, fault);

        // CAS: the old case revision under a new command ID conflicts, and
        // the original command ID with a changed body is refused.
        let run = fixture.outcome().retrospectives[0];
        let replay = fixture.store.case(fixture.case).unwrap();
        let account = replay.state.current(run).unwrap();
        let snapshot: Snapshot = serde_json::from_value(account.body["snapshot"].clone()).unwrap();
        let support = account.body["invalid_or_misleading_steps"][0]["support_refs"][0]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        for (key, reason, code) in [
            (
                "close-batch-again",
                "The only run failed.",
                ErrorCode::RevisionConflict,
            ),
            (
                "close-batch",
                "A changed account.",
                ErrorCode::IdempotencyKeyReused,
            ),
        ] {
            let (command, semantic) =
                Fixture::batch(fixture.case, support, snapshot.clone(), key, reason);
            let refused = apply(&mut fixture.store, &command, &semantic, 4_000).unwrap_err();
            assert_eq!(refused.error.code, code, "fault {fault} {key}");
        }
        assert_eq!(fixture.outcome(), settled, "fault {fault}");
        fixture.remove();
    }
    assert!(
        not_committed > 10,
        "the walk must reach the commit path: {not_committed}"
    );
    // No injected fault lands after the event directory's sync, so a kill
    // between the durable event and the acknowledgement is the acknowledged
    // branch followed by `restart`: the receipt is never delivered, and the
    // retry must find it.
    assert_eq!(
        committed_unacknowledged, 0,
        "a fault after the durable event is now possible: classify it"
    );
    assert!(
        unknown > 0,
        "the walk must reach an fsync with an unknown outcome"
    );
    assert!(acknowledged >= TAIL, "the walk must run past the last call");
}

#[test]
fn a_batch_that_never_committed_cannot_be_retried_past_a_concurrent_revision_or_new_evidence() {
    let mut checked = 0;
    let mut consecutive = 0;
    for fault in 1.. {
        if consecutive == TAIL {
            break;
        }
        for concurrent in ["amend", "evidence"] {
            let mut fixture = Fixture::new(2_000 + fault as u64);
            fixture.store.fs.fail_at(fault);
            let first = fixture.submit(1_000);
            restart(&mut fixture.store);
            if concurrent == "amend" {
                consecutive = if first.is_ok() { consecutive + 1 } else { 0 };
            }
            if fixture.lookup().is_some() {
                fixture.remove();
                continue;
            }
            checked += 1;
            // Another writer commits between the crash and the retry.
            let (command, semantic) = if concurrent == "amend" {
                parse(
                    Some(fixture.case),
                    "amend",
                    json!([{"op":"case.amend","payload":{
                        "reason":"Narrow the question.","title":"Is the nightly benchmark valid?"}}]),
                    json!([{"id":fixture.case,"revision":1}]),
                )
            } else {
                failed_run(fixture.case, "second-run")
            };
            let interleaved = apply(&mut fixture.store, &command, &semantic, 5_000).unwrap();
            let before = fixture.store.case(fixture.case).unwrap().events.len();
            assert_eq!(before, 3, "fault {fault} {concurrent}");

            let refused = fixture.submit(2_000).unwrap_err();
            if concurrent == "amend" {
                assert_eq!(
                    refused.error.code,
                    ErrorCode::RevisionConflict,
                    "fault {fault}"
                );
            } else {
                // The account's snapshot predates the new failed run.
                assert_eq!(
                    refused.error.code,
                    ErrorCode::RetrospectiveStale,
                    "fault {fault}"
                );
                assert_eq!(
                    refused.error.path.as_deref(),
                    Some("/operations/1/payload/retrospective_ref")
                );
                assert_eq!(
                    refused.error.actual,
                    Some(json!([{"id":interleaved.created[0].id,"revision":1}]))
                );
                let repair = refused.error.repair.as_ref().unwrap();
                assert_eq!(
                    repair.argv,
                    Some(investigation_core::retrospective_digest::draft_argv(
                        fixture.case
                    ))
                );
            }
            let outcome = fixture.outcome();
            assert_eq!(outcome.events, before, "fault {fault} {concurrent}");
            assert!(!outcome.closed && outcome.retrospectives.is_empty());
            fixture.remove();
        }
    }
    assert!(
        checked > 20,
        "the walk must reach uncommitted crash points: {checked}"
    );
}
