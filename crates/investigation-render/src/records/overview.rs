//! The case Overview: the bounded opening summary, the title/synopsis/full
//! ladder, the case's own reviews and one section chosen by what the case
//! contains. Pure.
//!
//! The Overview tells the same bounded story as `brief` for the same
//! snapshot. Its answer is the newest live Conclusion or Decision, its caveat
//! and contradiction follow the same order of precedence as the brief's
//! (unavailable evidence, a changed basis, unresolved conditions, open
//! objections, then the first recorded caveat), and its freshness word is
//! the brief's: `stale` when the case or any record still in evidentiary use
//! needs review. Retained-byte integrity is checked by `brief`; the planner
//! never reads retained bytes, so it reports recorded availability only.
//!
//! Every element of the opening sentence comes from typed fields. An absent
//! element is stated as absent, never left blank or invented. The strongest
//! outstanding objection comes before any count. An authored synopsis is
//! editorial data: it is shown only while current, quoted and labelled, and
//! never as corroboration; a stale one falls back to the projection of typed
//! fields and says so.

use std::{collections::BTreeSet, fmt::Write as _};

use investigation_core::{
    PROGRAM_NAME,
    classification::Field,
    command::RevisionRef,
    freshness::{Freshness, dependency_edges},
    gap_rules::enumerate_gaps,
    identity::{Id, Kind},
    next_rank::{NextResponse, rank_next},
    objection_policy::{self, OpenObjection},
    review::ObjectionSeverity,
    review_ops,
    state::{Record, State},
    synopsis::{self, SynopsisState},
};
use serde_json::Value;

use super::{criterion::threshold, effective_word, lifecycle, refs, status_note};
use crate::{
    RenderError,
    evidence::{MAX_LISTED, prose, revision_link},
    note::{NoteContext, text_at, wire_word},
    text::inline_code,
    yaml::Frontmatter,
};

/// Freshness reasons shown for a stale synopsis.
const SHOWN_REASONS: usize = 3;

// The caveat and contradiction reasons `brief` reports, word for word, so the
// two reads can be compared for the same snapshot.
const EXTERNAL_BYTES: &str = "The cited bytes are held at an external location and are not retained here (source_unavailable). Selectors on them cannot be verified.";
const MISSING_BYTES: &str = "Expected retained bytes are missing or fail integrity verification.";
const BASIS_CHANGED: &str = "The exact review or evidence basis has changed.";
const CONDITIONS_OPEN: &str = "Material conditions remain unresolved.";
const HARD_CRITERIA_OPEN: &str = "The proposed choice depends on unresolved hard criteria.";
const OBJECTIONS_OPEN: &str = "Open objections apply to the record or its basis.";
const DISPUTED: &str = "An open critical objection applies to the record or its basis, or an objection was raised after its review.";

/// The concise section the Overview shows after the ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    Explanation,
    Measurement,
    Evaluation,
    Research,
}

impl Focus {
    const fn heading(self) -> &'static str {
        match self {
            Self::Explanation => "Explanation",
            Self::Measurement => "Measurement",
            Self::Evaluation => "Evaluation",
            Self::Research => "Research",
        }
    }

    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Explanation => "explanation",
            Self::Measurement => "measurement",
            Self::Evaluation => "evaluation",
            Self::Research => "research",
        }
    }

    /// Whether the case holds records this section is about.
    fn has_content(self, state: &State) -> bool {
        state.records().any(|record| match self {
            Self::Explanation => record.header.id.kind() == Kind::Hypothesis,
            Self::Evaluation => matches!(
                record.header.id.kind(),
                Kind::Option | Kind::Criterion | Kind::Decision
            ),
            Self::Measurement => record.header.id.kind() == Kind::Run || is_measurement(record),
            Self::Research => record.header.id.kind() == Kind::Source,
        })
    }
}

fn is_measurement(record: &Record) -> bool {
    record.header.id.kind() == Kind::Observation
        && text_at(&record.body, "value.kind") == Some("measurement")
}

/// The section for a case: the one its mode names when the case holds
/// records for it, otherwise the first that the case holds records for, so
/// a comparison case never shows an empty explanation. A case with none of
/// them gets its mode's section, which states what is absent.
pub(crate) fn focus(case: &Record, state: &State) -> Focus {
    let by_mode = match text_at(&case.body, "mode") {
        Some("explain") => Some(Focus::Explanation),
        Some("measure") => Some(Focus::Measurement),
        Some("evaluate") => Some(Focus::Evaluation),
        _ => None,
    };
    if let Some(focus) = by_mode
        && focus.has_content(state)
    {
        return focus;
    }
    [
        Focus::Evaluation,
        Focus::Explanation,
        Focus::Measurement,
        Focus::Research,
    ]
    .into_iter()
    .find(|focus| focus.has_content(state))
    .or(by_mode)
    .unwrap_or(Focus::Research)
}

fn is_live(record: &Record) -> bool {
    record.body["lifecycle"] != "superseded" && record.body["status"] != "superseded"
}

/// The outcome whose answer, caveat and contradiction lead the Overview, as
/// in `brief`: the newest Conclusion or Decision that is not superseded.
pub(crate) fn primary_outcome(state: &State) -> Option<&Record> {
    state
        .records()
        .filter(|record| {
            matches!(record.header.id.kind(), Kind::Conclusion | Kind::Decision) && is_live(record)
        })
        .max_by_key(|record| (record.header.acceptance_sequence, record.header.id))
}

/// The case's freshness word as `brief` reports it: `stale` when the case
/// record or any record still in evidentiary use needs review.
pub(crate) fn case_freshness(notes: &NoteContext<'_>, case: &Record) -> &'static str {
    let state = notes.case().state();
    let index = notes.freshness();
    if index.assess(state, case).needs_review() || index.overview().needs_review > 0 {
        "stale"
    } else {
        "current"
    }
}

/// Exact revisions an outcome rests on: its promotion closure, or the
/// revisions it cites directly when the closure cannot be walked.
fn read_set(state: &State, record: &Record) -> Vec<RevisionRef> {
    review_ops::required_read_set(state, record).unwrap_or_else(|_| {
        dependency_edges(record)
            .unwrap_or_default()
            .into_iter()
            .map(|edge| edge.target)
            .collect()
    })
}

/// Artifacts in a read set whose recorded state says their bytes are not
/// retained here. Whether retained bytes still verify is checked by `brief`.
fn recorded_unavailable(state: &State, read: &[RevisionRef]) -> Vec<RevisionRef> {
    read.iter()
        .copied()
        .filter(|reference| {
            reference.id.kind() == Kind::Artifact
                && state.revision(*reference).is_none_or(|artifact| {
                    let body = &artifact.body;
                    body["availability"] != "retained"
                        || body["digest"].is_null()
                        || body["size_bytes"].is_null()
                })
        })
        .collect()
}

/// Everything the opening summary and the answer rung say about the leading
/// outcome, computed once.
struct Standing<'a> {
    outcome: &'a Record,
    /// The brief's effective status word.
    state: String,
    read: Vec<RevisionRef>,
    unavailable: Vec<RevisionRef>,
    /// Open objections on the outcome or its basis, critical first.
    open: Vec<OpenObjection>,
}

impl<'a> Standing<'a> {
    fn new(notes: &NoteContext<'a>, outcome: &'a Record) -> Self {
        let state = notes.case().state();
        let read = read_set(state, outcome);
        let unavailable = recorded_unavailable(state, &read);
        let mut word = effective_word(notes, outcome).unwrap_or_else(|| "recorded".to_owned());
        if !unavailable.is_empty() && word != "superseded" && word != "retracted" {
            word = "needs_review".to_owned();
        }
        let mut open =
            objection_policy::relevant_open_objections(state, outcome).unwrap_or_default();
        sort_objections(&mut open);
        Self {
            outcome,
            state: word,
            read,
            unavailable,
            open,
        }
    }
}

fn sort_objections(open: &mut [OpenObjection]) {
    open.sort_by_key(|objection| {
        (
            objection.severity != ObjectionSeverity::Critical,
            objection.objection_ref.id,
        )
    });
}

/// Everything the Overview reads, computed once per case.
struct Overview<'n, 'a> {
    notes: &'n NoteContext<'a>,
    case: &'a Record,
    state: &'a State,
    focus: Focus,
    standing: Option<Standing<'a>>,
    next: Option<NextResponse>,
}

/// `inv_mode`, `inv_focus` and `inv_summary` on a visible case Overview.
pub(crate) fn properties(notes: &NoteContext<'_>, case: &Record, front: &mut Frontmatter) {
    let state = notes.case().state();
    if let Some(mode) = text_at(&case.body, "mode") {
        front.text("inv_mode", &super::wire(mode));
    }
    front.text("inv_focus", focus(case, state).word());
    let subject = primary_outcome(state).unwrap_or(case);
    front.text("inv_summary", summary_word(notes, subject).0);
}

/// The case Overview body after the shared opening: every section of the
/// ladder, the case's reviews, the focus section and resumption commands.
pub(crate) fn sections(
    notes: &NoteContext<'_>,
    out: &mut String,
    case: &Record,
) -> Result<(), RenderError> {
    let state = notes.case().state();
    if !notes.visible(case) {
        out.push_str("\n## Opening summary\n\nWithheld.\n");
        return resume(notes, out);
    }
    let next = enumerate_gaps(state, notes.case().head(), notes.case().case_id())
        .ok()
        .map(|inventory| rank_next(state, &inventory, 3));
    let overview = Overview {
        notes,
        case,
        state,
        focus: focus(case, state),
        standing: primary_outcome(state).map(|outcome| Standing::new(notes, outcome)),
        next,
    };
    overview.opening(out)?;
    overview.answer(out)?;
    overview.closure(out)?;
    overview.evidence(out)?;
    overview.sources(out)?;
    overview.reviews(out)?;
    overview.focus_section(out)?;
    resume(notes, out)
}

/// `summary_current` or `summary_stale` for a subject, with the newest
/// visible authored synopsis when there is one, and the number withheld.
fn summary_word<'a>(
    notes: &NoteContext<'a>,
    subject: &Record,
) -> (
    &'static str,
    Option<(synopsis::SynopsisStatus, &'a Record)>,
    usize,
) {
    let state = notes.case().state();
    let mut withheld = 0;
    for status in synopsis::for_subject(state, notes.freshness(), subject.header.id) {
        let Some(annotation) = state.revision(status.annotation_ref) else {
            continue;
        };
        if !notes.may_show(annotation, Field::Content) {
            withheld += 1;
            continue;
        }
        let word = if status.state == SynopsisState::Current {
            "summary_current"
        } else {
            "summary_stale"
        };
        return (word, Some((status, annotation)), withheld);
    }
    ("summary_current", None, withheld)
}

impl<'a> Overview<'_, 'a> {
    fn link(&self, id: Id) -> Result<String, RenderError> {
        self.notes.link(id)
    }

    /// Whether the leading outcome's state and basis may appear; true when
    /// there is none. A withheld outcome's caveat, objections and evidence
    /// would reveal its lineage.
    fn outcome_visible(&self) -> bool {
        self.standing.as_ref().is_none_or(|standing| {
            self.notes.visible(standing.outcome)
                && self.notes.may_show(standing.outcome, Field::Lineage)
        })
    }

    /// The root question: the one the leading outcome answers, otherwise
    /// the earliest recorded.
    fn question(&self) -> Option<Id> {
        if let Some(standing) = &self.standing
            && let Some(reference) = refs(standing.outcome.body.get("question_ref")).first()
        {
            return Some(reference.id);
        }
        self.state
            .records()
            .filter(|record| record.header.id.kind() == Kind::Question)
            .min_by_key(|record| (record.header.acceptance_sequence, record.header.id))
            .map(|record| record.header.id)
    }

    fn opening(&self, out: &mut String) -> Result<(), RenderError> {
        let body = &self.case.body;
        let question = match self.question() {
            Some(id) => self.link(id)?,
            None => "(no question recorded yet)".to_owned(),
        };
        let decision = text_at(body, "decision_relevance")
            .filter(|text| !text.trim().is_empty())
            .map_or_else(|| "(no dependent decision recorded)".to_owned(), prose);
        let (noun, answer) = match &self.standing {
            Some(standing) => (
                if standing.outcome.header.id.kind() == Kind::Decision {
                    "choice"
                } else {
                    "answer"
                },
                if self.outcome_visible() {
                    format!(
                        "{} ({})",
                        self.link(standing.outcome.header.id)?,
                        inline_code(&standing.state)
                    )
                } else {
                    format!("{} (withheld)", self.link(standing.outcome.header.id)?)
                },
            ),
            None if self.focus == Focus::Evaluation => ("choice", "(no decision yet)".to_owned()),
            None => ("answer", "(no conclusion yet)".to_owned()),
        };
        let scope = match body.get("scope") {
            Some(value) if value["state"] == "unknown" => "(scope recorded as unknown)".to_owned(),
            _ => text_at(body, "scope")
                .filter(|text| !text.trim().is_empty())
                .map_or_else(|| "(no scope stated)".to_owned(), prose),
        };
        let evidence = self.strongest_evidence()?;
        let objection = self.strongest_objection()?;
        let next = self.next_information()?;
        out.push_str("\n## Opening summary\n\n");
        let _ = writeln!(
            out,
            "We are investigating {question} because it informs {decision}. Our current {noun} is \
             {answer}, within scope {scope}. The strongest original evidence is {evidence}, the \
             strongest outstanding objection is {objection}, and the most useful next information \
             is {next}."
        );
        Ok(())
    }

    /// The strongest outcome-cited Observation: current before stale, valid
    /// before partial or unknown, read from an original acquisition before
    /// a derivative or an assertion, earliest first. Without an outcome, the
    /// same ranking over every Observation in the case.
    fn strongest_evidence(&self) -> Result<String, RenderError> {
        if !self.outcome_visible() {
            return Ok("(withheld with the answer)".to_owned());
        }
        let candidates: Vec<&Record> = match &self.standing {
            Some(standing) => standing
                .read
                .iter()
                .filter(|reference| reference.id.kind() == Kind::Observation)
                .filter_map(|reference| self.state.current(reference.id))
                .collect(),
            None => self
                .state
                .records()
                .filter(|record| record.header.id.kind() == Kind::Observation)
                .collect(),
        };
        let visible: Vec<&Record> = candidates
            .iter()
            .copied()
            .filter(|record| self.notes.visible(record))
            .collect();
        let best = visible.into_iter().min_by_key(|record| {
            let fresh = self
                .notes
                .effective(record)
                .is_none_or(|effective| effective.freshness == Freshness::Current);
            let validity = match text_at(&record.body, "validity") {
                Some("valid") => 0,
                Some("partial") => 1,
                Some("unknown") | None => 2,
                _ => 3,
            };
            (
                !fresh,
                validity,
                self.origin(record).is_none(),
                record.header.acceptance_sequence,
                record.header.id,
            )
        });
        let Some(best) = best else {
            return Ok(match candidates.first() {
                Some(withheld) => format!("{} (withheld)", self.link(withheld.header.id)?),
                None if self.standing.is_some() => "(no observation cited)".to_owned(),
                None => "(no observation recorded yet)".to_owned(),
            });
        };
        let mut text = format!(
            "{} (validity {}",
            self.link(best.header.id)?,
            inline_code(&super::wire(
                text_at(&best.body, "validity").unwrap_or("unknown")
            ))
        );
        match self.origin(best) {
            Some(artifact) => {
                let _ = write!(
                    text,
                    ", from the original acquisition {}",
                    self.link(artifact)?
                );
            }
            None => text.push_str(", not read from an original acquisition"),
        }
        text.push(')');
        Ok(text)
    }

    /// The original Artifact an Observation was read from, if it was.
    fn origin(&self, observation: &Record) -> Option<Id> {
        let reference = refs(observation.body["basis"].get("artifact_ref"))
            .first()
            .copied()?;
        let artifact = self.state.revision(reference)?;
        is_original(artifact).then_some(reference.id)
    }

    /// The strongest open objection on the leading outcome or its basis, or
    /// on any record of the case when there is no outcome yet.
    fn strongest_objection(&self) -> Result<String, RenderError> {
        if !self.outcome_visible() {
            return Ok("withheld with the answer".to_owned());
        }
        let open = match &self.standing {
            Some(standing) => standing.open.clone(),
            None => {
                let subjects: BTreeSet<Id> = self
                    .state
                    .records()
                    .map(|record| record.header.id)
                    .collect();
                let mut open = objection_policy::open_objections(
                    self.state,
                    self.notes.case().case_id(),
                    &subjects,
                )
                .unwrap_or_default();
                sort_objections(&mut open);
                open
            }
        };
        let Some(strongest) = open.first() else {
            return Ok(if self.standing.is_some() {
                "none open on the answer or its basis".to_owned()
            } else {
                "none open".to_owned()
            });
        };
        let mut text = self.link(strongest.objection_ref.id)?;
        if self
            .state
            .current(strongest.objection_ref.id)
            .is_some_and(|objection| self.notes.visible(objection))
        {
            let _ = write!(
                text,
                " ({}, against {})",
                inline_code(&wire_word(&strongest.severity)),
                self.link(strongest.subject_ref.id)?
            );
        }
        Ok(text)
    }

    /// The first ranked recommendation of `next`.
    fn next_information(&self) -> Result<String, RenderError> {
        let Some(next) = &self.next else {
            return Ok("(unknown: the gap inventory is unavailable)".to_owned());
        };
        let Some(first) = next.recommendations.first() else {
            return Ok("(no ranked next step)".to_owned());
        };
        let mut text = if self.notes.gap_withheld(&first.gap_id) {
            format!(
                "what `next` ranks first: rule {}, its details withheld by the view policy",
                inline_code(&first.rule_id)
            )
        } else {
            format!(
                "what `next` ranks first: {}",
                prose(first.purpose.trim_end_matches('.'))
            )
        };
        let subjects: Vec<String> = first
            .subject_refs
            .iter()
            .take(3)
            .map(|reference| self.link(reference.id))
            .collect::<Result<_, _>>()?;
        if !subjects.is_empty() {
            let _ = write!(text, " (on {})", subjects.join(", "));
        }
        Ok(text)
    }

    // ------------------------------------------------------------ title rung

    fn answer(&self, out: &mut String) -> Result<(), RenderError> {
        out.push_str("\n## Answer and caveat\n\n");
        match &self.standing {
            Some(standing) => self.standing_lines(out, standing)?,
            None => {
                let what = if self.focus == Focus::Evaluation {
                    "No decision is recorded yet. Options and comparisons are below; a case \
                     still comparing options has not failed."
                } else {
                    "No conclusion is recorded yet."
                };
                let _ = writeln!(out, "- Answer: {what}");
                let _ = writeln!(
                    out,
                    "- Strongest outstanding objection: {}",
                    self.strongest_objection()?
                );
            }
        }
        let overview = self.notes.freshness().overview();
        let in_use = overview.current + overview.needs_review + overview.disputed;
        let _ = writeln!(
            out,
            "- Freshness: {} (effective {}; of {in_use} record{} in evidentiary use, {} need{} \
             review and {} {} disputed)",
            inline_code(case_freshness(self.notes, self.case)),
            inline_code(overview.freshness.as_str()),
            if in_use == 1 { "" } else { "s" },
            overview.needs_review,
            if overview.needs_review == 1 { "s" } else { "" },
            overview.disputed,
            if overview.disputed == 1 { "is" } else { "are" },
        );
        let subject = self
            .standing
            .as_ref()
            .map_or(self.case, |standing| standing.outcome);
        self.synopsis(out, subject)
    }

    /// How a closed case ended and what should reopen it (§6.6): the outcome,
    /// its reason, the questions left open, the reopen condition and the
    /// method account. Only a closed case has this section.
    fn closure(&self, out: &mut String) -> Result<(), RenderError> {
        let body = &self.case.body;
        let Some(closure) = body.get("closure").filter(|closure| closure.is_object()) else {
            return Ok(());
        };
        out.push_str("\n## Closure\n\n");
        let outcome = text_at(body, "closure_outcome").unwrap_or("unknown");
        let _ = writeln!(out, "- Outcome: {}", inline_code(&super::wire(outcome)));
        if !self.notes.may_show(self.case, Field::Content) {
            out.push_str(
                "- Reason, open questions and reopen condition: withheld by the view policy\n",
            );
            return Ok(());
        }
        let authored = |path: &str| {
            text_at(closure, path)
                .or_else(|| text_at(body, path))
                .filter(|text| !text.trim().is_empty())
        };
        let _ = writeln!(
            out,
            "- Reason: {}",
            authored("transition_reason").map_or_else(|| "not recorded".to_owned(), prose)
        );
        let open = refs(closure.get("unresolved_questions"));
        let _ = writeln!(
            out,
            "- Unresolved questions: {}",
            if open.is_empty() {
                "none listed".to_owned()
            } else {
                self.links(&open)?
            }
        );
        let reopen = match authored("reopen_condition") {
            Some(condition) => prose(condition),
            None if outcome == "inconclusive" => {
                "none recorded; an inconclusive close without one states no way back".to_owned()
            }
            None => "none recorded".to_owned(),
        };
        let _ = writeln!(out, "- Reopen condition: {reopen}");
        let account = match (
            refs(closure.get("retrospective_ref")).first(),
            authored("retrospective_waived"),
        ) {
            (Some(reference), _) => revision_link(self.notes, *reference)?,
            (None, Some(waiver)) => format!("waived: {}", prose(waiver)),
            (None, None) => "none recorded".to_owned(),
        };
        let _ = writeln!(out, "- Retrospective: {account}");
        Ok(())
    }

    fn standing_lines(&self, out: &mut String, standing: &Standing<'_>) -> Result<(), RenderError> {
        let outcome = standing.outcome;
        if !self.outcome_visible() {
            let _ = writeln!(
                out,
                "- Answer: {} revision {}; its state, caveat, objections and basis are withheld \
                 by the view policy",
                self.link(outcome.header.id)?,
                outcome.header.revision.0
            );
            return Ok(());
        }
        let recorded = lifecycle(outcome).map_or_else(|| "recorded".to_owned(), super::wire);
        let _ = writeln!(
            out,
            "- Answer: {} revision {}, effective {} (recorded lifecycle {})",
            self.link(outcome.header.id)?,
            outcome.header.revision.0,
            inline_code(&standing.state),
            inline_code(&recorded)
        );
        let _ = writeln!(
            out,
            "- Strongest outstanding objection: {}",
            self.strongest_objection()?
        );
        if standing.open.len() > 1 {
            let _ = writeln!(
                out,
                "  - {} more open objection{} on the answer or its basis",
                standing.open.len() - 1,
                if standing.open.len() == 2 { "" } else { "s" }
            );
        }
        let _ = writeln!(out, "- Material caveat: {}", self.caveat(standing)?);
        let _ = writeln!(out, "- Contradiction: {}", self.contradiction(standing)?);
        Ok(())
    }

    fn links(&self, references: &[RevisionRef]) -> Result<String, RenderError> {
        let links: Vec<String> = references
            .iter()
            .take(MAX_LISTED)
            .map(|reference| revision_link(self.notes, *reference))
            .collect::<Result<_, _>>()?;
        let mut text = links.join(", ");
        if references.len() > MAX_LISTED {
            let _ = write!(text, " and {} more", references.len() - MAX_LISTED);
        }
        Ok(text)
    }

    /// The brief's material caveat, in the same order of precedence.
    fn caveat(&self, standing: &Standing<'_>) -> Result<String, RenderError> {
        let outcome = standing.outcome;
        if !standing.unavailable.is_empty() {
            let external = standing.unavailable.iter().all(|reference| {
                self.state
                    .revision(*reference)
                    .is_some_and(|artifact| artifact.body["availability"] == "external")
            });
            return Ok(format!(
                "{} Artifacts: {}.",
                if external {
                    EXTERNAL_BYTES
                } else {
                    MISSING_BYTES
                },
                self.links(&standing.unavailable)?
            ));
        }
        let conditions: Vec<RevisionRef> = outcome.body["hard_conditions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|condition| refs(condition.get("criterion_ref")).first().copied())
            .collect();
        let with_conditions = |reason: &str| -> Result<String, RenderError> {
            Ok(if conditions.is_empty() {
                reason.to_owned()
            } else {
                format!("{reason} Hard conditions: {}.", self.links(&conditions)?)
            })
        };
        match standing.state.as_str() {
            "needs_review" => return Ok(BASIS_CHANGED.to_owned()),
            "conditional" => return with_conditions(CONDITIONS_OPEN),
            "proposed" if !conditions.is_empty() => return with_conditions(HARD_CRITERIA_OPEN),
            _ => {}
        }
        if !standing.open.is_empty() {
            let objections: Vec<RevisionRef> = standing
                .open
                .iter()
                .map(|objection| objection.objection_ref)
                .collect();
            return Ok(format!(
                "{OBJECTIONS_OPEN} Objections: {}.",
                self.links(&objections)?
            ));
        }
        if !self.notes.may_show(outcome, Field::Content) {
            return Ok("withheld".to_owned());
        }
        Ok(outcome.body["caveats"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(Value::as_str)
            .map_or_else(
                || "none recorded".to_owned(),
                |caveat| format!("recorded caveat: {}", prose(caveat)),
            ))
    }

    /// The brief's contradiction: a disputed outcome's open objections, or
    /// the contradicting records it names.
    fn contradiction(&self, standing: &Standing<'_>) -> Result<String, RenderError> {
        if standing.state == "disputed" {
            let objections: Vec<RevisionRef> = standing
                .open
                .iter()
                .map(|objection| objection.objection_ref)
                .collect();
            return Ok(if objections.is_empty() {
                DISPUTED.to_owned()
            } else {
                format!("{DISPUTED} Objections: {}.", self.links(&objections)?)
            });
        }
        let named = refs(standing.outcome.body.get("contradiction_refs"));
        if named.is_empty() {
            return Ok("none recorded".to_owned());
        }
        Ok(format!("contradicting records {}", self.links(&named)?))
    }

    // --------------------------------------------------------- synopsis rung

    /// The summary of the leading outcome (or the case): an authored
    /// synopsis while it is current, the projection of typed fields otherwise.
    fn synopsis(&self, out: &mut String, subject: &Record) -> Result<(), RenderError> {
        let (word, newest, withheld) = summary_word(self.notes, subject);
        let projection = self
            .notes
            .title(subject)
            .unwrap_or_else(|| "withheld".to_owned());
        match newest {
            None => {
                let _ = writeln!(
                    out,
                    "- Summary: {} for {} revision {}: the deterministic projection of typed \
                     fields, {projection}",
                    inline_code(word),
                    self.link(subject.header.id)?,
                    subject.header.revision.0
                );
            }
            Some((status, annotation)) if status.state == SynopsisState::Current => {
                let _ = writeln!(
                    out,
                    "- Summary: {}: the authored synopsis {} bound to {}, recorded by {}. It is \
                     editorial text quoted as data, not evidence, and not independent \
                     corroboration of anything it describes.",
                    inline_code(word),
                    self.link(annotation.header.id)?,
                    revision_link(self.notes, status.subject_ref)?,
                    inline_code(&status.recorded_by.to_string())
                );
                let _ = writeln!(
                    out,
                    "\n> [!quote] Authored synopsis (editorial, not evidence)\n> {}\n",
                    prose(synopsis::text(annotation).unwrap_or_default())
                );
            }
            Some((status, annotation)) => {
                let _ = writeln!(
                    out,
                    "- Summary: {}: the newest authored synopsis {} is bound to {}, which changed \
                     materially or needs review. The summary falls back to the deterministic \
                     projection of typed fields, {projection}",
                    inline_code(word),
                    self.link(annotation.header.id)?,
                    revision_link(self.notes, status.subject_ref)?
                );
                for reason in status.reasons.iter().take(SHOWN_REASONS) {
                    let _ = writeln!(out, "  - {}", self.notes.reason_text(reason)?);
                }
            }
        }
        if withheld > 0 {
            let _ = writeln!(
                out,
                "- {withheld} authored synopsis{} withheld by the view policy.",
                if withheld == 1 { " is" } else { "es are" }
            );
        }
        Ok(())
    }

    /// `## Evidence and assessments`: what the leading outcome cites directly.
    fn evidence(&self, out: &mut String) -> Result<(), RenderError> {
        out.push_str("\n## Evidence and assessments\n\n");
        let Some(standing) = &self.standing else {
            let _ = writeln!(
                out,
                "Nothing is cited by a conclusion or decision yet. The {} section below lists \
                 what is recorded.",
                self.focus.heading()
            );
            return Ok(());
        };
        if !self.notes.may_show(standing.outcome, Field::Lineage) {
            out.push_str("Withheld.\n");
            return Ok(());
        }
        let edges = dependency_edges(standing.outcome).unwrap_or_default();
        if edges.is_empty() {
            out.push_str("The answer cites no record.\n");
            return Ok(());
        }
        out.push_str("What the answer cites, by the field that cites it:\n\n");
        let mut seen = BTreeSet::new();
        let mut listed = 0;
        for edge in &edges {
            if !seen.insert(edge.target.id) {
                continue;
            }
            listed += 1;
            if listed > MAX_LISTED {
                continue;
            }
            let status = self
                .state
                .current(edge.target.id)
                .map(|current| status_note(self.notes, current))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "- {} through {}{status}",
                revision_link(self.notes, edge.target)?,
                inline_code(edge.field)
            );
        }
        if listed > MAX_LISTED {
            let _ = writeln!(out, "- and {} more", listed - MAX_LISTED);
        }
        Ok(())
    }

    // ------------------------------------------------------------- full rung

    /// `## Sources`: the exact Sources and Artifacts the answer rests on.
    fn sources(&self, out: &mut String) -> Result<(), RenderError> {
        out.push_str("\n## Sources\n\n");
        let read: Vec<RevisionRef> = match &self.standing {
            Some(standing) if !self.notes.may_show(standing.outcome, Field::Lineage) => {
                out.push_str("Withheld.\n");
                return Ok(());
            }
            Some(standing) => with_origins(self.state, &standing.read),
            None => self
                .state
                .records()
                .filter(|record| matches!(record.header.id.kind(), Kind::Source | Kind::Artifact))
                .map(Record::reference)
                .collect(),
        };
        let sources: Vec<RevisionRef> = read
            .iter()
            .copied()
            .filter(|reference| reference.id.kind() == Kind::Source)
            .collect();
        let artifacts: Vec<RevisionRef> = read
            .iter()
            .copied()
            .filter(|reference| reference.id.kind() == Kind::Artifact)
            .collect();
        if sources.is_empty() && artifacts.is_empty() {
            out.push_str(if self.standing.is_some() {
                "The answer rests on no recorded Source or Artifact.\n"
            } else {
                "No Source or Artifact is recorded yet.\n"
            });
            return Ok(());
        }
        out.push_str(if self.standing.is_some() {
            "The exact revisions the answer rests on. Selectors and quotes are on each \
             Observation note.\n"
        } else {
            "Nothing is cited by an answer yet; every Source and Artifact of the case.\n"
        });
        if !sources.is_empty() {
            out.push_str("\n### Sources\n\n");
            for reference in sources.iter().take(MAX_LISTED) {
                let _ = writeln!(out, "- {}", revision_link(self.notes, *reference)?);
            }
            more(out, sources.len());
        }
        if !artifacts.is_empty() {
            out.push_str("\n### Artifacts\n\n");
            for reference in artifacts.iter().take(MAX_LISTED) {
                let mut line = format!("- {}", revision_link(self.notes, *reference)?);
                if let Some(artifact) = self.state.revision(*reference)
                    && self.notes.may_show(artifact, Field::Metadata)
                {
                    let body = &artifact.body;
                    let _ = write!(
                        line,
                        ": digest {}, availability {}",
                        body["digest"]
                            .as_str()
                            .map_or_else(|| "not recorded".to_owned(), inline_code),
                        inline_code(&super::wire(
                            body["availability"].as_str().unwrap_or("unknown")
                        ))
                    );
                    line.push_str(if is_original(artifact) {
                        ", original acquisition"
                    } else {
                        ", derivative"
                    });
                }
                let _ = writeln!(out, "{line}");
            }
            more(out, artifacts.len());
        }
        Ok(())
    }

    // --------------------------------------------- reviews and retrospectives

    fn reviews(&self, out: &mut String) -> Result<(), RenderError> {
        out.push_str("\n## Reviews and retrospectives\n\n");
        out.push_str("### Reviews of the case\n\n");
        if super::reviews::case_reviews(self.notes, out, self.case)? == 0 {
            out.push_str("No review names the case itself.\n");
        }
        out.push('\n');
        let latest = |kind: Kind, blindspot: bool| {
            self.state
                .records()
                .filter(|record| {
                    record.header.id.kind() == kind
                        && (!blindspot || record.body["kind"] == "blindspot")
                        && self.notes.may_show(record, Field::Lineage)
                })
                .max_by_key(|record| (record.header.acceptance_sequence, record.header.id))
        };
        match latest(Kind::Review, true) {
            Some(review) => {
                let mut line = format!(
                    "- Most recent blindspot review: {}",
                    self.link(review.header.id)?
                );
                if self.notes.visible(review) {
                    if let Some(subject) = refs(review.body.get("subject_ref")).first() {
                        let _ = write!(line, " of {}", revision_link(self.notes, *subject)?);
                    }
                    let _ = write!(
                        line,
                        ", freshness {}",
                        inline_code(super::reviews::freshness_word(self.notes, review))
                    );
                }
                let _ = writeln!(out, "{line}");
            }
            None => out.push_str("- Most recent blindspot review: none recorded\n"),
        }
        match latest(Kind::Retrospective, false) {
            Some(retrospective) => {
                let mut line = format!(
                    "- Most recent retrospective: {}",
                    self.link(retrospective.header.id)?
                );
                if self.notes.visible(retrospective) {
                    let _ = write!(
                        line,
                        ", freshness {}",
                        inline_code(super::reviews::freshness_word(self.notes, retrospective))
                    );
                }
                let _ = writeln!(out, "{line}");
            }
            None => out.push_str("- Most recent retrospective: none recorded\n"),
        }
        Ok(())
    }

    // ---------------------------------------------------------- focus section

    fn focus_section(&self, out: &mut String) -> Result<(), RenderError> {
        let _ = write!(out, "\n## {}\n\n", self.focus.heading());
        match self.focus {
            Focus::Explanation => self.explanation(out),
            Focus::Measurement => self.measurement(out),
            Focus::Evaluation => self.evaluation(out),
            Focus::Research => self.research(out),
        }
    }

    fn of_kind(&self, kind: Kind) -> Vec<&'a Record> {
        let mut found: Vec<&Record> = self
            .state
            .records()
            .filter(|record| record.header.id.kind() == kind)
            .collect();
        found.sort_by_key(|record| (record.header.acceptance_sequence, record.header.id));
        found
    }

    /// One line per record: its link and status, and `extra` when visible.
    fn list(
        &self,
        out: &mut String,
        records: &[&Record],
        none: &str,
        mut extra: impl FnMut(&Record) -> Result<String, RenderError>,
    ) -> Result<(), RenderError> {
        if records.is_empty() {
            let _ = writeln!(out, "{none}");
            return Ok(());
        }
        for record in records.iter().take(MAX_LISTED) {
            let mut line = format!(
                "- {}{}",
                self.link(record.header.id)?,
                status_note(self.notes, record)
            );
            if self.notes.may_show(record, Field::Content) {
                line.push_str(&extra(record)?);
            }
            let _ = writeln!(out, "{line}");
        }
        more(out, records.len());
        Ok(())
    }

    fn explanation(&self, out: &mut String) -> Result<(), RenderError> {
        let assessments = self.of_kind(Kind::Assessment);
        out.push_str("### Rival hypotheses\n\n");
        let hypotheses = self.of_kind(Kind::Hypothesis);
        self.list(
            out,
            &hypotheses,
            "No hypothesis is recorded yet.",
            |hypothesis| {
                let id = hypothesis.header.id.to_string();
                let count = |relation: &str| {
                    assessments
                        .iter()
                        .filter(|assessment| {
                            self.notes.visible(assessment)
                                && text_at(&assessment.body, "subject_ref.id") == Some(id.as_str())
                                && text_at(&assessment.body, "relation") == Some(relation)
                        })
                        .count()
                };
                let mut text = format!(
                    "; assessments: {} contradicting, {} supporting, {} non-diagnostic",
                    count("contradicts"),
                    count("supports"),
                    count("non_diagnostic")
                );
                // Withheld assessments may contradict it: counted, not hidden.
                let withheld = assessments
                    .iter()
                    .filter(|assessment| {
                        self.notes.placeholder(assessment)
                            && text_at(&assessment.body, "subject_ref.id") == Some(id.as_str())
                    })
                    .count();
                if withheld > 0 {
                    let _ = write!(text, ", {withheld} withheld by the view policy");
                }
                Ok(text)
            },
        )?;

        out.push_str("\n### Decisive and non-diagnostic observations\n\n");
        let visible: Vec<&Record> = assessments
            .iter()
            .copied()
            .filter(|assessment| self.notes.visible(assessment))
            .collect();
        let groups: [(&str, &[&str]); 2] = [
            (
                "Discriminating (contradicts or supports a hypothesis)",
                &["contradicts", "supports"],
            ),
            (
                "Non-diagnostic or inconclusive (expected under the rivals alike)",
                &["non_diagnostic", "inconclusive"],
            ),
        ];
        let withheld: Vec<&Record> = assessments
            .iter()
            .copied()
            .filter(|assessment| self.notes.placeholder(assessment))
            .collect();
        if visible.is_empty() && withheld.is_empty() {
            out.push_str("No assessment relates evidence to a hypothesis yet.\n");
        }
        for (label, relations) in groups {
            let group: Vec<&Record> = visible
                .iter()
                .copied()
                .filter(|assessment| {
                    text_at(&assessment.body, "relation")
                        .is_some_and(|relation| relations.contains(&relation))
                })
                .collect();
            if group.is_empty() {
                continue;
            }
            let _ = writeln!(out, "{label}:\n");
            self.list(out, &group, "", |assessment| {
                let body = &assessment.body;
                let mut text = format!(
                    ": {}",
                    inline_code(&super::wire(text_at(body, "relation").unwrap_or("unknown")))
                );
                if let Some(subject) = refs(body.get("subject_ref")).first() {
                    let _ = write!(text, " {}", self.link(subject.id)?);
                }
                let evidence: Vec<String> = refs(body.get("evidence_refs"))
                    .into_iter()
                    .take(3)
                    .map(|reference| self.link(reference.id))
                    .collect::<Result<_, _>>()?;
                if !evidence.is_empty() {
                    let _ = write!(text, "; evidence {}", evidence.join(", "));
                }
                Ok(text)
            })?;
            out.push('\n');
        }
        if !withheld.is_empty() {
            // Their relation is withheld too, so they may be decisive.
            out.push_str("Withheld by the view policy (relation not shown):\n\n");
            self.list(out, &withheld, "", |_| Ok(String::new()))?;
            out.push('\n');
        }

        out.push_str("\n### Unresolved confounds\n\n");
        let assumptions: Vec<&Record> = self
            .of_kind(Kind::Assumption)
            .into_iter()
            .filter(|assumption| {
                !self.notes.visible(assumption)
                    || matches!(
                        text_at(&assumption.body, "status"),
                        Some("unassessed" | "challenged") | None
                    )
            })
            .collect();
        self.list(
            out,
            &assumptions,
            "No unassessed or challenged assumption is recorded.",
            |assumption| {
                Ok(format!(
                    "; materiality {}",
                    inline_code(&super::wire(
                        text_at(&assumption.body, "materiality").unwrap_or("unknown")
                    ))
                ))
            },
        )?;

        out.push_str("\n### Inquiry timeline\n\n");
        let inquiries = self.of_kind(Kind::Inquiry);
        let runs = self.of_kind(Kind::Run);
        self.list(out, &inquiries, "No inquiry is recorded yet.", |inquiry| {
            let id = inquiry.header.id.to_string();
            let mut text = format!(
                ", accepted at sequence {}",
                inquiry.header.acceptance_sequence
            );
            let mine: Vec<&&Record> = runs
                .iter()
                .filter(|run| text_at(&run.body, "intent.plan_ref.id") == Some(id.as_str()))
                .collect();
            for run in mine.iter().take(3) {
                let _ = write!(text, "; run {}", self.link(run.header.id)?);
                if self.notes.visible(run) {
                    let _ = write!(
                        text,
                        " {}, validity {}",
                        inline_code(&super::wire(
                            text_at(&run.body, "execution_status").unwrap_or("unknown")
                        )),
                        inline_code(&super::wire(
                            text_at(&run.body, "evidence_validity").unwrap_or("unknown")
                        ))
                    );
                }
            }
            Ok(text)
        })
    }

    fn comparisons(&self, out: &mut String) -> Result<(), RenderError> {
        let comparisons = self.of_kind(Kind::Comparison);
        self.list(
            out,
            &comparisons,
            "No comparison is recorded yet.",
            |comparison| {
                let body = &comparison.body;
                let mut text = format!(
                    "; outcome {}",
                    inline_code(&super::wire(text_at(body, "outcome").unwrap_or("unknown")))
                );
                if let Some(value) = body.get("value").and_then(super::decimal) {
                    let _ = write!(text, ", value {value}");
                }
                Ok(text)
            },
        )
    }

    fn evaluation(&self, out: &mut String) -> Result<(), RenderError> {
        out.push_str("### Options\n\n");
        let options = self.of_kind(Kind::Option);
        self.list(out, &options, "No option is recorded yet.", |_| {
            Ok(String::new())
        })?;

        out.push_str("\n### Hard constraints and criteria\n\n");
        let mut criteria = self.of_kind(Kind::Criterion);
        criteria.sort_by_key(|criterion| text_at(&criterion.body, "kind") != Some("hard"));
        self.list(
            out,
            &criteria,
            "No criterion is recorded yet.",
            |criterion| {
                Ok(format!(
                    "; {} criterion, threshold {}",
                    inline_code(&super::wire(
                        text_at(&criterion.body, "kind").unwrap_or("unknown")
                    )),
                    criterion
                        .body
                        .get("threshold")
                        .and_then(threshold)
                        .unwrap_or_else(|| "none recorded".to_owned())
                ))
            },
        )?;

        out.push_str("\n### Compared evidence\n\n");
        self.comparisons(out)?;

        out.push_str("\n### Decision\n\n");
        let mut decisions = self.of_kind(Kind::Decision);
        decisions.reverse();
        self.list(
            out,
            &decisions,
            "No decision is proposed yet; the comparisons above are the current state of the \
             evaluation.",
            |_| Ok(String::new()),
        )?;

        out.push_str("\n### Sensitivity and reversal\n\n");
        let Some(decision) = decisions.iter().find(|decision| is_live(decision)) else {
            out.push_str("No live decision states reversal conditions.\n");
            return Ok(());
        };
        if !self.notes.may_show(decision, Field::Content) {
            out.push_str("Withheld.\n");
            return Ok(());
        }
        let body = &decision.body;
        let conditions = body["hard_conditions"].as_array().map_or(0, Vec::len);
        let _ = writeln!(
            out,
            "- Hard conditions still to verify on {}: {}",
            self.link(decision.header.id)?,
            if conditions == 0 {
                "none".to_owned()
            } else {
                conditions.to_string()
            }
        );
        let reversals: Vec<&str> = body["reversal_conditions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|reversal| text_at(reversal, "condition"))
            .collect();
        if reversals.is_empty() {
            out.push_str("- Reversal conditions: none recorded\n");
        } else {
            out.push_str("- Reversal conditions:\n");
            for reversal in reversals.iter().take(MAX_LISTED) {
                let _ = writeln!(out, "  - {}", prose(reversal));
            }
            if reversals.len() > MAX_LISTED {
                let _ = writeln!(out, "  - and {} more", reversals.len() - MAX_LISTED);
            }
        }
        let _ = writeln!(
            out,
            "- Reopen trigger: {}",
            text_at(body, "reopen_trigger")
                .filter(|text| !text.trim().is_empty())
                .map_or_else(|| "not recorded".to_owned(), prose)
        );
        Ok(())
    }

    fn measurement(&self, out: &mut String) -> Result<(), RenderError> {
        out.push_str("### Measured values\n\n");
        let measured: Vec<&Record> = self
            .of_kind(Kind::Observation)
            .into_iter()
            .filter(|record| is_measurement(record))
            .collect();
        self.list(
            out,
            &measured,
            "No measurement is recorded yet.",
            |observation| {
                Ok(format!(
                    "; validity {}",
                    inline_code(&super::wire(
                        text_at(&observation.body, "validity").unwrap_or("unknown")
                    ))
                ))
            },
        )?;

        out.push_str("\n### Runs and repetitions\n\n");
        let runs = self.of_kind(Kind::Run);
        if !runs.is_empty() {
            let _ = writeln!(
                out,
                "{} run{} recorded.\n",
                runs.len(),
                if runs.len() == 1 { "" } else { "s" }
            );
        }
        self.list(out, &runs, "No run is recorded yet.", |run| {
            Ok(format!(
                "; {}, validity {}, completeness {}",
                inline_code(&super::wire(
                    text_at(&run.body, "execution_status").unwrap_or("unknown")
                )),
                inline_code(&super::wire(
                    text_at(&run.body, "evidence_validity").unwrap_or("unknown")
                )),
                inline_code(&super::wire(
                    text_at(&run.body, "evidence_completeness").unwrap_or("unknown")
                ))
            ))
        })?;

        out.push_str("\n### Baseline and variant comparisons\n\n");
        self.comparisons(out)
    }

    fn research(&self, out: &mut String) -> Result<(), RenderError> {
        out.push_str("### Source matrix\n\n");
        let sources = self.of_kind(Kind::Source);
        self.list(out, &sources, "No source is recorded yet.", |_| {
            Ok(String::new())
        })?;

        out.push_str("\n### Contradicting accounts\n\n");
        let contradicting: Vec<&Record> = self
            .of_kind(Kind::Assessment)
            .into_iter()
            .filter(|assessment| {
                // A withheld assessment may be a contradicting account.
                self.notes.placeholder(assessment)
                    || (self.notes.visible(assessment)
                        && text_at(&assessment.body, "relation") == Some("contradicts"))
            })
            .collect();
        self.list(
            out,
            &contradicting,
            "No assessment records a contradiction.",
            |assessment| {
                Ok(match refs(assessment.body.get("subject_ref")).first() {
                    Some(subject) => format!("; contradicts {}", self.link(subject.id)?),
                    None => String::new(),
                })
            },
        )?;

        out.push_str("\n### Open questions\n\n");
        let open: Vec<&Record> = self
            .of_kind(Kind::Question)
            .into_iter()
            .filter(|question| {
                !self.notes.visible(question)
                    || matches!(
                        text_at(&question.body, "disposition"),
                        Some("open" | "deferred")
                    )
            })
            .collect();
        self.list(out, &open, "No open question is recorded.", |_| {
            Ok(String::new())
        })
    }
}

/// A read set with the Artifact each Observation in it was read from and
/// that Artifact's Source, in read-set order, each once.
fn with_origins(state: &State, read: &[RevisionRef]) -> Vec<RevisionRef> {
    let mut found: Vec<RevisionRef> = Vec::new();
    let add = |reference: RevisionRef, found: &mut Vec<RevisionRef>| {
        if !found.contains(&reference) {
            found.push(reference);
        }
    };
    for reference in read {
        add(*reference, &mut found);
        let Some(record) = state.revision(*reference) else {
            continue;
        };
        let artifact = match reference.id.kind() {
            Kind::Observation => refs(record.body["basis"].get("artifact_ref"))
                .first()
                .copied(),
            Kind::Artifact => Some(*reference),
            _ => None,
        };
        let Some(artifact) = artifact else { continue };
        add(artifact, &mut found);
        if let Some(source) = state
            .revision(artifact)
            .and_then(|artifact| refs(artifact.body.get("source")).first().copied())
        {
            add(source, &mut found);
        }
    }
    found
}

/// Whether an Artifact is an original acquisition: its recorded role, or
/// for records without one, no transformation inputs.
fn is_original(artifact: &Record) -> bool {
    match artifact.body.get("role").and_then(Value::as_str) {
        Some(role) => role == "original",
        None => artifact.body["transforms"]
            .as_array()
            .is_none_or(Vec::is_empty),
    }
}

fn more(out: &mut String, total: usize) {
    if total > MAX_LISTED {
        let _ = writeln!(out, "- and {} more", total - MAX_LISTED);
    }
}

/// `## Resume`: the exact commands that read the same story from the store.
fn resume(notes: &NoteContext<'_>, out: &mut String) -> Result<(), RenderError> {
    let case = notes.case().case_id();
    let head = notes.case().head();
    let _ = write!(
        out,
        "\n## Resume\n\nThis note reflects sequence {}. Run these in the workspace that holds this \
         vault; each reads one verified snapshot, and `brief` also verifies retained bytes:\n\n",
        head.sequence
    );
    for command in ["brief", "frontier", "next"] {
        let _ = writeln!(
            out,
            "- {}",
            inline_code(&format!(
                "{PROGRAM_NAME} --case {case} {command} --format json"
            ))
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use investigation_core::{
        state::{AcceptedTransaction, State},
        wire::{RecordChange, Snapshot},
    };
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        Audience, CaseRenderer, RenderPlan, VaultRenderer, VerifiedSnapshot, ViewPolicy, layout,
    };

    const TIME: &str = "2026-11-21T22:00:00.000Z";

    fn id(kind: Kind, n: u64) -> Id {
        format!("{}_018d87fc-5234-7000-8000-{n:012}", kind.prefix())
            .parse()
            .unwrap()
    }

    fn at(id: Id, revision: u64) -> Value {
        json!({"id":id, "revision":revision})
    }

    fn record(id: Id, case: Id, sequence: u64, revision: u64, level: &str, body: Value) -> Record {
        serde_json::from_value(json!({
            "header":{
                "id":id, "case_id":case, "schema_version":"inv.record.v0-alpha",
                "created_by":"agent:overview-test", "actor_descriptor_self_reported":null,
                "created_at":TIME, "recorded_at":TIME, "recorded_by":"agent:overview-test",
                "observed_at":null, "acquired_at":null, "acceptance_sequence":sequence,
                "revision":revision, "history":(1..revision).collect::<Vec<_>>(),
                "classification":{"level":level, "reason":null, "tags":[]},
                "provenance":{"references":[]}, "extensions":{}
            },
            "body":body
        }))
        .unwrap()
    }

    fn apply(state: &mut State, case: Id, sequence: u64, records: Vec<Record>) {
        let (created, updated): (Vec<_>, Vec<_>) = records
            .iter()
            .map(|record| RecordChange {
                client_ref: None,
                id: record.header.id,
                revision: record.header.revision.0,
            })
            .partition(|change| change.revision == 1);
        state
            .apply(&AcceptedTransaction {
                case_id: case,
                sequence,
                records,
                created,
                updated,
                warnings: Vec::new(),
            })
            .unwrap();
    }

    fn overview(state: &State, policy: &ViewPolicy) -> String {
        let head = Snapshot {
            sequence: state.sequence(),
            head_hash: serde_json::from_value(json!(format!("sha256:{}", "cd".repeat(32))))
                .unwrap(),
        };
        let snapshot =
            VerifiedSnapshot::from_states(id(Kind::Workspace, 9), vec![(head, state.clone())]);
        let plan: RenderPlan = VaultRenderer.render(&snapshot, policy).unwrap();
        let path = layout::case_overview(id(Kind::Case, 1)).unwrap();
        String::from_utf8(plan.entry(path.as_str()).unwrap().content().to_vec()).unwrap()
    }

    fn section<'a>(note: &'a str, heading: &str) -> &'a str {
        let start = note
            .find(&format!("\n## {heading}\n"))
            .unwrap_or_else(|| panic!("no {heading}:\n{note}"))
            + 1;
        let rest = &note[start..];
        let end = rest[3..].find("\n## ").map_or(rest.len(), |at| at + 4);
        &rest[..end]
    }

    fn case_body(title: &str, mode: &str) -> Value {
        json!({"title":title, "mode":mode, "objective":null,
            "scope":{"state":"known", "value":"Staging only"},
            "decision_relevance":"Pick a pool size", "exclusions":[], "stop_condition":null,
            "policy_pins":[], "method_pins":[], "expectations":null, "lifecycle":"active",
            "closure_outcome":null})
    }

    /// An explanatory case: a question, a hypothesis, a conclusion disputed
    /// by an open critical objection, a current authored synopsis of the
    /// conclusion, a restricted hypothesis, and a blindspot review of the
    /// case itself written against its first revision.
    fn disputed() -> State {
        explanatory(true)
    }

    fn explanatory(objection: bool) -> State {
        let case = id(Kind::Case, 1);
        let question = id(Kind::Question, 2);
        let hypothesis = id(Kind::Hypothesis, 3);
        let conclusion = id(Kind::Conclusion, 4);
        let mut state = State::default();
        apply(
            &mut state,
            case,
            1,
            vec![
                record(
                    case,
                    case,
                    1,
                    1,
                    "internal",
                    case_body("Pool stalls", "explain"),
                ),
                record(
                    question,
                    case,
                    1,
                    1,
                    "internal",
                    json!({"wording":"Why does the pool stall?", "disposition":"open"}),
                ),
                record(
                    hypothesis,
                    case,
                    1,
                    1,
                    "internal",
                    json!({"statement":"The pool is exhausted", "lifecycle":"proposed",
                        "question_ref":at(question, 1)}),
                ),
                record(
                    id(Kind::Hypothesis, 5),
                    case,
                    1,
                    1,
                    "restricted",
                    json!({"statement":"TOP-SECRET rival", "lifecycle":"proposed",
                        "question_ref":at(question, 1)}),
                ),
                record(
                    conclusion,
                    case,
                    1,
                    1,
                    "internal",
                    json!({"statement":"Pool exhaustion causes the stall", "lifecycle":"proposed",
                        "question_ref":at(question, 1), "caveats":["Staging traffic only"],
                        "support_refs":[at(hypothesis, 1)]}),
                ),
                record(
                    id(Kind::Review, 6),
                    case,
                    1,
                    1,
                    "internal",
                    json!({"kind":"blindspot", "status":"submitted", "subject_ref":at(case, 1),
                        "verdict":"gaps_identified", "read_set":[], "scope_examined":"whole case",
                        "method":"read every record"}),
                ),
            ],
        );
        let mut later = vec![record(
            id(Kind::Annotation, 8),
            case,
            2,
            1,
            "internal",
            json!({"role":"synopsis", "subject_refs":[at(conclusion, 1)],
                "content":{"kind":"text", "text":"Exhaustion [[explains]] it"}}),
        )];
        if objection {
            later.push(record(
                id(Kind::Objection, 7),
                case,
                2,
                1,
                "internal",
                json!({"subject_ref":at(conclusion, 1), "statement":"The sample is biased",
                    "scope":"sample", "severity":"critical", "status":"open",
                    "review_ref":null, "evidence_refs":{"state":"unknown"},
                    "resolution_reason":null, "resolution_evidence_refs":[]}),
            ));
        }
        apply(&mut state, case, 2, later);
        state
    }

    #[test]
    fn the_opening_summary_states_each_element_and_the_objection_before_counts() {
        let note = overview(&disputed(), &ViewPolicy::default());
        let opening = section(&note, "Opening summary");
        let conclusion = id(Kind::Conclusion, 4).to_string();
        for expected in [
            "We are investigating [[",
            "|Why does the pool stall?]] because it informs Pick a pool size.",
            "Our current answer is [[",
            "|Pool exhaustion causes the stall]] (`disputed`), within scope Staging only.",
            "The strongest original evidence is (no observation cited)",
            "the strongest outstanding objection is [[",
            "|The sample is biased]] (`critical`, against [[",
        ] {
            assert!(opening.contains(expected), "{expected}\n{opening}");
        }
        assert!(opening.contains(&conclusion));
        let objection = note.find("strongest outstanding objection is").unwrap();
        assert!(objection < note.find("records in evidentiary use").unwrap());

        // A disputed answer leads with the objections, as `brief` does.
        let answer = section(&note, "Answer and caveat");
        assert!(answer.contains(&format!(
            "- Material caveat: {OBJECTIONS_OPEN} Objections: [["
        )));
        assert!(answer.contains(&format!("- Contradiction: {DISPUTED} Objections: [[")));

        // The dispute makes the synopsis stale: it is not quoted.
        assert!(
            answer.contains("- Summary: `summary_stale`: the newest authored synopsis [["),
            "{answer}"
        );
        assert!(answer.contains("  - `disputed` because of `critical_objection` at [["));
        assert!(!answer.contains("[!quote]"));

        // The case's own review is shown with its freshness.
        let reviews = section(&note, "Reviews and retrospectives");
        let line = reviews
            .lines()
            .find(|line| {
                line.contains(&id(Kind::Review, 6).to_string()) && line.contains("review of")
            })
            .unwrap_or_else(|| panic!("{reviews}"));
        assert!(
            line.contains(
                "`blindspot` review of revision 1, verdict `gaps_identified`, freshness `current`"
            ),
            "{line}"
        );
        assert!(
            reviews.contains("- Most recent blindspot review: [["),
            "{reviews}"
        );

        // The explanatory section; the restricted rival is listed by ID only.
        let explanation = section(&note, "Explanation");
        assert!(
            explanation.contains("|The pool is exhausted]], `proposed`"),
            "{explanation}"
        );
        assert!(explanation.contains(&format!(
            "[[Cases/{}/Hypotheses/{}|{}]]\n",
            id(Kind::Case, 1),
            id(Kind::Hypothesis, 5),
            id(Kind::Hypothesis, 5)
        )));
        assert!(!note.contains("TOP-SECRET"));
        assert!(note.contains("\ninv_focus: \"explanation\"\n"));
    }

    #[test]
    fn a_current_synopsis_is_quoted_editorial_data() {
        let note = overview(&explanatory(false), &ViewPolicy::default());
        let answer = section(&note, "Answer and caveat");
        assert!(
            answer.contains("- Summary: `summary_current`: the authored synopsis [["),
            "{answer}"
        );
        assert!(answer.contains("not evidence, and not independent corroboration"));
        assert!(
            answer.contains("> [!quote] Authored synopsis (editorial, not evidence)\n> Exhaustion")
        );
        assert!(!answer.contains("[[explains]]"), "{answer}");
        assert!(
            note.contains("\ninv_summary: \"summary_current\"\n"),
            "{note}"
        );
        // Without an objection the brief's caveat is the first recorded one.
        assert!(
            answer.contains("- Material caveat: recorded caveat: Staging traffic only"),
            "{answer}"
        );
        assert!(answer.contains("- Contradiction: none recorded"));
        assert!(section(&note, "Opening summary").contains(
            "the strongest outstanding objection is none open on the answer or its basis"
        ));
    }

    #[test]
    fn a_material_change_stales_the_synopsis_and_the_case_review() {
        let mut state = explanatory(false);
        let case = id(Kind::Case, 1);
        let conclusion = id(Kind::Conclusion, 4);
        apply(
            &mut state,
            case,
            3,
            vec![
                record(
                    case,
                    case,
                    3,
                    2,
                    "internal",
                    case_body("Pool stalls under load", "explain"),
                ),
                record(
                    conclusion,
                    case,
                    3,
                    2,
                    "internal",
                    json!({"statement":"Pool exhaustion causes most stalls", "lifecycle":"proposed",
                        "question_ref":at(id(Kind::Question, 2), 1),
                        "caveats":["Staging traffic only"],
                        "support_refs":[at(id(Kind::Hypothesis, 3), 1)]}),
                ),
            ],
        );
        let note = overview(&state, &ViewPolicy::default());
        let answer = section(&note, "Answer and caveat");
        assert!(
            answer.contains("- Summary: `summary_stale`: the newest authored synopsis [["),
            "{answer}"
        );
        assert!(answer.contains(
            "falls back to the deterministic projection of typed fields, Pool exhaustion causes most stalls"
        ));
        assert!(
            !answer.contains("[!quote]"),
            "a stale synopsis is not quoted"
        );
        assert!(note.contains("\ninv_summary: \"summary_stale\"\n"));
        let reviews = section(&note, "Reviews and retrospectives");
        assert!(
            reviews.contains("review of revision 1 (this is revision 2)")
                && reviews.contains("freshness `stale`: this review no longer covers"),
            "{reviews}"
        );
    }

    #[test]
    fn a_comparison_case_without_a_decision_is_an_evaluation_not_a_failure() {
        let case = id(Kind::Case, 1);
        let question = id(Kind::Question, 2);
        let mut state = State::default();
        apply(
            &mut state,
            case,
            1,
            vec![
                // Mode `explore`: the section follows what the case holds.
                record(
                    case,
                    case,
                    1,
                    1,
                    "internal",
                    case_body("Queue choice", "explore"),
                ),
                record(
                    question,
                    case,
                    1,
                    1,
                    "internal",
                    json!({"wording":"Which queue?", "disposition":"open"}),
                ),
                record(
                    id(Kind::Option, 3),
                    case,
                    1,
                    1,
                    "internal",
                    json!({"title":"Managed queue", "lifecycle":"eligible", "question_ref":at(question, 1)}),
                ),
                record(
                    id(Kind::Criterion, 4),
                    case,
                    1,
                    1,
                    "internal",
                    json!({"title":"Recovery time", "kind":"hard", "lifecycle":"active",
                        "question_ref":at(question, 1)}),
                ),
            ],
        );
        let note = overview(&state, &ViewPolicy::default());
        assert!(note.contains("\ninv_focus: \"evaluation\"\n"));
        let opening = section(&note, "Opening summary");
        assert!(
            opening.contains("Our current choice is (no decision yet)"),
            "{opening}"
        );
        assert!(opening.contains("the strongest outstanding objection is none open"));
        let evaluation = section(&note, "Evaluation");
        assert!(
            evaluation.contains("|Managed queue]], `eligible`"),
            "{evaluation}"
        );
        assert!(evaluation.contains("No decision is proposed yet"));
        for absent in [
            "Conclusions: 0",
            "0 conclusions",
            "no conclusion yet",
            "## Explanation",
        ] {
            assert!(!note.contains(absent), "{absent}\n{note}");
        }
    }

    #[test]
    fn a_withheld_answer_keeps_its_caveat_basis_and_objections_out() {
        let mut state = disputed();
        let case = id(Kind::Case, 1);
        let conclusion = id(Kind::Conclusion, 4);
        apply(
            &mut state,
            case,
            3,
            vec![record(
                conclusion,
                case,
                3,
                2,
                "restricted",
                json!({"statement":"SECRET-ANSWER", "lifecycle":"proposed",
                    "question_ref":at(id(Kind::Question, 2), 1),
                    "caveats":["SECRET-CAVEAT"], "support_refs":[at(id(Kind::Hypothesis, 3), 1)]}),
            )],
        );
        let note = overview(&state, &ViewPolicy::default());
        for secret in ["SECRET-ANSWER", "SECRET-CAVEAT"] {
            assert!(!note.contains(secret), "{secret}\n{note}");
        }
        // The objection is visible on its own, in the record index; the
        // summary does not tie it to the withheld answer.
        let summary = note.split("\n## Records\n").next().unwrap();
        assert!(!summary.contains("The sample is biased"), "{summary}");
        let opening = section(&note, "Opening summary");
        assert!(
            opening.contains(&format!("|{conclusion}]] (withheld)")),
            "{opening}"
        );
        assert!(
            opening.contains("the strongest outstanding objection is withheld with the answer")
        );
        let answer = section(&note, "Answer and caveat");
        assert!(
            answer.contains("its state, caveat, objections and basis are withheld"),
            "{answer}"
        );
        assert!(section(&note, "Evidence and assessments").contains("Withheld."));
        assert!(section(&note, "Sources").contains("Withheld."));
    }

    #[test]
    fn a_withheld_case_shows_no_summary_text() {
        let note = overview(&disputed(), &ViewPolicy::for_audience(Audience::Public));
        assert!(section(&note, "Opening summary").contains("Withheld."));
        for secret in ["Pick a pool size", "Staging only", "Exhaustion", "biased"] {
            assert!(!note.contains(secret), "{secret}\n{note}");
        }
        assert!(section(&note, "Resume").contains("brief --format json"));
    }
}
