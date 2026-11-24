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
    identity::Id,
    state::Record,
    wire::{Digest, Failure},
};
use investigation_store::{Store, journal::CaseReplay, store::evidence_read::Excerpt};
use serde_json::{Value, json};

use crate::source_text::{self, SourceText};

/// Longer quotes get the generic advice only; the search is O(quote × source).
const MAX_QUOTE_CHARS: usize = 1_000;
/// A line is near when at most this fraction of the shorter side must change.
const NEAR_NUMERATOR: usize = 1;
const NEAR_DENOMINATOR: usize = 3;
/// A suggested line longer than this is shown but not offered as a retry.
const MAX_SUGGESTED_CHARS: usize = 2_048;
/// The largest source the hint reads, in chunks of one excerpt each. A larger
/// source gets a line-range read as its repair instead of a named line.
pub(crate) const MAX_HINT_SOURCE_BYTES: u64 = 512 * 1024;
/// Bound on the closest-line search, in quote characters times source bytes:
/// past it only the longest-prefix search runs. A 1,000-character quote
/// against a 32 KiB source stays inside it.
const MAX_EDIT_WORK: usize = 1 << 26;

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
    longest_prefix(source, quote, chars).or_else(|| {
        (chars.saturating_mul(source.len()) <= MAX_EDIT_WORK)
            .then(|| closest_line(source, quote))
            .flatten()
    })
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
        let mut candidates = Vec::new();
        // The quote inside the line needs at least one edit per character
        // the line lacks; skip the search when that alone is too many.
        if quote.len().saturating_sub(line.len()) * NEAR_DENOMINATOR <= quote.len() * NEAR_NUMERATOR
        {
            candidates.push((substring_distance(&quote, &line), quote.len()));
        }
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

/// Add the nearest source passage to a refused `observe` quote, when the
/// source text can be read here (see [`hint`]). The retry repeats the call
/// with that line as `--quote`.
pub(crate) fn point_at_nearest_passage(
    store: &mut Store,
    envelope: &Value,
    quote: Option<&str>,
    failure: Failure,
) -> Failure {
    hint(store, envelope, 0, quote, Caller::Shortcut, failure)
}

/// Add the nearest source passage to a refused quote in a raw `record`
/// request. The operation is the one the failure path names. A request is a
/// file, so no retry is offered for a near line: the message names the
/// JSON Pointer to change.
pub(crate) fn point_record_at_nearest_passage(
    store: &mut Store,
    envelope: &Value,
    failure: Failure,
) -> Failure {
    let index = failure
        .error
        .path
        .as_deref()
        .and_then(|path| {
            path.strip_prefix("/operations/")?
                .split('/')
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or(0);
    let quote = envelope
        .pointer(&format!(
            "/operations/{index}/payload/basis/text_quote/exact"
        ))
        .and_then(Value::as_str)
        .map(str::to_owned);
    hint(
        store,
        envelope,
        index,
        quote.as_deref(),
        Caller::Record { index },
        failure,
    )
}

/// Who refused the quote, which decides the form of the advice.
#[derive(Clone, Copy)]
enum Caller {
    Shortcut,
    Record { index: usize },
}

/// The text a refused quote was checked against, read the way `evidence
/// read` reads it. The hint reveals one line of it in a refusal; it records
/// no session read, because nothing is written.
struct Cited<'a> {
    case: Id,
    record: &'a Record,
    /// The whole source, or the lines a `text_lines` selector names.
    text: SourceText,
    /// Source line number of the first line of `text`.
    first_line: usize,
    /// Whether `text` is the whole source.
    whole: bool,
    /// The digest the cited selector names.
    digest: Digest,
}

/// Add the nearest passage to a refused quote: the source text is read
/// through `evidence_read` (retained, decoded text, not restricted or
/// withdrawn), whole or in bounded chunks up to [`MAX_HINT_SOURCE_BYTES`].
/// A quote cited inside a `text_lines` selection is compared with those
/// lines only; other positional selectors get no hint. Otherwise the failure
/// is returned unchanged. What is accepted never changes.
fn hint(
    store: &mut Store,
    envelope: &Value,
    index: usize,
    quote: Option<&str>,
    caller: Caller,
    mut failure: Failure,
) -> Failure {
    if !failure
        .error
        .message
        .starts_with("OBSERVATION_TEXT_QUOTE_NOT_FOUND")
    {
        return failure;
    }
    let Some(quote) = quote else {
        return failure;
    };
    let Some(basis) = envelope.pointer(&format!("/operations/{index}/payload/basis")) else {
        return failure;
    };
    let Some(case) = envelope
        .get("case_id")
        .and_then(Value::as_str)
        .and_then(|case| case.parse::<Id>().ok())
    else {
        return failure;
    };
    let Ok(replay) = store.case(case) else {
        return failure;
    };
    let Some(cited) = cited(store, &replay, case, basis) else {
        return failure;
    };
    let read_argv = || source_text::read_argv(store, &replay, cited.case, cited.record);
    let source = match &cited.text {
        SourceText::Read(text) => text,
        SourceText::Unreadable => return failure,
        SourceText::TooLarge { size } => {
            failure.error.message.push_str(&format!(
                " The source holds {size} bytes, more than the {MAX_HINT_SOURCE_BYTES} this hint searches, so no nearest line is named. Read it in line ranges with investigation evidence read {} --lines START:END and copy a passage as it stands.",
                cited.record.header.id
            ));
            if let Some(argv) = read_argv() {
                failure = failure.with_argv(argv);
            }
            return failure;
        }
    };
    let scope = if cited.whole {
        "the source".to_owned()
    } else {
        format!(
            "the selected lines {}-{}",
            cited.first_line,
            cited.first_line + source.split_inclusive('\n').count().max(1) - 1
        )
    };
    let quote_pointer = match caller {
        Caller::Shortcut => None,
        Caller::Record { index } => Some(format!("/operations/{index}/payload/basis/text_quote")),
    };
    // A record quote may carry a prefix or suffix: the exact text can be
    // present while the text around it differs.
    if let Some(pointer) = &quote_pointer
        && let Some(start) = source.find(quote)
    {
        failure.error.message.push_str(&format!(
            " The exact text is in {scope} from line {}, so {pointer}/prefix or {pointer}/suffix does not match the text beside it there. Copy them from the source as it stands, or omit them.",
            cited.first_line + source[..start].matches('\n').count()
        ));
        return failure;
    }
    // A quote the source holds outside a text_lines selection is not a near
    // miss: the selection does not cover it.
    if !cited.whole
        && let SourceText::Read(whole) = source_text::read(
            store,
            &replay,
            cited.record,
            &cited.digest,
            MAX_HINT_SOURCE_BYTES,
        )
        && let Some(start) = whole.find(quote)
    {
        failure.error.message.push_str(&format!(
            " The quote is in the source from line {}, outside {scope}. Widen the selector to cover it, or cite the whole source.",
            whole[..start].matches('\n').count() + 1
        ));
        return failure;
    }
    let Some(near) = nearest_line(source, quote) else {
        failure.error.message.push_str(&format!(
            " No line of {scope} is near this quote, so it reads as reworded."
        ));
        if let Some(argv) = read_argv() {
            // `--whole`, or `--lines 1:N` for a source over one read.
            let range: Vec<&str> = argv
                .iter()
                .map(String::as_str)
                .skip_while(|part| !matches!(*part, "--whole" | "--lines"))
                .take_while(|part| *part != "--format")
                .collect();
            let range = range.join(" ");
            failure.error.message.push_str(&format!(
                " Read the source with investigation evidence read {} {range} and copy a passage as it stands.",
                cited.record.header.id
            ));
            failure = failure.with_argv(argv);
        } else {
            failure
                .error
                .message
                .push_str(" Copy a passage from the source file as it stands.");
        }
        return failure;
    };
    let number = cited.first_line + near.number - 1;
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
            "The first {matched} of {} characters of the quote are in {scope}, from line {number}, where {scope} continues with {}. ",
            quote.chars().count(),
            match next {
                None => "its end".to_owned(),
                Some('\n' | '\r') => "a line break".to_owned(),
                Some(character) => format!("{:?}", character.to_string()),
            }
        ),
        Found::Closest => String::new(),
    };
    let advice = match &quote_pointer {
        None => "quote that line or an exact part of it.".to_owned(),
        Some(pointer) => format!("set {pointer}/exact to that line or an exact part of it."),
    };
    failure.error.message.push_str(&format!(
        " {found}The nearest source passage is line {number}: \"{shown}\". If it supports the statement, {advice}"
    ));
    failure.error.expected = Some(json!({
        "nearest_source_line": number,
        "text": near.text,
    }));
    // Offer a retry only when that line is a quote the source holds once.
    let offered = matches!(caller, Caller::Shortcut)
        && shown == near.text
        && near.text.chars().count() <= MAX_SUGGESTED_CHARS
        && source.matches(near.text.as_str()).count() == 1;
    if offered && let Some(argv) = with_quote(env::args_os(), &near.text) {
        failure = failure.with_argv(argv);
    }
    failure
}

/// The cited Artifact and the text its quote was checked against.
fn cited<'a>(store: &Store, replay: &'a CaseReplay, case: Id, basis: &Value) -> Option<Cited<'a>> {
    let reference: RevisionRef = serde_json::from_value(basis.get("artifact_ref")?.clone()).ok()?;
    let selector: SelectorRequest = serde_json::from_value(basis.get("selector")?.clone()).ok()?;
    let record = replay.state.revision(reference)?;
    let (text, first_line, whole) = match &selector.selector {
        SelectorKind::Whole => (
            source_text::read(
                store,
                replay,
                record,
                &selector.artifact_digest,
                MAX_HINT_SOURCE_BYTES,
            ),
            1,
            true,
        ),
        SelectorKind::TextLines { start_line, .. } => {
            let text = match store.evidence_read(replay, record, selector.clone()) {
                Ok(read) => match read.excerpt {
                    Excerpt::Text { text, .. } => SourceText::Read(text),
                    _ => SourceText::Unreadable,
                },
                Err(_) => SourceText::Unreadable,
            };
            (text, usize::try_from(*start_line).ok()?, false)
        }
        _ => return None,
    };
    Some(Cited {
        case,
        record,
        text,
        first_line,
        whole,
        digest: selector.artifact_digest,
    })
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
