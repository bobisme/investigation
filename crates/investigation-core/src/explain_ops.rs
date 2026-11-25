//! Canonical operations for explanatory investigation.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::{
    catalog::{OperationDescriptor, OperationInfo},
    command::{Reference, RevisionRef},
    evidence::{
        ArtifactAvailability, ArtifactBody, ArtifactIntegrity, MeasurementValidity,
        ObservationBasis, ObservationBody,
    },
    explain::{
        AssumptionCreate, AssumptionStatus, EvidenceCompleteness, EvidenceKind, ExecutionStatus,
        HypothesisCreate, HypothesisDisposition, HypothesisLifecycle, InquiryCreate,
        InquiryLifecycle, InquiryMode, RunIntent, RunRecord,
    },
    header::Actor,
    identity::{Id, Kind},
    measurement_fields::{MeasurementFields, Repetition, recorded_repetition},
    review::{AssessmentAdd, AssessmentRelation, Expectation, RivalExpectation},
    state::{OperationContext, Record},
    wire::{Digest, ErrorCode, Failure},
};

const EVIDENCE: &[Kind] = &[
    Kind::Observation,
    Kind::Artifact,
    Kind::Source,
    Kind::Comparison,
    Kind::Run,
];
const SUBJECTS: &[Kind] = &[
    Kind::Case,
    Kind::Question,
    Kind::Hypothesis,
    Kind::Assumption,
    Kind::Conclusion,
    Kind::Option,
    Kind::Criterion,
    Kind::Comparison,
    Kind::Decision,
    Kind::Inquiry,
    Kind::Run,
];

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HypothesisAmend {
    hypothesis_ref: RevisionRef,
    replacement: HypothesisCreate,
    rationale: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AssumptionAssess {
    assumption_ref: RevisionRef,
    status: AssumptionStatus,
    rationale: String,
    evidence_refs: Vec<Reference>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InquiryChange {
    inquiry_ref: RevisionRef,
    rationale: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InquiryAmend {
    inquiry_ref: RevisionRef,
    replacement: InquiryCreate,
    rationale: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InquiryFreeze {
    inquiry_ref: RevisionRef,
    question_ref: Option<Reference>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Chronology {
    Retrospective,
    OrderUnknown,
    RecordedBeforeRunDeclaration,
}

#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum PriorExposure {
    UnseenDeclared,
    PartlySeen,
    Seen,
    Unknown,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunStart {
    intent: RunIntent,
    producer: Actor,
    tool: Option<String>,
    environment: Option<String>,
    external_ref: Option<String>,
    evidence_kind: EvidenceKind,
    #[serde(default)]
    variables_changed: Vec<String>,
    /// Optional typed measurement fields known when the Run is declared.
    #[serde(default)]
    measurement: Option<MeasurementFields>,
    /// Identity of this Run as one repeat of a measurement procedure.
    #[serde(default)]
    repetition: Option<Repetition>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OrderingReceipt {
    artifact_ref: Reference,
    format: OrderingReceiptFormat,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum OrderingReceiptFormat {
    InTotoDsse,
    Rfc3161,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunSubmission {
    #[serde(flatten)]
    outcome: RunRecord,
    external_ref: Option<String>,
    evidence_validity: Option<MeasurementValidity>,
    ordering_receipt: Option<OrderingReceipt>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunReconcile {
    run_ref: RevisionRef,
    outcome: RunRecord,
    evidence_validity: Option<MeasurementValidity>,
    external_ref: Option<String>,
    retrospective_plan_ref: Option<RevisionRef>,
    rationale: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AssessmentSupersede {
    assessment_ref: RevisionRef,
    replacement: AssessmentAdd,
    rationale: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AssessmentRetract {
    assessment_ref: RevisionRef,
    rationale: String,
}

fn invalid(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(path)
}

fn blocked(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::GateFailed, message).at(path)
}

fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "A stored explanatory record is invalid.",
    )
}

fn exact_example(kind: Kind) -> Value {
    json!({
        "id": format!("{}_019acd16-0000-7000-8000-000000000001", kind.prefix()),
        "revision": 1,
    })
}

fn text(value: &str, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() {
        return Err(invalid(path, "Supply nonblank text describing this field."));
    }
    Ok(())
}

fn encode(value: impl Serialize) -> Result<Value, Failure> {
    serde_json::to_value(value).map_err(|_| integrity())
}

fn metadata(body: &mut Value, fields: Value) -> Result<(), Failure> {
    let body = body.as_object_mut().ok_or_else(integrity)?;
    let fields = fields.as_object().ok_or_else(integrity)?;
    body.extend(fields.clone());
    Ok(())
}

fn decode<T: DeserializeOwned>(body: &Value) -> Result<T, Failure> {
    crate::input::decode(body).map_err(|_| integrity())
}

fn content<T: DeserializeOwned>(body: &Value, metadata: &[&str]) -> Result<T, Failure> {
    let mut body = body.clone();
    let object = body.as_object_mut().ok_or_else(integrity)?;
    for key in metadata {
        object.remove(*key);
    }
    decode(&body)
}

fn digest(value: &Value) -> Result<Digest, Failure> {
    let bytes = serde_json::to_vec(value).map_err(|_| integrity())?;
    format!("sha256:{:x}", Sha256::digest(bytes))
        .parse()
        .map_err(|_| integrity())
}

fn pin(
    ctx: &OperationContext<'_>,
    reference: &mut Reference,
    kind: Kind,
    path: &str,
) -> Result<RevisionRef, Failure> {
    let revision = ctx.resolve(reference, kind, path)?;
    *reference = Reference::Existing(revision);
    Ok(revision)
}

fn pin_any(
    ctx: &OperationContext<'_>,
    reference: &mut Reference,
    allowed: &[Kind],
    path: &str,
) -> Result<RevisionRef, Failure> {
    if let Reference::Existing(revision) = reference {
        let kind = revision.id.kind();
        if allowed.contains(&kind) {
            return pin(ctx, reference, kind, path);
        }
        return Err(invalid(
            path,
            "The record kind is unsuitable for this reference.",
        ));
    }
    for &kind in allowed {
        if let Ok(revision) = ctx.resolve(reference, kind, path) {
            *reference = Reference::Existing(revision);
            return Ok(revision);
        }
    }
    Err(Failure::new(
        ErrorCode::UnknownReference,
        "Use an exact reference or an earlier alias of a permitted record kind in this case.",
    )
    .at(path))
}

fn pin_all(
    ctx: &OperationContext<'_>,
    references: &mut [Reference],
    kinds: &[Kind],
    path: &str,
) -> Result<(), Failure> {
    let mut seen = BTreeSet::new();
    for (index, reference) in references.iter_mut().enumerate() {
        let path = format!("{path}/{index}");
        let revision = pin_any(ctx, reference, kinds, &path)?;
        if !seen.insert(revision.id) {
            return Err(invalid(&path, "Supply only one revision per record."));
        }
    }
    Ok(())
}

fn pinned(reference: &Reference) -> Result<RevisionRef, Failure> {
    match reference {
        Reference::Existing(revision) => Ok(*revision),
        Reference::Client(_) => Err(integrity()),
    }
}

/// A cited Source or Artifact at its current revision when only its
/// evidentiary use changed since the cited revision; otherwise exact.
fn cited_evidence<'a>(
    ctx: &'a OperationContext<'_>,
    reference: RevisionRef,
    kind: Kind,
    path: &str,
) -> Result<&'a Record, Failure> {
    match ctx.case_id().and_then(|case| {
        crate::source_ops::use_successor(ctx.state(), case, reference)
            .filter(|record| record.header.id.kind() == kind)
    }) {
        Some(record) => Ok(record),
        None => current(ctx, reference, kind, path),
    }
}

fn current<'a>(
    ctx: &'a OperationContext<'_>,
    reference: RevisionRef,
    kind: Kind,
    path: &str,
) -> Result<&'a Record, Failure> {
    if reference.id.kind() != kind {
        return Err(invalid(path, format!("Expected a {kind} record.")));
    }
    let record = ctx.current(reference.id).ok_or_else(|| {
        Failure::new(
            ErrorCode::UnknownReference,
            "The record is unavailable in this case.",
        )
        .at(path)
    })?;
    if record.reference() != reference {
        let mut failure = Failure::new(
            ErrorCode::RevisionConflict,
            "The referenced record changed. Read the current revision and reassess.",
        )
        .at(path);
        failure.error.expected = Some(json!(reference.revision));
        failure.error.actual = Some(json!(record.reference().revision));
        failure.error.retry = Some("reassess".into());
        return Err(failure);
    }
    Ok(record)
}

const HYP_META: &[&str] = &["lifecycle", "rationale", "evidence_refs", "alternative_to"];

fn hypothesis_shape(payload: &HypothesisCreate) -> Result<(), Failure> {
    text(&payload.statement, "/statement")?;
    if let Some(scope) = &payload.applicability {
        text(scope, "/applicability")?;
    }
    for (index, falsifier) in payload.possible_falsifiers.iter().enumerate() {
        text(falsifier, &format!("/possible_falsifiers/{index}"))?;
    }
    Ok(())
}

fn hypothesis_content(
    mut payload: HypothesisCreate,
    ctx: &OperationContext<'_>,
    id: Option<Id>,
) -> Result<Value, Failure> {
    let question = pin(
        ctx,
        &mut payload.question_ref,
        Kind::Question,
        "/question_ref",
    )?;
    forward_rivals(ctx, &payload.rival_refs)?;
    pin_all(
        ctx,
        &mut payload.rival_refs,
        &[Kind::Hypothesis],
        "/rival_refs",
    )?;
    pin_all(
        ctx,
        &mut payload.assumption_refs,
        &[Kind::Assumption],
        "/assumption_refs",
    )?;
    for (index, rival) in payload.rival_refs.iter().enumerate() {
        let path = format!("/rival_refs/{index}");
        let reference = pinned(rival)?;
        let rival = ctx.exact(reference).ok_or_else(integrity)?;
        let rival_content: HypothesisCreate = content(&rival.body, HYP_META)?;
        if Some(reference.id) == id || pinned(&rival_content.question_ref)?.id != question.id {
            return Err(invalid(
                &path,
                "A rival must be a different hypothesis on the same question.",
            ));
        }
    }
    let rivals = encode(&payload.rival_refs)?;
    let mut body = encode(payload)?;
    metadata(&mut body, json!({"alternative_to": rivals}))?;
    Ok(body)
}

/// What a rival alias of a later operation is told (DF.03): rivals recorded
/// in one transaction name only those created before them, and the link
/// counts both ways, so nothing is lost.
const FORWARD_RIVAL: &str = "rival_refs may name only earlier operations; a rivalry \
                                 recorded once counts in both directions. Create the rivals in \
                                 order and let each name only those before it: assessments, \
                                 blindspots and show treat the first as a rival of the later \
                                 ones too.";

/// Refuses a `rival_refs` alias that a later operation of this transaction
/// declares, with the order rule and a read of the schema whose transaction
/// example creates two rivals in order. Any other unresolved reference is
/// left to [`pin_all`].
fn forward_rivals(ctx: &OperationContext<'_>, rivals: &[Reference]) -> Result<(), Failure> {
    for (index, rival) in rivals.iter().enumerate() {
        if let Reference::Client(alias) = rival
            && ctx.alias_declared_later(&alias.client_ref)
        {
            return Err(Failure::new(ErrorCode::UnknownReference, FORWARD_RIVAL)
                .at(format!("/rival_refs/{index}"))
                .with_schema("hypothesis.create")
                .with_argv(
                    [
                        crate::PROGRAM_NAME,
                        "schema",
                        "hypothesis.create",
                        "--format",
                        "json",
                    ]
                    .map(str::to_owned)
                    .to_vec(),
                ));
        }
    }
    Ok(())
}

fn hypothesis_create(
    payload: HypothesisCreate,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let mut body = hypothesis_content(payload, ctx, None)?;
    metadata(
        &mut body,
        json!({"lifecycle": HypothesisLifecycle::Proposed}),
    )?;
    ctx.create(Kind::Hypothesis, body)?;
    Ok(())
}

fn is_live(lifecycle: HypothesisLifecycle) -> bool {
    matches!(
        lifecycle,
        HypothesisLifecycle::Active | HypothesisLifecycle::Proposed
    )
}

fn hypothesis_amend(
    payload: HypothesisAmend,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let record = current(
        ctx,
        payload.hypothesis_ref,
        Kind::Hypothesis,
        "/hypothesis_ref",
    )?;
    let lifecycle = decode(record.body.get("lifecycle").ok_or_else(integrity)?)?;
    if !is_live(lifecycle) {
        return Err(blocked(
            "/hypothesis_ref",
            "Create a new candidate if a revised explanation warrants reconsideration.",
        ));
    }
    let mut body = hypothesis_content(payload.replacement, ctx, Some(payload.hypothesis_ref.id))?;
    metadata(
        &mut body,
        json!({"lifecycle": lifecycle, "rationale": payload.rationale}),
    )?;
    ctx.update(payload.hypothesis_ref, body)?;
    Ok(())
}

fn hypothesis_disposition(
    mut payload: HypothesisDisposition,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let record = current(
        ctx,
        payload.hypothesis_ref,
        Kind::Hypothesis,
        "/hypothesis_ref",
    )?;
    let lifecycle = decode(record.body.get("lifecycle").ok_or_else(integrity)?)?;
    if !is_live(lifecycle)
        || payload.lifecycle == lifecycle
        || payload.lifecycle == HypothesisLifecycle::Proposed
    {
        return Err(blocked(
            "/lifecycle",
            "Choose an attributed transition from a live hypothesis.",
        ));
    }
    pin_all(ctx, &mut payload.evidence_refs, EVIDENCE, "/evidence_refs")?;
    if payload.lifecycle == HypothesisLifecycle::RejectedWithinScope {
        evidence_basis(ctx, &payload.evidence_refs, true)?;
    }
    let mut body = record.body.clone();
    metadata(
        &mut body,
        json!({
            "lifecycle": payload.lifecycle,
            "rationale": payload.rationale,
            "evidence_refs": payload.evidence_refs,
        }),
    )?;
    ctx.update(payload.hypothesis_ref, body)?;
    Ok(())
}

fn assumption_create(
    mut payload: AssumptionCreate,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    pin_all(ctx, &mut payload.affected_refs, SUBJECTS, "/affected_refs")?;
    let mut body = encode(payload)?;
    metadata(&mut body, json!({"status": AssumptionStatus::Unassessed}))?;
    ctx.create(Kind::Assumption, body)?;
    Ok(())
}

fn assumption_assess(
    mut payload: AssumptionAssess,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let record = current(
        ctx,
        payload.assumption_ref,
        Kind::Assumption,
        "/assumption_ref",
    )?;
    let previous: AssumptionStatus = decode(record.body.get("status").ok_or_else(integrity)?)?;
    if previous == AssumptionStatus::Superseded || payload.status == AssumptionStatus::Unassessed {
        return Err(blocked(
            "/status",
            "Assess a current assumption with a reasoned disposition.",
        ));
    }
    pin_all(ctx, &mut payload.evidence_refs, EVIDENCE, "/evidence_refs")?;
    let mut body = record.body.clone();
    metadata(
        &mut body,
        json!({
            "status": payload.status,
            "rationale": payload.rationale,
            "evidence_refs": payload.evidence_refs,
        }),
    )?;
    ctx.update(payload.assumption_ref, body)?;
    Ok(())
}

fn inquiry_shape(payload: &InquiryCreate) -> Result<(), Failure> {
    text(&payload.purpose, "/purpose")?;
    text(&payload.method, "/method")?;
    for (path, items) in [
        ("/desired_observations", &payload.desired_observations),
        ("/limits", &payload.limits),
        ("/controls", &payload.controls),
    ] {
        for (index, item) in items.iter().enumerate() {
            text(item, &format!("{path}/{index}"))?;
        }
    }
    if let Some(plan) = &payload.equivalence {
        crate::absence_gate::plan_shape(plan, "/equivalence")?;
    }
    Ok(())
}

fn inquiry_content(
    mut payload: InquiryCreate,
    ctx: &OperationContext<'_>,
) -> Result<InquiryCreate, Failure> {
    if let InquiryMode::Confirmatory(plan) = &mut payload.inquiry {
        for (index, prediction) in plan.predictions.iter_mut().enumerate() {
            let path = format!("/inquiry/confirmatory/predictions/{index}/rival_ref");
            pin(ctx, &mut prediction.rival_ref, Kind::Hypothesis, &path)?;
        }
    }
    Ok(payload)
}

fn inquiry_create(payload: InquiryCreate, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let mut body = encode(inquiry_content(payload, ctx)?)?;
    metadata(&mut body, json!({"lifecycle": InquiryLifecycle::Draft}))?;
    ctx.create(Kind::Inquiry, body)?;
    Ok(())
}

fn inquiry_lifecycle(record: &Record) -> Result<InquiryLifecycle, Failure> {
    decode(record.body.get("lifecycle").ok_or_else(integrity)?)
}

fn inquiry_amend(payload: InquiryAmend, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let record = current(ctx, payload.inquiry_ref, Kind::Inquiry, "/inquiry_ref")?;
    let lifecycle = inquiry_lifecycle(record)?;
    if lifecycle == InquiryLifecycle::Retired {
        return Err(blocked(
            "/inquiry_ref",
            "Create a fresh inquiry after retirement.",
        ));
    }
    let mut body = encode(inquiry_content(payload.replacement, ctx)?)?;
    metadata(
        &mut body,
        json!({"lifecycle": InquiryLifecycle::Draft, "rationale": payload.rationale}),
    )?;
    if lifecycle == InquiryLifecycle::Frozen {
        metadata(&mut body, json!({"predecessor_ref": payload.inquiry_ref}))?;
        ctx.create(Kind::Inquiry, body)?;
    } else {
        if let Some(predecessor) = record.body.get("predecessor_ref") {
            metadata(&mut body, json!({"predecessor_ref": predecessor}))?;
        }
        ctx.update(payload.inquiry_ref, body)?;
    }
    Ok(())
}

fn inquiry_retire(payload: InquiryChange, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let record = current(ctx, payload.inquiry_ref, Kind::Inquiry, "/inquiry_ref")?;
    if inquiry_lifecycle(record)? == InquiryLifecycle::Retired {
        return Err(blocked("/inquiry_ref", "This inquiry is already retired."));
    }
    let mut body = record.body.clone();
    metadata(
        &mut body,
        json!({"lifecycle": InquiryLifecycle::Retired, "rationale": payload.rationale}),
    )?;
    ctx.update(payload.inquiry_ref, body)?;
    Ok(())
}

fn live_hypotheses(
    ctx: &OperationContext<'_>,
    question: Id,
) -> Result<Vec<(RevisionRef, HypothesisCreate)>, Failure> {
    let mut live = Vec::new();
    for record in ctx
        .records()
        .filter(|record| record.header.id.kind() == Kind::Hypothesis)
    {
        let lifecycle = decode(record.body.get("lifecycle").ok_or_else(integrity)?)?;
        if is_live(lifecycle) {
            let content: HypothesisCreate = content(&record.body, HYP_META)?;
            if pinned(&content.question_ref)?.id == question {
                live.push((record.reference(), content));
            }
        }
    }
    Ok(live)
}

const INQUIRY_META: &[&str] = &[
    "lifecycle",
    "rationale",
    "predecessor_ref",
    "question_ref",
    "plan_digest",
    "prior_exposure",
    "warnings",
];

fn inquiry_freeze(
    mut payload: InquiryFreeze,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let record = current(ctx, payload.inquiry_ref, Kind::Inquiry, "/inquiry_ref")?;
    if inquiry_lifecycle(record)? != InquiryLifecycle::Draft {
        return Err(blocked(
            "/inquiry_ref",
            "Freeze a draft inquiry. Amend a frozen inquiry to create a successor.",
        ));
    }
    let inquiry: InquiryCreate = content(&record.body, INQUIRY_META)?;
    if inquiry.desired_observations.is_empty() {
        return Err(blocked(
            "/desired_observations",
            "Describe the observation channel before freezing.",
        ));
    }
    if inquiry.limits.is_empty() {
        return Err(blocked("/limits", "Supply a bounded stopping rule."));
    }
    let mut body = record.body.clone();
    let question = payload
        .question_ref
        .as_mut()
        .map(|reference| pin(ctx, reference, Kind::Question, "/question_ref"))
        .transpose()?;
    let mut warnings = Vec::new();
    if let InquiryMode::Confirmatory(plan) = &inquiry.inquiry {
        let question = question.ok_or_else(|| {
            blocked(
                "/question_ref",
                "Pin the question for a confirmatory inquiry.",
            )
        })?;
        current(ctx, question, Kind::Question, "/question_ref")?;
        if inquiry.controls.is_empty() {
            return Err(blocked(
                "/controls",
                "Describe the control conditions before freezing.",
            ));
        }
        for (field, value) in [
            ("evidence_holdout_boundary", &plan.evidence_holdout_boundary),
            ("stop_analysis_rule", &plan.stop_analysis_rule),
        ] {
            if value.trim().is_empty() {
                return Err(blocked(
                    &format!("/inquiry/confirmatory/{field}"),
                    "Describe this condition before freezing.",
                ));
            }
        }
        let exposure: PriorExposure =
            serde_json::from_value(json!(plan.prior_exposure)).map_err(|_| {
                invalid(
                    "/inquiry/confirmatory/prior_exposure",
                    "Choose unseen_declared, partly_seen, seen, or unknown.",
                )
            })?;
        let live = live_hypotheses(ctx, question.id)?;
        let mut covered = BTreeSet::new();
        let mut outcomes = BTreeSet::new();
        for (index, prediction) in plan.predictions.iter().enumerate() {
            let path = format!("/inquiry/confirmatory/predictions/{index}");
            let rival = pinned(&prediction.rival_ref)?;
            if !live.iter().any(|(current, _)| *current == rival) || !covered.insert(rival.id) {
                return Err(blocked(
                    &path,
                    "Supply one prediction per current live candidate on this question.",
                ));
            }
            text(
                &prediction.predicted_outcome,
                &format!("{path}/predicted_outcome"),
            )?;
            if !outcomes.insert(prediction.predicted_outcome.trim()) {
                let warning = "inquiry.non_discriminating_plan: Two or more live candidates predict the same outcome.";
                if warnings.is_empty() {
                    warnings.push(warning);
                }
            }
        }
        if covered.is_empty() || covered.len() != live.len() {
            let mut failure = blocked(
                "/inquiry/confirmatory/predictions",
                "Supply one predicted outcome for every live hypothesis on the question.",
            );
            failure.error.expected = Some(json!(
                live.iter().map(|(basis, _)| basis).collect::<Vec<_>>()
            ));
            return Err(failure);
        }
        metadata(&mut body, json!({"prior_exposure": exposure}))?;
    }
    let plan_digest = digest(&json!({"content": inquiry, "question_ref": question}))?;
    metadata(
        &mut body,
        json!({
            "lifecycle": InquiryLifecycle::Frozen,
            "question_ref": question,
            "plan_digest": plan_digest,
            "warnings": warnings,
        }),
    )?;
    ctx.update(payload.inquiry_ref, body)?;
    for warning in warnings {
        ctx.warn(warning.into());
    }
    Ok(())
}

fn run_intent_shape(payload: &RunIntent) -> Result<(), Failure> {
    if let RunIntent::Exploration { intent } = payload {
        text(intent, "/intent/intent")?;
    }
    Ok(())
}

fn run_shape(payload: &RunRecord) -> Result<(), Failure> {
    run_intent_shape(&payload.intent)?;
    if payload.evidence_kind == EvidenceKind::Interventional && payload.variables_changed.is_empty()
    {
        return Err(invalid(
            "/variables_changed",
            "List the variables intentionally changed by the intervention.",
        ));
    }
    if let Some(coverage) = &payload.search_coverage {
        crate::search_coverage::validate(coverage, "/search_coverage")?;
        if matches!((coverage.examined_hits, coverage.total_hits), (Some(examined), Some(total)) if examined > total)
            || matches!((coverage.empty_hits, coverage.examined_hits), (Some(empty), Some(examined)) if empty > examined)
        {
            return Err(invalid(
                "/search_coverage",
                "Report consistent total, examined, and empty hit counts.",
            ));
        }
    }
    if let Some(measurement) = &payload.measurement {
        measurement.validate("/measurement")?;
    }
    if let Some(repetition) = &payload.repetition {
        repetition.validate("/repetition")?;
    }
    Ok(())
}

/// Pin a repetition's shared setup references to exact revisions.
fn pin_repetition(
    ctx: &OperationContext<'_>,
    repetition: Option<&mut Repetition>,
    path: &str,
) -> Result<(), Failure> {
    match repetition {
        Some(repetition) => pin_all(
            ctx,
            &mut repetition.setup_refs,
            &[Kind::Artifact, Kind::Run, Kind::Source],
            &format!("{path}/setup_refs"),
        ),
        None => Ok(()),
    }
}

/// A repeat is a distinct Run: no other Run in the case may record the same
/// acquisition identity.
fn distinct_acquisition(
    ctx: &OperationContext<'_>,
    repetition: Option<&Repetition>,
    this: Option<Id>,
    path: &str,
) -> Result<(), Failure> {
    let Some(repetition) = repetition else {
        return Ok(());
    };
    let prior = ctx.records().find(|record| {
        record.header.id.kind() == Kind::Run
            && Some(record.header.id) != this
            && recorded_repetition(&record.body)
                .is_some_and(|other| other.acquisition == repetition.acquisition)
    });
    if let Some(prior) = prior {
        let mut failure = blocked(
            &format!("{path}/acquisition"),
            "Another Run already records this acquisition. Give each repeat its own acquisition identity, or reconcile the existing Run.",
        );
        failure.error.actual = Some(json!(prior.reference()));
        return Err(failure);
    }
    Ok(())
}

fn chronology(plan_sequence: u64, declaration_sequence: u64) -> Chronology {
    match plan_sequence.cmp(&declaration_sequence) {
        std::cmp::Ordering::Less => Chronology::RecordedBeforeRunDeclaration,
        std::cmp::Ordering::Equal => Chronology::OrderUnknown,
        std::cmp::Ordering::Greater => Chronology::Retrospective,
    }
}

fn plan_order(
    ctx: &OperationContext<'_>,
    plan_ref: RevisionRef,
    declaration_sequence: u64,
) -> Result<Value, Failure> {
    if plan_ref.id.kind() != Kind::Inquiry {
        return Err(invalid(
            "/intent/plan_ref",
            "Use an exact frozen inquiry revision.",
        ));
    }
    let plan = ctx.exact(plan_ref).ok_or_else(|| {
        Failure::new(
            ErrorCode::UnknownReference,
            "The inquiry revision is unavailable in this case.",
        )
        .at("/intent/plan_ref")
    })?;
    if inquiry_lifecycle(plan)? != InquiryLifecycle::Frozen {
        return Err(blocked(
            "/intent/plan_ref",
            "Pin a frozen inquiry revision.",
        ));
    }
    let plan_digest: Digest = decode(plan.body.get("plan_digest").ok_or_else(integrity)?)?;
    Ok(json!({
        "plan_digest": plan_digest,
        "chronology": chronology(plan.header.acceptance_sequence, declaration_sequence),
        "prior_exposure": plan.body.get("prior_exposure"),
    }))
}

fn run_create(
    mut payload: RunRecord,
    external_ref: Option<String>,
    validity: MeasurementValidity,
    order_claim: Option<Value>,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    run_shape(&payload)?;
    if let Some(reference) = &external_ref {
        text(reference, "/external_ref")?;
    }
    pin_all(
        ctx,
        &mut payload.artifact_refs,
        &[Kind::Artifact],
        "/artifact_refs",
    )?;
    pin_repetition(ctx, payload.repetition.as_mut(), "/repetition")?;
    distinct_acquisition(ctx, payload.repetition.as_ref(), None, "/repetition")?;
    let sequence = ctx.acceptance_sequence();
    let order = match payload.intent {
        RunIntent::Plan { plan_ref } => plan_order(ctx, plan_ref, sequence)?,
        RunIntent::Exploration { .. } => json!({"chronology": Chronology::OrderUnknown}),
    };
    let mut body = encode(payload)?;
    metadata(&mut body, order)?;
    metadata(
        &mut body,
        json!({
            "declared_at_sequence": sequence,
            "external_ref": external_ref,
            "evidence_validity": validity,
        "ordering_receipt": order_claim,
        }),
    )?;
    ctx.create(Kind::Run, body)?;
    Ok(())
}

fn run_record(mut payload: RunSubmission, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let order_claim = if let Some(receipt) = &mut payload.ordering_receipt {
        let artifact = pin(
            ctx,
            &mut receipt.artifact_ref,
            Kind::Artifact,
            "/ordering_receipt/artifact_ref",
        )?;
        Some(json!({
            "artifact_ref": artifact,
            "format": receipt.format,
            "verification": "unverified",
        }))
    } else {
        None
    };
    run_create(
        payload.outcome,
        payload.external_ref,
        payload
            .evidence_validity
            .unwrap_or(MeasurementValidity::Unknown),
        order_claim,
        ctx,
    )
}

fn run_start(payload: RunStart, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    run_create(
        RunRecord {
            intent: payload.intent,
            producer: payload.producer,
            tool: payload.tool,
            environment: payload.environment,
            started_at: None,
            finished_at: None,
            result_assertion: None,
            artifact_refs: Vec::new(),
            manifest_digest: None,
            execution_status: ExecutionStatus::Declared,
            evidence_completeness: EvidenceCompleteness::Unknown,
            evidence_kind: payload.evidence_kind,
            variables_changed: payload.variables_changed,
            search_coverage: None,
            measurement: payload.measurement,
            repetition: payload.repetition,
        },
        payload.external_ref,
        MeasurementValidity::Unknown,
        None,
        ctx,
    )
}

const RUN_META: &[&str] = &[
    "declared_at_sequence",
    "chronology",
    "plan_digest",
    "evidence_validity",
    "prior_exposure",
    "external_ref",
    "retrospective_plan_ref",
    "reconciliation_reason",
    "ordering_receipt",
    // Structure checks of the producer's receipts and the verification level
    // of a chronology they upgraded.
    crate::receipt_check::RECEIPT_CHECKS_KEY,
    crate::receipt_check::CHRONOLOGY_VERIFICATION_KEY,
    // Capture binding (handle, producer identity, staging) set by `capture.begin`.
    "capture",
    // Producer statements of a Run ingested from an `inv.producer-run.v1` envelope.
    crate::producer_run::PRODUCER_RUN_KEY,
    // What survived a capture, frozen by `capture.finalize`.
    crate::producer_run::CAPTURE_FINALIZATION_KEY,
];

fn run_reconcile(mut payload: RunReconcile, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let record = current(ctx, payload.run_ref, Kind::Run, "/run_ref")?;
    let old: RunRecord = content(&record.body, RUN_META)?;
    if old.intent != payload.outcome.intent
        || old.producer != payload.outcome.producer
        || old.evidence_kind != payload.outcome.evidence_kind
    {
        return Err(blocked(
            "/outcome",
            "Preserve the run intent, producer, and evidence kind. A separate attempt requires a new run.",
        ));
    }
    pin_all(
        ctx,
        &mut payload.outcome.artifact_refs,
        &[Kind::Artifact],
        "/outcome/artifact_refs",
    )?;
    pin_repetition(
        ctx,
        payload.outcome.repetition.as_mut(),
        "/outcome/repetition",
    )?;
    match (&old.repetition, &payload.outcome.repetition) {
        (Some(old), Some(new))
            if old.series == new.series
                && old.acquisition == new.acquisition
                && old
                    .setup_refs
                    .iter()
                    .all(|reference| new.setup_refs.contains(reference)) => {}
        (Some(_), _) => {
            return Err(blocked(
                "/outcome/repetition",
                "Preserve the repetition series, acquisition and setup references. A separate attempt requires a new run.",
            ));
        }
        (None, new) => distinct_acquisition(
            ctx,
            new.as_ref(),
            Some(payload.run_ref.id),
            "/outcome/repetition",
        )?,
    }
    let open_execution = matches!(
        old.execution_status,
        ExecutionStatus::Declared | ExecutionStatus::Running | ExecutionStatus::Unknown
    );
    if !crate::attempt::keeps_terminal_status(
        old.execution_status,
        payload.outcome.execution_status,
    ) {
        return Err(blocked(
            "/outcome/execution_status",
            "Preserve the original terminal execution status.",
        ));
    }
    // A finalized capture's manifest is immutable: late evidence is linked
    // beside it, never written into it.
    if record
        .body
        .get(crate::producer_run::CAPTURE_FINALIZATION_KEY)
        .is_some_and(|section| !section.is_null())
        && payload.outcome.manifest_digest != old.manifest_digest
    {
        return Err(blocked(
            "/outcome/manifest_digest",
            "Preserve the manifest digest of a finalized capture. Link late evidence as further artifact references instead.",
        ));
    }
    // Linking further artifacts, such as late evidence, leaves a complete
    // outcome as recorded; every other field must stay as it is.
    let linked_only = RunRecord {
        artifact_refs: old.artifact_refs.clone(),
        ..payload.outcome.clone()
    } == old;
    if !open_execution
        && old.evidence_completeness == EvidenceCompleteness::Complete
        && !linked_only
    {
        return Err(blocked(
            "/outcome",
            "The recorded outcome is already complete. Only further artifact references can be linked to it.",
        ));
    }
    if old
        .artifact_refs
        .iter()
        .any(|reference| !payload.outcome.artifact_refs.contains(reference))
    {
        return Err(blocked(
            "/outcome/artifact_refs",
            "Preserve every previously recorded artifact reference.",
        ));
    }
    let declaration = decode(
        record
            .body
            .get("declared_at_sequence")
            .ok_or_else(integrity)?,
    )?;
    let mut body = encode(payload.outcome)?;
    for key in RUN_META {
        if let Some(value) = record.body.get(*key) {
            body[*key] = value.clone();
        }
    }
    if let Some(plan_ref) = payload.retrospective_plan_ref {
        if !matches!(old.intent, RunIntent::Exploration { .. })
            || !record.body["retrospective_plan_ref"].is_null()
        {
            return Err(blocked(
                "/retrospective_plan_ref",
                "Only an exploration run without an attached retrospective plan can be linked.",
            ));
        }
        let order = plan_order(ctx, plan_ref, declaration)?;
        if order["chronology"] != "retrospective" {
            return Err(blocked(
                "/retrospective_plan_ref",
                "The retrospective inquiry must have been frozen after the run declaration.",
            ));
        }
        metadata(&mut body, order)?;
        body["retrospective_plan_ref"] = json!(plan_ref);
    }
    if let Some(external_ref) = payload.external_ref {
        text(&external_ref, "/external_ref")?;
        if !record.body["external_ref"].is_null() && record.body["external_ref"] != external_ref {
            return Err(blocked(
                "/external_ref",
                "Preserve the external identity of the attempt.",
            ));
        }
        body["external_ref"] = json!(external_ref);
    }
    if let Some(validity) = payload.evidence_validity {
        body["evidence_validity"] = json!(validity);
    }
    body["reconciliation_reason"] = json!(payload.rationale);
    ctx.update(payload.run_ref, body)?;
    Ok(())
}

fn assessment_shape(payload: &AssessmentAdd) -> Result<(), Failure> {
    text(&payload.rationale, "/rationale")?;
    if let Some(confidence) = &payload.confidence {
        text(&confidence.rationale, "/confidence/rationale")?;
    }
    for (index, expectation) in payload.rival_expectations.iter().enumerate() {
        text(
            &expectation.rationale,
            &format!("/rival_expectations/{index}/rationale"),
        )?;
    }
    crate::absence_gate::assessment_shape(payload)
}

fn ensure_observation_usable(
    ctx: &OperationContext<'_>,
    reference: RevisionRef,
    diagnostic: bool,
    path: &str,
) -> Result<(), Failure> {
    let observation = current(ctx, reference, Kind::Observation, path)?;
    if observation.body["status"] == "retracted"
        || observation.body["retracted"] == true
        || observation.body["lifecycle"] == "retracted"
    {
        let mut failure = blocked(
            path,
            "The current observation is retracted. Read its revision and choose another evidentiary basis.",
        );
        failure.error.actual = Some(json!(reference.revision));
        return Err(failure);
    }
    if !diagnostic {
        return Ok(());
    }
    // Read the typed body only: corrections, assessments, origin traces and
    // guardrails keep their keys beside it.
    let body = ObservationBody::from_stored(&observation.body).map_err(|_| integrity())?;
    let usable = match &body.basis {
        ObservationBasis::Assertion { .. } => false,
        ObservationBasis::Source {
            artifact_ref,
            selector,
            text_quote,
        } => {
            let artifact = cited_evidence(ctx, *artifact_ref, Kind::Artifact, path)?;
            if let Some(withdrawn) = crate::source_ops::withdrawn_lineage(ctx.state(), artifact) {
                return Err(blocked(
                    path,
                    format!(
                        "Evidence from withdrawn {} has no diagnostic force. Withdrawal is not erasure; use an inconclusive or non_diagnostic relation to record it.",
                        withdrawn.id
                    ),
                ));
            }
            let artifact: ArtifactBody = content(
                &artifact.body,
                &["status", "lifecycle", "rationale", "withdrawal_reason"],
            )?;
            let source = cited_evidence(ctx, artifact.source, Kind::Source, path)?;
            crate::measurement_ops::effective_validity(ctx.state(), observation)
                == MeasurementValidity::Valid
                && selector.validation == crate::evidence::SelectorValidation::Verified
                && matches!(
                    text_quote.state,
                    crate::evidence::TextQuoteState::Verified
                        | crate::evidence::TextQuoteState::NotApplicable
                )
                && artifact.availability == ArtifactAvailability::Retained
                && artifact.integrity == ArtifactIntegrity::Verified
                && artifact.digest.as_ref() == Some(&selector.artifact_digest)
                && source.body["status"] != "withdrawn"
        }
    };
    if !usable {
        return Err(blocked(
            path,
            "Use an inconclusive or non_diagnostic relation for an invalid, unverified, or unattributed measurement.",
        ));
    }
    Ok(())
}

fn evidence_basis(
    ctx: &OperationContext<'_>,
    evidence: &[Reference],
    diagnostic: bool,
) -> Result<(), Failure> {
    let mut valid_observation = false;
    for (index, reference) in evidence.iter().enumerate() {
        let reference = pinned(reference)?;
        let path = format!("/evidence_refs/{index}");
        let kind = reference.id.kind();
        if kind == Kind::Observation {
            ensure_observation_usable(ctx, reference, diagnostic, &path)?;
            valid_observation = true;
        } else if diagnostic {
            let record = current(ctx, reference, kind, &path)?;
            if matches!(kind, Kind::Artifact | Kind::Source)
                && let Some(withdrawn) = crate::source_ops::withdrawn_lineage(ctx.state(), record)
            {
                return Err(blocked(
                    &path,
                    format!(
                        "Withdrawn {} has no diagnostic force. Withdrawal is not erasure; use an inconclusive or non_diagnostic relation to record it.",
                        withdrawn.id
                    ),
                ));
            }
            if kind == Kind::Artifact {
                let artifact: ArtifactBody = content(
                    &record.body,
                    &["status", "lifecycle", "rationale", "withdrawal_reason"],
                )?;
                if artifact.availability != ArtifactAvailability::Retained
                    || artifact.integrity != ArtifactIntegrity::Verified
                {
                    return Err(blocked(
                        &path,
                        "A supporting artifact must remain available and verified.",
                    ));
                }
            }
            if kind == Kind::Run {
                return Err(blocked(
                    &path,
                    "Cite a verified observation of this run. A run status or assertion alone has no diagnostic force.",
                ));
            }
        }
    }
    if diagnostic && !valid_observation {
        return Err(blocked(
            "/evidence_refs",
            "Cite a valid observation tied to verified source bytes.",
        ));
    }
    Ok(())
}

fn live_rivals(
    ctx: &OperationContext<'_>,
    subject: RevisionRef,
) -> Result<BTreeMap<Id, RevisionRef>, Failure> {
    if subject.id.kind() != Kind::Hypothesis {
        return Ok(BTreeMap::new());
    }
    let record = current(ctx, subject, Kind::Hypothesis, "/subject_ref")?;
    let focal: HypothesisCreate = content(&record.body, HYP_META)?;
    let question = pinned(&focal.question_ref)?.id;
    let outgoing: BTreeSet<_> = focal
        .rival_refs
        .iter()
        .map(|reference| pinned(reference).map(|reference| reference.id))
        .collect::<Result<_, _>>()?;
    let mut rivals = BTreeMap::new();
    for (reference, content) in live_hypotheses(ctx, question)? {
        if reference.id != subject.id
            && (outgoing.contains(&reference.id)
                || content
                    .rival_refs
                    .iter()
                    .any(|rival| pinned(rival).is_ok_and(|basis| basis.id == subject.id)))
        {
            rivals.insert(reference.id, reference);
        }
    }
    Ok(rivals)
}

fn pin_expectations(
    ctx: &OperationContext<'_>,
    expectations: &mut [RivalExpectation],
    rivals: &BTreeMap<Id, RevisionRef>,
    diagnostic: bool,
) -> Result<(), Failure> {
    let mut seen = BTreeSet::new();
    for (index, expectation) in expectations.iter_mut().enumerate() {
        let path = format!("/rival_expectations/{index}/rival_ref");
        let reference = pin(ctx, &mut expectation.rival_ref, Kind::Hypothesis, &path)?;
        current(ctx, reference, Kind::Hypothesis, &path)?;
        if rivals.get(&reference.id) != Some(&reference) || !seen.insert(reference.id) {
            return Err(invalid(
                &path,
                "Supply at most one expectation per current live rival of the subject.",
            ));
        }
    }
    if diagnostic && seen.len() != rivals.len() {
        let mut failure = blocked(
            "/rival_expectations",
            "Compare every live rival, using not_assessed with a reason where an expectation is unknown.",
        );
        failure.error.expected = Some(json!(
            rivals
                .values()
                .filter(|basis| !seen.contains(&basis.id))
                .map(|basis| {
                    json!({"rival_ref": basis, "expectation": Expectation::NotAssessed})
                })
                .collect::<Vec<_>>()
        ));
        return Err(failure);
    }
    Ok(())
}

fn assessment_body(
    mut payload: AssessmentAdd,
    ctx: &OperationContext<'_>,
) -> Result<Value, Failure> {
    let subject = pin_any(ctx, &mut payload.subject_ref, SUBJECTS, "/subject_ref")?;
    current(ctx, subject, subject.id.kind(), "/subject_ref")?;
    pin_all(ctx, &mut payload.evidence_refs, EVIDENCE, "/evidence_refs")?;
    pin_all(
        ctx,
        &mut payload.assumption_refs,
        &[Kind::Assumption],
        "/assumption_refs",
    )?;
    for (index, reference) in payload.assumption_refs.iter().enumerate() {
        current(
            ctx,
            pinned(reference)?,
            Kind::Assumption,
            &format!("/assumption_refs/{index}"),
        )?;
    }
    let diagnostic = matches!(
        payload.relation,
        AssessmentRelation::Supports | AssessmentRelation::Contradicts
    );
    let rivals = live_rivals(ctx, subject)?;
    pin_expectations(ctx, &mut payload.rival_expectations, &rivals, diagnostic)?;
    evidence_basis(ctx, &payload.evidence_refs, diagnostic)?;
    if let Some(frozen) = &mut payload.frozen_basis {
        crate::evidence_reuse::pin_frozen_basis(ctx, frozen, "/frozen_basis")?;
    }
    crate::absence_gate::pin_assessment(ctx, &mut payload)?;
    let sorted_refs = |references: &[Reference]| -> Result<Vec<RevisionRef>, Failure> {
        let mut refs = references
            .iter()
            .map(pinned)
            .collect::<Result<Vec<_>, _>>()?;
        refs.sort_unstable_by_key(|reference| (reference.id, reference.revision));
        Ok(refs)
    };
    let mut basis = json!({
        "schema_version": "inv.assessment-basis.v0-alpha",
        "subject_ref": subject,
        "evidence_refs": sorted_refs(&payload.evidence_refs)?,
        "assumption_refs": sorted_refs(&payload.assumption_refs)?,
        "live_rivals": rivals.values().collect::<Vec<_>>(),
    });
    if let Some(frozen) = &payload.frozen_basis {
        basis["frozen_basis"] = encode(frozen)?;
    }
    let basis = digest(&basis)?;
    let mut body = encode(payload)?;
    metadata(
        &mut body,
        json!({"basis_digest": basis, "status": "current"}),
    )?;
    crate::absence_gate::record_assessment(ctx, &mut body)?;
    Ok(body)
}

fn assessment_add(payload: AssessmentAdd, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let body = assessment_body(payload, ctx)?;
    let reference = ctx.create(Kind::Assessment, body)?;
    crate::evidence_reuse::warn_recorded(ctx, reference);
    crate::absence_gate::warn_recorded(ctx, reference);
    Ok(())
}

fn assessment_supersede(
    payload: AssessmentSupersede,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let record = current(
        ctx,
        payload.assessment_ref,
        Kind::Assessment,
        "/assessment_ref",
    )?;
    if record.body["status"] != "current" {
        return Err(blocked(
            "/assessment_ref",
            "Supersede a current assessment.",
        ));
    }
    let mut successor = assessment_body(payload.replacement, ctx)?;
    if record.body["subject_ref"]["id"] != successor["subject_ref"]["id"] {
        return Err(blocked(
            "/replacement/subject_ref",
            "A successor must address the same subject.",
        ));
    }
    successor["supersedes"] = json!(payload.assessment_ref);
    let mut old = record.body.clone();
    let new_ref = ctx.create(Kind::Assessment, successor)?;
    crate::evidence_reuse::warn_recorded(ctx, new_ref);
    crate::absence_gate::warn_recorded(ctx, new_ref);
    metadata(
        &mut old,
        json!({
            "status": "superseded",
            "superseded_by": new_ref,
            "rationale_for_status": payload.rationale,
        }),
    )?;
    ctx.update(payload.assessment_ref, old)?;
    Ok(())
}

fn assessment_retract(
    payload: AssessmentRetract,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let record = current(
        ctx,
        payload.assessment_ref,
        Kind::Assessment,
        "/assessment_ref",
    )?;
    if record.body["status"] != "current" {
        return Err(blocked("/assessment_ref", "Retract a current assessment."));
    }
    let mut body = record.body.clone();
    metadata(
        &mut body,
        json!({"status": "retracted", "rationale_for_status": payload.rationale}),
    )?;
    ctx.update(payload.assessment_ref, body)?;
    Ok(())
}

pub(crate) fn operations() -> Vec<OperationDescriptor> {
    let hypothesis = || {
        json!({
            "question_ref": exact_example(Kind::Question),
            "statement": "A queue builds up during bursts.",
            "applicability": "The sampled test workload",
            "possible_falsifiers": ["Queue depth remains flat during a burst."],
            "rival_refs": [],
            "assumption_refs": [],
        })
    };
    let inquiry = || {
        json!({
            "purpose": "Locate the stage where the delay first appears.",
            "method": "Inspect a bounded sample of execution traces.",
            "controls": [],
            "limits": ["Read the first ten complete traces."],
            "desired_observations": ["Stage timings for each complete trace."],
            "inquiry": {"mode": "exploratory"},
        })
    };
    let intent = || json!({"kind": "exploration", "intent": "Inspect recorded stage timings."});
    let outcome = || {
        json!({
            "intent": intent(),
            "producer": "agent:example",
            "artifact_refs": [],
            "execution_status": "completed",
            "evidence_completeness": "missing",
            "evidence_kind": "observational",
            "variables_changed": [],
        })
    };
    let benchmark = || {
        let mut run = outcome();
        run["measurement"] = json!({
            "unit": {"state": "known", "value": "ms"},
            "workload": {"state": "known", "value": "Synthetic request mix A."},
            "observation_window": {"state": "known", "value": {"value": "300", "unit": "s"}},
            "warmup": {"state": "known", "value": {"value": "30", "unit": "s"}},
            "sample_count": {"state": "known", "value": 2},
            "samples": {"shape": "leveled", "unit": "ms", "samples": [
                {"value": "10.4", "order": {"state": "known", "value": 0},
                 "build": {"state": "known", "value": "build-41"},
                 "process": {"state": "known", "value": "1"},
                 "iteration": {"state": "known", "value": 0}},
                {"value": "10.9", "order": {"state": "known", "value": 1},
                 "build": {"state": "known", "value": "build-41"},
                 "process": {"state": "known", "value": "1"},
                 "iteration": {"state": "known", "value": 1}}
            ]},
            "trial_order": {"state": "known", "value": "interleaved"},
            "load_model": {"state": "known", "value": {
                "loop": {"state": "known", "value": "open"},
                "timed_from_intended_send": {"state": "known", "value": true}
            }},
            "attempts": {
                "started": {"count": {"state": "known", "value": 100}, "definition": {"state": "known", "value": "Requests sent."}},
                "observed": {"count": {"state": "known", "value": 95}, "definition": {"state": "known", "value": "Requests answered."}},
                "timed_out": {"count": {"state": "known", "value": 5}, "definition": {"state": "known", "value": "Requests unanswered after 1 s."}},
                "excluded": {"count": {"state": "known", "value": 0}, "definition": {"state": "known", "value": "Requests removed by the stated exclusion policy."}}
            },
            "environment_identity": {"state": "known", "value": {
                "build": {"state": "known", "value": "build-41"},
                "host": {"state": "unknown"}
            }},
            "tool_identity": {"state": "known", "value": {"version": {"state": "known", "value": "2.1"}}},
            "instrumentation_changed": {"state": "known", "value": false},
            "limitations": ["Synthetic example; one host."]
        });
        run
    };
    let registry_search = || {
        let mut run = outcome();
        run["intent"] = json!({"kind": "exploration", "intent": "Search a registry for errata."});
        run["result_assertion"] = json!("No matching erratum was found in the searched registry.");
        run["evidence_completeness"] = json!("partial");
        run["search_coverage"] = json!({
            "corpus": "Example errata registry",
            "revision_bounds": "Snapshot taken before the search.",
            "queries": ["part 7 erratum"],
            "filters": ["edition 3"],
            "inclusion_rules": ["Published errata only."],
            "exclusion_rules": ["Forum posts."],
            "total_hits": 0,
            "examined_hits": 0,
            "empty_hits": 0,
            "collections": ["errata"],
            "date_range": "2020 to 2026",
            "query_results": [{"query": "part 7 erratum", "hits": 0}],
            "not_searched": ["Unpublished drafts."]
        });
        run
    };
    let assessment = || {
        json!({
            "subject_ref": exact_example(Kind::Hypothesis),
            "evidence_refs": [],
            "relation": "inconclusive",
            "rationale": "The retained trace provides insufficient timing detail.",
        })
    };

    macro_rules! op {
        ($name:literal, $stage:literal, $summary:literal, $use:literal, $lifecycle:literal,
         $effects:literal, $returns:literal, $revisions:literal, $repair:literal,
         [$($related:literal),*], $example:expr, $validate:expr, $handler:expr) => {
            OperationDescriptor::active(
                OperationInfo {
                    name: $name,
                    stage: $stage,
                    summary: $summary,
                    when_to_use: $use,
                    lifecycle: $lifecycle,
                    effects: $effects,
                    returns: $returns,
                    revision_rule: $revisions,
                    idempotency: "Reuse the same actor, command ID, and semantic request only for an identical retry.",
                    repair: $repair,
                    related: &[$($related),*],
                },
                $example,
                $validate,
                $handler,
            )
        };
    }

    vec![
        op!(
            "hypothesis.create",
            "explain",
            "Record a scoped candidate explanation.",
            "A question has a plausible factual explanation worth testing.",
            "Existing case and question.",
            "Create an attributed proposed hypothesis.",
            "Hypothesis ID and revision 1.",
            "Pin the question and any declared alternatives and assumptions.",
            "Use the exact question and name rivals from that question; in one transaction, name only rivals created by earlier operations (a rivalry counts both ways).",
            ["inquiry.create", "assessment.add"],
            hypothesis(),
            hypothesis_shape,
            hypothesis_create
        ),
        op!(
            "hypothesis.amend",
            "explain",
            "Revise a live candidate explanation.",
            "New information changes the statement, scope, or falsifiers.",
            "Proposed or active hypothesis.",
            "Append an attributed revision.",
            "Updated Hypothesis revision.",
            "Supply the current exact hypothesis revision.",
            "Read the current hypothesis and explain the amendment.",
            ["assessment.add"],
            json!({"hypothesis_ref": exact_example(Kind::Hypothesis), "replacement": hypothesis(), "rationale": "Narrow to scheduled bursts."}),
            |payload: &HypothesisAmend| {
                text(&payload.rationale, "/rationale")?;
                hypothesis_shape(&payload.replacement)
            },
            hypothesis_amend
        ),
        op!(
            "hypothesis.disposition",
            "explain",
            "Record an attributed lifecycle decision.",
            "A live candidate merits activation, retirement, or rejection within scope.",
            "Proposed or active hypothesis.",
            "Append lifecycle rationale and the cited evidence.",
            "Updated Hypothesis revision.",
            "Supply the current exact hypothesis revision.",
            "Explain the disposition and cite evidence for a scoped rejection.",
            ["assessment.add"],
            json!({"hypothesis_ref": exact_example(Kind::Hypothesis), "lifecycle": "retired", "rationale": "The sampled workload excludes this mechanism.", "evidence_refs": []}),
            |payload: &HypothesisDisposition| text(&payload.rationale, "/rationale"),
            hypothesis_disposition
        ),
        op!(
            "assumption.create",
            "frame",
            "Record a premise and the subjects relying on it.",
            "A material inference rests on something still uncertain.",
            "Existing case.",
            "Create an unassessed assumption with exact affected subjects.",
            "Assumption ID and revision 1.",
            "Supply exact same-case references to the affected subjects.",
            "State the premise, consequence, and materiality.",
            ["assumption.assess"],
            json!({"claim": "The instrumentation preserves event order.", "consequence": "Stage timing comparisons depend on ordering.", "materiality": "material", "affected_refs": [exact_example(Kind::Hypothesis)]}),
            |payload: &AssumptionCreate| {
                text(&payload.claim, "/claim")?;
                text(&payload.consequence, "/consequence")
            },
            assumption_create
        ),
        op!(
            "assumption.assess",
            "assess",
            "Assess a recorded premise.",
            "Evidence or a bounded reason changes how an assumption should be treated.",
            "Current assumption.",
            "Append the attributed disposition and its evidentiary rationale.",
            "Updated Assumption revision.",
            "Supply the current exact assumption revision.",
            "Read the current premise and provide a reason.",
            ["assessment.add"],
            json!({"assumption_ref": exact_example(Kind::Assumption), "status": "challenged", "rationale": "The available clock information leaves ordering unresolved.", "evidence_refs": []}),
            |payload: &AssumptionAssess| text(&payload.rationale, "/rationale"),
            assumption_assess
        ),
        op!(
            "inquiry.create",
            "collect",
            "Draft an exploratory or confirmatory inquiry.",
            "Plan which observation or bounded inspection could reduce uncertainty.",
            "Existing case.",
            "Create an inquiry draft.",
            "Inquiry ID and revision 1.",
            "Pin any named candidate predictions to exact hypotheses.",
            "Supply a purpose, method, and mode.",
            ["inquiry.freeze"],
            inquiry(),
            inquiry_shape,
            inquiry_create
        ),
        op!(
            "inquiry.amend",
            "collect",
            "Amend a draft or create a successor to a frozen inquiry.",
            "The investigation requires different observations or analysis conditions.",
            "Draft or frozen inquiry.",
            "Append a draft revision or create a new draft linked to the immutable predecessor.",
            "Updated or created Inquiry revision.",
            "Supply the current exact inquiry revision.",
            "Read the current inquiry and give the reason for the change.",
            ["inquiry.freeze"],
            json!({"inquiry_ref": exact_example(Kind::Inquiry), "replacement": inquiry(), "rationale": "Inspect a bounded sample."}),
            |payload: &InquiryAmend| {
                text(&payload.rationale, "/rationale")?;
                inquiry_shape(&payload.replacement)
            },
            inquiry_amend
        ),
        op!(
            "inquiry.freeze",
            "collect",
            "Pin the current inquiry as a versioned plan.",
            "A bounded plan is ready to record before declaring a run.",
            "Draft inquiry.",
            "Freeze the plan, issue a digest, and report duplicate rival predictions.",
            "Frozen Inquiry revision and plan digest.",
            "Use the current draft, and pin the current question and its live candidates for confirmation.",
            "Supply the observation channel, stop rule, and confirmatory controls, holdout, prior exposure, and predictions.",
            ["run.start"],
            json!({"inquiry_ref": exact_example(Kind::Inquiry)}),
            |_: &InquiryFreeze| Ok(()),
            inquiry_freeze
        ),
        op!(
            "inquiry.retire",
            "collect",
            "Retire an inquiry while preserving its revisions.",
            "The plan has served its purpose or no longer fits the question.",
            "Draft or frozen inquiry.",
            "Append a retirement revision with a reason.",
            "Updated Inquiry revision.",
            "Supply the current exact inquiry revision.",
            "Read the current inquiry and give the reason for retirement.",
            ["inquiry.create"],
            json!({"inquiry_ref": exact_example(Kind::Inquiry), "rationale": "The planned trace stream has been decommissioned."}),
            |payload: &InquiryChange| text(&payload.rationale, "/rationale"),
            inquiry_retire
        ),
        op!(
            "run.start",
            "collect",
            "Declare an external attempt.",
            "Record one exploration intent or a pinned inquiry before an external action.",
            "Existing case.",
            "Create a declaration with independently unknown evidence completeness and measurement validity.",
            "Run ID and revision 1.",
            "Pin a frozen inquiry revision for a planned attempt.",
            "Use an exact frozen plan revision or give an explicit exploration intent.",
            ["run.reconcile"],
            json!({"intent": intent(), "producer": "agent:example", "evidence_kind": "observational"}),
            |payload: &RunStart| run_intent_shape(&payload.intent),
            run_start
        ),
        op!(
            "run.record",
            "collect",
            "Record an externally observed attempt.",
            "An attempt already happened and has an attributed status or result.",
            "Existing case.",
            "Create a run with separate execution, completeness and validity.",
            "Run ID and revision 1.",
            "Pin every retained artifact and any frozen plan revision.",
            "Give execution status and evidence completeness as independent claims.",
            ["run.reconcile", "observation.add", "measurement.assess"],
            benchmark(),
            |payload: &RunSubmission| run_shape(&payload.outcome),
            run_record
        ),
        op!(
            "run.reconcile",
            "collect",
            "Append an attributable update to an existing attempt.",
            "The execution result or evidence was previously incomplete, or a retrospective plan was added.",
            "Current run revision.",
            "Append a reconciliation and preserve previous outcome revisions.",
            "Updated Run revision.",
            "Supply the current run and preserve its original intent and producer.",
            "Reconcile the original attempt only. A repeated attempt needs a new run.",
            ["observation.add"],
            json!({"run_ref": exact_example(Kind::Run), "outcome": registry_search(), "rationale": "The producer returned a terminal status."}),
            |payload: &RunReconcile| {
                text(&payload.rationale, "/rationale")?;
                run_shape(&payload.outcome)
            },
            run_reconcile
        ),
        op!(
            "assessment.add",
            "assess",
            "Interpret evidence against a precise subject.",
            "Record an attributable interpretation and why it discriminates or remains inconclusive.",
            "Existing case and scoped subject.",
            "Create an assessment with an exact basis digest.",
            "Assessment ID and revision 1.",
            "Use current subject, assumption and diagnostic evidence revisions.",
            "Use non_diagnostic or inconclusive for invalid evidence. Include explicit expectations for live rivals.",
            ["assessment.supersede", "assessment.retract"],
            assessment(),
            assessment_shape,
            assessment_add
        ),
        op!(
            "assessment.supersede",
            "assess",
            "Record a successor assessment with a new basis.",
            "A revised interpretation should replace an earlier assessment.",
            "Current assessment.",
            "Create a successor and append the supersession disposition to the earlier record.",
            "New Assessment ID and prior Assessment revision.",
            "Use the current exact assessment and the same subject ID.",
            "Read the existing and current case evidence before recording a revised interpretation.",
            ["assessment.retract"],
            json!({"assessment_ref": exact_example(Kind::Assessment), "replacement": assessment(), "rationale": "Record the limitation identified in review."}),
            |payload: &AssessmentSupersede| {
                text(&payload.rationale, "/rationale")?;
                assessment_shape(&payload.replacement)
            },
            assessment_supersede
        ),
        op!(
            "assessment.retract",
            "assess",
            "Retract an assessment without erasing its history.",
            "The interpretation should no longer be used.",
            "Current assessment.",
            "Append the attributed retraction.",
            "Updated Assessment revision.",
            "Supply the current exact assessment revision.",
            "Give a concrete reason the interpretation should no longer stand.",
            ["assessment.add"],
            json!({"assessment_ref": exact_example(Kind::Assessment), "rationale": "The extraction was shown to mix incompatible traces."}),
            |payload: &AssessmentRetract| text(&payload.rationale, "/rationale"),
            assessment_retract
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
        state::{Acceptance, AcceptedTransaction, State, validate_and_apply},
    };

    struct Case {
        state: State,
        case: Option<Id>,
    }

    impl Case {
        fn new() -> Self {
            let mut test = Self {
                state: State::default(),
                case: None,
            };
            let accepted = test
                .submit(
                    "case.create",
                    json!({"title": "Trace study", "mode": "record"}),
                )
                .unwrap();
            test.case = Some(accepted.case_id);
            test
        }

        fn submit(&mut self, op: &str, payload: Value) -> Result<AcceptedTransaction, Failure> {
            let sequence = self.state.sequence() + 1;
            let command: Command = serde_json::from_value(json!({
                "schema_version": "inv.command.v0-alpha",
                "command_id": format!("command-{sequence}"),
                "case_id": self.case,
                "actor": "agent:test",
                "preconditions": [],
                "operations": [{"op": op, "payload": payload}],
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
            Ok(accepted)
        }

        fn create(&mut self, op: &str, payload: Value) -> RevisionRef {
            self.submit(op, payload)
                .unwrap()
                .records
                .last()
                .unwrap()
                .reference()
        }

        fn rival_pair(&mut self) -> (RevisionRef, RevisionRef, RevisionRef) {
            let question = self.create(
                "question.create",
                json!({"wording": "Why is the stage slow?", "importance": "normal"}),
            );
            let first = self.create(
                "hypothesis.create",
                json!({
                    "question_ref": question, "statement": "The local queue grows.",
                    "possible_falsifiers": [], "rival_refs": [], "assumption_refs": [],
                }),
            );
            let second = self.create(
                "hypothesis.create",
                json!({
                    "question_ref": question, "statement": "The downstream queue grows.",
                    "possible_falsifiers": [], "rival_refs": [first], "assumption_refs": [],
                }),
            );
            (question, first, second)
        }

        fn fixture_image(
            &mut self,
            kind: Kind,
            body: Value,
            replaces: Option<RevisionRef>,
        ) -> RevisionRef {
            use crate::{identity::IdSource, wire::RecordChange};

            let case = self.case.unwrap();
            let sequence = self.state.sequence() + 1;
            let mut header = self
                .state
                .current(replaces.map_or(case, |reference| reference.id))
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
                    case_id: case,
                    sequence,
                    records: vec![record],
                    created,
                    updated,
                    warnings: vec![],
                })
                .unwrap();
            reference
        }
    }

    fn example(op: &str) -> Value {
        standard_catalog()
            .get(op)
            .unwrap()
            .example()
            .unwrap()
            .clone()
    }

    #[test]
    fn catalog_examples_decode_and_run_payload_rejects_unknown_fields() {
        for operation in operations() {
            operation.validate_example().unwrap_or_else(|error| {
                panic!("{}: {error}", operation.info().name);
            });
            let schema = encode(operation.payload_schema().unwrap()).unwrap();
            assert!(
                jsonschema::is_valid(&schema, operation.example().unwrap()),
                "{} needs a schema-valid example",
                operation.info().name,
            );
        }
        let mut run = example("run.record");
        run["extraneous"] = json!(true);
        let failure = standard_catalog()
            .get("run.record")
            .unwrap()
            .validate_payload(&run)
            .unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn explanatory_candidates_coexist_and_disposition_requires_a_reason() {
        let mut case = Case::new();
        let (_, first, second) = case.rival_pair();
        let assumption = case.create(
            "assumption.create",
            json!({
                "claim": "The queues use comparable clocks.",
                "consequence": "Timing across stages depends on comparable clocks.",
                "materiality": "material",
                "affected_refs": [first, second],
            }),
        );
        assert_eq!(
            case.state.current(assumption.id).unwrap().body["affected_refs"],
            json!([first, second])
        );
        assert_eq!(
            case.state.current(second.id).unwrap().body["alternative_to"],
            json!([first])
        );
        let sequence = case.state.sequence();
        let failure = case.submit("hypothesis.disposition", json!({
            "hypothesis_ref": first, "lifecycle": "retired", "rationale": " ", "evidence_refs": [],
        })).unwrap_err();
        assert_eq!(
            failure.error.path.as_deref(),
            Some("/operations/0/payload/rationale")
        );
        assert_eq!(case.state.sequence(), sequence);
    }

    #[test]
    fn confirmation_freeze_pins_rivals_and_preserves_the_frozen_plan() {
        let mut case = Case::new();
        let (question, first, second) = case.rival_pair();
        let mut plan = example("inquiry.create");
        plan["controls"] = json!(["Repeat against the same synthetic load."]);
        plan["inquiry"] = json!({"mode": "confirmatory", "confirmatory": {
            "predictions": [],
            "evidence_holdout_boundary": "Traces 11 through 20 are held out.",
            "prior_exposure": "seen",
            "stop_analysis_rule": "Compare after collecting ten traces.",
        }});
        let draft = case.create("inquiry.create", plan.clone());
        let failure = case
            .submit(
                "inquiry.freeze",
                json!({
                    "inquiry_ref": draft, "question_ref": question,
                }),
            )
            .unwrap_err();
        assert_eq!(
            failure.error.path.as_deref(),
            Some("/operations/0/payload/inquiry/confirmatory/predictions")
        );
        assert!(
            case.state
                .revision(RevisionRef {
                    revision: 2,
                    ..draft
                })
                .is_none()
        );

        plan["inquiry"]["confirmatory"]["predictions"] = json!([
            {"rival_ref":first,"predicted_outcome":"Timing rises."},
            {"rival_ref":second,"predicted_outcome":"Timing rises."},
        ]);
        let draft = case.submit("inquiry.amend", json!({
            "inquiry_ref": draft, "replacement": plan, "rationale": "Add both candidate predictions.",
        })).unwrap().records[0].reference();
        let frozen = case
            .submit(
                "inquiry.freeze",
                json!({
                    "inquiry_ref": draft, "question_ref": question,
                }),
            )
            .unwrap();
        assert_eq!(frozen.warnings.len(), 1);
        assert!(frozen.warnings[0].contains("inquiry.non_discriminating_plan"));
        let frozen = &frozen.records[0];
        assert_eq!(frozen.body["question_ref"], json!(question));
        let frozen_ref = frozen.reference();
        let successor = case.create("inquiry.amend", json!({
            "inquiry_ref": frozen_ref, "replacement": example("inquiry.create"), "rationale": "Remediate identical predictions.",
        }));
        assert_ne!(successor.id, frozen_ref.id);
        assert_eq!(case.state.current(frozen_ref.id).unwrap(), frozen);
        assert_eq!(
            case.state.current(successor.id).unwrap().body["predecessor_ref"],
            json!(frozen_ref)
        );

        let declaration = case.create(
            "run.start",
            json!({
                "intent": {"kind":"plan", "plan_ref": frozen_ref},
                "producer":"agent:test","evidence_kind":"observational",
            }),
        );
        let run = &case.state.current(declaration.id).unwrap().body;
        assert_eq!(run["plan_digest"], frozen.body["plan_digest"]);
        assert_eq!(run["chronology"], "recorded_before_run_declaration");
        assert_eq!(run["prior_exposure"], "seen");
    }

    #[test]
    fn outcomes_remain_independent_through_retrospective_reconciliation() {
        let mut case = Case::new();
        let mut failed = example("run.record");
        failed["execution_status"] = json!("failed");
        failed["evidence_completeness"] = json!("complete");
        let failure = case.create("run.record", failed.clone());
        let completed = case.create("run.record", example("run.record"));
        assert_eq!(
            case.state.current(completed.id).unwrap().body["execution_status"],
            "completed"
        );
        assert_eq!(
            case.state.current(completed.id).unwrap().body["evidence_completeness"],
            "missing"
        );
        let draft = case.create("inquiry.create", example("inquiry.create"));
        let frozen = case
            .submit("inquiry.freeze", json!({"inquiry_ref": draft}))
            .unwrap()
            .records[0]
            .reference();
        let original = case.state.current(failure.id).unwrap().clone();
        let updated = case
            .submit(
                "run.reconcile",
                json!({
                    "run_ref": failure, "outcome": failed, "retrospective_plan_ref": frozen,
                    "rationale": "The follow-up plan was written after this attempt.",
                }),
            )
            .unwrap();
        assert_eq!(updated.records[0].body["chronology"], "retrospective");
        assert_eq!(updated.records[0].body["execution_status"], "failed");
        assert_eq!(updated.records[0].body["evidence_completeness"], "complete");
        assert_eq!(case.state.revision(failure).unwrap(), &original);
    }

    #[test]
    fn diagnostic_assessment_requires_rivals_and_verified_evidence() {
        let mut case = Case::new();
        let (_, subject, rival) = case.rival_pair();
        let run = case.create("run.record", example("run.record"));
        let mut payload = json!({
            "subject_ref": subject, "evidence_refs": [run], "relation": "supports",
            "rationale": "Attempted to interpret the status alone.",
        });
        let failure = case.submit("assessment.add", payload.clone()).unwrap_err();
        assert_eq!(
            failure.error.path.as_deref(),
            Some("/operations/0/payload/rival_expectations")
        );
        assert_eq!(
            failure.error.expected.unwrap()[0]["rival_ref"],
            json!(rival)
        );
        payload["rival_expectations"] = json!([{"rival_ref": rival, "expectation":"not_assessed", "rationale":"No measurement on this run."}]);
        let failure = case.submit("assessment.add", payload.clone()).unwrap_err();
        assert!(failure.error.message.contains("no diagnostic force"));
        payload["relation"] = json!("non_diagnostic");
        let assessment = case.create("assessment.add", payload);
        assert_eq!(
            case.state.current(assessment.id).unwrap().body["status"],
            "current"
        );
        assert!(
            decode::<Digest>(&case.state.current(assessment.id).unwrap().body["basis_digest"])
                .is_ok()
        );
    }

    #[test]
    fn distinct_judgments_can_cite_the_same_verified_basis_and_retraction_blocks_reuse() {
        let mut case = Case::new();
        let (_, subject, rival) = case.rival_pair();
        let source = case.fixture_image(Kind::Source, json!({"access":"local_only"}), None);
        let sha256 = digest(&json!("Synthetic fixture source")).unwrap();
        let artifact = case.fixture_image(Kind::Artifact, json!({
            "digest": sha256, "size_bytes": 24, "format": "text/plain", "source": source,
            "transforms": [], "availability":"retained", "integrity":"verified", "sensitivity":"internal"
        }), None);
        let mut image = json!({
            "status":"active",
            "value":{"kind":"text", "statement":"Queue depth grew during bursts."},
            "basis":{"kind":"source", "artifact_ref": artifact,
                "selector":{"schema_version":"inv.selector.v1", "artifact_digest":sha256, "selector":{"type":"whole"}, "validation":"verified"},
                "text_quote":{"state":"not_applicable"}
            },
            "validity":"valid", "quality":[], "limitations":[], "variables_changed":[]
        });
        let observation = case.fixture_image(Kind::Observation, image.clone(), None);
        let mut assessment = json!({
            "subject_ref": subject, "evidence_refs":[observation], "relation":"supports",
            "rival_expectations":[{"rival_ref":rival, "expectation":"not_assessed", "rationale":"The downstream queue was not measured."}],
            "rationale":"Local rise is consistent with the candidate."
        });
        let supporting = case.create("assessment.add", assessment.clone());
        assessment["relation"] = json!("contradicts");
        assessment["rationale"] =
            json!("The timing of the rise contradicts the proposed sequence.");
        let contradicting = case.create("assessment.add", assessment.clone());
        let prior = &case.state.current(supporting.id).unwrap().body;
        let alternate = &case.state.current(contradicting.id).unwrap().body;
        assert_eq!(prior["basis_digest"], alternate["basis_digest"]);
        assert_eq!(prior["relation"], "supports");
        assert_eq!(alternate["relation"], "contradicts");
        image["status"] = json!("retracted");
        let latest = case.fixture_image(Kind::Observation, image, Some(observation));
        let failure = case.submit("assessment.add", assessment).unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::RevisionConflict);
        assert_eq!(failure.error.actual, Some(json!(latest.revision)));
    }
}
