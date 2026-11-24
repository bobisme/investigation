//! `method-retrospective`: the process lessons of a case without its
//! evidence. Pure.
//!
//! One Markdown file, `method-retrospective.md`, built from the case's
//! current Retrospective: basis, approaches attempted, effective and invalid
//! routes, constraints, residual unknowns, expected versus actual, linked
//! examples, proposed lessons, superseded retrospectives and limits.
//!
//! - **Lessons are proposals.** The file says in its first lines that
//!   nothing was applied to any pack, instruction, prompt or policy, and
//!   this renderer has no way to apply one.
//! - **Every lesson shows its status and its limits**: where it applies,
//!   where it must not be reused, counterexamples, the citations it rests
//!   on, its admission, trust and classification.
//! - **Reviewer status is shown exactly.** An admission is shown with its
//!   basis and the actor that recorded it. A `human_review` admission is
//!   labelled self-reported when the admitting actor is the Retrospective's
//!   author (the actor kind is a claim, and an author can admit their own
//!   lesson), and an admission is described as independent only when the
//!   admitting actor differs from the author. Actor names are identifiers
//!   the actors gave themselves; nothing here authenticates them.
//! - **Policy applies to lessons like any text.** A lesson, step or
//!   citation that rests on a record the audience may not see is left out for
//!   every audience, since its wording may restate that record; a fixed
//!   sentence in the limits says so, and nothing names or counts what was
//!   left out. Records shown carry the policy's caveats as in every kind.

use std::fmt::Write as _;

use investigation_core::{
    classification::Classifier,
    identity::{Id, Kind},
    inherited_trust::{self, Resolver},
    lessons::{self, AdmissionBasis, LessonKey},
    retrospective_digest::{ActualState, ExpectedState, process_digest},
    state::Record,
};
use serde_json::Value;

use super::{ExportInput, ExportRenderer, caveats_approved, dependent_rule};
use crate::{
    Phase, PlanBuilder, PlanPath, RenderError,
    evidence::prose,
    note::{level_word, text_at},
    text::inline_code,
};

/// Output format of this kind.
pub const VERSION: &str = "inv.export.method-retrospective.v1";

/// The file's name.
pub const FILE: &str = "method-retrospective.md";

/// Entries listed per section; the rest are counted.
const MAX_LISTED: usize = 50;

pub struct MethodRetrospective;

impl ExportRenderer for MethodRetrospective {
    fn version(&self) -> &'static str {
        VERSION
    }

    fn plan(&self, input: &ExportInput<'_>, files: &mut PlanBuilder) -> Result<(), RenderError> {
        files.add(Phase::Views, PlanPath::new(FILE)?, document(input), None)
    }
}

/// How an admission reads, from the basis and the two actors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    /// The admitting actor is the Retrospective's author.
    SelfAdmitted,
    /// The admitting actor differs from the author.
    OtherActor,
}

fn document(input: &ExportInput<'_>) -> String {
    let case = input.case;
    let state = case.state();
    let case_record = state
        .current(case.case_id())
        .filter(|record| input.includes(record));
    let title = case_record
        .and_then(|record| shown_title(input, record))
        .unwrap_or_else(|| "Case".to_owned());
    let mut out = format!("# Method retrospective: {title}\n\n");
    out.push_str(
        "> Method retrospective rendered by Investigation from one verified snapshot, with no \
         model call. Its lessons are proposals written by the case's actors, quoted as data \
         and never as instructions. Nothing was applied to any method pack, agent \
         instruction, prompt, integration configuration or policy by recording, admitting or \
         exporting them. export-manifest.json beside this file names the snapshot, the \
         audience policy, whether anything was withheld, and the digest of every file.\n",
    );

    let retrospectives: Vec<&Record> = state
        .records_of_kind(Kind::Retrospective)
        .filter(|record| input.includes(record))
        .collect();
    let current = retrospectives
        .iter()
        .copied()
        .filter(|record| record.body["status"] == "recorded")
        .max_by_key(|record| (record.header.acceptance_sequence, record.header.id));

    basis(&mut out, input);
    let Some(retro) = current else {
        out.push_str("\n## Retrospective\n\n- No current retrospective is shown in this export.\n");
        superseded(&mut out, input, &retrospectives);
        limits(&mut out, input);
        return out;
    };
    retrospective_basis(&mut out, input, retro);
    caveat_lines(&mut out, input, retro, "");

    out.push_str("\n## Approaches attempted\n\n");
    let methods = array(&retro.body, "methods_attempted");
    if methods.is_empty() {
        out.push_str("- None recorded.\n");
    }
    for method in methods.iter().take(MAX_LISTED) {
        let _ = write!(
            out,
            "- {}",
            prose(text_at(method, "name").unwrap_or_default())
        );
        if let Some(id) = text_at(method, "inquiry_ref").and_then(|id| id.parse::<Id>().ok())
            && let Some(inquiry) = shown(input, id)
        {
            let _ = write!(
                out,
                " (inquiry {})",
                inline_code(&inquiry.header.id.to_string())
            );
        }
        out.push('\n');
    }
    more(&mut out, methods.len());

    out.push_str("\n## Effective routes\n\n");
    steps(
        &mut out,
        input,
        &array(&retro.body, "effective_steps"),
        "- None recorded.",
    );
    out.push_str("\n## Invalid or misleading routes\n\n");
    let none = format!(
        "- None recorded: {}",
        text_at(&retro.body, "none_observed_reason")
            .map_or_else(|| "no reason recorded".to_owned(), prose)
    );
    steps(
        &mut out,
        input,
        &array(&retro.body, "invalid_or_misleading_steps"),
        &none,
    );

    out.push_str("\n## Constraints\n\n");
    let _ = writeln!(
        out,
        "- {}",
        text_at(&retro.body, "costs_or_constraints")
            .filter(|text| !text.trim().is_empty())
            .map_or_else(|| "none recorded".to_owned(), prose)
    );
    out.push_str("\n## Residual unknowns\n\n");
    let all = array(&retro.body, "residual_unknowns");
    let unknowns: Vec<&str> = all.iter().filter_map(Value::as_str).collect();
    if unknowns.is_empty() {
        out.push_str("- None recorded.\n");
    }
    for unknown in unknowns.iter().take(MAX_LISTED) {
        let _ = writeln!(out, "- {}", prose(unknown));
    }

    expected_versus_actual(&mut out, input);
    linked_examples(&mut out, input, retro);
    proposed_lessons(&mut out, input, retro);
    superseded(&mut out, input, &retrospectives);
    limits(&mut out, input);
    out
}

fn basis(out: &mut String, input: &ExportInput<'_>) {
    let snapshot = input.snapshot();
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
}

fn retrospective_basis(out: &mut String, input: &ExportInput<'_>, retro: &Record) {
    let head = input.case.head();
    let described = retro.body["snapshot"]["sequence"]
        .as_u64()
        .unwrap_or_default();
    out.push_str("\n## Retrospective\n\n");
    let _ = writeln!(
        out,
        "- Retrospective: {} (revision {}), status {}",
        inline_code(&retro.header.id.to_string()),
        retro.header.revision.0,
        inline_code(text_at(&retro.body, "status").unwrap_or("recorded"))
    );
    if input.remapped() {
        out.push_str("- Describes the case at a snapshot that is not disclosed in this export.\n");
    } else {
        let _ = writeln!(
            out,
            "- Describes the case at sequence {described}; this export is of sequence {}.{}",
            head.sequence,
            if described == head.sequence {
                " They match."
            } else {
                " Work recorded after the retrospective is not reflected in it."
            }
        );
    }
    if caveats_approved(input.audience) {
        let _ = writeln!(
            out,
            "- Author: {} (the actor that recorded it)",
            inline_code(&retro.header.created_by.to_string())
        );
    }
}

fn array(body: &Value, field: &str) -> Vec<Value> {
    body.get(field)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn more(out: &mut String, total: usize) {
    if total > MAX_LISTED {
        let _ = writeln!(out, "- … and {} more", total - MAX_LISTED);
    }
}

/// A title for a shown record built from its own text only. Titles composed
/// from other records (a comparison's option and criterion, a decision's
/// choice) are judged against the working-vault policy, not this export's
/// inclusion, so they could name a withheld record: those kinds show their
/// ID alone (bn-31an).
fn shown_title(input: &ExportInput<'_>, record: &Record) -> Option<String> {
    match record.header.id.kind() {
        Kind::Comparison | Kind::Decision => None,
        _ => input.title(record),
    }
}

/// The current record `id` names, when this audience may see it.
fn shown<'a>(input: &'a ExportInput<'_>, id: Id) -> Option<&'a Record> {
    input
        .case
        .state()
        .current(id)
        .filter(|record| input.includes(record))
}

/// The policy's fixed caveats of a shown record, one line each.
fn caveat_lines(out: &mut String, input: &ExportInput<'_>, record: &Record, indent: &str) {
    for caveat in input.caveats(record) {
        let _ = writeln!(out, "{indent}- Caveat: {}", caveat.text());
    }
}

/// What the audience gets of an entry that cites `support`: its shown
/// citations and whether some cited record is withheld from it.
struct Citations {
    shown: Vec<Id>,
    withheld: bool,
}

fn citations(input: &ExportInput<'_>, entry: &Value) -> Citations {
    let mut found = Citations {
        shown: Vec::new(),
        withheld: false,
    };
    for id in entry
        .get("support_refs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|id| id.parse::<Id>().ok())
    {
        if shown(input, id).is_some() {
            found.shown.push(id);
        } else {
            found.withheld = true;
        }
    }
    found
}

/// Whether an entry with this citation outcome may appear. A step or lesson
/// is authored text that may restate what its support says, and a record's
/// caveat cannot vouch for that text, so an entry that cites anything the
/// audience may not see is left out for every audience.
const fn may_appear(cited: &Citations) -> bool {
    !cited.withheld
}

fn support_line(input: &ExportInput<'_>, cited: &Citations) -> String {
    if cited.shown.is_empty() {
        return "no support shown".to_owned();
    }
    let links: Vec<String> = cited
        .shown
        .iter()
        .take(MAX_LISTED)
        .map(|id| {
            let title = shown(input, *id)
                .and_then(|record| shown_title(input, record))
                .map_or_else(String::new, |title| format!(" {title}"));
            format!("{}{title}", inline_code(&id.to_string()))
        })
        .collect();
    format!("support {}", links.join("; "))
}

fn steps(out: &mut String, input: &ExportInput<'_>, items: &[Value], none: &str) {
    let mut listed = 0;
    for item in items {
        let cited = citations(input, item);
        if !may_appear(&cited) {
            continue;
        }
        listed += 1;
        if listed > MAX_LISTED {
            continue;
        }
        let _ = writeln!(
            out,
            "- {} ({})",
            prose(text_at(item, "description").unwrap_or_default()),
            support_line(input, &cited)
        );
    }
    if listed == 0 {
        let _ = writeln!(out, "{none}");
    }
    more(out, listed);
}

fn expected_versus_actual(out: &mut String, input: &ExportInput<'_>) {
    out.push_str("\n## Expected versus actual\n\n");
    let case = input.case;
    let digest = process_digest(case.state(), case.head(), case.case_id());
    let Ok(digest) = digest else {
        out.push_str(
            "- The comparison could not be computed at this snapshot. Read the process digest of \
             the case itself.\n",
        );
        return;
    };
    out.push_str(
        "- Recorded expectations beside recorded outcomes at this snapshot. The comparison does \
         not judge the difference, and an entry is listed only when the record it concerns may \
         be shown to this audience.\n",
    );
    let mut listed = 0;
    for entry in &digest.expected_versus_actual {
        let Some(record) = shown(input, entry.cite) else {
            continue;
        };
        listed += 1;
        if listed > MAX_LISTED {
            continue;
        }
        let expected = match entry.expected_state {
            ExpectedState::Recorded => "expectation recorded",
            ExpectedState::NoExpectationRecorded => "no expectation recorded",
            ExpectedState::Withheld => "expectation not shown",
        };
        let actual = match entry.actual_state {
            ActualState::Recorded => "outcome recorded",
            ActualState::NoOutcomeRecorded => "no outcome recorded",
        };
        let hindsight = match entry.recorded_after_work_began {
            Some(true) => {
                "; expectation was written after work had begun, so hindsight may have shaped it"
            }
            _ => "",
        };
        let title = shown_title(input, record).unwrap_or_else(|| "untitled".to_owned());
        let _ = writeln!(
            out,
            "- {} {title}: {expected}; {actual}{hindsight}",
            inline_code(&entry.cite.to_string())
        );
        caveat_lines(out, input, record, "  ");
    }
    if listed == 0 {
        out.push_str("- None to show.\n");
    }
    more(out, listed);
}

fn linked_examples(out: &mut String, input: &ExportInput<'_>, retro: &Record) {
    out.push_str("\n## Linked examples\n\n");
    let mut ids: Vec<Id> = Vec::new();
    if let Some(reads) = retro.body.get("read_set").and_then(Value::as_array) {
        for reference in reads {
            if let Some(id) = reference
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| id.parse::<Id>().ok())
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
    }
    ids.sort();
    let records: Vec<&Record> = ids.iter().filter_map(|id| shown(input, *id)).collect();
    if records.is_empty() {
        out.push_str("- None shown.\n");
    }
    for record in records.iter().take(MAX_LISTED) {
        let title = shown_title(input, record).unwrap_or_else(|| "untitled".to_owned());
        let _ = writeln!(
            out,
            "- {} {title}",
            inline_code(&record.header.id.to_string())
        );
        caveat_lines(out, input, record, "  ");
    }
    more(out, records.len());
}

/// " by `actor`" for audiences that may see actor names; empty for the public.
fn actor_suffix(input: &ExportInput<'_>, record: &Record) -> String {
    if caveats_approved(input.audience) {
        format!(" by {}", inline_code(&record.header.created_by.to_string()))
    } else {
        String::new()
    }
}

fn standing(admission: &Record, retro: &Record) -> Standing {
    if admission.header.created_by == retro.header.created_by {
        Standing::SelfAdmitted
    } else {
        Standing::OtherActor
    }
}

/// The reviewer status of one lesson, stated exactly.
fn reviewer_status(
    input: &ExportInput<'_>,
    retro: &Record,
    index: u32,
    digest: &investigation_core::wire::Digest,
) -> Vec<String> {
    let state = input.case.state();
    let key = LessonKey {
        retrospective_id: retro.header.id,
        index,
    };
    let admitted = lessons::admissions(state, retro.header.case_id);
    let Some((record, body)) = admitted
        .get(&key)
        .filter(|(record, _)| input.includes(record))
    else {
        return vec![
            "Not shown as admitted: no admission is shown in this export, so treat this lesson \
             as an unreviewed proposal."
                .to_owned(),
        ];
    };
    let mut lines = Vec::new();
    // Actor names can name real people: the public audience gets the role
    // wording without the name.
    let who = actor_suffix(input, record);
    let current =
        body.lesson.lesson_digest == *digest && body.lesson.retrospective_ref == retro.reference();
    let by = standing(record, retro);
    lines.push(match (&body.basis, by) {
        (AdmissionBasis::HumanReview { .. }, Standing::SelfAdmitted) => format!(
            "Admitted on human review{who}, the author of this retrospective. SELF-REPORTED: \
             the actor kind is a claim the author made about themselves, so this is not an \
             independent review."
        ),
        (AdmissionBasis::HumanReview { .. }, Standing::OtherActor) => format!(
            "Admitted on human review{who}, an actor other than the retrospective's author. \
             The actor kind is self-reported and the actor is not authenticated."
        ),
        (AdmissionBasis::OutcomeCheck { outcome_refs, .. }, Standing::OtherActor) => format!(
            "Admitted on a recorded outcome check{who}, an actor other than the \
             retrospective's author, citing {} later record(s).",
            outcome_refs.len()
        ),
        (AdmissionBasis::OutcomeCheck { outcome_refs, .. }, Standing::SelfAdmitted) => format!(
            "Admitted on a recorded outcome check{who}, the author of this retrospective, \
             citing {} later record(s). SELF-REPORTED: not an independent review.",
            outcome_refs.len()
        ),
    });
    if !current {
        lines.push(
            "The admission is bound to different lesson text or to another revision of the \
             retrospective, so it does not vouch for the text above."
                .to_owned(),
        );
    }
    let rationale = match &body.basis {
        AdmissionBasis::HumanReview { rationale }
        | AdmissionBasis::OutcomeCheck { rationale, .. } => rationale,
    };
    lines.push(format!("Admission rationale: {}", prose(rationale)));
    lines.extend(
        input
            .caveats(record)
            .iter()
            .map(|caveat| format!("Caveat: {}", caveat.text())),
    );
    lines
}

fn proposed_lessons(out: &mut String, input: &ExportInput<'_>, retro: &Record) {
    out.push_str("\n## Proposed lessons\n\n");
    out.push_str(
        "- Lessons are proposals with stated transfer limits. Admission records that someone \
         chose to make a lesson visible to other cases; it is not acceptance as a rule, and \
         it changes no pack or instruction.\n",
    );
    let all = array(&retro.body, "lessons");
    if all.is_empty() {
        let _ = writeln!(
            out,
            "- No reusable lesson identified. Reason: {}",
            text_at(&retro.body, "no_reusable_lesson_reason")
                .filter(|text| !text.trim().is_empty())
                .map_or_else(|| "no reason recorded".to_owned(), prose)
        );
        return;
    }
    let state = input.case.state();
    let classification = level_word(Classifier::new(state).effective(retro).level);
    let trust = Resolver::new(state, input.case.sessions())
        .inherited(retro)
        .level;
    let mut listed = 0;
    for (index, lesson) in all.iter().enumerate() {
        let cited = citations(input, lesson);
        if !may_appear(&cited) {
            continue;
        }
        listed += 1;
        if listed > MAX_LISTED {
            continue;
        }
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        let digest = lessons::lesson_digest(lesson);
        let _ = writeln!(
            out,
            "\n### Lesson {}\n",
            inline_code(
                &LessonKey {
                    retrospective_id: retro.header.id,
                    index
                }
                .to_string()
            )
        );
        let _ = writeln!(
            out,
            "- Proposed guidance: {}",
            prose(text_at(lesson, "guidance").unwrap_or_default())
        );
        let _ = writeln!(
            out,
            "- Applies when: {}",
            prose(text_at(lesson, "applies_when").unwrap_or_default())
        );
        let _ = writeln!(
            out,
            "- Do not reuse where (transfer limits): {}",
            prose(text_at(lesson, "transfer_limits").unwrap_or_default())
        );
        let _ = writeln!(
            out,
            "- Beyond those limits: this lesson comes from one case ({}) and has not been shown \
             to hold elsewhere unless the status below says it was checked.",
            inline_code(&input.case.case_id().to_string())
        );
        let _ = writeln!(
            out,
            "- Counterexamples: none recorded in this snapshot. Counterexamples found by other \
             cases that consulted this lesson are recorded in those cases and are not part of \
             this export."
        );
        let _ = writeln!(
            out,
            "- Supporting cases and records: {}",
            support_line(input, &cited)
        );
        out.push_str("- Reviewer status:\n");
        for line in reviewer_status(input, retro, index, &digest) {
            let _ = writeln!(out, "  - {line}");
        }
        let _ = writeln!(
            out,
            "- Consultation history: not part of this export. Consultations are recorded in the \
             cases that read the lesson, not in its source case."
        );
        let _ = writeln!(
            out,
            "- Trust and classification: inherited trust {} (a label for weighing authored \
             text, not a check for injected instructions); classification {} (effective, \
             of the retrospective that holds the lesson).",
            inline_code(trust.as_str()),
            inline_code(classification)
        );
        let _ = writeln!(out, "- Lesson digest: {}", inline_code(digest.as_str()));
        let _ = writeln!(out, "- Applied to any pack or instruction: no.");
    }
    if listed == 0 {
        out.push_str("- No lesson is shown to this audience.\n");
    }
    more(out, listed);
}

fn superseded(out: &mut String, input: &ExportInput<'_>, retrospectives: &[&Record]) {
    out.push_str("\n## Superseded retrospectives\n\n");
    let old: Vec<&&Record> = retrospectives
        .iter()
        .filter(|record| record.body["status"] == "superseded")
        .collect();
    if old.is_empty() {
        out.push_str("- None shown.\n");
    }
    for record in old.iter().take(MAX_LISTED) {
        let mut line = if input.remapped() {
            format!("- {}", inline_code(&record.header.id.to_string()))
        } else {
            format!(
                "- {} (describes sequence {})",
                inline_code(&record.header.id.to_string()),
                record.body["snapshot"]["sequence"]
                    .as_u64()
                    .unwrap_or_default()
            )
        };
        if let Some(successor) = text_at(&record.body, "successor_ref")
            .or_else(|| record.body["successor_ref"]["id"].as_str())
            .and_then(|id| id.parse::<Id>().ok())
            .and_then(|id| shown(input, id))
        {
            let _ = write!(
                line,
                "; superseded by {}",
                inline_code(&successor.header.id.to_string())
            );
        }
        if let Some(reason) = text_at(&record.body, "superseded_reason") {
            let _ = write!(line, ": {}", prose(reason));
        }
        let _ = writeln!(out, "{line}");
        caveat_lines(out, input, record, "  ");
    }
    out.push_str("- Superseded retrospectives are linked here, not merged into the current one.\n");
    more(out, old.len());
}

fn limits(out: &mut String, input: &ExportInput<'_>) {
    out.push_str("\n## Limits\n\n");
    out.push_str(
        "- Only records and lessons this audience may see are shown. export-manifest.json says \
         whether anything was withheld.\n",
    );
    let _ = writeln!(out, "- {}", dependent_rule(input.audience));
    out.push_str(
        "- A step or lesson that cites a record withheld from this audience is left out for \
         every audience, because its wording may restate what that record says. It is not \
         named, counted or described.\n",
    );
    out.push_str(
        "- The process digest covers recorded work only; effort, dead ends and decisions never \
         recorded in the case are absent.\n",
    );
    let _ = writeln!(out, "- {}", inherited_trust::LIMITS);
    let _ = writeln!(out, "- {}", lessons::NO_SIDE_EFFECTS);
    if input.remapped() {
        out.push_str(
            "- IDs in this export were remapped for this export only. To read the whole case, \
             ask the publisher, who can open it at the snapshot it was rendered from.\n",
        );
    } else {
        let snapshot = input.snapshot();
        let _ = writeln!(
            out,
            "- To read the whole case at this snapshot: {}",
            inline_code(&format!(
                "investigation --case {} brief --snapshot {} --head-hash {}",
                snapshot.case_id, snapshot.sequence, snapshot.head_hash
            ))
        );
    };
}
