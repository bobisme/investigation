//! Context-specific Markdown escaping for untrusted and authored text. Pure.
//!
//! Authored text is data. Every authored or captured string in a generated
//! file passes through exactly one of these functions, chosen by where the
//! string lands. None of them can produce a link, embed, HTML, heading, block
//! structure, callout, comment, tag, math, highlight, block reference or
//! inline field from data, and none passes through a source-provided fence
//! language. Internal links are built only by [`crate::layout::wikilink`]
//! from validated plan paths.
//!
//! | Context | Function | Technique |
//! |---|---|---|
//! | Title, H1, alias, link display text, `inv_status` | [`display_text`] | ASCII markup mapped to inert look-alikes; no escapes needed anywhere |
//! | One line of prose, including a table cell | [`inline_text`] | every ASCII punctuation mark backslash-escaped |
//! | Exact machine text on a line | [`inline_code`] | code span longer than any backtick run |
//! | Exact machine text in a table cell | [`table_code`] | [`inline_code`] with GFM pipe escapes |
//! | Block excerpt | [`fenced_block`] | fixed [`EXCERPT_LANGUAGE`] fence longer than any backtick run |
//! | YAML scalar | [`crate::yaml::quoted`] | double-quoted with escapes |
//!
//! Every context also drops bidirectional controls and other invisible format
//! characters ([`is_invisible_format`]), and the text contexts defang URL
//! patterns a viewer would autolink (`//`, `www.`, `scheme:`, `name@host`).
//! The defanged text stays readable but is never a URL. Bare host names
//! without a scheme or `www.` are left as text; viewers that linkify those
//! fuzzily are outside what text escaping can promise.
//!
//! These are product integrity controls, not a claim that labeling alone
//! prevents every prompt-injection failure.

use std::fmt::Write as _;

pub use investigation_core::authored_text::is_invisible_format;
use investigation_core::authored_text::{defang, one_line};

/// The only info string generated fenced blocks carry. A source-provided
/// language is never passed through, so a plugin fence cannot run.
pub const EXCERPT_LANGUAGE: &str = "text";

/// Characters no context carries literally: controls, invisible format
/// characters and the noncharacters YAML cannot hold.
fn is_unsafe(c: char) -> bool {
    c.is_control() || is_invisible_format(c) || matches!(c, '\u{fffe}' | '\u{ffff}')
}

/// Single-line display text for titles, H1 headings, aliases, link labels
/// and other short labels, or `None` when nothing displayable remains.
///
/// This is the core title sanitizer,
/// [`investigation_core::authored_text::display_title`], which the CLI uses
/// too. The result needs no escaping in a heading, a wikilink label, a table
/// cell or a YAML scalar: ASCII markup characters become inert look-alikes
/// (`[` shows as `(`, `|` as `¦`, `#` as `＃`, and so on), URL patterns are
/// defanged, and line breaks and controls become spaces. Letters, digits,
/// emoji and other punctuation are kept. Bounded to `max_chars` characters.
/// Applying it again to its own output changes nothing.
pub fn display_text(input: &str, max_chars: usize) -> Option<String> {
    let out = investigation_core::authored_text::display_title(input, max_chars);
    (!out.is_empty()).then_some(out)
}

/// One line of escaped prose, bounded to `max_chars` characters after line
/// breaks are folded. An ellipsis marks truncation.
///
/// Every ASCII punctuation character is backslash-escaped, so nothing in the
/// text can open markup; this includes `|`, so the result is also a safe
/// table cell. URL patterns are defanged first, because escaping alone does
/// not stop a viewer from autolinking the decoded text.
pub fn inline_text(input: &str, max_chars: usize) -> String {
    let mut chars = one_line(&investigation_core::local_path::elide_local_paths(input));
    let truncated = chars.len() > max_chars;
    chars.truncate(max_chars);
    while chars.last() == Some(&' ') {
        chars.pop();
    }
    defang(&mut chars);
    let mut out = String::with_capacity(chars.len() * 2);
    for c in chars {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    if truncated {
        out.push('\u{2026}');
    }
    out
}

/// Visible `\u{...}` form of a character no context carries literally.
fn shown_escape(out: &mut String, c: char) {
    let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
}

fn longest_run(text: &str, of: char) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == of { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// A code span holding `input` verbatim, except that line breaks, control and
/// invisible format characters are shown as `\u{...}` escapes. The delimiter
/// is longer than any backtick run inside, and padding keeps leading and
/// trailing backticks and spaces exact.
pub fn inline_code(input: &str) -> String {
    let mut body = String::new();
    for c in input.chars() {
        if is_unsafe(c) {
            shown_escape(&mut body, c);
        } else {
            body.push(c);
        }
    }
    let fence = "`".repeat(longest_run(&body, '`') + 1);
    // A viewer strips one space from each end when both ends have one, and a
    // backtick at either end would merge with the delimiter.
    let spaced = body.starts_with(' ') && body.ends_with(' ') && body.trim() != "";
    if body.is_empty() || body.starts_with('`') || body.ends_with('`') || spaced {
        format!("{fence} {body} {fence}")
    } else {
        format!("{fence}{body}{fence}")
    }
}

/// [`inline_code`] for a Markdown table cell: a pipe would end the cell even
/// inside a code span, so each one is escaped as the table syntax requires.
pub fn table_code(input: &str) -> String {
    inline_code(input).replace('|', "\\|")
}

/// A fenced block excerpt with the fixed [`EXCERPT_LANGUAGE`] info string.
///
/// The fence is backticks, at least three and longer than any backtick run
/// in the excerpt, so no excerpt line can close it; tilde fences inside stay
/// text. CRLF becomes LF, and every other control or invisible format
/// character, including a lone CR, is shown as a `\u{...}` escape; tabs and
/// line feeds are kept. Every line, fences included, starts with
/// `line_prefix`, so a block can sit in a list item or a callout. The result
/// ends with a line feed.
pub fn fenced_block(input: &str, line_prefix: &str) -> String {
    let mut body = String::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\t' => body.push(c),
            c if is_unsafe(c) => shown_escape(&mut body, c),
            c => body.push(c),
        }
    }
    let fence = "`".repeat((longest_run(&body, '`') + 1).max(3));
    let mut out = format!("{line_prefix}{fence}{EXCERPT_LANGUAGE}\n");
    for line in body.lines() {
        let _ = writeln!(out, "{line_prefix}{line}");
    }
    let _ = writeln!(out, "{line_prefix}{fence}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_text_maps_markup_to_inert_look_alikes() {
        assert_eq!(
            display_text("Queue A \u{2014} p99 enqueue latency \u{1f600}", 120).unwrap(),
            "Queue A \u{2014} p99 enqueue latency \u{1f600}"
        );
        let hostile =
            "]] | [[Other]]\n---\n#tag ![[x]] <b>bold</b> $x$ %%c%% ==h== ~~s~~ ^block `c` *e* _u_";
        let shown = display_text(hostile, 200).unwrap();
        for bad in [
            "[", "]", "|", "\n", "#", "<", ">", "$", "%%", "==", "~", "^", "`", "*", " _", "\\",
        ] {
            assert!(!shown.contains(bad), "{bad:?} in {shown:?}");
        }
        assert_eq!(
            shown,
            ")) \u{a6} ((Other)) --- \u{ff03}tag !((x)) \u{2039}b\u{203a}bold\u{2039}/b\u{203a} \
             \u{ff04}x\u{ff04} \u{ff05}\u{ff05}c\u{ff05}\u{ff05} \u{ff1d}\u{ff1d}h\u{ff1d}\u{ff1d} \
             \u{223c}\u{223c}s\u{223c}\u{223c} \u{2c6}block 'c' \u{2217}e\u{2217} \u{ff3f}u\u{ff3f}"
        );
        assert_eq!(
            display_text("\u{202e}evil\u{200b}ly", 20).unwrap(),
            "evilly"
        );
        assert_eq!(display_text(" \n\u{200b}\t", 20), None);
        assert_eq!(display_text("abcdef ghij", 8).unwrap(), "abcdef g");
        assert_eq!(display_text("abcdef ghij", 7).unwrap(), "abcdef");
        assert_eq!(
            display_text("  spaced \r\n  out  ", 50).unwrap(),
            "spaced out"
        );
        // Ordinary punctuation and intraword underscores survive.
        assert_eq!(
            display_text("p=0.05, 5% & a_b: (x) 'q' \"r\" a/b -> 3!", 80).unwrap(),
            "p=0.05, 5% & a_b: (x) 'q' \"r\" a/b -\u{203a} 3!"
        );
        assert_eq!(
            display_text("&lt;b&gt; &#60; key:: v", 80).unwrap(),
            "\u{ff06}lt;b\u{ff06}gt; \u{ff06}\u{ff03}60; key:\u{2236} v"
        );
    }

    #[test]
    fn urls_are_defanged_in_every_text_context() {
        let shown = display_text(
            "see https://example.invalid/x, //cdn.invalid, WWW.example.invalid, \
             mailto:a@example.invalid and javascript:alert(1)",
            200,
        )
        .unwrap();
        assert_eq!(
            shown,
            "see https\u{2236}\u{2215}\u{2215}example.invalid/x, \u{2215}\u{2215}cdn.invalid, \
             WWW\u{2024}example.invalid, mailto\u{2236}a\u{ff20}example.invalid and \
             javascript\u{2236}alert(1)"
        );
        // Ordinary colons, slashes and at signs stay.
        assert_eq!(
            display_text("Note: a/b @team data: x", 80).unwrap(),
            "Note: a/b @team data: x"
        );
        assert_eq!(
            inline_text("https://example.invalid www.example.invalid", 200),
            "https\u{2236}\u{2215}\u{2215}example\\.invalid www\u{2024}example\\.invalid"
        );
    }

    #[test]
    fn display_text_is_idempotent() {
        for input in [
            "]] | [[Other]] #tag ==x== %%c%% __init__ &lt; a::b https://x.invalid www.x",
            "Queue A \u{2014} p99",
            "a@b.c mailto:x",
        ] {
            let once = display_text(input, 200).unwrap();
            assert_eq!(display_text(&once, 200).unwrap(), once, "{input:?}");
        }
    }

    #[test]
    fn inline_text_escapes_every_ascii_punctuation_mark() {
        assert_eq!(
            inline_text("a [[b|c]] ![x](y) <i>\n# d", 100),
            "a \\[\\[b\\|c\\]\\] \\!\\[x\\]\\(y\\) \\<i\\> \\# d"
        );
        assert_eq!(inline_text("abcdef", 3), "abc\u{2026}");
        assert_eq!(inline_text("ab  \n cd", 3), "ab\u{2026}");
        assert_eq!(inline_text("x\u{202e}y\u{0}z", 10), "xy z");
        assert_eq!(inline_text("emoji \u{1f600}", 10), "emoji \u{1f600}");
    }

    #[test]
    fn inline_code_delimiter_outlasts_any_backtick_run() {
        assert_eq!(inline_code("sha256:ab"), "`sha256:ab`");
        assert_eq!(inline_code("a``b"), "```a``b```");
        assert_eq!(inline_code("`x"), "`` `x ``");
        assert_eq!(inline_code(""), "`  `");
        assert_eq!(inline_code(" x "), "`  x  `");
        assert_eq!(inline_code(" x"), "` x`");
        assert_eq!(inline_code("a\nb\u{202e}"), "`a\\u{a}b\\u{202e}`");
        assert_eq!(table_code("a|b"), "`a\\|b`");
    }

    #[test]
    fn fenced_blocks_use_a_fixed_language_and_an_unclosable_fence() {
        assert_eq!(
            fenced_block("```dataviewjs\ndv.el()\n```", ""),
            "````text\n```dataviewjs\ndv.el()\n```\n````\n"
        );
        assert_eq!(
            fenced_block("a\r\nb\rc\u{202e}", "> "),
            "> ```text\n> a\n> b\\u{d}c\\u{202e}\n> ```\n"
        );
        assert_eq!(fenced_block("", ""), "```text\n```\n");
        assert_eq!(fenced_block("~~~\nx\n", ""), "```text\n~~~\nx\n```\n");
    }
}
