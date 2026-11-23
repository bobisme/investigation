//! Disposable SQLite projection of verified journal events. Mutation validation
//! and idempotency decisions always use canonical journal state.

use std::{path::Path, time::Duration};

use investigation_core::{
    command::RevisionRef,
    evidence::AcquisitionKey,
    freshness::dependency_edges,
    header::Actor,
    identity::{Id, Kind},
    retrospective::Relation,
    state::AcceptedTransaction,
    wire::{Digest, ErrorCode, Failure, Receipt, Snapshot},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::store::ingest::acquisition::AcquisitionCursor;

// Version 2 added acquisitions; version 3 adds the record dependency index.
// Older indexes are rebuilt.
const INDEX_USER_VERSION: u32 = 3;
const BUSY_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Clone, Copy)]
pub struct IndexEvent<'a> {
    pub previous_hash: &'a Digest,
    pub actor: &'a Actor,
    pub accepted: &'a AcceptedTransaction,
    pub receipt: &'a Receipt,
    pub relations: &'a [Relation],
}

pub struct Projection {
    database: Connection,
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
        let database = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(db_error)?;
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
        Self { database }.heads()
    }

    pub fn open(path: &Path, workspace: Id) -> Result<Self, Failure> {
        let database = Connection::open(path).map_err(db_error)?;
        database.busy_timeout(BUSY_TIMEOUT).map_err(db_error)?;
        database
            .execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;")
            .map_err(db_error)?;
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
                     case_id TEXT PRIMARY KEY, sequence INTEGER NOT NULL, head_hash TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS receipts (
                     actor TEXT NOT NULL, command_id TEXT NOT NULL, case_id TEXT NOT NULL,
                     receipt_json TEXT NOT NULL, PRIMARY KEY (actor, command_id)
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
                 CREATE TABLE IF NOT EXISTS dependencies (
                     case_id TEXT NOT NULL,
                     dependent_id TEXT NOT NULL, dependent_revision INTEGER NOT NULL,
                     field TEXT NOT NULL, propagation TEXT NOT NULL,
                     target_id TEXT NOT NULL, target_revision INTEGER NOT NULL,
                     PRIMARY KEY (dependent_id, dependent_revision, field, target_id, target_revision)
                 );
                 CREATE INDEX IF NOT EXISTS dependencies_targets
                     ON dependencies (case_id, target_id, target_revision);",
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
        database
            .pragma_update(None, "user_version", INDEX_USER_VERSION)
            .map_err(db_error)?;
        Ok(Self { database })
    }

    pub fn head(&self, case_id: Id) -> Result<Option<Snapshot>, Failure> {
        load_head(&self.database, case_id)
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
            let case_id = id
                .parse()
                .map_err(|_| invalid("Index contains an invalid case ID."))?;
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
            "INSERT INTO heads (case_id, sequence, head_hash) VALUES (?1, ?2, ?3)
             ON CONFLICT (case_id) DO UPDATE SET sequence=excluded.sequence, head_hash=excluded.head_hash",
            params![case_id, sequence, snapshot.head_hash.as_str()],
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
        let mut fresh = Self::open(temporary.path(), workspace)?;
        for event in events {
            fresh.apply(event)?;
        }
        drop(fresh);
        temporary
            .persist(path)
            .map_err(|_| index_failure("Could not replace the disposable index."))?;
        Self::open(path, workspace)
    }
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
            transaction_id: "tx_018d87fc-5234-7000-8000-000000000055".parse().unwrap(),
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
            previous_hash: &genesis_hash,
            actor: &actor,
            accepted: &genesis,
            receipt: &first_receipt,
            relations: &[],
        };
        let second = IndexEvent {
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
    fn version_one_indexes_are_not_reused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.sqlite");
        let workspace_id = "ws_018d87fc-5234-7000-8000-000000000097".parse().unwrap();
        drop(Projection::open(&path, workspace_id).unwrap());
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 1)
            .unwrap();
        assert!(Projection::open(&path, workspace_id).is_err());
        assert!(Projection::inspect(&path, workspace_id).is_err());
    }

    #[test]
    fn bundled_sqlite_supports_fts5() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE VIRTUAL TABLE smoke USING fts5(content)")
            .unwrap();
    }
}
