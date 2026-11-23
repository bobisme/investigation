use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    case::{
        CaseAmend, CaseBlocker, CaseBody, CaseClose, CaseClosureDetails, CaseLifecycle, CasePause,
        CaseReopen, CaseTransition, Knowledge, QuestionBody, QuestionDisposition, QuestionResolve,
    },
    catalog::{OperationDescriptor, OperationInfo},
    command::RevisionRef,
    identity::{Id, Kind},
    input,
    pack_policy::{self, Gate, Subject},
    retrospective_ops::{check_retrospective, material_revisions, retrospective_failure},
    state::{OperationContext, State},
    wire::{ErrorCode, Failure},
};

pub(crate) fn operations() -> [OperationDescriptor; 5] {
    [
        OperationDescriptor::active(
            info(
                "case.close",
                "Close a case with an attributed outcome and a method account when required.",
            ),
            json!({
                "outcome":"inconclusive",
                "reason":"The recorded observations do not resolve the question.",
                "unresolved_questions":[],
                "reopen_condition":"A comparable controlled observation becomes available.",
                "retrospective_waived":"The only prerequisite was unavailable, and no material work occurred."
            }),
            |payload: &CaseClose| {
                valid_reason(&payload.reason)?;
                if let Some(waiver) = &payload.retrospective_waived
                    && valid_reason(waiver).is_err()
                {
                    return Err(invalid("Supply a bounded retrospective waiver reason.")
                        .at("/retrospective_waived"));
                }
                if let Some(reopen) = &payload.reopen_condition
                    && valid_reason(reopen).is_err()
                {
                    return Err(
                        invalid("Supply a bounded reopening condition.").at("/reopen_condition")
                    );
                }
                Ok(())
            },
            close,
        ),
        OperationDescriptor::active(
            info("case.amend", "Revise a case's attributed framing."),
            json!({"reason":"The workload includes background requests","title":"Request latency with background traffic"}),
            |payload: &CaseAmend| {
                valid_reason(&payload.reason)?;
                payload.validate().map_err(|blocker| gate(blocker.code()))?;
                if payload
                    .title
                    .as_ref()
                    .is_some_and(|title| title.chars().count() > 256)
                {
                    return Err(
                        invalid("The case title must fit within 256 characters.").at("/title")
                    );
                }
                Ok(())
            },
            amend,
        ),
        OperationDescriptor::active(
            info(
                "case.pause",
                "Pause an active case while a prerequisite is outstanding.",
            ),
            json!({"reason":"Await the independent measurement"}),
            |payload: &CasePause| valid_reason(&payload.reason),
            |payload, context| transition(context, CaseTransition::Pause, payload.reason),
        ),
        OperationDescriptor::active(
            info(
                "case.reopen",
                "Reopen a paused or closed case with an attributed reason.",
            ),
            json!({"reason":"A new observation calls the former scope into question"}),
            |payload: &CaseReopen| valid_reason(&payload.reason),
            |payload, context| transition(context, CaseTransition::Reopen, payload.reason),
        ),
        OperationDescriptor::active(
            info(
                "question.resolve",
                "Answer, defer, or mark a recorded question inconclusive.",
            ),
            json!({
                "question_ref":{"id":"q_018d87fc-5234-7000-8000-000000000002","revision":1},
                "disposition":"inconclusive",
                "reason":"The available observations cannot distinguish the alternatives",
                "evidence_refs":[]
            }),
            |payload: &QuestionResolve| valid_reason(&payload.reason),
            resolve_question,
        ),
    ]
}

fn info(name: &'static str, summary: &'static str) -> OperationInfo {
    OperationInfo {
        name,
        stage: "frame",
        summary,
        when_to_use: summary,
        lifecycle: "An existing case and its applicable current record.",
        effects: "Record an attributed revision and preserve earlier revisions.",
        returns: "Existing record ID and new revision.",
        revision_rule: "Supply the expected revision. Case operations take it from preconditions.",
        idempotency: "Retry with the original actor and command ID.",
        repair: "Inspect the current record and resubmit the exact revision with a reason.",
        related: &["case.create", "question.create"],
    }
}

fn amend(payload: CaseAmend, context: &mut OperationContext<'_>) -> Result<(), Failure> {
    let (reference, mut body) = case_for_update(context)?;
    if body.lifecycle == CaseLifecycle::Closed {
        return Err(gate("Reopen the case before amending its framing."));
    }
    if let Some(title) = payload.title {
        body.title = title;
    }
    if let Some(mode) = payload.mode {
        body.mode = mode;
    }
    set_claim(&mut body.objective, payload.objective);
    set_claim(&mut body.decision_relevance, payload.decision_relevance);
    set_claim(&mut body.expectations, payload.expectations);
    if let Some(scope) = payload.scope {
        body.scope = Some(scope);
    }
    if let Some(stop_condition) = payload.stop_condition {
        body.stop_condition = Some(stop_condition);
    }
    if let Some(exclusions) = payload.exclusions {
        body.exclusions = exclusions;
    }
    if let Some(pins) = payload.policy_pins {
        body.policy_pins = pins;
    }
    if let Some(pins) = payload.method_pins {
        body.method_pins = pins;
    }
    body.transition_reason = Some(payload.reason);
    context.update(reference, encode(body)?)?;
    Ok(())
}

fn set_claim<T>(slot: &mut Option<T>, update: Option<Knowledge<T>>) {
    if let Some(update) = update {
        *slot = match update {
            Knowledge::Unknown => None,
            Knowledge::Known(value) => Some(value),
        };
    }
}

fn transition(
    context: &mut OperationContext<'_>,
    action: CaseTransition,
    reason: String,
) -> Result<(), Failure> {
    let (reference, mut body) = case_for_update(context)?;
    body.lifecycle = body
        .lifecycle
        .transition(action)
        .map_err(|blocker| gate(blocker.code()))?;
    body.transition_reason = Some(reason);
    if body.lifecycle == CaseLifecycle::Active {
        body.closure_outcome = None;
        body.closure = None;
    }
    context.update(reference, encode(body)?)?;
    Ok(())
}

fn close(payload: CaseClose, context: &mut OperationContext<'_>) -> Result<(), Failure> {
    let (expected, mut body) = case_for_update(context)?;
    let case = expected.id;
    let triggers = material_revisions(context.state(), case);
    // A pinned pack may declare further material-case triggers; it can never
    // make a material case waivable.
    let policy = pack_policy::consult(context, Gate::CaseClose)?;
    let pack_triggers = policy.closure_blockers(context.state());
    let material = !triggers.is_empty() || !pack_triggers.is_empty();
    if let Err(blocker) = payload.validate_materiality(body.mode, material) {
        if blocker == CaseBlocker::RetrospectiveRequired {
            let mut failure = retrospective_failure(
                ErrorCode::RetrospectiveRequired,
                "A material case needs a current Retrospective.",
            )
            .at("/retrospective_ref");
            failure.error.actual = Some(json!(triggers));
            if !pack_triggers.is_empty() {
                failure.error.expected = Some(json!({"policy_blockers": pack_triggers}));
            }
            return Err(failure);
        }
        return Err(gate(blocker.code()));
    }
    body.lifecycle = body
        .lifecycle
        .transition(CaseTransition::Close)
        .map_err(|blocker| gate(blocker.code()))?;
    let retrospective = payload
        .retrospective_ref
        .as_ref()
        .map(|reference| context.resolve(reference, Kind::Retrospective, "/retrospective_ref"))
        .transpose()?;
    if let Some(retrospective) = retrospective {
        check_retrospective(context.state(), case, retrospective)
            .map_err(|failure| failure.at("/retrospective_ref"))?;
    }
    let mut unresolved = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, reference) in payload.unresolved_questions.iter().enumerate() {
        let path = format!("/unresolved_questions/{index}");
        let reference = context.resolve(reference, Kind::Question, &path)?;
        unresolved_question(context.state(), case, reference, &path)?;
        if !seen.insert(reference.id) {
            return Err(invalid("List each unresolved question once.").at(&path));
        }
        unresolved.push(reference);
    }
    body.closure_outcome = Some(payload.outcome);
    body.transition_reason = Some(payload.reason);
    body.closure = Some(CaseClosureDetails {
        unresolved_questions: unresolved.clone(),
        reopen_condition: payload.reopen_condition,
        retrospective_ref: retrospective,
        retrospective_waived: payload.retrospective_waived,
        gate_policy: None,
    });
    let actor = context.actor().clone();
    let gated = policy.enforce(
        context.state(),
        &actor,
        &Subject {
            body: &encode(&body)?,
            record: context.current(case),
        },
    )?;
    for warning in policy.warnings() {
        context.warn(warning);
    }
    if let Some(closure) = body.closure.as_mut() {
        closure.gate_policy = Some(gated);
    }
    context.update(expected, encode(body)?)?;
    context.check_final(move |state| {
        policy.recheck(state, &actor, case)?;
        if retrospective.is_none() && !policy.closure_blockers(state).is_empty() {
            let mut failure = retrospective_failure(ErrorCode::RetrospectiveRequired, "The final transaction state meets a pinned pack's closure trigger, which needs a current Retrospective.")
                .at("/retrospective_ref");
            failure.error.expected = Some(json!({"policy_blockers": policy.closure_blockers(state)}));
            return Err(failure);
        }
        for (index, reference) in unresolved.into_iter().enumerate() {
            unresolved_question(state, case, reference, &format!("/unresolved_questions/{index}"))?;
        }
        if let Some(retrospective) = retrospective {
            return check_retrospective(state, case, retrospective).map_err(|failure| failure.at("/retrospective_ref"));
        }
        let triggers = material_revisions(state, case);
        if !triggers.is_empty() {
            let mut failure = retrospective_failure(ErrorCode::RetrospectiveRequired, "The final transaction state includes material work that needs a current Retrospective.")
                .at("/retrospective_ref");
            failure.error.actual = Some(json!(triggers));
            return Err(failure);
        }
        Ok(())
    });
    Ok(())
}

fn unresolved_question(
    state: &State,
    case: Id,
    reference: RevisionRef,
    path: &str,
) -> Result<(), Failure> {
    let record = state
        .current(reference.id)
        .filter(|record| record.header.case_id == case)
        .ok_or_else(|| invalid("The unresolved question is unavailable.").at(path))?;
    let current = record.reference();
    if current != reference {
        let mut conflict = Failure::new(
            ErrorCode::RevisionConflict,
            "Use the current revision of the unresolved question.",
        )
        .at(path);
        conflict.error.actual = Some(json!(current));
        return Err(conflict);
    }
    let question: QuestionBody = input::decode(&record.body).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Stored question body has an invalid shape.",
        )
    })?;
    if matches!(
        question.disposition,
        QuestionDisposition::Answered | QuestionDisposition::Superseded
    ) {
        return Err(gate("List only questions that remain unresolved.").at(path));
    }
    Ok(())
}

pub(crate) fn case_for_update(
    context: &OperationContext<'_>,
) -> Result<(RevisionRef, CaseBody), Failure> {
    let id = context
        .case_id()
        .ok_or_else(|| invalid("Select an existing case."))?;
    let revision = *context.preconditions().get(&id).ok_or_else(|| {
        invalid("Include the current case ID and revision in the transaction preconditions.")
    })?;
    let record = context
        .current(id)
        .ok_or_else(|| invalid("The selected case is absent."))?;
    let body = input::decode(&record.body).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Stored case body has an invalid shape.",
        )
    })?;
    Ok((RevisionRef { id, revision }, body))
}

fn resolve_question(
    payload: QuestionResolve,
    context: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let id = payload.question_ref.id;
    if id.kind() != Kind::Question {
        return Err(invalid("Select an exact question revision.").at("/question_ref"));
    }
    let record = context.current(id).ok_or_else(|| {
        Failure::new(
            ErrorCode::UnknownReference,
            "The question is absent from this case.",
        )
        .at("/question_ref")
    })?;
    let mut body: QuestionBody = input::decode(&record.body).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Stored question body has an invalid shape.",
        )
    })?;
    payload
        .validate_from(body.disposition)
        .map_err(|blocker| gate(blocker.code()))?;
    let mut evidence = Vec::new();
    for (index, reference) in payload.evidence_refs.iter().enumerate() {
        let path = format!("/evidence_refs/{index}");
        let exact = context.resolve_reference(reference, &path)?;
        if !matches!(
            exact.id.kind(),
            Kind::Observation
                | Kind::Assessment
                | Kind::Comparison
                | Kind::Conclusion
                | Kind::Decision
        ) {
            return Err(invalid(
                "Cite an observation or a recorded judgment relevant to this answer.",
            )
            .at(path));
        }
        evidence.push(exact);
    }
    body.disposition = payload.disposition;
    body.answer = payload.answer;
    body.resolution_reason = Some(payload.reason);
    body.evidence_refs = evidence;
    context.update(payload.question_ref, encode(body)?)?;
    Ok(())
}

fn encode(value: impl Serialize) -> Result<Value, Failure> {
    serde_json::to_value(value).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Could not encode accepted record.",
        )
    })
}

fn valid_reason(reason: &str) -> Result<(), Failure> {
    if reason.trim().is_empty() || reason.chars().count() > 2048 {
        return Err(invalid("Provide an attributed reason of 1–2,048 characters.").at("/reason"));
    }
    Ok(())
}

fn invalid(message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message)
}

fn gate(message: &str) -> Failure {
    Failure::new(ErrorCode::GateFailed, message)
}
