//! Review freshness on both ends: the Review note and its subject's note.
//!
//! A review is judged at the rendered snapshot, never by its recorded status
//! alone. It is `stale` when the freshness overlay says its basis needs
//! review (the subject's substantive basis changed, the read set changed or
//! grew, a scoped set it captured changed membership), `disputed` when an
//! open critical objection or one raised after it disputes the subject, and
//! `superseded` when a later review replaced it. The subject note lists each
//! review with that word and the reasons, so the staleness is visible where a
//! reader looks for the claim.

use std::fmt::Write as _;

use investigation_core::{
    classification::Field,
    freshness::Freshness,
    identity::{Id, Kind},
    review::EffectiveDisposition,
    review_ops,
    state::Record,
};

use crate::{
    RenderError,
    evidence::MAX_LISTED,
    note::{NoteContext, text_at},
    text::inline_code,
    yaml::Frontmatter,
};

/// Kinds a validation or blindspot review may name as its subject, apart
/// from the case itself (shown on the Overview).
const REVIEWABLE: &[Kind] = &[
    Kind::Question,
    Kind::Conclusion,
    Kind::Decision,
    Kind::Hypothesis,
    Kind::Comparison,
];

/// Freshness reasons shown per review on a subject note.
const SHOWN_REASONS: usize = 3;

/// The effective freshness word of a Review or Retrospective at this
/// snapshot: `current`, `stale`, `disputed` or `superseded`.
pub(crate) fn freshness_word(notes: &NoteContext<'_>, record: &Record) -> &'static str {
    if super::lifecycle(record) == Some("superseded") {
        return "superseded";
    }
    if record.header.id.kind() == Kind::Review {
        let state = notes.case().state();
        match review_ops::effective_disposition_with(state, notes.freshness(), record) {
            EffectiveDisposition::Disputed => return "disputed",
            EffectiveDisposition::NeedsReview => return "stale",
            _ => {}
        }
    }
    match notes.effective(record).map(|effective| effective.freshness) {
        Some(Freshness::Disputed) => "disputed",
        Some(Freshness::NeedsReview) => "stale",
        Some(Freshness::Current) | None => "current",
    }
}

/// Current reviews (or objections) whose subject is `id`, in record order.
/// Records whose references are withheld are left out.
fn naming_subject<'a>(notes: &NoteContext<'a>, id: Id, kind: Kind) -> Vec<&'a Record> {
    let target = id.to_string();
    notes
        .case()
        .state()
        .records()
        .filter(|record| {
            record.header.id.kind() == kind
                && record.header.case_id == notes.case().case_id()
                && text_at(&record.body, "subject_ref.id") == Some(target.as_str())
                && notes.may_show(record, Field::Lineage)
        })
        .collect()
}

/// `inv_review_freshness` on a reviewed record: `disputed` or `stale` when
/// any submitted review of it is, `current` otherwise; absent when no review
/// names it.
pub(crate) fn properties(notes: &NoteContext<'_>, record: &Record, front: &mut Frontmatter) {
    let words: Vec<&str> = naming_subject(notes, record.header.id, Kind::Review)
        .into_iter()
        .filter(|review| notes.visible(review))
        .map(|review| freshness_word(notes, review))
        .filter(|word| *word != "superseded")
        .collect();
    if words.is_empty() {
        return;
    }
    let word = ["disputed", "stale"]
        .into_iter()
        .find(|worst| words.contains(worst))
        .unwrap_or("current");
    front.text("inv_review_freshness", word);
}

/// `## Reviews and objections` on a reviewable or challenged record.
pub(crate) fn section(
    notes: &NoteContext<'_>,
    out: &mut String,
    record: &Record,
) -> Result<(), RenderError> {
    let kind = record.header.id.kind();
    if kind == Kind::Case {
        return Ok(());
    }
    let reviews = naming_subject(notes, record.header.id, Kind::Review);
    let objections = naming_subject(notes, record.header.id, Kind::Objection);
    if reviews.is_empty() && objections.is_empty() && !REVIEWABLE.contains(&kind) {
        return Ok(());
    }
    out.push_str("\n## Reviews and objections\n\n");
    if !notes.visible(record) {
        out.push_str("Withheld.\n");
        return Ok(());
    }
    if reviews.is_empty() {
        out.push_str("No review of this record is recorded.\n");
    } else {
        out.push_str(
            "Each review is judged at this snapshot. A `stale` review no longer covers the current \
             basis of what it reviewed and cannot stand in for a fresh one.\n\n",
        );
    }
    for review in reviews.iter().take(MAX_LISTED) {
        review_line(notes, out, record, review)?;
    }
    if reviews.len() > MAX_LISTED {
        let _ = writeln!(out, "- and {} more reviews", reviews.len() - MAX_LISTED);
    }
    if !objections.is_empty() {
        out.push_str("\nObjections raised against this record:\n\n");
    }
    for objection in objections.iter().take(MAX_LISTED) {
        let _ = write!(out, "- {}", notes.link(objection.header.id)?);
        if notes.visible(objection) {
            let word = |path: &str| {
                inline_code(&super::wire(
                    text_at(&objection.body, path).unwrap_or("unknown"),
                ))
            };
            let _ = write!(out, ": {} objection, {}", word("severity"), word("status"));
            if let Some(revision) = objection.body["subject_ref"]["revision"].as_u64()
                && revision != record.header.revision.0
            {
                let _ = write!(out, ", raised against revision {revision}");
            }
        }
        out.push('\n');
    }
    if objections.len() > MAX_LISTED {
        let _ = writeln!(
            out,
            "- and {} more objections",
            objections.len() - MAX_LISTED
        );
    }
    Ok(())
}

/// The reviews whose subject is the case itself, one line each with its
/// freshness, for the case Overview. Returns how many there are.
pub(crate) fn case_reviews(
    notes: &NoteContext<'_>,
    out: &mut String,
    case: &Record,
) -> Result<usize, RenderError> {
    let reviews = naming_subject(notes, case.header.id, Kind::Review);
    if !reviews.is_empty() {
        out.push_str(
            "Each review is judged at this snapshot. A `stale` review no longer covers the current \
             basis of the case and cannot stand in for a fresh one.\n\n",
        );
    }
    for review in reviews.iter().take(MAX_LISTED) {
        review_line(notes, out, case, review)?;
    }
    if reviews.len() > MAX_LISTED {
        let _ = writeln!(out, "- and {} more reviews", reviews.len() - MAX_LISTED);
    }
    Ok(reviews.len())
}

fn review_line(
    notes: &NoteContext<'_>,
    out: &mut String,
    subject: &Record,
    review: &Record,
) -> Result<(), RenderError> {
    let _ = write!(out, "- {}", notes.link(review.header.id)?);
    if !notes.visible(review) {
        out.push_str(": details withheld by the view policy\n");
        return Ok(());
    }
    let body = &review.body;
    let word = freshness_word(notes, review);
    let _ = write!(
        out,
        ": {} review",
        inline_code(&super::wire(text_at(body, "kind").unwrap_or("unknown")))
    );
    if let Some(revision) = body["subject_ref"]["revision"].as_u64() {
        let _ = write!(out, " of revision {revision}");
        if revision != subject.header.revision.0 {
            let _ = write!(out, " (this is revision {})", subject.header.revision.0);
        }
    }
    let _ = write!(
        out,
        ", verdict {}, ",
        inline_code(&super::wire(text_at(body, "verdict").unwrap_or("unknown")))
    );
    let _ = writeln!(
        out,
        "freshness {}{}",
        inline_code(word),
        match word {
            "stale" =>
                ": this review no longer covers the current basis and cannot stand in for a fresh one",
            "disputed" => ": an open objection contests what it covers",
            _ => "",
        }
    );
    if matches!(word, "stale" | "disputed")
        && let Some(effective) = notes.effective(review)
    {
        for reason in effective.reasons.iter().take(SHOWN_REASONS) {
            let _ = writeln!(out, "  - {}", notes.reason_text(reason)?);
        }
        if effective.reasons.len() > SHOWN_REASONS {
            let _ = writeln!(
                out,
                "  - and {} more reasons on the review note",
                effective.reasons.len() - SHOWN_REASONS
            );
        }
    }
    Ok(())
}
