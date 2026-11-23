//! `vault remove`: take the projection out of the vault, deleting only the
//! files the render manifest proves generated
//! ([`investigation_render::remove`]).
//!
//! Canonical state is never touched; the next `vault render` projects the
//! casebook again. `--dry-run` lists what would be removed and kept and
//! writes nothing.

use std::fmt::Write as _;

use investigation_core::wire::Failure;
use investigation_render::{
    Action, Refusal,
    remove::{RemoveReport, preview, remove_projection},
};
use investigation_store::Workspace;
use serde_json::{Value, json};

use crate::{Output, display_argv, vault::argv};

/// Paths listed per category in one response; counts are always complete.
const MAX_LISTED: usize = 50;

pub(crate) const AFTER_HELP: &str = "\
Delete the generated files of the projection: notes, views, Bases, the case
map and Start Here.md. A file is deleted only while it holds exactly the bytes
the render manifest records for it. Generated files you edited, paths released
to you, files reconcile moved into Notes/Reconciled/, your Notes/, Boards/ and
Inbox/ and everything else are left in place. In existing-vault mode nothing
outside the managed prefix is touched, and the prefix folder is removed only
when it is left empty. The workspace journal is unchanged; vault render
projects the casebook again. --dry-run lists what would happen.
Examples:
  investigation vault remove --dry-run --format json
  investigation vault remove --format json
Next: investigation vault status --format json";

#[derive(clap::Args)]
pub(crate) struct RemoveArgs {
    /// List what would be removed and kept; change nothing.
    #[arg(long)]
    dry_run: bool,
}

pub(crate) fn run(workspace: &Workspace, args: &RemoveArgs) -> Result<Output, Failure> {
    let report = if args.dry_run {
        preview(workspace)
    } else {
        remove_projection(workspace)
    }
    .map_err(|error| error.to_failure())?;
    Ok(output(workspace, args.dry_run, &report))
}

const fn kept_reason(refusal: Refusal) -> &'static str {
    match refusal {
        Refusal::Drifted => "edited",
        Refusal::NotARegularFile => "not_a_regular_file",
        Refusal::UnsafeParent => "unsafe_parent",
        Refusal::HumanOwned => "human_owned",
        Refusal::NotProvenOwned | Refusal::UnlistedAttachment => "not_proven_owned",
    }
}

fn capped<T: Clone>(items: &[T]) -> Vec<T> {
    items.iter().take(MAX_LISTED).cloned().collect()
}

fn output(workspace: &Workspace, dry_run: bool, report: &RemoveReport) -> Output {
    let removed: Vec<String> = report
        .paths
        .iter()
        .filter(|path| path.action == Action::Removed)
        .map(|path| path.path.as_str().to_owned())
        .collect();
    let kept: Vec<Value> = report
        .paths
        .iter()
        .filter_map(|path| match path.action {
            Action::Refused(refusal) => {
                Some(json!({"path":path.path.as_str(),"reason":kept_reason(refusal)}))
            }
            _ => None,
        })
        .collect();
    let released: Vec<String> = report
        .released
        .iter()
        .map(|path| path.as_str().to_owned())
        .collect();
    let next_tail: &[&str] = if dry_run {
        &["vault", "remove"]
    } else if kept.is_empty() {
        &["vault", "status"]
    } else {
        &["vault", "conflicts"]
    };
    let next_argv = argv(workspace, next_tail);
    let value = json!({
        "dry_run":dry_run,
        "counts":{
            "removed":removed.len(),
            "already_absent":report.paths.iter().filter(|path| path.action == Action::Unchanged).count(),
            "kept":kept.len(),
            "released":released.len(),
            "handed_over":report.handed_over.len(),
            "swept":report.swept.len(),
        },
        "removed":capped(&removed),
        "kept":capped(&kept),
        "released":capped(&released),
        "handed_over":capped(&report.handed_over),
        "removed_prefix_folder":report.removed_root,
        "lists_capped_at":MAX_LISTED,
        "next":[{"kind":if dry_run { "local_write" } else { "local_read" },"argv":next_argv}],
    });
    let mut text = format!(
        "{} {} generated file(s); kept {} edited or unsafe, {} released, {} in Notes/Reconciled/",
        if dry_run { "would remove" } else { "removed" },
        removed.len(),
        kept.len(),
        released.len(),
        report.handed_over.len()
    );
    for entry in kept.iter().take(20) {
        let _ = write!(
            text,
            "\nkept  {}  {}",
            entry["path"].as_str().unwrap_or_default(),
            entry["reason"].as_str().unwrap_or_default()
        );
    }
    let _ = write!(text, "\nNext: {}", display_argv(&next_argv));
    Output::success(
        if dry_run {
            "vault_remove_preview"
        } else {
            "vault_removed"
        },
        value,
        text,
    )
}
