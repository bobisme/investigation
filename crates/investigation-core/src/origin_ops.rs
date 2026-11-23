//! Claim-level origin and attributed dependence judgments.
//!
//! Independence is per claim, not per document. An Observation is one claim
//! read from one passage, and its Source records the activity that collected
//! the document. Where the claim itself comes from is a separate judgment:
//! one article can restate a specification value in one passage and report
//! its own bench test in another.
//!
//! `origin.trace` appends one attributed origin judgment to an Observation
//! under `claim_origin` in a new revision: the passage originates the claim
//! (`original`), restates the claim of another Observation (`restates`), or
//! its origin could not be established (`unknown`). A trace may record the
//! dimensions on which origins can share a dependence: upstream source,
//! collection run, method, fixture or environment, and named dependence
//! groups. The latest trace per assessor is current; assessors may disagree.
//!
//! `origin.judge_dependence` appends one attributed judgment linking two or
//! more origin roots (`shared`, `partly_shared`,
//! `independent_on_stated_dimensions` or `unknown`) with its basis, stored on
//! the first root. An absent judgment means `unknown`, never independent.
//!
//! Reads resolve restatement chains to an origin root ([`effective_origin`])
//! and summarize a set of cited Observations ([`origin_summary`]): the
//! acquisition count is reported separately from the known origin count, and
//! every acquisition stays its own record. Claims of one acquisition that have
//! no traced origin form one unknown origin group, so quotations, extractions,
//! screenshots and notes of one document never count as separate origins.
//! Shared recorded dimensions are computed facts; independence is only ever an
//! attributed judgment. Independence is one reliability aspect. It is reported
//! on its own and never combined with byte integrity, source standing,
//! measurement validity, interpretive confidence or reproduction status.
//!
//! Like other evidentiary-use fields, `claim_origin` is outside an
//! Observation's substantive basis. Its upstream and root references are
//! registered in the freshness index as change-only edges.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    catalog::{OperationDescriptor, OperationInfo},
    command::RevisionRef,
    derivation,
    header::Actor,
    identity::{Id, Kind},
    input,
    state::{OperationContext, Record, State},
    wire::{ErrorCode, Failure},
};

/// Observation body key that holds the append-only origin log.
pub const CLAIM_ORIGIN_KEY: &str = "claim_origin";

const MAX_TRACES: usize = 64;
const MAX_DEPENDENCE: usize = 64;
const MAX_ROOTS: usize = 16;
const MAX_GROUP_NAMES: usize = 16;
const MAX_REFS: usize = 32;
/// Restatement steps followed before an origin is reported as unknown.
pub const MAX_CHAIN: usize = 64;
/// Pairs listed in a summary; every pair is still counted.
pub const MAX_LISTED_PAIRS: usize = 64;
/// Dependence judgments listed in an Observation view.
pub const MAX_LISTED_JUDGMENTS: usize = 16;
const TEXT_LIMIT: usize = 2048;
const NAME_LIMIT: usize = 256;

/// Where one Observation's claim comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OriginLink {
    /// The passage originates the claim: its own measurement, a first-party
    /// statement or correction, or a report nobody upstream made.
    Original,
    /// The passage restates, quotes or summarizes the claim of another
    /// Observation, which may itself be a restatement.
    Restates { observation_ref: RevisionRef },
    /// The assessor looked and could not establish the origin.
    Unknown,
}

/// Dimensions on which claim origins can share a dependence. Each is optional;
/// an absent dimension was not recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OriginDimensions {
    /// Who or what first produced the claim, as the assessor understands it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_source: Option<String>,
    /// The Run that produced the originating observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_run: Option<RevisionRef>,
    /// How the originating claim was produced, for example a bench test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Fixture, unit, build or environment the originating claim was made in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Named groups of origins known to depend on each other.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependence_groups: Vec<String>,
}

/// One attributed origin judgment. The assessor is the command actor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OriginTrace {
    pub assessor: Actor,
    pub accepted_at_sequence: u64,
    pub observation_revision: u64,
    pub origin: OriginLink,
    #[serde(default)]
    pub dimensions: OriginDimensions,
    pub rationale: String,
    #[serde(default)]
    pub evidence_refs: Vec<RevisionRef>,
}

/// How far origin roots depend on each other. `unknown` is a normal state.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Dependence {
    Shared,
    PartlyShared,
    IndependentOnStatedDimensions,
    Unknown,
}

/// A dimension a dependence judgment covers or a recorded fact shares.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OriginDimension {
    Acquisition,
    UpstreamSource,
    CollectionRun,
    Method,
    Environment,
    DependenceGroup,
}

/// One attributed dependence judgment linking origin roots. The assessor is
/// the command actor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DependenceJudgment {
    pub assessor: Actor,
    pub accepted_at_sequence: u64,
    /// Revision of the first root when the judgment was recorded.
    pub observation_revision: u64,
    pub roots: Vec<RevisionRef>,
    pub judgment: Dependence,
    #[serde(default)]
    pub dimensions: Vec<OriginDimension>,
    pub basis: String,
    #[serde(default)]
    pub evidence_refs: Vec<RevisionRef>,
}

/// The stored origin log of one Observation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimOriginLog {
    #[serde(default)]
    pub traces: Vec<OriginTrace>,
    #[serde(default)]
    pub dependence: Vec<DependenceJudgment>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OriginTracePayload {
    /// Exact current Observation whose claim is traced.
    observation_ref: RevisionRef,
    origin: OriginLink,
    #[serde(default)]
    dimensions: OriginDimensions,
    /// Why the assessor judges this origin.
    rationale: String,
    /// Exact current Sources, Artifacts or Observations the judgment relies on.
    #[serde(default)]
    evidence_refs: Vec<RevisionRef>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DependencePayload {
    /// Exact current Observations, each its own known origin root. The
    /// judgment is stored on the first one.
    roots: Vec<RevisionRef>,
    judgment: Dependence,
    /// Dimensions the judgment covers. Required for
    /// `independent_on_stated_dimensions`.
    #[serde(default)]
    dimensions: Vec<OriginDimension>,
    /// Who judged on what basis: what was compared and found.
    basis: String,
    /// Exact current Sources, Artifacts or Observations the judgment relies on.
    #[serde(default)]
    evidence_refs: Vec<RevisionRef>,
}

pub(crate) fn operations() -> [OperationDescriptor; 2] {
    [
        OperationDescriptor::active(
            OperationInfo {
                name: "origin.trace",
                stage: "assess",
                summary: "Record an attributed judgment of where one Observation's claim originates, separately from the acquisition that collected its document.",
                when_to_use: "Several documents report the same claim, or one document mixes restated and original claims. Trace each claim before counting corroboration.",
                lifecycle: "A current, non-retracted Observation in an existing case.",
                effects: concat!(
                    "Append one origin trace to the Observation's claim_origin in a new revision. ",
                    "The value, basis and acquisition records stay unchanged. ",
                    "Reads follow restatements to an origin root and count acquisitions separately from known origins; an untraced claim's origin is unknown."
                ),
                returns: "The Observation ID and its new revision.",
                revision_rule: "Supply the exact current observation_ref and, for a restatement, the exact current upstream observation_ref.",
                idempotency: "Retry the identical command with its original actor and command ID.",
                repair: "Read the current Observations. A restatement names a different Observation in this case and must not make the origin chain circular. Use unknown when the origin cannot be established.",
                related: &[
                    "origin.judge_dependence",
                    "observation.add",
                    "source.assess",
                ],
            },
            json!({
                "observation_ref": {"id": "obs_019acd16-0000-7000-8000-000000000002", "revision": 1},
                "origin": {"kind": "restates", "observation_ref": {"id": "obs_019acd16-0000-7000-8000-000000000001", "revision": 1}},
                "dimensions": {"upstream_source": "The publisher's specification, Table 7."},
                "rationale": "The article states the specification value in the same words and cites no test of its own for it.",
                "evidence_refs": []
            }),
            validate_trace,
            trace,
        ),
        OperationDescriptor::active(
            OperationInfo {
                name: "origin.judge_dependence",
                stage: "assess",
                summary: "Record an attributed judgment of how far two or more claim origin roots depend on each other, and its basis.",
                when_to_use: "Before treating separately originating claims as independent corroboration.",
                lifecycle: "Two or more current, non-retracted Observations, each its own known origin root.",
                effects: concat!(
                    "Append one judgment to the first root's claim_origin in a new revision. ",
                    "Roots without a judgment stay unknown, never independent. ",
                    "Recorded shared dimensions are shown beside the judgment, and a stated independence that a recorded dimension contradicts is flagged."
                ),
                returns: "The first root's Observation ID and its new revision.",
                revision_rule: "Supply the exact current revision of every root.",
                idempotency: "Retry the identical command with its original actor and command ID.",
                repair: "Trace each claim with origin.trace first and cite the roots the traces resolve to. State at least one dimension for independent_on_stated_dimensions.",
                related: &["origin.trace", "assessment.add"],
            },
            json!({
                "roots": [
                    {"id": "obs_019acd16-0000-7000-8000-000000000001", "revision": 2},
                    {"id": "obs_019acd16-0000-7000-8000-000000000003", "revision": 2}
                ],
                "judgment": "independent_on_stated_dimensions",
                "dimensions": ["method", "environment"],
                "basis": "The specification value is a design limit; the bench test measured one production unit with its own harness.",
                "evidence_refs": []
            }),
            validate_dependence,
            judge_dependence,
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
        "A stored claim origin record is invalid.",
    )
}

fn text(value: &str, limit: usize, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() || value.chars().count() > limit {
        return Err(invalid(
            path,
            format!("Supply nonblank text no longer than {limit} characters."),
        ));
    }
    Ok(())
}

fn observation_kind(reference: RevisionRef, path: &str) -> Result<(), Failure> {
    if reference.id.kind() == Kind::Observation {
        Ok(())
    } else {
        Err(invalid(path, "Select an exact Observation revision."))
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

fn validate_dimensions(dimensions: &OriginDimensions) -> Result<(), Failure> {
    for (value, path) in [
        (&dimensions.upstream_source, "/dimensions/upstream_source"),
        (&dimensions.method, "/dimensions/method"),
        (&dimensions.environment, "/dimensions/environment"),
    ] {
        if let Some(value) = value {
            text(value, TEXT_LIMIT, path)?;
        }
    }
    if let Some(run) = dimensions.collection_run
        && run.id.kind() != Kind::Run
    {
        return Err(invalid(
            "/dimensions/collection_run",
            "Select an exact Run revision.",
        ));
    }
    if dimensions.dependence_groups.len() > MAX_GROUP_NAMES {
        return Err(invalid(
            "/dimensions/dependence_groups",
            "Name at most 16 dependence groups.",
        ));
    }
    let mut seen = BTreeSet::new();
    for (index, group) in dimensions.dependence_groups.iter().enumerate() {
        let path = format!("/dimensions/dependence_groups/{index}");
        text(group, NAME_LIMIT, &path)?;
        if !seen.insert(group.trim()) {
            return Err(invalid(&path, "Name each dependence group once."));
        }
    }
    Ok(())
}

fn validate_trace(payload: &OriginTracePayload) -> Result<(), Failure> {
    observation_kind(payload.observation_ref, "/observation_ref")?;
    if let OriginLink::Restates { observation_ref } = payload.origin {
        observation_kind(observation_ref, "/origin/observation_ref")?;
        if observation_ref.id == payload.observation_ref.id {
            return Err(invalid(
                "/origin/observation_ref",
                "A claim cannot restate itself; use original.",
            ));
        }
    }
    validate_dimensions(&payload.dimensions)?;
    text(&payload.rationale, TEXT_LIMIT, "/rationale")?;
    evidence_refs(&payload.evidence_refs)
}

fn validate_dependence(payload: &DependencePayload) -> Result<(), Failure> {
    if !(2..=MAX_ROOTS).contains(&payload.roots.len()) {
        return Err(invalid("/roots", "Link between 2 and 16 origin roots."));
    }
    let mut seen = BTreeSet::new();
    for (index, root) in payload.roots.iter().enumerate() {
        let path = format!("/roots/{index}");
        observation_kind(*root, &path)?;
        if !seen.insert(root.id) {
            return Err(invalid(&path, "Name each origin root once."));
        }
    }
    if payload.dimensions.iter().collect::<BTreeSet<_>>().len() != payload.dimensions.len() {
        return Err(invalid("/dimensions", "Name each dimension once."));
    }
    if payload.judgment == Dependence::IndependentOnStatedDimensions
        && payload.dimensions.is_empty()
    {
        return Err(invalid(
            "/dimensions",
            "State the dimensions on which the origins are independent.",
        ));
    }
    text(&payload.basis, TEXT_LIMIT, "/basis")?;
    evidence_refs(&payload.evidence_refs)
}

/// The stored origin log. An absent key is an empty log.
pub fn origin_log(body: &Value) -> Result<ClaimOriginLog, Failure> {
    match body.get(CLAIM_ORIGIN_KEY) {
        None | Some(Value::Null) => Ok(ClaimOriginLog::default()),
        Some(value) => input::decode(value).map_err(|_| integrity()),
    }
}

fn retracted(record: &Record) -> bool {
    record.body["status"] == "retracted" || record.body["retracted"] == true
}

/// Same-case current Observation at the exact cited revision.
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

fn active_observation<'a>(
    ctx: &'a OperationContext<'_>,
    reference: RevisionRef,
    path: &str,
) -> Result<&'a Record, Failure> {
    let record = exact_current(ctx, reference, path)?;
    if retracted(record) {
        return Err(gate(
            path,
            "A retracted Observation has no current claim to trace; its history stays readable.",
        ));
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

fn store(
    ctx: &mut OperationContext<'_>,
    target: &Record,
    log: &ClaimOriginLog,
) -> Result<(), Failure> {
    let mut body = target.body.clone();
    body.as_object_mut()
        .ok_or_else(integrity)?
        .insert(CLAIM_ORIGIN_KEY.into(), json!(log));
    ctx.update(target.reference(), body)?;
    Ok(())
}

fn trace(payload: OriginTracePayload, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let target = active_observation(ctx, payload.observation_ref, "/observation_ref")?.clone();
    if let OriginLink::Restates { observation_ref } = payload.origin {
        let upstream = exact_current(ctx, observation_ref, "/origin/observation_ref")?;
        let resolved = effective_origin(ctx.state(), upstream);
        if resolved
            .chain
            .iter()
            .any(|step| step.id == target.header.id)
        {
            return Err(gate(
                "/origin/observation_ref",
                "The upstream claim already traces back to this Observation; the origin chain would be circular.",
            ));
        }
    }
    if let Some(run) = payload.dimensions.collection_run
        && ctx.exact(run).is_none()
    {
        return Err(Failure::new(
            ErrorCode::UnknownReference,
            "collection_run is not an exact Run revision in this case.",
        )
        .at("/dimensions/collection_run"));
    }
    check_evidence(ctx, target.header.id, &payload.evidence_refs)?;
    let mut log = origin_log(&target.body)?;
    if log.traces.len() >= MAX_TRACES {
        return Err(gate(
            "/observation_ref",
            "This Observation has reached its origin trace limit.",
        ));
    }
    let mut dimensions = payload.dimensions;
    for value in [
        &mut dimensions.upstream_source,
        &mut dimensions.method,
        &mut dimensions.environment,
    ]
    .into_iter()
    .flatten()
    {
        *value = value.trim().to_owned();
    }
    for group in &mut dimensions.dependence_groups {
        *group = group.trim().to_owned();
    }
    log.traces.push(OriginTrace {
        assessor: ctx.actor().clone(),
        accepted_at_sequence: ctx.acceptance_sequence(),
        observation_revision: payload.observation_ref.revision,
        origin: payload.origin,
        dimensions,
        rationale: payload.rationale,
        evidence_refs: payload.evidence_refs,
    });
    store(ctx, &target, &log)
}

fn judge_dependence(
    payload: DependencePayload,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    for (index, root) in payload.roots.iter().enumerate() {
        let path = format!("/roots/{index}");
        let record = active_observation(ctx, *root, &path)?;
        let origin = effective_origin(ctx.state(), record);
        if origin.root.map(|root| root.id) != Some(root.id) {
            let message = match (origin.state, origin.root) {
                (OriginState::Known, Some(actual)) => format!(
                    "{} restates a claim that originates in {}; link the origin root instead.",
                    root.id, actual.id
                ),
                _ => format!(
                    "{} has no known origin; trace it with origin.trace first. Absent linkage is unknown, not independent.",
                    root.id
                ),
            };
            return Err(gate(&path, message));
        }
    }
    let first = exact_current(ctx, payload.roots[0], "/roots/0")?.clone();
    check_evidence(ctx, first.header.id, &payload.evidence_refs)?;
    let mut log = origin_log(&first.body)?;
    if log.dependence.len() >= MAX_DEPENDENCE {
        return Err(gate(
            "/roots/0",
            "This origin root has reached its dependence judgment limit.",
        ));
    }
    log.dependence.push(DependenceJudgment {
        assessor: ctx.actor().clone(),
        accepted_at_sequence: ctx.acceptance_sequence(),
        observation_revision: first.header.revision.0,
        roots: payload.roots,
        judgment: payload.judgment,
        dimensions: payload.dimensions,
        basis: payload.basis,
        evidence_refs: payload.evidence_refs,
    });
    store(ctx, &first, &log)
}

/// Latest trace per assessor, ordered by assessor.
pub fn current_traces(log: &ClaimOriginLog) -> Vec<&OriginTrace> {
    let mut latest: BTreeMap<&Actor, &OriginTrace> = BTreeMap::new();
    for trace in &log.traces {
        latest.insert(&trace.assessor, trace);
    }
    latest.into_values().collect()
}

/// Latest judgment per assessor and root set.
fn current_judgments(log: &ClaimOriginLog) -> Vec<&DependenceJudgment> {
    let mut latest: BTreeMap<(&Actor, BTreeSet<Id>), &DependenceJudgment> = BTreeMap::new();
    for judgment in &log.dependence {
        let roots = judgment.roots.iter().map(|root| root.id).collect();
        latest.insert((&judgment.assessor, roots), judgment);
    }
    latest.into_values().collect()
}

/// Exact references the current origin log cites, for the freshness index:
/// upstream claims of current traces and other roots of current judgments.
pub(crate) fn origin_references(
    record: &Record,
) -> Result<(Vec<RevisionRef>, Vec<RevisionRef>), Failure> {
    let log = origin_log(&record.body)?;
    let upstream = current_traces(&log)
        .into_iter()
        .filter_map(|trace| match trace.origin {
            OriginLink::Restates { observation_ref } => Some(observation_ref),
            _ => None,
        })
        .collect();
    let roots = current_judgments(&log)
        .into_iter()
        .flat_map(|judgment| judgment.roots.iter().copied())
        .filter(|root| root.id != record.header.id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok((upstream, roots))
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OriginState {
    /// The claim resolves to one origin root.
    Known,
    /// Nobody traced the claim, or tracing stops before an origin root.
    Unknown,
    /// Current traces by different assessors disagree.
    Disputed,
}

/// Why an origin is not known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    NotTraced,
    JudgedUnknown,
    AssessorsDisagree,
    UpstreamMissing,
    Circular,
    ChainTooLong,
    Unreadable,
}

/// The origin one Observation's claim resolves to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EffectiveOrigin {
    pub state: OriginState,
    /// Current revision of the originating Observation when known.
    pub root: Option<RevisionRef>,
    /// Upstream Observations followed from this one, nearest first, ending
    /// at the root or where tracing stops.
    pub chain: Vec<RevisionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<UnknownReason>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LinkKey {
    Original,
    Restates(Id),
    Unknown,
}

/// Resolve an Observation's claim to its origin root through the current
/// traces of each step. Disagreement anywhere on the chain is `disputed`.
pub fn effective_origin(state: &State, record: &Record) -> EffectiveOrigin {
    let case = record.header.case_id;
    let mut chain = Vec::new();
    let mut seen = BTreeSet::from([record.header.id]);
    let mut current = record;
    let stop =
        |chain: Vec<RevisionRef>, state: OriginState, reason: UnknownReason| EffectiveOrigin {
            state,
            root: None,
            chain,
            reason: Some(reason),
        };
    loop {
        let Ok(log) = origin_log(&current.body) else {
            return stop(chain, OriginState::Unknown, UnknownReason::Unreadable);
        };
        let links: BTreeSet<LinkKey> = current_traces(&log)
            .into_iter()
            .map(|trace| match trace.origin {
                OriginLink::Original => LinkKey::Original,
                OriginLink::Restates { observation_ref } => LinkKey::Restates(observation_ref.id),
                OriginLink::Unknown => LinkKey::Unknown,
            })
            .collect();
        if links.len() > 1 {
            return stop(
                chain,
                OriginState::Disputed,
                UnknownReason::AssessorsDisagree,
            );
        }
        match links.into_iter().next() {
            None => return stop(chain, OriginState::Unknown, UnknownReason::NotTraced),
            Some(LinkKey::Unknown) => {
                return stop(chain, OriginState::Unknown, UnknownReason::JudgedUnknown);
            }
            Some(LinkKey::Original) => {
                return EffectiveOrigin {
                    state: OriginState::Known,
                    root: Some(current.reference()),
                    chain,
                    reason: None,
                };
            }
            Some(LinkKey::Restates(id)) => {
                let Some(next) = state.current(id).filter(|next| {
                    next.header.case_id == case && next.header.id.kind() == Kind::Observation
                }) else {
                    return stop(chain, OriginState::Unknown, UnknownReason::UpstreamMissing);
                };
                if !seen.insert(id) {
                    return stop(chain, OriginState::Unknown, UnknownReason::Circular);
                }
                if chain.len() >= MAX_CHAIN {
                    return stop(chain, OriginState::Unknown, UnknownReason::ChainTooLong);
                }
                chain.push(next.reference());
                current = next;
            }
        }
    }
}

/// Sources (acquisitions) an Observation's basis Artifact traces to through
/// its derivation lineage. An assertion has none.
pub fn acquisitions(state: &State, record: &Record) -> BTreeSet<Id> {
    input::decode::<RevisionRef>(&record.body["basis"]["artifact_ref"])
        .ok()
        .and_then(|artifact| derivation::lineage(state, artifact))
        .map(|lineage| lineage.acquisitions())
        .unwrap_or_default()
}

/// Values recorded for one dimension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RecordedDimension {
    pub dimension: OriginDimension,
    pub values: Vec<String>,
}

type Recorded = BTreeMap<OriginDimension, BTreeSet<String>>;

fn record_run(recorded: &mut Recorded, record: &Record) {
    if let Ok(run) = input::decode::<RevisionRef>(&record.body["run_ref"]) {
        recorded
            .entry(OriginDimension::CollectionRun)
            .or_default()
            .insert(run.id.to_string());
    }
}

/// Dimensions recorded for an origin root: its acquisitions, its Run, and
/// what its current `original` traces state.
fn root_dimensions(state: &State, root: &Record) -> Recorded {
    let mut recorded = Recorded::new();
    let acquired: BTreeSet<String> = acquisitions(state, root)
        .into_iter()
        .map(|id| id.to_string())
        .collect();
    if !acquired.is_empty() {
        recorded.insert(OriginDimension::Acquisition, acquired);
    }
    record_run(&mut recorded, root);
    let log = origin_log(&root.body).unwrap_or_default();
    for trace in current_traces(&log) {
        if trace.origin != OriginLink::Original {
            continue;
        }
        let dimensions = &trace.dimensions;
        let mut add = |dimension, value: String| {
            recorded.entry(dimension).or_default().insert(value);
        };
        if let Some(value) = &dimensions.upstream_source {
            add(OriginDimension::UpstreamSource, value.clone());
        }
        if let Some(run) = dimensions.collection_run {
            add(OriginDimension::CollectionRun, run.id.to_string());
        }
        if let Some(value) = &dimensions.method {
            add(OriginDimension::Method, value.clone());
        }
        if let Some(value) = &dimensions.environment {
            add(OriginDimension::Environment, value.clone());
        }
        for group in &dimensions.dependence_groups {
            add(OriginDimension::DependenceGroup, group.clone());
        }
    }
    recorded
}

fn listed(recorded: &Recorded) -> Vec<RecordedDimension> {
    recorded
        .iter()
        .map(|(dimension, values)| RecordedDimension {
            dimension: *dimension,
            values: values.iter().cloned().collect(),
        })
        .collect()
}

/// One origin among the cited claims: a known root with every cited claim
/// that resolves to it, or cited claims of unknown origin that share an
/// acquisition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OriginGroup {
    pub index: usize,
    pub state: OriginState,
    /// The originating Observation of a known origin.
    pub root: Option<RevisionRef>,
    /// Cited Observations in this group, as cited.
    pub observations: Vec<RevisionRef>,
    /// Acquisitions (Sources) the cited Observations were collected through.
    pub acquisitions: Vec<Id>,
    /// Dimensions recorded for the origin. For a known origin they come from
    /// the root; for an unknown group from the cited Observations.
    pub recorded: Vec<RecordedDimension>,
}

/// One attributed dependence judgment that currently applies to a pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CurrentDependence {
    pub assessor: Actor,
    pub judgment: Dependence,
    pub dimensions: Vec<OriginDimension>,
    pub basis: String,
    pub accepted_at_sequence: u64,
    /// Every root the judgment links.
    pub roots: Vec<RevisionRef>,
}

/// Dependence between two origin groups.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OriginPair {
    /// Indexes of the two groups in `origins`.
    pub origins: [usize; 2],
    /// `unknown` when nobody judged the pair or the judgments disagree.
    pub dependence: Dependence,
    /// Latest attributed judgment per assessor.
    pub judgments: Vec<CurrentDependence>,
    pub disagreement: bool,
    /// Computed: dimensions on which the recorded values coincide.
    pub recorded_shared: Vec<OriginDimension>,
    /// Stated independent dimensions that a recorded value shows shared.
    pub conflicts: Vec<OriginDimension>,
    /// True only for an undisputed `independent_on_stated_dimensions`
    /// judgment with no conflict, between origins of different acquisitions.
    pub independent_corroboration: bool,
}

/// Claim-level origins of a set of cited Observations. Acquisitions and
/// known origins are counted separately; no combined score is computed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OriginSummary {
    pub observations: usize,
    /// Documents/acquisitions: distinct Sources the claims were collected through.
    pub acquisition_count: usize,
    pub acquisitions: Vec<Id>,
    /// Distinct origin roots the claims resolve to.
    pub known_origin_count: usize,
    /// Cited claims whose origin is unknown or disputed.
    pub unknown_origin_count: usize,
    pub origins: Vec<OriginGroup>,
    pub pairs: Vec<OriginPair>,
    pub omitted_pairs: usize,
    /// Pairs with `independent_corroboration`.
    pub independent_pairs: usize,
    /// Pairs whose dependence is `unknown`.
    pub unknown_pairs: usize,
}

type PairKey = (Id, Id);

fn pair_key(a: Id, b: Id) -> PairKey {
    if a <= b { (a, b) } else { (b, a) }
}

/// Latest dependence judgment per assessor for every pair of roots in the case.
fn dependence_index(state: &State, case: Id) -> BTreeMap<PairKey, Vec<CurrentDependence>> {
    let mut all: Vec<&DependenceJudgment> = Vec::new();
    let logs: Vec<ClaimOriginLog> = state
        .records()
        .filter(|record| {
            record.header.case_id == case
                && record.header.id.kind() == Kind::Observation
                && record.body.get(CLAIM_ORIGIN_KEY).is_some()
        })
        .filter_map(|record| origin_log(&record.body).ok())
        .collect();
    for log in &logs {
        all.extend(&log.dependence);
    }
    all.sort_by_key(|judgment| judgment.accepted_at_sequence);
    let mut latest: BTreeMap<PairKey, BTreeMap<Actor, CurrentDependence>> = BTreeMap::new();
    for judgment in all {
        for (index, first) in judgment.roots.iter().enumerate() {
            for second in &judgment.roots[index + 1..] {
                latest
                    .entry(pair_key(first.id, second.id))
                    .or_default()
                    .insert(
                        judgment.assessor.clone(),
                        CurrentDependence {
                            assessor: judgment.assessor.clone(),
                            judgment: judgment.judgment,
                            dimensions: judgment.dimensions.clone(),
                            basis: judgment.basis.clone(),
                            accepted_at_sequence: judgment.accepted_at_sequence,
                            roots: judgment.roots.clone(),
                        },
                    );
            }
        }
    }
    latest
        .into_iter()
        .map(|(key, judgments)| (key, judgments.into_values().collect()))
        .collect()
}

struct Group {
    state: OriginState,
    root: Option<RevisionRef>,
    observations: Vec<RevisionRef>,
    acquisitions: BTreeSet<Id>,
    recorded: Recorded,
}

fn pair(
    groups: &[Group],
    first: usize,
    second: usize,
    index: &BTreeMap<PairKey, Vec<CurrentDependence>>,
) -> OriginPair {
    let (a, b) = (&groups[first], &groups[second]);
    let judgments = match (a.root, b.root) {
        (Some(x), Some(y)) => index
            .get(&pair_key(x.id, y.id))
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let values: BTreeSet<Dependence> = judgments.iter().map(|item| item.judgment).collect();
    let disagreement = values.len() > 1;
    let dependence = match (values.len(), values.first()) {
        (1, Some(value)) => *value,
        _ => Dependence::Unknown,
    };
    let recorded_shared: Vec<OriginDimension> = a
        .recorded
        .iter()
        .filter(|(dimension, values)| {
            b.recorded
                .get(*dimension)
                .is_some_and(|other| !values.is_disjoint(other))
        })
        .map(|(dimension, _)| *dimension)
        .collect();
    let conflicts: Vec<OriginDimension> = judgments
        .iter()
        .filter(|item| item.judgment == Dependence::IndependentOnStatedDimensions)
        .flat_map(|item| item.dimensions.iter().copied())
        .filter(|dimension| recorded_shared.contains(dimension))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let independent_corroboration = dependence == Dependence::IndependentOnStatedDimensions
        && conflicts.is_empty()
        && !recorded_shared.contains(&OriginDimension::Acquisition);
    OriginPair {
        origins: [first, second],
        dependence,
        judgments,
        disagreement,
        recorded_shared,
        conflicts,
        independent_corroboration,
    }
}

/// Summarize the claim-level origins of the cited Observations. Origin data
/// is read from each Observation's current revision; other references are
/// ignored.
pub fn origin_summary(state: &State, case: Id, cited: &[RevisionRef]) -> OriginSummary {
    let mut seen = BTreeSet::new();
    let mut known: BTreeMap<Id, Group> = BTreeMap::new();
    let mut unknown: Vec<Group> = Vec::new();
    let mut all_acquisitions = BTreeSet::new();
    let mut observations = 0;
    let mut unknown_count = 0;
    for reference in cited {
        if reference.id.kind() != Kind::Observation || !seen.insert(reference.id) {
            continue;
        }
        observations += 1;
        let Some(current) = state
            .current(reference.id)
            .filter(|record| record.header.case_id == case)
        else {
            unknown_count += 1;
            unknown.push(Group {
                state: OriginState::Unknown,
                root: None,
                observations: vec![*reference],
                acquisitions: BTreeSet::new(),
                recorded: Recorded::new(),
            });
            continue;
        };
        let acquired = acquisitions(state, current);
        all_acquisitions.extend(acquired.iter().copied());
        let origin = effective_origin(state, current);
        if let (OriginState::Known, Some(root)) = (origin.state, origin.root) {
            let group = known.entry(root.id).or_insert_with(|| Group {
                state: OriginState::Known,
                root: Some(root),
                observations: Vec::new(),
                acquisitions: BTreeSet::new(),
                recorded: state
                    .current(root.id)
                    .map(|record| root_dimensions(state, record))
                    .unwrap_or_default(),
            });
            group.observations.push(*reference);
            group.acquisitions.extend(acquired);
            continue;
        }
        unknown_count += 1;
        let mut merged = Group {
            state: origin.state,
            root: None,
            observations: vec![*reference],
            acquisitions: acquired,
            recorded: Recorded::new(),
        };
        record_run(&mut merged.recorded, current);
        // Claims of unknown origin that share an acquisition are one group:
        // views and notes of one document are not separate origins.
        let mut index = 0;
        while index < unknown.len() {
            if merged
                .acquisitions
                .is_disjoint(&unknown[index].acquisitions)
            {
                index += 1;
                continue;
            }
            let other = unknown.remove(index);
            let mut observations = other.observations;
            observations.append(&mut merged.observations);
            merged.observations = observations;
            merged.acquisitions.extend(other.acquisitions);
            for (dimension, values) in other.recorded {
                merged.recorded.entry(dimension).or_default().extend(values);
            }
            merged.state = merged.state.max(other.state);
        }
        unknown.push(merged);
    }
    for group in &mut unknown {
        if !group.acquisitions.is_empty() {
            group.recorded.insert(
                OriginDimension::Acquisition,
                group.acquisitions.iter().map(Id::to_string).collect(),
            );
        }
    }
    let groups: Vec<Group> = known.into_values().chain(unknown).collect();
    let dependence = dependence_index(state, case);
    let mut pairs = Vec::new();
    let (mut independent, mut unknown_pairs, mut total) = (0, 0, 0);
    for first in 0..groups.len() {
        for second in first + 1..groups.len() {
            let pair = pair(&groups, first, second, &dependence);
            total += 1;
            independent += usize::from(pair.independent_corroboration);
            unknown_pairs += usize::from(pair.dependence == Dependence::Unknown);
            if pairs.len() < MAX_LISTED_PAIRS {
                pairs.push(pair);
            }
        }
    }
    let known_origin_count = groups
        .iter()
        .filter(|group| group.state == OriginState::Known)
        .count();
    OriginSummary {
        observations,
        acquisition_count: all_acquisitions.len(),
        acquisitions: all_acquisitions.into_iter().collect(),
        known_origin_count,
        unknown_origin_count: unknown_count,
        origins: groups
            .into_iter()
            .enumerate()
            .map(|(index, group)| OriginGroup {
                index,
                state: group.state,
                root: group.root,
                observations: group.observations,
                acquisitions: group.acquisitions.into_iter().collect(),
                recorded: listed(&group.recorded),
            })
            .collect(),
        omitted_pairs: total - pairs.len(),
        pairs,
        independent_pairs: independent,
        unknown_pairs,
    }
}

/// Origin summaries for each evidence field of a citing record that cites
/// Observations, in field order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FieldOrigins {
    pub field: String,
    pub summary: OriginSummary,
}

pub fn cited_origins(state: &State, record: &Record) -> Vec<FieldOrigins> {
    ["evidence_refs", "support_refs", "contradiction_refs"]
        .into_iter()
        .filter_map(|field| {
            let refs: Vec<RevisionRef> = record
                .body
                .get(field)
                .and_then(Value::as_array)?
                .iter()
                .filter_map(|item| input::decode::<RevisionRef>(item).ok())
                .filter(|reference| reference.id.kind() == Kind::Observation)
                .collect();
            (!refs.is_empty()).then(|| FieldOrigins {
                field: field.to_owned(),
                summary: origin_summary(state, record.header.case_id, &refs),
            })
        })
        .collect()
}

/// Origin of one Observation's claim, as shown wherever it is read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObservationOrigin {
    pub origin: EffectiveOrigin,
    /// Acquisitions (Sources) this claim was collected through. Separate from
    /// its origin.
    pub acquisitions: Vec<Id>,
    /// Latest trace per assessor; traces may disagree.
    pub traces: Vec<OriginTrace>,
    pub traces_kept: usize,
    /// Current dependence judgments that link this claim's origin root.
    pub dependence: Vec<CurrentDependence>,
    pub omitted_dependence: usize,
}

pub fn observation_origin(state: &State, record: &Record) -> Result<ObservationOrigin, Failure> {
    let log = origin_log(&record.body)?;
    let origin = effective_origin(state, record);
    let mut dependence: Vec<CurrentDependence> = Vec::new();
    if let Some(root) = origin.root {
        let mut seen = BTreeSet::new();
        for ((first, second), judgments) in dependence_index(state, record.header.case_id) {
            if first != root.id && second != root.id {
                continue;
            }
            for judgment in judgments {
                if seen.insert((judgment.assessor.clone(), judgment.accepted_at_sequence)) {
                    dependence.push(judgment);
                }
            }
        }
    }
    dependence.sort_by_key(|judgment| judgment.accepted_at_sequence);
    let omitted = dependence.len().saturating_sub(MAX_LISTED_JUDGMENTS);
    dependence.truncate(MAX_LISTED_JUDGMENTS);
    Ok(ObservationOrigin {
        acquisitions: acquisitions(state, record).into_iter().collect(),
        traces: current_traces(&log).into_iter().cloned().collect(),
        traces_kept: log.traces.len(),
        origin,
        dependence,
        omitted_dependence: omitted,
    })
}

#[cfg(test)]
mod tests;
