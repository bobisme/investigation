//! Annotation notes: attributed notes about other records. An annotation
//! carries no evidentiary weight; a note snapshot points at a retained
//! artifact whose bytes stay in the store.

use std::fmt::Write as _;

use investigation_core::coverage;

use super::Facts;
use crate::{
    RenderError,
    evidence::prose,
    note::{text_at, wire_word},
    text::inline_code,
};

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    let body = &f.record.body;
    let _ = writeln!(
        f.out,
        "- Role: {}; evidentiary status {}: an annotation is commentary, not evidence",
        inline_code(&super::wire(text_at(body, "role").unwrap_or("note"))),
        inline_code(&super::wire(
            text_at(body, "evidentiary_status").unwrap_or("none")
        ))
    );
    f.references("About", "subject_refs", "no subject recorded")?;
    match text_at(body, "content.kind") {
        Some("text") => f.text("Text", "content.text"),
        Some("artifact") => {
            f.reference("Note snapshot", "content.artifact_ref")?;
            f.out.push_str(
                "  - The snapshot's bytes are retained in the store and are not copied into the vault.\n",
            );
        }
        _ => {}
    }
    if body
        .get("classification_change")
        .is_some_and(|c| !c.is_null())
    {
        f.word("Classification from", "classification_change.from");
        f.word("Classification to", "classification_change.to");
        f.text("Reason", "classification_change.reason");
    }
    if coverage::is_plan(f.record) {
        coverage_section(f);
    }
    Ok(())
}

/// A coverage plan's reading: the percentage only as its full statement,
/// which names the plan revision and the denominator.
fn coverage_section(f: &mut Facts<'_, '_, '_>) {
    f.heading("Coverage");
    let Ok(plan) = coverage::evaluate(f.notes.case().state(), f.record) else {
        f.out
            .push_str("- Coverage: the stored plan could not be read.\n");
        return;
    };
    let statement = plan.percentage.as_ref().map_or_else(
        || "no applicable cells".to_owned(),
        |p| prose(p.statement()),
    );
    let _ = writeln!(f.out, "- Coverage: {statement}");
    let _ = writeln!(
        f.out,
        "- Plan: {} plan, revision {}, rule {}",
        inline_code(&super::wire(&wire_word(&plan.plan_kind))),
        inline_code(&plan.plan_revision.to_string()),
        inline_code(plan.rules_version)
    );
    let _ = writeln!(f.out, "- Applicability: {}", prose(&plan.applicability));
    let _ = writeln!(
        f.out,
        "- Cells: {} declared (bound {}), {} excluded, denominator {}",
        inline_code(&plan.cartesian_size.to_string()),
        inline_code(&plan.cartesian_bound.to_string()),
        inline_code(&plan.excluded.to_string()),
        inline_code(&plan.denominator.to_string())
    );
    let counts: Vec<String> = plan
        .counts
        .iter()
        .map(|(state, count)| format!("{} {}", super::wire(state), inline_code(&count.to_string())))
        .collect();
    let _ = writeln!(f.out, "- Cell states: {}", counts.join(", "));
    if !plan.stale_coordinates.is_empty() {
        let _ = writeln!(
            f.out,
            "- Changed coordinate records: {}; revise the plan to pin their current revisions",
            inline_code(&plan.stale_coordinates.len().to_string())
        );
    }
    let _ = writeln!(
        f.out,
        "- Activity, not coverage: {} links naming {} records",
        inline_code(&plan.activity.links_recorded.to_string()),
        inline_code(&plan.activity.linked_records.to_string())
    );
}
