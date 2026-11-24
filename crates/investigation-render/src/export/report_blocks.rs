//! Blocks shared by the report kinds (`case-report`, `analysis-report`). Pure.
//!
//! Every block follows the export contract in the module documentation of
//! `export`:
//!
//! - A record, or anything derived from it (its ID, title, a count, a value
//!   it holds), appears only when [`ExportInput::includes`] admits it. A
//!   reference to a record that is not included is left out, never replaced
//!   by a placeholder, and an empty list reads "none listed", which is true
//!   whether or not something was withheld.
//! - Every line that shows a record's standing (its effective state, an
//!   in-place annotation, its lifecycle) is followed by that record's
//!   [`ExportInput::caveats`], each rendered word for word.
//! - Effective states come from `input.notes`, computed on the full pinned
//!   snapshot before anything was withheld. A recorded `validated` or
//!   `accepted` that no longer holds is printed beside its effective state
//!   with a statement that it no longer holds, never on its own.
//! - Stale, retracted, superseded and withdrawn inputs are annotated where
//!   they are cited, not only in a list of their own.
//! - Content derived from records that is not itself one (an advisory
//!   flag's reason) is left out when it read anything not included, and
//!   reported through [`ExportInput::admits`] so the export reads
//!   incomplete.
//! - Authored text is escaped prose ([`prose`]); captured excerpts are
//!   bounded plain-text blocks ([`fenced_block`]). Confidence and likelihood
//!   are always on separate lines, never in one sentence.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

use investigation_core::{
    command::RevisionRef,
    derivation,
    evidence::{MeasurementValidity, ObservationBasis, ObservationBody, ObservationValue},
    freshness::Freshness,
    header::Knowledge,
    identity::{Id, Kind},
    measurement_fields::{self, MeasurementFields, SampleSet},
    measurement_flags, measurement_ops, objection_policy,
    review::ObjectionSeverity,
    source_ops::{self, UseState},
    state::{Record, State},
};
use serde_json::Value;

use super::{ExportInput, Omission, Omitted, dependent_rule};
use crate::{
    evidence::{EXCERPT_CHARS, EXCERPT_LINES, bounded_excerpt, prose},
    note::{text_at, value_at, wire_word},
    records::{effective_word, lifecycle, refs, reviews::freshness_word, scalar_word, token, wire},
    text::{fenced_block, inline_code},
};

/// Records listed per section; the rest are counted.
pub(super) const MAX_LISTED: usize = 50;
/// References or authored items listed per field; the rest are counted.
pub(super) const MAX_ITEMS: usize = 16;
/// Items per retrospective highlight list.
const MAX_HIGHLIGHTS: usize = 5;

/// Lifecycle words that mark an input as no longer standing as recorded.
const ENDED: &[&str] = &[
    "retracted",
    "withdrawn",
    "superseded",
    "retired",
    "rejected",
    "invalid",
];

/// Fields of the typed Observation body. Keys other operations keep beside
/// it (lifecycle, origin tracing, assessments) are not part of it.
const OBSERVATION_FIELDS: &[&str] = &[
    "value",
    "basis",
    "extraction_method",
    "validity",
    "quality",
    "limitations",
    "evidence_kind",
    "variables_changed",
    "observed_at",
    "measurement",
];

/// The typed Observation body, read from its own fields only.
fn observation_body(record: &Record) -> Option<ObservationBody> {
    let object = record.body.as_object()?;
    let typed: serde_json::Map<String, Value> = object
        .iter()
        .filter(|(key, _)| OBSERVATION_FIELDS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    serde_json::from_value(Value::Object(typed)).ok()
}

fn code(text: &str) -> String {
    inline_code(text)
}

fn or_not_recorded(text: Option<&str>) -> String {
    text.filter(|text| !text.trim().is_empty())
        .map_or_else(|| "not recorded".to_owned(), prose)
}

fn strings(value: Option<&Value>) -> Vec<&str> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .collect()
}

fn more(out: &mut String, indent: &str, total: usize, shown: usize) {
    if total > shown {
        let _ = writeln!(out, "{indent}- \u{2026} and {} more", total - shown);
    }
}

fn knowledge<T>(value: &Knowledge<T>, shown: impl Fn(&T) -> String) -> String {
    match value {
        Knowledge::Known(value) => shown(value),
        Knowledge::Unknown => "recorded as unknown".to_owned(),
    }
}

/// One report being written: the input it may show and the text so far.
pub(super) struct Report<'i, 'a> {
    pub input: &'i ExportInput<'a>,
    pub out: String,
}

impl<'i, 'a> Report<'i, 'a> {
    pub fn new(input: &'i ExportInput<'a>) -> Self {
        Self {
            input,
            out: String::new(),
        }
    }

    pub fn finish(self) -> String {
        self.out
    }

    fn state(&self) -> &'a State {
        self.input.case.state()
    }

    /// The case record, when it is included.
    pub fn case_record(&self) -> Option<&'a Record> {
        self.state()
            .current(self.input.case.case_id())
            .filter(|record| self.input.includes(record))
    }

    /// Included current records of `kinds` in this case, kind by kind in ID
    /// order.
    pub fn records(&self, kinds: &[Kind]) -> Vec<&'a Record> {
        let case = self.input.case.case_id();
        kinds
            .iter()
            .flat_map(|kind| self.state().records_of_kind(*kind))
            .filter(|record| record.header.case_id == case && self.input.includes(record))
            .collect()
    }

    /// The current record a reference names, when it is included.
    fn shown(&self, id: Id) -> Option<&'a Record> {
        self.state()
            .current(id)
            .filter(|record| self.input.includes(record))
    }

    /// The report text so far, for a framework section that writes itself
    /// (the human files relied on).
    pub const fn buffer(&mut self) -> &mut String {
        &mut self.out
    }

    pub fn heading(&mut self, title: &str) {
        let _ = write!(self.out, "\n## {title}\n\n");
    }

    pub fn subheading(&mut self, title: &str) {
        if !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
        let _ = write!(self.out, "### {title}\n\n");
    }

    pub fn line(&mut self, text: &str) {
        let _ = writeln!(self.out, "{text}");
    }

    /// `` `id` title `` of an included record.
    pub fn name(&self, record: &Record) -> String {
        let title = self
            .input
            .title(record)
            .unwrap_or_else(|| "untitled".to_owned());
        format!("{} {title}", code(&record.header.id.to_string()))
    }

    /// The policy's fixed caveats of a shown record, one line each.
    pub fn caveats(&mut self, record: &Record, indent: &str) {
        for caveat in self.input.caveats(record) {
            let _ = writeln!(self.out, "{indent}- Caveat: {}", caveat.text());
        }
    }

    /// `- name: detail` and the record's caveats under it.
    pub fn item(&mut self, indent: &str, record: &Record, detail: &str) {
        let name = self.name(record);
        if detail.is_empty() {
            let _ = writeln!(self.out, "{indent}- {name}");
        } else {
            let _ = writeln!(self.out, "{indent}- {name}: {detail}");
        }
        self.caveats(record, &format!("{indent}  "));
    }

    // ------------------------------------------------------------ Standing

    /// The effective state of a reviewable record beside its recorded
    /// lifecycle, or the recorded lifecycle and any annotation of another.
    pub fn standing(&self, record: &Record) -> String {
        let recorded = lifecycle(record).map_or_else(|| "recorded".to_owned(), wire);
        match effective_word(self.input.notes, record) {
            Some(word) => {
                let mut text = format!("effective {} (recorded {}", code(&word), code(&recorded));
                if matches!(recorded.as_str(), "validated" | "accepted") && word != recorded {
                    let _ = write!(text, ", which does not hold as it stands at this snapshot");
                }
                text.push(')');
                text
            }
            None => {
                let mut text = format!("status {}", code(&recorded));
                if let Some(note) = self.annotation(record, false) {
                    let _ = write!(text, "; {note}");
                }
                text
            }
        }
    }

    /// `; status word` for a cited line, whose annotation is added apart.
    fn status(&self, record: &Record) -> String {
        format!(
            "; status {}",
            code(&lifecycle(record).map_or_else(|| "recorded".to_owned(), wire))
        )
    }

    /// Why an input does not stand as recorded: a stale or disputed
    /// effective state, withdrawn evidentiary use, or an ended lifecycle.
    /// `None` when it stands. With `lifecycle_too`, an ended lifecycle is
    /// named as well.
    fn annotation(&self, record: &Record, lifecycle_too: bool) -> Option<String> {
        let mut notes = Vec::new();
        if lifecycle_too && let Some(word) = lifecycle(record).filter(|word| ENDED.contains(word)) {
            notes.push(format!("recorded {}", code(word)));
        }
        if let Some(word) = effective_word(self.input.notes, record) {
            if !matches!(word.as_str(), "current" | "validated" | "accepted")
                && Some(word.as_str()) != lifecycle(record)
            {
                notes.push(format!("effective {}", code(&word)));
            }
        } else if let Some(effective) = self.input.notes.effective(record)
            && effective.freshness != Freshness::Current
        {
            notes.push(format!("effective {}", code(effective.freshness.as_str())));
        }
        if matches!(
            record.header.id.kind(),
            Kind::Observation | Kind::Artifact | Kind::Source
        ) {
            match source_ops::evidentiary_use(self.state(), record).state {
                UseState::InUse => {}
                UseState::PartiallyWithdrawn => {
                    notes.push(format!("evidentiary use {}", code("partially_withdrawn")));
                }
                UseState::Withdrawn => {
                    notes.push(format!("evidentiary use {}", code("withdrawn")));
                }
            }
        }
        (!notes.is_empty()).then(|| format!("annotated: {}", notes.join(", ")))
    }

    /// Whether an included record's effective state asks for attention.
    pub fn unsettled(&self, record: &Record) -> bool {
        match effective_word(self.input.notes, record) {
            Some(word) => matches!(
                word.as_str(),
                "needs_review" | "disputed" | "conditional" | "stale"
            ),
            None => self
                .input
                .notes
                .effective(record)
                .is_some_and(|effective| effective.freshness != Freshness::Current),
        }
    }

    // ---------------------------------------------------------- References

    /// One cited revision as a list item, with its in-place annotation and
    /// caveats, when the record is included.
    fn cited(&mut self, indent: &str, reference: RevisionRef, extra: &str) -> bool {
        self.labeled(indent, "", reference, extra)
    }

    /// [`Self::cited`] with a label before the reference, such as `Baseline: `.
    fn labeled(&mut self, indent: &str, label: &str, reference: RevisionRef, extra: &str) -> bool {
        let Some(record) = self.shown(reference.id) else {
            return false;
        };
        let mut line = format!(
            "{indent}- {label}{} revision {}",
            code(&reference.id.to_string()),
            reference.revision
        );
        if record.header.revision.0 != reference.revision {
            let _ = write!(line, " (current revision {})", record.header.revision.0);
        }
        if let Some(title) = self.input.title(record) {
            let _ = write!(line, ": {title}");
        }
        line.push_str(extra);
        if let Some(note) = self.annotation(record, true) {
            let _ = write!(line, "; {note}");
        }
        let _ = writeln!(self.out, "{line}");
        self.caveats(record, &format!("{indent}  "));
        true
    }

    /// `- label:` and the included references at `value` under it, each
    /// with `extra(record)`, or `- label: none listed`.
    fn cited_list(
        &mut self,
        indent: &str,
        label: &str,
        value: Option<&Value>,
        extra: impl Fn(&Self, &Record) -> String,
    ) {
        let shown: Vec<RevisionRef> = refs(value)
            .into_iter()
            .filter(|reference| self.shown(reference.id).is_some())
            .collect();
        if shown.is_empty() {
            let _ = writeln!(self.out, "{indent}- {label}: none listed");
            return;
        }
        let _ = writeln!(self.out, "{indent}- {label}:");
        let inner = format!("{indent}  ");
        for reference in shown.iter().take(MAX_ITEMS) {
            let text = self
                .shown(reference.id)
                .map(|record| extra(self, record))
                .unwrap_or_default();
            self.cited(&inner, *reference, &text);
        }
        more(&mut self.out, &inner, shown.len(), MAX_ITEMS);
    }

    /// Bare included IDs, such as a retrospective step's support.
    fn id_list(&self, value: Option<&Value>) -> String {
        let ids: Vec<String> = value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.as_str()?.parse::<Id>().ok())
            .filter(|id| self.shown(*id).is_some())
            .map(|id| code(&id.to_string()))
            .collect();
        if ids.is_empty() {
            "none listed".to_owned()
        } else {
            let total = ids.len();
            let mut text = ids
                .into_iter()
                .take(MAX_ITEMS)
                .collect::<Vec<_>>()
                .join(", ");
            if total > MAX_ITEMS {
                let _ = write!(text, " and {} more", total - MAX_ITEMS);
            }
            text
        }
    }

    /// `- label: prose` for each authored string at `path`, bounded, or
    /// `- label: none` once.
    fn authored_list(&mut self, indent: &str, label: &str, value: Option<&Value>, none: &str) {
        let items = strings(value);
        if items.is_empty() {
            let _ = writeln!(self.out, "{indent}- {label}: {none}");
            return;
        }
        for item in items.iter().take(MAX_ITEMS) {
            let _ = writeln!(self.out, "{indent}- {label}: {}", prose(item));
        }
        more(&mut self.out, indent, items.len(), MAX_ITEMS);
    }

    // -------------------------------------------------------- Observations

    /// Value, citation and quote state of an observation, on one line, for
    /// an evidence list.
    pub fn evidence_note(&self, record: &Record) -> String {
        if record.header.id.kind() != Kind::Observation {
            return String::new();
        }
        let Some(body) = observation_body(record) else {
            return String::new();
        };
        let mut parts = Vec::new();
        if let ObservationValue::Measurement { value, unit } = &body.value {
            parts.push(format!("value {} {}", code(value.as_str()), code(unit)));
        }
        match &body.basis {
            ObservationBasis::Source {
                artifact_ref,
                selector,
                text_quote,
            } => {
                let mut cite = format!("selector {}", code(&wire_word(&selector.selector)));
                if self.shown(artifact_ref.id).is_some() {
                    let _ = write!(
                        cite,
                        " on {} revision {}",
                        code(&artifact_ref.id.to_string()),
                        artifact_ref.revision
                    );
                }
                let _ = write!(cite, ", quote {}", code(&wire_word(&text_quote.state)));
                parts.push(cite);
            }
            ObservationBasis::Assertion { .. } => {
                parts.push("attributed assertion, not source-backed".to_owned());
            }
        }
        format!("; {}", parts.join("; "))
    }

    /// One observation: standing, value, citation by source and selector
    /// with quote state, a bounded inert excerpt, and its limits.
    pub fn observation(&mut self, indent: &str, record: &Record) {
        let body = observation_body(record);
        let mut detail = String::new();
        if let Some(body) = &body {
            let _ = write!(detail, "validity {}", code(&wire_word(&body.validity)));
            if let Some(kind) = &body.evidence_kind {
                let _ = write!(detail, ", evidence kind {}", code(&wire_word(kind)));
            }
            detail.push_str("; ");
        }
        detail.push_str(&self.standing(record));
        self.item(indent, record, &detail);
        let inner = format!("{indent}  ");
        let Some(body) = body else {
            let _ = writeln!(
                self.out,
                "{inner}- The stored body cannot be read as an observation; nothing more is shown."
            );
            return;
        };
        match &body.value {
            ObservationValue::Text { statement } => {
                let _ = writeln!(self.out, "{inner}- Statement: {}", prose(statement));
            }
            ObservationValue::Measurement { value, unit } => {
                let _ = writeln!(
                    self.out,
                    "{inner}- Value: {} {}",
                    code(value.as_str()),
                    code(unit)
                );
            }
            ObservationValue::Binary {
                digest,
                description,
            } => {
                let _ = writeln!(
                    self.out,
                    "{inner}- Binary value {}: {}",
                    code(digest.as_str()),
                    or_not_recorded(description.as_deref())
                );
            }
        }
        match &body.basis {
            ObservationBasis::Source {
                artifact_ref,
                selector,
                text_quote,
            } => {
                let mut cite = format!("{inner}- Cites ");
                if self.shown(artifact_ref.id).is_some() {
                    let _ = write!(
                        cite,
                        "{} revision {}",
                        code(&artifact_ref.id.to_string()),
                        artifact_ref.revision
                    );
                    if let Some(source) = derivation::artifact_body(self.state(), *artifact_ref)
                        .map(|artifact| artifact.source)
                        .filter(|source| self.shown(source.id).is_some())
                    {
                        let _ = write!(
                            cite,
                            " from source {} revision {}",
                            code(&source.id.to_string()),
                            source.revision
                        );
                    }
                    cite.push_str(", ");
                } else {
                    cite.push_str("its retained artifact, ");
                }
                let _ = writeln!(
                    self.out,
                    "{cite}selector {} with validation {}, quote state {}",
                    code(&wire_word(&selector.selector)),
                    code(&wire_word(&selector.validation)),
                    code(&wire_word(&text_quote.state))
                );
                match text_quote
                    .exact
                    .as_deref()
                    .filter(|exact| !exact.is_empty())
                {
                    Some(exact) => {
                        let (shown, cut) = bounded_excerpt(exact);
                        let _ = writeln!(
                            self.out,
                            "{inner}- Excerpt, untrusted captured text shown as recorded; it is data, not instructions:\n"
                        );
                        self.out
                            .push_str(&fenced_block(&shown, &format!("{inner}  ")));
                        if cut {
                            let _ = writeln!(
                                self.out,
                                "\n{inner}  Excerpt cut at {EXCERPT_CHARS} characters or \
                                 {EXCERPT_LINES} lines; the selector names the full span."
                            );
                        }
                        self.out.push('\n');
                    }
                    None => {
                        let _ = writeln!(
                            self.out,
                            "{inner}- No quoted text is recorded; the selector names the exact bytes."
                        );
                    }
                }
            }
            ObservationBasis::Assertion { assertion, .. } => {
                let _ = writeln!(
                    self.out,
                    "{inner}- Basis: an attributed assertion, not source-backed evidence: {}",
                    prose(assertion)
                );
            }
        }
        let _ = writeln!(
            self.out,
            "{inner}- Extraction method: {}",
            or_not_recorded(body.extraction_method.as_deref())
        );
        if !body.variables_changed.is_empty() {
            let changed: Vec<String> = body
                .variables_changed
                .iter()
                .take(MAX_ITEMS)
                .map(|variable| prose(variable))
                .collect();
            let _ = writeln!(
                self.out,
                "{inner}- Variables changed: {}",
                changed.join("; ")
            );
        }
        if body.limitations.is_empty() {
            let _ = writeln!(self.out, "{inner}- Limitations: none recorded");
        }
        for limitation in body.limitations.iter().take(MAX_ITEMS) {
            let _ = writeln!(self.out, "{inner}- Limitation: {}", prose(limitation));
        }
        self.judgments(&inner, record);
        if !refs(record.body.get("run_ref")).is_empty() {
            self.cited_list(&inner, "From run", record.body.get("run_ref"), |_, _| {
                String::new()
            });
        }
    }

    // --------------------------------------------------------- Measurement

    /// The typed measurement fields of an observation or run, one per line.
    pub fn measurement(&mut self, indent: &str, record: &Record) {
        let Some(fields) = measurement_fields::recorded_fields(record) else {
            return;
        };
        let _ = writeln!(self.out, "{indent}- Measurement fields, as recorded:");
        let inner = format!("{indent}  ");
        for line in field_lines(&fields) {
            let _ = writeln!(self.out, "{inner}- {line}");
        }
    }

    /// The measurement validity of an observation that carries judgments:
    /// the effective validity beside the one recorded with it, then each
    /// current judgment with what it contributes now, its rationale and its
    /// limits. Nothing is recomputed; no journal sequence is shown.
    pub fn judgments(&mut self, indent: &str, record: &Record) {
        if record.header.id.kind() != Kind::Observation {
            return;
        }
        let Ok(view) = measurement_ops::validity_view(self.state(), record) else {
            return;
        };
        if view.judgments_kept == 0 {
            return;
        }
        let _ = writeln!(
            self.out,
            "{indent}- Measurement validity: effective {} (recorded with the observation {}){}",
            code(validity_word(view.state)),
            code(validity_word(view.recorded)),
            if view.disagreement {
                "; the current judgments disagree"
            } else {
                ""
            }
        );
        let log = measurement_ops::judgment_log(&record.body).unwrap_or_default();
        let inner = format!("{indent}  ");
        let judged: Vec<_> = view
            .current
            .iter()
            .filter(|judgment| judgment.accepted_at_sequence.is_some())
            .collect();
        for judgment in judged.iter().take(MAX_ITEMS) {
            let mut line = format!(
                "{inner}- Judgment {} by {}",
                code(validity_word(judgment.validity)),
                code(&judgment.assessor.to_string())
            );
            if let Some(channel) = &judgment.channel {
                let _ = write!(line, " on channel {}", code(channel));
            }
            if judgment.contributes != judgment.validity {
                let _ = write!(
                    line,
                    "; contributes {} now, because its verification changed since it was recorded",
                    code(validity_word(judgment.contributes))
                );
            }
            let _ = writeln!(self.out, "{line}");
            let recorded = log.judgments.iter().rev().find(|entry| {
                entry.assessor == judgment.assessor && entry.channel == judgment.channel
            });
            if let Some(recorded) = recorded {
                let _ = writeln!(
                    self.out,
                    "{inner}  - Rationale: {}",
                    prose(&recorded.rationale)
                );
                for limit in recorded.limits.iter().take(MAX_ITEMS) {
                    let _ = writeln!(self.out, "{inner}  - Limit: {}", prose(limit));
                }
            }
        }
        more(&mut self.out, &inner, judged.len(), MAX_ITEMS);
    }

    /// The effective measurement validity of an observation that carries
    /// judgments, or `None` when nobody judged it.
    pub fn judged_validity(&self, record: &Record) -> Option<MeasurementValidity> {
        if record.header.id.kind() != Kind::Observation {
            return None;
        }
        measurement_ops::validity_view(self.state(), record)
            .ok()
            .filter(|view| view.judgments_kept > 0)
            .map(|view| view.state)
    }

    /// Advisory measurement flags on one record, when a rule raised any.
    pub fn flags(&mut self, indent: &str, record: &Record) {
        let Some(report) = measurement_flags::record_flags(self.state(), record) else {
            return;
        };
        if report.flags.is_empty() {
            return;
        }
        let _ = writeln!(
            self.out,
            "{indent}- Advisory flags, rule version {}; a flag never changes validity:",
            code(&report.rule_version)
        );
        let inner = format!("{indent}  ");
        for flag in &report.flags {
            // A reason quotes the fields it read; it is shown only when every
            // record the rule read is included, and reported as left out
            // otherwise.
            let read = flag
                .fields
                .iter()
                .map(|field| field.record_ref.id)
                .chain(flag.related_refs.iter().map(|reference| reference.id));
            let readable = self.input.admits(read, |reason| {
                Omission::new(Omitted::FlagReason, reason)
                    .of(record.header.id)
                    .at(flag.flag_id.clone())
            });
            if readable {
                let _ = writeln!(
                    self.out,
                    "{inner}- {}: {}",
                    code(&flag.flag_id),
                    prose(&flag.reason)
                );
            } else {
                let _ = writeln!(self.out, "{inner}- {}", code(&flag.flag_id));
            }
        }
        more(
            &mut self.out,
            &inner,
            report.flags.len() + report.omitted,
            report.flags.len(),
        );
    }

    /// Flag IDs raised on one record, for a summary line.
    pub fn flag_ids(&self, record: &Record) -> Vec<String> {
        measurement_flags::record_flags(self.state(), record)
            .map(|report| {
                report
                    .flags
                    .iter()
                    .map(|flag| code(&flag.flag_id))
                    .collect()
            })
            .unwrap_or_default()
    }

    // ---------------------------------------------------------------- Runs

    /// One run: intent, execution status, evidence completeness and evidence
    /// validity on separate lines, evidence kind, changed variables,
    /// measurement fields, advisory flags and the observations it produced.
    pub fn run(&mut self, indent: &str, record: &Record) {
        let body = &record.body;
        let detail = format!(
            "execution {}; {}",
            code(&wire(
                text_at(body, "execution_status").unwrap_or("unknown")
            )),
            self.standing(record)
        );
        self.item(indent, record, &detail);
        let inner = format!("{indent}  ");
        let intent_kind = wire(text_at(body, "intent.kind").unwrap_or("unknown"));
        match text_at(body, "intent.intent") {
            Some(intent) => {
                let _ = writeln!(
                    self.out,
                    "{inner}- Intent: {} {}",
                    code(&intent_kind),
                    prose(intent)
                );
            }
            None => {
                let _ = writeln!(self.out, "{inner}- Intent: {}", code(&intent_kind));
                if body
                    .get("intent")
                    .and_then(|intent| intent.get("plan_ref"))
                    .is_some()
                {
                    self.cited_list(
                        &inner,
                        "Plan",
                        body.get("intent").and_then(|intent| intent.get("plan_ref")),
                        |_, _| String::new(),
                    );
                }
            }
        }
        let _ = writeln!(
            self.out,
            "{inner}- Environment: {}",
            or_not_recorded(text_at(body, "environment"))
        );
        let _ = writeln!(
            self.out,
            "{inner}- Execution status: {}",
            code(&wire(
                text_at(body, "execution_status").unwrap_or("unknown")
            ))
        );
        let _ = writeln!(
            self.out,
            "{inner}- Evidence completeness: {}",
            code(&wire(
                text_at(body, "evidence_completeness").unwrap_or("unknown")
            ))
        );
        let validity = wire(text_at(body, "evidence_validity").unwrap_or("unknown"));
        let _ = writeln!(
            self.out,
            "{inner}- Evidence validity: {}{}",
            code(&validity),
            if validity == "invalid" {
                "; its results must not ground claims"
            } else {
                ""
            }
        );
        let _ = writeln!(
            self.out,
            "{inner}- Evidence kind: {}",
            code(&wire(
                text_at(body, "evidence_kind").unwrap_or("not_recorded")
            ))
        );
        let own = strings(body.get("variables_changed"));
        let changed = self.changed_variables(record);
        if changed.is_empty() {
            let _ = writeln!(self.out, "{inner}- Variables changed: none recorded");
        } else {
            let shown: Vec<String> = changed.iter().take(MAX_ITEMS).map(|v| prose(v)).collect();
            let source = if own.is_empty() {
                ", as its observations record them"
            } else {
                ""
            };
            let _ = writeln!(
                self.out,
                "{inner}- Variables changed{source}: {}",
                shown.join("; ")
            );
        }
        if let Some(repetition) = measurement_fields::recorded_repetition(body) {
            let _ = writeln!(
                self.out,
                "{inner}- Repetition series: {}",
                code(&repetition.series)
            );
        }
        self.measurement(&inner, record);
        self.flags(&inner, record);
        let observations: Vec<&'a Record> = self.produced(record);
        if observations.is_empty() {
            let _ = writeln!(self.out, "{inner}- Observations from this run: none listed");
        } else {
            let _ = writeln!(self.out, "{inner}- Observations from this run:");
            let deeper = format!("{inner}  ");
            let fields = format!("{deeper}  ");
            for observation in observations.iter().take(MAX_ITEMS) {
                let note = self.evidence_note(observation);
                let detail = format!("{}{note}", self.standing(observation));
                self.item(&deeper, observation, &detail);
                self.measurement(&fields, observation);
                self.judgments(&fields, observation);
                self.flags(&fields, observation);
            }
            more(&mut self.out, &deeper, observations.len(), MAX_ITEMS);
        }
    }

    /// The variables a run changed: those recorded on the run and those its
    /// included observations record, sorted and without repeats.
    pub fn changed_variables(&self, run: &Record) -> Vec<String> {
        let mut changed: BTreeSet<String> = strings(run.body.get("variables_changed"))
            .into_iter()
            .map(str::to_owned)
            .collect();
        for observation in self.produced(run) {
            changed.extend(
                strings(observation.body.get("variables_changed"))
                    .into_iter()
                    .map(str::to_owned),
            );
        }
        changed.into_iter().collect()
    }

    /// Included observations that cite `run` as the run that produced them.
    pub fn produced(&self, run: &Record) -> Vec<&'a Record> {
        let mut found: Vec<&'a Record> = Vec::new();
        for citation in self.input.notes.citations(run.header.id) {
            if citation.field != "run_ref" || citation.by.kind() != Kind::Observation {
                continue;
            }
            if let Some(record) = self.shown(citation.by)
                && !found.iter().any(|seen| seen.header.id == record.header.id)
            {
                found.push(record);
            }
        }
        found.sort_by_key(|record| record.header.id);
        found
    }

    /// Repetition series among the included runs: how many repeats were
    /// recorded and how many completed with complete, valid evidence.
    pub fn repetitions(&mut self, runs: &[&Record]) {
        let mut series: BTreeMap<String, Vec<&Record>> = BTreeMap::new();
        for run in runs {
            if let Some(repetition) = measurement_fields::recorded_repetition(&run.body) {
                series.entry(repetition.series).or_default().push(run);
            }
        }
        if series.is_empty() {
            self.line("- No repetition series is recorded.");
            return;
        }
        for (name, members) in series {
            let usable = members
                .iter()
                .filter(|run| {
                    text_at(&run.body, "execution_status") == Some("completed")
                        && text_at(&run.body, "evidence_completeness") == Some("complete")
                        && text_at(&run.body, "evidence_validity") == Some("valid")
                })
                .count();
            let ids: Vec<String> = members
                .iter()
                .take(MAX_ITEMS)
                .map(|run| code(&run.header.id.to_string()))
                .collect();
            let _ = writeln!(
                self.out,
                "- Series {}: {usable} of {} repeat{} completed with complete, valid evidence ({}). \
                 Whether each repeat reproduced the result is shown by its observations.",
                code(&name),
                members.len(),
                if members.len() == 1 { "" } else { "s" },
                ids.join(", ")
            );
        }
    }

    // --------------------------------------------------------- Conclusions

    /// One conclusion with every element §15.3 names: significance,
    /// applicability, evidence selectors, alternatives addressed,
    /// uncertainty, dissent and review, what would invalidate it, and its
    /// causal account when one is recorded. No severity score is derived.
    pub fn conclusion(&mut self, record: &Record) {
        let body = &record.body;
        let mut detail = self.standing(record);
        self.supersession(&mut detail, record);
        self.item("", record, &detail);
        let i = "  ";
        let _ = writeln!(
            self.out,
            "{i}- Statement: {}",
            or_not_recorded(text_at(body, "statement"))
        );
        if let Some(form) = text_at(body, "claim_form") {
            let _ = writeln!(self.out, "{i}- Claim form: {}", code(&wire(form)));
        }
        let _ = writeln!(
            self.out,
            "{i}- Applicability: {}",
            or_not_recorded(text_at(body, "scope"))
        );
        let _ = writeln!(
            self.out,
            "{i}- Significance: {}",
            or_not_recorded(text_at(body, "significance"))
        );
        self.cited_list(i, "Evidence", body.get("support_refs"), |report, cited| {
            report.evidence_note(cited)
        });
        self.cited_list(
            i,
            "Alternatives addressed",
            body.get("contradiction_refs"),
            |report, cited| report.status(cited),
        );
        self.strongest_contradiction(i, record);
        self.uncertainty(i, record);
        self.review(i, record);
        self.invalidation(i, record);
        self.causal(i, record);
    }

    /// One decision: its standing and supersession, the choice, tradeoffs,
    /// consequences, reopen trigger, review and strongest contradiction.
    pub fn decision(&mut self, record: &Record) {
        let body = &record.body;
        let mut detail = self.standing(record);
        self.supersession(&mut detail, record);
        self.item("", record, &detail);
        let i = "  ";
        let choice = wire(text_at(body, "choice.kind").unwrap_or("unknown"));
        let option = body
            .get("choice")
            .and_then(|choice| choice.get("option_ref"));
        if option.is_some() {
            self.cited_list(i, &format!("Choice ({})", code(&choice)), option, |_, _| {
                String::new()
            });
        } else {
            let _ = writeln!(self.out, "{i}- Choice: {}", code(&choice));
        }
        self.authored_list(i, "Tradeoff", body.get("tradeoffs"), "none recorded");
        self.authored_list(i, "Consequence", body.get("consequences"), "none recorded");
        if let Some(expected) = text_at(body, "expected_outcome") {
            let _ = writeln!(self.out, "{i}- Expected outcome: {}", prose(expected));
        }
        let _ = writeln!(
            self.out,
            "{i}- Reopen trigger: {}",
            or_not_recorded(text_at(body, "reopen_trigger"))
        );
        if let Some(check) = text_at(body, "check_when") {
            let _ = writeln!(self.out, "{i}- Check when: {}", prose(check));
        }
        if let Some(reason) = text_at(body, "change_reason") {
            let _ = writeln!(self.out, "{i}- Change reason: {}", prose(reason));
        }
        self.strongest_contradiction(i, record);
        self.review(i, record);
    }

    /// Supersession links to included records, appended to a standing.
    fn supersession(&self, detail: &mut String, record: &Record) {
        for (label, field) in [
            ("supersedes", "supersedes"),
            ("superseded by", "superseded_by"),
        ] {
            if let Some(reference) = refs(record.body.get(field)).first()
                && self.shown(reference.id).is_some()
            {
                let _ = write!(detail, "; {label} {}", code(&reference.id.to_string()));
            }
        }
    }

    /// The strongest included open objection on the record or its basis,
    /// critical first, else the first included contradicting record.
    fn strongest_contradiction(&mut self, indent: &str, record: &Record) {
        let mut open =
            objection_policy::relevant_open_objections(self.state(), record).unwrap_or_default();
        open.retain(|objection| self.shown(objection.objection_ref.id).is_some());
        open.sort_by_key(|objection| {
            (
                objection.severity != ObjectionSeverity::Critical,
                objection.objection_ref.id,
            )
        });
        if let Some(strongest) = open.first() {
            let objection = strongest.objection_ref;
            let mut line = format!(
                "{indent}- Strongest contradiction: open objection {} revision {}, severity {}",
                code(&objection.id.to_string()),
                objection.revision,
                code(&wire_word(&strongest.severity))
            );
            if strongest.subject_ref.id != record.header.id
                && self.shown(strongest.subject_ref.id).is_some()
            {
                let _ = write!(
                    line,
                    ", against {} revision {}",
                    code(&strongest.subject_ref.id.to_string()),
                    strongest.subject_ref.revision
                );
            }
            let _ = writeln!(self.out, "{line}: {}", prose(&strongest.statement));
            if let Some(objection) = self.shown(objection.id) {
                self.caveats(objection, &format!("{indent}  "));
            }
            if open.len() > 1 {
                let _ = writeln!(
                    self.out,
                    "{indent}  - {} more open objection{} listed on this record or its basis",
                    open.len() - 1,
                    if open.len() == 2 { "" } else { "s" }
                );
            }
            return;
        }
        let contradicting = refs(record.body.get("contradiction_refs"))
            .into_iter()
            .find(|reference| self.shown(reference.id).is_some());
        match contradicting.and_then(|reference| self.shown(reference.id)) {
            Some(other) => {
                let line = format!(
                    "{indent}- Strongest contradiction: the recorded alternative {}; {}",
                    self.name(other),
                    self.standing(other)
                );
                let _ = writeln!(self.out, "{line}");
                self.caveats(other, &format!("{indent}  "));
            }
            None => {
                let _ = writeln!(self.out, "{indent}- Strongest contradiction: none listed");
            }
        }
    }

    /// Recorded caveats, then confidence and likelihood, each on its own
    /// line: they are separate judgments and never share a sentence.
    fn uncertainty(&mut self, indent: &str, record: &Record) {
        let body = &record.body;
        let caveats = strings(body.get("caveats"));
        let confidence = value_at(body, "confidence").filter(|value| !value.is_null());
        let likelihood = value_at(body, "likelihood").filter(|value| !value.is_null());
        let uncertainty = text_at(body, "uncertainty");
        if caveats.is_empty()
            && confidence.is_none()
            && likelihood.is_none()
            && uncertainty.is_none()
        {
            let _ = writeln!(self.out, "{indent}- Uncertainty: none recorded");
            return;
        }
        let _ = writeln!(self.out, "{indent}- Uncertainty:");
        let inner = format!("{indent}  ");
        if let Some(text) = uncertainty {
            let _ = writeln!(self.out, "{inner}- Stated uncertainty: {}", prose(text));
        }
        for caveat in caveats.iter().take(MAX_ITEMS) {
            let _ = writeln!(self.out, "{inner}- Recorded caveat: {}", prose(caveat));
        }
        more(&mut self.out, &inner, caveats.len(), MAX_ITEMS);
        if let Some(confidence) = confidence {
            let level = confidence
                .get("level")
                .and_then(scalar_word)
                .map_or_else(|| "not recorded".to_owned(), |word| code(&token(&word)));
            let _ = writeln!(
                self.out,
                "{inner}- Confidence: {level}. Rationale: {}",
                or_not_recorded(text_at(confidence, "rationale"))
            );
            for limitation in strings(confidence.get("limitations"))
                .iter()
                .take(MAX_ITEMS)
            {
                let _ = writeln!(self.out, "{inner}  - Limitation: {}", prose(limitation));
            }
        }
        if let Some(likelihood) = likelihood {
            let estimate = likelihood
                .get("estimate")
                .and_then(scalar_word)
                .map_or_else(|| "not recorded".to_owned(), |word| code(&token(&word)));
            let _ = writeln!(self.out, "{inner}- Likelihood: {estimate}.");
            let _ = writeln!(
                self.out,
                "{inner}  - Resolved when: {}",
                or_not_recorded(text_at(likelihood, "resolution_condition"))
            );
        }
    }

    /// The validating review and its state, other included reviews, and
    /// included objections: dissent and review.
    fn review(&mut self, indent: &str, record: &Record) {
        let body = &record.body;
        let (label, none) = if record.header.id.kind() == Kind::Decision {
            (
                "Accepting review",
                "none accepted; the decision is not accepted",
            )
        } else {
            (
                "Validating review",
                "none accepted; the recorded claim is not validated",
            )
        };
        match refs(body.get("review_ref")).first() {
            Some(reference) => match self.shown(reference.id) {
                Some(review) => {
                    let line = format!(
                        "{indent}- {label}: {} revision {}, review state {}",
                        code(&reference.id.to_string()),
                        reference.revision,
                        code(freshness_word(self.input.notes, review))
                    );
                    let _ = writeln!(self.out, "{line}");
                    self.caveats(review, &format!("{indent}  "));
                    for caveat in strings(review.body.get("caveats")).iter().take(MAX_ITEMS) {
                        let _ =
                            writeln!(self.out, "{indent}  - Reviewer caveat: {}", prose(caveat));
                    }
                }
                None => {
                    let _ = writeln!(self.out, "{indent}- {label}: not listed");
                }
            },
            None => {
                let _ = writeln!(self.out, "{indent}- {label}: {none}");
            }
        }
        // The digest of the exact reviewed basis, when nothing of that basis
        // is withheld: a digest over withheld material is not shown.
        if let Some(basis) = text_at(body, "reviewed_basis")
            && self.input.caveats(record).is_empty()
        {
            let _ = writeln!(
                self.out,
                "{indent}- Reviewed basis digest: {}",
                code(&token(basis))
            );
        }
        let id = record.header.id;
        let validating = refs(body.get("review_ref")).first().map(|r| r.id);
        let reviews: Vec<&'a Record> = self
            .input
            .notes
            .naming_subject(Kind::Review, id)
            .iter()
            .copied()
            .filter(|review| Some(review.header.id) != validating && self.input.includes(review))
            .collect();
        let objections: Vec<&'a Record> = self
            .input
            .notes
            .naming_subject(Kind::Objection, id)
            .iter()
            .copied()
            .filter(|objection| self.input.includes(objection))
            .collect();
        if reviews.is_empty() && objections.is_empty() {
            let _ = writeln!(self.out, "{indent}- Dissent and other reviews: none listed");
            return;
        }
        let _ = writeln!(self.out, "{indent}- Dissent and other reviews:");
        let inner = format!("{indent}  ");
        for review in reviews.iter().take(MAX_ITEMS) {
            let detail = format!(
                "review of kind {}, state {}",
                code(&wire(text_at(&review.body, "kind").unwrap_or("unknown"))),
                code(freshness_word(self.input.notes, review))
            );
            self.item(&inner, review, &detail);
        }
        for objection in objections.iter().take(MAX_ITEMS) {
            let detail = format!(
                "objection, severity {}, {}: {}",
                code(&wire(
                    text_at(&objection.body, "severity").unwrap_or("unknown")
                )),
                self.standing(objection),
                or_not_recorded(text_at(&objection.body, "statement"))
            );
            self.item(&inner, objection, &detail);
        }
    }

    /// What would invalidate the claim: what would change its confidence,
    /// and the falsifiers of the included hypotheses it names as factors.
    fn invalidation(&mut self, indent: &str, record: &Record) {
        let body = &record.body;
        let mut items: Vec<String> = Vec::new();
        if let Some(change) = text_at(body, "confidence.what_would_change_it") {
            items.push(prose(change));
        }
        for factor in body
            .get("causal_factors")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(reference) = refs(factor.get("hypothesis_ref")).first().copied() else {
                continue;
            };
            let Some(hypothesis) = self.shown(reference.id) else {
                continue;
            };
            for falsifier in strings(hypothesis.body.get("possible_falsifiers")) {
                items.push(format!(
                    "{} (falsifier of {})",
                    prose(falsifier),
                    code(&reference.id.to_string())
                ));
            }
        }
        if items.is_empty() {
            let _ = writeln!(self.out, "{indent}- What would invalidate it: not recorded");
            return;
        }
        let _ = writeln!(self.out, "{indent}- What would invalidate it:");
        for item in items.iter().take(MAX_ITEMS) {
            let _ = writeln!(self.out, "{indent}  - {item}");
        }
        more(
            &mut self.out,
            &format!("{indent}  "),
            items.len(),
            MAX_ITEMS,
        );
    }

    /// The recorded causal account: factors with their roles and the causal
    /// test's baseline, treatment, reversal and failure boundary.
    pub fn causal(&mut self, indent: &str, record: &Record) {
        let body = &record.body;
        let factors: Vec<Value> = body["causal_factors"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let test = body.get("causal_test").filter(|test| !test.is_null());
        if factors.is_empty() && test.is_none() {
            return;
        }
        let _ = writeln!(self.out, "{indent}- Causal account:");
        let inner = format!("{indent}  ");
        for factor in factors.iter().take(MAX_ITEMS) {
            let role = code(&wire(text_at(factor, "role").unwrap_or("unknown")));
            let reference = refs(factor.get("hypothesis_ref")).first().copied();
            if let Some(reference) = reference
                && self.shown(reference.id).is_some()
            {
                self.cited(&inner, reference, &format!("; factor role {role}"));
            }
        }
        let Some(test) = test else {
            return;
        };
        let _ = writeln!(
            self.out,
            "{inner}- Causal test of variable {}:",
            code(text_at(test, "variable").unwrap_or("not recorded"))
        );
        let deeper = format!("{inner}  ");
        for (label, key) in [
            ("Baseline", "baseline"),
            ("Treatment", "treatment"),
            ("Reversal", "reversal"),
        ] {
            let Some(reference) = refs(test.get(key)).first().copied() else {
                continue;
            };
            let Some(cited) = self.shown(reference.id) else {
                continue;
            };
            let note = self.evidence_note(cited);
            self.labeled(&deeper, &format!("{label}: "), reference, &note);
        }
        if let Some(boundary) = test.get("failure_boundary").filter(|b| !b.is_null()) {
            let _ = writeln!(
                self.out,
                "{deeper}- Failure boundary: {} {} {}",
                code(&wire(text_at(boundary, "operator").unwrap_or("unknown"))),
                code(text_at(boundary, "value").unwrap_or_default()),
                code(text_at(boundary, "unit").unwrap_or_default())
            );
        }
    }

    // -------------------------------------------------------- Case framing

    /// `## Basis`: the snapshot, audience and renderer. The snapshot is the
    /// export's own: the bundle when IDs are remapped.
    pub fn basis(&mut self, kind: &str, version: &str) {
        let snapshot = self.input.snapshot();
        self.heading("Basis");
        self.out.push_str("| Basis | Value |\n|---|---|\n");
        let _ = writeln!(self.out, "| Kind | {} |", code(kind));
        let _ = writeln!(
            self.out,
            "| Case | {} |",
            code(&snapshot.case_id.to_string())
        );
        if self.input.remapped() {
            let _ = writeln!(
                self.out,
                "| Bundle | {} (IDs remapped; the publisher keeps the case snapshot) |",
                code(snapshot.head_hash.as_str())
            );
        } else {
            let _ = writeln!(
                self.out,
                "| Snapshot | sequence {}, head {} |",
                snapshot.sequence,
                code(snapshot.head_hash.as_str())
            );
        }
        let _ = writeln!(
            self.out,
            "| Audience | {} |",
            code(self.input.audience.as_str())
        );
        let _ = writeln!(self.out, "| Renderer | {} |", code(version));
    }

    /// `## Status and outcome`: the case's lifecycle, closure outcome and
    /// reopen condition. An inconclusive case says so in its own words.
    pub fn outcome(&mut self) {
        self.heading("Status and outcome");
        let Some(case) = self.case_record() else {
            self.line("- Not included for this audience.");
            return;
        };
        let body = &case.body;
        let status = wire(lifecycle(case).unwrap_or("not_recorded"));
        let _ = writeln!(self.out, "- Case status: {}", code(&status));
        self.caveats(case, "  ");
        let outcome = text_at(body, "closure_outcome").map(wire);
        match outcome.as_deref() {
            Some("inconclusive") => {
                self.line(
                    "- Outcome: `inconclusive`. This case is inconclusive: the recorded evidence \
                     did not settle its question, and nothing below is a settled answer.",
                );
            }
            Some(word) => {
                let _ = writeln!(self.out, "- Outcome: {}", code(word));
            }
            None if status == "closed" => self.line("- Outcome: not recorded"),
            None => {
                self.line("- Outcome: none yet. The case is not closed; what follows may change.")
            }
        }
        if status == "closed" {
            let _ = writeln!(
                self.out,
                "- Closure reason: {}",
                or_not_recorded(text_at(body, "transition_reason"))
            );
        }
        let closure = body.get("closure").filter(|value| !value.is_null());
        if let Some(closure) = closure {
            let _ = writeln!(
                self.out,
                "- Reopen condition: {}",
                or_not_recorded(text_at(closure, "reopen_condition"))
            );
            self.cited_list(
                "",
                "Unresolved at closure",
                closure.get("unresolved_questions"),
                |report, question| report.status(question),
            );
            if let Some(reason) = text_at(closure, "retrospective_waived") {
                let _ = writeln!(self.out, "- Retrospective waived: {}", prose(reason));
            }
        } else if outcome.is_some() {
            self.line("- Reopen condition: not recorded");
        }
    }

    /// The objective, decision relevance, scope and mode of the case.
    pub fn objective(&mut self) {
        let Some(case) = self.case_record() else {
            self.line("- Not included for this audience.");
            return;
        };
        let body = &case.body;
        let _ = writeln!(
            self.out,
            "- Objective: {}",
            or_not_recorded(text_at(body, "objective"))
        );
        if let Some(relevance) = text_at(body, "decision_relevance") {
            let _ = writeln!(self.out, "- Decision relevance: {}", prose(relevance));
        }
        if let Some(scope) = text_at(body, "scope") {
            let _ = writeln!(self.out, "- Scope: {}", prose(scope));
        }
        let _ = writeln!(
            self.out,
            "- Mode: {}",
            code(&wire(text_at(body, "mode").unwrap_or("not_recorded")))
        );
    }

    /// Each included question with its disposition and recorded answer.
    pub fn questions(&mut self) {
        let questions = self.records(&[Kind::Question]);
        if questions.is_empty() {
            self.line("- Questions: none listed");
            return;
        }
        for question in questions.iter().take(MAX_LISTED) {
            let detail = self.standing(question);
            self.item("", question, &detail);
            if let Some(answer) = text_at(&question.body, "answer") {
                let _ = writeln!(self.out, "  - Recorded answer: {}", prose(answer));
            }
            if let Some(context) = text_at(&question.body, "context") {
                let _ = writeln!(self.out, "  - Context: {}", prose(context));
            }
        }
        more(&mut self.out, "", questions.len(), MAX_LISTED);
    }

    /// Each included inquiry: purpose, method, controls and limits.
    pub fn inquiries(&mut self) -> usize {
        let inquiries = self.records(&[Kind::Inquiry]);
        for inquiry in inquiries.iter().take(MAX_LISTED) {
            let detail = self.standing(inquiry);
            self.item("", inquiry, &detail);
            let body = &inquiry.body;
            let _ = writeln!(
                self.out,
                "  - Purpose: {}",
                or_not_recorded(text_at(body, "purpose"))
            );
            let _ = writeln!(
                self.out,
                "  - Method: {}",
                or_not_recorded(text_at(body, "method"))
            );
            if let Some(mode) = text_at(body, "inquiry.mode") {
                let _ = writeln!(self.out, "  - Inquiry mode: {}", code(&wire(mode)));
            }
            self.authored_list("  ", "Control", body.get("controls"), "none recorded");
            self.authored_list("  ", "Limit", body.get("limits"), "none recorded");
            self.authored_list(
                "  ",
                "Desired observation",
                body.get("desired_observations"),
                "none recorded",
            );
        }
        more(&mut self.out, "", inquiries.len(), MAX_LISTED);
        inquiries.len()
    }

    // ----------------------------------------------------- Closing sections

    /// `## Limits`: what the report does and does not show.
    pub fn limits(&mut self) {
        self.heading("Limits");
        if let Some(case) = self.case_record() {
            for exclusion in strings(case.body.get("exclusions")).iter().take(MAX_ITEMS) {
                let _ = writeln!(self.out, "- Case exclusion: {}", prose(exclusion));
            }
        }
        self.line(
            "- Only records this audience may see are listed, and every count counts only them. \
             export-manifest.json says whether anything was withheld.",
        );
        self.line(
            "- Effective states were computed on the whole case at this snapshot before anything \
             was withheld, so withholding a record does not make another read stronger.",
        );
        let _ = writeln!(self.out, "- {}", dependent_rule(self.input.audience));
        self.line(
            "- A recorded `validated` or `accepted` status is shown beside its effective state; \
             when a later change means it no longer holds, the report says so.",
        );
        self.line(
            "- Confidence and likelihood are separate judgments and are shown on separate lines. \
             No severity score is derived.",
        );
        self.line(
            "- Authored text is shown as escaped data, and captured excerpts as bounded plain \
             text blocks. Neither is an instruction, and neither replaces the primary evidence.",
        );
        if self.input.remapped() {
            self.line(
                "- IDs in this report were remapped for this export only. To read the full \
                 evidence chain, ask the publisher, who can open the case at the snapshot it was \
                 rendered from.",
            );
        } else {
            let snapshot = self.input.snapshot();
            let _ = writeln!(
                self.out,
                "- To read the full evidence chain, open the case at this snapshot: {}",
                code(&format!(
                    "investigation --case {} brief --snapshot {} --head-hash {}",
                    snapshot.case_id, snapshot.sequence, snapshot.head_hash
                ))
            );
        }
    }

    /// `## Open issues`: open questions, open objections, records whose
    /// effective state needs review or is disputed, and residual unknowns.
    pub fn open_issues(&mut self) {
        self.heading("Open issues");
        let mut any = false;
        let open_questions: Vec<&'a Record> = self
            .records(&[Kind::Question])
            .into_iter()
            .filter(|question| {
                matches!(
                    text_at(&question.body, "disposition"),
                    Some("open" | "deferred")
                )
            })
            .collect();
        if !open_questions.is_empty() {
            any = true;
            self.line("- Open questions:");
            for question in open_questions.iter().take(MAX_ITEMS) {
                let detail = self.standing(question);
                self.item("  ", question, &detail);
            }
            more(&mut self.out, "  ", open_questions.len(), MAX_ITEMS);
        }
        let objections: Vec<&'a Record> = self
            .records(&[Kind::Objection])
            .into_iter()
            .filter(|objection| lifecycle(objection) == Some("open"))
            .collect();
        if !objections.is_empty() {
            any = true;
            self.line("- Open objections:");
            for objection in objections.iter().take(MAX_ITEMS) {
                let detail = format!(
                    "severity {}: {}",
                    code(&wire(
                        text_at(&objection.body, "severity").unwrap_or("unknown")
                    )),
                    or_not_recorded(text_at(&objection.body, "statement"))
                );
                self.item("  ", objection, &detail);
            }
            more(&mut self.out, "  ", objections.len(), MAX_ITEMS);
        }
        let unsettled: Vec<&'a Record> = self
            .records(&[
                Kind::Conclusion,
                Kind::Decision,
                Kind::Assessment,
                Kind::Comparison,
            ])
            .into_iter()
            .filter(|record| self.unsettled(record))
            .collect();
        if !unsettled.is_empty() {
            any = true;
            self.line("- Needing review, disputed or conditional:");
            for record in unsettled.iter().take(MAX_ITEMS) {
                let detail = self.standing(record);
                self.item("  ", record, &detail);
            }
            more(&mut self.out, "  ", unsettled.len(), MAX_ITEMS);
        }
        for retrospective in self.current_retrospectives() {
            let unknowns = strings(retrospective.body.get("residual_unknowns"));
            if unknowns.is_empty() {
                continue;
            }
            any = true;
            let _ = writeln!(
                self.out,
                "- Residual unknowns recorded by {}:",
                code(&retrospective.header.id.to_string())
            );
            self.caveats(retrospective, "  ");
            for unknown in unknowns.iter().take(MAX_ITEMS) {
                let _ = writeln!(self.out, "  - {}", prose(unknown));
            }
            more(&mut self.out, "  ", unknowns.len(), MAX_ITEMS);
        }
        if !any {
            self.line("- None listed.");
        }
    }

    fn current_retrospectives(&self) -> Vec<&'a Record> {
        self.records(&[Kind::Retrospective])
            .into_iter()
            .filter(|record| lifecycle(record) != Some("superseded"))
            .collect()
    }

    /// `## Methodological retrospective`: an exact reference to each
    /// included account, and bounded highlights when it is substantive.
    pub fn retrospective(&mut self) {
        self.heading("Methodological retrospective");
        if let Some(reason) = self
            .case_record()
            .and_then(|case| text_at(&case.body, "closure.retrospective_waived"))
        {
            let _ = writeln!(self.out, "- Waived at closure: {}", prose(reason));
        }
        let retrospectives = self.records(&[Kind::Retrospective]);
        if retrospectives.is_empty() {
            self.line("- No retrospective listed.");
            return;
        }
        for retrospective in retrospectives.iter().take(MAX_LISTED) {
            let body = &retrospective.body;
            let mut detail = format!(
                "revision {}, account state {}",
                retrospective.header.revision.0,
                code(freshness_word(self.input.notes, retrospective)),
            );
            // A remapped export discloses no journal sequence or head.
            if !self.input.remapped() {
                let _ = write!(
                    detail,
                    ", describes the case at sequence {}, head {}",
                    body["snapshot"]["sequence"].as_u64().unwrap_or_default(),
                    code(body["snapshot"]["head_hash"].as_str().unwrap_or("unknown"))
                );
            }
            if let Some(successor) = refs(body.get("successor_ref")).first()
                && self.shown(successor.id).is_some()
            {
                let _ = write!(
                    detail,
                    "; superseded by {}",
                    code(&successor.id.to_string())
                );
            }
            self.item("", retrospective, &detail);
            let substantive = [
                "effective_steps",
                "invalid_or_misleading_steps",
                "residual_unknowns",
                "lessons",
            ]
            .iter()
            .any(|key| body[*key].as_array().is_some_and(|items| !items.is_empty()));
            if !substantive {
                self.line(
                    "  - No highlight: the account records no effective or misleading step, \
                     residual unknown or lesson.",
                );
                continue;
            }
            self.highlights(retrospective);
        }
        more(&mut self.out, "", retrospectives.len(), MAX_LISTED);
    }

    fn highlights(&mut self, retrospective: &Record) {
        let body = &retrospective.body;
        let methods: Vec<&str> = body["methods_attempted"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|method| text_at(method, "name"))
            .collect();
        for method in methods.iter().take(MAX_HIGHLIGHTS) {
            let _ = writeln!(self.out, "  - Method attempted: {}", prose(method));
        }
        more(&mut self.out, "  ", methods.len(), MAX_HIGHLIGHTS);
        for (label, key) in [
            ("Effective step", "effective_steps"),
            ("Invalid or misleading step", "invalid_or_misleading_steps"),
        ] {
            let steps: Vec<&Value> = body[key].as_array().into_iter().flatten().collect();
            for step in steps.iter().take(MAX_HIGHLIGHTS) {
                let _ = writeln!(
                    self.out,
                    "  - {label}: {} (support {})",
                    or_not_recorded(text_at(step, "description")),
                    self.id_list(step.get("support_refs"))
                );
            }
            more(&mut self.out, "  ", steps.len(), MAX_HIGHLIGHTS);
        }
        let unknowns = strings(body.get("residual_unknowns"));
        for unknown in unknowns.iter().take(MAX_HIGHLIGHTS) {
            let _ = writeln!(self.out, "  - Residual unknown: {}", prose(unknown));
        }
        more(&mut self.out, "  ", unknowns.len(), MAX_HIGHLIGHTS);
        let lessons: Vec<&Value> = body["lessons"].as_array().into_iter().flatten().collect();
        if lessons.is_empty() {
            let _ = writeln!(
                self.out,
                "  - No reusable lesson: {}",
                or_not_recorded(text_at(body, "no_reusable_lesson_reason"))
            );
        }
        for lesson in lessons.iter().take(MAX_HIGHLIGHTS) {
            let _ = writeln!(
                self.out,
                "  - Proposed lesson: {} (support {})\n    - Applies when: {}\n    - Transfer limits: {}",
                or_not_recorded(text_at(lesson, "guidance")),
                self.id_list(lesson.get("support_refs")),
                or_not_recorded(text_at(lesson, "applies_when")),
                or_not_recorded(text_at(lesson, "transfer_limits"))
            );
        }
        more(&mut self.out, "  ", lessons.len(), MAX_HIGHLIGHTS);
        if !lessons.is_empty() {
            self.line(
                "  - A lesson is a bounded proposal with stated transfer limits; recording one \
                 changes no method pack and no other case.",
            );
        }
    }
}

/// The recorded measurement fields as lines, in a fixed order. Nothing is
/// derived from another field.
fn field_lines(fields: &MeasurementFields) -> Vec<String> {
    let mut lines = Vec::new();
    let text = |value: &String| prose(value);
    let quantity = |value: &measurement_fields::Quantity| {
        format!("{} {}", code(value.value.as_str()), code(&value.unit))
    };
    if let Some(unit) = &fields.unit {
        lines.push(format!("Unit: {}", knowledge(unit, |unit| code(unit))));
    }
    if let Some(workload) = &fields.workload {
        lines.push(format!("Workload: {}", knowledge(workload, text)));
    }
    if let Some(window) = &fields.observation_window {
        lines.push(format!(
            "Observation window: {}",
            knowledge(window, quantity)
        ));
    }
    if let Some(warmup) = &fields.warmup {
        lines.push(format!("Warm-up: {}", knowledge(warmup, quantity)));
    }
    if let Some(count) = &fields.sample_count {
        lines.push(format!(
            "Sample count, as stated: {}",
            knowledge(count, |count| code(&count.to_string()))
        ));
    }
    if let Some(samples) = &fields.samples {
        let (shape, count, unit) = match samples {
            SampleSet::Flat { unit, values } => ("flat", values.len(), unit),
            SampleSet::Leveled { unit, samples } => ("leveled", samples.len(), unit),
        };
        lines.push(format!(
            "Raw samples: {} {} values in {}",
            code(&count.to_string()),
            code(shape),
            code(unit)
        ));
    }
    if let Some(statistic) = &fields.statistic {
        lines.push(format!(
            "Statistic: {}",
            knowledge(statistic, |statistic| {
                let mut text = code(&wire_word(&statistic.kind));
                if let Some(quantile) = &statistic.quantile {
                    let _ = write!(text, " at {}", code(quantile.as_str()));
                }
                if let Some(population) = &statistic.population {
                    let _ = write!(
                        text,
                        ", population {}",
                        knowledge(population, |word| code(&wire_word(word)))
                    );
                }
                if let Some(combination) = &statistic.combination {
                    let _ = write!(
                        text,
                        ", combination {}",
                        knowledge(combination, |word| code(&wire_word(word)))
                    );
                }
                text
            })
        ));
    }
    if let Some(order) = &fields.trial_order {
        lines.push(format!(
            "Trial order: {}",
            knowledge(order, |word| code(&wire_word(word)))
        ));
    }
    if let Some(model) = &fields.load_model {
        lines.push(format!(
            "Load model: {}",
            knowledge(model, |model| format!(
                "loop {}, timed from intended send {}",
                knowledge(&model.load_loop, |word| code(&wire_word(word))),
                knowledge(&model.timed_from_intended_send, |flag| code(
                    &flag.to_string()
                ))
            ))
        ));
    }
    if let Some(attempts) = &fields.attempts {
        for (name, count) in attempts.entries() {
            lines.push(format!(
                "Attempts {}: {} ({})",
                code(name),
                knowledge(&count.count, |count| code(&count.to_string())),
                knowledge(&count.definition, text)
            ));
        }
    }
    for (label, identity) in [
        ("Environment identity", &fields.environment_identity),
        ("Tool identity", &fields.tool_identity),
    ] {
        if let Some(identity) = identity {
            lines.push(format!(
                "{label}: {}",
                knowledge(identity, |components| {
                    let shown: Vec<String> = components
                        .iter()
                        .take(MAX_ITEMS)
                        .map(|(name, value)| {
                            format!("{} {}", code(name), knowledge(value, |value| code(value)))
                        })
                        .collect();
                    if shown.is_empty() {
                        "none named".to_owned()
                    } else {
                        shown.join(", ")
                    }
                })
            ));
        }
    }
    if let Some(changed) = &fields.instrumentation_changed {
        lines.push(format!(
            "Instrumentation changed: {}",
            knowledge(changed, |flag| code(&flag.to_string()))
        ));
    }
    for limitation in fields.limitations.iter().take(MAX_ITEMS) {
        lines.push(format!("Limitation: {}", prose(limitation)));
    }
    if lines.is_empty() {
        lines.push("none recorded".to_owned());
    }
    lines
}

/// The wire word of a measurement validity.
pub(super) const fn validity_word(validity: MeasurementValidity) -> &'static str {
    match validity {
        MeasurementValidity::Unknown => "unknown",
        MeasurementValidity::Valid => "valid",
        MeasurementValidity::Invalid => "invalid",
        MeasurementValidity::Partial => "partial",
    }
}
