//! `archive verify DIR`: check an export directory offline (plan §9.1,
//! §10.7). It needs no workspace: everything it checks comes from DIR.
//!
//! The checks live in `investigation_render::export::verify`. For a
//! workspace archive they include a restore into a private directory below
//! the scratch folder and the store's own `check` there; the copy is removed
//! afterwards and DIR is never written.
//!
//! `archive restore DIR PATH` and `archive fork DIR PATH` publish a verified
//! archive as a workspace at a new or empty PATH (W11.18, ADR 0013); see
//! `investigation_render::export::restore`. Afterwards the vault, when the
//! workspace renders one, is rendered like any `vault render`.

use std::{env, fmt::Write as _, path::PathBuf};

use clap::Subcommand;
use investigation_core::{
    PROGRAM_NAME,
    wire::{ErrorCode, Failure},
};
use investigation_render::{
    VaultRenderer,
    export::{
        restore::{Mode, Refusal, Restored, restore_archive},
        verify::{VerifyReport, verify_export},
    },
    render_vault, working_policy,
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

use crate::Output;

/// Problems listed in one response; the count is always complete.
const MAX_LISTED: usize = 50;

pub(crate) const AFTER_HELP: &str = "\
verify checks an export directory, and a workspace archive in full, without
the workspace it came from. Make a workspace archive with export first.

verify checks an archive; restore publishes it as the same workspace at a new
or empty PATH; fork publishes it as a new workspace that names its origin.

Examples:
  investigation archive verify ./backup --format json
  investigation archive restore ./backup ./recovered --format json
  investigation archive fork ./backup ./alternative --format json

Next: investigation export --kind workspace-archive --audience restricted --out ./backup --format json";

#[derive(Subcommand)]
pub(crate) enum ArchiveCommand {
    /// Check an export or workspace archive against its manifest, chains and blobs, offline.
    #[command(after_help = VERIFY_HELP)]
    Verify(VerifyArgs),
    /// Restore a verified workspace archive as the same workspace at a new or empty PATH.
    #[command(after_help = RESTORE_HELP)]
    Restore(PublishArgs),
    /// Fork a verified workspace archive into a new workspace, with a new identity, at PATH.
    #[command(after_help = FORK_HELP)]
    Fork(PublishArgs),
}

#[derive(clap::Args)]
pub(crate) struct PublishArgs {
    /// The workspace archive directory, holding export-manifest.json and archive.json.
    #[arg(value_name = "DIR")]
    dir: PathBuf,
    /// The new workspace: a directory that does not exist (its parent must) or is empty.
    #[arg(value_name = "PATH")]
    path: PathBuf,
}

pub(crate) const RESTORE_HELP: &str = "\
Runs every check of archive verify first and restores nothing unless all pass.
The copy check runs on is staged inside PATH, on the destination filesystem;
the index is rebuilt from the journal, check must pass there with every case
head equal to the archive's, and only then is the copy moved into PATH, with
state/ (the workspace marker) last. A restore stopped part way leaves PATH
without a marker, so check refuses it; a non-empty PATH is refused, so retry
into a new or emptied PATH. Nothing is written outside PATH, and DIR is never
written. The vault, when enabled, is rendered afterwards.

The result is the same workspace: same workspace ID, journals, heads, record
and command IDs, so every old command ID returns its original receipt. Use it
in place of the original, never beside it; to work beside the original, use
archive fork.

Examples:
  investigation archive restore ./backup ./recovered --format json
  investigation archive restore ./backup ./recovered --format text

Next: investigation --workspace ./recovered check --format json";

pub(crate) const FORK_HELP: &str = "\
Restores the archive exactly as archive restore does, then gives the copy a
new workspace ID before check runs and it is published. The fork keeps every
journal byte, so its case heads, record IDs, transaction and command IDs and
receipts at the fork are the origin's. Its workspace marker (state/workspace.json)
records a fork section: the origin workspace ID, the archive's export_id and
recovery head, and each inherited case head with the workspaces that wrote it.
Every event the fork commits names the fork's own workspace ID, and so do its
check report and its own workspace archives, so the fork never passes for its
origin and two forks never pass for one writer.

Examples:
  investigation archive fork ./backup ./alternative --format json
  investigation archive fork ./backup ./alternative --format text

Next: investigation --workspace ./alternative check --format json";

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
        ArchiveCommand::Restore(args) => publish(&args, Mode::Exact),
        ArchiveCommand::Fork(args) => publish(&args, Mode::Fork),
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
    let value = report_value(&shown, &report);
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

/// The fields `archive verify` reports, shared with a refused restore.
fn report_value(shown: &str, report: &VerifyReport) -> Value {
    json!({
        "directory": shown,
        "verified": report.ok(),
        "kind": report.kind,
        "export_id": report.export_id,
        "members": report.members,
        "bytes": report.bytes,
        "archive": report.archive,
        "problem_count": report.problems.len(),
        "problems": report.problems.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "lists_capped_at": MAX_LISTED,
    })
}

fn publish(args: &PublishArgs, mode: Mode) -> Result<Output, Failure> {
    let shown = args.dir.to_string_lossy().into_owned();
    let path = args.path.to_string_lossy().into_owned();
    let verb = match mode {
        Mode::Exact => "restore",
        Mode::Fork => "fork",
    };
    let restored = restore_archive(&args.dir, &args.path, mode).map_err(|refusal| {
        match refusal {
            Refusal::Destination(reason) => {
                let mut failure = Failure::new(ErrorCode::InvalidInput, reason).at("PATH");
                failure.error.actual = Some(json!({"path": path}));
                failure
            }
            Refusal::Unreadable(error) => {
                let mut failure = Failure::new(
                    ErrorCode::InvalidInput,
                    format!("{shown} cannot be read as an export directory ({error}). Nothing was written."),
                )
                .at("DIR");
                failure.error.actual = Some(json!({"directory": shown}));
                failure
            }
            Refusal::Unverified(report) => {
                let mut failure = Failure::new(
                    ErrorCode::IntegrityFailure,
                    format!(
                        "{shown} failed verification with {} problem(s); see error.actual.problems. Nothing was restored and PATH is as it was.",
                        report.problems.len()
                    ),
                )
                .at("DIR");
                failure.error.actual = Some(report_value(&shown, &report));
                failure
            }
            Refusal::Failed(reason) => Failure::new(
                ErrorCode::IntegrityFailure,
                format!("The {verb} of {shown} stopped: {reason}"),
            )
            .at("PATH"),
            Refusal::Interrupted(step) => Failure::new(
                ErrorCode::IoFailure,
                format!("The {verb} was stopped at {step:?}."),
            ),
        }
    })?;
    let vault = render_restored(&restored);
    Ok(publish_output(&restored, &shown, vault))
}

/// Render the vault of a published restore: the disposable projection is
/// rebuilt, never copied. A vault outside PATH is left alone, and a failed
/// render leaves the workspace published with the vault pending.
fn render_restored(restored: &Restored) -> Value {
    let pending = |reason: String| json!({"rendered": false, "reason": reason});
    let workspace = match Workspace::open(&restored.root) {
        Ok(workspace) => workspace,
        Err(error) => return pending(error.to_string()),
    };
    let vault = &workspace.config().vault_policy;
    if !vault.enabled {
        return json!({"rendered": false, "reason": "The vault is disabled in investigation.toml."});
    }
    if !vault.uses_workspace_vault() {
        return pending(
            "investigation.toml names an existing vault outside PATH, which a restore never writes. Run vault render when that vault is the right target for this workspace.".to_owned(),
        );
    }
    let policy = working_policy(&workspace);
    let mut store = Store::open(workspace);
    match render_vault(&mut store, &VaultRenderer, &policy, &mut |_| {}) {
        Ok(report) => json!({
            "rendered": true,
            "generation": report.publish.generation.id(),
        }),
        Err(error) => pending(error.to_string()),
    }
}

fn publish_output(restored: &Restored, shown: &str, vault: Value) -> Output {
    let root = restored.root.to_string_lossy().into_owned();
    let check_argv = [
        PROGRAM_NAME,
        "--workspace",
        root.as_str(),
        "check",
        "--format",
        "json",
    ];
    let (result, identity) = match restored.mode {
        Mode::Exact => (
            "restored",
            "The same workspace: its workspace ID, journals, case heads, record and command IDs and receipts. Use it in place of the original, never beside it; to work beside the original, fork the archive instead.",
        ),
        Mode::Fork => (
            "forked",
            "A new workspace: a new workspace ID, with the origin's journals, case heads, record and command IDs and receipts kept up to the fork. state/workspace.json names the origin and the inherited heads; every later event names this workspace.",
        ),
    };
    let check = &restored.check;
    let value = json!({
        "workspace": root,
        "workspace_id": restored.workspace_id,
        "mode": match restored.mode { Mode::Exact => "exact", Mode::Fork => "fork" },
        "identity": identity,
        "archive": {
            "directory": shown,
            "export_id": restored.export_id,
            "workspace_id": restored.origin_workspace_id,
            "recovery_head": restored.recovery_head,
            "members": restored.verification.members,
            "bytes": restored.verification.bytes,
        },
        "fork": restored.fork,
        "check": {
            "ok": check.integrity_ok(),
            "heads": check.heads,
            "verified_blobs": check.verified_blobs,
            "unavailable_artifacts": check.unavailable_artifacts,
            "index": check.index,
        },
        "human_notes": restored.human_notes,
        "vault": vault,
        "next": [{"kind": "local_read", "argv": check_argv}],
    });
    let mut text = format!(
        "{result}  {root}\nworkspace {}  {} case(s)  {} verified blob(s)",
        restored.workspace_id,
        check.heads.len(),
        check.verified_blobs,
    );
    if let Some(fork) = &restored.fork {
        let _ = write!(
            text,
            "\nfork of {} at recovery head {}",
            fork.origin_workspace_id, fork.origin_recovery_head
        );
    }
    let _ = write!(
        text,
        "\nfrom {shown}  {}\ncheck passed before publication; every case head matches the archive\n{identity}",
        restored.export_id
    );
    if value["vault"]["rendered"] == json!(false)
        && let Some(reason) = value["vault"]["reason"].as_str()
    {
        let _ = write!(text, "\nvault not rendered: {reason}");
    }
    let _ = write!(text, "\nNext: {}", check_argv.join(" "));
    Output::success(result, value, text)
}
