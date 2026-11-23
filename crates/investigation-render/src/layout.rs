//! Identity-based vault paths and links. Pure.
//!
//! File names come only from validated record IDs, never from titles or other
//! authored text, so a title change never renames a file or breaks a link.

use investigation_core::identity::{Id, Kind};

use crate::{PlanPath, RenderError};

pub const START_HERE: &str = "Start Here.md";
pub const CASES: &str = "Cases";
pub const OVERVIEW: &str = "Overview.md";

/// Top-level vault folders that belong to people. The publisher never writes
/// into them.
pub const HUMAN_OWNED_ROOTS: [&str; 3] = ["Notes", "Boards", "Inbox"];

/// Whether `path` lies inside a human-owned root folder, ignoring case.
pub fn is_human_owned(path: &PlanPath) -> bool {
    let mut components = path.components();
    let first = components.next().unwrap_or_default();
    components.next().is_some()
        && HUMAN_OWNED_ROOTS
            .iter()
            .any(|root| root.eq_ignore_ascii_case(first))
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

/// A vault-relative wikilink. `display` must already be safe link text, such
/// as the output of [`crate::text::display_text`] or a record ID.
pub fn wikilink(path: &PlanPath, display: &str) -> String {
    format!("[[{}|{display}]]", link_target(path))
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
