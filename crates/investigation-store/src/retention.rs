//! Retention obligations: what still holds a stored blob once some
//! references give up their availability (plan §16.5; W11.11).
//!
//! Retention applies to references, never to digests (`artifact.expire`).
//! A blob may be reclaimed only when nothing live holds its digest any more.
//! [`Store::retention_obligations`] lists, for each digest, every holder the
//! workspace knows of:
//!
//! - the Artifacts in any case whose current availability is `retained`;
//! - the method packs any case has adopted with those bytes;
//! - journal events that attached the bytes while no Artifact of their case
//!   records the digest (bytes kept for a record of another kind);
//! - live archive pins (W11.09), which hold until the archive ends;
//! - unsettled capture staging, which may still finalize into a reference
//!   to the same bytes, so it holds every digest until it is settled.
//!
//! A digest with no holder is *reclaimable*. This module never removes a
//! blob. Physical reclamation (W11.19) recomputes the obligations with
//! [`Store::retention_obligations_locked`] under the same coordination hold
//! in which it calls the removal, so an import, a pin or a capture cannot
//! slip in between. Until then a withdrawn reference is reported truthfully
//! as unavailable while its bytes stay stored.

use std::{
    collections::{BTreeMap, BTreeSet},
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

use super::{DurableFs, Store, archive::blob_path};

/// An Artifact that keeps the bytes retained.
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
/// records their digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct AttachmentHolder {
    pub case_id: Id,
    pub sequence: u64,
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
    /// Names of the live archive pins that hold the digest.
    pub archive_pins: Vec<String>,
    /// Capture staging that is not settled holds every digest.
    pub unsettled_staging: bool,
}

impl DigestObligations {
    /// Whether a record of the workspace still holds the bytes: a retained
    /// reference, an adopted pack or a journal attachment.
    pub fn held_by_records(&self) -> bool {
        !(self.references.is_empty() && self.method_packs.is_empty() && self.attachments.is_empty())
    }

    /// Whether nothing holds the stored bytes: a later reclamation could
    /// remove them. Never true for a blob that is not stored.
    pub fn reclaimable(&self) -> bool {
        self.stored
            && !self.held_by_records()
            && self.archive_pins.is_empty()
            && !self.unsettled_staging
    }
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
    /// coordination lock: physical reclamation must recheck and remove
    /// under one hold.
    pub(crate) fn retention_obligations_locked(
        &mut self,
        digests: &BTreeSet<Digest>,
        released: &BTreeSet<Id>,
    ) -> Result<Vec<DigestObligations>, Failure> {
        let verified = self.load_verified()?;
        let unsettled_staging = self.refuse_unsettled_staging(&verified).is_err();
        let pins = self.live_pins()?;
        let mut found: BTreeMap<Digest, DigestObligations> = digests
            .iter()
            .map(|digest| {
                (
                    digest.clone(),
                    DigestObligations {
                        digest: digest.clone(),
                        size_bytes: None,
                        stored: self.fs.identity(Path::new(&blob_path(digest))).is_ok(),
                        references: Vec::new(),
                        method_packs: Vec::new(),
                        attachments: Vec::new(),
                        archive_pins: pins
                            .iter()
                            .filter(|(_, held)| {
                                held.as_ref().is_none_or(|held| held.contains(digest))
                            })
                            .map(|(name, _)| name.clone())
                            .collect(),
                        unsettled_staging,
                    },
                )
            })
            .collect();
        for (case_id, replay) in &verified.cases {
            let state = &replay.state;
            // Every digest any Artifact revision of this case records: an
            // attachment of such bytes follows that Artifact's availability.
            let mut recorded = BTreeSet::new();
            for current in state.records_of_kind(Kind::Artifact) {
                for record in state.history(current.header.id) {
                    let Ok(body) = input::decode::<ArtifactBody>(&record.body) else {
                        continue;
                    };
                    let Some(digest) = body.digest else {
                        continue;
                    };
                    if let Some(entry) = found.get_mut(&digest) {
                        entry.size_bytes = entry.size_bytes.or(body.size_bytes);
                    }
                    recorded.insert(digest);
                }
                let Ok(body) = input::decode::<ArtifactBody>(&current.body) else {
                    continue;
                };
                if body.availability != ArtifactAvailability::Retained
                    || released.contains(&current.header.id)
                {
                    continue;
                }
                if let Some(entry) = body.digest.and_then(|digest| found.get_mut(&digest)) {
                    entry.references.push(ReferenceHolder {
                        case_id: *case_id,
                        artifact_ref: current.reference(),
                    });
                }
            }
            for pin in method_pack_ops::adopted_packs(state, *case_id, None)? {
                if let Some(entry) = found.get_mut(&pin.digest) {
                    entry.method_packs.push(MethodPackHolder {
                        case_id: *case_id,
                        pack_id: pin.pack_id,
                        version: pin.version,
                    });
                }
            }
            for verified in &replay.events {
                let event = &verified.event;
                for digest in event
                    .request
                    .attachments
                    .iter()
                    .chain(event.manifest_digest.iter())
                    .filter(|digest| !recorded.contains(*digest))
                {
                    if let Some(entry) = found.get_mut(digest) {
                        entry.attachments.push(AttachmentHolder {
                            case_id: *case_id,
                            sequence: event.sequence,
                        });
                    }
                }
            }
        }
        Ok(found
            .into_values()
            .map(|mut entry| {
                entry.references.sort();
                entry.method_packs.sort();
                entry.attachments.sort();
                entry.attachments.dedup();
                entry
            })
            .collect())
    }
}
