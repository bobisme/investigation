//! Annotation notes: attributed notes about other records. An annotation
//! carries no evidentiary weight; a note snapshot points at a retained
//! artifact whose bytes stay in the store. In the working vault, a snapshot
//! of a person-owned file (W11.14) also names the file, links to it and says
//! whether it still holds the captured bytes, as `vault note-snapshots`
//! reports; a review export never shows that comparison.

use std::fmt::Write as _;

use investigation_core::{PROGRAM_NAME, coverage, human_file::NoteSnapshot, selector_attest};

use super::Facts;
use crate::{
    PlanPath, RenderError,
    evidence::prose,
    human_files::LiveStatus,
    layout,
    note::{text_at, wire_word},
    text::inline_code,
};

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
            human_file_section(f);
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
    if coverage::is_plan(f.record) {
        coverage_section(f);
    }
    if selector_attest::is_attestation(f.record) {
        f.heading("Selector validation");
        f.out.push_str(
            "- An attributed attestation by a named reader, not a check by the core. The \
             Observation and the validation the core recorded are unchanged.\n",
        );
        f.word("Outcome", "selector_attestation.outcome");
        f.text("Validator", "selector_attestation.validator.name");
        f.text(
            "Validator version",
            "selector_attestation.validator.version",
        );
        f.word(
            "Recorded by the core",
            "selector_attestation.recorded_validation",
        );
        f.text("Rationale", "selector_attestation.rationale");
    }
    Ok(())
}

/// The person-owned file a note snapshot captured and whether it still holds
/// the captured bytes, as the working-vault render read it. Absent from a
/// review export, whose snapshot never carries the live files.
fn human_file_section(f: &mut Facts<'_, '_, '_>) {
    let case = f.notes.case();
    let Some(live) = case
        .live_human_file_for(f.record)
        .filter(|_| !f.notes.reviewing())
    else {
        return;
    };
    let Some(snapshot) = NoteSnapshot::of(case.state(), f.record) else {
        return;
    };
    let path = &snapshot.file.path;
    f.heading("Human file");
    let shown = linkable(path).map_or_else(
        || inline_code(path),
        |planned| layout::wikilink(&planned, path),
    );
    let _ = writeln!(
        f.out,
        "- Captured from: {shown}, a person's file in {}; the renderer never writes it",
        inline_code(&format!("{}/", snapshot.file.folder))
    );
    let size = snapshot.bytes.map_or_else(String::new, |bytes| {
        format!(", {} bytes", inline_code(&bytes.to_string()))
    });
    let _ = writeln!(
        f.out,
        "- Captured bytes: {}{size}",
        inline_code(snapshot.sha256.as_str())
    );
    let status = live.map(|file| file.status(&snapshot.sha256));
    let detail = match &status {
        Some(LiveStatus::Unchanged) => "it still holds the captured bytes".to_owned(),
        Some(LiveStatus::Changed { sha256, bytes }) => format!(
            "it now holds other bytes ({}, {} bytes). The annotation still relies on the \
             captured bytes; to rely on the edited version, take a new snapshot",
            inline_code(sha256.as_str()),
            inline_code(&bytes.to_string())
        ),
        Some(LiveStatus::Missing) => "nothing is at that path".to_owned(),
        Some(LiveStatus::Refused(reason)) => {
            format!("it was not read: {}", inline_code(reason))
        }
        None => "it was not read".to_owned(),
    };
    let _ = writeln!(
        f.out,
        "- Live file when this note was rendered: {}: {detail}",
        inline_code(status.as_ref().map_or("not_compared", LiveStatus::word))
    );
    let _ = writeln!(
        f.out,
        "- Compare it now: {}",
        inline_code(&format!(
            "{PROGRAM_NAME} --case {} vault note-snapshots --format json",
            case.case_id()
        ))
    );
}

/// `path` as a planned path a wikilink may target: only identifier
/// characters, as every generated link uses. Other paths are shown as code.
fn linkable(path: &str) -> Option<PlanPath> {
    path.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | ' '))
        .then(|| PlanPath::new(path).ok())
        .flatten()
}

/// A coverage plan's reading: the percentage only as its full statement,
/// which names the plan revision and the denominator.
fn coverage_section(f: &mut Facts<'_, '_, '_>) {
    f.heading("Coverage");
    let Ok(plan) = coverage::evaluate(f.notes.case().state(), f.record) else {
        f.out
            .push_str("- Coverage: the stored plan could not be read.\n");
        return;
    };
    let statement = plan.percentage.as_ref().map_or_else(
        || "no applicable cells".to_owned(),
        |p| prose(p.statement()),
    );
    let _ = writeln!(f.out, "- Coverage: {statement}");
    let _ = writeln!(
        f.out,
        "- Plan: {} plan, revision {}, rule {}",
        inline_code(&super::wire(&wire_word(&plan.plan_kind))),
        inline_code(&plan.plan_revision.to_string()),
        inline_code(plan.rules_version)
    );
    let _ = writeln!(f.out, "- Applicability: {}", prose(&plan.applicability));
    let _ = writeln!(
        f.out,
        "- Cells: {} declared (bound {}), {} excluded, denominator {}",
        inline_code(&plan.cartesian_size.to_string()),
        inline_code(&plan.cartesian_bound.to_string()),
        inline_code(&plan.excluded.to_string()),
        inline_code(&plan.denominator.to_string())
    );
    let counts: Vec<String> = plan
        .counts
        .iter()
        .map(|(state, count)| format!("{} {}", super::wire(state), inline_code(&count.to_string())))
        .collect();
    let _ = writeln!(f.out, "- Cell states: {}", counts.join(", "));
    if !plan.stale_coordinates.is_empty() {
        let _ = writeln!(
            f.out,
            "- Changed coordinate records: {}; revise the plan to pin their current revisions",
            inline_code(&plan.stale_coordinates.len().to_string())
        );
    }
    let _ = writeln!(
        f.out,
        "- Activity, not coverage: {} links naming {} records",
        inline_code(&plan.activity.links_recorded.to_string()),
        inline_code(&plan.activity.linked_records.to_string())
    );
}
