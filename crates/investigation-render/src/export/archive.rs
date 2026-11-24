//! `workspace-archive`: the canonical state, copied so it can be restored
//! and resumed (plan §10.7, §15.4). Pure.
//!
//! ```text
//! DIR/
//!   workspace/investigation.toml                       owner configuration
//!   workspace/state/workspace.json                     identity marker
//!   workspace/state/cases/<case>/genesis.json          every case journal,
//!   workspace/state/cases/<case>/events/<seq>.json     to its verified head
//!   workspace/state/blobs/sha256/<xx>/<hex>            every required blob
//!   human/sha256/<hex>                                 selected human notes
//!   schemas/v1/...                                     the published v1 contracts
//!   archive.json                                       index: heads, versions, counts, digests
//!   export-manifest.json                               every member's digest, written last
//! ```
//!
//! Files below `workspace/` keep their workspace paths, so a restore copies
//! them back and adds only the empty skeleton. There is no workspace-wide
//! head (ADR 0002): the archive's recovery point is every case's verified
//! head, read together under the coordination lock, and the manifest's
//! `snapshot.head_hash` is the recovery head, a digest of that list.
//!
//! An archive is the owner's recovery copy. A hash chain cannot omit an
//! event, so it is all or nothing: it holds restricted records and bytes,
//! is made only for the `restricted` audience, and is refused rather than
//! written incomplete when the inclusion rule withholds any record.

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::{identity::Id, wire::Digest};
use investigation_store::{
    journal::{EVENT_VERSION, hash_bytes},
    store::archive::{CanonicalState, NotRetained},
    workspace::{RECORD_FAMILY, WORKSPACE_VERSION},
};
use serde::{Deserialize, Serialize};

use super::{
    Completeness, ExportError, ExportPlan, ExportRenderer, FRAMEWORK_VERSION, Inclusion, KindSpec,
    MANIFEST_FILE, ManifestPolicy, ManifestSnapshot, POLICY_VERSION, Retained, Statements,
    manifest_parts,
};
use crate::{
    Audience, Generation, PlanBuilder, PlanPath, RenderError, VerifiedSnapshot, ViewPolicy,
    plan::Phase,
};

/// Version of the archive layout and index. Bump it when the bytes planned
/// for the same canonical state change.
pub const ARCHIVE_FORMAT: &str = "inv.workspace-archive.v1";
/// The archive index, beside the manifest.
pub const INDEX_FILE: &str = "archive.json";
/// The folder that holds the canonical files at their workspace paths.
pub const CANONICAL_ROOT: &str = "workspace";
/// The folder that holds selected human notes by digest.
pub const HUMAN_ROOT: &str = "human/sha256";
/// The folder that holds the published v1 contracts.
pub const SCHEMA_ROOT: &str = "schemas/v1";
/// The only audience an archive is made for.
pub const ARCHIVE_AUDIENCE: Audience = Audience::Restricted;

/// The renderer row of the registry. Its planning entry is
/// [`plan_archive`]: an archive is not a view of one case.
pub struct ArchiveRenderer;

impl ExportRenderer for ArchiveRenderer {
    fn version(&self) -> &'static str {
        ARCHIVE_FORMAT
    }

    fn scope(&self) -> super::Scope {
        super::Scope::All
    }

    /// Never called: [`super::plan_export`] refuses kinds of
    /// [`super::Scope::All`] before planning.
    fn plan(
        &self,
        _input: &super::ExportInput<'_>,
        _files: &mut PlanBuilder,
    ) -> Result<(), RenderError> {
        Ok(())
    }
}

/// A human note read from one of the vault's human folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanNote {
    /// Relative to the vault, `/`-separated, for example `Notes/plan.md`.
    pub path: String,
    pub bytes: Vec<u8>,
}

/// A human-folder entry the archive does not copy, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkippedNote {
    pub path: String,
    pub reason: String,
}

/// Everything an archive is planned from, read before planning.
#[derive(Debug, Clone)]
pub struct ArchiveSource {
    pub canonical: CanonicalState,
    /// The bytes of every required blob, checked against its digest.
    pub retained: Retained,
    /// Notes from the human folders, by path.
    pub human_notes: Vec<HumanNote>,
    pub skipped_notes: Vec<SkippedNote>,
    /// The published v1 contracts: file name below `schemas/v1/` and bytes.
    pub schemas: Vec<(String, Vec<u8>)>,
}

/// `archive.json`: what the archive holds, so a reader of the files alone
/// can check and restore it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveIndex {
    /// Always [`ARCHIVE_FORMAT`].
    pub format: String,
    pub workspace_id: Id,
    /// Digest over every case head, in case order; see [`recovery_head`].
    pub recovery_head: Digest,
    pub versions: IndexVersions,
    pub cases: Vec<IndexCase>,
    pub blobs: Vec<IndexBlob>,
    /// Artifacts recorded without retained bytes. Their records are in the
    /// journal; the archive cannot hold bytes the workspace never held.
    pub not_retained: Vec<IndexNotRetained>,
    /// Whether the owner configuration enables the vault.
    pub vault_enabled: bool,
    pub human_notes: Vec<IndexNote>,
    pub human_notes_skipped: Vec<SkippedNote>,
    /// Areas of the source left out on purpose, and why.
    pub excluded: Vec<Excluded>,
    pub statements: ArchiveStatements,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexVersions {
    /// The workspace marker version.
    pub workspace: String,
    pub record_family: String,
    /// The journal event version this program writes. Older events keep
    /// their own version inside their bytes.
    pub event: String,
    pub export_framework: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexCase {
    pub case_id: Id,
    /// The verified head the journal is archived to.
    pub sequence: u64,
    pub head_hash: Digest,
    pub genesis_sha256: Digest,
    pub records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexBlob {
    pub sha256: Digest,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexNotRetained {
    pub case_id: Id,
    pub artifact: Id,
    /// `external` (reference-only), `partial`, `missing` or
    /// `intentionally_removed`.
    pub availability: String,
    pub sha256: Option<Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexNote {
    /// Where the note lives, relative to the vault.
    pub path: String,
    pub sha256: Digest,
    pub bytes: u64,
    /// The archive member that holds its bytes.
    pub member: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Excluded {
    /// A workspace path; a trailing `/` names a folder.
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveStatements {
    pub recovery: String,
    pub disclosure: String,
    pub encryption: String,
    pub human_notes: String,
    pub restore: String,
}

/// The digest that names an archive's recovery point: the workspace and
/// every case head, in case ID order. ADR 0002 gives no order across cases,
/// so this is a set of heads, not a global sequence.
pub fn recovery_head<'a>(
    workspace_id: Id,
    heads: impl IntoIterator<Item = (Id, u64, &'a Digest)>,
) -> Digest {
    let mut text = format!("inv.recovery-head.v1\nworkspace {workspace_id}\n");
    for (case_id, sequence, head_hash) in heads {
        text.push_str(&format!("case {case_id} {sequence} {head_hash}\n"));
    }
    hash_bytes(text.as_bytes())
}

/// The member that holds a human note's bytes.
pub fn human_member(digest: &Digest) -> String {
    format!(
        "{HUMAN_ROOT}/{}",
        digest.as_str().trim_start_matches("sha256:")
    )
}

/// The member that holds a canonical file at workspace path `path`.
pub fn canonical_member(path: &str) -> String {
    format!("{CANONICAL_ROOT}/{path}")
}

/// The areas an archive leaves out on purpose.
pub fn excluded() -> Vec<Excluded> {
    [
        ("cache/", "Disposable: the index, recorded blob verdicts and kept journal heads are rebuilt from the journal (repair index) or recomputed."),
        ("local/", "Machine-local and not canonical: session read records, the render record, render conflicts and the stopped-writes marker belong to this machine."),
        ("state/tmp/", "Publication temporaries of interrupted writes; never canonical."),
        ("state/staging/", "Capture staging: an archive is refused while any holds evidence the journal does not reference; settled staging is already copied as canonical blobs."),
        ("state/locks/ and state/cases/*/commit.lock", "Empty lock files; a restore creates new ones."),
        ("vault/ (generated notes)", "Derived from the journal; vault render writes them again. Only the human folders are copied, as human notes."),
        ("exports/", "Earlier exports, which are copies of what this archive holds or views of it."),
        ("README.txt", "The pointer file; a restore writes it again."),
    ]
    .into_iter()
    .map(|(path, reason)| Excluded {
        path: path.to_owned(),
        reason: reason.to_owned(),
    })
    .collect()
}

fn statements() -> ArchiveStatements {
    ArchiveStatements {
        recovery: "Every case journal is copied to the verified head named here, with every blob it requires, the workspace identity and the owner configuration. Restoring them gives the same records, receipts and command IDs.".to_owned(),
        disclosure: "Made for the restricted audience: this archive holds every record and retained byte, restricted ones included. Share it only with people allowed to see all of it.".to_owned(),
        encryption: "Not encrypted. Files are owner-only (0600). Encrypt the folder with a reviewed external tool before it leaves this machine.".to_owned(),
        human_notes: "Human notes change outside the journal. They are copied as separate inputs, read when the archive was made, and are not covered by the case heads.".to_owned(),
        restore: "Verify with investigation archive verify DIR before restoring: it checks every member, the chains and the blobs, and restores a private copy to run check.".to_owned(),
    }
}

/// Plan the archive of `source` for `audience`. Pure: the same canonical
/// state, notes and contracts give byte-identical files and manifest.
pub fn plan_archive(
    kind: &'static KindSpec,
    source: &ArchiveSource,
    audience: Audience,
) -> Result<ExportPlan, ExportError> {
    if audience != ARCHIVE_AUDIENCE {
        return Err(ExportError::ArchiveAudience {
            required: ARCHIVE_AUDIENCE.as_str(),
        });
    }
    let canonical = &source.canonical;
    let snapshot = VerifiedSnapshot::from_replays(
        canonical.workspace_id,
        canonical
            .cases
            .iter()
            .map(|case| (case.case_id, case.replay.clone())),
    );
    // Every record of every case must be shown to the audience, and shown
    // as recorded: an archive is never incomplete and never caveated. Under
    // policy v2 the restricted audience is above every classification, so
    // nothing is withheld and no caveat arises (a caveat only marks a record
    // bearing on withheld material). Both are still checked, from the same
    // inclusion decisions every other kind uses, so a later policy that
    // withholds anything at restricted refuses the archive instead of
    // writing a journal copy the manifest would misdescribe.
    let mut exclusions = Vec::new();
    let mut caveated = Vec::new();
    let mut sources = Vec::new();
    let mut supersession = Vec::new();
    for case in snapshot.cases() {
        let inclusion = Inclusion::new(case.state(), None, audience);
        exclusions.extend(inclusion.exclusions());
        caveated.extend(inclusion.caveated());
        let (case_sources, case_supersession) =
            manifest_parts(case.state(), |record| inclusion.includes(record));
        sources.extend(case_sources);
        supersession.extend(case_supersession);
    }
    if !exclusions.is_empty() {
        return Err(ExportError::Incomplete {
            exclusions,
            omissions: Vec::new(),
        });
    }
    if !caveated.is_empty() {
        return Err(ExportError::Caveated { caveated });
    }

    let policy = ViewPolicy::for_audience(audience);
    let renderer = &ArchiveRenderer;
    let generation = Generation::new(renderer.version(), &snapshot, &policy);
    let mut files = PlanBuilder::new(generation);
    let mut add = |phase, path: String, bytes: &[u8]| -> Result<(), ExportError> {
        files.add(phase, PlanPath::new(path)?, bytes.to_vec(), None)?;
        Ok(())
    };
    add(
        Phase::Records,
        canonical_member(investigation_store::store::archive::CONFIG_FILE),
        &canonical.config,
    )?;
    add(
        Phase::Records,
        canonical_member(investigation_store::store::archive::MARKER_FILE),
        &canonical.marker,
    )?;
    let mut cases = Vec::with_capacity(canonical.cases.len());
    for case in &canonical.cases {
        add(
            Phase::Records,
            canonical_member(&case.genesis_path()),
            &case.genesis,
        )?;
        for (offset, bytes) in case.events.iter().enumerate() {
            add(
                Phase::Records,
                canonical_member(&case.event_path(offset as u64 + 1)),
                bytes,
            )?;
        }
        cases.push(IndexCase {
            case_id: case.case_id,
            sequence: case.replay.snapshot.sequence,
            head_hash: case.replay.snapshot.head_hash.clone(),
            genesis_sha256: hash_bytes(&case.genesis),
            records: case.replay.state.records().count() as u64,
        });
    }
    let mut blobs = Vec::with_capacity(canonical.blobs.len());
    for blob in &canonical.blobs {
        let bytes =
            source
                .retained
                .get(&blob.digest)
                .ok_or_else(|| ExportError::MissingRetained {
                    digest: blob.digest.clone(),
                })?;
        add(Phase::Records, canonical_member(&blob.path()), bytes)?;
        blobs.push(IndexBlob {
            sha256: blob.digest.clone(),
            bytes: bytes.len() as u64,
        });
    }
    let mut human_notes = Vec::with_capacity(source.human_notes.len());
    let mut copied = BTreeSet::new();
    let mut notes: Vec<&HumanNote> = source.human_notes.iter().collect();
    notes.sort_by(|left, right| left.path.cmp(&right.path));
    for note in notes {
        let digest = hash_bytes(&note.bytes);
        let member = human_member(&digest);
        if copied.insert(digest.clone()) {
            add(Phase::Records, member.clone(), &note.bytes)?;
        }
        human_notes.push(IndexNote {
            path: note.path.clone(),
            sha256: digest,
            bytes: note.bytes.len() as u64,
            member,
        });
    }
    let schemas: BTreeMap<&str, &[u8]> = source
        .schemas
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    for (name, bytes) in schemas {
        add(Phase::Views, format!("{SCHEMA_ROOT}/{name}"), bytes)?;
    }
    let head = recovery_head(
        canonical.workspace_id,
        canonical.cases.iter().map(|case| {
            let head = &case.replay.snapshot;
            (case.case_id, head.sequence, &head.head_hash)
        }),
    );
    let mut skipped = source.skipped_notes.clone();
    skipped.sort_by(|left, right| left.path.cmp(&right.path));
    let index = ArchiveIndex {
        format: ARCHIVE_FORMAT.to_owned(),
        workspace_id: canonical.workspace_id,
        recovery_head: head.clone(),
        versions: IndexVersions {
            workspace: version_text(&WORKSPACE_VERSION),
            record_family: version_text(&RECORD_FAMILY),
            event: EVENT_VERSION.to_owned(),
            export_framework: FRAMEWORK_VERSION.to_owned(),
        },
        cases,
        blobs,
        not_retained: canonical.not_retained.iter().map(not_retained).collect(),
        vault_enabled: canonical.vault_enabled,
        human_notes,
        human_notes_skipped: skipped,
        excluded: excluded(),
        statements: statements(),
    };
    let mut index_bytes = serde_json::to_vec_pretty(&index).expect("archive index serializes");
    index_bytes.push(b'\n');
    add(Phase::Entry, INDEX_FILE.to_owned(), &index_bytes)?;
    let files = files.finish();
    if files.entry(MANIFEST_FILE).is_some() {
        return Err(RenderError::DuplicatePath(MANIFEST_FILE.to_owned()).into());
    }
    let snapshot = ManifestSnapshot {
        case_id: canonical.workspace_id,
        sequence: 0,
        head_hash: head,
    };
    Ok(super::finish_plan(
        kind,
        renderer,
        files,
        snapshot,
        ManifestPolicy {
            version: POLICY_VERSION.to_owned(),
            audience: audience.as_str().to_owned(),
            placeholders: false,
        },
        (sources, supersession),
        Completeness::Complete,
        Statements::archive(),
        super::Withheld {
            exclusions: Vec::new(),
            omissions: Vec::new(),
            caveated: Vec::new(),
        },
    ))
}

fn not_retained(entry: &NotRetained) -> IndexNotRetained {
    IndexNotRetained {
        case_id: entry.case_id,
        artifact: entry.artifact,
        availability: version_text(&entry.availability),
        sha256: entry.sha256.clone(),
    }
}

/// The serialized string of a unit enum value.
fn version_text(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
