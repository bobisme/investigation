//! Assumption notes: the claim, what follows if it fails, its materiality
//! and support, and what relies on it.

use super::{Facts, citing};
use crate::RenderError;

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    f.text("Consequence if it fails", "consequence");
    f.word("Materiality", "materiality");
    f.word("Status", "status");
    f.text_present("Rationale", "rationale");
    f.references("Affects", "affected_refs", "none recorded")?;
    f.references("Supported by", "evidence_refs", "nothing cited")?;

    f.heading("Relied on by");
    let relying = citing(
        f.notes,
        f.record,
        &[
            "assumption_refs",
            "reversal_conditions.assumption_ref",
            "evidence_refs",
        ],
    );
    f.backlinks(
        &relying,
        "Nothing relies on this assumption yet.",
        |_, _| Ok(String::new()),
    )
}
