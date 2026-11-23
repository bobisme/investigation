//! Durable items on a capture handle: appended facts (`capture.append`) and,
//! sharing the same positions, attachment descriptors (`capture.attach`).
//!
//! Items are staged, not journaled. Each accepted item is one immutable file in
//! the handle's staging directory; no item is a journal transaction. Finalize
//! seals staging and summarizes the items into the Run's canonical manifest in
//! one transaction, so a stream of facts never becomes a stream of global
//! commits.
//!
//! # Layout
//!
//! ```text
//! state/staging/<run-id>/
//!   handle.json                    binding written by begin (read only here)
//!   handle.lock                    persistent per-handle lock; never removed or renamed
//!   items/<position:012>.json      0400, one accepted item
//!   keys/<sha256(key)>.json        0400, item key -> position
//!   items/*.tmp, keys/*.tmp        transient; only while one entry is published
//!   sealed.json                    the seal finalize publishes; its presence closes the handle
//! ```
//!
//! An item file holds `schema_version` (`inv.capture-item.v1`), `operation`,
//! `handle`, `run_id`, `position`, `key`, `request_digest`, `accepted_at`, the
//! normalized `request` and an optional operation-specific `outcome`. The
//! digest is SHA-256 over the compact JSON of `request` with sorted keys.
//!
//! # Invariants
//!
//! - Positions are contiguous from 1. Under the handle lock, item `p` is
//!   published only after item `p - 1` is durable, so the cursor is the highest
//!   published item. There is no mutable cursor file to replace.
//! - An item's bytes are synced, published by exclusive link and its directory
//!   synced before its receipt is returned. That one file is the item's bytes,
//!   its receipt and the cursor advance.
//! - Every item has a key entry, except possibly the last one after a crash.
//!   Every accepting call repairs that entry before it reads the key index.
//! - An identical retry returns the original receipt, including its acceptance
//!   time. The same key with a different request conflicts.
//!
//! Lock order: the handle lock, then the workspace commit lock, never the
//! reverse. The commit lock is held only to read the handle's case.

use std::io::{ErrorKind, Read as _};

use investigation_core::{
    classification::effective_classification,
    header::{Clock, UtcTimestamp},
    wire::Digest,
};

use super::*;
use crate::{
    durable::sync_existing_file,
    journal::{hash_bytes, replay_case},
};

/// Directory of accepted items inside a handle's staging directory.
pub const ITEMS_DIRECTORY: &str = "items";
/// Directory of the item-key index inside a handle's staging directory.
pub const KEYS_DIRECTORY: &str = "keys";
/// Persistent lock file that serializes calls on one handle.
pub const HANDLE_LOCK_FILE: &str = "handle.lock";
/// Seal marker that finalization publishes. Its presence closes the handle.
pub const SEAL_FILE: &str = "sealed.json";
const ITEM_VERSION: &str = "inv.capture-item.v1";
const KEY_VERSION: &str = "inv.capture-key.v1";
/// Item file names have twelve digits, like journal events.
const MAX_POSITION: u64 = 999_999_999_999;
/// Room for an item's envelope and outcome beyond its bounded request.
const ITEM_ENVELOPE_BYTES: usize = 64 * 1024;
const MAX_OUTCOME_BYTES: usize = 16 * 1024;
const MAX_KEY_FILE_BYTES: u64 = 4 * 1024;

/// Which capture operation accepted an item. Both share one position sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemOperation {
    #[serde(rename = "capture.append")]
    Append,
    #[serde(rename = "capture.attach")]
    Attach,
}

/// One item a recorder asks the store to accept at a position.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemSubmission {
    pub operation: ItemOperation,
    pub handle: Id,
    /// Producer item key, unique within the handle.
    pub key: String,
    /// The position the producer expects the item to take.
    pub position: u64,
    /// Normalized recorder request. A retry compares it structurally, and its
    /// serialized size is bounded by the handle's `max_event_bytes`.
    pub request: Value,
    /// Operation-specific durable result decided before acceptance, such as an
    /// attachment's staged digest. It is not part of the request digest; a
    /// retry returns the outcome of the first acceptance.
    pub outcome: Option<Value>,
}

/// One accepted item as staged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedItem {
    pub schema_version: String,
    pub operation: ItemOperation,
    pub handle: Id,
    pub run_id: Id,
    pub position: u64,
    pub key: String,
    pub request_digest: Digest,
    /// Local acceptance time, distinct from any time the producer reports.
    pub accepted_at: UtcTimestamp,
    pub request: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyEntry {
    schema_version: String,
    key: String,
    position: u64,
}

/// First acceptance or replay of an earlier acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemResult {
    Accepted,
    AlreadyAccepted,
}

/// A durable item and how this call obtained it.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemAcceptance {
    pub result: ItemResult,
    pub case_id: Id,
    pub item: StagedItem,
}

/// Every staged item of one handle, in position order.
#[derive(Debug, Clone, PartialEq)]
pub struct HandleItems {
    pub case_id: Id,
    pub run_id: Id,
    pub binding: CaptureBinding,
    /// Whether the seal marker is present.
    pub sealed: bool,
    pub items: Vec<StagedItem>,
}

/// A handle's staging directory, as its `handle.json` names it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LocatedHandle {
    pub handle: Id,
    pub case_id: Id,
    pub run_id: Id,
    pub directory: PathBuf,
}

/// A handle whose staging binding agrees with the journal.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResolvedHandle {
    pub case_id: Id,
    pub run_id: Id,
    pub binding: CaptureBinding,
    pub directory: PathBuf,
    /// Effective classification of the declared Run: the floor for its items.
    pub classification: ClassificationLevel,
}

/// The fields of `handle.json` that locate a handle.
#[derive(Deserialize)]
struct BindingFile {
    schema_version: String,
    handle: Id,
    run_id: Id,
    case_id: Id,
}

impl<F: DurableFs> Store<F> {
    /// Accept one item at the next position of its handle, durably, or return
    /// the original receipt of an identical earlier call.
    ///
    /// The commit lock is held only to read the handle's case, never while the
    /// item is written; a stream of items commits no journal transactions.
    pub fn stage_capture_item(
        &mut self,
        submission: &ItemSubmission,
        clock: &mut dyn Clock,
    ) -> Result<ItemAcceptance, Failure> {
        let outcome = submission.outcome.clone();
        self.stage_item_with(submission, clock, move |_, _| Ok(outcome))
    }

    /// Accept one item, deciding its outcome only once the item is known to be
    /// new, in order and within bounds, and while the handle lock is held.
    ///
    /// `outcome` runs at most once, never for a replay, and never under the
    /// commit lock; an error from it stages nothing and leaves the position
    /// free. It replaces `submission.outcome`.
    pub(crate) fn stage_item_with<O>(
        &mut self,
        submission: &ItemSubmission,
        clock: &mut dyn Clock,
        outcome: O,
    ) -> Result<ItemAcceptance, Failure>
    where
        O: FnOnce(&mut Self, &ResolvedHandle) -> Result<Option<Value>, Failure>,
    {
        self.gate.check().map_err(durability_failure)?;
        let located = self.locate_handle(submission.handle)?;
        let _handle = self.lock_handle(&located.directory)?;
        let handle = self.verify_handle(located)?;
        let limits = handle.binding.limits;
        let request = serde_json::to_vec(&submission.request)
            .map_err(|_| integrity("Could not encode the capture item."))?;
        let digest = hash_bytes(&request);
        let items = handle.directory.join(ITEMS_DIRECTORY);
        let keys = handle.directory.join(KEYS_DIRECTORY);

        let last = self.last_position(&items, &limits)?;
        if let Some(last) = last {
            let item = self.read_item(&handle, last)?;
            self.ensure_key(&keys, &item)?;
        }
        if let Some(position) = self.key_position(&keys, &submission.key)? {
            let item = self.read_item(&handle, position)?;
            if item.key != submission.key {
                return Err(integrity(
                    "A capture key entry names an item with another key.",
                ));
            }
            if item.operation == submission.operation
                && item.request_digest == digest
                && item.request == submission.request
            {
                // A crashed first call may have published without syncing.
                self.sync_published(&handle.directory, &items, position)?;
                return Ok(ItemAcceptance {
                    result: ItemResult::AlreadyAccepted,
                    case_id: handle.case_id,
                    item,
                });
            }
            return Err(key_reused(&item, submission, &digest));
        }

        if self.exists(&handle.directory.join(SEAL_FILE))? {
            return Err(Failure::new(
                ErrorCode::RevisionConflict,
                "This capture handle is sealed and accepts no new items. Preserve late evidence through an explicit linked reconciliation.",
            )
            .at("/handle"));
        }
        if u64::try_from(request.len()).unwrap_or(u64::MAX) > limits.max_event_bytes {
            let mut failure = Failure::new(
                ErrorCode::InvalidInput,
                format!(
                    "The serialized item is {} bytes; this handle accepts at most {}. Attach high-frequency samples as an artifact instead.",
                    request.len(),
                    limits.max_event_bytes
                ),
            );
            failure.error.expected = Some(json!({"max_event_bytes": limits.max_event_bytes}));
            failure.error.actual = Some(json!({"bytes": request.len()}));
            return Err(failure);
        }
        let next = last.map_or(1, |last| last + 1);
        if submission.position > limits.max_items.min(MAX_POSITION) {
            let mut failure = Failure::new(
                ErrorCode::InvalidInput,
                format!(
                    "This handle accepts at most {} items. Finalize it and attach further data as an artifact.",
                    limits.max_items
                ),
            )
            .at("/position");
            failure.error.expected = Some(json!({"max_items": limits.max_items}));
            failure.error.actual = Some(json!({"position": submission.position}));
            return Err(failure);
        }
        if submission.position != next {
            return Err(position_conflict(submission.position, next, last));
        }
        let outcome = outcome(self, &handle)?;
        let encoded = outcome
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|_| integrity("Could not encode the capture item outcome."))?;
        if encoded.is_some_and(|bytes| bytes.len() > MAX_OUTCOME_BYTES) {
            return Err(integrity("A capture item outcome exceeds its bound."));
        }

        self.ensure_directory(&handle.directory, &items)?;
        self.ensure_directory(&handle.directory, &keys)?;
        let item = StagedItem {
            schema_version: ITEM_VERSION.into(),
            operation: submission.operation,
            handle: submission.handle,
            run_id: handle.run_id,
            position: next,
            key: submission.key.clone(),
            request_digest: digest,
            accepted_at: clock.now_utc(),
            request: submission.request.clone(),
            outcome,
        };
        let mut bytes = serde_json::to_vec(&item)
            .map_err(|_| integrity("Could not encode the capture item."))?;
        bytes.push(b'\n');
        self.publish_entry(&items, &item_name(next), &bytes)?;
        self.ensure_key(&keys, &item)?;
        Ok(ItemAcceptance {
            result: ItemResult::Accepted,
            case_id: handle.case_id,
            item,
        })
    }

    /// Every staged item of a handle, in position order, with its seal state.
    ///
    /// This is a read: it creates nothing and takes no handle lock. Items are
    /// published one file at a time in position order, so a read concurrent
    /// with an append returns a durable prefix. A gap, a duplicate key or an
    /// item that disagrees with its digest is an integrity failure.
    pub fn capture_items(&mut self, handle: Id) -> Result<HandleItems, Failure> {
        self.gate.check().map_err(durability_failure)?;
        let located = self.locate_handle(handle)?;
        let resolved = self.verify_handle(located)?;
        let items = self.staged_items(&resolved)?;
        let sealed = self.exists(&resolved.directory.join(SEAL_FILE))?;
        Ok(HandleItems {
            case_id: resolved.case_id,
            run_id: resolved.run_id,
            binding: resolved.binding,
            sealed,
            items,
        })
    }

    /// Every staged item of a resolved handle, in position order, verified to
    /// be contiguous, uniquely keyed and consistent with its digest.
    pub(crate) fn staged_items(
        &self,
        resolved: &ResolvedHandle,
    ) -> Result<Vec<StagedItem>, Failure> {
        let items_dir = resolved.directory.join(ITEMS_DIRECTORY);
        let mut positions = Vec::new();
        match self.fs.list(&items_dir) {
            Ok(entries) => {
                for entry in entries {
                    let Some(name) = entry.file_name().and_then(|name| name.to_str()) else {
                        return Err(integrity("A capture item has a non-UTF-8 name."));
                    };
                    if name.ends_with(".tmp") {
                        continue;
                    }
                    let position = name
                        .strip_suffix(".json")
                        .filter(|digits| digits.len() == 12)
                        .and_then(|digits| digits.parse::<u64>().ok())
                        .ok_or_else(|| {
                            integrity("Capture items directory has an invalid entry.")
                        })?;
                    positions.push(position);
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(io_failure(error)),
        }
        positions.sort_unstable();
        let mut items = Vec::with_capacity(positions.len());
        let mut keys = BTreeSet::new();
        for (expected, position) in (1..).zip(positions) {
            if position != expected {
                return Err(integrity(format!(
                    "Capture item {expected} is missing before item {position}."
                )));
            }
            let item = self.read_item(resolved, position)?;
            if !keys.insert(item.key.clone()) {
                return Err(integrity("Two capture items share one key."));
            }
            items.push(item);
        }
        Ok(items)
    }

    /// Find a handle's staging directory from its `handle.json` binding, or
    /// explain why it has none.
    pub(crate) fn locate_handle(&mut self, handle: Id) -> Result<LocatedHandle, Failure> {
        let root = Path::new(STAGING_ROOT);
        let entries = match self.fs.list(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(io_failure(error)),
        };
        for directory in entries {
            let bytes = match read_bounded(&self.fs, &directory.join(BINDING_FILE), 64 * 1024) {
                Ok(bytes) => bytes,
                Err(error)
                    if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
                {
                    continue;
                }
                Err(error) => return Err(io_failure(error)),
            };
            let binding: BindingFile = serde_json::from_slice(&bytes).map_err(|_| {
                integrity(format!(
                    "Staging binding {} is not valid.",
                    directory.display()
                ))
            })?;
            if binding.handle != handle {
                continue;
            }
            if binding.schema_version != STAGING_VERSION
                || directory.file_name().and_then(|name| name.to_str())
                    != Some(binding.run_id.to_string().as_str())
            {
                return Err(integrity(format!(
                    "Staging binding {} does not name its own directory.",
                    directory.display()
                )));
            }
            return Ok(LocatedHandle {
                handle,
                case_id: binding.case_id,
                run_id: binding.run_id,
                directory,
            });
        }
        // Not staged: either never declared, or declared and never allocated.
        let declared = self
            .capture_handles()?
            .into_iter()
            .find(|declared| declared.binding.handle == handle);
        match declared {
            Some(declared) if declared.staging == StagingState::Present => Ok(LocatedHandle {
                handle,
                case_id: declared.case_id,
                run_id: declared.run_id,
                directory: PathBuf::from(&declared.binding.staging),
            }),
            Some(declared) => {
                let mut failure = Failure::new(
                    ErrorCode::ObjectRequired,
                    "This handle was declared but its staging was never allocated. Resend the identical begin request, then retry this request.",
                )
                .at("/handle");
                failure.error.expected = Some(json!({
                    "begin_command_id": declared.command_id,
                    "run_id": declared.run_id,
                }));
                Err(failure)
            }
            None => Err(Failure::new(
                ErrorCode::UnknownReference,
                format!(
                    "No capture handle {handle} is declared in this workspace. Use the handle from the begin receipt."
                ),
            )
            .at("/handle")),
        }
    }

    /// Check the staged binding against the declaring transaction and take the
    /// limits fixed at declaration from the journal.
    pub(crate) fn verify_handle(
        &mut self,
        located: LocatedHandle,
    ) -> Result<ResolvedHandle, Failure> {
        let replay = {
            let _lock = self.coordinate()?;
            replay_case(
                &self.fs,
                &case_directory(located.case_id),
                self.workspace.id(),
                located.case_id,
            )?
        };
        let binding = binding_of(&replay.state, located.run_id)?;
        if binding.handle != located.handle || Path::new(&binding.staging) != located.directory {
            return Err(integrity(
                "The staging binding disagrees with the declared capture handle.",
            ));
        }
        let run = replay
            .state
            .current(located.run_id)
            .ok_or_else(|| integrity("The declared Run is missing."))?;
        let classification = effective_classification(&replay.state, run).level;
        Ok(ResolvedHandle {
            case_id: located.case_id,
            run_id: located.run_id,
            binding,
            directory: located.directory,
            classification,
        })
    }

    /// Take the persistent per-handle lock, creating its file on first use.
    pub(crate) fn lock_handle(&mut self, directory: &Path) -> Result<F::Guard, Failure> {
        let path = directory.join(HANDLE_LOCK_FILE);
        let busy = |error| match error {
            LockError::Busy { waited } => Failure::new(
                ErrorCode::WorkspaceBusy,
                format!(
                    "Timed out after {} ms waiting for another call on this capture handle. Retry the identical request.",
                    waited.as_millis()
                ),
            ),
            LockError::Io(error) => io_failure(error),
        };
        match self.fs.lock(&path, WAIT_FOR_LOCK) {
            Err(LockError::Io(error)) if error.kind() == ErrorKind::NotFound => {
                match self.fs.create_private(&path) {
                    Ok(_) => {}
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(io_failure(error)),
                }
                self.fs.lock(&path, WAIT_FOR_LOCK).map_err(busy)
            }
            result => result.map_err(busy),
        }
    }

    /// The highest published position, found by probing: positions are
    /// contiguous, so existence is monotone.
    fn last_position(&self, items: &Path, limits: &HandleLimits) -> Result<Option<u64>, Failure> {
        let bound = limits.max_items.min(MAX_POSITION);
        let present = |position: u64| -> Result<bool, Failure> {
            if position > bound {
                return Ok(false);
            }
            self.exists(&items.join(item_name(position)))
        };
        if !present(1)? {
            return Ok(None);
        }
        let (mut low, mut high) = (1_u64, 2_u64);
        while present(high)? {
            low = high;
            high = high.saturating_mul(2);
        }
        // `low` is present and `high` is absent.
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            if present(middle)? {
                low = middle;
            } else {
                high = middle;
            }
        }
        Ok(Some(low))
    }

    pub(crate) fn read_item(
        &self,
        handle: &ResolvedHandle,
        position: u64,
    ) -> Result<StagedItem, Failure> {
        let path = handle
            .directory
            .join(ITEMS_DIRECTORY)
            .join(item_name(position));
        let limit = usize::try_from(handle.binding.limits.max_event_bytes)
            .unwrap_or(usize::MAX / 2)
            .saturating_add(ITEM_ENVELOPE_BYTES);
        let bytes = read_bounded(&self.fs, &path, limit as u64).map_err(io_failure)?;
        let corrupt = || integrity(format!("Capture item {} is not valid.", path.display()));
        if bytes.len() > limit {
            return Err(corrupt());
        }
        let item: StagedItem = serde_json::from_slice(&bytes).map_err(|_| corrupt())?;
        let request = serde_json::to_vec(&item.request).map_err(|_| corrupt())?;
        if item.schema_version != ITEM_VERSION
            || item.position != position
            || item.handle != handle.binding.handle
            || item.run_id != handle.run_id
            || hash_bytes(&request) != item.request_digest
        {
            return Err(corrupt());
        }
        Ok(item)
    }

    pub(crate) fn key_position(&self, keys: &Path, key: &str) -> Result<Option<u64>, Failure> {
        let path = keys.join(key_name(key));
        let bytes = match read_bounded(&self.fs, &path, MAX_KEY_FILE_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_failure(error)),
        };
        let entry: KeyEntry = serde_json::from_slice(&bytes)
            .ok()
            .filter(|entry: &KeyEntry| entry.schema_version == KEY_VERSION && entry.key == key)
            .ok_or_else(|| {
                integrity(format!(
                    "Capture key entry {} is not valid.",
                    path.display()
                ))
            })?;
        Ok(Some(entry.position))
    }

    /// Publish the key entry of an accepted item unless it is already there.
    fn ensure_key(&mut self, keys: &Path, item: &StagedItem) -> Result<(), Failure> {
        match self.key_position(keys, &item.key)? {
            Some(position) if position == item.position => Ok(()),
            Some(_) => Err(integrity("Two capture items share one key.")),
            None => {
                let mut bytes = serde_json::to_vec(&KeyEntry {
                    schema_version: KEY_VERSION.into(),
                    key: item.key.clone(),
                    position: item.position,
                })
                .map_err(|_| integrity("Could not encode a capture key entry."))?;
                bytes.push(b'\n');
                self.publish_entry(keys, &key_name(&item.key), &bytes)
            }
        }
    }

    /// Write, sync and exclusively publish one immutable entry, then sync its
    /// directory. A temporary left by a crashed writer is replaced first; only
    /// the handle lock holder writes here.
    pub(crate) fn publish_entry(
        &mut self,
        directory: &Path,
        name: &str,
        bytes: &[u8],
    ) -> Result<(), Failure> {
        let target = directory.join(name);
        let temporary = directory.join(format!("{name}.tmp"));
        match self.fs.remove(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(io_failure(error)),
        }
        write_private(&mut self.fs, &self.gate, &temporary, bytes, true)?;
        publish(&mut self.fs, &temporary, &target, &self.gate)
            .map_err(exclusive_publication_failure)?;
        let _ = self.fs.remove(&temporary);
        Ok(())
    }

    /// Create a staging subdirectory if needed and make its entry durable.
    pub(crate) fn ensure_directory(
        &mut self,
        parent: &Path,
        directory: &Path,
    ) -> Result<(), Failure> {
        match self.fs.create_dir(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_failure(error)),
        }
        sync_directory(&mut self.fs, parent, &self.gate).map_err(durability_failure)
    }

    /// Make an already published item durable before replaying its receipt.
    fn sync_published(
        &mut self,
        staging: &Path,
        items: &Path,
        position: u64,
    ) -> Result<(), Failure> {
        sync_existing_file(&mut self.fs, &items.join(item_name(position)), &self.gate)
            .map_err(durability_failure)?;
        sync_directory(&mut self.fs, items, &self.gate).map_err(durability_failure)?;
        sync_directory(&mut self.fs, staging, &self.gate).map_err(durability_failure)
    }

    pub(crate) fn exists(&self, path: &Path) -> Result<bool, Failure> {
        match self.fs.read(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(io_failure(error)),
        }
    }
}

fn item_name(position: u64) -> String {
    format!("{position:012}.json")
}

/// Keys may hold any printable text, so the index names them by digest.
fn key_name(key: &str) -> String {
    let digest = hash_bytes(key.as_bytes());
    let hex = digest.as_str().strip_prefix("sha256:").unwrap_or_default();
    format!("{hex}.json")
}

fn read_bounded(fs: &impl DurableFs, path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs.read(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn key_reused(item: &StagedItem, submission: &ItemSubmission, digest: &Digest) -> Failure {
    let mut failure = Failure::new(
        ErrorCode::IdempotencyKeyReused,
        format!(
            "This handle already accepted a different item with key {:?} at position {}. Resend the identical item to get its receipt, or use a new key for a new item.",
            item.key, item.position
        ),
    )
    .at("/key");
    failure.error.expected = Some(json!({
        "position": item.position,
        "operation": item.operation,
        "request_digest": item.request_digest,
    }));
    failure.error.actual = Some(json!({
        "position": submission.position,
        "operation": submission.operation,
        "request_digest": digest,
    }));
    failure
}

fn position_conflict(sent: u64, next: u64, last: Option<u64>) -> Failure {
    let message = if sent < next {
        format!(
            "Position {sent} already holds another item. The next position on this handle is {next}."
        )
    } else if next == sent - 1 {
        format!("Position {next} is missing. Send the item for position {next} first.")
    } else {
        format!(
            "Positions {next} to {} are missing. Send the item for position {next} first.",
            sent - 1
        )
    };
    let mut failure = Failure::new(ErrorCode::RevisionConflict, message).at("/position");
    failure.error.expected = Some(json!({"position": next, "last_position": last}));
    failure.error.actual = Some(json!({"position": sent}));
    failure.error.retry = Some("resume at the expected position".into());
    failure
}

#[cfg(test)]
#[path = "capture_items_tests.rs"]
pub(crate) mod tests;
