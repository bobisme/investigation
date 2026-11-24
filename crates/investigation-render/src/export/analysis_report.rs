//! `analysis-report`: the §15.4 report of a causal or empirical case. Pure.
//!
//! One Markdown file, `analysis-report.md`, in a fixed section order:
//!
//! 1. Basis. 2. Status and outcome, with the reopen condition. 3. Question.
//! 4. Reproduction conditions: inquiries (method, controls, limits), the
//!    recorded run environments and the variables each run changed.
//! 5. Baselines: causal-test baselines and runs that change no variable.
//! 6. Runs and controls: each run with execution status, evidence
//!    completeness and evidence validity shown separately, evidence kind,
//!    typed measurement fields, advisory measurement flags and the
//!    observations it produced; k-of-n completion per repetition series;
//!    measurements not tied to an included run.
//! 7. Uncertainty: runs and measurements that are not complete and valid,
//!    and records with advisory flags.
//! 8. Root-cause or measurement assessment: hypotheses, then each
//!    conclusion with its causal account and every §15.3 element.
//! 9. Limits. 10. Open issues.
//!
//! The inclusion and caveat contract, effective states and in-place
//! annotations are those of `case-report`; the shared blocks are in
//! `report_blocks`.

use std::{collections::BTreeMap, fmt::Write as _};

use investigation_core::{evidence::MeasurementValidity, identity::Kind, state::Record};

use super::{
    ExportInput, ExportRenderer,
    report_blocks::{MAX_ITEMS, MAX_LISTED, Report, validity_word},
};
use crate::{
    Phase, PlanBuilder, PlanPath, RenderError,
    evidence::prose,
    note::text_at,
    records::{refs, wire},
    text::inline_code,
};

/// Output format of this kind.
/// v1: the §15.4 analysis report (W11.04).
pub const VERSION: &str = "inv.export.analysis-report.v1";

/// The report's file name.
pub const FILE: &str = "analysis-report.md";

pub struct AnalysisReport;

impl ExportRenderer for AnalysisReport {
    fn version(&self) -> &'static str {
        VERSION
    }

    fn plan(&self, input: &ExportInput<'_>, files: &mut PlanBuilder) -> Result<(), RenderError> {
        let content = report(input);
        files.add(Phase::Views, PlanPath::new(FILE)?, content, None)
    }
}

fn report(input: &ExportInput<'_>) -> String {
    let mut report = Report::new(input);
    let title = report
        .case_record()
        .and_then(|record| input.title(record))
        .unwrap_or_else(|| "Analysis report".to_owned());
    report.line(&format!("# {title}\n"));
    report.line(
        "> Analysis report rendered by Investigation from one verified snapshot, with no model \
         call. It lists the recorded conditions, runs, measurements and assessments; it does not \
         replace the primary evidence or recompute any statistic. export-manifest.json beside \
         this file names the snapshot, the audience policy, whether anything was withheld, and \
         the digest of every file.",
    );
    report.basis("analysis-report", VERSION);
    report.outcome();

    report.heading("Question");
    report.objective();
    report.questions();

    let runs = report.records(&[Kind::Run]);
    conditions(&mut report, &runs);
    baselines(&mut report, &runs);
    runs_and_controls(&mut report, &runs);
    uncertainty(&mut report, &runs);
    assessment(&mut report);

    report.limits();
    report.open_issues();
    report.finish()
}

fn word(record: &Record, path: &str) -> String {
    wire(text_at(&record.body, path).unwrap_or("unknown"))
}

fn conditions(report: &mut Report<'_, '_>, runs: &[&Record]) {
    report.heading("Reproduction conditions");
    let inquiries = report.inquiries();
    let mut environments: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut variables: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for run in runs {
        let id = inline_code(&run.header.id.to_string());
        if let Some(environment) = text_at(&run.body, "environment") {
            environments
                .entry(prose(environment))
                .or_default()
                .push(id.clone());
        }
        for variable in report.changed_variables(run) {
            variables
                .entry(prose(&variable))
                .or_default()
                .push(id.clone());
        }
    }
    if inquiries == 0 && environments.is_empty() && variables.is_empty() {
        report.line("- None listed.");
        return;
    }
    for (label, map) in [
        ("Run environment", &environments),
        ("Variable changed", &variables),
    ] {
        for (text, ids) in map.iter().take(MAX_LISTED) {
            report.line(&format!("- {label}: {text} (runs {})", bounded(ids)));
        }
        if map.len() > MAX_LISTED {
            report.line(&format!("- \u{2026} and {} more", map.len() - MAX_LISTED));
        }
    }
}

fn bounded(ids: &[String]) -> String {
    let mut text = ids
        .iter()
        .take(MAX_ITEMS)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if ids.len() > MAX_ITEMS {
        let _ = write!(text, " and {} more", ids.len() - MAX_ITEMS);
    }
    text
}

fn baselines(report: &mut Report<'_, '_>, runs: &[&Record]) {
    report.heading("Baselines");
    let mut any = false;
    for conclusion in report.records(&[Kind::Conclusion]) {
        let Some(test) = conclusion
            .body
            .get("causal_test")
            .filter(|test| !test.is_null())
        else {
            continue;
        };
        let Some(reference) = refs(test.get("baseline")).first().copied() else {
            continue;
        };
        let Some(baseline) = report
            .input
            .case
            .state()
            .current(reference.id)
            .filter(|record| report.input.includes(record))
        else {
            continue;
        };
        any = true;
        let detail = format!(
            "baseline of the causal test of {} in {}, revision {}; {}{}",
            inline_code(text_at(test, "variable").unwrap_or("an unnamed variable")),
            inline_code(&conclusion.header.id.to_string()),
            reference.revision,
            report.standing(baseline),
            report.evidence_note(baseline)
        );
        report.item("", baseline, &detail);
    }
    let unchanged: Vec<&Record> = runs
        .iter()
        .copied()
        .filter(|run| report.changed_variables(run).is_empty())
        .collect();
    if !unchanged.is_empty() {
        any = true;
        report.line("- Runs that record no changed variable:");
        for run in unchanged.iter().take(MAX_LISTED) {
            let detail = format!(
                "execution {}, evidence validity {}; {}",
                inline_code(&word(run, "execution_status")),
                inline_code(&word(run, "evidence_validity")),
                report.standing(run)
            );
            report.item("  ", run, &detail);
        }
        if unchanged.len() > MAX_LISTED {
            report.line(&format!(
                "  - \u{2026} and {} more",
                unchanged.len() - MAX_LISTED
            ));
        }
    }
    if !any {
        report.line("- None listed.");
    }
}

fn runs_and_controls(report: &mut Report<'_, '_>, runs: &[&Record]) {
    report.heading("Runs and controls");
    report.line(
        "Execution status, evidence completeness and evidence validity are separate: a run can \
         finish and still produce incomplete or invalid evidence. Measurement fields are shown \
         as recorded; nothing is derived from another field.\n",
    );
    if runs.is_empty() {
        report.line("- No run listed.");
    }
    for run in runs.iter().take(MAX_LISTED) {
        report.run("", run);
    }
    if runs.len() > MAX_LISTED {
        report.line(&format!("- \u{2026} and {} more", runs.len() - MAX_LISTED));
    }

    report.subheading("Repetition");
    report.repetitions(runs);

    report.subheading("Measurements not tied to a listed run");
    let state = report.input.case.state();
    let loose: Vec<&Record> = report
        .records(&[Kind::Observation])
        .into_iter()
        .filter(|observation| text_at(&observation.body, "value.kind") == Some("measurement"))
        .filter(|observation| {
            refs(observation.body.get("run_ref"))
                .first()
                .is_none_or(|run| {
                    state
                        .current(run.id)
                        .is_none_or(|run| !report.input.includes(run))
                })
        })
        .collect();
    if loose.is_empty() {
        report.line("- None listed.");
    }
    for observation in loose.iter().take(MAX_LISTED) {
        report.observation("", observation);
        report.measurement("  ", observation);
        report.flags("  ", observation);
    }
    if loose.len() > MAX_LISTED {
        report.line(&format!("- \u{2026} and {} more", loose.len() - MAX_LISTED));
    }
}

fn uncertainty(report: &mut Report<'_, '_>, runs: &[&Record]) {
    report.heading("Uncertainty");
    let mut any = false;
    let weak_runs: Vec<&Record> = runs
        .iter()
        .copied()
        .filter(|run| {
            word(run, "execution_status") != "completed"
                || word(run, "evidence_completeness") != "complete"
                || word(run, "evidence_validity") != "valid"
        })
        .collect();
    if !weak_runs.is_empty() {
        any = true;
        report.line("- Runs whose execution or evidence is not completed, complete and valid:");
        for run in weak_runs.iter().take(MAX_LISTED) {
            let detail = format!(
                "execution {}, evidence completeness {}, evidence validity {}",
                inline_code(&word(run, "execution_status")),
                inline_code(&word(run, "evidence_completeness")),
                inline_code(&word(run, "evidence_validity"))
            );
            report.item("  ", run, &detail);
        }
    }
    let observations = report.records(&[Kind::Observation]);
    let weak: Vec<&Record> = observations
        .iter()
        .copied()
        .filter(|observation| text_at(&observation.body, "validity") != Some("valid"))
        .collect();
    if !weak.is_empty() {
        any = true;
        report.line("- Observations whose validity is not `valid`:");
        for observation in weak.iter().take(MAX_LISTED) {
            let detail = format!(
                "validity {}; {}",
                inline_code(&word(observation, "validity")),
                report.standing(observation)
            );
            report.item("  ", observation, &detail);
        }
    }
    let judged: Vec<(&Record, MeasurementValidity)> = observations
        .iter()
        .copied()
        .filter_map(|observation| {
            report
                .judged_validity(observation)
                .filter(|validity| *validity != MeasurementValidity::Valid)
                .map(|validity| (observation, validity))
        })
        .collect();
    if !judged.is_empty() {
        any = true;
        report.line(
            "- Measurements whose effective measurement validity, from their judgments, is not \
             `valid`:",
        );
        for (observation, validity) in judged.iter().take(MAX_LISTED) {
            let detail = format!(
                "effective measurement validity {}; {}",
                inline_code(validity_word(*validity)),
                report.standing(observation)
            );
            report.item("  ", observation, &detail);
        }
        if judged.len() > MAX_LISTED {
            report.line(&format!(
                "  - \u{2026} and {} more",
                judged.len() - MAX_LISTED
            ));
        }
    }
    let flagged: Vec<(&Record, Vec<String>)> = runs
        .iter()
        .copied()
        .chain(observations.iter().copied())
        .map(|record| (record, report.flag_ids(record)))
        .filter(|(_, flags)| !flags.is_empty())
        .collect();
    if !flagged.is_empty() {
        any = true;
        report.line("- Records with advisory measurement flags (a flag never changes validity):");
        for (record, flags) in flagged.iter().take(MAX_LISTED) {
            report.item("  ", record, &flags.join(", "));
        }
    }
    let unsettled: Vec<&Record> = runs
        .iter()
        .copied()
        .chain(observations.iter().copied())
        .filter(|record| report.unsettled(record))
        .collect();
    if !unsettled.is_empty() {
        any = true;
        report.line("- Inputs whose effective state needs review:");
        for record in unsettled.iter().take(MAX_LISTED) {
            let detail = report.standing(record);
            report.item("  ", record, &detail);
        }
    }
    if !any {
        report.line("- None listed beyond the uncertainty recorded on each conclusion below.");
    } else {
        report.line(
            "- The uncertainty each conclusion records is shown with it under the assessment \
             below.",
        );
    }
}

fn assessment(report: &mut Report<'_, '_>) {
    report.heading("Root-cause or measurement assessment");
    let hypotheses = report.records(&[Kind::Hypothesis]);
    if !hypotheses.is_empty() {
        report.subheading("Hypotheses");
        for hypothesis in hypotheses.iter().take(MAX_LISTED) {
            let detail = report.standing(hypothesis);
            report.item("", hypothesis, &detail);
        }
    }
    report.subheading("Conclusions");
    report.line(
        "Each conclusion shows its effective disposition beside its recorded status, its causal \
         account when one is recorded, and every element of a bounded conclusion. Negative and \
         inconclusive results are listed like any other.\n",
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
