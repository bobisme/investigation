//! Per-type sections for every record kind beyond the evidence family. Pure.
//!
//! Each kind has its own module that chooses what a reader must see first,
//! between the summary and the typed links of the shared skeleton in
//! [`crate::note`]:
//!
//! - Question: what addresses it (hypotheses, options, criteria, inquiries,
//!   decisions) and its answer.
//! - Hypothesis: standing, rival explanations, and the assessments that
//!   contradict, support or fail to discriminate it, contradicting data first.
//! - Option and Criterion: eligibility or kind, thresholds, and the
//!   comparisons and decisions that use them.
//! - Comparison: the measurement, the option and criterion it relates, the
//!   outcome, its read set and its limits.
//! - Assumption, Inquiry and Run: claim and consequence; frozen plan and
//!   predictions; execution, validity and what the run produced.
//! - Assessment, Conclusion and Decision: the effective state from the
//!   freshness overlay and objection policy before the recorded lifecycle,
//!   the strongest outstanding objection, contradicting records, causal
//!   tests, hard conditions and what an accepted decision was accepted over.
//! - Review, Objection, Retrospective and Annotation: the review's kind,
//!   verdict, basis and freshness (a stale review says so prominently); the
//!   objection's severity and status; the closure account; the note text.
//!
//! - Case: [`overview`] writes the Overview body after the shared opening:
//!   the bounded opening summary, the title/synopsis/full ladder, the
//!   case's own reviews and the section its contents call for.
//!
//! [`reviews`] adds `## Reviews and objections` to every reviewable or
//! challenged record, so a stale review reads as stale on its own note and on
//! its subject's note. Authored text goes through [`crate::text`]; links come
//! from [`NoteContext::link`], which shows a withheld record by ID only.
//! Each section is withheld with the record's content channel.

use std::fmt::Write as _;

use investigation_core::{
    classification::Field,
    command::RevisionRef,
    decision_ops,
    identity::{Id, Kind},
    measurement_flags,
    review::EffectiveDisposition,
    review_ops,
    state::Record,
};
use serde_json::Value;

use crate::{
    RenderError,
    evidence::{self, MAX_LISTED, or_not_recorded, prose, revision_link},
    note::{NoteContext, text_at, value_at, wire_word},
    text::inline_code,
    yaml::Frontmatter,
};

mod annotation;
mod assessment;
mod assumption;
mod comparison;
mod conclusion;
mod criterion;
mod decision;
mod hypothesis;
mod inquiry;
mod matrix;
mod objection;
mod option;
pub(crate) mod overview;
mod question;
mod retrospective;
mod review;
pub(crate) mod reviews;
mod run;

/// The first per-type heading of each kind, also used when it is withheld.
const fn heading(kind: Kind) -> &'static str {
    match kind {
        Kind::Question => "Question",
        Kind::Hypothesis => "Standing",
        Kind::Option => "Option",
        Kind::Criterion => "Criterion",
        Kind::Comparison => "Measurement",
        Kind::Assumption => "Assumption",
        Kind::Inquiry => "Plan",
        Kind::Run => "Run",
        Kind::Assessment => "Assessment",
        Kind::Conclusion => "Disposition",
        Kind::Decision => "Choice",
        Kind::Review => "Review",
        Kind::Objection => "Objection",
        Kind::Retrospective => "Retrospective",
        Kind::Annotation => "Annotation",
        _ => "Record",
    }
}

/// The per-type sections of a record note, between the summary and the
/// typed links.
pub(crate) fn sections(
    notes: &NoteContext<'_>,
    out: &mut String,
    record: &Record,
) -> Result<(), RenderError> {
    let kind = record.header.id.kind();
    match kind {
        Kind::Source | Kind::Artifact | Kind::Observation => {
            evidence::sections(notes, out, record)?;
        }
        Kind::Case => return Ok(()),
        _ if !notes.may_show(record, Field::Content) => {
            evidence::withheld_section(out, heading(kind));
        }
        _ => {
            let mut facts = Facts {
                notes,
                out: &mut *out,
                record,
            };
            let _ = write!(facts.out, "\n## {}\n\n", heading(kind));
            match kind {
                Kind::Question => question::sections(&mut facts)?,
                Kind::Hypothesis => hypothesis::sections(&mut facts)?,
                Kind::Option => option::sections(&mut facts)?,
                Kind::Criterion => criterion::sections(&mut facts)?,
                Kind::Comparison => comparison::sections(&mut facts)?,
                Kind::Assumption => assumption::sections(&mut facts)?,
                Kind::Inquiry => inquiry::sections(&mut facts)?,
                Kind::Run => run::sections(&mut facts)?,
                Kind::Assessment => assessment::sections(&mut facts)?,
                Kind::Conclusion => conclusion::sections(&mut facts)?,
                Kind::Decision => decision::sections(&mut facts)?,
                Kind::Review => review::sections(&mut facts)?,
                Kind::Objection => objection::sections(&mut facts)?,
                Kind::Retrospective => retrospective::sections(&mut facts)?,
                Kind::Annotation => annotation::sections(&mut facts)?,
                _ => facts
                    .out
                    .push_str("No per-type fields are rendered for this kind.\n"),
            }
        }
    }
    if notes.may_show(record, Field::Content) {
        flags_section(notes, out, record)?;
    }
    reviews::section(notes, out, record)
}

/// Flat frontmatter properties of a visible record beyond the skeleton's.
pub(crate) fn properties(notes: &NoteContext<'_>, record: &Record, front: &mut Frontmatter) {
    if let Some(word) = effective_word(notes, record) {
        front.text("inv_disposition", &word);
    }
    match record.header.id.kind() {
        Kind::Review => review::properties(notes, record, front),
        Kind::Retrospective => retrospective::properties(notes, record, front),
        Kind::Case => overview::properties(notes, record, front),
        Kind::Objection => {
            if let Some(severity) = text_at(&record.body, "severity") {
                front.text("inv_severity", &wire(severity));
            }
        }
        _ => {}
    }
    reviews::properties(notes, record, front);
}

/// A recorded enum word for a property or a code span: kept only when it is
/// a plain snake_case word, so authored text never reaches a property.
fn wire(word: &str) -> String {
    if !word.is_empty()
        && word.len() <= 64
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        word.to_owned()
    } else {
        "unrecognized".to_owned()
    }
}

/// A recorded token for a code span: an enum word, number, boolean, digest
/// or timestamp is kept; anything else is authored text, never shown here.
fn token(word: &str) -> String {
    if !word.is_empty()
        && word.len() <= 128
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':' | b'+'))
    {
        word.to_owned()
    } else {
        "unrecognized".to_owned()
    }
}

/// The recorded lifecycle word: `status`, `lifecycle` or `disposition`.
pub(crate) fn lifecycle(record: &Record) -> Option<&str> {
    ["status", "lifecycle", "disposition"]
        .iter()
        .find_map(|key| record.body.get(*key).and_then(Value::as_str))
}

/// The effective state of a reviewable record: the objection policy and the
/// freshness overlay decide it, and the recorded lifecycle shows only when
/// nothing overrides it. `None` for other kinds.
pub(crate) fn effective_word(notes: &NoteContext<'_>, record: &Record) -> Option<String> {
    let kind = record.header.id.kind();
    if kind == Kind::Review {
        return Some(reviews::freshness_word(notes, record).to_owned());
    }
    if !matches!(
        kind,
        Kind::Conclusion | Kind::Assessment | Kind::Comparison | Kind::Decision
    ) {
        return None;
    }
    let state = notes.case().state();
    let index = notes.freshness();
    let disposition = review_ops::effective_disposition_with(state, index, record);
    Some(match (kind, disposition) {
        (_, EffectiveDisposition::Disputed) => "disputed".to_owned(),
        (Kind::Decision, _) => decision_ops::effective_decision_with(state, index, record)
            .map_or_else(|_| "needs_review".to_owned(), |word| wire_word(&word)),
        (_, EffectiveDisposition::NeedsReview | EffectiveDisposition::Conditional) => {
            wire_word(&disposition)
        }
        (Kind::Comparison, _) => decision_ops::effective_comparison_with(state, index, record)
            .map_or_else(|_| "needs_review".to_owned(), |word| wire_word(&word)),
        _ => lifecycle(record).map_or_else(|| "current".to_owned(), wire),
    })
}

/// A scalar body value as a word: strings, numbers and booleans as written,
/// a `Knowledge` value by its state, a tagged object by its `kind`.
fn scalar_word(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        Value::Object(map) => match map.get("state").and_then(Value::as_str) {
            Some("known") => map.get("value").and_then(scalar_word),
            Some("unknown") => Some("unknown".to_owned()),
            _ => map.get("kind").and_then(Value::as_str).map(str::to_owned),
        },
        _ => None,
    }
}

/// Exact revision references at a body path: one, a list, or none.
pub(crate) fn refs(value: Option<&Value>) -> Vec<RevisionRef> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| serde_json::from_value(item.clone()).ok())
            .collect(),
        Some(value @ Value::Object(_)) => serde_json::from_value(value.clone())
            .ok()
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

/// Bare record IDs in a list, such as a retrospective step's support.
fn ids(value: Option<&Value>) -> Vec<Id> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str()?.parse().ok())
        .collect()
}

/// A recorded decimal with its unit and statistic, such as `110 seconds`.
fn decimal(value: &Value) -> Option<String> {
    let number = value
        .get("decimal")
        .or_else(|| value.get("value"))
        .and_then(scalar_word)?;
    let mut text = inline_code(&token(&number));
    if let Some(unit) = value.get("unit").and_then(Value::as_str) {
        let _ = write!(text, " {}", inline_code(unit));
    }
    let mut extra = Vec::new();
    if let Some(statistic) = value.get("statistic").and_then(Value::as_str) {
        extra.push(format!("statistic {}", inline_code(statistic)));
    }
    if let Some(count) = value.get("sample_count").and_then(Value::as_u64) {
        extra.push(format!(
            "{count} sample{}",
            if count == 1 { "" } else { "s" }
        ));
    }
    if !extra.is_empty() {
        let _ = write!(text, " ({})", extra.join(", "));
    }
    Some(text)
}

/// Lifecycle and effective state of another record, for a backlink line, or
/// nothing when that record is withheld.
pub(crate) fn status_note(notes: &NoteContext<'_>, record: &Record) -> String {
    if !notes.visible(record) {
        return String::new();
    }
    let recorded = lifecycle(record).map_or_else(|| "recorded".to_owned(), wire);
    let effective = effective_word(notes, record).or_else(|| {
        notes
            .effective(record)
            .map(|effective| effective.freshness.as_str().to_owned())
    });
    match effective {
        Some(effective) if effective != recorded => format!(
            ", {} (effective {})",
            inline_code(&recorded),
            inline_code(&effective)
        ),
        _ => format!(", {}", inline_code(&recorded)),
    }
}

/// Current records that cite `record` through one of `fields` (any field when
/// empty), with the revision they cite. Records whose references are
/// withheld are left out, as in the typed links.
pub(crate) fn citing<'a>(
    notes: &NoteContext<'a>,
    record: &Record,
    fields: &[&str],
) -> Vec<(&'a Record, RevisionRef)> {
    let state = notes.case().state();
    let mut found: Vec<(&'a Record, RevisionRef)> = Vec::new();
    for citation in notes.citations(record.header.id) {
        if !fields.is_empty() && !fields.contains(&citation.field) {
            continue;
        }
        let Some(citing) = state.current(citation.by) else {
            continue;
        };
        if !notes.may_show(citing, Field::Lineage)
            || found
                .iter()
                .any(|(seen, _)| seen.header.id == citing.header.id)
        {
            continue;
        }
        found.push((citing, citation.cites));
    }
    found
}

/// Current records of `kind` whose body names `record` in one of `fields`
/// without a dependency edge (a rival, a superseding record).
pub(crate) fn naming<'a>(
    notes: &NoteContext<'a>,
    record: &Record,
    kind: Kind,
    fields: &[&str],
) -> Vec<&'a Record> {
    let state = notes.case().state();
    let mut found: Vec<&'a Record> = Vec::new();
    for mention in notes.mentioned_by(record.header.id) {
        if mention.by.kind() != kind || !fields.contains(&mention.field.as_str()) {
            continue;
        }
        if let Some(by) = state.current(mention.by)
            && notes.may_show(by, Field::Lineage)
            && !found.iter().any(|seen| seen.header.id == by.header.id)
        {
            found.push(by);
        }
    }
    found
}

/// Writes the bullet facts of one record body.
pub(crate) struct Facts<'o, 'n, 'a> {
    pub notes: &'n NoteContext<'a>,
    pub out: &'o mut String,
    pub record: &'n Record,
}

impl<'n> Facts<'_, 'n, '_> {
    fn value(&self, path: &str) -> Option<&'n Value> {
        let record: &'n Record = self.record;
        value_at(&record.body, path).filter(|value| !value.is_null())
    }

    /// A sub-heading.
    fn heading(&mut self, title: &str) {
        let _ = write!(self.out, "\n### {title}\n\n");
    }

    /// A fixed paragraph of the template, set apart from any list before it.
    fn sentence(&mut self, text: &str) {
        if !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
        let _ = writeln!(self.out, "{text}\n");
    }

    /// `- Label: text` with authored text escaped and bounded.
    fn text(&mut self, label: &str, path: &str) {
        let text = or_not_recorded(text_at(&self.record.body, path));
        let _ = writeln!(self.out, "- {label}: {text}");
    }

    /// Like [`Self::text`], only when the field holds text.
    fn text_present(&mut self, label: &str, path: &str) {
        if text_at(&self.record.body, path).is_some_and(|text| !text.trim().is_empty()) {
            self.text(label, path);
        }
    }

    /// `- Label: `word`` for an enum, flag, number, digest or timestamp.
    /// Anything else is authored text and shows as `unrecognized`.
    fn word(&mut self, label: &str, path: &str) {
        let word = self.value(path).and_then(scalar_word).map_or_else(
            || "not recorded".to_owned(),
            |word| inline_code(&token(&word)),
        );
        let _ = writeln!(self.out, "- {label}: {word}");
    }

    fn word_present(&mut self, label: &str, path: &str) {
        if self.value(path).and_then(scalar_word).is_some() {
            self.word(label, path);
        }
    }

    /// `- Label:` with one nested item per string, or `- Label: none`.
    fn list(&mut self, label: &str, path: &str, none: &str) {
        let items: Vec<&str> = self
            .value(path)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if items.is_empty() {
            let _ = writeln!(self.out, "- {label}: {none}");
            return;
        }
        let _ = writeln!(self.out, "- {label}:");
        for item in items.iter().take(MAX_LISTED) {
            let _ = writeln!(self.out, "  - {}", prose(item));
        }
        if items.len() > MAX_LISTED {
            let _ = writeln!(self.out, "  - and {} more", items.len() - MAX_LISTED);
        }
    }

    /// A text field that some operations store as a list.
    fn text_or_list(&mut self, label: &str, path: &str) {
        if self.value(path).is_some_and(Value::is_array) {
            self.list(label, path, "none recorded");
        } else {
            self.text(label, path);
        }
    }

    /// `- Label: [[record]] revision N`, or `not recorded`.
    fn reference(&mut self, label: &str, path: &str) -> Result<(), RenderError> {
        match refs(self.value(path)).first() {
            Some(reference) => {
                let link = revision_link(self.notes, *reference)?;
                let _ = writeln!(self.out, "- {label}: {link}");
            }
            None => {
                let _ = writeln!(self.out, "- {label}: not recorded");
            }
        }
        Ok(())
    }

    fn reference_present(&mut self, label: &str, path: &str) -> Result<(), RenderError> {
        if refs(self.value(path)).is_empty() {
            return Ok(());
        }
        self.reference(label, path)
    }

    /// `- Label:` with one nested link per exact reference, each with the
    /// record's lifecycle and effective state, or `- Label: none`.
    fn references(&mut self, label: &str, path: &str, none: &str) -> Result<(), RenderError> {
        let found = refs(self.value(path));
        self.reference_list(label, &found, none)
    }

    fn reference_list(
        &mut self,
        label: &str,
        found: &[RevisionRef],
        none: &str,
    ) -> Result<(), RenderError> {
        if found.is_empty() {
            let _ = writeln!(self.out, "- {label}: {none}");
            return Ok(());
        }
        let _ = writeln!(self.out, "- {label}:");
        for reference in found.iter().take(MAX_LISTED) {
            let status = self
                .notes
                .case()
                .state()
                .current(reference.id)
                .map(|current| status_note(self.notes, current))
                .unwrap_or_default();
            let _ = writeln!(
                self.out,
                "  - {}{status}",
                revision_link(self.notes, *reference)?
            );
        }
        if found.len() > MAX_LISTED {
            let _ = writeln!(self.out, "  - and {} more", found.len() - MAX_LISTED);
        }
        Ok(())
    }

    /// Backlinks: one line per citing record, with what it says about this
    /// one, or `none` when nothing cites it that way.
    fn backlinks(
        &mut self,
        records: &[(&Record, RevisionRef)],
        none: &str,
        mut describe: impl FnMut(&NoteContext<'_>, &Record) -> Result<String, RenderError>,
    ) -> Result<(), RenderError> {
        if records.is_empty() {
            let _ = writeln!(self.out, "{none}");
            return Ok(());
        }
        for (citing, cited) in records.iter().take(MAX_LISTED) {
            let mut line = format!(
                "- {}{}",
                self.notes.link(citing.header.id)?,
                status_note(self.notes, citing)
            );
            if self.notes.visible(citing) {
                line.push_str(&describe(self.notes, citing)?);
            }
            if cited.revision != self.record.header.revision.0 {
                let _ = write!(line, "; cites revision {}", cited.revision);
            }
            let _ = writeln!(self.out, "{line}");
        }
        if records.len() > MAX_LISTED {
            let _ = writeln!(self.out, "- and {} more", records.len() - MAX_LISTED);
        }
        Ok(())
    }

    /// The effective state line of a reviewable record.
    fn effective(&mut self) {
        if let Some(word) = effective_word(self.notes, self.record) {
            let recorded = lifecycle(self.record).map_or_else(|| "recorded".to_owned(), wire);
            let _ = writeln!(
                self.out,
                "- Effective disposition: {} (recorded lifecycle {})",
                inline_code(&word),
                inline_code(&recorded)
            );
        }
    }

    /// Confidence and likelihood, when recorded.
    fn judgment(&mut self) {
        let body = &self.record.body;
        if let Some(confidence) = self.value("confidence") {
            let level = confidence.get("level").and_then(scalar_word).map_or_else(
                || "not recorded".to_owned(),
                |word| inline_code(&token(&word)),
            );
            let _ = writeln!(
                self.out,
                "- Confidence: {level}. {}",
                or_not_recorded(text_at(body, "confidence.rationale"))
            );
            for limitation in confidence
                .get("limitations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .take(MAX_LISTED)
            {
                let _ = writeln!(self.out, "  - Limitation: {}", prose(limitation));
            }
            if let Some(change) = text_at(body, "confidence.what_would_change_it") {
                let _ = writeln!(self.out, "  - What would change it: {}", prose(change));
            }
        }
        if let Some(likelihood) = self.value("likelihood") {
            let estimate = likelihood
                .get("estimate")
                .and_then(scalar_word)
                .map_or_else(
                    || "not recorded".to_owned(),
                    |word| inline_code(&token(&word)),
                );
            let _ = writeln!(
                self.out,
                "- Likelihood: {estimate}, resolved when: {}",
                or_not_recorded(text_at(body, "likelihood.resolution_condition"))
            );
        }
        // The recorded outcome of the resolution condition, on its own line.
        if let Some(view) =
            investigation_core::likelihood::view(self.notes.case().state(), self.record)
            && let Some(outcome) = view.resolution
        {
            let score = outcome
                .brier
                .map_or_else(String::new, |brier| format!(", Brier score {brier}"));
            let _ = writeln!(
                self.out,
                "- Likelihood outcome: {}{score}",
                inline_code(&token(
                    serde_json::to_value(outcome.outcome)
                        .ok()
                        .as_ref()
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                )),
            );
        }
    }

    /// `- Strongest outstanding objection:` from the objection policy: open
    /// objections on the record or its basis, critical first.
    fn strongest_objection(&mut self) -> Result<(), RenderError> {
        let state = self.notes.case().state();
        let mut open =
            investigation_core::objection_policy::relevant_open_objections(state, self.record)
                .unwrap_or_default();
        open.sort_by_key(|objection| {
            (
                objection.severity != investigation_core::review::ObjectionSeverity::Critical,
                objection.objection_ref.id,
            )
        });
        let Some(strongest) = open.first() else {
            self.out.push_str(
                "- Strongest outstanding objection: none open on this record or its basis\n",
            );
            return Ok(());
        };
        let _ = write!(
            self.out,
            "- Strongest outstanding objection: {}",
            self.notes.link(strongest.objection_ref.id)?
        );
        let visible = state
            .current(strongest.objection_ref.id)
            .is_some_and(|objection| self.notes.visible(objection));
        if visible {
            let _ = write!(
                self.out,
                ", {} against {}",
                inline_code(&wire_word(&strongest.severity)),
                revision_link(self.notes, strongest.subject_ref)?
            );
        }
        self.out.push('\n');
        if open.len() > 1 {
            let _ = writeln!(
                self.out,
                "  - {} more open objection{} on this record or its basis",
                open.len() - 1,
                if open.len() == 2 { "" } else { "s" }
            );
        }
        Ok(())
    }
}

/// `### Measurement flags`: advisory flags from the measurement rules, shown
/// only when a rule raised one. They never change validity.
fn flags_section(
    notes: &NoteContext<'_>,
    out: &mut String,
    record: &Record,
) -> Result<(), RenderError> {
    let Some(report) = measurement_flags::record_flags(notes.case().state(), record) else {
        return Ok(());
    };
    if report.flags.is_empty() {
        return Ok(());
    }
    let _ = write!(
        out,
        "\n## Measurement flags\n\nAdvisory, rule version {}: a flag never blocks recording or changes validity.\n\n",
        inline_code(&report.rule_version)
    );
    for flag in &report.flags {
        let _ = write!(
            out,
            "- {}: {}",
            inline_code(&flag.flag_id),
            prose(&flag.reason)
        );
        for related in &flag.related_refs {
            let _ = write!(out, " See {}.", revision_link(notes, *related)?);
        }
        out.push('\n');
    }
    if report.omitted > 0 {
        let _ = writeln!(out, "- and {} more", report.omitted);
    }
    Ok(())
}
