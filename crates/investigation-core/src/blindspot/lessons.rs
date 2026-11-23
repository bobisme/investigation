//! Lesson follow-through (§8.6): a case that read another case's admitted
//! lesson is asked, near or after closure, whether it changed anything. The
//! check reads typed annotation fields only and never quotes a lesson.

use serde_json::Value;

use crate::{identity::Kind, lessons::consultations, state::Record, wire::Failure};

use super::{AuditContext, CheckSource, CheckSpec, Detection, FindingFact, Severity};

pub(super) const CHECKS: &[CheckSpec] = &[CheckSpec {
    check_id: "lesson.consultation_outcome_unrecorded",
    version: "v1",
    subject_kinds: &[Kind::Annotation],
    reads: &[
        "role and lesson_consultation.outcome of each Annotation",
        "lifecycle and header.extensions.inv.closure_ready or inv.ready_to_close of the Case",
        "status of each Retrospective",
    ],
    condition: "The case recorded a consultation of another case's admitted lesson with no outcome, and the case is closed, declared ready, or has an unsuperseded Retrospective. One finding per consultation.",
    why: "A consulted lesson has no record of whether it changed anything in this case.",
    severity: Severity::Advisory,
    severity_reason: "Without the outcome, the lesson's source cannot tell whether it is useful; closure does not require it.",
    suggested_schema: Some("lesson.record_outcome"),
    guide: Some("retrospective"),
    source: CheckSource::Native(consultation_outcome_unrecorded),
}];

fn consultation_outcome_unrecorded(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let Some(case) = context.state.current(context.case_id) else {
        return Ok(Vec::new());
    };
    let mut signal = None;
    if case.body.get("lifecycle").and_then(Value::as_str) == Some("closed") {
        signal = Some(("closure_signal:closed", vec![case.reference()]));
    } else if ["inv.closure_ready", "inv.ready_to_close"]
        .iter()
        .any(|key| case.header.extensions.get(*key).and_then(Value::as_bool) == Some(true))
    {
        signal = Some(("closure_signal:declared_ready", vec![case.reference()]));
    } else {
        let retrospectives: Vec<_> = context
            .current(Kind::Retrospective)
            .into_iter()
            .filter(|record| record.body["status"] != "superseded")
            .map(Record::reference)
            .collect();
        if !retrospectives.is_empty() {
            signal = Some(("closure_signal:retrospective_recorded", retrospectives));
        }
    }
    let Some((signal, signal_refs)) = signal else {
        return Ok(Vec::new());
    };
    Ok(consultations(context.state, context.case_id)
        .into_iter()
        .filter(|(_, body)| body.outcome.is_none())
        .map(|(record, body)| Detection {
            subject_refs: vec![record.reference()],
            related_refs: signal_refs.clone(),
            facts: vec![
                FindingFact {
                    fact: signal.to_owned(),
                    refs: signal_refs.clone(),
                },
                FindingFact {
                    fact: body.lesson.key().to_string(),
                    refs: Vec::new(),
                },
            ],
        })
        .collect())
}
