//! Text that a disposable full-text index may hold for navigation, and the
//! label each piece carries.
//!
//! A search match is a pointer, never evidence. Each indexed field carries a
//! [`TextLabel`] so a reader can tell an editorial summary or an authored claim
//! from text recorded as evidence. Only [`TextLabel::RecordedEvidence`] counts
//! as a source hit, and even that match only shows where to look: verifying a
//! claim still needs `show` or an evidence read with a selector.
//!
//! The same extractor feeds the index and re-checks every candidate against
//! the canonical record, so the index can narrow a search but never decide
//! what a result says. Raw artifact bytes are not indexed.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    classification::CHANGE_ROLE,
    identity::{Id, Kind},
    state::Record,
    wire::{ErrorCode, Failure},
};

/// Version of the extraction and labelling rules below.
pub const SEARCH_TEXT_VERSION: &str = "inv.search-text.v1";
/// Longest accepted query, in UTF-8 bytes.
pub const MAX_QUERY_BYTES: usize = 512;
/// Most distinct terms in one query. Every term must match.
pub const MAX_QUERY_TERMS: usize = 16;
/// Longest bounded excerpt around a match, in UTF-8 bytes before markers.
pub const MAX_EXCERPT_BYTES: usize = 240;

const MAX_FIELD_BYTES: usize = 16 * 1024;
const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_FIELDS: usize = 256;

/// What a piece of indexed text is, so a match in it is not mistaken for
/// corroboration.
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TextLabel {
    /// Summary material about other records: an authored synopsis annotation
    /// or a generated projection. Never original evidence.
    EditorialSummary,
    /// An attributed claim, wording, rationale or note written by an actor.
    AuthoredText,
    /// Text recorded as evidence or its provenance: observed values, verified
    /// quotes, and producer-supplied source, artifact and run descriptions.
    /// It is untrusted source content.
    RecordedEvidence,
}

impl TextLabel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EditorialSummary => "editorial_summary",
            Self::AuthoredText => "authored_text",
            Self::RecordedEvidence => "recorded_evidence",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "editorial_summary" => Some(Self::EditorialSummary),
            "authored_text" => Some(Self::AuthoredText),
            "recorded_evidence" => Some(Self::RecordedEvidence),
            _ => None,
        }
    }

    /// Whether a match in this text counts as a hit in recorded source
    /// material. Editorial and authored text never does.
    pub const fn counts_as_source_hit(self) -> bool {
        matches!(self, Self::RecordedEvidence)
    }

    /// Trust marking for an excerpt. Both kinds are data, never instructions.
    pub const fn content_trust(self) -> &'static str {
        match self {
            Self::RecordedEvidence => crate::authored_text::UNTRUSTED_EVIDENCE,
            Self::EditorialSummary | Self::AuthoredText => crate::authored_text::AUTHORED_DATA,
        }
    }
}

/// One bounded text field of one record revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexedText {
    /// Dotted body path, with array positions, such as `value.statement`.
    pub field: String,
    pub label: TextLabel,
    pub text: String,
}

/// Bounded, labelled free text of one record. Identifiers, digests, enum
/// values, selectors, namespaced producer metadata and extensions are left
/// out. Classification change records are handling metadata and yield none.
pub fn indexed_texts(record: &Record) -> Vec<IndexedText> {
    body_texts(record.header.id.kind(), &record.body)
}

/// [`indexed_texts`] for a record body of a known kind.
pub fn body_texts(kind: Kind, body: &Value) -> Vec<IndexedText> {
    let role = body.get("role").and_then(Value::as_str);
    if kind == Kind::Annotation && role == Some(CHANGE_ROLE) {
        return Vec::new();
    }
    let mut fields = Vec::new();
    let mut budget = MAX_RECORD_BYTES;
    walk(body, &mut String::new(), &mut fields, &mut budget);
    fields
        .into_iter()
        .map(|(field, text)| IndexedText {
            label: label(kind, role, &field),
            field,
            text,
        })
        .collect()
}

fn label(kind: Kind, role: Option<&str>, field: &str) -> TextLabel {
    let top = field.split('.').next().unwrap_or_default();
    match kind {
        Kind::Annotation if role == Some("synopsis") => TextLabel::EditorialSummary,
        Kind::Observation if matches!(top, "value" | "basis") => TextLabel::RecordedEvidence,
        Kind::Source | Kind::Artifact | Kind::Run => TextLabel::RecordedEvidence,
        _ => TextLabel::AuthoredText,
    }
}

fn skipped_key(key: &str) -> bool {
    matches!(
        key,
        "metadata"
            | "extensions"
            | "classification_change"
            | "acquisition_key"
            | "selector"
            | "schema_version"
            | "id"
    ) || ["_ref", "_refs", "_id", "_ids", "digest"]
        .iter()
        .any(|suffix| key.ends_with(suffix))
}

/// Enum values, identifiers and digests carry no searchable prose.
fn structural_value(value: &str) -> bool {
    let token = !value.is_empty()
        && value.split('_').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        });
    token || value.starts_with("sha256:") || value.parse::<Id>().is_ok()
}

fn walk(value: &Value, path: &mut String, out: &mut Vec<(String, String)>, budget: &mut usize) {
    if out.len() >= MAX_FIELDS || *budget == 0 {
        return;
    }
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() || structural_value(trimmed) {
                return;
            }
            let bounded = truncate(trimmed, MAX_FIELD_BYTES.min(*budget));
            *budget -= bounded.len();
            out.push((path.clone(), bounded.to_owned()));
        }
        Value::Object(map) => {
            for (key, child) in map {
                if skipped_key(key) {
                    continue;
                }
                descend(path, key, child, out, budget);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                descend(path, &index.to_string(), child, out, budget);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn descend(
    path: &mut String,
    segment: &str,
    child: &Value,
    out: &mut Vec<(String, String)>,
    budget: &mut usize,
) {
    let length = path.len();
    if !path.is_empty() {
        path.push('.');
    }
    path.push_str(segment);
    walk(child, path, out, budget);
    path.truncate(length);
}

fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Lower-cased runs of alphanumeric characters with their byte offsets. The
/// index tokenizer splits text the same way, without folding diacritics.
fn tokens(text: &str) -> impl Iterator<Item = (usize, usize, String)> + '_ {
    let mut chars = text.char_indices().peekable();
    std::iter::from_fn(move || {
        while chars.peek().is_some_and(|(_, c)| !c.is_alphanumeric()) {
            chars.next();
        }
        let (start, _) = *chars.peek()?;
        let mut end = start;
        while let Some(&(offset, c)) = chars.peek() {
            if !c.is_alphanumeric() {
                break;
            }
            end = offset + c.len_utf8();
            chars.next();
        }
        Some((start, end, text[start..end].to_lowercase()))
    })
}

/// Parse a free-text query into distinct terms, all of which must match one
/// field. Query syntax is never passed through to the index.
pub fn query_terms(query: &str) -> Result<Vec<String>, Failure> {
    if query.len() > MAX_QUERY_BYTES {
        let mut failure = Failure::new(
            ErrorCode::InvalidInput,
            format!("Use a query of at most {MAX_QUERY_BYTES} bytes."),
        )
        .at("/query");
        failure.error.actual = Some(serde_json::json!({ "bytes": query.len() }));
        return Err(failure);
    }
    let mut seen = BTreeSet::new();
    let terms: Vec<_> = tokens(query)
        .map(|(_, _, term)| term)
        .filter(|term| seen.insert(term.clone()))
        .collect();
    if terms.is_empty() {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "Supply at least one word or number to search for.",
        )
        .at("/query"));
    }
    if terms.len() > MAX_QUERY_TERMS {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            format!("Use at most {MAX_QUERY_TERMS} distinct search terms."),
        )
        .at("/query"));
    }
    Ok(terms)
}

/// Whether every term appears as a whole token of `text`.
pub fn contains_all(text: &str, terms: &[String]) -> bool {
    let present: BTreeSet<_> = tokens(text).map(|(_, _, token)| token).collect();
    terms.iter().all(|term| present.contains(term))
}

/// A bounded window of `text` around the first matching term, with control
/// characters replaced by spaces. Cut ends are marked with `…`.
pub fn excerpt(text: &str, terms: &[String]) -> String {
    let (first, first_end) = tokens(text)
        .find(|(_, _, token)| terms.contains(token))
        .map_or((0, 0), |(start, end, _)| (start, end));
    let (mut start, mut end) = (0, text.len());
    if text.len() > MAX_EXCERPT_BYTES {
        start = first.saturating_sub(MAX_EXCERPT_BYTES / 4);
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        end = (start + MAX_EXCERPT_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        // Prefer whole words at a cut, without losing the matched term.
        if start > 0
            && let Some(space) = text[start..first].find(char::is_whitespace)
        {
            start += space;
        }
        if end < text.len()
            && let Some(space) = text[first_end.min(end)..end].rfind(char::is_whitespace)
        {
            end = first_end.min(end) + space;
        }
    }
    let body: String = text[start..end]
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        body.trim(),
        if end < text.len() { "…" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn labels_separate_editorial_authored_and_recorded_text() {
        let synopsis = json!({"role":"synopsis","content":{"kind":"text","text":"Pool exhaustion explains it."},
            "subject_refs":[{"id":"hyp_018d87fc-5234-7000-8000-000000000003","revision":1}]});
        let texts = body_texts(Kind::Annotation, &synopsis);
        assert_eq!(
            texts,
            vec![IndexedText {
                field: "content.text".into(),
                label: TextLabel::EditorialSummary,
                text: "Pool exhaustion explains it.".into()
            }]
        );
        assert!(!texts[0].label.counts_as_source_hit());
        let mut note = synopsis.clone();
        note["role"] = json!("note");
        assert_eq!(
            body_texts(Kind::Annotation, &note)[0].label,
            TextLabel::AuthoredText
        );

        let observation = json!({"value":{"kind":"text","statement":"Held for 28 seconds."},
            "basis":{"kind":"source","artifact_ref":{"id":"art_018d87fc-5234-7000-8000-000000000005","revision":1},
                "text_quote":{"exact":"waited 28 seconds"}},
            "limitations":["Single trace."],"validity":"valid",
            "metadata":{"producer.example":{"note":"Never indexed."}}});
        let mut labels: Vec<_> = body_texts(Kind::Observation, &observation)
            .into_iter()
            .map(|text| (text.field, text.label, text.text))
            .collect();
        labels.sort();
        assert_eq!(
            labels,
            vec![
                (
                    "basis.text_quote.exact".into(),
                    TextLabel::RecordedEvidence,
                    "waited 28 seconds".into()
                ),
                (
                    "limitations.0".into(),
                    TextLabel::AuthoredText,
                    "Single trace.".into()
                ),
                (
                    "value.statement".into(),
                    TextLabel::RecordedEvidence,
                    "Held for 28 seconds.".into()
                ),
            ]
        );
        let change = json!({"role":"classification","classification_change":{"reason":"Codename"}});
        assert!(body_texts(Kind::Annotation, &change).is_empty());
    }

    #[test]
    fn queries_are_terms_not_index_syntax() {
        assert_eq!(
            query_terms("Pool* OR \"exhaustion\" NEAR(pool)").unwrap(),
            vec!["pool", "or", "exhaustion", "near"]
        );
        assert!(query_terms(" -- * ").is_err());
        assert!(query_terms(&"x ".repeat(300)).is_err());
        let many: String = (0..17).map(|i| format!("w{i} ")).collect();
        assert!(query_terms(&many).is_err());
        assert!(contains_all(
            "The pool was exhausted.",
            &["pool".into(), "the".into()]
        ));
        assert!(!contains_all("Pooled", &["pool".into()]));
    }

    #[test]
    fn excerpts_are_bounded_and_single_line() {
        let text = format!("{}needle\nnext line{}", "a ".repeat(200), " b".repeat(200));
        let excerpt = excerpt(&text, &["needle".into()]);
        assert!(excerpt.starts_with('…') && excerpt.ends_with('…'));
        assert!(excerpt.contains("needle next line"));
        assert!(
            excerpt.starts_with("…a ") && excerpt.ends_with(" b…"),
            "{excerpt}"
        );
        let words = format!("{} needle", "word ".repeat(100));
        assert!(super::excerpt(&words, &["needle".into()]).starts_with("…word "));
        assert!(excerpt.len() <= MAX_EXCERPT_BYTES + 2 * '…'.len_utf8());
        assert_eq!(
            super::excerpt("café au lait", &["lait".into()]),
            "café au lait"
        );
    }
}
