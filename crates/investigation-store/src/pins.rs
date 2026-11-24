//! Archive pins: a recovery archive's head and blobs held against
//! reclamation while the archive is written (plan §10.7, §16.5; W11.09).
//!
//! A pin has two parts. The *record* is a small file below `local/pins/`
//! naming the per-case heads and every blob digest the archive needs: it is
//! what `archive_pins` lists, and it is non-canonical, machine-local
//! bookkeeping. The *lock* is an advisory lock on that same file, held by
//! the process that is writing the archive. The lock is what protects the
//! blobs: the kernel drops it when the holder exits, is killed or crashes,
//! so a dead archive can never leave a permanent pin. A record whose lock
//! is free is *stale*; it protects nothing, and the next pin creation or
//! reclamation expires it. Only files this module created (named
//! `archive-<digits>.json` below `local/pins/`) are ever removed.
//!
//! Pin creation, stale expiry and reclamation all run under the workspace
//! coordination lock, so a reclamation either sees a pin or runs wholly
//! before it. Ordinary appends take the same lock only briefly and are not
//! blocked by a pin: an archive covers exactly the heads it pinned.
//!
//! Reclamation (W11.11) calls [`Store::remove_blob_unless_pinned`] or, for a
//! batch it plans itself, [`Store::pinned_digests`] under its own
//! coordination; this module never decides whether a blob is referenced.

use std::{
    any::Any,
    collections::BTreeSet,
    io::{self, Write as _},
    path::{Path, PathBuf},
    time::Duration,
};

use investigation_core::{
    identity::Id,
    wire::{Digest, ErrorCode, Failure},
};
use serde::{Deserialize, Serialize};

use super::{
    DurableFile, DurableFs, LockError, Store, archive::RequiredBlob, archive::blob_path, integrity,
    io_failure,
};
use crate::store::CaseHead;

/// The pin directory, relative to the workspace root.
pub const PIN_DIRECTORY: &str = "local/pins";
const RECORD_SCHEMA: &str = "inv.archive-pin.v1";
const PREFIX: &str = "archive-";
const SUFFIX: &str = ".json";
/// Largest pin record read back; a larger one is treated as unreadable and
/// protects every blob.
const MAX_RECORD_BYTES: u64 = 256 << 20;

/// One case head a pin holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedHead {
    pub case_id: Id,
    pub sequence: u64,
    pub head_hash: Digest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PinRecord {
    schema: String,
    purpose: String,
    heads: Vec<PinnedHead>,
    blobs: Vec<Digest>,
}

/// A pin held by the running archive. Dropping it releases the lock, so the
/// pin protects nothing from then on, on every exit path; the leftover
/// record is expired by the next pin or reclamation. [`Store::release_pin`]
/// also removes the record at once.
#[must_use = "dropping a pin ends its protection"]
pub struct ArchivePin {
    id: Option<String>,
    heads: Vec<PinnedHead>,
    blobs: usize,
    guard: Option<Box<dyn Any>>,
}

impl std::fmt::Debug for ArchivePin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ArchivePin")
            .field("id", &self.id)
            .field("heads", &self.heads)
            .field("blobs", &self.blobs)
            .finish()
    }
}

/// A live pin's name and the digests it holds; `None` when its record
/// cannot be read, so it holds every blob.
pub(crate) type LivePin = (String, Option<BTreeSet<Digest>>);

impl ArchivePin {
    /// The pin's name (`archive-0001`), or `None` in a dry run, which writes
    /// nothing and so pins nothing.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// The case heads this pin holds, in case ID order.
    pub fn heads(&self) -> &[PinnedHead] {
        &self.heads
    }

    /// How many blobs this pin holds.
    pub fn blob_count(&self) -> usize {
        self.blobs
    }
}

/// A pin record found on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PinInfo {
    pub id: String,
    /// Whether a process still holds the pin. A pin that is not live was
    /// abandoned by an archive that died; it protects nothing.
    pub live: bool,
    pub heads: Vec<PinnedHead>,
    pub blobs: usize,
}

impl<F: DurableFs> Store<F> {
    /// Record and lock a pin for `heads` and `blobs`. The caller holds the
    /// coordination lock, so the pin and the head it names are one atomic
    /// step with respect to commits and reclamation.
    pub(super) fn create_archive_pin(
        &mut self,
        heads: &[CaseHead],
        blobs: &[RequiredBlob],
    ) -> Result<ArchivePin, Failure>
    where
        F::Guard: 'static,
    {
        let heads: Vec<PinnedHead> = heads
            .iter()
            .map(|head| PinnedHead {
                case_id: head.case_id,
                sequence: head.snapshot.sequence,
                head_hash: head.snapshot.head_hash.clone(),
            })
            .collect();
        if self.fs.dry_run().is_some() {
            // A dry run writes nothing, so nothing can be reclaimed either.
            return Ok(ArchivePin {
                id: None,
                heads,
                blobs: blobs.len(),
                guard: None,
            });
        }
        self.expire_stale_pins()?;
        match self.fs.create_dir(Path::new(PIN_DIRECTORY)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_failure(error)),
        }
        let record = PinRecord {
            schema: RECORD_SCHEMA.to_owned(),
            purpose: "archive".to_owned(),
            heads: heads.clone(),
            blobs: blobs.iter().map(|blob| blob.digest.clone()).collect(),
        };
        let mut bytes =
            serde_json::to_vec(&record).map_err(|error| integrity(error.to_string()))?;
        bytes.push(b'\n');
        for number in 1u32.. {
            let id = format!("{PREFIX}{number:04}");
            let path = pin_path(&id);
            let mut file = match self.fs.create_private(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(io_failure(error)),
            };
            let written = file.write_all(&bytes).and_then(|()| file.sync_all());
            drop(file);
            // The record is complete before it is locked; nothing else looks
            // at pins without the coordination lock this caller holds.
            let locked = written
                .map_err(io_failure)
                .and_then(|()| self.fs.lock(&path, Duration::ZERO).map_err(lock_error))
                .and_then(|guard| {
                    self.fs
                        .sync_dir(Path::new(PIN_DIRECTORY))
                        .map_err(io_failure)?;
                    Ok(guard)
                });
            return match locked {
                Ok(guard) => Ok(ArchivePin {
                    id: Some(id),
                    heads,
                    blobs: blobs.len(),
                    guard: Some(Box::new(guard)),
                }),
                Err(failure) => {
                    // This call created the file, so it may remove it.
                    let _ = self.fs.remove(&path);
                    Err(failure)
                }
            };
        }
        unreachable!("the pin numbers are not exhausted")
    }

    /// End a pin: remove its record and release its lock. Safe to skip: a
    /// dropped pin is released and its record expires on the next use.
    pub fn release_pin(&mut self, mut pin: ArchivePin) -> Result<(), Failure> {
        let Some(id) = pin.id.take() else {
            return Ok(());
        };
        let _lock = self.coordinate()?;
        match self.fs.remove(&pin_path(&id)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_failure(error)),
        }
        pin.guard = None;
        Ok(())
    }

    /// Every pin record, live or abandoned, in name order. Writes nothing.
    pub fn archive_pins(&mut self) -> Result<Vec<PinInfo>, Failure> {
        let _lock = self.coordinate()?;
        let mut found = Vec::new();
        for (id, path) in self.pin_files()? {
            let live = self.pin_is_live(&path)?;
            let record = self.read_record(&path);
            found.push(PinInfo {
                id,
                live,
                heads: record.as_ref().map(|r| r.heads.clone()).unwrap_or_default(),
                blobs: record.as_ref().map_or(0, |r| r.blobs.len()),
            });
        }
        Ok(found)
    }

    /// Remove the records of abandoned pins, those no process holds, and
    /// return their names. A live pin is never touched.
    pub fn release_stale_pins(&mut self) -> Result<Vec<String>, Failure> {
        let _lock = self.coordinate()?;
        self.expire_stale_pins()
    }

    /// The digests every live pin holds. Call under the coordination lock
    /// that also covers the removal; a pin that cannot be read holds
    /// everything, which is reported as `None`.
    pub fn pinned_digests(&mut self) -> Result<Option<BTreeSet<Digest>>, Failure> {
        let _lock = self.coordinate()?;
        self.live_pinned_digests()
    }

    /// Remove the stored blob `digest` unless a live archive pin holds it.
    /// Refused with `GATE_FAILED` naming the pins; the blob is kept and the
    /// caller retries after the archive ends. Whether anything still
    /// references the blob is the caller's decision (W11.11): this only
    /// serializes the removal against pin creation and release.
    pub fn remove_blob_unless_pinned(&mut self, digest: &Digest) -> Result<(), Failure> {
        let _lock = self.coordinate()?;
        let mut holders = Vec::new();
        for (id, path) in self.pin_files()? {
            if !self.pin_is_live(&path)? {
                continue;
            }
            match self.read_record(&path) {
                Some(record) if !record.blobs.contains(digest) => {}
                _ => holders.push(id),
            }
        }
        if !holders.is_empty() {
            let mut failure = Failure::new(
                ErrorCode::GateFailed,
                "An archive is being written and holds this blob. The blob is kept; retry \
                 when the archive has finished.",
            );
            failure.error.actual = Some(serde_json::json!({
                "digest": digest,
                "pins": holders,
            }));
            return Err(failure);
        }
        self.expire_stale_pins()?;
        match self.fs.remove(Path::new(&blob_path(digest))) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_failure(error)),
        }
    }

    /// Pin records by name. Other files in the directory are not ours and
    /// are ignored.
    fn pin_files(&self) -> Result<Vec<(String, PathBuf)>, Failure> {
        let entries = match self.fs.list(Path::new(PIN_DIRECTORY)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_failure(error)),
        };
        Ok(entries
            .into_iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?;
                let id = name.strip_suffix(SUFFIX)?;
                let digits = id.strip_prefix(PREFIX)?;
                (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| (id.to_owned(), path.clone()))
            })
            .collect())
    }

    fn pin_is_live(&mut self, path: &Path) -> Result<bool, Failure> {
        match self.fs.lock(path, Duration::ZERO) {
            Ok(_free) => Ok(false),
            Err(LockError::Busy { .. }) => Ok(true),
            Err(LockError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(LockError::Io(error)) => Err(io_failure(error)),
        }
    }

    fn read_record(&self, path: &Path) -> Option<PinRecord> {
        use std::io::Read as _;
        let mut bytes = Vec::new();
        self.fs
            .read(path)
            .ok()?
            .take(MAX_RECORD_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return None;
        }
        serde_json::from_slice::<PinRecord>(&bytes)
            .ok()
            .filter(|record| record.schema == RECORD_SCHEMA)
    }

    /// Every live pin by name with the digests it holds; `None` for a pin
    /// whose record cannot be read, which holds every blob. The caller
    /// holds the coordination lock.
    pub(crate) fn live_pins(&mut self) -> Result<Vec<LivePin>, Failure> {
        let mut live = Vec::new();
        for (id, path) in self.pin_files()? {
            if !self.pin_is_live(&path)? {
                continue;
            }
            let held = self
                .read_record(&path)
                .map(|record| record.blobs.into_iter().collect());
            live.push((id, held));
        }
        Ok(live)
    }

    fn live_pinned_digests(&mut self) -> Result<Option<BTreeSet<Digest>>, Failure> {
        let mut held = BTreeSet::new();
        for (_, path) in self.pin_files()? {
            if !self.pin_is_live(&path)? {
                continue;
            }
            let Some(record) = self.read_record(&path) else {
                return Ok(None);
            };
            held.extend(record.blobs);
        }
        Ok(Some(held))
    }

    /// Remove the records no process holds. The caller holds the
    /// coordination lock, which every pin creation also holds, so a record
    /// being created is never mistaken for an abandoned one.
    fn expire_stale_pins(&mut self) -> Result<Vec<String>, Failure> {
        let mut expired = Vec::new();
        for (id, path) in self.pin_files()? {
            if self.pin_is_live(&path)? {
                continue;
            }
            match self.fs.remove(&path) {
                Ok(()) => expired.push(id),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_failure(error)),
            }
        }
        Ok(expired)
    }
}

fn pin_path(id: &str) -> PathBuf {
    Path::new(PIN_DIRECTORY).join(format!("{id}{SUFFIX}"))
}

fn lock_error(error: LockError) -> Failure {
    match error {
        LockError::Busy { .. } => Failure::new(
            ErrorCode::WorkspaceBusy,
            "A new archive pin was already locked; retry the identical request.",
        ),
        LockError::Io(error) => io_failure(error),
    }
}
