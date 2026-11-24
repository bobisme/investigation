//! Reference-only Artifacts: bytes held at an external location and not
//! retained by the workspace.
//!
//! A reference records a locator plus the digest and size of the bytes it
//! names. Its `availability` is `external`: it cannot satisfy an offline
//! completeness guarantee, selectors on it stay `unavailable`, and no read,
//! check or gate treats it as retained evidence. The core never fetches a
//! locator. A producer or user supplies bytes later (`ingest` with
//! `reverify`), and the core compares them with the recorded digest and size:
//!
//! - A match appends an attributed `verified` entry in a new Artifact
//!   revision. When the bytes were copied, the revision is `retained`.
//! - A mismatch appends an attributed `mismatch` entry and sets integrity
//!   `mismatch`. The recorded digest never changes and the supplied bytes are
//!   not retained in its place. Dependents need review.
//!
//! The verification log is append-only: a later revision keeps every entry.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    command::RevisionRef,
    evidence::{ArtifactAvailability, ArtifactBody, ArtifactIntegrity, ArtifactRole},
    freshness::Trigger,
    header::{Actor, UtcTimestamp},
    identity::Kind,
    input,
    state::{Record, State},
    wire::Digest,
};

/// Longest accepted locator, in characters.
pub const MAX_LOCATOR_CHARS: usize = 2048;

/// Where the bytes of a reference-only Artifact are held, and every attempt to
/// check bytes against its recorded digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExternalReference {
    /// Opaque locator text. It is never fetched, resolved or interpreted.
    pub locator: String,
    /// How the recorded digest and size became known.
    pub digest_basis: DigestBasis,
    /// Append-only attributed verifications, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verifications: Vec<ExternalVerification>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DigestBasis {
    /// Stated by the submitter; the core has not read the bytes.
    Declared,
    /// Computed by the core from a local file it read without copying.
    ReadAtIngest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOutcome {
    Verified,
    Mismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMethod {
    /// Bytes supplied to the core (a local path or stdin) and copied.
    SuppliedBytes,
    /// A local file read in reference mode: hashed, not copied.
    LocalRead,
}

/// One attributed comparison of supplied bytes with the recorded digest and size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExternalVerification {
    pub outcome: VerificationOutcome,
    pub method: VerificationMethod,
    pub observed_digest: Digest,
    pub observed_size: u64,
    pub verified_at: UtcTimestamp,
    pub verified_by: Actor,
    /// Command that supplied the bytes.
    pub command_id: String,
}

/// Verification standing of an Artifact's external reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    /// No supplied bytes have matched the recorded digest.
    Unverified,
    /// The latest verification matched.
    Verified,
    /// The latest verification found different bytes.
    Mismatch,
}

impl ExternalReference {
    pub fn state(&self) -> VerificationState {
        match self.verifications.last().map(|entry| entry.outcome) {
            None => VerificationState::Unverified,
            Some(VerificationOutcome::Verified) => VerificationState::Verified,
            Some(VerificationOutcome::Mismatch) => VerificationState::Mismatch,
        }
    }
}

/// Whether the bytes behind an evidence record are retained in the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetentionState {
    Retained,
    /// The bytes are not retained here. Nothing can be read or verified from
    /// them, and no retained bytes are substituted.
    SourceUnavailable,
}

/// Retention of one Artifact, shown wherever a Source, Artifact or
/// Observation is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Retention {
    pub artifact_ref: RevisionRef,
    pub state: RetentionState,
    /// `retained`, `external_unverified`, `external_verified`,
    /// `external_mismatch` or the recorded availability word.
    pub label: String,
    pub availability: ArtifactAvailability,
    pub integrity: ArtifactIntegrity,
    pub locator: Option<String>,
    pub expected_digest: Option<Digest>,
    pub expected_size: Option<u64>,
    pub digest_basis: Option<DigestBasis>,
    pub verification: Option<VerificationState>,
    pub last_verification: Option<ExternalVerification>,
    pub verification_count: usize,
    /// Always false for bytes that are not retained: a reference cannot
    /// satisfy an offline-completeness guarantee.
    pub offline_complete: bool,
    pub reason: String,
    /// The attributed withdrawal of this reference's availability, when its
    /// bytes are `intentionally_removed` by `artifact.expire`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_change: Option<crate::retention_ops::AvailabilityChange>,
}

fn artifact_body(record: &Record) -> Option<ArtifactBody> {
    (record.header.id.kind() == Kind::Artifact)
        .then(|| input::decode(&record.body).ok())
        .flatten()
}

fn wire_word(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Retention of one Artifact revision. `None` for retained bytes that never
/// were a reference: those need no label.
pub fn artifact_retention(record: &Record) -> Option<Retention> {
    let body = artifact_body(record)?;
    if body.availability == ArtifactAvailability::Retained && body.external.is_none() {
        return None;
    }
    let external = body.external.as_ref();
    let verification = external.map(ExternalReference::state);
    let retained = body.availability == ArtifactAvailability::Retained;
    let label = match (body.availability, verification) {
        (ArtifactAvailability::Retained, _) => "retained".to_owned(),
        (ArtifactAvailability::External, Some(VerificationState::Verified)) => {
            "external_verified".to_owned()
        }
        (ArtifactAvailability::External, Some(VerificationState::Mismatch)) => {
            "external_mismatch".to_owned()
        }
        (ArtifactAvailability::External, _) => "external_unverified".to_owned(),
        (other, _) => wire_word(&other),
    };
    let reason = match (retained, verification) {
        (true, _) => {
            "The bytes were supplied, matched the recorded digest and size, and are retained."
        }
        (false, Some(VerificationState::Mismatch)) => {
            "The bytes are not retained. Bytes supplied for verification differ from the recorded digest or size; the recorded digest is unchanged and nothing replaced it."
        }
        (false, Some(VerificationState::Verified)) => {
            "The bytes are not retained. They matched the recorded digest when last read, but may have changed since. Reverify before relying on them."
        }
        (false, _) if body.availability == ArtifactAvailability::IntentionallyRemoved => {
            "This reference's availability was withdrawn: nothing can be read or verified through it, at any snapshot. Its digest, size and provenance stay recorded; another reference to the same bytes keeps its own availability."
        }
        (false, _) if external.is_some() => {
            "The bytes are not retained, and no supplied bytes have been checked against the declared digest. Reverify before relying on them."
        }
        (false, _) => "The bytes are not retained in this workspace.",
    };
    Some(Retention {
        artifact_ref: record.reference(),
        state: if retained {
            RetentionState::Retained
        } else {
            RetentionState::SourceUnavailable
        },
        label,
        availability: body.availability,
        integrity: body.integrity,
        locator: external.map(|external| external.locator.clone()),
        expected_digest: body.digest.clone(),
        expected_size: body.size_bytes,
        digest_basis: external.map(|external| external.digest_basis),
        verification,
        last_verification: external.and_then(|external| external.verifications.last().cloned()),
        verification_count: external.map_or(0, |external| external.verifications.len()),
        offline_complete: retained,
        reason: reason.to_owned(),
        availability_change: body.availability_change.clone(),
    })
}

/// Retention of the Artifacts behind a Source, Artifact or Observation: the
/// current originals of a Source, the Artifact itself, or the current revision
/// of the Artifact an Observation cites. Empty when every one is retained and
/// never was a reference.
pub fn retention(state: &State, record: &Record) -> Vec<Retention> {
    let case = record.header.case_id;
    match record.header.id.kind() {
        Kind::Artifact => artifact_retention(record).into_iter().collect(),
        Kind::Observation => {
            let Ok(cited) = input::decode::<RevisionRef>(&record.body["basis"]["artifact_ref"])
            else {
                return Vec::new();
            };
            state
                .current(cited.id)
                .filter(|current| current.header.case_id == case)
                .and_then(artifact_retention)
                .into_iter()
                .collect()
        }
        Kind::Source => state
            .records()
            .filter(|candidate| {
                candidate.header.case_id == case && candidate.header.id.kind() == Kind::Artifact
            })
            .filter(|candidate| {
                artifact_body(candidate).is_some_and(|body| {
                    body.source.id == record.header.id && body.role() == ArtifactRole::Original
                })
            })
            .filter_map(artifact_retention)
            .collect(),
        _ => Vec::new(),
    }
}

/// Current reference-only Artifacts of a case, for exports and archives that
/// must list them as omissions rather than as retained content.
pub fn reference_only(state: &State, case: crate::identity::Id) -> Vec<RevisionRef> {
    state
        .records()
        .filter(|record| record.header.case_id == case)
        .filter_map(artifact_retention)
        .filter(|retention| retention.state == RetentionState::SourceUnavailable)
        .map(|retention| retention.artifact_ref)
        .collect()
}

fn verifications(body: &Value) -> Vec<ExternalVerification> {
    input::decode::<ExternalReference>(&body["external"])
        .map(|external| external.verifications)
        .unwrap_or_default()
}

/// How a change between two revisions of a reference-only Artifact reaches a
/// dependent. `Some(Some(trigger))`: a newly recorded mismatch. `Some(None)`:
/// only a successful verification was added (and possibly the bytes became
/// retained); the cited bytes are unchanged, so dependents stay current.
/// `None`: not a verification change; the caller applies its other rules.
pub(crate) fn verification_change(cited: &Record, current: &Record) -> Option<Option<Trigger>> {
    if current.header.id.kind() != Kind::Artifact || current.body["external"].is_null() {
        return None;
    }
    let before = verifications(&cited.body);
    let after = verifications(&current.body);
    if after.len() <= before.len() || after[..before.len()] != before[..] {
        return None;
    }
    if after[before.len()..]
        .iter()
        .any(|entry| entry.outcome == VerificationOutcome::Mismatch)
    {
        return Some(Some(Trigger::ExternalMismatch));
    }
    let strip = |body: &Value| {
        let mut body = body.clone();
        if let Some(object) = body.as_object_mut() {
            for field in ["availability", "integrity", "external"] {
                object.remove(field);
            }
        }
        body
    };
    let upgrade = strip(&cited.body) == strip(&current.body)
        && matches!(
            text(&current.body["availability"]),
            Some("retained" | "external")
        );
    upgrade.then_some(None)
}

fn text(value: &Value) -> Option<&str> {
    value.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::{Classification, Provenance, RecordHeader, Revision, SchemaVersion};
    use serde_json::json;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const CASE: &str = "case_018d87fc-5234-7000-8000-000000000001";
    const SOURCE: &str = "src_018d87fc-5234-7000-8000-000000000002";
    const ARTIFACT: &str = "art_018d87fc-5234-7000-8000-000000000003";

    fn record(revision: u64, body: Value) -> Record {
        let at: UtcTimestamp = "2026-11-22T08:12:44.000Z".parse().unwrap();
        let actor: Actor = "tester".parse().unwrap();
        Record {
            header: RecordHeader {
                id: ARTIFACT.parse().unwrap(),
                case_id: CASE.parse().unwrap(),
                schema_version: SchemaVersion::RecordV0Alpha,
                created_by: actor.clone(),
                actor_descriptor_self_reported: None,
                created_at: at.clone(),
                recorded_at: at,
                recorded_by: actor,
                observed_at: None,
                acquired_at: None,
                acceptance_sequence: revision,
                revision: Revision(revision),
                history: (1..revision).map(Revision).collect(),
                classification: Classification::default(),
                provenance: Provenance::default(),
                extensions: Default::default(),
            },
            body,
        }
    }

    fn entry(outcome: &str, digest: &str) -> Value {
        json!({"outcome":outcome,"method":"supplied_bytes","observed_digest":digest,"observed_size":4,
            "verified_at":"2026-11-22T08:12:44.000Z","verified_by":"tester","command_id":"verify-1"})
    }

    fn body(availability: &str, integrity: &str, entries: Vec<Value>) -> Value {
        json!({"digest":DIGEST,"size_bytes":4,"format":null,"source":{"id":SOURCE,"revision":1},
            "transforms":[],"availability":availability,"integrity":integrity,"sensitivity":"internal",
            "role":"original","external":{"locator":"archive://box-7/item-12","digest_basis":"declared","verifications":entries}})
    }

    #[test]
    fn an_unverified_reference_is_labeled_source_unavailable_and_never_offline_complete() {
        let reference = record(1, body("external", "not_checked", Vec::new()));
        let retention = artifact_retention(&reference).unwrap();
        assert_eq!(retention.state, RetentionState::SourceUnavailable);
        assert_eq!(retention.label, "external_unverified");
        assert_eq!(retention.verification, Some(VerificationState::Unverified));
        assert_eq!(
            retention.locator.as_deref(),
            Some("archive://box-7/item-12")
        );
        assert!(!retention.offline_complete);
    }

    #[test]
    fn a_verified_upgrade_keeps_dependents_current_and_a_mismatch_marks_them() {
        let cited = record(1, body("external", "not_checked", Vec::new()));
        let upgraded = record(
            2,
            body("retained", "verified", vec![entry("verified", DIGEST)]),
        );
        assert_eq!(verification_change(&cited, &upgraded), Some(None));
        assert_eq!(
            artifact_retention(&upgraded).unwrap().state,
            RetentionState::Retained
        );
        let failed = record(
            2,
            body("external", "mismatch", vec![entry("mismatch", OTHER)]),
        );
        assert_eq!(
            verification_change(&cited, &failed),
            Some(Some(Trigger::ExternalMismatch))
        );
        let retention = artifact_retention(&failed).unwrap();
        assert_eq!(retention.label, "external_mismatch");
        assert_eq!(retention.expected_digest.unwrap().as_str(), DIGEST);
        // Any other change alongside a verification is left to the other rules.
        let mut moved = body("retained", "verified", vec![entry("verified", DIGEST)]);
        moved["format"] = json!("text/plain");
        assert_eq!(verification_change(&cited, &record(2, moved)), None);
        // A log that drops an earlier entry is not a verification.
        let shrunk = record(3, body("external", "not_checked", Vec::new()));
        assert_eq!(verification_change(&failed, &shrunk), None);
    }
}
