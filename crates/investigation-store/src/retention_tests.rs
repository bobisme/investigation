//! Reclamation crash schedules (W11.19). Every numbered filesystem call of
//! a reclamation is an injected fault, followed by a crash that keeps only
//! what was synced, and a retry. The blob ends removed exactly once, the
//! journal never changes, and `check` accepts the removed bytes.

use super::{
    crash_tests::{restart, setup},
    retention::ReclaimOutcome,
    *,
};
use crate::durable::MemoryFs;
use investigation_core::{evidence::ArtifactBody, wire::Digest};
use std::fs;

const ACTOR: &str = "agent:reclaim-test";

/// A new case retaining `bytes` from stdin: (case, digest).
fn ingest(store: &mut Store<MemoryFs>, key: &str, bytes: &[u8]) -> (Id, Digest) {
    let manifest = json!({
        "schema_version":"inv.ingest.v1","command_id":key,"actor":ACTOR,
        "new_case":{"title":"Which token leaked?"},
        "source":{"title":"Capture"},
        "content":{"kind":"stdin","name":"capture"},
    });
    let mut input = bytes;
    let receipt = store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), Some(&mut input))
        .unwrap();
    let case = receipt
        .receipt
        .created
        .iter()
        .find(|change| change.id.kind() == Kind::Case)
        .unwrap()
        .id;
    (case, receipt.items[0].digest.clone())
}

/// Withdraw the availability of every Artifact of `case` recording `digest`.
fn expire_all(store: &mut Store<MemoryFs>, case: Id, digest: &Digest) {
    let state = store.case(case).unwrap().state;
    let operations: Vec<Value> = state
        .records_of_kind(Kind::Artifact)
        .filter(|record| {
            investigation_core::input::decode::<ArtifactBody>(&record.body)
                .is_ok_and(|body| body.digest.as_ref() == Some(digest))
        })
        .map(|record| {
            json!({"op":"artifact.expire","payload":{
                "artifact_ref": record.reference(), "reason":"sensitive_content",
                "rationale":"The capture holds a live token."
            }})
        })
        .collect();
    assert!(!operations.is_empty());
    let request = json!({
        "schema_version":"inv.command.v1","command_id":format!("expire-{case}"),"case_id":case,
        "actor":ACTOR,"preconditions":[],"operations":operations,
    });
    store
        .record(&serde_json::to_vec(&request).unwrap())
        .unwrap();
}

fn stored(store: &Store<MemoryFs>, digest: &Digest) -> bool {
    store
        .fs
        .identity(Path::new(&archive::blob_path(digest)))
        .is_ok()
}

/// Two captures whose withdrawn bytes are reclaimed in one call, so a fault
/// can land before the first unlink is durable, between the two removals,
/// or after both.
#[test]
fn an_interrupted_reclamation_removes_each_blob_once_on_retry_at_every_step() {
    let mut before_any = 0;
    let mut between = 0;
    let mut held_safe = 0;
    let mut clean = 0;
    for fault in 1..=80 {
        let (mut store, root) = setup(7_000 + fault);
        let (first_case, first) = ingest(&mut store, "capture-1", b"token=abc123 one copy\n");
        let (second_case, second) = ingest(&mut store, "capture-2", b"token=def456 one copy\n");
        expire_all(&mut store, first_case, &first);
        expire_all(&mut store, second_case, &second);
        let events: Vec<usize> = [first_case, second_case]
            .iter()
            .map(|case| store.case(*case).unwrap().events.len())
            .collect();
        let digests = BTreeSet::from([first.clone(), second.clone()]);

        store.fs.fail_at(fault as usize);
        let attempt = store.reclaim_blobs(&digests);
        restart(&mut store);
        let present: Vec<bool> = digests
            .iter()
            .map(|digest| stored(&store, digest))
            .collect();
        match &attempt {
            // A holder that could not be read (a pin record, the staging
            // root) holds the bytes: a fault keeps them, never the reverse.
            Ok(done) if done.iter().any(|d| d.outcome == ReclaimOutcome::Waiting) => {
                for (reclamation, present) in done.iter().zip(&present) {
                    assert_eq!(
                        *present,
                        reclamation.outcome == ReclaimOutcome::Waiting,
                        "fault {fault}"
                    );
                }
                held_safe += 1;
            }
            Ok(done) => {
                assert!(
                    done.iter().all(|d| d.outcome == ReclaimOutcome::Reclaimed),
                    "fault {fault}"
                );
                assert_eq!(
                    present,
                    [false, false],
                    "an acknowledged removal came back, fault {fault}"
                );
                clean += 1;
            }
            Err(_) if present == [true, true] => before_any += 1,
            Err(_) => {
                assert_eq!(
                    present,
                    [false, true],
                    "removals are made in order, fault {fault}"
                );
                between += 1;
            }
        }

        let retry = store.reclaim_blobs(&digests).unwrap();
        for (reclamation, present) in retry.iter().zip(&present) {
            assert_eq!(
                reclamation.outcome,
                if *present {
                    ReclaimOutcome::Reclaimed
                } else {
                    ReclaimOutcome::AlreadyAbsent
                },
                "fault {fault}"
            );
            assert!(
                !stored(&store, &reclamation.obligations.digest),
                "fault {fault}"
            );
        }
        // Records, digests and the journal are untouched.
        for (case, events) in [first_case, second_case].iter().zip(events) {
            let replay = store.case(*case).unwrap();
            assert_eq!(replay.events.len(), events, "fault {fault}");
            assert!(replay.state.records_of_kind(Kind::Artifact).all(|record| {
                record.body["availability"] == "intentionally_removed"
                    || !digests
                        .iter()
                        .any(|digest| record.body["digest"] == json!(digest))
            }));
        }
        let report = store.check().unwrap();
        assert!(report.integrity_ok(), "fault {fault}: {:?}", report.issues);
        assert_eq!(report.reclaimed_blobs, 2, "fault {fault}");
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
    assert!(
        before_any > 0 && between > 0 && clean > 0,
        "schedule missed a step: before {before_any}, between {between}, \
         held {held_safe}, clean {clean}"
    );
}

#[test]
fn bytes_another_case_holds_are_never_removed_and_unwithdrawn_bytes_are_refused() {
    let (mut store, root) = setup(7_900);
    let (case_a, digest) = ingest(&mut store, "capture-a", b"shared capture bytes\n");
    let (case_b, same) = ingest(&mut store, "capture-b", b"shared capture bytes\n");
    assert_eq!(digest, same);
    let digests = BTreeSet::from([digest.clone()]);
    // Nothing withdrawn yet: reclamation refuses to touch the bytes.
    let refused = store.reclaim_blobs(&digests).unwrap();
    assert_eq!(refused[0].outcome, ReclaimOutcome::NotWithdrawn);
    expire_all(&mut store, case_a, &digest);
    let held = store.reclaim_blobs(&digests).unwrap();
    assert_eq!(held[0].outcome, ReclaimOutcome::HeldByRecords);
    assert!(
        held[0]
            .obligations
            .references
            .iter()
            .all(|holder| holder.case_id == case_b)
    );
    assert!(stored(&store, &digest));
    // A reimport would publish the bytes again under the commit lock; here
    // the surviving case reads them as before.
    let replay = store.case(case_b).unwrap();
    let artifact = replay
        .state
        .records_of_kind(Kind::Artifact)
        .find(|record| record.body["digest"] == json!(digest))
        .unwrap();
    let request = investigation_core::input::decode(&json!({
        "schema_version":"inv.selector.v1","artifact_digest":digest,"selector":{"type":"whole"}
    }))
    .unwrap();
    store.evidence_read(&replay, artifact, request).unwrap();
    expire_all(&mut store, case_b, &digest);
    let done = store.reclaim_blobs(&digests).unwrap();
    assert_eq!(done[0].outcome, ReclaimOutcome::Reclaimed);
    assert_eq!(done[0].obligations.withdrawn.len(), {
        let mut withdrawn = 0;
        for case in [case_a, case_b] {
            withdrawn += store
                .case(case)
                .unwrap()
                .state
                .records_of_kind(Kind::Artifact)
                .filter(|record| record.body["digest"] == json!(digest))
                .count();
        }
        withdrawn
    });
    assert!(!stored(&store, &digest));
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
