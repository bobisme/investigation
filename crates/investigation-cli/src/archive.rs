//! `archive verify DIR`: check an export directory offline (plan §9.1,
//! §10.7). It needs no workspace: everything it checks comes from DIR.
//!
//! The checks live in `investigation_render::export::verify`. For a
//! workspace archive they include a restore into a private directory below
//! the scratch folder and the store's own `check` there; the copy is removed
//! afterwards and DIR is never written.

use std::{env, fmt::Write as _, path::PathBuf};

use clap::Subcommand;
use investigation_core::{
    PROGRAM_NAME,
    wire::{ErrorCode, Failure},
};
use investigation_render::export::verify::verify_export;
use serde_json::json;

use crate::Output;

/// Problems listed in one response; the count is always complete.
const MAX_LISTED: usize = 50;

pub(crate) const AFTER_HELP: &str = "\
verify checks an export directory, and a workspace archive in full, without
the workspace it came from. Make a workspace archive with export first.

Examples:
  investigation archive verify ./backup --format json

Next: investigation export --kind workspace-archive --audience restricted --out ./backup --format json";

#[derive(Subcommand)]
pub(crate) enum ArchiveCommand {
    /// Check an export or workspace archive against its manifest, chains and blobs, offline.
    #[command(after_help = VERIFY_HELP)]
    Verify(VerifyArgs),
}

#[derive(clap::Args)]
pub(crate) struct VerifyArgs {
    /// The export directory, holding export-manifest.json.
    #[arg(value_name = "DIR")]
    dir: PathBuf,
    /// Folder for the private restored copy of an archive; default the system temporary folder.
    #[arg(long, value_name = "DIR")]
    scratch: Option<PathBuf>,
}

pub(crate) const VERIFY_HELP: &str = "\
Checks DIR without the workspace it came from. Every member the manifest lists
must be present with its SHA-256 digest and size, and nothing else may be in
DIR; the manifest's export_id is recomputed from its own fields. For a
workspace archive, archive.json must agree with the manifest, and the archive
is restored into a private folder below --scratch, where check replays every
chain, checks command and record uniqueness and hashes every blob. The
restored case heads must equal the archived ones. The copy is removed
afterwards; DIR is never written. Exit 0 means verified; exit 6 lists the
problems.

Examples:
  investigation archive verify ./backup --format json
  investigation archive verify ./backup --scratch ./verify-scratch --format json

Next: investigation check --format json";

pub(crate) fn run(command: ArchiveCommand) -> Result<Output, Failure> {
    match command {
        ArchiveCommand::Verify(args) => verify(&args),
    }
}

fn verify(args: &VerifyArgs) -> Result<Output, Failure> {
    let scratch = args.scratch.clone().unwrap_or_else(env::temp_dir);
    let shown = args.dir.to_string_lossy().into_owned();
    let report = verify_export(&args.dir, &scratch).map_err(|error| {
        let mut failure = Failure::new(
            ErrorCode::InvalidInput,
            format!("{shown} cannot be read as an export directory ({error})."),
        )
        .at("DIR");
        failure.error.actual = Some(json!({"directory": shown}));
        failure
    })?;
    let problem_count = report.problems.len();
    let value = json!({
        "directory": shown,
        "verified": report.ok(),
        "kind": report.kind,
        "export_id": report.export_id,
        "members": report.members,
        "bytes": report.bytes,
        "archive": report.archive,
        "problem_count": problem_count,
        "problems": report.problems.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "lists_capped_at": MAX_LISTED,
    });
    if !report.ok() {
        let mut failure = Failure::new(
            ErrorCode::IntegrityFailure,
            format!(
                "{shown} failed verification with {problem_count} problem(s); see error.actual.problems. Do not restore it."
            ),
        )
        .at("DIR");
        failure.error.actual = Some(value);
        return Err(failure);
    }
    let mut text = format!(
        "verified  {}  {}\n{} member(s)  {} byte(s)",
        report.kind.as_deref().unwrap_or("export"),
        shown,
        report.members,
        report.bytes
    );
    if let Some(archive) = &report.archive {
        let _ = write!(
            text,
            "\n{} case(s)  {} blob(s)  {} without bytes  {} human note(s)\nrecovery head {}",
            archive.cases,
            archive.blobs,
            archive.not_retained,
            archive.human_notes,
            archive.recovery_head
        );
        if archive.rehearsal.as_ref().is_some_and(|r| r.check_ok) {
            text.push_str("\nrestored a private copy: check passed and every case head matches");
        }
    }
    let _ = write!(text, "\nNext: {PROGRAM_NAME} check --format json");
    Ok(Output::success("verified", value, text))
}
