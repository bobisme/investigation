//! `archive verify DIR`: check an export directory offline, without the
//! workspace it came from.
//!
//! 1. Every member the manifest lists is present, a regular file, and has
//!    the listed SHA-256 digest and size; nothing else is in the directory
//!    (a link or special file is unsafe, any other file is extra).
//! 2. The manifest's `export_id` is recomputed from its own fields and file
//!    digests, so an edited manifest is found even when it parses.
//! 3. For a workspace archive, `archive.json` agrees with the manifest: the
//!    recovery head is recomputed from the case heads, and every blob and
//!    human note it lists is a member with that digest.
//! 4. Then the archive is restored into a private directory below the
//!    scratch folder and the store's own `check` runs there: it replays and
//!    hash-checks every chain, checks workspace-wide transaction, command and
//!    record uniqueness, and hashes every required blob. The restored heads
//!    must equal the archived ones. The copy is removed afterwards.
//!
//! Members are read once while they are copied and hashed, so a member that
//! changes between the check and the restore is found too. Nothing in DIR is
//! written.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};

use investigation_core::{identity::Id, wire::Digest};
use investigation_store::{
    Store, Workspace,
    store::{
        CaseHead, Severity,
        archive::{MAX_INDEX_BYTES, copy_hashed, restore_layout},
    },
};
use serde::Serialize;

use super::{
    ExportManifest, MANIFEST_FILE, MANIFEST_SCHEMA, Product,
    archive::{
        ARCHIVE_FORMAT, ArchiveIndex, CANONICAL_ROOT, INDEX_FILE, canonical_member, recovery_head,
    },
    expected_export_id,
};
use crate::PlanPath;

/// One finding of a verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// `MANIFEST_MISSING`, `MANIFEST_INVALID`, `MANIFEST_ALTERED`,
    /// `MEMBER_MISSING`, `MEMBER_EXTRA`, `MEMBER_UNSAFE`,
    /// `MEMBER_CORRUPTED`, `MEMBER_CHANGED`, `INDEX_INVALID`,
    /// `INDEX_MISMATCH`, `HEAD_MISMATCH`, `REHEARSAL_FAILED`, or a `check`
    /// code prefixed with `CHECK_`.
    pub code: String,
    /// The member, relative to DIR, when the finding is about one.
    pub path: Option<String>,
    pub detail: String,
}

/// What a restored copy of an archive showed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rehearsal {
    /// Whether every member was copied into the private workspace.
    pub restored: bool,
    /// Whether `check` found no error there.
    pub check_ok: bool,
    /// Case heads `check` verified in the copy, in case order.
    pub heads: Vec<CaseHead>,
    pub verified_blobs: usize,
    pub unavailable_artifacts: usize,
}

/// The archive part of a verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchiveVerdict {
    pub workspace_id: Id,
    pub recovery_head: Digest,
    pub cases: usize,
    pub blobs: usize,
    pub not_retained: usize,
    pub human_notes: usize,
    /// `None` when earlier findings made a restore pointless.
    pub rehearsal: Option<Rehearsal>,
}

/// The result of [`verify_export`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyReport {
    pub kind: Option<String>,
    pub export_id: Option<String>,
    /// Members checked against the manifest, the manifest excluded.
    pub members: usize,
    pub bytes: u64,
    pub archive: Option<ArchiveVerdict>,
    pub problems: Vec<Problem>,
}

impl VerifyReport {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

fn problem(code: &str, path: Option<&str>, detail: impl Into<String>) -> Problem {
    Problem {
        code: code.to_owned(),
        path: path.map(str::to_owned),
        detail: detail.into(),
    }
}

/// What is on disk below DIR, by relative `/`-separated path.
enum Entry {
    File,
    Unsafe(&'static str),
}

/// Verify the export in `dir`. A workspace archive is also restored into a
/// new private directory below `scratch` and checked there. `Err` only when
/// `dir` itself cannot be read.
pub fn verify_export(dir: &Path, scratch: &Path) -> io::Result<VerifyReport> {
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a directory (a link to one is refused too)",
        ));
    }
    let mut report = VerifyReport {
        kind: None,
        export_id: None,
        members: 0,
        bytes: 0,
        archive: None,
        problems: Vec::new(),
    };
    let mut on_disk = BTreeMap::new();
    list(dir, "", &mut on_disk)?;
    let manifest = match on_disk.remove(MANIFEST_FILE) {
        None => {
            report.problems.push(problem(
                "MANIFEST_MISSING",
                Some(MANIFEST_FILE),
                "No export-manifest.json: the export is unfinished or not an export.",
            ));
            return Ok(report);
        }
        Some(Entry::Unsafe(kind)) => {
            report.problems.push(problem(
                "MEMBER_UNSAFE",
                Some(MANIFEST_FILE),
                format!("The manifest is a {kind}."),
            ));
            return Ok(report);
        }
        Some(Entry::File) => match read_manifest(&dir.join(MANIFEST_FILE)) {
            Ok(manifest) => manifest,
            Err(detail) => {
                report
                    .problems
                    .push(problem("MANIFEST_INVALID", Some(MANIFEST_FILE), detail));
                return Ok(report);
            }
        },
    };
    report.kind = Some(manifest.kind.clone());
    report.export_id = Some(manifest.export_id.clone());
    check_members(dir, &manifest, on_disk, &mut report)?;
    if expected_export_id(&manifest) != manifest.export_id {
        report.problems.push(problem(
            "MANIFEST_ALTERED",
            Some(MANIFEST_FILE),
            "The export_id does not match the manifest's own fields and file digests.",
        ));
    }
    if manifest.product == Product::Archive {
        verify_archive(dir, scratch, &manifest, &mut report);
    }
    Ok(report)
}

fn read_manifest(path: &Path) -> Result<ExportManifest, String> {
    let bytes = read_small(path)?;
    let manifest: ExportManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("The manifest does not parse: {error}."))?;
    if manifest.schema_version != MANIFEST_SCHEMA {
        return Err(format!(
            "The manifest version {} is not {MANIFEST_SCHEMA}, the one this program reads.",
            manifest.schema_version
        ));
    }
    Ok(manifest)
}

/// A file of at most [`MAX_INDEX_BYTES`], never through a final link.
fn read_small(path: &Path) -> Result<Vec<u8>, String> {
    let file = open_member(path).map_err(|error| format!("{:?}", error.kind()))?;
    let mut bytes = Vec::new();
    file.take(MAX_INDEX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{:?}", error.kind()))?;
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err("Larger than archive.index_bytes.".to_owned());
    }
    Ok(bytes)
}

fn open_member(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

/// Every entry below `directory`, never following a link.
fn list(directory: &Path, prefix: &str, found: &mut BTreeMap<String, Entry>) -> io::Result<()> {
    for item in fs::read_dir(directory)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let file_type = fs::symlink_metadata(item.path())?.file_type();
        if file_type.is_dir() {
            list(&item.path(), &relative, found)?;
        } else if file_type.is_file() {
            found.insert(relative, Entry::File);
        } else if file_type.is_symlink() {
            found.insert(relative, Entry::Unsafe("symbolic link"));
        } else {
            found.insert(relative, Entry::Unsafe("special file"));
        }
    }
    Ok(())
}

fn check_members(
    dir: &Path,
    manifest: &ExportManifest,
    mut on_disk: BTreeMap<String, Entry>,
    report: &mut VerifyReport,
) -> io::Result<()> {
    let mut listed = BTreeSet::new();
    for file in &manifest.files {
        let path = file.path.as_str();
        if PlanPath::new(path).is_err() || path == MANIFEST_FILE || !listed.insert(path) {
            report.problems.push(problem(
                "MANIFEST_INVALID",
                Some(path),
                "The manifest lists an invalid or repeated member path.",
            ));
            continue;
        }
        match on_disk.remove(path) {
            None => report.problems.push(problem(
                "MEMBER_MISSING",
                Some(path),
                "Listed in the manifest but absent.",
            )),
            Some(Entry::Unsafe(kind)) => report.problems.push(problem(
                "MEMBER_UNSAFE",
                Some(path),
                format!("Listed in the manifest but a {kind}."),
            )),
            Some(Entry::File) => {
                let (digest, bytes) = copy_hashed(open_member(&dir.join(path))?, None)?;
                report.members += 1;
                report.bytes += bytes;
                if digest != file.sha256 || bytes != file.bytes {
                    report.problems.push(problem(
                        "MEMBER_CORRUPTED",
                        Some(path),
                        format!(
                            "Expected {} ({} bytes), found {digest} ({bytes} bytes).",
                            file.sha256, file.bytes
                        ),
                    ));
                }
            }
        }
    }
    for (path, entry) in on_disk {
        let (code, detail) = match entry {
            Entry::File => ("MEMBER_EXTRA", "Not listed in the manifest.".to_owned()),
            Entry::Unsafe(kind) => ("MEMBER_UNSAFE", format!("A {kind}, not listed.")),
        };
        report.problems.push(problem(code, Some(&path), detail));
    }
    Ok(())
}

fn verify_archive(
    dir: &Path,
    scratch: &Path,
    manifest: &ExportManifest,
    report: &mut VerifyReport,
) {
    let listed: BTreeMap<&str, (&Digest, u64)> = manifest
        .files
        .iter()
        .map(|file| (file.path.as_str(), (&file.sha256, file.bytes)))
        .collect();
    if !listed.contains_key(INDEX_FILE) {
        report.problems.push(problem(
            "INDEX_INVALID",
            Some(INDEX_FILE),
            "A workspace archive must list archive.json.",
        ));
        return;
    }
    let index: ArchiveIndex = match read_small(&dir.join(INDEX_FILE)).and_then(|bytes| {
        serde_json::from_slice(&bytes).map_err(|error| format!("It does not parse: {error}."))
    }) {
        Ok(index) => index,
        Err(detail) => {
            report
                .problems
                .push(problem("INDEX_INVALID", Some(INDEX_FILE), detail));
            return;
        }
    };
    if index.format != ARCHIVE_FORMAT {
        report.problems.push(problem(
            "INDEX_INVALID",
            Some(INDEX_FILE),
            format!(
                "The archive format {} is not {ARCHIVE_FORMAT}, the one this program restores.",
                index.format
            ),
        ));
        return;
    }
    let mismatch = |detail: String, path: Option<&str>| problem("INDEX_MISMATCH", path, detail);
    let recomputed = recovery_head(
        index.workspace_id,
        index
            .cases
            .iter()
            .map(|case| (case.case_id, case.sequence, &case.head_hash)),
    );
    if recomputed != index.recovery_head
        || index.recovery_head != manifest.snapshot.head_hash
        || index.workspace_id != manifest.snapshot.case_id
        || manifest.snapshot.sequence != 0
    {
        report.problems.push(mismatch(
            "The recovery head or workspace in archive.json does not match the case heads and the manifest snapshot.".to_owned(),
            Some(INDEX_FILE),
        ));
    }
    let mut indexed: BTreeSet<String> = BTreeSet::new();
    for blob in &index.blobs {
        let member = canonical_member(&investigation_store::store::archive::blob_path(
            &blob.sha256,
        ));
        match listed.get(member.as_str()) {
            Some((digest, bytes)) if **digest == blob.sha256 && *bytes == blob.bytes => {}
            _ => report.problems.push(mismatch(
                format!(
                    "Blob {} is not a member with that digest and size.",
                    blob.sha256
                ),
                Some(&member),
            )),
        }
        indexed.insert(member);
    }
    for note in &index.human_notes {
        if !safe_relative(&note.path) {
            report.problems.push(mismatch(
                format!(
                    "The human note path {:?} is not a safe relative path.",
                    note.path
                ),
                Some(INDEX_FILE),
            ));
        }
        match listed.get(note.member.as_str()) {
            Some((digest, bytes)) if **digest == note.sha256 && *bytes == note.bytes => {}
            _ => report.problems.push(mismatch(
                format!("Human note {} is not a member with that digest.", note.path),
                Some(&note.member),
            )),
        }
    }
    let blob_prefix = format!("{CANONICAL_ROOT}/state/blobs/");
    for path in listed.keys() {
        if path.starts_with(&blob_prefix) && !indexed.contains(*path) {
            report.problems.push(mismatch(
                "A blob member is not listed in archive.json.".to_owned(),
                Some(path),
            ));
        }
    }
    let mut verdict = ArchiveVerdict {
        workspace_id: index.workspace_id,
        recovery_head: index.recovery_head.clone(),
        cases: index.cases.len(),
        blobs: index.blobs.len(),
        not_retained: index.not_retained.len(),
        human_notes: index.human_notes.len(),
        rehearsal: None,
    };
    if report.problems.is_empty() {
        verdict.rehearsal = Some(rehearse(dir, scratch, manifest, &index, report));
    }
    report.archive = Some(verdict);
}

/// Restore into a private directory below `scratch`, run `check`, compare
/// heads, and remove the copy.
fn rehearse(
    dir: &Path,
    scratch: &Path,
    manifest: &ExportManifest,
    index: &ArchiveIndex,
    report: &mut VerifyReport,
) -> Rehearsal {
    let mut rehearsal = Rehearsal {
        restored: false,
        check_ok: false,
        heads: Vec::new(),
        verified_blobs: 0,
        unavailable_artifacts: 0,
    };
    let private = match private_directory(scratch) {
        Ok(private) => private,
        Err(error) => {
            report.problems.push(problem(
                "REHEARSAL_FAILED",
                None,
                format!(
                    "No private directory could be made below the scratch folder ({:?}).",
                    error.kind()
                ),
            ));
            return rehearsal;
        }
    };
    let root = private.join("workspace");
    let outcome = restore_into(dir, manifest, index, &root, false);
    match outcome {
        Err(RestoreError::Changed(path)) => report.problems.push(problem(
            "MEMBER_CHANGED",
            Some(&path),
            "The member changed while it was restored.",
        )),
        Err(RestoreError::Unsafe(path)) => report.problems.push(problem(
            "MEMBER_UNSAFE",
            Some(&path),
            "The path would leave the restore destination.",
        )),
        Err(RestoreError::Io(path, error)) => report.problems.push(problem(
            "REHEARSAL_FAILED",
            path.as_deref(),
            format!(
                "The private copy could not be written ({:?}).",
                error.kind()
            ),
        )),
        Ok(()) => {
            rehearsal.restored = true;
            check_restored(&root, index, &mut rehearsal, report);
        }
    }
    let _ = fs::remove_dir_all(&private);
    rehearsal
}

fn check_restored(
    root: &Path,
    index: &ArchiveIndex,
    rehearsal: &mut Rehearsal,
    report: &mut VerifyReport,
) {
    let workspace = match Workspace::open(root) {
        Ok(workspace) => workspace,
        Err(error) => {
            report.problems.push(problem(
                "REHEARSAL_FAILED",
                None,
                format!("The restored copy does not open as a workspace: {error}"),
            ));
            return;
        }
    };
    if workspace.id() != index.workspace_id {
        report.problems.push(problem(
            "INDEX_MISMATCH",
            Some(INDEX_FILE),
            "The restored workspace marker names another workspace than archive.json.",
        ));
    }
    let mut store = Store::open(workspace);
    let checked = match store.check() {
        Ok(checked) => checked,
        Err(failure) => {
            report
                .problems
                .push(problem("REHEARSAL_FAILED", None, failure.error.message));
            return;
        }
    };
    rehearsal.check_ok = checked.integrity_ok();
    rehearsal.heads = checked.heads.clone();
    rehearsal.verified_blobs = checked.verified_blobs;
    rehearsal.unavailable_artifacts = checked.unavailable_artifacts;
    for issue in checked
        .issues
        .iter()
        .filter(|issue| issue.severity == Severity::Error)
    {
        report.problems.push(problem(
            &format!("CHECK_{}", issue.code),
            None,
            issue.reason.clone(),
        ));
    }
    if !checked.structurally_valid {
        return;
    }
    let archived: Vec<(Id, u64, &Digest)> = index
        .cases
        .iter()
        .map(|case| (case.case_id, case.sequence, &case.head_hash))
        .collect();
    let restored: Vec<(Id, u64, &Digest)> = checked
        .heads
        .iter()
        .map(|head| {
            (
                head.case_id,
                head.snapshot.sequence,
                &head.snapshot.head_hash,
            )
        })
        .collect();
    if archived != restored {
        report.problems.push(problem(
            "HEAD_MISMATCH",
            Some(INDEX_FILE),
            "The restored journals verify to other case heads than archive.json names.",
        ));
    }
    if checked.verified_blobs != index.blobs.len()
        || checked.unavailable_artifacts != index.not_retained.len()
    {
        report.problems.push(problem(
            "INDEX_MISMATCH",
            Some(INDEX_FILE),
            format!(
                "check verified {} blob(s) and {} artifact(s) without bytes; archive.json lists {} and {}.",
                checked.verified_blobs,
                checked.unavailable_artifacts,
                index.blobs.len(),
                index.not_retained.len()
            ),
        ));
    }
}

/// Why [`restore_into`] stopped.
#[derive(Debug)]
pub enum RestoreError {
    /// A member did not hash to its manifest digest while it was copied.
    Changed(String),
    /// A member or human note path would leave the restore destination.
    Unsafe(String),
    /// A read or write failed; the path is the member, when there is one.
    Io(Option<String>, io::Error),
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Changed(path) => write!(formatter, "{path} changed while it was restored"),
            Self::Unsafe(path) => write!(formatter, "{path} is not a safe relative path"),
            Self::Io(Some(path), error) => write!(formatter, "{path}: {error}"),
            Self::Io(None, error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for RestoreError {}

/// Write the canonical members of the archive in `dir` below `root`, which
/// must not exist, and the empty skeleton around them, so that
/// [`Workspace::open`] accepts it. With `human_notes`, the human notes go
/// back to their vault paths. Each member is hashed while it is copied and
/// must equal its manifest digest. Call only after [`verify_export`]
/// found no problem; publication, forks and retries are W11.18.
pub fn restore_into(
    dir: &Path,
    manifest: &ExportManifest,
    index: &ArchiveIndex,
    root: &Path,
    human_notes: bool,
) -> Result<(), RestoreError> {
    let io_at = |path: Option<&str>| {
        let path = path.map(str::to_owned);
        move |error| RestoreError::Io(path, error)
    };
    fs::DirBuilder::new()
        .mode(0o700)
        .create(root)
        .map_err(io_at(None))?;
    let prefix = format!("{CANONICAL_ROOT}/");
    for file in &manifest.files {
        let Some(relative) = file.path.strip_prefix(&prefix) else {
            continue;
        };
        if !safe_relative(relative) || PlanPath::new(file.path.as_str()).is_err() {
            return Err(RestoreError::Unsafe(file.path.clone()));
        }
        copy_member(dir, &file.path, &file.sha256, &root.join(relative))?;
    }
    let cases: Vec<Id> = index.cases.iter().map(|case| case.case_id).collect();
    restore_layout(root, index.vault_enabled, &cases)
        .map_err(|error| RestoreError::Io(None, io::Error::other(error.to_string())))?;
    if human_notes && index.vault_enabled {
        for note in &index.human_notes {
            if !safe_relative(&note.path) {
                return Err(RestoreError::Unsafe(note.path.clone()));
            }
            copy_member(
                dir,
                &note.member,
                &note.sha256,
                &root.join("vault").join(&note.path),
            )?;
        }
    }
    Ok(())
}

fn copy_member(
    dir: &Path,
    member: &str,
    expected: &Digest,
    target: &Path,
) -> Result<(), RestoreError> {
    let failed = |error| RestoreError::Io(Some(member.to_owned()), error);
    if let Some(parent) = target.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(failed)?;
    }
    let source = open_member(&dir.join(member)).map_err(failed)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(target)
        .map_err(failed)?;
    let (digest, _) = copy_hashed(source, Some(&mut output as &mut dyn Write)).map_err(failed)?;
    output.sync_all().map_err(failed)?;
    if &digest != expected {
        return Err(RestoreError::Changed(member.to_owned()));
    }
    Ok(())
}

/// Only normal components: no root, `..`, `.` or empty part.
fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.split('/').any(str::is_empty)
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// A new owner-only directory below `scratch`. Creation is exclusive, so a
/// concurrent verification takes the next free name.
fn private_directory(scratch: &Path) -> io::Result<PathBuf> {
    for attempt in 0..10_000u32 {
        let candidate = scratch.join(format!("investigation-verify-{attempt}"));
        match fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free private directory name",
    ))
}
