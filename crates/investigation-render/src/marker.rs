//! Per-note generation markers. Pure.
//!
//! A multi-file render is not atomic to a reader: while it runs, or after it
//! stops early, the vault mixes files from two generations. Every Markdown
//! note therefore says which snapshot it reflects, in flat frontmatter
//! properties a reader can check without any other file:
//!
//! | Note                         | Properties                               |
//! |------------------------------|------------------------------------------|
//! | under `Cases/<case-id>/`     | `inv_render_sequence`, `inv_snapshot`    |
//! | elsewhere (`Start Here.md`)  | `inv_generation`                         |
//!
//! These are the only places a note names its generation. A case note
//! carries its own case's verified head, never the workspace generation ID:
//! its content is computed from that case's state alone (backlinks,
//! freshness and gaps span the case), so the case head is exactly its basis,
//! and a commit to one case leaves every other case's notes byte-identical.
//! A reader compares the marker with the heads of the last complete
//! generation in `local/render-manifest.json`: a note whose head differs
//! belongs to another, possibly unfinished, render.
//!
//! [`stamp_plan`] adds the markers after the templates have run, so it
//! composes with any template output: it inserts the properties into an
//! existing frontmatter block, or adds a block when there is none. A template
//! that already emits a marker property must emit the same value; a
//! different value is a planner bug and fails the plan.

use investigation_core::wire::{Digest, Snapshot};

use crate::{Generation, PlanBuilder, PlanPath, RenderError, RenderPlan, VerifiedSnapshot, layout};

/// The case journal sequence a case note reflects.
pub const SEQUENCE_KEY: &str = "inv_render_sequence";
/// The case journal head hash a case note reflects.
pub const SNAPSHOT_KEY: &str = "inv_snapshot";
/// The generation a workspace-wide note belongs to.
pub const GENERATION_KEY: &str = "inv_generation";

const FENCE: &str = "---";

/// Which snapshot one note reflects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteMarker {
    /// A note inside one case's folder: that case's verified head.
    Case(Snapshot),
    /// A workspace-wide note: the generation ID.
    Generation(String),
}

impl NoteMarker {
    /// The marker for a planned path: the case head for paths under
    /// `Cases/<case-id>/`, the generation for every other path.
    pub fn for_path(
        path: &PlanPath,
        snapshot: &VerifiedSnapshot,
        generation: &Generation,
    ) -> Result<Self, RenderError> {
        let mut components = path.components();
        if components.next() != Some(layout::CASES) {
            return Ok(Self::Generation(generation.id().to_owned()));
        }
        let case = components
            .next()
            .and_then(|case_id| case_id.parse().ok())
            .and_then(|case_id| snapshot.case(case_id))
            .ok_or_else(|| RenderError::Marker {
                path: path.as_str().to_owned(),
                reason: "the path is under Cases/ but names no case in the snapshot",
            })?;
        Ok(Self::Case(case.head().clone()))
    }

    /// The frontmatter properties, as `key` and YAML value text.
    pub fn properties(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::Case(head) => vec![
                (SEQUENCE_KEY, head.sequence.to_string()),
                (SNAPSHOT_KEY, format!("\"{}\"", head.head_hash)),
            ],
            Self::Generation(id) => vec![(GENERATION_KEY, format!("\"{id}\""))],
        }
    }

    /// Read the marker from a note's frontmatter, or `None` when the note
    /// carries none. Only the properties this module writes are parsed.
    pub fn read(content: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(content).ok()?;
        let (block, _) = frontmatter(text)?;
        let value = |key: &str| {
            block.iter().find_map(|line| {
                line.strip_prefix(key)
                    .and_then(|rest| rest.strip_prefix(": "))
            })
        };
        let unquote = |text: &str| {
            text.strip_prefix('"')
                .and_then(|text| text.strip_suffix('"'))
                .map(str::to_owned)
        };
        if let Some(id) = value(GENERATION_KEY).and_then(unquote) {
            return Some(Self::Generation(id));
        }
        let sequence = value(SEQUENCE_KEY)?.parse().ok()?;
        let head_hash: Digest = value(SNAPSHOT_KEY).and_then(unquote)?.parse().ok()?;
        Some(Self::Case(Snapshot {
            sequence,
            head_hash,
        }))
    }
}

/// The value of the frontmatter property `key` in `content`, unquoted when
/// it is a double-quoted string, or `None` when the note has no frontmatter
/// or no such property. Only flat `key: value` lines are read.
pub fn property(content: &[u8], key: &str) -> Option<String> {
    let text = std::str::from_utf8(content).ok()?;
    let (block, _) = frontmatter(text)?;
    let value = block.iter().find_map(|line| {
        line.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(':'))
            .map(str::trim)
    })?;
    Some(
        value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(value)
            .to_owned(),
    )
}

/// Lines of the leading frontmatter block (between the fences) and the byte
/// offset of the closing fence line. `None` when the text has no block.
fn frontmatter(text: &str) -> Option<(Vec<&str>, usize)> {
    let body = text.strip_prefix("---\n")?;
    let mut offset = FENCE.len() + 1;
    let mut lines = Vec::new();
    for line in body.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        if bare == FENCE {
            return Some((lines, offset));
        }
        lines.push(bare);
        offset += line.len();
    }
    None
}

/// `content` with the marker's properties in its frontmatter.
pub fn stamp(content: &str, marker: &NoteMarker) -> Result<String, &'static str> {
    let properties = marker.properties();
    let Some((block, closing)) = frontmatter(content) else {
        if content.starts_with("---\n") {
            return Err("the note opens a frontmatter block that never closes");
        }
        let mut out = String::from("---\n");
        for (key, value) in &properties {
            out.push_str(&format!("{key}: {value}\n"));
        }
        out.push_str("---\n");
        out.push_str(content);
        return Ok(out);
    };
    let mut missing = String::new();
    for (key, value) in &properties {
        let expected = format!("{key}: {value}");
        let mut present = block.iter().filter(|line| {
            line.strip_prefix(key)
                .is_some_and(|rest| rest.starts_with(':'))
        });
        match present.next() {
            None => {
                missing.push_str(&expected);
                missing.push('\n');
            }
            Some(line) if *line == expected && present.next().is_none() => {}
            Some(_) => return Err("the template emits a different generation marker"),
        }
    }
    let mut out = String::with_capacity(content.len() + missing.len());
    out.push_str(&content[..closing]);
    out.push_str(&missing);
    out.push_str(&content[closing..]);
    Ok(out)
}

/// Stamp every Markdown entry of `plan` with its [`NoteMarker`]. Other files
/// (Bases, Canvas) are left as planned. Order, phases and projections are
/// unchanged; digests are recomputed.
pub fn stamp_plan(
    plan: RenderPlan,
    snapshot: &VerifiedSnapshot,
) -> Result<RenderPlan, RenderError> {
    let generation = plan.generation().clone();
    let mut builder = PlanBuilder::new(generation.clone());
    for entry in plan.entries() {
        let path = entry.path();
        let content = if path.as_str().ends_with(".md") {
            let marker = NoteMarker::for_path(path, snapshot, &generation)?;
            let invalid = |reason| RenderError::Marker {
                path: path.as_str().to_owned(),
                reason,
            };
            let text = std::str::from_utf8(entry.content())
                .map_err(|_| invalid("the note is not UTF-8"))?;
            stamp(text, &marker).map_err(invalid)?.into_bytes()
        } else {
            entry.content().to_vec()
        };
        builder.add(entry.phase(), path.clone(), content, entry.projects())?;
    }
    Ok(builder.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head() -> NoteMarker {
        NoteMarker::Case(Snapshot {
            sequence: 42,
            head_hash: format!("sha256:{}", "ab".repeat(32)).parse().unwrap(),
        })
    }

    #[test]
    fn a_note_without_frontmatter_gains_a_block_that_reads_back() {
        let stamped = stamp("# Title\n\nBody\n", &head()).unwrap();
        assert!(stamped.starts_with("---\ninv_render_sequence: 42\ninv_snapshot: \"sha256:"));
        assert!(stamped.ends_with("---\n# Title\n\nBody\n"));
        assert_eq!(NoteMarker::read(stamped.as_bytes()), Some(head()));
        assert_eq!(NoteMarker::read(b"# Title\n"), None);
    }

    #[test]
    fn existing_frontmatter_keeps_its_properties_and_gains_the_missing_ones() {
        let note = "---\ninv_id: \"q_1\"\ninv_render_sequence: 42\n---\n# Title\n";
        let stamped = stamp(note, &head()).unwrap();
        assert!(
            stamped.starts_with("---\ninv_id: \"q_1\"\ninv_render_sequence: 42\ninv_snapshot: ")
        );
        assert!(stamped.ends_with("\"\n---\n# Title\n"));
        assert_eq!(stamp(&stamped, &head()).unwrap(), stamped, "idempotent");
        assert_eq!(NoteMarker::read(stamped.as_bytes()), Some(head()));
    }

    #[test]
    fn a_conflicting_or_unterminated_marker_is_refused() {
        let conflicting = "---\ninv_render_sequence: 41\n---\n";
        assert!(stamp(conflicting, &head()).is_err());
        let doubled = "---\ninv_render_sequence: 42\ninv_render_sequence: 42\n---\n";
        assert!(stamp(doubled, &head()).is_err());
        assert!(stamp("---\ninv_id: x\n", &head()).is_err());
        // A longer key with the same prefix is a different property.
        let longer = "---\ninv_render_sequence_note: 1\n---\n";
        assert!(stamp(longer, &head()).is_ok());
    }

    #[test]
    fn generation_markers_round_trip() {
        let marker = NoteMarker::Generation("gen_abc".to_owned());
        let stamped = stamp("# Start Here\n", &marker).unwrap();
        assert_eq!(
            stamped,
            "---\ninv_generation: \"gen_abc\"\n---\n# Start Here\n"
        );
        assert_eq!(NoteMarker::read(stamped.as_bytes()), Some(marker));
    }
}
