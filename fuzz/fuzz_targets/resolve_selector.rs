#![no_main]

#[path = "../../crates/investigation-store/tests/support/quote_oracle.rs"]
mod quote_oracle;

use investigation_core::{
    evidence::{SelectorKind, SelectorRequest},
    input,
};
use investigation_store::selectors::{
    QuoteResult, Selection, SelectorError, TextDerivative, resolve, verify_quote,
};
use libfuzzer_sys::fuzz_target;
use quote_oracle::exact_matches;

fuzz_target!(|data: &[u8]| {
    let Some((&flags, data)) = data.split_first() else {
        return;
    };
    let (selector, artifact) = match data.iter().position(|&byte| byte == 0) {
        Some(split) => {
            let selector = input::parse_request(&data[..split])
                .ok()
                .and_then(|value| input::decode::<SelectorRequest>(&value).ok())
                .map(|request| request.selector);
            (selector, &data[split + 1..])
        }
        None => (None, data),
    };
    let artifact = &artifact[..artifact.len().min(8192)];
    let selector = selector.unwrap_or_else(|| native_range(artifact, flags));
    let retained = (flags & 1 == 0).then_some(artifact);
    let text = (flags & 2 == 0).then(|| TextDerivative {
        encoding: "utf-8".into(),
        parser_version: "v1".into(),
    });
    let limit = usize::from(flags & !3) * 8;
    let result = resolve(retained, &selector, text.as_ref(), limit);
    assert_eq!(result, resolve(retained, &selector, text.as_ref(), limit));
    if retained.is_none() {
        assert!(matches!(result, Err(SelectorError::Unavailable(_))));
        return;
    }
    match &result {
        Ok(Selection::Bytes { start, end }) => {
            let selected = artifact
                .get(*start..*end)
                .expect("selection in the original bytes");
            assert!(selected.len() <= limit);
            if let SelectorKind::ByteRange {
                start: request_start,
                end: request_end,
            } = &selector
            {
                assert_eq!((*start as u64, *end as u64), (*request_start, *request_end));
                assert!(!selected.is_empty());
            }
        }
        Ok(Selection::Json(value)) => {
            assert!(serde_json::to_vec(value).unwrap().len() <= limit);
            assert!(matches!(selector, SelectorKind::JsonPointer { .. }));
        }
        Err(SelectorError::TooLarge { size, limit: bound }) => {
            assert_eq!(*bound, limit);
            assert!(*size > limit);
        }
        Err(SelectorError::Ambiguous { matches }) => assert!(*matches > 1),
        Err(SelectorError::Invalid(_) | SelectorError::Unavailable(_)) => {}
    }
    if let SelectorKind::TextQuote {
        exact,
        prefix,
        suffix,
    } = &selector
    {
        let direct = verify_quote(retained, exact, prefix.as_deref(), suffix.as_deref());
        if exact.is_empty() || std::str::from_utf8(artifact).is_err() {
            assert!(matches!(direct, Err(SelectorError::Invalid(_))));
        } else if exact.len()
            + prefix.as_deref().unwrap_or_default().len()
            + suffix.as_deref().unwrap_or_default().len()
            <= 2048
        {
            let expected = exact_matches(artifact, exact, prefix.as_deref(), suffix.as_deref());
            assert_eq!(direct, Ok(expected.clone()));
            if let Ok(Selection::Bytes { start, end }) = result {
                assert_eq!(expected, QuoteResult::Verified { start, end });
            }
        }
    }
});

fn native_range(artifact: &[u8], flags: u8) -> SelectorKind {
    let offset = |position| u64::from(artifact.get(position).copied().unwrap_or(0));
    match flags % 3 {
        0 => SelectorKind::ByteRange {
            start: offset(0),
            end: offset(1),
        },
        1 => SelectorKind::TextLines {
            start_line: offset(0),
            end_line: offset(1),
            encoding: "utf-8".into(),
            parser_version: "v1".into(),
        },
        _ => SelectorKind::Whole,
    }
}
