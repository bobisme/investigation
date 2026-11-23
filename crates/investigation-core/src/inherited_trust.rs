//! Inherited trust of authored text (§16.1). Pure.
//!
//! Authored text (titles, synopses, notes, rationales, lessons, statements)
//! may have been written after reading hostile evidence. Each record therefore
//! shows the lowest trust level among the inputs it is known to rest on:
//!
//! - the references its body and provenance declare, exact `{id, revision}`
//!   objects anywhere and bare IDs in `*_ref` / `*_refs` fields, each read at
//!   its own inherited level, so a conclusion that cites an assessment of an
//!   observation inherits the observation's level;
//! - the `evidence read` calls that the recording actor made through this tool
//!   under the session its command declared, which the store stamps on the
//!   journal event when it commits ([`SessionReads`]).
//!
//! Every revision of a record counts: text written at revision 1 can survive an
//! amendment. Nothing is inferred. A revision recorded without a declared
//! session contributes [`TrustLevel::Unknown`], and a record with no declared
//! input at all is `unknown`, never trusted. Recorded source content (Sources,
//! Artifacts, Observations and Runs) is `untrusted_external`.
//!
//! The scale is ordered from lowest to highest. This version has no attributed
//! way to raise an Artifact above `untrusted_external`, so a record's level is
//! `untrusted_external` when any known input is recorded source content and
//! `unknown` otherwise. A later level above `unknown` is an additive value.
//!
//! The session is a self-reported claim. The tool cannot see what else a
//! session read, so every display carries [`LIMITS`]. The level is a label for
//! weighing authored text; it does not detect or prevent prompt injection.

use std::{collections::BTreeMap, fmt, str::FromStr};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::{
    command::RevisionRef,
    identity::{Id, Kind},
    state::{Record, State},
};

/// The stated limit shown wherever an inherited trust level is shown.
pub const LIMITS: &str = "Only inputs that records declare and evidence reads made through investigation by the same actor under the session its command declared are known. Investigation cannot see what else that session read. The session is a self-reported claim. This level is a label for weighing authored text; it does not detect or prevent prompt injection.";

/// Most session reads stored on one journal event; the rest are counted.
pub const MAX_SESSION_READS: usize = 64;

/// Longest chain of authored inputs followed before the level is `unknown`.
const MAX_DEPTH: usize = 64;

/// Ordered trust scale, lowest first. Combining inputs takes the minimum.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// Recorded source content: Sources, Artifacts, Observations and Runs,
    /// or text that rests on them.
    UntrustedExternal,
    /// Nothing declared or recorded establishes a level.
    Unknown,
}

impl TrustLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UntrustedExternal => "untrusted_external",
            Self::Unknown => "unknown",
        }
    }
}

/// Recorded source content, whose level does not depend on what it cites.
pub const fn is_recorded_source_content(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Source | Kind::Artifact | Kind::Observation | Kind::Run
    )
}

/// Record kinds whose text is authored by an attributed actor and that carry
/// an inherited trust level.
pub const fn is_authored(kind: Kind) -> bool {
    !is_recorded_source_content(kind)
        && !matches!(
            kind,
            Kind::Transaction | Kind::Handle | Kind::Checkpoint | Kind::Import | Kind::Workspace
        )
}

/// The level of a recorded Artifact read through `evidence read`. Every
/// Artifact is recorded source content; no attributed raise exists yet.
pub const fn artifact_level() -> TrustLevel {
    TrustLevel::UntrustedExternal
}

/// A self-reported session label declared on a command and on `evidence
/// read`. It is a claim that groups reads and writes, never authentication.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Session(String);

impl JsonSchema for Session {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Session".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type":"string", "minLength":1, "maxLength":128, "pattern":"^[A-Za-z0-9:_.-]+$",
            "description":"Self-reported session label. Evidence reads made through investigation by the same actor under this label before the commit are recorded on the transaction for inherited trust."
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionError;

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("session must be 1-128 ASCII letters, digits, ':', '_', '-' or '.'")
    }
}

impl std::error::Error for SessionError {}

impl FromStr for Session {
    type Err = SessionError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty()
            || s.len() > 128
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-' | b'.'))
        {
            return Err(SessionError);
        }
        Ok(Self(s.to_owned()))
    }
}

impl Session {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Session {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Session {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// One Artifact revision read through `evidence read`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRead {
    pub case_id: Id,
    pub artifact: RevisionRef,
}

/// The evidence reads recorded for a command's actor and declared session
/// when the store committed it. Stored on the journal event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionReads {
    pub session: Session,
    /// Distinct reads in (case, artifact) order, at most [`MAX_SESSION_READS`].
    pub reads: Vec<SessionRead>,
    /// Every distinct read recorded for the session at commit.
    pub reads_total: u64,
    /// The lowest level among every recorded read; absent when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lowest: Option<TrustLevel>,
}

impl SessionReads {
    /// Bound and summarize every distinct read recorded for `session`.
    /// `unlisted` counts recorded reads whose entry could not be read: each
    /// still proves an Artifact read, so it counts toward the total and level.
    pub fn new(session: Session, mut reads: Vec<SessionRead>, unlisted: u64) -> Self {
        reads.sort();
        reads.dedup();
        let reads_total = reads.len() as u64 + unlisted;
        let lowest = (reads_total > 0).then(artifact_level);
        reads.truncate(MAX_SESSION_READS);
        Self {
            session,
            reads,
            reads_total,
            lowest,
        }
    }
}

/// Session reads of one case's journal, by acceptance sequence. A sequence
/// with no entry was committed without a declared session.
pub type SessionLedger = BTreeMap<u64, SessionReads>;

/// Where the session reads stamped on a case's journal events are found.
pub trait SessionSource {
    /// The reads stamped on the event at `sequence`, or `None` when that
    /// command declared no session.
    fn session_reads(&self, sequence: u64) -> Option<&SessionReads>;
}

impl SessionSource for SessionLedger {
    fn session_reads(&self, sequence: u64) -> Option<&SessionReads> {
        self.get(&sequence)
    }
}

/// One declared input of a record revision and its level.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeclaredInput {
    pub id: Id,
    /// The exact revision named, or the current one for a bare ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    pub level: TrustLevel,
    /// The input is not a record of this case; its level follows its kind.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub outside_case: bool,
}

/// How one revision of a record stands on session reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RevisionSession {
    pub revision: u64,
    pub acceptance_sequence: u64,
    /// The declared session, or `None` when the command declared none.
    pub session: Option<Session>,
    pub reads_total: u64,
}

/// The inherited trust of one record revision, with what it rests on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InheritedTrust {
    pub level: TrustLevel,
    /// Declared inputs of every revision up to this one, distinct.
    pub inputs: Vec<DeclaredInput>,
    /// One entry per revision up to this one.
    pub sessions: Vec<RevisionSession>,
    /// Distinct recorded session reads of those revisions, bounded per event.
    pub session_reads: Vec<SessionRead>,
}

impl InheritedTrust {
    /// Revisions recorded without a declared session.
    pub fn undeclared_sessions(&self) -> usize {
        self.sessions
            .iter()
            .filter(|session| session.session.is_none())
            .count()
    }

    /// Every distinct recorded read, listed or not, summed per revision.
    pub fn session_reads_total(&self) -> u64 {
        self.sessions
            .iter()
            .map(|session| session.reads_total)
            .sum()
    }
}

/// Computes inherited levels over one case's state and session ledger,
/// memoized per exact revision.
pub struct Resolver<'a> {
    state: &'a State,
    ledger: &'a dyn SessionSource,
    memo: BTreeMap<RevisionRef, TrustLevel>,
}

impl<'a> Resolver<'a> {
    pub fn new(state: &'a State, ledger: &'a dyn SessionSource) -> Self {
        Self {
            state,
            ledger,
            memo: BTreeMap::new(),
        }
    }

    /// The inherited level and its basis for `record`.
    pub fn inherited(&mut self, record: &Record) -> InheritedTrust {
        let mut inputs: Vec<DeclaredInput> = Vec::new();
        let mut sessions = Vec::new();
        let mut session_reads: Vec<SessionRead> = Vec::new();
        for revision in self.lineage(record) {
            for (id, exact) in declared_references(revision) {
                let (level, outside_case) = self.input_level(id, exact, 1);
                let input = DeclaredInput {
                    id,
                    revision: exact,
                    level,
                    outside_case,
                };
                if !inputs.contains(&input) {
                    inputs.push(input);
                }
            }
            let reads = self
                .ledger
                .session_reads(revision.header.acceptance_sequence);
            sessions.push(RevisionSession {
                revision: revision.header.revision.0,
                acceptance_sequence: revision.header.acceptance_sequence,
                session: reads.map(|reads| reads.session.clone()),
                reads_total: reads.map_or(0, |reads| reads.reads_total),
            });
            for read in reads.into_iter().flat_map(|reads| &reads.reads) {
                if !session_reads.contains(read) {
                    session_reads.push(*read);
                }
            }
        }
        let level = self.level(record, 0);
        InheritedTrust {
            level,
            inputs,
            sessions,
            session_reads,
        }
    }

    /// The inherited level of one exact revision.
    pub fn level(&mut self, record: &Record, depth: usize) -> TrustLevel {
        let kind = record.header.id.kind();
        if is_recorded_source_content(kind) {
            return TrustLevel::UntrustedExternal;
        }
        let key = record.reference();
        if let Some(level) = self.memo.get(&key) {
            return *level;
        }
        if depth > MAX_DEPTH {
            return TrustLevel::Unknown;
        }
        // A cycle through bare current-revision references reads as unknown.
        self.memo.insert(key, TrustLevel::Unknown);
        let mut lowest: Option<TrustLevel> = None;
        let mut take = |level: TrustLevel| {
            lowest = Some(lowest.map_or(level, |known| known.min(level)));
        };
        for revision in self.lineage(record) {
            match self
                .ledger
                .session_reads(revision.header.acceptance_sequence)
            {
                None => take(TrustLevel::Unknown),
                Some(reads) => {
                    if let Some(level) = reads.lowest {
                        take(level);
                    }
                }
            }
            for (id, exact) in declared_references(revision) {
                take(self.input_level(id, exact, depth + 1).0);
            }
        }
        // No declared input at all: unknown, never trusted.
        let level = lowest.unwrap_or(TrustLevel::Unknown);
        self.memo.insert(key, level);
        level
    }

    /// Earlier revisions and `record` itself, in revision order.
    fn lineage(&self, record: &Record) -> Vec<&'a Record> {
        let id = record.header.id;
        record
            .header
            .history
            .iter()
            .map(|revision| revision.0)
            .chain(std::iter::once(record.header.revision.0))
            .filter_map(|revision| self.state.revision(RevisionRef { id, revision }))
            .collect()
    }

    /// The level of a declared input, and whether it lies outside this case.
    fn input_level(&mut self, id: Id, exact: Option<u64>, depth: usize) -> (TrustLevel, bool) {
        if is_recorded_source_content(id.kind()) {
            let outside = self.state.current(id).is_none();
            return (TrustLevel::UntrustedExternal, outside);
        }
        let found = match exact {
            Some(revision) => self.state.revision(RevisionRef { id, revision }),
            None => self.state.current(id),
        };
        match found {
            Some(input) => (self.level(input, depth), false),
            // Another case's authored record: this case cannot establish it.
            None => (TrustLevel::Unknown, true),
        }
    }
}

/// Every record reference one revision declares, in body order: exact
/// `{id, revision}` objects anywhere in the body, bare IDs in `*_ref` and
/// `*_refs` fields, and header provenance. References to the record itself
/// and to non-record kinds are skipped.
pub fn declared_references(record: &Record) -> Vec<(Id, Option<u64>)> {
    fn walk(value: &Value, key: Option<&str>, found: &mut Vec<(Id, Option<u64>)>) {
        let referencing = key.is_some_and(|key| key.ends_with("_ref") || key.ends_with("_refs"));
        match value {
            Value::Object(map) => {
                if let (Some(id), Some(revision)) = (
                    map.get("id")
                        .and_then(Value::as_str)
                        .and_then(|id| id.parse::<Id>().ok()),
                    map.get("revision").and_then(Value::as_u64),
                ) {
                    found.push((id, Some(revision)));
                    return;
                }
                for (key, child) in map {
                    walk(child, Some(key), found);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, key, found);
                }
            }
            Value::String(text) if referencing => {
                if let Ok(id) = text.parse::<Id>() {
                    found.push((id, None));
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk(&record.body, None, &mut found);
    found.extend(
        record
            .header
            .provenance
            .references
            .iter()
            .map(|id| (*id, None)),
    );
    let mut out: Vec<(Id, Option<u64>)> = Vec::new();
    for reference in found {
        let kind = reference.0.kind();
        if reference.0 == record.header.id
            || !(is_authored(kind) || is_recorded_source_content(kind))
        {
            continue;
        }
        if !out.contains(&reference) {
            out.push(reference);
        }
    }
    out
}

#[cfg(test)]
mod tests;
