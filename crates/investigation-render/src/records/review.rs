//! Review notes: kind, subject, verdict and basis, and whether the review
//! still covers its subject. A stale review says so before anything else.
//! Validation and blindspot reviews share the note kind; `inv_review_kind`
//! tells them apart.

use std::fmt::Write as _;

use investigation_core::state::Record;
use serde_json::Value;

use super::{Facts, refs};
use crate::{
    RenderError,
    evidence::{MAX_LISTED, prose},
    note::{NoteContext, text_at},
    text::inline_code,
    yaml::Frontmatter,
};

/// The snapshot sequence a review's basis reflects: the audited snapshot of
/// a blindspot review, or for a validation review the latest acceptance among
/// the exact revisions it read (subject and read set).
fn basis_sequence(notes: &NoteContext<'_>, record: &Record) -> Option<u64> {
    if !notes.shows_positions() {
        return None;
    }
    let body = &record.body;
    if let Some(sequence) = body["audited_snapshot"]["sequence"].as_u64() {
        return Some(sequence);
    }
    let state = notes.case().state();
    refs(body.get("subject_ref"))
        .into_iter()
        .chain(refs(body.get("read_set")))
        .filter_map(|reference| state.revision(reference))
        .map(|read| read.header.acceptance_sequence)
        .max()
}

pub(super) fn properties(notes: &NoteContext<'_>, record: &Record, front: &mut Frontmatter) {
    if let Some(kind) = text_at(&record.body, "kind") {
        front.text("inv_review_kind", &super::wire(kind));
    }
    if let Some(verdict) = text_at(&record.body, "verdict") {
        front.text("inv_verdict", &super::wire(verdict));
    }
    if let Some(sequence) = basis_sequence(notes, record) {
        front.integer("inv_basis_sequence", sequence);
    }
    front.text(
        "inv_freshness",
        super::reviews::freshness_word(notes, record),
    );
}

/// A `[!warning]` callout when a review or retrospective is not current.
pub(super) fn stale_callout(f: &mut Facts<'_, '_, '_>, what: &str) -> Result<(), RenderError> {
    let word = super::reviews::freshness_word(f.notes, f.record);
    let meaning = match word {
        "stale" => format!(
            "This {what} no longer covers the current basis of what it read. It cannot stand in \
             for a fresh {what}."
        ),
        "disputed" => {
            format!("An open objection disputes what this {what} covers; its verdict is contested.")
        }
        "superseded" => format!("A later {what} replaced this one."),
        _ => return Ok(()),
    };
    let _ = writeln!(
        f.out,
        "> [!warning] {} {what}\n> {meaning}",
        match word {
            "stale" => "Stale",
            "disputed" => "Disputed",
            _ => "Superseded",
        }
    );
    if let Some(effective) = f.notes.effective(f.record) {
        for reason in effective.reasons.iter().take(MAX_LISTED) {
            let _ = writeln!(f.out, "> - {}", f.notes.reason_text(reason)?);
        }
        if effective.reasons.len() > MAX_LISTED {
            let _ = writeln!(
                f.out,
                "> - and {} more",
                effective.reasons.len() - MAX_LISTED
            );
        }
    }
    f.out.push('\n');
    Ok(())
}

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    stale_callout(f, "review")?;
    let body = &f.record.body;
    let kind = text_at(body, "kind").unwrap_or("unknown");
    let _ = writeln!(
        f.out,
        "- Kind: {}; freshness at this snapshot {}",
        inline_code(&super::wire(kind)),
        inline_code(super::reviews::freshness_word(f.notes, f.record))
    );
    f.reference("Subject", "subject_ref")?;
    if let Some(revision) = body["subject_ref"]["revision"].as_u64()
        && let Some(id) = text_at(body, "subject_ref.id").and_then(|id| id.parse().ok())
        && let Some(current) = f.notes.case().state().current(id)
        && current.header.revision.0 != revision
    {
        let _ = writeln!(
            f.out,
            "  - The subject is now at revision {}; see the freshness above for whether that \
             change matters.",
            current.header.revision.0
        );
    }
    f.word("Verdict", "verdict");
    f.word("Status", "status");
    f.text("Method", "method");
    f.word("Context isolation", "context_isolation");
    if body["context_isolation"] == "same_session_self_review" {
        let _ = writeln!(
            f.out,
            "- Low independence: same-session self-review, declared by the reviewer"
        );
    }
    if let Some(sequence) = basis_sequence(f.notes, f.record) {
        let read = refs(body.get("read_set")).len();
        let _ = writeln!(
            f.out,
            "- Basis: {read} exact revision{} read, reflecting sequence {sequence}",
            if read == 1 { "" } else { "s" }
        );
    }
    f.word_present("Basis digest", "basis_digest");
    if kind == "blindspot" {
        blindspot(f)?;
    } else {
        f.list("Caveats", "caveats", "none recorded");
        f.list(
            "Shared dependencies",
            "shared_dependencies",
            "none recorded",
        );
        f.references("Materials examined", "materials_examined", "none listed")?;
        f.references("Objections considered", "objection_refs", "none")?;
        memberships(f);
    }
    context(f);
    Ok(())
}

fn blindspot(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    let body = &f.record.body;
    if let Some(snapshot) = body.get("audited_snapshot").filter(|s| !s.is_null())
        && f.notes.shows_positions()
    {
        let _ = writeln!(
            f.out,
            "- Audited snapshot: sequence {}, head {}",
            snapshot["sequence"].as_u64().unwrap_or_default(),
            inline_code(snapshot["head_hash"].as_str().unwrap_or("unknown"))
        );
    }
    f.text("Scope examined", "scope_examined");
    f.list("Checks considered", "checks_considered", "none recorded");
    f.list("Gaps identified", "gaps", "none identified");
    f.text_present("Why no gap was identified", "no_gap_reason");
    f.sentence(
        "Zero gaps means none was identified within the examined scope, not that none exists.",
    );
    f.list("Limitations", "limitations", "none recorded");
    f.references("Evidence", "evidence_refs", "none cited")?;
    f.references("Assumptions", "assumption_refs", "none cited")
}

/// Scoped sets the review captured, by rule and size.
fn memberships(f: &mut Facts<'_, '_, '_>) {
    let sets: Vec<&Value> = f.record.body["memberships"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    if sets.is_empty() {
        return;
    }
    f.out.push_str("- Scoped sets captured:\n");
    for set in sets {
        let members = set["members"].as_array().map_or(0, Vec::len);
        let _ = writeln!(
            f.out,
            "  - {}: {members} member{}",
            inline_code(&super::wire(set["rule"].as_str().unwrap_or("unknown"))),
            if members == 1 { "" } else { "s" }
        );
    }
}

/// The reviewer's recorded context: what it saw and how independent it is.
fn context(f: &mut Facts<'_, '_, '_>) {
    let context = &f.record.body["context"];
    if context.is_null() {
        return;
    }
    f.heading("Reviewer context");
    let knowledge = |value: &Value| match value["state"].as_str() {
        Some("known") => inline_code(&value["value"].to_string()),
        _ => "unknown".to_owned(),
    };
    let _ = writeln!(
        f.out,
        "- Activity: {}",
        prose(context["activity"].as_str().unwrap_or_default())
    );
    let _ = writeln!(
        f.out,
        "- Saw the original claim: {}; gathered new evidence: {}",
        knowledge(&context["saw_original_conclusion"]),
        knowledge(&context["gathered_new_evidence"])
    );
    let _ = writeln!(
        f.out,
        "- Dependence on the reviewed work: {}: {}",
        inline_code(&super::wire(
            context["dependencies"].as_str().unwrap_or("unknown")
        )),
        prose(context["dependence_basis"].as_str().unwrap_or_default())
    );
    let changes: Vec<&str> = context["method_or_source_changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    for change in changes.iter().take(MAX_LISTED) {
        let _ = writeln!(f.out, "- Method or source change: {}", prose(change));
    }
    // Independence declarations added later; absent means undeclared.
    let declared = |key: &str| match context.get(key) {
        None => "undeclared".to_owned(),
        Some(value) if value["state"] == "known" && value["value"].is_string() => {
            inline_code(&super::wire(value["value"].as_str().unwrap_or_default()))
        }
        Some(value) => knowledge(value),
    };
    let _ = writeln!(
        f.out,
        "- Work kind: {}; read other reviews first: {}",
        declared("work_kind"),
        declared("read_other_reviews")
    );
    match context.get("prior_involvement") {
        None => {
            let _ = writeln!(f.out, "- Prior involvement: undeclared");
        }
        Some(value) if value["state"] != "known" => {
            let _ = writeln!(f.out, "- Prior involvement: unknown");
        }
        Some(value) => {
            let items: Vec<&str> = value["value"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if items.is_empty() {
                let _ = writeln!(f.out, "- Prior involvement: none declared");
            }
            for item in items.iter().take(MAX_LISTED) {
                let _ = writeln!(f.out, "- Prior involvement: {}", prose(item));
            }
        }
    }
}
