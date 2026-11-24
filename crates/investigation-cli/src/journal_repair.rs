//! `repair journal` (ADR 0016): restore the written bytes of journal files
//! another program reformatted without changing their content.

use investigation_core::wire::Failure;
use investigation_store::Store;
use serde_json::json;

use crate::{Output, display_argv, selection::workspace_argv};

pub(crate) const AFTER_HELP: &str = "\
Journal verification refuses a case genesis or event file whose bytes parse
but differ from the bytes this program writes for that content, when no later
file records them: whitespace, key order, escapes or a line ending changed
(NON_CANONICAL_JOURNAL_FILE). The program's serialization is deterministic,
so the written bytes follow from the content. A file is restored only when a witness records their hash: the
next journal file of the case, or, for a case's last file, the head the
index recorded. Without one, nothing is written and the refusal names the
file; restore the workspace from an archive instead.

Each file is replaced in one rename under the commit locks, so a crash
leaves either file whole, and the whole journal is verified again after.
A file whose content changed is never rewritten.";

#[derive(clap::Args)]
pub(crate) struct Args {
    /// List the files that would be restored and write nothing.
    #[arg(long)]
    dry_run: bool,
}

pub(crate) fn run(store: &mut Store, args: &Args) -> Result<Output, Failure> {
    let repair = store.repair_journal(args.dry_run)?;
    let verb = if repair.dry_run {
        "would restore"
    } else {
        "restored"
    };
    let mut text = format!("journal  {verb} {} file(s)\n", repair.files.len());
    for file in &repair.files {
        text.push_str(&format!(
            "  {}  {} -> {}  witness {}\n",
            file.path,
            file.found,
            file.restored,
            json!(file.witness).as_str().unwrap_or_default()
        ));
    }
    let mut check = workspace_argv(store.workspace());
    check.extend(["check".into(), "--format".into(), "json".into()]);
    text.push_str(&format!("Next: {}", display_argv(&check)));
    Ok(Output::success(
        if repair.dry_run {
            "journal_repair_planned"
        } else {
            "journal_repaired"
        },
        json!({
            "dry_run": repair.dry_run,
            "files": repair.files,
            "next": [{"kind": "local_read", "argv": check}],
        }),
        text,
    ))
}
