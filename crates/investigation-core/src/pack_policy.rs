//! Pack policy inputs at promotion gates (§2.4, §6.4, §8.5, §16.1).
//!
//! A case that pins a method pack may tighten its promotion and closure gates
//! through the closed set of [`PolicyInput`]s the pack declares. Policy only
//! ever adds a requirement at one of the five [`Gate`]s; it is consulted from
//! those handlers and nowhere else, so recording, ingest and capture can never
//! be refused for pack policy. Built-in gates run first and unchanged.
//!
//! Only packs pinned to the case and resolved from their retained bytes at the
//! pinned digest supply policy. The built-in pack never does, so the core has
//! no default policy. A pinned pack whose bytes are missing or unusable
//! supplies none either, but the gate records it as unavailable and warns, so
//! a gate can never pass silently without the policy the case adopted.
//!
//! Every gate stores a [`GatePolicy`] on the gated record's new revision:
//! which pinned packs it consulted (ID, version, digest, availability) and
//! which rules it applied, with the records that satisfied them. A later
//! adoption therefore never rewrites what an earlier gate required.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    basis::basis_digest,
    command::RevisionRef,
    guidance::TriggerContext,
    header::Actor,
    identity::{Id, Kind},
    method_pack::{PolicyInput, PolicyProposal},
    method_pack_ops::{PackStatus, ResolvedPack, UnavailableReason, adopted_packs, resolve_pin},
    state::{OperationContext, Record, State},
    wire::{Digest, ErrorCode, Failure},
};

/// Version of the policy evaluation rules in this module.
pub const POLICY_RULE_VERSION: &str = "pack-policy.v1";

/// Retained method pack bytes by content digest. Implementations read only
/// content-addressed retained blobs, never the path a pack was ingested from.
pub trait PackSource {
    /// The retained bytes for `digest`, or `None` when they cannot be read.
    fn pack_bytes(&self, digest: &Digest) -> Option<Vec<u8>>;
}

/// The closed set of operations pack policy can tighten. None records evidence.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum Gate {
    #[serde(rename = "conclusion.request_review")]
    ConclusionRequestReview,
    #[serde(rename = "conclusion.validate")]
    ConclusionValidate,
    #[serde(rename = "decision.propose")]
    DecisionPropose,
    #[serde(rename = "decision.accept")]
    DecisionAccept,
    #[serde(rename = "case.close")]
    CaseClose,
}

impl Gate {
    pub const ALL: [Self; 5] = [
        Self::ConclusionRequestReview,
        Self::ConclusionValidate,
        Self::DecisionPropose,
        Self::DecisionAccept,
        Self::CaseClose,
    ];

    pub const fn operation(self) -> &'static str {
        match self {
            Self::ConclusionRequestReview => "conclusion.request_review",
            Self::ConclusionValidate => "conclusion.validate",
            Self::DecisionPropose => "decision.propose",
            Self::DecisionAccept => "decision.accept",
            Self::CaseClose => "case.close",
        }
    }
}

/// One pinned pack as a gate consulted it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConsultedPack {
    pub pack_id: String,
    pub version: String,
    pub digest: Digest,
    pub status: PackStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<UnavailableReason>,
}

/// One pack rule a gate applied, with what satisfied it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppliedRule {
    pub pack_id: String,
    pub version: String,
    pub digest: Digest,
    /// The policy's name inside its pack.
    pub policy: String,
    pub input: PolicyInput,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// `blindspot_review`: the current reviews that satisfied the rule.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub satisfied_by: Vec<RevisionRef>,
    /// `closure_retrospective`: whether the declared triggers held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggered: Option<bool>,
}

/// The pack policy in force at one gate, stored with the gated revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GatePolicy {
    pub gate: Gate,
    pub rule_version: String,
    /// The acceptance sequence of the gate transaction. A later revision that
    /// copies the body forward keeps this value, so it names its own gate.
    pub sequence: u64,
    /// Every pack pinned to the case at the gate, in adoption order.
    pub packs: Vec<ConsultedPack>,
    pub rules: Vec<AppliedRule>,
}

/// The gated record as a rule sees it.
pub struct Subject<'a> {
    /// The record body the gate will store (its after-image).
    pub body: &'a Value,
    /// The current revision before the gate, when the record already exists.
    pub record: Option<&'a Record>,
}

/// The pinned packs resolved for one gate, and the rules they declare there.
#[derive(Clone, Debug)]
pub struct Consulted {
    gate: Gate,
    case: Id,
    sequence: u64,
    packs: Vec<ResolvedPack>,
}

/// Resolve the case's current pins (including any adopted earlier in the same
/// transaction) from retained bytes for `gate`.
pub fn consult(ctx: &OperationContext<'_>, gate: Gate) -> Result<Consulted, Failure> {
    let case = ctx.case_id().ok_or_else(integrity)?;
    let source = ctx.pack_source();
    let packs = adopted_packs(ctx.state(), case, None)?
        .iter()
        .map(|pin| {
            let bytes = source.and_then(|source| source.pack_bytes(&pin.digest));
            resolve_pin(pin, bytes.as_deref())
        })
        .collect();
    Ok(Consulted {
        gate,
        case,
        sequence: ctx.acceptance_sequence(),
        packs,
    })
}

impl Consulted {
    /// Rules declared for this gate by available pinned packs, in adoption
    /// order, then file order.
    pub fn rules(&self) -> impl Iterator<Item = (&ResolvedPack, &PolicyProposal)> {
        self.packs
            .iter()
            .filter(|pack| pack.is_available())
            .filter_map(|pack| pack.loaded.as_ref().map(|loaded| (pack, loaded)))
            .flat_map(move |(pack, loaded)| {
                loaded
                    .pack()
                    .policies
                    .iter()
                    .filter(move |policy| policy.gate == self.gate.operation())
                    .map(move |policy| (pack, policy))
            })
    }

    pub fn requires(&self, input: PolicyInput) -> bool {
        self.rules().any(|(_, policy)| policy.input == input)
    }

    /// Receipt warnings for pinned packs that could not supply policy.
    pub fn warnings(&self) -> Vec<String> {
        self.packs
            .iter()
            .filter(|pack| !pack.is_available())
            .map(|pack| {
                format!(
                    "Pinned method pack {}@{} ({}) is unavailable; its policy was not applied at {}.",
                    pack.pack_id,
                    pack.version,
                    pack.digest,
                    self.gate.operation()
                )
            })
            .collect()
    }

    /// One blocker per `closure_retrospective` rule whose triggers hold in
    /// `state`: each makes the case material, so closing needs a current
    /// Retrospective rather than a waiver.
    pub fn closure_blockers(&self, state: &State) -> Vec<Value> {
        let context = TriggerContext::evaluate_recorded(state, self.case);
        self.rules()
            .filter(|(_, policy)| {
                policy.input == PolicyInput::ClosureRetrospective && triggers_hold(&context, policy)
            })
            .map(|(pack, policy)| {
                let held: Vec<_> = policy
                    .triggers
                    .iter()
                    .filter(|trigger| context.holds(trigger))
                    .collect();
                self.blocker(
                    pack,
                    policy,
                    json!({"missing": "retrospective", "triggers": held}),
                )
            })
            .collect()
    }

    /// One machine-readable blocker: the pack and rule, what is unmet, and
    /// the operation whose schema shows how to satisfy it, when there is one.
    fn blocker(&self, pack: &ResolvedPack, policy: &PolicyProposal, unmet: Value) -> Value {
        let repair = match policy.input {
            PolicyInput::BlindspotReview => Some("review.submit"),
            PolicyInput::ClosureRetrospective => Some("retrospective.record"),
            PolicyInput::RequiredFields
            | PolicyInput::DistinctActor
            | PolicyInput::AcknowledgeOpenObjections => None,
        };
        let mut blocker = json!({
            "pack_id": pack.pack_id, "version": pack.version, "digest": pack.digest,
            "policy": policy.name, "input": policy.input, "gate": self.gate,
            "rationale": policy.rationale, "unmet": unmet,
        });
        if let Some(schema) = repair {
            blocker["repair_schema"] = json!(schema);
        }
        blocker
    }

    /// Check every rule for this gate against `subject` and return the record
    /// to store with the gated revision. Unmet rules fail with one
    /// machine-readable blocker per rule, naming its pack and policy.
    pub fn enforce(
        &self,
        state: &State,
        actor: &Actor,
        subject: &Subject<'_>,
    ) -> Result<GatePolicy, Failure> {
        let mut applied = Vec::new();
        let mut blockers = Vec::new();
        let closure = (self.gate == Gate::CaseClose)
            .then(|| TriggerContext::evaluate_recorded(state, self.case));
        for (pack, policy) in self.rules() {
            let mut rule = AppliedRule {
                pack_id: pack.pack_id.clone(),
                version: pack.version.clone(),
                digest: pack.digest.clone(),
                policy: policy.name.clone(),
                input: policy.input,
                fields: Vec::new(),
                satisfied_by: Vec::new(),
                triggered: None,
            };
            let unmet = match policy.input {
                PolicyInput::RequiredFields => {
                    rule.fields = policy.fields.clone();
                    let missing: Vec<&String> = policy
                        .fields
                        .iter()
                        .filter(|field| !field_present(subject, field))
                        .collect();
                    (!missing.is_empty()).then(|| json!({"missing_fields": missing}))
                }
                PolicyInput::BlindspotReview => {
                    let record = subject.record.ok_or_else(integrity)?;
                    rule.satisfied_by = current_blindspot_reviews(state, record)?;
                    rule.satisfied_by.is_empty().then(|| {
                        json!({"missing": "current_blindspot_review", "subject_ref": record.reference()})
                    })
                }
                PolicyInput::DistinctActor => {
                    let record = subject.record.ok_or_else(integrity)?;
                    let authors = content_authors(state, record)?;
                    authors.contains(actor).then(
                        || json!({"missing": "distinct_actor", "actor": actor, "authors": authors}),
                    )
                }
                PolicyInput::ClosureRetrospective => {
                    // Enforced by the closure gate as materiality; recorded here.
                    rule.triggered = closure
                        .as_ref()
                        .map(|context| triggers_hold(context, policy));
                    None
                }
                // Enforced by the objection acknowledgment check.
                PolicyInput::AcknowledgeOpenObjections => None,
            };
            match unmet {
                Some(unmet) => blockers.push(self.blocker(pack, policy, unmet)),
                None => applied.push(rule),
            }
        }
        if !blockers.is_empty() {
            let mut failure = Failure::new(
                ErrorCode::GateFailed,
                format!(
                    "A pinned method pack policy adds requirements to {} that are not met.",
                    self.gate.operation()
                ),
            );
            failure.error.expected = Some(json!({"policy_blockers": blockers}));
            return Err(failure);
        }
        Ok(GatePolicy {
            gate: self.gate,
            rule_version: POLICY_RULE_VERSION.to_owned(),
            sequence: self.sequence,
            packs: self
                .packs
                .iter()
                .map(|pack| ConsultedPack {
                    pack_id: pack.pack_id.clone(),
                    version: pack.version.clone(),
                    digest: pack.digest.clone(),
                    status: pack.status,
                    unavailable_reason: pack.unavailable_reason,
                })
                .collect(),
            rules: applied,
        })
    }

    /// Re-check the rules that depend on case state against the final
    /// transaction state, so a later operation cannot undo what the gate saw.
    pub fn recheck(&self, state: &State, actor: &Actor, id: Id) -> Result<(), Failure> {
        let record = state.current(id).ok_or_else(integrity)?;
        self.enforce(
            state,
            actor,
            &Subject {
                body: &record.body,
                record: Some(record),
            },
        )
        .map(|_| ())
    }
}

/// An empty trigger list always holds; otherwise any one trigger suffices.
fn triggers_hold(context: &TriggerContext, policy: &PolicyProposal) -> bool {
    policy.triggers.is_empty() || policy.triggers.iter().any(|trigger| context.holds(trigger))
}

/// Encode a gate policy for storage in an untyped record body.
pub fn encode(policy: &GatePolicy) -> Result<Value, Failure> {
    serde_json::to_value(policy).map_err(|_| integrity())
}

fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "Invalid stored state for a policy gate.",
    )
}

/// A required field is present when every dotted segment resolves and the
/// value is not null, blank text, an empty list or object, or an explicit
/// `unknown`. Paths under `extensions` fall back to the record header's
/// extension map when the body has none.
fn field_present(subject: &Subject<'_>, path: &str) -> bool {
    let mut segments = path.split('.');
    let Some(first) = segments.next() else {
        return false;
    };
    let header_extensions = subject
        .record
        .filter(|record| !record.header.extensions.is_empty())
        .map(|record| {
            Value::Object(
                record
                    .header
                    .extensions
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )
        });
    let mut value = match (subject.body.get(first), first, &header_extensions) {
        (Some(value), _, _) => value,
        (None, "extensions", Some(extensions)) => extensions,
        _ => return false,
    };
    for segment in segments {
        match value.get(segment) {
            Some(next) => value = next,
            None => return false,
        }
    }
    meaningful(value)
}

fn meaningful(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => match (map.get("state"), map.get("value"), map.len()) {
            (Some(state), None, 1) if state == "unknown" => false,
            (Some(state), Some(inner), 2) if state == "known" => meaningful(inner),
            _ => !map.is_empty(),
        },
        Value::Bool(_) | Value::Number(_) => true,
    }
}

/// Submitted blindspot reviews of `subject` that are still current: they
/// reviewed a revision with the same substantive basis, and every revision in
/// their read-set is still the current one (the `review.stale_read_set` rule).
pub fn current_blindspot_reviews(
    state: &State,
    subject: &Record,
) -> Result<Vec<RevisionRef>, Failure> {
    let digest = basis_digest(subject)?;
    let mut found = Vec::new();
    for review in state.records().filter(|record| {
        record.header.case_id == subject.header.case_id
            && record.header.id.kind() == Kind::Review
            && record.body["kind"] == "blindspot"
            && record.body["status"] == "submitted"
    }) {
        let Ok(reviewed) =
            serde_json::from_value::<RevisionRef>(review.body["subject_ref"].clone())
        else {
            continue;
        };
        if reviewed.id != subject.header.id {
            continue;
        }
        let Some(reviewed) = state.revision(reviewed) else {
            continue;
        };
        if basis_digest(reviewed)? != digest {
            continue;
        }
        let read_set: Vec<RevisionRef> =
            serde_json::from_value(review.body["read_set"].clone()).map_err(|_| integrity())?;
        if read_set.iter().all(|reference| {
            state
                .current(reference.id)
                .is_some_and(|current| current.reference() == *reference)
        }) {
            found.push(review.reference());
        }
    }
    found.sort_by_key(|reference| reference.id);
    Ok(found)
}

/// Actors who recorded the subject's current substantive content: the creator
/// and whoever recorded a revision that changed its basis digest.
pub fn content_authors(state: &State, subject: &Record) -> Result<BTreeSet<Actor>, Failure> {
    let mut authors = BTreeSet::new();
    let mut previous = None;
    for revision in state
        .history(subject.header.id)
        .take_while(|revision| revision.header.revision <= subject.header.revision)
    {
        let digest = basis_digest(revision)?;
        if previous.as_ref() != Some(&digest) {
            authors.insert(revision.header.recorded_by.clone());
            if revision.header.revision.0 == 1 {
                authors.insert(revision.header.created_by.clone());
            }
        }
        previous = Some(digest);
    }
    Ok(authors)
}

/// Gates in fixed order, by operation name. Used by tests and the catalog check.
pub fn gate_for(operation: &str) -> Option<Gate> {
    Gate::ALL
        .into_iter()
        .find(|gate| gate.operation() == operation)
}

#[cfg(test)]
mod tests;
