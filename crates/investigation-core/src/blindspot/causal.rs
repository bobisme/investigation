//! Causal and empirical checks of `builtin-blindspot.v1` (plan §8.5, §2.1).
//!
//! Each check reads generic typed fields only: per-rival predictions and
//! expectations, `causal_factors` and `causal_test`, `evidence_kind`, attempt
//! counts, recorded Run environments and structured environment identity,
//! effective measurement validity, sample count and window, and the
//! evidence-reuse reading of an assessment of a frozen plan. Nothing here
//! reads statement text, infers a causal role or names a domain. Facts are
//! fixed tokens with exact revisions; no record text is quoted.
//!
//! A causal claim without any acknowledged rival account is reported by the
//! seed check `conclusion.causal_claim_without_rival_account`; the rival
//! check here starts where that one stops, when rivals are recorded.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::{
    command::RevisionRef,
    evidence::MeasurementValidity,
    evidence_reuse::{evidence_reuse, frozen_basis},
    gap_rules::{exact_ref, field, is_live, is_retracted},
    header::Knowledge,
    identity::{Id, Kind},
    measurement_fields::{IdentityComponents, MeasurementFields, cited_run, observation_fields},
    measurement_ops::{
        ValidityBasis, VerificationRequest, judgment_log, recorded_environment, validity_view,
    },
    next_rank::live_predictions_by_outcome,
    review::Expectation,
    state::Record,
    wire::Failure,
};

use super::{AuditContext, CheckSource, CheckSpec, Detection, FindingFact, Severity};

#[cfg(test)]
mod tests;

/// Most Observations of one subject compared pairwise for comparability.
const MAX_COMPARED: usize = 16;

pub(super) const CHECKS: &[CheckSpec] = &[
    CheckSpec {
        check_id: "inquiry.non_discriminating_plan",
        version: "v1",
        subject_kinds: &[Kind::Inquiry],
        reads: &[
            "inquiry.mode and inquiry.confirmatory.predictions[].rival_ref and predicted_outcome",
            "lifecycle and predecessor_ref of each InquiryPlan",
            "lifecycle of each predicted rival Hypothesis",
            "intent.plan_ref of each Run",
        ],
        condition: "A draft or frozen confirmatory InquiryPlan without a successor records the same predicted outcome (trimmed text equality) for two or more current live rival Hypotheses. It is reported from the plan alone, before any Run cites it.",
        why: "A confirmatory plan predicts the same outcome for two or more live rivals, so its result cannot tell them apart.",
        severity: Severity::Material,
        severity_reason: "A test supports a hypothesis only to the extent it would probably have failed if a rival were true; a shared prediction cannot do that for these rivals.",
        suggested_schema: Some("inquiry.amend"),
        guide: Some("discriminate"),
        source: CheckSource::Native(non_discriminating_plan),
    },
    CheckSpec {
        check_id: "conclusion.support_not_assessed_against_rival",
        version: "v1",
        subject_kinds: &[Kind::Conclusion],
        reads: &[
            "Conclusion status, support_refs and causal_factors[].hypothesis_ref",
            "subject_ref, relation and rival_expectations of each supporting Assessment",
            "rival_refs and lifecycle of each Hypothesis in the case",
        ],
        condition: "A live Conclusion with support has a live rival: a live Hypothesis linked by rival_refs, in either direction, to a Hypothesis the Conclusion names as a causal factor or that a supporting Assessment is about. For that rival, no supporting Assessment records an expectation other than not_assessed, or every recorded expectation is equally_expected.",
        why: "The support of a Conclusion was never assessed against a live rival, or is equally expected under it.",
        severity: Severity::Material,
        severity_reason: "Evidence that is as expected under a live rival does not favour the Conclusion over it.",
        suggested_schema: Some("assessment.add"),
        guide: Some("discriminate"),
        source: CheckSource::Native(support_not_assessed_against_rival),
    },
    CheckSpec {
        check_id: "causal.failure_only_selection",
        version: "v1",
        subject_kinds: &[Kind::Conclusion],
        reads: &[
            "Conclusion status, causal_factors, support_refs and causal_test",
            "evidence_refs of each supporting Assessment",
            "measurement.attempts observed and timed_out counts of each supporting Observation or its Run",
            "control verifications in each supporting Observation's measurement_assessment",
        ],
        condition: "A live causal Conclusion without a causal_test cites at least one supporting Observation whose attempt counts record every observed attempt as timed out, none whose counts record a non-failing attempt, and no control in any supporting Observation's measurement judgments. Observations without attempt counts are not taken as a baseline.",
        why: "A causal Conclusion rests on failure-filtered observations with no recorded non-failure baseline.",
        severity: Severity::Material,
        severity_reason: "Selecting observations because the failure happened can create an association that does not exist across all attempts.",
        suggested_schema: Some("observation.add"),
        guide: Some("causal-structure"),
        source: CheckSource::Native(failure_only_selection),
    },
    CheckSpec {
        check_id: "causal.observational_only_support",
        version: "v1",
        subject_kinds: &[Kind::Conclusion],
        reads: &[
            "Conclusion status, causal_factors, support_refs and causal_test",
            "evidence_refs of each supporting Assessment",
            "evidence_kind of each supporting Observation and of its Run",
        ],
        condition: "A live causal Conclusion cites supporting Observations (directly, through supporting Assessments or in its causal_test) and none of them, or the Runs they cite, records evidence_kind interventional.",
        why: "A causal Conclusion rests only on observational evidence; it stands as a candidate, not as a confirmed cause.",
        severity: Severity::Material,
        severity_reason: "An observed association can come from a confounder; only an intervention on the named factor can confirm it.",
        suggested_schema: Some("run.record"),
        guide: Some("intervene"),
        source: CheckSource::Native(observational_only_support),
    },
    CheckSpec {
        check_id: "comparability.environment_differs",
        version: "v1",
        subject_kinds: &[Kind::Observation, Kind::Assessment, Kind::Conclusion],
        reads: &[
            "control verifications in each Observation's measurement_assessment",
            "evidence_refs of each Assessment",
            "support_refs, contradiction_refs and causal_test of each Conclusion, and evidence_refs of the Assessments they cite",
            "environment of the Run each Observation cites",
            "measurement.environment_identity and variables_changed of each Observation and its Run",
        ],
        condition: "Two Observations used together were recorded under different conditions: an Observation and a control its measurement judgment cites, or two Observations an active Assessment or live Conclusion relies on. They differ when both cited Runs record an environment and the two differ, or both record a known environment identity component with different known values that neither names in variables_changed. When both Run environments are recorded as string-keyed entries (as a producer-run envelope reports them), they are compared entry by entry and an entry whose key either Observation or its Run names in variables_changed is not a difference.",
        why: "Evidence used together was recorded under a different build or environment.",
        severity: Severity::Material,
        severity_reason: "A difference in build or environment is a possible confounder, so the compared results may not bear on the condition under test.",
        suggested_schema: Some("run.record"),
        guide: Some("baseline"),
        source: CheckSource::Native(environment_differs),
    },
    CheckSpec {
        check_id: "empirical.confirmatory_basis_incomplete",
        version: "v1",
        subject_kinds: &[Kind::Assessment, Kind::Conclusion],
        reads: &[
            "frozen_basis, relation and evidence_refs of each Assessment",
            "lifecycle of the frozen InquiryPlan revision and of its current revision",
            "status, support_refs and causal_test of each validated Conclusion",
            "effective measurement validity and its basis for each cited measurement Observation",
            "measurement.sample_count and measurement.observation_window of each cited measurement Observation or its Run",
        ],
        condition: "A confirmatory claim (an active supporting or contradicting Assessment of a frozen plan, or a validated Conclusion) relies on a measurement Observation that is not effectively valid, is valid only by its recorder's own statement with no instrument check, control or justified equivalent, or has an unrecorded or unknown sample count or observation window; or an Assessment's plan revision is not frozen or the plan is now retired.",
        why: "A confirmatory empirical claim relies on a measurement whose validity, control, sample or window basis is missing, or on a plan that is not frozen.",
        severity: Severity::Material,
        severity_reason: "A confirmatory result is only as strong as the measurement and frozen plan it rests on.",
        suggested_schema: Some("measurement.assess"),
        guide: Some("measure"),
        source: CheckSource::Native(confirmatory_basis_incomplete),
    },
    CheckSpec {
        check_id: "conclusion.known_outcome_reuse_as_prospective",
        version: "v1",
        subject_kinds: &[Kind::Conclusion],
        reads: &[
            "status and support_refs of each Conclusion",
            "frozen_basis of each supporting Assessment",
            "the evidence-reuse classification and stop-rule deviation of each such Assessment (evidence-reuse.v1)",
        ],
        condition: "A live Conclusion cites supporting Assessments of frozen confirmatory plans and none of them is classified fresh or held_out_within_declared_limits, or one of them applied a stop rule that differs from the frozen rule with no early_stop or late_stop deviation recorded.",
        why: "A Conclusion presents a test as prospective although its inputs were already known or its reuse is undetermined, or its stop rule changed without a declared deviation.",
        severity: Severity::Material,
        severity_reason: "A result whose outcome was known before the test, or whose stop rule moved, may reproduce a finding but does not test it prospectively.",
        suggested_schema: Some("inquiry.create"),
        guide: Some("discriminate"),
        source: CheckSource::Native(known_outcome_reuse),
    },
];

// ---------------------------------------------------------------------------
// Shared reads
// ---------------------------------------------------------------------------

fn text<'a>(record: &'a Record, key: &str) -> Option<&'a str> {
    field(&record.body, key).and_then(Value::as_str)
}

fn refs(value: Option<&Value>) -> Vec<RevisionRef> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| exact_ref(Some(item)))
        .collect()
}

/// A Conclusion that is not rejected, retracted or superseded.
fn live_conclusion(record: &Record) -> bool {
    !matches!(
        text(record, "status"),
        Some("rejected" | "retracted" | "superseded")
    ) && !is_retracted(&record.body)
}

/// A record that is not retracted, withdrawn, rejected, superseded or retired.
fn active(record: &Record) -> bool {
    is_live(record)
        && !matches!(
            text(record, "status"),
            Some("retracted" | "withdrawn" | "rejected" | "superseded")
        )
        && text(record, "lifecycle") != Some("withdrawn")
}

fn causal(record: &Record) -> bool {
    field(&record.body, "causal_factors")
        .and_then(Value::as_array)
        .is_some_and(|factors| !factors.is_empty())
}

fn token(value: impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn fact(name: impl Into<String>, refs: Vec<RevisionRef>) -> FindingFact {
    FindingFact {
        fact: name.into(),
        refs,
    }
}

/// Exact cited revisions, in the audited case.
struct Reader<'c, 'a> {
    context: &'c AuditContext<'a>,
}

impl<'a> Reader<'_, 'a> {
    fn revision(&self, reference: RevisionRef) -> Option<&'a Record> {
        self.context
            .state
            .revision(reference)
            .filter(|record| record.header.case_id == self.context.case_id)
    }

    fn current(&self, id: Id) -> Option<&'a Record> {
        self.context
            .state
            .current(id)
            .filter(|record| record.header.case_id == self.context.case_id)
    }

    /// Observations an Assessment cites as evidence, at the cited revisions.
    fn assessment_observations(&self, assessment: &Record) -> Vec<&'a Record> {
        refs(field(&assessment.body, "evidence_refs"))
            .into_iter()
            .filter(|reference| reference.id.kind() == Kind::Observation)
            .filter_map(|reference| self.revision(reference))
            .collect()
    }

    /// Supporting Assessments a Conclusion cites, at the cited revisions.
    fn supporting_assessments(&self, conclusion: &Record) -> Vec<&'a Record> {
        refs(field(&conclusion.body, "support_refs"))
            .into_iter()
            .filter(|reference| reference.id.kind() == Kind::Assessment)
            .filter_map(|reference| self.revision(reference))
            .filter(|assessment| text(assessment, "relation") == Some("supports"))
            .collect()
    }

    /// Observations the Conclusion's causal_test names.
    fn causal_test(&self, conclusion: &Record) -> Vec<&'a Record> {
        let Some(test) = field(&conclusion.body, "causal_test") else {
            return Vec::new();
        };
        ["baseline", "treatment", "reversal"]
            .into_iter()
            .filter_map(|role| exact_ref(test.get(role)))
            .filter(|reference| reference.id.kind() == Kind::Observation)
            .filter_map(|reference| self.revision(reference))
            .collect()
    }

    /// Observations a Conclusion rests on: direct support, the evidence of
    /// supporting Assessments and its causal_test, in that order, once each.
    fn support(&self, conclusion: &Record) -> Vec<&'a Record> {
        let mut observations: Vec<&'a Record> = refs(field(&conclusion.body, "support_refs"))
            .into_iter()
            .filter(|reference| reference.id.kind() == Kind::Observation)
            .filter_map(|reference| self.revision(reference))
            .collect();
        for assessment in self.supporting_assessments(conclusion) {
            observations.extend(self.assessment_observations(assessment));
        }
        observations.extend(self.causal_test(conclusion));
        dedup(observations)
    }

    /// Support plus contradicting Observations and the evidence of
    /// contradicting Assessments.
    fn everything(&self, conclusion: &Record) -> Vec<&'a Record> {
        let mut observations = self.support(conclusion);
        for reference in refs(field(&conclusion.body, "contradiction_refs")) {
            let Some(record) = self.revision(reference) else {
                continue;
            };
            match reference.id.kind() {
                Kind::Observation => observations.push(record),
                Kind::Assessment => observations.extend(self.assessment_observations(record)),
                _ => {}
            }
        }
        dedup(observations)
    }
}

fn dedup(records: Vec<&Record>) -> Vec<&Record> {
    let mut seen = BTreeSet::new();
    records
        .into_iter()
        .filter(|record| seen.insert(record.reference()))
        .collect()
}

fn references(records: &[&Record]) -> Vec<RevisionRef> {
    records.iter().map(|record| record.reference()).collect()
}

// ---------------------------------------------------------------------------
// inquiry.non_discriminating_plan
// ---------------------------------------------------------------------------

fn non_discriminating_plan(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let plans = context.current(Kind::Inquiry);
    let succeeded: BTreeSet<Id> = plans
        .iter()
        .filter_map(|plan| exact_ref(field(&plan.body, "predecessor_ref")))
        .map(|reference| reference.id)
        .collect();
    let runs = context.current(Kind::Run);
    let mut detections = Vec::new();
    for plan in plans {
        let lifecycle = text(plan, "lifecycle").unwrap_or("draft");
        let confirmatory = field(&plan.body, "inquiry")
            .and_then(|inquiry| inquiry.get("mode"))
            .and_then(Value::as_str)
            == Some("confirmatory");
        if !confirmatory
            || !matches!(lifecycle, "draft" | "frozen")
            || succeeded.contains(&plan.header.id)
            || is_retracted(&plan.body)
        {
            continue;
        }
        let shared: Vec<BTreeSet<RevisionRef>> = live_predictions_by_outcome(context.state, plan)
            .into_values()
            .filter(|rivals| rivals.len() >= 2)
            .collect();
        if shared.is_empty() {
            continue;
        }
        let citing: Vec<RevisionRef> = runs
            .iter()
            .filter(|run| {
                field(&run.body, "intent")
                    .and_then(|intent| exact_ref(intent.get("plan_ref")))
                    .is_some_and(|reference| reference.id == plan.header.id)
            })
            .map(|run| run.reference())
            .collect();
        let mut facts = vec![fact(
            format!("plan_lifecycle:{lifecycle}"),
            vec![plan.reference()],
        )];
        facts.push(if citing.is_empty() {
            fact("runs_citing_plan:none", vec![plan.reference()])
        } else {
            fact("runs_citing_plan:recorded", citing.clone())
        });
        let mut related = citing;
        for rivals in shared {
            related.extend(rivals.iter().copied());
            facts.push(fact("same_predicted_outcome", rivals.into_iter().collect()));
        }
        detections.push(Detection {
            subject_refs: vec![plan.reference()],
            related_refs: related,
            facts,
        });
    }
    Ok(detections)
}

// ---------------------------------------------------------------------------
// conclusion.support_not_assessed_against_rival
// ---------------------------------------------------------------------------

/// Live rival Hypotheses of the Hypotheses a Conclusion names or whose
/// supporting Assessments are about: those linked to one of them by
/// rival_refs in either direction, as `assessment.add` counts rivals.
fn live_rivals(
    context: &AuditContext<'_>,
    reader: &Reader<'_, '_>,
    conclusion: &Record,
) -> Vec<RevisionRef> {
    let mut accounts = BTreeSet::new();
    if let Some(factors) = field(&conclusion.body, "causal_factors").and_then(Value::as_array) {
        accounts.extend(
            factors
                .iter()
                .filter_map(|factor| exact_ref(factor.get("hypothesis_ref")))
                .map(|reference| reference.id),
        );
    }
    accounts.extend(
        reader
            .supporting_assessments(conclusion)
            .into_iter()
            .filter_map(|assessment| exact_ref(field(&assessment.body, "subject_ref")))
            .filter(|reference| reference.id.kind() == Kind::Hypothesis)
            .map(|reference| reference.id),
    );
    let named: BTreeSet<Id> = accounts
        .iter()
        .filter_map(|id| reader.current(*id))
        .flat_map(|hypothesis| refs(field(&hypothesis.body, "rival_refs")))
        .map(|reference| reference.id)
        .collect();
    context
        .current(Kind::Hypothesis)
        .into_iter()
        .filter(|hypothesis| !accounts.contains(&hypothesis.header.id) && is_live(hypothesis))
        .filter(|hypothesis| {
            named.contains(&hypothesis.header.id)
                || refs(field(&hypothesis.body, "rival_refs"))
                    .iter()
                    .any(|reference| accounts.contains(&reference.id))
        })
        .map(|hypothesis| hypothesis.reference())
        .collect()
}

fn expectation_of(assessment: &Record, rival: Id) -> Vec<Expectation> {
    field(&assessment.body, "rival_expectations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| exact_ref(item.get("rival_ref")).is_some_and(|r| r.id == rival))
        .filter_map(|item| serde_json::from_value(item.get("expectation")?.clone()).ok())
        .collect()
}

fn support_not_assessed_against_rival(
    context: &AuditContext<'_>,
) -> Result<Vec<Detection>, Failure> {
    let reader = Reader { context };
    let mut detections = Vec::new();
    for conclusion in context.current(Kind::Conclusion) {
        if !live_conclusion(conclusion) || refs(field(&conclusion.body, "support_refs")).is_empty()
        {
            continue;
        }
        let rivals = live_rivals(context, &reader, conclusion);
        if rivals.is_empty() {
            continue;
        }
        let assessments = reader.supporting_assessments(conclusion);
        let mut facts = Vec::new();
        let mut related = references(&assessments);
        for rival in rivals {
            let mut assessed = Vec::new();
            let mut expectations = Vec::new();
            for assessment in &assessments {
                let recorded: Vec<Expectation> = expectation_of(assessment, rival.id)
                    .into_iter()
                    .filter(|expectation| *expectation != Expectation::NotAssessed)
                    .collect();
                if !recorded.is_empty() {
                    assessed.push(assessment.reference());
                    expectations.extend(recorded);
                }
            }
            if expectations.is_empty() {
                facts.push(fact("rival_not_assessed", vec![rival]));
            } else if expectations
                .iter()
                .all(|expectation| *expectation == Expectation::EquallyExpected)
            {
                let mut cited = vec![rival];
                cited.extend(assessed);
                facts.push(fact("support_equally_expected_under_rival", cited));
            } else {
                continue;
            }
            related.push(rival);
        }
        if !facts.is_empty() {
            detections.push(Detection {
                subject_refs: vec![conclusion.reference()],
                related_refs: related,
                facts,
            });
        }
    }
    Ok(detections)
}

// ---------------------------------------------------------------------------
// causal.failure_only_selection and causal.observational_only_support
// ---------------------------------------------------------------------------

fn known_count(count: Option<&crate::measurement_fields::AttemptCount>) -> Option<u64> {
    match count.map(|count| &count.count) {
        Some(Knowledge::Known(value)) => Some(*value),
        _ => None,
    }
}

/// (observed, timed out) attempt counts the Observation or its Run records.
fn attempt_counts(context: &AuditContext<'_>, observation: &Record) -> Option<(u64, Option<u64>)> {
    let fields = observation_fields(context.state, observation);
    let (_, attempts) = fields.first(|fields: &MeasurementFields| fields.attempts.as_ref())?;
    Some((
        known_count(attempts.observed.as_ref())?,
        known_count(attempts.timed_out.as_ref()),
    ))
}

fn has_control(observation: &Record) -> bool {
    judgment_log(&observation.body).is_ok_and(|log| {
        log.judgments
            .iter()
            .flat_map(|judgment| &judgment.verification)
            .any(|item| matches!(item.request, VerificationRequest::Control { .. }))
    })
}

fn failure_only_selection(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let reader = Reader { context };
    let mut detections = Vec::new();
    for conclusion in context.current(Kind::Conclusion) {
        if !live_conclusion(conclusion)
            || !causal(conclusion)
            || field(&conclusion.body, "causal_test").is_some_and(|test| !test.is_null())
        {
            continue;
        }
        let support = reader.support(conclusion);
        let mut failure_only = Vec::new();
        let mut baseline = false;
        for observation in &support {
            match attempt_counts(context, observation) {
                Some((observed, Some(failed))) if observed > 0 && failed >= observed => {
                    failure_only.push(observation.reference());
                }
                Some((observed, Some(failed))) if failed < observed => baseline = true,
                _ => {}
            }
            let current = reader.current(observation.header.id).unwrap_or(observation);
            baseline |= has_control(current);
        }
        if failure_only.is_empty() || baseline {
            continue;
        }
        detections.push(Detection {
            subject_refs: vec![conclusion.reference()],
            related_refs: references(&support),
            facts: vec![
                fact("selection:failure_only_attempts", failure_only),
                fact("baseline:none_recorded", references(&support)),
            ],
        });
    }
    Ok(detections)
}

fn interventional(context: &AuditContext<'_>, observation: &Record) -> bool {
    text(observation, "evidence_kind") == Some("interventional")
        || cited_run(context.state, observation)
            .is_some_and(|run| text(run, "evidence_kind") == Some("interventional"))
}

fn observational_only_support(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let reader = Reader { context };
    let mut detections = Vec::new();
    for conclusion in context.current(Kind::Conclusion) {
        if !live_conclusion(conclusion) || !causal(conclusion) {
            continue;
        }
        let support = reader.support(conclusion);
        if support.is_empty()
            || support
                .iter()
                .any(|observation| interventional(context, observation))
        {
            continue;
        }
        let status = text(conclusion, "status").unwrap_or("candidate");
        let subject = vec![conclusion.reference()];
        let mut facts = vec![
            fact("evidence_kind:observational_only", references(&support)),
            fact(format!("conclusion_status:{status}"), subject.clone()),
            fact("causal_standing:candidate", subject.clone()),
        ];
        if matches!(status, "validated" | "review_ready") {
            facts.push(fact(format!("status_exceeds_evidence:{status}"), subject));
        }
        detections.push(Detection {
            subject_refs: vec![conclusion.reference()],
            related_refs: references(&support),
            facts,
        });
    }
    Ok(detections)
}

// ---------------------------------------------------------------------------
// comparability.environment_differs
// ---------------------------------------------------------------------------

/// What one Observation records about the conditions it was taken under.
struct Conditions {
    reference: RevisionRef,
    environment: Option<String>,
    identity: Option<IdentityComponents>,
    variables: BTreeSet<String>,
}

fn variables(record: &Record) -> impl Iterator<Item = String> + '_ {
    field(&record.body, "variables_changed")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
}

fn conditions(context: &AuditContext<'_>, observation: &Record) -> Conditions {
    let fields = observation_fields(context.state, observation);
    let identity = match fields.first(|fields| fields.environment_identity.as_ref()) {
        Some((_, Knowledge::Known(identity))) => Some(identity.clone()),
        _ => None,
    };
    let mut changed: BTreeSet<String> = variables(observation).collect();
    if let Some(run) = cited_run(context.state, observation) {
        changed.extend(variables(run));
    }
    Conditions {
        reference: observation.reference(),
        environment: recorded_environment(context.state, observation),
        identity,
        variables: changed,
    }
}

/// A Run environment recorded as string-keyed entries, as ingest records a
/// producer-run envelope's reported environment. Free text is `None`.
fn environment_entries(environment: &str) -> Option<BTreeMap<String, String>> {
    serde_json::from_str(environment).ok()
}

/// Whether two recorded Run environments differ in anything not named as a
/// changed variable. Entries are compared key by key when both environments
/// are structured; free-text environments are compared whole.
fn environments_differ(left: &Conditions, right: &Conditions, a: &str, b: &str) -> bool {
    if a == b {
        return false;
    }
    let (Some(a), Some(b)) = (environment_entries(a), environment_entries(b)) else {
        return true;
    };
    a.keys().chain(b.keys()).any(|key| {
        a.get(key) != b.get(key) && !left.variables.contains(key) && !right.variables.contains(key)
    })
}

/// Fact names for how two Observations' recorded conditions differ.
fn differences(left: &Conditions, right: &Conditions, prefix: &str) -> Vec<FindingFact> {
    let pair = vec![left.reference, right.reference];
    let mut facts = Vec::new();
    if let (Some(a), Some(b)) = (&left.environment, &right.environment)
        && environments_differ(left, right, a, b)
    {
        facts.push(fact(
            format!("{prefix}run_environment_differs"),
            pair.clone(),
        ));
    }
    if let (Some(a), Some(b)) = (&left.identity, &right.identity) {
        let differs = a.iter().any(|(component, value)| {
            !left.variables.contains(component)
                && !right.variables.contains(component)
                && matches!(
                    (value, b.get(component)),
                    (Knowledge::Known(x), Some(Knowledge::Known(y))) if x != y
                )
        });
        if differs {
            facts.push(fact(format!("{prefix}environment_identity_differs"), pair));
        }
    }
    facts
}

fn pairwise(context: &AuditContext<'_>, observations: &[&Record]) -> Vec<FindingFact> {
    let compared: Vec<Conditions> = observations
        .iter()
        .take(MAX_COMPARED)
        .map(|observation| conditions(context, observation))
        .collect();
    let mut facts = Vec::new();
    for (index, left) in compared.iter().enumerate() {
        for right in &compared[index + 1..] {
            facts.extend(differences(left, right, ""));
        }
    }
    facts
}

fn environment_differs(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let reader = Reader { context };
    let mut detections = Vec::new();
    let mut push = |subject: &Record, related: Vec<RevisionRef>, facts: Vec<FindingFact>| {
        if !facts.is_empty() {
            detections.push(Detection {
                subject_refs: vec![subject.reference()],
                related_refs: related,
                facts,
            });
        }
    };
    for observation in context.current(Kind::Observation) {
        if !active(observation) {
            continue;
        }
        let Ok(log) = judgment_log(&observation.body) else {
            continue;
        };
        let target = conditions(context, observation);
        let mut seen = BTreeSet::new();
        let mut facts = Vec::new();
        let mut related = Vec::new();
        for verification in log.judgments.iter().flat_map(|j| &j.verification) {
            let VerificationRequest::Control {
                observation_ref, ..
            } = &verification.request
            else {
                continue;
            };
            let Some(control) = reader
                .revision(*observation_ref)
                .filter(|_| seen.insert(*observation_ref))
            else {
                continue;
            };
            let found = differences(&target, &conditions(context, control), "control_");
            if !found.is_empty() {
                related.push(control.reference());
                facts.extend(found);
            }
        }
        push(observation, related, facts);
    }
    for assessment in context.current(Kind::Assessment) {
        if !active(assessment) {
            continue;
        }
        let observations = reader.assessment_observations(assessment);
        push(
            assessment,
            references(&observations),
            pairwise(context, &observations),
        );
    }
    for conclusion in context.current(Kind::Conclusion) {
        if !live_conclusion(conclusion) {
            continue;
        }
        let observations = reader.everything(conclusion);
        push(
            conclusion,
            references(&observations),
            pairwise(context, &observations),
        );
    }
    Ok(detections)
}

// ---------------------------------------------------------------------------
// empirical.confirmatory_basis_incomplete
// ---------------------------------------------------------------------------

fn measurement_valued(observation: &Record) -> bool {
    field(&observation.body, "value")
        .and_then(|value| value.get("kind"))
        .and_then(Value::as_str)
        == Some("measurement")
}

fn knowledge_token<T>(name: &str, value: Option<&Knowledge<T>>) -> Option<String> {
    match value {
        None => Some(format!("{name}:unrecorded")),
        Some(Knowledge::Unknown) => Some(format!("{name}:unknown")),
        Some(Knowledge::Known(_)) => None,
    }
}

/// Facts about the measurement basis of the Observations a claim relies on.
fn measurement_basis(
    context: &AuditContext<'_>,
    reader: &Reader<'_, '_>,
    observations: &[&Record],
) -> Vec<FindingFact> {
    let mut facts = Vec::new();
    for cited in observations
        .iter()
        .filter(|observation| measurement_valued(observation))
    {
        let observation = reader.current(cited.header.id).unwrap_or(cited);
        let reference = vec![observation.reference()];
        match validity_view(context.state, observation) {
            Ok(view) if view.state != MeasurementValidity::Valid => {
                facts.push(fact(
                    format!("validity:{}", token(view.state)),
                    reference.clone(),
                ));
            }
            Ok(view) if view.basis == ValidityBasis::Recorded => {
                facts.push(fact("validity_basis:recorded_only", reference.clone()));
            }
            Ok(_) => {}
            Err(_) => facts.push(fact("validity:unknown", reference.clone())),
        }
        let fields = observation_fields(context.state, observation);
        let sample = fields.first(|fields| fields.sample_count.as_ref());
        let window = fields.first(|fields| fields.observation_window.as_ref());
        for token in [
            knowledge_token("sample_count", sample.map(|(_, value)| value)),
            knowledge_token("observation_window", window.map(|(_, value)| value)),
        ]
        .into_iter()
        .flatten()
        {
            facts.push(fact(token, reference.clone()));
        }
    }
    facts
}

fn confirmatory_basis_incomplete(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let reader = Reader { context };
    let mut detections = Vec::new();
    for assessment in context.current(Kind::Assessment) {
        let Some(basis) = frozen_basis(assessment) else {
            continue;
        };
        if !active(assessment)
            || !matches!(
                text(assessment, "relation"),
                Some("supports" | "contradicts")
            )
        {
            continue;
        }
        let mut observations = reader.assessment_observations(assessment);
        observations.extend(
            basis
                .actual_inputs
                .iter()
                .filter_map(|input| match &input.input_ref {
                    crate::command::Reference::Existing(reference) => Some(*reference),
                    crate::command::Reference::Client(_) => None,
                })
                .filter(|reference| reference.id.kind() == Kind::Observation)
                .filter_map(|reference| reader.revision(reference)),
        );
        let observations = dedup(observations);
        let mut facts = measurement_basis(context, &reader, &observations);
        let mut related = references(&observations);
        let plan_ref = match &basis.plan_ref {
            crate::command::Reference::Existing(reference) => Some(*reference),
            crate::command::Reference::Client(_) => None,
        };
        if let Some(plan) = plan_ref.and_then(|reference| reader.revision(reference)) {
            related.push(plan.reference());
            if text(plan, "lifecycle") != Some("frozen") {
                facts.push(fact("frozen_plan:not_frozen", vec![plan.reference()]));
            }
            if let Some(current) = reader
                .current(plan.header.id)
                .filter(|current| text(current, "lifecycle") == Some("retired"))
            {
                facts.push(fact("frozen_plan:retired", vec![current.reference()]));
            }
        }
        if !facts.is_empty() {
            detections.push(Detection {
                subject_refs: vec![assessment.reference()],
                related_refs: related,
                facts,
            });
        }
    }
    for conclusion in context.current(Kind::Conclusion) {
        if text(conclusion, "status") != Some("validated") {
            continue;
        }
        let observations = reader.support(conclusion);
        let facts = measurement_basis(context, &reader, &observations);
        if !facts.is_empty() {
            detections.push(Detection {
                subject_refs: vec![conclusion.reference()],
                related_refs: references(&observations),
                facts,
            });
        }
    }
    Ok(detections)
}

// ---------------------------------------------------------------------------
// conclusion.known_outcome_reuse_as_prospective
// ---------------------------------------------------------------------------

fn known_outcome_reuse(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let reader = Reader { context };
    let mut detections = Vec::new();
    for conclusion in context.current(Kind::Conclusion) {
        if !live_conclusion(conclusion) {
            continue;
        }
        let tests: Vec<_> = reader
            .supporting_assessments(conclusion)
            .into_iter()
            .filter_map(|assessment| evidence_reuse(context.state, assessment))
            .collect();
        if tests.is_empty() {
            continue;
        }
        let mut facts = Vec::new();
        if !tests
            .iter()
            .any(|test| test.classification.counts_as_prospective())
        {
            let mut by_class = BTreeMap::<&str, Vec<RevisionRef>>::new();
            for test in &tests {
                by_class
                    .entry(test.classification.as_str())
                    .or_default()
                    .push(test.assessment_ref);
            }
            facts.extend(
                by_class.into_iter().map(|(class, assessments)| {
                    fact(format!("evidence_reuse:{class}"), assessments)
                }),
            );
        }
        let moved: Vec<RevisionRef> = tests
            .iter()
            .filter(|test| test.stop_rule_differs_without_deviation)
            .map(|test| test.assessment_ref)
            .collect();
        if !moved.is_empty() {
            facts.push(fact("stop_rule_differs_without_deviation", moved));
        }
        if facts.is_empty() {
            continue;
        }
        let mut related = Vec::new();
        for test in &tests {
            related.push(test.assessment_ref);
            related.push(test.plan_ref);
        }
        detections.push(Detection {
            subject_refs: vec![conclusion.reference()],
            related_refs: related,
            facts,
        });
    }
    Ok(detections)
}
