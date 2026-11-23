//! Objection notes: what is challenged, how severely, with what evidence,
//! and how the challenge stands now.

use std::fmt::Write as _;

use super::Facts;
use crate::{RenderError, note::text_at, text::inline_code};

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    let body = &f.record.body;
    let severity = text_at(body, "severity").unwrap_or("unknown");
    let _ = writeln!(
        f.out,
        "- Severity: {}{}",
        inline_code(&super::wire(severity)),
        match severity {
            "critical" => ": while open it blocks validation and disputes its subject",
            "material" =>
                ": shown with its subject and recorded with any decision accepted over it",
            _ => "",
        }
    );
    f.word("Status", "status");
    f.reference("Challenges", "subject_ref")?;
    if let Some(revision) = body["subject_ref"]["revision"].as_u64()
        && let Some(id) = text_at(body, "subject_ref.id").and_then(|id| id.parse().ok())
        && let Some(current) = f.notes.case().state().current(id)
        && current.header.revision.0 != revision
    {
        let _ = writeln!(
            f.out,
            "  - The subject has changed since: it is now at revision {}.",
            current.header.revision.0
        );
    }
    f.text("Scope", "scope");
    f.reference_present("Raised in review", "review_ref")?;
    match text_at(body, "evidence_refs.state") {
        Some("unknown") => f.out.push_str("- Evidence: unknown; none was cited\n"),
        _ => f.references("Evidence", "evidence_refs.value", "none cited")?,
    }
    f.text_present("Resolution", "resolution_reason");
    if body["resolution_evidence_refs"]
        .as_array()
        .is_some_and(|items| !items.is_empty())
    {
        f.references("Resolution evidence", "resolution_evidence_refs", "none")?;
    }
    f.reference_present("Resolution review", "resolution_review_ref")
}
