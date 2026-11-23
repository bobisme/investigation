//! Attachment descriptors on a capture handle (`capture.attach`): stable bytes
//! copied into the handle's staging, verified, and accepted at the next
//! position.
//!
//! An attachment is an item like an appended fact ([`items`]): it takes the next
//! position under the handle lock, its key replays its original receipt, and a
//! sealed handle refuses it. What differs is the outcome, decided while the
//! handle lock is held and before the item is published:
//!
//! 1. The source is opened once beneath a configured import root, with no
//!    symlink followed at any component, and must be a regular file with one
//!    link and at most `max_attachment_bytes`.
//! 2. Its bytes are streamed into a private temporary in the staging `blobs/`
//!    directory while hashing, then read again from the same descriptor and
//!    hashed again. Both passes, the copied length and the descriptor's
//!    metadata (inode, size, modification and change times, link count) must
//!    agree, or the item is refused as unstable and nothing is staged.
//! 3. A declared size or digest that disagrees refuses the item.
//! 4. The verified copy is published by exclusive link as `blobs/<sha256>`, a
//!    read-only file that only the store wrote. Nothing is ever linked to a
//!    producer path.
//! 5. The item is published with an [`AttachOutcome`] naming the staged blob,
//!    or recording the artifact as missing.
//!
//! A refusal at any step stages no item and leaves the position free, so the
//! handle stays usable. The commit lock is never held during the copy.
//!
//! # Layout
//!
//! ```text
//! state/staging/<run-id>/
//!   blobs/<sha256-hex>             0400, verified bytes of one or more items
//!   blobs/<position:012>.tmp       transient; only while one copy is verified
//! ```
//!
//! Blobs are durable before the item that names them. A crash between the two
//! leaves an unreferenced blob, which a retry reuses after checking its bytes.
//! Finalization moves referenced blobs into the canonical blob store.
//!
//! # Stability
//!
//! "Stable" is the producer's obligation: close and flush before attaching.
//! The checks above catch a writer that appends, truncates, rewrites or
//! replaces the file during the copy. A writer that pauses for the whole copy
//! and resumes afterwards changes nothing the recorder can see; declaring
//! `expected_size_bytes` or `expected_digest` closes that gap.

use std::io::{self, ErrorKind, Read, Seek as _};

use investigation_core::{header::ClassificationLevel, wire::Digest};
use sha2::{Digest as _, Sha256};

use super::{
    items::{ItemAcceptance, ItemOperation, ItemSubmission, KEYS_DIRECTORY, ResolvedHandle},
    *,
};
use crate::{
    blob::{BlobError, stage, verify_blob},
    import_paths::{ImportErrorKind, ImportRoot, OpenedImport},
};

/// Directory of staged artifact bytes inside a handle's staging directory.
pub const BLOBS_DIRECTORY: &str = "blobs";

/// Where an attachment's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachSource {
    /// A closed file beneath a configured import root. A relative path is
    /// tried beneath each root in order; an absolute path must lie beneath one.
    File(PathBuf),
    /// The producer declares the artifact absent.
    Missing,
}

/// One attachment descriptor to stage at a position.
#[derive(Debug, Clone, PartialEq)]
pub struct AttachSubmission {
    /// The item itself; `operation` is [`ItemOperation::Attach`] and its
    /// `outcome` is ignored, because the store decides it.
    pub item: ItemSubmission,
    pub source: AttachSource,
    pub expected_digest: Option<Digest>,
    pub expected_size_bytes: Option<u64>,
    /// Requested level; never looser than the declared Run's.
    pub classification: Option<ClassificationLevel>,
    /// Item key of the earlier attachment this one derives from.
    pub derived_from: Option<String>,
}

/// Whether an accepted attachment has staged bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachAvailability {
    Staged,
    Missing,
}

/// Why an accepted attachment has no bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingCause {
    /// The producer declared it missing; its reason is in the request.
    Declared,
    /// No import root held a file at the declared path.
    NotFound,
}

/// The original a derivative names, resolved when it was accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedFrom {
    pub key: String,
    pub position: u64,
}

/// The durable result of an accepted attachment, stored as the item's
/// `outcome`. A retry returns the outcome of the first acceptance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachOutcome {
    pub availability: AttachAvailability,
    /// Digest of the retained bytes, when staged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Staged blob, relative to the handle's staging directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<MissingCause>,
    /// Effective level: the requested one, or the declared Run's.
    pub classification: ClassificationLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<DerivedFrom>,
}

impl AttachOutcome {
    /// The outcome of a staged attachment item; `None` for an appended fact.
    pub fn of(item: &items::StagedItem) -> Result<Option<Self>, Failure> {
        if item.operation != ItemOperation::Attach {
            return Ok(None);
        }
        let invalid = || {
            integrity(format!(
                "Capture attachment {} has no valid outcome.",
                item.position
            ))
        };
        let outcome: Self = item
            .outcome
            .clone()
            .ok_or_else(invalid)
            .and_then(|value| serde_json::from_value(value).map_err(|_| invalid()))?;
        let staged = outcome.availability == AttachAvailability::Staged;
        let consistent = if staged {
            outcome.missing.is_none()
                && outcome.size_bytes.is_some()
                && matches!(
                    (&outcome.digest, &outcome.blob),
                    (Some(digest), Some(blob)) if *blob == blob_name(digest)
                )
        } else {
            outcome.missing.is_some()
                && outcome.digest.is_none()
                && outcome.size_bytes.is_none()
                && outcome.blob.is_none()
        };
        if !consistent {
            return Err(invalid());
        }
        Ok(Some(outcome))
    }
}

/// Bytes snapshotted from a source and verified.
struct Snapshot {
    digest: Digest,
    size: u64,
}

impl<F: DurableFs> Store<F> {
    /// Copy one artifact into the handle's staging and accept its descriptor
    /// at the next position, or record it as missing, or return the original
    /// receipt of an identical earlier call.
    pub fn stage_capture_attachment(
        &mut self,
        submission: &AttachSubmission,
        clock: &mut dyn Clock,
    ) -> Result<ItemAcceptance, Failure> {
        self.stage_attachment_observed(submission, clock, &mut |_| {})
    }

    /// [`Self::stage_capture_attachment`], calling `during` with the bytes
    /// copied so far after each read of the source.
    pub(crate) fn stage_attachment_observed(
        &mut self,
        submission: &AttachSubmission,
        clock: &mut dyn Clock,
        during: &mut dyn FnMut(u64),
    ) -> Result<ItemAcceptance, Failure> {
        if submission.item.operation != ItemOperation::Attach {
            return Err(integrity("An attachment must be staged as capture.attach."));
        }
        self.stage_item_with(&submission.item, clock, |store, handle| {
            let outcome = store.attachment_outcome(handle, submission, during)?;
            serde_json::to_value(outcome)
                .map(Some)
                .map_err(|_| integrity("Could not encode the attachment outcome."))
        })
    }

    /// The verified path of an attachment's staged bytes, for finalization.
    pub fn staged_attachment(
        &self,
        staging: &Path,
        outcome: &AttachOutcome,
    ) -> Result<Option<PathBuf>, Failure> {
        let (Some(blob), Some(digest), Some(size)) =
            (&outcome.blob, &outcome.digest, outcome.size_bytes)
        else {
            return Ok(None);
        };
        let path = staging.join(blob);
        let hex = digest.as_str().trim_start_matches("sha256:");
        verify_blob(&self.fs, &path, hex, size).map_err(|error| match error {
            BlobError::DigestMismatch => integrity(format!(
                "Staged attachment {} does not match its digest.",
                path.display()
            )),
            other => blob_failure(other),
        })?;
        Ok(Some(path))
    }

    fn attachment_outcome(
        &mut self,
        handle: &ResolvedHandle,
        submission: &AttachSubmission,
        during: &mut dyn FnMut(u64),
    ) -> Result<AttachOutcome, Failure> {
        let classification = match submission.classification {
            Some(requested) if requested < handle.classification => {
                let mut failure = Failure::new(
                    ErrorCode::InvalidInput,
                    "An attachment may be classified more strictly than its Run, never less. Omit classification to use the Run's level.",
                )
                .at("/classification");
                failure.error.expected = Some(json!({"minimum": handle.classification}));
                failure.error.actual = Some(json!({"classification": requested}));
                return Err(failure);
            }
            Some(requested) => requested,
            None => handle.classification,
        };
        let limit = handle.binding.limits.max_attachment_bytes;
        if let Some(size) = submission.expected_size_bytes
            && size > limit
        {
            let mut failure = Failure::new(
                ErrorCode::InvalidInput,
                format!("This handle accepts attachments of at most {limit} bytes."),
            )
            .at("/expected_size_bytes");
            failure.error.expected = Some(json!({"max_attachment_bytes": limit}));
            failure.error.actual = Some(json!({"expected_size_bytes": size}));
            return Err(failure);
        }
        let derived_from = match &submission.derived_from {
            Some(key) => Some(self.original(handle, key)?),
            None => None,
        };
        let missing = |cause| AttachOutcome {
            availability: AttachAvailability::Missing,
            digest: None,
            size_bytes: None,
            blob: None,
            missing: Some(cause),
            classification,
            derived_from: derived_from.clone(),
        };
        let path = match &submission.source {
            AttachSource::Missing => return Ok(missing(MissingCause::Declared)),
            AttachSource::File(path) => path,
        };
        let Some(opened) = self.open_source(path, limit)? else {
            return Ok(missing(MissingCause::NotFound));
        };
        let snapshot = self.snapshot(handle, submission, opened, limit, during)?;
        Ok(AttachOutcome {
            availability: AttachAvailability::Staged,
            blob: Some(blob_name(&snapshot.digest)),
            digest: Some(snapshot.digest),
            size_bytes: Some(snapshot.size),
            missing: None,
            classification,
            derived_from,
        })
    }

    /// The earlier attachment a derivative names, which must have bytes.
    fn original(&mut self, handle: &ResolvedHandle, key: &str) -> Result<DerivedFrom, Failure> {
        let keys = handle.directory.join(KEYS_DIRECTORY);
        let Some(position) = self.key_position(&keys, key)? else {
            return Err(Failure::new(
                ErrorCode::UnknownReference,
                format!(
                    "No earlier item on this handle has key {key:?}. Attach the original first, then its derivative."
                ),
            )
            .at("/derived_from/from"));
        };
        let item = self.read_item(handle, position)?;
        let outcome = AttachOutcome::of(&item)?.ok_or_else(|| {
            Failure::new(
                ErrorCode::InvalidInput,
                format!("Item {key:?} is an appended fact, not an attached artifact."),
            )
            .at("/derived_from/from")
        })?;
        if outcome.availability != AttachAvailability::Staged {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                format!(
                    "Attachment {key:?} is recorded as missing, so nothing can derive from its bytes."
                ),
            )
            .at("/derived_from/from"));
        }
        Ok(DerivedFrom {
            key: key.to_owned(),
            position,
        })
    }

    /// Open the source beneath the first import root that holds it. `None`
    /// when no root has a file at that path; any other refusal is an error.
    fn open_source(&self, path: &Path, limit: u64) -> Result<Option<OpenedImport>, Failure> {
        let at = "/source/path";
        let roots = &self.workspace.config().import_roots;
        if roots.is_empty() {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                "No import root is configured. Add the directory that holds the producer's outputs to import_roots in investigation.toml.",
            )
            .at(at));
        }
        let mut refusal = None;
        let mut unusable_root = None;
        let mut beneath_any = false;
        let mut absent = false;
        for configured in roots {
            let root = lexical(&if configured.is_absolute() {
                configured.clone()
            } else {
                self.workspace.root().join(configured)
            });
            let relative = if path.is_absolute() {
                match lexical(path).strip_prefix(&root) {
                    Ok(relative) => relative.to_owned(),
                    Err(_) => continue,
                }
            } else {
                lexical(path)
            };
            beneath_any = true;
            let directory = match ImportRoot::open(&root) {
                Ok(directory) => directory,
                Err(error) => {
                    unusable_root.get_or_insert_with(|| {
                        format!(
                            "Import root {} cannot be opened: {}.",
                            root.display(),
                            reason(&error.reason)
                        )
                    });
                    continue;
                }
            };
            match directory.open_file(&relative, limit) {
                Ok(opened) => return Ok(Some(opened)),
                Err(error) => match &error.reason {
                    ImportErrorKind::Io(io) if io.kind() == ErrorKind::NotFound => absent = true,
                    other => {
                        refusal.get_or_insert_with(|| refused(other, &root));
                    }
                },
            }
        }
        // A root that refuses the file outright wins over a root that lacks it.
        let message = match (refusal, absent) {
            (Some(message), _) => message,
            (None, true) => return Ok(None),
            (None, false) if !beneath_any => format!(
                "Path {} is outside every configured import root.",
                path.display()
            ),
            (None, false) => {
                unusable_root.unwrap_or_else(|| "No configured import root can be opened.".into())
            }
        };
        Err(Failure::new(ErrorCode::InvalidInput, message).at(at))
    }

    /// Copy the source into a staging temporary while hashing, verify it is
    /// stable and matches what was declared, then publish it by digest.
    fn snapshot(
        &mut self,
        handle: &ResolvedHandle,
        submission: &AttachSubmission,
        mut opened: OpenedImport,
        limit: u64,
        during: &mut dyn FnMut(u64),
    ) -> Result<Snapshot, Failure> {
        let at_open = opened.witness.size;
        if let Some(expected) = submission.expected_size_bytes
            && expected != at_open
        {
            return Err(size_mismatch(expected, at_open));
        }
        let blobs = handle.directory.join(BLOBS_DIRECTORY);
        self.ensure_directory(&handle.directory, &blobs)?;
        let temporary = blobs.join(format!("{:012}.tmp", submission.item.position));
        self.remove_if_present(&temporary)?;
        let result = self.verified_copy(&temporary, submission, &mut opened, limit, during);
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(failure) => {
                // The temporary holds no accepted bytes; a failed removal is
                // repaired by the next attempt at this position.
                let _ = self.fs.remove(&temporary);
                return Err(failure);
            }
        };
        let target = blobs.join(blob_hex(&snapshot.digest));
        match publish(&mut self.fs, &temporary, &target, &self.gate) {
            Ok(()) => {}
            Err(DurableError::Io(error)) if error.kind() == ErrorKind::AlreadyExists => {
                // Identical bytes staged earlier, by another item or by an
                // attempt that crashed before its item was published.
                let hex = blob_hex(&snapshot.digest);
                verify_blob(&self.fs, &target, &hex, snapshot.size).map_err(
                    |error| match error {
                        BlobError::DigestMismatch => integrity(format!(
                            "Staged attachment {} does not match its name.",
                            target.display()
                        )),
                        other => blob_failure(other),
                    },
                )?;
                sync_existing_file(&mut self.fs, &target, &self.gate)
                    .map_err(durability_failure)?;
                sync_directory(&mut self.fs, &blobs, &self.gate).map_err(durability_failure)?;
            }
            Err(error) => {
                let _ = self.fs.remove(&temporary);
                return Err(durability_failure(error));
            }
        }
        let _ = self.fs.remove(&temporary);
        Ok(snapshot)
    }

    fn verified_copy(
        &mut self,
        temporary: &Path,
        submission: &AttachSubmission,
        opened: &mut OpenedImport,
        limit: u64,
        during: &mut dyn FnMut(u64),
    ) -> Result<Snapshot, Failure> {
        let at_open = opened.witness.size;
        let staged = {
            let reader = Watched {
                input: &mut opened.file,
                remaining: limit,
                copied: 0,
                during,
            };
            match stage(&mut self.fs, &self.gate, temporary, reader) {
                Ok(staged) => staged,
                // The source grew past the limit while it was copied.
                Err(BlobError::Io(error)) if error.kind() == ErrorKind::FileTooLarge => {
                    return Err(unstable(at_open, None));
                }
                Err(BlobError::Io(error)) => return Err(source_failure(&error)),
                Err(other) => return Err(blob_failure(other)),
            }
        };
        let unchanged = |opened: &OpenedImport| -> Result<bool, Failure> {
            opened
                .witness
                .verify_unchanged(&opened.file)
                .map_err(|error| source_failure(&error))
        };
        if staged.size != at_open || !unchanged(opened)? {
            return Err(unstable(at_open, Some(staged.size)));
        }
        // Timestamps are coarse; a second pass over the same descriptor catches
        // a rewrite that kept the size within one timestamp tick.
        opened
            .file
            .rewind()
            .map_err(|error| source_failure(&error))?;
        let (again, length) =
            hash(&mut opened.file, limit).map_err(|error| source_failure(&error))?;
        if again != staged.digest || length != staged.size || !unchanged(opened)? {
            return Err(unstable(at_open, Some(length)));
        }
        let digest: Digest = format!("sha256:{}", staged.digest)
            .parse()
            .map_err(|_| integrity("Could not encode an attachment digest."))?;
        if let Some(expected) = &submission.expected_digest
            && *expected != digest
        {
            let mut failure = Failure::new(
                ErrorCode::InvalidInput,
                "The artifact's bytes do not match expected_digest, so they were not retained. The handle is unchanged; attach the intended bytes or correct the digest.",
            )
            .at("/expected_digest");
            failure.error.expected = Some(json!({"digest": expected}));
            failure.error.actual = Some(json!({"digest": digest, "size_bytes": staged.size}));
            return Err(failure);
        }
        Ok(Snapshot {
            digest,
            size: staged.size,
        })
    }

    fn remove_if_present(&mut self, path: &Path) -> Result<(), Failure> {
        match self.fs.remove(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_failure(error)),
        }
    }
}

/// A bounded source read that reports progress.
struct Watched<'a, R> {
    input: R,
    remaining: u64,
    copied: u64,
    during: &'a mut dyn FnMut(u64),
}

impl<R: Read> Read for Watched<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 && !buffer.is_empty() {
            if self.input.read(&mut [0; 1])? == 0 {
                return Ok(0);
            }
            return Err(io::Error::new(
                ErrorKind::FileTooLarge,
                "attachment exceeds its limit",
            ));
        }
        let room = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = self.input.read(&mut buffer[..room])?;
        self.remaining -= read as u64;
        self.copied += read as u64;
        if read > 0 {
            (self.during)(self.copied);
        }
        Ok(read)
    }
}

fn hash(input: &mut impl Read, limit: u64) -> io::Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut bounded = input.take(limit.saturating_add(1));
    loop {
        let read = bounded.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        length += read as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), length))
}

fn blob_hex(digest: &Digest) -> String {
    digest.as_str().trim_start_matches("sha256:").to_owned()
}

fn blob_name(digest: &Digest) -> String {
    format!("{BLOBS_DIRECTORY}/{}", blob_hex(digest))
}

/// Drop `.` components so a configured `.` root and an absolute path compare.
fn lexical(path: &Path) -> PathBuf {
    path.components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .collect()
}

fn unstable(at_open: u64, copied: Option<u64>) -> Failure {
    let mut failure = Failure::new(
        ErrorCode::RevisionConflict,
        "The artifact changed while it was copied, so no bytes were retained and the position is still free. Retry the identical request once the producer has closed and flushed the file.",
    )
    .at("/source/path");
    failure.error.expected = Some(json!({"size_bytes": at_open, "stable": true}));
    failure.error.actual = Some(json!({"size_bytes": copied, "stable": false}));
    failure.error.retry = Some("retry once the writer has closed the file".into());
    failure
}

fn size_mismatch(expected: u64, actual: u64) -> Failure {
    let mut failure = Failure::new(
        ErrorCode::InvalidInput,
        "The artifact's size does not match expected_size_bytes, so it was not copied. If the producer is still writing it, retry once the file is closed.",
    )
    .at("/expected_size_bytes");
    failure.error.expected = Some(json!({"size_bytes": expected}));
    failure.error.actual = Some(json!({"size_bytes": actual}));
    failure
}

fn source_failure(error: &io::Error) -> Failure {
    Failure::new(
        ErrorCode::IoFailure,
        format!(
            "The artifact could not be read ({:?}). Nothing was staged; retry the identical request.",
            error.kind()
        ),
    )
    .at("/source/path")
}

fn refused(kind: &ImportErrorKind, root: &Path) -> String {
    match kind {
        ImportErrorKind::InvalidPath => {
            "Use a path beneath an import root, with no `..` component.".into()
        }
        ImportErrorKind::NotRegular => {
            "The artifact is not a regular file. Attach a closed regular file.".into()
        }
        ImportErrorKind::MultipleLinks => "The artifact has more than one hard link, so another path could change its bytes. Attach a copy with a single link.".into(),
        ImportErrorKind::TooLarge { size, limit } => format!(
            "The artifact is {size} bytes; this handle accepts attachments of at most {limit} bytes."
        ),
        other => format!(
            "The artifact cannot be opened beneath import root {} without following a symlink: {}.",
            root.display(),
            reason(other)
        ),
    }
}

fn reason(kind: &ImportErrorKind) -> String {
    match kind {
        ImportErrorKind::Io(error) => error.to_string(),
        ImportErrorKind::Unsupported => "this kernel lacks openat2".into(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
#[path = "capture_attach_tests.rs"]
mod tests;
