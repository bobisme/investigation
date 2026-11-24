//! Versioned case genesis and exact-byte hash chains for canonical events.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use investigation_core::{
    command::{Command, CommandVersion, RevisionRef, SemanticRequest},
    header::{Actor, UtcTimestamp},
    identity::{Id, Kind},
    inherited_trust::{SessionLedger, SessionReads, SessionSource},
    input::{self, MAX_REQUEST_BYTES},
    state::{AcceptedTransaction, SharedImages, State},
    wire::{Digest, ErrorCode, Failure, Snapshot},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::{
    durable::DurableFs,
    store::{
        capture::{CAPTURE_OPERATIONS, CaptureProtocol, CaptureRequest},
        ingest::IngestVersion,
    },
};

pub const GENESIS_FILENAME: &str = "genesis.json";
pub const EVENTS_DIRECTORY: &str = "events";
pub const MAX_EVENT_BYTES: usize = 4 * MAX_REQUEST_BYTES;
const MAX_GENESIS_BYTES: usize = 4096;
pub const MAX_EVENT_DEPTH: usize = 64;
const MAX_SEQUENCE: u64 = 999_999_999_999;
const GENESIS_VERSION: &str = "inv.case-genesis.v0-alpha";
const LEGACY_EVENT_VERSION: &str = "inv.event.v0-alpha";
/// Version written on every new journal event.
pub const EVENT_VERSION: &str = "inv.event.v0-alpha.2";

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
    /// The file parses, but its bytes are not the serialization this program
    /// writes for that content (ADR 0016).
    NonCanonical,
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
            Self::NonCanonical => "NON_CANONICAL_JOURNAL_FILE",
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
    /// The evidence reads recorded for the actor under the session the
    /// command declared, as known at commit. Absent when no session was
    /// declared. See [`investigation_core::inherited_trust`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_reads: Option<SessionReads>,
}

pub(crate) struct EventInput<'a> {
    pub actor: &'a Actor,
    pub command_id: &'a str,
    pub preconditions: &'a [RevisionRef],
    pub manifest_digest: Option<Digest>,
    pub session_reads: Option<SessionReads>,
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
            session_reads: input.session_reads,
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
        let request_version = &self.request.input["schema_version"];
        if input::decode::<CommandVersion>(request_version).is_ok() {
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
        } else if input::decode::<IngestVersion>(request_version).is_ok() {
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
        } else if input::decode::<CaptureProtocol>(request_version).is_ok() {
            let request: CaptureRequest = decode(
                &self.request.input,
                path,
                JournalErrorKind::MetadataMismatch,
            )?;
            // Only finalization retains a manifest; a declaration has none.
            let manifest = request.operation == crate::store::capture::finalize::FINALIZE_OPERATION;
            if !CAPTURE_OPERATIONS.contains(&request.operation.as_str())
                || request.actor != self.actor
                || request.command_id != self.command_id
                || request.case_id != ((self.sequence != 1).then_some(self.accepted.case_id))
                || !self.preconditions.is_empty()
                || self.manifest_digest.is_some() != manifest
            {
                return Err(invalid(
                    "Capture request disagrees with event metadata or case scope.",
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

impl SessionSource for CaseReplay {
    fn session_reads(&self, sequence: u64) -> Option<&SessionReads> {
        let offset = usize::try_from(sequence.checked_sub(1)?).ok()?;
        let event = &self.events.get(offset)?.event;
        (event.sequence == sequence)
            .then_some(event.session_reads.as_ref())
            .flatten()
    }
}

impl CaseReplay {
    /// Session reads stamped on this case's events, by acceptance sequence,
    /// for the inherited trust of authored text.
    pub fn session_ledger(&self) -> SessionLedger {
        self.events
            .iter()
            .filter_map(|verified| {
                let event = &verified.event;
                Some((event.sequence, event.session_reads.clone()?))
            })
            .collect()
    }
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

/// Decode one event file on its own, without rewriting it. The hash covers
/// the exact input bytes. Nothing records the hash of a file decoded alone,
/// so its bytes must be the bytes this program writes for its content
/// (ADR 0016), unless the event is of the legacy version.
pub fn decode_event(
    path: &Path,
    sequence: u64,
    bytes: &[u8],
) -> Result<VerifiedEvent, JournalError> {
    let (verified, canonical) = decode_event_layout(path, sequence, bytes)?;
    if !canonical {
        return Err(non_canonical(path));
    }
    Ok(verified)
}

/// Decode an event with every check but its layout, and say whether its
/// bytes are the bytes this program writes for its content. A legacy event
/// was written in a layout this program cannot reproduce and counts as
/// canonical. A replay accepts other bytes only where the next event records
/// their hash (ADR 0016).
fn decode_event_layout(
    path: &Path,
    sequence: u64,
    bytes: &[u8],
) -> Result<(VerifiedEvent, bool), JournalError> {
    let (event, written_by_this_format) = decode_event_content(path, sequence, bytes)?;
    let canonical = !written_by_this_format || is_canonical(&event, bytes);
    Ok((
        VerifiedEvent {
            event,
            hash: hash_bytes(bytes),
        },
        canonical,
    ))
}

/// The bytes this program writes for the content of the event file at
/// `path`, when its bytes are not they; `None` when they are, or when the
/// event is of the legacy version. Every other check of [`decode_event`]
/// applies.
pub fn canonical_event(
    path: &Path,
    sequence: u64,
    bytes: &[u8],
) -> Result<Option<Vec<u8>>, JournalError> {
    let (event, written_by_this_format) = decode_event_content(path, sequence, bytes)?;
    Ok(written_by_this_format
        .then(|| canonical_form(&event, bytes))
        .flatten())
}

/// The bytes this program writes for the content of the genesis file at
/// `path`, when its bytes are not they.
pub fn canonical_genesis(
    path: &Path,
    bytes: &[u8],
    workspace_id: Id,
    case_id: Id,
) -> Result<Option<Vec<u8>>, JournalError> {
    let genesis = decode_genesis(path, bytes)?;
    genesis.validate(path, workspace_id, case_id)?;
    Ok(canonical_form(&genesis, bytes))
}

/// An event's content and whether it is of the current version, with every
/// check but the canonical bytes.
fn decode_event_content(
    path: &Path,
    sequence: u64,
    bytes: &[u8],
) -> Result<(Event, bool), JournalError> {
    let value = parse(bytes, path, MAX_EVENT_BYTES, JournalErrorKind::CorruptEvent)?;
    let mut written_by_this_format = false;
    let event: Event = match version(&value) {
        Some(EVENT_VERSION) => {
            written_by_this_format = true;
            decode(&value, path, JournalErrorKind::CorruptEvent)?
        }
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
    Ok((event, written_by_this_format))
}

/// The bytes this program writes for `value`, when `bytes` are not they:
/// `None` for canonical bytes.
fn canonical_form<T: Serialize>(value: &T, bytes: &[u8]) -> Option<Vec<u8>> {
    (!is_canonical(value, bytes)).then(|| serde_json::to_vec(value).expect("JSON serializes"))
}

/// Whether `bytes` are exactly the serialization of `value` this program
/// writes (compact `serde_json`, no trailing newline). The serialization is
/// compared as it is produced, so no copy is made and the first differing
/// byte stops it.
fn is_canonical<T: Serialize>(value: &T, bytes: &[u8]) -> bool {
    struct Compare<'a> {
        rest: &'a [u8],
    }
    impl io::Write for Compare<'_> {
        fn write(&mut self, written: &[u8]) -> io::Result<usize> {
            match self.rest.strip_prefix(written) {
                Some(rest) => {
                    self.rest = rest;
                    Ok(written.len())
                }
                None => Err(io::Error::other("differs")),
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut compare = Compare { rest: bytes };
    serde_json::to_writer(&mut compare, value).is_ok() && compare.rest.is_empty()
}

fn non_canonical(path: &Path) -> JournalError {
    JournalError::new(
        JournalErrorKind::NonCanonical,
        path,
        "Another program rewrote this file: its bytes are not the bytes this program writes for \
         its content (whitespace, key order, escapes or a line ending differ), and no later \
         journal file records them. Run investigation repair journal: it restores the written \
         bytes when the next journal file, or the index for a case head, records their hash, \
         which shows the content is unchanged. Otherwise restore the workspace from an archive.",
    )
}

/// Decode event bytes that are expected to be byte-identical to bytes
/// [`decode_event`] accepted earlier, skipping the checks that are a pure
/// function of those bytes: the bounded duplicate-key parse and the metadata
/// validation. For bytes `decode_event` accepts, the result is equal to its
/// result. The caller accepts it only after the chain these bytes belong to
/// ends at a [`KnownHead`], which proves the bytes are the ones verified.
pub(crate) fn decode_known(
    path: &Path,
    sequence: u64,
    bytes: &[u8],
) -> Result<VerifiedEvent, JournalError> {
    let unknown = || {
        JournalError::new(
            JournalErrorKind::CorruptEvent,
            path,
            "Event bytes differ from a verified head.",
        )
    };
    let mut event: Event = serde_json::from_slice(bytes).map_err(|_| unknown())?;
    match event.schema_version.as_str() {
        EVENT_VERSION => {}
        LEGACY_EVENT_VERSION if event.manifest_digest.is_none() => {
            event.schema_version = EVENT_VERSION.into();
        }
        _ => return Err(unknown()),
    }
    if event.sequence != sequence {
        return Err(unknown());
    }
    Ok(VerifiedEvent {
        event,
        hash: hash_bytes(bytes),
    })
}

/// Which workspace wrote each part of one case journal.
///
/// An ordinary case is written by one workspace, from its genesis on: every
/// event and the genesis name `workspace_id`. A case a fork inherited
/// (`archive fork`) keeps the exact bytes its origin wrote, so up to the
/// inherited head its genesis and events name the workspaces that wrote
/// them, and only later events name the fork. See [`InheritedCase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalWriters<'a> {
    /// The workspace reading the journal; it writes every later event.
    pub workspace_id: Id,
    /// The prefix inherited from an origin workspace, when there is one.
    pub inherited: Option<&'a InheritedCase>,
}

impl From<Id> for JournalWriters<'_> {
    fn from(workspace_id: Id) -> Self {
        Self {
            workspace_id,
            inherited: None,
        }
    }
}

impl JournalWriters<'_> {
    /// The workspace the genesis (sequence 0) or event `sequence` must name.
    pub fn at(&self, sequence: u64) -> Id {
        self.inherited
            .filter(|inherited| sequence <= inherited.sequence)
            .and_then(|inherited| {
                inherited
                    .writers
                    .iter()
                    .find(|writer| sequence <= writer.through_sequence)
            })
            .map_or(self.workspace_id, |writer| writer.workspace_id)
    }
}

/// A case journal a fork inherited: its verified head at the fork, and
/// which workspaces wrote it up to there. Recorded once, in the fork's
/// immutable workspace marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InheritedCase {
    pub case_id: Id,
    /// The head the fork started from. The event at this sequence must
    /// hash to `head_hash`, so every inherited byte is the origin's.
    pub sequence: u64,
    pub head_hash: Digest,
    /// Consecutive runs of events by writing workspace, in sequence order.
    /// The first writer also wrote the genesis; the last run ends at
    /// `sequence`.
    pub writers: Vec<JournalWriter>,
}

/// One workspace that wrote events of an inherited case, through
/// `through_sequence`, since the previous writer's last event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalWriter {
    pub workspace_id: Id,
    pub through_sequence: u64,
}

impl InheritedCase {
    /// The writers of `events`, a verified journal from sequence 1, as runs.
    pub fn writers_of<'a>(events: impl IntoIterator<Item = &'a Event>) -> Vec<JournalWriter> {
        let mut writers: Vec<JournalWriter> = Vec::new();
        for event in events {
            match writers.last_mut() {
                Some(last) if last.workspace_id == event.workspace_id => {
                    last.through_sequence = event.sequence;
                }
                _ => writers.push(JournalWriter {
                    workspace_id: event.workspace_id,
                    through_sequence: event.sequence,
                }),
            }
        }
        writers
    }

    /// Whether the record is well formed for a fork with `own` identity:
    /// a case ID, at least one writer, runs that increase strictly, change
    /// writer at each run, end at the head and never name `own`.
    pub fn is_well_formed(&self, own: Id) -> bool {
        let mut previous: Option<&JournalWriter> = None;
        for writer in &self.writers {
            if writer.workspace_id.kind() != Kind::Workspace
                || writer.workspace_id == own
                || previous.is_some_and(|previous| {
                    previous.through_sequence >= writer.through_sequence
                        || previous.workspace_id == writer.workspace_id
                })
                || writer.through_sequence == 0
            {
                return false;
            }
            previous = Some(writer);
        }
        self.case_id.kind() == Kind::Case
            && self.sequence > 0
            && previous.is_some_and(|last| last.through_sequence == self.sequence)
    }
}

/// A case head this program verified in full earlier: event `sequence`
/// hashed to `head_hash`. Read from a disposable checkpoint; see
/// [`replay_case_from`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownHead {
    pub sequence: u64,
    pub head_hash: Digest,
}

/// Replay and verify a case like [`replay_case`], reusing an earlier
/// verification of its first `known.sequence` events.
///
/// Every event file is still read and hashed, and every check that relates
/// events to one another or to the state still runs: parent hashes,
/// identities, duplicate transaction and command IDs, preconditions and the
/// application of after-images. Only the checks that are a pure function of
/// one event's bytes are skipped for events up to the known head, and that
/// prefix is accepted only if its chain ends at exactly the known head hash.
/// Each event names its parent's hash, so equal head hashes mean equal bytes
/// for the whole prefix and its genesis. Events after the known head are
/// verified in full. On any failure, a journal shorter than the known head
/// or a head that differs, the case is replayed again in full and that
/// result, success or error, is returned.
pub fn replay_case_from<'a>(
    fs: &impl DurableFs,
    case_dir: &Path,
    writers: impl Into<JournalWriters<'a>>,
    case_id: Id,
    known: Option<&KnownHead>,
) -> Result<CaseReplay, JournalError> {
    let writers = writers.into();
    if let Some(known) = known
        && let Ok(replay) = replay(fs, case_dir, writers, case_id, Some(known))
    {
        return Ok(replay);
    }
    replay_case(fs, case_dir, writers, case_id)
}

/// Replay and verify a case from its genesis. `writers` is the workspace
/// reading it, or with an inherited prefix, see [`JournalWriters`].
pub fn replay_case<'a>(
    fs: &impl DurableFs,
    case_dir: &Path,
    writers: impl Into<JournalWriters<'a>>,
    case_id: Id,
) -> Result<CaseReplay, JournalError> {
    replay(fs, case_dir, writers.into(), case_id, None)
}

fn replay(
    fs: &impl DurableFs,
    case_dir: &Path,
    writers: JournalWriters<'_>,
    case_id: Id,
    known: Option<&KnownHead>,
) -> Result<CaseReplay, JournalError> {
    if writers
        .inherited
        .is_some_and(|inherited| inherited.case_id != case_id)
    {
        return Err(JournalError::new(
            JournalErrorKind::InvalidIdentity,
            case_dir,
            "The inherited head names another case.",
        ));
    }
    let genesis_path = case_dir.join(GENESIS_FILENAME);
    let genesis_bytes = read_bounded(
        fs,
        &genesis_path,
        MAX_GENESIS_BYTES,
        JournalErrorKind::MissingGenesis,
        JournalErrorKind::CorruptGenesis,
    )?;
    let genesis = decode_genesis(&genesis_path, &genesis_bytes)?;
    genesis.validate(&genesis_path, writers.at(0), case_id)?;
    // A journal file whose bytes are not the bytes this program writes for
    // its content is accepted only once the next event records their hash:
    // a program before ADR 0016 accepted such a file and chained onto it.
    // A reformatted file that nothing records, at the head or anywhere else,
    // is refused at that file.
    let mut unrecorded = (!is_canonical(&genesis, &genesis_bytes)).then(|| genesis_path.clone());

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
    let mut seen = (BTreeSet::new(), BTreeSet::new());
    let events_dir = case_dir.join(EVENTS_DIRECTORY);
    let paths: Vec<_> = ordered_paths(fs, &events_dir)?.into_iter().collect();
    let mut remaining = paths.as_slice();
    while !remaining.is_empty() {
        // Read a window of events in order on this thread, decode it in
        // parallel, then check and apply each event in order. Each decode
        // is a pure function of its bytes, and results are consumed in
        // sequence order, so errors are reported as a sequential replay
        // reports them.
        let mut reads = Vec::new();
        let mut window_bytes = 0;
        for (_, path) in remaining {
            let read = read_bounded(
                fs,
                path,
                MAX_EVENT_BYTES,
                JournalErrorKind::MissingEvent,
                JournalErrorKind::CorruptEvent,
            );
            let stop = match &read {
                Ok(bytes) => {
                    window_bytes += bytes.len();
                    window_bytes >= DECODE_WINDOW_BYTES
                }
                Err(_) => true,
            };
            reads.push(read);
            if stop {
                break;
            }
        }
        let (window, rest) = remaining.split_at(reads.len());
        remaining = rest;
        for ((sequence, path), verified) in window.iter().zip(decode_window(window, &reads, known))
        {
            let sequence = *sequence;
            let expected = result.snapshot.sequence + 1;
            if sequence != expected {
                let missing = events_dir.join(event_filename(expected)?);
                return Err(JournalError::new(
                    JournalErrorKind::MissingEvent,
                    &missing,
                    format!("Found event {sequence} before expected event {expected}."),
                ));
            }
            let (verified, images, canonical) = verified?;
            if let Some(reformatted) = &unrecorded
                && verified.event.previous_hash != result.snapshot.head_hash
            {
                return Err(non_canonical(reformatted));
            }
            // A fork's inherited head records the hash of the origin's bytes:
            // a reformatted file there is named as such, not as a fork that
            // differs from its origin.
            if !canonical
                && writers.inherited.is_some_and(|inherited| {
                    inherited.sequence == sequence && inherited.head_hash != verified.hash
                })
            {
                return Err(non_canonical(path));
            }
            apply_event(
                &mut result,
                &mut seen,
                (verified, images),
                path,
                (writers, case_id),
            )?;
            unrecorded = (!canonical).then(|| path.clone());
        }
    }
    if let Some(reformatted) = unrecorded {
        return Err(non_canonical(&reformatted));
    }
    if known.is_some_and(|known| known.sequence > result.snapshot.sequence) {
        return Err(JournalError::new(
            JournalErrorKind::MissingEvent,
            &events_dir,
            "The journal ends before the verified head.",
        ));
    }
    // A fork's inherited prefix was published before its marker, so a
    // journal shorter than it lost canonical events.
    if writers
        .inherited
        .is_some_and(|inherited| inherited.sequence > result.snapshot.sequence)
    {
        return Err(JournalError::new(
            JournalErrorKind::MissingEvent,
            &events_dir,
            "The journal ends before the head this fork inherited.",
        ));
    }
    Ok(result)
}

/// Decode a case genesis file, without the identity and canonical checks.
fn decode_genesis(path: &Path, bytes: &[u8]) -> Result<CaseGenesis, JournalError> {
    let value = parse(
        bytes,
        path,
        MAX_GENESIS_BYTES,
        JournalErrorKind::CorruptGenesis,
    )?;
    let genesis: CaseGenesis = match version(&value) {
        Some(GENESIS_VERSION) => decode(&value, path, JournalErrorKind::CorruptGenesis)?,
        Some(other) => {
            return Err(unsupported(
                JournalErrorKind::UnsupportedGenesisVersion,
                path,
                other,
            ));
        }
        None => {
            return Err(JournalError::new(
                JournalErrorKind::CorruptGenesis,
                path,
                "Case genesis has no string schema_version.",
            ));
        }
    };
    Ok(genesis)
}

/// Bytes read ahead of decoding at most, beyond one event.
const DECODE_WINDOW_BYTES: usize = 64 << 20;
/// Threads that decode one window at most.
const DECODE_THREADS: usize = 8;

/// Decode one event: in full, or as bytes expected to equal verified ones
/// when it is at or before the known head.
fn decode_one(
    path: &Path,
    sequence: u64,
    bytes: &[u8],
    known: Option<&KnownHead>,
) -> Result<(VerifiedEvent, bool), JournalError> {
    match known {
        Some(known) if sequence <= known.sequence => {
            let verified = decode_known(path, sequence, bytes)?;
            if sequence == known.sequence && verified.hash != known.head_hash {
                return Err(JournalError::new(
                    JournalErrorKind::ParentHashMismatch,
                    path,
                    "The journal differs from the verified head.",
                ));
            }
            // The prefix ends at a head this build verified in full, so its
            // layout was accepted then.
            Ok((verified, true))
        }
        _ => decode_event_layout(path, sequence, bytes),
    }
}

/// Decode a window of read events on up to [`DECODE_THREADS`] threads,
/// returning one result per read, in order, with each event's after-images
/// copied for the state. Each thread takes the next undecoded event when it
/// finishes one, since event sizes vary widely.
fn decode_window(
    window: &[(u64, PathBuf)],
    reads: &[Result<Vec<u8>, JournalError>],
    known: Option<&KnownHead>,
) -> Vec<Result<(VerifiedEvent, SharedImages, bool), JournalError>> {
    let decode = |index: usize| -> Result<(VerifiedEvent, SharedImages, bool), JournalError> {
        let (sequence, path) = &window[index];
        let bytes = reads[index].as_ref().map_err(Clone::clone)?;
        let (verified, canonical) = decode_one(path, *sequence, bytes, known)?;
        let images = SharedImages::of(&verified.event.accepted);
        Ok((verified, images, canonical))
    };
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(DECODE_THREADS)
        .min(reads.len());
    if threads <= 1 {
        return (0..reads.len()).map(decode).collect();
    }
    let next = AtomicUsize::new(0);
    let work = || {
        let mut done = Vec::new();
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            if index >= reads.len() {
                return done;
            }
            done.push((index, decode(index)));
        }
    };
    let mut results: Vec<_> = std::iter::repeat_with(|| None).take(reads.len()).collect();
    std::thread::scope(|scope| {
        // A thread that cannot start leaves its share to the others and to
        // this thread, which takes events too.
        let helpers: Vec<_> = (1..threads)
            .filter_map(|_| std::thread::Builder::new().spawn_scoped(scope, work).ok())
            .collect();
        let mine = work();
        for (index, result) in mine
            .into_iter()
            .chain(helpers.into_iter().flat_map(|helper| {
                helper
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            }))
        {
            results[index] = Some(result);
        }
    });
    results
        .into_iter()
        .map(|result| result.expect("every event is decoded exactly once"))
        .collect()
}

/// Check one decoded event against the chain so far and apply it.
fn apply_event(
    result: &mut CaseReplay,
    seen: &mut (BTreeSet<Id>, BTreeSet<(Actor, String)>),
    (verified, images): (VerifiedEvent, SharedImages),
    path: &Path,
    identity: (JournalWriters<'_>, Id),
) -> Result<(), JournalError> {
    let (transactions, command_keys) = seen;
    let (writers, case_id) = identity;
    let event = &verified.event;
    if event.workspace_id != writers.at(event.sequence) || event.accepted.case_id != case_id {
        return Err(JournalError::new(
            JournalErrorKind::InvalidIdentity,
            path,
            "Event belongs to a different workspace or case.",
        ));
    }
    if let Some(inherited) = writers.inherited
        && inherited.sequence == event.sequence
        && inherited.head_hash != verified.hash
    {
        return Err(JournalError::new(
            JournalErrorKind::ParentHashMismatch,
            path,
            "The journal differs from the head this fork inherited.",
        ));
    }
    if event.previous_hash != result.snapshot.head_hash {
        return Err(JournalError::new(
            JournalErrorKind::ParentHashMismatch,
            path,
            format!(
                "Expected parent {}; stored parent is {}.",
                result.snapshot.head_hash, event.previous_hash
            ),
        ));
    }
    if !transactions.insert(event.transaction_id) {
        return Err(JournalError::new(
            JournalErrorKind::DuplicateTransaction,
            path,
            "Transaction ID already appears in this chain.",
        ));
    }
    if !command_keys.insert((event.actor.clone(), event.command_id.clone())) {
        return Err(JournalError::new(
            JournalErrorKind::DuplicateCommand,
            path,
            "Actor and command ID already appear in this chain.",
        ));
    }
    verify_preconditions(&result.state, event, path)?;
    result
        .state
        .apply_shared(&event.accepted, images)
        .map_err(|error| {
            JournalError::new(JournalErrorKind::InvalidAfterImage, path, error.to_string())
        })?;
    result.snapshot = Snapshot {
        sequence: event.sequence,
        head_hash: verified.hash.clone(),
    };
    result
        .state
        .note_verified_snapshot(result.snapshot.clone())
        .map_err(|error| {
            JournalError::new(JournalErrorKind::InvalidAfterImage, path, error.to_string())
        })?;
    result.events.push(verified);
    Ok(())
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
