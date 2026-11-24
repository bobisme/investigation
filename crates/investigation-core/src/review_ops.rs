//! Attributed review, scoped objections, and conclusion promotion.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{
    PROGRAM_NAME,
    basis::{
        MembershipBasis, MembershipRule, MembershipScope, ObjectionBody, basis_digest, membership,
        membership_members,
    },
    catalog::{OperationDescriptor, OperationInfo},
    command::{Reference, RevisionRef},
    evaluate::ThresholdOperator,
    evidence::{
        ArtifactAvailability, ArtifactBody, ArtifactIntegrity, MeasurementValidity,
        ObservationBasis, ObservationBody, SelectorValidation, TextQuoteState,
    },
    freshness::{Freshness, FreshnessIndex},
    guardrails::GuardedOperation,
    header::Knowledge,
    identity::{Id, Kind},
    objection_policy::{open_objections, raised_after_review},
    pack_policy::{self, Gate, Subject},
    review::{
        AssessmentRelation, ConclusionCreate, ConclusionStatus, ConclusionValidate,
        ContextIsolation, EffectiveDisposition, ObjectionChange, ObjectionRaise, ObjectionSeverity,
        ObjectionStatus, ReviewContext, ReviewStatus, ReviewSubmit, ValidationVerdict,
    },
    state::{OperationContext, Record, State},
    wire::{Digest, ErrorCode, Failure},
};

const EVIDENCE: &[Kind] = &[
    Kind::Observation,
    Kind::Assessment,
    Kind::Artifact,
    Kind::Source,
    Kind::Run,
    Kind::Comparison,
    Kind::Conclusion,
];
const SUBJECTS: &[Kind] = &[
    Kind::Case,
    Kind::Question,
    Kind::Conclusion,
    Kind::Decision,
    Kind::Hypothesis,
    Kind::Comparison,
];

/// The review body stores exact references and attributed review statements.
/// Memberships are computed at acceptance, including empty scoped sets.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidationReviewBody {
    pub kind: ReviewKind,
    pub status: ReviewStatus,
    pub subject_ref: RevisionRef,
    pub basis_digest: Digest,
    pub read_set: Vec<RevisionRef>,
    pub memberships: Vec<MembershipBasis>,
    pub method: String,
    pub materials_examined: Vec<RevisionRef>,
    pub shared_dependencies: Vec<String>,
    pub objection_refs: Vec<RevisionRef>,
    pub verdict: ValidationVerdict,
    pub caveats: Vec<String>,
    pub context_isolation: ContextIsolation,
    pub context: ReviewContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewKind {
    Validation,
    Blindspot,
}

#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CausalTest {
    factor_ref: Reference,
    variable: String,
    baseline: Reference,
    treatment: Reference,
    reversal: Reference,
    failure_boundary: FailureBoundary,
}

#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FailureBoundary {
    operator: ThresholdOperator,
    value: crate::input::DecimalString,
    unit: String,
}

impl FailureBoundary {
    fn matches(&self, measured: &crate::input::DecimalString) -> bool {
        let ordering = measured.numeric_cmp(&self.value);
        match self.operator {
            ThresholdOperator::LessThan => ordering.is_lt(),
            ThresholdOperator::AtMost => ordering.is_le(),
            ThresholdOperator::Equal => ordering.is_eq(),
            ThresholdOperator::AtLeast => ordering.is_ge(),
            ThresholdOperator::GreaterThan => ordering.is_gt(),
        }
    }
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConclusionRequest {
    #[serde(flatten)]
    content: ConclusionCreate,
    #[serde(skip_serializing_if = "Option::is_none")]
    causal_test: Option<CausalTest>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConclusionAmend {
    conclusion_ref: RevisionRef,
    replacement: ConclusionRequest,
    reason: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConclusionReview {
    conclusion_ref: RevisionRef,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConclusionDisposition {
    conclusion_ref: RevisionRef,
    status: ConclusionStatus,
    reason: String,
    evidence_refs: Vec<Reference>,
    successor_ref: Option<Reference>,
}

fn failure(code: ErrorCode, path: &str, message: impl Into<String>) -> Failure {
    Failure::new(code, message).at(path)
}

fn gate(path: &str, message: impl Into<String>) -> Failure {
    failure(ErrorCode::GateFailed, path, message)
}

fn invalid(path: &str, message: impl Into<String>) -> Failure {
    failure(ErrorCode::InvalidInput, path, message)
}

fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "Invalid stored review dependency.",
    )
}

fn text(value: &str, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() {
        return Err(invalid(path, "Supply nonblank text."));
    }
    Ok(())
}

fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, Failure> {
    crate::input::decode(value).map_err(|_| integrity())
}

fn encode(value: impl Serialize) -> Result<Value, Failure> {
    serde_json::to_value(value).map_err(|_| integrity())
}

fn resolve(
    ctx: &OperationContext<'_>,
    reference: &Reference,
    kinds: &[Kind],
    path: &str,
) -> Result<RevisionRef, Failure> {
    if let Reference::Existing(exact) = reference {
        let kind = exact.id.kind();
        return if kinds.contains(&kind) {
            ctx.resolve(reference, kind, path)
        } else {
            Err(invalid(
                path,
                "This record kind is unsuitable for the reference.",
            ))
        };
    }
    kinds
        .iter()
        .find_map(|kind| ctx.resolve(reference, *kind, path).ok())
        .ok_or_else(|| {
            failure(
                ErrorCode::UnknownReference,
                path,
                "Use an exact same-case revision or a preceding alias of the expected kind.",
            )
        })
}

fn refs(
    ctx: &OperationContext<'_>,
    entries: &[Reference],
    kinds: &[Kind],
    path: &str,
) -> Result<Vec<RevisionRef>, Failure> {
    let mut seen = BTreeSet::new();
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let path = format!("{path}/{index}");
            let revision = resolve(ctx, entry, kinds, &path)?;
            if !seen.insert(revision.id) {
                return Err(invalid(&path, "Supply one revision per record."));
            }
            Ok(revision)
        })
        .collect()
}

fn current<'a>(
    state: &'a State,
    case: Id,
    expected: RevisionRef,
    path: &str,
) -> Result<&'a Record, Failure> {
    let record = state
        .current(expected.id)
        .filter(|record| record.header.case_id == case)
        .ok_or_else(|| {
            failure(
                ErrorCode::UnknownReference,
                path,
                "The record is unavailable in this case.",
            )
        })?;
    if record.reference() != expected {
        let mut conflict = failure(
            ErrorCode::RevisionConflict,
            path,
            "The record changed. Inspect its current revision.",
        );
        conflict.error.expected = Some(json!(expected.revision));
        conflict.error.actual = Some(json!(record.reference().revision));
        conflict.error.retry = Some("reassess".into());
        return Err(conflict);
    }
    Ok(record)
}

/// The revision a promotion reads for a cited one. A cited Source or Artifact
/// reads its current revision when only evidentiary use changed: the reader
/// examines the judgments too. A cited Assessment or Conclusion reads its
/// current revision when only its likelihood changed. Any cited record reads
/// its current revision when the shared freshness predicate finds the change
/// benign ([`crate::freshness::benign_successor`]), such as a measurement
/// judgment that keeps or raises effective validity or a criterion restated
/// without change. Any other change is a revision conflict naming the record
/// and the trigger. The freshness index's basis-expansion check walks this
/// same closure, so a benign revision neither blocks a promotion nor hides a
/// new required read from the index. `reader` is the record whose field cites
/// `reference`, so the reader-relative resolution rule applies as it does to
/// the index's edge from that record; `None` judges without a reader.
fn cited<'a>(
    state: &'a State,
    case: Id,
    reference: RevisionRef,
    path: &str,
    reader: Option<&Record>,
) -> Result<&'a Record, Failure> {
    let successor = match reference.id.kind() {
        Kind::Source | Kind::Artifact => crate::source_ops::use_successor(state, case, reference),
        Kind::Assessment | Kind::Conclusion => {
            crate::likelihood::likelihood_only_successor(state, case, reference)
        }
        _ => None,
    }
    .or_else(|| match reader {
        Some(reader) => crate::freshness::benign_successor_for(state, case, reference, reader),
        None => crate::freshness::benign_successor(state, case, reference),
    });
    if let Some(record) = successor {
        return Ok(record);
    }
    current(state, case, reference, path).map_err(|mut failure| {
        if failure.error.code == ErrorCode::RevisionConflict
            && let Some(trigger) = match reader {
                Some(reader) => {
                    crate::freshness::successor_change_for(state, case, reference, reader)
                }
                None => crate::freshness::successor_change(state, case, reference),
            }
        {
            failure.error.message = format!(
                "{} changed since revision {}: {}. Inspect its current revision.",
                reference.id,
                reference.revision,
                trigger.describe()
            );
        }
        failure
    })
}

fn scoped<'a>(
    ctx: &'a OperationContext<'_>,
    expected: RevisionRef,
    kind: Kind,
    path: &str,
) -> Result<&'a Record, Failure> {
    if expected.id.kind() != kind {
        return Err(invalid(path, format!("Expected a {kind} record.")));
    }
    current(
        ctx.state(),
        ctx.case_id().ok_or_else(integrity)?,
        expected,
        path,
    )
}

fn add_field_refs(
    body: &Value,
    fields: &[&str],
    output: &mut Vec<RevisionRef>,
) -> Result<(), Failure> {
    for field in fields {
        match &body[*field] {
            Value::Null => {}
            value @ Value::Array(_) => output.extend(decode::<Vec<RevisionRef>>(value)?),
            value => output.push(decode(value)?),
        }
    }
    Ok(())
}

/// The promotion closure walks the same field table as the freshness index.
fn links(record: &Record) -> Result<Vec<RevisionRef>, Failure> {
    crate::freshness::closure_links(record)
}

fn inactive(record: &Record) -> bool {
    let body = &record.body;
    body["retracted"] == true
        || matches!(
            body["status"].as_str(),
            Some("retracted" | "withdrawn" | "rejected" | "superseded")
        )
        || matches!(
            body["lifecycle"].as_str(),
            Some("withdrawn" | "retired" | "rejected_within_scope")
        )
        || matches!(
            body["availability"].as_str(),
            Some("missing" | "intentionally_removed")
        )
}

fn subject_memberships(state: &State, target: &Record) -> Result<[MembershipBasis; 2], Failure> {
    let scope = MembershipScope::Subject {
        case_id: target.header.case_id,
        subject_id: target.header.id,
    };
    Ok([
        membership(state, MembershipRule::OpenObjections, scope.clone())?,
        membership(state, MembershipRule::MaterialAssumptions, scope)?,
    ])
}

pub fn promotion_memberships(
    state: &State,
    target: &Record,
) -> Result<Vec<MembershipBasis>, Failure> {
    let mut memberships = subject_memberships(state, target)?.to_vec();
    if target.header.id.kind() == Kind::Decision {
        let question: RevisionRef = decode(&target.body["question_ref"])?;
        let selected: Option<RevisionRef> = decode(&target.body["choice"]["option_ref"])?;
        let scope = |option_id| MembershipScope::Question {
            case_id: target.header.case_id,
            question_id: question.id,
            option_id,
        };
        memberships.push(membership(
            state,
            MembershipRule::EligibleOptions,
            scope(None),
        )?);
        memberships.push(membership(
            state,
            MembershipRule::ActiveHardCriteria,
            scope(selected.map(|option| option.id)),
        )?);
    }
    Ok(memberships)
}

fn causal_rivals(state: &State, record: &Record) -> Result<Vec<RevisionRef>, Failure> {
    causal_rivals_in(state, record, false)
}

/// Rival hypotheses of a causal conclusion's factors. With `tolerant`, a
/// factor whose later revision the gate would refuse is read at its cited
/// revision ([`required_reads`]).
fn causal_rivals_in(
    state: &State,
    record: &Record,
    tolerant: bool,
) -> Result<Vec<RevisionRef>, Failure> {
    if record.header.id.kind() != Kind::Conclusion {
        return Ok(Vec::new());
    }
    let mut factors = BTreeSet::new();
    let mut questions = BTreeSet::new();
    let case = record.header.case_id;
    for factor in record.body["causal_factors"]
        .as_array()
        .ok_or_else(integrity)?
    {
        let factor: RevisionRef = decode(&factor["hypothesis_ref"])?;
        let hypothesis = match cited(state, case, factor, "/causal_factors", Some(record)) {
            Err(_) if tolerant => state.revision(factor).ok_or_else(integrity)?,
            read => read?,
        };
        let question: RevisionRef = decode(&hypothesis.body["question_ref"])?;
        factors.insert(factor.id);
        questions.insert(question.id);
    }
    if factors.is_empty() {
        return Ok(Vec::new());
    }
    let mut rivals = Vec::new();
    for hypothesis in state.records().filter(|hypothesis| {
        hypothesis.header.case_id == case
            && hypothesis.header.id.kind() == Kind::Hypothesis
            && !factors.contains(&hypothesis.header.id)
            && !inactive(hypothesis)
    }) {
        let question: RevisionRef = decode(&hypothesis.body["question_ref"])?;
        if questions.contains(&question.id) {
            rivals.push(hypothesis.reference());
        }
    }
    Ok(rivals)
}

/// Exact dependency closure, including current revisions of material assumptions
/// whose reverse links name the subject or a transitive dependency.
pub fn required_read_set(state: &State, root: &Record) -> Result<Vec<RevisionRef>, Failure> {
    dependency_closure(state, root, false)
}

/// The records a reviewer of `root` must read: the same closure as
/// [`required_read_set`], walked without its promotion refusals. A
/// dependency whose later revision is not benign is read at its cited
/// revision, and a withdrawn or rejected dependency, a circular citation or
/// an assessment of an older claim basis is kept rather than refused. The
/// freshness index checks a validation review's read set against it, so a
/// new required read (such as a material assumption about a closure record)
/// is reported however else the closure changed. Only the IDs are meaningful.
pub fn required_reads(state: &State, root: &Record) -> Result<Vec<RevisionRef>, Failure> {
    dependency_closure(state, root, true)
}

fn dependency_closure(
    state: &State,
    root: &Record,
    tolerant: bool,
) -> Result<Vec<RevisionRef>, Failure> {
    struct Walk<'a> {
        state: &'a State,
        root: &'a Record,
        tolerant: bool,
        open: BTreeSet<Id>,
        seen: BTreeMap<Id, RevisionRef>,
    }
    impl<'a> Walk<'a> {
        /// A promotion refusal, or nothing in the tolerant walk.
        fn refuse(&self, message: &str) -> Result<(), Failure> {
            if self.tolerant {
                Ok(())
            } else {
                Err(gate("/read_set", message))
            }
        }

        fn visit(&mut self, record: &Record) -> Result<(), Failure> {
            let id = record.header.id;
            if self.open.len() >= 128 {
                return Err(gate(
                    "/read_set",
                    "The evidence dependency chain exceeds the promotion bound.",
                ));
            }
            if !self.open.insert(id) {
                return self.refuse("The evidence basis contains circular justification.");
            }
            let mut dependencies = links(record)?;
            dependencies.extend(causal_rivals_in(self.state, record, self.tolerant)?);
            // Only the members are read here, so their basis is not hashed.
            let assumptions = membership_members(
                self.state,
                MembershipRule::MaterialAssumptions,
                &MembershipScope::Subject {
                    case_id: self.root.header.case_id,
                    subject_id: id,
                },
            )?;
            for assumption in assumptions {
                dependencies.push(
                    self.state
                        .current(assumption.id)
                        .ok_or_else(integrity)?
                        .reference(),
                );
            }
            // An Assessment's subject is an association when this is itself the
            // Conclusion under review. Its evidence still participates in cycle checks.
            if record.header.id.kind() == Kind::Assessment
                && let Some(subject) = record.body.get("subject_ref")
            {
                let subject: RevisionRef = decode(subject)?;
                if subject.id == self.root.header.id {
                    if let Some(position) = dependencies
                        .iter()
                        .position(|reference| *reference == subject)
                    {
                        dependencies.remove(position);
                    }
                    let historic = self.state.revision(subject).ok_or_else(integrity)?;
                    if basis_digest(historic)? != basis_digest(self.root)? {
                        self.refuse("An assessment belongs to an older substantive claim basis.")?;
                    }
                }
            }
            for reference in dependencies {
                let case = self.root.header.case_id;
                // Source and Artifact use changes (assessment, withdrawal) and
                // benign later revisions of any kind keep the cited revision's
                // content; the current revision is what is read.
                let child: &'a Record =
                    match cited(self.state, case, reference, "/read_set", Some(record)) {
                        Err(_) if self.tolerant => {
                            self.state.revision(reference).ok_or_else(integrity)?
                        }
                        read => read?,
                    };
                if inactive(child) {
                    self.refuse(&format!(
                        "Dependency {} is withdrawn, retracted, or retired.",
                        reference.id
                    ))?;
                }
                if self.open.contains(&reference.id) {
                    self.refuse("The evidence basis contains circular justification.")?;
                }
                if self.seen.insert(reference.id, child.reference()).is_none() {
                    self.visit(child)?;
                }
            }
            self.open.remove(&id);
            Ok(())
        }
    }
    let mut walk = Walk {
        state,
        root,
        tolerant,
        open: BTreeSet::new(),
        seen: BTreeMap::new(),
    };
    walk.visit(root)?;
    Ok(walk.seen.into_values().collect())
}

fn shape_conclusion(request: &ConclusionRequest) -> Result<(), Failure> {
    let claim = &request.content;
    text(&claim.statement, "/statement")?;
    text(&claim.scope, "/scope")?;
    text(&claim.significance, "/significance")?;
    for (index, caveat) in claim.caveats.iter().enumerate() {
        text(caveat, &format!("/caveats/{index}"))?;
    }
    if let Some(test) = &request.causal_test {
        text(&test.variable, "/causal_test/variable")?;
        text(
            &test.failure_boundary.unit,
            "/causal_test/failure_boundary/unit",
        )?;
    }
    Ok(())
}

fn pin_conclusion(
    mut request: ConclusionRequest,
    ctx: &OperationContext<'_>,
) -> Result<Value, Failure> {
    let claim = &mut request.content;
    let support = refs(ctx, &claim.support_refs, EVIDENCE, "/support_refs")?;
    let alternatives = [EVIDENCE, &[Kind::Hypothesis]].concat();
    let contradictions = refs(
        ctx,
        &claim.contradiction_refs,
        &alternatives,
        "/contradiction_refs",
    )?;
    claim.support_refs = support.into_iter().map(Reference::Existing).collect();
    claim.contradiction_refs = contradictions
        .into_iter()
        .map(Reference::Existing)
        .collect();
    let mut factors = BTreeSet::new();
    for (index, factor) in claim.causal_factors.iter_mut().enumerate() {
        let path = format!("/causal_factors/{index}/hypothesis_ref");
        let reference = resolve(ctx, &factor.hypothesis_ref, &[Kind::Hypothesis], &path)?;
        if !factors.insert(reference.id) {
            return Err(invalid(&path, "Name a hypothesis once as a causal factor."));
        }
        factor.hypothesis_ref = Reference::Existing(reference);
    }
    if let Some(test) = &mut request.causal_test {
        let factor = resolve(
            ctx,
            &test.factor_ref,
            &[Kind::Hypothesis],
            "/causal_test/factor_ref",
        )?;
        if !factors.contains(&factor.id) {
            return Err(invalid(
                "/causal_test/factor_ref",
                "Name one of the causal factors explicitly addressed by the toggle.",
            ));
        }
        test.factor_ref = Reference::Existing(factor);
        for (name, reference) in [
            ("baseline", &mut test.baseline),
            ("treatment", &mut test.treatment),
            ("reversal", &mut test.reversal),
        ] {
            *reference = Reference::Existing(resolve(
                ctx,
                reference,
                &[Kind::Observation],
                &format!("/causal_test/{name}"),
            )?);
        }
    }
    let mut body = encode(request)?;
    body["status"] = json!(ConclusionStatus::Candidate);
    Ok(body)
}

fn conclusion_create(
    payload: ConclusionRequest,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    ctx.create(Kind::Conclusion, pin_conclusion(payload, ctx)?)?;
    Ok(())
}

fn conclusion_amend(
    payload: ConclusionAmend,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let previous = scoped(
        ctx,
        payload.conclusion_ref,
        Kind::Conclusion,
        "/conclusion_ref",
    )?;
    if inactive(previous) {
        return Err(gate(
            "/conclusion_ref",
            "Create a successor to amend a terminal conclusion.",
        ));
    }
    let mut body = pin_conclusion(payload.replacement, ctx)?;
    body["change_reason"] = json!(payload.reason);
    ctx.update(payload.conclusion_ref, body)?;
    Ok(())
}

fn qualification(
    state: &State,
    case: Id,
    observation: RevisionRef,
) -> Result<(ObservationBody, &Record), Failure> {
    if observation.id.kind() != Kind::Observation {
        return Err(gate(
            "/support_refs",
            "Use an active observation in the same case.",
        ));
    }
    let record = cited(state, case, observation, "/support_refs", None)?;
    if inactive(record) {
        return Err(gate(
            "/support_refs",
            "Use an active observation in the same case.",
        ));
    }
    // Read the typed body only: corrections, assessments, origin traces and
    // guardrails keep their keys beside it.
    let body = ObservationBody::from_stored(&record.body).map_err(|_| integrity())?;
    if crate::measurement_ops::effective_validity(state, record) != MeasurementValidity::Valid {
        return Err(gate(
            "/support_refs",
            "An invalid or unevaluated observation cannot provide direct support.",
        ));
    }
    let ObservationBasis::Source {
        artifact_ref,
        selector,
        text_quote,
    } = &body.basis
    else {
        return Err(gate(
            "/support_refs",
            "Cite a verified observation tied to inspectable original bytes.",
        ));
    };
    if artifact_ref.id.kind() != Kind::Artifact {
        return Err(integrity());
    }
    let artifact = match crate::source_ops::use_successor(state, case, *artifact_ref) {
        Some(artifact) => artifact,
        None => current(state, case, *artifact_ref, "/support_refs")?,
    };
    if let Some(withdrawn) = crate::source_ops::withdrawn_lineage(state, artifact) {
        return Err(gate(
            "/support_refs",
            format!(
                "Evidence from withdrawn {} cannot provide direct support. Withdrawal is not erasure; its bytes stay readable.",
                withdrawn.id
            ),
        ));
    }
    let artifact_body: ArtifactBody = decode(&artifact.body)?;
    let source = match crate::source_ops::use_successor(state, case, artifact_body.source) {
        Some(source) => source,
        None => current(state, case, artifact_body.source, "/support_refs")?,
    };
    if source.header.id.kind() != Kind::Source
        || inactive(source)
        || inactive(artifact)
        || artifact_body.availability != ArtifactAvailability::Retained
        || artifact_body.integrity != ArtifactIntegrity::Verified
        || artifact_body.digest.as_ref() != Some(&selector.artifact_digest)
        || selector.validation != SelectorValidation::Verified
        || !matches!(
            text_quote.state,
            TextQuoteState::Verified | TextQuoteState::NotApplicable
        )
    {
        return Err(gate(
            "/support_refs",
            "Direct support requires retained, verified source bytes and an exact verified selector.",
        ));
    }
    Ok((body, record))
}

fn qualify_run<'a>(state: &'a State, case: Id, record: &Record) -> Result<&'a Record, Failure> {
    let run: RevisionRef = decode(&record.body["run_ref"]).map_err(|_| {
        gate(
            "/causal_test",
            "Every toggle observation must link an exact run revision.",
        )
    })?;
    let run = current(state, case, run, "/causal_test")?;
    if run.header.id.kind() != Kind::Run
        || run.body["execution_status"] != "completed"
        || run.body["evidence_completeness"] != "complete"
        || matches!(
            run.body["evidence_validity"].as_str(),
            Some("invalid" | "partial")
        )
    {
        return Err(gate(
            "/causal_test",
            "Each toggle observation must come from a completed attempt with complete evidence and no invalidity finding.",
        ));
    }
    Ok(run)
}

fn causal_gate(state: &State, conclusion: &Record) -> Result<(), Failure> {
    let factors: Vec<crate::review::CausalFactor> = decode(&conclusion.body["causal_factors"])?;
    if factors.is_empty() {
        return Ok(());
    }
    let test: CausalTest = decode(&conclusion.body["causal_test"]).map_err(|_| {
        gate(
            "/causal_test",
            "A causal claim requires an explicit verified baseline, intervention, and reversal.",
        )
    })?;
    let exact = |reference| match reference {
        Reference::Existing(reference) => Ok(reference),
        Reference::Client(_) => Err(integrity()),
    };
    let factor = exact(test.factor_ref)?;
    if factors.len() != 1
        || !factors
            .iter()
            .any(|row| row.hypothesis_ref == Reference::Existing(factor))
    {
        return Err(gate(
            "/causal_test/factor_ref",
            "This toggle qualifies only the exact named factor. Keep the claim scoped to that factor.",
        ));
    }
    let case = conclusion.header.case_id;
    let baseline_ref = exact(test.baseline)?;
    let treatment_ref = exact(test.treatment)?;
    let reversal_ref = exact(test.reversal)?;
    let (baseline, baseline_observation) = qualification(state, case, baseline_ref)?;
    let (treatment, treatment_observation) = qualification(state, case, treatment_ref)?;
    let (reversal, reversal_observation) = qualification(state, case, reversal_ref)?;
    let baseline_run = qualify_run(state, case, baseline_observation)?;
    let treatment_run = qualify_run(state, case, treatment_observation)?;
    let reversal_run = qualify_run(state, case, reversal_observation)?;
    let builds =
        [&baseline_run, &treatment_run, &reversal_run].map(|run| run.body["environment"].as_str());
    if !matches!(builds, [Some(a), Some(b), Some(c)] if !a.trim().is_empty() && a == b && b == c) {
        return Err(gate(
            "/causal_test",
            "Record a compatible, matching build and environment for the baseline, treatment, and reversal.",
        ));
    }
    if !(baseline_run.header.acceptance_sequence < treatment_run.header.acceptance_sequence
        && treatment_run.header.acceptance_sequence < reversal_run.header.acceptance_sequence)
        || baseline_run.header.id == treatment_run.header.id
        || baseline_run.header.id == reversal_run.header.id
        || treatment_run.header.id == reversal_run.header.id
    {
        return Err(gate(
            "/causal_test",
            "Cite three independently recorded attempts in baseline, treatment, and reversal order.",
        ));
    }
    let expected = json!([test.variable]);
    if [treatment_run, reversal_run].iter().any(|run| {
        run.body["evidence_kind"] != "interventional" || run.body["variables_changed"] != expected
    }) || baseline_run.body["variables_changed"] != json!([])
    {
        return Err(gate(
            "/causal_test/variable",
            "Only the named factor may change in the intervention and reversal.",
        ));
    }
    if [&treatment, &reversal].iter().any(|observation| {
        observation.evidence_kind != Some(crate::evidence::EvidenceKind::Interventional)
            || json!(observation.variables_changed) != expected
    }) || !baseline.variables_changed.is_empty()
    {
        return Err(gate(
            "/causal_test/variable",
            "The qualified observations must record the same intervention as their runs.",
        ));
    }
    match (&baseline.value, &treatment.value, &reversal.value) {
        (
            crate::evidence::ObservationValue::Measurement {
                value: before,
                unit: unit_a,
            },
            crate::evidence::ObservationValue::Measurement {
                value: during,
                unit: unit_b,
            },
            crate::evidence::ObservationValue::Measurement {
                value: after,
                unit: unit_c,
            },
        ) if unit_a == unit_b
            && unit_b == unit_c
            && unit_c == &test.failure_boundary.unit
            && test.failure_boundary.matches(before)
            && !test.failure_boundary.matches(during)
            && test.failure_boundary.matches(after)
            && baseline.extraction_method.is_some()
            && baseline.extraction_method == treatment.extraction_method
            && treatment.extraction_method == reversal.extraction_method =>
        {
            Ok(())
        }
        _ => Err(gate(
            "/causal_test",
            "The declared failure condition must hold at baseline, stop under treatment, and recur on reversal, using a shared unit and extraction method.",
        )),
    }
}

fn ready(state: &State, record: &Record) -> Result<Vec<RevisionRef>, Failure> {
    let support: Vec<RevisionRef> = decode(&record.body["support_refs"])?;
    let mut missing = Vec::new();
    if support.is_empty() {
        missing.push("direct_support");
    }
    if record.body["caveats"].as_array().is_none_or(Vec::is_empty) {
        missing.push("limitations");
    }
    if !missing.is_empty() {
        let mut failure = gate(
            "/conclusion_ref",
            "The candidate is missing required promotion inputs.",
        );
        failure.error.actual = Some(json!(missing));
        return Err(failure);
    }
    crate::absence_gate::promotion_gate(state, record)?;
    let basis = required_read_set(state, record)?;
    let case = record.header.case_id;
    let mut direct = false;
    for reference in support {
        let evidence = cited(state, case, reference, "/support_refs", Some(record))?;
        match reference.id.kind() {
            Kind::Observation => {
                qualification(state, case, reference)?;
                direct = true;
            }
            Kind::Assessment
                if evidence.body["relation"] == json!(AssessmentRelation::Supports) =>
            {
                for observation in decode::<Vec<RevisionRef>>(&evidence.body["evidence_refs"])? {
                    if observation.id.kind() == Kind::Observation {
                        qualification(state, case, observation)?;
                        direct = true;
                    }
                }
            }
            _ => {}
        }
    }
    if !direct {
        return Err(gate(
            "/support_refs",
            "Link a qualified observation or a supportive assessment of qualified observation evidence.",
        ));
    }
    causal_gate(state, record)?;
    let rivals = causal_rivals(state, record)?;
    if !rivals.is_empty() {
        let mut addressed: Vec<RevisionRef> = decode(&record.body["contradiction_refs"])?;
        for reference in &basis {
            if reference.id.kind() == Kind::Assessment {
                let assessment = current(state, case, *reference, "/contradiction_refs")?;
                if let Some(expectations) = assessment.body["rival_expectations"].as_array() {
                    for expectation in expectations {
                        add_field_refs(expectation, &["rival_ref"], &mut addressed)?;
                    }
                }
            }
        }
        let missing: Vec<_> = rivals
            .into_iter()
            .filter(|rival| !addressed.contains(rival))
            .collect();
        if !missing.is_empty() {
            let mut failure = gate(
                "/contradiction_refs",
                "Expose the live rival explanations or cite assessments that compare them.",
            );
            failure.error.expected = Some(json!(missing));
            return Err(failure);
        }
    }
    Ok(basis)
}

fn require_expected(ctx: &OperationContext<'_>, required: &[RevisionRef]) -> Result<(), Failure> {
    if required
        .iter()
        .any(|reference| ctx.preconditions().get(&reference.id) != Some(&reference.revision))
    {
        let mut failure = gate(
            "/preconditions",
            "Include every current required revision as a transaction precondition.",
        );
        failure.error.expected = Some(json!(required));
        return Err(failure);
    }
    Ok(())
}

fn review_argv(id: Id) -> Vec<String> {
    vec![
        PROGRAM_NAME.into(),
        "show".into(),
        id.to_string(),
        "--depth".into(),
        "full".into(),
        "--format".into(),
        "json".into(),
    ]
}

fn closure_changed(
    record: &Record,
    state: &State,
    basis: &Digest,
    expected: &[RevisionRef],
    sets: &[MembershipBasis],
) -> Result<(), Failure> {
    let current = state.current(record.header.id).ok_or_else(integrity)?;
    if basis_digest(current)? != *basis
        || required_read_set(state, current)? != expected
        || promotion_memberships(state, current)? != sets
    {
        let mut failure = gate(
            "/read_set",
            "The final transaction state changes the required review basis.",
        );
        failure.error.expected = Some(json!(required_read_set(state, current)?));
        return Err(failure.with_argv(review_argv(record.header.id)));
    }
    Ok(())
}

fn conclusion_request_review(
    payload: ConclusionReview,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let record = scoped(
        ctx,
        payload.conclusion_ref,
        Kind::Conclusion,
        "/conclusion_ref",
    )?;
    if record.body["status"] != json!(ConclusionStatus::Candidate) {
        return Err(gate(
            "/conclusion_ref",
            "Request review for a current candidate conclusion.",
        ));
    }
    let reads = ready(ctx.state(), record)
        .map_err(|failure| failure.with_argv(review_argv(record.header.id)))?;
    crate::confidence::require_named_reasons(record, "/conclusion_ref")?;
    let basis = basis_digest(record)?;
    let sets = promotion_memberships(ctx.state(), record)?;
    let mut expected = reads.clone();
    expected.push(payload.conclusion_ref);
    require_expected(ctx, &expected)?;
    let mut body = record.body.clone();
    let tracked = record.clone();
    warn_open_challenges(ctx, &tracked, &reads)?;
    body["status"] = json!(ConclusionStatus::ReviewReady);
    let (body, policy) = apply_pack_policy(ctx, Gate::ConclusionRequestReview, &tracked, body)?;
    ctx.update(payload.conclusion_ref, body)?;
    let actor = ctx.actor().clone();
    let id = tracked.header.id;
    ctx.check_final(move |state| {
        closure_changed(&tracked, state, &basis, &reads, &sets)?;
        policy.recheck(state, &actor, id)
    });
    Ok(())
}

/// Enforce the case's pinned pack policy for a Conclusion gate and store the
/// policy it consulted on the new revision.
fn apply_pack_policy(
    ctx: &mut OperationContext<'_>,
    gate_kind: Gate,
    record: &Record,
    mut body: Value,
) -> Result<(Value, pack_policy::Consulted), Failure> {
    let policy = pack_policy::consult(ctx, gate_kind)?;
    if let Some(fields) = body.as_object_mut() {
        fields.remove("gate_policy");
    }
    let gated = policy.enforce(
        ctx.state(),
        ctx.actor(),
        &Subject {
            body: &body,
            record: Some(record),
        },
    )?;
    for warning in policy.warnings() {
        ctx.warn(warning);
    }
    body["gate_policy"] = pack_policy::encode(&gated)?;
    Ok((body, policy))
}

fn context_shape(context: &ReviewContext) -> Result<(), Failure> {
    text(&context.activity, "/context/activity")?;
    text(&context.dependence_basis, "/context/dependence_basis")?;
    if let Some(Knowledge::Known(involvement)) = &context.prior_involvement {
        for (index, statement) in involvement.iter().enumerate() {
            text(
                statement,
                &format!("/context/prior_involvement/value/{index}"),
            )?;
        }
    }
    Ok(())
}

fn review_shape(payload: &ReviewSubmit) -> Result<(), Failure> {
    match payload {
        ReviewSubmit::Validation {
            method, context, ..
        } => {
            text(method, "/method")?;
            context_shape(context)
        }
        ReviewSubmit::Blindspot {
            method,
            scope_examined,
            checks_considered,
            limitations,
            context,
            ..
        } => {
            text(method, "/method")?;
            text(scope_examined, "/scope_examined")?;
            context_shape(context)?;
            if checks_considered.is_empty() || limitations.is_empty() {
                return Err(invalid(
                    "/checks_considered",
                    "Record the checks performed and the audit limitations.",
                ));
            }
            crate::blindspot_review::shape(&blindspot_detail(payload).ok_or_else(integrity)?)
        }
    }
}

fn blindspot_detail(payload: &ReviewSubmit) -> Option<crate::blindspot_review::Detail<'_>> {
    match payload {
        ReviewSubmit::Blindspot {
            verdict,
            gaps,
            no_gap_reason,
            audit,
            gap_dispositions,
            novel_gaps,
            ..
        } => Some(crate::blindspot_review::Detail {
            verdict: *verdict,
            gaps,
            no_gap_reason: no_gap_reason.as_deref(),
            audit: audit.as_deref(),
            gap_dispositions,
            novel_gaps,
        }),
        ReviewSubmit::Validation { .. } => None,
    }
}

fn normalized_read_set(
    ctx: &OperationContext<'_>,
    target: &Record,
    supplied: &[RevisionRef],
    required: &[RevisionRef],
) -> Result<Vec<RevisionRef>, Failure> {
    let mut by_id = BTreeMap::new();
    for (index, reference) in supplied.iter().enumerate() {
        let path = format!("/read_set/{index}");
        let record = current(ctx.state(), target.header.case_id, *reference, &path)?;
        if inactive(record) || by_id.insert(reference.id, *reference).is_some() {
            return Err(invalid(
                &path,
                "Use one current, active revision per record.",
            ));
        }
    }
    by_id.remove(&target.header.id);
    if required
        .iter()
        .any(|required| by_id.get(&required.id) != Some(required))
    {
        let mut failure = gate(
            "/read_set",
            "The review omits part of the current dependency closure.",
        );
        failure.error.expected = Some(json!(required));
        return Err(failure.with_argv(review_argv(target.header.id)));
    }
    Ok(by_id.into_values().collect())
}

fn review_submit(payload: ReviewSubmit, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let detail = blindspot_detail(&payload);
    let independence = match &payload {
        ReviewSubmit::Validation {
            context_isolation,
            context,
            ..
        }
        | ReviewSubmit::Blindspot {
            context_isolation,
            context,
            ..
        } => crate::review_independence::submission_warnings(*context_isolation, context),
    };
    let mut blindspot = None;
    let mut body = match payload.clone() {
        ReviewSubmit::Validation {
            subject_ref,
            basis_digest: supplied_basis,
            read_set,
            method,
            materials_examined,
            shared_dependencies,
            objection_refs,
            verdict,
            caveats,
            context_isolation,
            context,
        } => {
            let subject_ref = resolve(
                ctx,
                &subject_ref,
                &[Kind::Conclusion, Kind::Decision, Kind::Comparison],
                "/subject_ref",
            )?;
            let target = current(
                ctx.state(),
                ctx.case_id().ok_or_else(integrity)?,
                subject_ref,
                "/subject_ref",
            )?;
            if inactive(target) {
                return Err(gate("/subject_ref", "Review an active target."));
            }
            let basis_digest = basis_digest(target)?;
            if basis_digest != supplied_basis {
                let mut failure = gate(
                    "/basis_digest",
                    "The declared review basis differs from this exact claim or decision.",
                );
                failure.error.expected = Some(json!(basis_digest));
                return Err(failure.with_argv(review_argv(subject_ref.id)));
            }
            let mut required = required_read_set(ctx.state(), target)?;
            let materials_examined =
                refs(ctx, &materials_examined, EVIDENCE, "/materials_examined")?;
            let objection_refs = refs(ctx, &objection_refs, &[Kind::Objection], "/objection_refs")?;
            for reference in &objection_refs {
                let objection: ObjectionBody =
                    decode(&ctx.exact(*reference).ok_or_else(integrity)?.body)?;
                if objection.subject_ref.id != subject_ref.id {
                    return Err(gate(
                        "/objection_refs",
                        "Cite objections concerning the review target.",
                    ));
                }
            }
            required.extend(materials_examined.iter().copied());
            required.extend(objection_refs.iter().copied());
            required.sort_by_key(|reference| (reference.id, reference.revision));
            required.dedup();
            let read_set = normalized_read_set(ctx, target, &read_set, &required)?;
            encode(ValidationReviewBody {
                kind: ReviewKind::Validation,
                status: ReviewStatus::Submitted,
                subject_ref,
                basis_digest,
                read_set,
                memberships: promotion_memberships(ctx.state(), target)?,
                method,
                materials_examined,
                shared_dependencies,
                objection_refs,
                verdict,
                caveats,
                context_isolation,
                context,
            })?
        }
        ReviewSubmit::Blindspot {
            subject_ref,
            audited_snapshot,
            read_set,
            method,
            scope_examined,
            checks_considered,
            gaps,
            no_gap_reason,
            evidence_refs,
            assumption_refs,
            limitations,
            verdict,
            context_isolation,
            context,
            ..
        } => {
            let subject_ref = resolve(ctx, &subject_ref, SUBJECTS, "/subject_ref")?;
            let target = current(
                ctx.state(),
                ctx.case_id().ok_or_else(integrity)?,
                subject_ref,
                "/subject_ref",
            )?;
            let evidence_refs = refs(ctx, &evidence_refs, EVIDENCE, "/evidence_refs")?;
            let assumption_refs = refs(
                ctx,
                &assumption_refs,
                &[Kind::Assumption],
                "/assumption_refs",
            )?;
            let resolved = crate::blindspot_review::resolve(
                ctx,
                subject_ref.id,
                &audited_snapshot,
                detail.as_ref().ok_or_else(integrity)?,
            )?;
            let mut required = evidence_refs.clone();
            required.extend(assumption_refs.iter().copied());
            required.extend(resolved.read.iter().copied());
            let read_set = normalized_read_set(ctx, target, &read_set, &required)?;
            blindspot = Some(resolved);
            json!({
                "kind": ReviewKind::Blindspot, "status": ReviewStatus::Submitted, "subject_ref": subject_ref,
                "audited_snapshot": audited_snapshot, "read_set": read_set, "method": method, "scope_examined": scope_examined,
                "checks_considered": checks_considered, "gaps": gaps, "no_gap_reason": no_gap_reason, "evidence_refs": evidence_refs,
                "assumption_refs": assumption_refs, "limitations": limitations, "verdict": verdict, "context_isolation": context_isolation,
                "context": context,
            })
        }
    };
    if let Some(resolved) = blindspot {
        resolved.store(&mut body, ctx);
    }
    ctx.create(Kind::Review, body)?;
    independence
        .into_iter()
        .for_each(|warning| ctx.warn(warning));
    Ok(())
}

fn critical_objections(
    state: &State,
    case: Id,
    subject: Id,
    refs: &[RevisionRef],
) -> Result<Vec<RevisionRef>, Failure> {
    let relevant: BTreeSet<_> = refs
        .iter()
        .map(|reference| reference.id)
        .chain([subject])
        .collect();
    Ok(open_objections(state, case, &relevant)?
        .into_iter()
        .filter(|objection| objection.severity == ObjectionSeverity::Critical)
        .map(|objection| objection.objection_ref)
        .collect())
}

/// Promotion never hides disagreement: name every open objection on the claim
/// or its basis, and every live rival explanation, in the receipt.
fn warn_open_challenges(
    ctx: &mut OperationContext<'_>,
    record: &Record,
    reads: &[RevisionRef],
) -> Result<(), Failure> {
    let id = record.header.id;
    let relevant: BTreeSet<_> = reads
        .iter()
        .map(|reference| reference.id)
        .chain([id])
        .collect();
    let mut warnings = Vec::new();
    for objection in open_objections(ctx.state(), record.header.case_id, &relevant)? {
        let severity = match objection.severity {
            ObjectionSeverity::Critical => "critical",
            ObjectionSeverity::Material => "material",
        };
        warnings.push(format!(
            "Conclusion {id} has open {severity} objection {} on {}.",
            objection.objection_ref.id, objection.subject_ref.id
        ));
    }
    for rival in causal_rivals(ctx.state(), record)? {
        warnings.push(format!(
            "Conclusion {id} has live rival explanation {}.",
            rival.id
        ));
    }
    for warning in warnings {
        ctx.warn(warning);
    }
    Ok(())
}

fn enforce_no_critical(
    state: &State,
    target: &Record,
    refs: &[RevisionRef],
) -> Result<(), Failure> {
    let critical = critical_objections(state, target.header.case_id, target.header.id, refs)?;
    if !critical.is_empty() {
        let mut failure = gate(
            "/review_ref",
            "Resolve relevant open critical objections before validating.",
        );
        failure.error.actual = Some(json!(critical));
        return Err(failure.with_argv(review_argv(target.header.id)));
    }
    Ok(())
}

/// Check reviewed exact dependencies and computed scoped sets at the caller's
/// verified snapshot. Lifecycle-only promotion leaves the review current.
pub fn validation_review_current(
    state: &State,
    target: &Record,
    review: &ValidationReviewBody,
) -> Result<(), Failure> {
    let case = target.header.case_id;
    if review.kind != ReviewKind::Validation
        || review.status != ReviewStatus::Submitted
        || review.subject_ref.id != target.header.id
        || basis_digest(target)? != review.basis_digest
        || state
            .revision(review.subject_ref)
            .is_none_or(|prior| prior.header.case_id != case)
        || promotion_memberships(state, target)? != review.memberships
    {
        return Err(gate(
            "/review_ref",
            "Obtain a current review against this claim and its scoped membership.",
        )
        .with_argv(vec![
            PROGRAM_NAME.into(),
            "show".into(),
            target.header.id.to_string(),
            "--case".into(),
            case.to_string(),
            "--depth".into(),
            "full".into(),
            "--format".into(),
            "json".into(),
        ])
        .with_schema("review.submit"));
    }
    let required = required_read_set(state, target)?;
    // A reviewed revision still covers its current revision when the shared
    // freshness predicate finds every change since benign, such as a Source
    // judgment that raises no concern or a measurement judgment that keeps or
    // raises effective validity. The review reads for its subject, so the
    // subject is the reader: a question answered with the subject (or with a
    // record resting on it) is unchanged to the review, as in the index.
    let covered = |revision: &RevisionRef| {
        review.read_set.iter().any(|read| {
            read == revision
                || (read.id == revision.id
                    && crate::freshness::benign_successor_for(state, case, *read, target)
                        .is_some_and(|current| current.reference() == *revision))
        })
    };
    if !required.iter().all(covered) {
        let mut failure = gate(
            "/read_set",
            "The reviewed dependency closure is stale or incomplete.",
        );
        failure.error.expected = Some(json!(required));
        return Err(failure);
    }
    for reference in &review.read_set {
        let examined = match crate::freshness::benign_successor_for(state, case, *reference, target)
        {
            Some(record) => record,
            None => current(state, case, *reference, "/read_set")?,
        };
        if inactive(examined) {
            return Err(gate(
                "/read_set",
                "A dependency examined by the reviewer is no longer eligible.",
            ));
        }
    }
    Ok(())
}

fn conclusion_validate(
    payload: ConclusionValidate,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let record = scoped(
        ctx,
        payload.conclusion_ref,
        Kind::Conclusion,
        "/conclusion_ref",
    )?;
    if !matches!(
        record.body["status"].as_str(),
        Some("review_ready" | "validated")
    ) {
        return Err(gate(
            "/conclusion_ref",
            "Request review before validating a conclusion.",
        ));
    }
    let reads = ready(ctx.state(), record)?;
    crate::confidence::require_named_reasons(record, "/conclusion_ref")?;
    enforce_no_critical(ctx.state(), record, &reads)?;
    let record = record.clone();
    warn_open_challenges(ctx, &record, &reads)?;
    let record = &record;
    let review_ref = resolve(ctx, &payload.review_ref, &[Kind::Review], "/review_ref")?;
    let review = scoped(ctx, review_ref, Kind::Review, "/review_ref")?;
    let review_body: ValidationReviewBody = decode(&review.body).map_err(|_| {
        gate(
            "/review_ref",
            "A blindspot audit cannot supply a validation verdict.",
        )
    })?;
    if review_body.verdict != ValidationVerdict::Supports {
        return Err(gate(
            "/review_ref",
            "The validation review must support the scoped conclusion.",
        ));
    }
    validation_review_current(ctx.state(), record, &review_body)?;
    // The reviewed revisions as they stand now: a benign successor the review
    // still covers is what the caller pins.
    let case = record.header.case_id;
    let mut expected: Vec<RevisionRef> = review_body
        .read_set
        .iter()
        .map(|read| {
            crate::freshness::benign_successor_for(ctx.state(), case, *read, record)
                .map_or(*read, Record::reference)
        })
        .collect();
    expected.push(payload.conclusion_ref);
    if review.header.acceptance_sequence != ctx.acceptance_sequence() {
        expected.push(review_ref);
    }
    require_expected(ctx, &expected)?;
    let mut body = record.body.clone();
    let id = record.header.id;
    body["status"] = json!(ConclusionStatus::Validated);
    body["review_ref"] = json!(review_ref);
    body["reviewed_basis"] = json!(review_body.basis_digest);
    let (mut body, policy) = apply_pack_policy(ctx, Gate::ConclusionValidate, record, body)?;
    if let Some(guardrail) =
        crate::guardrails::enforce(ctx, GuardedOperation::ConclusionValidate, record)?
    {
        body["gate_policy"]["guardrail"] = crate::guardrails::encode(&guardrail)?;
    }
    ctx.update(payload.conclusion_ref, body)?;
    let actor = ctx.actor().clone();
    ctx.check_final(move |state| {
        policy.recheck(state, &actor, id)?;
        let final_record = state.current(id).ok_or_else(integrity)?;
        if final_record.body["status"] != "validated"
            || final_record.body["review_ref"] != json!(review_ref)
        {
            return Err(gate(
                "/conclusion_ref",
                "The final transaction changes the newly validated disposition.",
            ));
        }
        current(state, case, review_ref, "/review_ref")?;
        validation_review_current(state, final_record, &review_body)?;
        let reads = ready(state, final_record)?;
        enforce_no_critical(state, final_record, &reads)
    });
    Ok(())
}

fn conclusion_disposition(
    payload: ConclusionDisposition,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let record = scoped(
        ctx,
        payload.conclusion_ref,
        Kind::Conclusion,
        "/conclusion_ref",
    )?;
    if inactive(record) {
        return Err(gate(
            "/conclusion_ref",
            "Only an active conclusion can receive a disposition.",
        ));
    }
    let evidence = refs(ctx, &payload.evidence_refs, EVIDENCE, "/evidence_refs")?;
    for reference in &evidence {
        current(
            ctx.state(),
            record.header.case_id,
            *reference,
            "/evidence_refs",
        )?;
    }
    let successor = payload
        .successor_ref
        .as_ref()
        .map(|entry| resolve(ctx, entry, &[Kind::Conclusion], "/successor_ref"))
        .transpose()?;
    if (payload.status == ConclusionStatus::Superseded) != successor.is_some() {
        return Err(invalid(
            "/successor_ref",
            "Provide a different conclusion only when superseding.",
        ));
    }
    if let Some(successor) = successor
        && (successor.id == payload.conclusion_ref.id
            || inactive(current(
                ctx.state(),
                record.header.case_id,
                successor,
                "/successor_ref",
            )?))
    {
        return Err(gate(
            "/successor_ref",
            "Use a different active successor conclusion.",
        ));
    }
    let mut body = record.body.clone();
    body["status"] = json!(payload.status);
    body["change_reason"] = json!(payload.reason);
    body["disposition_evidence_refs"] = json!(evidence);
    body["successor_ref"] = json!(successor);
    ctx.update(payload.conclusion_ref, body)?;
    Ok(())
}

fn objection_raise(payload: ObjectionRaise, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let subject_ref = resolve(ctx, &payload.subject_ref, SUBJECTS, "/subject_ref")?;
    let case = ctx.case_id().ok_or_else(integrity)?;
    current(ctx.state(), case, subject_ref, "/subject_ref")?;
    let review_ref = payload
        .review_ref
        .as_ref()
        .map(|entry| resolve(ctx, entry, &[Kind::Review], "/review_ref"))
        .transpose()?;
    if let Some(review_ref) = review_ref {
        let review = current(ctx.state(), case, review_ref, "/review_ref")?;
        if review.body["subject_ref"] != json!(subject_ref) {
            return Err(gate(
                "/review_ref",
                "The originating review must address the exact disputed revision.",
            ));
        }
    }
    let evidence_refs = match payload.evidence_refs {
        Knowledge::Unknown => Knowledge::Unknown,
        Knowledge::Known(evidence) => {
            Knowledge::Known(refs(ctx, &evidence, EVIDENCE, "/evidence_refs")?)
        }
    };
    let body = ObjectionBody {
        subject_ref,
        statement: payload.statement,
        scope: payload.scope,
        severity: payload.severity,
        status: ObjectionStatus::Open,
        review_ref,
        evidence_refs,
        resolution_reason: None,
        resolution_evidence_refs: Vec::new(),
        resolution_review_ref: None,
    };
    ctx.create(Kind::Objection, encode(body)?)?;
    Ok(())
}

fn objection_change(
    payload: ObjectionChange,
    status: ObjectionStatus,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let record = scoped(
        ctx,
        payload.objection_ref,
        Kind::Objection,
        "/objection_ref",
    )?;
    let mut body: ObjectionBody = decode(&record.body)?;
    if body.status != ObjectionStatus::Open {
        return Err(gate("/objection_ref", "The objection is no longer open."));
    }
    let evidence = refs(ctx, &payload.evidence_refs, EVIDENCE, "/evidence_refs")?;
    for reference in &evidence {
        current(
            ctx.state(),
            record.header.case_id,
            *reference,
            "/evidence_refs",
        )?;
    }
    if status == ObjectionStatus::Resolved && evidence.is_empty() {
        return Err(gate(
            "/evidence_refs",
            "Cite the evidence that resolves this objection, or withdraw it with a reason.",
        ));
    }
    let review_ref = payload
        .review_ref
        .as_ref()
        .map(|entry| resolve(ctx, entry, &[Kind::Review], "/review_ref"))
        .transpose()?;
    if let Some(review_ref) = review_ref {
        let review = current(
            ctx.state(),
            record.header.case_id,
            review_ref,
            "/review_ref",
        )?;
        let reviewed: RevisionRef = decode(&review.body["subject_ref"])?;
        if reviewed.id != body.subject_ref.id {
            return Err(gate(
                "/review_ref",
                "Cite a review of the disputed subject.",
            ));
        }
    }
    // A new revision records the disposition; the statement, scope, severity
    // and evidence of the original objection are kept unchanged.
    body.status = status;
    body.resolution_reason = Some(payload.reason);
    body.resolution_evidence_refs = evidence;
    body.resolution_review_ref = review_ref;
    ctx.update(payload.objection_ref, encode(body)?)?;
    Ok(())
}

/// Effective status against this exact State. Record lifecycle stays canonical;
/// freshness is a read-time judgment from [`FreshnessIndex`]. Builds an index
/// for the record's case; use [`effective_disposition_with`] to share one.
pub fn effective_disposition(state: &State, record: &Record) -> EffectiveDisposition {
    let index = FreshnessIndex::build(state, record.header.case_id);
    effective_disposition_with(state, &index, record)
}

/// Effective status using an index built for `state` and the record's case.
/// Open critical objections on the record, the subject it reviews or its
/// dependency closure dispute it. Otherwise the index decides whether it needs
/// review, and unsupported material assumptions in its closure make it
/// conditional.
pub fn effective_disposition_with(
    state: &State,
    index: &FreshnessIndex,
    record: &Record,
) -> EffectiveDisposition {
    fn compute(
        state: &State,
        index: &FreshnessIndex,
        record: &Record,
    ) -> Result<EffectiveDisposition, Failure> {
        let kind = record.header.id.kind();
        let case = record.header.case_id;
        let subject = record.header.id;
        let freshness = index.assess(state, record);
        if freshness.freshness == Freshness::Disputed
            || !critical_objections(state, case, subject, &[])?.is_empty()
        {
            return Ok(EffectiveDisposition::Disputed);
        }
        let stale = freshness.needs_review();
        if kind == Kind::Review {
            let reference: RevisionRef = decode(&record.body["subject_ref"])?;
            if !critical_objections(state, case, reference.id, &[])?.is_empty() {
                return Ok(EffectiveDisposition::Disputed);
            }
            return Ok(if stale {
                EffectiveDisposition::NeedsReview
            } else {
                EffectiveDisposition::Current
            });
        }
        if !matches!(
            kind,
            Kind::Conclusion | Kind::Assessment | Kind::Comparison | Kind::Decision
        ) || inactive(record)
        {
            return Ok(EffectiveDisposition::Current);
        }
        // The exact closure cannot be walked when a cited revision changed;
        // the index has already judged whether that change matters.
        let read_set = required_read_set(state, record).ok();
        if let Some(read_set) = &read_set
            && !critical_objections(state, case, subject, read_set)?.is_empty()
        {
            return Ok(EffectiveDisposition::Disputed);
        }
        // A challenge raised after promotion disputes the subject; it is not
        // just a stale review awaiting refresh, so it outranks needs_review.
        if (kind == Kind::Conclusion && record.body["status"] == "validated")
            || (kind == Kind::Decision
                && (record.body["status"] == "accepted" || record.body["lifecycle"] == "accepted"))
        {
            let review: RevisionRef = decode(&record.body["review_ref"])?;
            let review_record = state
                .revision(review)
                .filter(|review| review.header.case_id == case)
                .ok_or_else(integrity)?;
            let review: ValidationReviewBody = decode(&review_record.body)?;
            if raised_after_review(state, record, &review)? {
                return Ok(EffectiveDisposition::Disputed);
            }
        }
        if stale {
            return Ok(EffectiveDisposition::NeedsReview);
        }
        let assumptions: Vec<Id> = match read_set {
            Some(read_set) => read_set
                .iter()
                .filter(|reference| reference.id.kind() == Kind::Assumption)
                .map(|reference| reference.id)
                .collect(),
            None => {
                let scope = MembershipScope::Subject {
                    case_id: case,
                    subject_id: subject,
                };
                membership(state, MembershipRule::MaterialAssumptions, scope)?
                    .members
                    .iter()
                    .map(|member| member.id)
                    .collect()
            }
        };
        if assumptions.iter().any(|id| {
            state.current(*id).is_some_and(|assumption| {
                assumption.body["materiality"] == "material"
                    && assumption.body["status"] != "supported"
            })
        }) {
            return Ok(EffectiveDisposition::Conditional);
        }
        Ok(EffectiveDisposition::Current)
    }
    compute(state, index, record).unwrap_or(EffectiveDisposition::NeedsReview)
}

pub(crate) fn operations() -> Vec<OperationDescriptor> {
    let exact = |kind: Kind| json!({"id":format!("{}_019acd16-0000-7000-8000-000000000001", kind.prefix()), "revision":1});
    let claim = || {
        json!({
            "statement":"Queue depth rose during the observed interval.",
            "scope":"The recorded test interval.",
            "significance":"Check the queue on the next compatible run.",
            "support_refs":[exact(Kind::Observation)],
            "contradiction_refs":[],
            "caveats":["Only one interval was retained."],
        })
    };
    let context = || {
        json!({
            "saw_original_conclusion":{"state":"known","value":true},
            "gathered_new_evidence":{"state":"known","value":false},
            "method_or_source_changes":[],
            "activity":"Independent inspection of the retained trace sample.",
            "dependencies":"shared",
            "dependence_basis":"Both readings use the same retained trace.",
        })
    };

    macro_rules! op {
        ($name:literal, $purpose:literal, $when:literal, $effects:literal, $results:literal, $revision:literal, $repair:literal, [$($related:literal),*], $example:expr, $validate:expr, $handler:expr) => {
            OperationDescriptor::active(OperationInfo {
                name:$name, stage:"review", summary:$purpose, when_to_use:$when,
                lifecycle:"Existing case and any required current subjects.", effects:$effects,
                returns:$results, revision_rule:$revision,
                idempotency:"Reuse the actor and command ID only for an identical request.",
                repair:$repair, related:&[$($related),*],
            }, $example, $validate, $handler)
        };
    }

    vec![
        op!(
            "conclusion.create",
            "Record a bounded candidate claim.",
            "Synthesize a scoped interpretation and preserve rival evidence.",
            "Create an attributed candidate conclusion.",
            "Conclusion ID and revision 1.",
            "Pin exact support, contradictions, and named causal factors.",
            "Choose a concrete scope, significance, and evidence links.",
            ["conclusion.request_review"],
            claim(),
            shape_conclusion,
            conclusion_create
        ),
        op!(
            "conclusion.amend",
            "Revise a bounded claim.",
            "The claim, scope, caveats, or evidence basis has changed.",
            "Append a revised candidate and preserve history.",
            "Updated Conclusion revision.",
            "Supply the current exact conclusion.",
            "Read the current conclusion before changing its basis.",
            ["conclusion.request_review"],
            json!({"conclusion_ref":exact(Kind::Conclusion),"replacement":claim(),"reason":"Narrow the claim to the retained interval."}),
            |payload: &ConclusionAmend| {
                text(&payload.reason, "/reason")?;
                shape_conclusion(&payload.replacement)
            },
            conclusion_amend
        ),
        op!(
            "conclusion.request_review",
            "Move a qualified candidate into review.",
            "The claim has direct verified support, scope, caveats and any required causal toggle.",
            "Append the review-ready lifecycle revision.",
            "Updated Conclusion revision.",
            "Declare the complete exact read-set as preconditions, including the candidate.",
            "Resolve blockers and inspect the current required revisions.",
            ["review.submit"],
            json!({"conclusion_ref":exact(Kind::Conclusion)}),
            |_: &ConclusionReview| Ok(()),
            conclusion_request_review
        ),
        op!(
            "review.submit",
            "Record an attributed validation review or bounded blindspot audit.",
            "A reviewer examined the precise basis of a claim or checked a defined scope.",
            "Record verdict, context, dependence, exact materials and membership sets.",
            "Review ID and revision 1.",
            "Supply the exact subject, basis digest, and complete current read-set for validation.",
            "Read the claim and exact dependencies, then record review context and limitations.",
            ["conclusion.validate", "objection.raise"],
            json!({
                "kind":"validation","subject_ref":exact(Kind::Conclusion),
                "basis_digest": format!("sha256:{}", "1".repeat(64)),
                "read_set":[exact(Kind::Observation)],"method":"Compare measurements against the retained trace.",
                "materials_examined":[exact(Kind::Observation)],"shared_dependencies":[],"objection_refs":[],
                "verdict":"supports","caveats":["Scope covers one retained interval."],
                "context_isolation":"fresh_context_unblinded","context":context()
            }),
            review_shape,
            review_submit
        ),
        op!(
            "conclusion.validate",
            "Validate a scoped reviewed conclusion.",
            "A current supportive review covers the exact evidence and there are no relevant critical objections.",
            "Append a validated lifecycle revision with the accepted review basis.",
            "Updated Conclusion revision.",
            "Supply the exact reviewed dependency closure, conclusion, and prior review as preconditions.",
            "Refresh any stale evidence or reviews and resolve critical objections.",
            ["conclusion.disposition"],
            json!({"conclusion_ref":exact(Kind::Conclusion),"review_ref": exact(Kind::Review)}),
            |_: &ConclusionValidate| Ok(()),
            conclusion_validate
        ),
        op!(
            "conclusion.disposition",
            "Reject, retract, or supersede a scoped conclusion.",
            "Changed evidence or a successor requires a reasoned disposition.",
            "Append the disposition, rationale, and supporting links.",
            "Updated Conclusion revision.",
            "Supply the exact current conclusion and exact evidence.",
            "Provide a rationale and an active successor when superseding.",
            ["conclusion.create"],
            json!({"conclusion_ref":exact(Kind::Conclusion),"status":"retracted","reason":"The relevant bytes were withdrawn.","evidence_refs":[]}),
            |payload: &ConclusionDisposition| {
                text(&payload.reason, "/reason")?;
                if !matches!(
                    payload.status,
                    ConclusionStatus::Rejected
                        | ConclusionStatus::Retracted
                        | ConclusionStatus::Superseded
                ) {
                    return Err(invalid(
                        "/status",
                        "Use rejected, retracted, or superseded.",
                    ));
                }
                Ok(())
            },
            conclusion_disposition
        ),
        op!(
            "objection.raise",
            "Open an attributed challenge to a specific revision.",
            "A material or critical disagreement warrants tracking.",
            "Create a challenge with exact subject and optional originating review.",
            "Objection ID and revision 1.",
            "Supply the current subject and explicit evidence status.",
            "Read the current subject before recording its scope and objection.",
            ["objection.resolve"],
            json!({"subject_ref": exact(Kind::Conclusion),"statement":"The traces may exclude the baseline.","scope":"Completeness of the retained interval.","severity":"critical","evidence_refs":{"state":"unknown"}}),
            |payload: &ObjectionRaise| {
                text(&payload.statement, "/statement")?;
                text(&payload.scope, "/scope")
            },
            objection_raise
        ),
        op!(
            "objection.resolve",
            "Record an evidence-backed resolution to a challenge.",
            "New evidence addresses the stated challenge.",
            "Append a resolution and preserve the original challenge.",
            "Updated Objection revision.",
            "Supply the exact objection and its supporting evidence.",
            "Cite evidence and explain how it addresses the objection.",
            ["review.submit"],
            json!({"objection_ref": exact(Kind::Objection),"reason":"The baseline trace was independently recovered.","evidence_refs":[exact(Kind::Observation)]}),
            |payload: &ObjectionChange| text(&payload.reason, "/reason"),
            |payload, ctx| objection_change(payload, ObjectionStatus::Resolved, ctx)
        ),
        op!(
            "objection.withdraw",
            "Withdraw a challenge with an attributed reason.",
            "The challenge should cease to block review.",
            "Append a withdrawal and preserve the original challenge.",
            "Updated Objection revision.",
            "Supply the exact open objection.",
            "Explain why the original challenge is withdrawn.",
            ["review.submit"],
            json!({"objection_ref": exact(Kind::Objection),"reason":"The objection was based on an unrelated scope.","evidence_refs":[]}),
            |payload: &ObjectionChange| text(&payload.reason, "/reason"),
            |payload, ctx| objection_change(payload, ObjectionStatus::Withdrawn, ctx)
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
        identity::{IdSource, SystemIdSource},
        state::{Acceptance, AcceptedTransaction, validate_and_apply},
        wire::RecordChange,
    };

    struct Scenario {
        state: State,
        case: Option<Id>,
    }

    impl Scenario {
        fn new() -> Self {
            let mut scenario = Self {
                state: State::default(),
                case: None,
            };
            let case = scenario.one(
                "case.create",
                json!({"title":"Bounded trace study", "mode":"record"}),
            );
            scenario.case = Some(case.id);
            scenario
        }

        fn transact(
            &mut self,
            actor: &str,
            ops: Vec<Value>,
            preconditions: &[RevisionRef],
        ) -> Result<AcceptedTransaction, Failure> {
            let sequence = self.state.sequence() + 1;
            let command: Command = serde_json::from_value(json!({
                "schema_version":"inv.command.v0-alpha","case_id":self.case,"actor":actor,
                "command_id":format!("review-{sequence}"),"preconditions":preconditions,"operations":ops,
            })).unwrap();
            let receipt = validate_and_apply(
                &self.state,
                &command,
                &standard_catalog(),
                Acceptance {
                    sequence,
                    clock: &mut SystemClock,
                    ids: &mut SystemIdSource,
                },
            )?;
            self.state.apply(&receipt)?;
            Ok(receipt)
        }

        fn one(&mut self, name: &str, payload: Value) -> RevisionRef {
            self.transact(
                "agent:investigator",
                vec![json!({"op":name,"payload":payload})],
                &[],
            )
            .unwrap()
            .records[0]
                .reference()
        }

        fn image(&mut self, kind: Kind, body: Value, replaces: Option<RevisionRef>) -> RevisionRef {
            let case_id = self.case.unwrap();
            let sequence = self.state.sequence() + 1;
            let mut header = self
                .state
                .current(replaces.map_or(case_id, |reference| reference.id))
                .unwrap()
                .header
                .clone();
            header.acceptance_sequence = sequence;
            if replaces.is_some() {
                header.history.push(header.revision);
                header.revision.0 += 1;
            } else {
                header.id = SystemIdSource.next_id(kind).unwrap();
            }
            let record = Record { header, body };
            let reference = record.reference();
            let change = RecordChange {
                client_ref: None,
                id: reference.id,
                revision: reference.revision,
            };
            let (created, updated) = if replaces.is_some() {
                (vec![], vec![change])
            } else {
                (vec![change], vec![])
            };
            self.state
                .apply(&AcceptedTransaction {
                    case_id,
                    sequence,
                    records: vec![record],
                    created,
                    updated,
                    warnings: vec![],
                })
                .unwrap();
            reference
        }

        fn observation(
            &mut self,
            decimal: &str,
            run: Option<RevisionRef>,
            interventional: bool,
        ) -> RevisionRef {
            let source = self.image(Kind::Source, json!({"access":"local_only"}), None);
            let hash = format!("sha256:{}", "1".repeat(64));
            let artifact = self.image(Kind::Artifact, json!({
                "digest":hash,"size_bytes":64,"format":"text/plain","source":source,
                "transforms":[],"availability":"retained","integrity":"verified","sensitivity":"internal"
            }), None);
            self.image(Kind::Observation, json!({
                "value":{"kind":"measurement","value":decimal,"unit":"failures"},
                "basis":{"kind":"source","artifact_ref":artifact,
                    "selector":{"schema_version":"inv.selector.v1","artifact_digest":hash,"selector":{"type":"whole"},"validation":"verified"},
                    "text_quote":{"state":"not_applicable"}},
                "extraction_method":"Count failures per 100 fixed attempts",
                "validity":"valid","quality":[],"limitations":[],
                "evidence_kind":if interventional {"interventional"} else {"observational"},
                "variables_changed":if interventional {vec!["queue-scheduler"]} else {vec![]},
                "status":"active","run_ref":run,
            }), None)
        }

        fn conclusion(&mut self, observation: Option<RevisionRef>) -> RevisionRef {
            self.one("conclusion.create", claim(observation))
        }

        fn get(&self, reference: RevisionRef) -> &Record {
            self.state.revision(reference).unwrap()
        }

        fn prepare(&mut self, candidate: RevisionRef) -> RevisionRef {
            let mut reads = required_read_set(&self.state, self.get(candidate)).unwrap();
            reads.push(candidate);
            self.transact("agent:investigator", vec![json!({"op":"conclusion.request_review","payload":{"conclusion_ref":candidate}})], &reads).unwrap().records[0].reference()
        }

        fn validation(&mut self, ready: RevisionRef) -> RevisionRef {
            let basis = basis_digest(self.get(ready)).unwrap();
            let reads = required_read_set(&self.state, self.get(ready)).unwrap();
            self.transact("agent:reviewer", vec![json!({"op":"review.submit","payload":{
                "kind":"validation","subject_ref":ready,"basis_digest":basis,"read_set":reads,
                "method":"Inspect measurements and their retained sources.","materials_examined":reads.iter().filter(|reference| reference.id.kind() == Kind::Observation).collect::<Vec<_>>(),
                "shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":["One synthetic trial."],
                "context_isolation":"fresh_context_unblinded","context":review_context(),
            }})], &[]).unwrap().records[0].reference()
        }

        fn promotion(&self, ready: RevisionRef, review: RevisionRef) -> (Value, Vec<RevisionRef>) {
            let mut reads = required_read_set(&self.state, self.get(ready)).unwrap();
            reads.extend([ready, review]);
            (
                json!({"op":"conclusion.validate","payload":{"conclusion_ref":ready,"review_ref":review}}),
                reads,
            )
        }
    }

    fn review_context() -> Value {
        json!({
            "saw_original_conclusion":{"state":"known","value":true},
            "gathered_new_evidence":{"state":"known","value":false},
            "method_or_source_changes":[],"activity":"Witnessed review of staged measurements.",
            "dependencies":"shared","dependence_basis":"Same recorded data.",
        })
    }

    fn claim(observation: Option<RevisionRef>) -> Value {
        json!({
            "statement":"The recorded interval had failed requests.",
            "scope":"A fixed synthetic workload and stable build.",
            "significance":"Determine whether to investigate further.",
            "support_refs":observation.into_iter().collect::<Vec<_>>(),
            "contradiction_refs":[],"caveats":["No evidence from the deployed environment."],
        })
    }

    #[test]
    fn ready_gate_names_missing_support_and_expected_read_set() {
        let mut s = Scenario::new();
        let empty = s.conclusion(None);
        let request =
            |id| json!({"op":"conclusion.request_review","payload":{"conclusion_ref":id}});
        let missing = s
            .transact("agent:investigator", vec![request(empty)], &[])
            .unwrap_err();
        assert_eq!(missing.error.actual, Some(json!(["direct_support"])));

        let observation = s.observation("3", None, false);
        let candidate = s.conclusion(Some(observation));
        let missing = s
            .transact("agent:investigator", vec![request(candidate)], &[candidate])
            .unwrap_err();
        let expected = missing.error.expected.unwrap();
        assert_eq!(expected.as_array().unwrap().len(), 4);
        assert!(expected.as_array().unwrap().contains(&json!(observation)));
    }

    /// A corrected Observation carries `corrects` and `revision_reason`
    /// beside its typed body, and a traced or assessed one `claim_origin` and
    /// `measurement_assessment`: none of them stops it qualifying direct
    /// support, directly or through a supportive assessment (bn-eaz1).
    #[test]
    fn a_corrected_traced_observation_qualifies_as_direct_support() {
        let mut s = Scenario::new();
        let observation = s.observation("3", None, false);
        let mut body = s.get(observation).body.clone();
        body["value"]["value"] = json!("4");
        body["revision_reason"] = json!("Recounted the failures.");
        body["corrects"] = json!(observation);
        body[crate::origin_ops::CLAIM_ORIGIN_KEY] = json!({"traces":[],"dependence":[]});
        let corrected = s.image(Kind::Observation, body, Some(observation));
        let candidate = s.conclusion(Some(corrected));
        let ready = s.prepare(candidate);
        let review = s.validation(ready);
        let (promote, preconditions) = s.promotion(ready, review);
        let accepted = s
            .transact("agent:investigator", vec![promote], &preconditions)
            .unwrap();
        assert_eq!(accepted.records[0].body["status"], "validated");
    }

    #[test]
    fn lifecycle_only_validation_stays_current_and_retraction_invalidates_it() {
        let mut s = Scenario::new();
        let observation = s.observation("3", None, false);
        let candidate = s.conclusion(Some(observation));
        let ready = s.prepare(candidate);
        let review = s.validation(ready);
        let (promote, preconditions) = s.promotion(ready, review);
        let accepted = s
            .transact("agent:investigator", vec![promote], &preconditions)
            .unwrap();
        let confirmed = &accepted.records[0];
        assert_eq!(
            basis_digest(confirmed).unwrap(),
            basis_digest(s.get(ready)).unwrap()
        );
        assert_eq!(
            effective_disposition(&s.state, confirmed),
            EffectiveDisposition::Current
        );
        assert_eq!(
            effective_disposition(&s.state, s.get(review)),
            EffectiveDisposition::Current
        );
        s.one(
            "question.create",
            json!({"importance":"normal","wording":"Unrelated question?"}),
        );
        assert_eq!(
            effective_disposition(&s.state, confirmed),
            EffectiveDisposition::Current
        );
        let mut body = s.get(observation).body.clone();
        body["status"] = json!("retracted");
        s.image(Kind::Observation, body, Some(observation));
        assert_eq!(confirmed.body["status"], "validated");
        assert_eq!(
            effective_disposition(&s.state, confirmed),
            EffectiveDisposition::NeedsReview
        );
    }

    #[test]
    fn validation_checks_open_critical_objections_and_final_atomic_state() {
        let mut s = Scenario::new();
        let observation = s.observation("3", None, false);
        let candidate = s.conclusion(Some(observation));
        let ready = s.prepare(candidate);
        let review = s.validation(ready);
        let (promote, preconditions) = s.promotion(ready, review);
        let raise = |subject_ref| {
            json!({
                "subject_ref":subject_ref,"statement":"Comparable conditions are uncertain.","scope":"Build comparability.",
                "severity":"critical","evidence_refs":{"state":"unknown"},
            })
        };
        let new_revision = RevisionRef {
            revision: ready.revision + 1,
            ..ready
        };
        let assume = json!({"op":"objection.raise","payload":raise(new_revision)});
        let sequence = s.state.sequence();
        assert!(
            s.transact(
                "agent:investigator",
                vec![promote.clone(), assume],
                &preconditions
            )
            .is_err()
        );
        assert_eq!(s.state.sequence(), sequence);
        assert!(s.state.revision(new_revision).is_none());
        let object = s.one("objection.raise", raise(ready));
        assert_eq!(
            effective_disposition(&s.state, s.get(ready)),
            EffectiveDisposition::Disputed
        );
        let err = s
            .transact("agent:investigator", vec![promote], &preconditions)
            .unwrap_err();
        assert!(err.error.message.contains("critical"));
        s.one("objection.withdraw", json!({"objection_ref":object,"reason":"This concern applies to a different trial.","evidence_refs":[]}));
        assert_eq!(s.get(object).body["status"], "open");
    }

    #[test]
    fn old_claim_and_blindspot_cannot_supply_a_validation_review() {
        let mut s = Scenario::new();
        let event = s.observation("4", None, false);
        let candidate = s.conclusion(Some(event));
        let ready = s.prepare(candidate);
        let old_review = s.validation(ready);
        let blind = s.one("review.submit", json!({
            "kind":"blindspot","subject_ref":ready,"audited_snapshot":{"sequence":1,"head_hash":format!("sha256:{}","2".repeat(64))},
            "read_set":[],"method":"Checklist","scope_examined":"Declared workload","checks_considered":["Scope"],
            "gaps":[],"no_gap_reason":"The declared scope is present.","evidence_refs":[],"assumption_refs":[],
            "limitations":["Only checked scope."],"verdict":"no_gap_identified_within_scope","context_isolation":"same_session_self_review",
            "context":review_context(),
        }));
        let (promote, expected) = s.promotion(ready, blind);
        assert!(
            s.transact("agent:investigator", vec![promote], &expected)
                .unwrap_err()
                .error
                .message
                .contains("blindspot")
        );
        let mut replacement = claim(Some(event));
        replacement["statement"] = json!("A changed and narrower claim.");
        let revised = s.one("conclusion.amend", json!({"conclusion_ref":ready,"replacement":replacement,"reason":"Revise after review."}));
        let revised = s.prepare(revised);
        let (promote, expected) = s.promotion(revised, old_review);
        assert!(
            s.transact("agent:investigator", vec![promote], &expected)
                .is_err()
        );
    }

    #[test]
    fn causal_toggles_require_compatible_attempts_and_a_reversal() {
        let mut s = Scenario::new();
        let question = s.one(
            "question.create",
            json!({"importance":"normal","wording":"What causes the failures?"}),
        );
        let hypothesis = s.one("hypothesis.create", json!({
            "question_ref":question,"statement":"A scheduler setting contributes.","possible_falsifiers":[],
            "rival_refs":[],"assumption_refs":[]
        }));
        let mut run = |status: &str, build: &str, intervention: bool| {
            s.one("run.record", json!({
            "intent":{"kind":"exploration","intent":"Observe failure count"},"producer":"agent:test","environment":build,
            "artifact_refs":[],"execution_status":status,"evidence_completeness":"complete",
            "evidence_kind":if intervention {"interventional"} else {"observational"},
            "variables_changed":if intervention {vec!["queue-scheduler"]} else {vec![]},
        }))
        };
        let baseline = run("completed", "sha256:build1 stable workload", false);
        let treatment = run("completed", "sha256:build1 stable workload", true);
        let different = run("completed", "sha256:build2 stable workload", true);
        let invalid = run("failed", "sha256:build1 stable workload", true);
        let reversal = run("completed", "sha256:build1 stable workload", true);
        let baseline = s.observation("4", Some(baseline), false);
        let treatment = s.observation("0", Some(treatment), true);
        let different = s.observation("4", Some(different), true);
        let invalid = s.observation("4", Some(invalid), true);
        let reversal = s.observation("4", Some(reversal), true);
        let mut draft = claim(Some(baseline));
        draft["causal_factors"] = json!([{"hypothesis_ref":hypothesis,"role":"contributor"}]);
        let candidate = s.one("conclusion.create", draft.clone());
        assert!(ready(&s.state, s.get(candidate)).is_err());
        for wrong in [different, invalid] {
            draft["causal_test"] = json!({
                "factor_ref":hypothesis,"variable":"queue-scheduler","baseline":baseline,"treatment":treatment,"reversal":wrong,
                "failure_boundary":{"operator":"greater_than","value":"0","unit":"failures"},
            });
            let candidate = s.one("conclusion.create", draft.clone());
            assert!(ready(&s.state, s.get(candidate)).is_err());
        }
        draft["causal_test"]["reversal"] = json!(reversal);
        let rival = s.one(
            "hypothesis.create",
            json!({
                "question_ref":question,"statement":"Thermal throttling accounts for the failures.",
                "possible_falsifiers":[],"rival_refs":[hypothesis],"assumption_refs":[]
            }),
        );
        let unqualified = s.one("conclusion.create", draft.clone());
        let missing = ready(&s.state, s.get(unqualified)).unwrap_err();
        assert_eq!(missing.error.expected, Some(json!([rival])));
        draft["contradiction_refs"] = json!([rival]);
        let candidate = s.one("conclusion.create", draft);
        let ready = s.prepare(candidate);
        let review = s.validation(ready);
        let (promote, expected) = s.promotion(ready, review);
        let accepted = s
            .transact("agent:investigator", vec![promote], &expected)
            .unwrap();
        assert_eq!(accepted.records[0].body["status"], "validated");
    }

    #[test]
    fn dependency_walk_rejects_circular_justification() {
        let mut s = Scenario::new();
        let root = s.conclusion(None);
        let mut body = s.get(root).body.clone();
        body["support_refs"] = json!([RevisionRef {
            revision: root.revision + 1,
            ..root
        }]);
        let root = s.image(Kind::Conclusion, body, Some(root));
        assert!(
            required_read_set(&s.state, s.get(root))
                .unwrap_err()
                .error
                .message
                .contains("circular")
        );
    }
}
