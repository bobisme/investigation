//! Source, artifact, observation and selector payloads. Source/artifact records
//! are produced by ingest; callers cannot create them with a standalone write.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::BTreeMap};

use crate::{
    command::{Reference, RevisionRef},
    header::SourceTime,
    identity::{Id, Kind},
    input::DecimalString,
    wire::Digest,
};

/// The selector schema is versioned independently from record and command schemas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SelectorVersion {
    #[serde(rename = "inv.selector.v1")]
    V1,
}

/// The actual discriminated union. `EvidenceSelector` binds this to a digest
/// and a core-computed validation result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SelectorKind {
    Whole,
    ByteRange {
        start: u64,
        end: u64,
    },
    TextLines {
        start_line: u64,
        end_line: u64,
        encoding: String,
        parser_version: String,
    },
    TextQuote {
        exact: String,
        prefix: Option<String>,
        suffix: Option<String>,
    },
    JsonPointer {
        pointer: String,
        parser: Option<String>,
    },
    TableCell {
        table: String,
        row_identity: Option<String>,
        row_index: Option<u64>,
        column: String,
        extraction_version: String,
    },
    TimeWindow {
        start: DecimalString,
        end: DecimalString,
        unit: String,
        coordinate_origin: String,
        channel: String,
    },
    CodeSpan {
        repository: String,
        commit: String,
        blob_digest: Digest,
        path: String,
        start_line: u64,
        end_line: u64,
    },
    ExternalSelector {
        namespace: String,
        schema_uri: String,
        schema_version: String,
        #[schemars(with = "serde_json::Value")]
        value: serde_json::Value,
        validator: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SelectorValidation {
    Unverified,
    Verified,
    Ambiguous,
    NotFound,
    Unavailable,
}

/// Accepted, persisted selector. Validation is computed by a trusted core
/// reader and must never be copied from an ingest/observation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSelector {
    pub schema_version: SelectorVersion,
    pub artifact_digest: Digest,
    pub selector: SelectorKind,
    pub validation: SelectorValidation,
}

/// Request form deliberately lacks a validation field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectorRequest {
    pub schema_version: SelectorVersion,
    pub artifact_digest: Digest,
    pub selector: SelectorKind,
}

impl SelectorRequest {
    /// Engine-only conversion after selector verification has actually run.
    pub(crate) fn into_accepted(self, validation: SelectorValidation) -> EvidenceSelector {
        EvidenceSelector {
            schema_version: self.schema_version,
            artifact_digest: self.artifact_digest,
            selector: self.selector,
            validation,
        }
    }
}

impl SelectorKind {
    /// Structural checks that do not require opening artifact bytes.
    pub fn validate_shape(&self) -> Result<(), SelectorBlocker> {
        match self {
            Self::ByteRange { start, end } if start >= end => {
                Err(SelectorBlocker::EmptyOrReversedRange)
            }
            Self::TextLines {
                start_line,
                end_line,
                ..
            } if *start_line == 0 || start_line > end_line => {
                Err(SelectorBlocker::InvalidLineRange)
            }
            Self::TextQuote { exact, .. } if exact.is_empty() => Err(SelectorBlocker::EmptyQuote),
            Self::JsonPointer { pointer, .. }
                if !pointer.is_empty() && !pointer.starts_with('/') =>
            {
                Err(SelectorBlocker::InvalidJsonPointer)
            }
            Self::TableCell {
                row_identity,
                row_index,
                ..
            } if row_identity.is_none() && row_index.is_none() => {
                Err(SelectorBlocker::TableRowRequired)
            }
            Self::TimeWindow { start, end, .. } if start.numeric_cmp(end) != Ordering::Less => {
                Err(SelectorBlocker::InvalidTimeWindow)
            }
            Self::CodeSpan {
                start_line,
                end_line,
                ..
            } if *start_line == 0 || start_line > end_line => {
                Err(SelectorBlocker::InvalidLineRange)
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorBlocker {
    EmptyOrReversedRange,
    InvalidLineRange,
    EmptyQuote,
    InvalidJsonPointer,
    TableRowRequired,
    InvalidTimeWindow,
}

impl SelectorBlocker {
    pub const fn code(self) -> &'static str {
        match self {
            Self::EmptyOrReversedRange => "SELECTOR_RANGE_INVALID",
            Self::InvalidLineRange => "SELECTOR_LINE_RANGE_INVALID",
            Self::EmptyQuote => "SELECTOR_QUOTE_EMPTY",
            Self::InvalidJsonPointer => "SELECTOR_JSON_POINTER_INVALID",
            Self::TableRowRequired => "SELECTOR_TABLE_ROW_REQUIRED",
            Self::InvalidTimeWindow => "SELECTOR_TIME_WINDOW_INVALID",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionChannel {
    LocalFile,
    Producer,
    ExternalReference,
    Manual,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceAcquisition {
    pub channel: AcquisitionChannel,
    pub acquired_at: Option<SourceTime>,
    pub method: Option<String>,
    pub context: Option<String>,
    pub environment: Option<String>,
}

/// Ingest-produced body; attribution/provenance header is attached by the engine.
/// This retains source claims and limits, never a caller assertion of "verified".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceBody {
    pub producer: Option<String>,
    pub publisher: Option<String>,
    pub identity: Option<String>,
    pub acquisition: SourceAcquisition,
    pub access: SourceAccess,
    pub verification_limits: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
    /// Producer-supplied identity of the logical acquisition this Source records.
    /// Absent when the producer supplied none: each such ingest is its own acquisition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquisition_key: Option<AcquisitionKey>,
    /// Digest of the manifest description that produced this revision. A new
    /// description of a known acquisition revises the Source; it is not a new trial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_digest: Option<Digest>,
    /// `withdrawn` while an entire withdrawal is in effect (`source.withdraw`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<crate::source_ops::EvidentiaryStatus>,
    /// Append-only attributed withdrawals. Withdrawal is not erasure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub withdrawals: Vec<crate::source_ops::Withdrawal>,
    /// Append-only attributed judgments recorded by `source.assess`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_assessment: Option<crate::source_ops::SourceAssessmentLog>,
}

/// Producer namespace plus stable external ID identify one logical acquisition
/// attempt; `item` names one retained input within it. These are producer claims,
/// not verified identity, and they never merge distinct keys that share bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionKey {
    pub namespace: String,
    pub external_id: String,
    pub item: String,
}

impl AcquisitionKey {
    /// Whether this key belongs to the same producer attempt, whatever the item.
    pub fn same_attempt(&self, namespace: &str, external_id: &str) -> bool {
        self.namespace == namespace && self.external_id == external_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceAccess {
    Public,
    LocalOnly,
    Restricted,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAvailability {
    Retained,
    External,
    Partial,
    Missing,
    IntentionallyRemoved,
}

/// Integrity describes bytes; availability describes whether/how bytes can be
/// retrieved. This is computed during ingest or later verification, not input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactIntegrity {
    NotChecked,
    Verified,
    Mismatch,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactSensitivity {
    Public,
    Internal,
    Restricted,
}

/// One direct input of the transformation that produced a derivative. A
/// derivative with several inputs (an aggregation, a plot of two runs) lists one
/// entry per input with the same transformation and version. A chain of steps
/// is a derivative of a derivative: each step is its own Artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactTransform {
    pub input: RevisionRef,
    #[schemars(length(min = 1, max = 256))]
    pub transformation: String,
    #[schemars(length(min = 1, max = 128))]
    pub version: String,
    /// Recorded digest of the input bytes the transformation read. Resolved by
    /// the core from the input record; absent on early text derivatives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_digest: Option<Digest>,
}

/// What an Artifact retains. Originals are the bytes of an acquisition; a
/// derivative is the output of a transformation of other Artifacts and never
/// a new acquisition; a manifest retains the import request itself.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRole {
    Original,
    Derivative,
    Manifest,
}

impl ArtifactRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Derivative => "derivative",
            Self::Manifest => "manifest",
        }
    }
}

/// Ingest-produced accepted body, not a standalone ArtifactCreate payload.
/// Digest, size and integrity are derived from the captured bytes or explicit
/// external-reference state by ingest; callers cannot claim them as verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactBody {
    pub digest: Option<Digest>,
    pub size_bytes: Option<u64>,
    pub format: Option<String>,
    pub source: RevisionRef,
    pub transforms: Vec<ArtifactTransform>,
    pub availability: ArtifactAvailability,
    pub integrity: ArtifactIntegrity,
    pub sensitivity: ArtifactSensitivity,
    /// `withdrawn` while an entire withdrawal is in effect (`artifact.withdraw`).
    /// Withdrawal never changes `availability`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<crate::source_ops::EvidentiaryStatus>,
    /// Append-only attributed withdrawals. Withdrawal is not erasure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub withdrawals: Vec<crate::source_ops::Withdrawal>,
    /// Explicit role. Records written before roles were recorded omit it; see
    /// [`ArtifactBody::role`] for how their role is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ArtifactRole>,
    /// How a derivative was produced, beyond its inputs in `transforms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation: Option<crate::derivation::DerivationRecord>,
}

impl ArtifactBody {
    /// The recorded role, or for records without one: a derivative when it
    /// has transformation inputs, otherwise an original. Such records cannot
    /// mark a manifest; the ingest journal names those (`manifest` client ref).
    pub fn role(&self) -> ArtifactRole {
        self.role.unwrap_or(if self.transforms.is_empty() {
            ArtifactRole::Original
        } else {
            ArtifactRole::Derivative
        })
    }

    pub fn validate_shape(&self) -> Result<(), ArtifactBlocker> {
        let derivative = self.role() == ArtifactRole::Derivative;
        if derivative == self.transforms.is_empty() {
            return Err(ArtifactBlocker::RoleDisagreesWithTransforms);
        }
        if !derivative && self.derivation.is_some() {
            return Err(ArtifactBlocker::RoleDisagreesWithTransforms);
        }
        match self.availability {
            ArtifactAvailability::Retained
                if self.digest.is_none() || self.size_bytes.is_none() =>
            {
                Err(ArtifactBlocker::RetainedBytesNeedDigestAndSize)
            }
            ArtifactAvailability::Retained
            | ArtifactAvailability::External
            | ArtifactAvailability::Partial
            | ArtifactAvailability::Missing
            | ArtifactAvailability::IntentionallyRemoved => Ok(()),
        }
    }

    pub fn validate_references(&self) -> Result<(), ArtifactBlocker> {
        if self.source.id.expect_kind(Kind::Source).is_err() {
            return Err(ArtifactBlocker::SourceReferenceWrongKind);
        }
        if self
            .transforms
            .iter()
            .any(|transform| transform.input.id.expect_kind(Kind::Artifact).is_err())
        {
            return Err(ArtifactBlocker::TransformReferenceWrongKind);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactBlocker {
    RetainedBytesNeedDigestAndSize,
    SourceReferenceWrongKind,
    TransformReferenceWrongKind,
    RoleDisagreesWithTransforms,
}

impl ArtifactBlocker {
    pub const fn code(self) -> &'static str {
        match self {
            Self::RetainedBytesNeedDigestAndSize => "ARTIFACT_RETAINED_BYTES_INCOMPLETE",
            Self::SourceReferenceWrongKind => "ARTIFACT_SOURCE_WRONG_KIND",
            Self::TransformReferenceWrongKind => "ARTIFACT_TRANSFORM_WRONG_KIND",
            Self::RoleDisagreesWithTransforms => "ARTIFACT_ROLE_TRANSFORMS_MISMATCH",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementValidity {
    Unknown,
    Valid,
    Invalid,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Observational,
    Interventional,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationValue {
    Text {
        statement: String,
    },
    Measurement {
        value: DecimalString,
        unit: String,
    },
    Binary {
        digest: Digest,
        description: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextQuoteRequest {
    #[schemars(length(min = 1))]
    pub exact: String,
    pub prefix: Option<String>,
    pub suffix: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextQuoteState {
    Unverified,
    Verified,
    Ambiguous,
    NotFound,
    Unavailable,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextQuote {
    pub exact: Option<String>,
    pub prefix: Option<String>,
    pub suffix: Option<String>,
    pub state: TextQuoteState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationBasisRequest {
    Source {
        /// Distinguishes acquisitions even when their artifact bytes deduplicate.
        artifact_ref: Reference,
        selector: Box<SelectorRequest>,
        text_quote: Option<TextQuoteRequest>,
    },
    /// Explicitly attributed assertion, not a source-backed observation.
    Assertion {
        assertion: String,
        context: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationBasis {
    Source {
        /// Exact source acquisition revision, resolved from the command reference.
        artifact_ref: RevisionRef,
        selector: Box<EvidenceSelector>,
        text_quote: TextQuote,
    },
    Assertion {
        assertion: String,
        context: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservationAdd {
    pub value: ObservationValue,
    pub basis: ObservationBasisRequest,
    pub extraction_method: Option<String>,
    pub validity: MeasurementValidity,
    pub quality: Vec<String>,
    pub limitations: Vec<String>,
    pub evidence_kind: Option<EvidenceKind>,
    pub variables_changed: Vec<String>,
    pub observed_at: Option<SourceTime>,
    /// Optional typed measurement fields; see `measurement_fields`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<crate::measurement_fields::MeasurementFields>,
}

/// Persisted observation domain fields. `basis` stores the core-computed
/// selector/quote state; author attribution is supplied by RecordHeader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservationBody {
    pub value: ObservationValue,
    pub basis: ObservationBasis,
    pub extraction_method: Option<String>,
    pub validity: MeasurementValidity,
    pub quality: Vec<String>,
    pub limitations: Vec<String>,
    pub evidence_kind: Option<EvidenceKind>,
    pub variables_changed: Vec<String>,
    pub observed_at: Option<SourceTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<crate::measurement_fields::MeasurementFields>,
}

impl ObservationAdd {
    /// Validate invariants that depend only on the typed request. Byte checks
    /// and quote verification are performed by trusted readers after this.
    pub fn validate_shape(&self) -> Result<(), ObservationBlocker> {
        match (&self.value, &self.basis) {
            (
                ObservationValue::Text { .. },
                ObservationBasisRequest::Source {
                    artifact_ref,
                    selector,
                    text_quote: Some(quote),
                    ..
                },
            ) => {
                validate_artifact_reference(artifact_ref)?;
                selector
                    .selector
                    .validate_shape()
                    .map_err(|_| ObservationBlocker::SelectorInvalid)?;
                if quote.exact.trim().is_empty() {
                    return Err(ObservationBlocker::QuoteRequired);
                }
                if matches!(selector.selector, SelectorKind::TextQuote { .. }) {
                    return Err(ObservationBlocker::PositionalSelectorRequired);
                }
                self.validate_value_and_intervention()
            }
            (
                ObservationValue::Text { .. },
                ObservationBasisRequest::Source {
                    text_quote: None, ..
                },
            ) => Err(ObservationBlocker::TextNeedsQuote),
            (
                _,
                ObservationBasisRequest::Source {
                    artifact_ref,
                    selector,
                    ..
                },
            ) => {
                validate_artifact_reference(artifact_ref)?;
                selector
                    .selector
                    .validate_shape()
                    .map_err(|_| ObservationBlocker::SelectorInvalid)?;
                self.validate_value_and_intervention()
            }
            (_, ObservationBasisRequest::Assertion { assertion, .. })
                if assertion.trim().is_empty() =>
            {
                Err(ObservationBlocker::AssertionNeedsLabel)
            }
            _ => self.validate_value_and_intervention(),
        }
    }

    fn validate_value_and_intervention(&self) -> Result<(), ObservationBlocker> {
        if matches!(&self.value, ObservationValue::Text { statement } if statement.trim().is_empty())
        {
            return Err(ObservationBlocker::ObservationValueRequired);
        }
        if matches!(&self.value, ObservationValue::Measurement { unit, .. } if unit.trim().is_empty())
        {
            return Err(ObservationBlocker::MeasurementUnitRequired);
        }
        if self.evidence_kind == Some(EvidenceKind::Interventional)
            && self.variables_changed.is_empty()
        {
            return Err(ObservationBlocker::InterventionNeedsChangedVariable);
        }
        if matches!(
            (&self.value, &self.basis),
            (
                ObservationValue::Measurement { .. } | ObservationValue::Binary { .. },
                ObservationBasisRequest::Source {
                    text_quote: Some(_),
                    ..
                }
            )
        ) {
            return Err(ObservationBlocker::NonTextQuoteNotApplicable);
        }
        Ok(())
    }

    /// Build the accepted body after the trusted store reader has checked the
    /// exact bytes. Never derive status arguments from caller-supplied claims.
    pub fn into_body(
        self,
        resolved_artifact_ref: Option<RevisionRef>,
        resolved_artifact_digest: Option<Digest>,
        selector_validation: Option<SelectorValidation>,
        quote_validation: Option<SelectorValidation>,
    ) -> Result<ObservationBody, ObservationBlocker> {
        self.validate_shape()?;
        let basis = match self.basis {
            ObservationBasisRequest::Assertion { assertion, context } => {
                if resolved_artifact_ref.is_some() || resolved_artifact_digest.is_some() {
                    return Err(ObservationBlocker::UnexpectedArtifactReference);
                }
                ObservationBasis::Assertion { assertion, context }
            }
            ObservationBasisRequest::Source {
                artifact_ref,
                selector,
                text_quote,
            } => {
                let resolved_artifact_ref =
                    resolved_artifact_ref.ok_or(ObservationBlocker::ArtifactReferenceUnresolved)?;
                if let Reference::Existing(expected) = artifact_ref
                    && expected != resolved_artifact_ref
                {
                    return Err(ObservationBlocker::ArtifactReferenceMismatch);
                }
                if resolved_artifact_ref
                    .id
                    .expect_kind(Kind::Artifact)
                    .is_err()
                {
                    return Err(ObservationBlocker::ArtifactReferenceWrongKind);
                }
                if resolved_artifact_digest.as_ref() != Some(&selector.artifact_digest) {
                    return Err(ObservationBlocker::ArtifactDigestMismatch);
                }
                let validation =
                    selector_validation.ok_or(ObservationBlocker::SelectorValidationRequired)?;
                let selector = (*selector).into_accepted(validation);
                let text_quote = match text_quote {
                    Some(quote) => {
                        let state =
                            quote_validation.ok_or(ObservationBlocker::QuoteValidationRequired)?;
                        if state == SelectorValidation::NotFound {
                            return Err(ObservationBlocker::QuoteNotFound);
                        }
                        TextQuote {
                            exact: Some(quote.exact),
                            prefix: quote.prefix,
                            suffix: quote.suffix,
                            state: state.into(),
                        }
                    }
                    None => TextQuote {
                        exact: None,
                        prefix: None,
                        suffix: None,
                        state: TextQuoteState::NotApplicable,
                    },
                };
                ObservationBasis::Source {
                    artifact_ref: resolved_artifact_ref,
                    selector: Box::new(selector),
                    text_quote,
                }
            }
        };
        Ok(ObservationBody {
            value: self.value,
            basis,
            extraction_method: self.extraction_method,
            validity: self.validity,
            quality: self.quality,
            limitations: self.limitations,
            evidence_kind: self.evidence_kind,
            variables_changed: self.variables_changed,
            observed_at: self.observed_at,
            measurement: self.measurement,
        })
    }
}

fn validate_artifact_reference(reference: &Reference) -> Result<(), ObservationBlocker> {
    if let Reference::Existing(reference) = reference
        && reference.id.expect_kind(Kind::Artifact).is_err()
    {
        return Err(ObservationBlocker::ArtifactReferenceWrongKind);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationBlocker {
    TextNeedsQuote,
    NonTextQuoteNotApplicable,
    AssertionNeedsLabel,
    ObservationValueRequired,
    MeasurementUnitRequired,
    InterventionNeedsChangedVariable,
    SelectorInvalid,
    QuoteRequired,
    SelectorValidationRequired,
    QuoteValidationRequired,
    QuoteNotFound,
    PositionalSelectorRequired,
    ArtifactReferenceUnresolved,
    ArtifactReferenceMismatch,
    ArtifactReferenceWrongKind,
    ArtifactDigestMismatch,
    UnexpectedArtifactReference,
}

impl ObservationBlocker {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TextNeedsQuote => "OBSERVATION_TEXT_QUOTE_REQUIRED",
            Self::NonTextQuoteNotApplicable => "OBSERVATION_TEXT_QUOTE_NOT_APPLICABLE",
            Self::AssertionNeedsLabel => "OBSERVATION_ASSERTION_LABEL_REQUIRED",
            Self::ObservationValueRequired => "OBSERVATION_VALUE_REQUIRED",
            Self::MeasurementUnitRequired => "OBSERVATION_MEASUREMENT_UNIT_REQUIRED",
            Self::InterventionNeedsChangedVariable => "OBSERVATION_INTERVENTION_VARIABLE_REQUIRED",
            Self::SelectorInvalid => "OBSERVATION_SELECTOR_INVALID",
            Self::QuoteRequired => "OBSERVATION_TEXT_QUOTE_REQUIRED",
            Self::SelectorValidationRequired => "OBSERVATION_SELECTOR_VALIDATION_REQUIRED",
            Self::QuoteValidationRequired => "OBSERVATION_QUOTE_VALIDATION_REQUIRED",
            Self::QuoteNotFound => "OBSERVATION_TEXT_QUOTE_NOT_FOUND",
            Self::PositionalSelectorRequired => "OBSERVATION_POSITIONAL_SELECTOR_REQUIRED",
            Self::ArtifactReferenceUnresolved => "OBSERVATION_ARTIFACT_REFERENCE_UNRESOLVED",
            Self::ArtifactReferenceMismatch => "OBSERVATION_ARTIFACT_REFERENCE_MISMATCH",
            Self::ArtifactReferenceWrongKind => "OBSERVATION_ARTIFACT_REFERENCE_WRONG_KIND",
            Self::ArtifactDigestMismatch => "OBSERVATION_ARTIFACT_DIGEST_MISMATCH",
            Self::UnexpectedArtifactReference => "OBSERVATION_ARTIFACT_REFERENCE_UNEXPECTED",
        }
    }
}

impl From<SelectorValidation> for TextQuoteState {
    fn from(value: SelectorValidation) -> Self {
        match value {
            SelectorValidation::Unverified => Self::Unverified,
            SelectorValidation::Verified => Self::Verified,
            SelectorValidation::Ambiguous => Self::Ambiguous,
            SelectorValidation::NotFound => Self::NotFound,
            SelectorValidation::Unavailable => Self::Unavailable,
        }
    }
}

pub fn require_record_kind(id: Id, expected: Kind) -> Result<Id, KindMismatch> {
    id.expect_kind(expected).map_err(|_| KindMismatch {
        expected,
        actual: id.kind(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindMismatch {
    pub expected: Kind,
    pub actual: Kind,
}
