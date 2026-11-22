#[path = "support/quote_oracle.rs"]
mod quote_oracle;

use investigation_core::evidence::SelectorKind;
use investigation_store::selectors::{
    QuoteResult, Selection, SelectorError, TextDerivative, resolve, verify_quote,
};
use proptest::{
    prelude::*,
    test_runner::{Config, RngAlgorithm, TestRng, TestRunner},
};
use quote_oracle::exact_matches;

fn runner(seed: u8) -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases: 160,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[seed; 32]),
    )
}

#[test]
fn unicode_quote_results_match_an_independent_original_byte_oracle() {
    // A small mixed alphabet produces ambiguous and overlapping matches.
    let alphabet = prop::sample::select(vec!['a', 'b', 'é', 'e', '\u{301}', '\r', '\n', '🧪']);
    let cases = (
        prop::collection::vec(alphabet, 1..96),
        any::<usize>(),
        any::<usize>(),
        any::<bool>(),
    );
    for seed in [1, 13, 87, 221] {
        runner(seed)
            .run(&cases, |(characters, offset, length, contextual)| {
                let source: String = characters.iter().collect();
                let start = offset % characters.len();
                let end = start + 1 + length % (characters.len() - start);
                let quote: String = characters[start..end].iter().collect();
                let prefix: String = characters[start.saturating_sub(2)..start].iter().collect();
                let suffix: String = characters[end..].iter().take(2).collect();
                let context = |text| contextual.then_some(text);
                let expected = exact_matches(
                    source.as_bytes(),
                    &quote,
                    context(prefix.as_str()),
                    context(suffix.as_str()),
                );
                let actual = verify_quote(
                    Some(source.as_bytes()),
                    &quote,
                    context(prefix.as_str()),
                    context(suffix.as_str()),
                )
                .unwrap();
                prop_assert_eq!(&actual, &expected);
                if let QuoteResult::Verified { start, end } = actual {
                    prop_assert_eq!(source.as_bytes().get(start..end), Some(quote.as_bytes()));
                }
                prop_assert_ne!(actual, QuoteResult::NotFound);
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn mutation_to_an_absent_byte_cannot_verify_and_overlap_remains_ambiguous() {
    let cases = (
        prop::collection::vec(b'a'..=b'd', 1..96),
        any::<usize>(),
        any::<usize>(),
    );
    for seed in [1, 13, 87, 221] {
        runner(seed)
            .run(&cases, |(source, offset, length)| {
                let start = offset % source.len();
                let end = start + 1 + length % (source.len() - start);
                let mut quote = source[start..end].to_vec();
                quote[0] = b'Z';
                let quote = String::from_utf8(quote).unwrap();
                prop_assert_eq!(
                    verify_quote(Some(&source), &quote, None, None),
                    Ok(QuoteResult::NotFound)
                );
                Ok(())
            })
            .unwrap();
    }
    for (source, quote) in [("aaaa", "aaa"), ("ééé", "éé")] {
        assert_eq!(
            verify_quote(Some(source.as_bytes()), quote, None, None),
            Ok(QuoteResult::Ambiguous { matches: 2 })
        );
    }
    assert_eq!(
        verify_quote(Some("cafe\u{301}".as_bytes()), "café", None, None),
        Ok(QuoteResult::NotFound)
    );
    assert!(matches!(
        verify_quote(Some(&[0xff, b'a']), "a", None, None),
        Err(SelectorError::Invalid(_))
    ));
}

#[test]
fn byte_selection_is_bounded_and_repeatable_on_arbitrary_bytes() {
    let cases = (
        prop::collection::vec(any::<u8>(), 0..2048),
        any::<u64>(),
        any::<u64>(),
        0usize..2049,
    );
    for seed in [1, 13, 87, 221] {
        runner(seed)
            .run(&cases, |(source, start_bound, end_bound, limit)| {
                let extent = source.len() as u64 + 1;
                for (start, end) in [
                    (start_bound, end_bound),
                    (start_bound % extent, end_bound % extent),
                ] {
                    let selector = SelectorKind::ByteRange { start, end };
                    let result = resolve(Some(&source), &selector, None, limit);
                    prop_assert_eq!(&result, &resolve(Some(&source), &selector, None, limit));
                    if start >= end || end >= extent {
                        prop_assert!(matches!(result, Err(SelectorError::Invalid(_))));
                    } else if end - start > limit as u64 {
                        prop_assert_eq!(
                            result,
                            Err(SelectorError::TooLarge {
                                size: (end - start) as usize,
                                limit
                            })
                        );
                    } else {
                        prop_assert_eq!(
                            result,
                            Ok(Selection::Bytes {
                                start: start as usize,
                                end: end as usize
                            })
                        );
                    }
                }
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn lines_preserve_multibyte_offsets_and_original_line_endings() {
    let source = "first\r\n🧪\r\ncafe\u{301}".as_bytes();
    let derivative = TextDerivative {
        encoding: "utf-8".into(),
        parser_version: "v1".into(),
    };
    let lines = |end_line| SelectorKind::TextLines {
        start_line: 2,
        end_line,
        encoding: derivative.encoding.clone(),
        parser_version: derivative.parser_version.clone(),
    };
    let Selection::Bytes { start, end } =
        resolve(Some(source), &lines(2), Some(&derivative), 64).unwrap()
    else {
        panic!("text lines must resolve to original bytes");
    };
    assert_eq!(&source[start..end], "🧪\r\n".as_bytes());
    assert!(matches!(
        resolve(Some(source), &lines(u64::MAX), Some(&derivative), 64),
        Err(SelectorError::Invalid(_))
    ));
    assert!(matches!(
        resolve(None, &lines(2), Some(&derivative), 64),
        Err(SelectorError::Unavailable(_))
    ));
}
