//! Reference-only ingest and reverification.
//!
//! Two explicit modes record an Artifact whose bytes the workspace does not
//! retain:
//!
//! - `external_refs` names bytes held elsewhere by an opaque locator with
//!   their expected digest and size. The core reads nothing: the reference is
//!   `external` and unverified.
//! - `retention: "reference"` hashes local `paths` without copying them, for
//!   inputs too large to retain. The digest is read at ingest, but the bytes
//!   can change afterwards and are not retained.
//!
//! Neither mode can satisfy an offline-completeness guarantee, and a failed
//! copy is a failed ingest: nothing ever falls back to a reference.
//!
//! `reverify` compares bytes supplied later with the exact Artifact revision
//! it names and appends an attributed verification in a new revision. A
//! match of copied bytes retains them. A mismatch sets integrity `mismatch`,
//! keeps the recorded digest and retains nothing in its place. The core
//! never fetches a locator; a producer or user supplies the bytes.

use std::{io::Read, path::Path};

use investigation_core::{
    command::RevisionRef,
    evidence::{ArtifactAvailability, ArtifactBody, ArtifactIntegrity, ArtifactRole},
    external_ref::{
        DigestBasis, ExternalReference, ExternalVerification, MAX_LOCATOR_CHARS,
        VerificationMethod, VerificationOutcome, VerificationState, artifact_retention,
    },
    header::ClassificationLevel,
    identity::{Id, Kind},
    state::{Acceptance, AcceptedTransaction, Record},
    wire::{Digest, ErrorCode, Failure},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{
    IngestReceipt, IngestRecords, ItemReceipt, MAX_IMPORT_BYTES, MAX_ITEMS, Manifest,
    StreamContent, TextStatus, acquisition::ItemOutcome, add_text_derivative, digest_for, invalid,
    new_id, sensitivity, stage_input,
};
use crate::{
    blob::StagedBlob,
    durable::DurableFs,
    journal::EventInput,
    store::{CommitPlan, Prepared, Reads, Store, integrity, io_failure, show_argv},
};
use investigation_core::{command::SemanticRequest, header::SystemClock, identity::SystemIdSource};

#[cfg(test)]
#[path = "reference_tests.rs"]
mod reference_tests;

/// Largest local file accepted in reference mode. Reference mode reads and
/// hashes the file but never copies it.
pub const MAX_REFERENCE_BYTES: u64 = 1 << 40;

/// Bytes held at an external location that this ingest does not supply. The
/// expected digest and size are recorded as declared; the Artifact stays
/// `external` and unverified until supplied bytes match (`reverify`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExternalRefRequest {
    /// Opaque locator. It is recorded as text and never fetched.
    #[schemars(length(min = 1, max = 2048))]
    pub locator: String,
    pub digest: Digest,
    pub size_bytes: u64,
}

/// How local `paths` are kept. `copy` (the default) snapshots the bytes into
/// the workspace. `reference` hashes them without copying: the Artifact is
/// `external` and cannot satisfy an offline-completeness guarantee.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetentionMode {
    #[default]
    Copy,
    Reference,
}

/// Compare the supplied bytes (one path or stdin) with an exact reference-only
/// Artifact revision. Requires `case_id`; creates no Source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReverifyRequest {
    pub artifact_ref: RevisionRef,
}

/// A manifest item whose bytes are not retained.
#[derive(Debug, Clone)]
pub(super) struct PreparedReference {
    pub locator: String,
    pub basis: DigestBasis,
}

pub(super) fn reference_mode(manifest: &Manifest) -> bool {
    manifest.retention == Some(RetentionMode::Reference)
}

/// Shape checks that need no canonical state.
pub(super) fn validate(manifest: &Manifest) -> Result<(), Failure> {
    let reference = reference_mode(manifest);
    if !manifest.external_refs.is_empty() {
        if !manifest.paths.is_empty() || manifest.content.is_some() {
            return Err(invalid(
                "Supply external_refs, paths or one stdin content descriptor, not several.",
                "/external_refs",
            ));
        }
        if manifest.external_refs.len() > MAX_ITEMS {
            return Err(invalid(
                "Name at most 32 external references.",
                "/external_refs",
            ));
        }
        if manifest.retention.is_some() {
            return Err(invalid(
                "External references never copy bytes. Use retention only with local paths.",
                "/retention",
            ));
        }
        if manifest.derived_from.is_some() {
            return Err(invalid(
                "Ingest a derivative's bytes; a derivative cannot be reference-only.",
                "/derived_from",
            ));
        }
        let mut locators = std::collections::BTreeSet::new();
        for (index, item) in manifest.external_refs.iter().enumerate() {
            let path = format!("/external_refs/{index}/locator");
            if item.locator.trim().is_empty()
                || item.locator.chars().count() > MAX_LOCATOR_CHARS
                || item.locator.chars().any(char::is_control)
            {
                return Err(invalid(
                    "Use a locator of 1–2048 characters without control characters.",
                    &path,
                ));
            }
            if !locators.insert(item.locator.as_str()) {
                return Err(invalid("Name each external locator once.", &path));
            }
        }
    }
    if reference && manifest.paths.is_empty() {
        return Err(invalid(
            "Reference mode hashes local paths without copying them. Supply paths.",
            "/retention",
        ));
    }
    if reference && manifest.derived_from.is_some() {
        return Err(invalid(
            "Ingest a derivative's bytes; a derivative cannot be reference-only.",
            "/retention",
        ));
    }
    if manifest.extract_text && (reference || !manifest.external_refs.is_empty()) {
        return Err(invalid(
            "Text extraction needs retained bytes. Omit extract_text for references.",
            "/extract_text",
        ));
    }
    if let Some(reverify) = &manifest.reverify {
        if manifest.case_id.is_none() {
            return Err(invalid(
                "Reverify an Artifact in the existing case that records it. Supply case_id.",
                "/case_id",
            ));
        }
        if reverify.artifact_ref.id.kind() != Kind::Artifact {
            return Err(invalid(
                "Name an exact Artifact revision.",
                "/reverify/artifact_ref",
            ));
        }
        if manifest.paths.len() + usize::from(manifest.content.is_some()) != 1
            || !manifest.external_refs.is_empty()
        {
            return Err(invalid(
                "Supply the bytes to compare as one path or one stdin content descriptor.",
                "/reverify",
            ));
        }
        let source = &manifest.source;
        for (present, field) in [
            (manifest.derived_from.is_some(), "/derived_from"),
            (source.acquisition_key.is_some(), "/source/acquisition_key"),
            (source.producer.is_some(), "/source/producer"),
            (source.publisher.is_some(), "/source/publisher"),
            (source.identity.is_some(), "/source/identity"),
            (!source.metadata.is_empty(), "/source/metadata"),
        ] {
            if present {
                return Err(invalid(
                    "Reverification records no new acquisition. Describe the acquisition on the referenced Artifact's Source.",
                    field,
                ));
            }
        }
    }
    Ok(())
}

fn hash(mut input: impl Read) -> std::io::Result<(Digest, u64)> {
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = input.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        hash.update(&chunk[..read]);
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| std::io::Error::other("file too large"))?;
    }
    format!("sha256:{:x}", hash.finalize())
        .parse()
        .map(|digest| (digest, size))
        .map_err(|_| std::io::Error::other("invalid digest"))
}

fn changed(index: usize) -> Failure {
    Failure::new(
        ErrorCode::IoFailure,
        "The source changed while it was read. Retry with a stable snapshot.",
    )
    .at(format!("/paths/{index}"))
}

fn rank(level: ClassificationLevel) -> u8 {
    match level {
        ClassificationLevel::Public => 0,
        ClassificationLevel::Internal => 1,
        ClassificationLevel::Restricted => 2,
    }
}

/// The supplied bytes of a reverification.
struct Supplied {
    digest: Digest,
    size: u64,
    method: VerificationMethod,
    text: TextStatus,
}

impl<F: DurableFs> Store<F> {
    /// Hash a local file beneath an import root without copying it.
    pub(super) fn read_reference(
        &self,
        path: &Path,
        index: usize,
        during: &mut dyn FnMut(u64),
    ) -> Result<(Digest, u64), Failure> {
        let mut opened = self.open_import(path, index, MAX_REFERENCE_BYTES)?;
        // One byte past the limit is enough to see a file that grew after open;
        // the witness check below then refuses it.
        let (digest, size) = hash(super::Progress::new(
            (&mut opened.file).take(MAX_REFERENCE_BYTES + 1),
            during,
        ))
        .map_err(io_failure)?;
        if !opened
            .witness
            .verify_unchanged(&opened.file)
            .map_err(io_failure)?
        {
            return Err(changed(index));
        }
        Ok((digest, size))
    }

    fn supplied(
        &mut self,
        manifest: &Manifest,
        stream: Option<&mut dyn Read>,
        stages: &mut Vec<StagedBlob>,
    ) -> Result<Supplied, Failure> {
        if let Some(path) = manifest.paths.first() {
            let label = path.to_string_lossy().into_owned();
            if reference_mode(manifest) {
                let (digest, size) = self.read_reference(path, 0, &mut |_| {})?;
                return Ok(Supplied {
                    digest,
                    size,
                    method: VerificationMethod::LocalRead,
                    text: TextStatus::NotRequested,
                });
            }
            let mut opened = self.open_import(path, 0, MAX_IMPORT_BYTES)?;
            let staged = stage_input(
                &mut self.fs,
                &self.gate,
                super::Limited::new(&mut opened.file),
            )?;
            stages.push(staged);
            if !opened
                .witness
                .verify_unchanged(&opened.file)
                .map_err(io_failure)?
            {
                return Err(changed(0));
            }
            return self.supplied_copy(stages, label, manifest.extract_text);
        }
        let Some(StreamContent::Stdin { name }) = &manifest.content else {
            return Err(invalid(
                "Supply the bytes to compare as one path or one stdin content descriptor.",
                "/reverify",
            ));
        };
        if name.trim().is_empty() || name.chars().count() > 256 {
            return Err(invalid(
                "Name the stdin source with 1–256 characters.",
                "/content/name",
            ));
        }
        let stream = stream.ok_or_else(|| {
            invalid(
                "Supply the bytes on stdin using a manifest file.",
                "/content",
            )
        })?;
        stages.push(stage_input(
            &mut self.fs,
            &self.gate,
            super::Limited::new(stream),
        )?);
        self.supplied_copy(stages, name.clone(), manifest.extract_text)
    }

    fn supplied_copy(
        &self,
        stages: &[StagedBlob],
        label: String,
        extract: bool,
    ) -> Result<Supplied, Failure> {
        let item = self.prepare_item(
            stages.last().expect("just staged"),
            label.clone(),
            label,
            investigation_core::evidence::AcquisitionChannel::LocalFile,
            extract,
        )?;
        Ok(Supplied {
            digest: item.digest,
            size: item.size,
            method: VerificationMethod::SuppliedBytes,
            text: item.text,
        })
    }

    /// Compare supplied bytes with a reference-only Artifact and record the
    /// attributed result in its next revision.
    pub(super) fn reverify(
        &mut self,
        bytes: &[u8],
        manifest: &Manifest,
        raw: Value,
        stream: Option<&mut dyn Read>,
        stages: &mut Vec<StagedBlob>,
    ) -> Result<IngestReceipt, Failure> {
        let request = manifest
            .reverify
            .as_ref()
            .ok_or_else(|| integrity("Reverification has no request."))?;
        let case_id = manifest
            .case_id
            .ok_or_else(|| invalid("Supply case_id.", "/case_id"))?;
        let supplied = self.supplied(manifest, stream, stages)?;
        // The named revision is immutable, so its digest and size decide the
        // outcome; the commit below checks that it is still current.
        let replay = self.case(case_id)?;
        let body = reference_body(&replay.state, case_id, request.artifact_ref)?;
        let matched = body.digest.as_ref() == Some(&supplied.digest)
            && body.size_bytes == Some(supplied.size);
        let retain = matched && supplied.method == VerificationMethod::SuppliedBytes;
        let copied = usize::from(supplied.method == VerificationMethod::SuppliedBytes);
        let staged_manifest = stage_input(&mut self.fs, &self.gate, bytes)?;
        let manifest_digest = digest_for(&staged_manifest)?;
        let manifest_size = staged_manifest.size;
        stages.push(staged_manifest);
        // Bytes that do not match are never retained in place of the reference.
        let blobs = if retain {
            &stages[..]
        } else {
            &stages[copied..]
        };
        let attachments = if retain {
            vec![supplied.digest.clone()]
        } else {
            Vec::new()
        };
        let receipt = self.commit_plan(
            CommitPlan {
                case_id: Some(case_id),
                reads: Reads::Target,
                event: EventInput {
                    actor: &manifest.actor,
                    command_id: &manifest.command_id,
                    preconditions: &[],
                    manifest_digest: Some(manifest_digest.clone()),
                    session_reads: None,
                },
                request: SemanticRequest {
                    input: raw,
                    attachments,
                },
                blobs,
            },
            &mut SystemClock,
            &mut SystemIdSource,
            |state, _, acceptance, _| {
                let current = state
                    .current(request.artifact_ref.id)
                    .filter(|record| record.header.case_id == case_id);
                let Some(current) =
                    current.filter(|record| record.reference() == request.artifact_ref)
                else {
                    let mut failure = Failure::new(
                        ErrorCode::RevisionConflict,
                        "Reverify the current revision of the Artifact.",
                    )
                    .at("/reverify/artifact_ref")
                    .with_argv(show_argv(request.artifact_ref.id, case_id));
                    failure.error.expected = Some(json!(request.artifact_ref.revision));
                    failure.error.actual = current.map(|record| json!(record.reference().revision));
                    return Err(failure);
                };
                record_verification(
                    manifest,
                    current,
                    &body,
                    &supplied,
                    matched,
                    retain,
                    &manifest_digest,
                    manifest_size,
                    acceptance,
                )
                .map(Prepared::Accept)
            },
        )?;
        let id = |client_ref: &str| {
            receipt.created.iter().find_map(|change| {
                (change.client_ref.as_deref() == Some(client_ref)).then_some(change.id)
            })
        };
        let manifest_artifact_id = id("manifest");
        let derivative_id = id("text:0");
        let revised = receipt
            .updated
            .iter()
            .find(|change| change.id == request.artifact_ref.id)
            .map(|change| RevisionRef {
                id: change.id,
                revision: change.revision,
            })
            .ok_or_else(|| integrity("The reverification receipt names no Artifact revision."))?;
        let state = self.state_after_write(case_id)?;
        let retention = state
            .revision(revised)
            .and_then(artifact_retention)
            .ok_or_else(|| integrity("The reverified Artifact revision is unreadable."))?;
        let verified = retention.verification == Some(VerificationState::Verified);
        let mut receipt = receipt;
        if !verified {
            receipt.warnings.push(format!(
                "The supplied bytes differ from the digest or size recorded for {}. Nothing was replaced: the recorded digest stands, the supplied bytes are not retained, and records that cite this Artifact need review. To keep the changed bytes, ingest them as a separate acquisition.",
                request.artifact_ref.id
            ));
        }
        Ok(IngestReceipt {
            receipt,
            items: vec![ItemReceipt {
                source_id: body.source.id,
                artifact_id: revised.id,
                digest: supplied.digest,
                size_bytes: supplied.size,
                derivative_id,
                text_status: supplied.text,
                diagnostics: Vec::new(),
                outcome: Some(if verified {
                    ItemOutcome::Reverified
                } else {
                    ItemOutcome::ReverificationFailed
                }),
                acquisition_key: None,
                prior: None,
                lineage: None,
                retention: Some(retention),
            }],
            manifest_artifact_id,
        })
    }
}

/// The body of an exact reference-only Artifact revision in the case.
fn reference_body(
    state: &investigation_core::state::State,
    case_id: Id,
    reference: RevisionRef,
) -> Result<ArtifactBody, Failure> {
    let record = state
        .revision(reference)
        .filter(|record| record.header.case_id == case_id)
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::UnknownReference,
                "The Artifact revision is absent from this case.",
            )
            .at("/reverify/artifact_ref")
        })?;
    let body: ArtifactBody = investigation_core::input::decode(&record.body)
        .map_err(|_| integrity("Invalid canonical Artifact record."))?;
    if body.availability != ArtifactAvailability::External || body.external.is_none() {
        return Err(invalid(
            "Only a reference-only Artifact (availability external) can be reverified. Retained bytes are verified by investigation check.",
            "/reverify/artifact_ref",
        )
        .with_argv(show_argv(reference.id, case_id)));
    }
    Ok(body)
}

#[allow(clippy::too_many_arguments)]
fn record_verification(
    manifest: &Manifest,
    current: &Record,
    body: &ArtifactBody,
    supplied: &Supplied,
    matched: bool,
    retain: bool,
    manifest_digest: &Digest,
    manifest_size: u64,
    acceptance: Acceptance<'_>,
) -> Result<AcceptedTransaction, Failure> {
    let recorded = current.header.classification.level;
    let classification = if rank(recorded) > rank(manifest.classification) {
        recorded
    } else {
        manifest.classification
    };
    let mut builder = IngestRecords {
        actor: &manifest.actor,
        descriptor: manifest.actor_descriptor.as_ref(),
        classification,
        case_id: current.header.case_id,
        timestamp: acceptance.clock.now_utc(),
        sequence: acceptance.sequence,
        ids: acceptance.ids,
        records: Vec::new(),
        created: Vec::new(),
        updated: Vec::new(),
    };
    let mut external: ExternalReference = body
        .external
        .clone()
        .ok_or_else(|| integrity("A reference-only Artifact has no locator."))?;
    external.verifications.push(ExternalVerification {
        outcome: if matched {
            VerificationOutcome::Verified
        } else {
            VerificationOutcome::Mismatch
        },
        method: supplied.method,
        observed_digest: supplied.digest.clone(),
        observed_size: supplied.size,
        verified_at: builder.timestamp.clone(),
        verified_by: manifest.actor.clone(),
        command_id: manifest.command_id.clone(),
    });
    let mut revised = body.clone();
    revised.external = Some(external);
    if matched {
        revised.integrity = ArtifactIntegrity::Verified;
        if retain {
            revised.availability = ArtifactAvailability::Retained;
        }
    } else {
        // The recorded digest and size stand; only the result is recorded.
        revised.integrity = ArtifactIntegrity::Mismatch;
    }
    let reference = builder.revise(current, &revised)?;
    if retain && matches!(supplied.text, TextStatus::Decoded) {
        add_text_derivative(&mut builder, 0, reference, &revised)?;
    }
    let id = new_id(builder.ids, Kind::Artifact)?;
    builder.add(
        id,
        "manifest".into(),
        ArtifactBody {
            digest: Some(manifest_digest.clone()),
            size_bytes: Some(manifest_size),
            format: Some("application/json".into()),
            source: body.source,
            transforms: Vec::new(),
            availability: ArtifactAvailability::Retained,
            integrity: ArtifactIntegrity::Verified,
            sensitivity: sensitivity(classification),
            status: None,
            withdrawals: Vec::new(),
            availability_change: None,
            role: Some(ArtifactRole::Manifest),
            derivation: None,
            external: None,
        },
        vec![body.source.id, reference.id],
    )?;
    Ok(AcceptedTransaction {
        case_id: builder.case_id,
        sequence: builder.sequence,
        records: builder.records,
        created: builder.created,
        updated: builder.updated,
        warnings: Vec::new(),
    })
}

/// The recorded external block of a new reference-only Artifact.
pub(super) fn external_block(
    reference: &PreparedReference,
    digest: &Digest,
    size: u64,
    manifest: &Manifest,
    timestamp: &investigation_core::header::UtcTimestamp,
) -> ExternalReference {
    let verifications = match reference.basis {
        DigestBasis::Declared => Vec::new(),
        DigestBasis::ReadAtIngest => vec![ExternalVerification {
            outcome: VerificationOutcome::Verified,
            method: VerificationMethod::LocalRead,
            observed_digest: digest.clone(),
            observed_size: size,
            verified_at: timestamp.clone(),
            verified_by: manifest.actor.clone(),
            command_id: manifest.command_id.clone(),
        }],
    };
    ExternalReference {
        locator: reference.locator.clone(),
        digest_basis: reference.basis,
        verifications,
    }
}

/// Integrity recorded for a new reference: verified only when the core read
/// the bytes itself.
pub(super) fn reference_integrity(reference: &PreparedReference) -> ArtifactIntegrity {
    match reference.basis {
        DigestBasis::Declared => ArtifactIntegrity::NotChecked,
        DigestBasis::ReadAtIngest => ArtifactIntegrity::Verified,
    }
}
