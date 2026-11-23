//! Annotation notes: attributed notes about other records. An annotation
//! carries no evidentiary weight; a note snapshot points at a retained
//! artifact whose bytes stay in the store.

use std::fmt::Write as _;

use super::Facts;
use crate::{RenderError, note::text_at, text::inline_code};

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    let body = &f.record.body;
    let _ = writeln!(
        f.out,
        "- Role: {}; evidentiary status {}: an annotation is commentary, not evidence",
        inline_code(&super::wire(text_at(body, "role").unwrap_or("note"))),
        inline_code(&super::wire(
            text_at(body, "evidentiary_status").unwrap_or("none")
        ))
    );
    f.references("About", "subject_refs", "no subject recorded")?;
    match text_at(body, "content.kind") {
        Some("text") => f.text("Text", "content.text"),
        Some("artifact") => {
            f.reference("Note snapshot", "content.artifact_ref")?;
            f.out.push_str(
                "  - The snapshot's bytes are retained in the store and are not copied into the vault.\n",
            );
        }
        _ => {}
    }
    if body
        .get("classification_change")
        .is_some_and(|c| !c.is_null())
    {
        f.word("Classification from", "classification_change.from");
        f.word("Classification to", "classification_change.to");
        f.text("Reason", "classification_change.reason");
    }
    Ok(())
}
