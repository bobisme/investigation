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
//! so a Decision without one cannot be recorded. The collection
//! streak and what counts as a consequence are defined once in
//! [`crate::stall`] and shared with the stall gap rules
//! (`guidance::STALL_GAP_RULES`); the other stall patterns are not yet adopted
//! by a gap-rule check of their own.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{
    command::RevisionRef,
    decision_ops::DecisionBody,
    evaluate::{
        ComparisonBasis, ComparisonOutcome, CriterionKind, CriterionLifecycle, DecisionLifecycle,
        OptionLifecycle,
    },
    evaluate_ops::{ComparisonBody, CriterionBody, OptionBody},
    freshness::{Trigger, dependency_edges, successor_change},
    identity::{Id, Kind},
    objection_policy::relevant_open_objections,
    retrospective_ops::{check_retrospective, material_revisions},
    review::ObjectionSeverity,
    review_ops::required_read_set,
    stall::{self, Journal},
    state::{Record, State},
    weighting::WeightStatus,
    wire::Failure,
};

use super::{
    AuditContext, CheckSource, CheckSpec, Detection, FindingFact, Severity, is_promoted,
    seed::trigger_name,
};

/// Collection transactions in a row, with no consequence between them, that
/// make `case.collection_without_consequence` report. Part of its version.
pub(crate) use crate::stall::STALL_COLLECTION_TRANSACTIONS;

/// The gap rules `criterion.hard_unmeasured` groups by criterion.
const HARD_CELL_MISSING: &str = "decision.hard_criterion_cell_missing";
const HARD_CELL_UNKNOWN: &str = "decision.hard_criterion_cell_unknown";
/// A Comparison citing an earlier Option or Criterion revision. The rule never
/// emits the unknown-cell rule for the same Comparison, so the check judges
/// these cells itself.
const STALE_COMPARISON: &str = "comparison.stale_dependency_revision";

pub(super) const CHECKS: &[CheckSpec] = &[
    CheckSpec {
        check_id: "criterion.hard_unmeasured",
        version: "v2",
        subject_kinds: &[Kind::Criterion],
        reads: &[
            "gaps of rules decision.hard_criterion_cell_missing, decision.hard_criterion_cell_unknown and comparison.stale_dependency_revision (slice-gaps.v2)",
            "kind, lifecycle, question_ref and option_refs of each Criterion, and every revision since the one a Comparison cites",
            "question_ref and lifecycle of each Option",
            "option_ref, criterion_ref, outcome and retracted of each Comparison",
        ],
        condition: "An active hard Criterion has an applicable eligible Option with no Comparison, with a Comparison whose outcome is unknown, or whose only Comparisons with a known outcome cite an earlier revision of the Criterion that a later revision changed for its readers (the shared freshness predicate, for example a changed threshold). A later revision that is benign to a reader, such as a restatement without change, keeps the cell measured. One finding per Criterion; the facts list the Options without a cell, the unknown Comparisons, the Comparisons that cite a superseded Criterion revision with the freshness trigger, and the frontier gap IDs.",
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
        check_id: "criterion.weight_without_range",
        version: "v1",
        subject_kinds: &[Kind::Criterion],
        reads: &["lifecycle, question_ref and weight.status of each Criterion"],
        condition: "An active Criterion records a weight with status stated_preference: no worst and best values it trades off. The facts name the other active Criteria of the same question whose weight has a range.",
        why: "A weight is recorded without the range it trades off, so it is kept as a stated preference and never enters a weighted score.",
        severity: Severity::Advisory,
        severity_reason: "A weight without a range has no meaning for a score; the preference stays visible but does not rank options.",
        suggested_schema: Some("criterion.amend"),
        guide: Some("sensitivity"),
        source: CheckSource::Native(weight_without_range),
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
///
/// A Comparison that cites an earlier Criterion revision is a
/// `comparison.stale_dependency_revision` gap, never an unknown-cell gap. Its
/// cell is unmeasured for the current Criterion when the shared freshness
/// predicate ([`successor_change`]) finds the later revision changed it for a
/// reader (a threshold, unit or kind change, for example) and no other
/// Comparison of the same Option measures it; a benign later revision keeps
/// the recorded outcome, so only an unknown one is reported.
fn hard_unmeasured(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    #[derive(Default)]
    struct Cells {
        missing: Vec<RevisionRef>,
        unknown: Vec<RevisionRef>,
        superseded: Vec<RevisionRef>,
        triggers: BTreeMap<String, Vec<RevisionRef>>,
        related: Vec<RevisionRef>,
        gaps: Vec<FindingFact>,
    }
    let mut criteria = BTreeMap::<Id, Cells>::new();
    let mut measured = None;
    for gap in &context.gaps.gaps {
        let cells = if gap.rule_id == STALE_COMPARISON {
            let measured = measured.get_or_insert_with(|| measured_cells(context));
            let Some(cell) = stale_hard_cell(context, gap, measured) else {
                continue;
            };
            let cells = criteria.entry(cell.criterion.id).or_default();
            match cell.superseded {
                Some((cited, trigger)) => {
                    cells.superseded.push(cell.comparison);
                    cells
                        .triggers
                        .entry(format!("freshness_trigger:{}", trigger_name(&trigger)))
                        .or_default()
                        .extend([cited, cell.criterion]);
                }
                None => cells.unknown.push(cell.comparison),
            }
            cells.related.push(cell.criterion);
            cells
        } else {
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
            cells
        };
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
        if !cells.superseded.is_empty() {
            facts.push(fact(
                "hard_cell:criterion_superseded",
                cells.superseded.clone(),
            ));
        }
        let mut related = cells.related;
        for (trigger, mut refs) in cells.triggers {
            refs.sort_unstable();
            refs.dedup();
            related.extend(refs.iter().copied());
            facts.push(fact(trigger, refs));
        }
        related.extend(cells.missing);
        related.extend(cells.unknown);
        related.extend(cells.superseded);
        detections.push(Detection {
            subject_refs: vec![criterion.reference()],
            related_refs: related,
            facts,
        });
    }
    Ok(detections)
}

/// A hard cell behind a `comparison.stale_dependency_revision` gap that has
/// no evidence-backed disposition for the current Criterion.
struct StaleCell {
    /// The current revision of the active hard Criterion.
    criterion: RevisionRef,
    comparison: RevisionRef,
    /// The cited Criterion revision and the trigger its later revision raises,
    /// or `None` when every later revision is benign and the outcome unknown.
    superseded: Option<(RevisionRef, Trigger)>,
}

/// The current same-case revision of an active hard Criterion.
fn active_hard<'a>(context: &AuditContext<'a>, id: Id) -> Option<(&'a Record, CriterionBody)> {
    let record = context
        .state
        .current(id)
        .filter(|record| record.header.case_id == context.case_id)?;
    decode::<CriterionBody>(record)
        .filter(|body| {
            body.kind == CriterionKind::Hard && body.lifecycle == CriterionLifecycle::Active
        })
        .map(|body| (record, body))
}

/// The Criterion revision a Comparison cites is still the one a reader of the
/// current Criterion would apply: it is current, or every later revision is
/// benign to it.
fn criterion_unchanged(context: &AuditContext<'_>, criterion: &Record, cited: RevisionRef) -> bool {
    criterion.reference() == cited
        || successor_change(context.state, context.case_id, cited).is_none()
}

/// Option and Criterion ID pairs with a Comparison that has a known outcome
/// against an unchanged Criterion revision.
fn measured_cells(context: &AuditContext<'_>) -> BTreeSet<(Id, Id)> {
    context
        .current(Kind::Comparison)
        .into_iter()
        .filter_map(decode::<ComparisonBody>)
        .filter(|comparison| {
            !comparison.retracted
                && comparison.outcome != ComparisonOutcome::Unknown
                && context
                    .state
                    .current(comparison.criterion_ref.id)
                    .is_some_and(|criterion| {
                        criterion_unchanged(context, criterion, comparison.criterion_ref)
                    })
        })
        .map(|comparison| (comparison.option_ref.id, comparison.criterion_ref.id))
        .collect()
}

/// Judges one stale-dependency gap as a hard cell: the cited Criterion
/// revision is materially superseded and no other Comparison measures the
/// cell, or the Criterion is unchanged for a reader and the outcome unknown.
/// The Option must still apply to the Criterion as the missing-cell rule reads
/// it: same question, in scope, and not ruled out, withdrawn or superseded.
fn stale_hard_cell(
    context: &AuditContext<'_>,
    gap: &crate::gap_rules::Gap,
    measured: &BTreeSet<(Id, Id)>,
) -> Option<StaleCell> {
    let comparison_ref = *gap.subject_refs.first()?;
    let comparison = decode::<ComparisonBody>(context.state.revision(comparison_ref)?)?;
    if comparison.retracted {
        return None;
    }
    let (criterion, body) = active_hard(context, comparison.criterion_ref.id)?;
    let option = context
        .state
        .current(comparison.option_ref.id)
        .filter(|record| record.header.case_id == context.case_id)
        .and_then(decode::<OptionBody>)?;
    if option.question_ref.id != body.question_ref.id
        || matches!(
            option.lifecycle,
            OptionLifecycle::RuledOut | OptionLifecycle::Withdrawn | OptionLifecycle::Superseded
        )
        || body.option_refs.as_ref().is_some_and(|scope| {
            !scope
                .iter()
                .any(|reference| reference.id == comparison.option_ref.id)
        })
    {
        return None;
    }
    let superseded = if criterion_unchanged(context, criterion, comparison.criterion_ref) {
        if comparison.outcome != ComparisonOutcome::Unknown {
            return None;
        }
        None
    } else {
        if measured.contains(&(comparison.option_ref.id, criterion.header.id)) {
            return None;
        }
        let trigger = successor_change(context.state, context.case_id, comparison.criterion_ref)?;
        Some((comparison.criterion_ref, trigger))
    };
    Some(StaleCell {
        criterion: criterion.reference(),
        comparison: comparison_ref,
        superseded,
    })
}

/// Active weighted Criteria whose weight has no stated range (§15.1).
fn weight_without_range(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let active: Vec<(RevisionRef, CriterionBody)> = context
        .current(Kind::Criterion)
        .into_iter()
        .filter_map(|record| {
            decode::<CriterionBody>(record)
                .filter(|criterion| criterion.lifecycle == CriterionLifecycle::Active)
                .map(|criterion| (record.reference(), criterion))
        })
        .collect();
    let status = |criterion: &CriterionBody| criterion.weight.as_ref().map(|weight| weight.status);
    let mut detections = Vec::new();
    for (subject, criterion) in &active {
        if status(criterion) != Some(WeightStatus::StatedPreference) {
            continue;
        }
        let ranged: Vec<RevisionRef> = active
            .iter()
            .filter(|(_, other)| {
                other.question_ref.id == criterion.question_ref.id
                    && status(other) == Some(WeightStatus::Ranged)
            })
            .map(|(reference, _)| *reference)
            .collect();
        let mut facts = vec![fact("weight:stated_preference", vec![*subject])];
        if !ranged.is_empty() {
            facts.push(fact("question_ranged_weights", ranged.clone()));
        }
        detections.push(Detection {
            subject_refs: vec![*subject],
            related_refs: ranged,
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

/// The streak and the consequence definition are shared with the
/// `case.collection_without_consequence` gap rule (see [`crate::stall`]).
fn collection_without_consequence(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let Some(case) = context.state.current(context.case_id) else {
        return Ok(Vec::new());
    };
    if text(case, "lifecycle") == Some("closed") {
        return Ok(Vec::new());
    }
    let journal = Journal::build(context.state, context.case_id);
    let Some(streak) = stall::collection_without_consequence(&journal) else {
        return Ok(Vec::new());
    };
    let mut facts = vec![
        fact(
            format!("collection_transactions_counted:{}", streak.counted.len()),
            Vec::new(),
        ),
        fact(
            format!("stall_threshold:{STALL_COLLECTION_TRANSACTIONS}"),
            Vec::new(),
        ),
        match streak.last_consequence {
            Some((sequence, refs)) => {
                fact(format!("last_consequence_transaction:{sequence}"), refs)
            }
            None => fact("last_consequence_transaction:none", Vec::new()),
        },
    ];
    let mut related = Vec::new();
    for (sequence, collected) in streak.counted {
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
