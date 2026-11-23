//! `vault copy-map`: an editable, human-owned snapshot of a case's generated
//! map ([`investigation_render::copy_map`]).
//!
//! Vault-only: nothing is recorded in the journal, the manifest or any
//! canonical record, and the copy is never rendered over, adopted or
//! reported as drift. The copy needs a vault rendered at the current
//! snapshot, because its file nodes name generated notes.

use investigation_core::wire::{ErrorCode, Failure};
use investigation_render::{
    compat::Profile,
    copy_map::{CopyMapError, copy_map},
    working_policy,
};
use investigation_store::Store;
use serde_json::json;

use crate::{
    Global, Output,
    selection::select_case,
    vault::{argv, render_failure},
};

pub(crate) const AFTER_HELP: &str = "\
Copy the generated case map into Boards/ as a canvas you own. The generated
Cases/<case-id>/Map.canvas is rewritten on every render, so editing it is
drift; the copy is never rendered over, adopted or reported as drift, and
nothing drawn on it changes a record or writes to the journal.
The destination must be a new .canvas file under Boards/: an existing file or
link is never replaced, and a path outside Boards/ is refused. A text node on
the copy names the snapshot it was taken from. Its file nodes keep the paths
the notes had then; if a generated note later moves (vault reconcile
release-path) or goes, the node shows as missing: re-point or delete it, or
copy the current map to a new name.
The map needs a stated viewer release ([vault_policy] viewer_release in
investigation.toml) and a vault rendered at the current snapshot.
Examples:
  investigation vault copy-map --to Boards/my-map.canvas --format json
  investigation vault copy-map --case CASE_ID --to Boards/rival-map.canvas
Next: investigation vault status --format json";

#[derive(clap::Args)]
pub(crate) struct CopyMapArgs {
    /// New canvas file to create, under Boards/ (for example Boards/my-map.canvas).
    #[arg(long, value_name = "PATH")]
    to: String,
}

fn failure(store: &Store, error: CopyMapError) -> Failure {
    let workspace = store.workspace();
    let invalid = |message: String| Failure::new(ErrorCode::InvalidInput, message);
    match error {
        CopyMapError::Destination { path, reason } => invalid(format!(
            "Cannot copy the map to {path:?}: {reason}. Name a new .canvas file under Boards/, \
             for example Boards/my-map.canvas."
        ))
        .at("/to"),
        CopyMapError::Exists(path) => invalid(format!(
            "{path} already exists, so nothing was copied and nothing was replaced. Choose a new \
             name under Boards/."
        ))
        .at("/to"),
        CopyMapError::Unsafe(path) => invalid(format!(
            "A link or file sits on the path of {path}, so nothing was copied. The copy never \
             follows links; choose a destination whose folders under Boards/ are real folders."
        ))
        .at("/to"),
        CopyMapError::NotPlanned => {
            let release = &Profile::bundled().pinned_release.version;
            invalid(format!(
                "This vault plans no case map, so there is nothing to copy: the workspace states \
                 no supported Obsidian release. State yours in investigation.toml under \
                 [vault_policy] (for example viewer_release = \"{release}\"), render the vault, \
                 then copy the map again."
            ))
            .with_argv(argv(workspace, &["vault", "render"]))
        }
        CopyMapError::NothingToPlace => invalid(
            "This case has no record to place on a map yet, so there is nothing to copy. Record \
             a question, hypothesis or other record, render the vault, then copy the map."
                .to_owned(),
        )
        .with_argv(argv(workspace, &["vault", "render"])),
        CopyMapError::NotRendered { state } => invalid(format!(
            "The vault is {state} at the current snapshot, and the copy's file nodes name notes \
             a render creates. Render the vault, then copy the map again."
        ))
        .with_argv(argv(workspace, &["vault", "render"])),
        CopyMapError::Render(error) => render_failure(workspace, &error),
    }
}

pub(crate) fn run(
    store: &mut Store,
    global: &Global,
    args: &CopyMapArgs,
) -> Result<Output, Failure> {
    let case = select_case(store, global.case.as_deref())?;
    let policy = working_policy(store.workspace());
    let copied = copy_map(store, &policy, case, &args.to).map_err(|error| failure(store, error))?;
    let next = json!([{"kind":"local_read","argv":argv(store.workspace(), &["vault", "status"])}]);
    let text = format!(
        "copied  {}  ({} nodes, {} arrows)\nfrom {} at generation {}\nThe copy is yours: renders never write, adopt or report it, and nothing on it is recorded.\nNext: {}",
        copied.to,
        copied.nodes,
        copied.edges,
        copied.source,
        copied.generation,
        crate::display_argv(&argv(store.workspace(), &["vault", "status"])),
    );
    let value = json!({
        "to":copied.to,
        "case_id":copied.case_id,
        "source":copied.source,
        "generation":copied.generation,
        "nodes":copied.nodes,
        "edges":copied.edges,
        "bytes":copied.bytes,
        "owner":"human",
        "recorded":false,
        "next":next,
    });
    Ok(Output::success("vault_map_copied", value, text))
}
