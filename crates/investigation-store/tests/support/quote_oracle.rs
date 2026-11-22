use investigation_store::selectors::QuoteResult;

/// Enumerate every raw starting offset, including overlapping matches.
pub fn exact_matches(
    bytes: &[u8],
    exact: &str,
    prefix: Option<&str>,
    suffix: Option<&str>,
) -> QuoteResult {
    let prefix = prefix.unwrap_or_default();
    let suffix = suffix.unwrap_or_default();
    let needle = [prefix.as_bytes(), exact.as_bytes(), suffix.as_bytes()].concat();
    assert!(!exact.is_empty());
    let mut matches = bytes
        .windows(needle.len())
        .enumerate()
        .filter(|(_, window)| *window == needle.as_slice());
    let Some((offset, _)) = matches.next() else {
        return QuoteResult::NotFound;
    };
    let other = matches.count();
    if other == 0 {
        let start = offset + prefix.len();
        QuoteResult::Verified {
            start,
            end: start + exact.len(),
        }
    } else {
        QuoteResult::Ambiguous { matches: other + 1 }
    }
}
