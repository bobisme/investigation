//! Assessment notes: what evidence says about one subject, against which
//! rivals, with what confidence, and whether that judgment is still current.

use std::fmt::Write as _;

use super::{Facts, refs};
use crate::{
    RenderError,
    evidence::{MAX_LISTED, prose, revision_link},
    note::text_at,
    text::inline_code,
};

fn relation_meaning(relation: &str) -> &'static str {
    match relation {
        "supports" => "the evidence is more expected if the subject holds",
        "contradicts" => "the evidence is less expected if the subject holds",
        "non_diagnostic" => {
            "the evidence is about as expected under the rivals, so it does not discriminate"
        }
        "inconclusive" => "the evidence does not settle the relation",
        _ => "",
    }
}

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    f.effective();
    f.reference("Subject", "subject_ref")?;
    let relation = text_at(&f.record.body, "relation").unwrap_or("unknown");
    let _ = writeln!(
        f.out,
        "- Relation: {}: {}",
        inline_code(&super::wire(relation)),
        relation_meaning(relation)
    );
    f.text("Rationale", "rationale");
    f.references("Evidence", "evidence_refs", "none cited")?;
    f.references("Assumptions", "assumption_refs", "none cited")?;
    let expectations: Vec<_> = f.record.body["rival_expectations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if expectations.is_empty() {
        f.out.push_str(
            "- Rival expectations: none recorded; the evidence was not weighed against a rival\n",
        );
    } else {
        f.out.push_str("- Rival expectations:\n");
        for expectation in expectations.iter().take(MAX_LISTED) {
            let rival = match refs(expectation.get("rival_ref")).first() {
                Some(reference) => revision_link(f.notes, *reference)?,
                None => "an unresolved rival".to_owned(),
            };
            let _ = writeln!(
                f.out,
                "  - {rival}: {}. {}",
                inline_code(&super::wire(
                    text_at(expectation, "expectation").unwrap_or("unknown")
                )),
                prose(text_at(expectation, "rationale").unwrap_or_default())
            );
        }
    }
    f.judgment();
    f.reference_present("Supersedes", "supersedes")?;
    f.reference_present("Superseded by", "superseded_by")?;
    f.text_present("Status rationale", "rationale_for_status");
    Ok(())
}
