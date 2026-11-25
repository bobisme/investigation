//! `lesson.admit`, `lesson.consult` and `lesson.record_outcome` (§8.6, §16.1).
//! See [`crate::lessons`] for the rules. Each operation creates or revises one
//! annotation and nothing else.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::{
    catalog::{OperationDescriptor, OperationInfo},
    command::{Reference, RevisionRef},
    header::ActorKind,
    identity::{Id, Kind},
    lessons::{
        ADMISSION_ROLE, AdmissionBasis, AdmissionBody, AdmittedLesson, CONSULTATION_ROLE,
        ConsultationBody, ConsultationEffect, ConsultationOutcome, LessonKey, LessonPointer,
        admissions, consultation_body, consultations, lesson, lesson_digest,
    },
    pack_policy::authors_by,
    state::{OperationContext, State},
    wire::{ErrorCode, Failure, Snapshot},
};

pub const ADMIT: &str = "lesson.admit";
pub const CONSULT: &str = "lesson.consult";
pub const RECORD_OUTCOME: &str = "lesson.record_outcome";

/// Longest rationale or purpose, in characters.
const MAX_TEXT_CHARS: usize = 2048;

/// Kinds that can record an outcome check of a lesson.
const OUTCOME_KINDS: &[Kind] = &[Kind::Run, Kind::Observation, Kind::Assessment, Kind::Review];

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum BasisInput {
    /// A person other than the lesson's author reviewed the lesson. The
    /// admitting actor must not be the author, and the command's
    /// actor_descriptor kind must be human; both are claims, not authentication.
    HumanReview { rationale: String },
    /// Records accepted after the Retrospective's snapshot check the lesson
    /// against an outcome. The admitting actor must not be the lesson's author.
    OutcomeCheck {
        rationale: String,
        outcome_refs: Vec<Reference>,
    },
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LessonAdmit {
    /// The current, unsuperseded Retrospective revision that proposes the lesson.
    retrospective_ref: Reference,
    /// Zero-based position in the Retrospective's lessons.
    lesson_index: u32,
    basis: BasisInput,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LessonConsult {
    /// The admitted lesson, exactly as a lesson read lists it.
    lesson: LessonPointer,
    /// Why this case read the lesson.
    purpose: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LessonRecordOutcome {
    /// The current revision of this case's consultation.
    consultation_ref: Reference,
    effect: ConsultationEffect,
    rationale: String,
    /// Same-case records the lesson changed or that contradict it. Required
    /// for changed_work and contradicted.
    #[serde(default)]
    record_refs: Vec<Reference>,
}

fn text(value: &str, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() || value.chars().count() > MAX_TEXT_CHARS {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            format!("Supply nonempty text no longer than {MAX_TEXT_CHARS} characters."),
        )
        .at(path));
    }
    Ok(())
}

fn gate(message: &str, path: &str) -> Failure {
    Failure::new(ErrorCode::GateFailed, message).at(path)
}

fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "The stored lesson record is invalid.",
    )
}

fn admit_shape(payload: &LessonAdmit) -> Result<(), Failure> {
    match &payload.basis {
        BasisInput::HumanReview { rationale } => text(rationale, "/basis/rationale"),
        BasisInput::OutcomeCheck {
            rationale,
            outcome_refs,
        } => {
            text(rationale, "/basis/rationale")?;
            if outcome_refs.is_empty() {
                return Err(Failure::new(
                    ErrorCode::InvalidInput,
                    "Cite the records that check the lesson against an outcome.",
                )
                .at("/basis/outcome_refs"));
            }
            Ok(())
        }
    }
}

fn admit(payload: LessonAdmit, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let case = ctx.case_id().ok_or_else(integrity)?;
    let retrospective_ref = ctx.resolve(
        &payload.retrospective_ref,
        Kind::Retrospective,
        "/retrospective_ref",
    )?;
    let record = ctx.current(retrospective_ref.id).ok_or_else(integrity)?;
    if record.reference() != retrospective_ref || record.body["status"] != "recorded" {
        let mut failure = gate(
            "Admit lessons from the current revision of an unsuperseded Retrospective.",
            "/retrospective_ref",
        );
        failure.error.actual = Some(json!(record.reference()));
        return Err(failure);
    }
    let content = lesson(record, payload.lesson_index).ok_or_else(|| {
        Failure::new(
            ErrorCode::InvalidInput,
            "The Retrospective has no lesson at that index.",
        )
        .at("/lesson_index")
    })?;
    let key = LessonKey {
        retrospective_id: retrospective_ref.id,
        index: payload.lesson_index,
    };
    if let Some((existing, _)) = admissions(ctx.state(), case).get(&key) {
        let mut failure = gate("This lesson is already admitted.", "/lesson_index");
        failure.error.actual = Some(json!(existing.reference()));
        return Err(failure);
    }
    let lesson_digest = lesson_digest(content);
    // Who wrote the lesson, by the rule review independence and the
    // distinct_actor policy use: the actors who recorded the subject's current
    // content. A Retrospective has no basis digest and is revised only by
    // supersession, so its content is its body. Actor strings are compared,
    // a procedural check that authenticates nobody.
    let authors = authors_by(ctx.state(), record, |revision| Ok(revision.body.clone()))?;
    let self_admission = authors
        .contains(ctx.actor())
        .then(|| json!({"actor": ctx.actor(), "authors": authors}));
    let classification = record.header.classification.clone();
    let snapshot: Snapshot =
        crate::input::decode(&record.body["snapshot"]).map_err(|_| integrity())?;
    let reported = ctx
        .actor_descriptor()
        .map(|descriptor| descriptor.kind.clone());
    let basis = match payload.basis {
        BasisInput::HumanReview { rationale } => {
            if let Some(actual) = self_admission {
                let mut failure = gate(
                    "Admission on human review needs an actor other than the author of the lesson: a person who reviewed it submits lesson.admit as themselves, with an actor_descriptor whose kind is human. Or an actor other than the author cites a recorded outcome check.",
                    "/basis",
                );
                failure.error.actual = Some(actual);
                return Err(failure);
            }
            if reported != Some(ActorKind::Human) {
                return Err(gate(
                    "Admission on human review is submitted by a person other than the lesson's author, with a command actor_descriptor whose kind is human. Otherwise, have an actor other than the author cite a recorded outcome check.",
                    "/basis",
                ));
            }
            AdmissionBasis::HumanReview { rationale }
        }
        BasisInput::OutcomeCheck {
            rationale,
            outcome_refs,
        } => {
            if let Some(actual) = self_admission {
                let mut failure = gate(
                    "An outcome-check admission needs an actor other than the author of the lesson.",
                    "/basis",
                );
                failure.error.actual = Some(actual);
                return Err(failure);
            }
            let mut seen = BTreeSet::new();
            let mut refs = Vec::new();
            for (index, reference) in outcome_refs.iter().enumerate() {
                let path = format!("/basis/outcome_refs/{index}");
                let exact = ctx.resolve_reference(reference, &path)?;
                let cited = ctx.exact(exact).ok_or_else(integrity)?;
                if !OUTCOME_KINDS.contains(&exact.id.kind()) {
                    return Err(Failure::new(
                        ErrorCode::InvalidInput,
                        "Cite a Run, Observation, Assessment or Review as an outcome check.",
                    )
                    .at(path));
                }
                if cited.header.acceptance_sequence <= snapshot.sequence {
                    return Err(gate(
                        "An outcome check is recorded after the Retrospective's snapshot. Cite later work.",
                        &path,
                    ));
                }
                if !seen.insert(exact.id) {
                    return Err(
                        Failure::new(ErrorCode::InvalidInput, "Cite each record once.").at(path),
                    );
                }
                refs.push(exact);
            }
            AdmissionBasis::OutcomeCheck {
                rationale,
                outcome_refs: refs,
            }
        }
    };
    let body = AdmissionBody {
        lesson: AdmittedLesson {
            retrospective_ref,
            lesson_index: payload.lesson_index,
            lesson_digest,
        },
        basis,
        actor_kind_self_reported: reported,
    };
    ctx.create_classified(
        Kind::Annotation,
        json!({
            "subject_refs": [retrospective_ref],
            "role": ADMISSION_ROLE,
            "evidentiary_status": "none",
            "lesson_admission": body,
        }),
        classification,
    )?;
    Ok(())
}

fn consult_shape(payload: &LessonConsult) -> Result<(), Failure> {
    let lesson = &payload.lesson;
    for (ok, path, kind) in [
        (
            lesson.source_case_id.kind() == Kind::Case,
            "/lesson/source_case_id",
            "Case",
        ),
        (
            lesson.retrospective_ref.id.kind() == Kind::Retrospective,
            "/lesson/retrospective_ref",
            "Retrospective",
        ),
        (
            lesson.admission_ref.id.kind() == Kind::Annotation,
            "/lesson/admission_ref",
            "Annotation",
        ),
    ] {
        if !ok {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                format!("Expected a {kind} reference."),
            )
            .at(path));
        }
    }
    if let Some(purpose) = &payload.purpose {
        text(purpose, "/purpose")?;
    }
    Ok(())
}

/// The pointer is checked against the source case by the store at commit.
fn consult(payload: LessonConsult, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let case = ctx.case_id().ok_or_else(integrity)?;
    if payload.lesson.source_case_id == case {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "A case reads its own lessons with the retrospective command. Consult lessons of other cases.",
        )
        .at("/lesson/source_case_id"));
    }
    let key = payload.lesson.key();
    if let Some((existing, _)) = consultations(ctx.state(), case)
        .into_iter()
        .find(|(_, body)| body.lesson.key() == key)
    {
        let mut failure = gate(
            "This case already recorded a consultation of the lesson. Record its outcome with lesson.record_outcome.",
            "/lesson",
        );
        failure.error.actual = Some(json!(existing.reference()));
        return Err(failure);
    }
    let subject = ctx.current(case).ok_or_else(integrity)?.reference();
    let body = ConsultationBody {
        lesson: payload.lesson,
        purpose: payload.purpose,
        outcome: None,
    };
    ctx.create(
        Kind::Annotation,
        json!({
            "subject_refs": [subject],
            "role": CONSULTATION_ROLE,
            "evidentiary_status": "none",
            "lesson_consultation": body,
        }),
    )?;
    Ok(())
}

fn outcome_shape(payload: &LessonRecordOutcome) -> Result<(), Failure> {
    text(&payload.rationale, "/rationale")?;
    if payload.effect.needs_records() && payload.record_refs.is_empty() {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "Cite the same-case records that the lesson changed or that contradict it.",
        )
        .at("/record_refs"));
    }
    Ok(())
}

fn record_outcome(
    payload: LessonRecordOutcome,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let exact = ctx.resolve(
        &payload.consultation_ref,
        Kind::Annotation,
        "/consultation_ref",
    )?;
    let record = ctx.exact(exact).ok_or_else(integrity)?;
    if consultation_body(record).is_none() {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "Reference a lesson consultation recorded in this case.",
        )
        .at("/consultation_ref"));
    }
    let mut body = record.body.clone();
    let mut seen = BTreeSet::new();
    let mut refs: Vec<RevisionRef> = Vec::new();
    for (index, reference) in payload.record_refs.iter().enumerate() {
        let path = format!("/record_refs/{index}");
        let cited = ctx.resolve_reference(reference, &path)?;
        if cited.id == exact.id || !seen.insert(cited.id) {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                "Cite each other record of this case once.",
            )
            .at(path));
        }
        refs.push(cited);
    }
    body["lesson_consultation"]["outcome"] = json!(ConsultationOutcome {
        effect: payload.effect,
        rationale: payload.rationale,
        record_refs: refs,
    });
    ctx.update(exact, body)?;
    Ok(())
}

/// The question `case.close` asks when the case consulted lessons and has not
/// said whether they mattered. It is a warning, never a blocker.
pub(crate) fn closure_question(state: &State, case: Id) -> Option<String> {
    let open: Vec<String> = consultations(state, case)
        .into_iter()
        .filter(|(_, body)| body.outcome.is_none())
        .map(|(record, body)| format!("{} ({})", record.header.id, body.lesson.key()))
        .collect();
    (!open.is_empty()).then(|| {
        format!(
            "lesson.consultation_outcome_unrecorded: this case consulted admitted lessons without recording whether they changed anything: {}. Record each with {RECORD_OUTCOME}; closing does not require it.",
            open.join(", ")
        )
    })
}

pub(crate) fn operations() -> [OperationDescriptor; 3] {
    let info = |name, stage, summary, when_to_use, effects, returns, revision_rule, repair| {
        OperationInfo {
            name,
            stage,
            summary,
            when_to_use,
            lifecycle: "An existing case. Writes annotations only; never method packs, prompts, agent settings or policy.",
            effects,
            returns,
            revision_rule,
            idempotency: "Retry the same request with the original actor and command ID.",
            repair,
            related: &["retrospective.record", "annotation.add"],
        }
    };
    let pointer = json!({
        "source_case_id":"case_019acd16-0000-7000-8000-000000000001",
        "retrospective_ref":{"id":"retro_019acd16-0000-7000-8000-000000000002","revision":1},
        "lesson_index":0,
        "admission_ref":{"id":"ann_019acd16-0000-7000-8000-000000000003","revision":1},
        "lesson_digest":format!("sha256:{}", "1".repeat(64)),
    });
    [
        OperationDescriptor::active(
            info(
                ADMIT,
                "close",
                "Admit one proposed lesson for reading in other cases.",
                "A person other than its author reviewed the lesson, or another actor checked it against later recorded work.",
                "Create an attributed admission annotation bound to the exact Retrospective revision and lesson digest. Until then the lesson is visible only in its own case.",
                "Annotation ID and revision 1.",
                "Use the current revision of an unsuperseded Retrospective in this case.",
                "Admit as an actor other than the lesson's author: human_review with an actor_descriptor of kind human, or outcome_check citing Runs, Observations, Assessments or Reviews recorded after the Retrospective's snapshot.",
            ),
            json!({
                "retrospective_ref":{"id":"retro_019acd16-0000-7000-8000-000000000002","revision":1},
                "lesson_index":0,
                "basis":{"kind":"human_review","rationale":"Checked the cited runs; the limit is stated."}
            }),
            admit_shape,
            admit,
        ),
        OperationDescriptor::active(
            info(
                CONSULT,
                "record",
                "Record that this case read an admitted lesson of another case.",
                "Before relying on another case's lesson. The retrospective command with --consult records it and returns the text.",
                "Create a consultation annotation that points at the lesson and its digest without copying its text. The store checks the admission in the source case at commit.",
                "Annotation ID and revision 1.",
                "Use the pointer and digest from a lesson read; the admission must be current in the source case.",
                "List admitted lessons with the retrospective command and --lessons, then copy the pointer.",
            ),
            json!({"lesson":pointer,"purpose":"Planning the tracer setup."}),
            consult_shape,
            consult,
        ),
        OperationDescriptor::active(
            info(
                RECORD_OUTCOME,
                "close",
                "Record whether a consulted lesson changed anything in this case.",
                "At or before closing a case that consulted a lesson.",
                "Revise the consultation with an attributed effect, rationale and the records it concerns.",
                "Annotation ID and next revision.",
                "Use the consultation's current revision.",
                "Cite same-case records for changed_work or contradicted.",
            ),
            json!({
                "consultation_ref":{"id":"ann_019acd16-0000-7000-8000-000000000004","revision":1},
                "effect":"no_change",
                "rationale":"The tracer was already attached before the burst."
            }),
            outcome_shape,
            record_outcome,
        ),
    ]
}

#[cfg(test)]
mod tests;
