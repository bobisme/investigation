//! Closure follow-through (§6.4, §6.6): a case closed `answered` whose live
//! Conclusions all need review or are disputed. Closure is not a promotion
//! gate, so the close is recorded; the check says the recorded answer no
//! longer stands as current (bn-2xmp).

use crate::{case_ops::answer_not_current, identity::Kind, wire::Failure};

use super::{AuditContext, CheckSource, CheckSpec, Detection, FindingFact, Severity};

pub(super) const CHECKS: &[CheckSpec] = &[CheckSpec {
    check_id: "closure.answered_without_current_conclusion",
    version: "v1",
    subject_kinds: &[Kind::Conclusion],
    reads: &[
        "lifecycle and closure_outcome of the Case",
        "status and effective freshness of each Conclusion",
    ],
    condition: "The case is closed with outcome answered, it has at least one Conclusion that is not rejected, retracted or superseded, and none of those Conclusions is effective current. One finding for the case, naming each such Conclusion.",
    why: "The recorded answer rests on Conclusions that need review or are disputed, so the closure and the analysis report disagree.",
    severity: Severity::Material,
    severity_reason: "The closure is consequential and the basis of every Conclusion it could rest on has changed or is challenged.",
    suggested_schema: Some("conclusion.request_review"),
    guide: Some("review"),
    source: CheckSource::Native(answered_without_current_conclusion),
}];

fn answered_without_current_conclusion(
    context: &AuditContext<'_>,
) -> Result<Vec<Detection>, Failure> {
    let Some(case) = context.state.current(context.case_id) else {
        return Ok(Vec::new());
    };
    if case.body["lifecycle"] != "closed" || case.body["closure_outcome"] != "answered" {
        return Ok(Vec::new());
    }
    let Some(conclusions) = answer_not_current(context.state, context.case_id, context.freshness)
    else {
        return Ok(Vec::new());
    };
    let facts = conclusions
        .iter()
        .map(|reference| FindingFact {
            fact: format!(
                "freshness:{}",
                context
                    .freshness
                    .get(reference.id)
                    .map_or("missing", |result| result.freshness.as_str())
            ),
            refs: vec![*reference],
        })
        .chain([FindingFact {
            fact: "closure_outcome:answered".to_owned(),
            refs: vec![case.reference()],
        }])
        .collect();
    Ok(vec![Detection {
        subject_refs: conclusions,
        related_refs: vec![case.reference()],
        facts,
    }])
}
