//! `capture.attach`: snapshot a completed artifact into staging.

use std::path::PathBuf;

use investigation_core::{header::ClassificationLevel, wire::Digest};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    append::ItemReceipt,
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
