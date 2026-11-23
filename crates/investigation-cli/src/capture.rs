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
use investigation_recording::{BeginCapture, CaseTarget, LocalRecorder, Recorder, RecordingError};
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

#[derive(clap::Subcommand)]
pub(crate) enum CaptureCommand {
    /// Declare one Run before an external action and allocate its staging.
    #[command(after_help = BEGIN_HELP)]
    Begin {
        /// Read the begin request from FILE, or - for stdin.
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

fn rejection(error: RecordingError) -> Failure {
    match error {
        RecordingError::Rejected(failure) => failure,
        other => Failure::new(ErrorCode::InvalidInput, other.to_string()),
    }
}
