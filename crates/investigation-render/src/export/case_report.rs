//! `case-report`: the framework's proof kind. Pure.
//!
//! One Markdown file, `case-report.md`, in a fixed section order: basis,
//! objective, questions, conclusions and decisions with their effective
//! disposition, observations, and limits. Every record is listed only when
//! the export includes it; effective dispositions come from the freshness
//! engine and the objection policy on the full pinned snapshot, so a
//! withheld objection still leaves its subject `disputed`. Negative and
//! inconclusive results are listed like any other, and a superseded record
//! names its successor instead of being dropped. A record that rests on, or
//! is challenged by, withheld material carries the policy's fixed caveats on
//! the lines under it.
//!
//! This is the minimal skeleton of the §15.4 case report. Methods,
//! alternatives, contradictions, evidence selectors and retrospective
//! highlights are later sections of the same file; adding them bumps
//! [`VERSION`].

use std::fmt::Write as _;

use investigation_core::{
    command::RevisionRef,
    identity::{Id, Kind},
    state::Record,
};

use super::{ExportInput, ExportRenderer, dependent_rule};
use crate::{
    Phase, PlanBuilder, PlanPath, RenderError,
    evidence::prose,
    note::text_at,
    records::{effective_word, lifecycle},
    text::inline_code,
};

/// Output format of this kind.
/// v2: the caveats of `inv.export-policy.v2` under each caveated record, and
/// the dependent-record rule in the limits (W11.02).
/// v3: the basis names the export's snapshot, which is the bundle when IDs
/// are remapped; a remapped report gives no command to open the case
/// (W11.13).
pub const VERSION: &str = "inv.export.case-report.v3";

/// The report's file name.
pub const FILE: &str = "case-report.md";

/// Records listed per section; the rest are counted.
const MAX_LISTED: usize = 50;

pub struct CaseReport;

impl ExportRenderer for CaseReport {
    fn version(&self) -> &'static str {
        VERSION
    }

    fn plan(&self, input: &ExportInput<'_>, files: &mut PlanBuilder) -> Result<(), RenderError> {
        let content = report(input);
        files.add(Phase::Views, PlanPath::new(FILE)?, content, None)
    }
}

fn report(input: &ExportInput<'_>) -> String {
    let case = input.case;
    let state = case.state();
    let case_record = state
        .current(case.case_id())
        .filter(|record| input.includes(record));
    let title = case_record
        .and_then(|record| input.title(record))
        .unwrap_or_else(|| "Case report".to_owned());
    let snapshot = input.snapshot();
    let mut out = format!("# {title}\n\n");
    out.push_str(
        "> Case report rendered by Investigation from one verified snapshot, with no model \
         call. It lists what the case records; it does not replace the primary evidence. \
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

    out.push_str("\n## Objective\n\n");
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
                "- Mode: {}",
                inline_code(text_at(&record.body, "mode").unwrap_or("not recorded"))
            );
            let _ = writeln!(
                out,
                "- Case status: {}",
                inline_code(lifecycle(record).unwrap_or("not recorded"))
            );
            caveats(&mut out, input, record, "");
        }
        None => out.push_str("- Not included for this audience.\n"),
    }

    section(&mut out, input, "Questions", &[Kind::Question], |record| {
        format!(
            "status {}",
            inline_code(lifecycle(record).unwrap_or("not recorded"))
        )
    });
    section(
        &mut out,
        input,
        "Conclusions and decisions",
        &[Kind::Conclusion, Kind::Decision],
        |record| {
            let recorded = lifecycle(record).unwrap_or("recorded");
            let mut line = match effective_word(input.notes, record) {
                Some(word) => format!(
                    "effective {} (recorded {})",
                    inline_code(&word),
                    inline_code(recorded)
                ),
                None => format!("recorded {}", inline_code(recorded)),
            };
            for (label, field) in [
                ("supersedes", "supersedes"),
                ("superseded by", "superseded_by"),
            ] {
                if let Some(other) = included_ref(input, record, field) {
                    let _ = write!(line, "; {label} {}", inline_code(&other.to_string()));
                }
            }
            line
        },
    );
    section(
        &mut out,
        input,
        "Observations",
        &[Kind::Observation],
        |record| {
            format!(
                "status {}",
                inline_code(lifecycle(record).unwrap_or("recorded"))
            )
        },
    );

    out.push_str("\n## Limits\n\n");
    out.push_str(
        "- Only records this audience may see are listed. export-manifest.json says whether \
         anything was withheld.\n",
    );
    out.push_str(
        "- Effective dispositions were computed on the whole case at this snapshot before \
         anything was withheld, so withholding a record does not make another read stronger.\n",
    );
    let _ = writeln!(out, "- {}", dependent_rule(input.audience));
    if input.remapped() {
        out.push_str(
            "- IDs in this report were remapped for this export only. To read the full evidence \
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

/// One bounded section listing the included records of `kinds` in ID order.
fn section(
    out: &mut String,
    input: &ExportInput<'_>,
    heading: &str,
    kinds: &[Kind],
    detail: impl Fn(&Record) -> String,
) {
    let _ = write!(out, "\n## {heading}\n\n");
    let state = input.case.state();
    let records: Vec<&Record> = kinds
        .iter()
        .flat_map(|kind| state.records_of_kind(*kind))
        .filter(|record| input.includes(record))
        .collect();
    if records.is_empty() {
        out.push_str("- None recorded or included.\n");
        return;
    }
    for record in records.iter().take(MAX_LISTED) {
        let title = input.title(record).unwrap_or_else(|| "untitled".to_owned());
        let _ = writeln!(
            out,
            "- {} {title}: {}",
            inline_code(&record.header.id.to_string()),
            detail(record)
        );
        caveats(out, input, record, "  ");
    }
    if records.len() > MAX_LISTED {
        let _ = writeln!(out, "- … and {} more", records.len() - MAX_LISTED);
    }
}

/// The policy's fixed caveats of a shown record, one line each.
fn caveats(out: &mut String, input: &ExportInput<'_>, record: &Record, indent: &str) {
    for caveat in input.caveats(record) {
        let _ = writeln!(out, "{indent}- Caveat: {}", caveat.text());
    }
}

/// The record a revision reference in `field` names, when it is included.
fn included_ref(input: &ExportInput<'_>, record: &Record, field: &str) -> Option<Id> {
    let reference: RevisionRef = serde_json::from_value(record.body.get(field)?.clone()).ok()?;
    input
        .case
        .state()
        .current(reference.id)
        .filter(|target| input.includes(target))
        .map(|_| reference.id)
}
