//! Conclusion notes: the effective disposition and the strongest outstanding
//! objection first, then contradicting records, support, the causal account
//! and the review that validated it.

use std::fmt::Write as _;

use super::{Facts, refs};
use crate::{
    RenderError,
    evidence::{MAX_LISTED, revision_link},
    note::text_at,
    text::inline_code,
};

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    f.effective();
    f.strongest_objection()?;
    f.text("Scope", "scope");
    f.text("Significance", "significance");
    match refs(f.record.body.get("review_ref")).first() {
        Some(reference) => {
            let mut line = format!(
                "- Validating review: {}",
                revision_link(f.notes, *reference)?
            );
            if let Some(review) = f.notes.case().state().current(reference.id)
                && f.notes.visible(review)
            {
                let _ = write!(
                    line,
                    ", {}",
                    inline_code(super::reviews::freshness_word(f.notes, review))
                );
            }
            let _ = writeln!(f.out, "{line}");
        }
        None => f.out.push_str(
            "- Validating review: none accepted; this claim is not validated (submitted reviews are listed below)\n",
        ),
    }
    f.word_present("Reviewed basis", "reviewed_basis");
    f.list("Caveats", "caveats", "none recorded");
    f.judgment();

    f.heading("Contradicting records");
    f.references("Contradicted by", "contradiction_refs", "none recorded")?;
    f.heading("Support");
    f.references("Supported by", "support_refs", "none recorded")?;

    let factors: Vec<_> = f.record.body["causal_factors"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let test = f
        .record
        .body
        .get("causal_test")
        .filter(|test| !test.is_null());
    if factors.is_empty() && test.is_none() {
        return Ok(());
    }
    f.heading("Causal account");
    for factor in factors.iter().take(MAX_LISTED) {
        if let Some(reference) = refs(factor.get("hypothesis_ref")).first() {
            let _ = writeln!(
                f.out,
                "- Factor: {} as {}",
                revision_link(f.notes, *reference)?,
                inline_code(&super::wire(text_at(factor, "role").unwrap_or("unknown")))
            );
        }
    }
    if let Some(test) = test {
        let _ = writeln!(
            f.out,
            "- Causal test of {}:",
            inline_code(text_at(test, "variable").unwrap_or("an unnamed variable"))
        );
        for (label, key) in [
            ("Factor", "factor_ref"),
            ("Baseline", "baseline"),
            ("Treatment", "treatment"),
            ("Reversal", "reversal"),
        ] {
            if let Some(reference) = refs(test.get(key)).first() {
                let _ = writeln!(
                    f.out,
                    "  - {label}: {}",
                    revision_link(f.notes, *reference)?
                );
            }
        }
        if let Some(boundary) = test.get("failure_boundary").filter(|b| !b.is_null()) {
            let _ = writeln!(
                f.out,
                "  - Failure boundary: {} {} {}",
                inline_code(&super::wire(
                    text_at(boundary, "operator").unwrap_or("unknown")
                )),
                inline_code(text_at(boundary, "value").unwrap_or_default()),
                inline_code(text_at(boundary, "unit").unwrap_or_default())
            );
        }
    }
    Ok(())
}
