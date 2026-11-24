//! `vault snapshot-note` and `vault note-snapshots`: capture the exact bytes
//! of a person-owned vault file a record relies on, and report when the live
//! file no longer matches them (plan §12.1, W11.14).
//!
//! A snapshot is two ordinary commits under one command ID:
//!
//! 1. an ingest of the bytes read from the file (Source with acquisition
//!    context `human note snapshot`, retained and verified Artifact), and
//! 2. an `annotation.add` (role `note`) on the records that rely on it, with
//!    the Artifact and its digest as content and `human_file` naming the
//!    file.
//!
//! The file is read through the projection root without following links
//! ([`investigation_render::human_files`]) and is never written, moved or
//! renamed. Retrying the same `--command-id` after a failure between the two
//! commits finishes the snapshot without retaining the bytes twice.

use investigation_core::{
    PROGRAM_NAME,
    header::ClassificationLevel,
    human_file::{self, NoteSnapshot},
    identity::Id,
    wire::{ErrorCode, Failure},
};
use investigation_render::{
    RenderError,
    human_files::{FileRead, LiveStatus, read_live, read_one},
};
use investigation_store::{Store, Workspace, journal::hash_bytes, store::ingest::IngestVersion};
use serde_json::{Value, json};

use crate::{
    Global, Output, display_argv,
    selection::{select_actor, select_case},
    vault::argv,
};

/// Longest `--command-id`: the annotation commit appends `.annotation`.
const MAX_COMMAND_ID_CHARS: usize = 100;
/// Records one snapshot may name with `--on`.
const MAX_SUBJECTS: usize = 32;
/// The acquisition context every snapshot Source records.
pub(crate) const CONTEXT: &str = "human note snapshot";

pub(crate) const SNAPSHOT_AFTER_HELP: &str = "\
Capture the exact bytes of a person-owned vault file (under Notes/, Boards/
or Inbox/) that a record relies on. The bytes are retained as an Artifact
through an ordinary ingest, and an annotation on each --on record cites that
Artifact and its SHA-256 digest. A later edit to the file changes no record:
vault note-snapshots reports the difference, and exports show the captured
bytes, not the live file. The file itself is only read: never written, moved
or renamed.
PATH is relative to the vault (to the managed prefix in existing-vault mode).
A link anywhere on the path, a special file, a path outside the human folders
and a file over vault.note_snapshot_bytes are refused. The capture keeps the
classification of its records: --classification, or by default the highest
of internal and the --on records' levels. A restricted note never appears in
an internal or public export. The annotation is authored text with no
evidentiary standing.
Examples:
  investigation vault snapshot-note Notes/plan.md --on con_019acd16 --format json
  investigation vault snapshot-note Boards/map.canvas --on dec_019acd16 --classification restricted --command-id board-1
Next: investigation vault note-snapshots --format json";

pub(crate) const LIST_AFTER_HELP: &str = "\
List the human-file snapshots of a case and compare each with the live file:
unchanged, changed (with the live digest and size), missing or refused (for
example a symbolic link now). Nothing is written and no record changes; to
rely on the edited version, take a new snapshot. A snapshot whose record the
working audience may not see is listed as restricted, without its path.
Examples:
  investigation vault note-snapshots --format json
  investigation --case CASE_ID vault note-snapshots
Next: investigation vault snapshot-note Notes/plan.md --on RECORD_ID --format json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum LevelArg {
    Public,
    Internal,
    Restricted,
}

impl LevelArg {
    const fn level(self) -> ClassificationLevel {
        match self {
            Self::Public => ClassificationLevel::Public,
            Self::Internal => ClassificationLevel::Internal,
            Self::Restricted => ClassificationLevel::Restricted,
        }
    }
}

#[derive(clap::Args)]
pub(crate) struct SnapshotNoteArgs {
    /// Vault-relative file under Notes/, Boards/ or Inbox/, for example Notes/plan.md.
    #[arg(value_name = "PATH")]
    path: String,
    /// Record that relies on the file: full ID or unique prefix; repeat for several.
    #[arg(long, value_name = "RECORD_ID", required = true)]
    on: Vec<String>,
    /// Classification of the capture; default the highest of internal and the --on records'.
    #[arg(long, value_enum, value_name = "LEVEL")]
    classification: Option<LevelArg>,
    /// Stable idempotency key (1-100 characters); generated if omitted.
    #[arg(long, value_name = "KEY")]
    command_id: Option<String>,
}

fn invalid(message: impl Into<String>, at: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(at)
}

fn read_failure(workspace: &Workspace, path: &str, error: RenderError) -> Failure {
    match error {
        RenderError::InvalidPath { reason, .. } => invalid(
            format!(
                "Cannot snapshot {path:?}: {reason}. Name a file under Notes/, Boards/ or Inbox/, \
                 relative to the vault."
            ),
            "/path",
        ),
        other => crate::vault::render_failure(workspace, &other),
    }
}

pub(crate) fn snapshot(
    store: &mut Store,
    global: &Global,
    args: &SnapshotNoteArgs,
) -> Result<Output, Failure> {
    let case = select_case(store, global.case.as_deref())?;
    let actor = select_actor(store.workspace(), global.actor.as_ref(), None)?;
    human_file::folder(&args.path).map_err(|reason| {
        invalid(
            format!(
                "Cannot snapshot {:?}: {reason}. Name a file under Notes/, Boards/ or Inbox/, \
                 relative to the vault.",
                args.path
            ),
            "/path",
        )
    })?;
    if args.on.len() > MAX_SUBJECTS {
        return Err(invalid(
            format!("Name at most {MAX_SUBJECTS} records with --on."),
            "/on",
        ));
    }
    if let Some(key) = &args.command_id
        && (key.is_empty()
            || key.chars().count() > MAX_COMMAND_ID_CHARS
            || key.chars().any(char::is_control))
    {
        return Err(invalid(
            format!("Use a command ID of 1-{MAX_COMMAND_ID_CHARS} printable characters."),
            "--command-id",
        ));
    }

    // The records that rely on the file, at their current revisions.
    let replay = store.case(case)?;
    let mut subjects = Vec::new();
    let mut level = ClassificationLevel::Internal;
    for selector in &args.on {
        let id = crate::shortcuts::resolve_any_subject(selector, &replay.state)?;
        let record = replay
            .state
            .current(id)
            .expect("a resolved record is current");
        if subjects
            .iter()
            .any(|subject: &Value| subject["id"] == json!(id))
        {
            return Err(invalid("Name each record once with --on.", "/on"));
        }
        level = level.max(record.header.classification.level);
        subjects.push(json!(record.reference()));
    }
    let level = args.classification.map_or(level, LevelArg::level);

    let bytes = match read_one(store.workspace(), &args.path)
        .map_err(|error| read_failure(store.workspace(), &args.path, error))?
    {
        FileRead::Bytes(bytes) => bytes,
        FileRead::Missing => {
            return Err(Failure::new(
                ErrorCode::UnknownReference,
                format!(
                    "No file {} exists in the vault's person-owned folders. Snapshot an existing \
                     file under Notes/, Boards/ or Inbox/.",
                    args.path
                ),
            )
            .at("/path"));
        }
        FileRead::Refused(reason) => {
            return Err(invalid(
                format!(
                    "Cannot snapshot {}: {reason}. A snapshot never follows a link or reads a \
                     special file, and a file over vault.note_snapshot_bytes is not retained.",
                    args.path
                ),
                "/path",
            ));
        }
    };
    let sha256 = hash_bytes(&bytes);
    // Generated only once nothing is left to refuse before the first write.
    let command_id = match &args.command_id {
        Some(key) => key.clone(),
        None => {
            let generated = crate::shortcuts::generated_command_id();
            crate::shortcuts::announce_generated_id(&generated)?;
            generated
        }
    };

    // 1. Retain the bytes through the ordinary ingest commit.
    let title: String = format!("Human file snapshot: {}", args.path)
        .chars()
        .take(256)
        .collect();
    let manifest = json!({
        "schema_version": IngestVersion::V1,
        "command_id": command_id,
        "actor": actor,
        "case_id": case,
        "source": {
            "title": title,
            "identity": args.path,
            "format": format(&args.path),
            "context": CONTEXT,
            "verification_limits": [
                "Authored by a person outside the journal and captured as found; nothing in it is verified."
            ],
        },
        "content": {"kind":"stdin","name":args.path},
        "classification": level,
    });
    let manifest = serde_json::to_vec(&manifest).expect("an ingest manifest serializes");
    let ingest = store
        .ingest_local_bytes(&manifest, &mut bytes.as_slice())
        .map_err(|failure| crate::with_workspace_repair(store.workspace(), failure))?;
    let item = ingest.items.first().ok_or_else(|| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "The ingest receipt lists no item.",
        )
    })?;
    if item.digest != sha256 {
        return Err(Failure::new(
            ErrorCode::IoFailure,
            format!(
                "{} changed while it was captured (or command ID {command_id} already captured \
                 other bytes). Retry with a new --command-id.",
                args.path
            ),
        )
        .at("/path"));
    }
    let artifact = store
        .case(case)?
        .state
        .current(item.artifact_id)
        .map(|record| record.reference())
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::IntegrityFailure,
                "The captured Artifact is not in the case.",
            )
        })?;

    // 2. Record the reliance.
    let annotation_command = format!("{command_id}.annotation");
    let command = json!({
        "schema_version": "inv.command.v1",
        "command_id": annotation_command,
        "actor": actor,
        "case_id": case,
        "preconditions": [],
        "operations": [{
            "op": "annotation.add",
            "payload": {
                "subject_refs": subjects,
                "content": {"kind":"artifact","artifact_ref":artifact,"digest":sha256},
                "role": "note",
                "classification": {"level":level,"reason":null,"tags":[]},
                "human_file": {"path":args.path},
            },
        }],
    });
    let command = serde_json::to_vec(&command).expect("a command serializes");
    let recorded = store.record(&command).map_err(|mut failure| {
        failure.error.message = format!(
            "{} The bytes are retained as {} (command {command_id}); retry with --command-id \
             {command_id} to record the annotation.",
            failure.error.message, artifact.id
        );
        failure
    })?;
    let annotation = recorded
        .created
        .first()
        .map(|change| change.id)
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::IntegrityFailure,
                "The annotation receipt lists no record.",
            )
        })?;

    let folder = human_file::folder(&args.path).expect("checked above");
    let list = argv(store.workspace(), &["vault", "note-snapshots"]);
    let value = json!({
        "path": args.path,
        "folder": folder,
        "role": human_file::role(folder),
        "owner": "human",
        "sha256": sha256,
        "bytes": bytes.len(),
        "classification": level,
        "case_id": case,
        "subjects": subjects,
        "source_id": item.source_id,
        "artifact": artifact,
        "annotation_id": annotation,
        "command_id": command_id,
        "transactions": [ingest.receipt.transaction_id, recorded.transaction_id],
        "already_committed": [
            ingest.receipt.result == investigation_core::wire::CommitResult::AlreadyCommitted,
            recorded.result == investigation_core::wire::CommitResult::AlreadyCommitted,
        ],
        "file_modified": false,
        "next": [{"kind":"local_read","argv":list}],
    });
    let text = format!(
        "captured  {}  {} bytes  {}\nartifact {}  annotation {}  classification {}\nThe file was only read. A later edit changes no record; vault note-snapshots reports it.\nNext: {}",
        args.path,
        bytes.len(),
        sha256,
        artifact.id,
        annotation,
        value["classification"].as_str().unwrap_or_default(),
        display_argv(&list),
    );
    Ok(Output::success("note_snapshot_recorded", value, text))
}

/// A format hint from the extension; never used to choose a parser.
fn format(path: &str) -> Option<&'static str> {
    let extension = path.rsplit_once('.')?.1;
    match extension {
        "md" => Some("text/markdown"),
        "canvas" | "json" => Some("application/json"),
        "txt" => Some("text/plain"),
        _ => None,
    }
}

pub(crate) fn list(store: &mut Store, global: &Global) -> Result<Output, Failure> {
    use investigation_core::classification::{AudiencePolicy, Classifier, Field};
    let case = select_case(store, global.case.as_deref())?;
    let replay = store.case(case)?;
    let state = &replay.state;
    let classifier = Classifier::new(state);
    let policy = AudiencePolicy::default();
    let workspace = store.workspace();
    let mut items = Vec::new();
    let mut lines = Vec::new();
    let mut counts = [0usize; 5];
    for snapshot in NoteSnapshot::all(state) {
        let record = state
            .revision(snapshot.annotation)
            .expect("a listed snapshot resolves");
        if !classifier.may_show(record, Field::Metadata, &policy) {
            counts[4] += 1;
            items.push(json!({
                "annotation": snapshot.annotation,
                "content_state": "restricted",
            }));
            lines.push(format!("restricted  {}", snapshot.annotation.id));
            continue;
        }
        // The same comparison the working vault's annotation note shows.
        let live = read_live(workspace, &snapshot.file.path)
            .map_err(|error| read_failure(workspace, &snapshot.file.path, error))?
            .status(&snapshot.sha256);
        let detail = match &live {
            LiveStatus::Unchanged => {
                counts[0] += 1;
                json!(null)
            }
            LiveStatus::Changed { sha256, bytes } => {
                counts[1] += 1;
                json!({"sha256":sha256,"bytes":bytes})
            }
            LiveStatus::Missing => {
                counts[2] += 1;
                json!(null)
            }
            LiveStatus::Refused(reason) => {
                counts[3] += 1;
                json!({"reason":reason})
            }
        };
        let status = live.word();
        lines.push(format!(
            "{status:<9}  {}  captured {}  annotation {}",
            snapshot.file.path, snapshot.sha256, snapshot.annotation.id
        ));
        items.push(json!({
            "annotation": snapshot.annotation,
            "path": snapshot.file.path,
            "folder": snapshot.file.folder,
            "role": snapshot.role(),
            "owner": snapshot.file.owner,
            "captured": {"sha256":snapshot.sha256,"bytes":snapshot.bytes,"artifact":snapshot.artifact},
            "subjects": snapshot.subjects,
            "classification": snapshot.classification,
            "live": status,
            "live_file": detail,
        }));
    }
    let case_text = case.to_string();
    let snapshot_argv = |id: Id| -> Vec<String> {
        argv(workspace, &["--case", &case_text, "show", &id.to_string()])
    };
    let next: Vec<Value> = items
        .iter()
        .find(|item| item["live"] == "changed")
        .and_then(|item| item["annotation"]["id"].as_str()?.parse::<Id>().ok())
        .map(|id| json!({"kind":"local_read","argv":snapshot_argv(id)}))
        .into_iter()
        .collect();
    let value = json!({
        "case_id": case,
        "snapshot": replay.snapshot,
        "snapshots": items,
        "counts": {
            "unchanged": counts[0],
            "changed": counts[1],
            "missing": counts[2],
            "refused": counts[3],
            "restricted": counts[4],
        },
        "records_changed": false,
        "next": next,
    });
    let mut text = format!(
        "{} snapshot(s): {} unchanged, {} changed, {} missing, {} refused, {} restricted",
        lines.len(),
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        counts[4]
    );
    for line in lines {
        text.push('\n');
        text.push_str(&line);
    }
    if counts[1] > 0 {
        text.push_str(&format!(
            "\nA changed file changes no record: each record still relies on the captured bytes. To rely on the edited version, take a new snapshot ({PROGRAM_NAME} vault snapshot-note PATH --on RECORD_ID)."
        ));
    }
    Ok(Output::success("note_snapshots", value, text))
}
