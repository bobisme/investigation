//! `capture.attach`: snapshot a completed artifact into staging.
//!
//! The local recorder copies the file into the handle's staging while hashing,
//! reads it again to confirm it did not change, checks any declared digest and
//! size, and only then accepts the descriptor at its position. A refusal stages
//! nothing and leaves the position free.

use std::path::PathBuf;

use investigation_core::{
    header::{ClassificationLevel, SystemClock},
    wire::{Digest, ErrorCode, Failure},
};
use investigation_store::{
    Store, Workspace,
    store::capture::{
        attach::{AttachAvailability, AttachOutcome, AttachSource, AttachSubmission},
        items::{ItemAcceptance, ItemOperation, ItemSubmission},
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    append::{ItemReceipt, item_receipt},
    error::RecordingError,
    key::{CaptureHandle, IdempotencyKey, Metadata, Position},
};

/// Descriptor of one completed artifact at the next position of a handle.
///
/// "Stable" is a producer obligation: the writer has closed and flushed the file.
/// The recorder still copies and verifies the bytes and rejects a file that
/// changes during the copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct StableArtifact {
    /// Handle returned by `begin`.
    pub handle: CaptureHandle,
    /// Producer item key, unique within the handle.
    pub key: IdempotencyKey,
    /// The position the producer expects this item to take.
    pub position: Position,
    /// What the artifact is to the Run.
    pub role: ArtifactRole,
    /// Where the bytes are, or why they are absent.
    pub source: ArtifactSource,
    /// Channel the artifact belongs to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Earlier item this artifact derives from, for derivatives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<Derivation>,
    /// Media type hint. It is never used to choose a parser automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Digest the producer expects; a mismatch rejects the item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_digest: Option<Digest>,
    /// Size the producer expects; a mismatch rejects the item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_size_bytes: Option<u64>,
    /// Classification; it may be stricter than the handle default, never looser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<ClassificationLevel>,
    /// Inert namespaced producer metadata.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub metadata: Metadata,
}

impl StableArtifact {
    /// A closed file to copy into staging.
    pub fn file(
        handle: CaptureHandle,
        key: IdempotencyKey,
        position: Position,
        role: ArtifactRole,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self::with_source(
            handle,
            key,
            position,
            role,
            ArtifactSource::File { path: path.into() },
        )
    }

    /// A declared artifact the producer could not supply. It stays missing; it is
    /// never replaced by an empty file.
    pub fn missing(
        handle: CaptureHandle,
        key: IdempotencyKey,
        position: Position,
        role: ArtifactRole,
        reason: impl Into<String>,
    ) -> Self {
        Self::with_source(
            handle,
            key,
            position,
            role,
            ArtifactSource::Missing {
                reason: reason.into(),
            },
        )
    }

    fn with_source(
        handle: CaptureHandle,
        key: IdempotencyKey,
        position: Position,
        role: ArtifactRole,
        source: ArtifactSource,
    ) -> Self {
        Self {
            handle,
            key,
            position,
            role,
            source,
            channel: None,
            derived_from: None,
            media_type: None,
            expected_digest: None,
            expected_size_bytes: None,
            classification: None,
            metadata: Metadata::new(),
        }
    }
}

/// Role of an artifact within the Run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ArtifactRole {
    /// Original output of the external action.
    OriginalOutput,
    /// Derived from another item; set [`StableArtifact::derived_from`].
    Derivative,
    /// A log of the producer or the action.
    Log,
    /// An external receipt, such as a signed execution statement.
    Receipt,
}

/// Where an artifact's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum ArtifactSource {
    /// A closed file the recorder copies and verifies.
    File {
        /// Path to the file. The implementation defines which roots it may read.
        /// [`LocalRecorder`](crate::LocalRecorder) reads only beneath the
        /// workspace's configured `import_roots`: a relative path is tried
        /// beneath each root in order, and an absolute path must lie beneath
        /// one. No symlink is followed, and a file with more than one hard link
        /// is refused. A path that no root holds is recorded as missing.
        path: PathBuf,
    },
    /// The artifact was declared but is absent.
    Missing {
        /// Why it is absent, as reported.
        reason: String,
    },
}

/// Lineage of a derived artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Derivation {
    /// Item key of the original on the same handle.
    pub from: IdempotencyKey,
    /// Transform name and version, such as `text-extract/1`.
    pub transform: String,
}

impl Derivation {
    /// Lineage from an earlier item through a named transform.
    pub fn new(from: IdempotencyKey, transform: impl Into<String>) -> Self {
        Self {
            from,
            transform: transform.into(),
        }
    }
}

/// Whether staged bytes exist for an accepted artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StagedAvailability {
    /// The verified bytes are durable in staging.
    Staged,
    /// The artifact is recorded as missing.
    Missing,
}

/// Durable receipt for an accepted artifact descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ArtifactRef {
    /// Item receipt at the artifact's position.
    pub receipt: ItemReceipt,
    /// Accepted role.
    pub role: ArtifactRole,
    /// Staged or missing.
    pub availability: StagedAvailability,
    /// Digest of the retained bytes, when staged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
    /// Size of the retained bytes, when staged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

const MAX_NAME_CHARS: usize = 256;
const MAX_REASON_CHARS: usize = 4096;
const MAX_MEDIA_TYPE_CHARS: usize = 255;
const MAX_PATH_BYTES: usize = 4096;

/// Copy one artifact into staging and accept its descriptor at the next
/// position of its handle, or record it as missing.
pub(crate) fn attach(
    workspace: &Workspace,
    artifact: &StableArtifact,
) -> Result<ArtifactRef, RecordingError> {
    validate(artifact)?;
    // The normalized form, so an identical retry compares equal however its
    // JSON was spelled.
    let request = serde_json::to_value(artifact).map_err(|_| {
        Failure::new(
            ErrorCode::InvalidInput,
            "Could not encode the attach request; use a UTF-8 path.",
        )
        .at("/source/path")
    })?;
    let source = match &artifact.source {
        ArtifactSource::File { path } => AttachSource::File(path.clone()),
        ArtifactSource::Missing { .. } => AttachSource::Missing,
    };
    let accepted = Store::open(workspace.clone()).stage_capture_attachment(
        &AttachSubmission {
            item: ItemSubmission {
                operation: ItemOperation::Attach,
                handle: artifact.handle.id(),
                key: artifact.key.as_str().to_owned(),
                position: artifact.position.get(),
                request,
                outcome: None,
            },
            source,
            expected_digest: artifact.expected_digest.clone(),
            expected_size_bytes: artifact.expected_size_bytes,
            classification: artifact.classification,
            derived_from: artifact
                .derived_from
                .as_ref()
                .map(|derivation| derivation.from.as_str().to_owned()),
        },
        &mut SystemClock,
    )?;
    Ok(artifact_ref(&accepted)?)
}

/// The receipt of a staged attachment, from what was durably accepted.
fn artifact_ref(accepted: &ItemAcceptance) -> Result<ArtifactRef, Failure> {
    let receipt = item_receipt(accepted)?;
    let invalid = || {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "A staged capture attachment does not hold a valid receipt.",
        )
    };
    let outcome = AttachOutcome::of(&accepted.item)?.ok_or_else(invalid)?;
    let role = accepted
        .item
        .request
        .get("role")
        .cloned()
        .and_then(|role| serde_json::from_value(role).ok())
        .ok_or_else(invalid)?;
    Ok(ArtifactRef {
        receipt,
        role,
        availability: match outcome.availability {
            AttachAvailability::Staged => StagedAvailability::Staged,
            AttachAvailability::Missing => StagedAvailability::Missing,
        },
        digest: outcome.digest,
        size_bytes: outcome.size_bytes,
    })
}

fn validate(artifact: &StableArtifact) -> Result<(), Failure> {
    let invalid = |path: &str, message: String| {
        Failure::new(ErrorCode::InvalidInput, message).at(path.to_owned())
    };
    let name = |path: &str, value: &str| {
        if value.trim().is_empty()
            || value.chars().count() > MAX_NAME_CHARS
            || value.chars().any(char::is_control)
        {
            Err(invalid(
                path,
                format!("Use 1 to {MAX_NAME_CHARS} characters without control characters."),
            ))
        } else {
            Ok(())
        }
    };
    match (&artifact.role, &artifact.derived_from) {
        (ArtifactRole::Derivative, None) => {
            return Err(invalid(
                "/derived_from",
                "A derivative names the item it derives from and the transform. Set derived_from, or use another role.".into(),
            ));
        }
        (ArtifactRole::Derivative, Some(derivation)) => {
            if derivation.from == artifact.key {
                return Err(invalid(
                    "/derived_from/from",
                    "A derivative cannot derive from itself.".into(),
                ));
            }
            name("/derived_from/transform", &derivation.transform)?;
        }
        (_, Some(_)) => {
            return Err(invalid(
                "/derived_from",
                "Only a derivative names derived_from. Use role derivative, or omit derived_from."
                    .into(),
            ));
        }
        (_, None) => {}
    }
    match &artifact.source {
        ArtifactSource::File { path } => {
            let bytes = path.as_os_str().len();
            if bytes == 0 || bytes > MAX_PATH_BYTES {
                return Err(invalid(
                    "/source/path",
                    format!("Use a path of 1 to {MAX_PATH_BYTES} bytes."),
                ));
            }
        }
        ArtifactSource::Missing { reason } => {
            if reason.trim().is_empty() || reason.chars().count() > MAX_REASON_CHARS {
                return Err(invalid(
                    "/source/reason",
                    format!("Use 1 to {MAX_REASON_CHARS} characters."),
                ));
            }
        }
    }
    if let Some(channel) = &artifact.channel {
        name("/channel", channel)?;
    }
    if let Some(media_type) = &artifact.media_type {
        let shaped = media_type
            .split_once('/')
            .is_some_and(|(kind, rest)| !kind.is_empty() && !rest.is_empty());
        if !shaped
            || media_type.len() > MAX_MEDIA_TYPE_CHARS
            || !media_type
                .chars()
                .all(|c| c.is_ascii() && !c.is_ascii_control())
        {
            return Err(invalid(
                "/media_type",
                format!(
                    "Use a media type such as application/json: up to {MAX_MEDIA_TYPE_CHARS} printable ASCII characters. It is a hint only."
                ),
            ));
        }
    }
    investigation_store::opaque::validate_metadata(&artifact.metadata)
        .map_err(|(message, key)| invalid(&format!("/metadata{key}"), message))
}
