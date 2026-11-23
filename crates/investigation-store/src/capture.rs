//! Durable capture declarations (`capture.begin`) and their private staging.
//!
//! A declaration commits one transaction through the operation catalog: a
//! `record`-mode `case.create` when the producer asks for a new case, then a
//! `run.start` in the `declared` state. The Run body carries the capture binding
//! under `capture`: the handle, the producer namespace and external run ID that
//! identify the logical attempt, the handle limits and the staging path. The
//! journal keeps the recorder's request in an `inv.capture.v1` envelope, so an
//! identical retry returns the original receipt and a changed request under the
//! same key conflicts.
//!
//! # Staging layout
//!
//! ```text
//! state/staging/<run-id>/          0700, created after the declaration commits
//!   handle.json                    0400, binding of this directory to its handle
//!   handle.json.tmp                transient; only while handle.json is published
//! ```
//!
//! `handle.json` holds `schema_version` (`inv.capture-staging.v1`), `handle`,
//! `run_id`, `case_id`, `transaction_id`, `command_id`, `actor`, `producer` and
//! `external_run_id`. Its bytes are a pure function of the committed declaration,
//! so a retry can verify or recreate it. Later capture operations add their own
//! entries beside it; they never rewrite it.
//!
//! Staging is durable recovery material bound to a committed handle, not cache.
//! Publication-temporary cleanup only looks in `state/tmp` and never reclaims
//! it. The one path that removes staging is explicit retirement ([`retire`]),
//! and only for a finalized Run whose bytes the canonical store holds. A crash
//! after the commit and before allocation leaves a committed handle without
//! staging; [`Store::capture_handles`] reports that, and resending the
//! identical `begin` allocates it.
//!
//! Appended facts and attachment descriptors are staged items with per-handle
//! positions; see [`items`] for their layout and durability rules, and
//! [`attach`] for how an attachment's bytes are copied and verified.

use std::io::ErrorKind;

use investigation_core::{
    attempt::{self, AttemptIdentity, AttemptPath},
    command::{Command, CommandVersion, Operation as CommandOperation},
    header::ClassificationLevel,
    state::{Record, validate_and_apply, validate_and_apply_composed},
};

use super::*;
use crate::durable::sync_existing_file;
use serde::Deserialize;

/// Journal operation names in the capture request family.
///
/// `capture.append` and `capture.attach` are absent by design: their items are
/// staged per handle ([`items`]) and summarized into the manifest that
/// finalization ([`finalize`]) commits, never journaled one transaction per item.
pub const CAPTURE_OPERATIONS: &[&str] = &["capture.begin", finalize::FINALIZE_OPERATION];
/// Root of every handle's staging directory, relative to the workspace.
pub const STAGING_ROOT: &str = "state/staging";
/// The binding file in each staging directory.
pub const BINDING_FILE: &str = "handle.json";
const STAGING_VERSION: &str = "inv.capture-staging.v1";
const RUN_ALIAS: &str = "run";
const CASE_ALIAS: &str = "case";

/// Version tag of a capture request in the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureProtocol {
    #[serde(rename = "inv.capture.v1")]
    V1,
}

/// Journal envelope of a capture request. The payload is the recorder's request,
/// opaque to the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRequest {
    pub schema_version: CaptureProtocol,
    pub operation: String,
    pub command_id: String,
    pub actor: Actor,
    pub case_id: Option<Id>,
    pub payload: Value,
}

/// Limits a handle was declared with. They are fixed at `begin` so a retry
/// returns the same receipt even if a later build changes its defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandleLimits {
    pub max_event_bytes: u64,
    pub max_attachment_bytes: u64,
    pub max_items: u64,
}

impl HandleLimits {
    /// Limits for handles declared by this build.
    pub const DEFAULT: Self = Self {
        max_event_bytes: 64 * 1024,
        max_attachment_bytes: ingest::MAX_IMPORT_BYTES,
        max_items: 100_000,
    };
}

/// The capture binding stored on the Run body under `capture`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureBinding {
    pub protocol: CaptureProtocol,
    pub handle: Id,
    pub producer: String,
    pub external_run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_plan_digest: Option<investigation_core::wire::Digest>,
    pub limits: HandleLimits,
    /// Workspace-relative staging directory.
    pub staging: String,
}

/// Case that receives a declared Run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclarationTarget {
    Existing(Id),
    /// A minimal `record`-mode case created in the same transaction.
    NewRecordCase {
        title: String,
    },
}

/// A capture declaration, already validated by the recorder.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureDeclaration {
    pub command_id: String,
    pub actor: Actor,
    pub target: DeclarationTarget,
    /// `run.start` payload; the catalog validates it like any other.
    pub run: Value,
    /// Producer namespace and external run ID: the logical attempt.
    pub producer: String,
    pub external_run_id: String,
    pub external_plan_digest: Option<investigation_core::wire::Digest>,
    pub classification: ClassificationLevel,
    /// Normalized recorder request retained in the journal.
    pub request: Value,
}

/// A committed declaration with allocated staging.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredCapture {
    pub receipt: Receipt,
    pub case_id: Id,
    pub run_id: Id,
    pub binding: CaptureBinding,
}

/// Whether a handle's staging directory holds its binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StagingState {
    Present,
    /// Committed but never allocated, after a crash; resend the identical begin.
    Missing,
}

/// One declared handle, as recovery reads it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeclaredHandle {
    pub case_id: Id,
    pub run_id: Id,
    pub command_id: String,
    pub actor: Actor,
    pub binding: CaptureBinding,
    /// The declaring request payload, as the recorder sent it.
    pub request: Value,
    pub staging: StagingState,
}

#[derive(Serialize)]
struct StagingBinding<'a> {
    schema_version: &'a str,
    handle: Id,
    run_id: Id,
    case_id: Id,
    transaction_id: Id,
    command_id: &'a str,
    actor: &'a Actor,
    producer: &'a str,
    external_run_id: &'a str,
}

impl<F: DurableFs> Store<F> {
    /// Commit one Run declaration, then allocate its staging directory.
    ///
    /// The commit lock is held only for the commit and, separately, for the
    /// few filesystem calls that allocate staging; never while a producer acts.
    pub fn declare_capture(
        &mut self,
        declaration: &CaptureDeclaration,
    ) -> Result<DeclaredCapture, Failure> {
        let existing = match &declaration.target {
            DeclarationTarget::Existing(case_id) => Some(*case_id),
            DeclarationTarget::NewRecordCase { .. } => None,
        };
        let request = CaptureRequest {
            schema_version: CaptureProtocol::V1,
            operation: "capture.begin".into(),
            command_id: declaration.command_id.clone(),
            actor: declaration.actor.clone(),
            case_id: existing,
            payload: declaration.request.clone(),
        };
        let input = serde_json::to_value(&request)
            .map_err(|_| integrity("Could not encode the capture request."))?;
        let mut declared = None;
        let receipt = self
            .commit_plan(
                CommitPlan {
                    case_id: existing,
                    event: EventInput {
                        actor: &declaration.actor,
                        command_id: &declaration.command_id,
                        preconditions: &[],
                        manifest_digest: None,
                        session_reads: None,
                    },
                    request: SemanticRequest {
                        input,
                        attachments: Vec::new(),
                    },
                    blobs: &[],
                },
                &mut SystemClock,
                &mut SystemIdSource,
                |state, catalog, acceptance, visible| {
                    if let Some(prior) = prior_declaration(visible, declaration) {
                        return Err(prior);
                    }
                    current_name(declaration)?;
                    let (accepted, binding) =
                        declare(state, catalog, acceptance, declaration, existing)?;
                    declared = Some(binding);
                    Ok(Prepared::Accept(accepted))
                },
            )
            .map_err(|failure| {
                if failure.error.code == ErrorCode::DurabilityUnknown {
                    failure.with_argv(receipt_argv(&declaration.actor, &declaration.command_id))
                } else {
                    failure
                }
            })?;
        let run_id = created(&receipt, Kind::Run)?;
        let case_id = match existing {
            Some(case_id) => case_id,
            None => created(&receipt, Kind::Case)?,
        };
        let (binding, finalized) = match declared {
            Some(binding) => (binding, false),
            // An identical earlier request: read the binding it committed.
            None => {
                let replay = self.case(case_id)?;
                let finalized = replay.state.current(run_id).is_some_and(late::is_finalized);
                (binding_of(&replay.state, run_id)?, finalized)
            }
        };
        // A finalized handle never gets staging again: its manifest answers for
        // it, and an empty directory would read as an open handle.
        if finalized {
            return Ok(DeclaredCapture {
                receipt,
                case_id,
                run_id,
                binding,
            });
        }
        self.allocate_staging(&StagingBinding {
            schema_version: STAGING_VERSION,
            handle: binding.handle,
            run_id,
            case_id,
            transaction_id: receipt.transaction_id,
            command_id: &declaration.command_id,
            actor: &declaration.actor,
            producer: &binding.producer,
            external_run_id: &binding.external_run_id,
        })?;
        Ok(DeclaredCapture {
            receipt,
            case_id,
            run_id,
            binding,
        })
    }

    /// Every declared handle in the workspace, with its staging state.
    ///
    /// This is the read that recovery builds on; it never changes anything.
    pub fn capture_handles(&mut self) -> Result<Vec<DeclaredHandle>, Failure> {
        let replay = {
            let _lock = self.coordinate()?;
            self.load_verified()?
        };
        self.declared_handles(&replay)
    }

    /// Every declared handle in a verified replay, with its staging state.
    fn declared_handles(&self, replay: &WorkspaceReplay) -> Result<Vec<DeclaredHandle>, Failure> {
        let mut handles = Vec::new();
        for (&case_id, case) in &replay.cases {
            for record in case.state.records() {
                if record.header.id.kind() != Kind::Run || record.body.get("capture").is_none() {
                    continue;
                }
                let run_id = record.header.id;
                let binding = binding_of(&case.state, run_id)?;
                let event = declaring_event(case, &case.state, run_id)?;
                let request: CaptureRequest =
                    investigation_core::input::decode(&event.request.input)
                        .map_err(|_| integrity("A capture declaration has no capture request."))?;
                let staging = match self
                    .fs
                    .read(&Path::new(&binding.staging).join(BINDING_FILE))
                {
                    Ok(_) => StagingState::Present,
                    Err(error) if error.kind() == ErrorKind::NotFound => StagingState::Missing,
                    Err(error) => return Err(io_failure(error)),
                };
                handles.push(DeclaredHandle {
                    case_id,
                    run_id,
                    command_id: event.command_id.clone(),
                    actor: event.actor.clone(),
                    binding,
                    request: request.payload,
                    staging,
                });
            }
        }
        handles.sort_by_key(|handle| (handle.binding.handle, handle.run_id));
        Ok(handles)
    }

    fn allocate_staging(&mut self, binding: &StagingBinding<'_>) -> Result<(), Failure> {
        let bytes = binding.bytes()?;
        let root = Path::new(STAGING_ROOT);
        let directory = root.join(binding.run_id.to_string());
        let target = directory.join(BINDING_FILE);
        let temporary = directory.join(format!("{BINDING_FILE}.tmp"));
        // Serialize allocation with every other writer; no producer code runs here.
        let _lock = self.coordinate()?;
        self.gate.check().map_err(durability_failure)?;
        for path in [root, directory.as_path()] {
            match self.fs.create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_failure(error)),
            }
        }
        match read_all(&self.fs, &target) {
            Ok(existing) if existing == bytes => {
                sync_existing_file(&mut self.fs, &target, &self.gate)
                    .map_err(durability_failure)?;
                sync_directory(&mut self.fs, &directory, &self.gate).map_err(durability_failure)?;
            }
            Ok(_) => {
                return Err(integrity(format!(
                    "Staging {} is bound to a different declaration.",
                    directory.display()
                )));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                match self.fs.remove(&temporary) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => return Err(io_failure(error)),
                }
                write_private(&mut self.fs, &self.gate, &temporary, &bytes, true)?;
                publish(&mut self.fs, &temporary, &target, &self.gate)
                    .map_err(exclusive_publication_failure)?;
                let _ = self.fs.remove(&temporary);
            }
            Err(error) => return Err(io_failure(error)),
        }
        sync_directory(&mut self.fs, root, &self.gate).map_err(durability_failure)
    }
}

impl StagingBinding<'_> {
    /// The exact bytes of `handle.json`: a pure function of the declaration.
    fn bytes(&self) -> Result<Vec<u8>, Failure> {
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|_| integrity("Could not encode the staging binding."))?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

/// Validate the composed transaction through the catalog and bind the handle.
fn declare(
    state: &State,
    catalog: &Catalog,
    acceptance: Acceptance<'_>,
    declaration: &CaptureDeclaration,
    existing: Option<Id>,
) -> Result<(AcceptedTransaction, CaptureBinding), Failure> {
    let handle = acceptance
        .ids
        .next_id(Kind::Handle)
        .map_err(|_| integrity("Could not allocate a capture handle."))?;
    let mut operations = Vec::new();
    if let DeclarationTarget::NewRecordCase { title } = &declaration.target {
        operations.push(CommandOperation {
            op: "case.create".into(),
            client_ref: Some(CASE_ALIAS.into()),
            payload: json!({"title": title, "mode": "record"}),
            extensions: None,
        });
    }
    operations.push(CommandOperation {
        op: "run.start".into(),
        client_ref: Some(RUN_ALIAS.into()),
        payload: declaration.run.clone(),
        extensions: None,
    });
    let new_case = existing.is_none();
    let command = Command {
        schema_version: CommandVersion::V1,
        command_id: declaration.command_id.clone(),
        case_id: existing,
        actor: declaration.actor.clone(),
        actor_descriptor: None,
        preconditions: Vec::new(),
        operations,
        session: None,
    };
    let result = if new_case {
        validate_and_apply_composed(state, &command, catalog, acceptance)
    } else {
        validate_and_apply(state, &command, catalog, acceptance)
    };
    let mut accepted = result.map_err(|failure| declaration_path(failure, new_case))?;
    let run = accepted
        .records
        .iter_mut()
        .find(|record| record.header.id.kind() == Kind::Run)
        .ok_or_else(|| integrity("The declaration produced no Run."))?;
    let binding = CaptureBinding {
        protocol: CaptureProtocol::V1,
        handle,
        producer: declaration.producer.clone(),
        external_run_id: declaration.external_run_id.clone(),
        external_plan_digest: declaration.external_plan_digest.clone(),
        limits: HandleLimits::DEFAULT,
        staging: format!("{STAGING_ROOT}/{}", run.header.id),
    };
    run.body["capture"] = serde_json::to_value(&binding)
        .map_err(|_| integrity("Could not encode the capture binding."))?;
    // Records are internal unless a stricter level is requested.
    if declaration.classification == ClassificationLevel::Restricted {
        run.header.classification.level = ClassificationLevel::Restricted;
    }
    Ok((accepted, binding))
}

/// Report catalog failures against the declaration, not the composed command.
fn declaration_path(mut failure: Failure, new_case: bool) -> Failure {
    let Some(path) = failure.error.path.take() else {
        return failure;
    };
    let mapped = path
        .strip_prefix("/operations/")
        .and_then(|rest| rest.split_once('/').or(Some((rest, ""))))
        .map(|(index, rest)| {
            let rest = rest.strip_prefix("payload").unwrap_or("");
            let part = if new_case && index == "0" {
                CASE_ALIAS
            } else {
                RUN_ALIAS
            };
            format!("/{part}{rest}")
        })
        .unwrap_or(path);
    failure.error.path = Some(mapped);
    if let Some(repair) = &mut failure.error.repair
        && repair.schema.as_deref() == Some("record")
    {
        repair.schema = None;
    }
    failure
}

/// Every Run in the visible cases that records `identity`, by either path.
///
/// One attempt has one Run. Workspaces written before the paths shared an
/// identity may hold one Run per path; callers prefer the Run of their own path.
pub(super) fn recorded_attempts<'v>(
    visible: &Visible<'v>,
    identity: AttemptIdentity<'_>,
) -> Vec<(&'v CaseReplay, &'v Record, AttemptPath)> {
    visible
        .target
        .into_iter()
        .chain(visible.others.cases.values())
        .flat_map(|case| case.state.records().map(move |record| (case, record)))
        .filter(|(_, record)| record.header.id.kind() == Kind::Run)
        .filter_map(|(case, record)| {
            let (recorded, path) = attempt::recorded_attempt(&record.body)?;
            (recorded == identity).then_some((case, record, path))
        })
        .collect()
}

/// Refusal of a capture or envelope for an attempt that the other path
/// already recorded, naming the existing Run.
pub(super) fn recorded_by_other_path(record: &Record, path: AttemptPath) -> Failure {
    let run_id = record.header.id;
    let case_id = record.header.case_id;
    let (message, argv, handle) = match path {
        AttemptPath::Capture => {
            let handle = record.body[attempt::CAPTURE_KEY]["handle"].clone();
            let argv = handle
                .as_str()
                .and_then(|handle| handle.parse::<Id>().ok())
                .map_or_else(|| show_argv(run_id, case_id), recover_argv);
            (
                format!(
                    "Run {run_id} already records this producer and external run ID through capture.begin. Continue that capture through its handle, or describe a separate attempt with another external run ID."
                ),
                argv,
                handle,
            )
        }
        AttemptPath::ProducerRun => (
            format!(
                "Run {run_id} already records this producer and external run ID from a producer-run envelope. Ingest a changed envelope to revise it, or declare a separate attempt with another external run ID."
            ),
            show_argv(run_id, case_id),
            Value::Null,
        ),
    };
    let mut failure = Failure::new(ErrorCode::IdempotencyKeyReused, message)
        .at("/external_run_id")
        .with_argv(argv);
    failure.error.expected = Some(json!({
        "run_id": run_id,
        "case_id": case_id,
        "recorded_by": match path {
            AttemptPath::Capture => "capture.begin",
            AttemptPath::ProducerRun => "producer-run",
        },
        "handle": handle,
    }));
    failure
}

/// The same logical attempt declared earlier under another key or actor, or
/// recorded by a producer-run envelope.
fn prior_declaration(visible: Visible<'_>, declaration: &CaptureDeclaration) -> Option<Failure> {
    let identity = AttemptIdentity {
        producer: &declaration.producer,
        external_run_id: &declaration.external_run_id,
    };
    let recorded = recorded_attempts(&visible, identity);
    let Some((case, record, _)) = recorded
        .iter()
        .find(|(_, _, path)| *path == AttemptPath::Capture)
    else {
        return recorded
            .first()
            .map(|(_, record, path)| recorded_by_other_path(record, *path));
    };
    let capture = &record.body[attempt::CAPTURE_KEY];
    let event = declaring_event(case, &case.state, record.header.id).ok()?;
    let mut failure = Failure::new(
        ErrorCode::IdempotencyKeyReused,
        "This producer already declared this external run ID under another command ID or actor. Resend that begin request to get its handle.",
    )
    .at("/external_run_id")
    .with_argv(receipt_argv(&event.actor, &event.command_id));
    failure.error.expected = Some(json!({
        "command_id": event.command_id,
        "actor": event.actor,
        "run_id": record.header.id,
        "handle": capture["handle"],
    }));
    failure.error.actual = Some(json!({
        "command_id": declaration.command_id,
        "actor": declaration.actor,
    }));
    Some(failure)
}

/// A new declaration's producer name follows the shared rule. Retries of a
/// declaration recorded before the rules were aligned never reach this check:
/// the journal answers them with the original receipt.
fn current_name(declaration: &CaptureDeclaration) -> Result<(), Failure> {
    if attempt::is_producer_name(&declaration.producer) {
        return Ok(());
    }
    Err(Failure::new(
        ErrorCode::InvalidInput,
        format!("Use a producer name of {}.", attempt::PRODUCER_NAME_RULE),
    )
    .at("/producer/name"))
}

pub(crate) fn declaring_event<'a>(
    case: &'a CaseReplay,
    state: &State,
    run_id: Id,
) -> Result<&'a Event, Failure> {
    let first = state
        .history(run_id)
        .next()
        .ok_or_else(|| integrity("A declared Run has no first revision."))?;
    usize::try_from(first.header.acceptance_sequence.saturating_sub(1))
        .ok()
        .and_then(|offset| case.events.get(offset))
        .map(|verified| &verified.event)
        .ok_or_else(|| integrity("A declared Run has no declaring transaction."))
}

pub(crate) fn binding_of(state: &State, run_id: Id) -> Result<CaptureBinding, Failure> {
    let first = state
        .history(run_id)
        .next()
        .ok_or_else(|| integrity("The declared Run is missing."))?;
    investigation_core::input::decode(&first.body["capture"])
        .map_err(|_| integrity("The declared Run has no valid capture binding."))
}

/// The read that reports what a handle accepted, for repair guidance on a
/// refused capture call.
pub fn recover_argv(handle: Id) -> Vec<String> {
    vec![
        PROGRAM_NAME.into(),
        "capture".into(),
        "recover".into(),
        "--handle".into(),
        handle.to_string(),
        "--format".into(),
        "json".into(),
    ]
}

fn created(receipt: &Receipt, kind: Kind) -> Result<Id, Failure> {
    receipt
        .created
        .iter()
        .map(|change| change.id)
        .find(|id| id.kind() == kind)
        .ok_or_else(|| {
            integrity("The command ID names a transaction that is not a capture declaration.")
        })
}

fn read_all(fs: &impl DurableFs, path: &Path) -> io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    fs.read(path)?.take(64 * 1024).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[path = "capture_items.rs"]
pub mod items;

#[path = "capture_attach.rs"]
pub mod attach;

#[path = "capture_finalize.rs"]
pub mod finalize;

#[path = "capture_recover.rs"]
pub mod recover;

#[path = "capture_late.rs"]
pub mod late;

#[path = "capture_retire.rs"]
pub mod retire;

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
