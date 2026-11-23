//! Retrospective notes: the closure account of methods, effective and
//! misleading steps, residual unknowns and bounded lessons, with the
//! snapshot it describes and whether that account is still current.
//! Lessons are proposals with transfer limits, never automatic doctrine.

use std::fmt::Write as _;

use investigation_core::state::Record;
use serde_json::Value;

use super::{Facts, ids, refs};
use crate::{
    RenderError,
    evidence::{MAX_LISTED, prose},
    note::{NoteContext, text_at},
    text::inline_code,
    yaml::Frontmatter,
};

pub(super) fn properties(notes: &NoteContext<'_>, record: &Record, front: &mut Frontmatter) {
    if let Some(sequence) = record.body["snapshot"]["sequence"].as_u64() {
        front.integer("inv_basis_sequence", sequence);
    }
    front.text(
        "inv_freshness",
        super::reviews::freshness_word(notes, record),
    );
}

/// Linked support of one step or lesson: bare IDs resolved in this case.
fn support(f: &Facts<'_, '_, '_>, item: &Value) -> Result<String, RenderError> {
    let links: Vec<String> = ids(item.get("support_refs"))
        .into_iter()
        .take(MAX_LISTED)
        .map(|id| f.notes.link(id))
        .collect::<Result<_, _>>()?;
    Ok(if links.is_empty() {
        "no support cited".to_owned()
    } else {
        format!("support {}", links.join(", "))
    })
}

fn steps(f: &mut Facts<'_, '_, '_>, path: &str, none: &str) -> Result<(), RenderError> {
    let items: Vec<Value> = f.record.body[path].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        let _ = writeln!(f.out, "{none}");
    }
    for item in items.iter().take(MAX_LISTED) {
        let support = support(f, item)?;
        let _ = writeln!(
            f.out,
            "- {} ({support})",
            prose(text_at(item, "description").unwrap_or_default())
        );
    }
    Ok(())
}

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    super::review::stale_callout(f, "retrospective")?;
    let body = &f.record.body;
    let _ = writeln!(
        f.out,
        "- Describes the case at sequence {}, head {}",
        body["snapshot"]["sequence"].as_u64().unwrap_or_default(),
        inline_code(body["snapshot"]["head_hash"].as_str().unwrap_or("unknown"))
    );
    f.reference("Case", "case_ref")?;
    f.word("Status", "status");
    f.reference_present("Superseded by", "successor_ref")?;
    f.text_present("Superseded because", "superseded_reason");
    f.text("Costs or constraints", "costs_or_constraints");

    f.heading("Methods attempted");
    let methods: Vec<Value> = body["methods_attempted"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if methods.is_empty() {
        f.out.push_str("None recorded.\n");
    }
    for method in methods.iter().take(MAX_LISTED) {
        let _ = write!(
            f.out,
            "- {}",
            prose(text_at(method, "name").unwrap_or_default())
        );
        if let Some(id) = text_at(method, "inquiry_ref").and_then(|id| id.parse().ok()) {
            let _ = write!(f.out, " under {}", f.notes.link(id)?);
        }
        f.out.push('\n');
    }

    f.heading("Effective steps");
    steps(f, "effective_steps", "None recorded.")?;
    f.heading("Invalid or misleading steps");
    let none = format!(
        "None recorded: {}",
        super::or_not_recorded(text_at(body, "none_observed_reason"))
    );
    steps(f, "invalid_or_misleading_steps", &none)?;

    f.heading("Residual unknowns");
    let unknowns: Vec<&str> = body["residual_unknowns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    crate::evidence::bullets(
        f.out,
        &unknowns.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        "None recorded.",
    );

    f.heading("Lessons");
    f.sentence(
        "Lessons are bounded proposals with stated transfer limits. Recording one never changes \
         doctrine or another case.",
    );
    let lessons: Vec<Value> = body["lessons"].as_array().cloned().unwrap_or_default();
    if lessons.is_empty() {
        let _ = writeln!(
            f.out,
            "No reusable lesson: {}",
            super::or_not_recorded(text_at(body, "no_reusable_lesson_reason"))
        );
    }
    for lesson in lessons.iter().take(MAX_LISTED) {
        let support = support(f, lesson)?;
        let _ = writeln!(
            f.out,
            "- {} ({support})\n  - Applies when: {}\n  - Transfer limits: {}",
            prose(text_at(lesson, "guidance").unwrap_or_default()),
            prose(text_at(lesson, "applies_when").unwrap_or_default()),
            prose(text_at(lesson, "transfer_limits").unwrap_or_default())
        );
    }
    let read = refs(body.get("read_set")).len();
    let _ = writeln!(
        f.out,
        "\nThe account read {read} exact revision{}; they are listed under the typed links.",
        if read == 1 { "" } else { "s" }
    );
    Ok(())
}
