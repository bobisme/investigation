//! Line spans of recorded citations, for a successor who must cite the
//! source by file and line.
//!
//! A line span is located only where the core already proved the location
//! when the Observation was accepted: a verified quote, or a verified
//! `text_lines` selector. A verified quote is found again in the same
//! retained bytes, within the same selection, by the same exact matcher, so
//! the span is where that verification found it. Nothing is located for any
//! other state, and nothing here changes a recorded state.
use super::*;
use investigation_core::{
    classification::{AudiencePolicy, Classifier, Field},
    evidence::{
        ArtifactAvailability, EvidenceSelector, SelectorKind, SelectorRequest, SelectorValidation,
        TextQuote, TextQuoteState,
    },
    input,
};

use crate::selectors::{self, QuoteResult, Selection};

use super::evidence_read::{MAX_SELECTOR_ARTIFACT_BYTES, derivative};

/// One recorded citation to locate: the artifact revision an Observation
/// cites, its accepted selector and its recorded quote.
#[derive(Debug, Clone, Copy)]
pub struct CitedSpan<'a> {
    pub artifact: RevisionRef,
    pub selector: &'a EvidenceSelector,
    pub quote: &'a TextQuote,
}

/// Where a citation lies in the retained bytes of the cited artifact revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CitedLines {
    /// One-based, inclusive line numbers: of the verified quote, or of the
    /// verified `text_lines` selector when no quote is recorded.
    Located { start: u64, end: u64 },
    /// No span can be given; the reason is a fixed sentence.
    NotLocated(&'static str),
    /// The citation carries neither a quote nor a line selector.
    NotApplicable,
}

impl<F: DurableFs> Store<F> {
    /// Locate every citation in `spans`, keyed by the caller's key. Each
    /// cited artifact is read and digest-checked once. `classification` is
    /// the state whose effective classification governs disclosure: bytes
    /// whose content it hides from the default audience are never opened.
    pub fn cited_lines<K: Ord + Copy>(
        &self,
        state: &State,
        classification: &State,
        spans: &[(K, CitedSpan<'_>)],
    ) -> BTreeMap<K, CitedLines> {
        let classifier = Classifier::new(classification);
        let policy = AudiencePolicy::default();
        let mut by_artifact: BTreeMap<RevisionRef, Vec<(K, CitedSpan<'_>)>> = BTreeMap::new();
        let mut out = BTreeMap::new();
        for (key, span) in spans {
            match needs_bytes(span) {
                Ok(()) => by_artifact
                    .entry(span.artifact)
                    .or_default()
                    .push((*key, *span)),
                Err(lines) => {
                    out.insert(*key, lines);
                }
            }
        }
        for (artifact, spans) in by_artifact {
            match self.cited_bytes(state, &classifier, &policy, artifact, spans[0].1.selector) {
                Ok((bytes, derivative)) => {
                    let cited = Cited {
                        text: std::str::from_utf8(&bytes).ok(),
                        line_feeds: bytes
                            .iter()
                            .enumerate()
                            .filter_map(|(at, byte)| (*byte == b'\n').then_some(at))
                            .collect(),
                        bytes: &bytes,
                        derivative: derivative.as_ref(),
                    };
                    for (key, span) in spans {
                        out.insert(key, cited.locate(span));
                    }
                }
                Err(reason) => {
                    for (key, _) in spans {
                        out.insert(key, CitedLines::NotLocated(reason));
                    }
                }
            }
        }
        out
    }

    /// The retained, digest-checked bytes of one cited artifact revision.
    fn cited_bytes(
        &self,
        state: &State,
        classifier: &Classifier<'_>,
        policy: &AudiencePolicy,
        artifact: RevisionRef,
        selector: &EvidenceSelector,
    ) -> Result<(Vec<u8>, Option<selectors::TextDerivative>), &'static str> {
        let record = state
            .revision(artifact)
            .ok_or("the cited artifact revision is not in this snapshot")?;
        if !classifier.may_show(record, Field::Content, policy) {
            return Err("the cited bytes are restricted");
        }
        let body: ArtifactBody =
            input::decode(&record.body).map_err(|_| "the cited artifact record cannot be read")?;
        if body.availability != ArtifactAvailability::Retained {
            return Err("the cited bytes are not retained in this workspace");
        }
        if body
            .size_bytes
            .is_none_or(|size| size > MAX_SELECTOR_ARTIFACT_BYTES)
        {
            return Err("the cited artifact is too large for a native selector");
        }
        let request = SelectorRequest {
            schema_version: selector.schema_version,
            artifact_digest: selector.artifact_digest.clone(),
            selector: selector.selector.clone(),
        };
        let bytes = self
            .read_artifact(&body, &request)
            .map_err(|_| "the retained bytes are missing or do not match their digest")?;
        Ok((bytes, derivative(&body)))
    }
}

/// Whether a citation needs the cited bytes to be located, or its answer.
fn needs_bytes(span: &CitedSpan<'_>) -> Result<(), CitedLines> {
    let selector_verified = span.selector.validation == SelectorValidation::Verified;
    match span.quote.state {
        TextQuoteState::Verified if selector_verified && span.quote.exact.is_some() => Ok(()),
        TextQuoteState::Verified => Err(CitedLines::NotLocated(
            "the selector the quote was checked within is not verified",
        )),
        TextQuoteState::Unverified => Err(CitedLines::NotLocated(
            "the quote was not checked against the source bytes",
        )),
        TextQuoteState::Ambiguous => Err(CitedLines::NotLocated(
            "the quote occurs more than once in the source bytes",
        )),
        TextQuoteState::NotFound => Err(CitedLines::NotLocated(
            "the quote was not found in the source bytes",
        )),
        TextQuoteState::Unavailable => Err(CitedLines::NotLocated(
            "the core could not check the quote against the source bytes",
        )),
        TextQuoteState::NotApplicable => match span.selector.selector {
            SelectorKind::TextLines { .. } if selector_verified => Ok(()),
            SelectorKind::TextLines { .. } => {
                Err(CitedLines::NotLocated("the line selector is not verified"))
            }
            _ => Err(CitedLines::NotApplicable),
        },
    }
}

/// The digest-checked bytes of one cited artifact revision, read once for
/// every citation of it.
struct Cited<'a> {
    bytes: &'a [u8],
    /// The bytes as UTF-8, when they are.
    text: Option<&'a str>,
    /// Offsets of every line feed, ascending.
    line_feeds: Vec<usize>,
    derivative: Option<&'a selectors::TextDerivative>,
}

impl Cited<'_> {
    /// Resolve the selector as acceptance did, find the quote within the
    /// selection and turn its byte span into lines.
    ///
    /// The bytes match the digest that acceptance verified the quote against,
    /// and the selection is resolved by the same reader, so the quote occurs
    /// exactly once there: its first occurrence is the one acceptance found.
    fn locate(&self, span: CitedSpan<'_>) -> CitedLines {
        let selection = selectors::resolve(
            Some(self.bytes),
            &span.selector.selector,
            self.derivative,
            MAX_SELECTOR_ARTIFACT_BYTES as usize,
        );
        let Ok(Selection::Bytes { start, end }) = selection else {
            return CitedLines::NotLocated("the selector no longer resolves");
        };
        let Some(exact) = span.quote.exact.as_deref() else {
            // A verified `text_lines` selector: its own span.
            if let SelectorKind::TextLines {
                start_line,
                end_line,
                ..
            } = span.selector.selector
            {
                return CitedLines::Located {
                    start: start_line,
                    end: end_line,
                };
            }
            return CitedLines::NotApplicable;
        };
        let prefix = span.quote.prefix.as_deref().unwrap_or_default();
        let suffix = span.quote.suffix.as_deref().unwrap_or_default();
        let found = match self.text.and_then(|text| text.get(start..end)) {
            Some(selected) => selected
                .find(&format!("{prefix}{exact}{suffix}"))
                .map(|at| at + prefix.len()),
            // A selection that is not whole UTF-8 text: the acceptance matcher.
            None => match selectors::verify_quote(
                Some(&self.bytes[start..end]),
                exact,
                Some(prefix),
                Some(suffix),
            ) {
                Ok(QuoteResult::Verified { start, .. }) => Some(start),
                _ => None,
            },
        };
        match found {
            Some(at) if !exact.is_empty() => {
                let (first, last) =
                    line_span(&self.line_feeds, start + at, start + at + exact.len());
                CitedLines::Located {
                    start: first,
                    end: last,
                }
            }
            _ => CitedLines::NotLocated("the quote no longer occurs in the cited bytes"),
        }
    }
}

/// One-based lines of the nonempty byte span `start..end`, given the offsets
/// of every line feed: the line holding its first byte and the line holding
/// its last byte. A line ends after its line feed.
fn line_span(line_feeds: &[usize], start: usize, end: usize) -> (u64, u64) {
    let line_of = |offset: usize| 1 + line_feeds.partition_point(|at| *at < offset) as u64;
    (line_of(start), line_of(end.saturating_sub(1).max(start)))
}

#[cfg(test)]
mod tests {
    use super::line_span;

    #[test]
    fn line_span_counts_line_feeds_before_the_first_and_last_byte() {
        let feeds = |text: &[u8]| -> Vec<usize> {
            text.iter()
                .enumerate()
                .filter_map(|(at, byte)| (*byte == b'\n').then_some(at))
                .collect()
        };
        let text = feeds(b"one\ntwo\nthree\nfour");
        // "two"
        assert_eq!(line_span(&text, 4, 7), (2, 2));
        // "two\n": the line feed belongs to line 2.
        assert_eq!(line_span(&text, 4, 8), (2, 2));
        // "o\nthr"
        assert_eq!(line_span(&text, 6, 11), (2, 3));
        // "four", the last line without a line feed.
        assert_eq!(line_span(&text, 14, 18), (4, 4));
        // The first byte of the text.
        assert_eq!(line_span(&text, 0, 1), (1, 1));
        // CRLF text counts the same lines.
        assert_eq!(line_span(&feeds(b"a\r\nb\r\nc"), 3, 4), (2, 2));
    }
}
