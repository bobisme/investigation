//! Bounded native selection over already-retained bytes. No path is reopened.
use investigation_core::{evidence::SelectorKind, input};
use serde_json::Value;

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
    Ambiguous { matches: usize },
    TooLarge { size: usize, limit: usize },
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
            let a = usize::try_from(*start_line).map_err(|_| invalid("line index too large"))?;
            let b = usize::try_from(*end_line).map_err(|_| invalid("line index too large"))?;
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
            Some((
                start.expect("valid line start"),
                end.expect("valid line end"),
            ))
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
                    return Err(SelectorError::Ambiguous { matches });
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
            let doc = input::parse_json(bytes, 16 * 1024 * 1024, 64)
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
        _ => {
            return Err(SelectorError::Unavailable(
                "selector parser is not implemented",
            ));
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
