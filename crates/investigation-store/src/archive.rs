//! What a workspace archive copies: the canonical state, read and verified
//! (plan §10.7, §15.4).
//!
//! [`Store::read_canonical`] verifies every case journal in full under the
//! coordination lock, as `check` does, and returns each case's genesis and
//! event bytes up to its verified head, the workspace marker, the owner
//! configuration and the digests of every blob the journals require. Per
//! ADR 0002 there is no workspace-wide head: the recovery point is the set
//! of per-case heads read together under the lock.
//!
//! Blob bytes are read separately, after the lock is released, with
//! [`Store::retained_bytes`]; the caller checks them against their digests.
//! Before the lock is released the read creates an [`ArchivePin`] naming
//! the heads and every required blob (W11.09, see [`super::pins`]): a
//! reclamation cannot remove a pinned blob while the pin is held, and the
//! pin ends when the caller drops or releases it, or when the process
//! dies. A blob that vanished or changed still fails the archive rather
//! than being skipped. The pin record, under `local/`, is the only write.
//!
//! [`restore_layout`] creates the non-canonical skeleton (directories, empty
//! lock files, the pointer file) that a directory of restored canonical
//! files needs before [`Workspace::open`] accepts it.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
};

use investigation_core::{
    evidence::{ArtifactAvailability, ArtifactBody},
    identity::{Id, Kind},
    wire::{Digest, ErrorCode, Failure},
};
use serde::Serialize;

use super::{
    BLOBS, CASE_LOCK, CaseHead, CheckReport, DurableFs, ProjectionStatus, Store, case_directory,
    integrity, io_failure, pins::ArchivePin,
};
use crate::{
    journal::{CaseReplay, EVENTS_DIRECTORY, GENESIS_FILENAME, event_filename, hash_bytes},
    workspace::{self, POINTER_FILE, WorkspaceError},
};

/// The workspace marker, relative to the workspace root.
pub const MARKER_FILE: &str = "state/workspace.json";
/// The owner configuration, relative to the workspace root.
pub const CONFIG_FILE: &str = "investigation.toml";
/// Largest human note an archive copies; a larger one is listed as skipped.
pub const MAX_HUMAN_NOTE_BYTES: u64 = 16 << 20;
/// Most human notes one archive copies; the rest are listed as skipped.
pub const MAX_HUMAN_NOTES: usize = 10_000;
/// Largest person-owned vault file `vault snapshot-note` retains; a larger
/// one is refused. The archive's per-note bound, so every file a record can
/// rely on is one an archive would also copy.
pub const MAX_NOTE_SNAPSHOT_BYTES: u64 = MAX_HUMAN_NOTE_BYTES;
/// Largest manifest or index `archive verify` reads.
pub const MAX_INDEX_BYTES: u64 = 64 << 20;
/// The coordination lock, relative to the workspace root.
const COORDINATION_LOCK: &str = "state/locks/commit.lock";

/// One case journal at its verified head, with the exact bytes it was
/// replayed from.
#[derive(Debug, Clone)]
pub struct CaseJournal {
    pub case_id: Id,
    pub replay: CaseReplay,
    /// `genesis.json`; its hash is the parent of event 1.
    pub genesis: Vec<u8>,
    /// Event files from sequence 1 to the head, in order. Each one hashes to
    /// the verified hash of its event.
    pub events: Vec<Vec<u8>>,
}

impl CaseJournal {
    /// Path of `genesis.json` relative to the workspace root.
    pub fn genesis_path(&self) -> String {
        format!(
            "{}/{GENESIS_FILENAME}",
            case_directory(self.case_id).display()
        )
    }

    /// Path of event `sequence` relative to the workspace root.
    pub fn event_path(&self, sequence: u64) -> String {
        let name = event_filename(sequence).expect("a verified event has a valid sequence");
        format!(
            "{}/{EVENTS_DIRECTORY}/{name}",
            case_directory(self.case_id).display()
        )
    }
}

/// A blob some journal requires: cited by an event (attachments, a manifest)
/// or held by a retained Artifact, with every size recorded for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredBlob {
    pub digest: Digest,
    pub sizes: BTreeSet<u64>,
}

impl RequiredBlob {
    /// Path of the blob relative to the workspace root.
    pub fn path(&self) -> String {
        blob_path(&self.digest)
    }
}

/// Path of the blob with `digest` relative to the workspace root.
pub fn blob_path(digest: &Digest) -> String {
    let hex = digest.as_str().trim_start_matches("sha256:");
    format!("{BLOBS}/{}/{hex}", &hex[..2])
}

/// An Artifact whose bytes the workspace does not hold: reference-only
/// (`external`), `partial`, `missing` or `intentionally_removed`. An archive
/// records it; it cannot restore bytes the workspace never had.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotRetained {
    pub case_id: Id,
    pub artifact: Id,
    pub availability: ArtifactAvailability,
    /// The recorded digest, when one was recorded.
    pub sha256: Option<Digest>,
}

/// The canonical state of a workspace, verified, at one head per case.
#[derive(Debug, Clone)]
pub struct CanonicalState {
    pub workspace_id: Id,
    /// The bytes of [`MARKER_FILE`].
    pub marker: Vec<u8>,
    /// The bytes of [`CONFIG_FILE`].
    pub config: Vec<u8>,
    /// Whether the configuration enables the vault.
    pub vault_enabled: bool,
    /// Every case, in ID order.
    pub cases: Vec<CaseJournal>,
    /// Every required blob, in digest order.
    pub blobs: Vec<RequiredBlob>,
    /// Artifacts without retained bytes, in case and ID order.
    pub not_retained: Vec<NotRetained>,
}

impl CanonicalState {
    /// The verified head of every case, in case ID order.
    pub fn heads(&self) -> Vec<CaseHead> {
        self.cases
            .iter()
            .map(|case| CaseHead {
                case_id: case.case_id,
                snapshot: case.replay.snapshot.clone(),
            })
            .collect()
    }
}

impl<F: DurableFs> Store<F> {
    /// Verify every case journal in full and read the canonical state an
    /// archive copies, except blob bytes. Refused while a capture holds
    /// staged evidence that is not yet canonical (§10.7): an archive never
    /// drops it silently.
    ///
    /// The state comes with an [`ArchivePin`] created under the same lock,
    /// so the heads read and the heads pinned are one atomic step. Hold the
    /// pin until the archive is published or abandoned, then pass it to
    /// [`Store::release_pin`] (dropping it also ends it). Appends continue
    /// meanwhile; the archive covers exactly the pinned heads.
    pub fn read_canonical(&mut self) -> Result<(CanonicalState, ArchivePin), Failure>
    where
        F::Guard: 'static,
    {
        let _lock = self.coordinate()?;
        let verified = self.load_fully_verified()?;
        self.refuse_unsettled_staging(&verified)?;
        let marker = self.read_all(Path::new(MARKER_FILE))?;
        let config = self.read_all(Path::new(CONFIG_FILE))?;
        let mut cases = Vec::with_capacity(verified.cases.len());
        for (case_id, replay) in &verified.cases {
            cases.push(self.read_journal(*case_id, replay)?);
        }
        let (blobs, not_retained) =
            required_blobs(cases.iter().map(|case| (case.case_id, &case.replay)));
        let state = CanonicalState {
            workspace_id: self.workspace.id(),
            marker,
            config,
            vault_enabled: self.workspace.config().vault_policy.enabled,
            cases,
            blobs,
            not_retained,
        };
        let pin = self.create_archive_pin(&state.heads(), &state.blobs)?;
        Ok((state, pin))
    }

    /// The bytes of the retained blob with `digest`, as stored. The caller
    /// checks them against the digest; nothing here trusts the file name.
    pub fn retained_bytes(&self, digest: &Digest) -> Result<Vec<u8>, Failure> {
        let path = blob_path(digest);
        self.read_all(Path::new(&path)).map_err(|failure| {
            let mut failure = failure;
            if failure.error.code == ErrorCode::IoFailure {
                failure = integrity(format!(
                    "A recorded blob could not be read from canonical storage ({digest})."
                ));
            }
            failure
        })
    }

    fn read_all(&self, path: &Path) -> Result<Vec<u8>, Failure> {
        let mut bytes = Vec::new();
        self.fs
            .read(path)
            .and_then(|mut reader| reader.read_to_end(&mut bytes))
            .map_err(io_failure)?;
        Ok(bytes)
    }

    /// Re-read a verified journal's bytes and confirm each file hashes to
    /// the chain the replay verified.
    fn read_journal(&self, case_id: Id, replay: &CaseReplay) -> Result<CaseJournal, Failure> {
        let mut journal = CaseJournal {
            case_id,
            replay: replay.clone(),
            genesis: Vec::new(),
            events: Vec::with_capacity(replay.events.len()),
        };
        journal.genesis = self.read_all(Path::new(&journal.genesis_path()))?;
        let mut parent = hash_bytes(&journal.genesis);
        for verified in &replay.events {
            let bytes = self.read_all(Path::new(&journal.event_path(verified.event.sequence)))?;
            let hash = hash_bytes(&bytes);
            if verified.event.previous_hash != parent || hash != verified.hash {
                return Err(integrity(format!(
                    "Case {case_id} changed while it was read for the archive at sequence {}.",
                    verified.event.sequence
                )));
            }
            parent = hash;
            journal.events.push(bytes);
        }
        Ok(journal)
    }

    fn refuse_unsettled_staging(&self, verified: &super::WorkspaceReplay) -> Result<(), Failure> {
        let mut report = CheckReport {
            heads: Vec::new(),
            structurally_valid: true,
            verified_blobs: 0,
            unavailable_artifacts: 0,
            reference_only_artifacts: 0,
            source_authenticated: None,
            independently_anchored: None,
            index: ProjectionStatus::Pending,
            staging: super::capture::retire::StagingSummary::default(),
            issues: Vec::new(),
        };
        self.check_staging(verified, &mut report);
        let staging = &report.staging;
        // Every staging state that is not settled: an open or sealed handle,
        // a handle declared but never allocated, staging bound to no handle,
        // and staging that cannot be verified (a binding or seal finding, an
        // unreadable root). Only finalized captures, retired or awaiting
        // retirement, hold nothing the journal lacks.
        let unsettled: Vec<&super::CheckIssue> = report
            .issues
            .iter()
            .filter(|issue| {
                issue.severity == super::Severity::Error
                    || UNSETTLED_CODES.contains(&issue.code.as_str())
            })
            .collect();
        if staging.active + staging.sealed + staging.unallocated + staging.orphaned == 0
            && unsettled.is_empty()
        {
            return Ok(());
        }
        let mut failure = Failure::new(
            ErrorCode::GateFailed,
            "Capture staging holds evidence the journal does not reference yet, so a complete \
             archive would drop it. Finalize or recover each open capture, then archive again.",
        );
        failure.error.actual = Some(serde_json::json!({
            "active": staging.active,
            "sealed": staging.sealed,
            "unallocated": staging.unallocated,
            "orphaned": staging.orphaned,
            "handles": unsettled
                .iter()
                .take(MAX_LISTED_HANDLES)
                .map(|issue| serde_json::json!({
                    "code": issue.code,
                    "objects": issue.objects,
                    "reason": issue.reason,
                }))
                .collect::<Vec<_>>(),
            "handles_capped_at": MAX_LISTED_HANDLES,
        }));
        Err(failure.with_argv(vec![
            investigation_core::PROGRAM_NAME.to_owned(),
            "check".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ]))
    }
}

/// Staging findings that mean a capture is not settled, besides any error.
const UNSETTLED_CODES: [&str; 3] = [
    "CAPTURE_STAGING_UNALLOCATED",
    "CAPTURE_SEALED_UNCOMMITTED",
    "STAGING_ORPHAN",
];
/// Most unsettled handles a refusal lists.
const MAX_LISTED_HANDLES: usize = 50;

/// The blobs these verified journals require, and the Artifacts that hold
/// no bytes. The rule is `check`'s: event attachments and manifests, and
/// every retained Artifact's recorded digest and size.
pub fn required_blobs<'a>(
    replays: impl IntoIterator<Item = (Id, &'a CaseReplay)>,
) -> (Vec<RequiredBlob>, Vec<NotRetained>) {
    let mut blobs = BTreeMap::<Digest, BTreeSet<u64>>::new();
    let mut not_retained = Vec::new();
    for (case_id, replay) in replays {
        for verified in &replay.events {
            for digest in verified
                .event
                .request
                .attachments
                .iter()
                .chain(verified.event.manifest_digest.iter())
            {
                blobs.entry(digest.clone()).or_default();
            }
        }
        for record in replay
            .state
            .records()
            .filter(|record| record.header.id.kind() == Kind::Artifact)
        {
            let Ok(body) = investigation_core::input::decode::<ArtifactBody>(&record.body) else {
                // `check` reports undecodable metadata; there is nothing to copy.
                continue;
            };
            match (body.availability, body.digest, body.size_bytes) {
                (ArtifactAvailability::Retained, Some(digest), Some(size)) => {
                    blobs.entry(digest).or_default().insert(size);
                }
                (ArtifactAvailability::Retained, ..) => {}
                (availability, digest, _) => not_retained.push(NotRetained {
                    case_id,
                    artifact: record.header.id,
                    availability,
                    sha256: digest,
                }),
            }
        }
    }
    not_retained.sort_by_key(|entry| (entry.case_id, entry.artifact));
    let blobs = blobs
        .into_iter()
        .map(|(digest, sizes)| RequiredBlob { digest, sizes })
        .collect();
    (blobs, not_retained)
}

/// Create the directories, empty lock files and pointer file a workspace
/// needs around restored canonical files, below `root`, which must exist.
/// Existing directories are kept; an existing lock or pointer file is an
/// error, so nothing restored is overwritten. `cases` names the case
/// directories whose `commit.lock` to create.
pub fn restore_layout(root: &Path, vault: bool, cases: &[Id]) -> Result<(), WorkspaceError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    for relative in workspace::required_directories(vault) {
        let path = root.join(relative);
        builder
            .create(&path)
            .map_err(|source| workspace::io_at(&path, source))?;
    }
    let mut empty = vec![root.join(COORDINATION_LOCK)];
    for case_id in cases {
        let directory = root.join(case_directory(*case_id));
        builder
            .create(&directory)
            .map_err(|source| workspace::io_at(&directory, source))?;
        empty.push(directory.join(CASE_LOCK));
    }
    for path in &empty {
        create_new(path, b"")?;
    }
    create_new(
        &root.join(POINTER_FILE),
        workspace::pointer_text().as_bytes(),
    )
}

fn create_new(path: &Path, bytes: &[u8]) -> Result<(), WorkspaceError> {
    use std::io::Write as _;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| workspace::io_at(path, source))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| workspace::io_at(path, source))
}

/// Copy `input` to `output`, or only read it when `output` is `None`, and
/// return the SHA-256 digest and length of the bytes that passed. Memory
/// stays one 64 KiB buffer whatever the size.
pub fn copy_hashed(
    mut input: impl Read,
    mut output: Option<&mut dyn std::io::Write>,
) -> std::io::Result<(Digest, u64)> {
    use sha2::{Digest as _, Sha256};
    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = match input.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hash.update(&chunk[..read]);
        if let Some(output) = output.as_deref_mut() {
            output.write_all(&chunk[..read])?;
        }
        length = length
            .checked_add(read as u64)
            .ok_or_else(|| std::io::Error::other("file too large"))?;
    }
    let digest = format!("sha256:{:x}", hash.finalize())
        .parse()
        .expect("SHA-256 hex has a valid format");
    Ok((digest, length))
}
