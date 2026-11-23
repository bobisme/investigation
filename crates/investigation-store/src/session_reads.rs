//! Evidence reads per actor and declared session, for the inherited trust of
//! authored text (§16.1).
//!
//! `evidence read` under a declared session leaves one small file per distinct
//! Artifact revision in `local/session-reads/`, which holds machine-specific,
//! noncanonical bookkeeping. When a command declares the same session, commit
//! stamps every read recorded so far for that actor and session on the
//! journal event, so readers and replay find them in the canonical journal.
//! Reads recorded after a commit never reach it. Only reads made through this
//! tool are recorded; the session is a self-reported label.
use std::io::{ErrorKind, Read as _};

use super::*;
use investigation_core::inherited_trust::{Session, SessionRead, SessionReads};
use serde::Deserialize;

const ROOT: &str = "local/session-reads";
const VERSION: &str = "inv.session-read.v1";
const ENTRY_SUFFIX: &str = ".json";
const MAX_ENTRY_BYTES: u64 = 4 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    schema_version: String,
    actor: Actor,
    session: Session,
    #[serde(flatten)]
    read: SessionReadEntry,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionReadEntry {
    case_id: Id,
    artifact: RevisionRef,
}

fn hex(text: &str) -> String {
    hash_bytes(text.as_bytes())
        .as_str()
        .trim_start_matches("sha256:")
        .to_owned()
}

fn session_directory(actor: &Actor, session: &Session) -> PathBuf {
    Path::new(ROOT).join(hex(&format!("{actor}\n{session}")))
}

impl<F: DurableFs> Store<F> {
    /// Record that `actor` read `artifact` through `evidence read` under
    /// `session`. Recording the same read again changes nothing.
    pub fn record_session_read(
        &mut self,
        actor: &Actor,
        session: &Session,
        case_id: Id,
        artifact: RevisionRef,
    ) -> Result<(), Failure> {
        let directory = session_directory(actor, session);
        for path in [Path::new(ROOT), directory.as_path()] {
            match self.fs.create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_failure(error)),
            }
        }
        let name = hex(&format!(
            "{case_id}\n{}\n{}",
            artifact.id, artifact.revision
        ));
        let target = directory.join(format!("{name}{ENTRY_SUFFIX}"));
        match self.fs.read(&target) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(io_failure(error)),
        }
        let mut bytes = serde_json::to_vec(&Entry {
            schema_version: VERSION.into(),
            actor: actor.clone(),
            session: session.clone(),
            read: SessionReadEntry { case_id, artifact },
        })
        .map_err(|_| integrity("Could not encode a session read."))?;
        bytes.push(b'\n');
        // A temporary left by an interrupted read is replaced, never published.
        let temporary = directory.join(format!("{name}{ENTRY_SUFFIX}.tmp"));
        match self.fs.remove(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(io_failure(error)),
        }
        write_private(&mut self.fs, &self.gate, &temporary, &bytes, true)?;
        let published = publish(&mut self.fs, &temporary, &target, &self.gate);
        let _ = self.fs.remove(&temporary);
        match published {
            Ok(()) => Ok(()),
            // A concurrent read of the same Artifact recorded it first.
            Err(DurableError::Io(error)) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(durability_failure(error)),
        }
    }

    /// Every read recorded for `actor` and `session`. An entry that cannot be
    /// read still proves an evidence read, so it counts toward the total and
    /// the lowest level without being listed.
    pub fn session_reads(&self, actor: &Actor, session: &Session) -> Result<SessionReads, Failure> {
        let directory = session_directory(actor, session);
        let paths = match self.fs.list(&directory) {
            Ok(paths) => paths,
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(io_failure(error)),
        };
        let mut reads = Vec::new();
        let mut unreadable = 0;
        for path in paths {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.ends_with(ENTRY_SUFFIX) {
                continue;
            }
            let path = directory.join(name);
            let mut bytes = Vec::new();
            let entry = self
                .fs
                .read(&path)
                .and_then(|reader| reader.take(MAX_ENTRY_BYTES).read_to_end(&mut bytes))
                .ok()
                .and_then(|_| serde_json::from_slice::<Entry>(&bytes).ok())
                .filter(|entry| {
                    entry.schema_version == VERSION
                        && &entry.actor == actor
                        && &entry.session == session
                });
            match entry {
                Some(entry) => reads.push(SessionRead {
                    case_id: entry.read.case_id,
                    artifact: entry.read.artifact,
                }),
                None => unreadable += 1,
            }
        }
        Ok(SessionReads::new(session.clone(), reads, unreadable))
    }
}
