//! `capture` commands: the JSON-CLI form of the recording API (`inv.capture.v1`).
//!
//! Each command reads exactly the request type a Rust producer passes to
//! `investigation_recording::Recorder` and prints exactly the receipt it gets
//! back, through the same `LocalRecorder`, so the two paths cannot diverge.

use std::path::{Path, PathBuf};

use investigation_core::{
    input,
    wire::{ErrorCode, Failure},
};
use investigation_recording::{
    BeginCapture, CaptureEvent, CaptureHandle, CaseTarget, Finalization, FinalizeCapture,
    LocalRecorder, ReceiptResult, Recorder, RecordingError, RecoverScope, StableArtifact,
    StagedAvailability,
};
use investigation_store::Store;

use crate::{
    Global, Output, display_argv, read_input, scope_argv,
    selection::{require_case_agreement, require_selected_case, select_actor},
    with_workspace_repair,
};

const BEGIN_HELP: &str = "\
Reads one inv.capture.v1 begin request: key, actor, producer, external_run_id,
intent, case, and optional classification and metadata. Prints the begin
receipt with run_id, handle, protocol, limits and staging_dir. Resend the
identical request after a lost response to get the same run and handle.

Begin records a declaration only. It does not authorize, approve or start any
action; the producer runs its action under its own authority.

Example: investigation capture begin --input begin.json --format json";

const APPEND_HELP: &str = "\
Reads one inv.capture.v1 append request: handle, key, position, fact, and
optional reported_at and metadata. Prints the item receipt with position,
request_digest and accepted_at. The fact is staged durably before the receipt
is printed; it is summarized into the Run's manifest at finalize, not
committed as its own transaction.

The handle accepts only its next position; a gap or a taken position is
refused with the expected position. Resend the identical request after a lost
response to get the original receipt. The same key with other content is
refused.

Appending a fact reports what happened. It does not start, cancel or rerun
anything.

Example: investigation capture append --input event.json --format json";

const ATTACH_HELP: &str = "\
Reads one inv.capture.v1 attach request: handle, key, position, role, source,
and optional channel, derived_from, media_type, expected_digest,
expected_size_bytes, classification and metadata. Prints the artifact
receipt: the item receipt, role, availability, and the digest and size of the
retained bytes.

source is {\"kind\": \"file\", \"path\": ...} or {\"kind\": \"missing\",
\"reason\": ...}. A file is read only beneath the workspace's import_roots,
never through a symlink, and is copied into the handle's staging while it is
hashed, then read again. A file that changes during the copy, or that does not
match expected_digest or expected_size_bytes, is refused; nothing is staged
and the position stays free. A path that no import root holds is recorded as
missing, never as an empty file.

Close and flush the file before attaching it. Attaching runs no parser or
command and does not start, cancel or rerun anything.

Example: investigation capture attach --input artifact.json --format json";

const FINALIZE_HELP: &str = "\
Reads one inv.capture.v1 finalize request: handle, key, expected_last_position,
completion (complete, partial or interrupted), execution, and optional
channels, declared_artifacts, strict_artifact_inventory, assertions, controls,
receipts, limitations and metadata. The receipt gives the finalization,
execution, sealed position, missing positions, manifest digest and journal
receipt.

Finalize seals the handle, then commits one transaction: the retained bytes,
an immutable manifest of every item, and the Run's execution state, retained
and missing artifacts and per-channel completeness. A complete claim whose
expected_last_position is not the last accepted position is refused and seals
nothing. With completion complete, set strict_artifact_inventory to true to
require declared_artifacts to match the acknowledged attachment keys. A
mismatch leaves the handle open and returns error.actual.missing and
error.actual.unexpected. Correct the inventory and retry with the same key.
Resend the identical request after a lost response to get the original receipt.

Recording completeness and execution are independent: a failed run can be
finalized, and its original error is kept verbatim. Producer results stay
attributed producer claims. Finalize creates no Observation, Assessment or
Conclusion, and an exit code of 0 sets no measurement validity.

Example: investigation capture finalize --input finalize.json --format json";

const RECOVER_HELP: &str = "\
Reports what the recorder durably accepted, so a producer that crashed or lost
a response can continue its recording. With --handle, reports that handle in
any state. With --input, reads one inv.capture.v1 recover scope:
{\"kind\": \"unsettled\"} or {\"kind\": \"handle\", \"handle\": ...}. With
neither, reports every handle that is not finalized.

Each handle reports its seal state (open, sealed or finalized), staging state,
last accepted position, the original receipt of every accepted fact and
artifact (staged or missing), the original finalize receipt once finalized,
and the exact commands that continue or settle the recording. Execution stays
unknown until a finalize records the producer's report.

Recover is a read. It never reruns, resumes or schedules an external action,
never deletes staged bytes and never changes a handle's state. A producer that
cannot vouch for an unfinished step finalizes with completion interrupted and
execution state unknown.

Example: investigation capture recover --handle handle_... --format json";

pub(crate) const RETIRE_HELP: &str = "\
Removes the staging of finalized capture handles. A handle's staging is removed
only when its Run's finalization is durable, the manifest binds every staged
item, and the canonical blob store holds every staged byte, verified by digest.
Open handles, sealed handles whose finalization is not committed, handles
another call holds, and staging that holds anything else are kept, with the
reason. With --handle, reports that handle in any state.

Retirement moves the staging out of state/staging in one rename, then removes
it, so an interrupted retirement leaves the full staging or none; the next run
completes it. Retiring again changes nothing. The finalized manifest keeps
answering identical retries and recover.

Example: investigation repair staging --format json";

#[derive(clap::Subcommand)]
pub(crate) enum CaptureCommand {
    /// Declare one Run before an external action and allocate its staging.
    #[command(after_help = BEGIN_HELP)]
    Begin {
        /// Read the begin request from FILE, or - for stdin.
        #[arg(long, value_name = "FILE|-")]
        input: PathBuf,
    },
    /// Stage one lifecycle or collection fact at the next position of a handle.
    #[command(after_help = APPEND_HELP)]
    Append {
        /// Read the append request from FILE, or - for stdin.
        #[arg(long, value_name = "FILE|-")]
        input: PathBuf,
    },
    /// Copy one closed artifact into a handle's staging and verify it, or record it as missing.
    #[command(after_help = ATTACH_HELP)]
    Attach {
        /// Read the attach request from FILE, or - for stdin.
        #[arg(long, value_name = "FILE|-")]
        input: PathBuf,
    },
    /// Seal a handle and commit the Run's manifest, execution state and completeness.
    #[command(after_help = FINALIZE_HELP)]
    Finalize {
        /// Read the finalize request from FILE, or - for stdin.
        #[arg(long, value_name = "FILE|-")]
        input: PathBuf,
    },
    /// Report accepted positions, receipts and seal state; never reruns anything.
    #[command(after_help = RECOVER_HELP)]
    Recover {
        /// Report this handle, in any state.
        #[arg(long, value_name = "HANDLE", conflicts_with = "input")]
        handle: Option<String>,
        /// Read the recover scope from FILE, or - for stdin.
        #[arg(long, value_name = "FILE|-")]
        input: Option<PathBuf>,
    },
}

pub(crate) fn run(
    store: &mut Store,
    global: &Global,
    command: CaptureCommand,
) -> Result<Output, Failure> {
    match command {
        CaptureCommand::Begin { input } => begin(store, global, &input),
        CaptureCommand::Append { input } => append(store, global, &input),
        CaptureCommand::Attach { input } => attach(store, global, &input),
        CaptureCommand::Finalize { input } => finalize(store, global, &input),
        CaptureCommand::Recover { handle, input } => {
            recover(store, handle.as_deref(), input.as_deref())
        }
    }
}

fn begin(store: &mut Store, global: &Global, path: &Path) -> Result<Output, Failure> {
    let request: BeginCapture = input::parse_request(&read_input(path)?)
        .and_then(|value| input::decode(&value))
        .map_err(|error| Failure::new(ErrorCode::InvalidInput, error.message).at(error.path))?;
    select_actor(
        store.workspace(),
        global.actor.as_ref(),
        Some(&request.actor),
    )?;
    let case_id = match &request.case {
        CaseTarget::Existing { case_id } => Some(*case_id),
        _ => None,
    };
    require_case_agreement(store, global.case.as_deref(), case_id)?;
    let receipt = LocalRecorder::open(store.workspace().root())
        .and_then(|recorder| recorder.begin(&request))
        .map_err(|error| with_workspace_repair(store.workspace(), rejection(error)))?;
    let text = format!(
        "{}  declared  {}\ncase     {}\nhandle   {}\nstaging  {}\nprotocol {}\nDeclared only: nothing was authorized or started.",
        receipt.run_id,
        serde_json::to_value(receipt.commit.result)
            .ok()
            .as_ref()
            .and_then(|value| value.as_str())
            .unwrap_or_default(),
        receipt.case_id,
        receipt.handle,
        receipt.staging_dir.display(),
        receipt.protocol,
    );
    Ok(Output::new(receipt, text))
}

/// The handle names the Run, its case and its actor, so `--case` and
/// `--actor` are not needed. A `--case` that names another case is refused
/// before anything is staged, as `begin` refuses one that disagrees with its
/// request, so a call never writes into a case other than the one it named.
fn require_handle_case(
    store: &mut Store,
    global: &Global,
    handle: &CaptureHandle,
) -> Result<(), Failure> {
    let Some(selector) = global.case.as_deref() else {
        return Ok(());
    };
    let state = LocalRecorder::open(store.workspace().root())
        .and_then(|recorder| recorder.recover(&RecoverScope::Handle { handle: *handle }))
        .map_err(|error| with_workspace_repair(store.workspace(), rejection(error)))?;
    // An unknown handle is reported by the call itself.
    let Some(case_id) = state.handles.first().map(|recovered| recovered.case_id) else {
        return Ok(());
    };
    require_selected_case(store, Some(selector), case_id, "the handle's Run")
}

/// See [`require_handle_case`] for `--case`; `--actor` plays no part.
fn append(store: &mut Store, global: &Global, path: &Path) -> Result<Output, Failure> {
    let request: CaptureEvent = input::parse_request(&read_input(path)?)
        .and_then(|value| input::decode(&value))
        .map_err(|error| Failure::new(ErrorCode::InvalidInput, error.message).at(error.path))?;
    require_handle_case(store, global, &request.handle)?;
    let receipt = LocalRecorder::open(store.workspace().root())
        .and_then(|recorder| recorder.append(&request))
        .map_err(|error| with_workspace_repair(store.workspace(), rejection(error)))?;
    let result = match receipt.result {
        ReceiptResult::AlreadyAccepted => "already_accepted",
        _ => "accepted",
    };
    let text = format!(
        "{}  {result}  position {}  key {}\nrun      {}\ndigest   {}\naccepted {}\nStaged only: nothing was started, cancelled or rerun.",
        receipt.handle,
        receipt.position,
        receipt.key,
        receipt.run_id,
        receipt.request_digest,
        receipt.accepted_at,
    );
    Ok(Output::new(receipt, text))
}

/// Like `append`, the handle names the Run, its case and its actor.
fn attach(store: &mut Store, global: &Global, path: &Path) -> Result<Output, Failure> {
    let request: StableArtifact = input::parse_request(&read_input(path)?)
        .and_then(|value| input::decode(&value))
        .map_err(|error| Failure::new(ErrorCode::InvalidInput, error.message).at(error.path))?;
    require_handle_case(store, global, &request.handle)?;
    let receipt = LocalRecorder::open(store.workspace().root())
        .and_then(|recorder| recorder.attach(&request))
        .map_err(|error| with_workspace_repair(store.workspace(), rejection(error)))?;
    let item = &receipt.receipt;
    let result = match item.result {
        ReceiptResult::AlreadyAccepted => "already_accepted",
        _ => "accepted",
    };
    let retained = match (receipt.availability, &receipt.digest, receipt.size_bytes) {
        (StagedAvailability::Staged, Some(digest), Some(size)) => {
            format!("staged   {digest}  {size} bytes")
        }
        _ => "missing  no bytes retained".to_owned(),
    };
    let text = format!(
        "{}  {result}  position {}  key {}\nrun      {}\n{retained}\naccepted {}\nStaged only: nothing was parsed, started or rerun.",
        item.handle, item.position, item.key, item.run_id, item.accepted_at,
    );
    Ok(Output::new(receipt, text))
}

/// Like `append`, the handle names the Run, its case and its actor.
fn finalize(store: &mut Store, global: &Global, path: &Path) -> Result<Output, Failure> {
    let request: FinalizeCapture = input::parse_request(&read_input(path)?)
        .and_then(|value| input::decode(&value))
        .map_err(|error| Failure::new(ErrorCode::InvalidInput, error.message).at(error.path))?;
    require_handle_case(store, global, &request.handle)?;
    let receipt = LocalRecorder::open(store.workspace().root())
        .and_then(|recorder| recorder.finalize(&request))
        .map_err(|error| with_workspace_repair(store.workspace(), rejection(error)))?;
    let finalization = match receipt.finalization {
        Finalization::Finalized => "finalized",
        _ => "finalized_partial",
    };
    let execution = serde_json::to_value(receipt.execution)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    let missing = if receipt.missing_positions.is_empty() {
        "none".to_owned()
    } else {
        receipt
            .missing_positions
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let text = format!(
        "{}  {finalization}  {}\nhandle    {}\nexecution {execution}\nsealed    {}\nmissing   {missing}\nmanifest  {}\nRecorded only: no Observation, Assessment or Conclusion was created.",
        receipt.run_id,
        serde_json::to_value(receipt.commit.result)
            .ok()
            .as_ref()
            .and_then(|value| value.as_str())
            .unwrap_or_default(),
        receipt.handle,
        receipt
            .sealed_position
            .map_or_else(|| "none".to_owned(), |position| position.to_string()),
        receipt.manifest_digest,
    );
    Ok(Output::new(receipt, text))
}

/// A read: the handle names its Run, case and actor, so `--case` and
/// `--actor` play no part.
fn recover(
    store: &mut Store,
    handle: Option<&str>,
    path: Option<&Path>,
) -> Result<Output, Failure> {
    let scope = match (handle, path) {
        (Some(handle), _) => RecoverScope::Handle {
            handle: handle.parse().map_err(|_| {
                Failure::new(
                    ErrorCode::InvalidInput,
                    "Use the handle from the begin receipt, such as handle_<uuid>.",
                )
                .at("--handle")
            })?,
        },
        (None, Some(path)) => input::parse_request(&read_input(path)?)
            .and_then(|value| input::decode(&value))
            .map_err(|error| Failure::new(ErrorCode::InvalidInput, error.message).at(error.path))?,
        (None, None) => RecoverScope::Unsettled,
    };
    let mut state = LocalRecorder::open(store.workspace().root())
        .and_then(|recorder| recorder.recover(&scope))
        .map_err(|error| with_workspace_repair(store.workspace(), rejection(error)))?;
    // The next steps name this workspace, like repair guidance does.
    for handle in &mut state.handles {
        for step in &mut handle.next {
            scope_argv(store.workspace(), &mut step.argv);
        }
    }
    let mut lines = Vec::new();
    if state.handles.is_empty() {
        lines.push("No unsettled capture handles.".to_owned());
    }
    for handle in &state.handles {
        let word = |value: serde_json::Result<serde_json::Value>| {
            value
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default()
        };
        let staged = handle
            .artifacts
            .iter()
            .filter(|artifact| artifact.availability == StagedAvailability::Staged)
            .count();
        lines.push(format!(
            "{}  {}  {}
run       {}
producer  {} run {}
last      {}
items     {} facts, {} artifacts ({staged} staged, {} missing)
execution {}",
            handle.handle,
            word(serde_json::to_value(handle.seal)),
            handle
                .staging
                .map(|staging| word(serde_json::to_value(staging)))
                .unwrap_or_default(),
            handle.run_id,
            handle.producer.name,
            handle.external_run_id,
            handle
                .last_position
                .map_or_else(|| "none".to_owned(), |position| position.to_string()),
            handle.items.len(),
            handle.artifacts.len(),
            handle.artifacts.len() - staged,
            word(serde_json::to_value(handle.execution)),
        ));
        for step in &handle.next {
            lines.push(format!(
                "next      {}: {}",
                word(serde_json::to_value(step.action)),
                display_argv(&step.argv)
            ));
        }
    }
    lines.push("Read only: nothing was rerun, resumed, deleted or changed.".to_owned());
    Ok(Output::new(state, lines.join("\n")))
}

/// `repair staging`: retire finalized staging. The handle names its Run, so
/// `--case` and `--actor` play no part.
pub(crate) fn retire(store: &mut Store, handle: Option<&str>) -> Result<Output, Failure> {
    let handle = handle
        .map(|handle| {
            handle.parse().map_err(|_| {
                Failure::new(
                    ErrorCode::InvalidInput,
                    "Use the handle from the begin receipt, such as handle_<uuid>.",
                )
                .at("--handle")
            })
        })
        .transpose()?;
    let report = store.retire_staging(handle)?;
    let word = |value: serde_json::Result<serde_json::Value>| {
        value
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    let mut lines = Vec::new();
    for run_id in &report.completed {
        lines.push(format!("{run_id}  completed an interrupted retirement"));
    }
    for retirement in &report.handles {
        let mut line = format!(
            "{}  {}",
            retirement.handle,
            word(serde_json::to_value(retirement.result))
        );
        if let Some(reason) = retirement.reason {
            line.push_str(&format!("  {}", word(serde_json::to_value(reason))));
        }
        line.push_str(&format!("\nrun       {}", retirement.run_id));
        if let Some(detail) = &retirement.detail {
            line.push_str(&format!("\ndetail    {detail}"));
        }
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push("No capture staging to retire.".to_owned());
    }
    lines.push(
        "Only finalized staging whose bytes the canonical store holds was removed.".to_owned(),
    );
    let value = serde_json::to_value(&report).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Could not encode the retirement report.",
        )
    })?;
    Ok(Output::success("staging_retired", value, lines.join("\n")))
}

fn rejection(error: RecordingError) -> Failure {
    match error {
        RecordingError::Rejected(failure) => failure,
        other => Failure::new(ErrorCode::InvalidInput, other.to_string()),
    }
}
