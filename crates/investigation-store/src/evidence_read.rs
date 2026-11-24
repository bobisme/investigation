//! Digest-bound reads and observation verification over retained snapshots.
use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use investigation_core::{
    classification::{AudiencePolicy, Classifier, Field},
    command::Reference,
    evidence::{
        ArtifactAvailability, ArtifactBody, ArtifactIntegrity, ObservationBasisRequest,
        SelectorKind, SelectorRequest, SelectorValidation, TextQuoteRequest,
    },
    input,
    observation_ops::ObservationProof,
    state::Record,
};
use std::io::Read;

use crate::{
    durable::FileIdentity,
    selectors::{self, QuoteResult, Selection, SelectorError, TextDerivative},
};

pub const MAX_EXCERPT_BYTES: usize = 32 * 1024;
/// Largest retained artifact a native selector reads. Selectors resolve over
/// the whole artifact in memory, so this is separate from the streamed import
/// limit: a larger artifact is retained and verifiable, but a selector cannot
/// cite it directly.
pub const MAX_SELECTOR_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum Excerpt {
    Text {
        start: usize,
        end: usize,
        text: String,
    },
    JsonValue {
        value: Value,
    },
    Binary {
        start: usize,
        end: usize,
        bytes_base64: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceRead {
    pub artifact: RevisionRef,
    pub source: RevisionRef,
    pub snapshot: Snapshot,
    pub selector: SelectorRequest,
    pub content_trust: &'static str,
    /// Withdrawn bytes stay readable and are labeled here; withdrawal is not erasure.
    pub evidentiary_use: investigation_core::source_ops::EvidentiaryUse,
    pub excerpt: Excerpt,
}

/// How many commit attempts may end to hash promotion evidence outside the
/// commit lock. Each needs another writer to change the evidence a promotion
/// reads, or a blob to change, between two attempts. The attempt after the
/// last of them hashes what it still needs under the lock, so a command
/// always finishes.
const DEFERRED_ATTEMPTS: usize = 3;

/// One retained blob a promotion reads: its path and recorded digest and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RetainedBlob {
    path: PathBuf,
    hex: String,
    size: u64,
}

/// One hash of a retained blob, with the file's identity seen just before
/// the read. While the identity is unchanged, so is the result.
struct Hashed {
    identity: Option<FileIdentity>,
    intact: bool,
}

/// Retained promotion evidence one command hashed outside the commit lock.
///
/// Blobs are immutable and named by their digest, so a hash taken before the
/// lock holds under it unless the file changed in between. Under the lock
/// only the file's identity is compared (`DurableFs::identity`: device,
/// inode, size, modification and change times). A blob that is missing there
/// is unavailable at once; one with no hash yet, or a changed identity, is
/// pending, and the attempt ends without writing so it can be hashed again.
#[derive(Default)]
pub(super) struct PromotionBlobs {
    hashed: BTreeMap<(String, u64), Hashed>,
    attempts: usize,
    /// Whether a blob without a current hash is left pending. Otherwise it
    /// is hashed under the lock.
    defer: bool,
    /// What the last locked attempt needed and could not use a hash for.
    pub(super) pending: Vec<RetainedBlob>,
}

impl PromotionBlobs {
    /// Hash every needed blob under the lock, for commit paths that cannot
    /// retry.
    pub(super) fn under_lock() -> Self {
        Self::default()
    }

    /// Start an attempt. The first [`DEFERRED_ATTEMPTS`] attempts defer.
    pub(super) fn next_attempt(&mut self) {
        self.attempts += 1;
        self.defer = self.attempts <= DEFERRED_ATTEMPTS;
        self.pending.clear();
    }
}

/// Where the retained bytes of `reference` are, or `None` when the state
/// expects none that can be verified. A reference whose availability was
/// withdrawn in `state` or in `today` (`artifact.expire`) has none, at
/// whichever revision or snapshot it is cited.
fn retained_blob(state: &State, today: &State, reference: RevisionRef) -> Option<RetainedBlob> {
    if investigation_core::retention_ops::bytes_withdrawn(state, Some(today), reference.id) {
        return None;
    }
    let body = artifact_body(state.revision(reference)?).ok()?;
    if body.availability != ArtifactAvailability::Retained {
        return None;
    }
    let (Some(digest), Some(size)) = (body.digest, body.size_bytes) else {
        return None;
    };
    let hex = digest.as_str().strip_prefix("sha256:")?;
    Some(RetainedBlob {
        path: Path::new(BLOBS).join(hex.get(..2)?).join(hex),
        hex: hex.to_owned(),
        size,
    })
}

impl<F: DurableFs> Store<F> {
    /// For a read path (`show`, `brief`, `explain`, `search` and every other
    /// view of effective state): the exact references whose expected
    /// retained bytes are missing or do not match. `classification` is the
    /// state whose effective classification governs disclosure: the current
    /// head, also when `state` is an earlier snapshot.
    ///
    /// This never reads the bytes of an Artifact whose effective
    /// classification hides its content from the default audience: such a
    /// blob is available when a verdict recorded for its current file
    /// identity says intact or, with none, when the file is present with its
    /// recorded size. Any other blob is hashed unless a verdict is recorded
    /// for its current identity (see `blob_verdicts`). Promotion gates and
    /// `check` do not use this: they hash every blob they need.
    pub fn unavailable_artifacts(
        &self,
        state: &State,
        classification: &State,
        references: &[RevisionRef],
    ) -> Vec<RevisionRef> {
        let classifier = Classifier::new(classification);
        let policy = AudiencePolicy::default();
        references
            .iter()
            .copied()
            .filter(|reference| {
                if reference.id.kind() != Kind::Artifact {
                    return false;
                }
                let Some(blob) = retained_blob(state, classification, *reference) else {
                    return true;
                };
                let restricted = state
                    .revision(*reference)
                    .is_none_or(|artifact| !classifier.may_show(artifact, Field::Content, &policy));
                !self.read_intact(&blob, restricted)
            })
            .collect()
    }

    /// Whether a read path may call the blob intact. A `restricted` blob is
    /// never opened.
    fn read_intact(&self, blob: &RetainedBlob, restricted: bool) -> bool {
        let Ok(current) = self.fs.identity(&blob.path) else {
            return false;
        };
        if let Some(intact) = self.cached_verdict(&blob.hex, blob.size, &current) {
            return intact;
        }
        if restricted {
            return current.size() == blob.size;
        }
        self.hash(blob).intact
    }

    /// Read the whole blob and record the verdict. Never call this under the
    /// commit lock on a path that can defer instead.
    fn hash(&self, blob: &RetainedBlob) -> Hashed {
        let identity = self.fs.identity(&blob.path).ok();
        let verified =
            identity.map(|_| crate::blob::verify_blob(&self.fs, &blob.path, &blob.hex, blob.size));
        let intact = matches!(verified, Some(Ok(())));
        if let (Some(before), Some(Ok(()) | Err(crate::blob::BlobError::DigestMismatch))) =
            (&identity, &verified)
        {
            self.record_verdict(&blob.hex, blob.size, before, intact);
        }
        Hashed { identity, intact }
    }

    /// Hash, without the commit lock, what the last attempt left pending.
    pub(super) fn hash_pending(&self, promotion: &mut PromotionBlobs) {
        for blob in std::mem::take(&mut promotion.pending) {
            let hashed = self.hash(&blob);
            promotion.hashed.insert((blob.hex, blob.size), hashed);
        }
    }

    /// Under the commit lock: whether the blob is intact, or `None` when it
    /// is left pending.
    fn locked_intact(&self, blob: &RetainedBlob, promotion: &PromotionBlobs) -> Option<bool> {
        let Ok(current) = self.fs.identity(&blob.path) else {
            return Some(false);
        };
        match promotion.hashed.get(&(blob.hex.clone(), blob.size)) {
            Some(hashed) if hashed.identity == Some(current) => Some(hashed.intact),
            _ if promotion.defer => None,
            _ => Some(self.hash(blob).intact),
        }
    }

    pub(super) fn verify_promotion_artifacts(
        &self,
        state: &State,
        accepted: &AcceptedTransaction,
        promotion: &mut PromotionBlobs,
    ) -> Result<(), Failure> {
        for record in &accepted.records {
            let targets = match record.header.id.kind() {
                Kind::Conclusion
                    if matches!(
                        record.body["status"].as_str(),
                        Some("review_ready" | "validated")
                    ) =>
                {
                    investigation_core::review_ops::required_read_set(state, record)?
                }
                Kind::Decision if record.body["lifecycle"] == "accepted" => {
                    investigation_core::review_ops::required_read_set(state, record)?
                }
                Kind::Review
                    if record.body["kind"] == "validation"
                        && record.body["verdict"] == "supports" =>
                {
                    input::decode::<Vec<RevisionRef>>(&record.body["read_set"])
                        .map_err(|_| integrity("Invalid canonical validation read set."))?
                }
                _ => continue,
            };
            let mut missing = Vec::new();
            for reference in targets
                .into_iter()
                .filter(|reference| reference.id.kind() == Kind::Artifact)
            {
                let available = match retained_blob(state, state, reference) {
                    None => false,
                    Some(blob) => match self.locked_intact(&blob, promotion) {
                        Some(intact) => intact,
                        // Not missing: the next attempt decides.
                        None => {
                            if !promotion.pending.contains(&blob) {
                                promotion.pending.push(blob);
                            }
                            true
                        }
                    },
                };
                if !available {
                    missing.push(reference);
                }
            }
            if !missing.is_empty() {
                let mut failure = integrity(
                    "This promotion requires retained evidence that is missing or corrupt.",
                )
                .with_argv(read_argv("check"));
                failure.error.actual = Some(json!({"unavailable_artifacts":missing}));
                return Err(failure);
            }
        }
        Ok(())
    }

    pub fn evidence_read(
        &self,
        replay: &CaseReplay,
        artifact: &Record,
        selector: SelectorRequest,
    ) -> Result<EvidenceRead, Failure> {
        // Effective level: a derivative of restricted bytes is restricted too.
        if !Classifier::new(&replay.state).may_show(
            artifact,
            Field::Content,
            &AudiencePolicy::default(),
        ) {
            return Err(Failure::new(
                ErrorCode::GateFailed,
                "Restricted source content is unavailable in this excerpt.",
            )
            .at("/artifact"));
        }
        let body = artifact_body(artifact)?;
        if body.availability == ArtifactAvailability::IntentionallyRemoved
            || investigation_core::retention_ops::bytes_withdrawn(
                &replay.state,
                None,
                artifact.header.id,
            )
        {
            return Err(availability_withdrawn(&replay.state, artifact));
        }
        if body.availability != ArtifactAvailability::Retained {
            // Nothing is substituted for bytes the workspace does not hold.
            let mut failure = Failure::new(
                ErrorCode::GateFailed,
                "The Artifact's bytes are not retained in this workspace (source_unavailable), so nothing can be read or verified from them. Supply the bytes with an ingest manifest that names this Artifact in reverify.",
            )
            .at("/artifact")
            .with_schema("ingest");
            failure.error.actual = Some(json!({
                "state": "source_unavailable",
                "retention": investigation_core::external_ref::artifact_retention(artifact),
            }));
            return Err(failure);
        }
        let bytes = self.read_artifact(&body, &selector)?;
        let text = derivative(&body);
        let selection = selectors::resolve(
            Some(&bytes),
            &selector.selector,
            text.as_ref(),
            MAX_EXCERPT_BYTES,
        )
        .map_err(selection_failure)?;
        let excerpt = match selection {
            Selection::Bytes { start, end } => match std::str::from_utf8(&bytes[start..end]) {
                Ok(text) if !text.contains('\0') => Excerpt::Text {
                    start,
                    end,
                    text: text.into(),
                },
                _ => Excerpt::Binary {
                    start,
                    end,
                    bytes_base64: STANDARD.encode(&bytes[start..end]),
                },
            },
            Selection::Json(value) => Excerpt::JsonValue { value },
        };
        Ok(EvidenceRead {
            artifact: artifact.reference(),
            source: body.source,
            snapshot: replay.snapshot.clone(),
            selector,
            content_trust: "untrusted_evidence",
            evidentiary_use: investigation_core::source_ops::evidentiary_use(
                &replay.state,
                artifact,
            ),
            excerpt,
        })
    }

    pub(super) fn prepare_observations(
        &mut self,
        command: &Command,
    ) -> BTreeMap<usize, Result<ObservationProof, Failure>> {
        let sources: Vec<_> = command
            .operations
            .iter()
            .enumerate()
            .filter_map(|(index, operation)| {
                if !matches!(
                    operation.op.as_str(),
                    "observation.add" | "observation.correct"
                ) {
                    return None;
                }
                let basis =
                    input::decode::<ObservationBasisRequest>(operation.payload.get("basis")?)
                        .ok()?;
                if let ObservationBasisRequest::Source {
                    artifact_ref: Reference::Existing(reference),
                    selector,
                    text_quote,
                } = basis
                {
                    Some((index, reference, selector, text_quote))
                } else {
                    None
                }
            })
            .collect();
        if sources.is_empty() {
            return BTreeMap::new();
        }
        let Some(case) = command.case_id else {
            return BTreeMap::new();
        };
        let snapshot = self.case(case);
        sources
            .into_iter()
            .filter_map(|(index, reference, selector, quote)| {
                let replay = match &snapshot {
                    Ok(replay) => replay,
                    Err(error) => return Some((index, Err(error.clone()))),
                };
                let record = replay.state.revision(reference)?;
                // A read follows the reference's current availability.
                let withdrawn = investigation_core::retention_ops::bytes_withdrawn(
                    &replay.state,
                    None,
                    reference.id,
                );
                Some((
                    index,
                    self.verify_observation(record, &selector, quote.as_ref(), withdrawn),
                ))
            })
            .collect()
    }

    fn verify_observation(
        &self,
        artifact: &Record,
        selector: &SelectorRequest,
        quote: Option<&TextQuoteRequest>,
        withdrawn: bool,
    ) -> Result<ObservationProof, Failure> {
        let body = artifact_body(artifact)?;
        let mut proof = ObservationProof {
            artifact_ref: artifact.reference(),
            digest: selector.artifact_digest.clone(),
            selector_validation: SelectorValidation::Unavailable,
            quote_validation: quote.map(|_| SelectorValidation::Unavailable),
        };
        if withdrawn || body.availability != ArtifactAvailability::Retained {
            return Ok(proof);
        }
        let bytes = self.read_artifact(&body, selector)?;
        let text = derivative(&body);
        let selection = match selectors::resolve(
            Some(&bytes),
            &selector.selector,
            text.as_ref(),
            MAX_SELECTOR_ARTIFACT_BYTES as usize,
        ) {
            Ok(selection) => selection,
            Err(SelectorError::Unavailable(_)) => return Ok(proof),
            Err(SelectorError::Ambiguous { .. }) => {
                proof.selector_validation = SelectorValidation::Ambiguous;
                return Ok(proof);
            }
            Err(error) => return Err(selection_failure(error)),
        };
        proof.selector_validation = SelectorValidation::Verified;
        // A quote is checked inside a selection the reader already proved to
        // be text: a decoded-text derivative, a table cell or a code blob.
        let textual = text.is_some()
            || matches!(
                selector.selector,
                SelectorKind::TableCell { .. } | SelectorKind::CodeSpan { .. }
            );
        if let (Some(quote), true, Selection::Bytes { start, end }) = (quote, textual, selection) {
            let result = selectors::verify_quote(
                Some(&bytes[start..end]),
                &quote.exact,
                quote.prefix.as_deref(),
                quote.suffix.as_deref(),
            )
            .map_err(selection_failure)?;
            proof.quote_validation = Some(match result {
                QuoteResult::Verified { .. } => SelectorValidation::Verified,
                QuoteResult::Ambiguous { .. } => SelectorValidation::Ambiguous,
                QuoteResult::NotFound => SelectorValidation::NotFound,
            });
        }
        Ok(proof)
    }

    pub(super) fn read_artifact(
        &self,
        body: &ArtifactBody,
        selector: &SelectorRequest,
    ) -> Result<Vec<u8>, Failure> {
        if body.availability != ArtifactAvailability::Retained {
            return Err(selection_failure(SelectorError::Unavailable(
                "artifact bytes are not retained",
            )));
        }
        if matches!(
            body.integrity,
            ArtifactIntegrity::Mismatch | ArtifactIntegrity::Unavailable
        ) {
            return Err(integrity(
                "The retained artifact has no usable integrity state.",
            ));
        }
        let Some(digest) = &body.digest else {
            return Err(integrity("Retained artifact has no digest."));
        };
        if digest != &selector.artifact_digest {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                "The selector names a different artifact digest.",
            )
            .at("/basis/selector/artifact_digest")
            .with_schema("evidence-selector"));
        }
        let size = body
            .size_bytes
            .ok_or_else(|| integrity("Retained artifact size is absent."))?;
        if size > MAX_SELECTOR_ARTIFACT_BYTES {
            let mut failure = Failure::new(
                ErrorCode::InvalidInput,
                format!(
                    "The artifact holds {size} bytes; a native selector reads at most {MAX_SELECTOR_ARTIFACT_BYTES} bytes. Its bytes stay retained and verifiable. To cite part of it, ingest that part with derived_from naming this artifact and select within the derivative."
                ),
            )
            .at("/basis/selector")
            .with_schema("evidence-selector");
            failure.error.actual = Some(json!({"size_bytes": size}));
            failure.error.expected = Some(json!({"max_size_bytes": MAX_SELECTOR_ARTIFACT_BYTES}));
            return Err(failure);
        }
        let hex = digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or_else(|| integrity("Unsupported artifact digest."))?;
        let path = Path::new(BLOBS).join(&hex[..2]).join(hex);
        let before = self.fs.identity(&path).ok();
        let mut bytes = Vec::new();
        self.fs
            .read(&path)
            .and_then(|reader| reader.take(size + 1).read_to_end(&mut bytes))
            .map_err(|_| {
                integrity("The retained blob could not be opened.").with_argv(read_argv("check"))
            })?;
        let intact = bytes.len() as u64 == size && hash_bytes(&bytes) == *digest;
        if let Some(before) = &before {
            self.record_verdict(hex, size, before, intact);
        }
        if !intact {
            return Err(integrity(
                "The retained blob does not match its recorded size and digest.",
            )
            .with_argv(read_argv("check")));
        }
        Ok(bytes)
    }
}

/// The refusal of a read through a reference whose availability was
/// withdrawn. It names the withdrawal and suggests nothing that would put
/// the bytes back: another reference keeps its own availability.
fn availability_withdrawn(state: &State, artifact: &Record) -> Failure {
    let current = state.current(artifact.header.id).unwrap_or(artifact);
    let mut failure = Failure::new(
        ErrorCode::GateFailed,
        "This Artifact's availability was withdrawn (artifact.expire), so nothing can be read or verified through it, at any snapshot. Its digest, size and provenance stay recorded. Other references to the same bytes keep their own availability.",
    )
    .at("/artifact");
    failure.error.actual = Some(json!({
        "state": "source_unavailable",
        "availability": ArtifactAvailability::IntentionallyRemoved,
        "artifact_ref": current.reference(),
        "availability_change": investigation_core::retention_ops::availability_change(current),
    }));
    failure
}

fn artifact_body(artifact: &Record) -> Result<ArtifactBody, Failure> {
    if artifact.header.id.kind() != Kind::Artifact {
        return Err(
            Failure::new(ErrorCode::InvalidInput, "Supply an Artifact ID.").at("/artifact"),
        );
    }
    input::decode(&artifact.body).map_err(|_| integrity("Invalid canonical Artifact record."))
}

pub(super) fn derivative(body: &ArtifactBody) -> Option<TextDerivative> {
    let transform = body.transforms.iter().find(|transform| {
        transform.transformation == "decode_utf8"
            && transform.version == ingest::TEXT_PARSER_VERSION
    })?;
    Some(TextDerivative {
        encoding: "utf-8".into(),
        parser_version: transform.version.clone(),
    })
}

fn selection_failure(error: SelectorError) -> Failure {
    let unavailable = matches!(error, SelectorError::Unavailable(_));
    let (code, message) = match error {
        SelectorError::Unavailable(reason) => (ErrorCode::GateFailed, reason.into()),
        SelectorError::Invalid(reason) => (ErrorCode::InvalidInput, reason),
        SelectorError::Ambiguous {
            matches,
            what,
            repair,
        } => (
            ErrorCode::GateFailed,
            format!("{what} has {matches} exact matches. {repair}"),
        ),
        SelectorError::TooLarge { size, limit } => (
            ErrorCode::InvalidInput,
            format!("Selection has {size} bytes. Select at most {limit} bytes."),
        ),
    };
    let mut failure = Failure::new(code, message)
        .at("/basis/selector")
        .with_schema("evidence-selector");
    if unavailable {
        // The selector is not refuted: the core cannot check it here.
        failure.error.actual = Some(json!({"validation": "unavailable"}));
    }
    failure
}

#[path = "blob_verdicts.rs"]
mod blob_verdicts;

#[cfg(test)]
#[path = "evidence_read_lock_tests.rs"]
mod lock_tests;

#[cfg(test)]
#[path = "read_path_hash_tests.rs"]
mod read_path_hash_tests;
