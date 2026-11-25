//! `vault copy-map`: an editable, human-owned snapshot of a case's generated
//! map (plan §12.6, W12.04).
//!
//! The generated `Cases/<case-id>/Map.canvas` is tool-owned: editing it is
//! drift. This copies the map the current snapshot plans into `Boards/`,
//! which no render ever writes, adopts, sweeps or reports. Nothing here
//! touches the manifest, the journal or any canonical record.
//! - In existing-vault mode ([`crate::projection`]) `Boards/` is the one
//!   inside the managed prefix: the copy is written through the projection
//!   root and nothing outside the prefix is touched. Its file nodes carry
//!   the prefix, like the generated map's, so they resolve inside it. A
//!   manifest written for another root refuses the copy.
//!
//! - The copy is taken from the plan, not from the file on disk, so an edited
//!   (drifted) generated map still yields the generated one. It is refused
//!   unless the vault is complete at the current snapshot, because the
//!   copy's file nodes name generated notes that must already exist.
//! - The destination must be a `.canvas` file under `Boards/`. Existing
//!   files are never replaced (a no-replace rename, so a symlink or a file
//!   made meanwhile also counts as existing), and a link anywhere on the
//!   path is refused, never followed.
//! - A text node states the case, the snapshot, the renderer and the vault
//!   generation the copy was taken from. Nothing else about the copy is
//!   remembered anywhere.
//! - File nodes keep their vault-relative paths, which are fixed by record
//!   IDs. Obsidian rewrites them when a note is renamed inside Obsidian; a
//!   later render never rewrites a board. If a generated note is later moved
//!   (`vault reconcile --disposition release-path` relocates it) or removed,
//!   the board's node keeps pointing at the old path: Obsidian shows it as
//!   missing and the person re-points or deletes it. To start again from the
//!   current map, copy to a new name.

use serde_json::{Value, json};

use crate::{
    Generation, PlanPath, RENDERER_VERSION, RenderError, RenderLock, VerifiedSnapshot, ViewPolicy,
    layout,
    manifest::Manifest,
    map,
    plan::render,
    projection::Projection,
    publish::{Publisher, Walk, write_bytes_atomically},
};
use investigation_core::identity::Id;
use investigation_store::{Store, Workspace};

/// The only folder a copy may be written under.
pub const BOARDS: &str = "Boards";

/// Why no copy was made. Nothing in the vault or workspace changed.
#[derive(Debug)]
pub enum CopyMapError {
    /// The destination is not a `.canvas` file path under `Boards/`.
    Destination {
        path: String,
        reason: &'static str,
    },
    /// The view policy plans no map: the viewer release is not stated or
    /// does not support canvas files.
    NotPlanned,
    /// The viewer allows a map but this case has no record to place on it.
    NothingToPlace,
    /// The vault is not complete at the current snapshot; render first.
    NotRendered {
        state: &'static str,
    },
    /// A file or link already exists at the destination.
    Exists(String),
    /// A link or non-directory lies on the destination path.
    Unsafe(String),
    Render(RenderError),
}

impl From<RenderError> for CopyMapError {
    fn from(error: RenderError) -> Self {
        Self::Render(error)
    }
}

/// What a copy wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    pub to: String,
    pub case_id: Id,
    pub source: String,
    pub generation: String,
    pub nodes: usize,
    pub edges: usize,
    pub bytes: usize,
}

/// The vault-relative destination, validated: `Boards/<...>.canvas`, at
/// least one folder deep, every component a plain visible name.
pub fn destination(to: &str) -> Result<PlanPath, CopyMapError> {
    let bad = |reason| CopyMapError::Destination {
        path: to.to_owned(),
        reason,
    };
    let path = PlanPath::new(to).map_err(|_| {
        bad("it is not a plain vault-relative path (no absolute, hidden, empty or `..` parts)")
    })?;
    let mut components = path.components();
    if components.next() != Some(BOARDS) {
        return Err(bad("it is outside Boards/"));
    }
    if components.next().is_none() {
        return Err(bad("it names no file inside Boards/"));
    }
    drop(components);
    if !path.file_name().ends_with(".canvas") || path.file_name() == ".canvas" {
        return Err(bad("it is not a .canvas file"));
    }
    Ok(path)
}

fn note(case_id: Id, generation: &Generation, source: &str) -> String {
    let head = generation
        .heads()
        .iter()
        .find(|(id, _)| *id == case_id)
        .map_or_else(
            || "unknown".to_owned(),
            |(_, head)| format!("event {} ({})", head.sequence, head.head_hash),
        );
    format!(
        "# Snapshot of the case map\n\nCopied from `{source}` at case {case_id}, {head}, by \
         renderer {RENDERER_VERSION} (vault generation {}).\n\nThis board is yours. It is never \
         rendered over, adopted or reported as drift, and nothing on it changes a record. Its \
         file nodes keep the paths they had when it was copied; if a note later moves or is \
         removed, re-point or delete the node, or copy the current map to a new name.",
        generation.id()
    )
}

/// Copy the map of `case_id` to `to` under `Boards/`.
pub fn copy_map(
    store: &mut Store,
    policy: &ViewPolicy,
    case_id: Id,
    to: &str,
) -> Result<Copied, CopyMapError> {
    let target = destination(to)?;
    let lock = RenderLock::acquire(store.workspace(), crate::RENDER_LOCK_WAIT)?;
    let snapshot = VerifiedSnapshot::read(store)?;
    let workspace: &Workspace = store.workspace();
    let generation = Generation::new(RENDERER_VERSION, &snapshot, policy);
    let manifest = Manifest::load(workspace, &lock)?
        .manifest
        .bound_to(&Projection::of(workspace))?;
    let rendered = matches!(manifest.last_complete(), Some(last) if last.id == generation.id());
    if !rendered {
        return Err(CopyMapError::NotRendered {
            state: match manifest.last_complete() {
                None => "unrendered",
                Some(_) => "stale",
            },
        });
    }
    let source = layout::case_map(case_id)?;
    let plan = render(&snapshot, policy)?;
    let Some(entry) = plan.entry(source.as_str()) else {
        return Err(if policy.allows_all(map::FEATURES) {
            CopyMapError::NothingToPlace
        } else {
            CopyMapError::NotPlanned
        });
    };
    let mut canvas: Value = serde_json::from_slice(entry.content())
        .map_err(|error| RenderError::io(source.as_str(), error))?;
    let nodes = canvas["nodes"].as_array_mut().expect("the map has nodes");
    nodes.insert(
        0,
        json!({
            "id": "snapshot-note",
            "type": "text",
            "text": note(case_id, &generation, source.as_str()),
            "x": 0,
            "y": -760,
            "width": 640,
            "height": 280,
        }),
    );
    let counts = (nodes.len(), canvas["edges"].as_array().map_or(0, Vec::len));
    // In the viewer's layout, as the generated map is ([`crate::canvas`]).
    let bytes = crate::canvas::to_bytes(&canvas);

    let publisher = Publisher::open(workspace, &lock)?;
    let parents: Vec<&str> = target.components().collect();
    let (name, parents) = parents.split_last().expect("a destination has a name");
    let directory = match publisher.walk(&target, parents, true)? {
        Walk::Directory(directory) => directory,
        Walk::Missing | Walk::Unsafe => return Err(CopyMapError::Unsafe(target.as_str().into())),
    };
    let tag = generation.id().trim_start_matches("gen_");
    let tag = tag.get(..12).unwrap_or("copy-map");
    if !write_bytes_atomically(&directory, target.as_str(), name, &bytes, tag, false)? {
        return Err(CopyMapError::Exists(target.as_str().to_owned()));
    }
    Ok(Copied {
        to: target.as_str().to_owned(),
        case_id,
        source: source.as_str().to_owned(),
        generation: generation.id().to_owned(),
        nodes: counts.0,
        edges: counts.1,
        bytes: bytes.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_canvas_files_under_boards_are_destinations() {
        for good in ["Boards/map.canvas", "Boards/a/b/My map.canvas"] {
            assert_eq!(destination(good).unwrap().as_str(), good);
        }
        for bad in [
            "",
            "map.canvas",
            "Boards",
            "Boards/",
            "Boards/map.md",
            "Boards/map",
            "Boards/.canvas",
            "boards/map.canvas",
            "Notes/map.canvas",
            "Cases/x/Map.canvas",
            "/Boards/map.canvas",
            "Boards/../Cases/map.canvas",
            "Boards//map.canvas",
            "Boards/./map.canvas",
            "Boards/.hidden/map.canvas",
            "Boards\\map.canvas",
            "../Boards/map.canvas",
        ] {
            assert!(destination(bad).is_err(), "{bad:?}");
        }
    }
}
