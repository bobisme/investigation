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

/// The only info string generated fenced blocks carry. A source-provided
/// language is never passed through, so a plugin fence cannot run.
pub const EXCERPT_LANGUAGE: &str = "text";

/// Replaces each slash of a `//` run, so `scheme://` and `//host` never
/// autolink.
const SLASH: char = '\u{2215}';
/// Replaces the dot after `www`.
const DOT: char = '\u{2024}';
/// Replaces `@` between an address local part and a host.
const AT: char = '\u{ff20}';
/// Replaces the colon after a URI scheme, and the second colon of `::`.
const COLON: char = '\u{2236}';

/// URI schemes whose colon is defanged when text follows it directly.
const SCHEMES: &[&str] = &[
    "data",
    "file",
    "ftp",
    "ftps",
    "http",
    "https",
    "irc",
    "javascript",
    "mailto",
    "obsidian",
    "sftp",
    "sms",
    "ssh",
    "tel",
    "vbscript",
    "ws",
    "wss",
    "xmpp",
];

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

/// Characters no context carries literally: controls, invisible format
/// characters and the noncharacters YAML cannot hold.
fn is_unsafe(c: char) -> bool {
    c.is_control() || is_invisible_format(c) || matches!(c, '\u{fffe}' | '\u{ffff}')
}

/// The text on one line: invisible format characters and noncharacters are
/// dropped, every run of other controls and whitespace (including CR, LF and
/// the Unicode line separators) becomes one space, and the ends are trimmed.
fn one_line(input: &str) -> Vec<char> {
    let mut out = Vec::new();
    let mut pending_space = false;
    for c in input.chars() {
        if is_invisible_format(c) || matches!(c, '\u{fffe}' | '\u{ffff}') {
            continue;
        }
        if c.is_whitespace() || c.is_control() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    out
}

/// Whether the ASCII letters ending just before `at` name a URI scheme that
/// starts a word.
fn scheme_ends_at(chars: &[char], at: usize) -> bool {
    let start = chars[..at]
        .iter()
        .rposition(|c| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
        .map_or(0, |index| index + 1);
    if start == at || (start > 0 && chars[start - 1].is_alphanumeric()) {
        return false;
    }
    let scheme: String = chars[start..at].iter().collect();
    SCHEMES.contains(&scheme.to_ascii_lowercase().as_str())
}

fn is_address_local(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '_')
}

/// Break every pattern a viewer would autolink in plain text.
fn defang(chars: &mut [char]) {
    let original = chars.to_vec();
    for (index, slot) in chars.iter_mut().enumerate() {
        let prev = index.checked_sub(1).map(|i| original[i]);
        let next = original.get(index + 1).copied();
        *slot = match original[index] {
            '/' if prev == Some('/') || next == Some('/') => SLASH,
            '.' if index >= 3
                && original[index - 3..index]
                    .iter()
                    .all(|c| c.eq_ignore_ascii_case(&'w'))
                && (index == 3 || !original[index - 4].is_alphanumeric()) =>
            {
                DOT
            }
            '@' if prev.is_some_and(is_address_local)
                && next.is_some_and(|c| c.is_ascii_alphanumeric()) =>
            {
                AT
            }
            ':' if next.is_some_and(|c| !c.is_whitespace()) && scheme_ends_at(&original, index) => {
                COLON
            }
            c => c,
        };
    }
}

/// Whether `chars[at..]` starts an HTML character reference such as `&lt;`.
fn starts_reference(chars: &[char], at: usize) -> bool {
    let name = chars[at + 1..]
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || **c == '#')
        .count();
    name > 0 && chars.get(at + 1 + name) == Some(&';')
}

/// The inert look-alike for markup character `chars[at]` in display text.
fn display_char(chars: &[char], at: usize) -> char {
    let prev = at.checked_sub(1).map(|i| chars[i]);
    let next = chars.get(at + 1).copied();
    let doubled = |c| prev == Some(c) || next == Some(c);
    match chars[at] {
        '[' => '(',
        ']' => ')',
        '<' => '\u{2039}',
        '>' => '\u{203a}',
        '|' => '\u{a6}',
        '#' => '\u{ff03}',
        '`' => '\'',
        '*' => '\u{2217}',
        '\\' => '\u{29f5}',
        '$' => '\u{ff04}',
        '^' => '\u{2c6}',
        '~' => '\u{223c}',
        '=' if doubled('=') => '\u{ff1d}',
        '%' if doubled('%') => '\u{ff05}',
        '_' if !(prev.is_some_and(char::is_alphanumeric)
            && next.is_some_and(char::is_alphanumeric)) =>
        {
            '\u{ff3f}'
        }
        '&' if starts_reference(chars, at) => '\u{ff06}',
        ':' if prev == Some(':') => COLON,
        c => c,
    }
}

/// Single-line display text for titles, H1 headings, aliases, link labels
/// and other short labels, or `None` when nothing displayable remains.
///
/// The result needs no escaping in a heading, a wikilink label, a table cell
/// or a YAML scalar: ASCII markup characters become inert look-alikes (`[`
/// shows as `(`, `|` as `¦`, `#` as `＃`, and so on), URL patterns are
/// defanged, and line breaks and controls become spaces. Letters, digits,
/// emoji and other punctuation are kept. Bounded to `max_chars` characters.
/// Applying it again to its own output changes nothing.
pub fn display_text(input: &str, max_chars: usize) -> Option<String> {
    let mut chars = one_line(&investigation_core::local_path::elide_local_paths(input));
    defang(&mut chars);
    let mut out: String = (0..chars.len())
        .take(max_chars)
        .map(|at| display_char(&chars, at))
        .collect();
    out.truncate(out.trim_end().len());
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
