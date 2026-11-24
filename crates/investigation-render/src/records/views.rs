//! Static summary tables: one note per shipped view of a case (plan §12.5).
//! Pure.
//!
//! Each view is one plain Markdown table generator over one case's records
//! at one snapshot, with the default columns of §12.5, under
//! `Cases/<case-id>/Views/`. The tables are the baseline the dynamic views
//! build on, so they must read the same in any text viewer:
//!
//! - Every state is a word, never a colour or symbol. Hard failures and
//!   Pareto status are sentences under the comparison matrix.
//! - No table computes or shows an overall score, rank or weighted total.
//! - Rows are bounded ([`MAX_ROWS`]) with the count left out and a link to
//!   the Overview, which lists every record. Rows sort by a stable key: the
//!   record ID (creation order), the gap ID for the frontier, and the
//!   acceptance sequence for the timeline.
//! - Each table reads exactly one case: rows are filtered by the case ID.
//! - Cells are built only from the escapers of [`crate::text`] and links
//!   from [`NoteContext::table_link`]; a cell never holds a line break.
//! - A record the audience may not see is left out, or shown by ID with
//!   `withheld` cells when only its content is withheld.
//!
//! A view with no rows is not planned, except the epistemic frontier and
//! the timeline, which every case has.

use std::{collections::BTreeMap, fmt::Write as _};

use investigation_core::{
    PROGRAM_NAME,
    classification::Field,
    evaluate::{ComparisonOutcome, CriterionKind},
    gap_rules::{GAP_RULE_VERSION, GapStatus, PrerequisiteState},
    header::{Knowledge, SourceTime},
    identity::{Id, Kind},
    measurement_flags,
    origin_ops::{self, OriginState},
    source_ops,
    state::Record,
    weighting::{CellState, ConsequencesTable},
};
use serde_json::Value;

use super::{Facts, citing, effective_word, lifecycle, naming, refs, scalar_word, token, wire};
use crate::{
    PlanPath, RenderError, bases, layout, marker,
    note::{NoteContext, VAULT_SCHEMA, level_word, text_at, value_at, wire_word},
    text::{inline_code, inline_text, table_code},
    yaml::Frontmatter,
};

/// Most rows in one table.
pub(crate) const MAX_ROWS: usize = 50;
/// Most per-question matrices in the comparison matrix view.
/// A cell a review export with remapped identifiers leaves empty: journal
/// sequences and record times would tell how the case journal grew.
const NOT_SHOWN_CELL: &str = "not shown in this export";

const MAX_MATRICES: usize = 12;
/// Authored text in a cell, in characters.
const CELL_CHARS: usize = 80;
/// Subjects linked in one frontier cell.
const MAX_SUBJECTS: usize = 3;

/// The shipped views in §12.5 order: file name (also the H1) and the
/// `inv_view` word.
pub(crate) const VIEWS: &[(&str, &str)] = &[
    ("Questions", "questions"),
    ("Hypotheses", "hypotheses"),
    ("Options", "options"),
    ("Criteria", "criteria"),
    ("Comparison Matrix", "comparison_matrix"),
    ("Sources", "sources"),
    ("Evidence", "evidence"),
    ("Measurements", "measurements"),
    ("Inquiries and Runs", "inquiries_and_runs"),
    ("Conclusions", "conclusions"),
    ("Decisions", "decisions"),
    ("Reviews and Blindspots", "reviews"),
    ("Retrospectives and Lessons", "retrospectives"),
    ("Epistemic Frontier", "frontier"),
    ("Timeline", "timeline"),
];

/// Lifecycle keys that carry the stated reason for a revision.
const REASON_KEYS: &[&str] = &[
    "change_reason",
    "correction_reason",
    "revision_reason",
    "retraction_reason",
    "amendment_reason",
    "superseded_reason",
    "disposition_reason",
    "resolution_reason",
];

/// One planned view note, and its native Base when the policy plans one.
pub(crate) struct ViewNote {
    pub name: &'static str,
    pub path: PlanPath,
    pub rows: usize,
    pub content: String,
    pub base: Option<(PlanPath, String)>,
}

/// The view notes of one case, in [`VIEWS`] order. When the view policy
/// allows every Base feature, each view that has a Base spec
/// ([`bases::spec`]) also plans its Base, and its note says so.
pub(crate) fn plan(notes: &NoteContext<'_>) -> Result<Vec<ViewNote>, RenderError> {
    let views = Views::new(notes)?;
    let case_id = notes.case().case_id();
    let mut planned = Vec::new();
    for &(name, slug) in VIEWS {
        let Some(mut body) = views.body(slug)? else {
            continue;
        };
        let base = match bases::spec(slug).filter(|_| notes.policy().allows_all(bases::FEATURES)) {
            Some(spec) => {
                body.text.push_str(&base_paragraph(name, slug));
                Some(bases::plan(case_id, name, spec)?)
            }
            None => None,
        };
        planned.push(ViewNote {
            name,
            path: layout::case_view(case_id, name)?,
            rows: body.rows,
            content: views.note(name, slug, &body)?,
            base,
        });
    }
    Ok(planned)
}

/// Headings of the tables in the frontier and timeline notes whose rows are
/// notes, so a Base can list the same rows.
pub(crate) const FRONTIER_NOTES: &str = "Records named by an open gap";
pub(crate) const TIMELINE_NOTES: &str = "Latest revision of each record";

/// The paragraph a view note ends with when its Base is planned.
fn base_paragraph(name: &str, slug: &str) -> String {
    let rows = match slug {
        "frontier" => format!("the notes in the table under {FRONTIER_NOTES:?}"),
        "timeline" => format!("the notes in the table under {TIMELINE_NOTES:?}"),
        _ => "the same rows as the table above".to_owned(),
    };
    format!(
        "\n## Native Base\n\n{} beside this note lists {rows}, read from note properties and \
         filtered by this case's exact ID. It needs a viewer release that the compatibility \
         profile supports; this note does not. Editing a cell in the Base changes that note's \
         properties, which is drift: the edited note is left in place, no longer updated, and \
         the edit is not recorded. Editing the Base file itself is drift too.\n",
        inline_code(&format!("{name}.base"))
    )
}

/// A view's text after the shared opening, and its row count.
struct Body {
    rows: usize,
    text: String,
}

struct Views<'n, 'a> {
    notes: &'n NoteContext<'a>,
    /// Current records of this case other than the case itself whose
    /// identity the audience may see, in record ID order.
    records: Vec<&'a Record>,
    /// Consequences tables of questions with at least one cell.
    tables: BTreeMap<Id, &'n ConsequencesTable>,
    /// Questions shown in the comparison matrix view, in ID order.
    matrix_questions: Vec<&'a Record>,
    matrix_path: Option<PlanPath>,
}

impl<'n, 'a> Views<'n, 'a> {
    fn new(notes: &'n NoteContext<'a>) -> Result<Self, RenderError> {
        let case = notes.case().case_id();
        let state = notes.case().state();
        let mut records: Vec<&'a Record> = state
            .records()
            .filter(|record| {
                record.header.case_id == case
                    && record.header.id.kind() != Kind::Case
                    && notes.may_show(record, Field::Identity)
            })
            .collect();
        records.sort_by_key(|record| record.header.id);
        let mut tables = BTreeMap::new();
        let mut matrix_questions = Vec::new();
        for question in records
            .iter()
            .filter(|record| record.header.id.kind() == Kind::Question)
        {
            let id = question.header.id;
            let Some(table) = notes.consequences_table(id) else {
                continue;
            };
            if table.rows.is_empty() || table.criteria.is_empty() {
                continue;
            }
            if notes.visible(question) && notes.may_show(question, Field::Content) {
                matrix_questions.push(*question);
            }
            tables.insert(id, table);
        }
        let matrix_path = if matrix_questions.is_empty() {
            None
        } else {
            Some(layout::case_view(case, "Comparison Matrix")?)
        };
        Ok(Self {
            notes,
            records,
            tables,
            matrix_questions,
            matrix_path,
        })
    }

    fn body(&self, slug: &str) -> Result<Option<Body>, RenderError> {
        match slug {
            "questions" => self.questions(),
            "hypotheses" => self.hypotheses(),
            "options" => self.options(),
            "criteria" => self.criteria(),
            "comparison_matrix" => self.matrix(),
            "sources" => self.sources(),
            "evidence" => self.evidence(),
            "measurements" => self.measurements(),
            "inquiries_and_runs" => self.inquiries(),
            "conclusions" => self.conclusions(),
            "decisions" => self.decisions(),
            "reviews" => self.reviews(),
            "retrospectives" => self.retrospectives(),
            "frontier" => self.frontier().map(Some),
            "timeline" => self.timeline().map(Some),
            _ => Ok(None),
        }
    }

    /// Frontmatter, H1, banner and the case line, then the body.
    fn note(&self, name: &str, slug: &str, body: &Body) -> Result<String, RenderError> {
        let case = self.notes.case();
        let head = case.head();
        let mut front = Frontmatter::new();
        front
            .text("inv_kind", "view")
            .text("inv_view", slug)
            .text("inv_case_id", &case.case_id().to_string())
            .link("inv_case", &layout::case_overview(case.case_id())?)
            .integer("inv_rows", body.rows as u64)
            .integer(marker::SEQUENCE_KEY, head.sequence)
            .text(marker::SNAPSHOT_KEY, head.head_hash.as_str())
            .text("inv_schema", VAULT_SCHEMA)
            .list("tags", &["investigation/view"]);
        let mut out = front.finish();
        let _ = write!(out, "# {name}\n\n{}\n", self.notes.banner());
        let _ = writeln!(
            out,
            "Case: {}. Snapshot: sequence {}, head {}.",
            self.notes.link(case.case_id())?,
            head.sequence,
            inline_code(head.head_hash.as_str())
        );
        out.push_str(&body.text);
        Ok(out)
    }

    fn overview_link(&self) -> Result<String, RenderError> {
        self.notes.link(self.notes.case().case_id())
    }

    /// One table of current records of `kinds` that `keep` admits, a row per
    /// record in ID order. The first column links the record; `row` gives
    /// the rest, and is called only when the record's content may appear.
    fn records_table(
        &self,
        kinds: &[Kind],
        keep: impl Fn(&Record) -> bool,
        noun: (&str, &str),
        intro: &str,
        headers: &[&str],
        row: impl Fn(&Record) -> Result<Vec<String>, RenderError>,
    ) -> Result<Option<Body>, RenderError> {
        let selected: Vec<&Record> = self
            .records
            .iter()
            .copied()
            .filter(|record| kinds.contains(&record.header.id.kind()) && keep(record))
            .collect();
        if selected.is_empty() {
            return Ok(None);
        }
        let mut text = format!("\n{intro} One row per {}, in record ID order.\n\n", noun.0);
        header(&mut text, headers);
        for record in selected.iter().take(MAX_ROWS) {
            let mut cells = vec![self.notes.table_link(record.header.id)?];
            if self.notes.visible(record) && self.notes.may_show(record, Field::Content) {
                cells.extend(row(record)?);
            } else {
                cells.extend(std::iter::repeat_n(
                    "withheld".to_owned(),
                    headers.len() - 1,
                ));
            }
            line(&mut text, &cells);
        }
        if selected.len() > MAX_ROWS {
            let _ = writeln!(
                text,
                "\nShowing {MAX_ROWS} of {} {}. Every record is listed under Records on {}.",
                selected.len(),
                noun.1,
                self.overview_link()?
            );
        }
        Ok(Some(Body {
            rows: selected.len(),
            text,
        }))
    }

    fn count_citing(&self, record: &Record, fields: &[&str], kind: Kind) -> usize {
        citing(self.notes, record, fields)
            .iter()
            .filter(|(by, _)| by.header.id.kind() == kind)
            .count()
    }

    // ------------------------------------------------------------ Questions

    fn questions(&self) -> Result<Option<Body>, RenderError> {
        self.records_table(
            &[Kind::Question],
            |_| true,
            ("question", "questions"),
            "What each question is for and how far it is answered.",
            &[
                "Question",
                "Status",
                "Decision relevance",
                "Bounded answer",
                "Next discriminator",
            ],
            |record| {
                let decisions = self.count_citing(record, &["question_ref"], Kind::Decision);
                let relevance = format!(
                    "importance {}; {}",
                    word_at(record, "importance"),
                    if decisions == 0 {
                        "no decision cites it".to_owned()
                    } else {
                        format!("informs {}", plural(decisions, "decision", "decisions"))
                    }
                );
                Ok(vec![
                    word_at(record, "disposition"),
                    relevance,
                    text_or(text_at(&record.body, "answer"), "none recorded"),
                    self.next_discriminator(record.header.id),
                ])
            },
        )
    }

    /// The first open gap naming `id`, as its status and reason.
    fn next_discriminator(&self, id: Id) -> String {
        let Some(gaps) = self.notes.open_gaps(id) else {
            return "gap inventory unavailable at this snapshot".to_owned();
        };
        let Some(first) = gaps.first() else {
            return "no open gap names it".to_owned();
        };
        let mut text = if self.notes.gap_withheld(&first.gap_id) {
            format!(
                "{}: details withheld, the rule read a withheld record",
                table_code(&first.rule_id)
            )
        } else {
            format!(
                "{}: {}",
                table_code(&wire_word(&first.status)),
                inline_text(&first.reason, CELL_CHARS)
            )
        };
        if gaps.len() > 1 {
            let _ = write!(text, " (and {} more open)", gaps.len() - 1);
        }
        text
    }

    // ----------------------------------------------------------- Hypotheses

    fn hypotheses(&self) -> Result<Option<Body>, RenderError> {
        self.records_table(
            &[Kind::Hypothesis],
            |_| true,
            ("hypothesis", "hypotheses"),
            "Each explanation's standing, its rivals and how the assessed evidence bears on \
             it. Counts are of recorded assessments, not a weight of evidence.",
            &[
                "Hypothesis",
                "Standing",
                "Rival explanations",
                "Contradicting, supporting, non-diagnostic",
                "Assumptions",
            ],
            |record| {
                let id = record.header.id;
                let mut standing = word_at(record, "lifecycle");
                if let Some(effective) = self.notes.effective(record)
                    && effective.freshness.as_str() != "current"
                {
                    let _ = write!(
                        standing,
                        ", effective {}",
                        table_code(effective.freshness.as_str())
                    );
                }
                let mut rivals: Vec<Id> = Vec::new();
                for path in ["rival_refs", "alternative_to"] {
                    rivals.extend(refs(record.body.get(path)).iter().map(|r| r.id));
                }
                rivals.extend(
                    naming(
                        self.notes,
                        record,
                        Kind::Hypothesis,
                        &["rival_refs", "alternative_to"],
                    )
                    .iter()
                    .map(|rival| rival.header.id),
                );
                if let Some(question) = text_at(&record.body, "question_ref.id") {
                    rivals.extend(
                        self.notes
                            .hypotheses_of(question)
                            .iter()
                            .map(|other| other.header.id),
                    );
                }
                rivals.sort();
                rivals.dedup();
                rivals.retain(|rival| *rival != id);
                let rivals = if rivals.is_empty() {
                    "none recorded, so evidence cannot discriminate it".to_owned()
                } else {
                    plural(rivals.len(), "rival", "rivals")
                };
                let mut counts = [0_usize; 4];
                let mut withheld = 0;
                for (assessment, _) in citing(self.notes, record, &["subject_ref"]) {
                    if assessment.header.id.kind() != Kind::Assessment {
                        continue;
                    }
                    if !self.notes.visible(assessment) {
                        withheld += 1;
                        continue;
                    }
                    match text_at(&assessment.body, "relation") {
                        Some("contradicts") => counts[0] += 1,
                        Some("supports") => counts[1] += 1,
                        Some("non_diagnostic") => counts[2] += 1,
                        _ => counts[3] += 1,
                    }
                }
                let mut evidence = if counts.iter().sum::<usize>() + withheld == 0 {
                    "no assessment yet".to_owned()
                } else {
                    format!(
                        "{} contradicting, {} supporting, {} non-diagnostic, {} other",
                        counts[0], counts[1], counts[2], counts[3]
                    )
                };
                if withheld > 0 {
                    let _ = write!(evidence, ", {withheld} withheld");
                }
                let assumptions = refs(record.body.get("assumption_refs")).len();
                Ok(vec![
                    standing,
                    rivals,
                    evidence,
                    count_or(assumptions, "assumption", "assumptions", "none recorded"),
                ])
            },
        )
    }

    // -------------------------------------------------------------- Options

    fn options(&self) -> Result<Option<Body>, RenderError> {
        self.records_table(
            &[Kind::Option],
            |_| true,
            ("option", "options"),
            "Each option's eligibility, what its hard criteria record and what is still \
             unknown. A hard criterion counts as met only on a verified measurement; \
             nothing here is a score or a rank.",
            &[
                "Option",
                "Eligibility",
                "Hard constraints",
                "Critical unknowns",
                "Comparisons",
            ],
            |record| {
                let (hard, unknowns) = self.option_constraints(record)?;
                let compared = self.count_citing(record, &["option_ref"], Kind::Comparison);
                let mut comparisons =
                    count_or(compared, "comparison", "comparisons", "none recorded");
                if compared > 0
                    && let Some(path) = &self.matrix_path
                {
                    let _ = write!(
                        comparisons,
                        "; see {}",
                        layout::wikilink(path, "the comparison matrix").replace('|', "\\|")
                    );
                }
                Ok(vec![
                    word_at(record, "lifecycle"),
                    hard,
                    unknowns,
                    comparisons,
                ])
            },
        )
    }

    /// The hard-criterion standing of an option, and its unknown cells, in
    /// words, from the consequences table of its question.
    fn option_constraints(&self, record: &Record) -> Result<(String, String), RenderError> {
        let id = record.header.id;
        let row = text_at(&record.body, "question_ref.id")
            .and_then(|question| question.parse::<Id>().ok())
            .and_then(|question| self.tables.get(&question))
            .and_then(|table| {
                table
                    .rows
                    .iter()
                    .find(|row| row.option_ref.id == id)
                    .map(|row| (table, row))
            });
        let Some((table, row)) = row else {
            return Ok((
                "not in a comparison table: not eligible, or no active criterion".to_owned(),
                "not compared".to_owned(),
            ));
        };
        let mut failed = Vec::new();
        let (mut hard, mut met, mut unestablished, mut other) = (0, 0, 0, 0);
        for (cell, criterion) in row.cells.iter().zip(&table.criteria) {
            let established = cell.state == CellState::Measured;
            if criterion.kind == CriterionKind::Hard {
                hard += 1;
                match (cell.outcome, established) {
                    (Some(ComparisonOutcome::Fails), _) => {
                        let mut link = self.notes.table_link(criterion.criterion_ref.id)?;
                        if !established {
                            link.push_str(" (evidence not verified)");
                        }
                        failed.push(link);
                    }
                    (Some(ComparisonOutcome::Meets), true) => met += 1,
                    _ => unestablished += 1,
                }
            } else if matches!(
                cell.state,
                CellState::Unknown
                    | CellState::Missing
                    | CellState::Multiple
                    | CellState::NeedsReview
            ) {
                other += 1;
            }
        }
        let standing = if hard == 0 {
            "no active hard criterion".to_owned()
        } else if failed.is_empty() {
            format!("no recorded hard failure; meets {met} of {hard} on verified evidence")
        } else {
            format!("fails the hard criterion {}", failed.join(", "))
        };
        let unknowns = if unestablished + other == 0 {
            "none recorded".to_owned()
        } else {
            format!(
                "{unestablished} of {hard} hard criteria not established; {other} other cells \
                 unknown, missing or out of date"
            )
        };
        Ok((standing, unknowns))
    }

    // ------------------------------------------------------------- Criteria

    fn criteria(&self) -> Result<Option<Body>, RenderError> {
        self.records_table(
            &[Kind::Criterion],
            |_| true,
            ("criterion", "criteria"),
            "What each criterion measures, its threshold or preference, its version and \
             which decisions depend on it.",
            &[
                "Criterion",
                "Hard or soft",
                "Metric or scale",
                "Threshold or preference",
                "Version",
                "Decision sensitivity",
            ],
            |record| {
                let body = &record.body;
                let unit =
                    text_at(body, "scale.unit").map_or_else(|| "no unit".to_owned(), table_code);
                let scale = format!(
                    "{unit}: {}",
                    text_or(text_at(body, "scale.description"), "no description")
                );
                let threshold = match text_at(body, "threshold.kind") {
                    Some("measured") => format!(
                        "{} {} {}",
                        word_at(record, "threshold.operator"),
                        word_at(record, "threshold.value.decimal"),
                        text_at(body, "threshold.value.unit").map_or_else(String::new, table_code)
                    ),
                    Some("qualitative") => format!(
                        "qualitative: {}",
                        text_or(text_at(body, "threshold.requirement"), "not stated")
                    ),
                    Some(other) => table_code(&wire(other)),
                    None => "no threshold".to_owned(),
                };
                let preference = format!(
                    "{}; prefers {}",
                    threshold.trim_end(),
                    word_at(record, "preferred_direction")
                );
                let version = format!(
                    "revision {}, {}",
                    record.header.revision.0,
                    word_at(record, "lifecycle")
                );
                Ok(vec![
                    word_at(record, "kind"),
                    scale,
                    preference,
                    version,
                    self.sensitivity(record),
                ])
            },
        )
    }

    /// How decisions name a criterion: as a hard condition, a reversal
    /// condition, or otherwise.
    fn sensitivity(&self, record: &Record) -> String {
        let state = self.notes.case().state();
        let mut by_decision: BTreeMap<Id, (bool, bool)> = BTreeMap::new();
        for citation in self.notes.citations(record.header.id) {
            if citation.by.kind() != Kind::Decision
                || !state
                    .current(citation.by)
                    .is_some_and(|decision| self.notes.may_show(decision, Field::Lineage))
            {
                continue;
            }
            let entry = by_decision.entry(citation.by).or_default();
            entry.0 |= citation.field.starts_with("hard_conditions");
            entry.1 |= citation.field.starts_with("reversal_conditions");
        }
        if by_decision.is_empty() {
            return "no decision names it".to_owned();
        }
        let hard = by_decision.values().filter(|(hard, _)| *hard).count();
        let reversal = by_decision.values().filter(|(_, rev)| *rev).count();
        let mut parts = Vec::new();
        if hard > 0 {
            parts.push(format!(
                "hard condition of {}",
                plural(hard, "decision", "decisions")
            ));
        }
        if reversal > 0 {
            parts.push(format!(
                "reversal condition of {}",
                plural(reversal, "decision", "decisions")
            ));
        }
        if parts.is_empty() {
            parts.push(format!(
                "named by {}",
                plural(by_decision.len(), "decision", "decisions")
            ));
        }
        parts.join("; ")
    }

    // ---------------------------------------------------- Comparison matrix

    fn matrix(&self) -> Result<Option<Body>, RenderError> {
        if self.matrix_questions.is_empty() {
            return Ok(None);
        }
        let mut text = String::from(
            "\nOne option-by-criterion table per question, the same one its question and \
             decision notes show. Each recorded cell states its state, outcome, value, \
             unknowns and limits, and links its Comparison; a cell with no comparison reads \
             unknown. No overall score, rank or weighted total is computed. Hard failures and \
             Pareto status follow each table in words. One section per question, in record \
             ID order.\n",
        );
        // Headings are fixed text; the question's title appears only in its link.
        for (index, question) in self.matrix_questions.iter().take(MAX_MATRICES).enumerate() {
            let id = question.header.id;
            let _ = write!(
                text,
                "\n## Question {}\n\n{}\n",
                index + 1,
                self.notes.link(id)?
            );
            let mut facts = Facts {
                notes: self.notes,
                out: &mut text,
                record: question,
            };
            super::matrix::write(&mut facts, id)?;
        }
        if self.matrix_questions.len() > MAX_MATRICES {
            let _ = writeln!(
                text,
                "\nShowing {MAX_MATRICES} of {} questions. Each question note shows its own \
                 matrix; every record is listed under Records on {}.",
                self.matrix_questions.len(),
                self.overview_link()?
            );
        }
        let rows = self
            .matrix_questions
            .iter()
            .filter_map(|question| self.tables.get(&question.header.id))
            .map(|table| table.rows.len())
            .sum();
        Ok(Some(Body { rows, text }))
    }

    // -------------------------------------------------------------- Sources

    fn sources(&self) -> Result<Option<Body>, RenderError> {
        self.records_table(
            &[Kind::Source],
            |_| true,
            ("source", "sources"),
            "Where each source came from and what has been judged about it. A judgment \
             absent here was not made; independence counts the observations drawn from the \
             source by the origin recorded for their claims.",
            &[
                "Source",
                "Origin",
                "Acquisition",
                "Independence",
                "Currency",
                "Credibility",
            ],
            |record| {
                let judgments: Vec<&Value> = record.body["source_assessment"]["judgments"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .collect();
                let latest = |dimension: &str| -> String {
                    judgments
                        .iter()
                        .rev()
                        .find_map(|judgment| {
                            judgment
                                .get(dimension)
                                .filter(|value| !value.is_null())
                                .and_then(|value| value.get("judgment"))
                        })
                        .map_or_else(|| "not judged".to_owned(), |value| word(Some(value)))
                };
                let acquisition = format!(
                    "channel {}, access {}",
                    word_at(record, "acquisition.channel"),
                    word_at(record, "access")
                );
                let mut currency = latest("currency");
                if text_at(&record.body, "status") == Some("withdrawn") {
                    currency = format!("withdrawn; {currency}");
                }
                Ok(vec![
                    latest("origin"),
                    acquisition,
                    self.independence(record),
                    currency,
                    format!(
                        "authenticity {}, reliability {}",
                        latest("authenticity"),
                        latest("reliability")
                    ),
                ])
            },
        )
    }

    /// Visible observations drawn from `source`, by the origin of their claim.
    fn independence(&self, source: &Record) -> String {
        let state = self.notes.case().state();
        let mut counts = [0_usize; 4];
        for observation in self.records.iter().filter(|record| {
            record.header.id.kind() == Kind::Observation
                && self.notes.visible(record)
                && origin_ops::acquisitions(state, record).contains(&source.header.id)
        }) {
            let origin = origin_ops::effective_origin(state, observation);
            let slot = match origin.state {
                OriginState::Known
                    if origin.root.map(|root| root.id) == Some(observation.header.id) =>
                {
                    0
                }
                OriginState::Known => 1,
                OriginState::Disputed => 2,
                OriginState::Unknown => 3,
            };
            counts[slot] += 1;
        }
        let total: usize = counts.iter().sum();
        if total == 0 {
            return "no observation drawn from it".to_owned();
        }
        let labels = [
            "original",
            "restating another",
            "origin disputed",
            "origin untraced",
        ];
        let parts: Vec<String> = counts
            .iter()
            .zip(labels)
            .filter(|(count, _)| **count > 0)
            .map(|(count, label)| format!("{count} {label}"))
            .collect();
        format!(
            "{}: {}",
            plural(total, "observation", "observations"),
            parts.join(", ")
        )
    }

    // ------------------------------------------------------------- Evidence

    fn evidence(&self) -> Result<Option<Body>, RenderError> {
        let state = self.notes.case().state();
        self.records_table(
            &[Kind::Artifact, Kind::Observation],
            |record| !is_measurement(record),
            ("record", "records"),
            "Artifacts, then observations that are not measurements: whether each is \
             original or derivative, its integrity, selector, completeness and sensitivity. \
             Measurements have their own view.",
            &[
                "Evidence",
                "Original or derivative",
                "Integrity",
                "Selector",
                "Completeness",
                "Sensitivity",
            ],
            |record| {
                let body = &record.body;
                if record.header.id.kind() == Kind::Artifact {
                    let role = text_at(body, "role").map_or_else(
                        || {
                            if body["transforms"].as_array().is_none_or(Vec::is_empty) {
                                table_code("original")
                            } else {
                                table_code("derivative")
                            }
                        },
                        |role| table_code(&wire(role)),
                    );
                    let id = record.header.id.to_string();
                    let selecting = self
                        .records
                        .iter()
                        .filter(|other| {
                            other.header.id.kind() == Kind::Observation
                                && self.notes.visible(other)
                                && text_at(&other.body, "basis.artifact_ref.id")
                                    == Some(id.as_str())
                        })
                        .count();
                    let usage = source_ops::evidentiary_use(state, record);
                    return Ok(vec![
                        role,
                        word_at(record, "integrity"),
                        count_or(
                            selecting,
                            "observation selects from it",
                            "observations select from it",
                            "no observation selects from it",
                        ),
                        format!(
                            "availability {}, use {}",
                            word_at(record, "availability"),
                            table_code(&wire_word(&usage.state))
                        ),
                        word_at(record, "sensitivity"),
                    ]);
                }
                let level = self.notes.classifier().effective(record).level;
                let (role, integrity, selector) = if text_at(body, "basis.kind") == Some("source") {
                    (
                        "observation from a source".to_owned(),
                        format!("quote {}", word_at(record, "basis.text_quote.state")),
                        format!(
                            "{} {}",
                            word_at(record, "basis.selector.selector.type"),
                            word_at(record, "basis.selector.validation")
                        ),
                    )
                } else {
                    (
                        "attributed assertion".to_owned(),
                        "not source-backed".to_owned(),
                        "no selector".to_owned(),
                    )
                };
                Ok(vec![
                    role,
                    integrity,
                    selector,
                    format!("validity {}", word_at(record, "validity")),
                    table_code(level_word(level)),
                ])
            },
        )
    }

    // --------------------------------------------------------- Measurements

    fn measurements(&self) -> Result<Option<Body>, RenderError> {
        let state = self.notes.case().state();
        self.records_table(
            &[Kind::Observation],
            is_measurement,
            ("measurement", "measurements"),
            "Each measured value with its unit, conditions and validity, what cites it and \
             the run that produced it. Advisory flags never change validity.",
            &[
                "Measurement",
                "Value",
                "Conditions",
                "Validity and control",
                "Cited by",
                "Run",
            ],
            |record| {
                let value = format!(
                    "{} {}",
                    word_at(record, "value.value"),
                    text_at(&record.body, "value.unit").map_or_else(String::new, table_code)
                );
                let mut conditions = Vec::new();
                if let Some(workload) = text_at(&record.body, "measurement.workload") {
                    conditions.push(format!("workload {}", inline_text(workload, CELL_CHARS)));
                }
                if let Some(samples) =
                    value_at(&record.body, "measurement.sample_count").and_then(scalar_word)
                {
                    conditions.push(format!("{} samples", table_code(&token(&samples))));
                }
                let changed = strings(record.body.get("variables_changed")).len();
                if changed > 0 {
                    conditions.push(plural(changed, "variable changed", "variables changed"));
                }
                let conditions = if conditions.is_empty() {
                    "not recorded".to_owned()
                } else {
                    conditions.join(", ")
                };
                let mut validity = word_at(record, "validity");
                if let Some(report) = measurement_flags::record_flags(state, record)
                    && !report.flags.is_empty()
                {
                    let _ = write!(
                        validity,
                        "; {}",
                        plural(report.flags.len(), "advisory flag", "advisory flags")
                    );
                }
                let cited = citing(self.notes, record, &[]).len();
                let run = match refs(record.body.get("run_ref")).first() {
                    Some(reference) => self.notes.table_link(reference.id)?,
                    None => "none recorded".to_owned(),
                };
                Ok(vec![
                    value.trim_end().to_owned(),
                    conditions,
                    validity,
                    count_or(cited, "record", "records", "nothing cites it yet"),
                    run,
                ])
            },
        )
    }

    // ---------------------------------------------------- Inquiries and Runs

    fn inquiries(&self) -> Result<Option<Body>, RenderError> {
        self.records_table(
            &[Kind::Inquiry, Kind::Run],
            |_| true,
            ("record", "records"),
            "Inquiries, then runs: the plan mode, whether a prediction was frozen before \
             results, the method, the outcome and every incomplete or blocked attempt.",
            &[
                "Inquiry or run",
                "Plan mode",
                "Frozen prediction",
                "Method",
                "Outcome",
                "Incomplete or blocked",
            ],
            |record| {
                let body = &record.body;
                if record.header.id.kind() == Kind::Run {
                    let plan = match refs(value_at(body, "intent.plan_ref")).first() {
                        Some(reference) => {
                            format!("under plan {}", self.notes.table_link(reference.id)?)
                        }
                        None => "no plan named".to_owned(),
                    };
                    let complete = text_at(body, "execution_status") == Some("completed")
                        && text_at(body, "evidence_completeness") == Some("complete");
                    return Ok(vec![
                        format!("run, intent {}", word_at(record, "intent.kind")),
                        plan,
                        text_or(
                            text_at(body, "tool").or_else(|| text_at(body, "environment")),
                            "not recorded",
                        ),
                        format!(
                            "execution {}, validity {}",
                            word_at(record, "execution_status"),
                            word_at(record, "evidence_validity")
                        ),
                        if complete {
                            "no".to_owned()
                        } else {
                            format!(
                                "yes: execution {}, completeness {}",
                                word_at(record, "execution_status"),
                                word_at(record, "evidence_completeness")
                            )
                        },
                    ]);
                }
                let frozen = if text_at(body, "lifecycle") != Some("frozen") {
                    format!("not frozen ({})", word_at(record, "lifecycle"))
                } else if text_at(body, "inquiry.mode") == Some("confirmatory") {
                    let predictions = body["inquiry"]["confirmatory"]["predictions"]
                        .as_array()
                        .map_or(0, Vec::len);
                    format!(
                        "frozen before results, {}",
                        plural(predictions, "prediction", "predictions")
                    )
                } else {
                    "plan frozen before results; exploratory, so no prediction".to_owned()
                };
                let runs: Vec<&Record> = citing(self.notes, record, &["intent.plan_ref"])
                    .into_iter()
                    .map(|(run, _)| run)
                    .filter(|run| run.header.id.kind() == Kind::Run)
                    .collect();
                let incomplete = runs
                    .iter()
                    .filter(|run| {
                        text_at(&run.body, "execution_status") != Some("completed")
                            || text_at(&run.body, "evidence_completeness") != Some("complete")
                    })
                    .count();
                Ok(vec![
                    word_at(record, "inquiry.mode"),
                    frozen,
                    text_or(text_at(body, "method"), "not recorded"),
                    count_or(
                        runs.len(),
                        "run recorded",
                        "runs recorded",
                        "no run recorded",
                    ),
                    if runs.is_empty() {
                        "no run yet".to_owned()
                    } else {
                        format!("{incomplete} of {} runs", runs.len())
                    },
                ])
            },
        )
    }

    // ---------------------------------------------------------- Conclusions

    fn conclusions(&self) -> Result<Option<Body>, RenderError> {
        self.records_table(
            &[Kind::Conclusion],
            |_| true,
            ("conclusion", "conclusions"),
            "Each bounded claim with its effective disposition (from freshness and the \
             objection policy, before the recorded lifecycle), what contradicts it, the \
             review it rests on and where it applies.",
            &[
                "Conclusion",
                "Effective disposition",
                "Contradiction",
                "Review basis",
                "Applicability",
            ],
            |record| {
                let contradictions = refs(record.body.get("contradiction_refs")).len();
                let review = match refs(record.body.get("review_ref")).first() {
                    Some(reference) => {
                        let mut text = self.notes.table_link(reference.id)?;
                        if let Some(review) = self.notes.case().state().current(reference.id)
                            && self.notes.visible(review)
                        {
                            let _ = write!(
                                text,
                                ", {}",
                                table_code(super::reviews::freshness_word(self.notes, review))
                            );
                        }
                        text
                    }
                    None => "not validated: no accepted review".to_owned(),
                };
                Ok(vec![
                    self.effective(record),
                    count_or(
                        contradictions,
                        "contradicting record",
                        "contradicting records",
                        "none recorded",
                    ),
                    review,
                    text_or(text_at(&record.body, "scope"), "not recorded"),
                ])
            },
        )
    }

    /// Effective state, with the recorded lifecycle when they differ.
    fn effective(&self, record: &Record) -> String {
        let recorded = lifecycle(record).map_or_else(|| "recorded".to_owned(), wire);
        match effective_word(self.notes, record) {
            Some(effective) if effective != recorded => format!(
                "{} (recorded {})",
                table_code(&effective),
                table_code(&recorded)
            ),
            Some(effective) => table_code(&effective),
            None => table_code(&recorded),
        }
    }

    // ------------------------------------------------------------ Decisions

    fn decisions(&self) -> Result<Option<Body>, RenderError> {
        self.records_table(
            &[Kind::Decision],
            |_| true,
            ("decision", "decisions"),
            "Each decision's effective state, its choice, what it read, its stated \
             trade-offs and consequences, and what would reopen it.",
            &[
                "Decision",
                "Effective state",
                "Chosen option",
                "Criteria and evidence",
                "Trade-offs",
                "Consequences",
                "Reconsider when",
            ],
            |record| {
                let body = &record.body;
                let chosen = match refs(value_at(body, "choice.option_ref")).first() {
                    Some(reference) => self.notes.table_link(reference.id)?,
                    None => format!("no option chosen ({})", word_at(record, "choice.kind")),
                };
                let read = format!(
                    "{}, {}, {}",
                    plural(
                        refs(body.get("comparison_refs")).len(),
                        "comparison",
                        "comparisons"
                    ),
                    plural(
                        body["hard_conditions"].as_array().map_or(0, Vec::len),
                        "hard condition",
                        "hard conditions"
                    ),
                    plural(
                        refs(body.get("evidence_refs")).len(),
                        "evidence record",
                        "evidence records"
                    )
                );
                let reversals = body["reversal_conditions"].as_array().map_or(0, Vec::len);
                let mut reconsider = text_or(text_at(body, "reopen_trigger"), "no trigger stated");
                if reversals > 0 {
                    let _ = write!(
                        reconsider,
                        "; {}",
                        plural(reversals, "reversal condition", "reversal conditions")
                    );
                }
                Ok(vec![
                    self.effective(record),
                    chosen,
                    read,
                    first_of(&strings(body.get("tradeoffs")), "none recorded"),
                    first_of(&strings(body.get("consequences")), "none recorded"),
                    reconsider,
                ])
            },
        )
    }

    // -------------------------------------------------------------- Reviews

    fn reviews(&self) -> Result<Option<Body>, RenderError> {
        self.records_table(
            &[Kind::Review],
            |_| true,
            ("review", "reviews"),
            "Validation and blindspot reviews: what each examined, the gaps it left \
             unresolved, the audit rule version it read and whether it still covers its \
             basis. Zero gaps means none was identified within the examined scope, not that \
             none exists.",
            &[
                "Review",
                "Kind",
                "Subject",
                "Verdict",
                "Checks and assumptions reviewed",
                "Unresolved gaps",
                "Audit rule version",
                "Freshness",
            ],
            |record| {
                let body = &record.body;
                let subject = match refs(body.get("subject_ref")).first() {
                    Some(reference) => {
                        let mut text = self.notes.table_link(reference.id)?;
                        if let Some(current) = self.notes.case().state().current(reference.id)
                            && current.header.revision.0 != reference.revision
                        {
                            let _ = write!(
                                text,
                                " (reviewed revision {}, now {})",
                                reference.revision, current.header.revision.0
                            );
                        }
                        text
                    }
                    None => "the case as a whole".to_owned(),
                };
                let accepted = body["gap_dispositions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|disposition| disposition["disposition"] == "accepted")
                    .count();
                let gaps = strings(body.get("gaps")).len()
                    + body["novel_gaps"].as_array().map_or(0, Vec::len)
                    + accepted;
                let freshness = super::reviews::freshness_word(self.notes, record);
                Ok(vec![
                    word_at(record, "kind"),
                    subject,
                    word_at(record, "verdict"),
                    format!(
                        "{}, {}",
                        plural(
                            strings(body.get("checks_considered")).len(),
                            "check considered",
                            "checks considered"
                        ),
                        plural(
                            refs(body.get("assumption_refs")).len(),
                            "assumption",
                            "assumptions"
                        )
                    ),
                    count_or(gaps, "identified", "identified", "none identified"),
                    text_at(body, "audit.check_version")
                        .map_or_else(|| "no audit named".to_owned(), table_code),
                    if freshness == "stale" {
                        format!("{}: its read set changed", table_code(freshness))
                    } else {
                        table_code(freshness)
                    },
                ])
            },
        )
    }

    // ------------------------------------------------------- Retrospectives

    fn retrospectives(&self) -> Result<Option<Body>, RenderError> {
        self.records_table(
            &[Kind::Retrospective],
            |_| true,
            ("retrospective", "retrospectives"),
            "What each retrospective found: methods tried, what discriminated, what misled, \
             and lessons with their transfer limits. A lesson is a bounded proposal; nothing \
             here changes doctrine automatically.",
            &[
                "Retrospective",
                "Status",
                "Methods tried",
                "Useful discriminators",
                "Invalid or misleading paths",
                "Bounded improvements and transfer limits",
            ],
            |record| {
                let body = &record.body;
                let methods: Vec<&str> = body["methods_attempted"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item["name"].as_str())
                    .collect();
                // A review export leaves out every step or lesson citing a
                // record it does not show (bn-91gz).
                let field = |list: &str, key: &str| -> Vec<&str> {
                    self.notes
                        .retrospective_entries(record, list)
                        .into_iter()
                        .filter_map(|item| item[key].as_str())
                        .collect()
                };
                // Recorded entries none of which is shown are not "none
                // recorded".
                let none = |list: &str| {
                    if body[list].as_array().is_some_and(|all| !all.is_empty()) {
                        "none shown to this audience"
                    } else {
                        "none recorded"
                    }
                };
                let lessons = self.notes.retrospective_entries(record, "lessons");
                let recorded = body["lessons"]
                    .as_array()
                    .is_some_and(|all| !all.is_empty());
                let lesson = match lessons.as_slice() {
                    [] if recorded => "none shown to this audience".to_owned(),
                    [] => match text_at(body, "no_reusable_lesson_reason") {
                        Some(reason) => {
                            format!("no reusable lesson: {}", inline_text(reason, CELL_CHARS))
                        }
                        None => "none recorded".to_owned(),
                    },
                    [first, rest @ ..] => {
                        let mut text = format!(
                            "{} (transfer limits: {})",
                            text_or(first["guidance"].as_str(), "no guidance stated"),
                            text_or(first["transfer_limits"].as_str(), "not stated")
                        );
                        if !rest.is_empty() {
                            let _ = write!(text, " and {} more", rest.len());
                        }
                        text
                    }
                };
                Ok(vec![
                    format!(
                        "{}, {}",
                        word_at(record, "status"),
                        table_code(super::reviews::freshness_word(self.notes, record))
                    ),
                    first_of(&methods, "none recorded"),
                    first_of(
                        &field("effective_steps", "description"),
                        none("effective_steps"),
                    ),
                    first_of(
                        &field("invalid_or_misleading_steps", "description"),
                        none("invalid_or_misleading_steps"),
                    ),
                    lesson,
                ])
            },
        )
    }

    // ------------------------------------------------------------- Frontier

    fn frontier(&self) -> Result<Body, RenderError> {
        let case = self.notes.case();
        let command = |name: &str| {
            inline_code(&format!(
                "{PROGRAM_NAME} --case {} {name} --format json",
                case.case_id()
            ))
        };
        let mut text = format!(
            "\nOpen, blocked and stale uncertainty at this snapshot: every unresolved gap the \
             structural rules emit (rule version {}), with the prerequisite evidence each \
             needs. One row per gap, in gap ID order, unranked. A row describes missing or \
             stale recorded information; it is not a task, an assignment or an external \
             board, and nothing here is dispatched. Zero gaps means these rules detected \
             none, not that the investigation is complete. {} ranks the next steps.\n\n",
            inline_code(GAP_RULE_VERSION),
            command("next")
        );
        let Ok(inventory) = self.notes.gap_inventory() else {
            text.push_str("The gap inventory is unavailable at this snapshot.\n");
            return Ok(Body { rows: 0, text });
        };
        let resolved = inventory
            .gaps
            .iter()
            .filter(|gap| gap.status == GapStatus::Resolved)
            .count();
        let mut gaps: Vec<_> = inventory
            .gaps
            .iter()
            .filter(|gap| gap.status != GapStatus::Resolved)
            .collect();
        gaps.sort_by(|a, b| a.gap_id.cmp(&b.gap_id));
        let headers = [
            "Gap rule",
            "State",
            "Subjects",
            "Prerequisite evidence",
            "Why it is open",
            "Decision effect",
        ];
        if gaps.is_empty() {
            text.push_str("No unresolved gap is detected at this snapshot.\n");
        } else {
            header(&mut text, &headers);
        }
        for gap in gaps.iter().take(MAX_ROWS) {
            let state = match gap.status {
                GapStatus::ReadyToAnalyze => "open: ready to analyze",
                GapStatus::RequiresSource => "open: requires a source",
                GapStatus::RequiresExternalAction => "blocked: requires external action",
                GapStatus::BlockedByQuality => "blocked by evidence quality",
                GapStatus::StaleDueToRevision => "stale after a revision",
                GapStatus::Deferred => "deferred",
                GapStatus::Resolved => "resolved",
            };
            let mut subjects: Vec<String> = gap
                .subject_refs
                .iter()
                .take(MAX_SUBJECTS)
                .map(|reference| self.notes.table_link(reference.id))
                .collect::<Result<_, _>>()?;
            if gap.subject_refs.len() > MAX_SUBJECTS {
                subjects.push(format!(
                    "and {} more",
                    gap.subject_refs.len() - MAX_SUBJECTS
                ));
            }
            let subjects = if subjects.is_empty() {
                "the case".to_owned()
            } else {
                subjects.join(", ")
            };
            let mut cells = vec![table_code(&gap.rule_id), state.to_owned(), subjects];
            if self.notes.gap_withheld(&gap.gap_id) {
                cells.extend(std::iter::repeat_n(
                    "withheld: the rule read a withheld record".to_owned(),
                    3,
                ));
            } else {
                let prerequisites: Vec<String> = gap
                    .prerequisites
                    .iter()
                    .map(|prerequisite| {
                        format!(
                            "{} {}",
                            table_code(&prerequisite.name),
                            match prerequisite.state {
                                PrerequisiteState::Met => "met",
                                PrerequisiteState::Unmet => "unmet",
                                PrerequisiteState::Unknown => "unknown",
                            }
                        )
                    })
                    .collect();
                cells.push(if prerequisites.is_empty() {
                    "none recorded".to_owned()
                } else {
                    prerequisites.join("; ")
                });
                cells.push(inline_text(&gap.reason, 2 * CELL_CHARS));
                cells.push(table_code(&wire(&gap.decision_effect)));
            }
            line(&mut text, &cells);
        }
        if gaps.len() > MAX_ROWS {
            let _ = writeln!(
                text,
                "\nShowing {MAX_ROWS} of {} unresolved gaps. {} lists every gap.",
                gaps.len(),
                command("frontier")
            );
        }
        if resolved > 0 {
            let _ = writeln!(
                text,
                "\n{} not listed.",
                plural(resolved, "resolved gap is", "resolved gaps are")
            );
        }
        self.notes_table(
            &mut text,
            FRONTIER_NOTES,
            "The notes the unresolved gaps name, with how many name each (`inv_open_gaps`).",
            |record| self.notes.open_gap_count(record.header.id).unwrap_or(0) > 0,
            &["Record", "Kind", "Open gaps", "Effective state"],
            |record| {
                let count = self.notes.open_gap_count(record.header.id).unwrap_or(0);
                vec![
                    layout::kind_name(record.header.id.kind()).to_owned(),
                    count.to_string(),
                    self.effective_state(record),
                ]
            },
        )?;
        Ok(Body {
            rows: gaps.len(),
            text,
        })
    }

    // ------------------------------------------------------------- Timeline

    fn timeline(&self) -> Result<Body, RenderError> {
        let case = self.notes.case();
        let state = case.state();
        let mut ids: Vec<Id> = self.records.iter().map(|record| record.header.id).collect();
        if state
            .current(case.case_id())
            .is_some_and(|record| self.notes.may_show(record, Field::Identity))
        {
            ids.push(case.case_id());
        }
        let mut revisions: Vec<(Option<&Record>, &Record)> = Vec::new();
        for id in ids {
            let mut previous = None;
            for revision in state.history(id) {
                revisions.push((previous, revision));
                previous = Some(revision);
            }
        }
        revisions.sort_by_key(|(_, revision)| {
            (
                revision.header.acceptance_sequence,
                revision.header.id,
                revision.header.revision.0,
            )
        });
        let mut text = String::from(
            "\nEvery accepted record revision of this case, in acceptance order. Recorded is \
             when this workspace accepted the revision, and it alone decides order. Observed, \
             started or acquired is the time a source, producer or recorder claims, with the \
             clock quality it claims; it is provenance, never order. Change names lifecycle transitions (including \
             changed decisions), corrections, retractions, supersession and the stated \
             reason. One row per revision.\n\n",
        );
        header(
            &mut text,
            &[
                "Sequence",
                "Record",
                "Change",
                "Observed, started or acquired (claimed)",
                "Recorded",
                "Recorded by",
            ],
        );
        let skip = revisions.len().saturating_sub(MAX_ROWS);
        for (previous, revision) in revisions.iter().skip(skip) {
            let id = revision.header.id;
            let current = state.current(id).unwrap_or(*revision);
            let positions = self.notes.shows_positions();
            let mut cells = vec![
                if positions {
                    revision.header.acceptance_sequence.to_string()
                } else {
                    NOT_SHOWN_CELL.to_owned()
                },
                format!(
                    "{}, {} revision {}",
                    self.notes.table_link(id)?,
                    layout::record_kind(id.kind())
                        .map_or_else(|| layout::kind_name(id.kind()), |(_, name)| name),
                    revision.header.revision.0
                ),
            ];
            if self.notes.visible(revision) && self.notes.visible(current) {
                let content = self.notes.may_show(revision, Field::Content)
                    && self.notes.may_show(current, Field::Content);
                cells.extend([
                    self.change(*previous, revision, content)?,
                    claimed_time(revision),
                    if positions {
                        table_code(&revision.header.recorded_at.to_string())
                    } else {
                        NOT_SHOWN_CELL.to_owned()
                    },
                    table_code(&revision.header.recorded_by.to_string()),
                ]);
            } else {
                cells.extend(std::iter::repeat_n("withheld".to_owned(), 4));
            }
            line(&mut text, &cells);
        }
        if skip > 0 {
            let _ = writeln!(
                text,
                "\nShowing the latest {MAX_ROWS} of {} revisions. Each record note lists its \
                 earlier revisions; every record is listed under Records on {}.",
                revisions.len(),
                self.overview_link()?
            );
        }
        self.notes_table(
            &mut text,
            TIMELINE_NOTES,
            "Each note of this case at its latest revision, with the time this workspace \
             recorded that revision.",
            |_| true,
            &["Record", "Kind", "Revision", "Recorded"],
            |record| {
                let recorded = if !self.notes.shows_positions() {
                    NOT_SHOWN_CELL.to_owned()
                } else if self.notes.visible(record) {
                    table_code(&record.header.recorded_at.to_string())
                } else {
                    "withheld".to_owned()
                };
                vec![
                    layout::kind_name(record.header.id.kind()).to_owned(),
                    record.header.revision.0.to_string(),
                    recorded,
                ]
            },
        )?;
        Ok(Body {
            rows: revisions.len(),
            text,
        })
    }

    /// The case record and every record of this case with a note whose
    /// identity the audience may see, in record ID order: the notes a Base
    /// over this case can list (view notes aside).
    fn noted(&self) -> Vec<&'a Record> {
        let case = self.notes.case();
        let mut noted: Vec<&'a Record> = self
            .records
            .iter()
            .copied()
            .filter(|record| layout::record_kind(record.header.id.kind()).is_some())
            .collect();
        if let Some(record) = case
            .state()
            .current(case.case_id())
            .filter(|record| self.notes.may_show(record, Field::Identity))
        {
            noted.push(record);
        }
        noted.sort_by_key(|record| record.header.id);
        noted
    }

    /// The effective state a note's `inv_effective` property shows, or
    /// `withheld` when the note withholds it.
    fn effective_state(&self, record: &Record) -> String {
        if !self.notes.visible(record) {
            return "withheld".to_owned();
        }
        table_code(
            self.notes
                .effective(record)
                .map_or("unknown", |effective| effective.freshness.as_str()),
        )
    }

    /// A `## heading` section with one table row per note of [`Self::noted`]
    /// that `keep` admits, in record ID order and bounded, the rows a Base
    /// lists. The first column links the note; `row` gives the rest.
    fn notes_table(
        &self,
        text: &mut String,
        heading: &str,
        intro: &str,
        keep: impl Fn(&Record) -> bool,
        headers: &[&str],
        row: impl Fn(&Record) -> Vec<String>,
    ) -> Result<(), RenderError> {
        let selected: Vec<&Record> = self
            .noted()
            .into_iter()
            .filter(|record| keep(record))
            .collect();
        let _ = write!(
            text,
            "\n## {heading}\n\n{intro} One row per note, in record ID order.\n\n"
        );
        if selected.is_empty() {
            text.push_str("No note is listed here at this snapshot.\n");
            return Ok(());
        }
        header(text, headers);
        for record in selected.iter().take(MAX_ROWS) {
            let mut cells = vec![self.notes.table_link(record.header.id)?];
            cells.extend(row(record));
            line(text, &cells);
        }
        if selected.len() > MAX_ROWS {
            let _ = writeln!(
                text,
                "\nShowing {MAX_ROWS} of {} notes. Every record is listed under Records on {}.",
                selected.len(),
                self.overview_link()?
            );
        }
        Ok(())
    }

    /// What one revision changed relative to the one before it, in words.
    fn change(
        &self,
        previous: Option<&Record>,
        revision: &Record,
        content: bool,
    ) -> Result<String, RenderError> {
        let Some(previous) = previous else {
            return Ok("created".to_owned());
        };
        let body = &revision.body;
        let mut parts = Vec::new();
        let (before, after) = (lifecycle(previous), lifecycle(revision));
        if before != after {
            let decision = if revision.header.id.kind() == Kind::Decision {
                "decision "
            } else {
                ""
            };
            parts.push(format!(
                "{decision}{} to {}",
                table_code(&before.map_or_else(|| "none".to_owned(), wire)),
                table_code(&after.map_or_else(|| "none".to_owned(), wire))
            ));
        }
        let changed = |key: &str| {
            body.get(key).is_some_and(|value| !value.is_null())
                && body.get(key) != previous.body.get(key)
        };
        if changed("corrects") {
            let mut text = "correction".to_owned();
            if let Some(reference) = refs(body.get("corrects")).first() {
                if reference.id == revision.header.id {
                    let _ = write!(text, " of revision {}", reference.revision);
                } else {
                    let _ = write!(text, " of {}", self.notes.table_link(reference.id)?);
                }
            }
            parts.push(text);
        }
        if body["retracted"] == true && previous.body["retracted"] != true {
            parts.push("retracted".to_owned());
        }
        for (key, words) in [
            ("source_assessment", "source judged"),
            ("claim_origin", "claim origin traced"),
            ("withdrawals", "withdrawal recorded"),
        ] {
            if changed(key) {
                parts.push(words.to_owned());
            }
        }
        for (key, words) in [
            ("supersedes", "supersedes"),
            ("superseded_by", "superseded by"),
        ] {
            if changed(key)
                && let Some(reference) = refs(body.get(key)).first()
            {
                parts.push(format!("{words} {}", self.notes.table_link(reference.id)?));
            }
        }
        if content
            && let Some(reason) = REASON_KEYS.iter().find_map(|key| {
                text_at(body, key)
                    .filter(|text| !text.trim().is_empty())
                    .filter(|text| text_at(&previous.body, key) != Some(*text))
            })
        {
            parts.push(format!("reason: {}", inline_text(reason, CELL_CHARS)));
        }
        Ok(if parts.is_empty() {
            "revised".to_owned()
        } else {
            format!("revised: {}", parts.join("; "))
        })
    }
}

fn is_measurement(record: &Record) -> bool {
    record.header.id.kind() == Kind::Observation
        && text_at(&record.body, "value.kind") == Some("measurement")
}

/// The time a revision's source or recorder claims, or a statement that
/// none is claimed.
fn claimed_time(record: &Record) -> String {
    let shown = |label: &str, time: &SourceTime| -> String {
        match &time.timestamp {
            Knowledge::Known(timestamp) => format!(
                "{label} {} (clock {})",
                table_code(timestamp),
                table_code(&wire_word(&time.clock_quality))
            ),
            Knowledge::Unknown => format!("{label} at an unknown time"),
        }
    };
    if let Some(time) = &record.header.observed_at {
        return shown("observed", time);
    }
    if let Some(time) = record
        .body
        .get("observed_at")
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::from_value::<SourceTime>(value.clone()).ok())
    {
        return shown("observed", &time);
    }
    if let Some(time) = record
        .body
        .get("started_at")
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::from_value::<SourceTime>(value.clone()).ok())
    {
        return shown("started", &time);
    }
    if let Some(time) = &record.header.acquired_at {
        return shown("acquired", time);
    }
    "none claimed".to_owned()
}

/// `| a | b |` and the separator row.
fn header(out: &mut String, headers: &[&str]) {
    line(
        out,
        &headers
            .iter()
            .map(|text| (*text).to_owned())
            .collect::<Vec<_>>(),
    );
    out.push('|');
    for _ in headers {
        out.push_str("---|");
    }
    out.push('\n');
}

/// One table row. A cell never holds a line break, and an empty cell reads
/// `none`, so the row reads the same in a plain-text viewer.
fn line(out: &mut String, cells: &[String]) {
    out.push('|');
    for cell in cells {
        debug_assert!(!cell.contains('\n'), "cell with a line break: {cell:?}");
        let cell = cell.trim();
        let _ = write!(out, " {} |", if cell.is_empty() { "none" } else { cell });
    }
    out.push('\n');
}

/// A recorded enum word, number or flag as a cell, or `not recorded`.
fn word(value: Option<&Value>) -> String {
    value
        .filter(|value| !value.is_null())
        .and_then(scalar_word)
        .map_or_else(
            || "not recorded".to_owned(),
            |word| table_code(&token(&word)),
        )
}

fn word_at(record: &Record, path: &str) -> String {
    word(value_at(&record.body, path))
}

/// Bounded escaped authored text, or `none` when absent or blank.
fn text_or(text: Option<&str>, none: &str) -> String {
    text.filter(|text| !text.trim().is_empty())
        .map_or_else(|| none.to_owned(), |text| inline_text(text, CELL_CHARS))
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn count_or(count: usize, one: &str, many: &str, none: &str) -> String {
    if count == 0 {
        none.to_owned()
    } else {
        plural(count, one, many)
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

/// The first item, bounded, and how many more.
fn first_of(items: &[&str], none: &str) -> String {
    match items {
        [] => none.to_owned(),
        [only] => inline_text(only, CELL_CHARS),
        [first, rest @ ..] => format!("{} and {} more", inline_text(first, CELL_CHARS), rest.len()),
    }
}
