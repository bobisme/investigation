//! Digest-bound reads and observation verification over retained snapshots.
use super::*;
use investigation_core::{
    classification::{AudiencePolicy, Classifier, Field},
    command::Reference,
    evidence::{
        ArtifactAvailability, ArtifactBody, ArtifactIntegrity, ObservationBasisRequest,
        SelectorRequest, SelectorValidation, TextQuoteRequest,
    },
    input,
    observation_ops::ObservationProof,
    state::Record,
};
use std::io::Read;

use crate::selectors::{self, QuoteResult, Selection, SelectorError, TextDerivative};

const MAX_EXCERPT_BYTES: usize = 32 * 1024;

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

impl<F: DurableFs> Store<F> {
    /// Return exact references whose expected retained bytes cannot be verified.
    pub fn unavailable_artifacts(
        &self,
        state: &State,
        references: &[RevisionRef],
    ) -> Vec<RevisionRef> {
        references
            .iter()
            .copied()
            .filter(|reference| {
                reference.id.kind() == Kind::Artifact
                    && state.revision(*reference).is_none_or(|record| {
                        let Ok(body) = artifact_body(record) else {
                            return true;
                        };
                        if body.availability != ArtifactAvailability::Retained {
                            return true;
                        }
                        let (Some(digest), Some(size)) = (body.digest, body.size_bytes) else {
                            return true;
                        };
                        let Some(hex) = digest.as_str().strip_prefix("sha256:") else {
                            return true;
                        };
                        let path = Path::new(BLOBS).join(&hex[..2]).join(hex);
                        crate::blob::verify_blob(&self.fs, &path, hex, size).is_err()
                    })
            })
            .collect()
    }

    pub(super) fn verify_promotion_artifacts(
        &self,
        state: &State,
        accepted: &AcceptedTransaction,
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
            let missing = self.unavailable_artifacts(state, &targets);
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
                _ => Excerpt::Binary { start, end },
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
                Some((
                    index,
                    self.verify_observation(record, &selector, quote.as_ref()),
                ))
            })
            .collect()
    }

    fn verify_observation(
        &self,
        artifact: &Record,
        selector: &SelectorRequest,
        quote: Option<&TextQuoteRequest>,
    ) -> Result<ObservationProof, Failure> {
        let body = artifact_body(artifact)?;
        let mut proof = ObservationProof {
            artifact_ref: artifact.reference(),
            digest: selector.artifact_digest.clone(),
            selector_validation: SelectorValidation::Unavailable,
            quote_validation: quote.map(|_| SelectorValidation::Unavailable),
        };
        if body.availability != ArtifactAvailability::Retained {
            return Ok(proof);
        }
        let bytes = self.read_artifact(&body, selector)?;
        let text = derivative(&body);
        let selection = match selectors::resolve(
            Some(&bytes),
            &selector.selector,
            text.as_ref(),
            ingest::MAX_IMPORT_BYTES as usize,
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
        if let (Some(quote), Some(_), Selection::Bytes { start, end }) = (quote, text, selection) {
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

    fn read_artifact(
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
            .filter(|size| *size <= ingest::MAX_IMPORT_BYTES)
            .ok_or_else(|| {
                integrity("Retained artifact size is absent or exceeds the ingest limit.")
            })?;
        let hex = digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or_else(|| integrity("Unsupported artifact digest."))?;
        let path = Path::new(BLOBS).join(&hex[..2]).join(hex);
        let mut bytes = Vec::new();
        self.fs
            .read(&path)
            .and_then(|reader| reader.take(size + 1).read_to_end(&mut bytes))
            .map_err(|_| {
                integrity("The retained blob could not be opened.").with_argv(read_argv("check"))
            })?;
        if bytes.len() as u64 != size || hash_bytes(&bytes) != *digest {
            return Err(integrity(
                "The retained blob does not match its recorded size and digest.",
            )
            .with_argv(read_argv("check")));
        }
        Ok(bytes)
    }
}

fn artifact_body(artifact: &Record) -> Result<ArtifactBody, Failure> {
    if artifact.header.id.kind() != Kind::Artifact {
        return Err(
            Failure::new(ErrorCode::InvalidInput, "Supply an Artifact ID.").at("/artifact"),
        );
    }
    input::decode(&artifact.body).map_err(|_| integrity("Invalid canonical Artifact record."))
}

fn derivative(body: &ArtifactBody) -> Option<TextDerivative> {
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
    let (code, message) = match error {
        SelectorError::Unavailable(reason) => (ErrorCode::GateFailed, reason.into()),
        SelectorError::Invalid(reason) => (ErrorCode::InvalidInput, reason),
        SelectorError::Ambiguous { matches } => (
            ErrorCode::GateFailed,
            format!("Quote has {matches} exact matches. Add prefix or suffix context."),
        ),
        SelectorError::TooLarge { size, limit } => (
            ErrorCode::InvalidInput,
            format!("Selection has {size} bytes. Select at most {limit} bytes."),
        ),
    };
    Failure::new(code, message)
        .at("/basis/selector")
        .with_schema("evidence-selector")
}
