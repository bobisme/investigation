//! Every bound on bytes this build parses from outside, in one table.
//!
//! Each entry names the constant its parser enforces, so the table cannot
//! drift from the code. `doctor` reports [`LIMITS`] and [`EXPANSION`];
//! `docs/support-matrix.md` documents them, and a test keeps the document in
//! step with this table.
//!
//! Nothing here is executed or loaded. MIME types and file extensions are
//! recorded hints: no parser is chosen from them and no imported content is
//! run to identify it.
use investigation_core::{command, input, method_pack, producer_run, wire::ErrorCode};
use serde::Serialize;

use crate::{
    delimited, journal, opaque, selectors,
    store::{capture::HandleLimits, evidence_read, ingest},
    utf8,
};

/// What a limit counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Bytes,
    Items,
    /// Nesting levels, counted as JSON Pointer segments below the root.
    Levels,
    Characters,
}

/// One bound and what crossing it returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Limit {
    /// Stable dotted name, for reports and documentation.
    pub name: &'static str,
    pub value: u64,
    pub unit: Unit,
    /// The input the bound applies to.
    pub applies_to: &'static str,
    /// The error code an input over the bound receives. Selector bounds
    /// refuse the read or the observation that cites it. `None` for a bound
    /// on the work done, which no input can exceed.
    pub refusal: Option<ErrorCode>,
}

const fn limit(
    name: &'static str,
    value: u64,
    unit: Unit,
    applies_to: &'static str,
    refusal: Option<ErrorCode>,
) -> Limit {
    Limit {
        name,
        value,
        unit,
        applies_to,
        refusal,
    }
}

const INVALID_INPUT: Option<ErrorCode> = Some(ErrorCode::InvalidInput);
const INTEGRITY_FAILURE: Option<ErrorCode> = Some(ErrorCode::IntegrityFailure);
/// A bound on the work done rather than on the input.
const NO_REFUSAL: Option<ErrorCode> = None;
use Unit::{Bytes, Characters, Items, Levels};

/// The bounds, grouped by the input they protect.
pub const LIMITS: &[Limit] = &[
    limit(
        "request.bytes",
        input::MAX_REQUEST_BYTES as u64,
        Bytes,
        "A JSON request: command, ingest manifest, capture call or selector file.",
        INVALID_INPUT,
    ),
    limit(
        "request.depth",
        input::MAX_REQUEST_DEPTH as u64,
        Levels,
        "Nesting of a JSON request.",
        INVALID_INPUT,
    ),
    limit(
        "command.operations",
        command::MAX_OPERATIONS as u64,
        Items,
        "Operations in one command batch.",
        INVALID_INPUT,
    ),
    limit(
        "ingest.items",
        ingest::MAX_ITEMS as u64,
        Items,
        "Paths in one ingest manifest.",
        INVALID_INPUT,
    ),
    limit(
        "ingest.file_bytes",
        ingest::MAX_IMPORT_BYTES,
        Bytes,
        "One copied import: a local file or standard input. Checked before reading and again while copying.",
        INVALID_INPUT,
    ),
    limit(
        "ingest.reference_bytes",
        ingest::reference::MAX_REFERENCE_BYTES,
        Bytes,
        "One local file hashed in reference mode without copying.",
        INVALID_INPUT,
    ),
    limit(
        "ingest.derivation_inputs",
        ingest::derivation::MAX_INPUTS as u64,
        Items,
        "Input artifacts named by one derived artifact.",
        INVALID_INPUT,
    ),
    limit(
        "ingest.derivation_selectors",
        ingest::derivation::MAX_SELECTORS as u64,
        Items,
        "Selectors recorded for one derived artifact.",
        INVALID_INPUT,
    ),
    limit(
        "ingest.derivation_redactions",
        ingest::derivation::MAX_REDACTIONS as u64,
        Items,
        "Redaction decisions recorded for one derived artifact.",
        INVALID_INPUT,
    ),
    limit(
        "ingest.metadata_bytes",
        opaque::MAX_METADATA_BYTES as u64,
        Bytes,
        "Serialized opaque source metadata.",
        INVALID_INPUT,
    ),
    limit(
        "ingest.metadata_depth",
        opaque::MAX_METADATA_DEPTH as u64,
        Levels,
        "Nesting of one opaque metadata value.",
        INVALID_INPUT,
    ),
    limit(
        "ingest.metadata_key_chars",
        opaque::MAX_METADATA_KEY_CHARS as u64,
        Characters,
        "One opaque metadata key.",
        INVALID_INPUT,
    ),
    limit(
        "ingest.probe_bytes",
        opaque::MAX_PROBE_BYTES,
        Bytes,
        "Retained content probed for a producer envelope version. Larger content is retained without a probe; nested values are skipped without being built.",
        NO_REFUSAL,
    ),
    limit(
        "text.buffer_bytes",
        utf8::BUFFER_BYTES as u64,
        Bytes,
        "Memory for the UTF-8 check of a text derivative, whatever the artifact's size. Invalid UTF-8 is reported and yields no derivative.",
        NO_REFUSAL,
    ),
    limit(
        "producer_run.envelope_bytes",
        producer_run::MAX_ENVELOPE_BYTES as u64,
        Bytes,
        "One producer-run envelope.",
        INVALID_INPUT,
    ),
    limit(
        "producer_run.artifacts",
        producer_run::MAX_ARTIFACTS as u64,
        Items,
        "Artifacts in one producer-run envelope.",
        INVALID_INPUT,
    ),
    limit(
        "producer_run.assertions",
        producer_run::MAX_ASSERTIONS as u64,
        Items,
        "Assertions in one producer-run envelope.",
        INVALID_INPUT,
    ),
    limit(
        "producer_run.value_bytes",
        producer_run::MAX_VALUE_BYTES as u64,
        Bytes,
        "One serialized assertion value.",
        INVALID_INPUT,
    ),
    limit(
        "capture.event_bytes",
        HandleLimits::DEFAULT.max_event_bytes,
        Bytes,
        "One capture event, for handles declared by this build.",
        INVALID_INPUT,
    ),
    limit(
        "capture.attachment_bytes",
        HandleLimits::DEFAULT.max_attachment_bytes,
        Bytes,
        "One capture attachment, for handles declared by this build.",
        INVALID_INPUT,
    ),
    limit(
        "capture.items",
        HandleLimits::DEFAULT.max_items,
        Items,
        "Items accepted by one capture handle declared by this build.",
        INVALID_INPUT,
    ),
    limit(
        "selector.artifact_bytes",
        ingest::MAX_IMPORT_BYTES,
        Bytes,
        "A retained artifact a native selector reads. Larger recorded sizes are refused as an integrity failure.",
        INTEGRITY_FAILURE,
    ),
    limit(
        "selector.json_bytes",
        selectors::MAX_JSON_BYTES as u64,
        Bytes,
        "An artifact parsed by the strict json_pointer reader.",
        INVALID_INPUT,
    ),
    limit(
        "selector.json_depth",
        selectors::MAX_JSON_DEPTH as u64,
        Levels,
        "Nesting of an artifact parsed by the json_pointer reader.",
        INVALID_INPUT,
    ),
    limit(
        "selector.table_columns",
        delimited::MAX_COLUMNS as u64,
        Items,
        "Fields in one row of a CSV or TSV table read by table_cell.",
        INVALID_INPUT,
    ),
    limit(
        "selector.excerpt_bytes",
        evidence_read::MAX_EXCERPT_BYTES as u64,
        Bytes,
        "One excerpt returned by an evidence read.",
        INVALID_INPUT,
    ),
    limit(
        "search.candidates",
        crate::projection::MAX_SEARCH_CANDIDATES as u64,
        Items,
        "Index rows read for one search before the canonical checks.",
        NO_REFUSAL,
    ),
    limit(
        "method_pack.bytes",
        method_pack::MAX_PACK_BYTES as u64,
        Bytes,
        "One method pack file.",
        INVALID_INPUT,
    ),
    limit(
        "method_pack.depth",
        method_pack::MAX_PACK_DEPTH as u64,
        Levels,
        "Nesting of a method pack.",
        INVALID_INPUT,
    ),
    limit(
        "journal.event_bytes",
        journal::MAX_EVENT_BYTES as u64,
        Bytes,
        "One canonical journal event read from disk.",
        INTEGRITY_FAILURE,
    ),
    limit(
        "journal.event_depth",
        journal::MAX_EVENT_DEPTH as u64,
        Levels,
        "Nesting of one canonical journal event.",
        INTEGRITY_FAILURE,
    ),
];

/// Whether this build expands compressed or archived content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExpansionPolicy {
    pub decompression: bool,
    pub archive_extraction: bool,
    pub statement: &'static str,
}

/// No decompressor or archive reader is linked. Compressed files and archives
/// are retained as opaque bytes under the ordinary import limit, so there is
/// no expansion ratio, expanded size, entry count or entry size to bound.
pub const EXPANSION: ExpansionPolicy = ExpansionPolicy {
    decompression: false,
    archive_extraction: false,
    statement: "Compressed files and archives are retained as opaque bytes and never expanded. Entry names inside an archive are never used as paths.",
};

/// The limit with this name.
pub fn get(name: &str) -> Option<&'static Limit> {
    LIMITS.iter().find(|limit| limit.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_dotted_and_values_positive() {
        let mut names = std::collections::BTreeSet::new();
        for limit in LIMITS {
            assert!(names.insert(limit.name), "{} repeats", limit.name);
            let (group, rest) = limit.name.split_once('.').expect("group.name");
            for part in [group, rest] {
                assert!(
                    !part.is_empty()
                        && part
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
                    "{}",
                    limit.name
                );
            }
            assert!(limit.value > 0, "{}", limit.name);
            assert!(limit.applies_to.ends_with('.'), "{}", limit.name);
        }
        assert_eq!(
            get("ingest.file_bytes").map(|limit| limit.value),
            Some(ingest::MAX_IMPORT_BYTES)
        );
    }

    #[test]
    fn the_table_serializes_for_reports() {
        let value = serde_json::to_value(LIMITS[0]).unwrap();
        assert_eq!(value["name"], "request.bytes");
        assert_eq!(value["unit"], "bytes");
        assert_eq!(value["refusal"], "INVALID_INPUT");
        let expansion = serde_json::to_value(EXPANSION).unwrap();
        assert_eq!(expansion["decompression"], false);
    }
}
