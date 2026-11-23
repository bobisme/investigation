//! Elision of absolute local paths from displayed text.
//!
//! Records written before ingest relativised its labels may carry an absolute
//! local path inside an identity or title (for example `spec / /tmp/x/spec.txt`).
//! Views and the renderer never print one: they replace it with a marker.

use std::borrow::Cow;

/// The marker shown where an absolute local path was withheld.
pub const ELIDED: &str = "[local path elided]";

const ROOTS: &[&str] = &[
    "/tmp/",
    "/home/",
    "/Users/",
    "/var/",
    "/private/",
    "/mnt/",
    "/root/",
    "/opt/",
    "/srv/",
    "/media/",
    "/run/",
    "/etc/",
    "/usr/",
    "/data/",
];

fn is_local_path(token: &str) -> bool {
    let bytes = token.as_bytes();
    ROOTS.iter().any(|root| token.starts_with(root))
        || (bytes.len() > 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
        || token.starts_with("\\\\")
}

/// `text` with each whitespace-delimited absolute local path replaced by
/// [`ELIDED`]. Text without one is returned unchanged.
#[must_use]
pub fn elide_local_paths(text: &str) -> Cow<'_, str> {
    if !text.contains('/') && !text.contains('\\') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut rest = text;
    while !rest.is_empty() {
        let space = rest.len() - rest.trim_start().len();
        out.push_str(&rest[..space]);
        rest = &rest[space..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..end];
        let lead = token.len() - token.trim_start_matches(['(', '\'', '"', '`', '[']).len();
        let core = token[lead..].trim_end_matches(['.', ',', ';', ':', ')', '\'', '"', '`', ']']);
        if is_local_path(core) {
            out.push_str(&token[..lead]);
            out.push_str(ELIDED);
            out.push_str(&token[lead + core.len()..]);
            changed = true;
        } else {
            out.push_str(token);
        }
        rest = &rest[end..];
    }
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elides_absolute_paths_only() {
        assert_eq!(
            elide_local_paths("spec / /tmp/x/spec.txt"),
            "spec / [local path elided]"
        );
        assert_eq!(
            elide_local_paths("see (/home/bob/a b)."),
            "see ([local path elided] b)."
        );
        assert_eq!(
            elide_local_paths("a / b and/or /api/users"),
            "a / b and/or /api/users"
        );
        assert_eq!(elide_local_paths(r"C:\Users\x\f.txt"), ELIDED);
        assert_eq!(elide_local_paths("notes/spec.txt"), "notes/spec.txt");
    }
}
