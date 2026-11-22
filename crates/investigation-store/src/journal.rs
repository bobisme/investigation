//! Versioned case genesis and exact-byte hash chains for canonical events.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::{self, Read},
    path::{Path, PathBuf},
};

use investigation_core::{
    command::{Command, RevisionRef, SemanticRequest},
    header::{Actor, UtcTimestamp},
    identity::{Id, Kind},
    input::{self, MAX_REQUEST_BYTES},
    state::{AcceptedTransaction, State},
    wire::{Digest, ErrorCode, Failure, Snapshot},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::durable::DurableFs;

pub const GENESIS_FILENAME: &str = "genesis.json";
pub const EVENTS_DIRECTORY: &str = "events";
pub const MAX_EVENT_BYTES: usize = 4 * MAX_REQUEST_BYTES;
const MAX_GENESIS_BYTES: usize = 4096;
const MAX_EVENT_DEPTH: usize = 64;
const MAX_SEQUENCE: u64 = 999_999_999_999;
const GENESIS_VERSION: &str = "inv.case-genesis.v0-alpha";
const LEGACY_EVENT_VERSION: &str = "inv.event.v0-alpha";
const EVENT_VERSION: &str = "inv.event.v0-alpha.2";
const COMMAND_VERSION: &str = "inv.command.v0-alpha";
pub(crate) const INGEST_VERSION: &str = "inv.ingest.v0-alpha";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalErrorKind {
    Io,
    MissingGenesis,
    MissingEventsDirectory,
    CorruptGenesis,
    UnsupportedGenesisVersion,
    InvalidIdentity,
    InvalidFilename,
    DuplicateSequence,
    MissingEvent,
    CorruptEvent,
    UnsupportedEventVersion,
    SequenceMismatch,
    ParentHashMismatch,
    RequestHashMismatch,
    MetadataMismatch,
    DuplicateTransaction,
    DuplicateCommand,
    InvalidPrecondition,
    InvalidAfterImage,
}

impl JournalErrorKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Io => "JOURNAL_IO_FAILURE",
            Self::MissingGenesis => "MISSING_CASE_GENESIS",
            Self::MissingEventsDirectory => "MISSING_EVENTS_DIRECTORY",
            Self::CorruptGenesis => "CORRUPT_CASE_GENESIS",
            Self::UnsupportedGenesisVersion => "UNSUPPORTED_CASE_GENESIS_VERSION",
            Self::InvalidIdentity => "INVALID_JOURNAL_IDENTITY",
            Self::InvalidFilename => "INVALID_EVENT_FILENAME",
            Self::DuplicateSequence => "DUPLICATE_EVENT_SEQUENCE",
            Self::MissingEvent => "MISSING_EVENT",
            Self::CorruptEvent => "CORRUPT_EVENT",
            Self::UnsupportedEventVersion => "UNSUPPORTED_EVENT_VERSION",
            Self::SequenceMismatch => "EVENT_SEQUENCE_MISMATCH",
            Self::ParentHashMismatch => "EVENT_PARENT_HASH_MISMATCH",
            Self::RequestHashMismatch => "EVENT_REQUEST_HASH_MISMATCH",
            Self::MetadataMismatch => "EVENT_METADATA_MISMATCH",
            Self::DuplicateTransaction => "DUPLICATE_TRANSACTION_ID",
            Self::DuplicateCommand => "DUPLICATE_COMMAND_ID",
            Self::InvalidPrecondition => "INVALID_EVENT_PRECONDITION",
            Self::InvalidAfterImage => "INVALID_EVENT_AFTER_IMAGE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalError {
    pub kind: JournalErrorKind,
    pub object: PathBuf,
    pub reason: String,
}

impl JournalError {
    fn new(kind: JournalErrorKind, object: &Path, reason: impl Into<String>) -> Self {
        Self {
            kind,
            object: object.to_path_buf(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for JournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}: {}",
            self.kind.code(),
            self.object.display(),
            self.reason
        )
    }
}

impl std::error::Error for JournalError {}

impl From<JournalError> for Failure {
    fn from(error: JournalError) -> Self {
        let code = if error.kind == JournalErrorKind::Io {
            ErrorCode::IoFailure
        } else {
            ErrorCode::IntegrityFailure
        };
        let mut failure = Failure::new(code, error.to_string());
        failure.error.actual = Some(serde_json::json!({
            "diagnostic_code": error.kind.code(),
            "object": error.object,
        }));
        failure
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseGenesis {
    schema_version: String,
    pub workspace_id: Id,
    pub case_id: Id,
}

impl CaseGenesis {
    pub fn new(workspace_id: Id, case_id: Id) -> Result<Self, JournalError> {
        let genesis = Self {
            schema_version: GENESIS_VERSION.into(),
            workspace_id,
            case_id,
        };
        genesis.validate(Path::new(GENESIS_FILENAME), workspace_id, case_id)?;
        Ok(genesis)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, JournalError> {
        let object = Path::new(GENESIS_FILENAME);
        self.validate(object, self.workspace_id, self.case_id)?;
        encode(
            self,
            object,
            MAX_GENESIS_BYTES,
            JournalErrorKind::CorruptGenesis,
        )
    }

    fn validate(&self, path: &Path, workspace_id: Id, case_id: Id) -> Result<(), JournalError> {
        if self.workspace_id.kind() != Kind::Workspace
            || self.case_id.kind() != Kind::Case
            || self.workspace_id != workspace_id
            || self.case_id != case_id
        {
            return Err(JournalError::new(
                JournalErrorKind::InvalidIdentity,
                path,
                "Case genesis does not belong to the requested workspace and case.",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    schema_version: String,
    pub workspace_id: Id,
    pub transaction_id: Id,
    pub sequence: u64,
    pub previous_hash: Digest,
    pub actor: Actor,
    pub recorded_at: UtcTimestamp,
    pub command_id: String,
    pub request_hash: Digest,
    pub preconditions: Vec<RevisionRef>,
    pub request: SemanticRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_digest: Option<Digest>,
    #[serde(rename = "accepted_operations")]
    pub accepted: AcceptedTransaction,
}

pub(crate) struct EventInput<'a> {
    pub actor: &'a Actor,
    pub command_id: &'a str,
    pub preconditions: &'a [RevisionRef],
    pub manifest_digest: Option<Digest>,
}

impl Event {
    pub(crate) fn new(
        workspace_id: Id,
        transaction_id: Id,
        previous_hash: Digest,
        input: EventInput<'_>,
        request: SemanticRequest,
        accepted: AcceptedTransaction,
    ) -> Result<Self, JournalError> {
        let name = event_filename(accepted.sequence)?;
        let path = Path::new(&name);
        let first = accepted.records.first().ok_or_else(|| {
            JournalError::new(
                JournalErrorKind::InvalidAfterImage,
                path,
                "Expected at least one accepted after-image.",
            )
        })?;
        let event = Self {
            schema_version: EVENT_VERSION.into(),
            workspace_id,
            transaction_id,
            sequence: accepted.sequence,
            previous_hash,
            actor: input.actor.clone(),
            recorded_at: first.header.recorded_at.clone(),
            command_id: input.command_id.to_owned(),
            request_hash: request_digest(&request, path)?,
            preconditions: input.preconditions.to_vec(),
            request,
            manifest_digest: input.manifest_digest,
            accepted,
        };
        event.validate_metadata(path)?;
        Ok(event)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, JournalError> {
        let name = event_filename(self.sequence)?;
        let path = Path::new(&name);
        self.validate_metadata(path)?;
        encode(self, path, MAX_EVENT_BYTES, JournalErrorKind::CorruptEvent)
    }

    fn validate_metadata(&self, path: &Path) -> Result<(), JournalError> {
        if self.workspace_id.kind() != Kind::Workspace
            || self.transaction_id.kind() != Kind::Transaction
            || self.accepted.case_id.kind() != Kind::Case
        {
            return Err(JournalError::new(
                JournalErrorKind::InvalidIdentity,
                path,
                "Expected typed workspace, case and transaction IDs.",
            ));
        }
        let invalid = |reason| JournalError::new(JournalErrorKind::MetadataMismatch, path, reason);
        if self.sequence != self.accepted.sequence
            || self.command_id.is_empty()
            || self.command_id.chars().count() > 128
            || self.command_id.chars().any(char::is_control)
            || !self.request.input.is_object()
        {
            return Err(invalid(
                "Invalid event sequence, command ID or semantic request.",
            ));
        }
        if self.request_hash != request_digest(&self.request, path)? {
            return Err(JournalError::new(
                JournalErrorKind::RequestHashMismatch,
                path,
                "Stored request digest disagrees with the stored semantic request.",
            ));
        }
        let request_version = self
            .request
            .input
            .get("schema_version")
            .and_then(Value::as_str);
        if request_version == Some(COMMAND_VERSION) {
            let command: Command = decode(
                &self.request.input,
                path,
                JournalErrorKind::MetadataMismatch,
            )?;
            command.validate_shape().map_err(|error| {
                JournalError::new(JournalErrorKind::MetadataMismatch, path, error.to_string())
            })?;
            if command.actor != self.actor
                || command.command_id != self.command_id
                || command.preconditions != self.preconditions
                || command.case_id != ((self.sequence != 1).then_some(self.accepted.case_id))
            {
                return Err(invalid(
                    "Command envelope disagrees with event metadata or case scope.",
                ));
            }
            if self.manifest_digest.is_some() {
                return Err(invalid("Record events cannot carry an import manifest."));
            }
        } else if request_version == Some(INGEST_VERSION) {
            let request = &self.request.input;
            let target = (self.sequence != 1).then_some(self.accepted.case_id);
            let actual_target = request.get("case_id").cloned().unwrap_or(Value::Null);
            if request.get("actor") != Some(&serde_json::json!(self.actor))
                || request.get("command_id").and_then(Value::as_str)
                    != Some(self.command_id.as_str())
                || actual_target != serde_json::json!(target)
                || self.manifest_digest.is_none()
                || !self.preconditions.is_empty()
            {
                return Err(invalid(
                    "Ingest request disagrees with event metadata or case scope.",
                ));
            }
        } else {
            return Err(invalid(
                "Event has an unsupported semantic request version.",
            ));
        }
        for record in &self.accepted.records {
            if record.header.recorded_at != self.recorded_at
                || record.header.recorded_by != self.actor
            {
                return Err(invalid(
                    "Accepted revision attribution disagrees with event metadata.",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedEvent {
    pub event: Event,
    pub hash: Digest,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaseReplay {
    pub state: State,
    pub snapshot: Snapshot,
    pub events: Vec<VerifiedEvent>,
}

pub fn hash_bytes(bytes: &[u8]) -> Digest {
    format!("sha256:{:x}", Sha256::digest(bytes))
        .parse()
        .expect("SHA-256 hex has a valid format")
}

pub fn event_filename(sequence: u64) -> Result<String, JournalError> {
    if !(1..=MAX_SEQUENCE).contains(&sequence) {
        return Err(JournalError::new(
            JournalErrorKind::InvalidFilename,
            Path::new(EVENTS_DIRECTORY),
            "Event sequence exceeds the range of twelve-digit sequence names.",
        ));
    }
    Ok(format!("{sequence:012}.json"))
}

/// Decode an event without rewriting it. The hash covers the exact input bytes.
pub fn decode_event(
    path: &Path,
    sequence: u64,
    bytes: &[u8],
) -> Result<VerifiedEvent, JournalError> {
    let value = parse(bytes, path, MAX_EVENT_BYTES, JournalErrorKind::CorruptEvent)?;
    let event: Event = match version(&value) {
        Some(EVENT_VERSION) => decode(&value, path, JournalErrorKind::CorruptEvent)?,
        Some(LEGACY_EVENT_VERSION) => {
            if value.get("manifest_digest").is_some() {
                return Err(JournalError::new(
                    JournalErrorKind::CorruptEvent,
                    path,
                    "Legacy events cannot carry import manifests.",
                ));
            }
            let mut compatible: Event = decode(&value, path, JournalErrorKind::CorruptEvent)?;
            compatible.schema_version = EVENT_VERSION.into();
            compatible
        }
        Some(other) => {
            return Err(unsupported(
                JournalErrorKind::UnsupportedEventVersion,
                path,
                other,
            ));
        }
        None => {
            return Err(JournalError::new(
                JournalErrorKind::CorruptEvent,
                path,
                "Event has no string schema_version.",
            ));
        }
    };
    if event.sequence != sequence {
        return Err(JournalError::new(
            JournalErrorKind::SequenceMismatch,
            path,
            format!(
                "Filename sequence {sequence} disagrees with event sequence {}.",
                event.sequence
            ),
        ));
    }
    event.validate_metadata(path)?;
    Ok(VerifiedEvent {
        event,
        hash: hash_bytes(bytes),
    })
}

pub fn replay_case(
    fs: &impl DurableFs,
    case_dir: &Path,
    workspace_id: Id,
    case_id: Id,
) -> Result<CaseReplay, JournalError> {
    let genesis_path = case_dir.join(GENESIS_FILENAME);
    let genesis_bytes = read_bounded(
        fs,
        &genesis_path,
        MAX_GENESIS_BYTES,
        JournalErrorKind::MissingGenesis,
        JournalErrorKind::CorruptGenesis,
    )?;
    let value = parse(
        &genesis_bytes,
        &genesis_path,
        MAX_GENESIS_BYTES,
        JournalErrorKind::CorruptGenesis,
    )?;
    let genesis: CaseGenesis = match version(&value) {
        Some(GENESIS_VERSION) => decode(&value, &genesis_path, JournalErrorKind::CorruptGenesis)?,
        Some(other) => {
            return Err(unsupported(
                JournalErrorKind::UnsupportedGenesisVersion,
                &genesis_path,
                other,
            ));
        }
        None => {
            return Err(JournalError::new(
                JournalErrorKind::CorruptGenesis,
                &genesis_path,
                "Case genesis has no string schema_version.",
            ));
        }
    };
    genesis.validate(&genesis_path, workspace_id, case_id)?;

    let mut result = CaseReplay {
        state: State::default(),
        snapshot: Snapshot {
            sequence: 0,
            head_hash: hash_bytes(&genesis_bytes),
        },
        events: Vec::new(),
    };
    result
        .state
        .note_verified_snapshot(result.snapshot.clone())
        .map_err(|error| {
            JournalError::new(
                JournalErrorKind::CorruptGenesis,
                &genesis_path,
                error.to_string(),
            )
        })?;
    let mut transactions = BTreeSet::new();
    let mut command_keys = BTreeSet::new();
    let events_dir = case_dir.join(EVENTS_DIRECTORY);
    for (sequence, path) in ordered_paths(fs, &events_dir)? {
        let expected = result.snapshot.sequence + 1;
        if sequence != expected {
            let missing = events_dir.join(event_filename(expected)?);
            return Err(JournalError::new(
                JournalErrorKind::MissingEvent,
                &missing,
                format!("Found event {sequence} before expected event {expected}."),
            ));
        }
        let bytes = read_bounded(
            fs,
            &path,
            MAX_EVENT_BYTES,
            JournalErrorKind::MissingEvent,
            JournalErrorKind::CorruptEvent,
        )?;
        let verified = decode_event(&path, sequence, &bytes)?;
        let event = &verified.event;
        if event.workspace_id != workspace_id || event.accepted.case_id != case_id {
            return Err(JournalError::new(
                JournalErrorKind::InvalidIdentity,
                &path,
                "Event belongs to a different workspace or case.",
            ));
        }
        if event.previous_hash != result.snapshot.head_hash {
            return Err(JournalError::new(
                JournalErrorKind::ParentHashMismatch,
                &path,
                format!(
                    "Expected parent {}; stored parent is {}.",
                    result.snapshot.head_hash, event.previous_hash
                ),
            ));
        }
        if !transactions.insert(event.transaction_id) {
            return Err(JournalError::new(
                JournalErrorKind::DuplicateTransaction,
                &path,
                "Transaction ID already appears in this chain.",
            ));
        }
        if !command_keys.insert((event.actor.clone(), event.command_id.clone())) {
            return Err(JournalError::new(
                JournalErrorKind::DuplicateCommand,
                &path,
                "Actor and command ID already appear in this chain.",
            ));
        }
        verify_preconditions(&result.state, event, &path)?;
        result.state.apply(&event.accepted).map_err(|error| {
            JournalError::new(
                JournalErrorKind::InvalidAfterImage,
                &path,
                error.to_string(),
            )
        })?;
        result.snapshot = Snapshot {
            sequence,
            head_hash: verified.hash.clone(),
        };
        result
            .state
            .note_verified_snapshot(result.snapshot.clone())
            .map_err(|error| {
                JournalError::new(
                    JournalErrorKind::InvalidAfterImage,
                    &path,
                    error.to_string(),
                )
            })?;
        result.events.push(verified);
    }
    Ok(result)
}

fn verify_preconditions(state: &State, event: &Event, path: &Path) -> Result<(), JournalError> {
    let mut seen = BTreeSet::new();
    for expected in &event.preconditions {
        let current = state.current(expected.id).map(|record| record.reference());
        if current != Some(*expected) || !seen.insert(expected.id) {
            return Err(JournalError::new(
                JournalErrorKind::InvalidPrecondition,
                path,
                format!(
                    "Precondition {} revision {} was duplicate or not current.",
                    expected.id, expected.revision
                ),
            ));
        }
    }
    Ok(())
}

fn ordered_paths(
    fs: &impl DurableFs,
    directory: &Path,
) -> Result<BTreeMap<u64, PathBuf>, JournalError> {
    let mut paths = fs
        .list(directory)
        .map_err(|error| io_error(error, directory, JournalErrorKind::MissingEventsDirectory))?;
    paths.sort();
    let mut result = BTreeMap::new();
    for path in paths {
        let name = path.file_name().and_then(|value| value.to_str());
        let sequence = name
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|digits| digits.parse::<u64>().ok())
            .filter(|sequence| (1..=MAX_SEQUENCE).contains(sequence))
            .ok_or_else(|| bad_filename(&path))?;
        if let Some(other) = result.insert(sequence, path.clone()) {
            return Err(JournalError::new(
                JournalErrorKind::DuplicateSequence,
                &path,
                format!("Sequence {sequence} also appears at {}.", other.display()),
            ));
        }
    }
    for (sequence, path) in &result {
        if path.file_name().and_then(|value| value.to_str())
            != Some(event_filename(*sequence)?.as_str())
        {
            return Err(bad_filename(path));
        }
    }
    Ok(result)
}

fn bad_filename(path: &Path) -> JournalError {
    JournalError::new(
        JournalErrorKind::InvalidFilename,
        path,
        "Expected a twelve-digit sequence followed by .json.",
    )
}

fn read_bounded(
    fs: &impl DurableFs,
    path: &Path,
    max: usize,
    missing: JournalErrorKind,
    corrupt: JournalErrorKind,
) -> Result<Vec<u8>, JournalError> {
    let file = fs
        .read(path)
        .map_err(|error| io_error(error, path, missing))?;
    let mut bytes = Vec::new();
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(error, path, missing))?;
    limit(bytes, path, max, corrupt)
}

fn io_error(error: io::Error, path: &Path, missing: JournalErrorKind) -> JournalError {
    let kind = if error.kind() == io::ErrorKind::NotFound {
        missing
    } else {
        JournalErrorKind::Io
    };
    JournalError::new(kind, path, error.to_string())
}

fn limit(
    bytes: Vec<u8>,
    path: &Path,
    max: usize,
    kind: JournalErrorKind,
) -> Result<Vec<u8>, JournalError> {
    if bytes.len() > max {
        return Err(JournalError::new(
            kind,
            path,
            format!("JSON exceeds the {max}-byte limit."),
        ));
    }
    Ok(bytes)
}

fn version(value: &Value) -> Option<&str> {
    value.get("schema_version").and_then(Value::as_str)
}

fn unsupported(kind: JournalErrorKind, path: &Path, version: &str) -> JournalError {
    let display = version.chars().take(128).collect::<String>();
    JournalError::new(
        kind,
        path,
        format!("Unsupported required schema {display:?}."),
    )
}

fn parse(
    bytes: &[u8],
    path: &Path,
    max: usize,
    kind: JournalErrorKind,
) -> Result<Value, JournalError> {
    input::parse_json(bytes, max, MAX_EVENT_DEPTH)
        .map_err(|error| JournalError::new(kind, path, error.to_string()))
}

fn decode<T: DeserializeOwned>(
    value: &Value,
    path: &Path,
    kind: JournalErrorKind,
) -> Result<T, JournalError> {
    input::decode(value).map_err(|error| JournalError::new(kind, path, error.to_string()))
}

fn encode<T: Serialize>(
    value: &T,
    path: &Path,
    max: usize,
    kind: JournalErrorKind,
) -> Result<Vec<u8>, JournalError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| JournalError::new(kind, path, error.to_string()))?;
    let bytes = limit(bytes, path, max, kind)?;
    parse(&bytes, path, max, kind)?;
    Ok(bytes)
}

fn request_digest(request: &SemanticRequest, path: &Path) -> Result<Digest, JournalError> {
    // This digest fingerprints the parsed request and ordered attachments. Its
    // equality does not replace comparing the stored semantic requests.
    let bytes = serde_json::to_vec(request).map_err(|error| {
        JournalError::new(JournalErrorKind::CorruptEvent, path, error.to_string())
    })?;
    Ok(hash_bytes(&bytes))
}
