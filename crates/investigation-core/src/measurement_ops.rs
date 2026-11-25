//! Attributed measurement validity for one Observation or one of its channels.
//!
//! `measurement.assess` appends one judgment to the assessed Observation under
//! `measurement_assessment` in a new revision. The observed value, basis and
//! earlier revisions stay unchanged, and no judgment is removed. The latest
//! judgment per assessor and channel is current; judgments may disagree.
//!
//! Validity is its own outcome dimension. It is independent of Run execution
//! status and evidence completeness, and it never deletes what an instrument
//! produced. Reads compute effective validity from the stored judgments and the
//! current case state; `unknown` is the default and is always reported.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    catalog::{OperationDescriptor, OperationInfo},
    command::RevisionRef,
    evidence::MeasurementValidity,
    header::{Actor, Knowledge},
    identity::Kind,
    input,
    measurement_fields::{IdentityComponents, observation_fields},
    state::{OperationContext, Record, State},
    wire::{ErrorCode, Failure},
};

/// Observation metadata key that holds the append-only judgment log.
pub const MEASUREMENT_ASSESSMENT_KEY: &str = "measurement_assessment";
const MAX_JUDGMENTS: usize = 64;
const MAX_VERIFICATIONS: usize = 16;
const MAX_LIMITS: usize = 32;
const MAX_REFS: usize = 32;
const TEXT_LIMIT: usize = 2048;
const NAME_LIMIT: usize = 256;

/// What a judgment relies on. Controls and instrument checks cite exact,
/// current revisions; the core records whether each one qualifies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VerificationRequest {
    /// A comparable Observation. It qualifies only when its own effective
    /// validity is `valid` and its recorded Run environment equals
    /// `expected_environment`, or the assessed Observation's recorded Run
    /// environment when `expected_environment` is null. When the assessed
    /// Observation or its Run records a structured environment or tool
    /// identity, each known component must also be recorded with the same
    /// value for the control.
    Control {
        observation_ref: RevisionRef,
        expected_environment: Option<String>,
        rationale: String,
    },
    /// An Observation or Artifact showing that the instrument or setup behaved
    /// as specified. A cited Observation must itself be effectively `valid`.
    InstrumentCheck {
        evidence_ref: RevisionRef,
        rationale: String,
    },
    /// An explicitly justified equivalent verification without a recorded
    /// control. It is labeled `assessor_justified`, never `qualified`.
    Equivalent {
        justification: String,
        #[serde(default)]
        evidence_refs: Vec<RevisionRef>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Qualification {
    /// The core checked the cited records and the comparability rule.
    Qualified,
    /// The assessor's stated equivalent; the core checked only its references.
    AssessorJustified,
    /// Recorded for the history, but the judgment cannot rest on it.
    Unqualified,
}

/// A verification with the qualification computed when it was recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    pub request: VerificationRequest,
    pub qualification: Qualification,
    /// Stable reason codes for an unqualified verification.
    pub reasons: Vec<String>,
    /// Recorded Run environments compared for a control. Null when not compared.
    pub compared_environments: Option<ComparedEnvironments>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparedEnvironments {
    pub required: Option<String>,
    pub control: Option<String>,
    /// Structured environment and tool components compared, when the assessed
    /// Observation or its Run records them. Empty when none were compared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ComponentComparison>,
}

/// One known identity component of the assessed measurement and the control's
/// recorded value for it. `control` is null when unrecorded or unknown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentComparison {
    pub scope: IdentityScope,
    pub component: String,
    pub required: String,
    pub control: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdentityScope {
    Environment,
    Tool,
}

/// One attributed judgment. The assessor is the command actor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MeasurementJudgment {
    pub assessor: Actor,
    pub accepted_at_sequence: u64,
    pub observation_revision: u64,
    pub channel: Option<String>,
    pub validity: MeasurementValidity,
    pub verification: Vec<Verification>,
    pub rationale: String,
    pub limits: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MeasurementLog {
    pub judgments: Vec<MeasurementJudgment>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MeasurementAssess {
    /// Exact current Observation revision being judged.
    observation_ref: RevisionRef,
    /// Named channel or series within the Observation. Null judges the whole Observation.
    channel: Option<String>,
    validity: MeasurementValidity,
    /// Instrument checks, controls or justified equivalents relied on.
    /// `valid` and `partial` need at least one that qualifies.
    #[serde(default)]
    verification: Vec<VerificationRequest>,
    rationale: String,
    #[serde(default)]
    limits: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValidityBasis {
    /// At least one current `measurement.assess` judgment applies.
    Assessed,
    /// Only the validity recorded with the Observation applies.
    Recorded,
    /// Nothing has been judged; the state is `unknown`.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CurrentJudgment {
    pub assessor: Actor,
    pub channel: Option<String>,
    pub validity: MeasurementValidity,
    /// What this judgment contributes now. A `valid` or `partial` judgment
    /// whose verification changed since it was recorded contributes `unknown`.
    pub contributes: MeasurementValidity,
    /// Null for the validity recorded with the Observation.
    pub accepted_at_sequence: Option<u64>,
    pub verification_current: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChannelValidity {
    pub channel: String,
    pub state: MeasurementValidity,
}

/// Effective validity of one Observation revision against one case state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ValidityView {
    pub state: MeasurementValidity,
    pub basis: ValidityBasis,
    /// The validity recorded with the Observation by its recorder.
    pub recorded: MeasurementValidity,
    pub channels: Vec<ChannelValidity>,
    pub current: Vec<CurrentJudgment>,
    /// Every judgment kept for this revision, including earlier ones.
    pub judgments_kept: usize,
    pub disagreement: bool,
}

/// Validity beside one cited use of an Observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CitedValidity {
    pub observation_ref: RevisionRef,
    pub current_revision: Option<u64>,
    pub state: MeasurementValidity,
    pub basis: ValidityBasis,
}

pub(crate) fn operations() -> [OperationDescriptor; 1] {
    [OperationDescriptor::active(
        OperationInfo {
            name: "measurement.assess",
            stage: "assess",
            summary: "Record an attributed validity judgment for one Observation or one of its channels.",
            when_to_use: "Judge whether the instrument, setup and controls support interpreting a recorded measurement.",
            lifecycle: "A current, unretracted Observation revision in an existing case.",
            effects: "Append one judgment to the Observation's measurement_assessment in a new revision. The value, basis and earlier judgments stay unchanged.",
            returns: "The Observation ID and its new revision.",
            revision_rule: "Supply the exact current observation_ref. Interpretations that cite an earlier revision need review.",
            idempotency: "Retry the identical command with its original actor and command ID.",
            repair: "Read the current Observation and cite current exact revisions. A valid or partial judgment needs a qualifying instrument check, a comparable control or a justified equivalent.",
            related: &[
                "observation.add",
                "observation.correct",
                "run.record",
                "assessment.add",
            ],
        },
        json!({
            "observation_ref": {"id": "obs_019acd16-0000-7000-8000-000000000001", "revision": 1},
            "channel": null,
            "validity": "invalid",
            "verification": [{
                "kind": "equivalent",
                "justification": "The instrument log reports dropped samples during the recorded window.",
                "evidence_refs": []
            }],
            "rationale": "Dropped samples mean the recorded value cannot be interpreted.",
            "limits": ["The judgment covers the recorded window only."]
        }),
        validate,
        assess,
    )]
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
        "A stored Observation measurement assessment is invalid.",
    )
}

fn text(value: &str, max: usize, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() || value.chars().count() > max {
        return Err(invalid(
            path,
            format!("Supply nonblank text no longer than {max} characters."),
        ));
    }
    Ok(())
}

fn kind_in(reference: RevisionRef, kinds: &[Kind], path: &str) -> Result<(), Failure> {
    if kinds.contains(&reference.id.kind()) {
        Ok(())
    } else {
        Err(Failure::wrong_kind(
            ErrorCode::InvalidInput,
            path,
            kinds,
            reference.id.kind(),
        ))
    }
}

fn validate(payload: &MeasurementAssess) -> Result<(), Failure> {
    kind_in(
        payload.observation_ref,
        &[Kind::Observation],
        "/observation_ref",
    )?;
    if let Some(channel) = &payload.channel {
        text(channel, NAME_LIMIT, "/channel")?;
    }
    text(&payload.rationale, TEXT_LIMIT, "/rationale")?;
    if payload.limits.len() > MAX_LIMITS {
        return Err(invalid("/limits", "Supply at most 32 limits."));
    }
    for (index, limit) in payload.limits.iter().enumerate() {
        text(limit, TEXT_LIMIT, &format!("/limits/{index}"))?;
    }
    if payload.verification.len() > MAX_VERIFICATIONS {
        return Err(invalid("/verification", "Supply at most 16 verifications."));
    }
    for (index, verification) in payload.verification.iter().enumerate() {
        let path = format!("/verification/{index}");
        match verification {
            VerificationRequest::Control {
                observation_ref,
                expected_environment,
                rationale,
            } => {
                kind_in(
                    *observation_ref,
                    &[Kind::Observation],
                    &format!("{path}/observation_ref"),
                )?;
                if let Some(environment) = expected_environment {
                    text(
                        environment,
                        NAME_LIMIT,
                        &format!("{path}/expected_environment"),
                    )?;
                }
                text(rationale, TEXT_LIMIT, &format!("{path}/rationale"))?;
            }
            VerificationRequest::InstrumentCheck {
                evidence_ref,
                rationale,
            } => {
                kind_in(
                    *evidence_ref,
                    &[Kind::Observation, Kind::Artifact],
                    &format!("{path}/evidence_ref"),
                )?;
                text(rationale, TEXT_LIMIT, &format!("{path}/rationale"))?;
            }
            VerificationRequest::Equivalent {
                justification,
                evidence_refs,
            } => {
                text(justification, TEXT_LIMIT, &format!("{path}/justification"))?;
                if evidence_refs.len() > MAX_REFS {
                    return Err(invalid(
                        &format!("{path}/evidence_refs"),
                        "Supply at most 32 references.",
                    ));
                }
                for (item, reference) in evidence_refs.iter().enumerate() {
                    kind_in(
                        *reference,
                        &[Kind::Observation, Kind::Artifact, Kind::Source, Kind::Run],
                        &format!("{path}/evidence_refs/{item}"),
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Same-case current record at the exact cited revision.
fn exact_current<'a>(
    state: &'a State,
    case: crate::identity::Id,
    reference: RevisionRef,
    path: &str,
) -> Result<&'a Record, Failure> {
    let record = state
        .current(reference.id)
        .filter(|record| record.header.case_id == case)
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::UnknownReference,
                "The record is absent from this case.",
            )
            .at(path)
        })?;
    if record.reference() != reference {
        let mut failure = Failure::new(
            ErrorCode::RevisionConflict,
            "The record changed; read its current revision before retrying.",
        )
        .at(path);
        failure.error.expected = Some(json!(reference.revision));
        failure.error.actual = Some(json!(record.reference().revision));
        failure.error.retry = Some("read current revision and reassess".into());
        return Err(failure);
    }
    Ok(record)
}

fn inactive(record: &Record) -> bool {
    let body = &record.body;
    body["retracted"] == true
        || matches!(
            body["status"].as_str(),
            Some("retracted" | "withdrawn" | "rejected" | "superseded")
        )
        || matches!(body["lifecycle"].as_str(), Some("withdrawn" | "retired"))
        || matches!(
            body["availability"].as_str(),
            Some("missing" | "intentionally_removed")
        )
}

/// The environment recorded on the exact Run an Observation cites, if any.
pub fn recorded_environment(state: &State, observation: &Record) -> Option<String> {
    let run: RevisionRef = input::decode(observation.body.get("run_ref")?).ok()?;
    let record = state
        .revision(run)
        .filter(|record| record.header.case_id == observation.header.case_id)?;
    record
        .body
        .get("environment")
        .and_then(Value::as_str)
        .filter(|environment| !environment.trim().is_empty())
        .map(str::to_owned)
}

/// The structured identity an Observation records, or else its exact Run's.
fn recorded_identity(
    state: &State,
    observation: &Record,
    scope: IdentityScope,
) -> Option<Knowledge<IdentityComponents>> {
    let fields = observation_fields(state, observation);
    fields
        .first(|fields| match scope {
            IdentityScope::Environment => fields.environment_identity.as_ref(),
            IdentityScope::Tool => fields.tool_identity.as_ref(),
        })
        .map(|(_, identity)| identity.clone())
}

/// Compare every known structured component of the target with the control.
/// Unknown target components cannot be required and are not compared.
fn compare_identities(
    state: &State,
    target: &Record,
    control: &Record,
    reasons: &mut Vec<&'static str>,
) -> Vec<ComponentComparison> {
    let mut compared = Vec::new();
    for scope in [IdentityScope::Environment, IdentityScope::Tool] {
        let Some(Knowledge::Known(required)) = recorded_identity(state, target, scope) else {
            continue;
        };
        let control_identity = match recorded_identity(state, control, scope) {
            Some(Knowledge::Known(identity)) => Some(identity),
            _ => None,
        };
        let (unrecorded, unknown_component, mismatch) = match scope {
            IdentityScope::Environment => (
                "CONTROL_ENVIRONMENT_IDENTITY_UNRECORDED",
                "CONTROL_ENVIRONMENT_COMPONENT_UNRECORDED",
                "CONTROL_ENVIRONMENT_COMPONENT_MISMATCH",
            ),
            IdentityScope::Tool => (
                "CONTROL_TOOL_IDENTITY_UNRECORDED",
                "CONTROL_TOOL_COMPONENT_UNRECORDED",
                "CONTROL_TOOL_COMPONENT_MISMATCH",
            ),
        };
        for (component, value) in &required {
            let Knowledge::Known(value) = value else {
                continue;
            };
            let actual = control_identity
                .as_ref()
                .and_then(|identity| identity.get(component))
                .and_then(|actual| match actual {
                    Knowledge::Known(actual) => Some(actual.clone()),
                    Knowledge::Unknown => None,
                });
            let reason = match &actual {
                _ if control_identity.is_none() => unrecorded,
                None => unknown_component,
                Some(actual) if actual != value => mismatch,
                Some(_) => "",
            };
            if !reason.is_empty() && !reasons.contains(&reason) {
                reasons.push(reason);
            }
            compared.push(ComponentComparison {
                scope,
                component: component.clone(),
                required: value.clone(),
                control: actual,
            });
        }
    }
    compared
}

fn resolve(
    state: &State,
    target: &Record,
    request: VerificationRequest,
    path: &str,
) -> Result<Verification, Failure> {
    let case = target.header.case_id;
    let mut reasons = Vec::new();
    let mut compared = None;
    let qualification = match &request {
        VerificationRequest::Control {
            observation_ref,
            expected_environment,
            ..
        } => {
            let control = exact_current(
                state,
                case,
                *observation_ref,
                &format!("{path}/observation_ref"),
            )?;
            if control.header.id == target.header.id {
                reasons.push("CONTROL_IS_ASSESSED_OBSERVATION");
            }
            if inactive(control) {
                reasons.push("CONTROL_RETRACTED");
            }
            if effective_validity(state, control) != MeasurementValidity::Valid {
                reasons.push("CONTROL_NOT_VALID");
            }
            let required = expected_environment
                .clone()
                .or_else(|| recorded_environment(state, target));
            let actual = recorded_environment(state, control);
            let components = compare_identities(state, target, control, &mut reasons);
            let structured_environment = components
                .iter()
                .any(|item| item.scope == IdentityScope::Environment);
            match (&required, &actual) {
                (None, _) if structured_environment => {}
                (None, _) => reasons.push("REQUIRED_ENVIRONMENT_UNRECORDED"),
                (_, None) => reasons.push("CONTROL_ENVIRONMENT_UNRECORDED"),
                (Some(required), Some(actual)) if required != actual => {
                    reasons.push("CONTROL_ENVIRONMENT_MISMATCH");
                }
                _ => {}
            }
            compared = Some(ComparedEnvironments {
                required,
                control: actual,
                components,
            });
            if reasons.is_empty() {
                Qualification::Qualified
            } else {
                Qualification::Unqualified
            }
        }
        VerificationRequest::InstrumentCheck { evidence_ref, .. } => {
            let evidence =
                exact_current(state, case, *evidence_ref, &format!("{path}/evidence_ref"))?;
            if evidence.header.id == target.header.id {
                reasons.push("CHECK_IS_ASSESSED_OBSERVATION");
            }
            if inactive(evidence) {
                reasons.push("CHECK_UNAVAILABLE");
            }
            if evidence.header.id.kind() == Kind::Observation
                && effective_validity(state, evidence) != MeasurementValidity::Valid
            {
                reasons.push("CHECK_NOT_VALID");
            }
            if reasons.is_empty() {
                Qualification::Qualified
            } else {
                Qualification::Unqualified
            }
        }
        VerificationRequest::Equivalent { evidence_refs, .. } => {
            let mut seen = BTreeSet::new();
            for (index, reference) in evidence_refs.iter().enumerate() {
                let item = format!("{path}/evidence_refs/{index}");
                if !seen.insert(reference.id) {
                    return Err(invalid(&item, "Cite each record once."));
                }
                let record = exact_current(state, case, *reference, &item)?;
                if inactive(record) {
                    reasons.push("EVIDENCE_UNAVAILABLE");
                }
            }
            if reasons.is_empty() {
                Qualification::AssessorJustified
            } else {
                Qualification::Unqualified
            }
        }
    };
    Ok(Verification {
        request,
        qualification,
        reasons: reasons.into_iter().map(str::to_owned).collect(),
        compared_environments: compared,
    })
}

/// The stored judgment log. An absent key is an empty log.
pub fn judgment_log(body: &Value) -> Result<MeasurementLog, Failure> {
    match body.get(MEASUREMENT_ASSESSMENT_KEY) {
        None | Some(Value::Null) => Ok(MeasurementLog::default()),
        Some(value) => input::decode(value).map_err(|_| integrity()),
    }
}

fn assess(payload: MeasurementAssess, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let case = ctx.case_id().ok_or_else(integrity)?;
    let target = exact_current(
        ctx.state(),
        case,
        payload.observation_ref,
        "/observation_ref",
    )?
    .clone();
    if inactive(&target) {
        return Err(gate(
            "/observation_ref",
            "A retracted Observation has no current use to qualify; its history stays readable.",
        ));
    }
    let mut log = judgment_log(&target.body)?;
    if log.judgments.len() >= MAX_JUDGMENTS {
        return Err(gate(
            "/observation_ref",
            "This Observation has reached its judgment limit; record a corrected Observation instead.",
        ));
    }
    let verification = payload
        .verification
        .into_iter()
        .enumerate()
        .map(|(index, request)| {
            resolve(
                ctx.state(),
                &target,
                request,
                &format!("/verification/{index}"),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    if matches!(
        payload.validity,
        MeasurementValidity::Valid | MeasurementValidity::Partial
    ) && verification
        .iter()
        .all(|item| item.qualification == Qualification::Unqualified)
    {
        let (path, reasons) = verification
            .iter()
            .enumerate()
            .next()
            .map(|(index, item)| (format!("/verification/{index}"), item.reasons.join(", ")))
            .unwrap_or_else(|| ("/verification".into(), "none supplied".into()));
        let mut failure = gate(
            &path,
            format!(
                "A valid or partial judgment needs a qualifying instrument check, comparable control or justified equivalent. Unqualified: {reasons}."
            ),
        );
        failure.error.actual = Some(json!(
            verification
                .iter()
                .map(|item| &item.reasons)
                .collect::<Vec<_>>()
        ));
        return Err(failure);
    }
    let mut body = target.body.clone();
    log.judgments.push(MeasurementJudgment {
        assessor: ctx.actor().clone(),
        accepted_at_sequence: ctx.acceptance_sequence(),
        observation_revision: payload.observation_ref.revision,
        channel: payload.channel,
        validity: payload.validity,
        verification,
        rationale: payload.rationale,
        limits: payload.limits,
    });
    body.as_object_mut()
        .ok_or_else(integrity)?
        .insert(MEASUREMENT_ASSESSMENT_KEY.into(), json!(log));
    ctx.update(payload.observation_ref, body)?;
    Ok(())
}

/// Conservative order of effective validity: `invalid` < `partial` < `unknown` < `valid`.
pub(crate) const fn validity_rank(validity: MeasurementValidity) -> u8 {
    match validity {
        MeasurementValidity::Invalid => 0,
        MeasurementValidity::Partial => 1,
        MeasurementValidity::Unknown => 2,
        MeasurementValidity::Valid => 3,
    }
}

fn lowest(values: impl IntoIterator<Item = MeasurementValidity>) -> Option<MeasurementValidity> {
    values
        .into_iter()
        .min_by_key(|validity| validity_rank(*validity))
}

fn still_current(state: &State, case: crate::identity::Id, reference: RevisionRef) -> bool {
    state
        .current(reference.id)
        .filter(|record| record.header.case_id == case)
        .is_some_and(|record| record.reference() == reference && !inactive(record))
}

/// Whether at least one verification a judgment could rest on still cites
/// current, active revisions.
fn verification_current(
    state: &State,
    case: crate::identity::Id,
    judgment: &MeasurementJudgment,
) -> bool {
    judgment.verification.iter().any(|item| {
        item.qualification != Qualification::Unqualified
            && match &item.request {
                VerificationRequest::Control {
                    observation_ref, ..
                } => still_current(state, case, *observation_ref),
                VerificationRequest::InstrumentCheck { evidence_ref, .. } => {
                    still_current(state, case, *evidence_ref)
                }
                VerificationRequest::Equivalent { evidence_refs, .. } => evidence_refs
                    .iter()
                    .all(|reference| still_current(state, case, *reference)),
            }
    })
}

/// Effective validity of this Observation revision against `state`.
///
/// The latest judgment per assessor and channel is current. The validity
/// recorded with the Observation counts as its recorder's judgment of the whole
/// Observation unless it is `unknown` or that recorder judged it again. Current
/// judgments combine conservatively (`invalid` < `partial` < `unknown` <
/// `valid`). A whole Observation judged `valid` with any channel judged lower is
/// `partial`. Channel judgments alone never make the whole Observation valid.
pub fn validity_view(state: &State, record: &Record) -> Result<ValidityView, Failure> {
    if record.header.id.kind() != Kind::Observation {
        return Err(integrity());
    }
    let case = record.header.case_id;
    let recorded = match record.body.get("validity") {
        None | Some(Value::Null) => MeasurementValidity::Unknown,
        Some(value) => input::decode(value).map_err(|_| integrity())?,
    };
    let log = judgment_log(&record.body)?;
    let mut latest: BTreeMap<(Actor, Option<String>), &MeasurementJudgment> = BTreeMap::new();
    for judgment in &log.judgments {
        latest.insert(
            (judgment.assessor.clone(), judgment.channel.clone()),
            judgment,
        );
    }
    let mut current: Vec<CurrentJudgment> = latest
        .values()
        .map(|judgment| {
            let fresh = verification_current(state, case, judgment);
            let contributes = match judgment.validity {
                MeasurementValidity::Valid | MeasurementValidity::Partial if !fresh => {
                    MeasurementValidity::Unknown
                }
                validity => validity,
            };
            CurrentJudgment {
                assessor: judgment.assessor.clone(),
                channel: judgment.channel.clone(),
                validity: judgment.validity,
                contributes,
                accepted_at_sequence: Some(judgment.accepted_at_sequence),
                verification_current: fresh,
            }
        })
        .collect();
    let assessed = !current.is_empty();
    let recorder = &record.header.created_by;
    if recorded != MeasurementValidity::Unknown && !latest.contains_key(&(recorder.clone(), None)) {
        current.push(CurrentJudgment {
            assessor: recorder.clone(),
            channel: None,
            validity: recorded,
            contributes: recorded,
            accepted_at_sequence: None,
            verification_current: false,
        });
    }
    let whole_values: Vec<_> = current
        .iter()
        .filter(|judgment| judgment.channel.is_none())
        .map(|judgment| judgment.contributes)
        .collect();
    let whole = lowest(whole_values.iter().copied());
    let mut by_channel: BTreeMap<&str, Vec<MeasurementValidity>> = BTreeMap::new();
    for judgment in &current {
        if let Some(channel) = &judgment.channel {
            by_channel
                .entry(channel.as_str())
                .or_default()
                .push(judgment.contributes);
        }
    }
    let mut disagreement = whole_values
        .iter()
        .map(|value| validity_rank(*value))
        .collect::<BTreeSet<_>>()
        .len()
        > 1;
    let channels: Vec<_> = by_channel
        .iter()
        .map(|(channel, values)| {
            disagreement |= values
                .iter()
                .map(|value| validity_rank(*value))
                .collect::<BTreeSet<_>>()
                .len()
                > 1;
            ChannelValidity {
                channel: (*channel).to_owned(),
                state: lowest(values.iter().copied().chain(whole))
                    .unwrap_or(MeasurementValidity::Unknown),
            }
        })
        .collect();
    let state_value = match whole {
        Some(MeasurementValidity::Valid)
            if channels
                .iter()
                .any(|channel| channel.state != MeasurementValidity::Valid) =>
        {
            MeasurementValidity::Partial
        }
        Some(whole) => whole,
        None => MeasurementValidity::Unknown,
    };
    let basis = if assessed {
        ValidityBasis::Assessed
    } else if recorded != MeasurementValidity::Unknown {
        ValidityBasis::Recorded
    } else {
        ValidityBasis::None
    };
    Ok(ValidityView {
        state: state_value,
        basis,
        recorded,
        channels,
        current,
        judgments_kept: log.judgments.len(),
        disagreement,
    })
}

/// The effective validity used by evidence gates. Unreadable data is `unknown`.
pub fn effective_validity(state: &State, record: &Record) -> MeasurementValidity {
    validity_view(state, record)
        .map(|view| view.state)
        .unwrap_or(MeasurementValidity::Unknown)
}

/// Effective validity of the current revision of every Observation a record
/// cites in its evidence fields, in citation order.
pub fn cited_validity(state: &State, record: &Record) -> Vec<CitedValidity> {
    let mut seen = BTreeSet::new();
    let mut cited = Vec::new();
    for field in ["evidence_refs", "support_refs", "contradiction_refs"] {
        let Some(items) = record.body.get(field).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let Ok(reference) = input::decode::<RevisionRef>(item) else {
                continue;
            };
            if reference.id.kind() != Kind::Observation
                || !seen.insert((reference.id, reference.revision))
            {
                continue;
            }
            let current = state
                .current(reference.id)
                .filter(|current| current.header.case_id == record.header.case_id);
            let view = current.and_then(|current| validity_view(state, current).ok());
            cited.push(CitedValidity {
                observation_ref: reference,
                current_revision: current.map(|current| current.header.revision.0),
                state: view
                    .as_ref()
                    .map_or(MeasurementValidity::Unknown, |view| view.state),
                basis: view.map_or(ValidityBasis::None, |view| view.basis),
            });
        }
    }
    cited
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::standard_catalog,
        command::Command,
        header::SystemClock,
        identity::{Id, SystemIdSource},
        review::EffectiveDisposition,
        review_ops::effective_disposition,
        state::{Acceptance, AcceptedTransaction, validate_and_apply},
    };

    struct Case {
        state: State,
        case: Option<Id>,
    }

    impl Case {
        fn new() -> Self {
            let mut test = Self {
                state: State::default(),
                case: None,
            };
            let accepted = test
                .submit(
                    "case.create",
                    json!({"title": "Validity study", "mode": "record"}),
                )
                .unwrap();
            test.case = Some(accepted.case_id);
            test
        }

        fn submit(&mut self, op: &str, payload: Value) -> Result<AcceptedTransaction, Failure> {
            self.submit_as("agent:test", op, payload)
        }

        fn submit_as(
            &mut self,
            actor: &str,
            op: &str,
            payload: Value,
        ) -> Result<AcceptedTransaction, Failure> {
            let sequence = self.state.sequence() + 1;
            let command: Command = serde_json::from_value(json!({
                "schema_version": "inv.command.v1",
                "command_id": format!("command-{sequence}"),
                "case_id": self.case,
                "actor": actor,
                "preconditions": [],
                "operations": [{"op": op, "payload": payload}],
            }))
            .unwrap();
            let accepted = validate_and_apply(
                &self.state,
                &command,
                &standard_catalog(),
                Acceptance {
                    sequence,
                    clock: &mut SystemClock,
                    ids: &mut SystemIdSource,
                },
            )?;
            self.state.apply(&accepted)?;
            Ok(accepted)
        }

        fn create(&mut self, op: &str, payload: Value) -> RevisionRef {
            self.submit(op, payload)
                .unwrap()
                .records
                .last()
                .unwrap()
                .reference()
        }

        fn run(&mut self, status: &str, environment: Option<&str>) -> RevisionRef {
            self.create(
                "run.record",
                json!({
                    "intent": {"kind": "exploration", "intent": "Repeat the measurement."},
                    "producer": "agent:producer",
                    "environment": environment,
                    "artifact_refs": [],
                    "execution_status": status,
                    "evidence_completeness": "unknown",
                    "evidence_kind": "observational",
                    "variables_changed": [],
                }),
            )
        }

        fn observation(&mut self, validity: &str, run: Option<RevisionRef>) -> RevisionRef {
            self.create(
                "observation.add",
                json!({
                    "value": {"kind": "measurement", "value": "12.5", "unit": "unit"},
                    "basis": {"kind": "assertion", "assertion": "Reported by the recorder.", "context": null},
                    "extraction_method": null,
                    "validity": validity,
                    "quality": [],
                    "limitations": [],
                    "evidence_kind": "observational",
                    "variables_changed": [],
                    "observed_at": null,
                    "run_ref": run,
                }),
            )
        }

        fn assess(
            &mut self,
            actor: &str,
            target: RevisionRef,
            validity: &str,
            verification: Value,
        ) -> Result<RevisionRef, Failure> {
            self.assess_channel(actor, target, None, validity, verification)
        }

        fn assess_channel(
            &mut self,
            actor: &str,
            target: RevisionRef,
            channel: Option<&str>,
            validity: &str,
            verification: Value,
        ) -> Result<RevisionRef, Failure> {
            self.submit_as(
                actor,
                "measurement.assess",
                json!({
                    "observation_ref": target,
                    "channel": channel,
                    "validity": validity,
                    "verification": verification,
                    "rationale": "Judged from the recorded setup.",
                    "limits": ["Recorded window only."],
                }),
            )
            .map(|accepted| accepted.records[0].reference())
        }

        fn view(&self, reference: RevisionRef) -> ValidityView {
            validity_view(&self.state, self.state.current(reference.id).unwrap()).unwrap()
        }
    }

    fn equivalent() -> Value {
        json!([{"kind": "equivalent", "justification": "A known reference value was reproduced."}])
    }

    fn control(observation: RevisionRef, expected: Option<&str>) -> Value {
        json!([{
            "kind": "control",
            "observation_ref": observation,
            "expected_environment": expected,
            "rationale": "The control shares the target's recorded setup.",
        }])
    }

    #[test]
    fn a_valid_control_under_the_wrong_environment_does_not_qualify_the_target() {
        let mut case = Case::new();
        let treatment_run = case.run("completed", Some("build-41"));
        let wrong_run = case.run("completed", Some("build-40"));
        let right_run = case.run("completed", Some("build-41"));
        let target = case.observation("unknown", Some(treatment_run));
        let wrong = case.observation("valid", Some(wrong_run));
        let right = case.observation("valid", Some(right_run));
        let unrecorded = case.observation("valid", None);

        let failure = case
            .assess("agent:reviewer", target, "valid", control(wrong, None))
            .unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::GateFailed);
        assert_eq!(
            failure.error.path.as_deref(),
            Some("/operations/0/payload/verification/0")
        );
        assert!(
            failure
                .error
                .message
                .contains("CONTROL_ENVIRONMENT_MISMATCH")
        );
        assert_eq!(case.view(target).state, MeasurementValidity::Unknown);
        assert_eq!(case.state.current(target.id).unwrap().reference(), target);

        let unrecorded_failure = case
            .assess(
                "agent:reviewer",
                target,
                "partial",
                control(unrecorded, None),
            )
            .unwrap_err();
        assert!(
            unrecorded_failure
                .error
                .message
                .contains("CONTROL_ENVIRONMENT_UNRECORDED")
        );

        // A judgment that the target is invalid may still cite the unqualified
        // control; the record shows why it could not qualify anything.
        let judged = case
            .assess("agent:reviewer", target, "invalid", control(wrong, None))
            .unwrap();
        let log = judgment_log(&case.state.current(target.id).unwrap().body).unwrap();
        let verification = &log.judgments[0].verification[0];
        assert_eq!(verification.qualification, Qualification::Unqualified);
        assert_eq!(verification.reasons, ["CONTROL_ENVIRONMENT_MISMATCH"]);
        assert_eq!(
            verification.compared_environments,
            Some(ComparedEnvironments {
                required: Some("build-41".into()),
                control: Some("build-40".into()),
                components: Vec::new(),
            })
        );

        let accepted = case
            .assess("agent:reviewer", judged, "valid", control(right, None))
            .unwrap();
        let view = case.view(accepted);
        assert_eq!(view.state, MeasurementValidity::Valid);
        assert_eq!(view.basis, ValidityBasis::Assessed);
        assert_eq!(view.judgments_kept, 2);

        // An intended baseline in another environment qualifies only when the
        // assessor names that environment and the control was recorded there.
        let baseline = case.observation("unknown", Some(treatment_run));
        assert!(
            case.assess(
                "agent:reviewer",
                baseline,
                "valid",
                control(wrong, Some("build-40"))
            )
            .is_ok()
        );
    }

    #[test]
    fn an_invalid_judgment_keeps_the_observation_readable_and_dependents_need_review() {
        let mut case = Case::new();
        let observation = case.observation("unknown", None);
        let question = case.create(
            "question.create",
            json!({"wording": "Why does the value vary?", "importance": "normal"}),
        );
        let hypothesis = case.create(
            "hypothesis.create",
            json!({
                "question_ref": question, "statement": "The setup drifts.",
                "possible_falsifiers": [], "rival_refs": [], "assumption_refs": [],
            }),
        );
        let assessment = case.create(
            "assessment.add",
            json!({
                "subject_ref": hypothesis,
                "evidence_refs": [observation],
                "relation": "inconclusive",
                "rationale": "One reading is not enough to separate the explanations.",
            }),
        );
        let before = case.state.current(observation.id).unwrap().body.clone();
        assert_eq!(
            effective_disposition(&case.state, case.state.current(assessment.id).unwrap()),
            EffectiveDisposition::Current
        );

        let invalid = case
            .assess("agent:reviewer", observation, "invalid", json!([]))
            .unwrap();
        let after = case.state.current(observation.id).unwrap();
        assert_eq!(after.body["value"], before["value"]);
        assert_eq!(after.body["basis"], before["basis"]);
        assert_eq!(after.body["status"], "active");
        assert_eq!(case.state.revision(observation).unwrap().body, before);
        assert_eq!(case.view(invalid).state, MeasurementValidity::Invalid);
        assert_eq!(
            effective_disposition(&case.state, case.state.current(assessment.id).unwrap()),
            EffectiveDisposition::NeedsReview
        );
        let cited = cited_validity(&case.state, case.state.current(assessment.id).unwrap());
        assert_eq!(cited.len(), 1);
        assert_eq!(cited[0].observation_ref, observation);
        assert_eq!(cited[0].current_revision, Some(2));
        assert_eq!(cited[0].state, MeasurementValidity::Invalid);
    }

    #[test]
    fn execution_status_and_validity_vary_independently_on_the_same_run() {
        let mut case = Case::new();
        for status in [
            "declared",
            "running",
            "completed",
            "failed",
            "cancelled",
            "blocked",
            "unknown",
        ] {
            for validity in ["valid", "invalid", "partial", "unknown"] {
                let run = case.run(status, Some("setup-a"));
                let observation = case.observation("unknown", Some(run));
                let assessed = case
                    .assess("agent:reviewer", observation, validity, equivalent())
                    .unwrap_or_else(|error| panic!("{status}/{validity}: {error:?}"));
                let view = case.view(assessed);
                assert_eq!(json!(view.state), json!(validity), "{status}/{validity}");
                let run_record = case.state.current(run.id).unwrap();
                assert_eq!(run_record.reference(), run);
                assert_eq!(run_record.body["execution_status"], status);
            }
        }
        // The Run's own recorded evidence validity is a separate claim as well.
        let mut failed = json!({
            "intent": {"kind": "exploration", "intent": "Repeat the measurement."},
            "producer": "agent:producer", "artifact_refs": [],
            "execution_status": "failed", "evidence_completeness": "partial",
            "evidence_kind": "observational", "variables_changed": [],
            "evidence_validity": "valid",
        });
        let run = case.create("run.record", failed.clone());
        assert_eq!(
            case.state.current(run.id).unwrap().body["evidence_validity"],
            "valid"
        );
        failed["execution_status"] = json!("completed");
        failed["evidence_validity"] = json!("invalid");
        let run = case.create("run.record", failed);
        assert_eq!(
            case.state.current(run.id).unwrap().body["evidence_validity"],
            "invalid"
        );
    }

    #[test]
    fn latest_judgment_per_assessor_is_current_and_none_is_removed() {
        let mut case = Case::new();
        let mut observation = case.observation("valid", None);
        assert_eq!(case.view(observation).basis, ValidityBasis::Recorded);
        observation = case
            .assess("agent:first", observation, "valid", equivalent())
            .unwrap();
        observation = case
            .assess("agent:second", observation, "invalid", json!([]))
            .unwrap();
        let view = case.view(observation);
        assert_eq!(view.state, MeasurementValidity::Invalid);
        assert!(view.disagreement);
        // The recorder's own recorded validity still counts as its judgment.
        assert_eq!(view.current.len(), 3);

        observation = case
            .assess("agent:second", observation, "valid", equivalent())
            .unwrap();
        observation = case
            .assess("agent:test", observation, "partial", equivalent())
            .unwrap();
        let view = case.view(observation);
        assert_eq!(view.judgments_kept, 4);
        assert_eq!(view.current.len(), 3);
        assert_eq!(view.state, MeasurementValidity::Partial);
        let stale = case
            .assess(
                "agent:first",
                RevisionRef {
                    revision: 1,
                    ..observation
                },
                "valid",
                equivalent(),
            )
            .unwrap_err();
        assert_eq!(stale.error.code, ErrorCode::RevisionConflict);
    }

    #[test]
    fn channel_judgments_qualify_part_of_an_observation() {
        let mut case = Case::new();
        let mut observation = case.observation("unknown", None);
        observation = case
            .assess_channel(
                "agent:reviewer",
                observation,
                Some("secondary"),
                "valid",
                equivalent(),
            )
            .unwrap();
        assert_eq!(case.view(observation).state, MeasurementValidity::Unknown);
        observation = case
            .assess("agent:reviewer", observation, "valid", equivalent())
            .unwrap();
        assert_eq!(case.view(observation).state, MeasurementValidity::Valid);
        observation = case
            .assess_channel(
                "agent:other",
                observation,
                Some("secondary"),
                "invalid",
                json!([]),
            )
            .unwrap();
        let view = case.view(observation);
        assert_eq!(view.state, MeasurementValidity::Partial);
        assert_eq!(
            view.channels,
            [ChannelValidity {
                channel: "secondary".into(),
                state: MeasurementValidity::Invalid
            }]
        );
    }

    #[test]
    fn a_changed_verification_withdraws_the_valid_contribution() {
        let mut case = Case::new();
        let run = case.run("completed", Some("setup-a"));
        let control_run = case.run("completed", Some("setup-a"));
        let target = case.observation("unknown", Some(run));
        let checked = case.observation("valid", Some(control_run));
        let target = case
            .assess("agent:reviewer", target, "valid", control(checked, None))
            .unwrap();
        assert_eq!(case.view(target).state, MeasurementValidity::Valid);
        case.assess("agent:other", checked, "invalid", json!([]))
            .unwrap();
        let view = case.view(target);
        assert_eq!(view.state, MeasurementValidity::Unknown);
        assert!(!view.current[0].verification_current);
        assert_eq!(view.current[0].validity, MeasurementValidity::Valid);
    }

    #[test]
    fn corrections_start_a_new_judgment_history_and_retractions_are_refused() {
        let mut case = Case::new();
        let observation = case.observation("unknown", None);
        let judged = case
            .assess("agent:reviewer", observation, "invalid", json!([]))
            .unwrap();
        let corrected = case.create(
            "observation.correct",
            json!({
                "observation_ref": judged,
                "revision_reason": "The unit was recorded incorrectly.",
                "value": {"kind": "measurement", "value": "12.5", "unit": "other-unit"},
                "basis": {"kind": "assertion", "assertion": "Reported by the recorder.", "context": null},
                "extraction_method": null, "validity": "unknown", "quality": [], "limitations": [],
                "evidence_kind": "observational", "variables_changed": [], "observed_at": null, "run_ref": null,
            }),
        );
        assert_eq!(case.view(corrected).judgments_kept, 0);
        assert_eq!(
            judgment_log(&case.state.revision(judged).unwrap().body)
                .unwrap()
                .judgments
                .len(),
            1
        );
        let retracted = case.create(
            "observation.retract",
            json!({"observation_ref": corrected, "revision_reason": "Out of scope."}),
        );
        let failure = case
            .assess("agent:reviewer", retracted, "invalid", json!([]))
            .unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::GateFailed);
    }

    #[test]
    fn payload_rejects_unknown_fields_and_wrong_reference_kinds() {
        let descriptor = standard_catalog();
        let descriptor = descriptor.get("measurement.assess").unwrap();
        let mut payload = descriptor.example().unwrap().clone();
        descriptor.validate_payload(&payload).unwrap();
        payload["assessor"] = json!("agent:claimed");
        assert_eq!(
            descriptor
                .validate_payload(&payload)
                .unwrap_err()
                .error
                .code,
            ErrorCode::InvalidInput
        );
        let mut payload = descriptor.example().unwrap().clone();
        payload["observation_ref"]["id"] = json!("run_019acd16-0000-7000-8000-000000000001");
        assert_eq!(
            descriptor
                .validate_payload(&payload)
                .unwrap_err()
                .error
                .path
                .as_deref(),
            Some("/observation_ref")
        );
        let mut payload = descriptor.example().unwrap().clone();
        payload["rationale"] = json!(" ");
        assert!(descriptor.validate_payload(&payload).is_err());
    }
}
