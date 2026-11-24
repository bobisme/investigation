//! The audience-facing export manifest, `inv.export-manifest.v1`. Pure.
//!
//! It names what the export was rendered from and under which rules, and
//! lists every other file of the export with its digest. It carries no time,
//! no local path, no workspace identity and nothing the audience policy
//! withholds; exact exclusions go to the publisher's receipt instead. When
//! IDs are remapped, its snapshot names the bundle rather than the case
//! journal, and the mapping back goes to the publisher too.

use investigation_core::{command::RevisionRef, identity::Id, wire::Digest};
use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

use super::{IdentifierMode, Product, RecordTimes, Shortfall, dependent_rule};
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
    /// What the export was rendered from: the exact verified case snapshot;
    /// for a workspace archive, the workspace and its recovery head; when
    /// `identifiers` is `remapped`, the opaque identity of this bundle, with
    /// the mapping to the case snapshot kept by the publisher.
    pub snapshot: ManifestSnapshot,
    /// `canonical`: record IDs are as recorded and embed their creation
    /// time, and `snapshot` is canonical (a case head, or for a workspace
    /// archive the recovery head). `remapped`: every UUIDv7 value in the
    /// export, file names included, is an opaque value assigned for this
    /// export only, and `snapshot` names the bundle. Absent from manifests
    /// written before `inv.export.v2`, which were canonical.
    pub identifiers: Option<IdentifierMode>,
    pub policy: ManifestPolicy,
    /// `incomplete` when the audience policy withheld any record of the
    /// case, or the kind left out any content beyond record inclusion (a
    /// lesson, matrix row, gap or attachment resting on what it may not
    /// show, or bytes it could not copy exactly). An export that left
    /// anything out is never labelled complete; `statements.completeness`
    /// says which, without naming or counting what.
    pub completeness: Completeness,
    /// Digests of the retained artifacts the export includes.
    pub sources: Vec<SourceDigest>,
    /// Supersession links between included records, so a superseded record
    /// is marked rather than rewritten. When `identifiers` is `remapped`,
    /// each reference here and in `sources` names the remapped ID at the
    /// record's own revision, as the export's files cite it.
    pub supersession: Vec<Supersession>,
    /// Every file of the export except this manifest, in publish order.
    pub files: Vec<FileDigest>,
    pub statements: Statements,
}

/// The versions that determine the bytes of an export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManifestVersions {
    /// The shared export pipeline (`inv.export.v2`).
    pub framework: String,
    /// The kind's renderer, bumped whenever its bytes change.
    pub renderer: String,
    pub manifest_schema: String,
    /// The freshness rules that computed effective states.
    pub freshness_rules: u32,
}

/// The snapshot an export names. `sequence` is 0 in two cases, told apart
/// by the manifest's `kind` and `identifiers`: a workspace archive (kind
/// `workspace-archive`, identifiers `canonical`), which has no
/// workspace-wide sequence; and a remapped export (identifiers
/// `remapped`), whose sequence is not disclosed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManifestSnapshot {
    /// The case. For a workspace archive, which covers every case, the
    /// workspace ID. When remapped, the case's remapped ID.
    pub case_id: Id,
    /// The case journal sequence of the verified head. 0 for a workspace
    /// archive: there is no workspace-wide sequence, and `head_hash` is the
    /// recovery head over every case head. 0 when remapped: the sequence is
    /// not disclosed, and `head_hash` is an opaque bundle digest.
    pub sequence: u64,
    /// The verified head hash; the recovery head for a workspace archive;
    /// an opaque bundle digest when remapped.
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
    /// The statements of an export of a case view. `shortfall` picks the
    /// completeness sentence; it names, counts and describes nothing.
    pub fn fixed(shortfall: Shortfall, audience: Audience, mode: IdentifierMode) -> Self {
        Self {
            rendering: match mode {
                IdentifierMode::Canonical => "Rendered deterministically from the exact verified snapshot named here, with no model call. The same snapshot, kind, renderer and policy give the same bytes.",
                IdentifierMode::Remapped => "Rendered deterministically from one exact verified snapshot, which the publisher can name, with no model call. The same snapshot, kind, renderer and policy give the same bytes.",
            }
            .to_owned(),
            evidence: "This export is a view of the case record. It does not replace the primary evidence it cites; negative and inconclusive results are kept, and superseded records are marked, not rewritten.".to_owned(),
            completeness: match shortfall {
                Shortfall::None => "Nothing in the case was withheld from this audience.",
                Shortfall::Withheld => "Incomplete: the audience policy withheld material from this export, and content resting on it, or that this export could not copy exactly, may be left out too. What was withheld or left out is not named here.",
                Shortfall::LeftOut => "Incomplete: nothing in the case was withheld from this audience, but this export left out content it could not show or copy exactly. What was left out is not named here.",
            }
            .to_owned(),
            disclosure: format!(
                "Disclosure follows the audience policy and classifications in force when the export was made, also for an earlier snapshot. {}",
                dependent_rule(audience)
            ),
            identifiers: match mode {
                IdentifierMode::Canonical => "Record IDs are UUIDv7 values and embed their creation time to the millisecond.",
                IdentifierMode::Remapped => "Record IDs were remapped for this export only: every UUIDv7 value in it, file names included, is an opaque value whose time field carries no creation time, and one record has one value throughout. The snapshot names this bundle, not the case journal, and discloses no event count; the publisher keeps the mapping. A remapped export is a review product and cannot restore canonical state.",
            }
            .to_owned()
                + &RecordTimes::for_export(audience, mode)
                    .statement()
                    .map(|sentence| format!(" {sentence}"))
                    .unwrap_or_default(),
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
