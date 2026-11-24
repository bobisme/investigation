//! Finalization of a capture handle (`capture.finalize`): seal the handle's
//! staging, publish its retained bytes, and commit the Run's manifest in one
//! journal transaction.
//!
//! The store owns locking, the seal, byte publication and the transaction. The
//! recorder owns interpretation: under the handle lock it is handed every
//! staged item and returns a [`FinalizePlan`] with the exact manifest bytes,
//! the retained attachments to record as Artifacts, and the Run's finalization
//! section. The store never reads a producer statement.
//!
//! # Order
//!
//! 1. Take the handle lock, then look up the key in the journal. A committed
//!    identical request returns its original receipt, also after staging is
//!    retired; a different request under the key, or a second finalization of
//!    the Run, is refused.
//! 2. Read the staged items and plan. A plan refusal (a stale cursor, an
//!    unknown key) seals nothing; the handle stays open.
//! 3. Seal: publish `sealed.json` by exclusive link and sync it. Its presence
//!    closes the handle to new items. It binds the key, the request digest,
//!    the sealed position, the pre-finalization case snapshot and the manifest
//!    digest. The seal is the staging commit point.
//! 4. Sweep `blobs/*.tmp` and staged blobs no item references. After the seal
//!    no item can reference them.
//! 5. Copy each referenced blob and the manifest into private temporaries and
//!    commit one transaction: the journal publishes the bytes into the
//!    canonical blob store before the event, which creates the capture Source,
//!    an Artifact per retained attachment, the manifest Artifact, and the next
//!    revision of the Run. Staged blobs are copied, never moved or linked, so
//!    staging keeps its bytes until it is retired.
//!
//! # Crash model
//!
//! - Before the seal: nothing durable changed; a retry plans again.
//! - After the seal, before the event: the handle is closed and the seal names
//!   the request. The identical retry replans with the sealed snapshot, checks
//!   the manifest digest against the seal and commits. Any other request is
//!   refused with the sealing request in `expected`.
//! - After the event: the journal answers; the identical retry returns the
//!   original receipt.
//!
//! # Seal layout
//!
//! ```text
//! state/staging/<run-id>/
//!   sealed.json       0400; schema_version inv.capture-seal.v1, handle, run_id,
//!                     command_id, request_digest, sealed_position, snapshot,
//!                     manifest_digest
//!   sealed.json.tmp   transient; only while the seal is published
//! ```

use std::io::{ErrorKind, Read as _};

use investigation_core::{
    command::{Reference, RevisionRef},
    derivation::DerivationRecord,
    evidence::{
        AcquisitionChannel, ArtifactAvailability, ArtifactBody, ArtifactIntegrity, ArtifactRole,
        ArtifactTransform, SourceAccess, SourceAcquisition, SourceBody,
    },
    explain::{EvidenceCompleteness, ExecutionStatus},
    header::{ClockQuality, Knowledge, SourceTime},
    producer_run::CAPTURE_FINALIZATION_KEY,
    receipt_check::{self, MAX_RECEIPT_BYTES, ReceiptBytes, ReceiptFormat, ReceiptInput},
    wire::{Digest, Snapshot},
};

use super::{
    attach::{AttachAvailability, AttachOutcome, BLOBS_DIRECTORY},
    items::{ResolvedHandle, SEAL_FILE, StagedItem},
    *,
};
use crate::{
    blob::stage,
    durable::sync_existing_file,
    journal::{Event, hash_bytes, replay_case},
    store::ingest::{IngestRecords, new_id, sensitivity},
};

/// Journal operation name of a finalizing transaction.
pub const FINALIZE_OPERATION: &str = "capture.finalize";
const SEAL_VERSION: &str = "inv.capture-seal.v1";
const MAX_SEAL_BYTES: u64 = 16 * 1024;
const CAPTURE_LIMIT: &str = "Every producer statement in this capture is an attributed claim; the recorder verified only the digest and size of the bytes it retained.";

/// One finalize request, already validated by the recorder.
#[derive(Debug, Clone, PartialEq)]
pub struct FinalizeSubmission {
    pub handle: Id,
    /// Journal command ID; the producer's finalize key.
    pub command_id: String,
    /// Normalized recorder request retained in the journal. An identical retry
    /// compares it structurally.
    pub request: Value,
}

/// What the recorder plans from, read under the handle lock.
#[derive(Debug)]
pub struct FinalizeContext<'a> {
    pub case_id: Id,
    pub run_id: Id,
    /// The actor that declared the handle; every capture record is theirs.
    pub actor: &'a Actor,
    pub binding: &'a CaptureBinding,
    /// The declaring begin request, as the recorder sent it.
    pub declaration: &'a Value,
    /// Effective classification of the declared Run.
    pub classification: ClassificationLevel,
    /// Every staged item, in position order; positions are contiguous from 1.
    pub items: &'a [StagedItem],
    /// The case head before finalization. A retry after the seal receives the
    /// sealed snapshot, so the same request plans the same manifest.
    pub snapshot: &'a Snapshot,
}

/// How a retained attachment enters the canonical record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedRole {
    /// An original acquisition of the capture.
    Original,
    /// A derivative of the earlier planned attachment at `from`.
    Derivative {
        from: u64,
        transformation: String,
        version: String,
    },
}

/// One staged attachment with retained bytes, to record as an Artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedArtifact {
    pub position: u64,
    pub role: PlannedRole,
    /// Format hint, never used to select a parser.
    pub media_type: Option<String>,
}

/// One producer receipt, resolved by the recorder to the staged attachment
/// that holds its bytes. The store checks the structure of a receipt with a
/// declared format and records the result on the Run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedReceipt {
    /// The producer's kind, verbatim.
    pub kind: String,
    pub format: Option<ReceiptFormat>,
    /// Position of the attachment holding the bytes; `None` for a reference
    /// or an artifact that was never attached.
    pub position: Option<u64>,
}

/// Records the finalizing transaction created, for the Run's section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRefs {
    /// The capture Source.
    pub source: RevisionRef,
    /// The manifest Artifact.
    pub manifest: RevisionRef,
    pub manifest_digest: Digest,
    pub manifest_size: u64,
    /// One Artifact per planned attachment, in plan order.
    pub artifacts: Vec<RevisionRef>,
}

/// Builds the Run's finalization section once record IDs are allocated.
pub type SectionBuilder = Box<dyn FnOnce(&RecordedRefs) -> Result<Value, Failure>>;

/// What the recorder decided a finalization records.
pub struct FinalizePlan {
    /// Exact manifest bytes, retained as the manifest Artifact. They must be a
    /// function of the context and the request only.
    pub manifest: Vec<u8>,
    /// Retained attachments in position order; a derivative follows its input.
    pub artifacts: Vec<PlannedArtifact>,
    pub execution_status: ExecutionStatus,
    pub evidence_completeness: EvidenceCompleteness,
    pub started_at: Option<SourceTime>,
    pub finished_at: Option<SourceTime>,
    /// Producer-stated limitations, added to the Source's verification limits.
    pub limitations: Vec<String>,
    /// Inert namespaced producer metadata for the Source.
    pub metadata: BTreeMap<String, Value>,
    /// Receipt warnings.
    pub warnings: Vec<String>,
    /// The producer's receipts, in request order.
    pub receipts: Vec<PlannedReceipt>,
    /// The Run body section under [`CAPTURE_FINALIZATION_KEY`].
    pub section: SectionBuilder,
}

/// A committed finalization, or the original one on an identical retry.
#[derive(Debug, Clone, PartialEq)]
pub struct FinalizedCapture {
    /// `committed`, or `already_committed` on a retry.
    pub receipt: Receipt,
    pub case_id: Id,
    pub run_id: Id,
    pub handle: Id,
    /// The Run's finalization section as committed.
    pub section: Value,
}

/// The finalize request that sealed a handle, as its seal binds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSeal {
    /// The sealing request's key: its journal command ID.
    pub command_id: String,
    /// Digest of the sealing request; only the identical request completes it.
    pub request_digest: Digest,
    /// Last accepted position when the handle was sealed.
    pub sealed_position: Option<u64>,
    /// Digest of the manifest the sealing request plans.
    pub manifest_digest: Digest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealFile {
    schema_version: String,
    handle: Id,
    run_id: Id,
    command_id: String,
    request_digest: Digest,
    sealed_position: Option<u64>,
    snapshot: Snapshot,
    manifest_digest: Digest,
}

/// A retained attachment resolved against its staged outcome.
struct Retained<'a> {
    planned: &'a PlannedArtifact,
    key: &'a str,
    outcome: AttachOutcome,
    digest: Digest,
    size: u64,
}

impl<F: DurableFs> Store<F> {
    /// Seal a capture handle and commit its manifest in one transaction, or
    /// return the original receipt of an identical earlier finalization.
    ///
    /// `plan` runs at most once, under the handle lock and never under the
    /// commit lock. An error from it seals nothing.
    pub fn finalize_capture<P>(
        &mut self,
        submission: &FinalizeSubmission,
        clock: &mut dyn Clock,
        plan: P,
    ) -> Result<FinalizedCapture, Failure>
    where
        P: FnOnce(&FinalizeContext<'_>) -> Result<FinalizePlan, Failure>,
    {
        self.gate.check().map_err(durability_failure)?;
        let declared = self
            .capture_handles()?
            .into_iter()
            .find(|declared| declared.binding.handle == submission.handle)
            .ok_or_else(|| {
                Failure::new(
                    ErrorCode::UnknownReference,
                    format!(
                        "No capture handle {} is declared in this workspace. Use the handle from the begin receipt.",
                        submission.handle
                    ),
                )
                .at("/handle")
            })?;
        // The journal answers a committed finalization even without staging.
        if let Some(done) = self.committed_finalization(&declared, submission)? {
            return Ok(done);
        }
        let located = self.locate_handle(submission.handle)?;
        let _handle_lock = match self.lock_handle(&located.directory) {
            Ok(guard) => guard,
            // Retirement moved the staging after it was located; it retires
            // only finalized handles, so the journal answers.
            Err(failure) if !self.exists(&located.directory.join(BINDING_FILE))? => {
                return self
                    .committed_finalization(&declared, submission)?
                    .ok_or(failure);
            }
            Err(failure) => return Err(failure),
        };
        let handle = self.verify_handle(located)?;
        // Another call may have finalized while this one waited for the lock.
        if let Some(done) = self.committed_finalization(&declared, submission)? {
            return Ok(done);
        }
        let items = self.staged_items(&handle)?;
        let request = serde_json::to_vec(&submission.request)
            .map_err(|_| integrity("Could not encode the finalize request."))?;
        let request_digest = hash_bytes(&request);
        let sealed_position = items.last().map(|item| item.position);
        let seal_path = handle.directory.join(SEAL_FILE);
        let seal = self.read_seal(&handle, &seal_path)?;
        if let Some(seal) = &seal {
            if seal.sealed_position != sealed_position {
                return Err(integrity(
                    "The capture seal disagrees with the staged items.",
                ));
            }
            if seal.command_id != submission.command_id || seal.request_digest != request_digest {
                return Err(sealed_by_another(seal, submission, &request_digest));
            }
            // A crashed first call may have published the seal without syncing.
            sync_existing_file(&mut self.fs, &seal_path, &self.gate).map_err(durability_failure)?;
            sync_directory(&mut self.fs, &handle.directory, &self.gate)
                .map_err(durability_failure)?;
        }
        let snapshot = match &seal {
            Some(seal) => seal.snapshot.clone(),
            None => self.case_snapshot(handle.case_id)?,
        };
        let plan = plan(&FinalizeContext {
            case_id: handle.case_id,
            run_id: handle.run_id,
            actor: &declared.actor,
            binding: &handle.binding,
            declaration: &declared.request,
            classification: handle.classification,
            items: &items,
            snapshot: &snapshot,
        })?;
        // The manifest must bind every staged item, so identical retries still
        // resolve once staging is retired.
        if super::late::bound_items(&plan.manifest, submission.handle, handle.run_id)? != items {
            return Err(integrity(
                "The planned capture manifest does not bind the staged item map.",
            ));
        }
        let manifest_digest = hash_bytes(&plan.manifest);
        match &seal {
            Some(seal) if seal.manifest_digest != manifest_digest => {
                return Err(integrity(
                    "The sealed manifest digest disagrees with the manifest this request plans. The staged items or the recorder changed after the seal.",
                ));
            }
            Some(_) => {}
            None => {
                let mut bytes = serde_json::to_vec_pretty(&SealFile {
                    schema_version: SEAL_VERSION.into(),
                    handle: submission.handle,
                    run_id: handle.run_id,
                    command_id: submission.command_id.clone(),
                    request_digest,
                    sealed_position,
                    snapshot,
                    manifest_digest: manifest_digest.clone(),
                })
                .map_err(|_| integrity("Could not encode the capture seal."))?;
                bytes.push(b'\n');
                self.publish_entry(&handle.directory, SEAL_FILE, &bytes)?;
            }
        }
        self.sweep_staged_blobs(&handle, &items)?;
        let mut temporaries = Vec::new();
        let result = self.commit_finalization(
            &declared,
            &handle,
            &items,
            submission,
            plan,
            manifest_digest,
            clock,
            &mut temporaries,
        );
        for temporary in &temporaries {
            let _ = self.fs.remove(temporary);
        }
        result
    }

    /// The committed finalization this request repeats, or a refusal when the
    /// key or the Run is already taken by another request.
    fn committed_finalization(
        &mut self,
        declared: &DeclaredHandle,
        submission: &FinalizeSubmission,
    ) -> Result<Option<FinalizedCapture>, Failure> {
        let replay = {
            let _lock = self.coordinate()?;
            self.load_verified()?
        };
        if let Some(verified) = replay.find(&declared.actor, &submission.command_id) {
            let request: Option<CaptureRequest> =
                investigation_core::input::decode(&verified.event.request.input).ok();
            let section = finalization_of(&verified.event, declared.run_id);
            let (Some(request), Some(section)) = (request, section) else {
                let mut failure = Failure::new(
                    ErrorCode::IdempotencyKeyReused,
                    "This key already names another committed transaction of the declaring actor. Use a new key for this finalization.",
                )
                .at("/key")
                .with_argv(receipt_argv(&declared.actor, &submission.command_id));
                failure.error.expected =
                    Some(json!({"transaction_id": verified.event.transaction_id}));
                return Err(failure);
            };
            if request.payload != submission.request {
                let mut failure = Failure::new(
                    ErrorCode::IdempotencyKeyReused,
                    "This handle was finalized under this key with a different request. Resend the identical request to get its receipt.",
                )
                .at("/key")
                .with_argv(receipt_argv(&declared.actor, &submission.command_id));
                failure.error.expected = Some(json!({
                    "request_digest": digest_of(&request.payload)?,
                }));
                failure.error.actual = Some(json!({
                    "request_digest": digest_of(&submission.request)?,
                }));
                return Err(failure);
            }
            let mut receipt = receipt_for(verified).as_retry();
            self.apply_projection_health(&replay, &mut receipt);
            return Ok(Some(FinalizedCapture {
                receipt,
                case_id: declared.case_id,
                run_id: declared.run_id,
                handle: declared.binding.handle,
                section,
            }));
        }
        let earlier = replay
            .cases
            .get(&declared.case_id)
            .into_iter()
            .flat_map(|case| &case.events)
            .find(|verified| finalization_of(&verified.event, declared.run_id).is_some());
        if let Some(earlier) = earlier {
            let mut failure = Failure::new(
                ErrorCode::RevisionConflict,
                format!(
                    "This handle is already finalized by command {:?}. A Run is finalized once; look up that receipt, and preserve late evidence through an explicit linked reconciliation.",
                    earlier.event.command_id
                ),
            )
            .at("/handle")
            .with_argv(receipt_argv(&declared.actor, &earlier.event.command_id));
            failure.error.expected = Some(json!({
                "command_id": earlier.event.command_id,
                "manifest_digest": earlier.event.manifest_digest,
            }));
            failure.error.actual = Some(json!({"command_id": submission.command_id}));
            return Err(failure);
        }
        Ok(None)
    }

    fn case_snapshot(&mut self, case_id: Id) -> Result<Snapshot, Failure> {
        let _lock = self.coordinate()?;
        Ok(replay_case(
            &self.fs,
            &case_directory(case_id),
            self.workspace.id(),
            case_id,
        )?
        .snapshot)
    }

    /// The seal of a resolved handle, if finalize has sealed it. A read.
    pub(crate) fn capture_seal(
        &self,
        handle: &ResolvedHandle,
    ) -> Result<Option<CaptureSeal>, Failure> {
        let seal = self.read_seal(handle, &handle.directory.join(SEAL_FILE))?;
        Ok(seal.map(|seal| CaptureSeal {
            command_id: seal.command_id,
            request_digest: seal.request_digest,
            sealed_position: seal.sealed_position,
            manifest_digest: seal.manifest_digest,
        }))
    }

    fn read_seal(&self, handle: &ResolvedHandle, path: &Path) -> Result<Option<SealFile>, Failure> {
        let mut bytes = Vec::new();
        match self.fs.read(path) {
            Ok(reader) => reader
                .take(MAX_SEAL_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(io_failure)?,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_failure(error)),
        };
        let seal: SealFile = serde_json::from_slice(&bytes)
            .ok()
            .filter(|seal: &SealFile| {
                seal.schema_version == SEAL_VERSION
                    && seal.handle == handle.binding.handle
                    && seal.run_id == handle.run_id
            })
            .ok_or_else(|| integrity(format!("Capture seal {} is not valid.", path.display())))?;
        Ok(Some(seal))
    }

    /// Remove `blobs/*.tmp` and staged blobs that no item references. Only
    /// called after the seal, when no new item can reference a blob.
    fn sweep_staged_blobs(
        &mut self,
        handle: &ResolvedHandle,
        items: &[StagedItem],
    ) -> Result<(), Failure> {
        let mut referenced = BTreeSet::new();
        for item in items {
            if let Some(blob) = AttachOutcome::of(item)?.and_then(|outcome| outcome.blob) {
                referenced.insert(blob);
            }
        }
        let directory = handle.directory.join(BLOBS_DIRECTORY);
        let entries = match self.fs.list(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_failure(error)),
        };
        let mut removed = false;
        for entry in entries {
            let Some(name) = entry.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let digest_named =
                name.len() == 64 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
            let unreferenced =
                digest_named && !referenced.contains(&format!("{BLOBS_DIRECTORY}/{name}"));
            if !(name.ends_with(".tmp") || unreferenced) {
                continue;
            }
            match self.fs.remove(&entry) {
                Ok(()) => removed = true,
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(io_failure(error)),
            }
        }
        if removed {
            sync_directory(&mut self.fs, &directory, &self.gate).map_err(durability_failure)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_finalization(
        &mut self,
        declared: &DeclaredHandle,
        handle: &ResolvedHandle,
        items: &[StagedItem],
        submission: &FinalizeSubmission,
        mut plan: FinalizePlan,
        manifest_digest: Digest,
        clock: &mut dyn Clock,
        temporaries: &mut Vec<PathBuf>,
    ) -> Result<FinalizedCapture, Failure> {
        let planned = std::mem::take(&mut plan.artifacts);
        let retained = resolve_plan(&planned, items)?;
        // Copy retained bytes into private temporaries; the commit publishes
        // them into the canonical store before the event. Staging keeps its own.
        let mut attachments = Vec::new();
        let mut blobs = Vec::new();
        for artifact in &retained {
            if attachments.contains(&artifact.digest) {
                continue;
            }
            let blob = artifact
                .outcome
                .blob
                .as_ref()
                .ok_or_else(|| integrity("A retained attachment names no staged blob."))?;
            let path = handle.directory.join(blob);
            let input = self.fs.read(&path).map_err(io_failure)?;
            let staged = self.stage_temporary(input, temporaries)?;
            if Some(staged.digest.as_str()) != artifact.digest.as_str().strip_prefix("sha256:")
                || staged.size != artifact.size
            {
                return Err(integrity(format!(
                    "Staged attachment {} does not match its digest.",
                    path.display()
                )));
            }
            attachments.push(artifact.digest.clone());
            blobs.push(staged);
        }
        let receipt_bytes = self.receipt_bytes(handle, &retained, &plan.receipts)?;
        let manifest_size = plan.manifest.len() as u64;
        let manifest = std::mem::take(&mut plan.manifest);
        blobs.push(self.stage_temporary(Box::new(io::Cursor::new(manifest)), temporaries)?);

        let input = serde_json::to_value(CaptureRequest {
            schema_version: CaptureProtocol::V1,
            operation: FINALIZE_OPERATION.into(),
            command_id: submission.command_id.clone(),
            actor: declared.actor.clone(),
            case_id: Some(handle.case_id),
            payload: submission.request.clone(),
        })
        .map_err(|_| integrity("Could not encode the capture request."))?;
        let mut section = None;
        let records = FinalizationRecords {
            handle,
            actor: &declared.actor,
            retained: &retained,
            receipt_bytes: &receipt_bytes,
            manifest_digest: &manifest_digest,
            manifest_size,
        };
        let receipt = self
            .commit_plan(
                CommitPlan {
                    case_id: Some(handle.case_id),
                    reads: Reads::Target,
                    event: EventInput {
                        actor: &declared.actor,
                        command_id: &submission.command_id,
                        preconditions: &[],
                        manifest_digest: Some(manifest_digest.clone()),
                        session_reads: None,
                    },
                    request: SemanticRequest { input, attachments },
                    blobs: &blobs,
                },
                clock,
                &mut SystemIdSource,
                |state, _, acceptance, _| {
                    let (accepted, built) = records.build(state, acceptance, plan)?;
                    section = Some(built);
                    Ok(Prepared::Accept(accepted))
                },
            )
            .map_err(|failure| {
                if failure.error.code == ErrorCode::DurabilityUnknown {
                    failure.with_argv(receipt_argv(&declared.actor, &submission.command_id))
                } else {
                    failure
                }
            })?;
        let section = match section {
            Some(section) => section,
            // The journal already held this request: read what it committed.
            None => {
                return self
                    .committed_finalization(declared, submission)?
                    .ok_or_else(|| {
                        integrity("The committed finalization could not be read back.")
                    });
            }
        };
        Ok(FinalizedCapture {
            receipt,
            case_id: handle.case_id,
            run_id: handle.run_id,
            handle: handle.binding.handle,
            section,
        })
    }

    /// The bytes of each retained attachment that a receipt with a declared
    /// format names, read from staging up to the receipt limit.
    fn receipt_bytes(
        &mut self,
        handle: &ResolvedHandle,
        retained: &[Retained<'_>],
        receipts: &[PlannedReceipt],
    ) -> Result<BTreeMap<u64, Option<Vec<u8>>>, Failure> {
        let mut read = BTreeMap::new();
        for receipt in receipts {
            let (Some(_), Some(position)) = (receipt.format, receipt.position) else {
                continue;
            };
            let Some(artifact) = retained
                .iter()
                .find(|artifact| artifact.planned.position == position)
            else {
                continue;
            };
            if read.contains_key(&position) {
                continue;
            }
            if artifact.size > MAX_RECEIPT_BYTES as u64 {
                read.insert(position, None);
                continue;
            }
            let blob = artifact
                .outcome
                .blob
                .as_ref()
                .ok_or_else(|| integrity("A retained attachment names no staged blob."))?;
            let mut bytes = Vec::new();
            self.fs
                .read(&handle.directory.join(blob))
                .map_err(io_failure)?
                .take(MAX_RECEIPT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(io_failure)?;
            // The committed bytes are checked against the digest separately;
            // these are the bytes the check reads.
            if bytes.len() as u64 != artifact.size {
                return Err(integrity("A staged receipt does not match its size."));
            }
            read.insert(position, Some(bytes));
        }
        Ok(read)
    }

    fn stage_temporary(
        &mut self,
        input: Box<dyn io::Read>,
        temporaries: &mut Vec<PathBuf>,
    ) -> Result<crate::blob::StagedBlob, Failure> {
        let id = new_id(&mut SystemIdSource, Kind::Import)?;
        let path = Path::new(TMP).join(format!("capture-{id}.tmp"));
        temporaries.push(path.clone());
        stage(&mut self.fs, &self.gate, &path, input).map_err(blob_failure)
    }
}

/// Check the plan against the staged outcomes: every planned position holds a
/// staged attachment with retained bytes, and a derivative's input is the
/// earlier planned original its outcome names.
fn resolve_plan<'a>(
    planned: &'a [PlannedArtifact],
    items: &'a [StagedItem],
) -> Result<Vec<Retained<'a>>, Failure> {
    let mut retained: Vec<Retained<'a>> = Vec::with_capacity(planned.len());
    let mut seen = BTreeSet::new();
    for artifact in planned {
        let item = usize::try_from(artifact.position.saturating_sub(1))
            .ok()
            .and_then(|index| items.get(index))
            .filter(|item| item.position == artifact.position)
            .ok_or_else(|| integrity("A planned artifact names no staged item."))?;
        let outcome = AttachOutcome::of(item)?
            .filter(|outcome| outcome.availability == AttachAvailability::Staged)
            .ok_or_else(|| integrity("A planned artifact has no retained bytes."))?;
        if !seen.insert(artifact.position) {
            return Err(integrity("A staged attachment is planned twice."));
        }
        if let PlannedRole::Derivative { from, .. } = &artifact.role {
            let named = outcome
                .derived_from
                .as_ref()
                .map(|derived| derived.position);
            if named != Some(*from)
                || !retained
                    .iter()
                    .any(|earlier| earlier.planned.position == *from)
            {
                return Err(integrity(
                    "A planned derivative does not follow the original its attachment names.",
                ));
            }
        }
        let (Some(digest), Some(size)) = (outcome.digest.clone(), outcome.size_bytes) else {
            return Err(integrity("A retained attachment has no digest."));
        };
        retained.push(Retained {
            planned: artifact,
            key: &item.key,
            outcome,
            digest,
            size,
        });
    }
    Ok(retained)
}

struct FinalizationRecords<'a> {
    handle: &'a ResolvedHandle,
    actor: &'a Actor,
    retained: &'a [Retained<'a>],
    /// Receipt bytes by attachment position; `None` when over the limit.
    receipt_bytes: &'a BTreeMap<u64, Option<Vec<u8>>>,
    manifest_digest: &'a Digest,
    manifest_size: u64,
}

impl FinalizationRecords<'_> {
    /// The finalizing transaction, decided under the commit lock against the
    /// verified case.
    fn build(
        &self,
        state: &State,
        acceptance: Acceptance<'_>,
        plan: FinalizePlan,
    ) -> Result<(AcceptedTransaction, Value), Failure> {
        let handle = self.handle;
        let run = state
            .current(handle.run_id)
            .filter(|record| record.header.id.kind() == Kind::Run)
            .ok_or_else(|| integrity("The declared Run is missing."))?;
        if run.body["capture"]["handle"] != json!(handle.binding.handle) {
            return Err(integrity("The Run is not bound to this capture handle."));
        }
        if run
            .body
            .get(CAPTURE_FINALIZATION_KEY)
            .is_some_and(|section| !section.is_null())
        {
            return Err(integrity("The Run is already finalized."));
        }
        let level = self
            .retained
            .iter()
            .map(|artifact| artifact.outcome.classification)
            .fold(handle.classification, Ord::max);
        let mut builder = IngestRecords {
            actor: self.actor,
            descriptor: None,
            classification: level,
            case_id: handle.case_id,
            timestamp: acceptance.clock.now_utc(),
            sequence: acceptance.sequence,
            ids: acceptance.ids,
            records: Vec::new(),
            created: Vec::new(),
            updated: Vec::new(),
        };
        let tool = run.body["tool"].as_str().map(str::to_owned);
        let source = SourceBody {
            producer: tool.clone(),
            publisher: None,
            identity: Some(format!(
                "{} run {} capture",
                handle.binding.producer, handle.binding.external_run_id
            )),
            acquisition: SourceAcquisition {
                channel: AcquisitionChannel::Producer,
                acquired_at: Some(SourceTime {
                    timestamp: Knowledge::Known(builder.timestamp.to_string()),
                    offset: Knowledge::Known("+00:00".into()),
                    clock_quality: ClockQuality::Unverified,
                }),
                method: Some("capture staging snapshot".into()),
                context: None,
                environment: None,
            },
            access: if level == ClassificationLevel::Restricted {
                SourceAccess::Restricted
            } else {
                SourceAccess::LocalOnly
            },
            verification_limits: std::iter::once(CAPTURE_LIMIT.to_owned())
                .chain(plan.limitations.iter().cloned())
                .collect(),
            metadata: plan.metadata.clone(),
            acquisition_key: None,
            description_digest: None,
            status: None,
            withdrawals: Vec::new(),
            source_assessment: None,
        };
        let source_id = new_id(builder.ids, Kind::Source)?;
        builder.add(source_id, "capture.source".into(), source, Vec::new())?;
        let source_ref = RevisionRef {
            id: source_id,
            revision: 1,
        };
        let mut refs: Vec<RevisionRef> = Vec::with_capacity(self.retained.len());
        for artifact in self.retained {
            let (role, transforms, derivation, provenance) = match &artifact.planned.role {
                PlannedRole::Original => {
                    (ArtifactRole::Original, Vec::new(), None, vec![source_id])
                }
                PlannedRole::Derivative {
                    from,
                    transformation,
                    version,
                } => {
                    let index = self
                        .retained
                        .iter()
                        .position(|earlier| earlier.planned.position == *from)
                        .filter(|index| *index < refs.len())
                        .ok_or_else(|| integrity("A derivative precedes its input."))?;
                    let input = refs[index];
                    (
                        ArtifactRole::Derivative,
                        vec![ArtifactTransform {
                            input,
                            transformation: transformation.clone(),
                            version: version.clone(),
                            input_digest: Some(self.retained[index].digest.clone()),
                        }],
                        Some(DerivationRecord {
                            label: Some(artifact.key.to_owned()),
                            producer: tool.clone(),
                            ..DerivationRecord::default()
                        }),
                        vec![source_id, input.id],
                    )
                }
            };
            let body = ArtifactBody {
                digest: Some(artifact.digest.clone()),
                size_bytes: Some(artifact.size),
                format: artifact.planned.media_type.clone(),
                source: source_ref,
                transforms,
                availability: ArtifactAvailability::Retained,
                integrity: ArtifactIntegrity::Verified,
                sensitivity: sensitivity(artifact.outcome.classification),
                status: None,
                withdrawals: Vec::new(),
                external: None,
                role: Some(role),
                derivation,
            };
            body.validate_shape()
                .and_then(|()| body.validate_references())
                .map_err(|blocker| integrity(blocker.code()))?;
            builder.classification = artifact.outcome.classification;
            let id = new_id(builder.ids, Kind::Artifact)?;
            builder.add(
                id,
                format!("artifact:{}", artifact.planned.position),
                body,
                provenance,
            )?;
            refs.push(RevisionRef { id, revision: 1 });
        }
        builder.classification = level;
        let manifest_id = new_id(builder.ids, Kind::Artifact)?;
        builder.add(
            manifest_id,
            "manifest".into(),
            ArtifactBody {
                digest: Some(self.manifest_digest.clone()),
                size_bytes: Some(self.manifest_size),
                format: Some("application/json".into()),
                source: source_ref,
                transforms: Vec::new(),
                availability: ArtifactAvailability::Retained,
                integrity: ArtifactIntegrity::Verified,
                sensitivity: sensitivity(level),
                status: None,
                withdrawals: Vec::new(),
                external: None,
                role: Some(ArtifactRole::Manifest),
                derivation: None,
            },
            vec![source_id],
        )?;
        let manifest_ref = RevisionRef {
            id: manifest_id,
            revision: 1,
        };
        let refs_by_position: BTreeMap<u64, RevisionRef> = self
            .retained
            .iter()
            .zip(&refs)
            .map(|(artifact, reference)| (artifact.planned.position, *reference))
            .collect();
        let section = (plan.section)(&RecordedRefs {
            source: source_ref,
            manifest: manifest_ref,
            manifest_digest: self.manifest_digest.clone(),
            manifest_size: self.manifest_size,
            artifacts: refs.clone(),
        })?;

        let mut body = run.body.clone();
        let mut artifact_refs: Vec<Reference> =
            investigation_core::input::decode(&body["artifact_refs"]).unwrap_or_default();
        for reference in std::iter::once(manifest_ref).chain(refs) {
            let reference = Reference::Existing(reference);
            if !artifact_refs.contains(&reference) {
                artifact_refs.push(reference);
            }
        }
        body["artifact_refs"] = json!(artifact_refs);
        body["manifest_digest"] = json!(self.manifest_digest);
        body["execution_status"] = json!(plan.execution_status);
        body["evidence_completeness"] = json!(plan.evidence_completeness);
        if let Some(started_at) = &plan.started_at {
            body["started_at"] = json!(started_at);
        }
        if let Some(finished_at) = &plan.finished_at {
            body["finished_at"] = json!(finished_at);
        }
        body[CAPTURE_FINALIZATION_KEY] = section.clone();
        let receipts: Vec<ReceiptInput<'_>> = plan
            .receipts
            .iter()
            .map(|receipt| self.receipt_input(receipt, &refs_by_position))
            .collect();
        let mut warnings = plan.warnings;
        warnings.extend(receipt_check::record_receipts(&mut body, &receipts));
        builder.classification = level.max(run.header.classification.level);
        builder.revise(run, body)?;
        Ok((
            AcceptedTransaction {
                case_id: handle.case_id,
                sequence: builder.sequence,
                records: builder.records,
                created: builder.created,
                updated: builder.updated,
                warnings,
            },
            section,
        ))
    }
}

impl FinalizationRecords<'_> {
    /// One receipt as the core checks it: its recorded Artifact, digest and
    /// staged bytes, when an attachment holds them.
    fn receipt_input<'r>(
        &'r self,
        receipt: &'r PlannedReceipt,
        refs: &BTreeMap<u64, RevisionRef>,
    ) -> ReceiptInput<'r> {
        let artifact = receipt.position.and_then(|position| {
            self.retained
                .iter()
                .find(|artifact| artifact.planned.position == position)
        });
        ReceiptInput {
            kind: &receipt.kind,
            format: receipt.format,
            artifact_ref: receipt
                .position
                .and_then(|position| refs.get(&position).copied()),
            digest: artifact.map(|artifact| artifact.digest.clone()),
            bytes: match receipt
                .position
                .and_then(|position| self.receipt_bytes.get(&position))
            {
                Some(Some(bytes)) => ReceiptBytes::Retained(bytes),
                Some(None) => ReceiptBytes::TooLarge,
                None => ReceiptBytes::Unavailable,
            },
        }
    }
}

/// The finalization section a committed event set on the Run, when the event
/// is the Run's `capture.finalize`.
pub(crate) fn finalization_of(event: &Event, run_id: Id) -> Option<Value> {
    let request: CaptureRequest = investigation_core::input::decode(&event.request.input).ok()?;
    if request.operation != FINALIZE_OPERATION {
        return None;
    }
    event
        .accepted
        .records
        .iter()
        .find(|record| record.header.id == run_id)
        .and_then(|record| record.body.get(CAPTURE_FINALIZATION_KEY))
        .filter(|section| !section.is_null())
        .cloned()
}

fn digest_of(value: &Value) -> Result<Digest, Failure> {
    serde_json::to_vec(value)
        .map(|bytes| hash_bytes(&bytes))
        .map_err(|_| integrity("Could not encode the finalize request."))
}

fn sealed_by_another(seal: &SealFile, submission: &FinalizeSubmission, digest: &Digest) -> Failure {
    let same_key = seal.command_id == submission.command_id;
    let (code, path, message) = if same_key {
        (
            ErrorCode::IdempotencyKeyReused,
            "/key",
            "This handle was sealed under this key with a different finalize request. Resend the identical request to complete it.",
        )
    } else {
        (
            ErrorCode::RevisionConflict,
            "/handle",
            "This handle is sealed by another finalize request that has not committed yet. Resend that identical request to complete finalization; a sealed handle accepts no other.",
        )
    };
    let mut failure = Failure::new(code, message).at(path);
    failure.error.expected = Some(json!({
        "command_id": seal.command_id,
        "request_digest": seal.request_digest,
        "sealed_position": seal.sealed_position,
    }));
    failure.error.actual = Some(json!({
        "command_id": submission.command_id,
        "request_digest": digest,
    }));
    failure.error.retry = Some("resend the sealing finalize request".into());
    failure.with_argv(recover_argv(submission.handle))
}

#[cfg(test)]
#[path = "capture_finalize_tests.rs"]
mod tests;
