//! Conservative Markdown escaping for authored text. Pure.
//!
//! Authored text is data. These helpers are deliberately stricter than the
//! Markdown they target: they drop or escape anything that could become a
//! link, embed, tag, callout, HTML, math, comment or block structure. The
//! dedicated safe-rendering work replaces them with a context-specific escaper
//! library and a hostile-content corpus; until then every authored string in a
//! generated note passes through one of these functions.
//!
//! - [`display_text`]: titles, H1 headings, aliases and link display text.
//!   An allowlist; everything else becomes a space.
//! - [`inline_text`]: longer prose on one line. Every ASCII punctuation
//!   character is backslash-escaped, line breaks become spaces.
//! - [`inline_code`]: exact machine text such as selectors and digests, inside
//!   a code span whose delimiter is longer than any backtick run it contains.

/// Bidirectional controls, zero-width characters and other invisible format
/// characters. They can make text display differently from its bytes.
pub fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{34f}'
            | '\u{61c}'
            | '\u{115f}'
            | '\u{1160}'
            | '\u{17b4}'
            | '\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff0}'..='\u{fff8}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e0fff}'
    )
}

fn is_unsafe(c: char) -> bool {
    c.is_control() || is_invisible_format(c)
}

/// Punctuation kept in display text. None of it can open a link, embed, tag,
/// emphasis, code span, HTML element, math, comment, highlight or heading.
const DISPLAY_PUNCTUATION: &[char] = &[
    '.', ',', ';', ':', '\'', '"', '?', '(', ')', '/', '+', '-', '&', '@', '\u{2013}', '\u{2014}',
    '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2026}',
];

/// Single-line display text for titles, aliases and link labels, or `None`
/// when nothing displayable remains. Letters, digits, spaces and
/// [`DISPLAY_PUNCTUATION`] are kept; every other character becomes a space,
/// runs of spaces collapse, and the result is bounded to `max_chars`.
pub fn display_text(input: &str, max_chars: usize) -> Option<String> {
    let mut out = String::new();
    let mut pending_space = false;
    let mut count = 0;
    for c in input.chars() {
        if is_invisible_format(c) {
            continue;
        }
        // A colon before a slash would let a viewer autolink `scheme://` text.
        let breaks_url = c == '/' && out.ends_with(':');
        let keep = !breaks_url && (c.is_alphanumeric() || DISPLAY_PUNCTUATION.contains(&c));
        if !keep {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            if count + 1 >= max_chars {
                break;
            }
            out.push(' ');
            count += 1;
            pending_space = false;
        }
        if count >= max_chars {
            break;
        }
        out.push(c);
        count += 1;
    }
    (!out.is_empty()).then_some(out)
}

/// One line of escaped prose, bounded to `max_chars` source characters. An
/// ellipsis marks truncation.
pub fn inline_text(input: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut last_space = true;
    for (index, c) in input.chars().enumerate() {
        if index >= max_chars {
            out.push('\u{2026}');
            break;
        }
        if is_invisible_format(c) {
            continue;
        }
        if c.is_whitespace() || is_unsafe(c) {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
            continue;
        }
        last_space = false;
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    let trimmed = out.trim_end_matches(' ').len();
    out.truncate(trimmed);
    out
}

/// A code span holding `input` verbatim, except that line breaks, control and
/// invisible format characters are shown as `\u{...}` escapes.
pub fn inline_code(input: &str) -> String {
    let mut body = String::new();
    for c in input.chars() {
        if is_unsafe(c) {
            body.push_str(&format!("\\u{{{:x}}}", u32::from(c)));
        } else {
            body.push(c);
        }
    }
    let mut longest = 0;
    let mut run = 0;
    for c in body.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest + 1);
    // Pad so a leading or trailing backtick cannot merge with the delimiter.
    if body.is_empty() || body.starts_with('`') || body.ends_with('`') {
        format!("{fence} {body} {fence}")
    } else {
        format!("{fence}{body}{fence}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_text_keeps_words_and_drops_markup() {
        assert_eq!(
            display_text("Queue A \u{2014} p99 enqueue latency", 120).unwrap(),
            "Queue A \u{2014} p99 enqueue latency"
        );
        let hostile =
            "]] | [[Other]]\n---\n#tag ![[x]] <b>bold</b> $x$ %%c%% ==h== ~~s~~ ^block `c`";
        let shown = display_text(hostile, 200).unwrap();
        for bad in [
            "[", "]", "|", "\n", "#", "!", "<", ">", "$", "%", "=", "~", "^", "`", "*", "_",
        ] {
            assert!(!shown.contains(bad), "{bad:?} in {shown:?}");
        }
        assert_eq!(shown, "Other --- tag x b bold /b x c h s block c");
        assert_eq!(
            display_text("\u{202e}evil\u{200b}ly", 20).unwrap(),
            "evilly"
        );
        assert_eq!(display_text("[[]]|#", 20), None);
        assert_eq!(
            display_text("see https://example.invalid/x and a/b", 80).unwrap(),
            "see https: example.invalid/x and a/b"
        );
        assert_eq!(display_text("abcdef ghij", 8).unwrap(), "abcdef g");
        assert_eq!(display_text("  spaced   out  ", 50).unwrap(), "spaced out");
    }

    #[test]
    fn inline_text_escapes_every_ascii_punctuation_mark() {
        assert_eq!(
            inline_text("a [[b|c]] ![x](y) <i>\n# d", 100),
            "a \\[\\[b\\|c\\]\\] \\!\\[x\\]\\(y\\) \\<i\\> \\# d"
        );
        assert_eq!(inline_text("abcdef", 3), "abc\u{2026}");
        assert_eq!(inline_text("x\u{202e}y\u{0}z", 10), "xy z");
    }

    #[test]
    fn inline_code_delimiter_outlasts_any_backtick_run() {
        assert_eq!(inline_code("sha256:ab"), "`sha256:ab`");
        assert_eq!(inline_code("a``b"), "```a``b```");
        assert_eq!(inline_code("`x"), "`` `x ``");
        assert_eq!(inline_code(""), "`  `");
        assert_eq!(inline_code("a\nb\u{202e}"), "`a\\u{a}b\\u{202e}`");
    }
}
