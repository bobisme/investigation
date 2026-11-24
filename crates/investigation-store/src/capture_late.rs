//! Items that reach a handle after it is sealed, and identical retries after
//! its staging is retired.
//!
//! A sealed manifest is immutable. Quietly accepting a late item would falsify
//! it, and dropping the item would lose evidence, so a closed handle does
//! neither:
//!
//! - A new item on a sealed or finalized handle gets a closed-handle response
//!   (`REVISION_CONFLICT` at `/handle`). It names the manifest that closed the
//!   handle, and the producer preserves the late evidence as a new attributed
//!   record linked to the Run: an `ingest` with its own acquisition time, then
//!   a `run.reconcile` that links the new Artifact. Neither touches the
//!   manifest or the Run's frozen `capture_finalization` section.
//! - An identical retry of an accepted item returns its original receipt. While
//!   staging is present it answers; once the Run is finalized, the manifest
//!   Artifact answers, also after staging is retired.
//!
//! The last item and the seal race only under the handle lock: finalize seals
//! while holding it, and an item checks the seal while holding it. The loser of
//! that race is either a finalize that sees the item (and includes it, or
//! refuses a stale cursor without sealing) or an item that sees the seal and
//! gets the closed-handle response. No acknowledged item is left out.
//!
//! # The item map
//!
//! The finalized manifest binds every staged item under `items`: `position`,
//! `key`, `operation`, `request_digest`, `accepted_at`, `request` and, when
//! present, `outcome`. The recorder writes the rest of the manifest; the store
//! reads only this map, which it staged itself, and finalize refuses to seal a
//! manifest whose map differs from the staged items.

use std::io::Read as _;

use investigation_core::{
    command::{Reference, RevisionRef},
    evidence::{ArtifactBody, ArtifactRole},
    header::UtcTimestamp,
    producer_run::CAPTURE_FINALIZATION_KEY,
    state::Record,
    wire::Digest,
};

use super::{
    finalize::{CaptureSeal, finalization_of},
    items::{
        ITEM_VERSION, ItemAcceptance, ItemOperation, ItemResult, ItemSubmission, ResolvedHandle,
        SEAL_FILE, StagedItem, key_reused,
    },
    *,
};

/// The manifest Artifact a finalized capture Run holds, with the item map it
/// binds.
#[derive(Debug, Clone, PartialEq)]
pub struct FinalizedManifest {
    /// The manifest Artifact revision on the Run.
    pub artifact_ref: RevisionRef,
    pub digest: Digest,
    pub size_bytes: u64,
    /// Every item the handle accepted, in position order, as it was staged.
    pub items: Vec<StagedItem>,
}

/// The item map of a manifest; the rest of the document is the recorder's.
#[derive(Deserialize)]
struct ItemMap {
    items: Vec<BoundItem>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundItem {
    position: u64,
    key: String,
    operation: ItemOperation,
    request_digest: Digest,
    accepted_at: UtcTimestamp,
    request: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<Value>,
}

/// What closed a handle, for the closed-handle response.
enum Closure<'a> {
    Sealed(Option<&'a CaptureSeal>),
    Finalized {
        command_id: &'a str,
        case_id: Id,
        run_id: Id,
        manifest: &'a FinalizedManifest,
    },
}

/// Whether a Run record holds a committed capture finalization.
pub(crate) fn is_finalized(run: &Record) -> bool {
    run.body
        .get(CAPTURE_FINALIZATION_KEY)
        .is_some_and(|section| !section.is_null())
}

/// The staged items a manifest binds, checked to be contiguous from 1,
/// uniquely keyed and consistent with their digests.
pub(crate) fn bound_items(
    manifest: &[u8],
    handle: Id,
    run_id: Id,
) -> Result<Vec<StagedItem>, Failure> {
    let corrupt = || integrity("The capture manifest does not bind a valid item map.");
    let map: ItemMap = serde_json::from_slice(manifest).map_err(|_| corrupt())?;
    let mut keys = BTreeSet::new();
    let mut items = Vec::with_capacity(map.items.len());
    for (expected, bound) in (1_u64..).zip(map.items) {
        let request = serde_json::to_vec(&bound.request).map_err(|_| corrupt())?;
        if bound.position != expected
            || hash_bytes(&request) != bound.request_digest
            || !keys.insert(bound.key.clone())
        {
            return Err(corrupt());
        }
        items.push(StagedItem {
            schema_version: ITEM_VERSION.into(),
            operation: bound.operation,
            handle,
            run_id,
            position: bound.position,
            key: bound.key,
            request_digest: bound.request_digest,
            accepted_at: bound.accepted_at,
            request: bound.request,
            outcome: bound.outcome,
        });
    }
    Ok(items)
}

/// The item map a manifest must bind for `items`, for planners in tests.
#[cfg(test)]
pub(crate) fn item_map(items: &[StagedItem]) -> Value {
    json!(
        items
            .iter()
            .map(|item| BoundItem {
                position: item.position,
                key: item.key.clone(),
                operation: item.operation,
                request_digest: item.request_digest.clone(),
                accepted_at: item.accepted_at.clone(),
                request: item.request.clone(),
                outcome: item.outcome.clone(),
            })
            .collect::<Vec<_>>()
    )
}

impl<F: DurableFs> Store<F> {
    /// The manifest of a finalized capture Run, read from the canonical blob
    /// store and verified against its digest, or `None` while the Run is not
    /// finalized. A read.
    pub(crate) fn finalized_manifest(
        &self,
        state: &State,
        run_id: Id,
        handle: Id,
    ) -> Result<Option<FinalizedManifest>, Failure> {
        let run = state
            .current(run_id)
            .ok_or_else(|| integrity("The declared Run is missing."))?;
        if !is_finalized(run) {
            return Ok(None);
        }
        let missing = || integrity("The finalized Run names no valid manifest Artifact.");
        let digest: Digest = investigation_core::input::decode(&run.body["manifest_digest"])
            .map_err(|_| missing())?;
        let references: Vec<Reference> =
            investigation_core::input::decode(&run.body["artifact_refs"]).map_err(|_| missing())?;
        let (artifact_ref, size_bytes) = references
            .iter()
            .filter_map(|reference| match reference {
                Reference::Existing(reference) => Some(*reference),
                Reference::Client(_) => None,
            })
            .find_map(|reference| {
                let record = state
                    .history(reference.id)
                    .find(|record| record.header.revision.0 == reference.revision)?;
                let body: ArtifactBody = investigation_core::input::decode(&record.body).ok()?;
                (body.role == Some(ArtifactRole::Manifest) && body.digest.as_ref() == Some(&digest))
                    .then_some((reference, body.size_bytes?))
            })
            .ok_or_else(missing)?;
        if investigation_core::retention_ops::bytes_withdrawn(state, None, artifact_ref.id) {
            return Err(Failure::new(
                ErrorCode::GateFailed,
                "The availability of this Run's capture manifest was withdrawn (artifact.expire), so it is not read.",
            ));
        }
        let hex = digest
            .as_str()
            .strip_prefix("sha256:")
            .filter(|hex| hex.len() == 64)
            .ok_or_else(missing)?;
        let path = Path::new(BLOBS).join(&hex[..2]).join(hex);
        let mut bytes = Vec::new();
        self.fs
            .read(&path)
            .and_then(|reader| reader.take(size_bytes + 1).read_to_end(&mut bytes))
            .map_err(|_| {
                integrity("The capture manifest blob could not be opened.")
                    .with_argv(read_argv("check"))
            })?;
        if bytes.len() as u64 != size_bytes || hash_bytes(&bytes) != digest {
            return Err(
                integrity("The capture manifest blob does not match its digest.")
                    .with_argv(read_argv("check")),
            );
        }
        Ok(Some(FinalizedManifest {
            artifact_ref,
            digest,
            size_bytes,
            items: bound_items(&bytes, handle, run_id)?,
        }))
    }

    /// Answer an item on a handle that may be closed, under its handle lock,
    /// for a key that staging does not hold. `None` while the handle is open.
    pub(crate) fn closed_item(
        &mut self,
        handle: &ResolvedHandle,
        submission: &ItemSubmission,
        digest: &Digest,
    ) -> Result<Option<Result<ItemAcceptance, Failure>>, Failure> {
        if handle.finalized {
            let replay = self.case(handle.case_id)?;
            return self
                .answer_from_manifest(&replay, handle.case_id, handle.run_id, submission, digest)
                .map(Some);
        }
        if !self.exists(&handle.directory.join(SEAL_FILE))? {
            return Ok(None);
        }
        // Presence closes the handle. An unreadable seal still closes it; recover
        // and finalize report the seal itself.
        let seal = self.capture_seal(handle).ok().flatten();
        Ok(Some(Err(closed_handle(
            submission,
            digest,
            &Closure::Sealed(seal.as_ref()),
        ))))
    }

    /// Answer an item whose handle has no staging. A finalized handle's
    /// manifest answers; any other handle keeps the original refusal.
    pub(crate) fn retired_item(
        &mut self,
        submission: &ItemSubmission,
        digest: &Digest,
        refusal: Failure,
    ) -> Result<ItemAcceptance, Failure> {
        if refusal.error.code != ErrorCode::ObjectRequired {
            return Err(refusal);
        }
        let declared = self
            .capture_handles()?
            .into_iter()
            .find(|declared| declared.binding.handle == submission.handle);
        let Some(declared) = declared else {
            return Err(refusal);
        };
        let replay = self.case(declared.case_id)?;
        let finalized = replay
            .state
            .current(declared.run_id)
            .is_some_and(is_finalized);
        if !finalized {
            return Err(refusal);
        }
        self.answer_from_manifest(
            &replay,
            declared.case_id,
            declared.run_id,
            submission,
            digest,
        )?
    }

    /// The original receipt of an item the finalized manifest binds, a
    /// conflict for a changed retry, or the closed-handle response.
    fn answer_from_manifest(
        &self,
        replay: &CaseReplay,
        case_id: Id,
        run_id: Id,
        submission: &ItemSubmission,
        digest: &Digest,
    ) -> Result<Result<ItemAcceptance, Failure>, Failure> {
        let manifest = self
            .finalized_manifest(&replay.state, run_id, submission.handle)?
            .ok_or_else(|| integrity("The finalized Run has no manifest."))?;
        if let Some(item) = manifest
            .items
            .iter()
            .find(|item| item.key == submission.key)
        {
            if item.operation == submission.operation
                && &item.request_digest == digest
                && item.request == submission.request
            {
                return Ok(Ok(ItemAcceptance {
                    result: ItemResult::AlreadyAccepted,
                    case_id,
                    item: item.clone(),
                }));
            }
            return Ok(Err(key_reused(item, submission, digest)));
        }
        let command_id = replay
            .events
            .iter()
            .find(|verified| finalization_of(&verified.event, run_id).is_some())
            .map(|verified| verified.event.command_id.as_str())
            .ok_or_else(|| integrity("The finalized Run has no finalizing transaction."))?;
        Ok(Err(closed_handle(
            submission,
            digest,
            &Closure::Finalized {
                command_id,
                case_id,
                run_id,
                manifest: &manifest,
            },
        )))
    }
}

fn closed_handle(submission: &ItemSubmission, digest: &Digest, closure: &Closure<'_>) -> Failure {
    let (message, expected) = match closure {
        Closure::Sealed(None) => (
            "This capture handle is sealed and accepts no new items. Call recover for its seal state. Preserve late evidence as a new attributed record linked to the Run: ingest it, then link its Artifact to the Run with run.reconcile.".to_owned(),
            json!({"seal": "sealed"}),
        ),
        Closure::Sealed(Some(seal)) => (
            format!(
                "This capture handle is sealed by finalize request {:?} and accepts no new items; the manifest {} it seals cannot change. Resend that finalize request to complete the handle. Preserve late evidence as a new attributed record linked to the Run: ingest it, then link its Artifact to the Run with run.reconcile.",
                seal.command_id, seal.manifest_digest
            ),
            json!({
                "seal": "sealed",
                "command_id": seal.command_id,
                "request_digest": seal.request_digest,
                "sealed_position": seal.sealed_position,
                "manifest_digest": seal.manifest_digest,
            }),
        ),
        Closure::Finalized {
            command_id,
            case_id,
            run_id,
            manifest,
        } => (
            format!(
                "This capture handle is finalized and accepts no new items; its manifest {} is immutable. Preserve late evidence as a new attributed record linked to the Run: ingest it into case {case_id}, then link its Artifact to Run {run_id} with run.reconcile.",
                manifest.digest
            ),
            json!({
                "seal": "finalized",
                "command_id": command_id,
                "sealed_position": manifest.items.last().map(|item| item.position),
                "manifest_digest": manifest.digest,
                "manifest_ref": manifest.artifact_ref,
                "case_id": case_id,
                "run_id": run_id,
            }),
        ),
    };
    let mut failure = Failure::new(ErrorCode::RevisionConflict, message)
        .at("/handle")
        .with_argv(recover_argv(submission.handle));
    failure.error.expected = Some(expected);
    failure.error.actual = Some(json!({
        "key": submission.key,
        "position": submission.position,
        "operation": submission.operation,
        "request_digest": digest,
    }));
    failure
}

#[cfg(test)]
#[path = "capture_late_tests.rs"]
mod tests;
