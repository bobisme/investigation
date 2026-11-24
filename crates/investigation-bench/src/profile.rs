//! In-process phase timings of `next`, so a miss comes with a profile hint.
//!
//! The phases are the library calls the CLI makes: verified journal replay,
//! gap enumeration (the rule evaluation), ranking, and the weight-free Pareto
//! table that ranking reads, timed on its own. [`holder_scan`] times the
//! retention holder scan the same way (W11.19): `record_holders` over the
//! replayed case, and `Store::retention_obligations` for one digest, which
//! adds verified loading, pins and staging under the coordination lock.

use std::time::Instant;

use std::collections::BTreeSet;

use investigation_core::{
    decision_ops::pareto, gap_rules::enumerate_gaps, identity::Kind, next_rank::rank_next,
};
use investigation_store::{Store, Workspace, store::retention::record_holders};
use serde_json::{Value, json};

use crate::{fixtures::Fixture, measure::summarize};

fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1e3
}

pub fn next_phases(fixture: &Fixture, samples: usize) -> Result<Value, String> {
    let workspace = Workspace::open(&fixture.root).map_err(|error| error.to_string())?;
    let mut store = Store::open(workspace);
    let mut replay_ms = Vec::new();
    let mut gaps_ms = Vec::new();
    let mut rank_ms = Vec::new();
    let mut pareto_ms = Vec::new();
    let mut gap_count = 0;
    let mut pareto_options = 0;
    // One untimed warmup round, then the timed rounds.
    for round in 0..=samples {
        let started = Instant::now();
        let replay = store
            .case(fixture.case)
            .map_err(|failure| failure.error.message.clone())?;
        let replay_time = ms(started);
        let started = Instant::now();
        let inventory = enumerate_gaps(&replay.state, &replay.snapshot, fixture.case)
            .map_err(|failure| failure.error.message.clone())?;
        let gaps_time = ms(started);
        let started = Instant::now();
        let response = rank_next(&replay.state, &inventory, 3);
        let rank_time = ms(started);
        let started = Instant::now();
        let table = pareto(&replay.state, fixture.case, fixture.question)
            .map_err(|failure| failure.error.message.clone())?;
        let pareto_time = ms(started);
        std::hint::black_box(&response);
        gap_count = inventory.gaps.len();
        pareto_options = table.len();
        if round > 0 {
            replay_ms.push(replay_time);
            gaps_ms.push(gaps_time);
            rank_ms.push(rank_time);
            pareto_ms.push(pareto_time);
        }
    }
    Ok(json!({
        "fixture": fixture.name,
        "phases": "Store::case (verified replay) -> enumerate_gaps -> rank_next(limit 3); pareto(one question) timed separately and also called inside rank_next",
        "gap_inventory": gap_count,
        "pareto_question_options": pareto_options,
        "replay_ms": summarize(&replay_ms),
        "enumerate_gaps_ms": summarize(&gaps_ms),
        "rank_next_ms": summarize(&rank_ms),
        "pareto_ms": summarize(&pareto_ms),
    }))
}

/// In-process timings of the retention holder scan on `fixture`: the pure
/// `record_holders` pass over the replayed case, and
/// `Store::retention_obligations` for the digest of one Artifact.
pub fn holder_scan(fixture: &Fixture, samples: usize) -> Result<Value, String> {
    let workspace = Workspace::open(&fixture.root).map_err(|error| error.to_string())?;
    let mut store = Store::open(workspace);
    let replay = store
        .case(fixture.case)
        .map_err(|failure| failure.error.message.clone())?;
    let mut scan_ms = Vec::new();
    let mut obligations_ms = Vec::new();
    let mut digests_found = 0;
    let digest = record_holders([(fixture.case, &replay)], &BTreeSet::new())
        .map_err(|failure| failure.error.message.clone())?
        .into_iter()
        .find(|(_, holders)| !holders.references.is_empty())
        .map(|(digest, _)| digest);
    let artifacts = replay.state.records_of_kind(Kind::Artifact).count();
    for round in 0..=samples {
        let started = Instant::now();
        let found = record_holders([(fixture.case, &replay)], &BTreeSet::new())
            .map_err(|failure| failure.error.message.clone())?;
        let scan_time = ms(started);
        digests_found = found.len();
        std::hint::black_box(&found);
        let started = Instant::now();
        if let Some(digest) = &digest {
            let one = BTreeSet::from([digest.clone()]);
            let obligations = store
                .retention_obligations(&one, &BTreeSet::new())
                .map_err(|failure| failure.error.message.clone())?;
            std::hint::black_box(&obligations);
        }
        let obligations_time = ms(started);
        if round > 0 {
            scan_ms.push(scan_time);
            obligations_ms.push(obligations_time);
        }
    }
    Ok(json!({
        "fixture": fixture.name,
        "phases": "record_holders over the replayed case (pure); Store::retention_obligations for one digest (coordination lock, verified load of every case, live pins, staging, record_holders)",
        "objects": replay.state.records().count(),
        "artifacts": artifacts,
        "journal_events": replay.events.len(),
        "digests": digests_found,
        "record_holders_ms": summarize(&scan_ms),
        "retention_obligations_ms": summarize(&obligations_ms),
    }))
}
