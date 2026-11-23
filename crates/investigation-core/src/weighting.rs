//! Optional weights on soft criteria and the weighted analysis read (§15.1).
//!
//! A weighted score is an optional analytic artifact supplied by people, never
//! the core's truth criterion. The rules:
//!
//! - **A weight needs a range.** A weight is used in a score only with the
//!   worst and best values it trades off (`range`), in the criterion's declared
//!   unit and preferred direction. A weight without a range is stored as a
//!   `stated_preference`: kept, attributed and shown, never scored.
//! - **Scale anchors, not option-set normalization.** A cell's utility is its
//!   position on the stated range, `(value - worst) / (best - worst)`. Adding,
//!   removing or duplicating an option never changes another option's utility,
//!   so it cannot reorder the others. Weights are used as given, not rescaled.
//! - **Hard criteria are constraints.** They take no weight. An option that
//!   fails a hard criterion, or whose hard result is not a current verified
//!   `meets`, is excluded from every score, so a soft score cannot compensate
//!   for it.
//! - **Consequences first.** The analysis leads with the consequences table:
//!   each option's recorded cell for each criterion with its evidence, and a
//!   preference or qualitative basis labelled as such, never as a measurement.
//!   Declared overlaps between criteria are shown with it.
//! - **Sensitivity, not one number.** Each pair of scored options reports
//!   whether its order holds across every weight's stated uncertainty.
//!
//! Cells enter a score under the same evidence standard as the weight-free
//! Pareto status: one current, retained, verified measurement. The core never
//! proposes a weight, supplies no default, and never converts units.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    basis::{MembershipRule, MembershipScope, membership},
    command::{Reference, RevisionRef},
    decision_ops::{EffectiveComparison, effective_comparison_with, verified},
    evaluate::{
        ComparisonBasis, ComparisonOutcome, CriterionCreate, CriterionKind, CriterionLifecycle,
        PreferredDirection,
    },
    evaluate_ops::{ComparisonBody, CriterionBody, OptionBody},
    freshness::FreshnessIndex,
    identity::{Id, Kind},
    input::{DecimalString, Measurement},
    state::{OperationContext, Record, State},
    wire::{ErrorCode, Failure},
};

/// Version of the rules in this module.
pub const RULE_VERSION: &str = "weighted-analysis.v1";

/// The normalization assumption every weighted analysis states.
pub const NORMALIZATION: &str = "None by option set. A cell's utility is (value - worst) / (best - worst) on the range its criterion's weight states, so adding, removing or duplicating an option never changes another option's utility. Weights are used as given and are not rescaled to sum to one. A value outside the stated range is not scored.";

/// The worst and best values a weight trades off, in the criterion's unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WeightRange {
    pub worst: DecimalString,
    pub best: DecimalString,
}

/// The stated uncertainty of a weight value; sensitivity varies the weight
/// across it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WeightUncertainty {
    pub low: DecimalString,
    pub high: DecimalString,
}

/// An optional weight on a soft criterion, supplied by people.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CriterionWeightInput {
    /// A positive weight for the swing from `range.worst` to `range.best`.
    pub value: DecimalString,
    /// Without a range the weight is stored as a stated preference and never
    /// enters a score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<WeightRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<WeightUncertainty>,
    /// Why this swing is worth this weight.
    pub rationale: String,
    /// Whose preference this is, when not the recorder's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stated_by: Option<String>,
    /// Where the preference was stated: Sources, Artifacts or Observations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<Reference>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WeightStatus {
    /// Has a stated range; may enter a score.
    Ranged,
    /// No stated range; shown, never scored.
    StatedPreference,
}

/// A stored weight. A weight is always a preference, never a measurement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CriterionWeight {
    pub value: DecimalString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<WeightRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<WeightUncertainty>,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stated_by: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<RevisionRef>,
    pub status: WeightStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverlapRelation {
    /// Both criteria measure partly the same thing.
    Overlaps,
    /// This criterion stands in for the other one.
    ProxyFor,
}

/// An investigator's declaration that a criterion overlaps or proxies another.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CriterionOverlapInput {
    pub criterion_ref: Reference,
    pub relation: OverlapRelation,
    pub statement: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CriterionOverlap {
    pub criterion_id: Id,
    pub relation: OverlapRelation,
    pub statement: String,
}

fn invalid(path: &str, message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(path)
}

fn text(value: &str, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() || value.chars().count() > 2048 {
        Err(invalid(
            path,
            "Supply nonempty text of at most 2048 characters.",
        ))
    } else {
        Ok(())
    }
}

fn zero() -> DecimalString {
    DecimalString::try_from("0".to_owned()).expect("zero is a decimal")
}

/// Validate a weight against its criterion. Without a range it becomes a
/// stated preference.
pub(crate) fn normalize_weight(
    ctx: &OperationContext<'_>,
    criterion: &CriterionCreate,
    input: &CriterionWeightInput,
) -> Result<CriterionWeight, Failure> {
    use std::cmp::Ordering::{Greater, Less};
    if criterion.kind == CriterionKind::Hard {
        return Err(invalid(
            "/weight",
            "A hard criterion is a constraint, not a tradeoff; it takes no weight.",
        ));
    }
    if input.value.numeric_cmp(&zero()) != Greater {
        return Err(invalid("/weight/value", "A weight must be positive."));
    }
    text(&input.rationale, "/weight/rationale")?;
    if let Some(stated_by) = &input.stated_by {
        text(stated_by, "/weight/stated_by")?;
    }
    if let Some(range) = &input.range {
        if criterion.scale.unit.is_none() {
            return Err(invalid(
                "/weight/range",
                "A weight range is stated in the criterion's declared unit; declare one.",
            ));
        }
        let order = range.best.numeric_cmp(&range.worst);
        let expected = match criterion.preferred_direction {
            PreferredDirection::Higher => Greater,
            PreferredDirection::Lower => Less,
            PreferredDirection::Target | PreferredDirection::Unordered => {
                return Err(invalid(
                    "/weight/range",
                    "A weight range needs a higher or lower preferred direction.",
                ));
            }
        };
        if order != expected {
            return Err(invalid(
                "/weight/range",
                "The best value must differ from the worst in the preferred direction.",
            ));
        }
    }
    if let Some(uncertainty) = &input.uncertainty
        && (uncertainty.low.numeric_cmp(&zero()) == Less
            || uncertainty.low.numeric_cmp(&input.value) == Greater
            || uncertainty.high.numeric_cmp(&input.value) == Less)
    {
        return Err(invalid(
            "/weight/uncertainty",
            "State a nonnegative low and a high that bracket the weight.",
        ));
    }
    let mut source_refs = Vec::new();
    for reference in &input.source_refs {
        let path = "/weight/source_refs";
        let resolved = ctx.resolve_reference(reference, path)?;
        if !matches!(
            resolved.id.kind(),
            Kind::Source | Kind::Artifact | Kind::Observation
        ) {
            return Err(invalid(
                path,
                "Cite the Source, Artifact or Observation that states the preference.",
            ));
        }
        if ctx.current(resolved.id).map(Record::reference) != Some(resolved) {
            return Err(Failure::new(ErrorCode::RevisionConflict, "Reference is stale.").at(path));
        }
        if source_refs.contains(&resolved) {
            return Err(invalid(path, "Duplicate reference."));
        }
        source_refs.push(resolved);
    }
    Ok(CriterionWeight {
        value: input.value.clone(),
        range: input.range.clone(),
        uncertainty: input.uncertainty.clone(),
        rationale: input.rationale.clone(),
        stated_by: input.stated_by.clone(),
        source_refs,
        status: if input.range.is_some() {
            WeightStatus::Ranged
        } else {
            WeightStatus::StatedPreference
        },
    })
}

/// Resolve declared overlaps to active criteria of the same question.
pub(crate) fn normalize_overlaps(
    ctx: &OperationContext<'_>,
    question: Id,
    inputs: &[CriterionOverlapInput],
) -> Result<Vec<CriterionOverlap>, Failure> {
    let mut out: Vec<CriterionOverlap> = Vec::new();
    for (index, input) in inputs.iter().enumerate() {
        let path = format!("/overlaps/{index}/criterion_ref");
        text(&input.statement, &format!("/overlaps/{index}/statement"))?;
        let resolved = ctx.resolve(&input.criterion_ref, Kind::Criterion, &path)?;
        let other = ctx
            .current(resolved.id)
            .and_then(|record| serde_json::from_value::<CriterionBody>(record.body.clone()).ok())
            .ok_or_else(|| invalid(&path, "Reference is not a criterion of this case."))?;
        if other.question_ref.id != question || other.lifecycle != CriterionLifecycle::Active {
            return Err(invalid(
                &path,
                "Declare overlap only with an active criterion of the same question.",
            ));
        }
        if out.iter().any(|known| known.criterion_id == resolved.id) {
            return Err(invalid(&path, "Duplicate overlap."));
        }
        out.push(CriterionOverlap {
            criterion_id: resolved.id,
            relation: input.relation,
            statement: input.statement.clone(),
        });
    }
    Ok(out)
}

/// The receipt warning for a weight stored as a stated preference.
pub(crate) fn warn_stated_preference(ctx: &mut OperationContext<'_>, body: &CriterionBody) {
    if body
        .weight
        .as_ref()
        .is_some_and(|weight| weight.status == WeightStatus::StatedPreference)
    {
        ctx.warn(
            "Weight recorded without a range (criterion.weight_without_range): it is kept as a stated preference and never enters a weighted score. State the worst and best values it trades off with criterion.amend."
                .to_owned(),
        );
    }
}

/// What the consequences table shows for one option and criterion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CellState {
    /// One current cell with a verified measurement.
    Measured,
    /// One current cell with a measurement its evidence does not verify.
    MeasuredUnverified,
    /// A stated preference: never a measurement.
    Preference,
    /// A qualitative judgment: never a measurement.
    Qualitative,
    Unknown,
    NotApplicable,
    /// No comparison is recorded.
    Missing,
    /// More than one current comparison; resolve explicitly.
    Multiple,
    /// The comparison cites an older option or criterion revision, or its
    /// basis changed.
    NeedsReview,
    /// The criterion is scoped to other options.
    OutOfScope,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct TableCell {
    pub criterion_ref: RevisionRef,
    pub state: CellState,
    pub comparison_refs: Vec<RevisionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ComparisonOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Measurement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basis: Option<ComparisonBasis>,
    pub evidence_refs: Vec<RevisionRef>,
    pub assumption_refs: Vec<RevisionRef>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct TableRow {
    pub option_ref: RevisionRef,
    pub description: String,
    pub cells: Vec<TableCell>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct TableCriterion {
    pub criterion_ref: RevisionRef,
    pub name: String,
    pub kind: CriterionKind,
    pub unit: Option<String>,
    pub preferred_direction: PreferredDirection,
    pub weight: Option<WeightStatus>,
}

/// Raw recorded outcomes, before any weighting.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ConsequencesTable {
    pub criteria: Vec<TableCriterion>,
    pub rows: Vec<TableRow>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct OverlapDeclaration {
    pub criterion_ref: RevisionRef,
    pub other_criterion_id: Id,
    pub relation: OverlapRelation,
    pub statement: String,
    /// Both carry ranged weights, so a score may count one concern twice.
    pub both_weighted: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct WeightEntry {
    pub criterion_ref: RevisionRef,
    pub unit: Option<String>,
    pub weight: CriterionWeight,
    pub enters_score: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// A current verified cell fails a hard criterion.
    HardFailed,
    /// A hard criterion has no current verified `meets`; unknown is not a pass.
    HardUnresolved,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ExcludedOption {
    pub option_ref: RevisionRef,
    pub reason: ExclusionReason,
    pub criterion_refs: Vec<RevisionRef>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct Contribution {
    pub criterion_ref: RevisionRef,
    pub comparison_ref: RevisionRef,
    pub value: Measurement,
    /// Position on the stated range: 0 at worst, 1 at best.
    pub utility: f64,
    pub weight: DecimalString,
    pub weighted: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MissingReason {
    /// The cell is not one current verified measurement; see the table.
    NotMeasured,
    /// The value lies outside the range the weight states.
    OutsideRange,
    /// The criterion is scoped to other options.
    OutOfScope,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MissingCell {
    pub criterion_ref: RevisionRef,
    pub reason: MissingReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScoreStatus {
    Scored,
    /// Some ranged criterion has no scorable cell; no total is given.
    Incomplete,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct OptionScore {
    pub option_ref: RevisionRef,
    pub status: ScoreStatus,
    /// Present only when scored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
    /// The total with every weight at the low end of its stated uncertainty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_low: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_high: Option<f64>,
    pub contributions: Vec<Contribution>,
    pub missing: Vec<MissingCell>,
}

/// Whether the order of two scored options holds across every weight's
/// stated uncertainty. Values are fixed; only weights vary.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct PairSensitivity {
    pub option_refs: [RevisionRef; 2],
    /// First total minus second, at the stated weights.
    pub difference: f64,
    pub lowest_difference: f64,
    pub highest_difference: f64,
    /// The higher-scoring option at the stated weights; none for a tie.
    pub preferred_at_stated_weights: Option<RevisionRef>,
    /// True when some weights within their stated uncertainty tie or reverse
    /// the order.
    pub order_can_flip: bool,
}

/// The weighted analysis of one decision question. Keys sort with the
/// consequences table first.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct WeightedAnalysis {
    pub consequences: ConsequencesTable,
    pub excluded: Vec<ExcludedOption>,
    pub normalization: &'static str,
    pub overlaps: Vec<OverlapDeclaration>,
    pub rule_version: &'static str,
    pub scores: Vec<OptionScore>,
    pub sensitivity: Vec<PairSensitivity>,
    /// Active soft criteria with no ranged weight: in no score.
    pub unweighted_criteria: Vec<RevisionRef>,
    pub weights: Vec<WeightEntry>,
}

fn decode<T: serde::de::DeserializeOwned>(record: &Record) -> Result<T, Failure> {
    serde_json::from_value(record.body.clone()).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Stored record has an invalid body.",
        )
    })
}

fn number(value: &DecimalString) -> f64 {
    value.as_str().parse().unwrap_or(f64::NAN)
}

fn rounded(value: f64) -> f64 {
    (value * 1e9).round() / 1e9
}

const EPSILON: f64 = 1e-12;

/// One scored cell: its utility and its weight at the stated value and at
/// each end of the stated uncertainty.
struct Term {
    utility: f64,
    stated: f64,
    low: f64,
    high: f64,
}

struct Classified {
    state: CellState,
    refs: Vec<RevisionRef>,
    body: Option<ComparisonBody>,
}

fn classify(
    s: &State,
    case: Id,
    index: &FreshnessIndex,
    option: RevisionRef,
    criterion: &(RevisionRef, CriterionBody),
    cells: &[(&Record, ComparisonBody)],
) -> Result<Classified, Failure> {
    let (criterion_ref, body) = criterion;
    if body
        .option_refs
        .as_ref()
        .is_some_and(|scope| !scope.iter().any(|r| r.id == option.id))
    {
        return Ok(Classified {
            state: CellState::OutOfScope,
            refs: Vec::new(),
            body: None,
        });
    }
    let refs: Vec<RevisionRef> = cells.iter().map(|(r, _)| r.reference()).collect();
    let [(record, cell)] = cells else {
        return Ok(Classified {
            state: if cells.is_empty() {
                CellState::Missing
            } else {
                CellState::Multiple
            },
            refs,
            body: None,
        });
    };
    let current = cell.option_ref == option
        && cell.criterion_ref == *criterion_ref
        && effective_comparison_with(s, index, record)? == EffectiveComparison::Current;
    let state = if !current {
        CellState::NeedsReview
    } else if cell.outcome == ComparisonOutcome::NotApplicable {
        CellState::NotApplicable
    } else if cell.outcome == ComparisonOutcome::Unknown
        || matches!(cell.basis, Some(ComparisonBasis::Unknown { .. }))
    {
        CellState::Unknown
    } else if matches!(cell.basis, Some(ComparisonBasis::Preference { .. })) {
        CellState::Preference
    } else if cell.value.is_none() {
        CellState::Qualitative
    } else if verified(s, case, cell) && crate::absence_gate::comparison_basis_holds(s, case, cell)
    {
        CellState::Measured
    } else {
        CellState::MeasuredUnverified
    };
    Ok(Classified {
        state,
        refs,
        body: Some(cell.clone()),
    })
}

/// Whether any active criterion of the question records a weight.
fn weighted_criteria(criteria: &[(RevisionRef, CriterionBody)]) -> bool {
    criteria.iter().any(|(_, c)| c.weight.is_some())
}

/// The active criteria of `question`, in record order.
fn active_criteria(
    s: &State,
    case: Id,
    question: Id,
) -> Result<Vec<(RevisionRef, CriterionBody)>, Failure> {
    let mut criteria = Vec::new();
    for record in s.records().filter(|r| r.header.case_id == case) {
        if record.header.id.kind() == Kind::Criterion {
            let body: CriterionBody = decode(record)?;
            if body.question_ref.id == question && body.lifecycle == CriterionLifecycle::Active {
                criteria.push((record.reference(), body));
            }
        }
    }
    Ok(criteria)
}

/// The consequences table of `question` alone: every eligible option against
/// every active criterion, with no weights and no score, whether or not any
/// criterion records a weight. Read-only and computed from one snapshot, and
/// the table [`weighted_analysis`] leads with.
pub fn consequences_table(s: &State, case: Id, question: Id) -> Result<ConsequencesTable, Failure> {
    let criteria = active_criteria(s, case, question)?;
    table_for(s, case, question, &criteria)
}

fn table_for(
    s: &State,
    case: Id,
    question: Id,
    criteria: &[(RevisionRef, CriterionBody)],
) -> Result<ConsequencesTable, Failure> {
    let scope = MembershipScope::Question {
        case_id: case,
        question_id: question,
        option_id: None,
    };
    let members = membership(s, MembershipRule::EligibleOptions, scope)?.members;
    let mut cells: BTreeMap<(Id, Id), Vec<(&Record, ComparisonBody)>> = BTreeMap::new();
    for record in s.records().filter(|r| r.header.case_id == case) {
        if record.header.id.kind() == Kind::Comparison {
            let body: ComparisonBody = decode(record)?;
            if !body.retracted {
                cells
                    .entry((body.option_ref.id, body.criterion_ref.id))
                    .or_default()
                    .push((record, body));
            }
        }
    }
    let index = FreshnessIndex::build(s, case);
    let mut rows = Vec::new();
    for member in &members {
        let record = s
            .current(member.id)
            .ok_or_else(|| Failure::new(ErrorCode::IntegrityFailure, "Option disappeared."))?;
        let option_body: OptionBody = decode(record)?;
        let option = record.reference();
        let mut row = Vec::new();
        for criterion in criteria {
            let found = cells
                .get(&(option.id, criterion.0.id))
                .map(Vec::as_slice)
                .unwrap_or_default();
            let classified = classify(s, case, &index, option, criterion, found)?;
            let cell = classified.body.as_ref();
            row.push(TableCell {
                criterion_ref: criterion.0,
                state: classified.state,
                comparison_refs: classified.refs,
                outcome: cell.map(|b| b.outcome),
                value: cell.and_then(|b| b.value.clone()),
                basis: cell.and_then(|b| b.basis.clone()),
                evidence_refs: cell.map(|b| b.evidence_refs.clone()).unwrap_or_default(),
                assumption_refs: cell.map(|b| b.assumption_refs.clone()).unwrap_or_default(),
            });
        }
        rows.push(TableRow {
            option_ref: option,
            description: option_body.description,
            cells: row,
        });
    }
    Ok(ConsequencesTable {
        criteria: criteria
            .iter()
            .map(|(r, c)| TableCriterion {
                criterion_ref: *r,
                name: c.name.clone(),
                kind: c.kind,
                unit: c.scale.unit.clone(),
                preferred_direction: c.preferred_direction,
                weight: c.weight.as_ref().map(|w| w.status),
            })
            .collect(),
        rows,
    })
}

/// The weighted analysis of `question`, or `None` when no active criterion of
/// it records a weight. Read-only and computed from one snapshot.
pub fn weighted_analysis(
    s: &State,
    case: Id,
    question: Id,
) -> Result<Option<WeightedAnalysis>, Failure> {
    let criteria = active_criteria(s, case, question)?;
    if !weighted_criteria(&criteria) {
        return Ok(None);
    }
    let table = table_for(s, case, question, &criteria)?;

    let ranged = |c: &CriterionBody| {
        c.kind == CriterionKind::Soft
            && c.weight
                .as_ref()
                .is_some_and(|w| w.status == WeightStatus::Ranged && w.range.is_some())
    };
    let mut excluded = Vec::new();
    let mut scores = Vec::new();
    // Per scored option: one term per ranged criterion, in criterion order.
    let mut scored: Vec<(RevisionRef, Vec<Term>)> = Vec::new();
    for table_row in &table.rows {
        let option = &table_row.option_ref;
        let mut failed = Vec::new();
        let mut unresolved = Vec::new();
        let mut contributions = Vec::new();
        let mut missing = Vec::new();
        let mut terms = Vec::new();
        for (criterion, cell) in criteria.iter().zip(&table_row.cells) {
            let (criterion_ref, c) = criterion;
            if c.kind == CriterionKind::Hard && cell.state != CellState::OutOfScope {
                match (cell.state, cell.outcome) {
                    (CellState::Measured, Some(ComparisonOutcome::Fails)) => {
                        failed.push(*criterion_ref);
                    }
                    (CellState::Measured, Some(ComparisonOutcome::Meets)) => {}
                    _ => unresolved.push(*criterion_ref),
                }
            }
            if ranged(c) {
                let weight = c.weight.as_ref().expect("ranged weight");
                let range = weight.range.as_ref().expect("ranged weight has a range");
                match (cell.state, cell.value.as_ref()) {
                    (CellState::OutOfScope, _) => missing.push(MissingCell {
                        criterion_ref: *criterion_ref,
                        reason: MissingReason::OutOfScope,
                    }),
                    (CellState::Measured, Some(value)) => {
                        let value = value.clone();
                        let (worst, best) = (number(&range.worst), number(&range.best));
                        let utility = (number(&value.decimal) - worst) / (best - worst);
                        if !(-EPSILON..=1.0 + EPSILON).contains(&utility)
                            || c.scale.unit.as_deref() != Some(value.unit.as_str())
                        {
                            missing.push(MissingCell {
                                criterion_ref: *criterion_ref,
                                reason: MissingReason::OutsideRange,
                            });
                        } else {
                            let stated = number(&weight.value);
                            let (low, high) = weight
                                .uncertainty
                                .as_ref()
                                .map_or((stated, stated), |u| (number(&u.low), number(&u.high)));
                            terms.push(Term {
                                utility,
                                stated,
                                low,
                                high,
                            });
                            contributions.push(Contribution {
                                criterion_ref: *criterion_ref,
                                comparison_ref: cell.comparison_refs[0],
                                value,
                                utility: rounded(utility),
                                weight: weight.value.clone(),
                                weighted: rounded(utility * stated),
                            });
                        }
                    }
                    _ => missing.push(MissingCell {
                        criterion_ref: *criterion_ref,
                        reason: MissingReason::NotMeasured,
                    }),
                }
            }
        }
        if !failed.is_empty() {
            excluded.push(ExcludedOption {
                option_ref: *option,
                reason: ExclusionReason::HardFailed,
                criterion_refs: failed,
            });
            continue;
        }
        if !unresolved.is_empty() {
            excluded.push(ExcludedOption {
                option_ref: *option,
                reason: ExclusionReason::HardUnresolved,
                criterion_refs: unresolved,
            });
            continue;
        }
        let complete = missing.is_empty() && !terms.is_empty();
        let sum = |pick: fn(&Term) -> f64| rounded(terms.iter().map(|t| t.utility * pick(t)).sum());
        scores.push(OptionScore {
            option_ref: *option,
            status: if complete {
                ScoreStatus::Scored
            } else {
                ScoreStatus::Incomplete
            },
            total: complete.then(|| sum(|t| t.stated)),
            total_low: complete.then(|| sum(|t| t.low)),
            total_high: complete.then(|| sum(|t| t.high)),
            contributions,
            missing,
        });
        if complete {
            scored.push((*option, terms));
        }
    }

    if !criteria.iter().any(|(_, c)| ranged(c)) {
        scores.clear();
    }
    let mut sensitivity = Vec::new();
    for (i, (first, a)) in scored.iter().enumerate() {
        for (second, b) in &scored[i + 1..] {
            let (mut difference, mut lowest, mut highest) = (0.0, 0.0, 0.0);
            for (x, y) in a.iter().zip(b) {
                let d = x.utility - y.utility;
                difference += d * x.stated;
                lowest += d * if d > 0.0 { x.low } else { x.high };
                highest += d * if d > 0.0 { x.high } else { x.low };
            }
            let preferred = if difference > EPSILON {
                Some(*first)
            } else if difference < -EPSILON {
                Some(*second)
            } else {
                None
            };
            sensitivity.push(PairSensitivity {
                option_refs: [*first, *second],
                difference: rounded(difference),
                lowest_difference: rounded(lowest),
                highest_difference: rounded(highest),
                preferred_at_stated_weights: preferred,
                order_can_flip: !(lowest > EPSILON || highest < -EPSILON),
            });
        }
    }

    let ids: BTreeSet<Id> = criteria
        .iter()
        .filter(|(_, c)| ranged(c))
        .map(|(r, _)| r.id)
        .collect();
    let overlaps = criteria
        .iter()
        .flat_map(|(r, c)| {
            c.overlaps.iter().map(|o| OverlapDeclaration {
                criterion_ref: *r,
                other_criterion_id: o.criterion_id,
                relation: o.relation,
                statement: o.statement.clone(),
                both_weighted: ids.contains(&r.id) && ids.contains(&o.criterion_id),
            })
        })
        .collect();
    let weights = criteria
        .iter()
        .filter_map(|(r, c)| {
            c.weight.as_ref().map(|w| WeightEntry {
                criterion_ref: *r,
                unit: c.scale.unit.clone(),
                weight: w.clone(),
                enters_score: ranged(c),
            })
        })
        .collect();
    let unweighted_criteria = criteria
        .iter()
        .filter(|(_, c)| c.kind == CriterionKind::Soft && !ranged(c))
        .map(|(r, _)| *r)
        .collect();
    Ok(Some(WeightedAnalysis {
        consequences: table,
        excluded,
        normalization: NORMALIZATION,
        overlaps,
        rule_version: RULE_VERSION,
        scores,
        sensitivity,
        unweighted_criteria,
        weights,
    }))
}

#[cfg(test)]
mod tests;
