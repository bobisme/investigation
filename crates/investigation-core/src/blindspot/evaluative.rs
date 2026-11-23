//! Checks of decision frames, of what still bears on a promoted record, and of
//! case process (§8.5, §15.1). Every condition reads recorded fields only:
//! the engine never converts units, ranks options, reads record text or uses a
//! clock. Process checks count journal transactions, never elapsed time.
//!
//! Overlap with the seed family is deliberate and bounded:
//! `promoted.open_critical_objection` keeps critical objections and
//! `promoted.basis_not_current` keeps changed bases (stale or withdrawn
//! sources, retractions, changed assumption dispositions and scoped membership
//! changes after review, such as a new option or hard criterion). This family
//! adds what those leave out: material objections raised after promotion and
//! material assumptions that were never supported.
//!
//! Seams, not checks: a reopening trigger is required by `decision.propose`,
//! so a Decision without one cannot be recorded; a weight recorded without a
//! range (W10.20) needs the weight fields that bone adds; a stall pattern of
//! the gap rules (W05b.03, `guidance::STALL_GAP_RULES`) would be adopted by a
//! gap-rule check of its own.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{
    command::RevisionRef,
    decision_ops::DecisionBody,
    evaluate::{ComparisonBasis, CriterionLifecycle, DecisionLifecycle},
    evaluate_ops::{ComparisonBody, CriterionBody},
    freshness::dependency_edges,
    identity::{Id, Kind},
    objection_policy::relevant_open_objections,
    retrospective_ops::{check_retrospective, material_revisions},
    review::ObjectionSeverity,
    review_ops::required_read_set,
    state::{Record, State},
    wire::Failure,
};

use super::{AuditContext, CheckSource, CheckSpec, Detection, FindingFact, Severity, is_promoted};

/// Collection transactions in a row, with no consequence between them, that
/// make `case.collection_without_consequence` report. Part of its version.
pub(crate) const STALL_COLLECTION_TRANSACTIONS: usize = 4;

/// The gap rules `criterion.hard_unmeasured` groups by criterion.
const HARD_CELL_MISSING: &str = "decision.hard_criterion_cell_missing";
const HARD_CELL_UNKNOWN: &str = "decision.hard_criterion_cell_unknown";

pub(super) const CHECKS: &[CheckSpec] = &[
    CheckSpec {
        check_id: "criterion.hard_unmeasured",
        version: "v1",
        subject_kinds: &[Kind::Criterion],
        reads: &[
            "gaps of rules decision.hard_criterion_cell_missing and decision.hard_criterion_cell_unknown (slice-gaps.v1)",
            "kind, lifecycle, question_ref and option_refs of each Criterion",
            "option_ref, criterion_ref and outcome of each Comparison",
        ],
        condition: "An active hard Criterion has an applicable eligible Option with no Comparison, or a current Comparison whose outcome is unknown. One finding per Criterion; the facts list the Options without a cell, the unknown Comparisons and the frontier gap IDs.",
        why: "A hard requirement has no evidence-backed disposition for at least one applicable option.",
        severity: Severity::Material,
        severity_reason: "An unassessed hard requirement could rule out an option, including the one chosen.",
        suggested_schema: Some("comparison.add"),
        guide: Some("compare"),
        source: CheckSource::Native(hard_unmeasured),
    },
    CheckSpec {
        check_id: "comparison.unit_mismatch",
        version: "v1",
        subject_kinds: &[Kind::Comparison],
        reads: &[
            "value.unit and retracted of each Comparison",
            "scale.unit and lifecycle of the current revision of its Criterion",
            "comparison_refs of each Decision",
        ],
        condition: "A Comparison that is not retracted records a measured value whose unit differs from the unit the current revision of its non-retired Criterion declares, or the Criterion now declares no unit.",
        why: "A measured value is in a unit other than the one its criterion declares; it is not converted automatically.",
        severity: Severity::Material,
        severity_reason: "A value in another unit cannot be checked against the criterion's scale or threshold as recorded.",
        suggested_schema: Some("comparison.revise"),
        guide: Some("compare"),
        source: CheckSource::Native(unit_mismatch),
    },
    CheckSpec {
        check_id: "decision.preference_without_tradeoff",
        version: "v1",
        subject_kinds: &[Kind::Decision],
        reads: &[
            "lifecycle, comparison_refs and tradeoffs of each Decision",
            "basis and retracted of each cited Comparison revision",
        ],
        condition: "A proposed or accepted Decision cites a Comparison whose basis is a stated preference and records no tradeoff.",
        why: "A choice rests in part on a stated preference and records no tradeoff that discloses it.",
        severity: Severity::Advisory,
        severity_reason: "A preference is legitimate but must be visible as a preference, not read as a measurement.",
        suggested_schema: Some("decision.propose"),
        guide: Some("sensitivity"),
        source: CheckSource::Native(preference_without_tradeoff),
    },
    CheckSpec {
        check_id: "promoted.material_assumption_unsupported",
        version: "v1",
        subject_kinds: &[Kind::Conclusion, Kind::Decision],
        reads: &[
            "Conclusion status and Decision lifecycle or status",
            "the promotion dependency closure and cited references of the promoted record",
            "materiality, status and affected_refs of each Assumption",
        ],
        condition: "A validated Conclusion or accepted Decision cites, or is named by, a material Assumption that is not superseded and whose status is unassessed, challenged or rejected; the same holds for a record of its promotion dependency closure.",
        why: "A record relied on as validated or accepted rests on a material assumption that no recorded assessment supports.",
        severity: Severity::Material,
        severity_reason: "If the assumption is false, the promoted result may not follow from what it cites.",
        suggested_schema: Some("assumption.assess"),
        guide: Some("assumptions"),
        source: CheckSource::Native(material_assumption_unsupported),
    },
    CheckSpec {
        check_id: "promoted.open_material_objection",
        version: "v1",
        subject_kinds: &[Kind::Conclusion, Kind::Decision],
        reads: &[
            "Conclusion status and Decision lifecycle or status, by revision",
            "status, severity, subject_ref and acceptance sequence of each Objection",
            "the promotion dependency closure of the promoted record",
        ],
        condition: "A validated Conclusion or accepted Decision has an open material Objection on itself or on a record of its promotion dependency closure, recorded in a transaction after the one that promoted it. Critical objections are reported by promoted.open_critical_objection.",
        why: "A record relied on as validated or accepted is contested by a material Objection raised after it was promoted.",
        severity: Severity::Material,
        severity_reason: "The promotion did not consider this challenge; reliance on the record should account for it.",
        suggested_schema: Some("objection.resolve"),
        guide: Some("review"),
        source: CheckSource::Native(open_material_objection),
    },
    CheckSpec {
        check_id: "case.collection_without_consequence",
        version: "v1",
        subject_kinds: &[Kind::Case],
        reads: &[
            "acceptance sequence and revision of every record revision in the case",
            "relation of each Assessment, outcome of each Comparison, lifecycle of each Hypothesis and Option, status of each Assumption",
            "lifecycle of the Case",
        ],
        condition: "In an open case, the most recent 4 or more transactions that created an Observation, Run, Source or Artifact have no transaction after or among them that records a consequence: an Assessment that supports or contradicts, a Comparison with a known outcome, any Conclusion or Decision revision, a Hypothesis rejected within scope or retired, an Option ruled out, withdrawn or superseded, or an Assumption assessed. The facts cite each counted transaction by sequence.",
        why: "Evidence keeps being collected while no recorded judgment uses it to separate explanations or move a choice.",
        severity: Severity::Advisory,
        severity_reason: "Collection without consequence may be a stall; stopping with a bounded result is also a valid move.",
        suggested_schema: None,
        guide: Some("stalled"),
        source: CheckSource::Native(collection_without_consequence),
    },
    CheckSpec {
        check_id: "case.material_closure_without_current_retrospective",
        version: "v1",
        subject_kinds: &[Kind::Case],
        reads: &[
            "lifecycle and header.extensions.inv.closure_ready or inv.ready_to_close of the Case",
            "the material-work triggers that make case.close require a Retrospective",
            "each current Question and the accepted Decisions on it",
            "status and snapshot of each Retrospective",
        ],
        condition: "An open case with material work shows a closure signal (declared ready, every current Question has an accepted Decision, or a Retrospective was recorded) and no unsuperseded Retrospective covers its current material work. A declared-ready case with no Retrospective at all is reported by case.ready_to_close_without_retrospective instead.",
        why: "A case with material work is ready for closure and has no Retrospective that covers its current state.",
        severity: Severity::Advisory,
        severity_reason: "What the methods cost and what remains unknown would be lost or out of date at closure.",
        suggested_schema: Some("retrospective.record"),
        guide: Some("retrospective"),
        source: CheckSource::Native(material_closure_without_retrospective),
    },
];

fn decode<T: DeserializeOwned>(record: &Record) -> Option<T> {
    serde_json::from_value(record.body.clone()).ok()
}

fn text<'a>(record: &'a Record, field: &str) -> Option<&'a str> {
    record.body.get(field).and_then(Value::as_str)
}

fn fact(fact: impl Into<String>, refs: Vec<RevisionRef>) -> FindingFact {
    FindingFact {
        fact: fact.into(),
        refs,
    }
}

/// Hard cells without a known outcome, grouped by Criterion from the gap
/// inventory, so the frontier and the audit agree on which cells are open.
fn hard_unmeasured(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    #[derive(Default)]
    struct Cells {
        missing: Vec<RevisionRef>,
        unknown: Vec<RevisionRef>,
        related: Vec<RevisionRef>,
        gaps: Vec<FindingFact>,
    }
    let mut criteria = BTreeMap::<Id, Cells>::new();
    for gap in &context.gaps.gaps {
        let missing = gap.rule_id == HARD_CELL_MISSING;
        if !missing && gap.rule_id != HARD_CELL_UNKNOWN {
            continue;
        }
        let Some(criterion) = gap
            .subject_refs
            .iter()
            .chain(&gap.read_set)
            .find(|reference| reference.id.kind() == Kind::Criterion)
        else {
            continue;
        };
        let cells = criteria.entry(criterion.id).or_default();
        let subjects = gap
            .subject_refs
            .iter()
            .copied()
            .filter(|reference| reference.id.kind() != Kind::Criterion);
        if missing {
            cells.missing.extend(subjects);
        } else {
            cells.unknown.extend(subjects);
        }
        cells.related.extend(gap.read_set.iter().copied());
        cells.gaps.push(fact(
            format!("frontier_gap:{}", gap.gap_id),
            gap.subject_refs.clone(),
        ));
    }
    let mut detections = Vec::new();
    for (id, cells) in criteria {
        let Some(criterion) = context
            .state
            .current(id)
            .filter(|record| record.header.case_id == context.case_id)
        else {
            continue;
        };
        let mut facts = cells.gaps;
        if !cells.missing.is_empty() {
            facts.push(fact("hard_cell:missing", cells.missing.clone()));
        }
        if !cells.unknown.is_empty() {
            facts.push(fact("hard_cell:unknown", cells.unknown.clone()));
        }
        let mut related = cells.related;
        related.extend(cells.missing);
        related.extend(cells.unknown);
        detections.push(Detection {
            subject_refs: vec![criterion.reference()],
            related_refs: related,
            facts,
        });
    }
    Ok(detections)
}

/// Current Decisions that are not superseded and cite `comparison` by ID.
fn citing_decisions(context: &AuditContext<'_>, comparison: Id) -> Vec<RevisionRef> {
    context
        .current(Kind::Decision)
        .into_iter()
        .filter(|record| {
            decode::<DecisionBody>(record).is_some_and(|decision| {
                decision.lifecycle != DecisionLifecycle::Superseded
                    && decision
                        .comparison_refs
                        .iter()
                        .any(|reference| reference.id == comparison)
            })
        })
        .map(Record::reference)
        .collect()
}

fn unit_mismatch(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let mut detections = Vec::new();
    for record in context.current(Kind::Comparison) {
        let Some(comparison) = decode::<ComparisonBody>(record) else {
            continue;
        };
        let Some(value) = comparison.value.as_ref().filter(|_| !comparison.retracted) else {
            continue;
        };
        let Some(criterion) = context
            .state
            .current(comparison.criterion_ref.id)
            .filter(|criterion| criterion.header.case_id == context.case_id)
        else {
            continue;
        };
        let Some(declared) = decode::<CriterionBody>(criterion) else {
            continue;
        };
        if declared.lifecycle == CriterionLifecycle::Retired
            || declared.scale.unit.as_deref() == Some(value.unit.as_str())
        {
            continue;
        }
        let subject = record.reference();
        let current = criterion.reference();
        let mut facts = vec![fact("unit_mismatch", vec![subject, current])];
        if declared.scale.unit.is_none() {
            facts.push(fact("criterion_unit:undeclared", vec![current]));
        }
        if comparison.criterion_ref != current {
            facts.push(fact(
                "criterion_revised_since_comparison",
                vec![comparison.criterion_ref, current],
            ));
        }
        let decisions = citing_decisions(context, record.header.id);
        if !decisions.is_empty() {
            facts.push(fact("cited_by_decision", decisions.clone()));
        }
        let mut related = vec![comparison.criterion_ref, current];
        related.extend(decisions);
        detections.push(Detection {
            subject_refs: vec![subject],
            related_refs: related,
            facts,
        });
    }
    Ok(detections)
}

fn preference_without_tradeoff(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let mut detections = Vec::new();
    for record in context.current(Kind::Decision) {
        let Some(decision) = decode::<DecisionBody>(record) else {
            continue;
        };
        if decision.lifecycle == DecisionLifecycle::Superseded
            || decision
                .tradeoffs
                .iter()
                .any(|item| !item.trim().is_empty())
        {
            continue;
        }
        let preferences: Vec<RevisionRef> = decision
            .comparison_refs
            .iter()
            .copied()
            .filter(|reference| {
                context
                    .state
                    .revision(*reference)
                    .and_then(decode::<ComparisonBody>)
                    .is_some_and(|comparison| {
                        !comparison.retracted
                            && matches!(comparison.basis, Some(ComparisonBasis::Preference { .. }))
                    })
            })
            .collect();
        if preferences.is_empty() {
            continue;
        }
        detections.push(Detection {
            subject_refs: vec![record.reference()],
            related_refs: preferences.clone(),
            facts: vec![
                fact("comparison_basis:preference", preferences),
                fact("tradeoffs:none", vec![record.reference()]),
            ],
        });
    }
    Ok(detections)
}

/// The promoted record and the IDs it relies on: its promotion dependency
/// closure plus every record it cites directly.
fn reliance(state: &State, record: &Record) -> BTreeSet<Id> {
    let mut ids: BTreeSet<Id> = [record.header.id].into();
    ids.extend(
        required_read_set(state, record)
            .unwrap_or_default()
            .into_iter()
            .map(|reference| reference.id),
    );
    ids.extend(
        dependency_edges(record)
            .unwrap_or_default()
            .into_iter()
            .map(|edge| edge.target.id),
    );
    ids
}

fn material_assumption_unsupported(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let assumptions: Vec<(&Record, &str, Vec<Id>)> = context
        .current(Kind::Assumption)
        .into_iter()
        .filter(|record| text(record, "materiality") == Some("material"))
        .filter_map(|record| {
            let status = text(record, "status")?;
            if matches!(status, "supported" | "superseded") {
                return None;
            }
            let affected = record
                .body
                .get("affected_refs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|value| serde_json::from_value::<RevisionRef>(value.clone()).ok())
                .map(|reference| reference.id)
                .collect();
            Some((record, status, affected))
        })
        .collect();
    if assumptions.is_empty() {
        return Ok(Vec::new());
    }
    let mut detections = Vec::new();
    for record in context.promoted() {
        let relied = reliance(context.state, record);
        let mut by_status = BTreeMap::<&str, Vec<RevisionRef>>::new();
        for (assumption, status, affected) in &assumptions {
            if relied.contains(&assumption.header.id)
                || affected.iter().any(|id| relied.contains(id))
            {
                by_status
                    .entry(status)
                    .or_default()
                    .push(assumption.reference());
            }
        }
        if by_status.is_empty() {
            continue;
        }
        let related = by_status.values().flatten().copied().collect();
        let facts = by_status
            .into_iter()
            .map(|(status, refs)| fact(format!("assumption_status:{status}"), refs))
            .collect();
        detections.push(Detection {
            subject_refs: vec![record.reference()],
            related_refs: related,
            facts,
        });
    }
    Ok(detections)
}

/// The revision that began the record's current promoted run: the earliest
/// revision after which every revision up to the current one is promoted.
fn promoting_revision<'a>(state: &'a State, record: &Record) -> Option<&'a Record> {
    let mut start = None;
    for revision in state.history(record.header.id) {
        if is_promoted(revision) {
            start.get_or_insert(revision);
        } else {
            start = None;
        }
    }
    start
}

fn created_at(state: &State, id: Id) -> Option<u64> {
    state
        .history(id)
        .next()
        .map(|record| record.header.acceptance_sequence)
}

fn open_material_objection(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let mut detections = Vec::new();
    for record in context.promoted() {
        let Some(promoted) = promoting_revision(context.state, record) else {
            continue;
        };
        let since = promoted.header.acceptance_sequence;
        let later: Vec<_> = relevant_open_objections(context.state, record)?
            .into_iter()
            .filter(|objection| objection.severity == ObjectionSeverity::Material)
            .filter(|objection| {
                created_at(context.state, objection.objection_ref.id).is_some_and(|at| at > since)
            })
            .collect();
        if later.is_empty() {
            continue;
        }
        let objections: Vec<_> = later
            .iter()
            .map(|objection| objection.objection_ref)
            .collect();
        let disputed: Vec<_> = later
            .iter()
            .map(|objection| objection.subject_ref)
            .collect();
        detections.push(Detection {
            subject_refs: vec![record.reference()],
            related_refs: objections
                .iter()
                .chain(&disputed)
                .copied()
                .chain([promoted.reference()])
                .collect(),
            facts: vec![
                fact("open_objection:material", objections),
                fact("objection_subject", disputed),
                fact(
                    format!("promoted_at_transaction:{since}"),
                    vec![promoted.reference()],
                ),
            ],
        });
    }
    Ok(detections)
}

/// Whether one revision records a consequence of collected evidence: a
/// discriminating assessment, a known comparison outcome, a conclusion or
/// decision, a hypothesis rejected or retired, an option ruled out, withdrawn
/// or superseded, or an assumption assessed. Activating a hypothesis or making
/// an option eligible is not a consequence of evidence.
fn records_consequence(revision: &Record) -> bool {
    match revision.header.id.kind() {
        Kind::Assessment => matches!(text(revision, "relation"), Some("supports" | "contradicts")),
        Kind::Comparison => text(revision, "outcome").is_some_and(|outcome| outcome != "unknown"),
        Kind::Conclusion | Kind::Decision => true,
        Kind::Hypothesis => {
            revision.header.revision.0 > 1
                && matches!(
                    text(revision, "lifecycle"),
                    Some("rejected_within_scope" | "retired")
                )
        }
        Kind::Option => {
            revision.header.revision.0 > 1
                && matches!(
                    text(revision, "lifecycle"),
                    Some("ruled_out" | "withdrawn" | "superseded")
                )
        }
        Kind::Assumption => {
            revision.header.revision.0 > 1
                && matches!(
                    text(revision, "status"),
                    Some("supported" | "challenged" | "rejected")
                )
        }
        _ => false,
    }
}

fn collects_evidence(revision: &Record) -> bool {
    revision.header.revision.0 == 1
        && matches!(
            revision.header.id.kind(),
            Kind::Observation | Kind::Run | Kind::Source | Kind::Artifact
        )
}

fn collection_without_consequence(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let Some(case) = context.state.current(context.case_id) else {
        return Ok(Vec::new());
    };
    if text(case, "lifecycle") == Some("closed") {
        return Ok(Vec::new());
    }
    #[derive(Default)]
    struct Transaction {
        collected: Vec<RevisionRef>,
        consequences: Vec<RevisionRef>,
    }
    let mut transactions = BTreeMap::<u64, Transaction>::new();
    for record in context
        .state
        .records()
        .filter(|record| record.header.case_id == context.case_id)
    {
        for revision in context.state.history(record.header.id) {
            let collected = collects_evidence(revision);
            let consequence = records_consequence(revision);
            if !collected && !consequence {
                continue;
            }
            let transaction = transactions
                .entry(revision.header.acceptance_sequence)
                .or_default();
            if collected {
                transaction.collected.push(revision.reference());
            }
            if consequence {
                transaction.consequences.push(revision.reference());
            }
        }
    }
    let mut counted = Vec::new();
    let mut last = None;
    for (sequence, transaction) in transactions.iter().rev() {
        if !transaction.consequences.is_empty() {
            last = Some((*sequence, transaction.consequences.clone()));
            break;
        }
        counted.push((*sequence, transaction.collected.clone()));
    }
    if counted.len() < STALL_COLLECTION_TRANSACTIONS {
        return Ok(Vec::new());
    }
    counted.reverse();
    let mut facts = vec![
        fact(
            format!("collection_transactions_counted:{}", counted.len()),
            Vec::new(),
        ),
        fact(
            format!("stall_threshold:{STALL_COLLECTION_TRANSACTIONS}"),
            Vec::new(),
        ),
        match last {
            Some((sequence, refs)) => {
                fact(format!("last_consequence_transaction:{sequence}"), refs)
            }
            None => fact("last_consequence_transaction:none", Vec::new()),
        },
    ];
    let mut related = Vec::new();
    for (sequence, collected) in counted {
        related.extend(collected.iter().copied());
        facts.push(fact(
            format!("collection_transaction:{sequence}"),
            collected,
        ));
    }
    Ok(vec![Detection {
        subject_refs: vec![case.reference()],
        related_refs: related,
        facts,
    }])
}

fn material_closure_without_retrospective(
    context: &AuditContext<'_>,
) -> Result<Vec<Detection>, Failure> {
    let Some(case) = context.state.current(context.case_id) else {
        return Ok(Vec::new());
    };
    if text(case, "lifecycle") == Some("closed")
        || context
            .gaps
            .gaps
            .iter()
            .any(|gap| gap.rule_id == "case.ready_to_close_without_retrospective")
    {
        return Ok(Vec::new());
    }
    let material = material_revisions(context.state, context.case_id);
    if material.is_empty() {
        return Ok(Vec::new());
    }
    let case_ref = case.reference();
    let mut signals = Vec::new();
    let declared = ["inv.closure_ready", "inv.ready_to_close"]
        .iter()
        .any(|key| case.header.extensions.get(*key).and_then(Value::as_bool) == Some(true));
    if declared {
        signals.push(fact("closure_signal:declared_ready", vec![case_ref]));
    }
    let questions: Vec<&Record> = context
        .current(Kind::Question)
        .into_iter()
        .filter(|question| {
            !["lifecycle", "status"].iter().any(|field| {
                matches!(
                    text(question, field),
                    Some("retired" | "withdrawn" | "superseded")
                )
            })
        })
        .collect();
    let accepted: BTreeMap<Id, Vec<RevisionRef>> = context
        .current(Kind::Decision)
        .into_iter()
        .filter(|decision| is_promoted(decision))
        .filter_map(|decision| {
            let body = decode::<DecisionBody>(decision)?;
            Some((body.question_ref.id, decision.reference()))
        })
        .fold(BTreeMap::new(), |mut map, (question, decision)| {
            map.entry(question).or_insert_with(Vec::new).push(decision);
            map
        });
    if !questions.is_empty()
        && questions
            .iter()
            .all(|question| accepted.contains_key(&question.header.id))
    {
        signals.push(fact(
            "closure_signal:all_questions_decided",
            accepted.values().flatten().copied().collect(),
        ));
    }
    let retrospectives: Vec<RevisionRef> = context
        .current(Kind::Retrospective)
        .into_iter()
        .filter(|record| text(record, "status") != Some("superseded"))
        .map(Record::reference)
        .collect();
    if !retrospectives.is_empty() {
        signals.push(fact(
            "closure_signal:retrospective_recorded",
            retrospectives.clone(),
        ));
    }
    if signals.is_empty() {
        return Ok(Vec::new());
    }
    let mut changed = Vec::new();
    for retrospective in &retrospectives {
        match check_retrospective(context.state, context.case_id, *retrospective) {
            Ok(()) => return Ok(Vec::new()),
            Err(failure) => changed.extend(
                failure
                    .error
                    .actual
                    .and_then(|actual| serde_json::from_value::<Vec<RevisionRef>>(actual).ok())
                    .unwrap_or_default(),
            ),
        }
    }
    let mut facts = signals;
    facts.push(fact("material_work", material.clone()));
    if retrospectives.is_empty() {
        facts.push(fact("retrospective:none", Vec::new()));
    } else {
        facts.push(fact("retrospective:not_current", retrospectives.clone()));
        if !changed.is_empty() {
            facts.push(fact("changed_after_retrospective", changed.clone()));
        }
    }
    let mut related = material;
    related.extend(retrospectives);
    related.extend(changed);
    Ok(vec![Detection {
        subject_refs: vec![case_ref],
        related_refs: related,
        facts,
    }])
}

#[cfg(test)]
mod tests;
