//! Elision of absolute local paths from displayed text.
//!
//! Records written before ingest relativised its labels may carry an absolute
//! local path inside an identity or title (for example `spec / /tmp/x/spec.txt`).
//! Views and the renderer never print one: they replace it with a marker.

use std::borrow::Cow;

/// The marker shown where an absolute local path was withheld.
pub const ELIDED: &str = "[local path elided]";

/// Absolute path roots under which files of a person, a project, a build or
/// a mounted volume live. A path is recognised by its root, not by its shape:
/// an absolute path and a JSON pointer (`/roots/0`) or a URL path
/// (`/api/users`) look the same, so a shape rule would either miss paths or
/// elide pointers in messages. A root names a whole first segment, with its
/// slash. `/dev/` itself is not a root: `/dev/null` and the other device
/// names are not local files, but files live under `/dev/shm/`.
const ROOTS: &[&str] = &[
    // Homes, temporary and variable data, mounts (Unix and Linux).
    "/home/",
    "/root/",
    "/tmp/",
    "/var/",
    "/run/",
    "/dev/shm/",
    "/mnt/",
    "/media/",
    "/srv/",
    "/opt/",
    "/etc/",
    "/usr/",
    "/data/",
    "/snap/",
    // macOS: homes, the private tree, external and system data volumes.
    "/Users/",
    "/private/",
    "/Volumes/",
    "/System/Volumes/",
    // Package stores.
    "/nix/",
    "/gnu/",
    // Shared and cluster file systems.
    "/scratch/",
    "/gpfs/",
    "/lustre/",
    "/afs/",
    "/net/",
    // Container, dev-container and CI checkouts.
    "/workspace/",
    "/workspaces/",
    "/builds/",
    // Windows drives seen from Cygwin.
    "/cygdrive/",
];

fn is_local_path(token: &str) -> bool {
    let bytes = token.as_bytes();
    ROOTS.iter().any(|root| token.starts_with(root))
        // A drive path, `C:\x` or `C:/x`.
        || (bytes.len() > 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
        // A UNC path, `\\server\share`.
        || token.starts_with("\\\\")
        // A `file:` URL always names a local or shared file.
        || bytes
            .get(..6)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case(b"file:/"))
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

    /// One path under each root, as a legacy identity would hold it, inside
    /// punctuation; every one is elided, and the marker is stable.
    #[test]
    fn every_root_is_elided() {
        let cases = [
            "/home/ana/spec.txt",
            "/root/spec.txt",
            "/tmp/x/spec.txt",
            "/var/folders/ab/T/spec.txt",
            "/run/user/1000/inv-tests/spec.txt",
            "/dev/shm/x/legacy.txt",
            "/mnt/c/Users/ana/spec.txt",
            "/media/ana/usb/spec.txt",
            "/srv/cases/spec.txt",
            "/opt/data/spec.txt",
            "/etc/investigation/spec.txt",
            "/usr/local/share/spec.txt",
            "/data/cases/spec.txt",
            "/snap/tool/1/spec.txt",
            "/Users/ana/spec.txt",
            "/private/var/folders/spec.txt",
            "/Volumes/External/cases/spec.txt",
            "/System/Volumes/Data/Users/ana/spec.txt",
            "/nix/store/abc-spec/spec.txt",
            "/gnu/store/abc-spec/spec.txt",
            "/scratch/ana/spec.txt",
            "/gpfs/projects/a/spec.txt",
            "/lustre/fs1/ana/spec.txt",
            "/afs/cell/user/a/spec.txt",
            "/net/host/export/spec.txt",
            "/workspace/repo/spec.txt",
            "/workspaces/repo/spec.txt",
            "/builds/group/repo/spec.txt",
            "/cygdrive/c/Users/ana/spec.txt",
            r"C:\Users\ana\spec.txt",
            "D:/cases/spec.txt",
            r"\\server\share\spec.txt",
            "file:///home/ana/spec.txt",
            "FILE://server/share/spec.txt",
        ];
        for path in cases {
            assert_eq!(
                elide_local_paths(&format!("spec / ({path}).")),
                format!("spec / ({ELIDED})."),
                "{path}"
            );
            assert_eq!(elide_local_paths(ELIDED), ELIDED);
        }
        // Every listed root has a case above.
        for root in ROOTS {
            assert!(cases.iter().any(|path| path.starts_with(root)), "{root}");
        }
    }

    /// JSON pointers in messages, URL paths, ratios and device names are not
    /// local files and stay as they are.
    #[test]
    fn non_paths_stay_unchanged() {
        for text in [
            "/roots/0",
            "Set /option_ref to an eligible option.",
            "/operations/0/payload/preconditions",
            "path \"/basis/artifact_ref\" and /evidence_refs/2",
            "GET /api/users/7 returned 404",
            "https://example.invalid/home/ana/x and http://h/tmp/y",
            "a/b, 3/4 and and/or",
            "--- /dev/null\n+++ F\n",
            "--- F\n+++ /dev/null\n",
            "/dev/stdin /dev/stdout /dev/zero /dev/shm",
            "/homeward/x /tmpfile /Volumes",
            "see file: line 4, file:42",
            "notes/spec.txt and ./home/x and ../tmp/y",
        ] {
            assert_eq!(elide_local_paths(text), text);
        }
    }
}
