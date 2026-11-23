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
pub fn replay_case_from(
    fs: &impl DurableFs,
    case_dir: &Path,
    workspace_id: Id,
    case_id: Id,
    known: Option<&KnownHead>,
) -> Result<CaseReplay, JournalError> {
    if let Some(known) = known
        && let Ok(replay) = replay(fs, case_dir, workspace_id, case_id, Some(known))
    {
        return Ok(replay);
    }
    replay_case(fs, case_dir, workspace_id, case_id)
}

pub fn replay_case(
    fs: &impl DurableFs,
    case_dir: &Path,
    workspace_id: Id,
    case_id: Id,
) -> Result<CaseReplay, JournalError> {
    replay(fs, case_dir, workspace_id, case_id, None)
}

fn replay(
    fs: &impl DurableFs,
    case_dir: &Path,
    workspace_id: Id,
    case_id: Id,
    known: Option<&KnownHead>,
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
            let verified = verified?;
            apply_event(
                &mut result,
                &mut seen,
                verified,
                path,
                (workspace_id, case_id),
            )?;
        }
    }
    if known.is_some_and(|known| known.sequence > result.snapshot.sequence) {
        return Err(JournalError::new(
            JournalErrorKind::MissingEvent,
            &events_dir,
            "The journal ends before the verified head.",
        ));
    }
    Ok(result)
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
) -> Result<VerifiedEvent, JournalError> {
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
            Ok(verified)
        }
        _ => decode_event(path, sequence, bytes),
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
) -> Vec<Result<(VerifiedEvent, SharedImages), JournalError>> {
    let decode = |index: usize| -> Result<(VerifiedEvent, SharedImages), JournalError> {
        let (sequence, path) = &window[index];
        let bytes = reads[index].as_ref().map_err(Clone::clone)?;
        let verified = decode_one(path, *sequence, bytes, known)?;
        let images = SharedImages::of(&verified.event.accepted);
        Ok((verified, images))
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
    identity: (Id, Id),
) -> Result<(), JournalError> {
    let (transactions, command_keys) = seen;
    let (workspace_id, case_id) = identity;
    let event = &verified.event;
    if event.workspace_id != workspace_id || event.accepted.case_id != case_id {
        return Err(JournalError::new(
            JournalErrorKind::InvalidIdentity,
            path,
            "Event belongs to a different workspace or case.",
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
