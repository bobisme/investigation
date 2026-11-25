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
    guardrails::{self, GuardedOperation},
    identity::{Id, Kind},
    objection_policy::{
        AcceptanceChallenges, ObjectionAcknowledgmentInput, acknowledge, acknowledgment_policy,
        relevant_open_objections,
    },
    pack_policy::{self, Consulted, Gate, GatePolicy, Subject},
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
    /// At least one of `assumption_ref`, `criterion_ref` and `comparison_ref` is required: the
    /// record whose change would reverse the choice.
    pub assumption_ref: Option<Reference>,
    /// See `assumption_ref`: one of the three links is required.
    pub criterion_ref: Option<Reference>,
    /// See `assumption_ref`: one of the three links is required.
    pub comparison_ref: Option<Reference>,
    /// The option the choice would reverse to; one of `compared_alternatives`.
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
    /// Every option weighed, at the revision compared. A `select` choice's option must be one of
    /// them.
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
    /// Pinned pack policy consulted by the gate that wrote this revision
    /// (`decision.propose` or `decision.accept`). Absent on older records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_policy: Option<GatePolicy>,
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
        return Err(Failure::wrong_kind(
            ErrorCode::GateFailed,
            path,
            &[kind],
            r.id.kind(),
        ));
    }
    let found = s
        .current(r.id)
        .filter(|v| v.header.case_id == case)
        .ok_or_else(|| fail(path, "Reference is not in this case."))?;
    if found.reference() != r {
        let mut e = Failure::new(
            ErrorCode::RevisionConflict,
            format!(
                "{kind:?} {} is at revision {}, not the cited {}. Read the current revision and \
                 cite it if it still supports this; a proposal that cites the old one is replaced \
                 with decision.supersede.",
                r.id, found.header.revision.0, r.revision
            ),
        )
        .at(path)
        .with_argv(vec![
            crate::PROGRAM_NAME.into(),
            "show".into(),
            r.id.to_string(),
            "--depth".into(),
            "summary".into(),
            "--format".into(),
            "json".into(),
        ]);
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
        return Err(Failure::wrong_kind(
            ErrorCode::GateFailed,
            path,
            kinds,
            r.id.kind(),
        ));
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
        .enumerate()
        .map(|(i, r)| pin(c, r, Kind::Option, &format!("/compared_alternatives/{i}")))
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
                    "/choice/option_ref",
                    "The selected option must also be listed in compared_alternatives, at the same \
                     revision: compared_alternatives holds every option weighed, the chosen one \
                     included.",
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
    for (index, r) in p.reversal_conditions.into_iter().enumerate() {
        let at = |field: &str| format!("/reversal_conditions/{index}{field}");
        text(&r.condition, &at("/condition"))?;
        if r.assumption_ref.is_none() && r.criterion_ref.is_none() && r.comparison_ref.is_none() {
            return Err(fail(
                &at(""),
                "A reversal condition needs at least one of assumption_ref, criterion_ref or \
                 comparison_ref: the assumption, criterion or comparison whose change would \
                 reverse the choice.",
            ));
        }
        let a = r
            .assumption_ref
            .as_ref()
            .map(|v| pin(c, v, Kind::Assumption, &at("/assumption_ref")))
            .transpose()?;
        if let Some(a) = a
            && !assumption_refs.contains(&a)
        {
            assumption_refs.push(a);
        }
        let criterion_ref = r
            .criterion_ref
            .as_ref()
            .map(|v| pin(c, v, Kind::Criterion, &at("/criterion_ref")))
            .transpose()?;
        let comparison_ref = r
            .comparison_ref
            .as_ref()
            .map(|v| pin(c, v, Kind::Comparison, &at("/comparison_ref")))
            .transpose()?;
        let option_ref = pin(c, &r.option_ref, Kind::Option, &at("/option_ref"))?;
        if !alternatives.contains(&option_ref) {
            return Err(fail(
                &at("/option_ref"),
                "A reversal condition's option_ref must be one of compared_alternatives, at the \
                 same revision: the option the choice would reverse to.",
            ));
        }
        if let Some(criterion) = criterion_ref {
            let stored: CriterionBody = decode(
                &exact(
                    c.state(),
                    c.case_id().unwrap(),
                    criterion,
                    Kind::Criterion,
                    &at("/criterion_ref"),
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
                    &at("/criterion_ref"),
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
                    &at("/comparison_ref"),
                )?
                .body,
            )?;
            if stored.retracted
                || !alternatives.iter().any(|r| r.id == stored.option_ref.id)
                || criterion_ref.is_some_and(|r| r.id != stored.criterion_ref.id)
            {
                return Err(fail(
                    &at("/comparison_ref"),
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
        gate_policy: None,
    })
}
/// A claim without current validated, retained evidence stays uncertain.
pub(crate) fn verified(s: &State, case: Id, cell: &ComparisonBody) -> bool {
    cell.evidence_refs.iter().any(|r| {
        // A later measurement judgment that keeps or raises effective validity
        // is benign to the cell, as it is to the freshness index.
        let Some(rec) = crate::freshness::benign_successor(s, case, *r)
            .filter(|_| r.id.kind() == Kind::Observation)
        else {
            return false;
        };
        // Stored Observation bodies carry lifecycle and assessment metadata
        // beside the typed body; read the typed body only.
        let Ok(o) = ObservationBody::from_stored(&rec.body) else {
            return false;
        };
        if crate::measurement_ops::effective_validity(s, rec) != MeasurementValidity::Valid {
            return false;
        }
        // The cited Observation's own measurement fields decide whether its
        // value can settle the cell: a statistic over completed attempts only,
        // while recorded attempts fall outside it, cannot, whatever the
        // Comparison itself declares.
        if crate::measurement_flags::completion_only_statistic(s, rec) {
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
        let Ok(a) = ArtifactBody::from_stored(&artifact.body) else {
            return false;
        };
        selector.validation == SelectorValidation::Verified
            && a.availability == ArtifactAvailability::Retained
            && a.integrity == ArtifactIntegrity::Verified
            && a.digest.as_ref() == Some(&selector.artifact_digest)
    })
}

/// Applicable hard criteria of a selected option that a proposal leaves
/// unaddressed: it cites no Comparison of the option against the criterion's
/// current revision with a known outcome, and it states no hard condition for
/// it. `decision.accept` refuses such a decision, so `decision.propose` warns
/// and records it: a proposal may still be revised before review.
fn unconditioned_hard(s: &State, case: Id, b: &DecisionBody) -> Vec<RevisionRef> {
    let Choice::Select {
        option_ref: selected,
    } = b.choice
    else {
        return Vec::new();
    };
    let scope = MembershipScope::Question {
        case_id: case,
        question_id: b.question_ref.id,
        option_id: Some(selected.id),
    };
    let Ok(criteria) = membership(s, MembershipRule::ActiveHardCriteria, scope) else {
        return Vec::new();
    };
    let cited: Vec<ComparisonBody> = b
        .comparison_refs
        .iter()
        .filter_map(|reference| s.revision(*reference))
        .filter_map(|record| decode::<ComparisonBody>(&record.body).ok())
        .filter(|cell| !cell.retracted && cell.option_ref.id == selected.id)
        .collect();
    criteria
        .members
        .iter()
        .filter_map(|member| s.current(member.id).map(Record::reference))
        .filter(|current| {
            !b.hard_conditions
                .iter()
                .any(|condition| condition.criterion_ref == *current)
                && !cited.iter().any(|cell| {
                    cell.criterion_ref == *current
                        && matches!(
                            cell.outcome,
                            ComparisonOutcome::Meets | ComparisonOutcome::Fails
                        )
                })
        })
        .collect()
}

fn warn_unconditioned_hard(c: &mut OperationContext<'_>, b: &DecisionBody) {
    let Some(case) = c.case_id() else {
        return;
    };
    for criterion in unconditioned_hard(c.state(), case, b) {
        c.warn(format!(
            "Hard criterion {} applies to the selected option, but the proposal cites no Comparison of its current revision with a known outcome and states no hard condition for it (see criterion.hard_unmeasured). decision.accept refuses it until the cell is measured or a hard condition with a reason is stated.",
            criterion.id
        ));
    }
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
        return Err(not_eligible(selected, option.lifecycle));
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
    let mut cells: BTreeMap<Id, (RevisionRef, ComparisonBody)> = BTreeMap::new();
    for r in &b.comparison_refs {
        let cell: ComparisonBody =
            decode(&exact(s, case, *r, Kind::Comparison, "/comparison_refs")?.body)?;
        if cell.retracted {
            return Err(fail(
                "/comparison_refs",
                "Retracted comparison cannot support a decision.",
            ));
        }
        if cell.option_ref.id == selected.id
            && cells.insert(cell.criterion_ref.id, (*r, cell)).is_some()
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
        let outcome = if let Some((cell_ref, cell)) = cell {
            if cell.option_ref != selected || cell.criterion_ref != cr.reference() {
                return Err(stale_cell(*cell_ref, cell, selected, cr.reference()));
            }
            if verified(s, case, cell) && crate::absence_gate::comparison_basis_holds(s, case, cell)
            {
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
/// A cited comparison of the selected option that pins another revision of
/// the option or of the criterion than the decision's (bn-29t7). The
/// revisions are revision checks, as in REVISION_CONFLICT.
fn stale_cell(
    cell_ref: RevisionRef,
    cell: &ComparisonBody,
    option: RevisionRef,
    criterion: RevisionRef,
) -> Failure {
    let mut failure = fail(
        "/comparison_refs",
        &format!(
            "Comparison {} of the selected option pins option revision {} and criterion revision \
             {}, but the decision chooses option revision {} and the criterion is at revision {}. \
             comparison.revise it against those revisions, then decision.supersede this proposal \
             with one that cites the new comparison revision, submit a fresh review.submit, and \
             accept the replacement.",
            cell_ref.id,
            cell.option_ref.revision,
            cell.criterion_ref.revision,
            option.revision,
            criterion.revision
        ),
    )
    .with_argv(vec![
        crate::PROGRAM_NAME.into(),
        "schema".into(),
        "comparison.revise".into(),
        "--format".into(),
        "json".into(),
    ]);
    failure.error.actual = Some(json!({
        "comparison_ref": cell_ref,
        "option_ref": cell.option_ref,
        "criterion_ref": cell.criterion_ref,
    }));
    failure.error.expected = Some(json!({"option_ref": option, "criterion_ref": criterion}));
    failure
}

fn lifecycle_label(lifecycle: OptionLifecycle) -> &'static str {
    match lifecycle {
        OptionLifecycle::Proposed => "proposed",
        OptionLifecycle::Eligible => "eligible",
        OptionLifecycle::RuledOut => "ruled_out",
        OptionLifecycle::Withdrawn => "withdrawn",
        OptionLifecycle::Superseded => "superseded",
    }
}

/// The order that makes an option choosable. Comparisons and the Decision pin
/// the option's exact revision, and an `eligible` option is a member of the
/// review's scoped option set, so the disposition must come first (bn-30vf).
const ELIGIBILITY_ORDER: &str = "Order: option.disposition to eligible, then comparison.revise \
     each comparison of the option (they pin its revision), then decision.supersede this proposal \
     with one that cites the new revisions, then a fresh review.submit, then decision.accept.";

fn not_eligible(option: RevisionRef, lifecycle: OptionLifecycle) -> Failure {
    let mut failure = fail(
        "/choice",
        &format!(
            "Selected option {} is {}; only an eligible option can be chosen. {ELIGIBILITY_ORDER}",
            option.id,
            lifecycle_label(lifecycle)
        ),
    )
    // The repair schema stays the refused operation's, as for every gate;
    // the argv is the step to take first.
    .with_argv(vec![
        crate::PROGRAM_NAME.into(),
        "schema".into(),
        "option.disposition".into(),
        "--format".into(),
        "json".into(),
    ]);
    failure.error.actual = Some(json!({"option_ref": option, "lifecycle": lifecycle}));
    failure
}

/// `decision.accept` refuses a selected option that is not eligible, and the
/// disposition that fixes it stales what pins the option, so `propose` warns.
fn warn_ineligible_choice(c: &mut OperationContext<'_>, b: &DecisionBody) -> Result<(), Failure> {
    let Choice::Select { option_ref } = b.choice else {
        return Ok(());
    };
    let case = c
        .case_id()
        .ok_or_else(|| fail("/choice", "Case missing."))?;
    let option: OptionBody =
        decode(&exact(c.state(), case, option_ref, Kind::Option, "/choice")?.body)?;
    if option.lifecycle != OptionLifecycle::Eligible {
        c.warn(format!(
            "Selected option {} is {}; decision.accept refuses a choice that is not eligible. {ELIGIBILITY_ORDER}",
            option_ref.id,
            lifecycle_label(option.lifecycle)
        ));
    }
    Ok(())
}

/// Other proposals for the same question stay live until they are replaced;
/// `decision.supersede` replaces one with a reason (bn-30vf).
fn warn_other_proposals(c: &mut OperationContext<'_>, b: &DecisionBody) {
    let Some(case) = c.case_id() else {
        return;
    };
    let others: Vec<Id> = c
        .state()
        .records_of_kind(Kind::Decision)
        .filter(|record| record.header.case_id == case)
        .filter_map(|record| {
            let other: DecisionBody = decode(&record.body).ok()?;
            (other.lifecycle == DecisionLifecycle::Proposed
                && other.question_ref.id == b.question_ref.id)
                .then_some(record.header.id)
        })
        .collect();
    for other in others {
        c.warn(format!(
            "Decision {other} is also proposed for this question and stays proposed. To replace it, \
             use decision.supersede on it instead of a second decision.propose."
        ));
    }
}

/// Everything the decision is accepted over: relevant open objections (with
/// policy-required acknowledgments), uncertain comparisons and conditional
/// hard criteria. Acceptance under disagreement is allowed; silence is not.
fn acceptance_challenges(
    s: &State,
    target: &Record,
    b: &DecisionBody,
    acknowledgments: &[(RevisionRef, String)],
    pack_policy: &Consulted,
) -> Result<AcceptanceChallenges, Failure> {
    let case = target.header.case_id;
    let policy = acknowledgment_policy(pack_policy);
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
/// Apply the pinned pack policy for `decision.propose` to a new proposal.
fn proposal_gate(
    c: &mut OperationContext<'_>,
    mut b: DecisionBody,
) -> Result<DecisionBody, Failure> {
    let policy = pack_policy::consult(c, Gate::DecisionPropose)?;
    b.gate_policy = None;
    let gated = policy.enforce(
        c.state(),
        c.actor(),
        &Subject {
            body: &body(b.clone())?,
            record: None,
        },
    )?;
    for warning in policy.warnings() {
        c.warn(warning);
    }
    b.gate_policy = Some(gated);
    Ok(b)
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
/// What [`pareto`] reads of one case, in one pass over its records: the
/// active criteria of every question and the verified cells of every
/// (option, criterion) pair, each list in record order. Neither depends on
/// the question asked, so every question of the case shares them.
#[derive(Debug)]
pub(crate) struct ParetoInputs {
    criteria: BTreeMap<Id, Vec<(RevisionRef, CriterionBody)>>,
    cells: BTreeMap<(Id, Id), Vec<(RevisionRef, ComparisonBody)>>,
}

impl ParetoInputs {
    /// The inputs of `case`, or the first criterion or comparison of the case,
    /// in record order, that does not decode.
    pub(crate) fn read(s: &State, case: Id) -> Result<Self, Failure> {
        let mut criteria: BTreeMap<Id, Vec<(RevisionRef, CriterionBody)>> = BTreeMap::new();
        let mut cells: BTreeMap<(Id, Id), Vec<(RevisionRef, ComparisonBody)>> = BTreeMap::new();
        for r in s.records().filter(|r| r.header.case_id == case) {
            match r.header.id.kind() {
                Kind::Criterion => {
                    let c: CriterionBody = decode(&r.body)?;
                    if c.lifecycle == CriterionLifecycle::Active {
                        criteria
                            .entry(c.question_ref.id)
                            .or_default()
                            .push((r.reference(), c));
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
        Ok(Self { criteria, cells })
    }
}

/// Weight-free and conservative: unknown, stale, multiple, subjective, and
/// noncomparable cells never prove dominance. This is advisory, not a gate.
/// Computed once per question per snapshot, from inputs read once per case.
pub fn pareto(s: &State, case: Id, question: Id) -> Result<Vec<ParetoOption>, Failure> {
    s.pareto_table(case, question, || pareto_of(s, case, question))
}

fn pareto_of(s: &State, case: Id, question: Id) -> Result<Vec<ParetoOption>, Failure> {
    let scope = MembershipScope::Question {
        case_id: case,
        question_id: question,
        option_id: None,
    };
    let opts = membership(s, MembershipRule::EligibleOptions, scope)?.members;
    let inputs = s.pareto_inputs(case);
    let ParetoInputs { criteria, cells } = match inputs.as_ref() {
        Ok(inputs) => inputs,
        Err(failure) => return Err(failure.clone()),
    };
    let criteria = criteria.get(&question).map_or(&[][..], Vec::as_slice);
    let mut result = Vec::new();
    for member in &opts {
        let option = s
            .current(member.id)
            .ok_or_else(|| fail("/option", "Option disappeared."))?
            .reference();
        let mut determining = Vec::new();
        let mut uncertain = Vec::new();
        let mut disqualified = false;
        for (cr, c) in criteria {
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
        // The option's own cell on each criterion, the same against every rival.
        let own: Vec<Option<&[(RevisionRef, ComparisonBody)]>> = criteria
            .iter()
            .map(|(cr, _)| cells.get(&(option.id, cr.id)).map(Vec::as_slice))
            .collect();
        for other in &opts {
            if other.id == option.id {
                continue;
            }
            let mut better = false;
            let mut inferior = false;
            let mut comparable = !criteria.is_empty();
            let mut pair_cells = Vec::new();
            for ((cr, c), b) in criteria.iter().zip(&own) {
                if c.option_refs.as_ref().is_some_and(|v| {
                    !v.iter().any(|x| x.id == option.id) || !v.iter().any(|x| x.id == other.id)
                }) {
                    comparable = false;
                    break;
                }
                // Both cells must be single; the option's own is checked first
                // because it needs no lookup.
                let Some([(_, bc)]) = b else {
                    comparable = false;
                    break;
                };
                let Some([(ar, ac)]) = cells.get(&(other.id, cr.id)).map(Vec::as_slice) else {
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
    // A template that commits with only the question replaced. The rules for
    // a select choice (its option among compared_alternatives) and for
    // reversal links are in the field descriptions (bn-29t7).
    let example = json!({"question_ref":q,"choice":{"kind":"defer","reason":"Await data"},"compared_alternatives":[],"comparison_refs":[],"hard_conditions":[],"evidence_refs":[],"assumption_refs":[],"tradeoffs":[],"consequences":["Choice delayed"],"reopen_trigger":"Measurement available","reversal_conditions":[]});
    vec![
        OperationDescriptor::active(
            OperationInfo {
                when_to_use: "Record a decision and its reviewed basis. A select choice lists its option in compared_alternatives at the same revision ({\"choice\": {\"kind\": \"select\", \"option_ref\": OPT}, \"compared_alternatives\": [OPT, ...]}), and each reversal condition links an assumption_ref, criterion_ref or comparison_ref ({\"condition\": \"...\", \"criterion_ref\": CRIT}).",
                ..info("decision.propose", "Propose a decision.")
            },
            example.clone(),
            ok,
            |p: DecisionInput, c| {
                let b = proposal_gate(c, normalize(c, p)?)?;
                warn_unconditioned_hard(c, &b);
                warn_ineligible_choice(c, &b)?;
                warn_other_proposals(c, &b);
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
                crate::confidence::require_cited_reasons(
                    c.state(),
                    &b.evidence_refs,
                    "/decision_ref",
                )?;
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
                let policy = pack_policy::consult(c, Gate::DecisionAccept)?;
                let challenges =
                    acceptance_challenges(c.state(), &target, &b, &acknowledgments, &policy)?;
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
                b.gate_policy = None;
                let actor = c.actor().clone();
                let mut gated = policy.enforce(
                    c.state(),
                    &actor,
                    &Subject {
                        body: &body(b.clone())?,
                        record: Some(&target),
                    },
                )?;
                for warning in policy.warnings() {
                    c.warn(warning);
                }
                gated.guardrail =
                    guardrails::enforce(c, GuardedOperation::DecisionAccept, &target)?;
                b.gate_policy = Some(gated);
                c.update(p.decision_ref, body(b)?)?;
                let id = p.decision_ref.id;
                c.check_final(move |s| {
                    policy.recheck(s, &actor, id)?;
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
            info(
                "decision.supersede",
                "Replace an accepted or proposed decision with a new proposal.",
            ),
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
                // A proposal is replaced the same way as an accepted
                // decision: the old one is kept, marked superseded, with the
                // reason and a link to its successor (bn-30vf). There is no
                // withdrawal without a successor; a `defer` replacement
                // records that no option is chosen.
                if previous.lifecycle == DecisionLifecycle::Superseded {
                    return Err(fail(
                        "/decision_ref",
                        "This decision is already superseded. Supersede its successor instead.",
                    ));
                }
                let mut replacement = proposal_gate(c, normalize(c, p.replacement)?)?;
                warn_ineligible_choice(c, &replacement)?;
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
    fn a_proposal_that_ignores_an_unmeasured_hard_criterion_is_named() {
        let (mut s, case, q, o, d, _) = fixture();
        let k = id("crit", 30);
        add(&mut s, rec(k, case, 6, criterion(q)));
        let mut b: DecisionBody = decode(&s.current(d).unwrap().body).unwrap();
        let current = RevisionRef { id: k, revision: 1 };
        assert_eq!(unconditioned_hard(&s, case, &b), vec![current]);
        // A cited cell with a known outcome against the current revision
        // addresses the criterion; one against an earlier revision does not.
        let cell = id("cmp", 31);
        let comparison = json!({"option_ref":{"id":o,"revision":1},"criterion_ref":current,"outcome":"meets","method":"Benchmark","value":{"decimal":"20","unit":"ms"},"basis":null,"evidence_refs":[],"assumption_refs":[],"limitations":null,"uncertainty":"One run.","rationale":"Below the limit.","retracted":false,"change_reason":null});
        add(&mut s, rec(cell, case, 7, comparison));
        b.comparison_refs = vec![RevisionRef {
            id: cell,
            revision: 1,
        }];
        assert!(unconditioned_hard(&s, case, &b).is_empty());
        let mut stricter = criterion(q);
        stricter["threshold"]["value"]["decimal"] = json!("15");
        update_body(&mut s, k, stricter);
        let revised = RevisionRef { id: k, revision: 2 };
        assert_eq!(unconditioned_hard(&s, case, &b), vec![revised]);
        // A stated hard condition on the current revision acknowledges it.
        b.hard_conditions.push(HardCondition {
            criterion_ref: revised,
            reason: "Accept only if remeasured below the new limit".into(),
        });
        assert!(unconditioned_hard(&s, case, &b).is_empty());
        b.choice = Choice::Defer {
            reason: "Await data".into(),
        };
        b.hard_conditions.clear();
        assert!(unconditioned_hard(&s, case, &b).is_empty());
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
                            // A material correction: an unchanged body would be a
                            // benign successor that keeps the cell current.
                            let mut corrected = observation;
                            corrected["value"]["value"] = json!((value + 1).to_string());
                            update_body(&mut state, observation_id, corrected);
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
    fn completion_only_statistic_on_the_cited_observation_leaves_the_cell_unverified() {
        let rows = [[Cell::Measured(10); 3]];
        let (mut state, case, _) = generated_table(&rows, &[0]);
        let observation_id = id("obs", 1200);
        let comparison_id = id("cmp", 1300);
        let artifact_revision = state.current(id("art", 903)).unwrap().header.revision.0;
        let observation = state.current(observation_id).unwrap().body.clone();
        let count = |value: u64| {
            json!({"count": {"state": "known", "value": value},
                "definition": {"state": "known", "value": "requests"}})
        };
        let statistic = json!({"state": "known", "value": {"kind": "quantile", "quantile": "0.99",
            "population": {"state": "known", "value": "completed_attempts"}}});
        // Completed attempts only with five timed out: the value cannot settle
        // the cell, though the Comparison declares no statistic or sample count.
        // With every started attempt observed, the same statistic can.
        for (observed, timed_out, expected) in [(95, 5, false), (100, 0, true)] {
            let mut stored = observation.clone();
            stored["validity"] = json!("valid");
            stored["run_ref"] = Value::Null;
            stored["basis"]["artifact_ref"]["revision"] = json!(artifact_revision);
            stored["measurement"] = json!({
                "statistic": statistic,
                "attempts": {"started": count(100), "observed": count(observed),
                    "timed_out": count(timed_out)},
            });
            update_body(&mut state, observation_id, stored);
            let mut cell = state.current(comparison_id).unwrap().body.clone();
            let revision = state.current(observation_id).unwrap().header.revision.0;
            cell["evidence_refs"] = json!([{"id":observation_id,"revision":revision}]);
            let cell: ComparisonBody = decode(&cell).unwrap();
            assert!(cell.value.as_ref().unwrap().statistic.is_none());
            assert!(cell.value.as_ref().unwrap().sample_count.is_none());
            assert_eq!(verified(&state, case, &cell), expected, "{observed}");
        }
    }

    #[test]
    fn pareto_is_computed_once_per_snapshot_and_again_after_any_change() {
        let rows = [
            [Cell::Measured(10), Cell::Measured(5), Cell::Measured(10)],
            [Cell::Measured(20), Cell::Measured(20), Cell::Measured(20)],
        ];
        let (mut state, case, question) = generated_table(&rows, &[0, 1]);
        let table = |state: &State, question| {
            serde_json::to_value(pareto(state, case, question).unwrap()).unwrap()
        };
        let statuses = |state: &State| -> Vec<ParetoStatus> {
            pareto(state, case, question)
                .unwrap()
                .into_iter()
                .map(|option| option.status)
                .collect()
        };
        let first = table(&state, question);
        assert_eq!(
            statuses(&state),
            [
                ParetoStatus::ParetoFrontier,
                ParetoStatus::StrictlyDominated
            ]
        );
        // A repeated question reads the memo; an uncached replay of the same
        // snapshot computes the same table.
        assert_eq!(table(&state, question), first);
        assert_eq!(table(&state.as_of(state.sequence()), question), first);
        // Another question of the case has its own table and leaves this one.
        let other = id("q", 950);
        let next = state.sequence() + 1;
        add(&mut state, rec(other, case, next, json!({})));
        assert_eq!(table(&state, other), json!([]));
        assert_eq!(table(&state, question), first);
        // Both questions read the case's inputs from one pass.
        assert!(std::sync::Arc::ptr_eq(
            &state.pareto_inputs(case),
            &state.pareto_inputs(case)
        ));
        // A material correction of the dominating option's evidence leaves its
        // cell unverified: the memo of the earlier snapshot is not reused.
        let observation = id("obs", 1201);
        let mut corrected = state.current(observation).unwrap().body.clone();
        corrected["value"]["value"] = json!("6");
        update_body(&mut state, observation, corrected);
        assert_eq!(
            statuses(&state),
            [
                ParetoStatus::UncertainFrontier,
                ParetoStatus::UncertainFrontier
            ]
        );
        assert_eq!(
            table(&state, question),
            table(&state.as_of(state.sequence()), question)
        );
        // A case whose comparison does not decode fails every question alike.
        let mut broken = state.current(id("cmp", 1300)).unwrap().body.clone();
        broken["outcome"] = json!("sideways");
        update_body(&mut state, id("cmp", 1300), broken);
        let failure = pareto(&state, case, question).unwrap_err();
        assert_eq!(pareto(&state, case, other).unwrap_err(), failure);
        assert_eq!(
            pareto(&state.as_of(state.sequence()), case, question).unwrap_err(),
            failure
        );
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
