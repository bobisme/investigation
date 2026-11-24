//! Withdrawal of one Artifact reference's availability (plan §16.5; W11.11).
//!
//! Blobs deduplicate across cases, so retention applies to a reference, never
//! to a digest. `artifact.expire` records a new revision of one Artifact with
//! `availability: intentionally_removed` and an attributed
//! `availability_change`. The digest, size, provenance and every earlier
//! revision stay; nothing is deleted. From then on every read of bytes through
//! that Artifact is refused, also at a historical snapshot: a read follows the
//! *current* availability of the reference ([`bytes_withdrawn`]). Another
//! Artifact that records the same digest, in this case or another, keeps its
//! own availability, and deduplication never lets one case read another's
//! reference.
//!
//! Removing the bytes themselves is a separate, later step (W11.19). It may
//! only happen once no live obligation holds the digest; [`preview`] and the
//! store's retention obligations say which obligations those are.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    catalog::{OperationDescriptor, OperationInfo, standard_catalog},
    command::{Command, RevisionRef},
    evidence::{ArtifactAvailability, ArtifactBody},
    freshness::{Freshness, FreshnessIndex, Trigger},
    guardrails::SessionEvidence,
    header::{Actor, SystemClock},
    identity::{Id, Kind, SystemIdSource},
    inherited_trust::Session,
    input, method_pack_ops,
    pack_policy::PackSource,
    state::{Acceptance, OperationContext, Record, State, validate_and_apply_guarded},
    wire::{Digest, ErrorCode, Failure},
};

/// Artifact body key that holds the attributed availability change.
pub const AVAILABILITY_CHANGE_KEY: &str = "availability_change";
/// The operation that withdraws a reference's availability.
pub const OPERATION: &str = "artifact.expire";

const TEXT_LIMIT: usize = 2048;

/// Why a reference's availability is withdrawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExpiryReason {
    /// The case's retention period for these bytes has ended.
    RetentionPeriodEnded,
    /// The bytes must stop being readable here, for example a secret or personal data.
    SensitiveContent,
    /// The owner or originator of the material asked for its removal.
    OwnerRequest,
    /// The bytes were imported by mistake.
    CollectedInError,
    Other,
}

/// The attributed withdrawal of one reference's availability. The actor is
/// the command actor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityChange {
    pub actor: Actor,
    pub accepted_at_sequence: u64,
    /// The Artifact revision whose availability was withdrawn.
    pub revision: u64,
    /// The availability before the change. Always `retained`.
    pub from: ArtifactAvailability,
    pub reason: ExpiryReason,
    pub rationale: String,
    /// The write-side guardrails the change satisfied, when a pinned pack
    /// declares any for `artifact.expire`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardrail: Option<crate::guardrails::GuardrailRecord>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArtifactExpire {
    /// Exact current Artifact revision whose availability is withdrawn.
    artifact_ref: RevisionRef,
    reason: ExpiryReason,
    rationale: String,
}

pub(crate) fn operations() -> [OperationDescriptor; 1] {
    [OperationDescriptor::active(
        OperationInfo {
            name: OPERATION,
            stage: "observe",
            summary: "Withdraw one Artifact reference's availability: its bytes can no longer be read through it. The record and its history stay.",
            when_to_use: "A retention period ends, or retained bytes must stop being readable in this case. Run investigation retention preview first.",
            lifecycle: "A current Artifact revision whose bytes are retained and that no adopted method pack reads.",
            effects: concat!(
                "Record a new Artifact revision with availability intentionally_removed and an attributed availability_change. ",
                "Digest, size, provenance and earlier revisions stay unchanged. ",
                "Every read of bytes through this Artifact, at any snapshot, is refused from then on, and dependent records become needs_review (availability_removed). ",
                "Other Artifacts with the same digest, in this case or another, keep their own availability. ",
                "Nothing is deleted: the stored bytes stay until a separate reclamation finds no remaining obligation."
            ),
            returns: "The Artifact ID and its new revision.",
            revision_rule: "Supply the exact current artifact_ref.",
            idempotency: "Retry the identical command with its original actor and command ID.",
            repair: "Read the current Artifact and cite its exact revision. Only retained bytes can be withdrawn; an adopted method pack's bytes cannot.",
            related: &["artifact.withdraw", "source.withdraw"],
        },
        json!({
            "artifact_ref": {"id": "art_019acd16-0000-7000-8000-000000000001", "revision": 1},
            "reason": "retention_period_ended",
            "rationale": "The case keeps raw captures for 90 days."
        }),
        validate,
        expire,
    )]
}

fn validate(payload: &ArtifactExpire) -> Result<(), Failure> {
    if payload.artifact_ref.id.kind() != Kind::Artifact {
        return Err(
            Failure::new(ErrorCode::InvalidInput, "Cite an Artifact.").at("/artifact_ref/id")
        );
    }
    let rationale = payload.rationale.trim();
    if rationale.is_empty() || payload.rationale.len() > TEXT_LIMIT {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            format!("Give a nonempty rationale of at most {TEXT_LIMIT} bytes."),
        )
        .at("/rationale"));
    }
    Ok(())
}

fn gate(message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::GateFailed, message).at("/artifact_ref")
}

fn expire(payload: ArtifactExpire, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let target = crate::source_ops::exact_current(ctx, payload.artifact_ref, "/artifact_ref")?;
    let body: ArtifactBody = input::decode(&target.body).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "The stored Artifact record is invalid.",
        )
    })?;
    if body.availability != ArtifactAvailability::Retained {
        let mut failure = gate(
            "Only retained bytes can be withdrawn. Nothing is read through this reference, so there is no availability to withdraw.",
        );
        failure.error.actual = Some(json!({"availability": body.availability}));
        return Err(failure);
    }
    let case = target.header.case_id;
    let readers: Vec<_> = method_pack_ops::adopted_packs(ctx.state(), case, None)?
        .into_iter()
        .filter(|pin| pin.artifact_ref.id == payload.artifact_ref.id)
        .map(|pin| json!({"pack_id": pin.pack_id, "version": pin.version}))
        .collect();
    if !readers.is_empty() {
        let mut failure = gate(
            "An adopted method pack reads these bytes, and its policy would silently stop applying. Adopt another version of the pack first.",
        );
        failure.error.actual = Some(json!({"method_packs": readers}));
        return Err(failure);
    }
    let target = target.clone();
    let guardrail = crate::guardrails::enforce(
        ctx,
        crate::guardrails::GuardedOperation::ArtifactExpire,
        &target,
    )?;
    let change = AvailabilityChange {
        actor: ctx.actor().clone(),
        accepted_at_sequence: ctx.acceptance_sequence(),
        revision: payload.artifact_ref.revision,
        from: body.availability,
        reason: payload.reason,
        rationale: payload.rationale,
        guardrail,
    };
    let mut body = target.body.clone();
    let object = body.as_object_mut().ok_or_else(|| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "The stored Artifact record is invalid.",
        )
    })?;
    object.insert(
        "availability".into(),
        json!(ArtifactAvailability::IntentionallyRemoved),
    );
    object.insert(AVAILABILITY_CHANGE_KEY.into(), json!(change));
    ctx.update(payload.artifact_ref, body)?;
    Ok(())
}

/// Whether the current revision of `id` in `state` is an Artifact whose
/// availability was withdrawn.
pub fn availability_withdrawn(state: &State, id: Id) -> bool {
    id.kind() == Kind::Artifact
        && state
            .current(id)
            .is_some_and(|record| record.body["availability"] == "intentionally_removed")
}

/// Whether bytes may no longer be read through Artifact `id`. A read at a
/// historical snapshot (`state`) follows the current availability in
/// `today`, so a withdrawal recorded after the snapshot applies to it too.
/// Every reader of retained bytes asks this before opening a blob.
pub fn bytes_withdrawn(state: &State, today: Option<&State>, id: Id) -> bool {
    availability_withdrawn(state, id)
        || today.is_some_and(|today| availability_withdrawn(today, id))
}

/// The attributed availability change recorded on an Artifact revision.
pub fn availability_change(record: &Record) -> Option<AvailabilityChange> {
    record
        .body
        .get(AVAILABILITY_CHANGE_KEY)
        .and_then(|value| input::decode(value).ok())
}

/// What withdrawing one reference would do, computed before anything changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReferenceLoss {
    /// The current revision the withdrawal would cite.
    pub artifact_ref: RevisionRef,
    pub digest: Option<Digest>,
    pub size_bytes: Option<u64>,
    /// The availability now.
    pub availability: ArtifactAvailability,
    pub outcome: LossOutcome,
    /// For a refused withdrawal: the commit's own error code and message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
    /// Records whose freshness the withdrawal changes, in ID order: each
    /// becomes `needs_review` with an `availability_removed` reason.
    pub dependents: Vec<DependentLoss>,
    /// Artifacts derived from these bytes. Each holds its own bytes under its
    /// own digest and keeps its own availability: a copy downstream.
    pub derivatives: Vec<RevisionRef>,
    /// Observations citing this Artifact or a derivative of it whose recorded
    /// quote copies part of the bytes into their own record text, which a
    /// withdrawal leaves in place.
    pub quoting_observations: Vec<RevisionRef>,
    /// Warnings the commit would give, such as a pinned pack whose bytes are
    /// unavailable so its guardrails are not applied. Only a preview with
    /// pack bytes reports them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LossOutcome {
    /// A withdrawal would be accepted: reads through this reference stop.
    BecomesUnavailable,
    /// Nothing is read through this reference already.
    AlreadyUnavailable,
    /// The commit would refuse the withdrawal.
    Refused,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Refusal {
    pub code: ErrorCode,
    pub message: String,
    /// The commit's own `expected`: for a guardrail refusal, the
    /// `guardrail_blockers`, each with what is unmet and its repair.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
}

/// The write a preview simulates: the actor and declared session the real
/// `artifact.expire` would carry, the case's retained method pack bytes and
/// the session evidence the store would supply. With them the simulation
/// runs the same write-side guardrails as the commit, so a guardrail refusal
/// is predicted with the commit's code and repair (bn-r6ig). The preview
/// cannot know a human confirmation the real command may carry; it simulates
/// a command without one.
pub struct PreviewWriter<'a> {
    pub actor: Actor,
    pub session: Option<Session>,
    pub packs: Option<&'a dyn PackSource>,
    pub sessions: Option<&'a SessionEvidence>,
}

impl PreviewWriter<'_> {
    /// A writer with no pack bytes: pinned packs resolve as unavailable and
    /// supply no guardrail, as before bn-r6ig. For callers that need only the
    /// availability and dependents.
    pub fn unguarded() -> Self {
        Self {
            actor: "agent:retention-preview".parse().expect("a valid actor"),
            session: None,
            packs: None,
            sessions: None,
        }
    }
}

/// A pack source with no bytes.
struct NoPacks;

impl PackSource for NoPacks {
    fn pack_bytes(&self, _: &Digest) -> Option<Vec<u8>> {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DependentLoss {
    pub record: RevisionRef,
    pub freshness_before: Freshness,
    pub freshness_after: Freshness,
}

/// The logical loss of withdrawing each of `artifacts` in `case`, one at a
/// time, from the verified `state`. Each withdrawal is checked by the
/// operation itself against a copy of the state, so a refusal here is the
/// commit's refusal and the dependents are those the freshness index then
/// reports. The index is built once, for `state`, and updated for each
/// withdrawal. Nothing is written. Unknown IDs are the caller's to report.
pub fn preview(state: &State, case: Id, artifacts: &[Id]) -> Vec<ReferenceLoss> {
    preview_as(state, case, artifacts, &PreviewWriter::unguarded())
}

/// [`preview`] as `writer`: each simulated withdrawal is the command that
/// writer would commit, so a pinned pack's `artifact.expire` guardrail
/// refuses it here exactly when it would refuse the commit.
pub fn preview_as(
    state: &State,
    case: Id,
    artifacts: &[Id],
    writer: &PreviewWriter<'_>,
) -> Vec<ReferenceLoss> {
    let before = FreshnessIndex::build(state, case);
    artifacts
        .iter()
        .filter_map(|id| state.current(*id).filter(|r| r.header.case_id == case))
        .map(|record| loss(state, &before, case, record, writer))
        .collect()
}

fn loss(
    state: &State,
    before: &FreshnessIndex,
    case: Id,
    record: &Record,
    writer: &PreviewWriter<'_>,
) -> ReferenceLoss {
    let body: Option<ArtifactBody> = input::decode(&record.body).ok();
    let id = record.header.id;
    let mut loss = ReferenceLoss {
        artifact_ref: record.reference(),
        digest: body.as_ref().and_then(|body| body.digest.clone()),
        size_bytes: body.as_ref().and_then(|body| body.size_bytes),
        availability: body
            .as_ref()
            .map_or(ArtifactAvailability::Missing, |body| body.availability),
        outcome: LossOutcome::BecomesUnavailable,
        refusal: None,
        dependents: Vec::new(),
        derivatives: Vec::new(),
        quoting_observations: Vec::new(),
        warnings: Vec::new(),
    };
    loss.derivatives = derivatives(state, case, id);
    let copies: BTreeSet<Id> = std::iter::once(id)
        .chain(loss.derivatives.iter().map(|derivative| derivative.id))
        .collect();
    loss.quoting_observations = quoting(state, case, &copies);
    if loss.availability != ArtifactAvailability::Retained {
        loss.outcome = LossOutcome::AlreadyUnavailable;
        return loss;
    }
    match simulate(state, case, record.reference(), writer) {
        Ok((after, warnings)) => {
            if writer.packs.is_some() {
                loss.warnings = warnings;
            }
            // The index of `state`, brought forward to the one withdrawal:
            // only records it can affect are reassessed, and the result
            // equals a full build of `after` (bn-5h3z).
            let mut index = before.clone();
            if index.update(&after).is_err() {
                index = FreshnessIndex::build(&after, case);
            }
            let mut dependents: Vec<DependentLoss> = index
                .records()
                .filter(|result| {
                    result.reasons.iter().any(|reason| {
                        reason.trigger == Trigger::AvailabilityRemoved && reason.cause.id == id
                    })
                })
                .map(|result| DependentLoss {
                    record: result.record,
                    freshness_before: before
                        .get(result.record.id)
                        .map_or(Freshness::Current, |prior| prior.freshness),
                    freshness_after: result.freshness,
                })
                .collect();
            dependents.sort_by_key(|dependent| dependent.record.id);
            loss.dependents = dependents;
        }
        Err(failure) => {
            loss.outcome = LossOutcome::Refused;
            loss.refusal = Some(Refusal {
                code: failure.error.code,
                message: failure.error.message,
                expected: failure.error.expected,
            });
        }
    }
    loss
}

/// The state one accepted `artifact.expire` of `reference` would leave.
fn simulate(
    state: &State,
    case: Id,
    reference: RevisionRef,
    writer: &PreviewWriter<'_>,
) -> Result<(State, Vec<String>), Failure> {
    let sequence = state.sequence() + 1;
    let mut command = json!({
        "schema_version": "inv.command.v1",
        "command_id": "retention-preview",
        "case_id": case,
        "actor": writer.actor,
        "preconditions": [],
        "operations": [{"op": OPERATION, "payload": {
            "artifact_ref": reference,
            "reason": "other",
            "rationale": "Preview only; nothing is committed.",
        }}],
    });
    if let Some(session) = &writer.session {
        command["session"] = json!(session);
    }
    let command: Command = input::decode(&command)
        .map_err(|error| Failure::new(ErrorCode::IntegrityFailure, error.message))?;
    let accepted = validate_and_apply_guarded(
        state,
        &command,
        &standard_catalog(),
        Acceptance {
            sequence,
            clock: &mut SystemClock,
            ids: &mut SystemIdSource,
        },
        &std::collections::BTreeMap::new(),
        writer.packs.unwrap_or(&NoPacks),
        writer.sessions,
    )?;
    let mut after = state.clone();
    after.apply(&accepted)?;
    Ok((after, accepted.warnings))
}

/// Current Artifacts in `case` derived directly or transitively from `id`.
fn derivatives(state: &State, case: Id, id: Id) -> Vec<RevisionRef> {
    let mut found = BTreeSet::from([id]);
    let mut out = Vec::new();
    loop {
        let next: Vec<&Record> = state
            .records_of_kind(Kind::Artifact)
            .filter(|record| record.header.case_id == case && !found.contains(&record.header.id))
            .filter(|record| {
                record.body["transforms"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|transform| input::decode::<RevisionRef>(&transform["input"]).ok())
                    .any(|input| found.contains(&input.id))
            })
            .collect();
        if next.is_empty() {
            out.sort();
            return out;
        }
        for record in next {
            found.insert(record.header.id);
            out.push(record.reference());
        }
    }
}

/// Current Observations in `case` citing one of `artifacts` with recorded
/// quote text.
fn quoting(state: &State, case: Id, artifacts: &BTreeSet<Id>) -> Vec<RevisionRef> {
    let mut out: Vec<RevisionRef> = state
        .records_of_kind(Kind::Observation)
        .filter(|record| record.header.case_id == case)
        .filter(|record| {
            input::decode::<Id>(&record.body["basis"]["artifact_ref"]["id"])
                .is_ok_and(|cited| artifacts.contains(&cited))
        })
        .filter(|record| {
            record.body["basis"]["text_quote"]["exact"]
                .as_str()
                .is_some_and(|exact| !exact.is_empty())
        })
        .map(Record::reference)
        .collect();
    out.sort();
    out
}
