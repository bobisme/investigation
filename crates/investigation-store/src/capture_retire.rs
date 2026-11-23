//! Retirement of capture staging, and the read-only staging checks `check`
//! runs.
//!
//! Staging is the only copy of what a handle accepted until its Run is
//! finalized, so nothing reclaims it as cache. Retirement is the one path that
//! removes a staging directory, and it does so only when the staging no longer
//! holds anything the canonical record lacks:
//!
//! - the Run is finalized in the verified journal, and the finalizing event
//!   file and its directory are synced, so the finalization is durable;
//! - the manifest Artifact verifies against its digest and binds every staged
//!   item exactly;
//! - every blob the finalization recorded and every staged attachment's bytes
//!   verify, by digest, in the canonical blob store, and are synced;
//! - the staging holds only entries the store wrote; anything foreign keeps it.
//!
//! An open handle, a sealed handle whose finalization is not committed, a
//! declared handle without staging, a handle another call holds, and staging
//! that fails any check above are kept and reported with the reason.
//!
//! # Crash model
//!
//! Retirement holds the handle lock, so no item or finalization on the handle
//! interleaves with it. It first renames the whole staging directory out of
//! `state/staging` to `state/tmp/retire-<run-id>` and syncs both parents; only
//! then does it remove the moved entries. The rename is the retirement commit
//! point:
//!
//! - Before it: the full staging is in place. Recovery reports it present; a
//!   retry retires it.
//! - After it: the handle has no staging. Recovery reports it retired and the
//!   manifest answers identical retries. A crash while removing the moved tree
//!   leaves part of it under `state/tmp`, never under `state/staging`; the
//!   next retirement completes the removal and `check` reports it until then.
//!
//! Retiring a retired handle reports `already_retired` and changes nothing, so
//! retirement is idempotent.
//!
//! A call that opened the handle lock before the rename acquires it after
//! retirement releases it, and finds a finalized handle: its manifest answers
//! items and its journal answers finalize. A call that opens the lock after the
//! rename finds no staging, and the manifest answers it the same way.
//!
//! Publication-temporary cleanup never touches `retire-` directories; only
//! retirement removes them, and only the entry names it owns.

use std::io::ErrorKind;

use investigation_core::wire::Digest;

use super::{
    attach::{AttachOutcome, BLOBS_DIRECTORY},
    finalize::finalization_of,
    items::{HANDLE_LOCK_FILE, ITEMS_DIRECTORY, KEYS_DIRECTORY, SEAL_FILE},
    late::is_finalized,
    *,
};
use crate::{
    durable::sync_existing_file,
    journal::{CaseReplay, EVENTS_DIRECTORY},
    store::check::{CheckIssue, CheckReport, Severity, read_hash},
};

/// Prefix of a staging directory moved out of `state/staging` by retirement,
/// under `state/tmp`, while its entries are removed.
pub const RETIRING_PREFIX: &str = "retire-";

/// What retirement did with one handle's staging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetirementResult {
    /// This call removed the staging.
    Retired,
    /// The Run is finalized and has no staging.
    AlreadyRetired,
    /// The staging is kept; `reason` says why.
    Kept,
}

/// Why a handle's staging is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeptReason {
    /// The handle accepts items; its staging is their only copy.
    Open,
    /// Sealed, but the finalization is not committed; resend the sealing finalize.
    Sealed,
    /// Declared but never allocated and not finalized; resend the identical begin.
    Unallocated,
    /// Another call holds the handle lock.
    Busy,
    /// The manifest or some staged bytes are not verified in the canonical store.
    Unpublished,
    /// The staging disagrees with its declaration or the finalized manifest, or
    /// holds an entry the store did not write.
    Inconsistent,
}

/// Retirement of one handle's staging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StagingRetirement {
    pub handle: Id,
    pub run_id: Id,
    pub case_id: Id,
    /// Workspace-relative staging directory the handle was declared with.
    pub staging_dir: String,
    pub result: RetirementResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<KeptReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What one retirement call did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetirementReport {
    /// The requested handle in any state, or every handle that still has
    /// staging or was never allocated, in handle order.
    pub handles: Vec<StagingRetirement>,
    /// Runs whose interrupted retirement this call completed.
    pub completed: Vec<Id>,
}

/// Staging states across the workspace, as `check` counts them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StagingSummary {
    /// Open handles: staging present, not sealed, not finalized.
    pub active: usize,
    /// Sealed handles whose finalization is not committed.
    pub sealed: usize,
    /// Declared handles with no staging that are not finalized.
    pub unallocated: usize,
    /// Finalized handles whose staging is still present.
    pub retirable: usize,
    /// Finalized handles whose staging is retired.
    pub retired: usize,
    /// Staging directories bound to no declared handle.
    pub orphaned: usize,
}

type Outcome = (RetirementResult, Option<(KeptReason, String)>);

fn kept(reason: KeptReason, detail: impl Into<String>) -> Outcome {
    (RetirementResult::Kept, Some((reason, detail.into())))
}

/// Keep the staging for an integrity finding; any other failure, such as an
/// I/O error, fails the call and also keeps it.
fn inconsistent(reason: KeptReason, failure: Failure) -> Result<Outcome, Failure> {
    if failure.error.code == ErrorCode::IntegrityFailure {
        Ok(kept(reason, failure.error.message))
    } else {
        Err(failure)
    }
}

/// The files and directories of one staging tree, in removal order.
struct Inventory {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

impl<F: DurableFs> Store<F> {
    /// Retire the staging of one handle, or with `None` of every finalized
    /// handle, and complete any retirement a crash interrupted.
    ///
    /// Staging of a handle that is not finalized is never removed.
    pub fn retire_staging(&mut self, handle: Option<Id>) -> Result<RetirementReport, Failure> {
        self.gate.check().map_err(durability_failure)?;
        let completed = self.complete_retirements()?;
        let replay = {
            let _lock = self.coordinate()?;
            self.load_verified()?
        };
        let mut declared = self.declared_handles(&replay)?;
        if let Some(handle) = handle {
            declared.retain(|declared| declared.binding.handle == handle);
            if declared.is_empty() {
                return Err(Failure::new(
                    ErrorCode::UnknownReference,
                    format!(
                        "No capture handle {handle} is declared in this workspace. Use the handle from the begin receipt, or retire without a handle to retire every finalized one."
                    ),
                )
                .at("/handle"));
            }
        }
        let mut handles = Vec::with_capacity(declared.len());
        for declared in declared {
            let case = replay
                .cases
                .get(&declared.case_id)
                .ok_or_else(|| integrity("A declared handle names a case that is missing."))?;
            let finalized = case
                .state
                .current(declared.run_id)
                .is_some_and(is_finalized);
            let directory = PathBuf::from(&declared.binding.staging);
            let (result, reason) = match (declared.staging, finalized) {
                (StagingState::Missing, true) if handle.is_none() => continue,
                (StagingState::Missing, true) => (RetirementResult::AlreadyRetired, None),
                (StagingState::Missing, false) => kept(
                    KeptReason::Unallocated,
                    "The handle is declared but its staging was never allocated. Resend the identical begin request to allocate it.",
                ),
                (StagingState::Present, false) if self.exists(&directory.join(SEAL_FILE))? => kept(
                    KeptReason::Sealed,
                    "The handle is sealed but its finalization is not committed. Resend the sealing finalize request to complete it.",
                ),
                (StagingState::Present, false) => kept(
                    KeptReason::Open,
                    "The handle is open; its staging holds the only copy of what it accepted.",
                ),
                (StagingState::Present, true) => self.retire_one(&declared, case)?,
            };
            let (reason, detail) = reason.unzip();
            handles.push(StagingRetirement {
                handle: declared.binding.handle,
                run_id: declared.run_id,
                case_id: declared.case_id,
                staging_dir: declared.binding.staging,
                result,
                reason,
                detail,
            });
        }
        Ok(RetirementReport { handles, completed })
    }

    /// Retire the staging of one finalized handle under its handle lock.
    fn retire_one(
        &mut self,
        declared: &DeclaredHandle,
        case: &CaseReplay,
    ) -> Result<Outcome, Failure> {
        let directory = PathBuf::from(&declared.binding.staging);
        let binding = directory.join(BINDING_FILE);
        let _handle_lock = match self.lock_handle(&directory) {
            Ok(guard) => guard,
            Err(failure) if failure.error.code == ErrorCode::WorkspaceBusy => {
                return Ok(kept(
                    KeptReason::Busy,
                    "Another call holds this capture handle. Retry the retirement.",
                ));
            }
            // Another retirement moved the staging while this one looked it up.
            Err(_) if !self.exists(&binding)? => {
                return Ok((RetirementResult::AlreadyRetired, None));
            }
            Err(failure) => return Err(failure),
        };
        if !self.exists(&binding)? {
            return Ok((RetirementResult::AlreadyRetired, None));
        }
        let resolved = match self.resolve_declared(declared, &case.state) {
            Ok(resolved) => resolved,
            Err(failure) => return inconsistent(KeptReason::Inconsistent, failure),
        };
        let finalizing = case
            .events
            .iter()
            .find(|verified| finalization_of(&verified.event, declared.run_id).is_some())
            .map(|verified| &verified.event)
            .ok_or_else(|| integrity("The finalized Run has no finalizing transaction."))?;
        // The finalization must be durable before the staging that backs it goes.
        let events = case_directory(declared.case_id).join(EVENTS_DIRECTORY);
        let event = events.join(
            event_filename(finalizing.sequence)
                .map_err(|_| integrity("The finalizing event has no valid sequence."))?,
        );
        sync_existing_file(&mut self.fs, &event, &self.gate).map_err(durability_failure)?;
        sync_directory(&mut self.fs, &events, &self.gate).map_err(durability_failure)?;

        let manifest =
            match self.finalized_manifest(&case.state, declared.run_id, declared.binding.handle) {
                Ok(Some(manifest)) => manifest,
                Ok(None) => return Err(integrity("The finalized Run has no manifest.")),
                Err(failure) => return inconsistent(KeptReason::Unpublished, failure),
            };
        let staged = match self.staged_items(&resolved) {
            Ok(staged) => staged,
            Err(failure) => return inconsistent(KeptReason::Inconsistent, failure),
        };
        let mut digests = BTreeSet::new();
        for item in &staged {
            let index = usize::try_from(item.position.saturating_sub(1)).unwrap_or(usize::MAX);
            if manifest.items.get(index) != Some(item) {
                return Ok(kept(
                    KeptReason::Inconsistent,
                    format!(
                        "Staged item {} is not bound by the finalized manifest.",
                        item.position
                    ),
                ));
            }
            let outcome = match AttachOutcome::of(item) {
                Ok(outcome) => outcome,
                Err(failure) => return inconsistent(KeptReason::Inconsistent, failure),
            };
            if let Some(digest) = outcome.and_then(|outcome| outcome.digest) {
                digests.insert(digest);
            }
        }
        digests.extend(finalizing.request.attachments.iter().cloned());
        digests.insert(manifest.digest.clone());
        for digest in &digests {
            if let Some(detail) = self.durable_canonical_blob(digest)? {
                return Ok(kept(KeptReason::Unpublished, detail));
            }
        }
        if let Err(entry) = self.inventory(&directory)? {
            return Ok(kept(
                KeptReason::Inconsistent,
                format!(
                    "Staging {} holds {}, which the store did not write. Inspect it; retirement removes only entries the store owns.",
                    directory.display(),
                    entry.display()
                ),
            ));
        }

        // The commit point: the whole directory leaves state/staging at once.
        let retiring = retiring_directory(declared.run_id);
        match self.fs.rename_no_replace(&directory, &retiring) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                return Ok(kept(
                    KeptReason::Inconsistent,
                    format!(
                        "{} still holds an earlier interrupted retirement of this Run with entries retirement does not own. Inspect it.",
                        retiring.display()
                    ),
                ));
            }
            Err(error) => return Err(io_failure(error)),
        }
        sync_directory(&mut self.fs, Path::new(TMP), &self.gate).map_err(durability_failure)?;
        sync_directory(&mut self.fs, Path::new(STAGING_ROOT), &self.gate)
            .map_err(durability_failure)?;
        self.remove_retiring(&retiring)?;
        Ok((RetirementResult::Retired, None))
    }

    /// `None` when the canonical store holds bytes matching `digest`, synced;
    /// otherwise why it does not.
    fn durable_canonical_blob(&mut self, digest: &Digest) -> Result<Option<String>, Failure> {
        let Some(hex) = digest
            .as_str()
            .strip_prefix("sha256:")
            .filter(|hex| hex.len() == 64)
        else {
            return Ok(Some(format!("{digest} is not a SHA-256 digest.")));
        };
        let shard = Path::new(BLOBS).join(&hex[..2]);
        let path = shard.join(hex);
        match self.fs.read(&path) {
            Ok(reader) => match read_hash(reader) {
                Ok((actual, _)) if actual == hex => {}
                Ok(_) => {
                    return Ok(Some(format!(
                        "The canonical blob {digest} does not match its digest."
                    )));
                }
                Err(error) => return Err(io_failure(error)),
            },
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(Some(format!(
                    "The canonical blob store does not hold {digest}."
                )));
            }
            Err(error) => return Err(io_failure(error)),
        }
        // Verified: make it durable before the staged copy goes.
        sync_existing_file(&mut self.fs, &path, &self.gate).map_err(durability_failure)?;
        sync_directory(&mut self.fs, &shard, &self.gate).map_err(durability_failure)?;
        Ok(None)
    }

    /// Every entry of a staging tree, or the first one the store did not write.
    fn inventory(&self, directory: &Path) -> Result<Result<Inventory, PathBuf>, Failure> {
        let mut inventory = Inventory {
            files: Vec::new(),
            directories: Vec::new(),
        };
        let list = |path: &Path| match self.fs.list(path) {
            Ok(entries) => Ok(Some(entries)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_failure(error)),
        };
        let Some(entries) = list(directory)? else {
            return Ok(Ok(inventory));
        };
        for entry in entries {
            let Some(name) = entry.file_name().and_then(|name| name.to_str()) else {
                return Ok(Err(entry));
            };
            if [ITEMS_DIRECTORY, KEYS_DIRECTORY, BLOBS_DIRECTORY].contains(&name) {
                let children = match self.fs.list(&entry) {
                    Ok(children) => children,
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    // A file where the store keeps a directory.
                    Err(_) => return Ok(Err(entry)),
                };
                for child in children {
                    let owned = child
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|child| owned_child(name, child));
                    if !owned {
                        return Ok(Err(child));
                    }
                    inventory.files.push(child);
                }
                inventory.directories.push(entry);
            } else if owned_root_file(name) {
                inventory.files.push(entry);
            } else {
                return Ok(Err(entry));
            }
        }
        inventory.directories.push(directory.to_owned());
        Ok(Ok(inventory))
    }

    /// Remove a staging tree that retirement moved out of `state/staging`.
    /// Entries the store did not write stop it, and stay for inspection.
    fn remove_retiring(&mut self, retiring: &Path) -> Result<(), Failure> {
        let inventory = self.inventory(retiring)?.map_err(|entry| {
            integrity(format!(
                "{} holds {}, which the store did not write; it is left for inspection.",
                retiring.display(),
                entry.display()
            ))
        })?;
        for file in &inventory.files {
            match self.fs.remove(file) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(io_failure(error)),
            }
        }
        for directory in &inventory.directories {
            match self.fs.remove_dir(directory) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(io_failure(error)),
            }
        }
        sync_directory(&mut self.fs, Path::new(TMP), &self.gate).map_err(durability_failure)
    }

    /// Complete every retirement a crash interrupted after its commit point.
    fn complete_retirements(&mut self) -> Result<Vec<Id>, Failure> {
        let mut completed = Vec::new();
        for (run_id, path) in self.retiring_directories()? {
            // A tree with an entry the store did not write stays for inspection;
            // check reports it.
            if self.inventory(&path)?.is_err() {
                continue;
            }
            self.remove_retiring(&path)?;
            completed.push(run_id);
        }
        Ok(completed)
    }

    /// Directories under `state/tmp` that a retirement moved there.
    fn retiring_directories(&self) -> Result<Vec<(Id, PathBuf)>, Failure> {
        let entries = match self.fs.list(Path::new(TMP)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(io_failure(error)),
        };
        Ok(entries
            .into_iter()
            .filter_map(|path| {
                let run_id = path
                    .file_name()?
                    .to_str()?
                    .strip_prefix(RETIRING_PREFIX)?
                    .parse::<Id>()
                    .ok()
                    .filter(|id| id.kind() == Kind::Run)?;
                Some((run_id, path))
            })
            .collect())
    }

    /// Verify every staging directory against the handle it is bound to, and
    /// report staging that needs attention. Reads only.
    pub(in crate::store) fn check_staging(
        &self,
        replay: &WorkspaceReplay,
        report: &mut CheckReport,
    ) {
        let issue = |severity, code: &str, objects, reason: String, repair| CheckIssue {
            severity,
            code: code.into(),
            objects,
            reason,
            repair,
        };
        let declared = match self.declared_handles(replay) {
            Ok(declared) => declared,
            Err(failure) => {
                report.issues.push(issue(
                    Severity::Error,
                    "STAGING_UNVERIFIED",
                    Vec::new(),
                    failure.error.message,
                    None,
                ));
                return;
            }
        };
        let mut bound = BTreeSet::new();
        let mut retirable = Vec::new();
        for declared in &declared {
            bound.insert(declared.run_id);
            let Some(case) = replay.cases.get(&declared.case_id) else {
                continue;
            };
            let Some(run) = case.state.current(declared.run_id) else {
                continue;
            };
            let objects = vec![declared.binding.handle, declared.run_id];
            let finalized = is_finalized(run);
            if declared.staging == StagingState::Missing {
                if finalized {
                    report.staging.retired += 1;
                } else {
                    report.staging.unallocated += 1;
                    report.issues.push(issue(
                        Severity::Warning,
                        "CAPTURE_STAGING_UNALLOCATED",
                        objects,
                        "A capture handle is declared but its staging was never allocated. The producer resends the identical begin request to allocate it.".into(),
                        Some(recover_argv(declared.binding.handle)),
                    ));
                }
                continue;
            }
            if let Err(reason) = self.verify_binding(declared, case) {
                report.issues.push(issue(
                    Severity::Error,
                    "STAGING_BINDING_MISMATCH",
                    objects,
                    reason,
                    None,
                ));
                continue;
            }
            let seal = self
                .resolve_declared(declared, &case.state)
                .and_then(|resolved| self.capture_seal(&resolved));
            let seal = match seal {
                Ok(seal) => seal,
                Err(failure) => {
                    report.issues.push(issue(
                        Severity::Error,
                        "STAGING_SEAL_INVALID",
                        objects,
                        failure.error.message,
                        None,
                    ));
                    continue;
                }
            };
            match (finalized, seal) {
                (true, seal) => {
                    let recorded: Option<Digest> =
                        investigation_core::input::decode(&run.body["manifest_digest"]).ok();
                    if let Some(seal) = seal
                        && recorded.as_ref() != Some(&seal.manifest_digest)
                    {
                        report.issues.push(issue(
                            Severity::Error,
                            "STAGING_SEAL_MISMATCH",
                            objects,
                            format!(
                                "The staging seal binds manifest {}, but the finalized Run records another manifest.",
                                seal.manifest_digest
                            ),
                            None,
                        ));
                        continue;
                    }
                    report.staging.retirable += 1;
                    retirable.push(declared.run_id);
                }
                (false, Some(_)) => {
                    report.staging.sealed += 1;
                    report.issues.push(issue(
                        Severity::Warning,
                        "CAPTURE_SEALED_UNCOMMITTED",
                        objects,
                        "A capture handle is sealed but its finalization is not committed. Its staging is kept; the producer resends the sealing finalize request.".into(),
                        Some(recover_argv(declared.binding.handle)),
                    ));
                }
                (false, None) => report.staging.active += 1,
            }
        }
        if !retirable.is_empty() {
            report.issues.push(issue(
                Severity::Warning,
                "STAGING_RETIRABLE",
                retirable,
                "Finalized capture Runs still hold staging. Retirement removes it once the canonical store verifiably holds every staged byte.".into(),
                Some(retire_argv()),
            ));
        }
        match self.fs.list(Path::new(STAGING_ROOT)) {
            Ok(entries) => {
                for entry in entries {
                    let run_id = entry
                        .file_name()
                        .and_then(|name| name.to_str())
                        .and_then(|name| name.parse::<Id>().ok());
                    if run_id.is_some_and(|run_id| bound.contains(&run_id)) {
                        continue;
                    }
                    report.staging.orphaned += 1;
                    report.issues.push(issue(
                        Severity::Warning,
                        "STAGING_ORPHAN",
                        run_id.into_iter().collect(),
                        format!(
                            "{} is bound to no declared capture handle. It is kept; inspect it before removing anything by hand.",
                            entry.display()
                        ),
                        None,
                    ));
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => report.issues.push(issue(
                Severity::Error,
                "STAGING_UNREADABLE",
                Vec::new(),
                format!("The staging root could not be listed ({:?}).", error.kind()),
                None,
            )),
        }
        match self.retiring_directories() {
            Ok(retiring) if !retiring.is_empty() => report.issues.push(issue(
                Severity::Warning,
                "STAGING_RETIREMENT_INCOMPLETE",
                retiring.into_iter().map(|(run_id, _)| run_id).collect(),
                "An interrupted retirement left removed staging under state/tmp. Its Runs are finalized and retired; retirement completes the removal.".into(),
                Some(retire_argv()),
            )),
            Ok(_) => {}
            Err(failure) => report.issues.push(issue(
                Severity::Error,
                "STAGING_UNREADABLE",
                Vec::new(),
                failure.error.message,
                None,
            )),
        }
    }

    /// `handle.json` must be the exact bytes its committed declaration
    /// allocates.
    fn verify_binding(&self, declared: &DeclaredHandle, case: &CaseReplay) -> Result<(), String> {
        let event = declaring_event(case, &case.state, declared.run_id)
            .map_err(|failure| failure.error.message)?;
        let expected = StagingBinding {
            schema_version: STAGING_VERSION,
            handle: declared.binding.handle,
            run_id: declared.run_id,
            case_id: declared.case_id,
            transaction_id: event.transaction_id,
            command_id: &event.command_id,
            actor: &event.actor,
            producer: &declared.binding.producer,
            external_run_id: &declared.binding.external_run_id,
        }
        .bytes()
        .map_err(|failure| failure.error.message)?;
        let path = Path::new(&declared.binding.staging).join(BINDING_FILE);
        let actual = read_all(&self.fs, &path).map_err(|error| {
            format!(
                "Staging binding {} could not be read ({:?}).",
                path.display(),
                error.kind()
            )
        })?;
        if actual == expected {
            return Ok(());
        }
        Err(format!(
            "Staging binding {} (digest {}) is not the binding its declaring transaction allocates (digest {}).",
            path.display(),
            hash_bytes(&actual),
            hash_bytes(&expected)
        ))
    }
}

/// Where retirement moves a Run's staging before removing it.
fn retiring_directory(run_id: Id) -> PathBuf {
    Path::new(TMP).join(format!("{RETIRING_PREFIX}{run_id}"))
}

/// Files the store writes at the top of a staging directory.
fn owned_root_file(name: &str) -> bool {
    [BINDING_FILE, HANDLE_LOCK_FILE, SEAL_FILE]
        .iter()
        .any(|file| name == *file || name.strip_suffix(".tmp") == Some(file))
}

/// Entries the store writes inside `items/`, `keys/` and `blobs/`.
fn owned_child(directory: &str, name: &str) -> bool {
    let digits = |value: &str, length: usize| {
        value.len() == length && value.bytes().all(|byte| byte.is_ascii_digit())
    };
    let hex = |value: &str| {
        value.len() == 64
            && value
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    };
    let name = name.strip_suffix(".tmp").unwrap_or(name);
    match directory {
        ITEMS_DIRECTORY => name
            .strip_suffix(".json")
            .is_some_and(|stem| digits(stem, 12)),
        KEYS_DIRECTORY => name.strip_suffix(".json").is_some_and(hex),
        _ => hex(name) || digits(name, 12),
    }
}

/// The command that retires finalized staging.
pub fn retire_argv() -> Vec<String> {
    [PROGRAM_NAME, "repair", "staging", "--format", "json"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
#[path = "capture_retire_tests.rs"]
mod tests;
