//! Derived artifacts arriving through ingest.
//!
//! A trusted reader or external producer that transformed recorded Artifacts
//! (redaction, text extraction, a plot, an aggregation) ingests the output with
//! `derived_from`. The core snapshots the output bytes like any other input, but
//! records them as a derivative: no Source and no acquisition is created, and the
//! derivative links to its exact inputs, their recorded digests, the
//! transformation identity and version. The inputs are never modified; the
//! output is a new Artifact with its own record even when its bytes deduplicate.
//! Observations can then cite the derivative and still be traced to the original
//! acquisition.

use std::collections::BTreeSet;

use investigation_core::{
    classification::RedactionDecision,
    command::RevisionRef,
    derivation::{DerivationRecord, DerivationReview},
    evidence::{ArtifactBody, ArtifactRole, ArtifactTransform, SelectorRequest},
    identity::{Id, Kind},
    wire::{Digest, Failure},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Manifest, invalid};
use crate::journal::CaseReplay;

pub const MAX_INPUTS: usize = 64;
pub const MAX_SELECTORS: usize = 64;
pub const MAX_REDACTIONS: usize = 256;

/// One exact Artifact revision the transformation read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DerivationInputRequest {
    pub artifact_ref: RevisionRef,
    /// Digest of the bytes the transformation read. When supplied it must match
    /// the input's recorded digest; the recorded digest is what is stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
}

/// The ingested bytes are the output of a transformation of recorded Artifacts.
/// The output is recorded as a derivative of those inputs, never as a new
/// acquisition. Requires an existing `case_id`; the inputs must be in that case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DerivedFromRequest {
    /// Direct inputs; the first is the primary input whose acquisition the
    /// derivative's `source` names.
    #[schemars(length(min = 1, max = 64))]
    pub inputs: Vec<DerivationInputRequest>,
    /// Transformation identity, such as `example-org.redact`.
    #[schemars(length(min = 1, max = 256))]
    pub transformation: String,
    #[schemars(length(min = 1, max = 128))]
    pub version: String,
    /// Portions of the inputs the transformation used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 64))]
    pub selectors: Vec<SelectorRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 256))]
    pub redaction_decisions: Vec<RedactionDecision>,
    /// How the output was reviewed. Omit it when nobody reviewed the output.
    /// A review never lowers classification; use `classification.downgrade`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<DerivationReview>,
}

fn text(value: &str, max: usize, path: &str, what: &str) -> Result<(), Failure> {
    if value.trim().is_empty() || value.chars().count() > max || value.chars().any(char::is_control)
    {
        return Err(invalid(
            &format!("Provide {what} of 1–{max} characters without control characters."),
            path,
        ));
    }
    Ok(())
}

fn prose(value: &str, max: usize, path: &str, what: &str) -> Result<(), Failure> {
    if value.trim().is_empty() || value.chars().count() > max {
        return Err(invalid(
            &format!("Provide {what} of 1–{max} characters."),
            path,
        ));
    }
    Ok(())
}

impl DerivedFromRequest {
    /// Shape checks that need no canonical state.
    pub(super) fn validate(&self, manifest: &Manifest) -> Result<(), Failure> {
        if manifest.case_id.is_none() {
            return Err(invalid(
                "A derivative is ingested into the existing case that holds its inputs. Supply case_id.",
                "/case_id",
            ));
        }
        let source = &manifest.source;
        for (present, field) in [
            (source.acquisition_key.is_some(), "acquisition_key"),
            (source.publisher.is_some(), "publisher"),
            (source.identity.is_some(), "identity"),
        ] {
            if present {
                return Err(invalid(
                    "A derivative is not a new acquisition. Describe publisher, identity and acquisition key on the original's Source.",
                    &format!("/source/{field}"),
                ));
            }
        }
        if self.inputs.is_empty() || self.inputs.len() > MAX_INPUTS {
            return Err(invalid(
                "Name 1–64 exact input Artifact revisions.",
                "/derived_from/inputs",
            ));
        }
        let mut seen = BTreeSet::new();
        for (index, input) in self.inputs.iter().enumerate() {
            let path = format!("/derived_from/inputs/{index}/artifact_ref");
            if input.artifact_ref.id.kind() != Kind::Artifact {
                return Err(invalid("A derivation input must be an Artifact.", &path));
            }
            if !seen.insert(input.artifact_ref.id) {
                return Err(invalid("Name each input Artifact once.", &path));
            }
        }
        text(
            &self.transformation,
            256,
            "/derived_from/transformation",
            "a transformation identity",
        )?;
        text(
            &self.version,
            128,
            "/derived_from/version",
            "a transformation version",
        )?;
        if self.selectors.len() > MAX_SELECTORS {
            return Err(invalid(
                "Supply at most 64 selectors.",
                "/derived_from/selectors",
            ));
        }
        for (index, selector) in self.selectors.iter().enumerate() {
            selector.selector.validate_shape().map_err(|blocker| {
                let mut failure = invalid(
                    "The selector is malformed.",
                    &format!("/derived_from/selectors/{index}/selector"),
                );
                failure.error.actual = Some(serde_json::json!({"blocker": blocker.code()}));
                failure
            })?;
        }
        if self.redaction_decisions.len() > MAX_REDACTIONS {
            return Err(invalid(
                "Supply at most 256 redaction decisions.",
                "/derived_from/redaction_decisions",
            ));
        }
        for (index, decision) in self.redaction_decisions.iter().enumerate() {
            let path = format!("/derived_from/redaction_decisions/{index}");
            prose(
                &decision.scope,
                512,
                &format!("{path}/scope"),
                "a redaction scope",
            )?;
            prose(
                &decision.rationale,
                2048,
                &format!("{path}/rationale"),
                "a redaction rationale",
            )?;
        }
        if let Some(review) = &self.review {
            prose(
                &review.method,
                2048,
                "/derived_from/review/method",
                "a review method",
            )?;
            if review.limitations.len() > 32 {
                return Err(invalid(
                    "Supply at most 32 review limitations.",
                    "/derived_from/review/limitations",
                ));
            }
            for (index, limitation) in review.limitations.iter().enumerate() {
                prose(
                    limitation,
                    2048,
                    &format!("/derived_from/review/limitations/{index}"),
                    "a review limitation",
                )?;
            }
        }
        Ok(())
    }
}

/// A derivation request resolved against the verified target case.
pub(super) struct ResolvedDerivation {
    /// Acquisition of the primary input; the derivative's `source`.
    pub source: RevisionRef,
    pub transforms: Vec<ArtifactTransform>,
    pub record: DerivationRecord,
    /// Header provenance of each derivative: the source and every input.
    pub provenance: Vec<Id>,
}

/// Resolve inputs to exact recorded Artifacts. Runs under the commit lock.
pub(super) fn resolve(
    request: &DerivedFromRequest,
    manifest: &Manifest,
    case: &CaseReplay,
) -> Result<ResolvedDerivation, Failure> {
    let state = &case.state;
    let manifests = super::acquisition::manifest_artifacts(case, state.sequence());
    let mut transforms = Vec::new();
    let mut source = None;
    let mut digests = BTreeSet::new();
    let mut provenance = Vec::new();
    for (index, input) in request.inputs.iter().enumerate() {
        let path = format!("/derived_from/inputs/{index}");
        let record = state.revision(input.artifact_ref).ok_or_else(|| {
            invalid(
                "The input Artifact revision is not recorded in this case.",
                &format!("{path}/artifact_ref"),
            )
        })?;
        let body: ArtifactBody = investigation_core::input::decode(&record.body)
            .map_err(|_| crate::store::integrity("Invalid canonical Artifact record."))?;
        if body.role() == ArtifactRole::Manifest || manifests.contains(&record.header.id) {
            return Err(invalid(
                "An ingest manifest records an import request, not evidence. Derive from an original or a derivative.",
                &format!("{path}/artifact_ref"),
            ));
        }
        match (&input.digest, &body.digest) {
            (Some(claimed), Some(recorded)) if claimed != recorded => {
                let mut failure = invalid(
                    "The input digest does not match the recorded Artifact.",
                    &format!("{path}/digest"),
                );
                failure.error.actual = Some(serde_json::json!({"recorded_digest": recorded}));
                return Err(failure);
            }
            (Some(_), None) => {
                return Err(invalid(
                    "The input Artifact has no recorded digest to compare with.",
                    &format!("{path}/digest"),
                ));
            }
            _ => {}
        }
        if let Some(digest) = &body.digest {
            digests.insert(digest.clone());
        }
        source.get_or_insert(body.source);
        if !provenance.contains(&body.source.id) {
            provenance.push(body.source.id);
        }
        provenance.push(record.header.id);
        transforms.push(ArtifactTransform {
            input: input.artifact_ref,
            transformation: request.transformation.clone(),
            version: request.version.clone(),
            input_digest: body.digest,
        });
    }
    for (index, selector) in request.selectors.iter().enumerate() {
        if !digests.contains(&selector.artifact_digest) {
            return Err(invalid(
                "A selector must address the recorded digest of one of the inputs.",
                &format!("/derived_from/selectors/{index}/artifact_digest"),
            ));
        }
    }
    let source = source.ok_or_else(|| invalid("Name an input.", "/derived_from/inputs"))?;
    let described = &manifest.source;
    Ok(ResolvedDerivation {
        source,
        transforms,
        provenance,
        record: DerivationRecord {
            label: Some(described.title.clone()),
            producer: described.producer.clone(),
            context: described.context.clone(),
            selectors: request.selectors.clone(),
            redaction_decisions: request.redaction_decisions.clone(),
            review: request.review.clone(),
            limitations: described.verification_limits.clone(),
            metadata: described.metadata.clone(),
        },
    })
}
