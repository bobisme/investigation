//! `decision-record`: an ADR-style record of an evaluative decision. Pure.
//!
//! One Markdown file, `decision-record.md`, in a fixed order. Case level:
//! basis, context (objective and questions), one section per decision that
//! has not been superseded, then the supersession history and the limits.
//! Each decision section follows the ADR order:
//!
//! 1. status: the effective disposition beside the recorded lifecycle,
//!    freshness, review and the supersession links;
//! 2. options considered, with the chosen one marked and ruled-out options
//!    kept with their reasons;
//! 3. hard constraints and their dispositions, then all criteria, soft
//!    ones labelled as preferences;
//! 4. the option-by-criterion comparison matrix, from the same consequences
//!    table the vault matrix uses (`records::matrix`), with no score or
//!    rank: unknown, missing and stale cells say so and are never a pass;
//! 5. the choice, trade-offs, consequences, evidence and assumptions, the
//!    expected outcome and when to check it, the reopen trigger and the
//!    reversal conditions;
//! 6. what the decision was accepted over, and the warnings: freshness
//!    reasons (a changed criterion shows here), stale comparisons and open
//!    gaps.
//!
//! Disclosure: every record, title, count, cell and gap is asked of
//! [`ExportInput::includes`] first. A matrix row or column of a withheld
//! option or criterion is omitted without a trace. A cell of a withheld
//! comparison reads as not shown for an audience that approves caveats, and
//! drops its whole option row for one that does not (cascading omission). A
//! shown decision carries the policy's fixed caveats. Effective states come
//! from the full pinned snapshot, so withholding never makes a decision read
//! stronger. A derived title (a decision's or a comparison's) is built here
//! from shown records only, so it never repeats a withheld title.
//!
//! Every matrix row and cell, Pareto status and open gap left out this way
//! is reported as an [`super::Omission`] (bn-1vg8), so the manifest reads
//! incomplete whenever the record shows less than the case holds.

use std::fmt::Write as _;

use investigation_core::{
    command::RevisionRef,
    decision_ops,
    evaluate::{ComparisonOutcome, CriterionKind},
    freshness::Freshness,
    identity::{Id, Kind},
    state::Record,
    weighting::{CellState, ConsequencesTable, TableCell},
};
use serde_json::Value;

use super::{
    ExportInput, ExportRenderer, Omission, OmissionReason, Omitted, caveats_approved,
    dependent_rule, human_files,
};
use crate::{
    Phase, PlanBuilder, PlanPath, RenderError,
    evidence::{MAX_LISTED, prose},
    note::{text_at, wire_word},
    records::{
        criterion::threshold,
        effective_word, lifecycle,
        matrix::{MAX_COLUMNS, MAX_ROWS, cell_details, pareto_words, state_words},
        refs,
    },
    text::{inline_code, inline_text, table_code},
};

/// Output format of this kind.
/// v2: the basis names the export's snapshot, which is the bundle when IDs
/// are remapped; a remapped record gives no command to open the case
/// (W11.13).
/// v3: the human files records rely on, as captured, with their bytes under
/// `human/sha256/` (W11.14).
/// v4 (bn-91gz): every capped list says how many shown items it leaves out
/// (hard conditions, criteria, hard failures, Pareto entries, reversal
/// conditions, open objections, stale comparisons and option notes).
pub const VERSION: &str = "inv.export.decision-record.v4";

/// The record's file name.
pub const FILE: &str = "decision-record.md";

/// Characters of a title in a table header or row label.
const LABEL_CHARS: usize = 60;

pub struct DecisionRecord;

impl ExportRenderer for DecisionRecord {
    fn version(&self) -> &'static str {
        VERSION
    }

    fn plan(&self, input: &ExportInput<'_>, files: &mut PlanBuilder) -> Result<(), RenderError> {
        let human = human_files::plan(input, files)?;
        files.add(
            Phase::Views,
            PlanPath::new(FILE)?,
            report(input, &human),
            None,
        )
    }
}

fn report(input: &ExportInput<'_>, human: &[human_files::Shown]) -> String {
    let case = input.case;
    let case_record = case
        .state()
        .current(case.case_id())
        .filter(|record| input.includes(record));
    let title = case_record
        .and_then(|record| input.title(record))
        .unwrap_or_else(|| "Case".to_owned());
    let snapshot = input.snapshot();
    let mut out = format!("# Decision record: {title}\n\n");
    out.push_str(
        "> Decision record rendered by Investigation from one verified snapshot, with no model \
         call. It records the context, the options, the criteria and what was compared, the \
         choice and the conditions under which to revisit it. It records a decision as it \
         stood at this snapshot; it does not show the choice to be right. \
         export-manifest.json beside this file names the snapshot, the audience policy, \
         whether anything was withheld, and the digest of every file.\n",
    );
    out.push_str("\n## Basis\n\n| Basis | Value |\n|---|---|\n");
    let _ = writeln!(
        out,
        "| Case | {} |",
        inline_code(&snapshot.case_id.to_string())
    );
    if input.remapped() {
        let _ = writeln!(
            out,
            "| Bundle | {} (IDs remapped; the publisher keeps the case snapshot) |",
            inline_code(snapshot.head_hash.as_str())
        );
    } else {
        let _ = writeln!(
            out,
            "| Snapshot | sequence {}, head {} |",
            snapshot.sequence,
            inline_code(snapshot.head_hash.as_str())
        );
    }
    let _ = writeln!(
        out,
        "| Audience | {} |",
        inline_code(input.audience.as_str())
    );
    let _ = writeln!(out, "| Renderer | {} |", inline_code(VERSION));

    context(&mut out, input, case_record);

    let decisions = shown_of_kind(input, Kind::Decision);
    let (live, history): (Vec<&Record>, Vec<&Record>) = decisions
        .into_iter()
        .partition(|record| lifecycle(record) != Some("superseded"));
    if live.is_empty() {
        out.push_str("\n## Decision\n\n- No decision is recorded or included for this audience.\n");
    }
    // A question's options, criteria and matrix are written once, under the
    // first decision about it.
    let mut written: Vec<(Id, usize)> = Vec::new();
    for (index, decision) in live.iter().enumerate() {
        let number = index + 1;
        let question = refs(decision.body.get("question_ref"))
            .first()
            .map(|reference| reference.id);
        let earlier = question.and_then(|id| {
            written
                .iter()
                .find(|(seen, _)| *seen == id)
                .map(|(_, number)| *number)
        });
        if let (Some(id), None) = (question, earlier) {
            written.push((id, number));
        }
        decision_section(&mut out, input, decision, number, earlier);
    }
    if !history.is_empty() {
        out.push_str("\n## Supersession history\n\n");
        for decision in &history {
            let _ = write!(out, "- {}", label(input, decision));
            let _ = write!(
                out,
                ": superseded; recorded {}",
                inline_code(lifecycle(decision).unwrap_or("not recorded"))
            );
            for link in supersession(input, decision) {
                let _ = write!(out, "; {link}");
            }
            out.push('\n');
            caveats(&mut out, input, decision, "  ");
        }
    }

    human_files::section(&mut out, human);

    out.push_str("\n## Limits\n\n");
    out.push_str(
        "- Only records this audience may see are listed. export-manifest.json says whether \
         anything was withheld.\n",
    );
    out.push_str(
        "- Effective states were computed on the whole case at this snapshot before anything \
         was withheld, so withholding a record does not make a decision read stronger.\n",
    );
    let _ = writeln!(out, "- {}", dependent_rule(input.audience));
    out.push_str(
        "- Each matrix cell states what is recorded for one option against one criterion. A \
         matrix computes no overall score or ranking. Pareto status is listed only when every \
         option, criterion and comparison of the matrix is shown.\n",
    );
    out.push_str(
        "- Soft criteria and preference cells state preferences, not measurements. A \
         criterion's weight is a stated preference.\n",
    );
    if input.remapped() {
        out.push_str(
            "- IDs in this record were remapped for this export only. To read the full evidence \
             chain, ask the publisher, who can open the case at the snapshot it was rendered from.\n",
        );
    } else {
        let _ = writeln!(
            out,
            "- To read the full evidence chain, open the case at this snapshot: {}",
            inline_code(&format!(
                "investigation --case {} brief --snapshot {} --head-hash {}",
                snapshot.case_id, snapshot.sequence, snapshot.head_hash
            ))
        );
    }
    out
}

/// Objective, case status and the questions.
fn context(out: &mut String, input: &ExportInput<'_>, case_record: Option<&Record>) {
    out.push_str("\n## Context\n\n");
    match case_record {
        Some(record) => {
            let _ = writeln!(
                out,
                "- Objective: {}",
                text_at(&record.body, "objective")
                    .filter(|text| !text.trim().is_empty())
                    .map_or_else(|| "not recorded".to_owned(), prose)
            );
            let _ = writeln!(
                out,
                "- Case status: {}",
                inline_code(lifecycle(record).unwrap_or("not recorded"))
            );
            caveats(out, input, record, "  ");
        }
        None => out.push_str("- The case record is not included for this audience.\n"),
    }
    let questions = shown_of_kind(input, Kind::Question);
    if questions.is_empty() {
        out.push_str("- Questions: none recorded or included\n");
    }
    for question in questions.iter().take(MAX_LISTED) {
        let _ = writeln!(
            out,
            "- Question {}: status {}",
            label(input, question),
            inline_code(lifecycle(question).unwrap_or("not recorded"))
        );
        caveats(out, input, question, "  ");
    }
    if questions.len() > MAX_LISTED {
        let _ = writeln!(out, "- and {} more questions", questions.len() - MAX_LISTED);
    }
}

/// One decision, in ADR order.
fn decision_section(
    out: &mut String,
    input: &ExportInput<'_>,
    decision: &Record,
    number: usize,
    earlier: Option<usize>,
) {
    let body = &decision.body;
    let _ = write!(
        out,
        "\n## Decision {number}: {}\n\n",
        title_of(input, decision).unwrap_or_else(|| "untitled".to_owned())
    );
    let question = refs(body.get("question_ref"))
        .first()
        .and_then(|reference| shown_ref(input, *reference));

    // Status.
    out.push_str("### Status\n\n");
    let _ = writeln!(
        out,
        "- Decision: {}",
        inline_code(&decision.header.id.to_string())
    );
    let recorded = lifecycle(decision).unwrap_or("recorded");
    let freshness = input
        .notes
        .effective(decision)
        .map(|freshness| freshness.freshness);
    let disposition = effective_word(input.notes, decision);
    // A freshness verdict that asks for review outranks the disposition
    // word, which keeps saying proposed or accepted.
    let needs_review =
        freshness == Some(Freshness::NeedsReview) || disposition.as_deref() == Some("needs_review");
    let effective = match (&disposition, freshness) {
        (Some(word), _) if word == "disputed" => word.clone(),
        (_, Some(fresh)) if fresh != Freshness::Current => fresh.as_str().to_owned(),
        (Some(word), _) => word.clone(),
        (None, _) => recorded.to_owned(),
    };
    let mut line = format!(
        "- Effective status: {} (recorded {}",
        inline_code(&effective),
        inline_code(recorded)
    );
    if let Some(word) = disposition.as_deref().filter(|word| *word != effective) {
        let _ = write!(line, ", disposition {}", inline_code(word));
    }
    let _ = writeln!(out, "{line})");
    if needs_review {
        out.push_str(
            "- This decision needs review: something it rests on changed after it was recorded \
             or accepted. The warnings below say what.\n",
        );
    }
    match refs(body.get("review_ref")).first() {
        Some(reference) => match shown_ref(input, *reference) {
            Some(review) => {
                let _ = writeln!(out, "- Accepting review: {}", label(input, review));
            }
            None => out.push_str("- Accepting review: recorded\n"),
        },
        None => out.push_str("- Accepting review: none; this decision is not accepted\n"),
    }
    for link in supersession(input, decision) {
        let _ = writeln!(out, "- Supersession: {link}");
    }
    if let Some(reason) = text_at(body, "change_reason").filter(|r| !r.trim().is_empty()) {
        let _ = writeln!(out, "- Change reason: {}", prose(reason));
    }
    caveats(out, input, decision, "");

    // Context of this decision.
    out.push_str("\n### Context\n\n");
    match question {
        Some(question) => {
            let _ = writeln!(out, "- Question: {}", label(input, question));
            caveats(out, input, question, "  ");
        }
        None => out.push_str("- Question: not shown\n"),
    }

    if let (Some(question), None) = (question, earlier) {
        options(out, input, decision, question.header.id);
        criteria(out, input, question.header.id);
    }

    // Hard conditions still to verify.
    out.push_str("\n### Hard conditions to verify\n\n");
    let conditions: Vec<&Value> = body["hard_conditions"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    if conditions.is_empty() {
        out.push_str("- None: every hard criterion is treated as settled.\n");
    }
    for condition in conditions.iter().take(MAX_LISTED) {
        let named = refs(condition.get("criterion_ref"))
            .first()
            .and_then(|reference| shown_ref(input, *reference))
            .map_or_else(|| "a hard criterion".to_owned(), |c| label(input, c));
        let _ = writeln!(
            out,
            "- {named}: {}",
            prose(text_at(condition, "reason").unwrap_or_default())
        );
    }
    more(out, "", conditions.len(), "hard conditions");

    match (question, earlier) {
        (Some(question), None) => matrix(out, input, question.header.id),
        (Some(_), Some(first)) => {
            let _ = write!(
                out,
                "\n### Options, criteria and comparison matrix\n\nThese are the same question's \
                 options, criteria and matrix as under Decision {first}.\n"
            );
        }
        (None, _) => {}
    }

    // Choice and consequences.
    out.push_str("\n### Choice\n\n");
    match text_at(body, "choice.kind") {
        Some("select") => match chosen_id(body).and_then(|id| shown(input, id)) {
            Some(option) => {
                let _ = writeln!(out, "- Chosen option: {}", label(input, option));
                caveats(out, input, option, "  ");
            }
            None => out.push_str("- Chosen option: not shown\n"),
        },
        Some("defer") => {
            let _ = writeln!(
                out,
                "- Deferred because: {}",
                prose(text_at(body, "choice.reason").unwrap_or("not recorded"))
            );
        }
        _ => out.push_str("- Choice: not recorded\n"),
    }
    text_list(out, "Trade-offs", body.get("tradeoffs"));
    text_list(out, "Consequences", body.get("consequences"));
    let mut outcome = Vec::new();
    for (label, path) in [
        ("Expected outcome", "expected_outcome"),
        ("Check when", "check_when"),
        ("Reopen trigger", "reopen_trigger"),
    ] {
        outcome.push(format!(
            "- {label}: {}",
            text_at(body, path)
                .filter(|text| !text.trim().is_empty())
                .map_or_else(|| "not recorded".to_owned(), prose)
        ));
    }
    out.push_str("\n### Expected outcome and reopening\n\n");
    for line in &outcome {
        let _ = writeln!(out, "{line}");
    }
    reversals(out, input, body);

    // Evidence and assumptions.
    out.push_str("\n### Evidence and assumptions\n\n");
    ref_list(out, input, "Evidence", body.get("evidence_refs"));
    ref_list(out, input, "Assumptions", body.get("assumption_refs"));
    let comparisons = refs(body.get("comparison_refs"));
    comparison_list(out, input, &comparisons);

    accepted_over(out, input, body);
    warnings(out, input, decision, question.map(|q| q.header.id));
}

/// Every option of the question that is shown, the chosen one marked.
fn options(out: &mut String, input: &ExportInput<'_>, decision: &Record, question: Id) {
    out.push_str("\n### Options considered\n\n");
    let chosen = chosen_id(&decision.body);
    let compared: Vec<Id> = refs(decision.body.get("compared_alternatives"))
        .iter()
        .map(|reference| reference.id)
        .collect();
    let options: Vec<&Record> = shown_of_kind(input, Kind::Option)
        .into_iter()
        .filter(|option| question_of(option) == Some(question))
        .collect();
    if options.is_empty() {
        out.push_str("- No option is recorded or included.\n");
        return;
    }
    for option in options.iter().take(MAX_LISTED) {
        let id = option.header.id;
        let mut line = format!(
            "- {}: {}",
            label(input, option),
            inline_code(lifecycle(option).unwrap_or("not recorded"))
        );
        if chosen == Some(id) {
            line.push_str("; chosen");
        } else if compared.contains(&id) {
            line.push_str("; compared");
        }
        if let Some(reason) =
            text_at(&option.body, "disposition_reason").filter(|reason| !reason.trim().is_empty())
        {
            let _ = write!(line, "; reason: {}", prose(reason));
        }
        let _ = writeln!(out, "{line}");
        text_list_indented(out, "Trade-off notes", option.body.get("tradeoff_notes"));
        caveats(out, input, option, "  ");
    }
    if options.len() > MAX_LISTED {
        let _ = writeln!(out, "- and {} more options", options.len() - MAX_LISTED);
    }
}

/// Hard constraints first with their thresholds, then every other criterion;
/// soft criteria are labelled as preferences.
fn criteria(out: &mut String, input: &ExportInput<'_>, question: Id) {
    let all: Vec<&Record> = shown_of_kind(input, Kind::Criterion)
        .into_iter()
        .filter(|criterion| question_of(criterion) == Some(question))
        .collect();
    let (hard, soft): (Vec<&Record>, Vec<&Record>) = all
        .into_iter()
        .partition(|criterion| text_at(&criterion.body, "kind") == Some("hard"));
    out.push_str("\n### Hard constraints\n\n");
    if hard.is_empty() {
        out.push_str("- No hard criterion is recorded or included.\n");
    }
    for criterion in hard.iter().take(MAX_LISTED) {
        let _ = writeln!(
            out,
            "- {}{}",
            label(input, criterion),
            criterion_facts(criterion)
        );
        out.push_str(
            "  - A hard criterion gates eligibility: an option that fails it is ineligible, and \
             an unknown result is not a pass.\n",
        );
        caveats(out, input, criterion, "  ");
    }
    more(out, "", hard.len(), "hard criteria");
    out.push_str("\n### Criteria\n\n");
    if soft.is_empty() {
        out.push_str("- No soft criterion is recorded or included.\n");
    }
    for criterion in soft.iter().take(MAX_LISTED) {
        let _ = writeln!(
            out,
            "- {} (a preference, not a measurement){}",
            label(input, criterion),
            criterion_facts(criterion)
        );
        let body = &criterion.body;
        if let Some(value) = text_at(body, "weight.value") {
            let _ = writeln!(
                out,
                "  - Weight: {}, a stated preference{}",
                inline_code(value),
                text_at(body, "weight.stated_by").map_or_else(String::new, |who| format!(
                    " stated by {}",
                    inline_code(who)
                ))
            );
        }
        caveats(out, input, criterion, "  ");
    }
    more(out, "", soft.len(), "criteria");
}

/// `: lifecycle, threshold` for a criterion line.
fn criterion_facts(criterion: &Record) -> String {
    let body = &criterion.body;
    let mut facts = vec![format!(
        "lifecycle {}",
        inline_code(lifecycle(criterion).unwrap_or("not recorded"))
    )];
    if let Some(unit) = text_at(body, "scale.unit") {
        facts.push(format!("unit {}", inline_code(unit)));
    }
    if let Some(threshold) = body.get("threshold").and_then(threshold) {
        facts.push(format!("threshold {threshold}"));
    }
    format!(": {}", facts.join(", "))
}

/// The option-by-criterion matrix, then hard failures and Pareto status.
fn matrix(out: &mut String, input: &ExportInput<'_>, question: Id) {
    out.push_str("\n### Comparison matrix\n\n");
    let Some(table) = input.notes.consequences_table(question) else {
        out.push_str("- No eligible option or active criterion to compare.\n");
        return;
    };
    if table.rows.is_empty() || table.criteria.is_empty() {
        out.push_str("- No eligible option or active criterion to compare.\n");
        return;
    }
    out.push_str(
        "Each cell is what is recorded for one option against one criterion at this snapshot. \
         This matrix computes no overall score and no ranking; an unknown or missing cell is \
         not a pass.\n\n",
    );
    let state = input.case.state();
    let columns: Vec<usize> = (0..table.criteria.len())
        .filter(|index| shown_ref(input, table.criteria[*index].criterion_ref).is_some())
        .collect();
    let shown_columns = columns.len();
    let columns: Vec<usize> = columns.into_iter().take(MAX_COLUMNS).collect();
    let approved = caveats_approved(input.audience);
    // A row whose option is withheld is omitted. A row with a cell whose
    // comparison is withheld is kept, its cell not shown, for an audience
    // that approves caveats, and omitted for one that does not.
    let mut rows = Vec::new();
    let mut complete = shown_all(input, table);
    for row in &table.rows {
        if shown_ref(input, row.option_ref).is_none() {
            continue;
        }
        let hidden: Vec<usize> = columns
            .iter()
            .copied()
            .filter(|column| !cell_shown(input, &row.cells[*column]))
            .collect();
        if !hidden.is_empty() {
            complete = false;
            let option = row.option_ref.id;
            if !approved {
                let rests = hidden
                    .iter()
                    .flat_map(|column| comparisons(&row.cells[*column]));
                input.omit(
                    Omission::new(Omitted::MatrixRow, reason(input, rests))
                        .of(option)
                        .at(question.to_string()),
                );
                continue;
            }
            for column in &hidden {
                input.omit(
                    Omission::new(
                        Omitted::MatrixCell,
                        reason(input, comparisons(&row.cells[*column])),
                    )
                    .of(option)
                    .at(table.criteria[*column].criterion_ref.id.to_string()),
                );
            }
        }
        rows.push(row);
    }
    let total_rows = rows.len();
    rows.truncate(MAX_ROWS);

    out.push_str("| Option |");
    for column in &columns {
        let criterion = &table.criteria[*column];
        let kind = match criterion.kind {
            CriterionKind::Hard => "hard",
            CriterionKind::Soft => "soft preference",
        };
        let name = shown_ref(input, criterion.criterion_ref)
            .map_or_else(String::new, |record| table_label(input, record));
        let _ = write!(out, " {name} ({kind}) |");
    }
    out.push_str("\n|---|");
    for _ in &columns {
        out.push_str("---|");
    }
    out.push('\n');
    for row in &rows {
        let name = shown_ref(input, row.option_ref)
            .map_or_else(String::new, |record| table_label(input, record));
        let _ = write!(out, "| {name} |");
        for column in &columns {
            let _ = write!(out, " {} |", cell_text(input, &row.cells[*column]));
        }
        out.push('\n');
    }
    if total_rows > rows.len() || shown_columns > columns.len() {
        let _ = writeln!(
            out,
            "\nShowing {} of {} options and {} of {} criteria; the rest are listed above and \
             are not part of this table.",
            rows.len(),
            total_rows,
            columns.len(),
            shown_columns
        );
    }

    // The caveats of every shown comparison, beside the cell it stands in.
    let mut caveated_cells = false;
    for row in &rows {
        for column in &columns {
            for reference in &row.cells[*column].comparison_refs {
                if let Some(comparison) = shown_ref(input, *reference)
                    && !input.caveats(comparison).is_empty()
                {
                    if !caveated_cells {
                        out.push_str("\nCaveats on cells:\n\n");
                        caveated_cells = true;
                    }
                    let _ = writeln!(out, "- {}", inline_code(&reference.id.to_string()));
                    caveats(out, input, comparison, "  ");
                }
            }
        }
    }

    // Hard failures, from shown cells only.
    out.push_str("\nHard failures:\n\n");
    let mut any = false;
    for row in &rows {
        let mut failed = Vec::new();
        for column in &columns {
            let cell = &row.cells[*column];
            let criterion = &table.criteria[*column];
            if criterion.kind == CriterionKind::Hard
                && cell.outcome == Some(ComparisonOutcome::Fails)
                && cell_shown(input, cell)
                && let Some(record) = shown_ref(input, criterion.criterion_ref)
            {
                let name = label(input, record);
                failed.push(if cell.state == CellState::Measured {
                    name
                } else {
                    format!("{name} (recorded as failing, but its evidence is not verified)")
                });
            }
        }
        if let Some(option) = shown_ref(input, row.option_ref)
            && !failed.is_empty()
        {
            any = true;
            let total = failed.len();
            failed.truncate(MAX_LISTED);
            if total > MAX_LISTED {
                failed.push(format!("and {} more", total - MAX_LISTED));
            }
            let _ = writeln!(
                out,
                "- {} fails the hard criterion: {}",
                label(input, option),
                failed.join(", ")
            );
        }
    }
    if !any {
        out.push_str("- No shown option has a recorded hard failure.\n");
    }

    // Pareto status computed over every option and cell, so it is listed
    // only when all of them are shown.
    if !complete {
        input.omit(
            Omission::new(Omitted::ParetoStatus, reason(input, table_records(table))).of(question),
        );
    }
    if complete && let Ok(statuses) = decision_ops::pareto(state, input.case.case_id(), question) {
        out.push_str(
            "\nPareto status (advisory and weight-free; unknown, stale, subjective or missing \
             cells never prove dominance):\n\n",
        );
        for entry in statuses.iter().take(MAX_ROWS) {
            if let Some(option) = shown_ref(input, entry.option_ref) {
                let _ = writeln!(
                    out,
                    "- {}: {}",
                    label(input, option),
                    pareto_words(entry.status)
                );
            }
        }
        // Listed only when every option is shown, so the count names
        // nothing withheld.
        if statuses.len() > MAX_ROWS {
            let _ = writeln!(out, "- and {} more options", statuses.len() - MAX_ROWS);
        }
    }
}

/// Every option, criterion and comparison of the table is shown.
fn shown_all(input: &ExportInput<'_>, table: &ConsequencesTable) -> bool {
    table
        .criteria
        .iter()
        .all(|criterion| shown_ref(input, criterion.criterion_ref).is_some())
        && table.rows.iter().all(|row| {
            shown_ref(input, row.option_ref).is_some()
                && row.cells.iter().all(|cell| cell_shown(input, cell))
        })
}

/// The comparisons behind a cell.
fn comparisons(cell: &TableCell) -> impl Iterator<Item = Id> + '_ {
    cell.comparison_refs.iter().map(|reference| reference.id)
}

/// Every option, criterion and comparison of a table.
fn table_records(table: &ConsequencesTable) -> impl Iterator<Item = Id> + '_ {
    table
        .criteria
        .iter()
        .map(|criterion| criterion.criterion_ref.id)
        .chain(table.rows.iter().flat_map(|row| {
            std::iter::once(row.option_ref.id).chain(row.cells.iter().flat_map(comparisons))
        }))
}

/// Why content resting on `ids` is left out; withheld when no single record
/// can be named (a channel the audience may not see).
fn reason(input: &ExportInput<'_>, ids: impl IntoIterator<Item = Id>) -> OmissionReason {
    input
        .rests_on(ids)
        .unwrap_or(OmissionReason::RestsOnWithheld)
}

/// Whether every comparison behind a cell is shown.
fn cell_shown(input: &ExportInput<'_>, cell: &TableCell) -> bool {
    cell.comparison_refs
        .iter()
        .all(|reference| shown_ref(input, *reference).is_some())
}

/// One cell: its state in words, the details recorded and the comparisons.
fn cell_text(input: &ExportInput<'_>, cell: &TableCell) -> String {
    if !cell_shown(input, cell) {
        return "not shown".to_owned();
    }
    let mut text = state_words(cell.state).to_owned();
    let evidence = cell
        .evidence_refs
        .iter()
        .filter(|reference| shown_ref(input, **reference).is_some())
        .count();
    let details = cell_details(cell, evidence);
    if !details.is_empty() {
        text.push_str(": ");
        text.push_str(&details.join(", "));
    }
    let ids: Vec<String> = cell
        .comparison_refs
        .iter()
        .take(3)
        .map(|reference| table_code(&reference.id.to_string()))
        .collect();
    if !ids.is_empty() {
        let _ = write!(text, " ({})", ids.join(", "));
    }
    text
}

/// A short, escaped name for a row or column header.
fn table_label(input: &ExportInput<'_>, record: &Record) -> String {
    title_of(input, record).map_or_else(
        || table_code(&record.header.id.to_string()),
        |title| inline_text(&title, LABEL_CHARS),
    )
}

/// Reversal conditions, naming only the records that are shown.
fn reversals(out: &mut String, input: &ExportInput<'_>, body: &Value) {
    let reversals: Vec<&Value> = body["reversal_conditions"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    if reversals.is_empty() {
        out.push_str("- Reversal conditions: none recorded\n");
        return;
    }
    out.push_str("- Reversal conditions:\n");
    for reversal in reversals.iter().take(MAX_LISTED) {
        let named: Vec<String> = [
            "assumption_ref",
            "criterion_ref",
            "comparison_ref",
            "option_ref",
        ]
        .iter()
        .filter_map(|key| refs(reversal.get(*key)).first().copied())
        .filter_map(|reference| shown_ref(input, reference))
        .map(|record| inline_code(&record.header.id.to_string()))
        .collect();
        let _ = writeln!(
            out,
            "  - {}{}",
            prose(text_at(reversal, "condition").unwrap_or_default()),
            if named.is_empty() {
                String::new()
            } else {
                format!(" (names {})", named.join(", "))
            }
        );
    }
    more(out, "  ", reversals.len(), "reversal conditions");
}

/// A bulleted list of shown records cited at `value`.
fn ref_list(out: &mut String, input: &ExportInput<'_>, name: &str, value: Option<&Value>) {
    let shown: Vec<&Record> = refs(value)
        .into_iter()
        .filter_map(|reference| shown_ref(input, reference))
        .collect();
    if shown.is_empty() {
        let _ = writeln!(out, "- {name}: none shown");
        return;
    }
    let _ = writeln!(out, "- {name}:");
    for record in shown.iter().take(MAX_LISTED) {
        let _ = writeln!(out, "  - {}", label(input, record));
        caveats(out, input, record, "    ");
    }
    if shown.len() > MAX_LISTED {
        let _ = writeln!(out, "  - and {} more", shown.len() - MAX_LISTED);
    }
}

/// The comparisons this decision read, each with its effective state.
fn comparison_list(out: &mut String, input: &ExportInput<'_>, comparisons: &[RevisionRef]) {
    let shown: Vec<&Record> = comparisons
        .iter()
        .filter_map(|reference| shown_ref(input, *reference))
        .collect();
    if shown.is_empty() {
        out.push_str("- Comparisons read: none shown\n");
        return;
    }
    out.push_str("- Comparisons read:\n");
    for comparison in shown.iter().take(MAX_LISTED) {
        let _ = writeln!(
            out,
            "  - {}: effective {}",
            label(input, comparison),
            inline_code(&effective_word(input.notes, comparison).unwrap_or_default())
        );
        caveats(out, input, comparison, "    ");
    }
    if shown.len() > MAX_LISTED {
        let _ = writeln!(out, "  - and {} more", shown.len() - MAX_LISTED);
    }
}

/// The challenges recorded at acceptance, kept visible after the fact.
fn accepted_over(out: &mut String, input: &ExportInput<'_>, body: &Value) {
    let Some(challenges) = body
        .get("acceptance_challenges")
        .filter(|value| !value.is_null())
    else {
        return;
    };
    out.push_str("\n### Accepted over\n\n");
    let _ = writeln!(
        out,
        "Recorded at acceptance under the {} acknowledgment policy; never recomputed.\n",
        inline_code(text_at(challenges, "policy").unwrap_or("unknown"))
    );
    // Only shown objections are listed or counted.
    let objections: Vec<(&Value, &Record)> = challenges["open_objections"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|objection| {
            refs(objection.get("objection_ref"))
                .first()
                .and_then(|reference| shown_ref(input, *reference))
                .map(|record| (objection, record))
        })
        .collect();
    for (objection, record) in objections.iter().take(MAX_LISTED) {
        let (objection, record) = (*objection, *record);
        let mut line = format!(
            "- Open {} objection {}",
            inline_code(text_at(objection, "severity").unwrap_or("unknown")),
            label(input, record)
        );
        let _ = write!(
            line,
            ", now {}",
            inline_code(text_at(&record.body, "status").unwrap_or("unknown"))
        );
        match text_at(objection, "acknowledgment") {
            Some(text) => {
                let _ = write!(line, "; acknowledged: {}", prose(text));
            }
            None => line.push_str("; not acknowledged"),
        }
        let _ = writeln!(out, "{line}");
        caveats(out, input, record, "  ");
    }
    if objections.is_empty() {
        out.push_str("- Open objections shown: none\n");
    }
    more(out, "", objections.len(), "open objections");
    ref_list(
        out,
        input,
        "Uncertain comparisons (unknown outcome or unverified evidence)",
        challenges.get("uncertain_comparisons"),
    );
    ref_list(
        out,
        input,
        "Hard criteria accepted as conditions, not as satisfied",
        challenges.get("conditional_hard_criteria"),
    );
}

/// Freshness reasons, stale comparisons and open gaps, from shown records.
fn warnings(out: &mut String, input: &ExportInput<'_>, decision: &Record, question: Option<Id>) {
    out.push_str("\n### Warnings\n\n");
    let mut any = false;
    if let Some(freshness) = input.notes.effective(decision) {
        for reason in &freshness.reasons {
            any = true;
            let mut line = format!(
                "- {} because of {}",
                inline_code(reason.freshness.as_str()),
                inline_code(&wire_word(&reason.trigger))
            );
            if let Some(cause) = shown_ref(input, reason.cause) {
                let kind = if cause.header.id.kind() == Kind::Criterion {
                    "criterion"
                } else {
                    "record"
                };
                let _ = write!(
                    line,
                    " at {kind} {} revision {}",
                    label(input, cause),
                    reason.cause.revision
                );
            }
            if let Some(step) = reason.path.first() {
                let _ = write!(
                    line,
                    ", reached through {} ({} step{})",
                    inline_code(&step.field),
                    reason.path.len(),
                    if reason.path.len() == 1 { "" } else { "s" }
                );
            }
            let _ = writeln!(out, "{line}");
        }
    }
    // Comparisons of this question that are out of date.
    if let Some(table) = question.and_then(|question| input.notes.consequences_table(question)) {
        let mut stale: Vec<Id> = Vec::new();
        for row in &table.rows {
            for cell in &row.cells {
                if cell.state == CellState::NeedsReview && cell_shown(input, cell) {
                    stale.extend(cell.comparison_refs.iter().map(|reference| reference.id));
                }
            }
        }
        stale.sort();
        stale.dedup();
        // Only shown comparisons are listed or counted.
        let stale: Vec<&Record> = stale.iter().filter_map(|id| shown(input, *id)).collect();
        for record in stale.iter().take(MAX_LISTED) {
            any = true;
            let _ = writeln!(
                out,
                "- Stale comparison: {} is out of date",
                label(input, record)
            );
        }
        more(out, "", stale.len(), "stale comparisons");
    }
    // Open gaps naming this decision or its question, computed only from
    // records this audience may see.
    let mut seen: Vec<&str> = Vec::new();
    for subject in std::iter::once(decision.header.id).chain(question) {
        for gap in input.notes.open_gaps(subject).into_iter().flatten() {
            if seen.contains(&gap.gap_id.as_str()) {
                continue;
            }
            let read = gap
                .subject_refs
                .iter()
                .chain(&gap.read_set)
                .chain(gap.prerequisites.iter().flat_map(|p| &p.subject_refs))
                .map(|reference| reference.id);
            if !input.admits(read, |reason| {
                Omission::new(Omitted::Gap, reason)
                    .of(decision.header.id)
                    .at(gap.gap_id.clone())
            }) {
                continue;
            }
            seen.push(gap.gap_id.as_str());
            any = true;
            let _ = writeln!(
                out,
                "- Open gap {}: {} Effect on the decision: {}",
                inline_code(&gap.rule_id),
                prose(&gap.reason),
                prose(&gap.decision_effect)
            );
        }
    }
    if !any {
        out.push_str(
            "- None shown: no freshness reason, stale comparison or open gap is recorded for \
             this decision from the records shown.\n",
        );
    }
}

/// Supersession links to shown records: `supersedes X`, `superseded by Y`.
fn supersession(input: &ExportInput<'_>, record: &Record) -> Vec<String> {
    let mut found = Vec::new();
    for (word, field) in [
        ("supersedes", "supersedes"),
        ("superseded by", "superseded_by"),
    ] {
        if let Some(other) = refs(record.body.get(field))
            .first()
            .and_then(|reference| shown_ref(input, *reference))
        {
            found.push(format!("{word} {}", label(input, other)));
        }
    }
    found
}

/// The policy's fixed caveats of a shown record, one line each.
fn caveats(out: &mut String, input: &ExportInput<'_>, record: &Record, indent: &str) {
    for caveat in input.caveats(record) {
        let _ = writeln!(out, "{indent}- Caveat: {}", caveat.text());
    }
}

/// The current record of `id` when the export includes it.
fn shown<'a>(input: &ExportInput<'a>, id: Id) -> Option<&'a Record> {
    input
        .case
        .state()
        .current(id)
        .filter(|record| input.includes(record))
}

fn shown_ref<'a>(input: &ExportInput<'a>, reference: RevisionRef) -> Option<&'a Record> {
    shown(input, reference.id)
}

/// Included records of one kind, in ID order.
fn shown_of_kind<'a>(input: &'a ExportInput<'_>, kind: Kind) -> Vec<&'a Record> {
    input
        .case
        .state()
        .records_of_kind(kind)
        .filter(|record| input.includes(record))
        .collect()
}

fn question_of(record: &Record) -> Option<Id> {
    refs(record.body.get("question_ref"))
        .first()
        .map(|reference| reference.id)
}

fn chosen_id(body: &Value) -> Option<Id> {
    let reference: RevisionRef =
        serde_json::from_value(body["choice"]["option_ref"].clone()).ok()?;
    Some(reference.id)
}

/// `` `id` title ``, or the ID alone when no title may be shown.
fn label(input: &ExportInput<'_>, record: &Record) -> String {
    let id = inline_code(&record.header.id.to_string());
    match title_of(input, record) {
        Some(title) => format!("{id} {title}"),
        None => id,
    }
}

/// A shown record's title. A decision's and a comparison's titles are
/// derived from other records, so they are built here from shown records
/// only; every other title is the record's own.
fn title_of(input: &ExportInput<'_>, record: &Record) -> Option<String> {
    if !input.includes(record) {
        return None;
    }
    let part = |path: &str| -> Option<String> {
        let id: Id = text_at(&record.body, path)?.parse().ok()?;
        input.title(shown(input, id)?)
    };
    match record.header.id.kind() {
        Kind::Decision => Some(format!("Chosen option: {}", part("choice.option_ref.id")?)),
        Kind::Comparison => Some(format!(
            "{} against {}",
            part("option_ref.id")?,
            part("criterion_ref.id")?
        )),
        _ => input.title(record),
    }
}

/// `- name:` with one escaped item per line, or `- name: none recorded`.
fn text_list(out: &mut String, name: &str, value: Option<&Value>) {
    let items = strings(value);
    if items.is_empty() {
        let _ = writeln!(out, "- {name}: none recorded");
        return;
    }
    let _ = writeln!(out, "- {name}:");
    for item in items.iter().take(MAX_LISTED) {
        let _ = writeln!(out, "  - {}", prose(item));
    }
    if items.len() > MAX_LISTED {
        let _ = writeln!(out, "  - and {} more", items.len() - MAX_LISTED);
    }
}

/// An option's notes under its own line; nothing when there are none.
fn text_list_indented(out: &mut String, name: &str, value: Option<&Value>) {
    let items = strings(value);
    if items.is_empty() {
        return;
    }
    let _ = writeln!(out, "  - {name}:");
    for item in items.iter().take(MAX_LISTED) {
        let _ = writeln!(out, "    - {}", prose(item));
    }
    more(out, "    ", items.len(), "");
}

/// `- and N more <what>` under a list capped at [`MAX_LISTED`], so a
/// reader never takes a capped list for the whole one. Callers count only
/// what the audience may see.
fn more(out: &mut String, indent: &str, total: usize, what: &str) {
    if total > MAX_LISTED {
        let what = if what.is_empty() {
            String::new()
        } else {
            format!(" {what}")
        };
        let _ = writeln!(out, "{indent}- and {} more{what}", total - MAX_LISTED);
    }
}

fn strings(value: Option<&Value>) -> Vec<&str> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}
