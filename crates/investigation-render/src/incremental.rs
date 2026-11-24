//! Incremental publication: a commit rewrites only the notes whose content
//! changed.
//!
//! The planner always produces the full plan for the snapshot, and the
//! publisher never writes a file that already holds the planned bytes. That
//! alone would still rewrite every note of a case on each commit to it,
//! because a case note names its basis, the case head, twice: in the
//! `inv_render_sequence` and `inv_snapshot` properties
//! ([`crate::NoteMarker::Case`]) and in the `Snapshot` row of its basis
//! table. This module decides when a note may keep the basis it already
//! names.
//!
//! **Decision.** An otherwise unchanged case note keeps its earlier basis.
//! Before publishing, each case note of the plan is restamped from its case
//! head `H` to the basis `B` its owned file names. It is published with
//! basis `B` only when all of these hold:
//!
//! - the manifest's last complete generation used this renderer version and
//!   view policy, and the manifest records basis `B` for the path (it keeps
//!   a basis only for bytes planned under those rules);
//! - `B` is a head on this case's verified chain, older than `H`;
//! - the restamped bytes hash to exactly the hash the path is owned at.
//!
//! The last condition is the proof. The owned bytes are, by induction, what
//! this renderer planned at `B`; the restamped plan equals them; so the note
//! is byte-identical to a full render at `B`, and differs from a full render
//! at `H` only in the basis it names. Nothing about which records a note
//! reads is tracked or cached: any change anywhere in a note, including a
//! sentence that mentions the head in another form, makes the comparison
//! fail, and the note is published as planned with basis `H`. The full plan
//! stays the source of truth.
//!
//! A note's basis therefore reads "the case head at which these exact bytes
//! were rendered; they are unchanged since". Whether a file belongs to the
//! last complete generation is the manifest's to say: every owned path
//! records the generation that last confirmed it, and `vault status` checks
//! it. `Start Here.md` explains this to the reader.
//!
//! A note the manifest does not own (after a lost manifest) is read from the
//! vault instead: when its bytes are the plan restamped to an earlier head
//! of its case that they name, it is adopted as it stands, exactly as a file
//! holding the planned bytes is ([`carry`]).
//!
//! [`PublishMode::Full`] publishes the plan as planned, so every case note
//! names its case head, as a vault rendered from scratch does. Both modes
//! skip files whose bytes already match and handle drift identically.

use investigation_core::{identity::Id, wire::Snapshot};
use investigation_store::journal::hash_bytes;

use crate::{
    CaseSnapshot, Manifest, NoteMarker, PlanPath, Publisher, RenderError, RenderPlan,
    VerifiedSnapshot, drift::read_regular, layout, note::snapshot_row, publish::Walk,
};

/// How a working-vault render treats notes whose content did not change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PublishMode {
    /// A case note whose bytes are unchanged apart from its basis keeps the
    /// basis it names and is not rewritten.
    #[default]
    Incremental,
    /// Every case note names its case head, as in a render from scratch.
    Full,
}

/// `content` with the basis it names moved from `from` to `to`: the
/// `inv_render_sequence` and `inv_snapshot` lines of its frontmatter and the
/// `Snapshot` row of its basis table, each found exactly once. `None` when
/// any of them is missing, repeated or not exactly as rendered.
pub fn rebase(content: &[u8], from: &Snapshot, to: &Snapshot) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(content).ok()?;
    let block = text.strip_prefix("---\n")?;
    // The frontmatter, from its opening fence through the line break that
    // ends its last property.
    let split = 4 + block.find("\n---\n")? + 1;
    let (front, body) = text.split_at(split);
    let mut front = front.to_owned();
    let old = NoteMarker::Case(from.clone()).properties();
    let new = NoteMarker::Case(to.clone()).properties();
    for ((key, old), (_, new)) in old.iter().zip(&new) {
        front = replace_once(
            &front,
            &format!("\n{key}: {old}\n"),
            &format!("\n{key}: {new}\n"),
        )?;
    }
    let body = replace_once(
        body,
        &format!("\n{}\n", snapshot_row(from)),
        &format!("\n{}\n", snapshot_row(to)),
    )?;
    Some(format!("{front}{body}").into_bytes())
}

fn replace_once(text: &str, old: &str, new: &str) -> Option<String> {
    let at = text.find(old)?;
    if text[at + 1..].contains(old) {
        return None;
    }
    Some(format!("{}{new}{}", &text[..at], &text[at + old.len()..]))
}

/// The case a path under `Cases/<case-id>/` belongs to.
fn case_of(path: &PlanPath) -> Option<Id> {
    let mut components = path.components();
    (components.next()? == layout::CASES)
        .then(|| components.next()?.parse().ok())
        .flatten()
}

/// Reads the regular file at a planned path, without following links;
/// `None` when there is none.
pub(crate) type ReadNote<'a> = dyn FnMut(&PlanPath) -> Result<Option<Vec<u8>>, RenderError> + 'a;

/// The bytes of the regular file at `path` in the vault, read without
/// following links; `None` when it is absent, unsafe to reach, not a regular
/// file or too large to be a note.
pub(crate) fn read_note(
    publisher: &Publisher<'_>,
    path: &PlanPath,
) -> Result<Option<Vec<u8>>, RenderError> {
    let components: Vec<&str> = path.components().collect();
    let (name, parents) = components
        .split_last()
        .expect("plan paths have at least one component");
    match publisher.walk(path, parents, false)? {
        Walk::Directory(directory) => read_regular(&directory, *name, path.as_str()),
        Walk::Missing | Walk::Unsafe => Ok(None),
    }
}

/// The earlier head `basis` of `case`, when it is one: older than its head
/// and on its verified chain.
fn earlier_head(case: &CaseSnapshot, basis: &Snapshot) -> bool {
    basis.sequence < case.head().sequence && case.head_at(basis.sequence).as_ref() == Some(basis)
}

/// `plan` with every case note that may keep its earlier basis restamped to
/// it (see the module documentation), or `None` when no note keeps one and
/// `plan` stands as it is. The generation, order, phases and every other
/// entry are unchanged.
///
/// A planned case note the manifest does not own is read from the vault
/// with `read`. When its bytes are the plan restamped to an earlier head of
/// its case that they name, the plan is restamped to match, so the
/// publisher adopts the file as it adopts one holding the planned bytes.
/// This is how a lost manifest is rebuilt without rewriting, or refusing as
/// foreign, the notes an earlier render left unchanged.
pub(crate) fn carry(
    plan: &RenderPlan,
    snapshot: &VerifiedSnapshot,
    manifest: &Manifest,
    read: &mut ReadNote<'_>,
) -> Result<Option<RenderPlan>, RenderError> {
    let generation = plan.generation();
    let same_rules = manifest.last_complete().is_some_and(|last| {
        last.renderer == generation.renderer()
            && last.policy == generation.policy()
            && manifest.workspace_id() == Some(generation.workspace_id())
    });
    let mut kept: Vec<(usize, Vec<u8>)> = Vec::new();
    for (index, entry) in plan.entries().iter().enumerate() {
        let path = entry.path();
        if !path.as_str().ends_with(".md") {
            continue;
        }
        let Some(case) = case_of(path).and_then(|case_id| snapshot.case(case_id)) else {
            continue;
        };
        let head = case.head();
        let rebased = match manifest.get(path) {
            Some(record) => {
                let Some(basis) = record.basis.as_ref().filter(|_| same_rules) else {
                    continue;
                };
                if !earlier_head(case, basis) {
                    continue;
                }
                rebase(entry.content(), head, basis)
                    .filter(|rebased| hash_bytes(rebased) == record.sha256)
            }
            None => {
                let Some(bytes) = read(path)? else {
                    continue;
                };
                let Some(NoteMarker::Case(basis)) = NoteMarker::read(&bytes) else {
                    continue;
                };
                if !earlier_head(case, &basis) {
                    continue;
                }
                rebase(entry.content(), head, &basis).filter(|rebased| *rebased == bytes)
            }
        };
        if let Some(rebased) = rebased {
            kept.push((index, rebased));
        }
    }
    if kept.is_empty() {
        return Ok(None);
    }
    let mut carried = plan.clone();
    for (index, rebased) in kept {
        carried.replace_content(index, rebased);
    }
    Ok(Some(carried))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(sequence: u64, byte: &str) -> Snapshot {
        Snapshot {
            sequence,
            head_hash: format!("sha256:{}", byte.repeat(32)).parse().unwrap(),
        }
    }

    fn note(basis: &Snapshot) -> String {
        let properties: String = NoteMarker::Case(basis.clone())
            .properties()
            .iter()
            .map(|(key, value)| format!("{key}: {value}\n"))
            .collect();
        format!(
            "---\ninv_id: \"q_1\"\n{properties}inv_schema: \"v\"\n---\n# Title\n\n| Basis | Value |\n|---|---|\n{}\n\n## Summary\n",
            snapshot_row(basis)
        )
    }

    #[test]
    fn rebase_moves_exactly_the_basis_stamps_and_round_trips() {
        let (old, new) = (head(3, "ab"), head(9, "cd"));
        let rebased = rebase(note(&new).as_bytes(), &new, &old).unwrap();
        assert_eq!(String::from_utf8(rebased.clone()).unwrap(), note(&old));
        assert_eq!(
            NoteMarker::read(&rebased),
            Some(NoteMarker::Case(old.clone()))
        );
        assert_eq!(rebase(&rebased, &old, &new).unwrap(), note(&new).as_bytes());
    }

    #[test]
    fn rebase_refuses_a_missing_repeated_or_different_stamp() {
        let (old, new) = (head(3, "ab"), head(9, "cd"));
        // The note does not name `old`.
        assert!(rebase(note(&new).as_bytes(), &old, &new).is_none());
        // No basis row, or two.
        let row = format!("{}\n", snapshot_row(&new));
        let without = note(&new).replace(&row, "");
        assert!(rebase(without.as_bytes(), &new, &old).is_none());
        let twice = note(&new).replace(&row, &format!("{row}{row}"));
        assert!(rebase(twice.as_bytes(), &new, &old).is_none());
        // A stamp-like line in the body is not frontmatter.
        let body_only = format!("# Title\n\ninv_render_sequence: 9\n{row}");
        assert!(rebase(body_only.as_bytes(), &new, &old).is_none());
        assert!(rebase(b"\xff", &new, &old).is_none());
    }

    #[test]
    fn case_of_reads_the_case_folder_only() {
        let id = "case_018d87fc-5234-7000-8000-000000000001";
        let path = PlanPath::new(format!("Cases/{id}/Overview.md")).unwrap();
        assert_eq!(case_of(&path).map(|id| id.to_string()).as_deref(), Some(id));
        assert_eq!(case_of(&PlanPath::new("Start Here.md").unwrap()), None);
        assert_eq!(
            case_of(&PlanPath::new("Cases/x/Overview.md").unwrap()),
            None
        );
    }
}
