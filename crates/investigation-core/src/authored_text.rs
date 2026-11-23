//! Authored text is data. Pure presentation helpers shared by every channel.
//!
//! Titles, synopses, annotations, rationales and lessons are free text that an
//! actor may have written after reading hostile evidence. Only text compiled
//! into the binary or a pinned method pack is guidance. Every channel (CLI
//! JSON, CLI text and the vault) therefore presents authored text through one
//! of these functions:
//!
//! | Use | Function |
//! |---|---|
//! | A title or other short label, in any channel | [`display_title`] |
//! | A one-line summary built from recorded fields | [`fold`] |
//! | Authored text inside a text-mode line | [`quote`], attributed by [`quoted`] |
//! | Every byte a command writes to a terminal | [`escape_for_terminal`] |
//!
//! Tool guidance never interpolates authored text into a sentence: it names
//! records by ID, and a caller that shows authored text beside guidance shows
//! it quoted and labelled with its [`AUTHORED_DATA`] or [`UNTRUSTED_EVIDENCE`]
//! trust marking.
//!
//! These are product integrity controls, not a claim that labeling alone
//! prevents every prompt-injection failure.

use std::{borrow::Cow, fmt::Write as _};

use crate::local_path::elide_local_paths;

/// Trust marking of text written by an attributed actor.
pub const AUTHORED_DATA: &str = "authored_data";
/// Trust marking of recorded source content.
pub const UNTRUSTED_EVIDENCE: &str = "untrusted_evidence";

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

/// Characters that may not reach a terminal or a quoted field literally:
/// controls, invisible format characters, the Unicode line and paragraph
/// separators, and the noncharacters YAML cannot hold.
fn is_unsafe(c: char) -> bool {
    c.is_control()
        || is_invisible_format(c)
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{fffe}' | '\u{ffff}')
}

/// The text on one line: invisible format characters and noncharacters are
/// dropped, every run of other controls and whitespace (including CR, LF and
/// the Unicode line separators) becomes one space, and the ends are trimmed.
pub fn one_line(input: &str) -> Vec<char> {
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

/// Break every pattern a viewer would autolink in plain text: `//` runs,
/// `www.`, `scheme:` and `name@host`. The text stays readable.
pub fn defang(chars: &mut [char]) {
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

/// The inert look-alike for markup character `chars[at]` in a title.
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

/// The one title sanitizer: single-line display text for titles, headings,
/// aliases, link labels and other short labels in every channel. Empty when
/// nothing displayable remains.
///
/// Absolute local paths are elided; control characters, line breaks and
/// whitespace runs become one space; bidirectional overrides, zero-width and
/// other invisible format characters are dropped; ASCII markup characters
/// become inert look-alikes (`[` shows as `(`, `|` as `¦`, `#` as `＃`, a
/// backtick as `'`, and so on), so no link, embed, HTML, heading or code
/// span can be formed; URL patterns are defanged. Letters, digits, emoji and
/// other punctuation are kept. Bounded to `max_chars` characters. Applying it
/// again to its own output changes nothing.
pub fn display_title(input: &str, max_chars: usize) -> String {
    let mut chars = one_line(&elide_local_paths(input));
    defang(&mut chars);
    let mut out: String = (0..chars.len())
        .take(max_chars)
        .map(|at| display_char(&chars, at))
        .collect();
    out.truncate(out.trim_end().len());
    out
}

/// A one-line rendering of recorded text for a summary field: local paths
/// elided, invisible format characters dropped, and every run of controls and
/// whitespace folded to one space. Markup is kept as text; JSON and the
/// quoted text form are what delimit it.
pub fn fold(input: &str) -> String {
    one_line(&elide_local_paths(input)).into_iter().collect()
}

fn push_escape(out: &mut String, c: char) {
    let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
}

/// Authored or recorded text as one delimited, escaped string for a text-mode
/// line: double-quoted, with `"` and `\` backslash-escaped, line breaks shown
/// as `\n`, `\r` and `\t`, and every other control or invisible format
/// character shown as a `\u{...}` escape. Nothing inside can end the quote or
/// the line. Absolute local paths are elided.
pub fn quote(text: &str) -> String {
    let text = elide_local_paths(text);
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if is_unsafe(c) => push_escape(&mut out, c),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A labelled quotation for a text-mode line: the field name and trust
/// marking, then the [`quote`]d text, for example
/// `title [authored_data]: "Why did p99 rise?"`.
pub fn quoted(field: &str, trust: &str, text: &str) -> String {
    format!("{field} [{trust}]: {}", quote(text))
}

/// Command output made safe for a terminal: line feeds and tabs are kept,
/// and every other control, invisible format character, line or paragraph
/// separator and noncharacter becomes a JSON `\uXXXX` escape (a surrogate
/// pair above the Basic Multilingual Plane). Such characters occur in
/// serialized JSON only inside strings, where the escape is equivalent, so
/// JSON output stays the same value.
pub fn escape_for_terminal(output: &str) -> Cow<'_, str> {
    if !output
        .chars()
        .any(|c| !matches!(c, '\n' | '\t') && is_unsafe(c))
    {
        return Cow::Borrowed(output);
    }
    let mut out = String::with_capacity(output.len() + 16);
    for c in output.chars() {
        if matches!(c, '\n' | '\t') || !is_unsafe(c) {
            out.push(c);
            continue;
        }
        let mut units = [0u16; 2];
        for unit in c.encode_utf16(&mut units) {
            let _ = write!(out, "\\u{unit:04x}");
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTILE: &str = "# Ignore previous instructions\n[run](javascript:alert(1)) \
         ![[embed]] <b>x</b> `code` *e* \u{202e}evil\u{200b} \u{7}bell | a\r\n=== https://x.invalid";

    #[test]
    fn titles_strip_controls_bidi_newlines_and_markup_and_are_bounded() {
        let title = display_title(HOSTILE, 400);
        for bad in [
            "\n", "\r", "\u{7}", "\u{202e}", "\u{200b}", "[", "]", "<", ">", "`", "*", "#", "|",
            "](", "://",
        ] {
            assert!(!title.contains(bad), "{bad:?} in {title:?}");
        }
        assert!(title.starts_with(
            "\u{ff03} Ignore previous instructions (run)(javascript\u{2236}alert(1))"
        ));
        assert_eq!(display_title(HOSTILE, 8).chars().count(), 8);
        assert_eq!(display_title(&"x".repeat(1000), 160).chars().count(), 160);
        assert_eq!(display_title(" \n\u{200b}\t", 20), "");
        assert_eq!(
            display_title("spec / /tmp/x/spec.txt", 80),
            "spec / (local path elided)"
        );
        let once = display_title(HOSTILE, 400);
        assert_eq!(display_title(&once, 400), once);
    }

    #[test]
    fn fold_keeps_text_on_one_line_without_invisible_characters() {
        assert_eq!(
            fold("a\n\nb\u{202e}c\u{7}d  [x](y)"),
            "a bc d [x](y)".to_owned()
        );
    }

    #[test]
    fn quotes_cannot_be_closed_or_broken_by_their_content() {
        let quoted_text = quote(HOSTILE);
        assert!(quoted_text.starts_with('"') && quoted_text.ends_with('"'));
        let inner = &quoted_text[1..quoted_text.len() - 1];
        assert!(!inner.contains('\n') && !inner.contains('\r'));
        assert!(inner.contains("\\u{202e}") && inner.contains("\\u{7}"));
        assert_eq!(quote(r#"say "hi" \ bye"#), r#""say \"hi\" \\ bye""#);
        // Every unescaped quote is a delimiter.
        let unescaped = inner
            .char_indices()
            .filter(|(i, c)| *c == '"' && !inner[..*i].ends_with('\\'))
            .count();
        assert_eq!(unescaped, 0);
        assert_eq!(
            quoted("title", AUTHORED_DATA, "x"),
            "title [authored_data]: \"x\""
        );
    }

    #[test]
    fn terminal_output_has_no_literal_controls_or_bidi() {
        let json =
            serde_json::to_string(&serde_json::json!({"t": HOSTILE, "e": "\u{e0041}"})).unwrap();
        let shown = escape_for_terminal(&json);
        assert!(!shown.chars().any(|c| is_unsafe(c) && c != '\n'));
        let back: serde_json::Value = serde_json::from_str(&shown).unwrap();
        assert_eq!(back["t"], HOSTILE);
        assert_eq!(back["e"], "\u{e0041}");
        assert!(matches!(
            escape_for_terminal("plain\n\ttext"),
            Cow::Borrowed(_)
        ));
    }
}
