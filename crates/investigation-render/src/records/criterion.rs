//! Criterion notes: hard or soft, scale, threshold and scope, and the
//! comparisons and decisions that use the criterion.

use std::fmt::Write as _;

use investigation_core::identity::Kind;
use serde_json::Value;

use super::{Facts, citing, comparison::cell, decimal, refs};
use crate::{RenderError, evidence::prose, note::text_at, text::inline_code};

/// A recorded threshold: measured (operator, value) or qualitative.
pub(crate) fn threshold(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    if let Some(requirement) = text_at(value, "requirement") {
        return Some(format!("qualitative: {}", prose(requirement)));
    }
    let operator = text_at(value, "operator").unwrap_or("unknown");
    let amount = value.get("value").and_then(decimal).unwrap_or_default();
    Some(format!("{} {amount}", inline_code(&super::wire(operator))))
}

fn unit_text(body: &Value) -> String {
    text_at(body, "scale.unit").map_or_else(String::new, |unit| format!(" {}", inline_code(unit)))
}

/// A weight is a preference: with a range it may enter a weighted score;
/// without one it is shown and never scored.
fn weight(f: &mut Facts<'_, '_, '_>, unit: String) {
    let body = &f.record.body;
    let Some(value) = text_at(body, "weight.value") else {
        return;
    };
    let value = inline_code(value);
    let line = match (
        text_at(body, "weight.range.worst"),
        text_at(body, "weight.range.best"),
    ) {
        (Some(worst), Some(best)) => format!(
            "- Weight: {value} for the swing from {}{unit} (worst) to {}{unit} (best); a stated preference, not a measurement\n",
            inline_code(worst),
            inline_code(best),
        ),
        _ => format!(
            "- Weight: {value}, a stated preference with no range; it never enters a weighted score\n"
        ),
    };
    f.out.push_str(&line);
}

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    let body = &f.record.body;
    let kind = text_at(body, "kind").unwrap_or_default();
    let meaning = match kind {
        "hard" => ": an option that fails it is ineligible, and an unknown result is not a pass",
        "soft" => ": a preference weighed against the others, never a gate",
        _ => "",
    };
    let _ = writeln!(
        f.out,
        "- Kind: {}{meaning}",
        inline_code(&super::wire(kind))
    );
    f.word("Lifecycle", "lifecycle");
    f.reference("Question", "question_ref")?;
    let unit = text_at(body, "scale.unit").map_or_else(|| "no unit".to_owned(), inline_code);
    let _ = writeln!(
        f.out,
        "- Scale: {unit}; {}",
        super::or_not_recorded(text_at(body, "scale.description"))
    );
    f.word("Preferred direction", "preferred_direction");
    let _ = writeln!(
        f.out,
        "- Threshold: {}",
        body.get("threshold")
            .and_then(threshold)
            .unwrap_or_else(|| "none recorded".to_owned())
    );
    let scope = refs(body.get("option_refs"));
    if scope.is_empty() {
        f.out
            .push_str("- Applies to: all options of the question\n");
    } else {
        f.reference_list("Applies to", &scope, "")?;
    }
    f.text_present("Weighting rationale", "weighting_rationale");
    weight(f, unit_text(body));
    f.text_present("Weight rationale", "weight.rationale");
    f.text_present("Weight stated by", "weight.stated_by");
    f.text_present("Change reason", "change_reason");
    f.word_present("Material change", "material_change");

    f.heading("Comparisons");
    let comparisons: Vec<_> = citing(f.notes, f.record, &["criterion_ref"])
        .into_iter()
        .filter(|(record, _)| record.header.id.kind() == Kind::Comparison)
        .collect();
    f.backlinks(
        &comparisons,
        "No comparison is recorded against this criterion yet.",
        |notes, comparison| cell(notes, comparison, "option_ref"),
    )?;

    f.heading("Decisions");
    let decisions: Vec<_> = citing(
        f.notes,
        f.record,
        &[
            "hard_conditions.criterion_ref",
            "reversal_conditions.criterion_ref",
        ],
    );
    let id = f.record.header.id.to_string();
    f.backlinks(
        &decisions,
        "No decision is conditioned on this criterion.",
        |_, decision| {
            let condition = decision.body["hard_conditions"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|item| text_at(item, "criterion_ref.id") == Some(id.as_str()));
            Ok(match condition {
                Some(condition) => format!(
                    ": hard condition: {}",
                    prose(text_at(condition, "reason").unwrap_or_default())
                ),
                None => ": named in a reversal condition".to_owned(),
            })
        },
    )
}
