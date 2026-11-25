//! Opt-in write-side guardrails (§16.1).
//!
//! The worst injection against an epistemic ledger is a schema-valid write
//! that promotes a false claim, suppresses counter-evidence or declassifies a
//! record. A case that pins a method pack declaring `[[guardrails]]` adds
//! requirements to exactly seven writes ([`GuardedOperation`]):
//! `conclusion.validate`, `decision.accept`, `source.withdraw`,
//! `artifact.withdraw`, `artifact.expire`, `observation.retract` and
//! `classification.downgrade`. The two Artifact writes are the same
//! suppression as `source.withdraw`: one takes the Artifact out of
//! evidentiary use, the other withdraws its bytes' availability. No other operation
//! can be guarded, so recording, ingest and capture stay cheap. With no
//! guardrail pinned nothing is checked and nothing is stored, so behaviour is
//! unchanged.
//!
//! The requirements are:
//!
//! - `distinct_actor`: the writing actor did not record the subject's current
//!   content;
//! - `distinct_context`: the write declares a session that recorded none of
//!   the subject's current content and that has no recorded evidence read of
//!   untrusted material through investigation, so the context that read
//!   hostile evidence cannot promote or suppress. It does not require the
//!   writing context to have read the evidence, and reads through `brief` or
//!   `show` are not recorded, so a fresh context acting on a laundered
//!   synopsis passes; `human_confirmation` is the requirement for that case;
//! - `human_confirmation`: the envelope carries an attributed claim of who
//!   confirmed this exact write (operation and subject) and how, naming an
//!   actor other than the writer and the subject's content authors (the
//!   authors `distinct_actor` uses). A procedural check of actor strings.
//!
//! Actors, sessions and confirmations are self-reported. Investigation does
//! not authenticate people or contexts and sees only the evidence reads made
//! through it under a declared session; every refusal and record says so
//! ([`LIMITS`]). An unmet requirement fails with `GATE_FAILED`, writes
//! nothing, and names the neutral route to satisfy it. A met set of
//! requirements is stored on the guarded revision as a [`GuardrailRecord`].

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    basis::basis_digest,
    command::{Command, RevisionRef},
    header::Actor,
    identity::{Id, Kind},
    inherited_trust::{Session, SessionLedger, SessionReads, TrustLevel},
    method_pack::{GuardrailProposal, GuardrailRequirement},
    method_pack_ops::{ResolvedPack, adopted_packs, resolve_pin},
    pack_policy::{ConsultedPack, authors_by},
    state::{OperationContext, Record, State},
    wire::{ErrorCode, Failure},
};

/// Version of the guardrail evaluation rules in this module.
pub const GUARDRAIL_RULE_VERSION: &str = "write-guardrails.v1";

/// The stated limit carried by every guardrail refusal and record.
pub const LIMITS: &str = "Actors, sessions and human confirmations are self-reported claims; investigation does not authenticate people or contexts. Only evidence reads made through investigation under a declared session are known.";

/// Longest `method` text in a human confirmation, in characters.
const MAX_METHOD_CHARS: usize = 500;
/// How to satisfy `human_confirmation`, in every refusal that names it.
const CONFIRMATION_ROUTE: &str = "Ask a person other than the writer, who did not record the subject's current content, to confirm this exact write. Then resubmit with human_confirmation in the envelope naming that person in confirmed_by and stating how they confirmed it. Investigation compares actor strings and does not authenticate the person.";

/// Most writes one human confirmation may name.
const MAX_CONFIRMED_WRITES: usize = 128;
/// Most same-case session reads named in one refusal; the rest are counted.
const MAX_LISTED_READS: usize = 64;

/// The closed set of operations a guardrail can guard.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum GuardedOperation {
    #[serde(rename = "conclusion.validate")]
    ConclusionValidate,
    #[serde(rename = "decision.accept")]
    DecisionAccept,
    #[serde(rename = "source.withdraw")]
    SourceWithdraw,
    #[serde(rename = "artifact.withdraw")]
    ArtifactWithdraw,
    #[serde(rename = "artifact.expire")]
    ArtifactExpire,
    #[serde(rename = "observation.retract")]
    ObservationRetract,
    #[serde(rename = "classification.downgrade")]
    ClassificationDowngrade,
}

impl GuardedOperation {
    pub const ALL: [Self; 7] = [
        Self::ConclusionValidate,
        Self::DecisionAccept,
        Self::SourceWithdraw,
        Self::ArtifactWithdraw,
        Self::ArtifactExpire,
        Self::ObservationRetract,
        Self::ClassificationDowngrade,
    ];

    pub const fn operation(self) -> &'static str {
        match self {
            Self::ConclusionValidate => "conclusion.validate",
            Self::DecisionAccept => "decision.accept",
            Self::SourceWithdraw => "source.withdraw",
            Self::ArtifactWithdraw => "artifact.withdraw",
            Self::ArtifactExpire => "artifact.expire",
            Self::ObservationRetract => "observation.retract",
            Self::ClassificationDowngrade => "classification.downgrade",
        }
    }

    pub fn from_operation(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.operation() == name)
    }

    /// Pack policy already warns about unavailable pins at promotion gates.
    const fn promotion(self) -> bool {
        matches!(self, Self::ConclusionValidate | Self::DecisionAccept)
    }
}

/// An attributed claim, carried in the command envelope, that a person
/// confirmed specific guarded writes. Investigation records it as stated and
/// does not authenticate the person or the method.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HumanConfirmation {
    /// Who confirmed, as an actor string: not the writing actor and not an
    /// actor who recorded the subject's current content. Self-reported, not
    /// authenticated.
    pub confirmed_by: Actor,
    /// How the confirmation was obtained, in words. Self-reported.
    #[schemars(length(min = 1, max = 500))]
    pub method: String,
    /// The guarded writes confirmed: each names the operation and the ID of
    /// the record it changes (the Conclusion, Decision, Source, Observation,
    /// or the record being downgraded).
    #[schemars(length(min = 1, max = 128))]
    pub confirms: Vec<ConfirmedWrite>,
}

/// One guarded write a human confirmation covers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmedWrite {
    pub op: GuardedOperation,
    pub subject: Id,
}

impl HumanConfirmation {
    fn covers(&self, operation: GuardedOperation, subject: Id) -> bool {
        self.confirms
            .iter()
            .any(|write| write.op == operation && write.subject == subject)
    }
}

/// Envelope shape of a human confirmation.
pub fn validate_confirmation(confirmation: &HumanConfirmation) -> Result<(), Failure> {
    let method = &confirmation.method;
    if method.trim().is_empty()
        || method.chars().count() > MAX_METHOD_CHARS
        || method.chars().any(char::is_control)
    {
        return Err(envelope(
            "/human_confirmation/method",
            "State how the confirmation was obtained in 1-500 characters without control characters.",
        ));
    }
    if confirmation.confirms.is_empty() || confirmation.confirms.len() > MAX_CONFIRMED_WRITES {
        return Err(envelope(
            "/human_confirmation/confirms",
            "Name 1-128 confirmed writes, each with its operation and subject ID.",
        ));
    }
    let mut seen = BTreeSet::new();
    for (index, write) in confirmation.confirms.iter().enumerate() {
        if !seen.insert((write.op, write.subject)) {
            return Err(envelope(
                format!("/human_confirmation/confirms/{index}"),
                "Name each confirmed write once.",
            ));
        }
    }
    Ok(())
}

fn envelope(path: impl Into<String>, message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message)
        .at(path)
        .with_schema("record")
}

/// Whether `command` contains a guarded operation, so the store supplies the
/// session evidence only when it can matter.
pub fn guards_any(command: &Command) -> bool {
    command
        .operations
        .iter()
        .any(|operation| GuardedOperation::from_operation(&operation.op).is_some())
}

/// What the store knows about sessions when it commits a command with a
/// guarded operation.
#[derive(Clone, Debug, Default)]
pub struct SessionEvidence {
    /// Reads recorded now for the command's actor under its declared session,
    /// the same ones commit stamps on the journal event.
    pub current: Option<SessionReads>,
    /// Session reads stamped on the case's earlier journal events.
    pub ledger: SessionLedger,
}

/// One guardrail a write satisfied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppliedGuardrail {
    pub pack_id: String,
    pub version: String,
    pub digest: crate::wire::Digest,
    /// The guardrail's name inside its pack.
    pub guardrail: String,
    pub requirement: GuardrailRequirement,
}

/// The guardrails in force at one guarded write, stored with the revision it
/// writes. Present only when at least one guardrail applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuardrailRecord {
    pub operation: GuardedOperation,
    pub rule_version: String,
    /// The acceptance sequence of the guarded transaction.
    pub sequence: u64,
    /// Every pack pinned to the case at the write, in adoption order.
    pub packs: Vec<ConsultedPack>,
    pub rules: Vec<AppliedGuardrail>,
    /// The session the command declared. A self-reported label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Session>,
    /// The human confirmation the command carried, when a rule required one.
    /// A recorded claim; investigation does not authenticate people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_confirmation: Option<HumanConfirmation>,
    pub limits: String,
}

/// Encode a guardrail record for storage in an untyped record body.
pub fn encode(record: &GuardrailRecord) -> Result<Value, Failure> {
    serde_json::to_value(record).map_err(|_| integrity())
}

fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "Invalid stored state for a write guardrail.",
    )
}

/// Check every guardrail the case's pinned packs declare for `operation`
/// against the write to `subject` (its current revision before the write).
/// Returns the record to store with the new revision, or `None` when no
/// guardrail applies. Unmet requirements fail with one blocker per rule.
pub fn enforce(
    ctx: &mut OperationContext<'_>,
    operation: GuardedOperation,
    subject: &Record,
) -> Result<Option<GuardrailRecord>, Failure> {
    let case = ctx.case_id().ok_or_else(integrity)?;
    let pins = adopted_packs(ctx.state(), case, None)?;
    if pins.is_empty() {
        return Ok(None);
    }
    let source = ctx.pack_source();
    let packs: Vec<ResolvedPack> = pins
        .iter()
        .map(|pin| {
            let bytes = source.and_then(|source| source.pack_bytes(&pin.digest));
            resolve_pin(pin, bytes.as_deref())
        })
        .collect();
    if !operation.promotion() {
        for pack in packs.iter().filter(|pack| !pack.is_available()) {
            ctx.warn(format!(
                "Pinned method pack {}@{} ({}) is unavailable; its guardrails were not applied at {}.",
                pack.pack_id,
                pack.version,
                pack.digest,
                operation.operation()
            ));
        }
    }
    let rules: Vec<(&ResolvedPack, &GuardrailProposal)> = packs
        .iter()
        .filter(|pack| pack.is_available())
        .filter_map(|pack| pack.loaded.as_ref().map(|loaded| (pack, loaded)))
        .flat_map(|(pack, loaded)| {
            loaded
                .pack()
                .guardrails
                .iter()
                .filter(|rule| rule.operation == operation.operation())
                .map(move |rule| (pack, rule))
        })
        .collect();
    if rules.is_empty() {
        return Ok(None);
    }

    let write = Write {
        operation,
        state: ctx.state(),
        case,
        sequence: ctx.acceptance_sequence(),
        actor: ctx.actor(),
        session: ctx.session(),
        confirmation: ctx.human_confirmation(),
        evidence: ctx.session_evidence(),
        subject,
    };
    let content = write.content_revisions()?;
    let mut applied = Vec::new();
    let mut blockers = Vec::new();
    for (pack, rule) in rules {
        let unmet = match rule.requirement {
            GuardrailRequirement::DistinctActor => write.distinct_actor()?,
            GuardrailRequirement::DistinctContext => write.distinct_context(&content),
            GuardrailRequirement::HumanConfirmation => write.human_confirmation()?,
        };
        match unmet {
            Some((unmet, repair)) => blockers.push(json!({
                "pack_id": pack.pack_id, "version": pack.version, "digest": pack.digest,
                "guardrail": rule.name, "requirement": rule.requirement,
                "operation": operation, "rationale": rule.rationale,
                "unmet": unmet, "repair": repair,
            })),
            None => applied.push(AppliedGuardrail {
                pack_id: pack.pack_id.clone(),
                version: pack.version.clone(),
                digest: pack.digest.clone(),
                guardrail: rule.name.clone(),
                requirement: rule.requirement,
            }),
        }
    }
    if !blockers.is_empty() {
        let mut failure = Failure::new(
            ErrorCode::GateFailed,
            format!(
                "A pinned method pack guardrail adds requirements to {} that are not met. Nothing was written.",
                operation.operation()
            ),
        );
        failure.error.expected = Some(json!({
            "guardrail_blockers": blockers,
            "subject_ref": subject.reference(),
            "limits": LIMITS,
        }));
        return Err(failure);
    }
    let confirmation = applied
        .iter()
        .any(|rule| rule.requirement == GuardrailRequirement::HumanConfirmation)
        .then(|| write.confirmation.cloned())
        .flatten();
    Ok(Some(GuardrailRecord {
        operation,
        rule_version: GUARDRAIL_RULE_VERSION.to_owned(),
        sequence: write.sequence,
        packs: packs
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
        session: write.session.cloned(),
        human_confirmation: confirmation,
        limits: LIMITS.to_owned(),
    }))
}

type Unmet = Option<(Value, Value)>;

/// One guarded write as the requirements see it.
struct Write<'a> {
    operation: GuardedOperation,
    state: &'a State,
    case: Id,
    sequence: u64,
    actor: &'a Actor,
    session: Option<&'a Session>,
    confirmation: Option<&'a HumanConfirmation>,
    evidence: Option<&'a SessionEvidence>,
    subject: &'a Record,
}

impl Write<'_> {
    /// Revisions up to the subject's current one that recorded content: the
    /// first, and each that changed it. Evidentiary-use and lifecycle fields
    /// (withdrawals, judgments, status) are not content.
    fn content_revisions(&self) -> Result<Vec<&Record>, Failure> {
        let mut revisions = Vec::new();
        let mut previous: Option<Value> = None;
        for revision in self
            .state
            .history(self.subject.header.id)
            .take_while(|revision| revision.header.revision <= self.subject.header.revision)
        {
            let content = content(revision)?;
            if previous.as_ref() != Some(&content) {
                revisions.push(revision);
            }
            previous = Some(content);
        }
        Ok(revisions)
    }

    /// Actors who recorded the subject's current content, by the rule review
    /// independence and `lesson.admit` use ([`authors_by`]) with [`content`].
    fn authors(&self) -> Result<BTreeSet<Actor>, Failure> {
        authors_by(self.state, self.subject, content)
    }

    fn distinct_actor(&self) -> Result<Unmet, Failure> {
        let authors = self.authors()?;
        Ok(authors.contains(self.actor).then(|| {
            (
                json!({"missing": "distinct_actor", "actor": self.actor, "authors": authors}),
                json!({
                    "change": "actor",
                    "route": "Submit this write as an actor that did not record the subject's current content.",
                }),
            )
        }))
    }

    fn distinct_context(&self, content: &[&Record]) -> Unmet {
        let route = "Submit this write with a declared session that did not record the subject and has read no evidence through investigation, such as a separate context that reads only brief, show and review packets.";
        let repair = json!({"change": "session", "route": route});
        let Some(session) = self.session else {
            return Some((json!({"missing": "declared_session"}), repair));
        };
        let Some(evidence) = self.evidence else {
            return Some((
                json!({"missing": "distinct_context", "session": session, "session_reads": "unavailable"}),
                repair,
            ));
        };
        let authored: Vec<RevisionRef> = content
            .iter()
            .filter(|revision| {
                let sequence = revision.header.acceptance_sequence;
                if sequence == self.sequence {
                    true
                } else {
                    evidence
                        .ledger
                        .get(&sequence)
                        .is_some_and(|reads| &reads.session == session)
                }
            })
            .map(|revision| revision.reference())
            .collect();
        let entries: Vec<&SessionReads> = evidence
            .current
            .iter()
            .filter(|reads| &reads.session == session)
            .chain(
                evidence
                    .ledger
                    .values()
                    .filter(|reads| &reads.session == session),
            )
            .collect();
        let untrusted: Vec<&SessionReads> = entries
            .into_iter()
            .filter(|reads| {
                reads
                    .lowest
                    .is_some_and(|level| level <= TrustLevel::UntrustedExternal)
            })
            .collect();
        if authored.is_empty() && untrusted.is_empty() {
            return None;
        }
        let mut here = BTreeSet::new();
        let mut elsewhere = BTreeSet::new();
        let mut unlisted = false;
        for reads in &untrusted {
            unlisted |= reads.reads_total > reads.reads.len() as u64;
            for read in &reads.reads {
                if read.case_id == self.case {
                    here.insert(read.artifact);
                } else {
                    elsewhere.insert((read.case_id, read.artifact));
                }
            }
        }
        let listed: Vec<RevisionRef> = here.iter().take(MAX_LISTED_READS).copied().collect();
        let mut unmet = json!({
            "missing": "distinct_context",
            "session": session,
            "authored_revisions": authored,
            "untrusted_reads": listed,
            "untrusted_reads_in_this_case": here.len(),
            "untrusted_reads_in_other_cases": elsewhere.len(),
        });
        if unlisted {
            unmet["unlisted_reads"] = json!(true);
        }
        Some((unmet, repair))
    }

    /// The confirmation must name this write and a confirming actor other
    /// than the writer and the subject's content authors.
    fn human_confirmation(&self) -> Result<Unmet, Failure> {
        let subject = self.subject.header.id;
        let repair = json!({
            "change": "human_confirmation",
            "route": CONFIRMATION_ROUTE,
            "shape": {
                "confirmed_by": "<actor who confirmed>",
                "method": "<how the confirmation was obtained>",
                "confirms": [{"op": self.operation, "subject": subject}],
            },
        });
        Ok(match self.confirmation {
            None => Some((json!({"missing": "human_confirmation"}), repair)),
            Some(confirmation) if !confirmation.covers(self.operation, subject) => Some((
                json!({
                    "missing": "human_confirmation_for_this_write",
                    "op": self.operation, "subject": subject,
                }),
                repair,
            )),
            Some(confirmation) => {
                let authors = self.authors()?;
                let by = &confirmation.confirmed_by;
                (by == self.actor || authors.contains(by)).then(|| {
                    (
                        json!({
                            "missing": "human_confirmation_by_another_actor",
                            "confirmed_by": by, "actor": self.actor, "authors": authors,
                        }),
                        repair,
                    )
                })
            }
        })
    }
}

/// What counts as a revision's content for authorship: the substantive basis
/// where one is defined, otherwise the body without evidentiary-use and
/// lifecycle metadata.
fn content(revision: &Record) -> Result<Value, Failure> {
    let strip = |keys: &[&str]| {
        let mut body = revision.body.clone();
        if let Some(object) = body.as_object_mut() {
            for key in keys {
                object.remove(*key);
            }
        }
        body
    };
    Ok(match revision.header.id.kind() {
        Kind::Source | Kind::Artifact => strip(crate::source_ops::USE_FIELDS),
        Kind::Observation => strip(crate::observation_ops::OBSERVATION_METADATA),
        Kind::Conclusion | Kind::Decision => json!(basis_digest(revision)?),
        _ => match basis_digest(revision) {
            Ok(digest) => json!(digest),
            Err(_) => revision.body.clone(),
        },
    })
}

#[cfg(test)]
mod tests;
