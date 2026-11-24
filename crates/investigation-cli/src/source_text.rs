//! Read a retained source's text through `evidence_read`, whole or in bounded
//! chunks (bn-2yy4).
//!
//! One evidence read returns at most `MAX_EXCERPT_BYTES`. A larger text
//! source is read as consecutive byte ranges, each through
//! `Store::evidence_read`, so every chunk passes the same restriction,
//! retention, withdrawal and digest checks as any read. A chunk that would
//! end inside a UTF-8 character is shortened until it ends on one, so the
//! chunks decode exactly when the whole would: valid UTF-8 without NUL.

use investigation_core::{
    PROGRAM_NAME,
    evidence::{ArtifactBody, SelectorKind, SelectorRequest, SelectorVersion},
    identity::Id,
    state::Record,
    wire::Digest,
};
use investigation_store::{
    Store,
    journal::CaseReplay,
    store::{
        evidence_read::{Excerpt, MAX_EXCERPT_BYTES},
        ingest::TEXT_PARSER_VERSION,
    },
};

/// A UTF-8 character has at most four bytes, so a chunk end moves back at
/// most three bytes to reach a character boundary.
const MAX_BOUNDARY_SHIFT: usize = 3;

/// The recorded body of an Artifact, when it decodes.
pub(crate) fn artifact_body(record: &Record) -> Option<ArtifactBody> {
    serde_json::from_value(record.body.clone()).ok()
}

/// The recorded size of an Artifact's bytes.
pub(crate) fn size_bytes(record: &Record) -> Option<u64> {
    artifact_body(record)?.size_bytes
}

/// A selector over the Artifact's recorded digest.
pub(crate) fn selector(record: &Record, selector: SelectorKind) -> SelectorRequest {
    let digest = artifact_body(record)
        .and_then(|body| body.digest)
        // An Artifact with no recorded digest holds no retained bytes:
        // evidence_read refuses it (source_unavailable, or no digest) before
        // it compares this placeholder with anything.
        .unwrap_or_else(|| {
            format!("sha256:{}", "0".repeat(64))
                .parse()
                .expect("placeholder digest parses")
        });
    request(digest, selector)
}

fn request(artifact_digest: Digest, selector: SelectorKind) -> SelectorRequest {
    SelectorRequest {
        schema_version: SelectorVersion::V1,
        artifact_digest,
        selector,
    }
}

/// A `text_lines` selector over the Artifact's text: its own, or for an
/// original its recorded text derivative's, which `evidence_read` resolves.
pub(crate) fn lines(record: &Record, start_line: u64, end_line: u64) -> SelectorRequest {
    let parser_version = artifact_body(record)
        .and_then(|body| {
            body.transforms
                .into_iter()
                .find(|transform| transform.transformation == "decode_utf8")
                .map(|transform| transform.version)
        })
        .unwrap_or_else(|| TEXT_PARSER_VERSION.to_owned());
    selector(
        record,
        SelectorKind::TextLines {
            start_line,
            end_line,
            encoding: "utf-8".into(),
            parser_version,
        },
    )
}

/// The outcome of reading a source's text.
#[derive(Debug)]
pub(crate) enum SourceText {
    Read(String),
    /// The source holds more bytes than the caller reads.
    TooLarge {
        size: u64,
    },
    /// Restricted, not retained, withdrawn, not text, or the digest differs.
    Unreadable,
}

/// The decoded text of `record`'s bytes, read through `evidence_read` with
/// `digest` as the selector digest: whole when it fits one excerpt, else in
/// chunks while the source holds at most `max_bytes`.
pub(crate) fn read(
    store: &Store,
    replay: &CaseReplay,
    record: &Record,
    digest: &Digest,
    max_bytes: u64,
) -> SourceText {
    let Some(size) = size_bytes(record) else {
        return SourceText::Unreadable;
    };
    let read = |selector: SelectorKind| {
        store
            .evidence_read(replay, record, request(digest.clone(), selector))
            .ok()
            .map(|read| read.excerpt)
    };
    if size <= MAX_EXCERPT_BYTES as u64 {
        return match read(SelectorKind::Whole) {
            Some(Excerpt::Text { text, .. }) => SourceText::Read(text),
            _ => SourceText::Unreadable,
        };
    }
    if size > max_bytes {
        return SourceText::TooLarge { size };
    }
    let Ok(size) = usize::try_from(size) else {
        return SourceText::TooLarge { size };
    };
    let mut text = String::with_capacity(size);
    let mut start = 0;
    while start < size {
        let Some((piece, end)) = chunk(&read, start, size) else {
            return SourceText::Unreadable;
        };
        text.push_str(&piece);
        start = end;
    }
    SourceText::Read(text)
}

/// The text of the bytes from `start`, at most one excerpt long, ending on a
/// character boundary; with the byte offset where it ends.
fn chunk(
    read: &impl Fn(SelectorKind) -> Option<Excerpt>,
    start: usize,
    size: usize,
) -> Option<(String, usize)> {
    let full = (start + MAX_EXCERPT_BYTES).min(size);
    for shift in 0..=MAX_BOUNDARY_SHIFT {
        let end = full.checked_sub(shift).filter(|end| *end > start)?;
        match read(SelectorKind::ByteRange {
            start: start as u64,
            end: end as u64,
        })? {
            Excerpt::Text { text, .. } => return Some((text, end)),
            // Only a chunk cut inside a character can decode when shortened.
            Excerpt::Binary { .. } if end < size => {}
            _ => return None,
        }
    }
    None
}

/// How many whole lines the first excerpt of a text source holds, so
/// `--lines 1:N` prints as much as one read can. `None` when the source
/// cannot be read here or its first line alone exceeds one excerpt.
pub(crate) fn lines_in_first_excerpt(
    store: &Store,
    replay: &CaseReplay,
    record: &Record,
) -> Option<u64> {
    let size = usize::try_from(size_bytes(record)?).ok()?;
    let digest = artifact_body(record)?.digest?;
    let read = |selector: SelectorKind| {
        store
            .evidence_read(replay, record, request(digest.clone(), selector))
            .ok()
            .map(|read| read.excerpt)
    };
    let (text, end) = chunk(&read, 0, size)?;
    let mut lines = text.matches('\n').count();
    if end == size && !text.ends_with('\n') && !text.is_empty() {
        lines += 1;
    }
    (lines > 0).then_some(lines as u64)
}

/// The `evidence read` call that prints `record`'s text, scoped to the
/// workspace and case: `--whole` when the source fits one read, else the
/// first lines one read holds. `None` when no such read can succeed here.
pub(crate) fn read_argv(
    store: &Store,
    replay: &CaseReplay,
    case: Id,
    record: &Record,
) -> Option<Vec<String>> {
    let range = if size_bytes(record)? <= MAX_EXCERPT_BYTES as u64 {
        vec!["--whole".to_owned()]
    } else {
        let lines = lines_in_first_excerpt(store, replay, record)?;
        vec!["--lines".to_owned(), format!("1:{lines}")]
    };
    Some(scoped_read(store, case, record, range))
}

/// The `evidence read --lines START:END` call for `record`, scoped to the
/// workspace and case.
pub(crate) fn lines_argv(
    store: &Store,
    case: Id,
    record: &Record,
    start: u64,
    end: u64,
) -> Vec<String> {
    scoped_read(
        store,
        case,
        record,
        vec!["--lines".to_owned(), format!("{start}:{end}")],
    )
}

fn scoped_read(store: &Store, case: Id, record: &Record, range: Vec<String>) -> Vec<String> {
    let mut argv = vec![
        PROGRAM_NAME.to_owned(),
        "--case".to_owned(),
        case.to_string(),
        "evidence".to_owned(),
        "read".to_owned(),
        record.header.id.to_string(),
    ];
    argv.extend(range);
    argv.extend(["--format".to_owned(), "json".to_owned()]);
    crate::scope_argv(store.workspace(), &mut argv);
    argv
}
