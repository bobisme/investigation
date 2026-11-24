//! Explicit, digest-pinned adoption of declarative method packs per case.
//!
//! A case adopts a pack through the attributed `method_pack.adopt` operation.
//! The pack file must already be retained as an Artifact through the standard
//! ingest path, so the case carries the exact bytes and stays self-contained.
//! Adoption revises the Case record with a pin (pack ID, version, digest,
//! Artifact revision, reason and acceptance sequence). Earlier Case revisions
//! keep the pins they had, so a reading at an earlier sequence resolves the
//! policy that applied then.
//!
//! Resolution never reads a pack from a path on disk. It takes the retained
//! bytes for each pin, checks them against the pinned digest, and loads them
//! with the inert [`method_pack::load`]. Editing or replacing the original
//! file therefore cannot change an adopted case; moving to another revision is
//! a new adoption that names the replaced digest and gives a reason.
//!
//! The built-in `general` pack is always available and cannot be pinned or
//! shadowed. A pinned pack whose bytes are missing, altered or no longer
//! loadable resolves as unavailable with a reason; it never blocks recording,
//! reading or export, and no pack can change serialization, integrity checks,
//! evidence trust rules, atomicity or access decisions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::{
    case::{CaseBody, CaseLifecycle},
    case_ops::case_for_update,
    catalog::{OperationDescriptor, OperationInfo},
    command::RevisionRef,
    evidence::{ArtifactAvailability, ArtifactBody},
    identity::{Id, Kind},
    input,
    method_pack::{self, LoadedPack},
    state::{OperationContext, State},
    wire::{Digest, ErrorCode, Failure},
};

/// The operation that pins a pack revision to a case.
pub const ADOPT: &str = "method_pack.adopt";
/// The ID of the built-in pack. It is always available and cannot be pinned.
pub const BUILTIN_PACK_ID: &str = "general";

/// Payload for `method_pack.adopt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MethodPackAdopt {
    /// Exact revision of the retained Artifact that holds the original pack file bytes.
    pub artifact_ref: RevisionRef,
    /// The `id` declared in the pack file.
    #[schemars(length(min = 1, max = 64))]
    pub pack_id: String,
    /// The `version` declared in the pack file.
    #[schemars(length(min = 1, max = 64))]
    pub version: String,
    /// SHA-256 digest of the exact pack file bytes; it must match the Artifact.
    pub digest: Digest,
    #[schemars(length(min = 1, max = 2048))]
    pub reason: String,
    /// Digest currently pinned for this pack ID. Required, and must match, when
    /// the case already pins the pack; omit it for a first adoption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<Digest>,
}

/// One pack revision a case has adopted. Stored on the Case record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MethodPackPin {
    pub pack_id: String,
    pub version: String,
    pub digest: Digest,
    pub artifact_ref: RevisionRef,
    pub reason: String,
    pub adopted_at_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<PinnedRevision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PinnedRevision {
    pub version: String,
    pub digest: Digest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackOrigin {
    BuiltIn,
    Pinned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PackStatus {
    Available,
    Unavailable,
}

/// Why a pinned pack cannot be used. The pin itself stays on the case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// The retained bytes for the pinned digest cannot be read.
    BytesUnavailable,
    /// The bytes read do not hash to the pinned digest.
    DigestMismatch,
    /// The bytes do not load as a pack this build supports.
    InvalidPack,
    /// The bytes load, but declare another pack ID or version than the pin.
    IdentityMismatch,
}

/// A pack that applies to a case, with its availability.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedPack {
    pub pack_id: String,
    pub version: String,
    pub digest: Digest,
    pub origin: PackOrigin,
    pub status: PackStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<UnavailableReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<MethodPackPin>,
    #[serde(skip)]
    pub loaded: Option<LoadedPack>,
}

impl ResolvedPack {
    pub fn is_available(&self) -> bool {
        self.status == PackStatus::Available
    }

    fn builtin() -> Self {
        let loaded = method_pack::builtin_general();
        Self {
            pack_id: loaded.id().to_owned(),
            version: loaded.version().to_owned(),
            digest: loaded.digest().clone(),
            origin: PackOrigin::BuiltIn,
            status: PackStatus::Available,
            unavailable_reason: None,
            pin: None,
            loaded: Some(loaded),
        }
    }
}

/// The pins on a case, at its current revision or as of an acceptance sequence.
pub fn adopted_packs(
    state: &State,
    case_id: Id,
    at_sequence: Option<u64>,
) -> Result<Vec<MethodPackPin>, Failure> {
    let record = match at_sequence {
        Some(sequence) => state.record_at(case_id, sequence),
        None => state.current(case_id),
    };
    let Some(record) = record.filter(|_| case_id.kind() == Kind::Case) else {
        return Ok(Vec::new());
    };
    match record.body.get("adopted_method_packs") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value) => input::decode(value).map_err(|_| {
            Failure::new(
                ErrorCode::IntegrityFailure,
                "Stored method pack pins have an invalid shape.",
            )
        }),
    }
}

/// Resolve the built-in pack and every pin. `fetch` returns the retained bytes
/// for a pin, or `None` when they cannot be read; it must never consult the
/// original file path.
pub fn resolve(
    pins: &[MethodPackPin],
    mut fetch: impl FnMut(&MethodPackPin) -> Option<Vec<u8>>,
) -> Vec<ResolvedPack> {
    std::iter::once(ResolvedPack::builtin())
        .chain(
            pins.iter()
                .map(|pin| resolve_pin(pin, fetch(pin).as_deref())),
        )
        .collect()
}

/// Check retained bytes against one pin without side effects.
pub fn resolve_pin(pin: &MethodPackPin, bytes: Option<&[u8]>) -> ResolvedPack {
    let mut resolved = ResolvedPack {
        pack_id: pin.pack_id.clone(),
        version: pin.version.clone(),
        digest: pin.digest.clone(),
        origin: PackOrigin::Pinned,
        status: PackStatus::Unavailable,
        unavailable_reason: None,
        pin: Some(pin.clone()),
        loaded: None,
    };
    let outcome = match bytes {
        None => Err(UnavailableReason::BytesUnavailable),
        Some(bytes) if format!("sha256:{:x}", Sha256::digest(bytes)) != pin.digest.as_str() => {
            Err(UnavailableReason::DigestMismatch)
        }
        Some(bytes) => match method_pack::load(bytes) {
            Err(_) => Err(UnavailableReason::InvalidPack),
            Ok(loaded)
                if loaded.id() != pin.pack_id
                    || loaded.version() != pin.version
                    || loaded.id() == BUILTIN_PACK_ID =>
            {
                Err(UnavailableReason::IdentityMismatch)
            }
            Ok(loaded) => Ok(loaded),
        },
    };
    match outcome {
        Ok(loaded) => {
            resolved.status = PackStatus::Available;
            resolved.loaded = Some(loaded);
        }
        Err(reason) => resolved.unavailable_reason = Some(reason),
    }
    resolved
}

pub(crate) fn operations() -> [OperationDescriptor; 1] {
    [OperationDescriptor::active(
        OperationInfo {
            name: ADOPT,
            stage: "frame",
            summary: "Pin one retained method pack revision to the case by digest.",
            when_to_use: "Adopt optional declarative guidance for this case, or move an adopted pack to another revision explicitly.",
            lifecycle: "An active or paused case. Retain the pack file through ingest first.",
            effects: "Revise the case with the pack ID, version, digest, Artifact revision and reason. Earlier case revisions keep their pins.",
            returns: "Case ID and new revision.",
            revision_rule: "Include the current case revision in preconditions. To change a pinned pack, name its pinned digest in replaces.",
            idempotency: "Retry the identical command with its original actor and command ID.",
            repair: "Cite the exact retained Artifact revision, its digest, and the id and version declared in the pack file.",
            related: &["case.amend"],
        },
        json!({
            "artifact_ref": {"id": "art_018d87fc-5234-7000-8000-000000000003", "revision": 1},
            "pack_id": "field-notes",
            "version": "1.0.0",
            "digest": "sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae",
            "reason": "Use the shared field-note guidance for this case."
        }),
        validate_adopt,
        adopt,
    )]
}

fn validate_adopt(payload: &MethodPackAdopt) -> Result<(), Failure> {
    for (value, path) in [
        (&payload.pack_id, "/pack_id"),
        (&payload.version, "/version"),
    ] {
        if value.trim().is_empty() || value.len() > 64 {
            return Err(invalid("Supply the identifier declared in the pack file.").at(path));
        }
    }
    if payload.pack_id == BUILTIN_PACK_ID {
        return Err(
            invalid("The built-in general pack is always available and cannot be pinned.")
                .at("/pack_id"),
        );
    }
    if payload.reason.trim().is_empty() || payload.reason.chars().count() > 2048 {
        return Err(invalid("Provide an attributed reason of 1–2,048 characters.").at("/reason"));
    }
    if payload.replaces.as_ref() == Some(&payload.digest) {
        return Err(invalid("A pack revision cannot replace itself.").at("/replaces"));
    }
    Ok(())
}

fn adopt(payload: MethodPackAdopt, context: &mut OperationContext<'_>) -> Result<(), Failure> {
    let (reference, mut body): (RevisionRef, CaseBody) = case_for_update(context)?;
    if body.lifecycle == CaseLifecycle::Closed {
        return Err(gate("Reopen the case before adopting a method pack."));
    }
    let artifact = context
        .exact(payload.artifact_ref)
        .filter(|record| record.header.id.kind() == Kind::Artifact)
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::UnknownReference,
                "Cite an exact Artifact revision in this case that retains the pack file.",
            )
            .at("/artifact_ref")
        })?;
    let artifact: ArtifactBody = input::decode(&artifact.body).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Stored artifact body has an invalid shape.",
        )
    })?;
    // A read follows the reference's current availability, also when an
    // earlier revision is cited.
    if artifact.availability != ArtifactAvailability::Retained
        || crate::retention_ops::availability_withdrawn(context.state(), payload.artifact_ref.id)
    {
        return Err(gate("The cited Artifact does not retain its bytes.").at("/artifact_ref"));
    }
    if artifact.digest.as_ref() != Some(&payload.digest) {
        let mut failure =
            invalid("The digest does not match the cited Artifact's retained bytes.").at("/digest");
        failure.error.actual = Some(json!(artifact.digest));
        return Err(failure);
    }

    let existing = body
        .adopted_method_packs
        .iter()
        .position(|pin| pin.pack_id == payload.pack_id);
    let replaces = match (
        existing.map(|index| &body.adopted_method_packs[index]),
        &payload.replaces,
    ) {
        (None, None) => None,
        (None, Some(_)) => {
            return Err(
                invalid("The case does not pin this pack yet. Omit replaces.").at("/replaces"),
            );
        }
        (Some(pin), _) if pin.digest == payload.digest => {
            return Err(gate("The case already pins this exact pack revision.").at("/digest"));
        }
        (Some(pin), Some(replaced)) if &pin.digest == replaced => Some(PinnedRevision {
            version: pin.version.clone(),
            digest: pin.digest.clone(),
        }),
        (Some(pin), _) => {
            let mut failure = gate(
                "The case already pins another revision of this pack. Name its digest in replaces to adopt this one.",
            )
            .at("/replaces");
            failure.error.expected = Some(json!(pin.digest));
            return Err(failure);
        }
    };
    let pin = MethodPackPin {
        pack_id: payload.pack_id,
        version: payload.version,
        digest: payload.digest,
        artifact_ref: payload.artifact_ref,
        reason: payload.reason,
        adopted_at_sequence: context.acceptance_sequence(),
        replaces,
    };
    match existing {
        Some(index) => body.adopted_method_packs[index] = pin,
        None => body.adopted_method_packs.push(pin),
    }
    let body = serde_json::to_value(body).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Could not encode accepted record.",
        )
    })?;
    context.update(reference, body)?;
    Ok(())
}

fn invalid(message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message)
}

fn gate(message: &str) -> Failure {
    Failure::new(ErrorCode::GateFailed, message)
}

#[cfg(test)]
mod tests;
