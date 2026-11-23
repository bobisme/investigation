//! Confidence with named downgrade reasons (§5.7).
//!
//! Confidence is an ordinal judgment about the evidentiary basis of one
//! record: `low`, `moderate`, `high` or `not_assessed`. It is never a
//! probability, never a percentage and never combined across records.
//! Probability belongs to the separate `likelihood` field.
//!
//! This module does three things:
//!
//! - **Promotion check.** A `moderate` or `high` confidence on a record being
//!   promoted (a Conclusion at review request and validation, an Assessment
//!   cited by a Decision at acceptance) must name the downgrade reasons
//!   considered. Each of the five reasons is `ok`, `concern` or
//!   `not_assessed`; `not_assessed` is always acceptable. First recording is
//!   never blocked.
//! - **Computed concerns.** Recorded facts that bear on a downgrade reason
//!   are listed next to the recorded judgment, each with its source and the
//!   records it read: measurement validity and advisory measurement flags,
//!   claim origins, recorded contradictions and search coverage. They are
//!   advisory. They never change the recorded confidence and never gate.
//! - **Advisories.** A recorded `ok` that a computed concern contradicts, and
//!   a recorded label above the GRADE-style ceiling of its named concerns
//!   (one level down per reason with a concern), are flagged in words. The
//!   recorded label is shown as recorded.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    PROGRAM_NAME,
    command::RevisionRef,
    evidence::MeasurementValidity,
    identity::{Id, Kind},
    input, measurement_flags, measurement_ops, origin_ops,
    review::{Confidence, ConfidenceLevel, DowngradeJudgment, DowngradeReasons},
    search_coverage,
    state::{Record, State},
    wire::{ErrorCode, Failure},
};

/// Stated with every confidence view.
pub const NOTICE: &str = "Confidence is an ordinal judgment about the evidentiary basis, not a probability. It is never combined across records.";

/// Operations whose payload carries a `confidence` block, with the JSON
/// pointers where it may appear.
const CONFIDENCE_POINTERS: &[(&str, &[&str])] = &[
    ("assessment.add", &["/confidence"]),
    ("assessment.supersede", &["/replacement/confidence"]),
    ("conclusion.create", &["/confidence"]),
    ("conclusion.amend", &["/replacement/confidence"]),
];

/// Most records listed under one computed concern.
const MAX_CONCERN_REFS: usize = 8;

/// The five GRADE-style downgrade reasons, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DowngradeReason {
    /// Validity and risk of bias.
    Validity,
    /// Consistency across independent evidence.
    Consistency,
    /// Directness to the stated scope.
    Directness,
    /// Precision and sample adequacy.
    Precision,
    /// Search and reporting completeness.
    Completeness,
}

impl DowngradeReason {
    pub const ALL: [Self; 5] = [
        Self::Validity,
        Self::Consistency,
        Self::Directness,
        Self::Precision,
        Self::Completeness,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Validity => "validity",
            Self::Consistency => "consistency",
            Self::Directness => "directness",
            Self::Precision => "precision",
            Self::Completeness => "completeness",
        }
    }

    fn recorded(self, reasons: &DowngradeReasons) -> DowngradeJudgment {
        match self {
            Self::Validity => reasons.validity,
            Self::Consistency => reasons.consistency,
            Self::Directness => reasons.directness,
            Self::Precision => reasons.precision,
            Self::Completeness => reasons.completeness,
        }
    }
}

/// A recorded fact that bears on one downgrade reason. Advisory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ComputedConcern {
    pub reason: DowngradeReason,
    /// Stable code, for example `measurement_invalid` or `single_origin`.
    pub code: &'static str,
    /// Where the fact was read: `measurement_validity`, `measurement_flags`,
    /// `claim_origin`, `contradiction_refs` or `search_coverage`.
    pub source: &'static str,
    pub detail: String,
    /// The records the fact was read from, at most eight.
    pub refs: Vec<RevisionRef>,
    /// True when more records than `refs` show the same fact.
    pub more_refs: bool,
}

/// Recorded and computed state of one downgrade reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReasonState {
    pub reason: DowngradeReason,
    /// The assessor's judgment, or `None` when no reasons were recorded.
    pub recorded: Option<DowngradeJudgment>,
    /// Codes of computed concerns for this reason.
    pub computed: Vec<&'static str>,
    /// True when the recorded judgment or a computed fact names a concern.
    pub concern: bool,
}

/// Whether the promotion requirement is met by the recorded confidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PromotionRequirement {
    /// `low`, `not_assessed` or no confidence: nothing more is required.
    NotRequired,
    /// `moderate` or `high` with every downgrade reason named.
    Met,
    /// `moderate` or `high` without named downgrade reasons.
    MissingDowngradeReasons,
}

/// One advisory about the recorded confidence. Never a gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ConfidenceAdvisory {
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<DowngradeReason>,
    /// For `label_above_named_concerns`: the highest label the named concerns
    /// leave, one level down per reason with a concern.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ceiling: Option<ConfidenceLevel>,
    pub message: String,
}

/// Confidence of one Assessment or Conclusion as shown in views.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ConfidenceView {
    /// The recorded label; `not_assessed` when none was recorded.
    pub level: ConfidenceLevel,
    /// False when the record carries no confidence block.
    pub recorded: bool,
    pub downgrade_reasons_named: bool,
    pub promotion_requirement: PromotionRequirement,
    /// The five reasons in order, recorded beside computed.
    pub reasons: Vec<ReasonState>,
    /// The reasons with a concern, recorded or computed, in order.
    pub concerns: Vec<ReasonState>,
    pub computed_concerns: Vec<ComputedConcern>,
    pub advisories: Vec<ConfidenceAdvisory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limitations: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub what_would_change_it: Option<String>,
    pub notice: &'static str,
}

fn level_name(level: ConfidenceLevel) -> &'static str {
    match level {
        ConfidenceLevel::Low => "low",
        ConfidenceLevel::Moderate => "moderate",
        ConfidenceLevel::High => "high",
        ConfidenceLevel::NotAssessed => "not_assessed",
    }
}

fn rank(level: ConfidenceLevel) -> Option<u8> {
    match level {
        ConfidenceLevel::Low => Some(0),
        ConfidenceLevel::Moderate => Some(1),
        ConfidenceLevel::High => Some(2),
        ConfidenceLevel::NotAssessed => None,
    }
}

/// The recorded confidence of a record body. `Ok(None)` when absent.
pub fn recorded(body: &Value) -> Result<Option<Confidence>, Failure> {
    match body.get("confidence") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => input::decode(value)
            .map(Some)
            .map_err(|_| Failure::new(ErrorCode::IntegrityFailure, "Invalid stored confidence.")),
    }
}

/// Whether a recorded confidence meets the promotion requirement.
pub fn requirement(confidence: Option<&Confidence>) -> PromotionRequirement {
    match confidence {
        Some(confidence)
            if matches!(
                confidence.level,
                ConfidenceLevel::Moderate | ConfidenceLevel::High
            ) =>
        {
            if confidence.downgrade_reasons.is_some() {
                PromotionRequirement::Met
            } else {
                PromotionRequirement::MissingDowngradeReasons
            }
        }
        _ => PromotionRequirement::NotRequired,
    }
}

/// The repair: the operation that records named reasons, and their shape.
fn reasons_template(repair_schema: &str) -> Value {
    let mut reasons = serde_json::Map::new();
    for reason in DowngradeReason::ALL {
        reasons.insert(reason.as_str().into(), json!("ok | concern | not_assessed"));
    }
    json!({"repair_schema": repair_schema, "confidence": {"downgrade_reasons": reasons}})
}

fn show_argv(id: Id) -> Vec<String> {
    vec![
        PROGRAM_NAME.into(),
        "show".into(),
        id.to_string(),
        "--depth".into(),
        "full".into(),
        "--format".into(),
        "json".into(),
    ]
}

/// Promotion check for a Conclusion's own confidence, at review request and
/// at validation. The repair is `conclusion.amend` with named reasons, or a
/// `low` or `not_assessed` label.
pub fn require_named_reasons(record: &Record, path: &str) -> Result<(), Failure> {
    let confidence = recorded(&record.body)?;
    if requirement(confidence.as_ref()) != PromotionRequirement::MissingDowngradeReasons {
        return Ok(());
    }
    let level = confidence.map_or(ConfidenceLevel::NotAssessed, |value| value.level);
    let mut failure = Failure::new(
        ErrorCode::GateFailed,
        format!(
            "A {} confidence on a promoted record names the downgrade reasons considered: validity, consistency, directness, precision and completeness, each ok, concern or not_assessed. Amend the record with confidence.downgrade_reasons, or record the level as low or not_assessed.",
            level_name(level)
        ),
    )
    .at(path);
    failure.error.expected = Some(reasons_template(match record.header.id.kind() {
        Kind::Assessment => "assessment.supersede",
        _ => "conclusion.amend",
    }));
    failure.error.actual = Some(json!({
        "record_ref": record.reference(),
        "confidence": {"level": level, "downgrade_reasons": null},
    }));
    Err(failure.with_argv(show_argv(record.header.id)))
}

/// Promotion check at decision acceptance: every cited Assessment revision
/// with a `moderate` or `high` confidence names its downgrade reasons.
pub fn require_cited_reasons(
    state: &State,
    evidence_refs: &[RevisionRef],
    path: &str,
) -> Result<(), Failure> {
    let mut missing = Vec::new();
    for reference in evidence_refs {
        if reference.id.kind() != Kind::Assessment {
            continue;
        }
        let Some(record) = state.revision(*reference) else {
            continue;
        };
        if requirement(recorded(&record.body)?.as_ref())
            == PromotionRequirement::MissingDowngradeReasons
        {
            missing.push(*reference);
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let mut failure = Failure::new(
        ErrorCode::GateFailed,
        "A cited assessment states moderate or high confidence without naming the downgrade reasons considered. Supersede it with confidence.downgrade_reasons (each ok, concern or not_assessed), or with a low or not_assessed level, and cite the new revision.",
    )
    .at(path);
    failure.error.expected = Some(reasons_template("assessment.supersede"));
    failure.error.actual = Some(json!(missing));
    Err(failure.with_argv(show_argv(missing[0].id)))
}

fn numeric_like(value: &Value) -> bool {
    match value {
        Value::Number(_) => true,
        Value::String(text) => {
            let text = text.trim().trim_end_matches('%').trim();
            !text.is_empty() && text.parse::<f64>().is_ok()
        }
        _ => false,
    }
}

/// Replace a decode failure caused by a numeric confidence with an explicit
/// rejection that points to `likelihood`. Any other failure is returned
/// unchanged.
pub fn explain_decode_failure(operation: &str, payload: &Value, failure: Failure) -> Failure {
    let Some((_, pointers)) = CONFIDENCE_POINTERS
        .iter()
        .find(|(name, _)| *name == operation)
    else {
        return failure;
    };
    for pointer in *pointers {
        let candidates = [pointer.to_string(), format!("{pointer}/level")];
        for candidate in candidates {
            if let Some(value) = payload.pointer(&candidate)
                && numeric_like(value)
            {
                let mut explained = Failure::new(
                    ErrorCode::InvalidInput,
                    "Confidence is an ordinal judgment (low, moderate, high or not_assessed) with a rationale, never a number or probability. State a probability as likelihood on a claim with a resolution condition.",
                )
                .at(candidate);
                explained.error.expected = Some(json!({
                    "level": "low | moderate | high | not_assessed",
                    "rationale": "text",
                    "limitations": [],
                    "what_would_change_it": null,
                    "downgrade_reasons": null,
                }));
                return explained;
            }
        }
    }
    failure
}

/// Observations a record rests on: those it cites directly in its evidence
/// fields and, for supporting Assessments it cites, the Observations they
/// cite. Exact cited revisions, first citation order.
fn rested_observations(state: &State, record: &Record) -> Vec<RevisionRef> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let refs = |body: &Value, field: &str| -> Vec<RevisionRef> {
        body.get(field)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| input::decode::<RevisionRef>(item).ok())
            .collect()
    };
    let field = match record.header.id.kind() {
        Kind::Conclusion => "support_refs",
        _ => "evidence_refs",
    };
    for reference in refs(&record.body, field) {
        match reference.id.kind() {
            Kind::Observation => {
                if seen.insert(reference) {
                    out.push(reference);
                }
            }
            Kind::Assessment => {
                if let Some(assessment) = state.revision(reference)
                    && assessment.body["relation"] == "supports"
                {
                    for nested in refs(&assessment.body, "evidence_refs") {
                        if nested.id.kind() == Kind::Observation && seen.insert(nested) {
                            out.push(nested);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Supporting Assessment revisions a Conclusion cites.
fn supporting_assessments<'a>(state: &'a State, record: &Record) -> Vec<&'a Record> {
    if record.header.id.kind() != Kind::Conclusion {
        return Vec::new();
    }
    record.body["support_refs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| input::decode::<RevisionRef>(item).ok())
        .filter(|reference| reference.id.kind() == Kind::Assessment)
        .filter_map(|reference| state.revision(reference))
        .collect()
}

#[derive(Default)]
struct Collector {
    found: BTreeMap<(DowngradeReason, &'static str), (&'static str, String, Vec<RevisionRef>)>,
}

impl Collector {
    fn add(
        &mut self,
        reason: DowngradeReason,
        code: &'static str,
        source: &'static str,
        detail: impl Into<String>,
        reference: Option<RevisionRef>,
    ) {
        let entry = self
            .found
            .entry((reason, code))
            .or_insert_with(|| (source, detail.into(), Vec::new()));
        if let Some(reference) = reference
            && !entry.2.contains(&reference)
        {
            entry.2.push(reference);
        }
    }

    fn finish(self) -> Vec<ComputedConcern> {
        self.found
            .into_iter()
            .map(|((reason, code), (source, detail, mut refs))| {
                let more_refs = refs.len() > MAX_CONCERN_REFS;
                refs.truncate(MAX_CONCERN_REFS);
                ComputedConcern {
                    reason,
                    code,
                    source,
                    detail,
                    refs,
                    more_refs,
                }
            })
            .collect()
    }
}

fn flag_reason(flag_id: &str) -> Option<(DowngradeReason, &'static str)> {
    use measurement_flags::flag_id as id;
    Some(match flag_id {
        id::QUANTILE_SAMPLE_COUNT_UNRECORDED
        | id::QUANTILE_BELOW_ORDER_STATISTIC_BOUND
        | id::FLAT_SAMPLES => (DowngradeReason::Precision, "measurement_precision_flag"),
        id::SEQUENTIAL_TRIAL_ORDER
        | id::CLOSED_LOOP_WITHOUT_INTENDED_SEND_TIMING
        | id::QUANTILES_COMBINED_ACROSS_RUNS
        | id::COMPLETION_ONLY_STATISTIC
        | id::INSTRUMENTATION_CHANGED
        | id::OUTCOME_FROM_COMPLETION_ONLY_STATISTIC => {
            (DowngradeReason::Validity, "measurement_bias_flag")
        }
        id::COMPARED_WORKLOAD_DIFFERS
        | id::COMPARED_LOAD_MODEL_DIFFERS
        | id::COMPARED_WARMUP_DIFFERS
        | id::COMPARED_TOOL_IDENTITY_DIFFERS => {
            (DowngradeReason::Directness, "compared_conditions_differ")
        }
        _ => return None,
    })
}

/// Recorded facts that bear on the downgrade reasons of an Assessment or
/// Conclusion. Advisory; empty for other kinds.
pub fn computed_concerns(state: &State, record: &Record) -> Vec<ComputedConcern> {
    if !matches!(record.header.id.kind(), Kind::Assessment | Kind::Conclusion) {
        return Vec::new();
    }
    let case = record.header.case_id;
    let mut found = Collector::default();
    let observations = rested_observations(state, record);

    for reference in &observations {
        let Some(current) = state
            .current(reference.id)
            .filter(|current| current.header.case_id == case)
        else {
            continue;
        };
        // Validity and risk of bias: effective measurement validity.
        match measurement_ops::validity_view(state, current).map(|view| view.state) {
            Ok(MeasurementValidity::Invalid) => found.add(
                DowngradeReason::Validity,
                "measurement_invalid",
                "measurement_validity",
                "A cited observation's effective measurement validity is invalid.",
                Some(current.reference()),
            ),
            Ok(MeasurementValidity::Partial) => found.add(
                DowngradeReason::Validity,
                "measurement_partial",
                "measurement_validity",
                "A cited observation's effective measurement validity is partial.",
                Some(current.reference()),
            ),
            _ => {}
        }
        // Search and reporting completeness: the coverage of the search Run
        // the observation cites.
        if let Some(coverage) = search_coverage::cited_coverage(state, current) {
            let run = input::decode::<RevisionRef>(&current.body["run_ref"]).ok();
            if !search_coverage::states_bounds(&coverage) {
                found.add(
                    DowngradeReason::Completeness,
                    "search_bounds_unstated",
                    "search_coverage",
                    "A search behind a cited observation does not state its queries and bounds.",
                    run,
                );
            }
            if let (Some(total), Some(examined)) = (coverage.total_hits, coverage.examined_hits)
                && examined < total
            {
                found.add(
                    DowngradeReason::Completeness,
                    "search_hits_not_all_examined",
                    "search_coverage",
                    "A search behind a cited observation examined fewer hits than it found.",
                    run,
                );
            }
            if !coverage.not_searched.is_empty() {
                found.add(
                    DowngradeReason::Completeness,
                    "search_excludes_stated_items",
                    "search_coverage",
                    "A search behind a cited observation states material it did not search.",
                    run,
                );
            }
        }
    }

    // Advisory measurement flags on the record and on its supporting
    // assessments.
    let mut flagged = vec![record];
    flagged.extend(supporting_assessments(state, record));
    for subject in flagged {
        if let Some(report) = measurement_flags::record_flags(state, subject) {
            for flag in &report.flags {
                if let Some((reason, code)) = flag_reason(&flag.flag_id) {
                    found.add(
                        reason,
                        code,
                        "measurement_flags",
                        "An advisory measurement flag bears on this reason; see measurement_flags.",
                        Some(flag.subject_ref),
                    );
                }
            }
        }
    }

    // Consistency across independent evidence: claim origins and recorded
    // contradictions.
    if !observations.is_empty() {
        let summary = origin_ops::origin_summary(state, case, &observations);
        let roots = || {
            summary
                .origins
                .iter()
                .filter_map(|group| group.root.or_else(|| group.observations.first().copied()))
                .collect::<Vec<_>>()
        };
        if summary.origins.len() == 1 {
            for reference in roots() {
                found.add(
                    DowngradeReason::Consistency,
                    "single_origin",
                    "claim_origin",
                    "Every cited claim resolves to one origin group; there is no independent evidence to be consistent with.",
                    Some(reference),
                );
            }
        } else if summary.origins.len() > 1 && summary.independent_pairs == 0 {
            let (code, detail) = if summary.unknown_pairs > 0 {
                (
                    "independence_unknown",
                    "No pair of cited origins is judged independent; their dependence is unknown.",
                )
            } else {
                (
                    "origins_dependent",
                    "No pair of cited origins is judged independent; recorded judgments say they share an origin.",
                )
            };
            for reference in roots() {
                found.add(
                    DowngradeReason::Consistency,
                    code,
                    "claim_origin",
                    detail,
                    Some(reference),
                );
            }
        }
    }
    let contradictions: Vec<RevisionRef> = record.body["contradiction_refs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| input::decode::<RevisionRef>(item).ok())
        .collect();
    for reference in contradictions {
        found.add(
            DowngradeReason::Consistency,
            "recorded_contradiction",
            "contradiction_refs",
            "The record cites contradicting evidence or rival explanations.",
            Some(reference),
        );
    }
    found.finish()
}

/// Confidence view of an Assessment or Conclusion; `None` for other kinds.
/// `full` adds the recorded rationale, limitations and what would change it.
pub fn view(state: &State, record: &Record, full: bool) -> Option<ConfidenceView> {
    if !matches!(record.header.id.kind(), Kind::Assessment | Kind::Conclusion) {
        return None;
    }
    let confidence = recorded(&record.body).ok().flatten();
    let computed = computed_concerns(state, record);
    let named = confidence
        .as_ref()
        .and_then(|value| value.downgrade_reasons.as_ref());
    let reasons: Vec<ReasonState> = DowngradeReason::ALL
        .into_iter()
        .map(|reason| {
            let recorded = named.map(|named| reason.recorded(named));
            let codes: Vec<&'static str> = computed
                .iter()
                .filter(|concern| concern.reason == reason)
                .map(|concern| concern.code)
                .collect();
            ReasonState {
                reason,
                recorded,
                concern: recorded == Some(DowngradeJudgment::Concern) || !codes.is_empty(),
                computed: codes,
            }
        })
        .collect();
    let concerns: Vec<ReasonState> = reasons
        .iter()
        .filter(|state| state.concern)
        .cloned()
        .collect();
    let level = confidence
        .as_ref()
        .map_or(ConfidenceLevel::NotAssessed, |value| value.level);
    let mut advisories = Vec::new();
    for state in &reasons {
        if state.recorded == Some(DowngradeJudgment::Ok) && !state.computed.is_empty() {
            advisories.push(ConfidenceAdvisory {
                code: "recorded_ok_with_computed_concern",
                reason: Some(state.reason),
                ceiling: None,
                message: format!(
                    "{} is recorded as ok, but recorded facts raise a concern ({}). State why it does not apply, or revise the judgment.",
                    state.reason.as_str(),
                    state.computed.join(", ")
                ),
            });
        }
    }
    if let Some(recorded_rank) = rank(level) {
        let ceiling = match concerns.len() {
            0 => ConfidenceLevel::High,
            1 => ConfidenceLevel::Moderate,
            _ => ConfidenceLevel::Low,
        };
        if rank(ceiling).is_some_and(|ceiling| recorded_rank > ceiling) {
            let names: Vec<&str> = concerns.iter().map(|state| state.reason.as_str()).collect();
            advisories.push(ConfidenceAdvisory {
                code: "label_above_named_concerns",
                reason: None,
                ceiling: Some(ceiling),
                message: format!(
                    "Recorded {} while {} name a concern; one level down per reason with a concern leaves {}. The recorded label is shown unchanged; explain why the concerns do not lower it, or revise it.",
                    level_name(level),
                    names.join(", "),
                    level_name(ceiling)
                ),
            });
        }
    }
    if matches!(level, ConfidenceLevel::Moderate | ConfidenceLevel::High)
        && named.is_some_and(|named| {
            DowngradeReason::ALL
                .iter()
                .all(|reason| reason.recorded(named) == DowngradeJudgment::NotAssessed)
        })
    {
        advisories.push(ConfidenceAdvisory {
            code: "no_reason_assessed",
            reason: None,
            ceiling: None,
            message: format!(
                "Recorded {} with every downgrade reason not_assessed.",
                level_name(level)
            ),
        });
    }
    Some(ConfidenceView {
        level,
        recorded: confidence.is_some(),
        downgrade_reasons_named: named.is_some(),
        promotion_requirement: requirement(confidence.as_ref()),
        reasons,
        concerns,
        computed_concerns: computed,
        advisories,
        rationale: confidence
            .as_ref()
            .filter(|_| full)
            .map(|value| value.rationale.clone()),
        limitations: confidence
            .as_ref()
            .filter(|_| full)
            .map(|value| value.limitations.clone()),
        what_would_change_it: confidence
            .as_ref()
            .filter(|_| full)
            .and_then(|value| value.what_would_change_it.clone()),
        notice: NOTICE,
    })
}

impl ConfidenceView {
    /// The bounded summary-depth projection: the label, the reasons with a
    /// concern and advisory codes. Everything else is at full depth.
    pub fn summary(&self) -> Value {
        let mut out = json!({"level": self.level, "recorded": self.recorded});
        if self.recorded {
            out["downgrade_reasons_named"] = json!(self.downgrade_reasons_named);
        }
        if self.promotion_requirement != PromotionRequirement::NotRequired {
            out["promotion_requirement"] = json!(self.promotion_requirement);
        }
        if !self.concerns.is_empty() {
            out["concerns"] = self
                .concerns
                .iter()
                .map(|state| {
                    let mut item = json!({"reason": state.reason, "computed": state.computed});
                    if let Some(recorded) = state.recorded {
                        item["recorded"] = json!(recorded);
                    }
                    item
                })
                .collect();
        }
        if !self.advisories.is_empty() {
            out["advisories"] = self
                .advisories
                .iter()
                .map(|advisory| {
                    let mut item = json!({"code": advisory.code});
                    if let Some(reason) = advisory.reason {
                        item["reason"] = json!(reason);
                    }
                    if let Some(ceiling) = advisory.ceiling {
                        item["ceiling"] = json!(ceiling);
                    }
                    item
                })
                .collect();
        }
        out
    }
}

/// The `confidence` member of a record view: the full view at full depth,
/// otherwise its summary. `None` for kinds without confidence.
pub fn projection(state: &State, record: &Record, full: bool) -> Option<Value> {
    let view = view(state, record, full)?;
    Some(if full { json!(view) } else { view.summary() })
}

/// One text line for a confidence view, or empty when the view has none.
/// Labels and reason names only: never a number or percentage.
pub fn text(view: &Value) -> String {
    let Some(level) = view["confidence"]["level"].as_str() else {
        return String::new();
    };
    let mut line = format!("\nConfidence: {level}");
    if view["confidence"]["promotion_requirement"] == "missing_downgrade_reasons" {
        line.push_str(" (downgrade reasons not named)");
    }
    let reasons: Vec<String> = view["confidence"]["concerns"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|state| {
            let name = state["reason"].as_str().unwrap_or("?");
            let mut sources = Vec::new();
            if state["recorded"] == "concern" {
                sources.push("recorded".to_owned());
            }
            let computed: Vec<&str> = state["computed"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if !computed.is_empty() {
                sources.push(format!("computed: {}", computed.join(", ")));
            }
            format!("{name} concern ({})", sources.join("; "))
        })
        .collect();
    if !reasons.is_empty() {
        line.push_str(&format!("; {}", reasons.join("; ")));
    }
    for advisory in view["confidence"]["advisories"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Some(code) = advisory["code"].as_str() {
            line.push_str(&format!("\nConfidence advisory {code}"));
            if let Some(ceiling) = advisory["ceiling"].as_str() {
                line.push_str(&format!(" (ceiling {ceiling})"));
            }
            if let Some(reason) = advisory["reason"].as_str() {
                line.push_str(&format!(" ({reason})"));
            }
        }
    }
    line
}

#[cfg(test)]
mod tests;
