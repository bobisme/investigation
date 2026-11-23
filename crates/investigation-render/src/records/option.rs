//! Option notes: eligibility, dependencies and trade-offs, and the
//! comparisons and decisions that use the option.

use investigation_core::identity::Kind;

use super::{Facts, citing, comparison::cell};
use crate::RenderError;

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    f.word("Eligibility", "lifecycle");
    f.text_present("Disposition reason", "disposition_reason");
    f.reference("Question", "question_ref")?;
    f.text_present("Context", "context");
    f.text_present("Feasibility", "feasibility");
    f.references("Depends on", "dependencies", "nothing recorded")?;
    f.list("Trade-off notes", "tradeoff_notes", "none recorded");
    f.text_present("Amendment reason", "amendment_reason");

    f.heading("Comparisons");
    let comparisons: Vec<_> = citing(f.notes, f.record, &["option_ref"])
        .into_iter()
        .filter(|(record, _)| record.header.id.kind() == Kind::Comparison)
        .collect();
    f.backlinks(
        &comparisons,
        "No comparison evaluates this option yet.",
        |notes, comparison| cell(notes, comparison, "criterion_ref"),
    )?;

    f.heading("Decisions");
    let decisions: Vec<_> = citing(f.notes, f.record, &[])
        .into_iter()
        .filter(|(record, _)| record.header.id.kind() == Kind::Decision)
        .collect();
    let id = f.record.header.id.to_string();
    f.backlinks(
        &decisions,
        "No decision names this option.",
        |_, decision| {
            Ok(
                if decision.body["choice"]["option_ref"]["id"].as_str() == Some(id.as_str()) {
                    ": chosen".to_owned()
                } else {
                    ": compared or named as a reversal alternative".to_owned()
                },
            )
        },
    )
}
