//! Informative-absence gate and practical-equivalence margins (§6.4, §17.2).
//!
//! An Assessment or Comparison may record an `absence` block: it rests on
//! "nothing was seen". The record stays recordable whatever its basis. Until
//! the block names all three requirements below, a Conclusion that rests on it
//! cannot be promoted, and a Decision reads the Comparison as `unknown`:
//!
//! - `expected_signal`: what would have been seen if the effect, fact or
//!   difference were present;
//! - `opportunity`: completed Runs or Observations that would have shown it,
//!   with an adequacy standard the recorder stated: a minimum observation
//!   window or sample count that every cited record meets, or a search Run
//!   whose coverage states its bounds and an explicit empty result. A failed,
//!   blocked or incomplete Run is never negative evidence;
//! - a valid instrument: every cited Observation is effectively `valid` under
//!   `measurement.assess`. Where no judgment exists, `justified_equivalent`
//!   states the substitute. An `invalid` or `partial` judgment is never
//!   overridden.
//!
//! Practical equivalence compares an externally supplied interval with a
//! margin by arithmetic. A frozen inquiry may state population, unit,
//! aggregation, replication unit, timeout/exclusion policy and a margin. An
//! assessment's `equivalence` block names that inquiry, or states a margin
//! itself, and names the producer assertion on a Run that reports the
//! interval. The interval is read from the producer's recorded statements; the
//! core invents no interval, p-value or power. An interval inside the margin
//! supports bounded equivalence, one that crosses a bound leaves it
//! unresolved, and one entirely outside excludes it. The margin counts as
//! prespecified only when the referenced inquiry revision was frozen in an
//! earlier transaction than the first one that recorded the interval. A margin
//! stated later, or with the assessment, is recorded as `stated_after_results`.
//!
//! Units are compared as recorded text and never converted. The core supplies
//! no default margin, window or sample count. Whether the detection limits are
//! appropriate stays a human question.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    command::{Reference, RevisionRef},
    evaluate_ops::ComparisonBody,
    evidence::MeasurementValidity,
    header::Knowledge,
    identity::{Id, Kind},
    input::{self, DecimalString},
    measurement_fields::{MeasurementFields, Quantity, observation_fields, recorded_fields},
    review::{AssessmentAdd, AssessmentRelation},
    search_coverage::{run_coverage, states_bounds, states_empty_result},
    state::{OperationContext, Record, State},
    wire::{ErrorCode, Failure},
};

/// Version of the rules in this module.
pub const RULE_VERSION: &str = "absence-gate.v1";
/// The judgment the core leaves to people.
pub const HUMAN_QUESTION: &str = "Are detection limits appropriate?";
/// Assessment metadata key holding the equivalence evaluation made at acceptance.
pub const EQUIVALENCE_EVALUATION_KEY: &str = "equivalence_evaluation";

const TEXT_LIMIT: usize = 2048;
const MAX_REFS: usize = 32;
const MAX_WALK: usize = 128;

/// The basis of a record that treats "nothing was seen" as evidence. Every
/// field is optional so that the absence itself is always recordable; the gate
/// reports what is missing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AbsenceBasis {
    /// What would have been observed if the effect, fact or difference were
    /// present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_signal: Option<String>,
    /// The sampling or search that would have shown the signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opportunity: Option<AbsenceOpportunity>,
    /// A justified substitute for an instrument or control validity
    /// judgment, used only where the cited Observations have none. It never
    /// overrides an `invalid` or `partial` judgment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justified_equivalent: Option<String>,
    /// The recorder's statement of the detection limits. Shown with the human
    /// question; the core does not judge it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_limit: Option<String>,
}

/// The Runs or Observations that would have shown the expected signal, and
/// the adequacy standard they had to meet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AbsenceOpportunity {
    pub description: String,
    /// Exact Run or Observation revisions.
    #[serde(default)]
    pub refs: Vec<Reference>,
    /// Smallest observation window each cited record must record, in the
    /// recorded unit. Units are not converted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_observation_window: Option<Quantity>,
    /// Smallest sample count each cited record must record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_sample_count: Option<u64>,
}

/// A practical-equivalence specification stated on an inquiry plan before
/// results. Freezing the plan fixes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EquivalencePlan {
    pub population: String,
    pub unit: String,
    pub aggregation: String,
    pub replication_unit: String,
    pub timeout_exclusion_policy: String,
    pub margin: Margin,
}

/// A decision-relevant band, in the plan's unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Margin {
    pub lower: DecimalString,
    pub upper: DecimalString,
}

/// A margin stated with the assessment. It is always recorded as stated after
/// the interval and never counts as prespecified.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatedMargin {
    pub unit: String,
    pub lower: DecimalString,
    pub upper: DecimalString,
}

/// The producer assertion that reports the interval: a Run revision and the
/// assertion name. The assertion value must hold `lower` and `upper` as
/// numbers or decimal strings, and the assertion must record its unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IntervalSource {
    pub run_ref: Reference,
    pub assertion: String,
}

/// An assessment of practical equivalence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EquivalenceBasis {
    /// A frozen inquiry whose `equivalence` block states the margin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inquiry_ref: Option<Reference>,
    /// A margin stated now, instead of an inquiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stated_margin: Option<StatedMargin>,
    pub interval: IntervalSource,
}

/// What a Conclusion claims when it asserts that something is absent or that
/// two conditions are practically equivalent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClaimForm {
    Absence,
    Equivalence,
}

/// One unmet requirement, with the records it read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateItem {
    pub requirement: String,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<RevisionRef>,
}

fn item(
    requirement: &str,
    code: &str,
    message: impl Into<String>,
    refs: Vec<RevisionRef>,
) -> GateItem {
    GateItem {
        requirement: requirement.into(),
        code: code.into(),
        message: message.into(),
        refs,
    }
}

/// The absence gate's reading of one record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbsenceEvaluation {
    pub rule_version: String,
    pub satisfied: bool,
    /// `inconclusive` while a requirement is unmet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling: Option<String>,
    pub missing: Vec<GateItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_limit: Option<String>,
    pub human_question: String,
}

/// When the margin was stated relative to the interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarginTiming {
    Prespecified,
    StatedAfterResults,
    NotFrozen,
    Missing,
}

/// Arithmetic outcome of comparing the interval with the margin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EquivalenceOutcome {
    BoundedEquivalenceSupported,
    EquivalenceUnresolved,
    EquivalenceExcluded,
    NotEvaluable,
}

/// The margin as read, with where and when it was stated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarginRead {
    pub lower: DecimalString,
    pub upper: DecimalString,
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inquiry_ref: Option<RevisionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stated_at_sequence: Option<u64>,
}

/// The interval as read from the producer's recorded statement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntervalRead {
    pub lower: DecimalString,
    pub upper: DecimalString,
    pub unit: String,
    pub run_ref: RevisionRef,
    pub assertion: String,
    /// Acceptance sequence of the first Run revision that recorded this
    /// interval.
    pub first_recorded_at_sequence: u64,
}

/// The equivalence gate's reading of one assessment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EquivalenceEvaluation {
    pub rule_version: String,
    pub outcome: EquivalenceOutcome,
    pub margin_timing: MarginTiming,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin: Option<MarginRead>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<IntervalRead>,
    pub missing: Vec<GateItem>,
}

impl EquivalenceEvaluation {
    /// Bounded equivalence is supported by a prespecified margin.
    pub fn holds(&self) -> bool {
        self.outcome == EquivalenceOutcome::BoundedEquivalenceSupported
            && self.margin_timing == MarginTiming::Prespecified
            && self.missing.is_empty()
    }

    /// Every reason the evaluation does not support bounded equivalence.
    pub fn reasons(&self) -> Vec<GateItem> {
        let mut reasons = self.missing.clone();
        match self.margin_timing {
            MarginTiming::Prespecified | MarginTiming::Missing => {}
            MarginTiming::StatedAfterResults => reasons.push(item(
                "margin",
                "equivalence.margin_not_prespecified",
                "The margin was stated after the interval was recorded and does not count as prespecified. Freeze an inquiry with the margin before the results are recorded.",
                self.margin.iter().filter_map(|margin| margin.inquiry_ref).collect(),
            )),
            MarginTiming::NotFrozen => reasons.push(item(
                "margin",
                "equivalence.margin_not_frozen",
                "The inquiry stating the margin is not frozen. Only a frozen margin can be prespecified.",
                self.margin.iter().filter_map(|margin| margin.inquiry_ref).collect(),
            )),
        }
        let interval = self
            .interval
            .iter()
            .map(|interval| interval.run_ref)
            .collect();
        match self.outcome {
            EquivalenceOutcome::BoundedEquivalenceSupported | EquivalenceOutcome::NotEvaluable => {}
            EquivalenceOutcome::EquivalenceUnresolved => reasons.push(item(
                "interval",
                "equivalence.interval_crosses_margin",
                "The interval crosses a margin bound, so equivalence is unresolved.",
                interval,
            )),
            EquivalenceOutcome::EquivalenceExcluded => reasons.push(item(
                "interval",
                "equivalence.interval_outside_margin",
                "The interval lies outside the margin, so it does not support equivalence.",
                interval,
            )),
        }
        reasons
    }
}

fn invalid(path: &str, message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(path)
}

fn text(value: &str, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() {
        return Err(invalid(path, "Omit this field or supply nonblank text."));
    }
    if value.len() > TEXT_LIMIT {
        return Err(invalid(path, "Shorten this text to at most 2048 bytes."));
    }
    Ok(())
}

fn optional_text(value: Option<&String>, path: &str) -> Result<(), Failure> {
    value.map_or(Ok(()), |value| text(value, path))
}

fn band(lower: &DecimalString, upper: &DecimalString, path: &str) -> Result<(), Failure> {
    if lower.numeric_cmp(upper).is_ge() {
        return Err(invalid(
            path,
            "State a margin whose lower bound is below its upper bound.",
        ));
    }
    Ok(())
}

/// Shape checks for an `absence` block at `path`.
pub fn absence_shape(basis: &AbsenceBasis, path: &str) -> Result<(), Failure> {
    optional_text(
        basis.expected_signal.as_ref(),
        &format!("{path}/expected_signal"),
    )?;
    optional_text(
        basis.justified_equivalent.as_ref(),
        &format!("{path}/justified_equivalent"),
    )?;
    optional_text(
        basis.detection_limit.as_ref(),
        &format!("{path}/detection_limit"),
    )?;
    if let Some(opportunity) = &basis.opportunity {
        text(
            &opportunity.description,
            &format!("{path}/opportunity/description"),
        )?;
        if opportunity.refs.len() > MAX_REFS {
            return Err(invalid(
                &format!("{path}/opportunity/refs"),
                "Cite at most 32 records.",
            ));
        }
        if let Some(window) = &opportunity.minimum_observation_window {
            text(
                &window.unit,
                &format!("{path}/opportunity/minimum_observation_window/unit"),
            )?;
        }
    }
    Ok(())
}

/// Shape checks for an inquiry plan's `equivalence` block.
pub fn plan_shape(plan: &EquivalencePlan, path: &str) -> Result<(), Failure> {
    for (field, value) in [
        ("population", &plan.population),
        ("unit", &plan.unit),
        ("aggregation", &plan.aggregation),
        ("replication_unit", &plan.replication_unit),
        ("timeout_exclusion_policy", &plan.timeout_exclusion_policy),
    ] {
        text(value, &format!("{path}/{field}"))?;
    }
    band(
        &plan.margin.lower,
        &plan.margin.upper,
        &format!("{path}/margin"),
    )
}

/// Shape checks for an assessment's `equivalence` block.
pub fn equivalence_shape(basis: &EquivalenceBasis, path: &str) -> Result<(), Failure> {
    if basis.inquiry_ref.is_some() && basis.stated_margin.is_some() {
        return Err(invalid(
            path,
            "Name either a frozen inquiry or a stated margin, not both.",
        ));
    }
    if let Some(margin) = &basis.stated_margin {
        text(&margin.unit, &format!("{path}/stated_margin/unit"))?;
        band(
            &margin.lower,
            &margin.upper,
            &format!("{path}/stated_margin"),
        )?;
    }
    text(
        &basis.interval.assertion,
        &format!("{path}/interval/assertion"),
    )
}

/// Shape checks for the optional blocks of `assessment.add`.
pub fn assessment_shape(payload: &AssessmentAdd) -> Result<(), Failure> {
    if let Some(absence) = &payload.absence {
        absence_shape(absence, "/absence")?;
    }
    if let Some(equivalence) = &payload.equivalence {
        equivalence_shape(equivalence, "/equivalence")?;
    }
    Ok(())
}

fn pin(
    ctx: &OperationContext<'_>,
    reference: &mut Reference,
    kinds: &[Kind],
    path: &str,
) -> Result<RevisionRef, Failure> {
    let resolved = ctx.resolve_reference(reference, path)?;
    if !kinds.contains(&resolved.id.kind()) {
        return Err(Failure::wrong_kind(
            ErrorCode::InvalidInput,
            path,
            kinds,
            resolved.id.kind(),
        ));
    }
    *reference = Reference::Existing(resolved);
    Ok(resolved)
}

/// Resolve the references of an `absence` block to exact revisions.
pub fn pin_absence(
    ctx: &OperationContext<'_>,
    basis: &mut AbsenceBasis,
    path: &str,
) -> Result<(), Failure> {
    if let Some(opportunity) = &mut basis.opportunity {
        let mut seen = BTreeSet::new();
        for (index, reference) in opportunity.refs.iter_mut().enumerate() {
            let at = format!("{path}/opportunity/refs/{index}");
            let resolved = pin(ctx, reference, &[Kind::Run, Kind::Observation], &at)?;
            if !seen.insert(resolved.id) {
                return Err(invalid(&at, "Supply only one revision per record."));
            }
        }
    }
    Ok(())
}

/// Resolve the references of the optional blocks of `assessment.add`.
pub fn pin_assessment(
    ctx: &OperationContext<'_>,
    payload: &mut AssessmentAdd,
) -> Result<(), Failure> {
    if let Some(absence) = &mut payload.absence {
        pin_absence(ctx, absence, "/absence")?;
    }
    if let Some(equivalence) = &mut payload.equivalence {
        if let Some(inquiry) = &mut equivalence.inquiry_ref {
            pin(ctx, inquiry, &[Kind::Inquiry], "/equivalence/inquiry_ref")?;
        }
        pin(
            ctx,
            &mut equivalence.interval.run_ref,
            &[Kind::Run],
            "/equivalence/interval/run_ref",
        )?;
    }
    Ok(())
}

fn exact(reference: &Reference) -> Option<RevisionRef> {
    match reference {
        Reference::Existing(reference) => Some(*reference),
        Reference::Client(_) => None,
    }
}

fn in_case(state: &State, case: Id, reference: RevisionRef) -> Option<&Record> {
    state
        .revision(reference)
        .filter(|record| record.header.case_id == case)
}

fn latest(state: &State, case: Id, id: Id) -> Option<&Record> {
    state
        .current(id)
        .filter(|record| record.header.case_id == case)
}

/// Why a Run cannot provide an opportunity, if it cannot.
fn unusable_run(run: &Record) -> Option<&'static str> {
    let body = &run.body;
    if body["execution_status"] != "completed" {
        return Some("did not complete");
    }
    if body["evidence_completeness"] != "complete" {
        return Some("has incomplete evidence");
    }
    if matches!(
        body["evidence_validity"].as_str(),
        Some("invalid" | "partial")
    ) {
        return Some("has an invalidity finding");
    }
    None
}

fn retracted(record: &Record) -> bool {
    let body = &record.body;
    body["retracted"] == true || body["status"] == "retracted" || body["lifecycle"] == "retracted"
}

/// Measurement fields that apply to an opportunity record: an Observation's
/// own fields first, then its Run's, field by field.
fn first_field<T: Clone>(
    state: &State,
    record: &Record,
    field: impl Fn(&MeasurementFields) -> Option<&Knowledge<T>>,
) -> Option<Knowledge<T>> {
    if record.header.id.kind() == Kind::Observation {
        observation_fields(state, record)
            .first(field)
            .map(|(_, value)| value.clone())
    } else {
        recorded_fields(record).and_then(|fields| field(&fields).cloned())
    }
}

/// Evaluate an `absence` block. `evidence` lists the record's own cited
/// evidence; its Observations count toward the instrument requirement.
pub fn evaluate_absence(
    state: &State,
    case: Id,
    basis: &AbsenceBasis,
    evidence: &[RevisionRef],
) -> AbsenceEvaluation {
    let mut missing = Vec::new();
    if basis
        .expected_signal
        .as_deref()
        .is_none_or(|signal| signal.trim().is_empty())
    {
        missing.push(item(
            "expected_signal",
            "absence.expected_signal_missing",
            "Name the signal that would have been observed if the effect or fact were present.",
            vec![],
        ));
    }
    let mut observations: Vec<RevisionRef> = evidence
        .iter()
        .copied()
        .filter(|reference| reference.id.kind() == Kind::Observation)
        .collect();
    match &basis.opportunity {
        None => missing.push(item(
            "opportunity",
            "absence.opportunity_missing",
            "Cite the Runs or Observations that would have shown the signal, with the minimum window or sample count they had to reach or a search with recorded coverage.",
            vec![],
        )),
        Some(opportunity) => {
            let refs: Vec<RevisionRef> = opportunity.refs.iter().filter_map(exact).collect();
            observations.extend(
                refs.iter()
                    .copied()
                    .filter(|reference| reference.id.kind() == Kind::Observation),
            );
            opportunity_items(state, case, opportunity, &refs, &mut missing);
        }
    }
    instrument_items(state, case, basis, &observations, &mut missing);
    let satisfied = missing.is_empty();
    AbsenceEvaluation {
        rule_version: RULE_VERSION.into(),
        satisfied,
        ceiling: (!satisfied).then(|| "inconclusive".into()),
        missing,
        detection_limit: basis.detection_limit.clone(),
        human_question: HUMAN_QUESTION.into(),
    }
}

fn opportunity_items(
    state: &State,
    case: Id,
    opportunity: &AbsenceOpportunity,
    refs: &[RevisionRef],
    missing: &mut Vec<GateItem>,
) {
    if refs.is_empty() {
        missing.push(item(
            "opportunity",
            "absence.opportunity_refs_missing",
            "Cite the Runs or Observations that would have shown the signal.",
            vec![],
        ));
        return;
    }
    let mut searched = false;
    let mut records = Vec::new();
    for reference in refs {
        let Some(record) = latest(state, case, reference.id) else {
            missing.push(item(
                "opportunity",
                "absence.opportunity_ref_unavailable",
                "A cited opportunity record is unavailable in this case.",
                vec![*reference],
            ));
            continue;
        };
        if retracted(record) {
            missing.push(item(
                "opportunity",
                "absence.opportunity_ref_retracted",
                "A retracted Observation cannot provide an opportunity.",
                vec![*reference],
            ));
            continue;
        }
        let run = if record.header.id.kind() == Kind::Run {
            Some(record)
        } else {
            crate::measurement_fields::cited_run(state, record)
                .and_then(|run| latest(state, case, run.header.id))
        };
        if let Some(run) = run {
            if let Some(reason) = unusable_run(run) {
                missing.push(item(
                    "opportunity",
                    "absence.opportunity_run_not_usable",
                    format!(
                        "The Run {reason}. A failed, blocked or incomplete Run is not negative evidence; cite a separate valid observation."
                    ),
                    vec![run.reference()],
                ));
            }
            if run_coverage(run)
                .is_some_and(|coverage| states_bounds(&coverage) && states_empty_result(&coverage))
            {
                searched = true;
            }
        }
        records.push(record);
    }
    let window = opportunity.minimum_observation_window.as_ref();
    let samples = opportunity.minimum_sample_count;
    if window.is_none() && samples.is_none() && !searched {
        missing.push(item(
            "opportunity",
            "absence.adequacy_standard_missing",
            "State the minimum observation window or sample count the opportunity had to reach, or cite a search Run whose coverage states its bounds and an explicit empty result.",
            refs.to_vec(),
        ));
    }
    for record in records {
        let reference = record.reference();
        if let Some(minimum) = window {
            match first_field(state, record, |fields| fields.observation_window.as_ref()) {
                Some(Knowledge::Known(recorded)) if recorded.unit != minimum.unit => {
                    missing.push(item(
                        "opportunity",
                        "absence.observation_window_unit_differs",
                        format!(
                            "The recorded window unit {} differs from the minimum's unit {}. Units are not converted.",
                            recorded.unit, minimum.unit
                        ),
                        vec![reference],
                    ));
                }
                Some(Knowledge::Known(recorded))
                    if recorded.value.numeric_cmp(&minimum.value).is_lt() =>
                {
                    missing.push(item(
                        "opportunity",
                        "absence.observation_window_below_minimum",
                        format!(
                            "The recorded window {} {} is below the stated minimum {} {}.",
                            recorded.value.as_str(),
                            recorded.unit,
                            minimum.value.as_str(),
                            minimum.unit
                        ),
                        vec![reference],
                    ));
                }
                Some(Knowledge::Known(_)) => {}
                Some(Knowledge::Unknown) | None => missing.push(item(
                    "opportunity",
                    "absence.observation_window_unrecorded",
                    "Record the observation window, or remove the minimum it must meet.",
                    vec![reference],
                )),
            }
        }
        if let Some(minimum) = samples {
            match first_field(state, record, |fields| fields.sample_count.as_ref()) {
                Some(Knowledge::Known(count)) if count < minimum => missing.push(item(
                    "opportunity",
                    "absence.sample_count_below_minimum",
                    format!(
                        "The recorded sample count {count} is below the stated minimum {minimum}."
                    ),
                    vec![reference],
                )),
                Some(Knowledge::Known(_)) => {}
                Some(Knowledge::Unknown) | None => missing.push(item(
                    "opportunity",
                    "absence.sample_count_unrecorded",
                    "Record the sample count, or remove the minimum it must meet.",
                    vec![reference],
                )),
            }
        }
    }
}

fn instrument_items(
    state: &State,
    case: Id,
    basis: &AbsenceBasis,
    observations: &[RevisionRef],
    missing: &mut Vec<GateItem>,
) {
    let justified = basis
        .justified_equivalent
        .as_deref()
        .is_some_and(|text| !text.trim().is_empty());
    let mut seen = BTreeSet::new();
    let mut broken = Vec::new();
    let mut unassessed = Vec::new();
    for reference in observations {
        // An unavailable Observation counts as no instrument at all.
        let Some(record) = latest(state, case, reference.id) else {
            continue;
        };
        if !seen.insert(reference.id) {
            continue;
        }
        match crate::measurement_ops::effective_validity(state, record) {
            MeasurementValidity::Valid => {}
            MeasurementValidity::Invalid | MeasurementValidity::Partial => {
                broken.push(record.reference());
            }
            MeasurementValidity::Unknown => unassessed.push(record.reference()),
        }
    }
    if !broken.is_empty() {
        missing.push(item(
            "instrument",
            "absence.instrument_invalid",
            "A cited measurement is invalid or partially valid. Nothing seen through a broken instrument is evidence of absence or equality; a justified equivalent cannot override that judgment.",
            broken,
        ));
    }
    if justified {
        return;
    }
    if !unassessed.is_empty() {
        missing.push(item(
            "instrument",
            "absence.instrument_unassessed",
            "Record a valid instrument or control judgment with measurement.assess for each cited observation, or state a justified equivalent.",
            unassessed,
        ));
    } else if seen.is_empty() {
        missing.push(item(
            "instrument",
            "absence.instrument_missing",
            "Cite an observation with a valid instrument or control judgment, or state a justified equivalent.",
            vec![],
        ));
    }
}

fn decimal(value: &Value) -> Option<DecimalString> {
    let text = match value {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        _ => return None,
    };
    DecimalString::try_from(text).ok()
}

/// The producer assertion named `name` on a Run body. `None` unless exactly
/// one assertion has that name.
fn assertion<'a>(run: &'a Record, name: &str) -> Option<&'a Value> {
    let assertions = run.body[crate::producer_run::PRODUCER_RUN_KEY]["assertions"].as_array()?;
    let mut matching = assertions
        .iter()
        .filter(|assertion| assertion["name"] == name);
    let found = matching.next()?;
    matching.next().is_none().then_some(found)
}

fn read_interval(
    state: &State,
    case: Id,
    source: &IntervalSource,
    missing: &mut Vec<GateItem>,
) -> Option<IntervalRead> {
    let reference = exact(&source.run_ref)?;
    let fail = |missing: &mut Vec<GateItem>, code: &str, message: &str| {
        missing.push(item("interval", code, message, vec![reference]));
    };
    let Some(run) = in_case(state, case, reference).filter(|run| run.header.id.kind() == Kind::Run)
    else {
        fail(
            missing,
            "equivalence.interval_run_unavailable",
            "The interval's Run is unavailable in this case.",
        );
        return None;
    };
    if run.body["execution_status"] != "completed" {
        fail(
            missing,
            "equivalence.interval_run_not_completed",
            "The Run reporting the interval did not complete.",
        );
    }
    let Some(found) = assertion(run, &source.assertion) else {
        fail(
            missing,
            "equivalence.interval_assertion_missing",
            "The Run records no single producer assertion with this name. The core reads intervals only from recorded producer statements and invents none.",
        );
        return None;
    };
    let bounds = (
        decimal(&found["value"]["lower"]),
        decimal(&found["value"]["upper"]),
    );
    let (Some(lower), Some(upper)) = bounds else {
        fail(
            missing,
            "equivalence.interval_unreadable",
            "The assertion value must hold lower and upper bounds as numbers or decimal strings.",
        );
        return None;
    };
    if lower.numeric_cmp(&upper).is_gt() {
        fail(
            missing,
            "equivalence.interval_unreadable",
            "The assertion's lower bound exceeds its upper bound.",
        );
        return None;
    }
    let Some(unit) = found["unit"]
        .as_str()
        .filter(|unit| !unit.trim().is_empty())
    else {
        fail(
            missing,
            "equivalence.interval_unit_unrecorded",
            "The producer assertion records no unit, so it cannot be compared with the margin.",
        );
        return None;
    };
    // The first revision of this Run that recorded the same statement.
    let first_recorded_at_sequence = state
        .history(reference.id)
        .filter(|revision| revision.header.revision.0 <= reference.revision)
        .find(|revision| {
            assertion(revision, &source.assertion).is_some_and(|earlier| {
                earlier["value"] == found["value"] && earlier["unit"] == found["unit"]
            })
        })
        .map_or(run.header.acceptance_sequence, |revision| {
            revision.header.acceptance_sequence
        });
    Some(IntervalRead {
        lower,
        upper,
        unit: unit.into(),
        run_ref: reference,
        assertion: source.assertion.clone(),
        first_recorded_at_sequence,
    })
}

/// Evaluate an assessment's `equivalence` block. `stated_at` is the
/// acceptance sequence of the assessment, used for a margin stated with it.
pub fn evaluate_equivalence(
    state: &State,
    case: Id,
    basis: &EquivalenceBasis,
    stated_at: Option<u64>,
) -> EquivalenceEvaluation {
    let mut missing = Vec::new();
    let mut timing = MarginTiming::Missing;
    let mut margin = None;
    if let Some(reference) = basis.inquiry_ref.as_ref().and_then(exact) {
        let record = in_case(state, case, reference)
            .filter(|record| record.header.id.kind() == Kind::Inquiry);
        let plan = record.and_then(|record| {
            input::decode::<EquivalencePlan>(record.body.get("equivalence")?).ok()
        });
        match (record, plan) {
            (Some(record), Some(plan)) => {
                let frozen = record.body["lifecycle"] == "frozen";
                timing = if frozen {
                    MarginTiming::Prespecified
                } else {
                    MarginTiming::NotFrozen
                };
                margin = Some(MarginRead {
                    lower: plan.margin.lower,
                    upper: plan.margin.upper,
                    unit: plan.unit,
                    inquiry_ref: Some(reference),
                    stated_at_sequence: frozen.then_some(record.header.acceptance_sequence),
                });
            }
            _ => missing.push(item(
                "margin",
                "equivalence.inquiry_has_no_margin",
                "The named inquiry revision states no equivalence margin.",
                vec![reference],
            )),
        }
    } else if let Some(stated) = &basis.stated_margin {
        timing = MarginTiming::StatedAfterResults;
        margin = Some(MarginRead {
            lower: stated.lower.clone(),
            upper: stated.upper.clone(),
            unit: stated.unit.clone(),
            inquiry_ref: None,
            stated_at_sequence: stated_at,
        });
    } else {
        missing.push(item(
            "margin",
            "equivalence.margin_missing",
            "Name a frozen inquiry that states the margin before results.",
            vec![],
        ));
    }
    let interval = read_interval(state, case, &basis.interval, &mut missing);
    if let (MarginTiming::Prespecified, Some(read), Some(interval)) = (timing, &margin, &interval)
        && read
            .stated_at_sequence
            .is_none_or(|stated| stated >= interval.first_recorded_at_sequence)
    {
        timing = MarginTiming::StatedAfterResults;
    }
    let outcome = match (&margin, &interval) {
        (Some(margin), Some(interval)) if margin.unit != interval.unit => {
            missing.push(item(
                "interval",
                "equivalence.unit_differs",
                format!(
                    "The interval unit {} differs from the margin unit {}. Units are not converted.",
                    interval.unit, margin.unit
                ),
                vec![interval.run_ref],
            ));
            EquivalenceOutcome::NotEvaluable
        }
        (Some(margin), Some(interval)) => {
            if interval.upper.numeric_cmp(&margin.lower).is_lt()
                || interval.lower.numeric_cmp(&margin.upper).is_gt()
            {
                EquivalenceOutcome::EquivalenceExcluded
            } else if margin.lower.numeric_cmp(&interval.lower).is_le()
                && interval.upper.numeric_cmp(&margin.upper).is_le()
            {
                EquivalenceOutcome::BoundedEquivalenceSupported
            } else {
                EquivalenceOutcome::EquivalenceUnresolved
            }
        }
        _ => EquivalenceOutcome::NotEvaluable,
    };
    EquivalenceEvaluation {
        rule_version: RULE_VERSION.into(),
        outcome,
        margin_timing: timing,
        margin,
        interval,
        missing,
    }
}

fn decode_block<T: serde::de::DeserializeOwned>(body: &Value, key: &str) -> Option<T> {
    match body.get(key) {
        None | Some(Value::Null) => None,
        Some(value) => input::decode(value).ok(),
    }
}

fn evidence_refs(body: &Value) -> Vec<RevisionRef> {
    decode_block(body, "evidence_refs").unwrap_or_default()
}

/// Stores the equivalence evaluation at acceptance on a new assessment body,
/// so that a margin stated after results is recorded as such.
pub fn record_assessment(ctx: &OperationContext<'_>, body: &mut Value) -> Result<(), Failure> {
    let Some(basis) = decode_block::<EquivalenceBasis>(body, "equivalence") else {
        return Ok(());
    };
    let case = ctx
        .case_id()
        .ok_or_else(|| Failure::new(ErrorCode::IntegrityFailure, "An assessment needs a case."))?;
    let evaluation =
        evaluate_equivalence(ctx.state(), case, &basis, Some(ctx.acceptance_sequence()));
    let object = body.as_object_mut().ok_or_else(|| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Assessment body is not an object.",
        )
    })?;
    object.insert(
        EQUIVALENCE_EVALUATION_KEY.into(),
        serde_json::to_value(evaluation).map_err(|_| {
            Failure::new(
                ErrorCode::IntegrityFailure,
                "Could not encode the evaluation.",
            )
        })?,
    );
    Ok(())
}

fn warning_lines(state: &State, record: &Record, stated_at: Option<u64>) -> Vec<String> {
    let case = record.header.case_id;
    let mut lines = Vec::new();
    if let Some(basis) = decode_block::<AbsenceBasis>(&record.body, "absence") {
        let evaluation = evaluate_absence(state, case, &basis, &evidence_refs(&record.body));
        if !evaluation.satisfied {
            let codes: Vec<&str> = evaluation
                .missing
                .iter()
                .map(|item| item.code.as_str())
                .collect();
            lines.push(format!(
                "absence.basis_incomplete: {} rests on an absence whose basis is incomplete ({}). It is recorded, reads as inconclusive and cannot support promotion until the basis is complete.",
                record.header.id,
                codes.join(", ")
            ));
        }
    }
    if let Some(basis) = decode_block::<EquivalenceBasis>(&record.body, "equivalence") {
        let evaluation = evaluate_equivalence(state, case, &basis, stated_at);
        if !evaluation.holds() {
            let codes: Vec<String> = evaluation
                .reasons()
                .into_iter()
                .map(|item| item.code)
                .collect();
            lines.push(format!(
                "equivalence.not_supported: {} does not support bounded equivalence ({}).",
                record.header.id,
                codes.join(", ")
            ));
        }
    }
    lines
}

/// Receipt warnings for a just-written Assessment or Comparison.
pub fn warn_recorded(ctx: &mut OperationContext<'_>, reference: RevisionRef) {
    let sequence = ctx.acceptance_sequence();
    let lines = ctx
        .exact(reference)
        .map(|record| warning_lines(ctx.state(), record, Some(sequence)))
        .unwrap_or_default();
    for line in lines {
        ctx.warn(line);
    }
}

/// True unless a Comparison rests on an absence whose basis is incomplete.
pub fn comparison_basis_holds(state: &State, case: Id, cell: &ComparisonBody) -> bool {
    cell.absence
        .as_ref()
        .is_none_or(|basis| evaluate_absence(state, case, basis, &cell.evidence_refs).satisfied)
}

/// One record in a Conclusion's support that carries an absence or
/// equivalence basis, with the gate's reading.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SupportReading {
    pub record_ref: RevisionRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub absence: Option<AbsenceEvaluation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equivalence: Option<EquivalenceEvaluation>,
}

/// The absence and equivalence bases a Conclusion's support rests on: its
/// `support_refs` and, through their `evidence_refs`, any cited Assessment or
/// Comparison. Contradicting evidence is not walked.
pub fn support_readings(state: &State, conclusion: &Record) -> Vec<SupportReading> {
    let case = conclusion.header.case_id;
    let mut queue: Vec<RevisionRef> =
        decode_block(&conclusion.body, "support_refs").unwrap_or_default();
    let mut seen = BTreeSet::new();
    let mut readings = Vec::new();
    while let Some(reference) = queue.pop() {
        if seen.len() >= MAX_WALK || !seen.insert(reference.id) {
            continue;
        }
        if !matches!(reference.id.kind(), Kind::Assessment | Kind::Comparison) {
            continue;
        }
        let Some(record) = in_case(state, case, reference) else {
            continue;
        };
        let evidence = evidence_refs(&record.body);
        let absence = decode_block::<AbsenceBasis>(&record.body, "absence")
            .map(|basis| evaluate_absence(state, case, &basis, &evidence));
        let equivalence =
            decode_block::<EquivalenceBasis>(&record.body, "equivalence").map(|basis| {
                evaluate_equivalence(state, case, &basis, Some(record.header.acceptance_sequence))
            });
        if absence.is_some() || equivalence.is_some() {
            readings.push(SupportReading {
                record_ref: reference,
                absence,
                equivalence,
            });
        }
        queue.extend(evidence);
    }
    readings.sort_by_key(|reading| reading.record_ref.id);
    readings
}

fn supportive(state: &State, reference: RevisionRef) -> bool {
    reference.id.kind() == Kind::Assessment
        && state
            .revision(reference)
            .is_some_and(|record| record.body["relation"] == json!(AssessmentRelation::Supports))
}

/// Refuse promotion of a Conclusion whose support rests on an absence or
/// equivalence basis that does not hold, or that declares a `claim_form`
/// without such a basis. The failure lists every unmet requirement.
pub fn promotion_gate(state: &State, conclusion: &Record) -> Result<(), Failure> {
    let claim: Option<ClaimForm> = decode_block(&conclusion.body, "claim_form");
    let direct: BTreeSet<RevisionRef> =
        decode_block::<Vec<RevisionRef>>(&conclusion.body, "support_refs")
            .unwrap_or_default()
            .into_iter()
            .collect();
    let readings = support_readings(state, conclusion);
    let mut reasons = Vec::new();
    let mut absence_basis = false;
    let mut equivalence_basis = false;
    for reading in &readings {
        let qualifies =
            direct.contains(&reading.record_ref) && supportive(state, reading.record_ref);
        if let Some(absence) = &reading.absence {
            if absence.satisfied {
                absence_basis |= qualifies;
            } else {
                reasons.push(json!({"record_ref": reading.record_ref, "missing": absence.missing}));
            }
        }
        if let Some(equivalence) = &reading.equivalence {
            if equivalence.holds() {
                equivalence_basis |= qualifies;
            } else {
                reasons.push(json!({
                    "record_ref": reading.record_ref,
                    "outcome": equivalence.outcome,
                    "margin_timing": equivalence.margin_timing,
                    "missing": equivalence.reasons(),
                }));
            }
        }
    }
    match claim {
        Some(ClaimForm::Absence) if !absence_basis => reasons.push(json!({"missing": [item(
            "claim_basis",
            "absence.claim_without_basis",
            "An absence conclusion needs a supportive assessment in support_refs whose absence block names the expected signal, an adequate opportunity and a valid instrument or justified equivalent.",
            vec![],
        )]})),
        Some(ClaimForm::Equivalence) if !equivalence_basis => reasons.push(json!({"missing": [item(
            "claim_basis",
            "equivalence.claim_without_basis",
            "An equivalence conclusion needs a supportive assessment in support_refs whose externally supplied interval lies inside a margin frozen before results.",
            vec![],
        )]})),
        _ => {}
    }
    if reasons.is_empty() {
        return Ok(());
    }
    let mut failure = Failure::new(
        ErrorCode::GateFailed,
        "The conclusion rests on an absence or equivalence basis that does not support it. Until the missing requirements are recorded it reads as inconclusive.",
    )
    .at("/support_refs")
    .with_schema("assessment.add");
    failure.error.actual = Some(json!({
        "rule_version": RULE_VERSION,
        "ceiling": "inconclusive",
        "reasons": reasons,
        "human_question": HUMAN_QUESTION,
    }));
    failure.error.expected = Some(json!({
        "absence": ["expected_signal", "opportunity", "instrument"],
        "equivalence": ["margin frozen before results", "interval inside the margin"],
    }));
    Err(failure)
}

#[cfg(test)]
mod tests;
