//! `brief --since TRANSACTION_ID`: what changed in meaning after a transaction.
//!
//! The delta compares the case as known just after the named transaction with
//! the read snapshot. It reports meaning, not raw record diffs: new evidence,
//! evidence withdrawn or retracted, disposition changes, records that became
//! stale or disputed (from the freshness index at both snapshots), new gaps,
//! retrospectives and their lessons, and synopses whose subject has moved on.
//! Every list is bounded by the same byte budget as a brief; the mandatory
//! caveat and contradiction of the case are always included.

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::{
    classification::{AudiencePolicy, Classifier, Field},
    freshness::{Freshness, FreshnessIndex, RecordFreshness},
    gap_rules::enumerate_gaps,
    header::display_title,
    identity::{Id, Kind, ResolveError, resolve_prefix},
    source_ops,
    state::{Record, State},
    synopsis::{self, SynopsisState},
    wire::{ErrorCode, Failure, Snapshot},
};
use investigation_store::{Store, journal::CaseReplay};
use serde_json::{Value, json};

use crate::{
    brief::{BriefError, BriefInputs, BriefLevel, Caps, Section, fill, follow_up},
    snapshot::pin_argv,
    views::{self, ViewDepth, project_record},
};

/// Reasons listed for each newly stale record; the rest are counted.
const SHOWN_REASONS: usize = 3;

/// The transaction a delta starts after, and the case as known right after it.
pub(crate) struct Since {
    transaction_id: Id,
    before: CaseReplay,
}

/// Resolve `selector` to a transaction of this case at or before the read
/// snapshot and rebuild the case as known right after it.
pub(crate) fn resolve_since(
    replay: &CaseReplay,
    selector: &str,
    prefix: &[String],
) -> Result<Since, Failure> {
    let retry = follow_up(prefix, &["brief"]);
    let candidates = replay
        .events
        .iter()
        .map(|verified| verified.event.transaction_id);
    let transaction_id = resolve_prefix(selector, candidates).map_err(|error| {
        let failure = match error {
            ResolveError::InvalidPrefix => Failure::new(
                ErrorCode::InvalidInput,
                "Supply a transaction ID or unique tx_ prefix from a receipt.",
            ),
            ResolveError::NotFound => Failure::new(
                ErrorCode::UnknownReference,
                "No transaction of this case at or before the read snapshot matches that ID.",
            ),
            ResolveError::Ambiguous(candidates) => {
                let mut failure = Failure::new(
                    ErrorCode::AmbiguousReference,
                    "Several transactions match. Use a full ID.",
                );
                failure.error.actual = Some(json!(candidates));
                failure
            }
        };
        failure.at("--since").with_argv(retry.clone())
    })?;
    let verified = replay
        .events
        .iter()
        .find(|verified| verified.event.transaction_id == transaction_id)
        .expect("resolved transaction is in the replay");
    let before = replay.at_snapshot(&Snapshot {
        sequence: verified.event.sequence,
        head_hash: verified.hash.clone(),
    })?;
    Ok(Since {
        transaction_id,
        before,
    })
}

fn recorded_status(record: &Record) -> Option<&str> {
    record.body["status"]
        .as_str()
        .or_else(|| record.body["lifecycle"].as_str())
        .or_else(|| record.body["disposition"].as_str())
}

fn is_evidence(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Source | Kind::Artifact | Kind::Observation | Kind::Comparison | Kind::Run
    )
}

fn evidentiary_use(state: &State, record: &Record) -> Option<Value> {
    matches!(
        record.header.id.kind(),
        Kind::Source | Kind::Artifact | Kind::Observation
    )
    .then(|| json!(source_ops::evidentiary_use(state, record).state))
}

fn freshness_view(freshness: &RecordFreshness, level: BriefLevel) -> Value {
    let mut view = json!({"effective":freshness.freshness,"label":freshness.label()});
    if level == BriefLevel::Compact {
        view["reason_count"] = json!(freshness.reasons.len());
    } else {
        view["reasons"] = json!(
            freshness
                .reasons
                .iter()
                .take(SHOWN_REASONS)
                .collect::<Vec<_>>()
        );
        let omitted = freshness.reasons.len().saturating_sub(SHOWN_REASONS);
        if omitted > 0 {
            view["reasons_omitted"] = json!(omitted);
        }
    }
    view
}

/// Produce the semantic delta from `since` to the read snapshot of `replay`.
pub(crate) fn delta(
    store: &Store,
    replay: &CaseReplay,
    today: Option<&State>,
    since: &Since,
    level: BriefLevel,
    max_bytes: usize,
    inputs: &BriefInputs,
) -> Result<Value, BriefError> {
    let before = &since.before;
    let start = before.snapshot.sequence;
    let case = replay
        .state
        .records()
        .find(|record| record.header.id.kind() == Kind::Case);
    let case_id = case.map(|record| record.header.id);
    let entry = |record: &Record| {
        let mut entry = views::index_entry(record, replay, today, None);
        if synopsis::is_synopsis(record) {
            // A synopsis's title is its authored text, which a brief never carries.
            entry["title"] = json!("Authored synopsis");
            return entry;
        }
        if level == BriefLevel::Deep && entry.get("content_state").is_none() {
            entry["summary"] = project_record(
                record,
                replay,
                today,
                ViewDepth::Summary,
                &Default::default(),
            )["summary"]
                .clone();
        }
        entry
    };
    let mut changed: Vec<&Record> = replay
        .state
        .records()
        .filter(|record| record.header.acceptance_sequence > start)
        .collect();
    changed.sort_by_key(|record| (record.header.acceptance_sequence, record.header.id));
    let mut listed: BTreeSet<Id> = BTreeSet::new();

    // Freshness from the dependency index at both snapshots.
    let (index_now, index_before) = match case_id {
        Some(case_id) => (
            Some(FreshnessIndex::build(&replay.state, case_id)),
            Some(FreshnessIndex::build(&before.state, case_id)),
        ),
        None => (None, None),
    };
    let mut newly_stale = Vec::new();
    let mut no_longer_stale = Vec::new();
    if let (Some(now), Some(then)) = (&index_now, &index_before) {
        let affected_then: BTreeSet<Id> = then
            .overview()
            .affected
            .iter()
            .map(|reference| reference.id)
            .collect();
        let affected_now: BTreeSet<Id> = now
            .overview()
            .affected
            .iter()
            .map(|reference| reference.id)
            .collect();
        for id in &affected_now {
            let (Some(record), Some(freshness)) = (replay.state.current(*id), now.get(*id)) else {
                continue;
            };
            // Synopses are reported once, in `stale_synopses`.
            if synopsis::is_synopsis(record) {
                continue;
            }
            let was = then
                .get(*id)
                .filter(|_| affected_then.contains(id))
                .map(|freshness| freshness.freshness)
                .unwrap_or(Freshness::Current);
            if was == freshness.freshness {
                continue;
            }
            let mut item = entry(record);
            item["freshness"] = freshness_view(freshness, level);
            item["before"] = json!(was);
            newly_stale.push(item);
            listed.insert(*id);
        }
        for id in affected_then.difference(&affected_now) {
            if let Some(record) = replay.state.current(*id) {
                let mut item = entry(record);
                if let Some(freshness) = now.get(*id) {
                    item["freshness"] =
                        json!({"effective":freshness.freshness,"label":freshness.label()});
                }
                no_longer_stale.push(item);
            }
        }
    }

    // Evidence that is new, or that was withdrawn, retracted or superseded.
    let mut new_evidence = Vec::new();
    let mut retracted_evidence = Vec::new();
    for record in changed
        .iter()
        .copied()
        .filter(|record| is_evidence(record.header.id.kind()))
    {
        let id = record.header.id;
        match before.state.current(id) {
            None => {
                new_evidence.push(entry(record));
                listed.insert(id);
            }
            Some(old) => {
                let status = (recorded_status(old), recorded_status(record));
                let usage = (
                    evidentiary_use(&before.state, old),
                    evidentiary_use(&replay.state, record),
                );
                if status.0 != status.1 || usage.0 != usage.1 {
                    let mut item = entry(record);
                    item["before"] = json!({"status":status.0,"evidentiary_use":usage.0,"revision":old.header.revision.0});
                    item["after"] = json!({"status":status.1,"evidentiary_use":usage.1,"revision":record.header.revision.0});
                    retracted_evidence.push(item);
                    listed.insert(id);
                }
            }
        }
    }

    // Disposition changes. Outcomes compare effective state, which also moves
    // when only their basis changed; other records compare recorded status.
    let mut dispositions = Vec::new();
    for record in replay
        .state
        .records()
        .filter(|record| matches!(record.header.id.kind(), Kind::Conclusion | Kind::Decision))
    {
        let id = record.header.id;
        let now = inputs
            .record_effective
            .get(&id)
            .and_then(|effective| effective.status.clone())
            .map(|status| status["state"].clone())
            .unwrap_or(Value::Null);
        let then = match (before.state.current(id), &index_before) {
            (Some(old), Some(index)) => views::effective_state(store, before, index, old)
                .status
                .map(|status| status["state"].clone())
                .unwrap_or(Value::Null),
            _ => Value::Null,
        };
        if then != now {
            let mut item = entry(record);
            item["before"] = json!({"state":then});
            item["after"] = json!({"state":now});
            if let Some(effective) = inputs.record_effective.get(&id) {
                let unknown = || json!({"state":"unknown"});
                item["material_caveat"] = effective.material_caveat.clone().unwrap_or_else(unknown);
                item["contradiction"] = effective.contradiction.clone().unwrap_or_else(unknown);
            }
            dispositions.push(item);
            listed.insert(id);
        }
    }
    for record in changed.iter().copied().filter(|record| {
        matches!(
            record.header.id.kind(),
            Kind::Case
                | Kind::Question
                | Kind::Hypothesis
                | Kind::Assumption
                | Kind::Assessment
                | Kind::Objection
                | Kind::Inquiry
                | Kind::Review
        )
    }) {
        let old = before.state.current(record.header.id);
        let then = old.and_then(recorded_status);
        let now = recorded_status(record);
        if old.is_none() || then != now {
            let mut item = entry(record);
            item["before"] = old.map_or(Value::Null, |_| json!({"status":then}));
            item["after"] = json!({"status":now});
            dispositions.push(item);
            listed.insert(record.header.id);
        }
    }

    // Gaps emitted now that were not emitted right after the transaction.
    let mut new_gaps = Vec::new();
    if let Some(case_id) = case_id
        && let (Ok(now), Ok(then)) = (
            enumerate_gaps(&replay.state, &replay.snapshot, case_id),
            enumerate_gaps(&before.state, &before.snapshot, case_id),
        )
    {
        let known: BTreeSet<&str> = then.gaps.iter().map(|gap| gap.gap_id.as_str()).collect();
        for gap in now
            .gaps
            .iter()
            .filter(|gap| !known.contains(gap.gap_id.as_str()))
        {
            let mut item = json!({
                "gap_id":gap.gap_id,"rule_id":gap.rule_id,"status":gap.status,
                "subject_refs":gap.subject_refs,"reason":gap.reason,
                "suggested_operation":gap.suggested_operation,
            });
            Caps::default().cap("gap", &mut item, None);
            new_gaps.push(item);
        }
    }

    // Retrospectives and their lessons.
    let mut retrospectives = Vec::new();
    for record in changed
        .iter()
        .copied()
        .filter(|record| record.header.id.kind() == Kind::Retrospective)
    {
        let mut item = entry(record);
        if item.get("content_state").is_none() {
            let lessons = record.body["lessons"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            item["lesson_count"] = json!(lessons.len());
            item["lessons"] = json!(
                lessons
                    .iter()
                    .take(SHOWN_REASONS)
                    .map(|lesson| json!({
                        "guidance":display_title(lesson["guidance"].as_str().unwrap_or(""), 160),
                        "support_refs":lesson["support_refs"],
                    }))
                    .collect::<Vec<_>>()
            );
            Caps::default().cap("lessons", &mut item, None);
        }
        retrospectives.push(item);
        listed.insert(record.header.id);
    }

    // Authored synopses that stopped describing their subject since the
    // transaction: stale now (subject changed or needs review) but current, or
    // absent, then. Text is never part of a brief; the item says where to read.
    let mut stale_synopses = Vec::new();
    if let (Some(now), Some(then)) = (&index_now, &index_before) {
        let classifier = Classifier::new(today.unwrap_or(&replay.state));
        let policy = AudiencePolicy::default();
        for record in replay
            .state
            .records()
            .filter(|record| synopsis::is_synopsis(record))
            .filter(|record| classifier.may_show(record, Field::Content, &policy))
        {
            let Some(status) = synopsis::status(&replay.state, now, record) else {
                continue;
            };
            if status.state != SynopsisState::Stale {
                continue;
            }
            let was = before
                .state
                .current(record.header.id)
                .and_then(|old| synopsis::status(&before.state, then, old))
                .map(|old| old.state);
            if was == Some(SynopsisState::Stale) {
                continue;
            }
            // No title: a synopsis's title is its text, which a brief never carries.
            let mut item = json!({
                "id":record.header.id,"type":record.header.id.kind().to_string(),
                "basis_revision":record.header.revision.0,"label":status.label,
            });
            item["state"] = json!(status.state);
            item["before"] = json!(was);
            item["subject_ref"] = json!(status.subject_ref);
            item["subject_current"] = json!(status.subject_current);
            item["freshness"] = json!({
                "effective":status.freshness,
                "reasons":status.reasons.iter().take(SHOWN_REASONS).collect::<Vec<_>>(),
                "reasons_omitted":status.reasons.len().saturating_sub(SHOWN_REASONS),
            });
            stale_synopses.push(item);
            listed.insert(record.header.id);
        }
    }

    let mut other_changes: BTreeMap<String, usize> = BTreeMap::new();
    for record in changed
        .iter()
        .filter(|record| !listed.contains(&record.header.id))
    {
        *other_changes
            .entry(record.header.id.kind().to_string())
            .or_default() += 1;
    }
    let transactions = replay
        .events
        .iter()
        .filter(|verified| verified.event.sequence > start)
        .count();

    let prefix = &inputs.read_prefix;
    let pin = pin_argv(&replay.snapshot);
    let case_read = case_id.map(|id| {
        let id = id.to_string();
        let mut tail = vec!["show", id.as_str(), "--depth", "full"];
        tail.extend(pin.iter().map(String::as_str));
        follow_up(prefix, &tail)
    });
    let mut caps = Caps::default();
    let mut freshness =
        views::bounded_freshness(inputs.effective.freshness.as_ref(), ViewDepth::Summary);
    caps.cap("freshness", &mut freshness, case_read.clone());
    let mut caveat = inputs
        .effective
        .material_caveat
        .clone()
        .unwrap_or_else(|| json!({"state":"unknown"}));
    caps.cap("material_caveat", &mut caveat, case_read.clone());
    let mut contradiction = inputs
        .effective
        .contradiction
        .clone()
        .unwrap_or_else(|| json!({"state":"unknown"}));
    caps.cap("contradiction", &mut contradiction, case_read);
    let mut output = json!({
        "schema_version":"inv.brief.v0-alpha",
        "ok":true,
        "result":"brief_delta",
        "snapshot":replay.snapshot,
        "level":level.as_str(),
        "historical":inputs.historical.is_some(),
        "since":{"transaction_id":since.transaction_id,"snapshot":before.snapshot},
        "transactions":transactions,
        "changed_records":changed.len(),
        "case":case.map(|record| views::index_entry(record, replay, today, None)),
        "effective_status":inputs.effective.status.clone().unwrap_or_else(|| json!({"state":"unknown"})),
        "freshness":freshness,
        "material_caveat":caveat,
        "contradiction":contradiction,
        "other_changes":other_changes,
        "limits":"Changes are derived from committed records and the freshness and gap rules at both snapshots. Unrecorded developments do not appear.",
        "omitted":caps.into_entries(),
    });
    if let Some(label) = &inputs.historical {
        output["historical_read"] = label.clone();
    }
    let level_name = level.as_str();
    let full_brief = {
        let mut tail = vec!["brief", "--level", level_name];
        tail.extend(pin.iter().map(String::as_str));
        json!({"kind":"local_read","argv":follow_up(prefix, &tail),"snapshot":replay.snapshot})
    };
    let sections = vec![
        Section::new("deeper_read_commands", vec![full_brief]),
        Section::new("newly_stale", newly_stale),
        Section::new("retracted_evidence", retracted_evidence),
        Section::new("disposition_changes", dispositions),
        Section::new("new_gaps", new_gaps),
        Section::new("retrospectives", retrospectives),
        Section::new("stale_synopses", stale_synopses),
        Section::new("new_evidence", new_evidence),
        Section::new("no_longer_stale", no_longer_stale),
    ];
    let since_id = since.transaction_id.to_string();
    fill(output, sections, max_bytes, |bytes| {
        let bytes = bytes.to_string();
        let mut tail = vec![
            "brief",
            "--since",
            since_id.as_str(),
            "--level",
            level_name,
            "--max-bytes",
            bytes.as_str(),
        ];
        tail.extend(pin.iter().map(String::as_str));
        follow_up(prefix, &tail)
    })
}
