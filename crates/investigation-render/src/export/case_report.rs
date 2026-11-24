//! `case-report`: the §15.4 report of a general inquiry. Pure.
//!
//! One Markdown file, `case-report.md`, in a fixed section order:
//!
//! 1. Basis: kind, case, snapshot, audience, renderer.
//! 2. Status and outcome: lifecycle, closure outcome, closure reason and
//!    reopen condition. An inconclusive case says so in words.
//! 3. Objective: objective, decision relevance, scope, mode and questions.
//! 4. Methods: inquiries (purpose, method, controls, limits) and runs.
//! 5. Original observations: value, citation by source and selector with
//!    quote state, a bounded inert excerpt, validity and limits.
//! 6. Alternatives: hypotheses with their assessments, and options.
//! 7. Supported conclusions: every element §15.3 names (significance,
//!    applicability, evidence selectors, alternatives addressed,
//!    uncertainty, dissent and review, what would invalidate it).
//! 8. Decisions, marked as choices rather than empirical conclusions.
//! 9. Limits. 10. Open issues. 11. Methodological retrospective: exact
//!    references and bounded highlights when the account is substantive.
//!
//! Every record is listed only when the export includes it, with the
//! policy's caveats under each line that shows its standing. Effective
//! dispositions come from the freshness engine and the objection policy on
//! the full pinned snapshot, so a withheld objection still leaves its
//! subject `disputed` and a stale review basis leaves a recorded
//! `validated` beside `needs_review`. Negative and inconclusive results are
//! listed like any other, a superseded record names its successor, and
//! stale or withdrawn inputs are annotated where they are cited. The shared
//! blocks are in `report_blocks`.

use investigation_core::{identity::Kind, state::Record};

use super::{
    ExportInput, ExportRenderer, human_files,
    report_blocks::{MAX_ITEMS, MAX_LISTED, Report},
};
use crate::{
    Phase, PlanBuilder, PlanPath, RenderError, evidence::prose, note::text_at, records::wire,
    text::inline_code,
};

/// Output format of this kind.
/// v2: the caveats of `inv.export-policy.v2` under each caveated record, and
/// the dependent-record rule in the limits (W11.02).
/// v3: the basis names the export's snapshot, which is the bundle when IDs
/// are remapped; a remapped report gives no command to open the case
/// (W11.13).
/// v4: the human files records rely on, as captured, with their bytes under
/// `human/sha256/` (W11.14).
/// v5: the full §15.4 content (W11.04): status and outcome with the reopen
/// condition, methods, original observations with selectors and bounded
/// excerpts, alternatives, the §15.3 conclusion elements, decisions, open
/// issues and retrospective highlights; in-place annotation of stale and
/// withdrawn inputs. A remapped report shows no journal sequence or head of
/// a retrospective either. v6 (bn-91gz): retrospective highlights leave
/// out, and report, a step or lesson citing a record not shown.
pub const VERSION: &str = "inv.export.case-report.v6";

/// The report's file name.
pub const FILE: &str = "case-report.md";

pub struct CaseReport;

impl ExportRenderer for CaseReport {
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
    let mut report = Report::new(input);
    let title = report
        .case_record()
        .and_then(|record| input.title(record))
        .unwrap_or_else(|| "Case report".to_owned());
    report.line(&format!("# {title}\n"));
    report.line(
        "> Case report rendered by Investigation from one verified snapshot, with no model \
         call. It lists what the case records; it does not replace the primary evidence. \
         export-manifest.json beside this file names the snapshot, the audience policy, \
         whether anything was withheld, and the digest of every file.",
    );
    report.basis("case-report", VERSION);
    report.outcome();

    report.heading("Objective");
    report.objective();
    report.questions();

    methods(&mut report);
    observations(&mut report);
    alternatives(&mut report);
    conclusions(&mut report);
    decisions(&mut report);
    human_files::section(report.buffer(), human);

    report.limits();
    report.open_issues();
    report.retrospective();
    report.finish()
}

fn methods(report: &mut Report<'_, '_>) {
    report.heading("Methods");
    let inquiries = report.inquiries();
    let runs = report.records(&[Kind::Run]);
    if inquiries == 0 && runs.is_empty() {
        report.line("- None listed.");
        return;
    }
    if runs.is_empty() {
        return;
    }
    report.line("- Runs:");
    for run in runs.iter().take(MAX_LISTED) {
        let body = &run.body;
        let word = |path: &str| inline_code(&wire(text_at(body, path).unwrap_or("unknown")));
        let detail = format!(
            "execution {}, evidence completeness {}, evidence validity {}; {}",
            word("execution_status"),
            word("evidence_completeness"),
            word("evidence_validity"),
            report.standing(run)
        );
        report.item("  ", run, &detail);
    }
    if runs.len() > MAX_LISTED {
        report.line(&format!(
            "  - \u{2026} and {} more",
            runs.len() - MAX_LISTED
        ));
    }
}

fn observations(report: &mut Report<'_, '_>) {
    report.heading("Original observations");
    report.line(
        "Each observation is cited by its retained artifact and selector, with the state of its \
         quote. A quote shows what the cited bytes say, not that the statement is true.\n",
    );
    let observations = report.records(&[Kind::Observation]);
    if observations.is_empty() {
        report.line("- None listed.");
        return;
    }
    for observation in observations.iter().take(MAX_LISTED) {
        report.observation("", observation);
    }
    if observations.len() > MAX_LISTED {
        report.line(&format!(
            "- \u{2026} and {} more",
            observations.len() - MAX_LISTED
        ));
    }
}

fn alternatives(report: &mut Report<'_, '_>) {
    report.heading("Alternatives");
    let hypotheses = report.records(&[Kind::Hypothesis]);
    let options = report.records(&[Kind::Option]);
    if hypotheses.is_empty() && options.is_empty() {
        report.line("- None listed.");
        return;
    }
    let assessments = report.records(&[Kind::Assessment]);
    for hypothesis in hypotheses.iter().take(MAX_LISTED) {
        let detail = format!("hypothesis; {}", report.standing(hypothesis));
        report.item("", hypothesis, &detail);
        let body = &hypothesis.body;
        if let Some(applicability) = text_at(body, "applicability") {
            report.line(&format!("  - Applicability: {}", prose(applicability)));
        }
        for falsifier in body["possible_falsifiers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str())
            .take(MAX_ITEMS)
        {
            report.line(&format!("  - Possible falsifier: {}", prose(falsifier)));
        }
        let id = hypothesis.header.id.to_string();
        let judged: Vec<&Record> = assessments
            .iter()
            .copied()
            .filter(|assessment| text_at(&assessment.body, "subject_ref.id") == Some(&id))
            .collect();
        for assessment in judged.iter().take(MAX_ITEMS) {
            let detail = format!(
                "assessment, relation {}; {}: {}",
                inline_code(&wire(
                    text_at(&assessment.body, "relation").unwrap_or("unknown")
                )),
                report.standing(assessment),
                text_at(&assessment.body, "rationale")
                    .map_or_else(|| "rationale not recorded".to_owned(), prose)
            );
            report.item("  ", assessment, &detail);
        }
    }
    for option in options.iter().take(MAX_LISTED) {
        let detail = format!("option; {}", report.standing(option));
        report.item("", option, &detail);
        if let Some(reason) = text_at(&option.body, "reason") {
            report.line(&format!("  - Disposition reason: {}", prose(reason)));
        }
    }
}

fn conclusions(report: &mut Report<'_, '_>) {
    report.heading("Supported conclusions");
    report.line(
        "Each conclusion shows its effective disposition beside its recorded status. Negative \
         and inconclusive results are listed like any other; a superseded conclusion names its \
         successor.\n",
    );
    let conclusions = report.records(&[Kind::Conclusion]);
    if conclusions.is_empty() {
        report.line("- None listed.");
        return;
    }
    for conclusion in conclusions.iter().take(MAX_LISTED) {
        report.conclusion(conclusion);
    }
    if conclusions.len() > MAX_LISTED {
        report.line(&format!(
            "- \u{2026} and {} more",
            conclusions.len() - MAX_LISTED
        ));
    }
}

fn decisions(report: &mut Report<'_, '_>) {
    let decisions = report.records(&[Kind::Decision]);
    report.heading("Decisions");
    if decisions.is_empty() {
        report.line("- None listed.");
        return;
    }
    report.line(
        "A decision records a choice. Accepting it never makes it a validated empirical \
         conclusion, and a superseded decision is kept and marked, not rewritten.\n",
    );
    for decision in decisions.iter().take(MAX_LISTED) {
        report.decision(decision);
    }
    if decisions.len() > MAX_LISTED {
        report.line(&format!(
            "- \u{2026} and {} more",
            decisions.len() - MAX_LISTED
        ));
    }
}
