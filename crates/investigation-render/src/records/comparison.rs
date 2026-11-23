//! Comparison notes: one option against one criterion, the measurement and
//! outcome, the read set it relied on, and its limits.

use std::fmt::Write as _;

use investigation_core::state::Record;

use super::{Facts, decimal, refs};
use crate::{RenderError, note::NoteContext, note::text_at, text::inline_code};

/// What a comparison says, for a backlink on its option or criterion note:
/// the other end and the outcome.
pub(super) fn cell(
    notes: &NoteContext<'_>,
    comparison: &Record,
    other: &str,
) -> Result<String, RenderError> {
    let body = &comparison.body;
    let mut text = String::new();
    if let Some(reference) = refs(body.get(other)).first() {
        let _ = write!(text, ": {}", notes.link(reference.id)?);
    }
    let _ = write!(
        text,
        ", outcome {}",
        inline_code(&super::wire(text_at(body, "outcome").unwrap_or("unknown")))
    );
    if let Some(value) = body.get("value").and_then(decimal) {
        let _ = write!(text, ", value {value}");
    }
    Ok(text)
}

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    f.effective();
    f.reference("Option", "option_ref")?;
    f.reference("Criterion", "criterion_ref")?;
    let state = f.notes.case().state();
    if let Some(reference) = refs(f.record.body.get("criterion_ref")).first()
        && let Some(criterion) = state.revision(*reference)
        && f.notes.visible(criterion)
    {
        let _ = writeln!(
            f.out,
            "  - Cited criterion: {} criterion, threshold {}",
            inline_code(&super::wire(
                text_at(&criterion.body, "kind").unwrap_or("unknown")
            )),
            criterion
                .body
                .get("threshold")
                .and_then(super::criterion::threshold)
                .unwrap_or_else(|| "none recorded".to_owned())
        );
    }
    f.word("Outcome", "outcome");
    let value = f
        .record
        .body
        .get("value")
        .and_then(decimal)
        .unwrap_or_else(|| "not measured".to_owned());
    let _ = writeln!(f.out, "- Measured value: {value}");
    if text_at(&f.record.body, "basis.kind") == Some("unknown") {
        f.text("Unknown because", "basis.reason");
    }
    f.text("Method", "method");
    f.text("Uncertainty", "uncertainty");
    f.text_or_list("Limitations", "limitations");
    f.text("Rationale", "rationale");
    f.text_present("Change reason", "change_reason");
    f.sentence(
        "A comparison relates one option to one criterion under the method above. It is not a \
         universal ranking, and an unknown outcome is not a pass.",
    );

    f.heading("Read set");
    f.references("Evidence", "evidence_refs", "none cited")?;
    f.references("Assumptions", "assumption_refs", "none cited")
}
