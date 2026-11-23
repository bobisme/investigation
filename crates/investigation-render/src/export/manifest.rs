//! The audience-facing export manifest, `inv.export-manifest.v1`. Pure.
//!
//! It names what the export was rendered from and under which rules, and
//! lists every other file of the export with its digest. It carries no time,
//! no local path, no workspace identity and nothing the audience policy
//! withholds; exact exclusions go to the publisher's receipt instead.

use investigation_core::{command::RevisionRef, identity::Id, wire::Digest};
use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

use super::{Product, dependent_rule};
use crate::Audience;

/// Schema version of [`ExportManifest`].
pub const MANIFEST_SCHEMA: &str = "inv.export-manifest.v1";

/// The manifest written as `export-manifest.json` in every export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportManifest {
    /// Always `inv.export-manifest.v1`.
    pub schema_version: String,
    /// `exp_<sha256 hex>` over the kind, versions, policy, snapshot,
    /// completeness and file digests: identical exports share it.
    pub export_id: String,
    /// The export kind, as given to `export --kind`.
    pub kind: String,
    pub product: Product,
    /// Whether the export can restore canonical state. Only a workspace
    /// archive can; reports, review exports and bundles cannot.
    pub resumable: bool,
    /// The program that rendered the export.
    pub generator: String,
    pub versions: ManifestVersions,
    /// The exact verified case snapshot the export was rendered from.
    pub snapshot: ManifestSnapshot,
    pub policy: ManifestPolicy,
    /// `incomplete` when the audience policy withheld any record of the
    /// case. An incomplete export is never labelled complete.
    pub completeness: Completeness,
    /// Digests of the retained artifacts the export includes.
    pub sources: Vec<SourceDigest>,
    /// Supersession links between included records, so a superseded record
    /// is marked rather than rewritten.
    pub supersession: Vec<Supersession>,
    /// Every file of the export except this manifest, in publish order.
    pub files: Vec<FileDigest>,
    pub statements: Statements,
}

/// The versions that determine the bytes of an export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManifestVersions {
    /// The shared export pipeline (`inv.export.v1`).
    pub framework: String,
    /// The kind's renderer, bumped whenever its bytes change.
    pub renderer: String,
    pub manifest_schema: String,
    /// The freshness rules that computed effective states.
    pub freshness_rules: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManifestSnapshot {
    /// The case. For a workspace archive, which covers every case, the
    /// workspace ID.
    pub case_id: Id,
    /// The case journal sequence of the verified head. For a workspace
    /// archive 0: there is no workspace-wide sequence, and `head_hash` is
    /// the recovery head over every case head.
    pub sequence: u64,
    pub head_hash: Digest,
}

/// The audience policy the export was built under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManifestPolicy {
    /// The audience policy version: `inv.export-policy.v2` is positive
    /// inclusion by effective classification at the pinned and the current
    /// revision, plus fixed caveats (internal, restricted) or withholding
    /// (public) for records that rest on or are challenged by withheld
    /// material.
    pub version: String,
    /// `public`, `internal` or `restricted`.
    pub audience: String,
    /// Whether withheld records appear as ID-only placeholders. Always
    /// false under this policy version.
    pub placeholders: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    Incomplete,
}

impl Completeness {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceDigest {
    /// The Artifact revision at the pinned snapshot.
    pub artifact: RevisionRef,
    /// The recorded digest of its retained bytes.
    pub sha256: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Supersession {
    pub record: RevisionRef,
    pub supersedes: Option<RevisionRef>,
    pub superseded_by: Option<RevisionRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileDigest {
    /// Relative to the export directory, `/`-separated.
    pub path: String,
    pub sha256: Digest,
    pub bytes: u64,
}

/// Fixed statements every export carries, so a reader of the files alone
/// knows what they are and are not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Statements {
    pub rendering: String,
    pub evidence: String,
    pub completeness: String,
    pub disclosure: String,
    pub identifiers: String,
    pub distribution: String,
}

impl Statements {
    pub fn fixed(completeness: Completeness, audience: Audience) -> Self {
        Self {
            rendering: "Rendered deterministically from the exact verified snapshot named here, with no model call. The same snapshot, kind, renderer and policy give the same bytes.".to_owned(),
            evidence: "This export is a view of the case record. It does not replace the primary evidence it cites; negative and inconclusive results are kept, and superseded records are marked, not rewritten.".to_owned(),
            completeness: match completeness {
                Completeness::Complete => "Nothing in the case was withheld from this audience.",
                Completeness::Incomplete => "Incomplete: the audience policy withheld material from this export. What was withheld is not named here.",
            }
            .to_owned(),
            disclosure: format!(
                "Disclosure follows the audience policy and classifications in force when the export was made, also for an earlier snapshot. {}",
                dependent_rule(audience)
            ),
            identifiers: "Record IDs are UUIDv7 values and embed their creation time to the millisecond.".to_owned(),
            distribution: "Written to a local directory only. Publishing, sending or uploading it is a separate action.".to_owned(),
        }
    }

    /// The statements of a workspace archive, which is the record itself
    /// rather than a view of it.
    pub fn archive() -> Self {
        Self {
            rendering: "Copied byte for byte from the verified case heads named in archive.json, with no model call. The same canonical state, human notes and program give the same bytes.".to_owned(),
            evidence: "This archive holds the canonical journal and retained bytes themselves. Records are restored exactly as recorded, history included.".to_owned(),
            completeness: "Complete: every case, record and retained byte of the workspace at these heads. Bytes recorded as held elsewhere or removed are listed in archive.json.".to_owned(),
            disclosure: "Made for the restricted audience: it includes restricted records and bytes. The snapshot names the workspace, sequence 0 and the recovery head over every case head.".to_owned(),
            identifiers: "Record IDs are UUIDv7 values and embed their creation time to the millisecond.".to_owned(),
            distribution: "Written to a local directory only, owner-only and unencrypted. Publishing, sending, uploading or encrypting it is a separate action.".to_owned(),
        }
    }
}

/// JSON Schema of [`ExportManifest`], published as the `export-manifest`
/// wire schema.
pub fn manifest_schema() -> Schema {
    schema_for!(ExportManifest)
}
