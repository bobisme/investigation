//! Disposable SQLite projection of verified journal events. Mutation validation
//! uses canonical journal state. For the cases a commit does not load, its
//! idempotency and ID checks read this index only while the indexed head of
//! each such case is its canonical head (ADR 0011, `store::scope`).

use std::{path::Path, time::Duration};

use investigation_core::{
    command::RevisionRef,
    evidence::AcquisitionKey,
    freshness::dependency_edges,
    header::Actor,
    identity::{Id, Kind},
    retrospective::Relation,
    search_text::{TextLabel, indexed_texts},
    state::AcceptedTransaction,
    wire::{Digest, ErrorCode, Failure, Receipt, Snapshot},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::store::ingest::acquisition::AcquisitionCursor;

// Version 2 added acquisitions; version 3 added the record dependency index;
// version 4 added full-text search over labelled record text; version 5 added
// the transaction IDs that case-scoped commits check (ADR 0011); version 6
// adds the cases that claim each acquisition and producer attempt, so keyed
// ingest, capture begin and producer runs are case-scoped too (PERF.09).
// Older indexes are rebuilt.
pub const INDEX_USER_VERSION: u32 = 6;
/// Rows read from the full-text index for one query, before canonical checks.
pub const MAX_SEARCH_CANDIDATES: usize = 5_000;
const BUSY_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Clone, Copy)]
pub struct IndexEvent<'a> {
    /// The identity of the published event file (`FileIdentity::key`), kept
    /// with the head so a later freshness test can tell a different file at
    /// the same sequence (PERF.09). `None` leaves the head unconfirmed: a
    /// case-scoped commit then verifies that case.
    pub head_file: Option<&'a str>,
    pub previous_hash: &'a Digest,
    pub actor: &'a Actor,
    pub accepted: &'a AcceptedTransaction,
    pub receipt: &'a Receipt,
    pub relations: &'a [Relation],
}

pub struct Projection {
    database: Connection,
}

/// A workspace-wide identity that a recording claims, which one case may
/// hold: an ingest acquisition attempt or a producer attempt. The index lists
/// every case whose applied events recorded a revision that claims it; that
/// list may name a case that no longer does, never omit one that does.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Claim {
    /// A Source's `acquisition_key`: producer namespace and external ID.
    Acquisition {
        namespace: String,
        external_id: String,
    },
    /// A Run's producer name and external run ID, by either recording path.
    Attempt {
        producer: String,
        external_run_id: String,
    },
}

impl Claim {
    fn columns(&self) -> (&'static str, &str, &str) {
        match self {
            Self::Acquisition {
                namespace,
                external_id,
            } => ("acquisition", namespace, external_id),
            Self::Attempt {
                producer,
                external_run_id,
            } => ("attempt", producer, external_run_id),
        }
    }

    /// What a recorded revision claims, if anything.
    fn of(record: &investigation_core::state::Record) -> Option<Self> {
        match record.header.id.kind() {
            Kind::Source => {
                let key: AcquisitionKey =
                    serde_json::from_value(record.body.get("acquisition_key")?.clone()).ok()?;
                Some(Self::Acquisition {
                    namespace: key.namespace,
                    external_id: key.external_id,
                })
            }
            Kind::Run => {
                let (identity, _) = investigation_core::attempt::recorded_attempt(&record.body)?;
                Some(Self::Attempt {
                    producer: identity.producer.to_owned(),
                    external_run_id: identity.external_run_id.to_owned(),
                })
            }
            _ => None,
        }
    }
}

/// One indexed field whose text contains every query term. A pointer for the
/// caller to verify against canonical state, never a result by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchCandidate {
    pub case_id: Id,
    pub record: RevisionRef,
    pub field: String,
    pub label: TextLabel,
}

/// One exact reference from a record revision to the revision it cites.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredDependency {
    pub dependent: RevisionRef,
    pub field: String,
    pub propagation: String,
    pub target: RevisionRef,
}

impl Projection {
    pub fn inspect(path: &Path, workspace: Id) -> Result<Vec<(Id, Snapshot)>, Failure> {
        Self::open_read_only(path, workspace)?.heads()
    }

    /// Open an existing index of this workspace and version for reading
    /// only. Creates and changes nothing; an absent, older or foreign index
    /// is an error.
    pub fn open_read_only(path: &Path, workspace: Id) -> Result<Self, Failure> {
        let database = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(db_error)?;
        database.busy_timeout(BUSY_TIMEOUT).map_err(db_error)?;
        let version: u32 = database
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(db_error)?;
        let stored: String = database
            .query_row(
                "SELECT workspace_id FROM metadata WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if version != INDEX_USER_VERSION || stored != workspace.to_string() {
            return Err(invalid(
                "Index identity or version differs from this workspace.",
            ));
        }
        Ok(Self { database })
    }

    /// Open the index for writing, creating it when absent. The caller holds
    /// the store's commit lock, as every index writer does (ADR 0015).
    pub fn open(path: &Path, workspace: Id) -> Result<Self, Failure> {
        if !path.exists() {
            // A WAL or journal left beside a deleted database belongs to that
            // database; SQLite pairs them by name and would read it into the
            // new one (ADR 0015).
            remove_companions(path)?;
        }
        Self::open_with(path, workspace, Durability::Live)
    }

    fn open_with(path: &Path, workspace: Id, durability: Durability) -> Result<Self, Failure> {
        let database = Connection::open(path).map_err(db_error)?;
        database.busy_timeout(BUSY_TIMEOUT).map_err(db_error)?;
        database
            .execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;")
            .map_err(db_error)?;
        if let Durability::Unpublished = durability {
            // A temporary nobody reads until it is synced and renamed.
            database
                .execute_batch("PRAGMA journal_mode=MEMORY; PRAGMA synchronous=OFF;")
                .map_err(db_error)?;
        }
        let version: u32 = database
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(db_error)?;
        if version != 0 && version != INDEX_USER_VERSION {
            return Err(invalid("Index uses a different schema version."));
        }
        database
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS metadata (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     workspace_id TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS heads (
                     case_id TEXT PRIMARY KEY, sequence INTEGER NOT NULL, head_hash TEXT NOT NULL,
                     head_file TEXT
                 );
                 CREATE TABLE IF NOT EXISTS receipts (
                     actor TEXT NOT NULL, command_id TEXT NOT NULL, case_id TEXT NOT NULL,
                     receipt_json TEXT NOT NULL, PRIMARY KEY (actor, command_id)
                 );
                 CREATE TABLE IF NOT EXISTS transactions (
                     transaction_id TEXT PRIMARY KEY, case_id TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS relations (
                     case_id TEXT NOT NULL, kind TEXT NOT NULL,
                     source_id TEXT NOT NULL, source_revision INTEGER NOT NULL,
                     target_id TEXT NOT NULL, target_revision INTEGER NOT NULL,
                     attribution_id TEXT NOT NULL, attribution_revision INTEGER NOT NULL,
                     PRIMARY KEY (
                         kind, source_id, source_revision, target_id, target_revision,
                         attribution_id, attribution_revision
                     )
                 );
                 CREATE INDEX IF NOT EXISTS relations_dependents
                     ON relations (case_id, target_id, target_revision);
                 CREATE TABLE IF NOT EXISTS acquisitions (
                     namespace TEXT NOT NULL, external_id TEXT NOT NULL, item TEXT NOT NULL,
                     case_id TEXT NOT NULL, source_id TEXT NOT NULL,
                     source_revision INTEGER NOT NULL, description_digest TEXT,
                     acceptance_sequence INTEGER NOT NULL,
                     PRIMARY KEY (namespace, external_id, item)
                 );
                 CREATE TABLE IF NOT EXISTS claims (
                     kind TEXT NOT NULL, name TEXT NOT NULL, external_id TEXT NOT NULL,
                     case_id TEXT NOT NULL,
                     PRIMARY KEY (kind, name, external_id, case_id)
                 );
                 CREATE TABLE IF NOT EXISTS dependencies (
                     case_id TEXT NOT NULL,
                     dependent_id TEXT NOT NULL, dependent_revision INTEGER NOT NULL,
                     field TEXT NOT NULL, propagation TEXT NOT NULL,
                     target_id TEXT NOT NULL, target_revision INTEGER NOT NULL,
                     PRIMARY KEY (dependent_id, dependent_revision, field, target_id, target_revision)
                 );
                 CREATE INDEX IF NOT EXISTS dependencies_targets
                     ON dependencies (case_id, target_id, target_revision);
                 CREATE TABLE IF NOT EXISTS search_fields (
                     row INTEGER PRIMARY KEY, case_id TEXT NOT NULL,
                     record_id TEXT NOT NULL, revision INTEGER NOT NULL,
                     field TEXT NOT NULL, label TEXT NOT NULL, text TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS search_fields_record ON search_fields (record_id);
                 CREATE VIRTUAL TABLE IF NOT EXISTS search_text USING fts5(
                     text, content='search_fields', content_rowid='row',
                     tokenize='unicode61 remove_diacritics 0'
                 );",
            )
            .map_err(db_error)?;
        database
            .execute(
                "INSERT OR IGNORE INTO metadata (singleton, workspace_id) VALUES (1, ?1)",
                [workspace.to_string()],
            )
            .map_err(db_error)?;
        let stored: String = database
            .query_row(
                "SELECT workspace_id FROM metadata WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if stored != workspace.to_string() {
            return Err(invalid("Index belongs to a different workspace."));
        }
        // Rewriting an unchanged version is a write transaction with its own
        // syncs on every open, so it is set only on a new index.
        if version != INDEX_USER_VERSION {
            database
                .pragma_update(None, "user_version", INDEX_USER_VERSION)
                .map_err(db_error)?;
        }
        Ok(Self { database })
    }

    pub fn head(&self, case_id: Id) -> Result<Option<Snapshot>, Failure> {
        load_head(&self.database, case_id)
    }

    /// The case whose applied events hold `(actor, command_id)`, if any. A
    /// pointer for the caller to verify against canonical state.
    pub fn command_case(&self, actor: &Actor, command_id: &str) -> Result<Option<Id>, Failure> {
        self.database
            .query_row(
                "SELECT case_id FROM receipts WHERE actor = ?1 AND command_id = ?2",
                params![actor.to_string(), command_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_error)?
            .map(|id| parse_case_id(&id))
            .transpose()
    }

    /// The cases whose applied events created record `id` or accepted
    /// transaction `id`, by the ID's kind.
    pub fn id_cases(&self, id: Id) -> Result<Vec<Id>, Failure> {
        let table = if id.kind() == Kind::Transaction {
            "transactions".to_owned()
        } else {
            format!("record_{}", id.kind().prefix())
        };
        // Record tables are created with the first record of their kind.
        let exists: bool = self
            .database
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [&table],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !exists {
            return Ok(Vec::new());
        }
        let column = if id.kind() == Kind::Transaction {
            "transaction_id"
        } else {
            "id"
        };
        let mut query = self
            .database
            .prepare(&format!(
                "SELECT DISTINCT case_id FROM {table} WHERE {column} = ?1"
            ))
            .map_err(db_error)?;
        let rows = query
            .query_map([id.to_string()], |row| row.get::<_, String>(0))
            .map_err(db_error)?;
        rows.map(|row| parse_case_id(&row.map_err(db_error)?))
            .collect()
    }

    /// The cases whose applied events created a record of `kind` whose ID
    /// starts with `prefix`. A pointer for the caller to verify against
    /// canonical state (bn-1lau).
    pub fn id_prefix_cases(&self, kind: Kind, prefix: &str) -> Result<Vec<Id>, Failure> {
        let table = format!("record_{}", kind.prefix());
        let exists: bool = self
            .database
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [&table],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !exists {
            return Ok(Vec::new());
        }
        let mut query = self
            .database
            .prepare(&format!(
                "SELECT DISTINCT case_id FROM {table} WHERE substr(id, 1, length(?1)) = ?1"
            ))
            .map_err(db_error)?;
        let rows = query
            .query_map([prefix], |row| row.get::<_, String>(0))
            .map_err(db_error)?;
        rows.map(|row| parse_case_id(&row.map_err(db_error)?))
            .collect()
    }

    /// The cases whose applied events recorded a revision that makes
    /// `claim`. A pointer for the caller to verify against canonical state.
    pub fn claim_cases(&self, claim: &Claim) -> Result<Vec<Id>, Failure> {
        let (kind, name, external_id) = claim.columns();
        let mut query = self
            .database
            .prepare(
                "SELECT case_id FROM claims
                 WHERE kind = ?1 AND name = ?2 AND external_id = ?3 ORDER BY case_id",
            )
            .map_err(db_error)?;
        let rows = query
            .query_map(params![kind, name, external_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(db_error)?;
        rows.map(|row| parse_case_id(&row.map_err(db_error)?))
            .collect()
    }

    /// Each indexed head with the identity of the event file it was applied
    /// from, when one was recorded.
    pub fn head_files(&self) -> Result<Vec<(Id, Snapshot, Option<String>)>, Failure> {
        let files: std::collections::BTreeMap<String, Option<String>> = {
            let mut query = self
                .database
                .prepare("SELECT case_id, head_file FROM heads")
                .map_err(db_error)?;
            let rows = query
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(db_error)?;
            rows.collect::<Result<_, _>>().map_err(db_error)?
        };
        self.heads()?
            .into_iter()
            .map(|(case_id, snapshot)| {
                let file = files.get(&case_id.to_string()).cloned().flatten();
                Ok((case_id, snapshot, file))
            })
            .collect()
    }

    /// Record the event file identity of heads that did not move, such as
    /// after the workspace was copied: one transaction for all of them.
    pub fn confirm_head_files(&mut self, files: &[(Id, String)]) -> Result<(), Failure> {
        if files.is_empty() {
            return Ok(());
        }
        let tx = self
            .database
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        for (case_id, file) in files {
            tx.execute(
                "UPDATE heads SET head_file = ?2 WHERE case_id = ?1",
                params![case_id.to_string(), file],
            )
            .map_err(db_error)?;
        }
        tx.commit().map_err(db_error)
    }

    /// Inspect projections without taking them as proof of canonical state.
    pub fn heads(&self) -> Result<Vec<(Id, Snapshot)>, Failure> {
        let mut query = self
            .database
            .prepare("SELECT case_id, sequence, head_hash FROM heads ORDER BY case_id")
            .map_err(db_error)?;
        let rows = query
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(db_error)?;
        rows.map(|row| {
            let (id, sequence, digest) = row.map_err(db_error)?;
            let case_id = parse_case_id(&id)?;
            let head_hash = digest
                .parse()
                .map_err(|_| invalid("Index contains an invalid head digest."))?;
            Ok((
                case_id,
                Snapshot {
                    sequence: read_counter(sequence)?,
                    head_hash,
                },
            ))
        })
        .collect()
    }

    /// Apply one already verified event and update its head atomically. A
    /// failed write rolls back records, relations, receipt, and head together.
    pub fn apply(&mut self, event: IndexEvent<'_>) -> Result<(), Failure> {
        let accepted = event.accepted;
        let receipt = event.receipt;
        let snapshot = &receipt.snapshot;
        if snapshot.sequence != accepted.sequence {
            return Err(invalid(
                "Index event and receipt reference different sequences.",
            ));
        }
        let sequence = sql_counter(snapshot.sequence)?;
        let tx = self
            .database
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let current = load_head(&tx, accepted.case_id)?;
        match current {
            Some(head)
                if head.sequence.checked_add(1) == Some(snapshot.sequence)
                    && &head.head_hash == event.previous_hash => {}
            None if snapshot.sequence == 1 => {}
            _ => return Err(invalid("Index event does not extend the applied head.")),
        }
        let case_id = accepted.case_id.to_string();
        for record in &accepted.records {
            let table = format!("record_{}", record.header.id.kind().prefix());
            tx.execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS {table} (
                     id TEXT NOT NULL, revision INTEGER NOT NULL, case_id TEXT NOT NULL,
                     acceptance_sequence INTEGER NOT NULL, record_json TEXT NOT NULL,
                     PRIMARY KEY (id, revision)
                 );
                 CREATE INDEX IF NOT EXISTS {table}_case ON {table} (case_id, id, revision);"
            ))
            .map_err(db_error)?;
            let record_json = serde_json::to_string(record).map_err(codec_error)?;
            tx.execute(
                &format!(
                    "INSERT INTO {table}
                        (id, revision, case_id, acceptance_sequence, record_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)"
                ),
                params![
                    record.header.id.to_string(),
                    sql_counter(record.header.revision.0)?,
                    case_id,
                    sequence,
                    record_json
                ],
            )
            .map_err(db_error)?;
            let edges = dependency_edges(record)
                .map_err(|_| invalid("Accepted record has unreadable dependency references."))?;
            for edge in edges {
                tx.execute(
                    "INSERT OR IGNORE INTO dependencies
                         (case_id, dependent_id, dependent_revision, field, propagation,
                         target_id, target_revision)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        case_id,
                        record.header.id.to_string(),
                        sql_counter(record.header.revision.0)?,
                        edge.field,
                        edge.propagation.as_str(),
                        edge.target.id.to_string(),
                        sql_counter(edge.target.revision)?
                    ],
                )
                .map_err(db_error)?;
            }
            replace_search_text(&tx, &case_id, record)?;
        }
        for record in &accepted.records {
            if record.header.id.kind() != Kind::Source {
                continue;
            }
            // Import cursor: the latest Source revision of each acquisition item.
            let Some(key) = record
                .body
                .get("acquisition_key")
                .and_then(|key| serde_json::from_value::<AcquisitionKey>(key.clone()).ok())
            else {
                continue;
            };
            tx.execute(
                "INSERT INTO acquisitions
                     (namespace, external_id, item, case_id, source_id, source_revision,
                     description_digest, acceptance_sequence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (namespace, external_id, item) DO UPDATE SET
                     case_id=excluded.case_id, source_id=excluded.source_id,
                     source_revision=excluded.source_revision,
                     description_digest=excluded.description_digest,
                     acceptance_sequence=excluded.acceptance_sequence",
                params![
                    key.namespace,
                    key.external_id,
                    key.item,
                    case_id,
                    record.header.id.to_string(),
                    sql_counter(record.header.revision.0)?,
                    record
                        .body
                        .get("description_digest")
                        .and_then(|value| value.as_str()),
                    sequence
                ],
            )
            .map_err(db_error)?;
        }
        for claim in accepted.records.iter().filter_map(Claim::of) {
            let (kind, name, external_id) = claim.columns();
            tx.execute(
                "INSERT OR IGNORE INTO claims (kind, name, external_id, case_id)
                 VALUES (?1, ?2, ?3, ?4)",
                params![kind, name, external_id, case_id],
            )
            .map_err(db_error)?;
        }
        for edge in event.relations {
            let kind = serde_json::to_value(edge.kind).map_err(codec_error)?;
            tx.execute(
                "INSERT INTO relations
                     (case_id, kind, source_id, source_revision, target_id, target_revision,
                     attribution_id, attribution_revision)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    case_id,
                    kind.as_str(),
                    edge.source.id.to_string(),
                    sql_counter(edge.source.revision)?,
                    edge.target.id.to_string(),
                    sql_counter(edge.target.revision)?,
                    edge.attributed_to.id.to_string(),
                    sql_counter(edge.attributed_to.revision)?
                ],
            )
            .map_err(db_error)?;
        }
        tx.execute(
            "INSERT INTO transactions (transaction_id, case_id) VALUES (?1, ?2)",
            params![receipt.transaction_id.to_string(), case_id],
        )
        .map_err(db_error)?;
        tx.execute(
            "INSERT INTO receipts (actor, command_id, case_id, receipt_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                event.actor.to_string(),
                receipt.command_id,
                case_id,
                serde_json::to_string(receipt).map_err(codec_error)?
            ],
        )
        .map_err(db_error)?;
        tx.execute(
            "INSERT INTO heads (case_id, sequence, head_hash, head_file) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (case_id) DO UPDATE SET sequence=excluded.sequence,
                 head_hash=excluded.head_hash, head_file=excluded.head_file",
            params![
                case_id,
                sequence,
                snapshot.head_hash.as_str(),
                event.head_file
            ],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)
    }

    /// Recorded acquisition items of one producer namespace, ordered by external
    /// ID and item: a resumable importer's cursor. Disposable like the index.
    pub fn acquisitions(&self, namespace: &str) -> Result<Vec<AcquisitionCursor>, Failure> {
        let mut query = self
            .database
            .prepare(
                "SELECT external_id, item, case_id, source_id, source_revision,
                        description_digest, acceptance_sequence
                 FROM acquisitions WHERE namespace = ?1 ORDER BY external_id, item",
            )
            .map_err(db_error)?;
        let rows = query
            .query_map([namespace], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(db_error)?;
        rows.map(|row| {
            let (external_id, item, case_id, source_id, revision, digest, sequence) =
                row.map_err(db_error)?;
            let parse_id = |value: String| {
                value
                    .parse::<Id>()
                    .map_err(|_| invalid("Index contains an invalid acquisition ID."))
            };
            Ok(AcquisitionCursor {
                key: AcquisitionKey {
                    namespace: namespace.to_owned(),
                    external_id,
                    item,
                },
                case_id: parse_id(case_id)?,
                source: RevisionRef {
                    id: parse_id(source_id)?,
                    revision: read_counter(revision)?,
                },
                description_digest: digest
                    .map(|digest| {
                        digest
                            .parse()
                            .map_err(|_| invalid("Index contains an invalid description digest."))
                    })
                    .transpose()?,
                acceptance_sequence: read_counter(sequence)?,
            })
        })
        .collect()
    }

    /// Every indexed record revision that cites any revision of `target`.
    /// Callers decide which dependent revisions are current.
    pub fn dependents(&self, case_id: Id, target: Id) -> Result<Vec<StoredDependency>, Failure> {
        let mut query = self
            .database
            .prepare(
                "SELECT dependent_id, dependent_revision, field, propagation, target_id,
                     target_revision
                 FROM dependencies WHERE case_id = ?1 AND target_id = ?2
                 ORDER BY dependent_id, dependent_revision, field, target_revision",
            )
            .map_err(db_error)?;
        let rows = query
            .query_map([case_id.to_string(), target.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(db_error)?;
        let reference = |id: String, revision: i64| -> Result<RevisionRef, Failure> {
            Ok(RevisionRef {
                id: id
                    .parse()
                    .map_err(|_| invalid("Index contains an invalid record ID."))?,
                revision: read_counter(revision)?,
            })
        };
        rows.map(|row| {
            let (dependent, revision, field, propagation, target, target_revision) =
                row.map_err(db_error)?;
            Ok(StoredDependency {
                dependent: reference(dependent, revision)?,
                field,
                propagation,
                target: reference(target, target_revision)?,
            })
        })
        .collect()
    }

    /// Indexed fields whose text holds every term, best match first. Only the
    /// latest indexed revision of each record is present. Terms come from
    /// `search_text::query_terms`, so no index query syntax reaches SQLite.
    pub fn search(
        &self,
        terms: &[String],
        case_id: Option<Id>,
    ) -> Result<(Vec<SearchCandidate>, bool), Failure> {
        if terms.is_empty()
            || terms
                .iter()
                .any(|term| term.is_empty() || !term.chars().all(char::is_alphanumeric))
        {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                "Supply at least one word or number to search for.",
            )
            .at("/query"));
        }
        let expression = terms
            .iter()
            .map(|term| format!("\"{term}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let mut query = self
            .database
            .prepare(
                "SELECT f.case_id, f.record_id, f.revision, f.field, f.label
                 FROM search_text JOIN search_fields f ON f.row = search_text.rowid
                 WHERE search_text MATCH ?1 AND (?2 IS NULL OR f.case_id = ?2)
                 ORDER BY bm25(search_text), f.case_id, f.record_id, f.field
                 LIMIT ?3",
            )
            .map_err(db_error)?;
        let limit = sql_counter(MAX_SEARCH_CANDIDATES as u64 + 1)?;
        let rows = query
            .query_map(
                params![expression, case_id.map(|id| id.to_string()), limit],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .map_err(db_error)?;
        let mut candidates = Vec::new();
        for row in rows {
            let (case, record, revision, field, label) = row.map_err(db_error)?;
            let parse = |value: String| {
                value
                    .parse::<Id>()
                    .map_err(|_| invalid("Index contains an invalid search record ID."))
            };
            candidates.push(SearchCandidate {
                case_id: parse(case)?,
                record: RevisionRef {
                    id: parse(record)?,
                    revision: read_counter(revision)?,
                },
                field,
                label: TextLabel::parse(&label)
                    .ok_or_else(|| invalid("Index contains an unknown text label."))?,
            });
        }
        let truncated = candidates.len() > MAX_SEARCH_CANDIDATES;
        candidates.truncate(MAX_SEARCH_CANDIDATES);
        Ok((candidates, truncated))
    }

    /// Rebuild a fresh index and replace the disposable database only after
    /// every frame has been applied. The store holds its coordination lock.
    pub fn rebuild<'a>(
        path: &Path,
        workspace: Id,
        events: impl IntoIterator<Item = IndexEvent<'a>>,
    ) -> Result<Self, Failure> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("Index path has no directory."))?;
        let temporary = tempfile::Builder::new()
            .prefix(".index-")
            .tempfile_in(parent)
            .map_err(|_| index_failure("Could not create a temporary index."))?;
        // Built without syncs: nothing reads it before the one sync below.
        let mut fresh = Self::open_with(temporary.path(), workspace, Durability::Unpublished)?;
        for event in events {
            fresh.apply(event)?;
        }
        fresh
            .database
            .close()
            .map_err(|(_, error)| db_error(error))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| index_failure("Could not sync the rebuilt index."))?;
        // A journal left by a crashed writer belongs to the old database, and
        // SQLite pairs files by name: it would roll old pages into the new
        // one. Remove the old database first, then its journal, so that no
        // crash leaves either a torn old database without its journal or the
        // new one with the old journal (ADR 0015).
        let sync_parent = || {
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| index_failure("Could not sync the index directory."))
        };
        match std::fs::remove_file(path) {
            Ok(()) => sync_parent()?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(index_failure("Could not replace the disposable index.")),
        }
        remove_companions(path)?;
        temporary
            .persist(path)
            .map_err(|_| index_failure("Could not replace the disposable index."))?;
        sync_parent()?;
        Self::open(path, workspace)
    }
}

/// How an index connection trades durability for syncs (ADR 0015).
#[derive(Clone, Copy)]
enum Durability {
    /// `cache/index.sqlite`: rollback journal, full sync.
    Live,
    /// A rebuild's temporary file, synced once before it is published.
    Unpublished,
}

/// The files SQLite pairs with database `path` by name.
pub fn companions(path: &Path) -> [std::path::PathBuf; 3] {
    ["-wal", "-shm", "-journal"].map(|suffix| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        std::path::PathBuf::from(name)
    })
}

fn remove_companions(path: &Path) -> Result<(), Failure> {
    for companion in companions(path) {
        match std::fs::remove_file(&companion) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(index_failure("Could not remove a stale index log.")),
        }
    }
    Ok(())
}

/// Keep only the newest revision's text for a record. The index is a
/// downstream copy of journal text: retention or purge of canonical text must
/// be followed by `repair index`, which rebuilds it from what remains.
fn replace_search_text(
    tx: &Connection,
    case_id: &str,
    record: &investigation_core::state::Record,
) -> Result<(), Failure> {
    let id = record.header.id.to_string();
    let stale: Vec<(i64, String)> = {
        let mut query = tx
            .prepare("SELECT row, text FROM search_fields WHERE record_id = ?1")
            .map_err(db_error)?;
        query
            .query_map([&id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(db_error)?
            .collect::<Result<_, _>>()
            .map_err(db_error)?
    };
    for (row, text) in stale {
        tx.execute(
            "INSERT INTO search_text (search_text, rowid, text) VALUES ('delete', ?1, ?2)",
            params![row, text],
        )
        .map_err(db_error)?;
        tx.execute("DELETE FROM search_fields WHERE row = ?1", [row])
            .map_err(db_error)?;
    }
    let revision = sql_counter(record.header.revision.0)?;
    for text in indexed_texts(record) {
        tx.execute(
            "INSERT INTO search_fields (case_id, record_id, revision, field, label, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                case_id,
                id,
                revision,
                text.field,
                text.label.as_str(),
                text.text
            ],
        )
        .map_err(db_error)?;
        tx.execute(
            "INSERT INTO search_text (rowid, text) VALUES (?1, ?2)",
            params![tx.last_insert_rowid(), text.text],
        )
        .map_err(db_error)?;
    }
    Ok(())
}

fn load_head(database: &Connection, case_id: Id) -> Result<Option<Snapshot>, Failure> {
    database
        .query_row(
            "SELECT sequence, head_hash FROM heads WHERE case_id = ?1",
            [case_id.to_string()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(db_error)?
        .map(|(sequence, value)| {
            let head_hash = value
                .parse()
                .map_err(|_| invalid("Index contains an invalid head digest."))?;
            Ok(Snapshot {
                sequence: read_counter(sequence)?,
                head_hash,
            })
        })
        .transpose()
}

fn parse_case_id(value: &str) -> Result<Id, Failure> {
    value
        .parse::<Id>()
        .ok()
        .filter(|id| id.kind() == Kind::Case)
        .ok_or_else(|| invalid("Index contains an invalid case ID."))
}

fn sql_counter(counter: u64) -> Result<i64, Failure> {
    i64::try_from(counter).map_err(|_| invalid("Index counter exceeds SQLite's integer range."))
}

fn read_counter(counter: i64) -> Result<u64, Failure> {
    u64::try_from(counter).map_err(|_| invalid("Index contains a negative counter."))
}

fn db_error(_: rusqlite::Error) -> Failure {
    index_failure("SQLite could not read or update the disposable index.")
}

fn codec_error(_: serde_json::Error) -> Failure {
    index_failure("Could not project an accepted record into the index.")
}

fn invalid(reason: &'static str) -> Failure {
    Failure::new(ErrorCode::IntegrityFailure, reason).with_argv(repair_argv())
}

fn index_failure(reason: &'static str) -> Failure {
    Failure::new(ErrorCode::IoFailure, reason).with_argv(repair_argv())
}

fn repair_argv() -> Vec<String> {
    ["investigation", "repair", "index", "--format", "json"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use investigation_core::{
        catalog::standard_catalog,
        command::Command,
        header::SystemClock,
        identity::SystemIdSource,
        state::{Acceptance, State, validate_and_apply},
        wire::{CommitResult, Projection as ReceiptProjection, ProjectionStatus, ResponseVersion},
    };
    use serde_json::{Value, json};
    use std::fs;

    fn digest(character: char) -> Digest {
        format!("sha256:{}", character.to_string().repeat(64))
            .parse()
            .unwrap()
    }

    fn append(state: &mut State, case_id: Option<Id>, operations: Value) -> AcceptedTransaction {
        let command: Command = serde_json::from_value(json!({
            "schema_version":"inv.command.v0-alpha",
            "command_id":"index-test",
            "case_id":case_id,
            "actor":"agent:test",
            "preconditions":[],
            "operations":operations
        }))
        .unwrap();
        let accepted = validate_and_apply(
            state,
            &command,
            &standard_catalog(),
            Acceptance {
                sequence: state.sequence() + 1,
                clock: &mut SystemClock,
                ids: &mut SystemIdSource,
            },
        )
        .unwrap();
        state.apply(&accepted).unwrap();
        accepted
    }

    fn receipt(accepted: &AcceptedTransaction, command_id: &str, hash: Digest) -> Receipt {
        Receipt {
            schema_version: ResponseVersion::V1,
            ok: true,
            result: CommitResult::Committed,
            command_id: command_id.to_owned(),
            // One transaction per head hash: the index keeps them unique.
            transaction_id: format!(
                "tx_018d87fc-5234-7000-8000-00000000{}",
                &hash.as_str()[7..11]
            )
            .parse()
            .unwrap(),
            snapshot: Snapshot {
                sequence: accepted.sequence,
                head_hash: hash,
            },
            created: accepted.created.clone(),
            updated: accepted.updated.clone(),
            artifacts: Vec::new(),
            observation_checks: Vec::new(),
            projection: ReceiptProjection {
                index: ProjectionStatus::Current,
                vault: ProjectionStatus::Disabled,
            },
            warnings: Vec::new(),
            next: Vec::new(),
        }
    }

    #[test]
    fn index_rolls_back_an_event_and_rebuilds_exact_receipts() {
        let mut state = State::default();
        let genesis = append(
            &mut state,
            None,
            json!([
                {"op":"case.create", "payload":{"title":"Index fixture", "mode":"record"}}
            ]),
        );
        let case_id = genesis.case_id;
        let questions = append(
            &mut state,
            Some(case_id),
            json!([
                {"op":"question.create", "payload":{"wording":"What is the original observation?", "importance":"normal"}},
                {"op":"question.create", "payload":{"wording":"Which caveat remains open?", "importance":"normal"}}
            ]),
        );
        let genesis_hash = digest('1');
        let first_receipt = receipt(&genesis, "first", digest('2'));
        let second_receipt = receipt(&questions, "second", digest('3'));
        let actor = "agent:test".parse().unwrap();
        let first = IndexEvent {
            head_file: None,
            previous_hash: &genesis_hash,
            actor: &actor,
            accepted: &genesis,
            receipt: &first_receipt,
            relations: &[],
        };
        let second = IndexEvent {
            head_file: None,
            previous_hash: &first_receipt.snapshot.head_hash,
            actor: &actor,
            accepted: &questions,
            receipt: &second_receipt,
            relations: &[],
        };
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.sqlite");
        let workspace_id = "ws_018d87fc-5234-7000-8000-000000000099".parse().unwrap();
        let mut index = Projection::open(&path, workspace_id).unwrap();
        index.apply(first).unwrap();

        index
            .database
            .execute_batch(
                "CREATE TRIGGER fail_receipt BEFORE INSERT ON receipts
             BEGIN SELECT RAISE(ABORT, 'injected projection failure'); END;",
            )
            .unwrap();
        assert!(index.apply(second).is_err());
        assert_eq!(
            index.head(case_id).unwrap(),
            Some(first_receipt.snapshot.clone())
        );
        let partial: i64 = index
            .database
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='record_q'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(partial, 0);

        index
            .database
            .execute_batch("DROP TRIGGER fail_receipt")
            .unwrap();
        index.apply(second).unwrap();
        assert!(index.apply(second).is_err());
        drop(index);
        let rebuilt = Projection::rebuild(&path, workspace_id, [first, second]).unwrap();
        assert_eq!(
            rebuilt.heads().unwrap(),
            vec![(case_id, second_receipt.snapshot.clone())]
        );
        let stored: String = rebuilt
            .database
            .query_row(
                "SELECT receipt_json FROM receipts WHERE command_id='second'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Receipt>(&stored).unwrap(),
            second_receipt
        );
        let questions: i64 = rebuilt
            .database
            .query_row("SELECT COUNT(*) FROM record_q", [], |row| row.get(0))
            .unwrap();
        assert_eq!(questions, 2);
    }

    /// Two events of one case and their receipts, for the durability tests.
    fn two_events() -> (State, [AcceptedTransaction; 2], [Receipt; 2], Digest) {
        let mut state = State::default();
        let genesis = append(
            &mut state,
            None,
            json!([{"op":"case.create", "payload":{"title":"Durability", "mode":"record"}}]),
        );
        let question = append(
            &mut state,
            Some(genesis.case_id),
            json!([{"op":"question.create", "payload":{"wording":"Which pages?", "importance":"normal"}}]),
        );
        let receipts = [
            receipt(&genesis, "first", digest('2')),
            receipt(&question, "second", digest('3')),
        ];
        (state, [genesis, question], receipts, digest('1'))
    }

    fn integrity(index: &Projection) -> String {
        index
            .database
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap()
    }

    /// ADR 0015: commits trust the index for cross-case idempotency, so each
    /// index transaction must be atomic across power loss. The live index
    /// keeps SQLite's rollback journal with full sync; a weaker setting
    /// (`synchronous` NORMAL with a rollback journal, or OFF) fails here.
    #[test]
    fn the_live_index_keeps_a_fully_synced_rollback_journal() {
        let (_state, accepted, receipts, genesis_hash) = two_events();
        let actor = "agent:test".parse().unwrap();
        let events = [0, 1].map(|at| IndexEvent {
            head_file: None,
            previous_hash: if at == 0 {
                &genesis_hash
            } else {
                &receipts[0].snapshot.head_hash
            },
            actor: &actor,
            accepted: &accepted[at],
            receipt: &receipts[at],
            relations: &[],
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.sqlite");
        let workspace_id = "ws_018d87fc-5234-7000-8000-000000000099".parse().unwrap();
        let settings = |index: &Projection| -> (String, i64) {
            let mode = index
                .database
                .pragma_query_value(None, "journal_mode", |row| row.get(0))
                .unwrap();
            let sync = index
                .database
                .pragma_query_value(None, "synchronous", |row| row.get(0))
                .unwrap();
            (mode, sync)
        };
        let live = Projection::open(&path, workspace_id).unwrap();
        assert_eq!(settings(&live), ("delete".to_owned(), 2));
        drop(live);
        // A rebuild builds its unpublished file without syncs, but what it
        // publishes is opened like any live index.
        let rebuilt = Projection::rebuild(&path, workspace_id, events).unwrap();
        assert_eq!(settings(&rebuilt), ("delete".to_owned(), 2));
        assert_eq!(integrity(&rebuilt), "ok");
        let mut left: Vec<_> = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, vec!["index.sqlite"], "no temporary or log is left");
    }

    /// ADR 0015: SQLite pairs a database with `-journal` by name. A writer
    /// that crashed mid-transaction leaves a hot journal of the old index; a
    /// rebuild must not let it roll old pages into the new file, and an
    /// index deleted beside it starts clean.
    #[test]
    fn a_crashed_writers_journal_never_reaches_a_rebuilt_index() {
        let (_state, accepted, receipts, genesis_hash) = two_events();
        let actor = "agent:test".parse().unwrap();
        let events = [0, 1].map(|at| IndexEvent {
            head_file: None,
            previous_hash: if at == 0 {
                &genesis_hash
            } else {
                &receipts[0].snapshot.head_hash
            },
            actor: &actor,
            accepted: &accepted[at],
            receipt: &receipts[at],
            relations: &[],
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.sqlite");
        let journal = directory.path().join("index.sqlite-journal");
        let crashed = directory.path().join("crashed");
        fs::create_dir(&crashed).unwrap();
        let workspace_id = "ws_018d87fc-5234-7000-8000-000000000099".parse().unwrap();
        let mut index = Projection::open(&path, workspace_id).unwrap();
        index.apply(events[0]).unwrap();
        drop(index);
        // A writer changes the receipts and heads, then spills a large
        // transaction to the database file, so the synced journal holds
        // their original pages; copy both as a crash leaves them.
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "PRAGMA cache_size=2; BEGIN;
                 DELETE FROM receipts; DELETE FROM heads;
                 CREATE TABLE filler(bytes BLOB);
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<200)
                 INSERT INTO filler SELECT randomblob(3000) FROM n;",
            )
            .unwrap();
        assert!(journal.exists(), "the writer spilled to its journal");
        fs::copy(&path, crashed.join("index.sqlite")).unwrap();
        fs::copy(&journal, crashed.join("index.sqlite-journal")).unwrap();
        writer.execute_batch("ROLLBACK").unwrap();
        drop(writer);
        let crash = || {
            fs::copy(crashed.join("index.sqlite"), &path).unwrap();
            fs::copy(crashed.join("index.sqlite-journal"), &journal).unwrap();
        };

        crash();
        let rebuilt = Projection::rebuild(&path, workspace_id, events).unwrap();
        assert_eq!(integrity(&rebuilt), "ok");
        assert_eq!(
            rebuilt.heads().unwrap(),
            vec![(accepted[0].case_id, receipts[1].snapshot.clone())]
        );
        drop(rebuilt);
        // Rolled into the new file, the old journal would leave the new head
        // over the old receipts: the torn state that hides a key.
        let reopened = Projection::open(&path, workspace_id).unwrap();
        assert_eq!(integrity(&reopened), "ok");
        assert_eq!(
            reopened.heads().unwrap(),
            vec![(accepted[0].case_id, receipts[1].snapshot.clone())]
        );
        let held: i64 = reopened
            .database
            .query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(held, 2, "every receipt under the head is there");
        assert!(!journal.exists());
        drop(reopened);

        // A crash between removing the old index and its journal.
        crash();
        fs::remove_file(&path).unwrap();
        let fresh = Projection::open(&path, workspace_id).unwrap();
        assert_eq!(integrity(&fresh), "ok");
        assert_eq!(fresh.heads().unwrap(), Vec::new());
    }

    #[test]
    fn dependency_index_is_rebuilt_from_journal_events() {
        let mut state = State::default();
        let genesis = append(
            &mut state,
            None,
            json!([{"op":"case.create", "payload":{"title":"Dependency fixture", "mode":"record"}}]),
        );
        let case_id = genesis.case_id;
        let question = append(
            &mut state,
            Some(case_id),
            json!([{"op":"question.create", "payload":{"wording":"Which queue?", "importance":"normal"}}]),
        );
        let question_ref = question.records[0].reference();
        let options = append(
            &mut state,
            Some(case_id),
            json!([
                {"op":"option.create", "payload":{"question_ref":question_ref,"description":"Managed","dependencies":[],"tradeoff_notes":[]}},
                {"op":"option.create", "payload":{"question_ref":question_ref,"description":"Broker","dependencies":[],"tradeoff_notes":[]}}
            ]),
        );
        let receipts = [
            receipt(&genesis, "dep-1", digest('2')),
            receipt(&question, "dep-2", digest('3')),
            receipt(&options, "dep-3", digest('4')),
        ];
        let genesis_hash = digest('1');
        let actor = "agent:test".parse().unwrap();
        let accepted = [&genesis, &question, &options];
        let events: Vec<_> = (0..3)
            .map(|i| IndexEvent {
                head_file: None,
                previous_hash: if i == 0 {
                    &genesis_hash
                } else {
                    &receipts[i - 1].snapshot.head_hash
                },
                actor: &actor,
                accepted: accepted[i],
                receipt: &receipts[i],
                relations: &[],
            })
            .collect();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.sqlite");
        let workspace_id = "ws_018d87fc-5234-7000-8000-000000000098".parse().unwrap();
        let mut index = Projection::open(&path, workspace_id).unwrap();
        for event in &events {
            index.apply(*event).unwrap();
        }
        let mut expected: Vec<_> = options
            .records
            .iter()
            .map(|record| StoredDependency {
                dependent: record.reference(),
                field: "question_ref".into(),
                propagation: "change_only".into(),
                target: question_ref,
            })
            .collect();
        expected.sort_by_key(|row| row.dependent.id);
        assert_eq!(
            index.dependents(case_id, question_ref.id).unwrap(),
            expected
        );
        drop(index);
        let rebuilt = Projection::rebuild(&path, workspace_id, events).unwrap();
        assert_eq!(
            rebuilt.dependents(case_id, question_ref.id).unwrap(),
            expected
        );
        assert!(rebuilt.dependents(case_id, case_id).unwrap().is_empty());
    }

    #[test]
    fn search_index_holds_labelled_current_text_and_is_rebuilt() {
        let mut state = State::default();
        let genesis = append(
            &mut state,
            None,
            json!([{"op":"case.create", "payload":{"title":"Search fixture", "mode":"record"}}]),
        );
        let case_id = genesis.case_id;
        let question = append(
            &mut state,
            Some(case_id),
            json!([{"op":"question.create", "payload":{"wording":"Why did the pool stall?", "importance":"normal"}}]),
        );
        let question_ref = question.records[0].reference();
        let synopsis = append(
            &mut state,
            Some(case_id),
            json!([{"op":"annotation.add", "payload":{
                "subject_refs":[question_ref],"role":"synopsis",
                "content":{"kind":"text","text":"Pool exhaustion is the leading account."}
            }}]),
        );
        assert_eq!(synopsis.records[0].body["role"], "synopsis");
        let mut amended = question.clone();
        amended.sequence = 4;
        amended.records[0].header.revision = investigation_core::header::Revision(2);
        amended.records[0].body["wording"] = json!("Why did the queue stall?");
        let accepted = [&genesis, &question, &synopsis, &amended];
        let receipts: Vec<_> = accepted
            .iter()
            .enumerate()
            .map(|(i, accepted)| {
                receipt(
                    accepted,
                    &format!("search-{i}"),
                    digest(char::from(b'2' + i as u8)),
                )
            })
            .collect();
        let genesis_hash = digest('1');
        let actor = "agent:test".parse().unwrap();
        let events: Vec<_> = (0..accepted.len())
            .map(|i| IndexEvent {
                head_file: None,
                previous_hash: if i == 0 {
                    &genesis_hash
                } else {
                    &receipts[i - 1].snapshot.head_hash
                },
                actor: &actor,
                accepted: accepted[i],
                receipt: &receipts[i],
                relations: &[],
            })
            .collect();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.sqlite");
        let workspace_id = "ws_018d87fc-5234-7000-8000-000000000096".parse().unwrap();
        let mut index = Projection::open(&path, workspace_id).unwrap();
        for event in &events[..3] {
            index.apply(*event).unwrap();
        }
        let terms = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        let (pool, truncated) = index.search(&terms(&["pool"]), Some(case_id)).unwrap();
        assert!(!truncated);
        let mut labels: Vec<_> = pool
            .iter()
            .map(|hit| (hit.record.id.kind(), hit.field.as_str(), hit.label))
            .collect();
        labels.sort();
        let mut expected = vec![
            (Kind::Question, "wording", TextLabel::AuthoredText),
            (
                Kind::Annotation,
                "content.text",
                TextLabel::EditorialSummary,
            ),
        ];
        expected.sort();
        assert_eq!(labels, expected);
        assert!(pool.iter().all(|hit| !hit.label.counts_as_source_hit()));

        // A new revision replaces the old text: no stale pointer remains.
        index.apply(events[3]).unwrap();
        let (pool, _) = index.search(&terms(&["pool"]), Some(case_id)).unwrap();
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0].label, TextLabel::EditorialSummary);
        let (queue, _) = index.search(&terms(&["queue", "stall"]), None).unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].record.revision, 2);
        let other = "case_018d87fc-5234-7000-8000-000000000095".parse().unwrap();
        assert!(
            index
                .search(&terms(&["queue"]), Some(other))
                .unwrap()
                .0
                .is_empty()
        );
        assert!(index.search(&terms(&["\"queue\" OR x"]), None).is_err());

        drop(index);
        let rebuilt = Projection::rebuild(&path, workspace_id, events.clone()).unwrap();
        assert_eq!(
            rebuilt.search(&terms(&["queue", "stall"]), None).unwrap().0,
            queue
        );
        assert_eq!(rebuilt.search(&terms(&["pool"]), None).unwrap().0.len(), 1);
    }

    #[test]
    fn older_index_versions_are_not_reused() {
        // Version 3 lacks the search tables and version 4 the transaction
        // IDs, so they must be rebuilt too.
        for version in [1, 3, 4] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("index.sqlite");
            let workspace_id = "ws_018d87fc-5234-7000-8000-000000000097".parse().unwrap();
            drop(Projection::open(&path, workspace_id).unwrap());
            Connection::open(&path)
                .unwrap()
                .pragma_update(None, "user_version", version)
                .unwrap();
            assert!(Projection::open(&path, workspace_id).is_err());
            assert!(Projection::inspect(&path, workspace_id).is_err());
        }
    }

    #[test]
    fn bundled_sqlite_supports_fts5() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE VIRTUAL TABLE smoke USING fts5(content)")
            .unwrap();
    }
}
