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

/// A vault-relative wikilink. `display` must already be safe link text.
pub fn wikilink(path: &PlanPath, display: &str) -> String {
    let target = path.as_str().strip_suffix(".md").unwrap_or(path.as_str());
    format!("[[{target}|{display}]]")
}
