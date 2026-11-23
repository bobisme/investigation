//! Likelihood on resolvable claims, outcome recording and Brier calibration
//! (§5.7).
//!
//! Likelihood is a separate optional field on an Assessment or a Conclusion: a
//! number in [0, 1] or one of the seven ICD 203 terms, always with a stated
//! resolution condition. It is not confidence and is never shown in the same
//! sentence as confidence. A stated likelihood never satisfies or blocks a
//! gate: it is outside the reviewed basis (see [`crate::basis::basis_digest`]),
//! so adding, changing or removing one leaves reviews and dependents current.
//!
//! - **`likelihood.set`** states, changes or removes the likelihood on the
//!   current revision of an unresolved claim, attributed to the command actor
//!   and its self-reported descriptor under `likelihood_statement`.
//! - **`likelihood.resolve`** records the outcome of the resolution condition
//!   with the evidence that settles it, under `likelihood_resolution`. Each
//!   entry keeps the scored forecast: the likelihood as stated, the revision
//!   that stated it, who stated it, and the probability scored with the
//!   mapping version. A later entry with a `correction_reason` corrects the
//!   outcome; earlier entries stay in the log. A resolved forecast is fixed.
//! - **Brier score** is arithmetic on the recorded entry:
//!   `(probability − outcome)²`, with `holds` as 1 and `does_not_hold` as 0.
//!   An `annulled` outcome (the condition can no longer settle the claim) is
//!   counted and never scored. An ICD 203 term is scored at the midpoint of its
//!   published range under [`MAPPING_VERSION`]; the term stays stored as a term.
//! - **Calibration** groups scored forecasts by the forecaster's dependence
//!   group (`model_family:<lowercased family>` as in review independence) and
//!   self-reported model version, with counts. A mean is shown only from
//!   [`MIN_SCORED_FOR_MEAN`] scored forecasts; smaller groups show their n and
//!   nothing else. Groups are listed by key and never ranked.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    catalog::{OperationDescriptor, OperationInfo},
    command::RevisionRef,
    header::{Actor, ActorDescriptor},
    identity::{Id, Kind},
    input,
    review::{Likelihood, LikelihoodEstimate, LikelihoodTerm},
    review_independence::{GroupBasis, dependence_group},
    state::{OperationContext, Record, State},
    wire::{ErrorCode, Failure},
};

/// Version of the scoring and calibration rules in this module.
pub const RULE_VERSION: &str = "likelihood-calibration.v1";

/// Version of the term-to-probability mapping used only for scoring.
pub const MAPPING_VERSION: &str = "icd203-midpoint.v1";

/// Fewest scored forecasts from which a group's mean Brier score is shown.
pub const MIN_SCORED_FOR_MEAN: usize = 10;

/// Claim body key holding the attribution of the current likelihood when it
/// was stated through `likelihood.set`.
pub const STATEMENT_KEY: &str = "likelihood_statement";

/// Claim body key holding the append-only outcome log.
pub const RESOLUTION_KEY: &str = "likelihood_resolution";

/// Stated with every likelihood view.
pub const NOTICE: &str = "Likelihood is a stated probability that a claim's resolution condition will settle it as holding. It is separate from confidence and never satisfies or blocks a gate. A Brier score is (probability - outcome)^2 on the recorded outcome, holds = 1 and does_not_hold = 0; an ICD 203 term is scored at the midpoint of its published range.";

/// Stated with every calibration view.
pub const CALIBRATION_NOTICE: &str = "Groups follow the forecaster's self-reported model family and version, which are claims. A mean Brier score is shown only from the stated minimum of scored forecasts; a smaller group shows its count and no score, and no interpretation. Groups are listed by key, not ranked: a score is not a measure of a family's quality.";

const MAX_RESOLUTIONS: usize = 16;
const MAX_EVIDENCE: usize = 32;
const TEXT_LIMIT: usize = 2048;
const MAX_LISTED_CLAIMS: usize = 64;

/// The kinds that can carry a likelihood.
const CLAIM_KINDS: &[Kind] = &[Kind::Assessment, Kind::Conclusion];

/// Payload pointers at which each operation accepts a likelihood.
const LIKELIHOOD_POINTERS: &[(&str, &str)] = &[
    ("assessment.add", "/likelihood"),
    ("assessment.supersede", "/replacement/likelihood"),
    ("conclusion.create", "/likelihood"),
    ("conclusion.amend", "/replacement/likelihood"),
    ("likelihood.set", "/likelihood"),
];

/// The published ICD 203 range of a term (§D.6.e(2)(a), 2015 text), as
/// probabilities.
pub const fn term_range(term: LikelihoodTerm) -> (f64, f64) {
    match term {
        LikelihoodTerm::AlmostNoChance => (0.01, 0.05),
        LikelihoodTerm::VeryUnlikely => (0.05, 0.20),
        LikelihoodTerm::Unlikely => (0.20, 0.45),
        LikelihoodTerm::RoughlyEvenChance => (0.45, 0.55),
        LikelihoodTerm::Likely => (0.55, 0.80),
        LikelihoodTerm::VeryLikely => (0.80, 0.95),
        LikelihoodTerm::AlmostCertain => (0.95, 0.99),
    }
}

/// The probability a stated estimate is scored at, and the mapping version
/// when a term was mapped.
pub fn scored_probability(estimate: LikelihoodEstimate) -> (f64, Option<&'static str>) {
    match estimate {
        LikelihoodEstimate::Number(probability) => (f64::from(probability), None),
        LikelihoodEstimate::Ordinal(term) => {
            let (low, high) = term_range(term);
            (round6(f64::midpoint(low, high)), Some(MAPPING_VERSION))
        }
    }
}

/// Round to six decimal places, the precision every reported score uses.
fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

/// What the resolution condition settled.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ClaimOutcome {
    /// The condition settled the claim as holding (scored as 1).
    Holds,
    /// The condition settled the claim as not holding (scored as 0).
    DoesNotHold,
    /// The condition can no longer settle the claim. Counted, never scored.
    Annulled,
}

impl ClaimOutcome {
    const fn value(self) -> Option<f64> {
        match self {
            Self::Holds => Some(1.0),
            Self::DoesNotHold => Some(0.0),
            Self::Annulled => None,
        }
    }
}

/// The Brier score of one forecast, rounded to six decimals; `None` for an
/// annulled outcome.
pub fn brier(probability: f64, outcome: ClaimOutcome) -> Option<f64> {
    outcome
        .value()
        .map(|value| round6((probability - value).powi(2)))
}

/// State, change or remove the likelihood on an unresolved claim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LikelihoodSet {
    /// The exact current revision of an Assessment or Conclusion.
    pub claim_ref: RevisionRef,
    /// The new likelihood with its resolution condition, or null to remove it.
    pub likelihood: Option<Likelihood>,
    pub reason: String,
}

/// Record the outcome of a claim's resolution condition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LikelihoodResolve {
    /// The exact current revision of an Assessment or Conclusion that records
    /// a likelihood.
    pub claim_ref: RevisionRef,
    pub outcome: ClaimOutcome,
    /// Exact revisions of the records that settle the condition. At least one.
    pub evidence_refs: Vec<RevisionRef>,
    pub rationale: String,
    /// Required to correct an outcome already recorded; refused otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction_reason: Option<String>,
}

/// Who stated the current likelihood through `likelihood.set`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    /// The likelihood stated; null when it was removed.
    pub likelihood: Option<Likelihood>,
    pub stated_by: Actor,
    pub actor_descriptor_self_reported: Option<ActorDescriptor>,
    pub accepted_at_sequence: u64,
    pub reason: String,
}

/// The forecast a resolution scores, fixed when the outcome is recorded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Forecast {
    pub likelihood: Likelihood,
    /// The claim revision that stated this likelihood.
    pub revision: u64,
    pub stated_by: Actor,
    pub actor_descriptor_self_reported: Option<ActorDescriptor>,
    /// The probability scored: the stated number, or a term's range midpoint.
    pub scored_probability: f64,
    /// The mapping used for a term; null for a stated number.
    pub mapping_version: Option<String>,
}

/// One attributed outcome entry. The resolver is the command actor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Resolution {
    pub outcome: ClaimOutcome,
    pub evidence_refs: Vec<RevisionRef>,
    pub rationale: String,
    pub correction_reason: Option<String>,
    pub resolved_by: Actor,
    pub accepted_at_sequence: u64,
    /// The claim revision the outcome was recorded against.
    pub claim_revision: u64,
    pub forecast: Forecast,
}

pub(crate) fn operations() -> [OperationDescriptor; 2] {
    [
        OperationDescriptor::active(
            OperationInfo {
                name: "likelihood.set",
                stage: "assess",
                summary: "State, change or remove the likelihood on an unresolved Assessment or Conclusion.",
                when_to_use: "Attach a probability to a claim that a stated, observable condition will settle, before the outcome is known.",
                lifecycle: "A current, active Assessment or Conclusion revision whose likelihood has no recorded outcome.",
                effects: "Write the likelihood and its attribution in a new revision. The reviewed basis, reviews and dependents are unaffected: a likelihood never satisfies or blocks a gate.",
                returns: "The claim ID and its new revision.",
                revision_rule: "Supply the exact current claim_ref.",
                idempotency: "Retry the identical command with its original actor and command ID.",
                repair: "State a resolution condition with every likelihood. Without one, state confidence (low, moderate, high or not_assessed with a rationale) on the claim instead.",
                related: &["likelihood.resolve", "assessment.add", "conclusion.create"],
            },
            json!({
                "claim_ref": {"id": "concl_019acd16-0000-7000-8000-000000000001", "revision": 1},
                "likelihood": {
                    "estimate": "likely",
                    "resolution_condition": "The rerun scheduled for the next release reproduces the timeout."
                },
                "reason": "Forecast before the rerun."
            }),
            validate_set,
            set,
        ),
        OperationDescriptor::active(
            OperationInfo {
                name: "likelihood.resolve",
                stage: "assess",
                summary: "Record the outcome of a claim's resolution condition, citing the evidence that settles it.",
                when_to_use: "The condition stated with a likelihood has settled the claim, or can no longer settle it.",
                lifecycle: "A current, active Assessment or Conclusion revision that records a likelihood.",
                effects: "Append one attributed outcome, with the scored forecast, in a new revision. Calibration reads score it; no gate reads it.",
                returns: "The claim ID and its new revision.",
                revision_rule: "Supply the exact current claim_ref and exact evidence revisions in the same case.",
                idempotency: "Retry the identical command with its original actor and command ID.",
                repair: "Cite at least one record that settles the condition. To correct a recorded outcome, supply correction_reason.",
                related: &["likelihood.set", "observation.add"],
            },
            json!({
                "claim_ref": {"id": "concl_019acd16-0000-7000-8000-000000000001", "revision": 2},
                "outcome": "holds",
                "evidence_refs": [{"id": "obs_019acd16-0000-7000-8000-000000000002", "revision": 1}],
                "rationale": "The scheduled rerun reproduced the timeout."
            }),
            validate_resolve,
            resolve,
        ),
    ]
}

fn invalid(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(path)
}

fn gate(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::GateFailed, message).at(path)
}

fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "A stored likelihood, statement or outcome is invalid.",
    )
}

fn require_text(value: &str, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() || value.chars().count() > TEXT_LIMIT {
        return Err(invalid(
            path,
            format!("Supply nonblank text no longer than {TEXT_LIMIT} characters."),
        ));
    }
    Ok(())
}

fn validate_set(payload: &LikelihoodSet) -> Result<(), Failure> {
    require_text(&payload.reason, "/reason")?;
    if let Some(likelihood) = &payload.likelihood {
        let condition: &str = &String::from(likelihood.resolution_condition.clone());
        if condition.chars().count() > TEXT_LIMIT {
            return require_text(condition, "/likelihood/resolution_condition");
        }
    }
    Ok(())
}

fn validate_resolve(payload: &LikelihoodResolve) -> Result<(), Failure> {
    require_text(&payload.rationale, "/rationale")?;
    if let Some(reason) = &payload.correction_reason {
        require_text(reason, "/correction_reason")?;
    }
    if payload.evidence_refs.is_empty() || payload.evidence_refs.len() > MAX_EVIDENCE {
        return Err(invalid(
            "/evidence_refs",
            format!(
                "Cite between 1 and {MAX_EVIDENCE} exact revisions of the records that settle the resolution condition."
            ),
        ));
    }
    Ok(())
}

/// The current, active claim at `reference` in `case`.
fn claim(state: &State, case: Id, reference: RevisionRef, path: &str) -> Result<Record, Failure> {
    if !CLAIM_KINDS.contains(&reference.id.kind()) {
        return Err(invalid(
            path,
            "Likelihood is recorded on an Assessment or a Conclusion.",
        ));
    }
    let record = state
        .current(reference.id)
        .filter(|record| record.header.case_id == case)
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::UnknownReference,
                "The record is unavailable in this case.",
            )
            .at(path)
        })?;
    if record.reference() != reference {
        let mut conflict = Failure::new(
            ErrorCode::RevisionConflict,
            "The record changed. Inspect its current revision.",
        )
        .at(path);
        conflict.error.expected = Some(json!(reference.revision));
        conflict.error.actual = Some(json!(record.header.revision.0));
        conflict.error.retry = Some("reassess".into());
        return Err(conflict);
    }
    let body = &record.body;
    let ended = body["retracted"] == true
        || matches!(
            body["status"].as_str(),
            Some("retracted" | "rejected" | "superseded")
        );
    if ended {
        return Err(gate(
            path,
            "A retracted, rejected or superseded claim takes no new likelihood or outcome; its history stays readable.",
        ));
    }
    Ok(record.clone())
}

/// The likelihood recorded on a claim body, if any.
pub fn recorded(body: &Value) -> Result<Option<Likelihood>, Failure> {
    match body.get("likelihood") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => input::decode(value).map(Some).map_err(|_| integrity()),
    }
}

/// The outcome log recorded on a claim body, oldest first.
pub fn resolutions(body: &Value) -> Result<Vec<Resolution>, Failure> {
    match body.get(RESOLUTION_KEY) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value) => input::decode(value).map_err(|_| integrity()),
    }
}

fn set(payload: LikelihoodSet, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let case = ctx.case_id().ok_or_else(integrity)?;
    let target = claim(ctx.state(), case, payload.claim_ref, "/claim_ref")?;
    if !resolutions(&target.body)?.is_empty() {
        return Err(gate(
            "/claim_ref",
            "This claim's outcome is recorded, so its forecast stays as stated. Amend the claim to state a new claim and forecast.",
        ));
    }
    if recorded(&target.body)? == payload.likelihood {
        return Err(invalid(
            "/likelihood",
            "The claim already records this likelihood.",
        ));
    }
    let mut body = target.body;
    let object = body.as_object_mut().ok_or_else(integrity)?;
    object.insert("likelihood".into(), json!(payload.likelihood));
    object.insert(
        STATEMENT_KEY.into(),
        json!(Statement {
            likelihood: payload.likelihood,
            stated_by: ctx.actor().clone(),
            actor_descriptor_self_reported: ctx.actor_descriptor().cloned(),
            accepted_at_sequence: ctx.acceptance_sequence(),
            reason: payload.reason,
        }),
    );
    ctx.update(payload.claim_ref, body)?;
    Ok(())
}

fn resolve(payload: LikelihoodResolve, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let case = ctx.case_id().ok_or_else(integrity)?;
    let target = claim(ctx.state(), case, payload.claim_ref, "/claim_ref")?;
    let Some(likelihood) = recorded(&target.body)? else {
        return Err(gate(
            "/claim_ref",
            "This claim records no likelihood, so there is no forecast to score.",
        ));
    };
    let mut log = resolutions(&target.body)?;
    match (log.is_empty(), &payload.correction_reason) {
        (false, None) => {
            let mut failure = gate(
                "/correction_reason",
                "An outcome is already recorded for this claim. To correct it, supply correction_reason; the earlier outcome stays in the log.",
            );
            failure.error.actual = log.last().map(|entry| json!(entry.outcome));
            return Err(failure);
        }
        (true, Some(_)) => {
            return Err(invalid(
                "/correction_reason",
                "No outcome is recorded yet, so there is nothing to correct. Omit correction_reason.",
            ));
        }
        _ => {}
    }
    if log.len() >= MAX_RESOLUTIONS {
        return Err(gate(
            "/claim_ref",
            "This claim has reached its outcome limit.",
        ));
    }
    for (index, reference) in payload.evidence_refs.iter().enumerate() {
        let path = format!("/evidence_refs/{index}");
        if reference.id == target.header.id {
            return Err(invalid(
                &path,
                "A claim cannot settle its own resolution condition; cite the records that do.",
            ));
        }
        if ctx
            .state()
            .revision(*reference)
            .is_none_or(|record| record.header.case_id != case)
        {
            return Err(Failure::new(
                ErrorCode::UnknownReference,
                "Cite an exact revision of a record in this case.",
            )
            .at(path));
        }
    }
    let forecast = forecast(ctx.state(), &target, likelihood);
    log.push(Resolution {
        outcome: payload.outcome,
        evidence_refs: payload.evidence_refs,
        rationale: payload.rationale,
        correction_reason: payload.correction_reason,
        resolved_by: ctx.actor().clone(),
        accepted_at_sequence: ctx.acceptance_sequence(),
        claim_revision: payload.claim_ref.revision,
        forecast,
    });
    let mut body = target.body;
    body.as_object_mut()
        .ok_or_else(integrity)?
        .insert(RESOLUTION_KEY.into(), json!(log));
    ctx.update(payload.claim_ref, body)?;
    Ok(())
}

/// The forecast `record` currently states: the earliest revision of the
/// latest unbroken run that records the same likelihood, and its attribution.
/// A `likelihood.set` statement written in that revision names the stating
/// actor. Otherwise the revision's recording actor stated it, with the
/// creator's descriptor only when that actor is the creator.
fn forecast(state: &State, record: &Record, likelihood: Likelihood) -> Forecast {
    let mut introducing = record;
    let revisions: Vec<&Record> = state
        .history(record.header.id)
        .filter(|revision| revision.header.revision.0 <= record.header.revision.0)
        .collect();
    for revision in revisions.iter().rev() {
        match recorded(&revision.body) {
            Ok(Some(stated)) if stated == likelihood => introducing = revision,
            _ => break,
        }
    }
    let statement = introducing
        .body
        .get(STATEMENT_KEY)
        .and_then(|value| input::decode::<Statement>(value).ok())
        .filter(|statement| {
            statement.accepted_at_sequence == introducing.header.acceptance_sequence
                && statement.likelihood.as_ref() == Some(&likelihood)
        });
    let (stated_by, descriptor) = match statement {
        Some(statement) => (
            statement.stated_by,
            statement.actor_descriptor_self_reported,
        ),
        None => {
            let header = &introducing.header;
            let descriptor = if header.recorded_by == header.created_by {
                header.actor_descriptor_self_reported.clone()
            } else {
                None
            };
            (header.recorded_by.clone(), descriptor)
        }
    };
    let (probability, mapping) = scored_probability(likelihood.estimate);
    Forecast {
        likelihood,
        revision: introducing.header.revision.0,
        stated_by,
        actor_descriptor_self_reported: descriptor,
        scored_probability: probability,
        mapping_version: mapping.map(str::to_owned),
    }
}

/// The current same-case revision of a cited claim when every change since
/// the cited revision is to its likelihood, likelihood statement or outcome
/// log; `None` otherwise. Promotion gates read it in place of the cited
/// revision, so stating, changing, removing or resolving a likelihood never
/// changes a gate outcome.
pub fn likelihood_only_successor(state: &State, case: Id, cited: RevisionRef) -> Option<&Record> {
    let current = state
        .current(cited.id)
        .filter(|record| record.header.case_id == case)?;
    let historic = state.revision(cited)?;
    let strip = |body: &Value| {
        let mut body = body.clone();
        if let Some(object) = body.as_object_mut() {
            for key in ["likelihood", STATEMENT_KEY, RESOLUTION_KEY] {
                object.remove(key);
            }
        }
        body
    };
    (CLAIM_KINDS.contains(&cited.id.kind()) && strip(&historic.body) == strip(&current.body))
        .then_some(current)
}

/// Replace a decode failure caused by a likelihood without a stated
/// resolution condition with a rejection that suggests confidence instead.
/// Any other failure is returned unchanged.
pub fn explain_decode_failure(operation: &str, payload: &Value, failure: Failure) -> Failure {
    let Some((_, pointer)) = LIKELIHOOD_POINTERS
        .iter()
        .find(|(name, _)| *name == operation)
    else {
        return failure;
    };
    let Some(value) = payload.pointer(pointer) else {
        return failure;
    };
    let missing_condition = match value {
        Value::Null => false,
        Value::Object(map) => map
            .get("resolution_condition")
            .and_then(Value::as_str)
            .is_none_or(|condition| condition.trim().is_empty()),
        _ => true,
    };
    if !missing_condition {
        return failure;
    }
    let mut explained = Failure::new(
        ErrorCode::InvalidInput,
        "Likelihood is allowed only on a claim with a stated resolution condition: the observable event or check that will settle it. Without one, state confidence instead: an ordinal judgment of the evidentiary basis (low, moderate, high or not_assessed) with a rationale.",
    )
    .at(format!("{pointer}/resolution_condition"));
    explained.error.expected = Some(json!({
        "likelihood": {
            "estimate": "a number in [0, 1] or almost_no_chance | very_unlikely | unlikely | roughly_even_chance | likely | very_likely | almost_certain",
            "resolution_condition": "text naming what will settle the claim",
        },
        "or_confidence": {
            "level": "low | moderate | high | not_assessed",
            "rationale": "text",
            "limitations": [],
            "what_would_change_it": null,
            "downgrade_reasons": null,
        },
    }));
    explained
}

/// The stated likelihood on one claim, as read back.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct StatedView {
    pub estimate: LikelihoodEstimate,
    pub resolution_condition: String,
    /// The probability a resolution scores this estimate at.
    pub scored_probability: f64,
    /// The mapping used for a term; null for a stated number.
    pub mapping_version: Option<&'static str>,
    /// A term's published range; null for a stated number.
    pub term_range: Option<[f64; 2]>,
    pub stated_by: Actor,
    pub stated_at_revision: u64,
    pub dependence_group: String,
}

/// The current outcome of one claim's forecast.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct OutcomeView {
    pub outcome: ClaimOutcome,
    pub evidence_refs: Vec<RevisionRef>,
    pub rationale: String,
    pub resolved_by: Actor,
    pub accepted_at_sequence: u64,
    /// The scored forecast's probability and the mapping version used.
    pub scored_probability: f64,
    pub mapping_version: Option<String>,
    /// Null for an annulled outcome.
    pub brier: Option<f64>,
    /// Earlier outcome entries this one corrects.
    pub corrected_entries: usize,
}

/// Likelihood and outcome of one claim. Never part of a confidence view.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct LikelihoodView {
    pub rule_version: &'static str,
    /// Null when the current revision records no likelihood.
    pub stated: Option<StatedView>,
    /// `unresolved`, `resolved`, or `no_likelihood`.
    pub resolution_state: &'static str,
    pub resolution: Option<OutcomeView>,
    pub notice: &'static str,
}

/// The likelihood view of an Assessment or Conclusion revision that records a
/// likelihood or an outcome; `None` otherwise.
pub fn view(state: &State, record: &Record) -> Option<LikelihoodView> {
    if !CLAIM_KINDS.contains(&record.header.id.kind()) {
        return None;
    }
    let likelihood = recorded(&record.body).ok().flatten();
    let log = resolutions(&record.body).unwrap_or_default();
    if likelihood.is_none() && log.is_empty() {
        return None;
    }
    let stated = likelihood.map(|likelihood| {
        let forecast = forecast(state, record, likelihood.clone());
        let range = match likelihood.estimate {
            LikelihoodEstimate::Ordinal(term) => {
                let (low, high) = term_range(term);
                Some([low, high])
            }
            LikelihoodEstimate::Number(_) => None,
        };
        let (probability, mapping) = scored_probability(likelihood.estimate);
        StatedView {
            estimate: likelihood.estimate,
            resolution_condition: likelihood.resolution_condition.into(),
            scored_probability: probability,
            mapping_version: mapping,
            term_range: range,
            dependence_group: dependence_group(
                &forecast.stated_by,
                forecast.actor_descriptor_self_reported.as_ref(),
            )
            .0,
            stated_by: forecast.stated_by,
            stated_at_revision: forecast.revision,
        }
    });
    let resolution = log.last().map(|entry| OutcomeView {
        outcome: entry.outcome,
        evidence_refs: entry.evidence_refs.clone(),
        rationale: entry.rationale.clone(),
        resolved_by: entry.resolved_by.clone(),
        accepted_at_sequence: entry.accepted_at_sequence,
        scored_probability: entry.forecast.scored_probability,
        mapping_version: entry.forecast.mapping_version.clone(),
        brier: brier(entry.forecast.scored_probability, entry.outcome),
        corrected_entries: log.len() - 1,
    });
    let resolution_state = match (&resolution, &stated) {
        (Some(_), _) => "resolved",
        (None, Some(_)) => "unresolved",
        (None, None) => "no_likelihood",
    };
    Some(LikelihoodView {
        rule_version: RULE_VERSION,
        stated,
        resolution_state,
        resolution,
        notice: NOTICE,
    })
}

/// The view as JSON, for record reads.
pub fn projection(state: &State, record: &Record) -> Option<Value> {
    view(state, record).map(|view| json!(view))
}

fn estimate_words(estimate: &Value) -> String {
    match estimate {
        Value::String(term) => term.replace('_', " "),
        other => other.to_string(),
    }
}

/// One text line for a record view's likelihood, or empty. Always its own
/// sentence, never beside confidence.
pub fn text(view: &Value) -> String {
    let likelihood = &view["likelihood"];
    if !likelihood.is_object() {
        return String::new();
    }
    let mut line = String::from("\nLikelihood: ");
    let stated = &likelihood["stated"];
    if stated.is_object() {
        line.push_str(&estimate_words(&stated["estimate"]));
        if let Some(mapping) = stated["mapping_version"].as_str() {
            line.push_str(&format!(
                " (scored as {} under {mapping})",
                stated["scored_probability"]
            ));
        }
        line.push_str(&format!(
            ", stated by {} at revision {}. Resolves when: {}",
            stated["stated_by"].as_str().unwrap_or("?"),
            stated["stated_at_revision"],
            stated["resolution_condition"].as_str().unwrap_or("?"),
        ));
    } else {
        line.push_str("none stated on this revision.");
    }
    let resolution = &likelihood["resolution"];
    if resolution.is_object() {
        line.push_str(&format!(
            "\nLikelihood outcome: {}, recorded by {}",
            resolution["outcome"].as_str().unwrap_or("?"),
            resolution["resolved_by"].as_str().unwrap_or("?"),
        ));
        if !resolution["brier"].is_null() {
            line.push_str(&format!("; Brier score {}", resolution["brier"]));
        }
        line.push('.');
    } else if stated.is_object() {
        line.push_str("\nLikelihood outcome: not yet recorded.");
    }
    line
}

/// One scored or annulled forecast, as calibration reads it.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ResolvedForecast {
    /// The claim revision that stated the forecast.
    pub forecast_ref: RevisionRef,
    pub dependence_group: String,
    pub basis: GroupBasis,
    pub model_version: Option<String>,
    pub outcome: ClaimOutcome,
    pub scored_probability: f64,
    pub brier: Option<f64>,
}

/// Every resolved forecast in `state`, once each: the latest outcome entry
/// recorded for each stated forecast, across all revisions of each claim, so
/// a forecast stays scored after its claim is amended. Ordered by claim ID and
/// forecast revision.
pub fn resolved_forecasts(
    state: &State,
    include: &dyn Fn(&Record) -> bool,
) -> (Vec<ResolvedForecast>, usize) {
    let mut out = Vec::new();
    let mut withheld = 0;
    for current in state.records() {
        if !CLAIM_KINDS.contains(&current.header.id.kind()) {
            continue;
        }
        let mut latest: BTreeMap<u64, Resolution> = BTreeMap::new();
        for revision in state.history(current.header.id) {
            for entry in resolutions(&revision.body).unwrap_or_default() {
                let slot = latest.entry(entry.forecast.revision);
                match slot {
                    std::collections::btree_map::Entry::Vacant(vacant) => {
                        vacant.insert(entry);
                    }
                    std::collections::btree_map::Entry::Occupied(mut occupied) => {
                        if entry.accepted_at_sequence > occupied.get().accepted_at_sequence {
                            occupied.insert(entry);
                        }
                    }
                }
            }
        }
        if latest.is_empty() {
            continue;
        }
        if !include(current) {
            withheld += latest.len();
            continue;
        }
        for (revision, entry) in latest {
            let forecast = &entry.forecast;
            let (group, basis) = dependence_group(
                &forecast.stated_by,
                forecast.actor_descriptor_self_reported.as_ref(),
            );
            let model_version = match basis {
                GroupBasis::ModelFamily => forecast
                    .actor_descriptor_self_reported
                    .as_ref()
                    .and_then(|descriptor| descriptor.model_version.clone())
                    .filter(|version| !version.trim().is_empty()),
                _ => None,
            };
            out.push(ResolvedForecast {
                forecast_ref: RevisionRef {
                    id: current.header.id,
                    revision,
                },
                dependence_group: group,
                basis,
                model_version,
                outcome: entry.outcome,
                scored_probability: forecast.scored_probability,
                brier: brier(forecast.scored_probability, entry.outcome),
            });
        }
    }
    (out, withheld)
}

/// Whether a mean is shown for a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScoreState {
    /// At least the minimum number of scored forecasts; the mean is shown.
    Shown,
    /// Fewer scored forecasts than the minimum; only the count is shown.
    BelowMinimum,
}

/// Counts and, from the minimum, the mean Brier score of one version.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct VersionCell {
    /// Null when the forecaster declared no model version.
    pub model_version: Option<String>,
    pub scored: usize,
    pub score_state: ScoreState,
    pub mean_brier: Option<f64>,
}

/// Calibration of one dependence group.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct CalibrationGroup {
    pub dependence_group: String,
    pub basis: GroupBasis,
    pub scored: usize,
    pub annulled: usize,
    pub score_state: ScoreState,
    pub mean_brier: Option<f64>,
    /// Model families only: the same counts per self-reported version.
    pub versions: Vec<VersionCell>,
    /// The forecasts counted, by the claim revision that stated each.
    pub forecast_refs: Vec<RevisionRef>,
    pub forecast_refs_omitted: usize,
}

/// Calibration across resolved forecasts.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct CalibrationView {
    pub rule_version: &'static str,
    pub mapping_version: &'static str,
    pub minimum_scored_for_mean: usize,
    pub scored: usize,
    pub annulled: usize,
    /// Current claims stating a likelihood with no recorded outcome.
    pub unresolved: usize,
    /// Resolved forecasts on records this audience may not see; not counted
    /// elsewhere.
    pub withheld: usize,
    /// Ordered by key. Not a ranking.
    pub groups: Vec<CalibrationGroup>,
    pub notice: &'static str,
}

fn mean(values: &[f64]) -> (ScoreState, Option<f64>) {
    if values.len() < MIN_SCORED_FOR_MEAN {
        return (ScoreState::BelowMinimum, None);
    }
    let count = u32::try_from(values.len()).map_or(f64::from(u32::MAX), f64::from);
    (
        ScoreState::Shown,
        Some(round6(values.iter().sum::<f64>() / count)),
    )
}

/// Claims in `state` stating a likelihood with no recorded outcome.
pub fn unresolved(state: &State, include: &dyn Fn(&Record) -> bool) -> usize {
    state
        .records()
        .filter(|record| CLAIM_KINDS.contains(&record.header.id.kind()) && include(record))
        .filter(|record| {
            matches!(recorded(&record.body), Ok(Some(_)))
                && resolutions(&record.body).is_ok_and(|log| log.is_empty())
        })
        .count()
}

/// Group resolved forecasts into a calibration view.
pub fn calibration(
    forecasts: &[ResolvedForecast],
    unresolved: usize,
    withheld: usize,
) -> CalibrationView {
    let mut groups: BTreeMap<&str, Vec<&ResolvedForecast>> = BTreeMap::new();
    for forecast in forecasts {
        groups
            .entry(forecast.dependence_group.as_str())
            .or_default()
            .push(forecast);
    }
    let groups = groups
        .into_iter()
        .map(|(key, members)| {
            let scores: Vec<f64> = members.iter().filter_map(|member| member.brier).collect();
            let (score_state, mean_brier) = mean(&scores);
            let basis = members[0].basis;
            let mut by_version: BTreeMap<Option<&str>, Vec<f64>> = BTreeMap::new();
            if basis == GroupBasis::ModelFamily {
                for member in &members {
                    let scores = by_version
                        .entry(member.model_version.as_deref())
                        .or_default();
                    scores.extend(member.brier);
                }
            }
            let versions = by_version
                .into_iter()
                .map(|(version, scores)| {
                    let (score_state, mean_brier) = mean(&scores);
                    VersionCell {
                        model_version: version.map(str::to_owned),
                        scored: scores.len(),
                        score_state,
                        mean_brier,
                    }
                })
                .collect();
            let refs: Vec<RevisionRef> = members.iter().map(|member| member.forecast_ref).collect();
            CalibrationGroup {
                dependence_group: key.to_owned(),
                basis,
                scored: scores.len(),
                annulled: members.len() - scores.len(),
                score_state,
                mean_brier,
                versions,
                forecast_refs_omitted: refs.len().saturating_sub(MAX_LISTED_CLAIMS),
                forecast_refs: refs.into_iter().take(MAX_LISTED_CLAIMS).collect(),
            }
        })
        .collect::<Vec<_>>();
    CalibrationView {
        rule_version: RULE_VERSION,
        mapping_version: MAPPING_VERSION,
        minimum_scored_for_mean: MIN_SCORED_FOR_MEAN,
        scored: groups.iter().map(|group| group.scored).sum(),
        annulled: groups.iter().map(|group| group.annulled).sum(),
        unresolved,
        withheld,
        groups,
        notice: CALIBRATION_NOTICE,
    }
}

/// Text rendering of a calibration view: counts first, a mean only where it
/// is shown, groups in key order.
pub fn calibration_text(view: &CalibrationView) -> String {
    let mut text = format!(
        "Calibration ({}): {} scored, {} annulled, {} unresolved",
        view.rule_version, view.scored, view.annulled, view.unresolved
    );
    if view.withheld > 0 {
        text.push_str(&format!(", {} withheld", view.withheld));
    }
    text.push('\n');
    if view.groups.is_empty() {
        text.push_str("  No resolved forecasts are recorded.\n");
    }
    let score = |state: ScoreState, mean: Option<f64>, scored: usize| match (state, mean) {
        (ScoreState::Shown, Some(mean)) => format!("n={scored}, mean Brier {mean}"),
        _ => format!(
            "n={scored}, no score below {}",
            view.minimum_scored_for_mean
        ),
    };
    for group in &view.groups {
        text.push_str(&format!(
            "  {}: {}; {} annulled\n",
            group.dependence_group,
            score(group.score_state, group.mean_brier, group.scored),
            group.annulled
        ));
        for version in &group.versions {
            text.push_str(&format!(
                "    version {}: {}\n",
                version.model_version.as_deref().unwrap_or("undeclared"),
                score(version.score_state, version.mean_brier, version.scored)
            ));
        }
    }
    text.push_str(&format!(
        "Term mapping {} is used only for scoring.\n{}\n",
        view.mapping_version, view.notice
    ));
    text
}

#[cfg(test)]
mod tests;
