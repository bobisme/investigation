//! Where exposed text lives in a workspace: a read-only diagnosis (plan §16.5).
//!
//! A secret committed in event text cannot be taken back by a later event:
//! the journal is append-only and hash chained, and a redaction or withdrawal
//! adds history without removing the old bytes. Until a purge workflow exists
//! (see `docs/deferred.md`), the tool diagnoses the exposure and documents the
//! manual remediation (`investigation docs exposure`).
//!
//! This module names every workspace area by its path ([`Area`]) and searches
//! every regular file under the workspace root for the exposed bytes
//! ([`scan`]). It never writes, never follows a symbolic link, and opens no
//! file the caller lists as not to be opened: the caller passes the retained
//! blobs of Artifacts whose effective classification hides their content, so
//! the diagnosis keeps the read-path rule that restricted bytes are not
//! opened unless explicitly requested. Matches are reported by path only;
//! the searched bytes and their surroundings are never returned.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{ErrorKind, Read},
    path::{Component, Path, PathBuf},
};

use super::{BLOBS, CASES, capture::STAGING_ROOT};
use crate::journal::{EVENTS_DIRECTORY, GENESIS_FILENAME, event_filename};
use investigation_core::identity::Id;

/// Fewest bytes of exposed text the diagnosis searches for. Shorter text
/// matches too much unrelated data to say anything.
pub const MIN_TEXT_BYTES: usize = 4;
/// Most bytes of exposed text the diagnosis searches for.
pub const MAX_TEXT_BYTES: usize = 4096;

const READ_CHUNK: usize = 64 * 1024;

/// What a workspace path is, decided by its location alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Area {
    JournalEvent,
    CaseGenesis,
    Blob,
    Staging,
    Temporary,
    Checkpoint,
    WorkspaceState,
    Index,
    BlobVerdict,
    Cache,
    VaultNote,
    VaultConflictCopy,
    VaultTemporary,
    Vault,
    RenderConflict,
    RenderManifest,
    SessionReads,
    Local,
    Export,
    Config,
    Other,
}

impl Area {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JournalEvent => "journal_event",
            Self::CaseGenesis => "case_genesis",
            Self::Blob => "blob",
            Self::Staging => "staging",
            Self::Temporary => "temporary",
            Self::Checkpoint => "checkpoint",
            Self::WorkspaceState => "workspace_state",
            Self::Index => "index",
            Self::BlobVerdict => "blob_verdict",
            Self::Cache => "cache",
            Self::VaultNote => "vault_note",
            Self::VaultConflictCopy => "vault_conflict_copy",
            Self::VaultTemporary => "vault_temporary",
            Self::Vault => "vault",
            Self::RenderConflict => "render_conflict",
            Self::RenderManifest => "render_manifest",
            Self::SessionReads => "session_reads",
            Self::Local => "local",
            Self::Export => "export",
            Self::Config => "config",
            Self::Other => "other",
        }
    }

    /// Whether the area is canonical: removing or editing it changes what
    /// `check` verifies.
    pub const fn canonical(self) -> bool {
        matches!(
            self,
            Self::JournalEvent
                | Self::CaseGenesis
                | Self::Blob
                | Self::Checkpoint
                | Self::WorkspaceState
        )
    }

    /// What holding the text here means, and what removing it would take.
    /// Nothing here removes anything; see `investigation docs exposure`.
    pub const fn remediation(self) -> &'static str {
        match self {
            Self::JournalEvent => {
                "Canonical and append-only. A hash-chained event cannot be edited or removed without failing check; a later redaction, withdrawal or classification event leaves these bytes in place. Only the restore or fork procedures in the exposure docs leave it behind."
            }
            Self::CaseGenesis => {
                "Canonical start of a case chain; it cannot change without failing check. Only the restore or fork procedures leave it behind."
            }
            Self::Blob => {
                "Canonical retained bytes, shared by every Artifact with this digest in any case. Deleting or rewriting the file makes those Artifacts fail check; withdrawal and classification change use and display, not the bytes."
            }
            Self::Staging => {
                "Capture staging is recovery material, not cache. repair staging retires it only after its Run is finalized and its bytes are canonical."
            }
            Self::Temporary => {
                "Leftover of an interrupted write under state/tmp. It is not canonical; remove it only while no command runs."
            }
            Self::Checkpoint => {
                "Canonical checkpoint data; it cannot change without failing check."
            }
            Self::WorkspaceState => {
                "Canonical workspace identity or configuration state; it cannot change without failing check."
            }
            Self::Index => {
                "Disposable projection of the journal, including search text. repair index rebuilds it from the journal and so puts back any text the journal still holds. SQLite can keep removed rows in free pages and its write-ahead log until the file is replaced."
            }
            Self::BlobVerdict => {
                "Disposable cache entry named by a blob digest; it holds the digest and file identity, not the bytes. Deleting it only costs a hash."
            }
            Self::Cache => "Disposable cache; deleting it only costs recomputation.",
            Self::VaultNote => {
                "Generated note. A render after classification.raise replaces it with a restricted stub; copies a viewer, sync client or backup made are not replaced."
            }
            Self::VaultConflictCopy => {
                "A sync client's conflict copy beside a generated note. A render never removes or rewrites it."
            }
            Self::VaultTemporary => {
                "Hidden render temporary left by an interrupted render; the next render sweeps it when its bytes are proven generated."
            }
            Self::Vault => {
                "A vault file the renderer does not own, such as a human note; edit or remove it by hand."
            }
            Self::RenderConflict => {
                "A generated version withheld because the vault file had drifted. A render never removes it; remove it by hand after reconciling the drift."
            }
            Self::RenderManifest => "Render record of paths and hashes; it holds no note text.",
            Self::SessionReads => "Machine-local record of evidence reads for a session label.",
            Self::Local => "Machine-local file, not canonical.",
            Self::Export => {
                "A file written for use outside the workspace. Copies already sent on cannot be recalled automatically."
            }
            Self::Config => "Workspace configuration or pointer file; edit it by hand.",
            Self::Other => "A file outside the workspace layout; inspect it by hand.",
        }
    }
}

/// The area of `relative`, a path below the workspace root. Vault notes are
/// reported as [`Area::Vault`] unless they are hidden render temporaries; the
/// caller refines them with the render record.
pub fn area(relative: &Path) -> Area {
    let parts: Vec<&str> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    let name = parts.last().copied().unwrap_or("");
    match parts.as_slice() {
        ["state", "cases", _, events, _, ..] if *events == EVENTS_DIRECTORY => Area::JournalEvent,
        ["state", "cases", _, genesis] if *genesis == GENESIS_FILENAME => Area::CaseGenesis,
        ["state", "events", ..] => Area::JournalEvent,
        ["state", "blobs", ..] => Area::Blob,
        ["state", "staging", ..] => Area::Staging,
        ["state", "tmp", ..] => Area::Temporary,
        ["state", "checkpoints", ..] => Area::Checkpoint,
        ["state", ..] => Area::WorkspaceState,
        ["cache", index, ..] if index.starts_with("index.sqlite") => Area::Index,
        ["cache", "blob-verdicts", ..] => Area::BlobVerdict,
        ["cache", ..] => Area::Cache,
        ["vault", ..] if name.ends_with(".inv-tmp") || name.ends_with(".inv-del") => {
            Area::VaultTemporary
        }
        ["vault", ..] => Area::Vault,
        ["local", "conflicts", ..] => Area::RenderConflict,
        ["local", "render-manifest.json"] => Area::RenderManifest,
        ["local", "session-reads", ..] => Area::SessionReads,
        ["local", ..] => Area::Local,
        ["exports", ..] => Area::Export,
        ["investigation.toml" | "README.txt"] => Area::Config,
        _ => Area::Other,
    }
}

/// Path of the recorded verdict on the blob with this hex digest.
pub fn verdict_path(hex: &str) -> PathBuf {
    Path::new("cache/blob-verdicts").join(hex)
}

/// Path of a case's journal event.
pub fn event_path(case_id: Id, sequence: u64) -> Option<PathBuf> {
    let name = event_filename(sequence).ok()?;
    Some(
        Path::new(CASES)
            .join(case_id.to_string())
            .join(EVENTS_DIRECTORY)
            .join(name),
    )
}

/// Every file below the retained-blob and capture-staging roots whose name
/// is a SHA-256 hex digest, with that digest: the retained blobs and any
/// staged copy of the same bytes.
pub fn digest_named_files(root: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    for top in [BLOBS, STAGING_ROOT] {
        walk(root, Path::new(top), &mut |relative, kind| {
            let name = relative.file_name().and_then(|name| name.to_str());
            if kind == Entry::File
                && let Some(name) = name
                && name.len() == 64
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                found.push((relative.to_owned(), name.to_owned()));
            }
        });
    }
    found
}

/// One searched form of the exposed text, such as its JSON-escaped form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub label: &'static str,
    pub bytes: Vec<u8>,
}

/// A file that holds at least one form of the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    pub area: Area,
    /// Labels of the forms found in the file's bytes.
    pub forms: Vec<&'static str>,
    /// Whether the path itself holds a form of the text.
    pub in_name: bool,
}

/// A path the scan did not search, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unopened {
    pub path: PathBuf,
    pub area: Area,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub hits: Vec<Hit>,
    pub unopened: Vec<Unopened>,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    File,
    Link,
    Special,
    Unreadable,
}

/// Visit every entry below `root/start` in name order without following
/// symbolic links. Directories are entered; everything else is reported.
fn walk(root: &Path, start: &Path, visit: &mut dyn FnMut(&Path, Entry)) {
    let Ok(entries) = fs::read_dir(root.join(start)) else {
        if root.join(start).exists() {
            visit(start, Entry::Unreadable);
        }
        return;
    };
    let mut names: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    names.sort();
    for name in names {
        let relative = start.join(&name);
        match fs::symlink_metadata(root.join(&relative)) {
            Ok(meta) if meta.is_dir() => walk(root, &relative, visit),
            Ok(meta) if meta.is_file() => visit(&relative, Entry::File),
            Ok(meta) if meta.file_type().is_symlink() => visit(&relative, Entry::Link),
            Ok(_) => visit(&relative, Entry::Special),
            Err(_) => visit(&relative, Entry::Unreadable),
        }
    }
}

/// Search every regular file below `root` for each form. `skip` lists
/// relative paths that must not be opened, with the reason reported for
/// each. Nothing is written and no link is followed.
pub fn scan(root: &Path, forms: &[Form], skip: &BTreeMap<PathBuf, String>) -> ScanReport {
    let mut report = ScanReport::default();
    let forms: Vec<&Form> = forms.iter().filter(|form| !form.bytes.is_empty()).collect();
    walk(root, Path::new(""), &mut |relative, entry| {
        let area = area(relative);
        let in_name = {
            let name = relative.as_os_str().as_encoded_bytes();
            forms.iter().any(|form| contains(name, &form.bytes))
        };
        let unopened = |reason: &str| Unopened {
            path: relative.to_owned(),
            area,
            reason: reason.to_owned(),
        };
        match entry {
            Entry::Link => report
                .unopened
                .push(unopened("a symbolic link; not followed")),
            Entry::Special => report.unopened.push(unopened("not a regular file")),
            Entry::Unreadable => report.unopened.push(unopened("could not be read")),
            Entry::File => {
                if let Some(reason) = skip.get(relative) {
                    report.unopened.push(unopened(reason));
                } else {
                    match search_file(&root.join(relative), &forms) {
                        Ok((found, bytes)) => {
                            report.files += 1;
                            report.bytes += bytes;
                            if !found.is_empty() || in_name {
                                report.hits.push(Hit {
                                    path: relative.to_owned(),
                                    area,
                                    forms: found,
                                    in_name,
                                });
                            }
                            return;
                        }
                        Err(_) => report.unopened.push(unopened("could not be read")),
                    }
                }
            }
        }
        if in_name {
            report.hits.push(Hit {
                path: relative.to_owned(),
                area,
                forms: Vec::new(),
                in_name,
            });
        }
    });
    report
}

/// The labels of `forms` found in the file, and the bytes read.
fn search_file(path: &Path, forms: &[&Form]) -> std::io::Result<(Vec<&'static str>, u64)> {
    let mut file = File::open(path)?;
    let longest = forms.iter().map(|form| form.bytes.len()).max().unwrap_or(1);
    let mut found = vec![false; forms.len()];
    let mut window: Vec<u8> = Vec::with_capacity(READ_CHUNK + longest);
    let mut chunk = vec![0; READ_CHUNK];
    let mut total = 0u64;
    loop {
        let read = match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        total += read as u64;
        window.extend_from_slice(&chunk[..read]);
        for (index, form) in forms.iter().enumerate() {
            if !found[index] && contains(&window, &form.bytes) {
                found[index] = true;
            }
        }
        if found.iter().all(|done| *done) {
            break;
        }
        // Keep just enough of the tail for a match across chunk boundaries.
        let keep = longest.saturating_sub(1).min(window.len());
        window.drain(..window.len() - keep);
    }
    let labels = forms
        .iter()
        .zip(found)
        .filter_map(|(form, found)| found.then_some(form.label))
        .collect();
    Ok((labels, total))
}

/// Whether `needle` occurs in `haystack`.
pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return false;
    };
    if haystack.len() < needle.len() {
        return false;
    }
    haystack[..=haystack.len() - needle.len()]
        .iter()
        .enumerate()
        .any(|(at, &byte)| byte == first && haystack[at + 1..at + needle.len()] == *rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn areas_follow_the_workspace_layout() {
        let cases = [
            (
                "state/cases/case_x/events/000000000002.json",
                Area::JournalEvent,
            ),
            ("state/cases/case_x/genesis.json", Area::CaseGenesis),
            ("state/cases/case_x/commit.lock", Area::WorkspaceState),
            ("state/blobs/sha256/ab/abcd", Area::Blob),
            ("state/staging/run_x/blobs/abcd", Area::Staging),
            ("state/tmp/x", Area::Temporary),
            ("cache/index.sqlite", Area::Index),
            ("cache/index.sqlite-wal", Area::Index),
            ("cache/blob-verdicts/abcd", Area::BlobVerdict),
            ("vault/Cases/c/Overview.md", Area::Vault),
            (
                "vault/Cases/c/.Overview.md.r-1.inv-del",
                Area::VaultTemporary,
            ),
            ("local/conflicts/gen_x/Cases/c/o.md", Area::RenderConflict),
            ("local/render-manifest.json", Area::RenderManifest),
            ("exports/bundle.tar", Area::Export),
            ("investigation.toml", Area::Config),
            ("stray.txt", Area::Other),
        ];
        for (path, expected) in cases {
            assert_eq!(area(Path::new(path)), expected, "{path}");
        }
        assert!(Area::JournalEvent.canonical() && !Area::Index.canonical());
    }

    #[test]
    fn a_match_across_a_chunk_boundary_is_found_and_nothing_is_echoed() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let needle = b"SECRET-token-0123".to_vec();
        let mut bytes = vec![b'x'; READ_CHUNK - 5];
        bytes.extend_from_slice(&needle);
        bytes.extend_from_slice(b"tail");
        fs::create_dir_all(root.join("state/blobs/sha256/ab")).unwrap();
        fs::write(root.join("state/blobs/sha256/ab/abcd"), &bytes).unwrap();
        fs::write(root.join("state/blobs/sha256/ab/abce"), b"clean").unwrap();
        fs::create_dir_all(root.join("exports")).unwrap();
        fs::write(root.join("exports/held"), &needle).unwrap();
        std::os::unix::fs::symlink(root.join("exports/held"), root.join("exports/link")).unwrap();
        let forms = [Form {
            label: "raw",
            bytes: needle,
        }];
        let skip = BTreeMap::from([(
            PathBuf::from("exports/held"),
            "listed as not to be opened".to_owned(),
        )]);
        let report = scan(root, &forms, &skip);
        assert_eq!(report.hits.len(), 1);
        assert_eq!(report.hits[0].path, Path::new("state/blobs/sha256/ab/abcd"));
        assert_eq!(report.hits[0].forms, ["raw"]);
        let unopened: Vec<_> = report.unopened.iter().map(|u| u.path.clone()).collect();
        assert_eq!(
            unopened,
            [PathBuf::from("exports/held"), PathBuf::from("exports/link")]
        );
        assert_eq!(report.files, 2);
    }

    #[test]
    fn contains_handles_edges() {
        assert!(contains(b"abc", b"abc"));
        assert!(contains(b"xxabc", b"abc"));
        assert!(!contains(b"ab", b"abc"));
        assert!(!contains(b"abc", b""));
    }
}
