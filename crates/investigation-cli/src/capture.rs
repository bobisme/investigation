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
    BeginCapture, CaptureEvent, CaseTarget, LocalRecorder, ReceiptResult, Recorder, RecordingError,
    StableArtifact, StagedAvailability,
};
use investigation_store::Store;

use crate::{
    Global, Output, read_input,
    selection::{require_case_agreement, select_actor},
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
}

pub(crate) fn run(
    store: &mut Store,
    global: &Global,
    command: CaptureCommand,
) -> Result<Output, Failure> {
    match command {
        CaptureCommand::Begin { input } => begin(store, global, &input),
        CaptureCommand::Append { input } => append(store, &input),
        CaptureCommand::Attach { input } => attach(store, &input),
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
/// `--actor` play no part.
fn append(store: &mut Store, path: &Path) -> Result<Output, Failure> {
    let request: CaptureEvent = input::parse_request(&read_input(path)?)
        .and_then(|value| input::decode(&value))
        .map_err(|error| Failure::new(ErrorCode::InvalidInput, error.message).at(error.path))?;
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
fn attach(store: &mut Store, path: &Path) -> Result<Output, Failure> {
    let request: StableArtifact = input::parse_request(&read_input(path)?)
        .and_then(|value| input::decode(&value))
        .map_err(|error| Failure::new(ErrorCode::InvalidInput, error.message).at(error.path))?;
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

fn rejection(error: RecordingError) -> Failure {
    match error {
        RecordingError::Rejected(failure) => failure,
        other => Failure::new(ErrorCode::InvalidInput, other.to_string()),
    }
}
