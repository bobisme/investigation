//! Flat YAML frontmatter. Pure.
//!
//! Obsidian property types are vault-wide and its property editor does not
//! model nested objects, so every property is a flat scalar or a list of
//! scalars. Nested metadata belongs in Markdown tables in the note body.
//!
//! Every string, including every link, is emitted as a YAML double-quoted
//! scalar. Characters outside YAML's printable set, line breaks, bidirectional
//! controls and other invisible formatting characters are escaped, so no value
//! can end the frontmatter block, start a new key, or change how its own text
//! displays in a source view. Integers, booleans and validated UTC timestamps
//! are the only plain scalars.

use std::{collections::BTreeSet, fmt::Write as _};

use investigation_core::header::UtcTimestamp;

use crate::{PlanPath, layout};

/// Builds one frontmatter block in insertion order.
#[derive(Debug, Default)]
pub struct Frontmatter {
    out: String,
    keys: BTreeSet<&'static str>,
}

impl Frontmatter {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(&mut self, key: &'static str) {
        // Keys are code constants; a bad or repeated key is a programming error.
        assert!(valid_key(key), "invalid frontmatter key {key:?}");
        assert!(self.keys.insert(key), "repeated frontmatter key {key:?}");
        self.out.push_str(key);
        self.out.push(':');
    }

    /// A string property, always double-quoted.
    pub fn text(&mut self, key: &'static str, value: &str) -> &mut Self {
        self.key(key);
        self.out.push(' ');
        quoted(&mut self.out, value);
        self.out.push('\n');
        self
    }

    pub fn integer(&mut self, key: &'static str, value: u64) -> &mut Self {
        self.key(key);
        let _ = writeln!(self.out, " {value}");
        self
    }

    pub fn boolean(&mut self, key: &'static str, value: bool) -> &mut Self {
        self.key(key);
        let _ = writeln!(self.out, " {value}");
        self
    }

    /// A date-time property. Plain when the text is a strict UTC timestamp,
    /// so Obsidian can type it as a date; quoted otherwise.
    pub fn timestamp(&mut self, key: &'static str, value: &UtcTimestamp) -> &mut Self {
        let text = value.to_string();
        if !text.is_empty()
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b':' | b'.' | b'T' | b'Z'))
        {
            self.key(key);
            let _ = writeln!(self.out, " {text}");
            self
        } else {
            self.text(key, &text)
        }
    }

    /// An internal link to a validated plan path, quoted as Obsidian requires.
    pub fn link(&mut self, key: &'static str, path: &PlanPath) -> &mut Self {
        let link = layout::link_target(path);
        self.text(key, &format!("[[{link}]]"))
    }

    /// A list of strings, each double-quoted. An empty list is `[]`.
    pub fn list<S: AsRef<str>>(&mut self, key: &'static str, items: &[S]) -> &mut Self {
        self.key(key);
        if items.is_empty() {
            self.out.push_str(" []\n");
            return self;
        }
        self.out.push('\n');
        for item in items {
            self.out.push_str("  - ");
            quoted(&mut self.out, item.as_ref());
            self.out.push('\n');
        }
        self
    }

    /// The complete block, including both `---` delimiters.
    pub fn finish(&self) -> String {
        format!("---\n{}---\n", self.out)
    }
}

/// Lowercase ASCII letters, digits and underscores, starting with a letter.
pub fn valid_key(key: &str) -> bool {
    key.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Write `value` as a YAML double-quoted scalar.
pub fn quoted(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{85}' => out.push_str("\\N"),
            '\u{2028}' => out.push_str("\\L"),
            '\u{2029}' => out.push_str("\\P"),
            c if needs_escape(c) => {
                let code = u32::from(c);
                if code <= 0xff {
                    let _ = write!(out, "\\x{code:02X}");
                } else if code <= 0xffff {
                    let _ = write!(out, "\\u{code:04X}");
                } else {
                    let _ = write!(out, "\\U{code:08X}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Characters a double-quoted scalar must not carry literally: those outside
/// YAML's printable set, plus invisible formatting and bidirectional controls
/// that could make a value read differently from its bytes.
fn needs_escape(c: char) -> bool {
    c.is_control() || crate::text::is_invisible_format(c) || matches!(c, '\u{fffe}' | '\u{ffff}')
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Values chosen to break naive YAML, Markdown or Obsidian handling.
    pub(crate) const HOSTILE: &[&str] = &[
        "",
        "plain",
        "]] | [[Other]] ---",
        "line one\nline two",
        "---\ninv_id: forged\n---",
        "\"quoted\" and 'single' and \\back\\slash",
        "key: value",
        "- list item",
        "# heading #tag",
        "{flow: map}",
        "[flow, seq]",
        "&anchor *alias !tag %directive @at `tick`",
        "yes",
        "null",
        "~",
        "0x1F",
        "123",
        "2026-11-21T22:00:00Z",
        "nul\0 escape\u{1b}[31m bell\u{7} del\u{7f} c1\u{9b}",
        "separators\u{2028}\u{2029}\u{85}",
        "bidi \u{202e}evil\u{202c} \u{2066}iso\u{2069} zw\u{200b}j\u{200d} bom\u{feff}",
        "noncharacters \u{fffe}\u{ffff}",
        "unicode: Queue A \u{2014} p99 \u{1f600} \u{65e5}\u{672c}",
        "trailing space ",
        " leading space",
        "<script>alert(1)</script> ![[secret.png]] ![x](https://example.invalid/x.png)",
        "\r\n\tcontrol whitespace",
    ];

    fn parse(block: &str) -> yaml_serde::Mapping {
        let inner = block
            .strip_prefix("---\n")
            .and_then(|rest| rest.strip_suffix("---\n"))
            .expect("delimited block");
        yaml_serde::from_str(inner).expect("frontmatter parses as YAML")
    }

    #[test]
    fn hostile_strings_round_trip_as_single_flat_scalars() {
        for value in HOSTILE {
            let mut front = Frontmatter::new();
            front.text("inv_title", value).integer("inv_revision", 3);
            front.list("aliases", &[*value, "second"]);
            let block = front.finish();
            // The block has exactly the lines we wrote: nothing escaped into a new line.
            assert_eq!(block.lines().count(), 7, "{block}");
            assert!(
                !block
                    .chars()
                    .any(|c| c != '\n' && (c.is_control() || crate::text::is_invisible_format(c))),
                "{block:?}"
            );
            let map = parse(&block);
            assert_eq!(map.len(), 3);
            assert_eq!(map["inv_title"], yaml_serde::Value::String((*value).into()));
            assert_eq!(map["inv_revision"], yaml_serde::Value::Number(3.into()));
            let aliases = map["aliases"].as_sequence().unwrap();
            assert_eq!(aliases[0], yaml_serde::Value::String((*value).into()));
        }
    }

    #[test]
    fn links_lists_timestamps_and_booleans_are_flat() {
        let path = PlanPath::new("Cases/case_x/Overview.md").unwrap();
        let time: UtcTimestamp = "2026-11-21T22:00:00.000Z".parse().unwrap();
        let mut front = Frontmatter::new();
        front
            .link("inv_case", &path)
            .timestamp("inv_updated", &time)
            .boolean("inv_withheld", true)
            .list::<&str>("aliases", &[])
            .list("tags", &["investigation/question"]);
        let block = front.finish();
        assert!(block.contains("inv_case: \"[[Cases/case_x/Overview]]\"\n"));
        assert!(block.contains("inv_updated: 2026-11-21T22:00:00.000Z\n"));
        assert!(block.contains("aliases: []\n"));
        let map = parse(&block);
        assert_eq!(map["inv_case"], "[[Cases/case_x/Overview]]");
        assert_eq!(map["inv_updated"], "2026-11-21T22:00:00.000Z");
        assert_eq!(map["inv_withheld"], true);
        assert!(map["aliases"].as_sequence().unwrap().is_empty());
        assert_eq!(map["tags"][0], "investigation/question");
    }

    #[test]
    #[should_panic(expected = "repeated frontmatter key")]
    fn repeated_keys_are_refused() {
        Frontmatter::new()
            .integer("inv_revision", 1)
            .integer("inv_revision", 2);
    }

    #[test]
    fn keys_are_restricted() {
        assert!(valid_key("inv_render_sequence"));
        assert!(valid_key("aliases"));
        for bad in ["", "Inv", "1a", "a-b", "a b", "a:b", "_a"] {
            assert!(!valid_key(bad), "{bad:?}");
        }
    }
}
