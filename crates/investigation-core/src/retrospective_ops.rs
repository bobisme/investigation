//! Method accounts and attributed narrative annotations.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    PROGRAM_NAME,
    advice_feedback::{ADVICE_FEEDBACK_ROLE, AdviceFeedbackInput, feedback_body},
    catalog::{OperationDescriptor, OperationInfo},
    command::{Reference, RevisionRef},
    evidence::{ArtifactAvailability, ArtifactBody, ArtifactIntegrity, ArtifactSensitivity},
    header::ClassificationLevel,
    human_file::{HumanFile, HumanFileInput},
    identity::{Id, Kind},
    retrospective::{
        AnnotationAdd, AnnotationContent, RetrospectiveRecord, RetrospectiveStatus,
        RetrospectiveSupersede,
    },
    state::{OperationContext, Record, State},
    synopsis::{SYNOPSIS_MAX_BYTES, SYNOPSIS_ROLE},
    wire::{ErrorCode, Failure, Snapshot},
};

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RetrospectiveInput {
    #[serde(flatten)]
    content: RetrospectiveRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    no_reusable_lesson_reason: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum AnnotationRole {
    Note,
    Synopsis,
    AdviceFeedback,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AnnotationInput {
    #[serde(flatten)]
    content: AnnotationAdd,
    role: Option<AnnotationRole>,
    /// Required with `role: advice_feedback`, and only with it.
    #[serde(default)]
    advice_feedback: Option<AdviceFeedbackInput>,
    /// The person-owned vault file whose exact bytes the artifact content
    /// captured (a note snapshot). Only with artifact content and role
    /// `note`. `investigation vault snapshot-note` writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    human_file: Option<HumanFileInput>,
}

fn text(input: &str, path: &str) -> Result<(), Failure> {
    if input.trim().is_empty() {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "Supply a concrete description or reason.",
        )
        .at(path));
    }
    Ok(())
}

fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "The stored methodological record is invalid.",
    )
}

pub(crate) fn retrospective_failure(code: ErrorCode, message: &str) -> Failure {
    let mut failure = Failure::new(code, message).with_schema("retrospective.record");
    if let Some(repair) = &mut failure.error.repair {
        repair.argv = Some(
            [
                PROGRAM_NAME,
                "schema",
                "retrospective.record",
                "--format",
                "json",
            ]
            .map(str::to_owned)
            .into(),
        );
    }
    failure
}

/// Uncited entries are rejected in `retrospective_record`, where the case is
/// known and the repair can name its digest.
fn support_shape(refs: &[Id], path: &str) -> Result<(), Failure> {
    let unique: BTreeSet<_> = refs.iter().collect();
    if unique.len() != refs.len() {
        return Err(
            Failure::new(ErrorCode::InvalidInput, "List each supporting record once.").at(path),
        );
    }
    Ok(())
}

fn retrospective_shape(payload: &RetrospectiveInput) -> Result<(), Failure> {
    let content = &payload.content;
    text(&content.costs_or_constraints, "/costs_or_constraints")?;
    for (index, attempt) in content.methods_attempted.iter().enumerate() {
        text(&attempt.name, &format!("/methods_attempted/{index}/name"))?;
    }
    for (path, steps) in [
        ("/effective_steps", &content.effective_steps),
        (
            "/invalid_or_misleading_steps",
            &content.invalid_or_misleading_steps,
        ),
    ] {
        for (index, step) in steps.iter().enumerate() {
            text(&step.description, &format!("{path}/{index}/description"))?;
            support_shape(&step.support_refs, &format!("{path}/{index}/support_refs"))?;
        }
    }
    if content.invalid_or_misleading_steps.is_empty() {
        text(
            content.none_observed_reason.as_deref().unwrap_or_default(),
            "/none_observed_reason",
        )?;
    } else if content.none_observed_reason.is_some() {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "Describe the invalid or misleading steps, or state why none were observed.",
        )
        .at("/none_observed_reason"));
    }
    for (index, unknown) in content.residual_unknowns.iter().enumerate() {
        text(unknown, &format!("/residual_unknowns/{index}"))?;
    }
    for (index, lesson) in content.lessons.iter().enumerate() {
        let path = format!("/lessons/{index}");
        text(&lesson.guidance, &format!("{path}/guidance"))?;
        text(&lesson.applies_when, &format!("{path}/applies_when"))?;
        text(&lesson.transfer_limits, &format!("{path}/transfer_limits"))?;
        support_shape(&lesson.support_refs, &format!("{path}/support_refs"))?;
    }
    if let Some(reason) = &payload.no_reusable_lesson_reason {
        text(reason, "/no_reusable_lesson_reason")?;
        if !content.lessons.is_empty() {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                "Omit the no-lesson reason when proposing lessons.",
            )
            .at("/no_reusable_lesson_reason"));
        }
    }
    Ok(())
}

fn verify_snapshot(state: &State, snapshot: &Snapshot) -> Result<(), Failure> {
    if state.verified_snapshot(snapshot.sequence) != Some(&snapshot.head_hash) {
        return Err(retrospective_failure(
            ErrorCode::RetrospectiveStale,
            "Use a snapshot hash verified from the selected case journal.",
        )
        .at("/snapshot"));
    }
    Ok(())
}

fn cite(
    state: &State,
    case: Id,
    sequence: u64,
    id: Id,
    path: &str,
) -> Result<RevisionRef, Failure> {
    state
        .record_at(id, sequence)
        .filter(|record| record.header.case_id == case)
        .filter(|record| {
            !matches!(
                record.header.id.kind(),
                Kind::Annotation | Kind::Retrospective
            )
        })
        .map(Record::reference)
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::UnknownReference,
                "Cite a same-case record that existed at the retrospective snapshot.",
            )
            .at(path)
        })
}

/// Every step and lesson rests on a digest item or record; a generic, uncited
/// entry is rejected with a pointer to the case's process digest.
fn require_citations(content: &RetrospectiveRecord, case: Option<Id>) -> Result<(), Failure> {
    let entries =
        [
            ("/effective_steps", &content.effective_steps),
            (
                "/invalid_or_misleading_steps",
                &content.invalid_or_misleading_steps,
            ),
        ]
        .into_iter()
        .flat_map(|(path, steps)| {
            steps
                .iter()
                .enumerate()
                .map(move |(index, step)| (format!("{path}/{index}"), step.support_refs.is_empty()))
        })
        .chain(
            content.lessons.iter().enumerate().map(|(index, lesson)| {
                (format!("/lessons/{index}"), lesson.support_refs.is_empty())
            }),
        );
    for (path, uncited) in entries {
        if uncited {
            let mut failure = Failure::new(
                ErrorCode::GateFailed,
                "Cite a digest item or a recorded same-case record for this step or proposed lesson. Run the process digest to find citable facts.",
            )
            .with_schema("retrospective.record")
            .at(format!("{path}/support_refs"));
            if let Some(case) = case {
                failure = failure.with_argv(crate::retrospective_digest::draft_argv(case));
            }
            return Err(failure);
        }
    }
    Ok(())
}

fn retrospective_record(
    mut payload: RetrospectiveInput,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    require_citations(&payload.content, ctx.case_id())?;
    let case_ref = ctx.resolve(&payload.content.case_ref, Kind::Case, "/case_ref")?;
    let state = ctx.state();
    let snapshot = &payload.content.snapshot;
    verify_snapshot(state, snapshot)?;
    let case = ctx.case_id().ok_or_else(integrity)?;
    let expected = state
        .record_at(case, snapshot.sequence)
        .ok_or_else(integrity)?
        .reference();
    if expected != case_ref {
        let mut failure = retrospective_failure(
            ErrorCode::RetrospectiveStale,
            "Use the case revision effective at the declared snapshot.",
        )
        .at("/case_ref");
        failure.error.expected = Some(json!(expected));
        return Err(failure);
    }
    let sequence = snapshot.sequence;
    let mut cited = BTreeMap::new();
    for (index, attempt) in payload.content.methods_attempted.iter().enumerate() {
        if let Some(id) = attempt.inquiry_ref {
            let path = format!("/methods_attempted/{index}/inquiry_ref");
            if id.kind() != Kind::Inquiry {
                return Err(
                    Failure::new(ErrorCode::InvalidInput, "Reference an Inquiry record.").at(path),
                );
            }
            cited.insert(id, cite(state, case, sequence, id, &path)?);
        }
    }
    for (path, steps) in [
        ("/effective_steps", &payload.content.effective_steps),
        (
            "/invalid_or_misleading_steps",
            &payload.content.invalid_or_misleading_steps,
        ),
    ] {
        for (step, entry) in steps.iter().enumerate() {
            for (index, id) in entry.support_refs.iter().enumerate() {
                let path = format!("{path}/{step}/support_refs/{index}");
                cited.insert(*id, cite(state, case, sequence, *id, &path)?);
            }
        }
    }
    for (lesson, entry) in payload.content.lessons.iter().enumerate() {
        for (index, id) in entry.support_refs.iter().enumerate() {
            let path = format!("/lessons/{lesson}/support_refs/{index}");
            cited.insert(*id, cite(state, case, sequence, *id, &path)?);
        }
    }
    if payload.content.invalid_or_misleading_steps.is_empty() {
        let unsuccessful = crate::retrospective_digest::unsuccessful_work(state, case, sequence);
        if !unsuccessful.is_empty() {
            let ids: Vec<_> = unsuccessful
                .iter()
                .map(|reference| reference.id.to_string())
                .collect();
            ctx.warn(format!(
                "retrospective.unsuccessful_work_not_cited: the process digest lists invalid or failed work ({}). Cite it in invalid_or_misleading_steps if it was wasted or misleading.",
                ids.join(", ")
            ));
        }
    }
    payload.content.case_ref = Reference::Existing(case_ref);
    let mut body = serde_json::to_value(payload).map_err(|_| integrity())?;
    body["status"] = json!(RetrospectiveStatus::Recorded);
    body["read_set"] = json!(cited.into_values().collect::<Vec<_>>());
    ctx.create(Kind::Retrospective, body)?;
    Ok(())
}

fn current_retrospective(
    state: &State,
    case: Id,
    reference: RevisionRef,
) -> Result<&Record, Failure> {
    if let Some(record) = state.current(reference.id).filter(|record| {
        record.header.case_id == case && reference.id.kind() == Kind::Retrospective
    }) && record.reference() == reference
        && record.body["status"] == "recorded"
    {
        return Ok(record);
    }
    let mut failure = retrospective_failure(
        ErrorCode::RetrospectiveStale,
        "Use the current revision of an unsuperseded Retrospective.",
    );
    failure.error.actual = Some(json!(
        state
            .current(reference.id)
            .filter(|record| record.header.case_id == case)
            .map(Record::reference)
    ));
    Err(failure)
}

fn retrospective_supersede(
    payload: RetrospectiveSupersede,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let state = ctx.state();
    let case = ctx.case_id().ok_or_else(integrity)?;
    let previous = current_retrospective(state, case, payload.previous)
        .map_err(|failure| failure.at("/previous"))?;
    let successor = ctx.resolve(&payload.successor, Kind::Retrospective, "/successor")?;
    let next = current_retrospective(state, case, successor)
        .map_err(|failure| failure.at("/successor"))?;
    let previous_snapshot: Snapshot =
        crate::input::decode(&previous.body["snapshot"]).map_err(|_| integrity())?;
    let successor_snapshot: Snapshot =
        crate::input::decode(&next.body["snapshot"]).map_err(|_| integrity())?;
    if next.header.acceptance_sequence <= previous.header.acceptance_sequence
        || successor_snapshot.sequence < previous_snapshot.sequence
    {
        return Err(retrospective_failure(
            ErrorCode::RetrospectiveStale,
            "Choose a later recorded replacement that covers at least the previous snapshot.",
        )
        .at("/successor"));
    }
    let mut body = previous.body.clone();
    body["status"] = json!(RetrospectiveStatus::Superseded);
    body["successor_ref"] = json!(successor);
    body["superseded_reason"] = json!(payload.reason);
    ctx.update(payload.previous, body)?;
    Ok(())
}

/// Whether `record`, with `previous` its preceding revision, is material work a
/// closing Retrospective must account for.
fn material_trigger(previous: Option<&Record>, record: &Record) -> bool {
    let body = &record.body;
    match record.header.id.kind() {
        Kind::Conclusion => matches!(body["status"].as_str(), Some("review_ready" | "validated")),
        Kind::Decision => body["lifecycle"] == "accepted" || body["status"] == "accepted",
        Kind::Run => {
            matches!(body["execution_status"].as_str(), Some("failed"))
                || body["evidence_validity"] == "invalid"
        }
        // A judgment that raises no concern is not material: only a withdrawal,
        // a newly raised concern or a change outside the use fields is.
        Kind::Source | Kind::Artifact => previous.is_some_and(|previous| {
            crate::source_ops::use_trigger(previous, record).is_some()
                || (record.header.id.kind() == Kind::Source
                    && !crate::source_ops::use_only_change(previous, record))
        }),
        Kind::Observation => {
            record.header.revision.0 > 1
                || matches!(
                    body["status"].as_str(),
                    Some("retracted" | "withdrawn" | "corrected")
                )
        }
        _ => false,
    }
}

pub(crate) fn material_revisions(state: &State, case: Id) -> Vec<RevisionRef> {
    state
        .records()
        .filter(|record| record.header.case_id == case)
        .filter_map(|record| {
            let mut previous = None;
            let mut found = None;
            for historical in state.history(record.header.id) {
                if material_trigger(previous, historical) {
                    found = Some(historical.reference());
                }
                previous = Some(historical);
            }
            found
        })
        .collect()
}

fn case_framing(body: &Value) -> Value {
    let mut projection = body.clone();
    if let Some(fields) = projection.as_object_mut() {
        for field in [
            "lifecycle",
            "closure_outcome",
            "transition_reason",
            "closure",
        ] {
            fields.remove(field);
        }
    }
    projection
}

/// A closing account must cover the current material basis. A narrative note,
/// another process account, or a lifecycle-only case transition does not stale it.
pub(crate) fn check_retrospective(
    state: &State,
    case: Id,
    retrospective: RevisionRef,
) -> Result<(), Failure> {
    let record = current_retrospective(state, case, retrospective)?;
    let snapshot: Snapshot =
        crate::input::decode(&record.body["snapshot"]).map_err(|_| integrity())?;
    verify_snapshot(state, &snapshot)?;
    let changed: Vec<_> = state
        .records()
        .filter(|current| {
            current.header.case_id == case
                && current.header.acceptance_sequence > snapshot.sequence
                && match current.header.id.kind() {
                    Kind::Annotation | Kind::Retrospective => false,
                    Kind::Case => state
                        .record_at(case, snapshot.sequence)
                        .is_none_or(|previous| {
                            case_framing(&previous.body) != case_framing(&current.body)
                        }),
                    _ => true,
                }
        })
        .map(Record::reference)
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    let mut failure = retrospective_failure(
        ErrorCode::RetrospectiveStale,
        "The Retrospective snapshot predates materially relevant work. Record an updated account.",
    );
    failure.error.actual = Some(json!(changed));
    Err(failure)
}

fn annotation_shape(payload: &AnnotationInput) -> Result<(), Failure> {
    let advice = matches!(payload.role, Some(AnnotationRole::AdviceFeedback));
    if advice != payload.advice_feedback.is_some() {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "Role advice_feedback and the advice_feedback object go together: name the rule, version and disposition with that role, and only with it.",
        )
        .at("/advice_feedback"));
    }
    if advice && !matches!(payload.content.content, AnnotationContent::Text { .. }) {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "State the reason for advice feedback as text.",
        )
        .at("/content"));
    }
    if payload.content.subject_refs.is_empty() {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "Provide at least one exact subject reference.",
        )
        .at("/subject_refs"));
    }
    if let AnnotationContent::Text { text: content } = &payload.content.content {
        text(content, "/content/text")?;
    }
    if matches!(payload.role, Some(AnnotationRole::Synopsis)) {
        synopsis_shape(&payload.content)?;
    }
    if let Some(file) = &payload.human_file {
        if !matches!(payload.role, None | Some(AnnotationRole::Note))
            || !matches!(payload.content.content, AnnotationContent::Artifact { .. })
        {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                "A human_file names where a note snapshot's bytes were read; use it only with artifact content and role note.",
            )
            .at("/human_file"));
        }
        crate::human_file::folder(&file.path).map_err(|reason| {
            Failure::new(
                ErrorCode::InvalidInput,
                format!("The human_file path is refused: {reason}."),
            )
            .at("/human_file/path")
        })?;
    }
    Ok(())
}

/// A synopsis is bounded text about exactly one subject revision.
fn synopsis_shape(payload: &AnnotationAdd) -> Result<(), Failure> {
    if payload.subject_refs.len() != 1 {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "A synopsis summarizes exactly one subject revision.",
        )
        .at("/subject_refs"));
    }
    match &payload.content {
        AnnotationContent::Text { text } if text.len() > SYNOPSIS_MAX_BYTES => Err(Failure::new(
            ErrorCode::InvalidInput,
            format!("Keep a synopsis within {SYNOPSIS_MAX_BYTES} UTF-8 bytes."),
        )
        .at("/content/text")),
        AnnotationContent::Text { .. } => Ok(()),
        AnnotationContent::Artifact { .. } => Err(Failure::new(
            ErrorCode::InvalidInput,
            "A synopsis is text. Attach a note snapshot as an ordinary note.",
        )
        .at("/content")),
    }
}

fn annotation_add(payload: AnnotationInput, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let synopsis = matches!(payload.role, Some(AnnotationRole::Synopsis));
    let advice = payload.advice_feedback;
    let human_file = payload.human_file;
    let AnnotationAdd {
        subject_refs,
        mut content,
        classification,
    } = payload.content;
    let mut seen = BTreeSet::new();
    let mut subjects = Vec::new();
    for (index, reference) in subject_refs.iter().enumerate() {
        let path = format!("/subject_refs/{index}");
        let exact = ctx.resolve_reference(reference, &path)?;
        if !seen.insert(exact.id) {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                "List one revision per annotation subject.",
            )
            .at(path));
        }
        if synopsis {
            synopsis_subject(ctx, exact, &path)?;
        }
        subjects.push(exact);
    }
    if let AnnotationContent::Artifact {
        artifact_ref,
        digest,
    } = &mut content
    {
        let exact = ctx.resolve(artifact_ref, Kind::Artifact, "/content/artifact_ref")?;
        let record = ctx.exact(exact).ok_or_else(integrity)?;
        let artifact: ArtifactBody = crate::input::decode(&record.body).map_err(|_| integrity())?;
        if ctx
            .current(exact.id)
            .is_none_or(|current| current.reference() != exact)
            || artifact.availability != ArtifactAvailability::Retained
            || artifact.integrity != ArtifactIntegrity::Verified
            || artifact.digest.as_ref() != Some(digest)
        {
            return Err(Failure::new(ErrorCode::GateFailed, "A note snapshot must reference the exact current retained, verified artifact digest.").at("/content"));
        }
        let rank = |level: ClassificationLevel| match level {
            ClassificationLevel::Public => 0,
            ClassificationLevel::Internal => 1,
            ClassificationLevel::Restricted => 2,
        };
        let sensitivity = match artifact.sensitivity {
            ArtifactSensitivity::Public => ClassificationLevel::Public,
            ArtifactSensitivity::Internal => ClassificationLevel::Internal,
            ArtifactSensitivity::Restricted => ClassificationLevel::Restricted,
        };
        if rank(classification.level)
            < rank(sensitivity).max(rank(record.header.classification.level))
        {
            return Err(Failure::new(
                ErrorCode::GateFailed,
                "An annotation may not lower the classification of its note artifact.",
            )
            .at("/classification"));
        }
        *artifact_ref = Reference::Existing(exact);
    }
    let mut body = json!({
        "subject_refs": subjects,
        "content": content,
        "role": if synopsis { SYNOPSIS_ROLE } else { "note" },
        "evidentiary_status": "none",
    });
    if let Some(advice) = &advice {
        body["role"] = json!(ADVICE_FEEDBACK_ROLE);
        body["advice_feedback"] = feedback_body(ctx, advice, &subjects)?;
    }
    if let Some(file) = &human_file {
        let file = HumanFile::new(&file.path).map_err(|reason| {
            Failure::new(
                ErrorCode::InvalidInput,
                format!("The human_file path is refused: {reason}."),
            )
            .at("/human_file/path")
        })?;
        body["human_file"] = json!(file);
    }
    ctx.create_classified(Kind::Annotation, body, classification)?;
    Ok(())
}

/// A synopsis binds the subject's current revision, so it is never written
/// already stale, and it summarizes a record rather than another annotation.
fn synopsis_subject(
    ctx: &OperationContext<'_>,
    exact: RevisionRef,
    path: &str,
) -> Result<(), Failure> {
    if exact.id.kind() == Kind::Annotation {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "A synopsis summarizes a record, not another annotation.",
        )
        .at(path));
    }
    if ctx
        .current(exact.id)
        .is_none_or(|current| current.reference() != exact)
    {
        let mut failure = Failure::new(
            ErrorCode::InvalidInput,
            "A synopsis must bind the subject's current revision. Read the subject again.",
        )
        .at(path);
        failure.error.actual = ctx
            .current(exact.id)
            .map(|current| json!(current.reference()));
        return Err(failure);
    }
    Ok(())
}

pub(crate) fn operations() -> [OperationDescriptor; 3] {
    let info =
        |name, stage, summary, when_to_use, effects, returns, revision_rule, repair, related| {
            OperationInfo {
                name,
                stage,
                summary,
                when_to_use,
                lifecycle: "An existing case and exact same-case subjects.",
                effects,
                returns,
                revision_rule,
                idempotency: "Retry the same request with the original actor and command ID.",
                repair,
                related,
            }
        };
    [
        OperationDescriptor::active(
            info(
                "retrospective.record",
                "close",
                "Record an attributed account of what worked and what remains unknown.",
                "Prepare a closure or document a meaningful methodological result.",
                "Create a retrospective tied to a verified case snapshot and exact cited record revisions.",
                "Retrospective ID and revision 1.",
                "Use the case revision and genuine journal snapshot for the state examined.",
                "Inspect the current case snapshot and replace illustrative digests and references.",
                &["case.close", "retrospective.supersede"],
            ),
            json!({
                "case_ref": {"id":"case_019acd16-0000-7000-8000-000000000001","revision":1},
                "snapshot":{"sequence":1,"head_hash":format!("sha256:{}", "1".repeat(64))},
                "methods_attempted":[],
                "effective_steps":[],
                "invalid_or_misleading_steps":[],
                "none_observed_reason":"The case stopped before any tests were run.",
                "costs_or_constraints":"The measurement prerequisite was unavailable.",
                "residual_unknowns":["Whether the behavior reproduces in the scoped workload."],
                "lessons":[],
                "no_reusable_lesson_reason":"No recorded method ran to completion."
            }),
            retrospective_shape,
            retrospective_record,
        ),
        OperationDescriptor::active(
            info(
                "retrospective.supersede",
                "close",
                "Record which newer account replaces an earlier one.",
                "The earlier account no longer reflects recorded methods or residual unknowns.",
                "Append an attributed supersession and retain the earlier account.",
                "Updated Retrospective revision.",
                "Use the current earlier retrospective and a later existing or same-batch replacement.",
                "Record a newer retrospective covering a verified case snapshot first.",
                &["retrospective.record", "case.close"],
            ),
            json!({
                "previous":{"id":"retro_019acd16-0000-7000-8000-000000000002","revision":1},
                "successor":{"id":"retro_019acd16-0000-7000-8000-000000000003","revision":1},
                "reason":"The newer account addresses the final measurement."
            }),
            |payload: &RetrospectiveSupersede| text(&payload.reason, "/reason"),
            retrospective_supersede,
        ),
        OperationDescriptor::active(
            info(
                "annotation.add",
                "record",
                "Attach an attributed narrative note, authored synopsis or advice feedback.",
                "Record context, a note snapshot (optionally naming the person-owned vault file in human_file), with role synopsis a bounded summary of one subject revision, or with role advice_feedback a deferral or resumption of one emitted gap, without asserting evidence.",
                "Create a classified annotation with exact subjects and no evidentiary standing. A synopsis turns stale when its subject changes materially. A deferral stops next ranking its gap until the gap changes materially or is resumed; it satisfies no gate.",
                "Annotation ID and revision 1.",
                "Use exact revisions or aliases of subjects created earlier in this transaction.",
                "Use an exact subject and either text or a verified artifact with its digest.",
                &["observation.add", "assessment.add"],
            ),
            json!({
                "subject_refs":[{"id":"case_019acd16-0000-7000-8000-000000000001","revision":1}],
                "content":{"kind":"text","text":"Resume when the controlled build is available."}
            }),
            annotation_shape,
            annotation_add,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::standard_catalog,
        command::Command,
        header::SystemClock,
        identity::SystemIdSource,
        state::{Acceptance, AcceptedTransaction, validate_and_apply},
    };

    struct Case {
        state: State,
        case: Option<Id>,
    }

    impl Case {
        fn new(mode: &str) -> Self {
            let mut case = Self {
                state: State::default(),
                case: None,
            };
            let accepted = case.transact(vec![json!({
                "op":"case.create","payload":{"title":"Bounded method investigation","mode":mode},
            })], &[]).unwrap();
            case.case = Some(accepted.case_id);
            case
        }

        fn transact(
            &mut self,
            operations: Vec<Value>,
            expected: &[RevisionRef],
        ) -> Result<AcceptedTransaction, Failure> {
            let sequence = self.state.sequence() + 1;
            let command: Command = serde_json::from_value(json!({
                "schema_version":"inv.command.v0-alpha",
                "command_id":format!("retro-{sequence}"),"case_id":self.case,"actor":"agent:method",
                "preconditions":expected,"operations":operations,
            }))
            .unwrap();
            let accepted = validate_and_apply(
                &self.state,
                &command,
                &standard_catalog(),
                Acceptance {
                    sequence,
                    clock: &mut SystemClock,
                    ids: &mut SystemIdSource,
                },
            )?;
            self.state.apply(&accepted)?;
            // Emulates the Store's verified replay interface without file publication.
            self.state.note_verified_snapshot(Snapshot {
                sequence,
                head_hash: format!("sha256:{sequence:064x}").parse().unwrap(),
            })?;
            Ok(accepted)
        }

        fn clone_state(&self) -> Self {
            Self {
                state: self.state.clone(),
                case: self.case,
            }
        }

        fn case_ref(&self) -> RevisionRef {
            self.state.current(self.case.unwrap()).unwrap().reference()
        }

        fn retrospective_input(&self, support: Option<Id>) -> Value {
            let sequence = self.state.sequence();
            let step = support.map(
                |id| json!({"description":"Located the unsuccessful run.","support_refs":[id]}),
            );
            json!({
                "case_ref":self.case_ref(),
                "snapshot":{"sequence":sequence,"head_hash":self.state.verified_snapshot(sequence).unwrap()},
                "methods_attempted":[{"name":"Controlled synthetic run"}],
                "effective_steps":[],
                "invalid_or_misleading_steps":step.iter().collect::<Vec<_>>(),
                "none_observed_reason":step.is_none().then_some("No invalid work was recorded."),
                "costs_or_constraints":"One short test window.",
                "residual_unknowns":["Whether the measurement reproduces."],
                "lessons":[],
                "no_reusable_lesson_reason":"Insufficient evidence for portability.",
            })
        }

        fn failed_run(&mut self) -> RevisionRef {
            self.transact(vec![json!({
                "op":"run.record","payload":{
                    "intent":{"kind":"exploration","intent":"Record the initial synthetic attempt."},
                    "producer":"agent:producer","artifact_refs":[],"execution_status":"failed",
                    "evidence_completeness":"complete","evidence_kind":"observational","variables_changed":[],
                }
            })], &[]).unwrap().records[0].reference()
        }
    }

    fn close(extra: Value) -> Value {
        let mut payload = json!({
            "outcome":"inconclusive","reason":"The recorded work cannot resolve the question.",
            "unresolved_questions":[],"reopen_condition":"A verified comparable run becomes available."
        });
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        json!({"op":"case.close","payload":payload})
    }

    #[test]
    fn material_case_requires_an_account_and_same_batch_close_accepts_exact_alias() {
        let mut case = Case::new("explore");
        let run = case.failed_run();
        let expected = case.case_ref();
        let before = case.state.clone();
        let failure = case
            .transact(
                vec![close(json!({"retrospective_waived":"Waive by mistake."}))],
                &[expected],
            )
            .unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::RetrospectiveRequired);
        let repair = failure.error.repair.clone().unwrap();
        assert_eq!(repair.schema.as_deref(), Some("retrospective.record"));
        assert_eq!(
            repair.argv,
            Some(crate::retrospective_digest::draft_argv(expected.id))
        );
        assert_eq!(failure.error.actual, Some(json!([run])));
        assert_eq!(case.state, before);

        let payload = case.retrospective_input(Some(run.id));
        let accepted = case
            .transact(
                vec![
                    json!({"op":"retrospective.record","payload":payload,"client_ref":"account"}),
                    close(json!({"retrospective_ref":{"client_ref":"account"}})),
                ],
                &[expected],
            )
            .unwrap();
        let retrospective = accepted.records[0].reference();
        assert_eq!(accepted.records[0].body["read_set"], json!([run]));
        assert_eq!(
            accepted.records[1].body["closure"]["retrospective_ref"],
            json!(retrospective)
        );
        assert_eq!(
            case.state.current(expected.id).unwrap().body["lifecycle"],
            "closed"
        );
    }

    #[test]
    fn stale_snapshot_blocks_closure_but_notes_do_not_stale_current_account() {
        let mut case = Case::new("explore");
        let run = case.failed_run();
        let old = case.transact(vec![json!({"op":"retrospective.record","payload":case.retrospective_input(Some(run.id))})], &[]).unwrap().records[0].reference();
        let second = case.failed_run();
        let expected = case.case_ref();
        let failure = case
            .transact(vec![close(json!({"retrospective_ref":old}))], &[expected])
            .unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::RetrospectiveStale);
        assert_eq!(failure.error.actual, Some(json!([second])));

        let payload = case.retrospective_input(Some(second.id));
        let newer = case.transact(vec![
            json!({"op":"retrospective.record","payload":payload,"client_ref":"new"}),
            json!({"op":"retrospective.supersede","payload":{"previous":old,"successor":{"client_ref":"new"},"reason":"Cover the second run."}}),
        ], &[]).unwrap().records[0].reference();
        assert_eq!(
            case.state.current(old.id).unwrap().body["status"],
            "superseded"
        );
        assert_eq!(case.state.revision(old).unwrap().body["status"], "recorded");
        let note = case.transact(vec![json!({"op":"annotation.add","payload":{
            "subject_refs":[expected],"content":{"kind":"text","text":"Pending discussion."},
            "classification":{"level":"restricted","reason":"Author requested restricted handling.","tags":[]},
        }})], &[]).unwrap();
        assert_eq!(
            note.records[0].header.classification.level,
            ClassificationLevel::Restricted
        );
        assert_eq!(note.records[0].body["evidentiary_status"], "none");
        assert!(
            case.transact(vec![close(json!({"retrospective_ref":newer}))], &[expected])
                .is_ok()
        );
    }

    #[test]
    fn only_nonmaterial_cases_can_waive_and_record_only_cases_need_neither() {
        let mut explore = Case::new("explore");
        let expected = explore.case_ref();
        assert!(
            explore
                .transact(vec![close(json!({}))], &[expected])
                .is_err()
        );
        let accepted = explore
            .transact(
                vec![close(
                    json!({"retrospective_waived":"No material work occurred."}),
                )],
                &[expected],
            )
            .unwrap();
        assert_eq!(
            accepted.records[0].body["closure"]["retrospective_waived"],
            "No material work occurred."
        );

        let mut record = Case::new("record");
        let expected = record.case_ref();
        assert!(record.transact(vec![close(json!({}))], &[expected]).is_ok());
    }

    #[test]
    fn unknown_or_future_claims_and_reserved_annotations_never_commit() {
        let mut case = Case::new("explore");
        let expected = case.case_ref();
        let mut payload = case.retrospective_input(None);
        payload["snapshot"]["head_hash"] = json!(format!("sha256:{}", "f".repeat(64)));
        let stale = case
            .transact(
                vec![json!({"op":"retrospective.record","payload":payload})],
                &[],
            )
            .unwrap_err();
        assert_eq!(stale.error.code, ErrorCode::RetrospectiveStale);

        let run = case.failed_run();
        let mut payload = case.retrospective_input(Some(run.id));
        let old_sequence = case.state.sequence() - 1;
        payload["snapshot"] = json!({"sequence":old_sequence,"head_hash":case.state.verified_snapshot(old_sequence).unwrap()});
        let unknown = case
            .transact(
                vec![json!({"op":"retrospective.record","payload":payload})],
                &[],
            )
            .unwrap_err();
        assert_eq!(unknown.error.code, ErrorCode::UnknownReference);
        assert_eq!(
            unknown.error.path.as_deref(),
            Some("/operations/0/payload/invalid_or_misleading_steps/0/support_refs/0")
        );

        let bare = case.transact(vec![json!({"op":"annotation.add","payload":{
            "subject_refs":[expected],"content":{"kind":"text","text":"Advice feedback without its object."},"role":"advice_feedback"
        }})], &[]).unwrap_err();
        assert_eq!(
            bare.error.path.as_deref(),
            Some("/operations/0/payload/advice_feedback")
        );
    }

    #[test]
    fn material_work_after_close_in_the_same_batch_rejects_the_entire_transaction() {
        let mut case = Case::new("record");
        let expected = case.case_ref();
        let before = case.state.clone();
        let failure = case.transact(vec![
            close(json!({})),
            json!({"op":"run.record","payload":{
                "intent":{"kind":"exploration","intent":"Record an unsuccessful attempt."},
                "producer":"agent:producer","artifact_refs":[],"execution_status":"failed",
                "evidence_completeness":"complete","evidence_kind":"observational","variables_changed":[]
            }}),
        ], &[expected]).unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::RetrospectiveRequired);
        assert_eq!(case.state, before);
    }

    /// Sources are recorded by ingest; apply the accepted image directly.
    fn source(case: &mut Case) -> RevisionRef {
        use crate::identity::IdSource;
        let id = case.case.unwrap();
        let sequence = case.state.sequence() + 1;
        let mut header = case.state.current(id).unwrap().header.clone();
        header.acceptance_sequence = sequence;
        header.id = SystemIdSource.next_id(Kind::Source).unwrap();
        let record = Record {
            header,
            body: json!({
                "producer":null,"publisher":null,"identity":"bench.log",
                "acquisition":{"channel":"local_file","acquired_at":null,"method":null,"context":null,"environment":null},
                "access":"local_only","verification_limits":[]
            }),
        };
        let source = record.reference();
        case.state
            .apply(&AcceptedTransaction {
                case_id: id,
                sequence,
                records: vec![record],
                created: vec![crate::wire::RecordChange {
                    client_ref: None,
                    id: source.id,
                    revision: 1,
                }],
                updated: vec![],
                warnings: vec![],
            })
            .unwrap();
        source
    }

    fn assess(
        case: &mut Case,
        source: RevisionRef,
        dimension: &str,
        judgment: &str,
    ) -> RevisionRef {
        case.transact(
            vec![json!({"op":"source.assess","payload":{
                "source_ref":source,
                dimension:{"judgment":judgment,"rationale":"Checked against the vendor archive."}
            }})],
            &[],
        )
        .unwrap()
        .records[0]
            .reference()
    }

    #[test]
    fn only_a_withdrawal_or_raised_concern_makes_a_source_revision_material() {
        let mut case = Case::new("explore");
        let id = case.case.unwrap();
        let source = source(&mut case);
        let assessed = assess(&mut case, source, "currency", "current");
        assert!(material_revisions(&case.state, id).is_empty());
        let expected = case.case_ref();
        let mut waivable = case.clone_state();
        waivable
            .transact(
                vec![close(
                    json!({"retrospective_waived":"Only a source check occurred."}),
                )],
                &[expected],
            )
            .unwrap();

        let concern = assess(&mut case, assessed, "currency", "outdated");
        assert_eq!(material_revisions(&case.state, id), vec![concern]);
        let withdrawn = case
            .transact(
                vec![json!({"op":"source.withdraw","payload":{
                    "source_ref":concern,"reason":"retracted_by_origin","scope":{"extent":"entire"},
                    "rationale":"The publisher retracted the report.","evidence_refs":[]
                }})],
                &[],
            )
            .unwrap()
            .records[0]
            .reference();
        assert_eq!(material_revisions(&case.state, id), vec![withdrawn]);
        let failure = case
            .transact(
                vec![close(json!({"retrospective_waived":"Only source checks."}))],
                &[expected],
            )
            .unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::RetrospectiveRequired);
    }
}
