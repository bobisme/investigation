//! Run notes: why the run happened, how it executed, whether its evidence is
//! complete and valid, and what it retained and produced.

use std::fmt::Write as _;

use investigation_core::{header::SourceTime, identity::Kind, receipt_check};
use serde_json::Value;

use super::{Facts, citing};
use crate::{
    RenderError,
    evidence::{prose, source_time},
    note::text_at,
    text::inline_code,
};

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    let body = &f.record.body;
    let _ = writeln!(
        f.out,
        "- Intent: {} {}",
        inline_code(&super::wire(
            text_at(body, "intent.kind").unwrap_or("unknown")
        )),
        prose(text_at(body, "intent.intent").unwrap_or_default())
    );
    f.reference_present("Plan", "intent.plan_ref")?;
    f.text("Producer (as reported, not verified)", "producer");
    f.text_present("Tool", "tool");
    f.text("Environment", "environment");
    f.word("Execution status", "execution_status");
    f.word("Evidence completeness", "evidence_completeness");
    let validity = text_at(body, "evidence_validity").unwrap_or("unknown");
    let _ = writeln!(
        f.out,
        "- Evidence validity: {}{}",
        inline_code(&super::wire(validity)),
        if validity == "invalid" {
            ": its results must not ground claims"
        } else {
            ""
        }
    );
    f.word("Evidence kind", "evidence_kind");
    f.list("Variables changed", "variables_changed", "none");
    f.word_present("Chronology", "chronology");
    // Chronology is never shown without how it was established.
    if body.get("chronology").and_then(Value::as_str).is_some()
        && let Some(verification) = receipt_check::chronology_verification(body)
    {
        let _ = writeln!(
            f.out,
            "- Chronology verification: {}",
            inline_code(verification.as_str())
        );
    }
    // Reported times are source claims kept as provenance. They never order
    // records or strengthen the chronology above (§6.5).
    for (label, path) in [
        ("Started at (claimed)", "started_at"),
        ("Finished at (claimed)", "finished_at"),
    ] {
        if let Some(time) = body
            .get(path)
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::from_value::<SourceTime>(value.clone()).ok())
        {
            let _ = writeln!(f.out, "- {label}: {}", source_time(Some(&time)));
        }
    }
    f.text_present("Result assertion", "result_assertion");

    f.heading("Retained artifacts");
    f.references("Artifacts", "artifact_refs", "none retained")?;

    f.heading("Observations from this run");
    let observations: Vec<_> = citing(f.notes, f.record, &["run_ref"])
        .into_iter()
        .filter(|(record, _)| record.header.id.kind() == Kind::Observation)
        .collect();
    f.backlinks(&observations, "No observation cites this run.", |_, _| {
        Ok(String::new())
    })
}
