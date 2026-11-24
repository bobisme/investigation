//! Source-bound Observation operations.
//!
//! Selector and quote validation is supplied by the Store after it reads the
//! retained bytes. The operation payload has no trusted validation fields.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    catalog::{OperationDescriptor, OperationInfo},
    command::RevisionRef,
    evidence::{
        ArtifactAvailability, ArtifactBody, ArtifactIntegrity, EvidenceKind, MeasurementValidity,
        ObservationAdd, ObservationBasisRequest, ObservationBlocker, ObservationBody,
        ObservationValue, SelectorValidation,
    },
    header::SourceTime,
    identity::Kind,
    input,
    measurement_fields::MeasurementFields,
    state::{OperationContext, Record},
    wire::{Digest, ErrorCode, Failure},
};

pub(crate) const OBSERVATION_METADATA: &[&str] = &[
    "status",
    "revision_reason",
    "retraction_reason",
    "correction_reason",
    "retracted",
    "lifecycle",
    "rationale",
    "corrects",
    "measurement_assessment",
    "run_ref",
    crate::origin_ops::CLAIM_ORIGIN_KEY,
    GUARDRAIL_KEY,
];

/// Observation body key that holds the write-side guardrails a retraction
/// satisfied, when a pinned pack declares any.
pub const GUARDRAIL_KEY: &str = "guardrail";

/// Store-computed result for the exact source bytes named by an observation.
/// This value is only produced by the trusted read path, never decoded from an
/// operation payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationProof {
    pub artifact_ref: RevisionRef,
    pub digest: Digest,
    pub selector_validation: SelectorValidation,
    pub quote_validation: Option<SelectorValidation>,
}

/// Strict top-level ObservationAdd wire shape plus an optional exact Run link.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ObservationAddPayload {
    value: ObservationValue,
    basis: ObservationBasisRequest,
    extraction_method: Option<String>,
    validity: MeasurementValidity,
    quality: Vec<String>,
    limitations: Vec<String>,
    evidence_kind: Option<EvidenceKind>,
    variables_changed: Vec<String>,
    observed_at: Option<SourceTime>,
    run_ref: Option<RevisionRef>,
    /// Optional typed measurement fields. Absent means not recorded; a field
    /// with state `unknown` is recorded as explicitly unknown.
    #[serde(default)]
    measurement: Option<MeasurementFields>,
}

impl ObservationAddPayload {
    fn clone_add(&self) -> ObservationAdd {
        ObservationAdd {
            value: self.value.clone(),
            basis: self.basis.clone(),
            extraction_method: self.extraction_method.clone(),
            validity: self.validity,
            quality: self.quality.clone(),
            limitations: self.limitations.clone(),
            evidence_kind: self.evidence_kind,
            variables_changed: self.variables_changed.clone(),
            observed_at: self.observed_at.clone(),
            measurement: self.measurement.clone(),
        }
    }

    fn into_parts(self) -> (ObservationAdd, Option<RevisionRef>) {
        (
            ObservationAdd {
                value: self.value,
                basis: self.basis,
                extraction_method: self.extraction_method,
                validity: self.validity,
                quality: self.quality,
                limitations: self.limitations,
                evidence_kind: self.evidence_kind,
                variables_changed: self.variables_changed,
                observed_at: self.observed_at,
                measurement: self.measurement,
            },
            self.run_ref,
        )
    }
}

/// A correction carries the replacement ObservationAdd fields at top level.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ObservationCorrectPayload {
    observation_ref: RevisionRef,
    revision_reason: String,
    value: ObservationValue,
    basis: ObservationBasisRequest,
    extraction_method: Option<String>,
    validity: MeasurementValidity,
    quality: Vec<String>,
    limitations: Vec<String>,
    evidence_kind: Option<EvidenceKind>,
    variables_changed: Vec<String>,
    observed_at: Option<SourceTime>,
    run_ref: Option<RevisionRef>,
    /// Optional typed measurement fields. Absent means not recorded; a field
    /// with state `unknown` is recorded as explicitly unknown.
    #[serde(default)]
    measurement: Option<MeasurementFields>,
}

impl ObservationCorrectPayload {
    fn into_parts(self) -> (RevisionRef, String, ObservationAdd, Option<RevisionRef>) {
        (
            self.observation_ref,
            self.revision_reason,
            ObservationAdd {
                value: self.value,
                basis: self.basis,
                extraction_method: self.extraction_method,
                validity: self.validity,
                quality: self.quality,
                limitations: self.limitations,
                evidence_kind: self.evidence_kind,
                variables_changed: self.variables_changed,
                observed_at: self.observed_at,
                measurement: self.measurement,
            },
            self.run_ref,
        )
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ObservationRetractPayload {
    observation_ref: RevisionRef,
    revision_reason: String,
}

pub(crate) fn operations() -> [OperationDescriptor; 3] {
    [
        OperationDescriptor::active(
            info(
                "observation.add",
                "Record a bounded observation tied to a source selector or explicit assertion.",
            ),
            json!({
                "value":{"kind":"measurement","value":"0.62","unit":"fraction"},
                "basis":{"kind":"assertion","assertion":"A reviewer counted documents that cite a primary source.","context":"Synthetic demonstration only."},
                "extraction_method":"manual_count",
                "validity":"unknown",
                "quality":[],
                "limitations":["Synthetic source."],
                "evidence_kind":"observational",
                "variables_changed":[],
                "observed_at":null,
                "run_ref":null,
                "measurement":{
                    "unit":{"state":"known","value":"fraction"},
                    "workload":{"state":"known","value":"Synthetic sample of 50 documents."},
                    "sample_count":{"state":"known","value":48},
                    "trial_order":{"state":"unknown"},
                    "attempts":{
                        "started":{"count":{"state":"known","value":50},"definition":{"state":"known","value":"Documents drawn for review."}},
                        "observed":{"count":{"state":"known","value":48},"definition":{"state":"known","value":"Documents read in full."}},
                        "excluded":{"count":{"state":"known","value":2},"definition":{"state":"known","value":"Documents unavailable at review time."}}
                    },
                    "tool_identity":{"state":"known","value":{"rubric":{"state":"known","value":"citation-rubric-1"}}},
                    "limitations":["One reviewer; agreement between reviewers was not checked."]
                }
            }),
            validate_add,
            add,
        ),
        OperationDescriptor::active(
            info(
                "observation.correct",
                "Record a corrected Observation revision while preserving its prior revision.",
            ),
            json!({
                "observation_ref":{"id":"obs_019acd16-0000-7000-8000-000000000001","revision":1},
                "revision_reason":"The selector was corrected after rereading the source.",
                "value":{"kind":"text","statement":"The source reports the corrected value."},
                "basis":{"kind":"assertion","assertion":"Corrected report text.","context":null},
                "extraction_method":"direct_read",
                "validity":"unknown",
                "quality":[],
                "limitations":[],
                "evidence_kind":"observational",
                "variables_changed":[],
                "observed_at":null,
                "run_ref":null
            }),
            validate_correct,
            correct,
        ),
        OperationDescriptor::active(
            info(
                "observation.retract",
                "Retract an Observation from current evidentiary use without deleting its history.",
            ),
            json!({
                "observation_ref":{"id":"obs_019acd16-0000-7000-8000-000000000001","revision":1},
                "revision_reason":"The source was later found to be outside the declared scope."
            }),
            validate_retract,
            retract,
        ),
    ]
}

fn info(name: &'static str, summary: &'static str) -> OperationInfo {
    OperationInfo {
        name,
        stage: "observe",
        summary,
        when_to_use: summary,
        lifecycle: "An existing case; corrections and retractions target a current Observation revision.",
        effects: "Record an attributed Observation image and preserve prior revisions.",
        returns: "Observation ID and revision, plus the persisted selector and quote validation states.",
        revision_rule: "Supply the exact current observation_ref for correction or retraction.",
        idempotency: "Retry with the original actor and command ID.",
        repair: "Read the current Observation and exact source references before resubmitting.",
        related: &["run.record", "observation.correct", "observation.retract"],
    }
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
        "A stored Observation or Artifact record is invalid.",
    )
}

fn encode(value: impl serde::Serialize) -> Result<Value, Failure> {
    serde_json::to_value(value).map_err(|_| integrity())
}

fn decode<T: serde::de::DeserializeOwned>(body: &Value, metadata: &[&str]) -> Result<T, Failure> {
    let mut body = body.clone();
    let object = body.as_object_mut().ok_or_else(integrity)?;
    for key in metadata {
        object.remove(*key);
    }
    input::decode(&body).map_err(|_| integrity())
}

fn validate_add(payload: &ObservationAddPayload) -> Result<(), Failure> {
    let add = payload.clone_add();
    add.validate_shape().map_err(blocker_failure)?;
    measurement_shape(&add.value, add.measurement.as_ref())
}

fn validate_correct(payload: &ObservationCorrectPayload) -> Result<(), Failure> {
    text(&payload.revision_reason, "/revision_reason")?;
    ObservationAdd {
        value: payload.value.clone(),
        basis: payload.basis.clone(),
        extraction_method: payload.extraction_method.clone(),
        validity: payload.validity,
        quality: payload.quality.clone(),
        limitations: payload.limitations.clone(),
        evidence_kind: payload.evidence_kind,
        variables_changed: payload.variables_changed.clone(),
        observed_at: payload.observed_at.clone(),
        measurement: payload.measurement.clone(),
    }
    .validate_shape()
    .map_err(blocker_failure)?;
    measurement_shape(&payload.value, payload.measurement.as_ref())
}

fn measurement_shape(
    value: &ObservationValue,
    measurement: Option<&MeasurementFields>,
) -> Result<(), Failure> {
    measurement.map_or(Ok(()), |fields| {
        fields.validate_for_observation(value, "/measurement")
    })
}

fn validate_retract(payload: &ObservationRetractPayload) -> Result<(), Failure> {
    if payload.observation_ref.id.kind() != Kind::Observation {
        return Err(invalid(
            "/observation_ref",
            "Select an exact Observation revision.",
        ));
    }
    text(&payload.revision_reason, "/revision_reason")
}

fn text(value: &str, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() || value.chars().count() > 2048 {
        return Err(invalid(
            path,
            "Supply nonblank text no longer than 2048 characters.",
        ));
    }
    Ok(())
}

fn blocker_failure(blocker: ObservationBlocker) -> Failure {
    let path = match blocker {
        ObservationBlocker::TextNeedsQuote
        | ObservationBlocker::QuoteRequired
        | ObservationBlocker::QuoteValidationRequired
        | ObservationBlocker::QuoteNotFound
        | ObservationBlocker::PositionalSelectorRequired => "/basis/text_quote",
        ObservationBlocker::NonTextQuoteNotApplicable => "/basis/text_quote",
        ObservationBlocker::ArtifactReferenceUnresolved
        | ObservationBlocker::ArtifactReferenceMismatch
        | ObservationBlocker::ArtifactReferenceWrongKind
        | ObservationBlocker::UnexpectedArtifactReference => "/basis/artifact_ref",
        ObservationBlocker::ArtifactDigestMismatch => "/basis/selector/artifact_digest",
        ObservationBlocker::SelectorValidationRequired | ObservationBlocker::SelectorInvalid => {
            "/basis/selector"
        }
        ObservationBlocker::AssertionNeedsLabel => "/basis/assertion",
        ObservationBlocker::ObservationValueRequired => "/value",
        ObservationBlocker::MeasurementUnitRequired => "/value/unit",
        ObservationBlocker::InterventionNeedsChangedVariable => "/variables_changed",
    };
    invalid(path, blocker.code())
}

fn add(payload: ObservationAddPayload, context: &mut OperationContext<'_>) -> Result<(), Failure> {
    let (add, run_ref) = payload.into_parts();
    let run_ref = resolve_run(context, run_ref)?;
    let body = accepted_body(add, context)?;
    let body = with_metadata(body, Some("active"), run_ref, None, None)?;
    let created = context.create(Kind::Observation, body)?;
    crate::measurement_flags::warn_recorded(context, created);
    Ok(())
}

fn correct(
    payload: ObservationCorrectPayload,
    context: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let (reference, revision_reason, add, supplied_run_ref) = payload.into_parts();
    let old_record = current_observation(context, reference)?;
    if old_record.body.get("status").and_then(Value::as_str) == Some("retracted") {
        return Err(gate(
            "/observation_ref",
            "A retracted Observation cannot be corrected; add a new Observation instead.",
        ));
    }
    let old_body: ObservationBody = decode(&old_record.body, OBSERVATION_METADATA)?;
    let old_run_ref = optional_run_ref(&old_record.body)?;
    let run_ref = resolve_run(context, supplied_run_ref.or(old_run_ref))?;
    let body = accepted_body(add, context)?;
    let body = with_metadata(
        body,
        Some("active"),
        run_ref,
        Some(revision_reason),
        Some(reference),
    )?;
    // Decode the previous body before update so malformed history is never
    // replaced by a fresh valid image.
    let _ = old_body;
    let corrected = context.update(reference, body)?;
    crate::measurement_flags::warn_recorded(context, corrected);
    Ok(())
}

fn retract(
    payload: ObservationRetractPayload,
    context: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let record = current_observation(context, payload.observation_ref)?;
    if record.body.get("status").and_then(Value::as_str) == Some("retracted") {
        return Err(gate(
            "/observation_ref",
            "This Observation is already retracted.",
        ));
    }
    let record = record.clone();
    let mut body = mark_retracted(&record.body, payload.revision_reason)?;
    if let Some(guardrail) = crate::guardrails::enforce(
        context,
        crate::guardrails::GuardedOperation::ObservationRetract,
        &record,
    )? {
        body[GUARDRAIL_KEY] = crate::guardrails::encode(&guardrail)?;
    }
    context.update(payload.observation_ref, body)?;
    Ok(())
}

fn current_observation<'a>(
    context: &'a OperationContext<'_>,
    reference: RevisionRef,
) -> Result<&'a Record, Failure> {
    if reference.id.kind() != Kind::Observation {
        return Err(invalid(
            "/observation_ref",
            "Select an exact Observation revision.",
        ));
    }
    let record = context.current(reference.id).ok_or_else(|| {
        Failure::new(
            ErrorCode::UnknownReference,
            "The Observation is absent from this case.",
        )
        .at("/observation_ref")
    })?;
    if record.reference() != reference {
        let mut failure = Failure::new(
            ErrorCode::RevisionConflict,
            "The Observation changed; read the current revision before retrying.",
        )
        .at("/observation_ref");
        failure.error.expected = Some(json!(reference.revision));
        failure.error.actual = Some(json!(record.reference().revision));
        failure.error.retry = Some("read current revision and reassess".into());
        return Err(failure);
    }
    Ok(record)
}

fn optional_run_ref(body: &Value) -> Result<Option<RevisionRef>, Failure> {
    match body.get("run_ref") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => input::decode(value).map(Some).map_err(|_| integrity()),
    }
}

fn resolve_run(
    context: &OperationContext<'_>,
    run_ref: Option<RevisionRef>,
) -> Result<Option<RevisionRef>, Failure> {
    let Some(reference) = run_ref else {
        return Ok(None);
    };
    if reference.id.kind() != Kind::Run {
        return Err(invalid(
            "/run_ref",
            "run_ref must identify an exact Run revision.",
        ));
    }
    context
        .exact(reference)
        .map(|_| Some(reference))
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::UnknownReference,
                "run_ref is not an exact Run revision in this case.",
            )
            .at("/run_ref")
        })
}

fn accepted_body(
    add: ObservationAdd,
    context: &OperationContext<'_>,
) -> Result<ObservationBody, Failure> {
    add.validate_shape().map_err(blocker_failure)?;
    measurement_shape(&add.value, add.measurement.as_ref())?;
    let (artifact_ref, digest, selector_validation, quote_validation) = match &add.basis {
        ObservationBasisRequest::Assertion { .. } => {
            if context.observation_proof()?.is_some() {
                return Err(Failure::new(
                    ErrorCode::IntegrityFailure,
                    "A source proof was supplied for an assertion basis.",
                ));
            }
            (None, None, None, None)
        }
        ObservationBasisRequest::Source {
            artifact_ref,
            selector,
            text_quote,
            ..
        } => {
            let resolved = context.resolve(artifact_ref, Kind::Artifact, "/basis/artifact_ref")?;
            let artifact_record = context.current(resolved.id).ok_or_else(|| {
                Failure::new(
                    ErrorCode::UnknownReference,
                    "The Artifact is absent from this case.",
                )
                .at("/basis/artifact_ref")
            })?;
            if artifact_record.reference() != resolved {
                let mut failure = Failure::new(
                    ErrorCode::RevisionConflict,
                    "Use the current exact Artifact revision.",
                )
                .at("/basis/artifact_ref");
                failure.error.expected = Some(json!(resolved.revision));
                failure.error.actual = Some(json!(artifact_record.reference().revision));
                return Err(failure);
            }
            if let Some(withdrawn) =
                crate::source_ops::withdrawn_lineage(context.state(), artifact_record)
            {
                return Err(Failure::new(
                    ErrorCode::GateFailed,
                    format!(
                        "{} is withdrawn from evidentiary use and cannot ground a new Observation. Withdrawal is not erasure; its bytes stay readable.",
                        withdrawn.id
                    ),
                )
                .at("/basis/artifact_ref"));
            }
            let artifact =
                ArtifactBody::from_stored(&artifact_record.body).map_err(|_| integrity())?;
            if artifact.source.id.kind() != Kind::Source || context.exact(artifact.source).is_none()
            {
                return Err(Failure::new(
                    ErrorCode::IntegrityFailure,
                    "Artifact source lineage is unavailable or malformed.",
                )
                .at("/basis/artifact_ref"));
            }
            if artifact
                .digest
                .as_ref()
                .is_some_and(|digest| digest != &selector.artifact_digest)
            {
                return Err(blocker_failure(ObservationBlocker::ArtifactDigestMismatch));
            }
            let proof = context.observation_proof()?;
            let (selector_state, quote_state) = match proof {
                Some(proof) => {
                    if (proof.selector_validation != SelectorValidation::Unavailable
                        || proof
                            .quote_validation
                            .is_some_and(|v| v != SelectorValidation::Unavailable))
                        && (artifact.availability != ArtifactAvailability::Retained
                            || artifact.integrity == ArtifactIntegrity::Mismatch
                            || artifact.integrity == ArtifactIntegrity::Unavailable)
                    {
                        return Err(Failure::new(
                            ErrorCode::IntegrityFailure,
                            "Store proof is inconsistent with Artifact availability or integrity.",
                        ));
                    }
                    verify_proof(proof, resolved, &selector.artifact_digest)?;
                    (
                        proof.selector_validation,
                        text_quote.as_ref().map(|_| {
                            proof
                                .quote_validation
                                .unwrap_or_else(|| unavailable_or_unverified(&artifact))
                        }),
                    )
                }
                None => {
                    let fallback = unavailable_or_unverified(&artifact);
                    (fallback, text_quote.as_ref().map(|_| fallback))
                }
            };
            if let Some(quote) = quote_state
                && quote == SelectorValidation::NotFound
            {
                return Err(blocker_failure(ObservationBlocker::QuoteNotFound));
            }
            // The source reference is normalized to the exact Store-owned
            // Artifact revision before the typed body is accepted.
            (
                Some(resolved),
                Some(selector.artifact_digest.clone()),
                Some(selector_state),
                quote_state,
            )
        }
    };
    add.into_body(artifact_ref, digest, selector_validation, quote_validation)
        .map_err(blocker_failure)
}

fn unavailable_or_unverified(artifact: &ArtifactBody) -> SelectorValidation {
    match (artifact.availability, artifact.integrity) {
        (
            ArtifactAvailability::Retained,
            ArtifactIntegrity::Verified | ArtifactIntegrity::NotChecked,
        ) => SelectorValidation::Unverified,
        _ => SelectorValidation::Unavailable,
    }
}

fn mark_retracted(body: &Value, reason: String) -> Result<Value, Failure> {
    let mut body = body.clone();
    let object = body.as_object_mut().ok_or_else(integrity)?;
    object.insert("status".into(), json!("retracted"));
    object.insert("revision_reason".into(), json!(reason));
    Ok(body)
}

fn verify_proof(
    proof: &ObservationProof,
    artifact_ref: RevisionRef,
    digest: &Digest,
) -> Result<(), Failure> {
    if proof.artifact_ref != artifact_ref || &proof.digest != digest {
        return Err(Failure::new(
            ErrorCode::IntegrityFailure,
            "Store observation proof does not match the exact Artifact revision and digest.",
        ));
    }
    Ok(())
}

fn with_metadata(
    body: ObservationBody,
    status: Option<&str>,
    run_ref: Option<RevisionRef>,
    revision_reason: Option<String>,
    corrects: Option<RevisionRef>,
) -> Result<Value, Failure> {
    let mut value = encode(body)?;
    let object = value.as_object_mut().ok_or_else(integrity)?;
    if let Some(status) = status {
        object.insert("status".into(), json!(status));
    }
    if let Some(run_ref) = run_ref {
        object.insert("run_ref".into(), json!(run_ref));
    }
    if let Some(reason) = revision_reason {
        object.insert("revision_reason".into(), json!(reason));
    }
    if let Some(corrects) = corrects {
        object.insert("corrects".into(), json!(corrects));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        command::Reference,
        evidence::{ObservationBasis, SelectorRequest},
        identity::Id,
    };

    fn id(prefix: &str) -> Id {
        format!("{prefix}_019acd16-0000-7000-8000-000000000001")
            .parse()
            .unwrap()
    }

    fn source_text_request() -> ObservationAdd {
        ObservationAdd {
            value: ObservationValue::Text {
                statement: "A source reports a wait.".into(),
            },
            basis: ObservationBasisRequest::Source {
                artifact_ref: Reference::Existing(RevisionRef {
                    id: id("art"),
                    revision: 1,
                }),
                selector: Box::new(SelectorRequest {
                    schema_version: crate::evidence::SelectorVersion::V1,
                    artifact_digest: format!("sha256:{}", "a".repeat(64)).parse().unwrap(),
                    selector: crate::evidence::SelectorKind::TextLines {
                        start_line: 1,
                        end_line: 1,
                        encoding: "utf-8".into(),
                        parser_version: "plain.v1".into(),
                    },
                }),
                text_quote: Some(crate::evidence::TextQuoteRequest {
                    exact: "A source reports a wait.".into(),
                    prefix: None,
                    suffix: None,
                }),
            },
            extraction_method: Some("direct_read".into()),
            validity: MeasurementValidity::Unknown,
            quality: vec![],
            limitations: vec![],
            evidence_kind: Some(EvidenceKind::Observational),
            variables_changed: vec![],
            observed_at: None,
            measurement: None,
        }
    }

    #[test]
    fn add_wire_keeps_observation_fields_top_level_and_rejects_claimed_validation() {
        let add = source_text_request();
        let mut value = encode(add).unwrap();
        value["run_ref"] = Value::Null;
        assert!(input::decode::<ObservationAddPayload>(&value).is_ok());
        value["selector_validation"] = json!("verified");
        assert!(input::decode::<ObservationAddPayload>(&value).is_err());
    }

    #[test]
    fn exact_proof_must_match_artifact_revision_and_digest() {
        let expected = RevisionRef {
            id: id("art"),
            revision: 1,
        };
        let digest: Digest = format!("sha256:{}", "a".repeat(64)).parse().unwrap();
        let proof = ObservationProof {
            artifact_ref: expected,
            digest: digest.clone(),
            selector_validation: SelectorValidation::Verified,
            quote_validation: Some(SelectorValidation::Ambiguous),
        };
        assert!(verify_proof(&proof, expected, &digest).is_ok());
        assert!(
            verify_proof(
                &proof,
                RevisionRef {
                    revision: 2,
                    ..expected
                },
                &digest
            )
            .is_err()
        );
        let other: Digest = format!("sha256:{}", "b".repeat(64)).parse().unwrap();
        assert!(verify_proof(&proof, expected, &other).is_err());
    }

    #[test]
    fn absent_byte_proof_never_claims_verification() {
        let mut artifact: ArtifactBody = serde_json::from_value(json!({
            "digest":null,"size_bytes":null,
            "source":{"id":"src_019acd16-0000-7000-8000-000000000001","revision":1},
            "transforms":[],"availability":"retained","integrity":"not_checked","sensitivity":"internal"
        })).unwrap();
        assert_eq!(
            unavailable_or_unverified(&artifact),
            SelectorValidation::Unverified
        );
        artifact.availability = ArtifactAvailability::Missing;
        assert_eq!(
            unavailable_or_unverified(&artifact),
            SelectorValidation::Unavailable
        );
    }

    #[test]
    fn retraction_keeps_original_source_and_value_fields() {
        let prior = json!({"value":{"kind":"text","statement":"observed"},"basis":{"kind":"assertion","assertion":"operator","context":null},"status":"active","run_ref":null});
        let retracted = mark_retracted(&prior, "scope error".into()).unwrap();
        assert_eq!(retracted["value"], prior["value"]);
        assert_eq!(retracted["basis"], prior["basis"]);
        assert_eq!(retracted["status"], "retracted");
        assert_eq!(retracted["revision_reason"], "scope error");
    }

    #[test]
    fn text_quote_not_found_is_rejected_and_ambiguous_is_preserved() {
        let add = source_text_request();
        let accepted = add
            .clone()
            .into_body(
                Some(RevisionRef {
                    id: id("art"),
                    revision: 1,
                }),
                Some(format!("sha256:{}", "a".repeat(64)).parse().unwrap()),
                Some(SelectorValidation::Verified),
                Some(SelectorValidation::Ambiguous),
            )
            .unwrap();
        let ObservationBasis::Source { text_quote, .. } = accepted.basis else {
            panic!("source basis expected")
        };
        assert_eq!(text_quote.state, crate::evidence::TextQuoteState::Ambiguous);
        assert_eq!(
            add.into_body(
                Some(RevisionRef {
                    id: id("art"),
                    revision: 1
                }),
                Some(format!("sha256:{}", "a".repeat(64)).parse().unwrap()),
                Some(SelectorValidation::Verified),
                Some(SelectorValidation::NotFound),
            )
            .unwrap_err(),
            ObservationBlocker::QuoteNotFound
        );
    }

    #[test]
    fn assertion_body_does_not_create_source_support_metadata() {
        let add = ObservationAdd {
            value: ObservationValue::Text {
                statement: "An operator reported a delay.".into(),
            },
            basis: ObservationBasisRequest::Assertion {
                assertion: "Operator report".into(),
                context: None,
            },
            extraction_method: None,
            validity: MeasurementValidity::Unknown,
            quality: vec![],
            limitations: vec!["Not source verified.".into()],
            evidence_kind: None,
            variables_changed: vec![],
            observed_at: None,
            measurement: None,
        };
        let value = encode(add.into_body(None, None, None, None).unwrap()).unwrap();
        assert!(value.get("support").is_none());
        assert_eq!(value["basis"]["kind"], "assertion");
    }

    #[test]
    fn stored_body_fields_are_exactly_the_typed_body_properties() {
        let schema = serde_json::to_value(schemars::schema_for!(ObservationBody)).unwrap();
        let properties: std::collections::BTreeSet<&str> = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let fields: std::collections::BTreeSet<&str> =
            ObservationBody::FIELDS.iter().copied().collect();
        assert_eq!(fields, properties);
        for key in OBSERVATION_METADATA {
            assert!(
                !fields.contains(key),
                "{key} is metadata, not a typed field"
            );
        }
    }

    #[test]
    fn a_stored_body_reads_whatever_keys_other_operations_keep_beside_it() {
        let typed = json!({
            "value":{"kind":"text","statement":"observed"},
            "basis":{"kind":"assertion","assertion":"operator","context":null},
            "extraction_method":null,"validity":"unknown","quality":[],"limitations":[],
            "evidence_kind":null,"variables_changed":[],"observed_at":null
        });
        let expected: ObservationBody = serde_json::from_value(typed.clone()).unwrap();
        let mut stored = typed;
        for key in OBSERVATION_METADATA {
            stored[*key] = json!({"kept":"beside the typed body"});
        }
        stored["a_key_a_later_v1_operation_adds"] = json!(true);
        assert!(input::decode::<ObservationBody>(&stored).is_err());
        assert_eq!(ObservationBody::from_stored(&stored).unwrap(), expected);
        assert!(ObservationBody::from_stored(&json!("not an object")).is_err());
    }
}
