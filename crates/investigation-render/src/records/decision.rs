//! Decision notes: the effective state and choice first, then hard
//! conditions, the comparisons read, trade-offs, triggers for
//! reconsideration, and what an accepted decision was accepted over.

use std::fmt::Write as _;

use investigation_core::identity::Kind;

use super::{Facts, comparison::cell, refs};
use crate::{
    RenderError,
    evidence::{MAX_LISTED, prose, revision_link},
    note::text_at,
    text::inline_code,
};

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    let body = &f.record.body;
    f.effective();
    match text_at(body, "choice.kind") {
        Some("select") => f.reference("Chosen option", "choice.option_ref")?,
        Some("defer") => f.text("Deferred because", "choice.reason"),
        _ => f.out.push_str("- Choice: not recorded\n"),
    }
    f.reference("Question", "question_ref")?;
    f.strongest_objection()?;
    match refs(body.get("review_ref")).first() {
        Some(reference) => {
            let mut line = format!(
                "- Accepting review: {}",
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
        None => f
            .out
            .push_str("- Accepting review: none; this decision is not accepted\n"),
    }
    f.references(
        "Compared alternatives",
        "compared_alternatives",
        "none recorded",
    )?;
    let conditions: Vec<_> = body["hard_conditions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if conditions.is_empty() {
        f.out
            .push_str("- Hard conditions: none: every hard criterion is treated as settled\n");
    } else {
        f.out.push_str("- Hard conditions still to verify:\n");
        for condition in conditions.iter().take(MAX_LISTED) {
            if let Some(reference) = refs(condition.get("criterion_ref")).first() {
                let _ = writeln!(
                    f.out,
                    "  - {}: {}",
                    revision_link(f.notes, *reference)?,
                    prose(text_at(condition, "reason").unwrap_or_default())
                );
            }
        }
    }
    f.list("Trade-offs", "tradeoffs", "none recorded");
    f.list("Consequences", "consequences", "none recorded");
    f.text("Expected outcome", "expected_outcome");
    f.text("Check when", "check_when");
    f.text("Reopen trigger", "reopen_trigger");
    let reversals: Vec<_> = body["reversal_conditions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if reversals.is_empty() {
        f.out.push_str("- Reversal conditions: none recorded\n");
    } else {
        f.out.push_str("- Reversal conditions:\n");
        for reversal in reversals.iter().take(MAX_LISTED) {
            let mut named = Vec::new();
            for key in [
                "assumption_ref",
                "criterion_ref",
                "comparison_ref",
                "option_ref",
            ] {
                if let Some(reference) = refs(reversal.get(key)).first() {
                    named.push(f.notes.link(reference.id)?);
                }
            }
            let _ = writeln!(
                f.out,
                "  - {}{}",
                prose(text_at(reversal, "condition").unwrap_or_default()),
                if named.is_empty() {
                    String::new()
                } else {
                    format!(" (names {})", named.join(", "))
                }
            );
        }
    }
    f.references("Evidence", "evidence_refs", "none cited")?;
    f.references("Assumptions", "assumption_refs", "none cited")?;
    f.reference_present("Supersedes", "supersedes")?;
    f.reference_present("Superseded by", "superseded_by")?;
    f.text_present("Change reason", "change_reason");

    f.heading("Comparisons read");
    let comparisons = refs(body.get("comparison_refs"));
    if comparisons.is_empty() {
        f.out.push_str("No comparison is cited.\n");
    }
    let state = f.notes.case().state();
    for reference in comparisons.iter().take(MAX_LISTED) {
        let mut line = format!("- {}", revision_link(f.notes, *reference)?);
        if let Some(comparison) = state.revision(*reference)
            && comparison.header.id.kind() == Kind::Comparison
            && f.notes.visible(comparison)
        {
            line.push_str(&cell(f.notes, comparison, "criterion_ref")?);
            if let Some(current) = state.current(reference.id) {
                line.push_str(&super::status_note(f.notes, current));
            }
        }
        let _ = writeln!(f.out, "{line}");
    }
    if comparisons.len() > MAX_LISTED {
        let _ = writeln!(f.out, "- and {} more", comparisons.len() - MAX_LISTED);
    }
    accepted_over(f)
}

/// The challenges stored with an accepted decision, with each objection's
/// present status: accepting under disagreement keeps it visible.
fn accepted_over(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    let Some(challenges) = f
        .record
        .body
        .get("acceptance_challenges")
        .filter(|value| !value.is_null())
    else {
        return Ok(());
    };
    f.heading("Accepted over");
    let _ = writeln!(
        f.out,
        "Recorded at acceptance under the {} acknowledgment policy; never recomputed.\n",
        inline_code(&super::wire(
            text_at(challenges, "policy").unwrap_or("unknown")
        ))
    );
    let state = f.notes.case().state();
    let objections: Vec<_> = challenges["open_objections"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if objections.is_empty() {
        f.out.push_str("- Open objections: none\n");
    }
    for objection in objections.iter().take(MAX_LISTED) {
        let Some(reference) = refs(objection.get("objection_ref")).first().copied() else {
            continue;
        };
        let mut line = format!(
            "- Open {} objection {}",
            inline_code(&super::wire(
                text_at(objection, "severity").unwrap_or("unknown")
            )),
            revision_link(f.notes, reference)?
        );
        if let Some(current) = state.current(reference.id)
            && f.notes.visible(current)
        {
            let _ = write!(
                line,
                ", now {}",
                inline_code(&super::wire(
                    text_at(&current.body, "status").unwrap_or("unknown")
                ))
            );
            match text_at(objection, "acknowledgment") {
                Some(text) => {
                    let _ = write!(line, "; acknowledged: {}", prose(text));
                }
                None => line.push_str("; not acknowledged"),
            }
        }
        let _ = writeln!(f.out, "{line}");
    }
    let uncertain = refs(challenges.get("uncertain_comparisons"));
    f.reference_list(
        "Uncertain comparisons (unknown outcome or unverified evidence)",
        &uncertain,
        "none",
    )?;
    let conditional = refs(challenges.get("conditional_hard_criteria"));
    f.reference_list(
        "Hard criteria accepted as conditions, not as satisfied",
        &conditional,
        "none",
    )
}
