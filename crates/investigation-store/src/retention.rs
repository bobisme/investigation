//! Retention obligations and physical reclamation (plan §16.5; W11.11,
//! W11.19).
//!
//! Retention applies to references, never to digests (`artifact.expire`).
//! A blob may be reclaimed only when nothing live holds its digest any more.
//! [`record_holders`] is the one rule for which records still hold stored
//! bytes. `check`, the workspace archive and reclamation all use it, so a
//! blob that only withdrawn references record is neither demanded by `check`
//! nor copied into a new archive, and every other blob is:
//!
//! - the Artifacts in any case whose current availability is `retained`;
//! - the method packs any case has adopted with those bytes;
//! - journal events that attached the bytes, unless every Artifact of their
//!   case recording the digest is `retained` or `intentionally_removed` (the
//!   attachment then follows those Artifacts' availability). An attachment no
//!   Artifact records, or one recorded by an Artifact of another
//!   availability, always holds.
//!
//! [`Store::retention_obligations`] adds the live holders outside the
//! journal: live archive pins (W11.09), unsettled capture staging (which may
//! still finalize into a reference to the same bytes, so it holds every
//! digest) and copies of the bytes in capture staging that is finalized but
//! not yet retired (retirement verifies them against the canonical blob).
//!
//! [`Store::reclaim_blobs`] removes a blob only when, under one hold of the
//! coordination lock, nothing holds it and at least one reference to it was
//! explicitly withdrawn. [`Store::reclaim_withdrawn`] is the workspace-wide
//! sweep: it selects every digest a withdrawn reference records, in every
//! case, and applies the same per-digest decision under the same single hold
//! of the lock (one hold for the whole sweep, not one per digest); without
//! `apply` it decides and removes nothing. Imports, archive pins, capture begin and
//! finalization and every commit take the same lock, so none can slip in
//! between the recheck and the removal; a commit after it publishes the
//! bytes again from its own staged copy. Each removal is one unlink followed
//! by a sync of the shard directory, so an interruption at any step leaves
//! the blob either present (a retry removes it) or absent (a retry reports
//! it already absent); the journal is never touched.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
};

use investigation_core::{
    command::RevisionRef,
    evidence::{ArtifactAvailability, ArtifactBody},
    identity::{Id, Kind},
    input, method_pack_ops,
    wire::{Digest, Failure},
};
use serde::Serialize;

use super::{
    BLOBS, DurableFs, Store, archive::blob_path, capture::STAGING_ROOT, durability_failure,
    io_failure, sync_directory,
};
use crate::journal::CaseReplay;

/// An Artifact that records the bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ReferenceHolder {
    pub case_id: Id,
    pub artifact_ref: RevisionRef,
}

/// An adopted method pack that reads the bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct MethodPackHolder {
    pub case_id: Id,
    pub pack_id: String,
    pub version: String,
}

/// A journal event that attached the bytes while no Artifact of its case
/// accounts for their availability.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct AttachmentHolder {
    pub case_id: Id,
    pub sequence: u64,
}

/// What the journals record about one digest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordHolders {
    /// Sizes recorded by retained Artifacts.
    pub retained_sizes: BTreeSet<u64>,
    /// The first size any Artifact revision records.
    pub size_bytes: Option<u64>,
    pub references: Vec<ReferenceHolder>,
    pub method_packs: Vec<MethodPackHolder>,
    pub attachments: Vec<AttachmentHolder>,
    /// Current Artifacts recording the digest whose availability was
    /// withdrawn (or is planned to be, through `released`).
    pub withdrawn: Vec<ReferenceHolder>,
}

impl RecordHolders {
    /// Whether a record still needs the stored bytes.
    pub fn holds(&self) -> bool {
        !(self.references.is_empty() && self.method_packs.is_empty() && self.attachments.is_empty())
    }
}

/// Every digest the verified journals record (attached by an event or
/// named by an Artifact), with the records that still hold its bytes.
/// Artifacts in `released` count as withdrawn: a planned `artifact.expire`.
/// Fails only when a case's adopted method packs cannot be read; a caller
/// that must not fail then treats every recorded digest as held.
pub fn record_holders<'a>(
    replays: impl IntoIterator<Item = (Id, &'a CaseReplay)>,
    released: &BTreeSet<Id>,
) -> Result<BTreeMap<Digest, RecordHolders>, Failure> {
    let mut found = BTreeMap::<Digest, RecordHolders>::new();
    for (case_id, replay) in replays {
        let state = &replay.state;
        // Per digest, the availability of every current Artifact of this
        // case whose history records it.
        let mut recording = BTreeMap::<Digest, Vec<ArtifactAvailability>>::new();
        for current in state.records_of_kind(Kind::Artifact) {
            let mut digests = BTreeSet::new();
            for record in state.history(current.header.id) {
                let Ok(body) = input::decode::<ArtifactBody>(&record.body) else {
                    continue;
                };
                if let Some(digest) = body.digest {
                    let entry = found.entry(digest.clone()).or_default();
                    entry.size_bytes = entry.size_bytes.or(body.size_bytes);
                    digests.insert(digest);
                }
            }
            let availability = match input::decode::<ArtifactBody>(&current.body) {
                Ok(body) => {
                    let holder = ReferenceHolder {
                        case_id,
                        artifact_ref: current.reference(),
                    };
                    match (body.availability, body.digest) {
                        (ArtifactAvailability::Retained, Some(digest))
                            if !released.contains(&current.header.id) =>
                        {
                            let entry = found.entry(digest).or_default();
                            entry.references.push(holder);
                            entry.retained_sizes.extend(body.size_bytes);
                            ArtifactAvailability::Retained
                        }
                        (
                            ArtifactAvailability::Retained
                            | ArtifactAvailability::IntentionallyRemoved,
                            Some(digest),
                        ) => {
                            found.entry(digest).or_default().withdrawn.push(holder);
                            ArtifactAvailability::IntentionallyRemoved
                        }
                        (availability, _) => availability,
                    }
                }
                // Unreadable metadata accounts for nothing: attachments hold.
                Err(_) => ArtifactAvailability::Missing,
            };
            for digest in digests {
                recording.entry(digest).or_default().push(availability);
            }
        }
        for pin in method_pack_ops::adopted_packs(state, case_id, None)? {
            found
                .entry(pin.digest)
                .or_default()
                .method_packs
                .push(MethodPackHolder {
                    case_id,
                    pack_id: pin.pack_id,
                    version: pin.version,
                });
        }
        for verified in &replay.events {
            let event = &verified.event;
            for digest in event
                .request
                .attachments
                .iter()
                .chain(event.manifest_digest.iter())
            {
                let accounted = recording.get(digest).is_some_and(|availability| {
                    availability.iter().all(|availability| {
                        matches!(
                            availability,
                            ArtifactAvailability::Retained
                                | ArtifactAvailability::IntentionallyRemoved
                        )
                    })
                });
                let entry = found.entry(digest.clone()).or_default();
                if !accounted {
                    entry.attachments.push(AttachmentHolder {
                        case_id,
                        sequence: event.sequence,
                    });
                }
            }
        }
    }
    for entry in found.values_mut() {
        entry.references.sort();
        entry.method_packs.sort();
        entry.attachments.sort();
        entry.attachments.dedup();
        entry.withdrawn.sort();
    }
    Ok(found)
}

/// Every known obligation on one digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DigestObligations {
    pub digest: Digest,
    /// The size recorded for the digest, when any Artifact records one.
    pub size_bytes: Option<u64>,
    /// Whether the blob is stored in this workspace now.
    pub stored: bool,
    pub references: Vec<ReferenceHolder>,
    pub method_packs: Vec<MethodPackHolder>,
    pub attachments: Vec<AttachmentHolder>,
    /// Current Artifacts recording the digest whose availability was
    /// withdrawn. Reclamation removes only bytes with at least one.
    pub withdrawn: Vec<ReferenceHolder>,
    /// Names of the live archive pins that hold the digest.
    pub archive_pins: Vec<String>,
    /// Capture staging that is not settled holds every digest.
    pub unsettled_staging: bool,
    /// Copies in finalized capture staging that is not retired yet,
    /// relative to the workspace root. Retirement verifies them against the
    /// canonical blob, so they hold it until the staging is retired.
    pub staged_copies: Vec<String>,
}

impl DigestObligations {
    /// Whether a record of the workspace still holds the bytes: a retained
    /// reference, an adopted pack or a journal attachment.
    pub fn held_by_records(&self) -> bool {
        !(self.references.is_empty() && self.method_packs.is_empty() && self.attachments.is_empty())
    }

    /// Whether something outside the journal holds the bytes for now: a live
    /// archive pin, unsettled staging or an unretired staged copy.
    pub fn waiting(&self) -> bool {
        !self.archive_pins.is_empty() || self.unsettled_staging || !self.staged_copies.is_empty()
    }

    /// Whether nothing holds the stored bytes: a reclamation could remove
    /// them. Never true for a blob that is not stored.
    pub fn reclaimable(&self) -> bool {
        self.stored && !self.held_by_records() && !self.waiting()
    }
}

/// What a reclamation did with one digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReclaimOutcome {
    /// The blob was removed and its shard directory synced.
    Reclaimed,
    /// Nothing holds the digest and no blob is stored: an earlier
    /// reclamation removed it.
    AlreadyAbsent,
    /// A retained reference, adopted pack or attachment still holds it.
    HeldByRecords,
    /// A live pin or capture staging holds it; retry when it ends.
    Waiting,
    /// No reference to these bytes was withdrawn: reclamation never removes
    /// bytes whose loss of availability is not committed.
    NotWithdrawn,
    /// A dry run found nothing holding the stored blob: a sweep with `apply`
    /// would remove it. Nothing was removed.
    WouldReclaim,
}

/// One digest's outcome, with the obligations rechecked under the lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reclamation {
    pub outcome: ReclaimOutcome,
    pub obligations: DigestObligations,
}

impl<F: DurableFs> Store<F> {
    /// The obligations on each of `digests`, treating the Artifacts in
    /// `released` as no longer holding their bytes (a planned
    /// `artifact.expire`). Writes nothing. Takes the coordination lock, so
    /// the answer is consistent with one set of verified heads and pins.
    pub fn retention_obligations(
        &mut self,
        digests: &BTreeSet<Digest>,
        released: &BTreeSet<Id>,
    ) -> Result<Vec<DigestObligations>, Failure> {
        let _lock = self.coordinate()?;
        self.retention_obligations_locked(digests, released)
    }

    /// [`Self::retention_obligations`] for a caller that already holds the
    /// coordination lock: reclamation rechecks and removes under one hold.
    pub(crate) fn retention_obligations_locked(
        &mut self,
        digests: &BTreeSet<Digest>,
        released: &BTreeSet<Id>,
    ) -> Result<Vec<DigestObligations>, Failure> {
        let verified = super::FreeLater::new(self.load_verified()?);
        self.obligations_locked(&verified, released, |_| digests.clone())
    }

    /// One verified replay of `case_id` and the obligations on the digests
    /// `plan` picks from it, treating the Artifacts it names as released, all
    /// from one verified load under one hold of the coordination lock. The
    /// preview of a withdrawal needs both; loading them together replays the
    /// journal once instead of twice, and the obligations then describe
    /// exactly the snapshot of the returned replay. `plan` also gets the
    /// retained method pack bytes the commit's guardrails read, under the
    /// same hold, so a simulated withdrawal is guarded as the commit is.
    pub fn case_with_obligations(
        &mut self,
        case_id: Id,
        plan: impl FnOnce(
            &CaseReplay,
            &dyn investigation_core::pack_policy::PackSource,
        ) -> Result<(BTreeSet<Digest>, BTreeSet<Id>), Failure>,
    ) -> Result<(CaseReplay, Vec<DigestObligations>), Failure> {
        let _lock = self.coordinate()?;
        let mut verified = super::FreeLater::new(self.load_verified()?);
        let replay = verified
            .cases
            .get(&case_id)
            .ok_or_else(|| super::missing_case(case_id))?;
        let (digests, released) = plan(replay, &super::method_packs::RetainedPacks(&self.fs))?;
        let obligations = self.obligations_locked(&verified, &released, |_| digests)?;
        let replay = verified
            .cases
            .remove(&case_id)
            .ok_or_else(|| super::missing_case(case_id))?;
        Ok((replay, obligations))
    }

    /// The obligations on the digests `select` picks from every digest the
    /// verified journals record, with the holder scan run once.
    fn obligations_locked(
        &mut self,
        verified: &super::WorkspaceReplay,
        released: &BTreeSet<Id>,
        select: impl FnOnce(&BTreeMap<Digest, RecordHolders>) -> BTreeSet<Digest>,
    ) -> Result<Vec<DigestObligations>, Failure> {
        let unsettled_staging = self.refuse_unsettled_staging(verified).is_err();
        let pins = self.live_pins()?;
        let staging = self.staging_directories()?;
        let records = record_holders(
            verified.cases.iter().map(|(id, replay)| (*id, replay)),
            released,
        )?;
        let digests = select(&records);
        digests
            .iter()
            .map(|digest| {
                let holders = records.get(digest).cloned().unwrap_or_default();
                let hex = digest.as_str().trim_start_matches("sha256:");
                let mut staged_copies = Vec::new();
                for directory in &staging {
                    let copy = directory.join("blobs").join(hex);
                    // A copy that cannot be looked at holds the bytes too.
                    if self.blob_file_present(&copy).unwrap_or(true) {
                        staged_copies.push(copy.to_string_lossy().into_owned());
                    }
                }
                Ok(DigestObligations {
                    digest: digest.clone(),
                    size_bytes: holders.size_bytes,
                    stored: self.blob_file_present(Path::new(&blob_path(digest)))?,
                    references: holders.references,
                    method_packs: holders.method_packs,
                    attachments: holders.attachments,
                    withdrawn: holders.withdrawn,
                    archive_pins: pins
                        .iter()
                        .filter(|(_, held)| held.as_ref().is_none_or(|held| held.contains(digest)))
                        .map(|(name, _)| name.clone())
                        .collect(),
                    unsettled_staging,
                    staged_copies,
                })
            })
            .collect()
    }

    /// Whether a file is present. Only `NotFound` means absent: any other
    /// error fails, so an unreadable blob is never reported as removed.
    fn blob_file_present(&self, path: &Path) -> Result<bool, Failure> {
        match self.fs.identity(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(io_failure(error)),
        }
    }

    /// Remove the stored blob of each of `digests` that nothing holds and
    /// that at least one withdrawn reference records. The obligations are
    /// rechecked and every removal made under one hold of the coordination
    /// lock. Each removal is synced before the next; a retry after any
    /// interruption is safe and reports what an earlier attempt removed as
    /// `already_absent`. Records, journals and digests are never changed.
    pub fn reclaim_blobs(
        &mut self,
        digests: &BTreeSet<Digest>,
    ) -> Result<Vec<Reclamation>, Failure> {
        let _lock = self.coordinate()?;
        // A removal is a write: refused while a migration is in progress or
        // the format on disk is newer than this program writes.
        self.refuse_unwritable_format()?;
        let found = self.retention_obligations_locked(digests, &BTreeSet::new())?;
        self.reclaim_locked(found, true)
    }

    /// Every digest that a withdrawn reference in any case records, decided
    /// as [`Self::reclaim_blobs`] decides each named digest, all under one
    /// hold of the coordination lock. With `apply`, each blob nothing holds
    /// is removed and synced before the next; without it nothing is written
    /// (not even the disposable verdict cache) and such a blob is reported as
    /// [`ReclaimOutcome::WouldReclaim`]. Digests no withdrawn reference
    /// records are never considered. Results are in digest order.
    pub fn reclaim_withdrawn(&mut self, apply: bool) -> Result<Vec<Reclamation>, Failure> {
        let _lock = self.coordinate()?;
        if apply {
            self.refuse_unwritable_format()?;
        }
        let verified = super::FreeLater::new(self.load_verified()?);
        let found = self.obligations_locked(&verified, &BTreeSet::new(), |records| {
            records
                .iter()
                .filter(|(_, holders)| !holders.withdrawn.is_empty())
                .map(|(digest, _)| digest.clone())
                .collect()
        })?;
        self.reclaim_locked(found, apply)
    }

    /// The per-digest decision of a reclamation, for a caller that holds the
    /// coordination lock and has just rechecked `found` under it.
    fn reclaim_locked(
        &mut self,
        found: Vec<DigestObligations>,
        apply: bool,
    ) -> Result<Vec<Reclamation>, Failure> {
        let mut done = Vec::with_capacity(found.len());
        for obligations in found {
            let outcome = if obligations.withdrawn.is_empty() {
                ReclaimOutcome::NotWithdrawn
            } else if obligations.held_by_records() {
                ReclaimOutcome::HeldByRecords
            } else if !obligations.stored {
                if apply {
                    self.forget_verdict(&obligations.digest);
                }
                ReclaimOutcome::AlreadyAbsent
            } else if obligations.waiting() {
                ReclaimOutcome::Waiting
            } else if !apply {
                ReclaimOutcome::WouldReclaim
            } else {
                self.remove_blob(&obligations.digest)?;
                self.forget_verdict(&obligations.digest);
                ReclaimOutcome::Reclaimed
            };
            done.push(Reclamation {
                outcome,
                obligations,
            });
        }
        Ok(done)
    }

    /// Unlink one blob and sync its shard directory, so the removal is
    /// durable before it is reported.
    fn remove_blob(&mut self, digest: &Digest) -> Result<(), Failure> {
        self.gate.check().map_err(durability_failure)?;
        let path = blob_path(digest);
        match self.fs.remove(Path::new(&path)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_failure(error)),
        }
        let hex = digest.as_str().trim_start_matches("sha256:");
        sync_directory(&mut self.fs, &Path::new(BLOBS).join(&hex[..2]), &self.gate)
            .map_err(durability_failure)
    }

    /// Drop the disposable verdict that names the digest, best effort: it
    /// holds no bytes, and a stale entry only ever means "hash again".
    fn forget_verdict(&mut self, digest: &Digest) {
        let hex = digest.as_str().trim_start_matches("sha256:");
        let _ = self.fs.remove(&Path::new("cache/blob-verdicts").join(hex));
    }

    /// The capture staging directories present now.
    fn staging_directories(&self) -> Result<Vec<std::path::PathBuf>, Failure> {
        match self.fs.list(Path::new(STAGING_ROOT)) {
            Ok(entries) => Ok(entries),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(io_failure(error)),
        }
    }
}
