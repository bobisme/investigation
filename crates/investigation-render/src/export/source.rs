//! The disk reads of exports that copy more than records: retained bytes
//! and, for a workspace archive, the canonical state and human notes.
//!
//! Everything read here is checked before a planner sees it: blob bytes
//! against their recorded digest and size ([`Retained::insert`]), journal
//! bytes against the chain the store verified. Planning stays pure.

use investigation_core::wire::{Digest, Failure};
use investigation_store::{Store, durable::DurableFs, store::pins::ArchivePin};

use super::{Retained, archive::ArchiveSource};

/// The vault folders that belong to people, in the order they are read.
pub use investigation_core::human_file::HUMAN_FOLDERS;

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
/// published contracts the caller passes (`schemas`). The returned pin holds
/// the recovery heads and every required blob against reclamation: keep it
/// until the archive is written, then `store.release_pin(pin)`. Dropping it
/// also ends it, on any error path.
pub fn read_archive_source<F: DurableFs>(
    store: &mut Store<F>,
    schemas: Vec<(String, Vec<u8>)>,
) -> Result<(ArchiveSource, ArchivePin), Failure>
where
    F::Guard: 'static,
{
    let (canonical, pin) = store.read_canonical()?;
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
    // Through the projection root: the workspace's vault/ or, in
    // existing-vault mode, the managed prefix of the person's vault.
    let (human_notes, skipped_notes) = if canonical.vault_enabled {
        crate::human_files::read_all(store.workspace()).map_err(|error| {
            let mut failure = error.to_failure();
            failure.error.message = format!(
                "{} The human notes could not be read, so the archive was not written.",
                failure.error.message
            );
            failure
        })?
    } else {
        (Vec::new(), Vec::new())
    };
    let source = ArchiveSource {
        canonical,
        retained,
        human_notes,
        skipped_notes,
        schemas,
    };
    Ok((source, pin))
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
