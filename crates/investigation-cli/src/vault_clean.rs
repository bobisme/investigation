//! `vault clean-conflicts`: remove the observed bytes `vault reconcile` set
//! aside in `local/conflicts/discarded/` and `local/conflicts/proposed/`
//! ([`investigation_render::set_aside`]).
//!
//! It is a preview unless `--apply` is given, like `retention reclaim`: the
//! removal cannot be undone, so it never happens without the flag. With it,
//! the areas are walked again under the render lock and each file is removed
//! only while its bytes still hash to the name reconcile kept it under.
//! Canonical state, the vault and the render manifest are not touched.

use std::fmt::Write as _;

use investigation_core::wire::{ErrorCode, Failure};
use investigation_render::set_aside::{Action, Area, CleanReport, clean, preview};
use investigation_store::Workspace;
use serde_json::{Value, json};

use crate::{Output, display_argv, vault::argv};

/// Entries listed per category in one response; counts are always complete.
const MAX_LISTED: usize = 50;

pub(crate) const AFTER_HELP: &str = "\
Remove the observed bytes vault reconcile set aside: local/conflicts/discarded/
(kept by --disposition discard) and local/conflicts/proposed/ (kept by
propose-change once the change was recorded). Without --apply this is a
preview: it lists what would be removed and kept, and writes nothing. With
--apply the areas are walked again under the render lock, and a file is removed
only while its bytes still hash to the prefix of the folder reconcile kept it
under; then the folders that leaves empty are removed. Anything else there (a
file changed since, a link, a file you put there) is listed as kept and never
touched. The withheld generated versions under local/conflicts/<generation>/
are not touched; vault conflicts lists them. Removed bytes cannot be recovered.
The vault, the journal and the render manifest are unchanged.
Examples:
  investigation --workspace ./casework vault clean-conflicts --format json
  investigation --workspace ./casework vault clean-conflicts --area discarded --apply --format json
Next: investigation --workspace ./casework vault clean-conflicts --apply --format json";

#[derive(clap::Args)]
pub(crate) struct CleanArgs {
    /// Clean only this area: discarded or proposed (repeatable); both when omitted.
    #[arg(long = "area", value_name = "AREA", value_parser = ["discarded", "proposed"])]
    areas: Vec<String>,
    /// Remove the identified files; without it nothing is written.
    #[arg(long)]
    apply: bool,
}

pub(crate) fn run(workspace: &Workspace, args: &CleanArgs) -> Result<Output, Failure> {
    let areas: Vec<Area> = if args.areas.is_empty() {
        Area::ALL.to_vec()
    } else {
        args.areas
            .iter()
            .map(|name| {
                Area::parse(name).ok_or_else(|| {
                    Failure::new(ErrorCode::InvalidInput, "Unknown area.").at("--area")
                })
            })
            .collect::<Result<_, _>>()?
    };
    let report = if args.apply {
        clean(workspace, &areas)
    } else {
        preview(workspace, &areas)
    }
    .map_err(|error| error.to_failure())?;
    Ok(output(workspace, args, &areas, &report))
}

fn capped(items: Vec<Value>) -> Vec<Value> {
    items.into_iter().take(MAX_LISTED).collect()
}

fn output(workspace: &Workspace, args: &CleanArgs, areas: &[Area], report: &CleanReport) -> Output {
    let gone = |action: &Action| {
        matches!(
            action,
            Action::Removed | Action::Vanished | Action::WouldRemove
        )
    };
    let removed: Vec<Value> = report
        .entries
        .iter()
        .filter(|entry| gone(&entry.action))
        .map(|entry| {
            json!({
                "path":entry.path,
                "area":entry.area.as_str(),
                "vault_path":entry.vault_path,
                "sha256":entry.sha256,
                "bytes":entry.bytes,
            })
        })
        .collect();
    let kept: Vec<Value> = report
        .entries
        .iter()
        .filter_map(|entry| match entry.action {
            Action::Kept(why) => Some(json!({
                "path":entry.path,
                "area":entry.area.as_str(),
                "vault_path":entry.vault_path,
                "reason":why.as_str(),
            })),
            _ => None,
        })
        .collect();
    let mut tail = vec!["vault", "clean-conflicts"];
    for area in &args.areas {
        tail.extend(["--area", area.as_str()]);
    }
    let next_tail: Vec<&str> = if !args.apply || report.truncated {
        tail.push("--apply");
        tail
    } else {
        vec!["vault", "status"]
    };
    let next_argv = argv(workspace, &next_tail);
    let (removed_count, kept_count) = (removed.len(), kept.len());
    let removed_key = if args.apply {
        "removed"
    } else {
        "would_remove"
    };
    let mut counts = json!({
        "removed_bytes":report.removed_bytes(),
        "kept":kept_count,
        "removed_folders":report.removed_folders.len(),
    });
    counts[removed_key] = json!(removed_count);
    let mut value = json!({
        "apply":args.apply,
        "areas":areas.iter().map(|area| area.directory()).collect::<Vec<_>>(),
        "counts":counts,
        "kept":capped(kept.clone()),
        "truncated":report.truncated,
        "lists_capped_at":MAX_LISTED,
        "next":[{"kind":"local_write","argv":next_argv}],
    });
    value[removed_key] = json!(capped(removed));
    let mut text = format!(
        "{} {removed_count} set-aside file(s), {} byte(s); kept {kept_count}",
        if args.apply {
            "removed"
        } else {
            "would remove"
        },
        report.removed_bytes(),
    );
    for entry in kept.iter().take(20) {
        let _ = write!(
            text,
            "\nkept  {}  {}",
            entry["path"].as_str().unwrap_or_default(),
            entry["reason"].as_str().unwrap_or_default()
        );
    }
    if report.truncated {
        text.push_str("\nstopped at the entry bound; run again for the rest");
    }
    if !args.apply {
        text.push_str(
            "\nPreview only: nothing was removed. Run again with --apply to remove them.",
        );
    }
    let _ = write!(text, "\nNext: {}", display_argv(&next_argv));
    Output::success(
        if args.apply {
            "vault_conflicts_cleaned"
        } else {
            "vault_clean_conflicts_preview"
        },
        value,
        text,
    )
}
