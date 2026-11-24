//! Point a refused quote at the nearest source passage (W17.07).
//!
//! In the first-use trials a quote the source does not hold was the largest
//! refusal cause, and the refusal carried no repair. The refusal now names
//! the source line nearest the quote and repeats the call with that line as
//! `--quote`. The hint only reads: what is accepted does not change, and a
//! retry is verified against the source bytes like any other quote.

use std::{env, ffi::OsString};

use investigation_core::{
    PROGRAM_NAME,
    command::RevisionRef,
    evidence::{SelectorKind, SelectorRequest},
    wire::Failure,
};
use investigation_store::{Store, store::evidence_read::Excerpt};
use serde_json::{Value, json};

/// Longer quotes get the generic advice only; the search is O(quote × source).
const MAX_QUOTE_CHARS: usize = 1_000;
/// A line is near when at most this fraction of the shorter side must change.
const NEAR_NUMERATOR: usize = 1;
const NEAR_DENOMINATOR: usize = 3;
/// A suggested line longer than this is shown but not offered as a retry.
const MAX_SUGGESTED_CHARS: usize = 2_048;

/// One source line near a refused quote.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NearLine {
    /// 1-based line number in the source text.
    pub(crate) number: usize,
    /// The line without leading and trailing whitespace: bytes the source holds.
    pub(crate) text: String,
    /// How the line was found.
    pub(crate) found: Found,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Found {
    /// The quote's first `matched` characters occur from this line on; then
    /// the source continues with `next` (`None` at the end of the source).
    Prefix { matched: usize, next: Option<char> },
    /// No long prefix occurs; this line is the closest by edit distance.
    Closest,
}

/// The source line nearest `quote`, when one is near.
///
/// First the longest start of the quote the source holds: when it covers half
/// the quote, or runs to a line break (two lines joined, a typed `\n`), the
/// line it starts on is named. Otherwise the closest line by a bounded edit
/// distance: the quote inside the line, or the line inside the quote, with at
/// most a third of it changed. Ties go to the longer line, then the earlier.
pub(crate) fn nearest_line(source: &str, quote: &str) -> Option<NearLine> {
    let chars = quote.chars().count();
    if chars == 0 || chars > MAX_QUOTE_CHARS {
        return None;
    }
    longest_prefix(source, quote, chars).or_else(|| closest_line(source, quote))
}

fn longest_prefix(source: &str, quote: &str, chars: usize) -> Option<NearLine> {
    // Byte ends of each nonempty prefix; prefix k occurs whenever k + 1 does.
    let ends: Vec<usize> = quote
        .char_indices()
        .map(|(index, character)| index + character.len_utf8())
        .collect();
    let (mut low, mut high) = (0, ends.len());
    while low < high {
        let middle = (low + high).div_ceil(2);
        if source.contains(&quote[..ends[middle - 1]]) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let matched = low;
    if matched == 0 {
        return None;
    }
    let prefix = &quote[..ends[matched - 1]];
    let start = source.find(prefix)?;
    let next = source[start + prefix.len()..].chars().next();
    let substantial = prefix.trim().chars().count() >= 8;
    if !(matched * 2 >= chars || (substantial && matches!(next, Some('\n' | '\r')))) {
        return None;
    }
    let number = source[..start].matches('\n').count() + 1;
    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
    let line_end = source[start..]
        .find('\n')
        .map_or(source.len(), |index| start + index);
    let text = source[line_start..line_end].trim();
    (!text.is_empty()).then(|| NearLine {
        number,
        text: text.to_owned(),
        found: Found::Prefix { matched, next },
    })
}

fn closest_line(source: &str, quote: &str) -> Option<NearLine> {
    let quote: Vec<char> = quote.chars().collect();
    // (edits, length compared, line length, number, text)
    let mut best: Option<(usize, usize, usize, usize, &str)> = None;
    for (index, raw) in source.split('\n').enumerate() {
        let text = raw.trim();
        if text.is_empty() {
            continue;
        }
        let line: Vec<char> = text.chars().collect();
        let mut candidates = vec![(substring_distance(&quote, &line), quote.len())];
        // A short line inside a long quote is weak evidence: "2022" is inside
        // most quotes about that year. It must cover a third of the quote.
        if line.len() * 3 >= quote.len() {
            candidates.push((substring_distance(&line, &quote), line.len()));
        }
        for (edits, length) in candidates {
            if edits * NEAR_DENOMINATOR > length * NEAR_NUMERATOR {
                continue;
            }
            let better = match best {
                None => true,
                // Compare edits / length without division.
                Some((best_edits, best_length, best_line, ..)) => {
                    let (left, right) = (edits * best_length, best_edits * length);
                    left < right || (left == right && line.len() > best_line)
                }
            };
            if better {
                best = Some((edits, length, line.len(), index + 1, text));
            }
        }
    }
    best.map(|(.., number, text)| NearLine {
        number,
        text: text.to_owned(),
        found: Found::Closest,
    })
}

/// Fewest edits that turn `pattern` into some substring of `text`.
fn substring_distance(pattern: &[char], text: &[char]) -> usize {
    // Row i holds the cost of matching pattern[..i] ending at each text index;
    // row 0 is free everywhere, so the match may start anywhere in the text.
    let mut previous = vec![0; text.len() + 1];
    let mut current = vec![0; text.len() + 1];
    for (i, &wanted) in pattern.iter().enumerate() {
        current[0] = i + 1;
        for (j, &found) in text.iter().enumerate() {
            let substitute = previous[j] + usize::from(wanted != found);
            current[j + 1] = substitute.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous.into_iter().min().unwrap_or(pattern.len())
}

/// Add the nearest source passage to a refused quote, when the source text can
/// be read here: it is retained, decoded text, not restricted, and within the
/// whole-excerpt bound. Otherwise the failure is returned unchanged.
pub(crate) fn point_at_nearest_passage(
    store: &mut Store,
    envelope: &Value,
    quote: Option<&str>,
    mut failure: Failure,
) -> Failure {
    if !failure
        .error
        .message
        .starts_with("OBSERVATION_TEXT_QUOTE_NOT_FOUND")
    {
        return failure;
    }
    let (Some(quote), Some(source)) = (quote, source_text(store, envelope)) else {
        return failure;
    };
    let Some(near) = nearest_line(&source, quote) else {
        failure.error.message.push_str(
            " No line of the source is near this quote, so it reads as reworded; copy a passage from the source file as it stands.",
        );
        return failure;
    };
    let shown: String = near
        .text
        .chars()
        .flat_map(|character| {
            if character.is_control() && character != '\t' {
                character.escape_default().collect::<Vec<_>>()
            } else {
                vec![character]
            }
        })
        .collect();
    let found = match near.found {
        Found::Prefix { matched, next } => format!(
            "The first {matched} of {} characters of the quote are in the source, from line {}, where the source continues with {}. ",
            quote.chars().count(),
            near.number,
            match next {
                None => "its end".to_owned(),
                Some('\n' | '\r') => "a line break".to_owned(),
                Some(character) => format!("{:?}", character.to_string()),
            }
        ),
        Found::Closest => String::new(),
    };
    failure.error.message.push_str(&format!(
        " {found}The nearest source passage is line {}: \"{shown}\". If it supports the statement, quote that line or an exact part of it.",
        near.number
    ));
    failure.error.expected = Some(json!({
        "nearest_source_line": near.number,
        "text": near.text,
    }));
    // Offer a retry only when that line is a quote the source holds once.
    let offered = shown == near.text
        && near.text.chars().count() <= MAX_SUGGESTED_CHARS
        && source.matches(near.text.as_str()).count() == 1;
    if offered && let Some(argv) = with_quote(env::args_os(), &near.text) {
        failure = failure.with_argv(argv);
    }
    failure
}

/// The decoded text of the observation's source, read the way `evidence read`
/// reads it. The hint reveals one line of it in a refusal; it records no
/// session read, because nothing is written.
fn source_text(store: &mut Store, envelope: &Value) -> Option<String> {
    let case = envelope
        .get("case_id")
        .and_then(Value::as_str)?
        .parse()
        .ok()?;
    let basis = envelope.pointer("/operations/0/payload/basis")?;
    let reference: RevisionRef = serde_json::from_value(basis.get("artifact_ref")?.clone()).ok()?;
    let mut selector: SelectorRequest =
        serde_json::from_value(basis.get("selector")?.clone()).ok()?;
    selector.selector = SelectorKind::Whole;
    let replay = store.case(case).ok()?;
    let record = replay.state.revision(reference)?;
    match store.evidence_read(&replay, record, selector).ok()?.excerpt {
        Excerpt::Text { text, .. } => Some(text),
        _ => None,
    }
}

/// The current invocation with `--quote` set to `quote`.
fn with_quote(args: impl Iterator<Item = OsString>, quote: &str) -> Option<Vec<String>> {
    let mut argv: Vec<String> = args
        .skip(1)
        .map(OsString::into_string)
        .collect::<Result<_, _>>()
        .ok()?;
    if argv.iter().any(|arg| arg == "--") {
        return None;
    }
    if let Some(index) = argv.iter().position(|arg| arg == "--quote") {
        *argv.get_mut(index + 1)? = quote.to_owned();
    } else if let Some(index) = argv.iter().position(|arg| arg.starts_with("--quote=")) {
        argv[index] = format!("--quote={quote}");
    } else {
        argv.extend(["--quote".to_owned(), quote.to_owned()]);
    }
    argv.insert(0, PROGRAM_NAME.to_owned());
    Some(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "Station history\n  1998 gauge installed at north pier\n  2022 gauge relocated to the weir footbridge; readings at the new site are not comparable\nwithout a published datum adjustment.\nHard requirements\nH1. Every singer must be able to get the current score without creating any online account.\n";

    fn near(quote: &str) -> Option<(usize, String)> {
        nearest_line(SOURCE, quote).map(|line| (line.number, line.text))
    }

    #[test]
    fn joined_lines_point_at_the_first_line_they_join() {
        assert_eq!(
            near(
                "readings at the new site are not comparable without a published datum adjustment."
            ),
            Some((
                3,
                "2022 gauge relocated to the weir footbridge; readings at the new site are not comparable".into()
            ))
        );
        assert_eq!(
            nearest_line("line one\nline two\n", "line one line two"),
            Some(NearLine {
                number: 1,
                text: "line one".into(),
                found: Found::Prefix {
                    matched: 8,
                    next: Some('\n')
                }
            })
        );
        assert_eq!(
            nearest_line("line one\nline two\n", r"line one\nline two").map(|line| line.number),
            Some(1)
        );
    }

    #[test]
    fn a_quote_that_drifts_late_points_at_the_line_it_starts_on() {
        assert_eq!(
            nearest_line(SOURCE, "1998 gauge installed at north pier, east bank"),
            Some(NearLine {
                number: 2,
                text: "1998 gauge installed at north pier".into(),
                found: Found::Prefix {
                    matched: 34,
                    next: Some('\n')
                }
            })
        );
        let near = nearest_line(SOURCE, "Station histories").unwrap();
        assert_eq!(
            near.found,
            Found::Prefix {
                matched: 14,
                next: Some('y')
            }
        );
    }

    #[test]
    fn a_lightly_reworded_quote_points_at_its_line() {
        assert_eq!(
            near(
                "Hard requirement H1 states that every singer must be able to get the current score without creating any online account."
            ),
            Some((
                6,
                "H1. Every singer must be able to get the current score without creating any online account.".into()
            ))
        );
        assert_eq!(
            near("gauge instaled at north pier").map(|(number, _)| number),
            Some(2)
        );
    }

    #[test]
    fn a_paraphrase_with_no_near_line_names_none() {
        assert_eq!(near("The river gauge moved to a different bridge."), None);
        assert_eq!(
            near("2022"),
            Some((3, SOURCE.lines().nth(2).unwrap().trim().into()))
        );
        assert_eq!(near(""), None);
    }

    #[test]
    fn substring_distance_counts_edits_to_the_best_substring() {
        let chars = |text: &str| text.chars().collect::<Vec<_>>();
        assert_eq!(
            substring_distance(&chars("needle"), &chars("a needle here")),
            0
        );
        assert_eq!(
            substring_distance(&chars("neeedle"), &chars("a needle here")),
            1
        );
        assert_eq!(substring_distance(&chars("abc"), &chars("")), 3);
    }

    #[test]
    fn the_retry_replaces_or_adds_the_quote() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            with_quote(
                args(&["bin", "observe", "x", "--quote", "old", "--format", "json"]).into_iter(),
                "new"
            ),
            Some(
                [
                    "investigation",
                    "observe",
                    "x",
                    "--quote",
                    "new",
                    "--format",
                    "json"
                ]
                .map(String::from)
                .to_vec()
            )
        );
        assert_eq!(
            with_quote(args(&["bin", "observe", "x"]).into_iter(), "new"),
            Some(
                ["investigation", "observe", "x", "--quote", "new"]
                    .map(String::from)
                    .to_vec()
            )
        );
        assert_eq!(
            with_quote(
                args(&["bin", "observe", "--quote=old", "x"]).into_iter(),
                "new"
            ),
            Some(
                ["investigation", "observe", "--quote=new", "x"]
                    .map(String::from)
                    .to_vec()
            )
        );
        assert_eq!(
            with_quote(args(&["bin", "observe", "--", "x"]).into_iter(), "new"),
            None
        );
    }
}
