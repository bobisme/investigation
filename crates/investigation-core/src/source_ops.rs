//! Attributed source assessment and withdrawal of Sources and Artifacts.
//!
//! `source.assess` appends one attributed judgment to the Source under
//! `source_assessment` in a new revision. Authenticity and attribution,
//! reliability, origin role and currency are separate dimensions, each with
//! its own rationale; `cannot_judge` is allowed and no combined grade exists.
//!
//! `source.withdraw` and `artifact.withdraw` append one attributed withdrawal
//! to the record under `withdrawals` in a new revision. An `entire` withdrawal
//! also sets `status: withdrawn`, which removes the record from further
//! evidentiary use. A `partial` withdrawal names the part it covers and keeps
//! the rest usable. Withdrawal is not erasure: availability, digest, bytes and
//! every earlier revision stay unchanged, and reads label the bytes withdrawn.
//!
//! These fields record evidentiary use, not what the record describes. The
//! freshness index excludes them from the substantive basis and reacts to them
//! explicitly ([`use_trigger`]): each new withdrawal and each newly raised
//! assessment concern marks dependents `needs_review`. A dependent whose
//! current revision was accepted after a partial withdrawal or a raised
//! concern read the record with it in effect, so that change is not new to it
//! ([`UseChange::since`]); an `entire` withdrawal is new to every dependent.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    catalog::{OperationDescriptor, OperationInfo},
    command::RevisionRef,
    evidence::ArtifactAvailability,
    freshness::Trigger,
    header::Actor,
    identity::{Id, Kind},
    input,
    state::{OperationContext, Record, State},
    wire::{ErrorCode, Failure},
};

/// Source and Artifact body keys that record evidentiary use. A change limited
/// to these keys never changes what the record describes.
pub const USE_FIELDS: &[&str] = &["status", "withdrawals", "source_assessment"];
/// Source body key that holds the append-only assessment log.
pub const SOURCE_ASSESSMENT_KEY: &str = "source_assessment";
/// Source and Artifact body key that holds the append-only withdrawal log.
pub const WITHDRAWALS_KEY: &str = "withdrawals";

const MAX_JUDGMENTS: usize = 64;
const MAX_WITHDRAWALS: usize = 32;
const MAX_LIMITATIONS: usize = 32;
const MAX_REFS: usize = 32;
const MAX_LINEAGE: usize = 64;
const TEXT_LIMIT: usize = 2048;

/// Recorded when an `entire` withdrawal is in effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidentiaryStatus {
    Withdrawn,
}

/// Why a Source or Artifact is withdrawn from evidentiary use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WithdrawalReason {
    /// The originator retracted the material.
    RetractedByOrigin,
    /// An erratum or correction notice changes the material.
    CorrectedByErratum,
    /// A later edition or release replaces the material.
    Superseded,
    /// The material may not be what or from whom it claims to be.
    AuthenticityDoubt,
    /// The material was collected, imported or attributed incorrectly.
    CollectionError,
    /// The material is outside the case scope.
    OutOfScope,
    Other,
}

/// What a withdrawal covers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "extent", rename_all = "snake_case", deny_unknown_fields)]
pub enum WithdrawalScope {
    /// The whole record leaves evidentiary use.
    Entire,
    /// Only the named part is withdrawn; the rest stays usable. The part is a
    /// statement, not a selector, so the core cannot tell which part a
    /// dependent relies on: every dependent accepted before the withdrawal is
    /// marked for review, including one that quotes another part. A dependent
    /// revision accepted after it, such as a new observation, a measurement
    /// judgment or a revalidation, read the record with the withdrawal in
    /// effect and is not.
    Partial { statement: String },
}

/// One attributed withdrawal. The actor is the command actor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Withdrawal {
    pub actor: Actor,
    pub accepted_at_sequence: u64,
    /// The record revision that was withdrawn.
    pub revision: u64,
    pub reason: WithdrawalReason,
    pub scope: WithdrawalScope,
    pub rationale: String,
    /// Exact records that ground the withdrawal, such as an erratum.
    pub evidence_refs: Vec<RevisionRef>,
    /// `source.withdraw`: the write-side guardrails the withdrawal satisfied,
    /// when a pinned pack declares any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardrail: Option<crate::guardrails::GuardrailRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Authenticity {
    Authentic,
    Doubtful,
    NotAuthentic,
    CannotJudge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reliability {
    Reliable,
    Mixed,
    Unreliable,
    CannotJudge,
}

/// What a reliability judgment considered about the source itself.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ReliabilityGround {
    TrackRecord,
    Competence,
    Access,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OriginRole {
    Primary,
    Secondary,
    Tertiary,
    CannotJudge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Currency {
    Current,
    Outdated,
    Superseded,
    CannotJudge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthenticityJudgment {
    pub judgment: Authenticity,
    pub rationale: String,
}

/// Reliability of the source (track record, competence, access), judged
/// separately from the credibility of any specific datum it reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReliabilityJudgment {
    pub judgment: Reliability,
    #[serde(default)]
    pub grounds: Vec<ReliabilityGround>,
    pub rationale: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OriginJudgment {
    pub judgment: OriginRole,
    pub rationale: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CurrencyJudgment {
    pub judgment: Currency,
    pub rationale: String,
}

/// One attributed source judgment. The assessor is the command actor. Absent
/// dimensions were not judged; `cannot_judge` is a judgment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceJudgment {
    pub assessor: Actor,
    pub accepted_at_sequence: u64,
    pub source_revision: u64,
    pub authenticity: Option<AuthenticityJudgment>,
    pub reliability: Option<ReliabilityJudgment>,
    pub origin: Option<OriginJudgment>,
    pub currency: Option<CurrencyJudgment>,
    pub limitations: Vec<String>,
    pub evidence_refs: Vec<RevisionRef>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceAssessmentLog {
    pub judgments: Vec<SourceJudgment>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SourceAssess {
    /// Exact current Source revision being judged.
    source_ref: RevisionRef,
    /// Whether the source is what, and from whom, it claims to be.
    #[serde(default)]
    authenticity: Option<AuthenticityJudgment>,
    /// Track record, competence and access of the source itself.
    #[serde(default)]
    reliability: Option<ReliabilityJudgment>,
    /// Primary, secondary or tertiary relation to what it reports.
    #[serde(default)]
    origin: Option<OriginJudgment>,
    /// Whether the material is still current, for example after an erratum.
    #[serde(default)]
    currency: Option<CurrencyJudgment>,
    #[serde(default)]
    limitations: Vec<String>,
    /// Exact current Sources, Artifacts or Observations the judgment relies on.
    #[serde(default)]
    evidence_refs: Vec<RevisionRef>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SourceWithdraw {
    /// Exact current Source revision being withdrawn.
    source_ref: RevisionRef,
    reason: WithdrawalReason,
    scope: WithdrawalScope,
    rationale: String,
    /// Exact current Sources, Artifacts or Observations that ground the withdrawal.
    #[serde(default)]
    evidence_refs: Vec<RevisionRef>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArtifactWithdraw {
    /// Exact current Artifact revision being withdrawn.
    artifact_ref: RevisionRef,
    reason: WithdrawalReason,
    scope: WithdrawalScope,
    rationale: String,
    /// Exact current Sources, Artifacts or Observations that ground the withdrawal.
    #[serde(default)]
    evidence_refs: Vec<RevisionRef>,
}

pub(crate) fn operations() -> [OperationDescriptor; 3] {
    [
        OperationDescriptor::active(
            OperationInfo {
                name: "source.assess",
                stage: "assess",
                summary: "Record an attributed judgment of a Source's authenticity, reliability, origin role or currency.",
                when_to_use: "Judge the source itself, separately from the credibility of any datum it reports, for example after finding an erratum.",
                lifecycle: "A current Source revision in an existing case.",
                effects: "Append one judgment to the Source's source_assessment in a new revision. Each dimension keeps its own rationale; no combined grade is computed. A newly raised concern marks dependents needs_review.",
                returns: "The Source ID and its new revision.",
                revision_rule: "Supply the exact current source_ref.",
                idempotency: "Retry the identical command with its original actor and command ID.",
                repair: "Read the current Source, cite current exact revisions, and judge at least one dimension. Use cannot_judge when the evidence does not allow a judgment.",
                related: &["source.withdraw", "artifact.withdraw", "measurement.assess"],
            },
            json!({
                "source_ref": {"id": "src_019acd16-0000-7000-8000-000000000001", "revision": 1},
                "authenticity": {"judgment": "authentic", "rationale": "Retrieved from the publisher's own archive."},
                "reliability": {"judgment": "cannot_judge", "grounds": ["track_record"], "rationale": "No earlier material from this publisher was examined."},
                "origin": {"judgment": "primary", "rationale": "The publisher issued the specification."},
                "currency": {"judgment": "superseded", "rationale": "A later erratum changes the stated value."},
                "limitations": ["Only the retained edition was examined."],
                "evidence_refs": []
            }),
            validate_assess,
            assess,
        ),
        OperationDescriptor::active(
            OperationInfo {
                name: "source.withdraw",
                stage: "observe",
                summary: "Withdraw a Source, entirely or in part, from evidentiary use. This is not erasure.",
                when_to_use: "The source was retracted, corrected, superseded, misattributed or collected in error.",
                lifecycle: "A current Source revision that is not already entirely withdrawn.",
                effects: concat!(
                    "Append one attributed withdrawal to the Source in a new revision; an entire withdrawal also sets status withdrawn. ",
                    "Dependent observations, assessments, comparisons, conclusions and decisions become needs_review. ",
                    "Withdrawal is not erasure: digest, availability, retained bytes and earlier revisions stay unchanged and readable, labeled withdrawn."
                ),
                returns: "The Source ID and its new revision.",
                revision_rule: "Supply the exact current source_ref.",
                idempotency: "Retry the identical command with its original actor and command ID.",
                repair: "Read the current Source and cite its exact revision. Describe the withdrawn part for a partial withdrawal.",
                related: &["artifact.withdraw", "source.assess", "observation.retract"],
            },
            json!({
                "source_ref": {"id": "src_019acd16-0000-7000-8000-000000000001", "revision": 1},
                "reason": "corrected_by_erratum",
                "scope": {"extent": "partial", "statement": "The stated limit in the second table."},
                "rationale": "The publisher's erratum replaces the stated limit.",
                "evidence_refs": []
            }),
            validate_source_withdraw,
            withdraw_source,
        ),
        OperationDescriptor::active(
            OperationInfo {
                name: "artifact.withdraw",
                stage: "observe",
                summary: "Withdraw an Artifact, entirely or in part, from evidentiary use. This is not erasure.",
                when_to_use: "Specific retained or referenced bytes must no longer ground new or existing interpretations.",
                lifecycle: "A current Artifact revision that is not already entirely withdrawn.",
                effects: concat!(
                    "Append one attributed withdrawal to the Artifact in a new revision; an entire withdrawal also sets status withdrawn. ",
                    "Derivatives and dependent records become needs_review. ",
                    "Withdrawal is not erasure: digest, availability, retained bytes and earlier revisions stay unchanged and readable, labeled withdrawn."
                ),
                returns: "The Artifact ID and its new revision.",
                revision_rule: "Supply the exact current artifact_ref.",
                idempotency: "Retry the identical command with its original actor and command ID.",
                repair: "Read the current Artifact and cite its exact revision. Describe the withdrawn part for a partial withdrawal.",
                related: &["source.withdraw", "source.assess", "observation.retract"],
            },
            json!({
                "artifact_ref": {"id": "art_019acd16-0000-7000-8000-000000000001", "revision": 1},
                "reason": "collection_error",
                "scope": {"extent": "entire"},
                "rationale": "The capture recorded the wrong environment.",
                "evidence_refs": []
            }),
            validate_artifact_withdraw,
            withdraw_artifact,
        ),
    ]
}

fn invalid(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(path)
}

fn gate(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::GateFailed, message).at(path)
}

fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "A stored source assessment or withdrawal is invalid.",
    )
}

fn text(value: &str, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() || value.chars().count() > TEXT_LIMIT {
        return Err(invalid(
            path,
            format!("Supply nonblank text no longer than {TEXT_LIMIT} characters."),
        ));
    }
    Ok(())
}

fn kind_is(reference: RevisionRef, kind: Kind, path: &str) -> Result<(), Failure> {
    if reference.id.kind() == kind {
        Ok(())
    } else {
        Err(invalid(path, format!("Expected a {kind} reference.")))
    }
}

fn evidence_refs(refs: &[RevisionRef]) -> Result<(), Failure> {
    if refs.len() > MAX_REFS {
        return Err(invalid("/evidence_refs", "Supply at most 32 references."));
    }
    for (index, reference) in refs.iter().enumerate() {
        if !matches!(
            reference.id.kind(),
            Kind::Source | Kind::Artifact | Kind::Observation
        ) {
            return Err(invalid(
                &format!("/evidence_refs/{index}"),
                "Cite a Source, Artifact or Observation.",
            ));
        }
    }
    Ok(())
}

fn validate_assess(payload: &SourceAssess) -> Result<(), Failure> {
    kind_is(payload.source_ref, Kind::Source, "/source_ref")?;
    let mut judged = false;
    if let Some(item) = &payload.authenticity {
        text(&item.rationale, "/authenticity/rationale")?;
        judged = true;
    }
    if let Some(item) = &payload.reliability {
        text(&item.rationale, "/reliability/rationale")?;
        if item.grounds.iter().collect::<BTreeSet<_>>().len() != item.grounds.len() {
            return Err(invalid("/reliability/grounds", "Name each ground once."));
        }
        judged = true;
    }
    if let Some(item) = &payload.origin {
        text(&item.rationale, "/origin/rationale")?;
        judged = true;
    }
    if let Some(item) = &payload.currency {
        text(&item.rationale, "/currency/rationale")?;
        judged = true;
    }
    if !judged {
        return Err(invalid(
            "",
            "Judge at least one of authenticity, reliability, origin or currency; use cannot_judge when no judgment is possible.",
        ));
    }
    if payload.limitations.len() > MAX_LIMITATIONS {
        return Err(invalid("/limitations", "Supply at most 32 limitations."));
    }
    for (index, limitation) in payload.limitations.iter().enumerate() {
        text(limitation, &format!("/limitations/{index}"))?;
    }
    evidence_refs(&payload.evidence_refs)
}

fn validate_withdrawal(
    scope: &WithdrawalScope,
    rationale: &str,
    refs: &[RevisionRef],
) -> Result<(), Failure> {
    if let WithdrawalScope::Partial { statement } = scope {
        text(statement, "/scope/statement")?;
    }
    text(rationale, "/rationale")?;
    evidence_refs(refs)
}

fn validate_source_withdraw(payload: &SourceWithdraw) -> Result<(), Failure> {
    kind_is(payload.source_ref, Kind::Source, "/source_ref")?;
    validate_withdrawal(&payload.scope, &payload.rationale, &payload.evidence_refs)
}

fn validate_artifact_withdraw(payload: &ArtifactWithdraw) -> Result<(), Failure> {
    kind_is(payload.artifact_ref, Kind::Artifact, "/artifact_ref")?;
    validate_withdrawal(&payload.scope, &payload.rationale, &payload.evidence_refs)
}

/// Same-case current record at the exact cited revision.
fn exact_current<'a>(
    ctx: &'a OperationContext<'_>,
    reference: RevisionRef,
    path: &str,
) -> Result<&'a Record, Failure> {
    let record = ctx.current(reference.id).ok_or_else(|| {
        Failure::new(
            ErrorCode::UnknownReference,
            "The record is absent from this case.",
        )
        .at(path)
    })?;
    if record.reference() != reference {
        let mut failure = Failure::new(
            ErrorCode::RevisionConflict,
            "The record changed; read its current revision before retrying.",
        )
        .at(path);
        failure.error.expected = Some(json!(reference.revision));
        failure.error.actual = Some(json!(record.reference().revision));
        failure.error.retry = Some("read current revision and retry".into());
        return Err(failure);
    }
    Ok(record)
}

fn check_evidence(
    ctx: &OperationContext<'_>,
    target: Id,
    refs: &[RevisionRef],
) -> Result<(), Failure> {
    let mut seen = BTreeSet::new();
    for (index, reference) in refs.iter().enumerate() {
        let path = format!("/evidence_refs/{index}");
        if reference.id == target {
            return Err(invalid(&path, "Cite records other than the subject."));
        }
        if !seen.insert(reference.id) {
            return Err(invalid(&path, "Cite each record once."));
        }
        exact_current(ctx, *reference, &path)?;
    }
    Ok(())
}

/// The stored withdrawal log. An absent key is an empty log.
pub fn withdrawals(body: &Value) -> Result<Vec<Withdrawal>, Failure> {
    match body.get(WITHDRAWALS_KEY) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value) => input::decode(value).map_err(|_| integrity()),
    }
}

/// The stored source assessment log. An absent key is an empty log.
pub fn assessment_log(body: &Value) -> Result<SourceAssessmentLog, Failure> {
    match body.get(SOURCE_ASSESSMENT_KEY) {
        None | Some(Value::Null) => Ok(SourceAssessmentLog::default()),
        Some(value) => input::decode(value).map_err(|_| integrity()),
    }
}

/// Whether an `entire` withdrawal is in effect on this record.
pub fn entirely_withdrawn(record: &Record) -> bool {
    record.body["status"] == "withdrawn"
}

fn assess(payload: SourceAssess, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let target = exact_current(ctx, payload.source_ref, "/source_ref")?.clone();
    check_evidence(ctx, target.header.id, &payload.evidence_refs)?;
    let mut log = assessment_log(&target.body)?;
    if log.judgments.len() >= MAX_JUDGMENTS {
        return Err(gate(
            "/source_ref",
            "This Source has reached its judgment limit.",
        ));
    }
    log.judgments.push(SourceJudgment {
        assessor: ctx.actor().clone(),
        accepted_at_sequence: ctx.acceptance_sequence(),
        source_revision: payload.source_ref.revision,
        authenticity: payload.authenticity,
        reliability: payload.reliability,
        origin: payload.origin,
        currency: payload.currency,
        limitations: payload.limitations,
        evidence_refs: payload.evidence_refs,
    });
    let mut body = target.body;
    body.as_object_mut()
        .ok_or_else(integrity)?
        .insert(SOURCE_ASSESSMENT_KEY.into(), json!(log));
    ctx.update(payload.source_ref, body)?;
    Ok(())
}

struct WithdrawRequest {
    target: RevisionRef,
    path: &'static str,
    reason: WithdrawalReason,
    scope: WithdrawalScope,
    rationale: String,
    evidence_refs: Vec<RevisionRef>,
    /// The guarded operation, for `source.withdraw` only.
    guard: Option<crate::guardrails::GuardedOperation>,
}

fn withdraw(request: WithdrawRequest, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let target = exact_current(ctx, request.target, request.path)?.clone();
    if entirely_withdrawn(&target) {
        return Err(gate(
            request.path,
            "The record is already entirely withdrawn; its history stays readable.",
        ));
    }
    check_evidence(ctx, target.header.id, &request.evidence_refs)?;
    let mut log = withdrawals(&target.body)?;
    if log.len() >= MAX_WITHDRAWALS {
        return Err(gate(
            request.path,
            "This record has reached its withdrawal limit; withdraw it entirely instead.",
        ));
    }
    let guardrail = match request.guard {
        Some(operation) => crate::guardrails::enforce(ctx, operation, &target)?,
        None => None,
    };
    let entire = request.scope == WithdrawalScope::Entire;
    log.push(Withdrawal {
        actor: ctx.actor().clone(),
        accepted_at_sequence: ctx.acceptance_sequence(),
        revision: request.target.revision,
        reason: request.reason,
        scope: request.scope,
        rationale: request.rationale,
        evidence_refs: request.evidence_refs,
        guardrail,
    });
    let mut body = target.body;
    let object = body.as_object_mut().ok_or_else(integrity)?;
    object.insert(WITHDRAWALS_KEY.into(), json!(log));
    if entire {
        object.insert("status".into(), json!(EvidentiaryStatus::Withdrawn));
    }
    ctx.update(request.target, body)?;
    Ok(())
}

fn withdraw_source(payload: SourceWithdraw, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    withdraw(
        WithdrawRequest {
            target: payload.source_ref,
            path: "/source_ref",
            guard: Some(crate::guardrails::GuardedOperation::SourceWithdraw),
            reason: payload.reason,
            scope: payload.scope,
            rationale: payload.rationale,
            evidence_refs: payload.evidence_refs,
        },
        ctx,
    )
}

fn withdraw_artifact(
    payload: ArtifactWithdraw,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    withdraw(
        WithdrawRequest {
            target: payload.artifact_ref,
            path: "/artifact_ref",
            guard: None,
            reason: payload.reason,
            scope: payload.scope,
            rationale: payload.rationale,
            evidence_refs: payload.evidence_refs,
        },
        ctx,
    )
}

fn without_use(body: &Value) -> Value {
    let mut body = body.clone();
    if let Some(object) = body.as_object_mut() {
        for key in USE_FIELDS {
            object.remove(*key);
        }
    }
    body
}

/// The current same-case revision of a cited Source or Artifact when it differs
/// from the cited revision only in evidentiary use (withdrawals, assessments).
/// Callers still decide whether that use permits what they do. `None` for other
/// kinds, a different case, or a substantive change.
pub fn use_successor(state: &State, case: Id, cited: RevisionRef) -> Option<&Record> {
    if !matches!(cited.id.kind(), Kind::Source | Kind::Artifact) {
        return None;
    }
    let current = state
        .current(cited.id)
        .filter(|record| record.header.case_id == case)?;
    if current.header.revision.0 == cited.revision {
        return Some(current);
    }
    let historic = state.revision(cited)?;
    (without_use(&historic.body) == without_use(&current.body)).then_some(current)
}

/// Whether `after` differs from `before` only in evidentiary-use fields.
pub(crate) fn use_only_change(before: &Record, after: &Record) -> bool {
    without_use(&before.body) == without_use(&after.body)
}

/// Current revisions of a record and the Artifacts and Sources it derives
/// from, nearest first. Observations start from their basis Artifact.
fn lineage(state: &State, record: &Record) -> Vec<Record> {
    let case = record.header.case_id;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut queue = std::collections::VecDeque::new();
    match record.header.id.kind() {
        Kind::Observation => queue.push_back(record.body["basis"]["artifact_ref"].clone()),
        Kind::Source | Kind::Artifact => queue.push_back(json!(record.reference())),
        _ => {}
    }
    while let Some(value) = queue.pop_front() {
        if out.len() >= MAX_LINEAGE {
            break;
        }
        let Ok(reference) = input::decode::<RevisionRef>(&value) else {
            continue;
        };
        if !seen.insert(reference.id) {
            continue;
        }
        let Some(current) = state
            .current(reference.id)
            .filter(|current| current.header.case_id == case)
        else {
            continue;
        };
        if current.header.id.kind() == Kind::Artifact {
            for transform in current.body["transforms"].as_array().into_iter().flatten() {
                queue.push_back(transform["input"].clone());
            }
            queue.push_back(current.body["source"].clone());
        }
        if matches!(current.header.id.kind(), Kind::Source | Kind::Artifact) {
            out.push(current.clone());
        }
    }
    out
}

/// The nearest entirely withdrawn record in the current lineage of a Source,
/// Artifact or Observation: the record itself, an Artifact it was derived
/// from, or a Source. Evidence gates refuse new use of such material.
pub fn withdrawn_lineage(state: &State, record: &Record) -> Option<RevisionRef> {
    lineage(state, record)
        .into_iter()
        .find(entirely_withdrawn)
        .map(|record| record.reference())
}

/// Evidentiary use of a record and its lineage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UseState {
    InUse,
    PartiallyWithdrawn,
    Withdrawn,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WithdrawalInEffect {
    /// Current revision of the withdrawn record: this one or its lineage.
    pub record: RevisionRef,
    pub withdrawal: Withdrawal,
}

/// Evidentiary use shown wherever a Source, Artifact or Observation is read.
/// Withdrawal never changes availability; `availability` is reported as
/// recorded, so withdrawn bytes stay readable and are labeled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvidentiaryUse {
    pub state: UseState,
    pub availability: Option<ArtifactAvailability>,
    pub withdrawals: Vec<WithdrawalInEffect>,
    pub erasure: bool,
}

pub fn evidentiary_use(state: &State, record: &Record) -> EvidentiaryUse {
    let lineage = lineage(state, record);
    let availability = lineage
        .iter()
        .find(|item| item.header.id.kind() == Kind::Artifact)
        .and_then(|artifact| input::decode(&artifact.body["availability"]).ok());
    let mut withdrawn = false;
    let mut in_effect = Vec::new();
    for item in &lineage {
        withdrawn |= entirely_withdrawn(item);
        for withdrawal in withdrawals(&item.body).unwrap_or_default() {
            in_effect.push(WithdrawalInEffect {
                record: item.reference(),
                withdrawal,
            });
        }
    }
    EvidentiaryUse {
        state: if withdrawn {
            UseState::Withdrawn
        } else if in_effect.is_empty() {
            UseState::InUse
        } else {
            UseState::PartiallyWithdrawn
        },
        availability,
        withdrawals: in_effect,
        erasure: false,
    }
}

/// A separately judged source dimension. There is no combined grade.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SourceDimension {
    Authenticity,
    Reliability,
    Origin,
    Currency,
}

impl SourceDimension {
    const ALL: [Self; 4] = [
        Self::Authenticity,
        Self::Reliability,
        Self::Origin,
        Self::Currency,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Authenticity => "authenticity",
            Self::Reliability => "reliability",
            Self::Origin => "origin",
            Self::Currency => "currency",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CurrentSourceJudgment {
    pub assessor: Actor,
    pub judgment: String,
    pub rationale: String,
    pub accepted_at_sequence: u64,
    pub source_revision: u64,
    /// Whether this judgment raises a concern about using the source.
    pub concern: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DimensionStanding {
    pub dimension: SourceDimension,
    /// Latest judgment per assessor; judgments may disagree.
    pub current: Vec<CurrentSourceJudgment>,
    pub concern: bool,
    pub disagreement: bool,
}

/// Every source dimension, judged or not. `current` is empty for a dimension
/// nobody has judged; that is shown, never omitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceStanding {
    pub dimensions: Vec<DimensionStanding>,
    pub judgments_kept: usize,
}

fn dimension_judgment(
    judgment: &SourceJudgment,
    dimension: SourceDimension,
) -> Option<(Value, String, bool)> {
    match dimension {
        SourceDimension::Authenticity => judgment.authenticity.as_ref().map(|item| {
            (
                json!(item.judgment),
                item.rationale.clone(),
                matches!(
                    item.judgment,
                    Authenticity::Doubtful | Authenticity::NotAuthentic
                ),
            )
        }),
        SourceDimension::Reliability => judgment.reliability.as_ref().map(|item| {
            (
                json!(item.judgment),
                item.rationale.clone(),
                item.judgment == Reliability::Unreliable,
            )
        }),
        SourceDimension::Origin => judgment
            .origin
            .as_ref()
            .map(|item| (json!(item.judgment), item.rationale.clone(), false)),
        SourceDimension::Currency => judgment.currency.as_ref().map(|item| {
            (
                json!(item.judgment),
                item.rationale.clone(),
                matches!(item.judgment, Currency::Outdated | Currency::Superseded),
            )
        }),
    }
}

/// Current per-dimension source standing from the stored judgments. The latest
/// judgment per assessor and dimension is current.
pub fn standing(record: &Record) -> Result<SourceStanding, Failure> {
    let log = assessment_log(&record.body)?;
    let dimensions = SourceDimension::ALL
        .into_iter()
        .map(|dimension| {
            let mut latest: BTreeMap<&Actor, CurrentSourceJudgment> = BTreeMap::new();
            for judgment in &log.judgments {
                if let Some((value, rationale, concern)) = dimension_judgment(judgment, dimension) {
                    latest.insert(
                        &judgment.assessor,
                        CurrentSourceJudgment {
                            assessor: judgment.assessor.clone(),
                            judgment: value.as_str().unwrap_or_default().to_owned(),
                            rationale,
                            accepted_at_sequence: judgment.accepted_at_sequence,
                            source_revision: judgment.source_revision,
                            concern,
                        },
                    );
                }
            }
            let current: Vec<_> = latest.into_values().collect();
            DimensionStanding {
                dimension,
                concern: current.iter().any(|judgment| judgment.concern),
                disagreement: current
                    .iter()
                    .map(|judgment| &judgment.judgment)
                    .collect::<BTreeSet<_>>()
                    .len()
                    > 1,
                current,
            }
        })
        .collect();
    Ok(SourceStanding {
        dimensions,
        judgments_kept: log.judgments.len(),
    })
}

fn concerns(record: &Record) -> BTreeSet<SourceDimension> {
    standing(record)
        .map(|standing| {
            standing
                .dimensions
                .into_iter()
                .filter(|dimension| dimension.concern)
                .map(|dimension| dimension.dimension)
                .collect()
        })
        .unwrap_or_default()
}

/// A change in evidentiary use between a cited and the current revision of a
/// Source or Artifact, and when it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UseChange {
    pub trigger: Trigger,
    /// Acceptance sequence of the latest withdrawal or concern-raising judgment
    /// behind the trigger, when a reader accepted at or after it read the
    /// record with the change in effect. `None` for an `entire` withdrawal and
    /// unreadable use data: no later reading makes those current.
    pub since: Option<u64>,
}

/// Freshness trigger for a change in evidentiary use between the cited and the
/// current revision of a Source or Artifact: a new withdrawal, or a newly
/// raised authenticity, reliability or currency concern. Unreadable use data
/// errs toward review.
pub(crate) fn use_trigger(cited: &Record, current: &Record) -> Option<Trigger> {
    use_change(cited, current).map(|change| change.trigger)
}

/// [`use_trigger`] with the sequence of the change. When new partial
/// withdrawals and a newly raised concern are both present, the later event
/// names the trigger and `since` covers both.
pub(crate) fn use_change(cited: &Record, current: &Record) -> Option<UseChange> {
    if !matches!(current.header.id.kind(), Kind::Source | Kind::Artifact) {
        return None;
    }
    let unacknowledgeable = |trigger| {
        Some(UseChange {
            trigger,
            since: None,
        })
    };
    let withdrawn = match (withdrawals(&cited.body), withdrawals(&current.body)) {
        (Ok(before), Ok(after)) if after.len() <= before.len() => None,
        (Ok(before), Ok(after)) => {
            let new = &after[before.len()..];
            if entirely_withdrawn(current)
                || new.iter().any(|item| item.scope == WithdrawalScope::Entire)
            {
                return unacknowledgeable(Trigger::Withdrawn);
            }
            new.iter().map(|item| item.accepted_at_sequence).max()
        }
        _ => return unacknowledgeable(Trigger::Withdrawn),
    };
    if assessment_log(&current.body).is_err() {
        return unacknowledgeable(Trigger::UnreadableBasis);
    }
    let before = concerns(cited);
    let now = standing(current).ok()?;
    let mut raised = Vec::new();
    let mut raised_at = None;
    for dimension in now.dimensions.iter().filter(|d| d.concern) {
        if before.contains(&dimension.dimension) {
            continue;
        }
        raised.push(dimension.dimension.as_str().to_owned());
        raised_at = dimension
            .current
            .iter()
            .filter(|judgment| judgment.concern)
            .map(|judgment| judgment.accepted_at_sequence)
            .chain(raised_at)
            .max();
    }
    let concern = (!raised.is_empty()).then_some(Trigger::SourceConcern { dimensions: raised });
    match (withdrawn, concern) {
        (Some(at), Some(concern)) if raised_at.is_some_and(|raised| raised > at) => {
            Some(UseChange {
                trigger: concern,
                since: raised_at,
            })
        }
        (Some(at), concern) => Some(UseChange {
            trigger: Trigger::Withdrawn,
            since: Some(at).max(concern.and(raised_at)),
        }),
        (None, Some(concern)) => Some(UseChange {
            trigger: concern,
            since: raised_at,
        }),
        (None, None) => None,
    }
}
