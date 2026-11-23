//! Identity-based vault paths and links. Pure.
//!
//! File names come only from validated record IDs, never from titles or other
//! authored text, so a title change never renames a file or breaks a link.

use investigation_core::identity::{Id, Kind};

use crate::{PlanPath, RenderError};

pub const START_HERE: &str = "Start Here.md";
pub const CASES: &str = "Cases";
pub const OVERVIEW: &str = "Overview.md";
pub const ATTACHMENTS: &str = "Attachments";

/// Who may write in one area of the vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaOwner {
    /// Generated from the snapshot on every render; an edit is drift.
    Investigation,
    /// A person's. The publisher never writes, replaces, moves, deletes,
    /// adopts or sweeps anything inside, and a manifest that claims a path
    /// here is rejected.
    Person,
    /// Investigation's for approved derivative previews only
    /// ([`crate::Ownership::approves_attachment`]); every other file there
    /// is preserved and reported as unknown.
    ListedDerivatives,
}

/// What a row of [`VAULT_AREAS`] covers, by a path's first component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// A top-level file.
    File(&'static str),
    /// A top-level folder and everything below it.
    Folder(&'static str),
    /// Every top-level folder whose name starts with a dot, such as a
    /// viewer's settings folder. Plan paths can never name one. (The
    /// publisher's own hidden temporaries are files beside their targets.)
    HiddenFolders,
}

/// One area of the vault: where it is, who owns it, and how `Start Here.md`
/// describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultArea {
    pub locations: &'static [Location],
    pub owner: AreaOwner,
    /// The "Owner and use" cell of the Start Here table, addressed to the
    /// reader.
    pub guide: &'static str,
}

/// The ownership convention, defined once. `Start Here.md` prints it and the
/// publisher, manifest, sweep and drift rules enforce it through [`area`].
/// A path outside every row (a top-level name nobody planned) is unknown:
/// preserved and reported, never adopted.
///
/// Human notes can be linked from generated notes, and generated notes from
/// human ones, but a human note is not evidence: nothing in it is recorded
/// until it is explicitly added to a case, which retains its exact bytes by
/// hash, so a later edit never changes a recorded conclusion.
pub const VAULT_AREAS: &[VaultArea] = &[
    VaultArea {
        locations: &[Location::File(START_HERE), Location::Folder(CASES)],
        owner: AreaOwner::Investigation,
        guide: "Investigation. Regenerated from the snapshot on every render. Do not edit them: \
                an edit is drift, left in place, no longer updated and not recorded.",
    },
    VaultArea {
        locations: &[Location::Folder("Notes")],
        owner: AreaOwner::Person,
        guide: "You. Write your own analysis here; rendering never writes, moves or deletes \
                anything in it.",
    },
    VaultArea {
        locations: &[Location::Folder("Boards")],
        owner: AreaOwner::Person,
        guide: "You. Draw your own canvases and diagrams here; rendering never writes in it.",
    },
    VaultArea {
        locations: &[Location::Folder("Inbox")],
        owner: AreaOwner::Person,
        guide: "You. Drop candidate material here; rendering never writes in it, and it becomes \
                evidence only when you add it to a case with an `investigation` command.",
    },
    VaultArea {
        locations: &[Location::Folder(ATTACHMENTS)],
        owner: AreaOwner::ListedDerivatives,
        guide: "Investigation, for previews of reviewed derivatives only, and only when the view \
                policy allows them. Original artifacts are never copied here. Any other file in \
                it is left in place and reported as unknown.",
    },
    VaultArea {
        locations: &[Location::HiddenFolders],
        owner: AreaOwner::Person,
        guide: "You, including the viewer's own settings. Rendering never writes, moves or \
                deletes anything in them.",
    },
];

impl Location {
    /// Whether a top-level name falls in this location, ignoring ASCII case.
    pub fn covers(self, first: &str) -> bool {
        match self {
            Self::File(name) | Self::Folder(name) => name.eq_ignore_ascii_case(first),
            Self::HiddenFolders => first.starts_with('.'),
        }
    }
}

/// The area a top-level vault name belongs to, ignoring ASCII case.
pub fn area_of(first: &str) -> Option<&'static VaultArea> {
    VAULT_AREAS
        .iter()
        .find(|area| area.locations.iter().any(|location| location.covers(first)))
}

/// The area `path` belongs to, by its first component.
pub fn area(path: &PlanPath) -> Option<&'static VaultArea> {
    area_of(path.components().next().unwrap_or_default())
}

/// Whether a top-level vault name is a person's, ignoring ASCII case.
pub fn is_human_root(first: &str) -> bool {
    area_of(first).is_some_and(|area| area.owner == AreaOwner::Person)
}

/// Whether `path` is a human-owned folder or lies inside one, ignoring case.
pub fn is_human_owned(path: &PlanPath) -> bool {
    area(path).is_some_and(|area| area.owner == AreaOwner::Person)
}

/// Whether `path` lies in the attachments folder.
pub fn is_attachment(path: &PlanPath) -> bool {
    area(path).is_some_and(|area| area.owner == AreaOwner::ListedDerivatives)
}

/// `Attachments/<artifact-id>.<extension>`, the only shape an attachment
/// may have. The extension is lowercase ASCII letters and digits.
pub fn attachment(artifact: Id, extension: &str) -> Result<PlanPath, RenderError> {
    if artifact.kind() != Kind::Artifact || !valid_extension(extension) {
        return Err(RenderError::InvalidPath {
            path: format!("{ATTACHMENTS}/{artifact}.{extension}"),
            reason: "an attachment is an artifact ID with a short lowercase extension",
        });
    }
    PlanPath::new(format!("{ATTACHMENTS}/{artifact}.{extension}"))
}

/// The artifact an attachment path names, when it has exactly the shape
/// [`attachment`] builds.
pub fn attachment_artifact(path: &PlanPath) -> Option<Id> {
    let mut components = path.components();
    let (Some(folder), Some(name), None) =
        (components.next(), components.next(), components.next())
    else {
        return None;
    };
    let (stem, extension) = name.split_once('.')?;
    let id: Id = stem.parse().ok()?;
    (folder == ATTACHMENTS && id.kind() == Kind::Artifact && valid_extension(extension))
        .then_some(id)
}

fn valid_extension(extension: &str) -> bool {
    (1..=8).contains(&extension.len())
        && extension
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Record kinds with their own note, in display order: folder and singular name.
pub const RECORD_FOLDERS: &[(Kind, &str, &str)] = &[
    (Kind::Question, "Questions", "question"),
    (Kind::Hypothesis, "Hypotheses", "hypothesis"),
    (Kind::Option, "Options", "option"),
    (Kind::Criterion, "Criteria", "criterion"),
    (Kind::Comparison, "Comparisons", "comparison"),
    (Kind::Assumption, "Assumptions", "assumption"),
    (Kind::Source, "Sources", "source"),
    (Kind::Artifact, "Evidence", "evidence"),
    (Kind::Observation, "Observations", "observation"),
    (Kind::Inquiry, "Inquiries", "inquiry"),
    (Kind::Run, "Runs", "run"),
    (Kind::Assessment, "Assessments", "assessment"),
    (Kind::Conclusion, "Conclusions", "conclusion"),
    (Kind::Review, "Reviews", "review"),
    (Kind::Objection, "Objections", "objection"),
    (Kind::Retrospective, "Retrospectives", "retrospective"),
    (Kind::Decision, "Decisions", "decision"),
    (Kind::Annotation, "Annotations", "annotation"),
];

/// Folder and singular name for a kind that has its own note.
pub fn record_kind(kind: Kind) -> Option<(&'static str, &'static str)> {
    RECORD_FOLDERS
        .iter()
        .find(|(candidate, _, _)| *candidate == kind)
        .map(|&(_, folder, name)| (folder, name))
}

pub fn start_here() -> Result<PlanPath, RenderError> {
    PlanPath::new(START_HERE)
}

pub fn case_overview(case_id: Id) -> Result<PlanPath, RenderError> {
    PlanPath::new(format!("{CASES}/{case_id}/{OVERVIEW}"))
}

/// `Cases/<case-id>/<Folder>/<id>.md`, or `None` for kinds without a note.
pub fn record_note(case_id: Id, id: Id) -> Result<Option<PlanPath>, RenderError> {
    record_kind(id.kind())
        .map(|(folder, _)| PlanPath::new(format!("{CASES}/{case_id}/{folder}/{id}.md")))
        .transpose()
}

/// The link target for a planned note: its vault-relative path without `.md`.
/// Plan paths never contain link-breaking characters.
pub fn link_target(path: &PlanPath) -> &str {
    path.as_str().strip_suffix(".md").unwrap_or(path.as_str())
}

/// A vault-relative wikilink: the only way generated Markdown links anywhere.
/// The target is a validated plan path, never data. `display` passes through
/// [`crate::text::display_text`] again, which leaves safe labels and record
/// IDs unchanged, so no caller can smuggle markup into a label; an empty
/// label falls back to the file name.
pub fn wikilink(path: &PlanPath, display: &str) -> String {
    let target = link_target(path);
    debug_assert!(
        target
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | ' ')),
        "link target {target:?} is not built from identifiers"
    );
    let label = crate::text::display_text(display, usize::MAX)
        .unwrap_or_else(|| target.rsplit('/').next().unwrap_or(target).to_owned());
    format!("[[{target}|{label}]]")
}

/// A wikilink to one block of a planned note, such as an excerpt anchor.
/// The anchor is generated, never data: letters, digits and dashes only.
pub fn block_link(path: &PlanPath, anchor: &str, display: &str) -> String {
    debug_assert!(
        !anchor.is_empty()
            && anchor
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "block anchor {anchor:?} is not generated"
    );
    let link = wikilink(path, display);
    let target = link_target(path);
    link.replacen(
        &format!("[[{target}|"),
        &format!("[[{target}#^{anchor}|"),
        1,
    )
}

/// Stable lowercase kind word for `inv_kind` and tags, independent of folder
/// names and display labels.
pub const fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Case => "case",
        Kind::Question => "question",
        Kind::Source => "source",
        Kind::Artifact => "artifact",
        Kind::Observation => "observation",
        Kind::Hypothesis => "hypothesis",
        Kind::Assumption => "assumption",
        Kind::Option => "option",
        Kind::Criterion => "criterion",
        Kind::Comparison => "comparison",
        Kind::Assessment => "assessment",
        Kind::Conclusion => "conclusion",
        Kind::Decision => "decision",
        Kind::Review => "review",
        Kind::Objection => "objection",
        Kind::Annotation => "annotation",
        Kind::Retrospective => "retrospective",
        Kind::Inquiry => "inquiry",
        Kind::Run => "run",
        // Kinds without a note keep their ID prefix.
        other => other.prefix(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> PlanPath {
        PlanPath::new(text).unwrap()
    }

    #[test]
    fn the_ownership_table_covers_the_tool_and_human_areas_once() {
        let owner = |first: &str| area_of(first).map(|area| area.owner);
        assert_eq!(owner(START_HERE), Some(AreaOwner::Investigation));
        assert_eq!(owner(CASES), Some(AreaOwner::Investigation));
        assert_eq!(owner(ATTACHMENTS), Some(AreaOwner::ListedDerivatives));
        for root in [
            "Notes",
            "Boards",
            "Inbox",
            "notes",
            "BOARDS",
            ".obsidian",
            ".trash",
        ] {
            assert_eq!(owner(root), Some(AreaOwner::Person), "{root}");
            assert!(is_human_root(root));
        }
        assert_eq!(owner("Retired"), None);
        // No two rows claim the same name.
        let names: Vec<&str> = VAULT_AREAS
            .iter()
            .flat_map(|area| area.locations)
            .filter_map(|location| match location {
                Location::File(name) | Location::Folder(name) => Some(*name),
                Location::HiddenFolders => None,
            })
            .collect();
        for name in &names {
            assert_eq!(
                VAULT_AREAS
                    .iter()
                    .filter(|area| area.locations.iter().any(|l| l.covers(name)))
                    .count(),
                1,
                "{name}"
            );
        }
    }

    #[test]
    fn human_areas_include_their_folder_and_ignore_case() {
        for text in [
            "Notes",
            "Notes/a.md",
            "notes/x/y.md",
            "INBOX/z",
            "Boards/b.canvas",
        ] {
            assert!(is_human_owned(&path(text)), "{text}");
        }
        for text in [
            "Notes.md",
            "NotesX/a.md",
            "Cases/Notes/a.md",
            "Start Here.md",
        ] {
            assert!(!is_human_owned(&path(text)), "{text}");
        }
    }

    #[test]
    fn attachments_have_one_shape() {
        let artifact: Id = "art_01890a5d-ac96-774b-bcce-b302099a8057".parse().unwrap();
        let question: Id = "q_01890a5d-ac96-774b-bcce-b302099a8057".parse().unwrap();
        let planned = attachment(artifact, "png").unwrap();
        assert_eq!(planned.as_str(), format!("Attachments/{artifact}.png"));
        assert!(is_attachment(&planned));
        assert_eq!(attachment_artifact(&planned), Some(artifact));
        assert!(attachment(question, "png").is_err());
        assert!(attachment(artifact, "PNG").is_err());
        assert!(attachment(artifact, "").is_err());
        for text in [
            format!("Attachments/{question}.png"),
            format!("attachments/{artifact}.png"),
            format!("Attachments/sub/{artifact}.png"),
            format!("Attachments/{artifact}.tar.gz"),
            format!("Attachments/{artifact}"),
            "Attachments/picture.png".to_owned(),
        ] {
            assert_eq!(attachment_artifact(&path(&text)), None, "{text}");
        }
    }
}
