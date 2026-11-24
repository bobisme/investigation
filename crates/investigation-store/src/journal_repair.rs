//! `repair journal` (ADR 0016): restore the bytes this program wrote to a
//! journal file that another program rewrote without changing its content.
//!
//! Journal verification refuses a genesis or event file whose bytes parse
//! but are not the serialization this program writes for that content
//! (`NON_CANONICAL_JOURNAL_FILE`). The serialization is deterministic, so
//! the bytes this program wrote are the serialization of the content that is
//! still there. A file is restored only when a witness records the hash of
//! those bytes:
//!
//! - the next file of the case: event 1 names the genesis's hash, and each
//!   later event its predecessor's, in `previous_hash`;
//! - for a fork, the case head it inherited, at that sequence, which its
//!   workspace marker records;
//! - a workspace archive of this workspace, or of a fork's origin, given
//!   with `--archive`: the case head its `archive.json` records, at that
//!   sequence. The caller checks the archive's own consistency first.
//! - for the last file of a case, which no file names, the disposable index:
//!   the head it recorded for the case, at the same sequence.
//!
//! A file the next event records as it is was accepted by a program before
//! ADR 0016 and is verified as it is; it is left alone. If any other file
//! has no witness, nothing is written and the refusal names
//! every such file: its content can then not be told apart from an edit, so
//! the workspace is restored from an archive instead. A repair writes the
//! restored bytes to `state/tmp`, syncs them and renames them over the file
//! under the coordination and case locks, so a crash leaves either file
//! whole. The repaired journal must then verify in full.

use std::{fs, io::Read as _};

use investigation_core::wire::Digest;
use serde::Serialize;

use super::*;
use crate::journal::{EVENTS_DIRECTORY, GENESIS_FILENAME, canonical_event, canonical_genesis};

/// Where a restored file is staged before it replaces the file.
const STAGED: &str = "state/tmp/journal-repair";

/// What records the hash of the restored bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Witness {
    /// The next file of the case names it as `previous_hash`.
    NextEvent,
    /// The fork's marker records it as the head the fork inherited.
    InheritedHead,
    /// The given archive's `archive.json` records it as a case head.
    Archive,
    /// The index recorded it as the case head.
    Index,
}

/// The case heads a workspace archive records in its `archive.json`, as a
/// witness for [`Store::repair_journal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedHeads {
    /// The workspace the archive was made from.
    pub workspace_id: Id,
    pub export_id: String,
    /// Case, sequence and head hash.
    pub heads: Vec<(Id, u64, Digest)>,
}

/// One journal file whose written bytes are, or would be, restored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepairedFile {
    /// Workspace-relative path.
    pub path: String,
    pub case_id: Id,
    /// The hash of the bytes found.
    pub found: Digest,
    /// The hash of the bytes this program wrote, which the witness records.
    pub restored: Digest,
    pub witness: Witness,
}

/// The result of `repair journal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalRepair {
    /// True when nothing was written.
    pub dry_run: bool,
    pub files: Vec<RepairedFile>,
}

/// One planned rewrite.
struct Planned {
    file: RepairedFile,
    relative: PathBuf,
    bytes: Vec<u8>,
}

impl Store {
    /// Restore the bytes of every journal file another program rewrote
    /// without changing its content, when a witness records their hash; see
    /// the module documentation. With `dry_run`, report and write nothing.
    pub fn repair_journal(
        &mut self,
        dry_run: bool,
        archive: Option<&ArchivedHeads>,
    ) -> Result<JournalRepair, Failure> {
        let _lock = self.coordinate()?;
        let origin = self.workspace.fork().map(|fork| fork.origin_workspace_id);
        if let Some(archive) = archive
            && archive.workspace_id != self.workspace.id()
            && Some(archive.workspace_id) != origin
        {
            let mut failure = Failure::new(
                ErrorCode::InvalidInput,
                format!(
                    "Nothing was repaired. The archive {} was made from workspace {}, not from this \
                     workspace or the workspace it was forked from, so it records no head of this \
                     journal. Pass an archive of this workspace.",
                    archive.export_id, archive.workspace_id
                ),
            )
            .at("--archive");
            failure.error.actual = Some(json!({
                "archive_workspace_id": archive.workspace_id,
                "workspace_id": self.workspace.id(),
            }));
            return Err(failure);
        }
        let heads = Projection::inspect(&self.index_path(), self.workspace.id()).ok();
        let mut planned = Vec::new();
        let mut unwitnessed = Vec::new();
        for case_id in self.published_cases()? {
            let case_dir = case_directory(case_id);
            let mut files = vec![case_dir.join(GENESIS_FILENAME)];
            let mut events = self
                .fs
                .list(&case_dir.join(EVENTS_DIRECTORY))
                .map_err(io_failure)?;
            events.sort();
            files.extend(events);
            let read = |relative: &Path| -> Result<Vec<u8>, Failure> {
                let mut bytes = Vec::new();
                self.fs
                    .read(relative)
                    .and_then(|mut file| file.read_to_end(&mut bytes))
                    .map_err(io_failure)?;
                Ok(bytes)
            };
            for (position, relative) in files.iter().enumerate() {
                let bytes = read(relative)?;
                let canonical = if position == 0 {
                    let writer = self.workspace.writers(case_id).at(0);
                    canonical_genesis(relative, &bytes, writer, case_id)?
                } else {
                    canonical_event(relative, position as u64, &bytes)?
                };
                let Some(canonical) = canonical else {
                    continue;
                };
                let found = hash_bytes(&bytes);
                let restored = hash_bytes(&canonical);
                let recorded = match files.get(position + 1) {
                    Some(next) => {
                        let next: Value = serde_json::from_slice(&read(next)?).unwrap_or_default();
                        next["previous_hash"].as_str().map(str::to_owned)
                    }
                    None => None,
                };
                // The next event records these very bytes: a program before
                // ADR 0016 accepted them and chained onto them, so they are
                // verified as they are and must stay.
                if recorded.as_deref() == Some(found.as_str()) {
                    continue;
                }
                let sequence = position as u64;
                let next = files.get(position + 1).is_some()
                    && recorded.as_deref() == Some(restored.as_str());
                let inherited = self
                    .workspace
                    .writers(case_id)
                    .inherited
                    .is_some_and(|head| head.sequence == sequence && head.head_hash == restored);
                let archived = archive.is_some_and(|archive| {
                    archive.heads.iter().any(|(id, at, hash)| {
                        *id == case_id && *at == sequence && *hash == restored
                    })
                });
                // The index is disposable, so it witnesses only the last
                // file, which nothing else in the journal names.
                let indexed = files.get(position + 1).is_none()
                    && heads
                        .as_ref()
                        .and_then(|heads| heads.iter().find(|(id, _)| *id == case_id))
                        .is_some_and(|(_, head)| {
                            head.sequence == sequence && head.head_hash == restored
                        });
                let witness = [
                    (next, Witness::NextEvent),
                    (inherited, Witness::InheritedHead),
                    (archived, Witness::Archive),
                    (indexed, Witness::Index),
                ]
                .into_iter()
                .find_map(|(holds, witness)| holds.then_some(witness));
                let path = relative.display().to_string();
                match witness {
                    Some(witness) => planned.push(Planned {
                        file: RepairedFile {
                            path,
                            case_id,
                            found,
                            restored,
                            witness,
                        },
                        relative: relative.clone(),
                        bytes: canonical,
                    }),
                    None => unwitnessed.push(path),
                }
            }
        }
        if !unwitnessed.is_empty() {
            let mut failure = Failure::new(
                ErrorCode::IntegrityFailure,
                format!(
                    "Nothing was repaired. No later journal file, inherited fork head, archive \
                     head or index head records the hash of the bytes this program would write \
                     for {}, so its content cannot be told apart from an edit. If an archive of \
                     this workspace holds that head, pass it with --archive DIR; otherwise \
                     restore the workspace from an archive (investigation archive restore).",
                    unwitnessed.join(", ")
                ),
            );
            failure.error.actual = Some(json!({ "unwitnessed": unwitnessed }));
            return Err(failure);
        }
        if !dry_run {
            for planned in &planned {
                let _case_lock = self
                    .fs
                    .lock(
                        &case_directory(planned.file.case_id).join(CASE_LOCK),
                        WAIT_FOR_LOCK,
                    )
                    .map_err(lock_failure)?;
                self.replace(&planned.relative, &planned.bytes)?;
            }
            if !planned.is_empty() {
                self.load_fully_verified()?;
            }
        }
        Ok(JournalRepair {
            dry_run,
            files: planned.into_iter().map(|planned| planned.file).collect(),
        })
    }

    /// Replace the journal file at `relative` with `bytes`, keeping its
    /// permissions: stage, sync, rename over it, sync its directory.
    fn replace(&self, relative: &Path, bytes: &[u8]) -> Result<(), Failure> {
        let root = self.workspace.root();
        let target = root.join(relative);
        let stage = root.join(STAGED);
        // Only this function writes the staged name; a leftover is from a
        // repair killed before its rename.
        match fs::remove_file(&stage) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(io_failure(error)),
            _ => {}
        }
        let permissions = fs::metadata(&target).map_err(io_failure)?.permissions();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)
            .map_err(io_failure)?;
        file.write_all(bytes).map_err(io_failure)?;
        file.sync_all().map_err(io_failure)?;
        drop(file);
        fs::set_permissions(&stage, permissions).map_err(io_failure)?;
        fs::rename(&stage, &target).map_err(io_failure)?;
        let directory = target.parent().expect("a journal file has a directory");
        fs::File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(io_failure)
    }
}
