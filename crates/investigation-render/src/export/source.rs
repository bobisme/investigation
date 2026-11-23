//! The disk reads of exports that copy more than records: retained bytes
//! and, for a workspace archive, the canonical state and human notes.
//!
//! Everything read here is checked before a planner sees it: blob bytes
//! against their recorded digest and size ([`Retained::insert`]), journal
//! bytes against the chain the store verified. Planning stays pure.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use investigation_core::wire::{Digest, Failure};
use investigation_store::{
    Store,
    durable::DurableFs,
    store::archive::{MAX_HUMAN_NOTE_BYTES, MAX_HUMAN_NOTES},
};

use super::{
    Retained,
    archive::{ArchiveSource, HumanNote, SkippedNote},
};

/// The vault folders that belong to people, in the order they are read.
pub const HUMAN_FOLDERS: [&str; 3] = ["Notes", "Boards", "Inbox"];

/// Read the retained bytes of `digests` from `store`, each checked against
/// its digest. The seam for kinds that copy retained bytes.
pub fn read_retained<'a, F: DurableFs>(
    store: &Store<F>,
    digests: impl IntoIterator<Item = &'a Digest>,
) -> Result<Retained, Failure> {
    let mut retained = Retained::none();
    for digest in digests {
        let bytes = store.retained_bytes(digest)?;
        retained
            .insert(digest.clone(), bytes)
            .map_err(|mismatch| integrity(mismatch.to_string()))?;
    }
    Ok(retained)
}

/// Read what a workspace archive copies: the verified canonical state, every
/// required blob, the human notes when the vault is enabled, and the
/// published contracts the caller passes (`schemas`).
pub fn read_archive_source<F: DurableFs>(
    store: &mut Store<F>,
    schemas: Vec<(String, Vec<u8>)>,
) -> Result<ArchiveSource, Failure> {
    let canonical = store.read_canonical()?;
    let mut retained = Retained::none();
    for blob in &canonical.blobs {
        let bytes = store.retained_bytes(&blob.digest)?;
        if !blob.sizes.is_empty() && !blob.sizes.contains(&(bytes.len() as u64)) {
            return Err(integrity(format!(
                "Stored bytes differ from their recorded size ({}).",
                blob.digest
            )));
        }
        retained
            .insert(blob.digest.clone(), bytes)
            .map_err(|mismatch| integrity(mismatch.to_string()))?;
    }
    let (human_notes, skipped_notes) = if canonical.vault_enabled {
        read_human_notes(&store.workspace().root().join("vault"))?
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(ArchiveSource {
        canonical,
        retained,
        human_notes,
        skipped_notes,
        schemas,
    })
}

/// Every regular file below the human folders of `vault`, never following
/// a link. Links, special files, names that are not UTF-8, render
/// temporaries and notes over the limits are listed as skipped.
pub fn read_human_notes(vault: &Path) -> Result<(Vec<HumanNote>, Vec<SkippedNote>), Failure> {
    let mut notes = Vec::new();
    let mut skipped = Vec::new();
    for folder in HUMAN_FOLDERS {
        walk(&vault.join(folder), folder, &mut notes, &mut skipped, vault)?;
    }
    Ok((notes, skipped))
}

fn walk(
    directory: &Path,
    relative: &str,
    notes: &mut Vec<HumanNote>,
    skipped: &mut Vec<SkippedNote>,
    vault: &Path,
) -> Result<(), Failure> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io(directory, vault, &error)),
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        paths.push(entry.map_err(|error| io(directory, vault, &error))?.path());
    }
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let shown = format!("{relative}/{}", name.unwrap_or_default());
        let skip = |reason: &str, skipped: &mut Vec<SkippedNote>| {
            skipped.push(SkippedNote {
                path: shown.clone(),
                reason: reason.to_owned(),
            });
        };
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            skip("name is not UTF-8", skipped);
            continue;
        };
        let metadata = fs::symlink_metadata(&path).map_err(|error| io(&path, vault, &error))?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            skip("symbolic link", skipped);
        } else if file_type.is_dir() {
            walk(&path, &format!("{relative}/{name}"), notes, skipped, vault)?;
        } else if !file_type.is_file() {
            skip("not a regular file", skipped);
        } else if name.ends_with(".inv-tmp") || name.ends_with(".inv-del") {
            skip("render temporary", skipped);
        } else if metadata.len() > MAX_HUMAN_NOTE_BYTES {
            skip("larger than archive.human_note_bytes", skipped);
        } else if notes.len() >= MAX_HUMAN_NOTES {
            skip("over archive.human_notes", skipped);
        } else {
            let bytes = read_bounded(&path, vault)?;
            if bytes.len() as u64 > MAX_HUMAN_NOTE_BYTES {
                skip("larger than archive.human_note_bytes", skipped);
            } else {
                notes.push(HumanNote { path: shown, bytes });
            }
        }
    }
    Ok(())
}

/// Read a regular file without following a final link, at most one byte
/// past the note limit.
fn read_bounded(path: &Path, vault: &Path) -> Result<Vec<u8>, Failure> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(|error| io(path, vault, &error))?;
    let mut bytes = Vec::new();
    file.take(MAX_HUMAN_NOTE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io(path, vault, &error))?;
    Ok(bytes)
}

fn io(path: &Path, vault: &Path, error: &std::io::Error) -> Failure {
    let shown = path.strip_prefix(vault).unwrap_or(path);
    Failure::new(
        investigation_core::wire::ErrorCode::IoFailure,
        format!(
            "vault/{}: {:?}; the archive was not written.",
            shown.display(),
            error.kind()
        ),
    )
}

fn integrity(message: String) -> Failure {
    Failure::new(
        investigation_core::wire::ErrorCode::IntegrityFailure,
        message,
    )
    .with_argv(vec![
        investigation_core::PROGRAM_NAME.to_owned(),
        "check".to_owned(),
        "--format".to_owned(),
        "json".to_owned(),
    ])
}
