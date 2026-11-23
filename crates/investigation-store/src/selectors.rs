//! Bounded native selection over already-retained bytes. No path is reopened.
use investigation_core::{
    evidence::{SelectorKind, TABLE_CSV_READER, TABLE_TSV_READER},
    input,
};

use crate::delimited::{self, CellQuery, Format};
use serde_json::Value;

/// Largest retained artifact the strict `json_pointer` reader parses.
pub const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
/// Deepest nesting, in JSON Pointer segments, the `json_pointer` reader accepts.
pub const MAX_JSON_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextDerivative {
    pub encoding: String,
    pub parser_version: String,
}
#[derive(Debug, Clone, PartialEq)]
pub enum Selection {
    Bytes { start: usize, end: usize },
    Json(Value),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorError {
    Unavailable(&'static str),
    Invalid(String),
    /// More than one target matches. `what` names it and `repair` says how
    /// to cite one of them.
    Ambiguous {
        matches: usize,
        what: &'static str,
        repair: &'static str,
    },
    TooLarge {
        size: usize,
        limit: usize,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuoteResult {
    Verified { start: usize, end: usize },
    Ambiguous { matches: usize },
    NotFound,
}

/// Validates and resolves a selector. Text attestation must be checked against
/// artifact metadata by the caller. `Json` is a semantic value, not a claim
/// that its serialization is an exact excerpt from the original bytes.
pub fn resolve(
    bytes: Option<&[u8]>,
    selector: &SelectorKind,
    text: Option<&TextDerivative>,
    max_output: usize,
) -> Result<Selection, SelectorError> {
    let bytes = bytes.ok_or(SelectorError::Unavailable(
        "artifact bytes are not retained",
    ))?;
    let range = match selector {
        SelectorKind::Whole => Some((0, bytes.len())),
        SelectorKind::ByteRange { start, end } => {
            let a = usize::try_from(*start)
                .map_err(|_| invalid("byte offset exceeds platform range"))?;
            let b =
                usize::try_from(*end).map_err(|_| invalid("byte offset exceeds platform range"))?;
            if a >= b || b > bytes.len() {
                return Err(invalid(&format!(
                    "byte range must be within 0..{} and nonempty",
                    bytes.len()
                )));
            }
            Some((a, b))
        }
        SelectorKind::TextLines {
            start_line,
            end_line,
            encoding,
            parser_version,
        } => {
            let meta = text.ok_or(SelectorError::Unavailable("text derivative is unavailable"))?;
            if !meta.encoding.eq_ignore_ascii_case("utf-8")
                || meta.encoding != *encoding
                || meta.parser_version != *parser_version
            {
                return Err(SelectorError::Unavailable(
                    "text derivative metadata does not match selector",
                ));
            }
            let s = std::str::from_utf8(bytes)
                .map_err(|_| invalid("text derivative is not valid UTF-8"))?;
            Some(line_span(s, *start_line, *end_line)?)
        }
        SelectorKind::CodeSpan {
            blob_digest,
            start_line,
            end_line,
            ..
        } => {
            // Only the exact blob can be read: repository, commit and path are
            // recorded claims the core does not check against any tool.
            if crate::journal::hash_bytes(bytes) != *blob_digest {
                return Err(SelectorError::Unavailable(
                    "The cited artifact is not the blob named by blob_digest, so the code span cannot be checked (external_unverified). Import the blob's exact bytes and cite that artifact, or attach an attributed validation with selector.attest.",
                ));
            }
            let s =
                std::str::from_utf8(bytes).map_err(|_| invalid("code blob is not valid UTF-8"))?;
            Some(line_span(s, *start_line, *end_line)?)
        }
        SelectorKind::TableCell {
            row_identity,
            row_index,
            column,
            extraction_version,
            key_column,
            ..
        } => {
            let format = match extraction_version.as_str() {
                TABLE_CSV_READER => Format::Csv,
                TABLE_TSV_READER => Format::Tsv,
                _ => {
                    return Err(SelectorError::Unavailable(
                        "extraction_version names no core table reader, so the cell cannot be checked. The core reads inv.table.csv.v1 and inv.table.tsv.v1 tables; another extraction needs an attributed validation with selector.attest.",
                    ));
                }
            };
            let key = match (row_identity, key_column) {
                (Some(identity), Some(key)) => Some((key.as_str(), identity.as_str())),
                (Some(_), None) => {
                    return Err(SelectorError::Unavailable(
                        "row_identity cannot be checked without key_column, the header of the column that holds it. Add key_column, or cite the row by row_index.",
                    ));
                }
                (None, _) => None,
            };
            Some(delimited::locate(
                bytes,
                format,
                CellQuery {
                    column,
                    row_index: *row_index,
                    key,
                },
            )?)
        }
        SelectorKind::TimeWindow { .. } => {
            return Err(SelectorError::Unavailable(
                "A time_window's meaning is determined by source metadata; the core checks its shape only and cannot resolve it against bytes. Cite a derivative a trusted reader extracted for the window, or attach an attributed validation with selector.attest.",
            ));
        }
        SelectorKind::ExternalSelector { .. } => {
            return Err(SelectorError::Unavailable(
                "An external_selector is opaque to the core, which loads and runs no validator. Its validation is unavailable until an attributed validation is attached with selector.attest.",
            ));
        }
        SelectorKind::TextQuote {
            exact,
            prefix,
            suffix,
        } => {
            require_text(bytes, text)?;
            match verify_quote(Some(bytes), exact, prefix.as_deref(), suffix.as_deref())? {
                QuoteResult::Verified { start, end } => Some((start, end)),
                QuoteResult::Ambiguous { matches } => {
                    return Err(SelectorError::Ambiguous {
                        matches,
                        what: "Quote",
                        repair: "Add prefix or suffix context.",
                    });
                }
                QuoteResult::NotFound => return Err(invalid("quote was not found")),
            }
        }
        SelectorKind::JsonPointer { pointer, parser } => {
            if parser
                .as_deref()
                .is_some_and(|p| p != "serde_json-strict-v1")
            {
                return Err(SelectorError::Unavailable(
                    "requested JSON parser is unavailable",
                ));
            }
            let doc = input::parse_json(bytes, MAX_JSON_BYTES, MAX_JSON_DEPTH)
                .map_err(|_| invalid("invalid or over-limit strict JSON"))?;
            let value = pointer_value(&doc, pointer)?;
            let size = serde_json::to_vec(value)
                .map_err(|_| invalid("cannot encode JSON value"))?
                .len();
            if size > max_output {
                return Err(SelectorError::TooLarge {
                    size,
                    limit: max_output,
                });
            }
            return Ok(Selection::Json(value.clone()));
        }
    };
    let (start, end) = range.expect("native range");
    let size = end - start;
    if size > max_output {
        return Err(SelectorError::TooLarge {
            size,
            limit: max_output,
        });
    }
    Ok(Selection::Bytes { start, end })
}
fn invalid(s: &str) -> SelectorError {
    SelectorError::Invalid(s.to_owned())
}
/// Byte span of one-based inclusive lines `a..=b`, line ends included.
fn line_span(s: &str, a: u64, b: u64) -> Result<(usize, usize), SelectorError> {
    let a = usize::try_from(a).map_err(|_| invalid("line index too large"))?;
    let b = usize::try_from(b).map_err(|_| invalid("line index too large"))?;
    let mut offset = 0;
    let mut total = 0;
    let mut start = None;
    let mut end = None;
    for line in s.split_inclusive('\n') {
        total += 1;
        if total == a {
            start = Some(offset);
        }
        offset += line.len();
        if total == b {
            end = Some(offset);
        }
    }
    if a == 0 || a > b || b > total {
        return Err(invalid(&format!(
            "line range must be 1..={total} and inclusive"
        )));
    }
    Ok((
        start.expect("valid line start"),
        end.expect("valid line end"),
    ))
}
fn require_text(bytes: &[u8], meta: Option<&TextDerivative>) -> Result<(), SelectorError> {
    let meta = meta.ok_or(SelectorError::Unavailable("text derivative is unavailable"))?;
    if !meta.encoding.eq_ignore_ascii_case("utf-8") {
        return Err(SelectorError::Unavailable(
            "UTF-8 derivative is unavailable",
        ));
    }
    std::str::from_utf8(bytes).map_err(|_| invalid("text derivative is not valid UTF-8"))?;
    Ok(())
}
fn pointer_value<'a>(doc: &'a Value, pointer: &str) -> Result<&'a Value, SelectorError> {
    if pointer.is_empty() {
        return Ok(doc);
    }
    if !pointer.starts_with('/') {
        return Err(invalid("JSON pointer must be empty or begin with '/'"));
    }
    let mut current = doc;
    for raw in pointer[1..].split('/') {
        let mut token = String::new();
        let mut chars = raw.chars();
        while let Some(ch) = chars.next() {
            if ch == '~' {
                match chars.next() {
                    Some('0') => token.push('~'),
                    Some('1') => token.push('/'),
                    _ => return Err(invalid("invalid JSON pointer escape")),
                }
            } else {
                token.push(ch)
            }
        }
        current = match current {
            Value::Object(map) => map.get(&token),
            Value::Array(arr) => {
                if token.is_empty()
                    || (token.len() > 1 && token.starts_with('0'))
                    || !token.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(invalid("invalid JSON array index"));
                }
                token.parse::<usize>().ok().and_then(|n| arr.get(n))
            }
            _ => None,
        }
        .ok_or_else(|| invalid("JSON pointer does not resolve"))?;
    }
    Ok(current)
}
/// Exact byte matching, including overlapping matches. Prefix and suffix must
/// immediately adjoin the quote. A verified match proves presence only.
pub fn verify_quote(
    bytes: Option<&[u8]>,
    exact: &str,
    prefix: Option<&str>,
    suffix: Option<&str>,
) -> Result<QuoteResult, SelectorError> {
    let bytes = bytes.ok_or(SelectorError::Unavailable("text derivative is unavailable"))?;
    std::str::from_utf8(bytes).map_err(|_| invalid("text derivative is not valid UTF-8"))?;
    if exact.is_empty() {
        return Err(invalid("quote must not be empty"));
    }
    let prefix = prefix.unwrap_or_default();
    let suffix = suffix.unwrap_or_default();
    let needle: Vec<_> = prefix
        .bytes()
        .chain(exact.bytes())
        .chain(suffix.bytes())
        .collect();
    if needle.len() > bytes.len() {
        return Ok(QuoteResult::NotFound);
    }

    // Reuse the longest matching prefix when a byte differs. A single scan
    // finds overlaps even for repetitive captures or long quoted passages.
    let mut fallback = vec![0; needle.len()];
    let mut matched = 0;
    for i in 1..needle.len() {
        while matched != 0 && needle[matched] != needle[i] {
            matched = fallback[matched - 1];
        }
        if needle[matched] == needle[i] {
            matched += 1;
        }
        fallback[i] = matched;
    }
    let mut found = None;
    let mut count = 0;
    let mut matched = 0;
    for (i, byte) in bytes.iter().enumerate() {
        while matched != 0 && needle[matched] != *byte {
            matched = fallback[matched - 1];
        }
        if needle[matched] == *byte {
            matched += 1;
        }
        if matched == needle.len() {
            let start = i + 1 - needle.len() + prefix.len();
            found = Some((start, start + exact.len()));
            count += 1;
            matched = fallback[matched - 1];
        }
    }
    Ok(match (count, found) {
        (0, _) => QuoteResult::NotFound,
        (1, Some((start, end))) => QuoteResult::Verified { start, end },
        _ => QuoteResult::Ambiguous { matches: count },
    })
}
/// Optional positional cross-check; never silently treat disagreement as support.
pub fn quote_within(quote: &QuoteResult, selection: &Selection) -> Result<bool, SelectorError> {
    match (quote, selection) {
        (QuoteResult::Verified { start, end }, Selection::Bytes { start: a, end: b }) => {
            Ok(start >= a && end <= b)
        }
        (_, Selection::Json(_)) => Err(SelectorError::Unavailable(
            "JSON value has no exact source byte span",
        )),
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn range_lines_and_unavailable() {
        let b = b"one\ntwo\nthree";
        let t = TextDerivative {
            encoding: "utf-8".into(),
            parser_version: "v1".into(),
        };
        assert_eq!(
            resolve(
                Some(b),
                &SelectorKind::ByteRange { start: 4, end: 7 },
                None,
                10
            ),
            Ok(Selection::Bytes { start: 4, end: 7 })
        );
        assert!(matches!(
            resolve(
                Some(b),
                &SelectorKind::ByteRange { start: 4, end: 99 },
                None,
                100
            ),
            Err(SelectorError::Invalid(_))
        ));
        assert_eq!(
            resolve(
                Some(b),
                &SelectorKind::TextLines {
                    start_line: 2,
                    end_line: 3,
                    encoding: "utf-8".into(),
                    parser_version: "v1".into()
                },
                Some(&t),
                100
            ),
            Ok(Selection::Bytes { start: 4, end: 13 })
        );
        assert!(matches!(
            resolve(None, &SelectorKind::Whole, None, 100),
            Err(SelectorError::Unavailable(_))
        ));
    }
    #[test]
    fn pointer_rejects_bad_escapes_duplicate_keys_and_indexes() {
        let b = br#"{"a/b":[7,8],"~":4}"#;
        let s = |p: &str| SelectorKind::JsonPointer {
            pointer: p.into(),
            parser: None,
        };
        assert_eq!(
            resolve(Some(b), &s("/a~1b/0"), None, 100),
            Ok(Selection::Json(7.into()))
        );
        for p in ["/a~2b", "/a~1b/01", "/missing"] {
            assert!(matches!(
                resolve(Some(b), &s(p), None, 100),
                Err(SelectorError::Invalid(_))
            ));
        }
        assert!(resolve(Some(br#"{"a":1,"a":2}"#), &s("/a"), None, 100).is_err());
    }
    #[test]
    fn remaining_selectors_resolve_natively_or_are_unavailable_never_verified() {
        let code = b"a\nb\nc\n";
        let digest = crate::journal::hash_bytes(code);
        let span = |blob_digest| SelectorKind::CodeSpan {
            repository: "r".into(),
            commit: "c".into(),
            blob_digest,
            path: "p".into(),
            start_line: 2,
            end_line: 3,
        };
        assert_eq!(
            resolve(Some(code), &span(digest), None, 100),
            Ok(Selection::Bytes { start: 2, end: 6 })
        );
        let other = crate::journal::hash_bytes(b"other");
        assert!(matches!(
            resolve(Some(code), &span(other), None, 100),
            Err(SelectorError::Unavailable(_))
        ));
        let cell = |extraction_version: &str, key_column: Option<&str>| SelectorKind::TableCell {
            table: "t".into(),
            row_identity: Some("x".into()),
            row_index: None,
            column: "v".into(),
            extraction_version: extraction_version.into(),
            key_column: key_column.map(Into::into),
        };
        let table = b"k\tv\nx\t7\n";
        assert_eq!(
            resolve(Some(table), &cell(TABLE_TSV_READER, Some("k")), None, 100),
            Ok(Selection::Bytes { start: 6, end: 7 })
        );
        for unavailable in [
            cell("producer-export-1", Some("k")),
            cell(TABLE_TSV_READER, None),
            SelectorKind::TimeWindow {
                start: serde_json::from_value(serde_json::json!("0")).unwrap(),
                end: serde_json::from_value(serde_json::json!("1")).unwrap(),
                unit: "s".into(),
                coordinate_origin: "start".into(),
                channel: "c".into(),
            },
            SelectorKind::ExternalSelector {
                namespace: "n.x".into(),
                schema_uri: "urn:x".into(),
                schema_version: "1".into(),
                value: serde_json::json!({"frame": 1}),
                validator: Some("reader".into()),
            },
        ] {
            assert!(
                matches!(
                    resolve(Some(table), &unavailable, None, 100),
                    Err(SelectorError::Unavailable(_))
                ),
                "{unavailable:?}"
            );
        }
    }
    #[test]
    fn exact_quote_detects_overlaps_and_context() {
        assert_eq!(
            verify_quote(Some(b"ababa"), "aba", None, None).unwrap(),
            QuoteResult::Ambiguous { matches: 2 }
        );
        assert_eq!(
            verify_quote(Some(b"ababa"), "aba", Some("ab"), None).unwrap(),
            QuoteResult::Verified { start: 2, end: 5 }
        );
        assert_eq!(
            verify_quote(Some(b"a  b"), "a b", None, None).unwrap(),
            QuoteResult::NotFound
        );
    }
}
