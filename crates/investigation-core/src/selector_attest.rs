//! `selector.attest`: an attributed validation of one Observation's selector,
//! for selectors the core cannot check against retained bytes itself.
//!
//! The core never loads or runs a format reader named in a selector or a
//! manifest. A `time_window`, an `external_selector`, a `table_cell` from a
//! producer's extraction or a `code_span` whose blob is not retained is stored
//! `validation: unavailable`. A trusted reader or producer that did check it
//! records the outcome here, under its own actor and validator identity.
//!
//! Each attestation is a new Annotation with role [`ATTESTATION_ROLE`]. The
//! Observation is not revised: its recorded selector and the validation the
//! core computed stay as they were. Reads compute the displayed state with
//! [`selector_state`]: the core's own result when it checked the bytes,
//! otherwise the current attestations, labelled `attested`. An attested
//! `verified` is never the core's `verified`; direct-support gates still
//! require the core's own verification of retained bytes.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    catalog::{OperationDescriptor, OperationInfo},
    command::RevisionRef,
    evidence::{
        ArtifactRole, ArtifactTransform, ObservationBasis, SelectorKind, SelectorRequest,
        SelectorValidation,
    },
    header::Actor,
    identity::{Id, Kind},
    input,
    state::{OperationContext, Record, State},
    wire::{ErrorCode, Failure},
};

/// Annotation role of a stored attestation.
pub const ATTESTATION_ROLE: &str = "selector_validation";
/// Annotation body key holding the [`StoredAttestation`].
pub const ATTESTATION_KEY: &str = "selector_attestation";
const MAX_ATTESTATIONS: usize = 64;
const MAX_LIMITS: usize = 32;
const TEXT_LIMIT: usize = 2048;
const NAME_LIMIT: usize = 256;

/// What the attesting reader found. `unavailable` and `unverified` are not
/// outcomes of a check, so they cannot be attested.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttestedOutcome {
    Verified,
    Ambiguous,
    NotFound,
}

impl AttestedOutcome {
    const fn validation(self) -> SelectorValidation {
        match self {
            Self::Verified => SelectorValidation::Verified,
            Self::Ambiguous => SelectorValidation::Ambiguous,
            Self::NotFound => SelectorValidation::NotFound,
        }
    }

    /// Conservative order: `not_found` < `ambiguous` < `verified`.
    const fn rank(self) -> u8 {
        match self {
            Self::NotFound => 0,
            Self::Ambiguous => 1,
            Self::Verified => 2,
        }
    }
}

/// The reader or validator that performed the check, as self-reported.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidatorIdentity {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectorAttest {
    /// Exact current Observation revision whose selector was checked.
    pub observation_ref: RevisionRef,
    /// The selector request exactly as the Observation records it: what the
    /// reader checked. It must equal the Observation's selector.
    pub selector: SelectorRequest,
    pub outcome: AttestedOutcome,
    pub validator: ValidatorIdentity,
    /// Exact derivative Artifact the reader extracted from the cited
    /// Artifact while checking, if it kept one.
    #[serde(default)]
    pub extraction_ref: Option<RevisionRef>,
    pub rationale: String,
    #[serde(default)]
    pub limits: Vec<String>,
}

/// The attestation as stored on its Annotation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoredAttestation {
    pub observation_ref: RevisionRef,
    pub selector: SelectorRequest,
    pub outcome: AttestedOutcome,
    pub validator: ValidatorIdentity,
    /// The `validator` an `external_selector` declares, if any, and whether
    /// the attesting validator has that name. Null for other selectors.
    pub declared_validator: Option<String>,
    pub matches_declared_validator: Option<bool>,
    pub extraction_ref: Option<RevisionRef>,
    pub rationale: String,
    pub limits: Vec<String>,
    pub attested_by: Actor,
    pub accepted_at_sequence: u64,
    /// The validation the core recorded when the Observation was accepted.
    pub recorded_validation: SelectorValidation,
}

/// Whose result the displayed selector state is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SelectorStateBasis {
    /// The validation the core computed from retained bytes, or `unavailable`
    /// when it could not check them and nobody has attested.
    Core,
    /// Attributed attestations by named readers; the core did not check.
    Attested,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttestationView {
    pub annotation_ref: RevisionRef,
    pub attested_by: Actor,
    pub validator: ValidatorIdentity,
    pub outcome: AttestedOutcome,
    pub matches_declared_validator: Option<bool>,
    pub extraction_ref: Option<RevisionRef>,
    pub accepted_at_sequence: u64,
}

/// The displayed selector state of one source-backed Observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SelectorStateView {
    pub selector_type: String,
    /// What to display: the core's result, or the attested one.
    pub state: SelectorValidation,
    pub basis: SelectorStateBasis,
    /// The validation the core recorded with the Observation. Never changed.
    pub recorded: SelectorValidation,
    /// The latest attestation per attesting actor and validator name that
    /// applies to the Observation's current selector.
    pub attestations: Vec<AttestationView>,
    /// Every attestation kept for this Observation, earlier ones included.
    pub attestations_kept: usize,
    pub disagreement: bool,
    pub meaning: &'static str,
}

pub(crate) fn operations() -> [OperationDescriptor; 1] {
    [OperationDescriptor::active(
        OperationInfo {
            name: "selector.attest",
            stage: "observe",
            summary: "Record an attributed validation of an Observation's selector by a named reader or validator.",
            when_to_use: "A trusted reader or producer checked a selector the core cannot check itself, such as an external_selector, a time_window, a producer's table extraction or a code span whose blob is not retained.",
            lifecycle: "A current, unretracted, source-backed Observation whose recorded selector validation is unavailable or unverified.",
            effects: "Create one Annotation holding the attestation. The Observation, its selector and its recorded validation stay unchanged; reads show the attested state labelled attested.",
            returns: "The Annotation ID.",
            revision_rule: "Supply the exact current observation_ref and the selector it records.",
            idempotency: "Retry the identical command with its original actor and command ID.",
            repair: "Read the Observation and copy its exact selector request. A selector the core already verified, found ambiguous or did not find cannot be attested.",
            related: &["observation.add", "observation.correct", "annotation.add"],
        },
        json!({
            "observation_ref": {"id": "obs_019acd16-0000-7000-8000-000000000001", "revision": 1},
            "selector": {
                "schema_version": "inv.selector.v1",
                "artifact_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                "selector": {
                    "type": "external_selector",
                    "namespace": "example.frames",
                    "schema_uri": "urn:example:frame-index",
                    "schema_version": "1",
                    "value": {"frame": 12},
                    "validator": "example-frame-reader"
                }
            },
            "outcome": "verified",
            "validator": {"name": "example-frame-reader", "version": "2.1"},
            "extraction_ref": null,
            "rationale": "The reader decoded frame 12 from the cited bytes.",
            "limits": []
        }),
        validate,
        attest,
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
        "A stored Observation or selector attestation is invalid.",
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

fn validate(payload: &SelectorAttest) -> Result<(), Failure> {
    if payload.observation_ref.id.kind() != Kind::Observation {
        return Err(invalid(
            "/observation_ref",
            "The reference has the wrong record kind.",
        ));
    }
    if let Some(extraction) = payload.extraction_ref
        && extraction.id.kind() != Kind::Artifact
    {
        return Err(invalid(
            "/extraction_ref",
            "The reference has the wrong record kind.",
        ));
    }
    payload
        .selector
        .selector
        .validate_shape()
        .map_err(|blocker| invalid("/selector/selector", blocker.code()))?;
    text(&payload.validator.name, NAME_LIMIT, "/validator/name")?;
    text(&payload.validator.version, NAME_LIMIT, "/validator/version")?;
    text(&payload.rationale, TEXT_LIMIT, "/rationale")?;
    if payload.limits.len() > MAX_LIMITS {
        return Err(invalid("/limits", "Supply at most 32 limits."));
    }
    for (index, limit) in payload.limits.iter().enumerate() {
        text(limit, TEXT_LIMIT, &format!("/limits/{index}"))?;
    }
    Ok(())
}

/// Same-case current record at the exact cited revision.
fn exact_current<'a>(
    state: &'a State,
    case: Id,
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
        failure.error.retry = Some("read current revision and attest again".into());
        return Err(failure);
    }
    Ok(record)
}

/// The Observation's source basis: cited Artifact revision, selector request
/// and recorded validation. None for an attributed assertion.
fn source_basis(
    record: &Record,
) -> Result<Option<(RevisionRef, SelectorRequest, SelectorValidation)>, Failure> {
    let basis: ObservationBasis =
        input::decode(record.body.get("basis").ok_or_else(integrity)?).map_err(|_| integrity())?;
    Ok(match basis {
        ObservationBasis::Assertion { .. } => None,
        ObservationBasis::Source {
            artifact_ref,
            selector,
            ..
        } => Some((
            artifact_ref,
            SelectorRequest {
                schema_version: selector.schema_version,
                artifact_digest: selector.artifact_digest,
                selector: selector.selector,
            },
            selector.validation,
        )),
    })
}

fn declared_validator(selector: &SelectorKind) -> Option<&str> {
    match selector {
        SelectorKind::ExternalSelector { validator, .. } => validator.as_deref(),
        _ => None,
    }
}

fn attest(payload: SelectorAttest, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let case = ctx.case_id().ok_or_else(integrity)?;
    let observation = exact_current(
        ctx.state(),
        case,
        payload.observation_ref,
        "/observation_ref",
    )?;
    if observation.body["status"] == "retracted" || observation.body["retracted"] == true {
        return Err(gate(
            "/observation_ref",
            "A retracted Observation has no current use to validate; its history stays readable.",
        ));
    }
    let Some((artifact_ref, selector, recorded)) = source_basis(observation)? else {
        return Err(gate(
            "/observation_ref",
            "An attributed assertion cites no selector to validate.",
        ));
    };
    if selector != payload.selector {
        let mut failure = gate(
            "/selector",
            "The Observation records a different selector. Attest the exact selector request it records.",
        );
        failure.error.expected = Some(json!(selector));
        return Err(failure);
    }
    if !matches!(
        recorded,
        SelectorValidation::Unavailable | SelectorValidation::Unverified
    ) {
        let mut failure = gate(
            "/observation_ref",
            "The core checked this selector against the retained bytes itself; its result stands and cannot be attested over.",
        );
        failure.error.actual = Some(json!({"validation": recorded}));
        return Err(failure);
    }
    if let Some(extraction) = payload.extraction_ref {
        let record = exact_current(ctx.state(), case, extraction, "/extraction_ref")?;
        let transforms: Vec<ArtifactTransform> =
            input::decode(&record.body["transforms"]).map_err(|_| integrity())?;
        let role = record.body.get("role").and_then(Value::as_str);
        let derivative = role == Some(ArtifactRole::Derivative.as_str())
            || (role.is_none() && !transforms.is_empty());
        if !derivative
            || !transforms
                .iter()
                .any(|transform| transform.input.id == artifact_ref.id)
        {
            return Err(gate(
                "/extraction_ref",
                "An extraction is a derivative Artifact whose inputs include the Artifact the Observation cites. Ingest the extracted bytes with derived_from naming it.",
            ));
        }
    }
    let kept = attestations(ctx.state(), observation).count();
    if kept >= MAX_ATTESTATIONS {
        return Err(gate(
            "/observation_ref",
            "This Observation has reached its attestation limit; record a corrected Observation instead.",
        ));
    }
    let declared = declared_validator(&selector.selector).map(str::to_owned);
    let matches_declared = declared
        .as_deref()
        .map(|declared| declared == payload.validator.name);
    let stored = StoredAttestation {
        observation_ref: payload.observation_ref,
        selector: payload.selector,
        outcome: payload.outcome,
        validator: payload.validator,
        declared_validator: declared,
        matches_declared_validator: matches_declared,
        extraction_ref: payload.extraction_ref,
        rationale: payload.rationale,
        limits: payload.limits,
        attested_by: ctx.actor().clone(),
        accepted_at_sequence: ctx.acceptance_sequence(),
        recorded_validation: recorded,
    };
    let mut subjects = vec![payload.observation_ref];
    subjects.extend(payload.extraction_ref);
    let summary = format!(
        "Selector validation attested {} by {} {}",
        json!(stored.outcome).as_str().unwrap_or_default(),
        stored.validator.name,
        stored.validator.version
    );
    let body = json!({
        "subject_refs": subjects,
        "role": ATTESTATION_ROLE,
        "evidentiary_status": "none",
        "content": {"kind": "text", "text": summary},
        ATTESTATION_KEY: serde_json::to_value(&stored).map_err(|_| integrity())?,
    });
    ctx.create(Kind::Annotation, body)?;
    Ok(())
}

/// Whether `record` is a stored selector attestation.
pub fn is_attestation(record: &Record) -> bool {
    record.header.id.kind() == Kind::Annotation
        && record.body.get("role").and_then(Value::as_str) == Some(ATTESTATION_ROLE)
}

/// Every attestation kept for `observation`, in acceptance order.
fn attestations(
    state: &State,
    observation: &Record,
) -> impl Iterator<Item = (RevisionRef, StoredAttestation)> {
    let key = (observation.header.case_id, observation.header.id);
    state
        .attestation_index()
        .0
        .get(&key)
        .cloned()
        .unwrap_or_default()
        .into_iter()
}

/// Every readable attestation of the snapshot, by (case, Observation), each
/// list in acceptance order, ties in annotation ID order. Built once per
/// snapshot by [`State::attestation_index`] instead of once per Observation.
pub(crate) struct AttestationIndex(BTreeMap<(Id, Id), Vec<(RevisionRef, StoredAttestation)>>);

impl AttestationIndex {
    pub(crate) fn index(state: &State) -> Self {
        let mut index: BTreeMap<(Id, Id), Vec<(RevisionRef, StoredAttestation)>> = BTreeMap::new();
        for record in state.records().filter(|record| is_attestation(record)) {
            let Ok(stored) = input::decode::<StoredAttestation>(&record.body[ATTESTATION_KEY])
            else {
                continue;
            };
            index
                .entry((record.header.case_id, stored.observation_ref.id))
                .or_default()
                .push((record.reference(), stored));
        }
        for found in index.values_mut() {
            found.sort_by_key(|(_, stored)| stored.accepted_at_sequence);
        }
        Self(index)
    }
}

const CORE_MEANING: &str = "The validation the core computed from the retained bytes when the Observation was accepted. `unavailable` means the core could not check this selector; it is not verified.";
const ATTESTED_MEANING: &str = "Attested by the named readers, not checked by the core. An attested `verified` shows the reader's own check only; direct support still needs the core's verification of retained bytes.";

/// The displayed selector state of a source-backed Observation, or None for
/// an attributed assertion or an unreadable basis.
pub fn selector_state(state: &State, observation: &Record) -> Option<SelectorStateView> {
    if observation.header.id.kind() != Kind::Observation {
        return None;
    }
    let (_, selector, recorded) = source_basis(observation).ok()??;
    let kept: Vec<_> = attestations(state, observation).collect();
    let mut current = BTreeMap::new();
    if matches!(
        recorded,
        SelectorValidation::Unavailable | SelectorValidation::Unverified
    ) {
        for (annotation_ref, stored) in &kept {
            if stored.selector == selector {
                current.insert(
                    (stored.attested_by.clone(), stored.validator.name.clone()),
                    AttestationView {
                        annotation_ref: *annotation_ref,
                        attested_by: stored.attested_by.clone(),
                        validator: stored.validator.clone(),
                        outcome: stored.outcome,
                        matches_declared_validator: stored.matches_declared_validator,
                        extraction_ref: stored.extraction_ref,
                        accepted_at_sequence: stored.accepted_at_sequence,
                    },
                );
            }
        }
    }
    let attestations: Vec<AttestationView> = current.into_values().collect();
    let lowest = attestations
        .iter()
        .map(|attestation| attestation.outcome)
        .min_by_key(|outcome| outcome.rank());
    let disagreement = attestations
        .iter()
        .any(|attestation| Some(attestation.outcome) != lowest);
    let (state, basis, meaning) = match lowest {
        Some(outcome) => (
            outcome.validation(),
            SelectorStateBasis::Attested,
            ATTESTED_MEANING,
        ),
        None => (recorded, SelectorStateBasis::Core, CORE_MEANING),
    };
    Some(SelectorStateView {
        selector_type: selector.selector.type_name().to_owned(),
        state,
        basis,
        recorded,
        attestations,
        attestations_kept: kept.len(),
        disagreement,
        meaning,
    })
}
