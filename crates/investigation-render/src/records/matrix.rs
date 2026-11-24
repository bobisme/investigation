//! The option-by-criterion matrix for one question, in Markdown.
//!
//! Rendered on the question note and on a decision's note from the same
//! consequences table the weighted analysis leads with: one cell per eligible
//! option and active criterion at one snapshot, each cell stating its
//! recorded state in words and linking its comparison. No overall score, rank
//! or weighted total is computed or shown here; hard failures and Pareto
//! status follow the table in words. Cell text is authored data and goes
//! through the escapers of [`crate::text`]. A large matrix is capped, with
//! the count of what is left out. A withheld option, criterion or comparison
//! shows by ID only.

use std::fmt::Write as _;

use investigation_core::{
    classification::Field,
    command::RevisionRef,
    decision_ops::{self, ParetoStatus},
    evaluate::{ComparisonBasis, ComparisonOutcome, CriterionKind},
    identity::Id,
    weighting::{self, CellState, TableCell},
};

use super::{Facts, token, wire};
use crate::{
    RenderError,
    evidence::MAX_LISTED,
    layout,
    note::{NoteContext, wire_word},
    text::{inline_text, table_code},
};

/// Most options shown as rows.
pub(crate) const MAX_ROWS: usize = 12;
/// Most criteria shown as columns.
pub(crate) const MAX_COLUMNS: usize = 8;
const BASIS_CHARS: usize = 60;

/// `### Option-by-criterion comparison` for `question`, or nothing when it
/// has no eligible option or no active criterion to compare.
pub(super) fn write(f: &mut Facts<'_, '_, '_>, question: Id) -> Result<(), RenderError> {
    let notes = f.notes;
    let Some(table) = notes.consequences_table(question) else {
        return Ok(());
    };
    if table.rows.is_empty() || table.criteria.is_empty() {
        return Ok(());
    }
    f.out.push_str("\n### Option-by-criterion comparison\n\n");
    let _ = writeln!(
        f.out,
        "Each cell is what is recorded for one option against one criterion at this snapshot, \
         linked to its comparison. This matrix computes no overall score and no ranking; an \
         unknown or missing cell is not a pass.\n"
    );
    let columns = &table.criteria[..table.criteria.len().min(MAX_COLUMNS)];
    let rows = &table.rows[..table.rows.len().min(MAX_ROWS)];

    f.out.push_str("| Option |");
    for criterion in columns {
        let kind = match criterion.kind {
            CriterionKind::Hard => "hard",
            CriterionKind::Soft => "soft",
        };
        let _ = write!(
            f.out,
            " {} ({kind}) |",
            notes.table_link(criterion.criterion_ref.id)?
        );
    }
    f.out.push_str("\n|---|");
    for _ in columns {
        f.out.push_str("---|");
    }
    f.out.push('\n');
    for row in rows {
        let option_shown = option_visible(notes, row.option_ref);
        let _ = write!(f.out, "| {} |", notes.table_link(row.option_ref.id)?);
        for (cell, criterion) in row.cells.iter().zip(columns) {
            let shown = option_shown && criterion_visible(notes, criterion.criterion_ref);
            let _ = write!(f.out, " {} |", cell_text(notes, cell, shown)?);
        }
        f.out.push('\n');
    }
    if table.rows.len() > rows.len() || table.criteria.len() > columns.len() {
        let _ = writeln!(
            f.out,
            "\nShowing {} of {} options and {} of {} criteria; the rest are listed on their own \
             notes and are not part of this table.",
            rows.len(),
            table.rows.len(),
            columns.len(),
            table.criteria.len()
        );
    }
    standing(f, question, table)?;
    Ok(())
}

fn option_visible(notes: &NoteContext<'_>, reference: RevisionRef) -> bool {
    notes
        .case()
        .state()
        .current(reference.id)
        .is_some_and(|record| notes.visible(record))
}

fn criterion_visible(notes: &NoteContext<'_>, reference: RevisionRef) -> bool {
    option_visible(notes, reference)
}

/// What one cell records beyond its state: the outcome and value when
/// recorded, a bounded basis statement for a preference, qualitative or
/// unknown cell, and the `evidence` count the caller may show. Shared by the vault matrix and the
/// exported decision record, so both say the same thing about a cell.
pub(crate) fn cell_details(cell: &TableCell, evidence: usize) -> Vec<String> {
    let mut details = Vec::new();
    if let Some(outcome) = cell.outcome {
        details.push(format!(
            "outcome {}",
            table_code(&wire(&wire_word(&outcome)))
        ));
    }
    if let Some(value) = &cell.value {
        details.push(format!(
            "{} {}",
            table_code(&token(value.decimal.as_str())),
            table_code(&value.unit)
        ));
    }
    match &cell.basis {
        Some(
            ComparisonBasis::Preference { statement } | ComparisonBasis::Qualitative { statement },
        ) => details.push(format!("\"{}\"", inline_text(statement, BASIS_CHARS))),
        Some(ComparisonBasis::Unknown { reason }) => {
            details.push(format!("because \"{}\"", inline_text(reason, BASIS_CHARS)));
        }
        None => {}
    }
    if evidence > 0 {
        details.push(format!("{evidence} evidence"));
    }
    details
}

/// A Pareto status in words.
pub(crate) const fn pareto_words(status: ParetoStatus) -> &'static str {
    match status {
        ParetoStatus::DisqualifiedHard => "disqualified by a hard criterion",
        ParetoStatus::StrictlyDominated => {
            "strictly dominated by another option on every compared criterion"
        }
        ParetoStatus::ParetoFrontier => "on the Pareto frontier",
        ParetoStatus::UncertainFrontier => {
            "on the frontier only provisionally, because some cells are uncertain"
        }
    }
}

/// One cell: its state in words, the outcome and value when recorded, a
/// bounded basis statement for a preference, qualitative or unknown cell,
/// and a link to each comparison.
fn cell_text(
    notes: &NoteContext<'_>,
    cell: &TableCell,
    shown: bool,
) -> Result<String, RenderError> {
    if !shown {
        return Ok("withheld".to_owned());
    }
    let state = state_words(cell.state);
    let link = |id: Id, label: &str| -> Result<String, RenderError> {
        let visible = notes
            .case()
            .state()
            .current(id)
            .is_some_and(|record| notes.may_show(record, Field::Content));
        Ok(match (visible, notes.note_path(id)?) {
            (true, Some(path)) => layout::wikilink(&path, label).replace('|', "\\|"),
            _ => notes.table_link(id)?,
        })
    };
    let mut text = match cell.comparison_refs.as_slice() {
        [only] => link(only.id, state)?,
        [] => state.to_owned(),
        many => {
            let mut ids = Vec::new();
            for reference in many.iter().take(3) {
                ids.push(link(reference.id, &reference.id.to_string())?);
            }
            format!("{state} ({})", ids.join(", "))
        }
    };
    let content_shown = cell.comparison_refs.iter().all(|reference| {
        notes
            .case()
            .state()
            .current(reference.id)
            .is_some_and(|record| notes.may_show(record, Field::Content))
    });
    if !content_shown {
        return Ok(text);
    }
    let details = cell_details(cell, cell.evidence_refs.len());
    if !details.is_empty() {
        text.push_str(": ");
        text.push_str(&details.join(", "));
    }
    Ok(text)
}

pub(crate) fn state_words(state: CellState) -> &'static str {
    match state {
        CellState::Measured => "measured",
        CellState::MeasuredUnverified => "measured, evidence unverified",
        CellState::Preference => "preference, not a measurement",
        CellState::Qualitative => "qualitative, not a measurement",
        CellState::Unknown => "unknown",
        CellState::NotApplicable => "not applicable",
        CellState::Missing => "unknown: no comparison recorded",
        CellState::Multiple => "multiple current comparisons, unresolved",
        CellState::NeedsReview => "needs review: comparison is out of date",
        CellState::OutOfScope => "out of scope",
    }
}

/// Hard failures and Pareto status in words, per option, bounded.
fn standing(
    f: &mut Facts<'_, '_, '_>,
    question: Id,
    table: &weighting::ConsequencesTable,
) -> Result<(), RenderError> {
    let notes = f.notes;
    let state = notes.case().state();
    let case = notes.case().case_id();
    f.out.push_str("\nHard failures:\n\n");
    let mut any = false;
    for row in table.rows.iter().take(MAX_ROWS) {
        if !option_visible(notes, row.option_ref) {
            continue;
        }
        let mut failed = Vec::new();
        for (cell, criterion) in row.cells.iter().zip(&table.criteria) {
            if criterion.kind == CriterionKind::Hard
                && cell.outcome == Some(ComparisonOutcome::Fails)
                && criterion_visible(notes, criterion.criterion_ref)
            {
                let link = notes.link(criterion.criterion_ref.id)?;
                failed.push(if cell.state == CellState::Measured {
                    link
                } else {
                    format!("{link} (recorded as failing, but its evidence is not verified)")
                });
            }
        }
        if failed.is_empty() {
            continue;
        }
        any = true;
        failed.truncate(MAX_LISTED);
        let _ = writeln!(
            f.out,
            "- {} fails the hard criterion: {}",
            notes.link(row.option_ref.id)?,
            failed.join(", ")
        );
    }
    if !any {
        f.out
            .push_str("- No shown option has a recorded hard failure.\n");
    }

    // Dominance among the shown options alone could make one read stronger
    // than it is, so a review export that leaves anything out lists none.
    if notes.withholds_any() {
        return Ok(());
    }
    f.out.push_str(
        "\nPareto status (advisory and weight-free; unknown, stale, subjective or missing cells \
         never prove dominance):\n\n",
    );
    let Ok(statuses) = decision_ops::pareto(state, case, question) else {
        f.out.push_str("- Not computable at this snapshot.\n");
        return Ok(());
    };
    for entry in statuses.iter().take(MAX_ROWS) {
        let words = pareto_words(entry.status);
        let _ = writeln!(f.out, "- {}: {words}", notes.link(entry.option_ref.id)?);
    }
    if statuses.len() > MAX_ROWS {
        let _ = writeln!(f.out, "- and {} more options", statuses.len() - MAX_ROWS);
    }
    Ok(())
}
