//! Decisions retain their reviewed basis; acceptance never implies implementation success.
use crate::{
    basis::{MembershipRule, MembershipScope, membership},
    catalog::{OperationDescriptor, OperationInfo},
    command::{Reference, RevisionRef},
    evaluate::{
        ComparisonOutcome, CriterionKind, CriterionLifecycle, DecisionLifecycle, OptionLifecycle,
        PreferredDirection,
    },
    evaluate_ops::{ComparisonBody, CriterionBody, OptionBody},
    evidence::{
        ArtifactAvailability, ArtifactBody, ArtifactIntegrity, MeasurementValidity,
        ObservationBasis, ObservationBody, ObservationValue, SelectorValidation,
    },
    freshness::FreshnessIndex,
    identity::{Id, Kind},
    objection_policy::{
        AcceptanceChallenges, ObjectionAcknowledgmentInput, acknowledge, acknowledgment_policy,
        relevant_open_objections,
    },
    review::ValidationVerdict,
    review_ops::{ValidationReviewBody, validation_review_current},
    state::{OperationContext, Record, State},
    wire::{ErrorCode, Failure},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChoiceInput {
    Select { option_ref: Reference },
    Defer { reason: String },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Choice {
    Select { option_ref: RevisionRef },
    Defer { reason: String },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionInput {
    pub criterion_ref: Reference,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HardCondition {
    pub criterion_ref: RevisionRef,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReversalInput {
    pub assumption_ref: Option<Reference>,
    pub criterion_ref: Option<Reference>,
    pub comparison_ref: Option<Reference>,
    pub option_ref: Reference,
    pub condition: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReversalCondition {
    pub assumption_ref: Option<RevisionRef>,
    pub criterion_ref: Option<RevisionRef>,
    pub comparison_ref: Option<RevisionRef>,
    pub option_ref: RevisionRef,
    pub condition: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionInput {
    pub question_ref: Reference,
    pub choice: ChoiceInput,
    pub compared_alternatives: Vec<Reference>,
    pub comparison_refs: Vec<Reference>,
    pub hard_conditions: Vec<ConditionInput>,
    pub evidence_refs: Vec<Reference>,
    pub assumption_refs: Vec<Reference>,
    pub tradeoffs: Vec<String>,
    pub consequences: Vec<String>,
    pub reopen_trigger: String,
    pub expected_outcome: Option<String>,
    pub check_when: Option<String>,
    pub reversal_conditions: Vec<ReversalInput>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionBody {
    pub question_ref: RevisionRef,
    pub choice: Choice,
    pub compared_alternatives: Vec<RevisionRef>,
    pub criteria_set_version: String,
    pub comparison_refs: Vec<RevisionRef>,
    pub hard_conditions: Vec<HardCondition>,
    pub evidence_refs: Vec<RevisionRef>,
    pub assumption_refs: Vec<RevisionRef>,
    pub tradeoffs: Vec<String>,
    pub consequences: Vec<String>,
    pub reopen_trigger: String,
    pub expected_outcome: Option<String>,
    pub check_when: Option<String>,
    pub reversal_conditions: Vec<ReversalCondition>,
    pub lifecycle: DecisionLifecycle,
    pub review_ref: Option<RevisionRef>,
    pub supersedes: Option<RevisionRef>,
    pub superseded_by: Option<RevisionRef>,
    pub change_reason: Option<String>,
    /// Open objections and uncertainty the decision was accepted over (§14.3).
    /// Written once at acceptance; absent on proposals and older records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_challenges: Option<AcceptanceChallenges>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionAccept {
    pub decision_ref: RevisionRef,
    pub review_ref: Reference,
    /// Attributed acknowledgments of open objections on the decision or its
    /// basis. The case policy decides which objections require one.
    #[serde(default)]
    pub acknowledgments: Vec<ObjectionAcknowledgmentInput>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionSupersede {
    pub decision_ref: RevisionRef,
    pub reason: String,
    pub replacement: DecisionInput,
}

fn fail(path: &str, msg: &str) -> Failure {
    Failure::new(ErrorCode::GateFailed, msg).at(path)
}
fn text(s: &str, path: &str) -> Result<(), Failure> {
    if s.trim().is_empty() || s.chars().count() > 2048 {
        Err(fail(path, "Supply nonempty bounded text."))
    } else {
        Ok(())
    }
}
fn body<T: Serialize>(v: T) -> Result<Value, Failure> {
    serde_json::to_value(v)
        .map_err(|_| Failure::new(ErrorCode::IntegrityFailure, "Cannot encode decision."))
}
fn decode<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T, Failure> {
    serde_json::from_value(v.clone()).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Invalid canonical decision dependency.",
        )
    })
}
fn exact<'a>(
    s: &'a State,
    case: Id,
    r: RevisionRef,
    kind: Kind,
    path: &str,
) -> Result<&'a Record, Failure> {
    if r.id.kind() != kind {
        return Err(fail(path, "Unexpected record kind."));
    }
    let found = s
        .current(r.id)
        .filter(|v| v.header.case_id == case)
        .ok_or_else(|| fail(path, "Reference is not in this case."))?;
    if found.reference() != r {
        let mut e = Failure::new(ErrorCode::RevisionConflict, "Reference is stale.").at(path);
        e.error.actual = Some(json!(found.reference()));
        return Err(e);
    }
    Ok(found)
}
fn pin(
    c: &OperationContext<'_>,
    r: &Reference,
    kind: Kind,
    path: &str,
) -> Result<RevisionRef, Failure> {
    let r = c.resolve(r, kind, path)?;
    exact(c.state(), c.case_id().unwrap(), r, kind, path)?;
    Ok(r)
}
fn pin_any(
    c: &OperationContext<'_>,
    r: &Reference,
    kinds: &[Kind],
    path: &str,
) -> Result<RevisionRef, Failure> {
    let r = c.resolve_reference(r, path)?;
    if !kinds.contains(&r.id.kind()) {
        return Err(fail(path, "Unexpected reference kind."));
    }
    exact(c.state(), c.case_id().unwrap(), r, r.id.kind(), path)?;
    Ok(r)
}
fn normalize(c: &OperationContext<'_>, p: DecisionInput) -> Result<DecisionBody, Failure> {
    text(&p.reopen_trigger, "/reopen_trigger")?;
    if p.consequences.is_empty() {
        return Err(fail(
            "/consequences",
            "State the consequences of the choice.",
        ));
    }
    for x in p.consequences.iter().chain(&p.tradeoffs) {
        text(x, "/consequences")?;
    }
    let question_ref = pin(c, &p.question_ref, Kind::Question, "/question_ref")?;
    let alternatives: Vec<_> = p
        .compared_alternatives
        .iter()
        .map(|r| pin(c, r, Kind::Option, "/compared_alternatives"))
        .collect::<Result<_, _>>()?;
    if alternatives
        .iter()
        .map(|r| r.id)
        .collect::<BTreeSet<_>>()
        .len()
        != alternatives.len()
    {
        return Err(fail("/compared_alternatives", "Duplicate option."));
    }
    for r in &alternatives {
        let o: OptionBody = decode(
            &exact(
                c.state(),
                c.case_id().unwrap(),
                *r,
                Kind::Option,
                "/compared_alternatives",
            )?
            .body,
        )?;
        if o.question_ref.id != question_ref.id {
            return Err(fail(
                "/compared_alternatives",
                "Option belongs to another question.",
            ));
        }
    }
    let choice = match p.choice {
        ChoiceInput::Select { option_ref } => {
            let r = pin(c, &option_ref, Kind::Option, "/choice/option_ref")?;
            if !alternatives.contains(&r) {
                return Err(fail(
                    "/choice",
                    "Selected option must be among alternatives.",
                ));
            }
            Choice::Select { option_ref: r }
        }
        ChoiceInput::Defer { reason } => {
            text(&reason, "/choice/reason")?;
            Choice::Defer { reason }
        }
    };
    let mut comparison_refs = p
        .comparison_refs
        .iter()
        .map(|r| pin(c, r, Kind::Comparison, "/comparison_refs"))
        .collect::<Result<Vec<_>, _>>()?;
    let mut hard_conditions = Vec::new();
    for x in p.hard_conditions {
        text(&x.reason, "/hard_conditions/reason")?;
        let criterion_ref = pin(
            c,
            &x.criterion_ref,
            Kind::Criterion,
            "/hard_conditions/criterion_ref",
        )?;
        if hard_conditions
            .iter()
            .any(|h: &HardCondition| h.criterion_ref.id == criterion_ref.id)
        {
            return Err(fail("/hard_conditions", "Duplicate condition."));
        }
        hard_conditions.push(HardCondition {
            criterion_ref,
            reason: x.reason,
        });
    }
    let evidence_refs = p
        .evidence_refs
        .iter()
        .map(|r| {
            pin_any(
                c,
                r,
                &[
                    Kind::Observation,
                    Kind::Source,
                    Kind::Artifact,
                    Kind::Assessment,
                    Kind::Comparison,
                    Kind::Run,
                ],
                "/evidence_refs",
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut assumption_refs = p
        .assumption_refs
        .iter()
        .map(|r| pin(c, r, Kind::Assumption, "/assumption_refs"))
        .collect::<Result<Vec<_>, _>>()?;
    let mut reversal_conditions = Vec::new();
    for r in p.reversal_conditions {
        text(&r.condition, "/reversal_conditions/condition")?;
        if r.assumption_ref.is_none() && r.criterion_ref.is_none() && r.comparison_ref.is_none() {
            return Err(fail(
                "/reversal_conditions",
                "Link an assumption, criterion or comparison.",
            ));
        }
        let a = r
            .assumption_ref
            .as_ref()
            .map(|v| {
                pin(
                    c,
                    v,
                    Kind::Assumption,
                    "/reversal_conditions/assumption_ref",
                )
            })
            .transpose()?;
        if let Some(a) = a
            && !assumption_refs.contains(&a)
        {
            assumption_refs.push(a);
        }
        let criterion_ref = r
            .criterion_ref
            .as_ref()
            .map(|v| pin(c, v, Kind::Criterion, "/reversal_conditions/criterion_ref"))
            .transpose()?;
        let comparison_ref = r
            .comparison_ref
            .as_ref()
            .map(|v| {
                pin(
                    c,
                    v,
                    Kind::Comparison,
                    "/reversal_conditions/comparison_ref",
                )
            })
            .transpose()?;
        let option_ref = pin(
            c,
            &r.option_ref,
            Kind::Option,
            "/reversal_conditions/option_ref",
        )?;
        if !alternatives.contains(&option_ref) {
            return Err(fail(
                "/reversal_conditions/option_ref",
                "Reversal option must be among alternatives.",
            ));
        }
        if let Some(criterion) = criterion_ref {
            let stored: CriterionBody = decode(
                &exact(
                    c.state(),
                    c.case_id().unwrap(),
                    criterion,
                    Kind::Criterion,
                    "/reversal_conditions/criterion_ref",
                )?
                .body,
            )?;
            if stored.question_ref.id != question_ref.id
                || stored
                    .option_refs
                    .as_ref()
                    .is_some_and(|options| !options.iter().any(|r| r.id == option_ref.id))
            {
                return Err(fail(
                    "/reversal_conditions/criterion_ref",
                    "Criterion does not apply to the reversal option and question.",
                ));
            }
        }
        if let Some(comparison) = comparison_ref {
            let stored: ComparisonBody = decode(
                &exact(
                    c.state(),
                    c.case_id().unwrap(),
                    comparison,
                    Kind::Comparison,
                    "/reversal_conditions/comparison_ref",
                )?
                .body,
            )?;
            if stored.retracted
                || !alternatives.iter().any(|r| r.id == stored.option_ref.id)
                || criterion_ref.is_some_and(|r| r.id != stored.criterion_ref.id)
            {
                return Err(fail(
                    "/reversal_conditions/comparison_ref",
                    "Comparison does not match the stated decision basis.",
                ));
            }
            if !comparison_refs.contains(&comparison) {
                comparison_refs.push(comparison);
            }
        }
        reversal_conditions.push(ReversalCondition {
            assumption_ref: a,
            criterion_ref,
            comparison_ref,
            option_ref,
            condition: r.condition,
        });
    }
    let option_id = match choice {
        Choice::Select { option_ref } => Some(option_ref.id),
        Choice::Defer { .. } => None,
    };
    let scope = MembershipScope::Question {
        case_id: c.case_id().unwrap(),
        question_id: question_ref.id,
        option_id,
    };
    let criteria = membership(c.state(), MembershipRule::ActiveHardCriteria, scope)?;
    Ok(DecisionBody {
        question_ref,
        choice,
        compared_alternatives: alternatives,
        criteria_set_version: criteria.digest.as_str().to_owned(),
        comparison_refs,
        hard_conditions,
        evidence_refs,
        assumption_refs,
        tradeoffs: p.tradeoffs,
        consequences: p.consequences,
        reopen_trigger: p.reopen_trigger,
        expected_outcome: p.expected_outcome,
        check_when: p.check_when,
        reversal_conditions,
        lifecycle: DecisionLifecycle::Proposed,
        review_ref: None,
        supersedes: None,
        superseded_by: None,
        change_reason: None,
        acceptance_challenges: None,
    })
}
/// A claim without current validated, retained evidence stays uncertain.
fn verified(s: &State, case: Id, cell: &ComparisonBody) -> bool {
    cell.evidence_refs.iter().any(|r| {
        let Ok(rec) = exact(s, case, *r, Kind::Observation, "/evidence_refs") else {
            return false;
        };
        // Stored Observation bodies carry lifecycle and assessment metadata that
        // the strict content type rejects; strip it before decoding.
        let mut content = rec.body.clone();
        let Some(object) = content.as_object_mut() else {
            return false;
        };
        for key in crate::observation_ops::OBSERVATION_METADATA {
            object.remove(*key);
        }
        let Ok(o) = decode::<ObservationBody>(&content) else {
            return false;
        };
        if crate::measurement_ops::effective_validity(s, rec) != MeasurementValidity::Valid {
            return false;
        }
        let (Some(measurement), ObservationValue::Measurement { value, unit }) =
            (&cell.value, &o.value)
        else {
            return false;
        };
        if measurement.unit != *unit
            || measurement.decimal.numeric_cmp(value) != std::cmp::Ordering::Equal
            || measurement.statistic.is_some()
            || measurement.sample_count.is_some()
        {
            return false;
        }
        let ObservationBasis::Source {
            artifact_ref,
            selector,
            ..
        } = o.basis
        else {
            return false;
        };
        let Ok(artifact) = exact(s, case, artifact_ref, Kind::Artifact, "/evidence_refs") else {
            return false;
        };
        let mut artifact_content = artifact.body.clone();
        let Some(object) = artifact_content.as_object_mut() else {
            return false;
        };
        for key in crate::observation_ops::ARTIFACT_METADATA {
            object.remove(*key);
        }
        let Ok(a) = decode::<ArtifactBody>(&artifact_content) else {
            return false;
        };
        selector.validation == SelectorValidation::Verified
            && a.availability == ArtifactAvailability::Retained
            && a.integrity == ArtifactIntegrity::Verified
            && a.digest.as_ref() == Some(&selector.artifact_digest)
    })
}

/// Acceptance examines every applicable hard criterion; missing and uncertain
/// comparisons require an explicit condition and never imply satisfaction.
fn check_cells(s: &State, case: Id, b: &DecisionBody) -> Result<(), Failure> {
    let selected = match b.choice {
        Choice::Select { option_ref } => option_ref,
        Choice::Defer { .. } => return Ok(()),
    };
    let option: OptionBody = decode(&exact(s, case, selected, Kind::Option, "/choice")?.body)?;
    if option.lifecycle != OptionLifecycle::Eligible {
        return Err(fail("/choice", "Selected option must be eligible."));
    }
    let scope = MembershipScope::Question {
        case_id: case,
        question_id: b.question_ref.id,
        option_id: Some(selected.id),
    };
    let criteria = membership(s, MembershipRule::ActiveHardCriteria, scope)?;
    if b.criteria_set_version != criteria.digest.as_str() {
        return Err(fail(
            "/criteria_set_version",
            "Applicable hard criteria changed; propose and review a current decision.",
        ));
    }
    let mut cells: BTreeMap<Id, ComparisonBody> = BTreeMap::new();
    for r in &b.comparison_refs {
        let cell: ComparisonBody =
            decode(&exact(s, case, *r, Kind::Comparison, "/comparison_refs")?.body)?;
        if cell.retracted {
            return Err(fail(
                "/comparison_refs",
                "Retracted comparison cannot support a decision.",
            ));
        }
        if cell.option_ref.id == selected.id && cells.insert(cell.criterion_ref.id, cell).is_some()
        {
            return Err(fail(
                "/comparison_refs",
                "Resolve competing comparisons explicitly.",
            )
            .with_schema("comparison.add"));
        }
    }
    for member in criteria.members {
        let cr = s
            .current(member.id)
            .ok_or_else(|| fail("/comparison_refs", "Criterion disappeared."))?;
        let cell = cells.get(&member.id);
        let outcome = if let Some(cell) = cell {
            if cell.option_ref != selected || cell.criterion_ref != cr.reference() {
                return Err(fail("/comparison_refs", "Selected comparison is stale."));
            }
            if verified(s, case, cell) {
                cell.outcome
            } else {
                ComparisonOutcome::Unknown
            }
        } else {
            ComparisonOutcome::Unknown
        };
        match outcome {
            ComparisonOutcome::Fails => {
                return Err(fail(
                    "/choice",
                    "Selected option fails an applicable hard criterion.",
                ));
            }
            ComparisonOutcome::Meets => {}
            _ => {
                if !b
                    .hard_conditions
                    .iter()
                    .any(|h| h.criterion_ref == cr.reference() && !h.reason.trim().is_empty())
                {
                    return Err(fail(
                        "/hard_conditions",
                        &format!("Acknowledge uncertain hard criterion {}.", member.id),
                    )
                    .with_schema("decision.propose"));
                }
            }
        }
    }
    Ok(())
}
/// Everything the decision is accepted over: relevant open objections (with
/// policy-required acknowledgments), uncertain comparisons and conditional
/// hard criteria. Acceptance under disagreement is allowed; silence is not.
fn acceptance_challenges(
    s: &State,
    target: &Record,
    b: &DecisionBody,
    acknowledgments: &[(RevisionRef, String)],
) -> Result<AcceptanceChallenges, Failure> {
    let case = target.header.case_id;
    let policy = acknowledgment_policy(s, case);
    let open_objections = acknowledge(
        relevant_open_objections(s, target)?,
        acknowledgments,
        policy,
    )?;
    let mut uncertain_comparisons = Vec::new();
    for r in &b.comparison_refs {
        let cell: ComparisonBody =
            decode(&exact(s, case, *r, Kind::Comparison, "/comparison_refs")?.body)?;
        if cell.outcome == ComparisonOutcome::Unknown || !verified(s, case, &cell) {
            uncertain_comparisons.push(*r);
        }
    }
    Ok(AcceptanceChallenges {
        policy,
        open_objections,
        uncertain_comparisons,
        conditional_hard_criteria: b.hard_conditions.iter().map(|h| h.criterion_ref).collect(),
    })
}
fn current_review(s: &State, target: &Record, r: RevisionRef) -> Result<(), Failure> {
    let review = exact(s, target.header.case_id, r, Kind::Review, "/review_ref")?;
    let b: ValidationReviewBody = decode(&review.body)?;
    if b.verdict != ValidationVerdict::Supports {
        return Err(fail("/review_ref", "A supporting review is required."));
    }
    validation_review_current(s, target, &b)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveDecision {
    Proposed,
    Accepted,
    Conditional,
    NeedsReview,
    Superseded,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveComparison {
    Current,
    NeedsReview,
    Retracted,
}
/// Recompute against the current snapshot; no lifecycle flag is silently rewritten.
pub fn effective_comparison(s: &State, r: &Record) -> Result<EffectiveComparison, Failure> {
    effective_comparison_with(s, &FreshnessIndex::build(s, r.header.case_id), r)
}
/// As [`effective_comparison`], with an index built for `s` and this case. A
/// cited change the index judges irrelevant, such as a lifecycle-only Option
/// revision or a criterion re-scoped to other options, keeps the cell current.
pub fn effective_comparison_with(
    s: &State,
    index: &FreshnessIndex,
    r: &Record,
) -> Result<EffectiveComparison, Failure> {
    if r.header.id.kind() != Kind::Comparison {
        return Err(fail("/comparison", "Expected a comparison."));
    }
    let b: ComparisonBody = decode(&r.body)?;
    if b.retracted {
        return Ok(EffectiveComparison::Retracted);
    }
    Ok(if index.assess(s, r).needs_review() {
        EffectiveComparison::NeedsReview
    } else {
        EffectiveComparison::Current
    })
}
pub fn effective_decision(s: &State, r: &Record) -> Result<EffectiveDecision, Failure> {
    effective_decision_with(s, &FreshnessIndex::build(s, r.header.case_id), r)
}
/// As [`effective_decision`], with an index built for `s` and this case. An
/// accepted decision needs review when the index finds a relevant change in
/// its reviewed basis: cells, criteria, options, evidence, assumptions, or the
/// review and its scoped memberships.
pub fn effective_decision_with(
    s: &State,
    index: &FreshnessIndex,
    r: &Record,
) -> Result<EffectiveDecision, Failure> {
    let b: DecisionBody = decode(&r.body)?;
    match b.lifecycle {
        DecisionLifecycle::Proposed => Ok(EffectiveDecision::Proposed),
        DecisionLifecycle::Superseded => Ok(EffectiveDecision::Superseded),
        DecisionLifecycle::Accepted => {
            let supported = b
                .review_ref
                .and_then(|review| s.revision(review))
                .and_then(|review| decode::<ValidationReviewBody>(&review.body).ok())
                .is_some_and(|review| review.verdict == ValidationVerdict::Supports);
            if !supported || index.assess(s, r).needs_review() {
                Ok(EffectiveDecision::NeedsReview)
            } else if b.hard_conditions.is_empty() {
                Ok(EffectiveDecision::Accepted)
            } else {
                Ok(EffectiveDecision::Conditional)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParetoStatus {
    DisqualifiedHard,
    StrictlyDominated,
    ParetoFrontier,
    UncertainFrontier,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ParetoOption {
    pub option_ref: RevisionRef,
    pub status: ParetoStatus,
    pub determining_cells: Vec<RevisionRef>,
    pub uncertain_criteria: Vec<RevisionRef>,
}
/// Weight-free and conservative: unknown, stale, multiple, subjective, and
/// noncomparable cells never prove dominance. This is advisory, not a gate.
pub fn pareto(s: &State, case: Id, question: Id) -> Result<Vec<ParetoOption>, Failure> {
    let scope = MembershipScope::Question {
        case_id: case,
        question_id: question,
        option_id: None,
    };
    let opts = membership(s, MembershipRule::EligibleOptions, scope)?.members;
    let mut criteria = Vec::new();
    let mut cells: BTreeMap<(Id, Id), Vec<(RevisionRef, ComparisonBody)>> = BTreeMap::new();
    for r in s.records().filter(|r| r.header.case_id == case) {
        match r.header.id.kind() {
            Kind::Criterion => {
                let c: CriterionBody = decode(&r.body)?;
                if c.question_ref.id == question && c.lifecycle == CriterionLifecycle::Active {
                    criteria.push((r.reference(), c));
                }
            }
            Kind::Comparison => {
                let c: ComparisonBody = decode(&r.body)?;
                if !c.retracted
                    && exact(s, case, c.option_ref, Kind::Option, "/option").is_ok()
                    && exact(s, case, c.criterion_ref, Kind::Criterion, "/criterion").is_ok()
                    && verified(s, case, &c)
                {
                    cells
                        .entry((c.option_ref.id, c.criterion_ref.id))
                        .or_default()
                        .push((r.reference(), c));
                }
            }
            _ => {}
        }
    }
    let mut result = Vec::new();
    for member in &opts {
        let option = s
            .current(member.id)
            .ok_or_else(|| fail("/option", "Option disappeared."))?
            .reference();
        let mut determining = Vec::new();
        let mut uncertain = Vec::new();
        let mut disqualified = false;
        for (cr, c) in &criteria {
            if c.option_refs
                .as_ref()
                .is_some_and(|v| !v.iter().any(|x| x.id == option.id))
            {
                continue;
            }
            match cells.get(&(option.id, cr.id)).map(Vec::as_slice) {
                Some([(r, cell)])
                    if c.kind == CriterionKind::Hard
                        && cell.outcome == ComparisonOutcome::Fails =>
                {
                    disqualified = true;
                    determining.push(*r);
                }
                Some([(_, cell)])
                    if cell.outcome != ComparisonOutcome::Unknown && cell.value.is_some() => {}
                _ => uncertain.push(*cr),
            }
        }
        if disqualified {
            result.push(ParetoOption {
                option_ref: option,
                status: ParetoStatus::DisqualifiedHard,
                determining_cells: determining,
                uncertain_criteria: uncertain,
            });
            continue;
        }
        let mut dominated = false;
        let mut any_indeterminate = criteria.is_empty();
        for other in &opts {
            if other.id == option.id {
                continue;
            }
            let mut better = false;
            let mut inferior = false;
            let mut comparable = !criteria.is_empty();
            let mut pair_cells = Vec::new();
            for (cr, c) in &criteria {
                if c.option_refs.as_ref().is_some_and(|v| {
                    !v.iter().any(|x| x.id == option.id) || !v.iter().any(|x| x.id == other.id)
                }) {
                    comparable = false;
                    break;
                }
                let a = cells.get(&(other.id, cr.id)).map(Vec::as_slice);
                let b = cells.get(&(option.id, cr.id)).map(Vec::as_slice);
                let (Some([(ar, ac)]), Some([(_, bc)])) = (a, b) else {
                    comparable = false;
                    break;
                };
                let (Some(av), Some(bv)) = (&ac.value, &bc.value) else {
                    comparable = false;
                    break;
                };
                if ac.outcome == ComparisonOutcome::Unknown
                    || bc.outcome == ComparisonOutcome::Unknown
                    || av.unit != bv.unit
                    || c.scale.unit.as_deref() != Some(av.unit.as_str())
                {
                    comparable = false;
                    break;
                }
                let ord = av.decimal.numeric_cmp(&bv.decimal);
                let preferred = match c.preferred_direction {
                    PreferredDirection::Higher => ord,
                    PreferredDirection::Lower => ord.reverse(),
                    _ => {
                        comparable = false;
                        break;
                    }
                };
                if preferred == std::cmp::Ordering::Less {
                    inferior = true;
                    break;
                }
                if preferred == std::cmp::Ordering::Greater {
                    better = true;
                }
                pair_cells.push(*ar);
            }
            if comparable && !inferior && better {
                dominated = true;
                determining = pair_cells;
                break;
            }
            if !comparable {
                any_indeterminate = true;
            }
        }
        let status = if dominated {
            ParetoStatus::StrictlyDominated
        } else if any_indeterminate || !uncertain.is_empty() {
            ParetoStatus::UncertainFrontier
        } else {
            ParetoStatus::ParetoFrontier
        };
        result.push(ParetoOption {
            option_ref: option,
            status,
            determining_cells: determining,
            uncertain_criteria: uncertain,
        });
    }
    Ok(result)
}

fn info(name: &'static str, summary: &'static str) -> OperationInfo {
    OperationInfo {
        name,
        stage: "decide",
        summary,
        when_to_use: "Record a decision and its reviewed basis.",
        lifecycle: "Accepted decisions are immutable.",
        effects: "Create or update a decision.",
        returns: "Decision revision.",
        revision_rule: "Use current exact references.",
        idempotency: "Retry the same command ID.",
        repair: "Inspect the decision and schema.",
        related: &[],
    }
}
fn ok<T>(_: &T) -> Result<(), Failure> {
    Ok(())
}
pub(crate) fn operations() -> Vec<OperationDescriptor> {
    let q = json!({"id":"q_01890f3e-7c00-7000-8000-000000000001","revision":1});
    let d = json!({"id":"dec_01890f3e-7c00-7000-8000-000000000002","revision":1});
    let review = json!({"id":"rev_01890f3e-7c00-7000-8000-000000000003","revision":1});
    let example = json!({"question_ref":q,"choice":{"kind":"defer","reason":"Await data"},"compared_alternatives":[],"comparison_refs":[],"hard_conditions":[],"evidence_refs":[],"assumption_refs":[],"tradeoffs":[],"consequences":["Choice delayed"],"reopen_trigger":"Measurement available","reversal_conditions":[]});
    vec![
        OperationDescriptor::active(
            info("decision.propose", "Propose a decision."),
            example.clone(),
            ok,
            |p: DecisionInput, c| {
                let b = normalize(c, p)?;
                c.create(Kind::Decision, body(b)?)?;
                Ok(())
            },
        ),
        OperationDescriptor::active(
            info("decision.accept", "Accept a reviewed decision."),
            json!({"decision_ref":d,"review_ref":review}),
            ok,
            |p: DecisionAccept, c| {
                let case = c.case_id().unwrap();
                let target = exact(
                    c.state(),
                    case,
                    p.decision_ref,
                    Kind::Decision,
                    "/decision_ref",
                )?
                .clone();
                let mut b: DecisionBody = decode(&target.body)?;
                if b.lifecycle != DecisionLifecycle::Proposed {
                    return Err(fail(
                        "/decision_ref",
                        "Only a proposed decision may be accepted.",
                    ));
                }
                let r = pin(c, &p.review_ref, Kind::Review, "/review_ref")?;
                current_review(c.state(), &target, r)?;
                check_cells(c.state(), case, &b)?;
                let acknowledgments = p
                    .acknowledgments
                    .iter()
                    .enumerate()
                    .map(|(index, ack)| {
                        let path = format!("/acknowledgments/{index}/objection_ref");
                        Ok((
                            c.resolve(&ack.objection_ref, Kind::Objection, &path)?,
                            ack.rationale.clone(),
                        ))
                    })
                    .collect::<Result<Vec<_>, Failure>>()?;
                let challenges = acceptance_challenges(c.state(), &target, &b, &acknowledgments)?;
                for warning in challenges.warnings(p.decision_ref.id) {
                    c.warn(warning);
                }
                let recorded: Vec<_> = challenges
                    .open_objections
                    .iter()
                    .map(|objection| objection.objection_ref)
                    .collect();
                b.lifecycle = DecisionLifecycle::Accepted;
                b.review_ref = Some(r);
                b.acceptance_challenges = Some(challenges);
                c.update(p.decision_ref, body(b)?)?;
                let id = p.decision_ref.id;
                c.check_final(move |s| {
                    let record = s
                        .current(id)
                        .ok_or_else(|| fail("/decision_ref", "Decision missing."))?;
                    let b: DecisionBody = decode(&record.body)?;
                    current_review(s, record, r)?;
                    check_cells(s, case, &b)?;
                    let open: Vec<_> = relevant_open_objections(s, record)?
                        .into_iter()
                        .map(|objection| objection.objection_ref)
                        .collect();
                    if open != recorded {
                        let mut failure = fail(
                            "/acknowledgments",
                            "The final transaction changes the open objections this decision is accepted over.",
                        );
                        failure.error.expected = Some(json!(open));
                        return Err(failure);
                    }
                    Ok(())
                });
                Ok(())
            },
        ),
        OperationDescriptor::active(
            info("decision.supersede", "Supersede an accepted decision."),
            json!({"decision_ref":d,"reason":"New evidence","replacement":example}),
            ok,
            |p: DecisionSupersede, c| {
                text(&p.reason, "/reason")?;
                let case = c.case_id().unwrap();
                let old = exact(
                    c.state(),
                    case,
                    p.decision_ref,
                    Kind::Decision,
                    "/decision_ref",
                )?
                .clone();
                let mut previous: DecisionBody = decode(&old.body)?;
                if previous.lifecycle != DecisionLifecycle::Accepted {
                    return Err(fail(
                        "/decision_ref",
                        "Only accepted decisions may be superseded.",
                    ));
                }
                let mut replacement = normalize(c, p.replacement)?;
                if replacement.question_ref.id != previous.question_ref.id {
                    return Err(fail(
                        "/replacement/question_ref",
                        "Successor must address the same question.",
                    ));
                }
                replacement.supersedes = Some(p.decision_ref);
                let next = c.create(Kind::Decision, body(replacement)?)?;
                previous.lifecycle = DecisionLifecycle::Superseded;
                previous.superseded_by = Some(next);
                previous.change_reason = Some(p.reason);
                c.update(p.decision_ref, body(previous)?)?;
                Ok(())
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        basis::basis_digest,
        catalog::Catalog,
        command::Command,
        header::{Clock, RecordHeader, Revision, SchemaVersion, UtcTimestamp},
        identity::IdSource,
        review_ops::{promotion_memberships, required_read_set},
        state::{Acceptance, AcceptedTransaction, validate_and_apply},
        wire::RecordChange,
    };
    use uuid::Uuid;
    fn id(prefix: &str, n: u64) -> Id {
        format!("{prefix}_018d87fc-5234-7000-8000-{n:012x}")
            .parse()
            .unwrap()
    }
    fn rec(id: Id, case: Id, seq: u64, body: Value) -> Record {
        Record {
            header: RecordHeader {
                id,
                case_id: case,
                schema_version: SchemaVersion::RecordV0Alpha,
                created_by: "agent:test".parse().unwrap(),
                actor_descriptor_self_reported: None,
                created_at: "2026-11-22T08:12:44.000Z".parse().unwrap(),
                recorded_at: "2026-11-22T08:12:44.000Z".parse().unwrap(),
                recorded_by: "agent:test".parse().unwrap(),
                observed_at: None,
                acquired_at: None,
                acceptance_sequence: seq,
                revision: Revision(1),
                history: vec![],
                classification: Default::default(),
                provenance: Default::default(),
                extensions: Default::default(),
            },
            body,
        }
    }
    fn add(s: &mut State, r: Record) {
        let c = RecordChange {
            client_ref: None,
            id: r.header.id,
            revision: 1,
        };
        s.apply(&AcceptedTransaction {
            case_id: r.header.case_id,
            sequence: r.header.acceptance_sequence,
            records: vec![r],
            created: vec![c],
            updated: vec![],
            warnings: vec![],
        })
        .unwrap();
    }
    fn update_body(s: &mut State, id: Id, body: Value) {
        let mut r = s.current(id).unwrap().clone();
        r.header.acceptance_sequence = s.sequence() + 1;
        r.header.history.push(r.header.revision);
        r.header.revision.0 += 1;
        r.body = body;
        let c = RecordChange {
            client_ref: None,
            id,
            revision: r.header.revision.0,
        };
        s.apply(&AcceptedTransaction {
            case_id: r.header.case_id,
            sequence: r.header.acceptance_sequence,
            records: vec![r],
            created: vec![],
            updated: vec![c],
            warnings: vec![],
        })
        .unwrap();
    }
    struct Ids(u64);
    impl IdSource for Ids {
        fn next_uuid(&mut self) -> Uuid {
            self.0 += 1;
            format!("018d87fc-5234-7000-8000-{:012x}", self.0)
                .parse()
                .unwrap()
        }
    }
    struct Time;
    impl Clock for Time {
        fn now_utc(&mut self) -> UtcTimestamp {
            "2026-11-22T08:12:44.000Z".parse().unwrap()
        }
    }
    fn option(q: Id) -> Value {
        json!({"question_ref":{"id":q,"revision":1},"description":"Option","context":null,"feasibility":null,"dependencies":[],"tradeoff_notes":[],"lifecycle":"eligible","disposition_reason":null,"amendment_reason":null,"criterion_ref":null})
    }
    fn criterion(q: Id) -> Value {
        json!({"name":"Latency","kind":"hard","question_ref":{"id":q,"revision":1},"option_refs":null,"scale":{"unit":"ms","description":"p99","anchors":[]},"preferred_direction":"lower","threshold":{"kind":"measured","operator":"at_most","value":{"decimal":"25","unit":"ms"}},"weighting_rationale":null,"lifecycle":"active","change_reason":null,"material_change":false})
    }
    fn fixture() -> (State, Id, Id, Id, Id, Id) {
        let case = id("case", 1);
        let q = id("q", 2);
        let o = id("opt", 3);
        let d = id("dec", 4);
        let review = id("rev", 5);
        let mut s = State::default();
        add(&mut s, rec(case, case, 1, json!({})));
        add(&mut s, rec(q, case, 2, json!({})));
        add(&mut s, rec(o, case, 3, option(q)));
        let scope = MembershipScope::Question {
            case_id: case,
            question_id: q,
            option_id: Some(o),
        };
        let digest = membership(&s, MembershipRule::ActiveHardCriteria, scope)
            .unwrap()
            .digest;
        let b = json!({"question_ref":{"id":q,"revision":1},"choice":{"kind":"select","option_ref":{"id":o,"revision":1}},"compared_alternatives":[{"id":o,"revision":1}],"criteria_set_version":digest.as_str(),"comparison_refs":[],"hard_conditions":[],"evidence_refs":[],"assumption_refs":[],"tradeoffs":[],"consequences":["Keep system"],"reopen_trigger":"evidence changes","expected_outcome":null,"check_when":null,"reversal_conditions":[],"lifecycle":"proposed","review_ref":null,"supersedes":null,"superseded_by":null,"change_reason":null});
        add(&mut s, rec(d, case, 4, b));
        let target = s.current(d).unwrap();
        let reads = required_read_set(&s, target).unwrap();
        let groups = promotion_memberships(&s, target).unwrap();
        let basis = basis_digest(target).unwrap();
        let r = json!({"kind":"validation","status":"submitted","subject_ref":{"id":d,"revision":1},"basis_digest":basis,"read_set":reads,"memberships":groups,"method":"review","materials_examined":[],"shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":[],"context_isolation":"fresh_context_unblinded","context":{"saw_original_conclusion":{"state":"unknown"},"gathered_new_evidence":{"state":"unknown"},"method_or_source_changes":[],"activity":"review","dependencies":"unknown","dependence_basis":"unknown"}});
        add(&mut s, rec(review, case, 5, r));
        (s, case, q, o, d, review)
    }
    fn accept(
        s: &State,
        case: Id,
        d: Id,
        review: Id,
        extra: Vec<Value>,
    ) -> Result<AcceptedTransaction, Failure> {
        let mut ops = vec![
            json!({"op":"decision.accept","payload":{"decision_ref":{"id":d,"revision":1},"review_ref":{"id":review,"revision":1}}}),
        ];
        ops.extend(extra);
        let cmd:Command=serde_json::from_value(json!({"schema_version":"inv.command.v0-alpha","command_id":"accept","case_id":case,"actor":"agent:test","preconditions":[],"operations":ops})).unwrap();
        let mut descriptors = operations();
        descriptors.extend(crate::evaluate_ops::operations());
        let c = Catalog::new(descriptors).unwrap();
        validate_and_apply(
            s,
            &cmd,
            &c,
            Acceptance {
                sequence: s.sequence() + 1,
                clock: &mut Time,
                ids: &mut Ids(100),
            },
        )
    }
    #[test]
    fn final_batch_detects_late_hard_criterion_but_ignores_other_question() {
        let (mut s, case, q, _, d, r) = fixture();
        let unrelated = id("q", 20);
        add(&mut s, rec(unrelated, case, 6, json!({})));
        let cp = |question| json!({"op":"criterion.create","payload":{"name":"New requirement","kind":"hard","scope":{"kind":"all_options","question_ref":{"id":question,"revision":1}},"scale":{"unit":"ms","description":"latency","anchors":[]},"preferred_direction":"lower","threshold":{"kind":"measured","operator":"at_most","value":{"decimal":"25","unit":"ms"}}}});
        assert!(accept(&s, case, d, r, vec![cp(unrelated)]).is_ok());
        assert!(accept(&s, case, d, r, vec![cp(q)]).is_err());
    }
    #[test]
    fn accepted_decision_cannot_be_accepted_twice() {
        let (mut s, case, _, _, d, r) = fixture();
        let accepted = accept(&s, case, d, r, vec![]).unwrap();
        s.apply(&accepted).unwrap();
        assert_eq!(
            effective_decision(&s, s.current(d).unwrap()).unwrap(),
            EffectiveDecision::Accepted
        );
        let c = Catalog::new(operations()).unwrap();
        let cmd:Command=serde_json::from_value(json!({"schema_version":"inv.command.v0-alpha","command_id":"again","case_id":case,"actor":"agent:test","preconditions":[],"operations":[{"op":"decision.accept","payload":{"decision_ref":{"id":d,"revision":2},"review_ref":{"id":r,"revision":1}}}]})).unwrap();
        assert!(
            validate_and_apply(
                &s,
                &cmd,
                &c,
                Acceptance {
                    sequence: s.sequence() + 1,
                    clock: &mut Time,
                    ids: &mut Ids(500)
                }
            )
            .is_err()
        );
    }
    #[test]
    fn examples_are_schema_valid() {
        for op in operations() {
            op.validate_example().unwrap();
            let schema = serde_json::to_value(op.payload_schema().unwrap()).unwrap();
            jsonschema::validator_for(&schema)
                .unwrap()
                .validate(op.example().unwrap())
                .unwrap();
        }
    }
    #[test]
    fn unmeasured_hard_requirement_is_explicitly_conditional() {
        let (mut s, case, q, o, d, _) = fixture();
        let k = id("crit", 30);
        add(&mut s, rec(k, case, 6, criterion(q)));
        let mut b: DecisionBody = decode(&s.current(d).unwrap().body).unwrap();
        let scope = MembershipScope::Question {
            case_id: case,
            question_id: q,
            option_id: Some(o),
        };
        b.criteria_set_version = membership(&s, MembershipRule::ActiveHardCriteria, scope)
            .unwrap()
            .digest
            .as_str()
            .to_owned();
        assert!(check_cells(&s, case, &b).is_err());
        b.hard_conditions.push(HardCondition {
            criterion_ref: RevisionRef { id: k, revision: 1 },
            reason: "Accept only if measured below limit".into(),
        });
        assert!(check_cells(&s, case, &b).is_ok());
        let status = pareto(&s, case, q).unwrap();
        assert_eq!(status[0].status, ParetoStatus::UncertainFrontier);
        assert_eq!(
            status[0].uncertain_criteria,
            vec![RevisionRef { id: k, revision: 1 }]
        );
    }
    #[test]
    fn generated_unknown_tables_never_prove_dominance_and_threshold_changes_stale_basis() {
        for seed in 0..32u64 {
            let (mut s, case, q, o, _, _) = fixture();
            let other = id("opt", 100 + seed);
            let criterion_id = id("crit", 200 + seed);
            let next = s.sequence() + 1;
            add(&mut s, rec(other, case, next, option(q)));
            let next = s.sequence() + 1;
            let mut k = criterion(q);
            k["kind"] = json!(if seed % 2 == 0 { "hard" } else { "soft" });
            add(&mut s, rec(criterion_id, case, next, k.clone()));
            let results = pareto(&s, case, q).unwrap();
            assert_eq!(results.len(), 2, "seed {seed}");
            assert!(
                results
                    .iter()
                    .all(|r| r.status == ParetoStatus::UncertainFrontier),
                "seed {seed}"
            );
            let duplicate = id("opt", 300 + seed);
            let next = s.sequence() + 1;
            add(&mut s, rec(duplicate, case, next, option(q)));
            let with_duplicate = pareto(&s, case, q).unwrap();
            for before in &results {
                let after = with_duplicate
                    .iter()
                    .find(|x| x.option_ref.id == before.option_ref.id)
                    .unwrap();
                assert_eq!(before.status, after.status, "seed {seed}");
            }
            if seed % 2 == 0 {
                let scope = MembershipScope::Question {
                    case_id: case,
                    question_id: q,
                    option_id: Some(o),
                };
                let before =
                    membership(&s, MembershipRule::ActiveHardCriteria, scope.clone()).unwrap();
                k["threshold"]["value"]["decimal"] = json!(format!("{}", 26 + seed));
                update_body(&mut s, criterion_id, k);
                let after = membership(&s, MembershipRule::ActiveHardCriteria, scope).unwrap();
                assert_ne!(before.digest, after.digest, "seed {seed}");
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Cell {
        Measured(u64),
        Unknown,
        Stale(u64),
        Preference,
    }

    fn as_measurement(cell: Cell) -> Option<u64> {
        match cell {
            Cell::Measured(value) => Some(value),
            _ => None,
        }
    }

    fn generated_table(rows: &[[Cell; 3]], order: &[usize]) -> (State, Id, Id) {
        let case = id("case", 900);
        let question = id("q", 901);
        let source = id("src", 902);
        let artifact = id("art", 903);
        let digest = format!("sha256:{}", "a".repeat(64));
        let reference = |id| json!({"id":id,"revision":1});
        let mut state = State::default();
        for (id, body) in [
            (case, json!({})),
            (question, json!({})),
            (source, json!({})),
            (
                artifact,
                json!({"source":reference(source),"digest":digest,"size_bytes":12,"format":"application/json","transforms":[],"availability":"retained","integrity":"verified","sensitivity":"internal"}),
            ),
        ] {
            let next = state.sequence() + 1;
            add(&mut state, rec(id, case, next, body));
        }
        for column in 0..3 {
            let mut body = criterion(question);
            if column != 0 {
                body["kind"] = json!("soft");
                body["threshold"] = Value::Null;
            }
            let next = state.sequence() + 1;
            add(&mut state, rec(id("crit", 1000 + column), case, next, body));
        }
        for &row in order {
            let option_id = id("opt", 1100 + row as u64);
            let next = state.sequence() + 1;
            add(&mut state, rec(option_id, case, next, option(question)));
            for (column, cell) in rows[row].iter().enumerate() {
                let ordinal = (row * 3 + column) as u64;
                let observation_id = id("obs", 1200 + ordinal);
                let criterion_id = id("crit", 1000 + column as u64);
                let comparison_id = id("cmp", 1300 + ordinal);
                let (value, outcome, evidence, basis, method) = match cell {
                    Cell::Measured(value) | Cell::Stale(value) => {
                        let number = value.to_string();
                        let observation = json!({
                            "value":{"kind":"measurement","value":number,"unit":"ms"},
                            "basis":{"kind":"source","artifact_ref":reference(artifact),"selector":{"schema_version":"inv.selector.v1","artifact_digest":digest,"selector":{"type":"whole"},"validation":"verified"},"text_quote":{"exact":null,"prefix":null,"suffix":null,"state":"not_applicable"}},
                            "validity":"valid","quality":[],"limitations":[],"variables_changed":[],
                        });
                        let next = state.sequence() + 1;
                        add(
                            &mut state,
                            rec(observation_id, case, next, observation.clone()),
                        );
                        if matches!(cell, Cell::Stale(_)) {
                            update_body(&mut state, observation_id, observation);
                        }
                        (
                            json!({"decimal":number,"unit":"ms"}),
                            if column == 0 && *value > 25 {
                                "fails"
                            } else {
                                "meets"
                            },
                            json!([reference(observation_id)]),
                            Value::Null,
                            "measured",
                        )
                    }
                    Cell::Unknown => (
                        Value::Null,
                        "unknown",
                        json!([]),
                        json!({"kind":"unknown","reason":"No observation"}),
                        "unknown",
                    ),
                    Cell::Preference => (
                        Value::Null,
                        "unknown",
                        json!([]),
                        json!({"kind":"preference","statement":"Operator preference"}),
                        "preference",
                    ),
                };
                let body = json!({"option_ref":reference(option_id),"criterion_ref":reference(criterion_id),"outcome":outcome,"method":method,"value":value,"basis":basis,"evidence_refs":evidence,"assumption_refs":[],"limitations":null,"uncertainty":"Synthetic test table","rationale":"Generated case","retracted":false,"change_reason":null});
                let next = state.sequence() + 1;
                add(&mut state, rec(comparison_id, case, next, body));
            }
        }
        (state, case, question)
    }

    #[test]
    fn stored_observation_metadata_does_not_hide_verified_evidence() {
        let rows = [[Cell::Measured(10); 3]];
        let (mut state, case, _) = generated_table(&rows, &[0]);
        let observation_id = id("obs", 1200);
        let comparison_id = id("cmp", 1300);
        let artifact_id = id("art", 903);
        let mut artifact = state.current(artifact_id).unwrap().body.clone();
        artifact["status"] = json!("active");
        artifact["lifecycle"] = json!("active");
        update_body(&mut state, artifact_id, artifact);
        let artifact_revision = state.current(artifact_id).unwrap().header.revision.0;
        let observation = state.current(observation_id).unwrap().body.clone();
        for (validity, expected) in [("valid", true), ("invalid", false)] {
            let mut stored = observation.clone();
            stored["validity"] = json!(validity);
            stored["status"] = json!("active");
            stored["run_ref"] = Value::Null;
            stored["basis"]["artifact_ref"]["revision"] = json!(artifact_revision);
            stored["measurement_assessment"] = json!({"judgments":[]});
            update_body(&mut state, observation_id, stored);
            let mut cell = state.current(comparison_id).unwrap().body.clone();
            let revision = state.current(observation_id).unwrap().header.revision.0;
            cell["evidence_refs"] = json!([{"id":observation_id,"revision":revision}]);
            let cell: ComparisonBody = decode(&cell).unwrap();
            assert_eq!(verified(&state, case, &cell), expected, "{validity}");
        }
    }

    #[test]
    fn generated_pareto_proofs_require_current_ranked_cells() {
        let measure = |value| Cell::Measured(value);
        let witness = [
            [measure(10), measure(5), measure(10)],
            [measure(20), measure(20), measure(20)],
            [measure(20), Cell::Unknown, measure(20)],
            [measure(40), Cell::Preference, measure(1)],
        ];
        let complete = |row: &[Cell; 3]| -> Option<Vec<u64>> {
            row.iter().copied().map(as_measurement).collect()
        };
        for seed in 0..48u64 {
            let mut rows = witness;
            if seed != 0 {
                let mut random = seed;
                for row in &mut rows {
                    for cell in row {
                        random = random
                            .wrapping_mul(6_364_136_223_846_793_005)
                            .wrapping_add(1);
                        let value = (random >> 32) % 50;
                        *cell = match (random >> 54) % 8 {
                            0 => Cell::Unknown,
                            1 => Cell::Stale(value),
                            2 => Cell::Preference,
                            _ => measure(value),
                        };
                    }
                }
            }
            let run = |rows, order| {
                let (state, case, question) = generated_table(rows, order);
                pareto(&state, case, question)
                    .unwrap()
                    .into_iter()
                    .map(|option| option.status)
                    .collect::<Vec<_>>()
            };
            let results = run(&rows, &[0, 1, 2, 3]);
            assert_eq!(results, run(&rows, &[3, 1, 0, 2]), "seed {seed}");
            let expanded = [rows[0], rows[1], rows[2], rows[3], rows[1]];
            assert_eq!(
                &results,
                &run(&expanded, &[4, 1, 3, 0, 2])[..4],
                "seed {seed}"
            );
            for (row, result) in rows.iter().zip(results) {
                let disqualified = as_measurement(row[0]).is_some_and(|hard| hard > 25);
                let dominated = !disqualified
                    && complete(row).is_some_and(|values| {
                        rows.iter().filter_map(complete).any(|comparison| {
                            comparison.iter().zip(&values).all(|(a, b)| a <= b)
                                && comparison.iter().zip(&values).any(|(a, b)| a < b)
                        })
                    });
                assert_eq!(
                    result == ParetoStatus::DisqualifiedHard,
                    disqualified,
                    "seed {seed}"
                );
                assert_eq!(
                    result == ParetoStatus::StrictlyDominated,
                    dominated,
                    "seed {seed}"
                );
            }
            let (mut state, _, _) = generated_table(&rows, &[0, 1, 2, 3]);
            let changed = id("crit", 1000);
            let comparisons_before: Vec<_> = state
                .records()
                .filter(|record| record.header.id.kind() == Kind::Comparison)
                .map(|record| {
                    (
                        record.header.id,
                        effective_comparison(&state, record).unwrap(),
                    )
                })
                .collect();
            let mut threshold = state.current(changed).unwrap().body.clone();
            threshold["threshold"]["value"]["decimal"] = json!("13");
            update_body(&mut state, changed, threshold);
            for (comparison, before) in comparisons_before {
                let record = state.current(comparison).unwrap();
                let body: ComparisonBody = decode(&record.body).unwrap();
                let expected = if body.criterion_ref.id == changed {
                    EffectiveComparison::NeedsReview
                } else {
                    before
                };
                assert_eq!(
                    effective_comparison(&state, record).unwrap(),
                    expected,
                    "seed {seed}"
                );
            }
        }
    }
}
