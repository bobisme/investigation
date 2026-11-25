//! Case and Question payloads. These are bodies and command payloads, not
//! accepted records: the engine supplies the shared `RecordHeader` later.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use crate::header::Knowledge;
use crate::{
    command::{Reference, RevisionRef},
    identity::{Id, Kind},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaseMode {
    Explain,
    Measure,
    Evaluate,
    #[default]
    Explore,
    Record,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaseLifecycle {
    Active,
    Paused,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaseClosureOutcome {
    Answered,
    Chosen,
    Inconclusive,
    Abandoned,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    QuestionAnswered,
    ChoiceMade,
    ConditionalDecisionAccepted,
    UncertaintyTolerated,
    InsufficientEvidence,
    PrerequisiteUnavailable,
    CostOrTimeExhausted,
    ExperimentTooDisruptive,
    ExperimentNotAuthorized,
    VolatileEvidencePreserved,
    SupersededQuestion,
    InvestigatorPause,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StopCondition {
    #[schemars(length(min = 1, max = 2048))]
    pub description: String,
    pub reasons: Vec<StopReason>,
    pub residual_risks: Vec<String>,
    pub reopening_conditions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaseExpectations {
    pub expected_answer_or_choice: Option<String>,
    pub expected_effort: Option<String>,
    pub surprising_outcomes: Vec<String>,
}

/// Stored domain fields. Identity, attribution, revision and provenance belong
/// in the engine's common RecordHeader, never in this body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaseBody {
    #[schemars(length(min = 1, max = 256))]
    pub title: String,
    pub mode: CaseMode,
    pub objective: Option<String>,
    pub scope: Option<Knowledge<String>>,
    pub decision_relevance: Option<String>,
    pub exclusions: Vec<String>,
    pub stop_condition: Option<Knowledge<StopCondition>>,
    pub policy_pins: Vec<String>,
    pub method_pins: Vec<String>,
    pub expectations: Option<CaseExpectations>,
    pub lifecycle: CaseLifecycle,
    pub closure_outcome: Option<CaseClosureOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closure: Option<CaseClosureDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_reason: Option<String>,
    /// Digest-pinned method packs adopted through `method_pack.adopt`. The
    /// free-text `method_pins` above carry no pack resolution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adopted_method_packs: Vec<crate::method_pack_ops::MethodPackPin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaseClosureDetails {
    pub unresolved_questions: Vec<RevisionRef>,
    pub reopen_condition: Option<String>,
    pub retrospective_ref: Option<RevisionRef>,
    pub retrospective_waived: Option<String>,
    /// Pinned pack policy consulted by the `case.close` gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_policy: Option<crate::pack_policy::GatePolicy>,
}

/// Payload for `case.create`; the engine assigns a durable case ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaseCreate {
    #[schemars(length(min = 1, max = 256))]
    pub title: String,
    #[serde(default)]
    pub mode: CaseMode,
    pub objective: Option<String>,
    pub scope: Option<Knowledge<String>>,
    pub decision_relevance: Option<String>,
    #[serde(default)]
    pub exclusions: Vec<String>,
    pub stop_condition: Option<Knowledge<StopCondition>>,
    #[serde(default)]
    pub policy_pins: Vec<String>,
    #[serde(default)]
    pub method_pins: Vec<String>,
    pub expectations: Option<CaseExpectations>,
}

impl CaseCreate {
    pub fn validate(&self) -> Result<(), CaseBlocker> {
        if self.title.trim().is_empty() {
            return Err(CaseBlocker::TitleRequired);
        }
        Ok(())
    }

    pub fn into_body(self) -> CaseBody {
        CaseBody {
            title: self.title,
            mode: self.mode,
            objective: self.objective,
            scope: self.scope,
            decision_relevance: self.decision_relevance,
            exclusions: self.exclusions,
            stop_condition: self.stop_condition,
            policy_pins: self.policy_pins,
            method_pins: self.method_pins,
            expectations: self.expectations,
            lifecycle: CaseLifecycle::Active,
            closure_outcome: None,
            closure: None,
            transition_reason: None,
            adopted_method_packs: Vec::new(),
        }
    }
}

/// A mode change is an explicit, attributed amendment. `reason` is mandatory;
/// actor attribution is supplied by the command/header layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaseAmend {
    #[schemars(length(min = 1, max = 2048))]
    pub reason: String,
    pub title: Option<String>,
    pub mode: Option<CaseMode>,
    pub objective: Option<Knowledge<String>>,
    pub scope: Option<Knowledge<String>>,
    pub decision_relevance: Option<Knowledge<String>>,
    pub exclusions: Option<Vec<String>>,
    pub stop_condition: Option<Knowledge<StopCondition>>,
    pub policy_pins: Option<Vec<String>>,
    pub method_pins: Option<Vec<String>>,
    pub expectations: Option<Knowledge<CaseExpectations>>,
}

impl CaseAmend {
    /// A payload with a reason but no change is not an amendment.
    pub fn has_change(&self) -> bool {
        self.title.is_some()
            || self.mode.is_some()
            || self.objective.is_some()
            || self.scope.is_some()
            || self.decision_relevance.is_some()
            || self.exclusions.is_some()
            || self.stop_condition.is_some()
            || self.policy_pins.is_some()
            || self.method_pins.is_some()
            || self.expectations.is_some()
    }

    pub fn validate(&self) -> Result<(), CaseBlocker> {
        if self.reason.trim().is_empty() {
            return Err(CaseBlocker::AmendmentReasonRequired);
        }
        if !self.has_change() {
            return Err(CaseBlocker::EmptyAmendment);
        }
        if self
            .title
            .as_deref()
            .is_some_and(|title| title.trim().is_empty())
        {
            return Err(CaseBlocker::TitleRequired);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CasePause {
    #[schemars(length(min = 1, max = 2048))]
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaseReopen {
    #[schemars(length(min = 1, max = 2048))]
    pub reason: String,
}

/// Close a case. Order: resolve each Question with `question.resolve`, then
/// take the draft (`retrospective --draft`), then `retrospective.record` and
/// `case.close`; all three may share one transaction, in that order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaseClose {
    pub outcome: CaseClosureOutcome,
    #[schemars(length(min = 1, max = 2048))]
    pub reason: String,
    /// Questions left unresolved, each at its current revision. Outcome
    /// `answered` is refused while a Question is open and not listed here.
    pub unresolved_questions: Vec<Reference>,
    pub reopen_condition: Option<String>,
    /// Existing or same-transaction Retrospective. Mutually exclusive with waiver.
    pub retrospective_ref: Option<Reference>,
    /// A short reason for omitting a Retrospective on a non-material case.
    #[schemars(length(min = 1, max = 2048))]
    pub retrospective_waived: Option<String>,
}

impl CaseClose {
    pub fn validate_materiality(&self, mode: CaseMode, material: bool) -> Result<(), CaseBlocker> {
        if self.retrospective_ref.is_some() && self.retrospective_waived.is_some() {
            return Err(CaseBlocker::RetrospectiveDispositionConflict);
        }
        if self
            .retrospective_waived
            .as_deref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            return Err(CaseBlocker::RetrospectiveWaiverReasonRequired);
        }
        if let Some(Reference::Existing(reference)) = &self.retrospective_ref
            && reference.id.expect_kind(Kind::Retrospective).is_err()
        {
            return Err(CaseBlocker::RetrospectiveReferenceWrongKind);
        }
        if self
            .unresolved_questions
            .iter()
            .any(|reference| match reference {
                Reference::Existing(revision) => revision.id.expect_kind(Kind::Question).is_err(),
                Reference::Client(_) => false,
            })
        {
            return Err(CaseBlocker::UnresolvedQuestionWrongKind);
        }
        if material {
            if self.retrospective_ref.is_none() {
                return Err(CaseBlocker::RetrospectiveRequired);
            }
        } else if mode != CaseMode::Record
            && self.retrospective_ref.is_none()
            && self.retrospective_waived.is_none()
        {
            return Err(CaseBlocker::RetrospectiveWaiverRequired);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseTransition {
    Pause,
    Reopen,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseBlocker {
    CaseAlreadyActive,
    CaseAlreadyPaused,
    CaseAlreadyClosed,
    CaseCannotPauseWhenClosed,
    CaseCannotCloseWhenClosed,
    RetrospectiveRequired,
    RetrospectiveDispositionConflict,
    RetrospectiveWaiverReasonRequired,
    RetrospectiveWaiverRequired,
    RetrospectiveReferenceWrongKind,
    UnresolvedQuestionWrongKind,
    AmendmentReasonRequired,
    EmptyAmendment,
    TitleRequired,
}

impl CaseBlocker {
    pub const fn code(self) -> &'static str {
        match self {
            Self::CaseAlreadyActive => "CASE_ALREADY_ACTIVE",
            Self::CaseAlreadyPaused => "CASE_ALREADY_PAUSED",
            Self::CaseAlreadyClosed => "CASE_ALREADY_CLOSED",
            Self::CaseCannotPauseWhenClosed => "CASE_CANNOT_PAUSE_WHEN_CLOSED",
            Self::CaseCannotCloseWhenClosed => "CASE_CANNOT_CLOSE_WHEN_CLOSED",
            Self::RetrospectiveRequired => "RETROSPECTIVE_REQUIRED",
            Self::RetrospectiveDispositionConflict => "RETROSPECTIVE_DISPOSITION_CONFLICT",
            Self::RetrospectiveWaiverReasonRequired => "RETROSPECTIVE_WAIVER_REASON_REQUIRED",
            Self::RetrospectiveWaiverRequired => "RETROSPECTIVE_WAIVER_REQUIRED",
            Self::RetrospectiveReferenceWrongKind => "RETROSPECTIVE_REFERENCE_WRONG_KIND",
            Self::UnresolvedQuestionWrongKind => "CASE_UNRESOLVED_QUESTION_WRONG_KIND",
            Self::AmendmentReasonRequired => "CASE_AMENDMENT_REASON_REQUIRED",
            Self::EmptyAmendment => "CASE_AMENDMENT_EMPTY",
            Self::TitleRequired => "CASE_TITLE_REQUIRED",
        }
    }
}

impl CaseLifecycle {
    /// Pure state transition; persistence and attribution remain engine duties.
    pub fn transition(self, action: CaseTransition) -> Result<Self, CaseBlocker> {
        use CaseLifecycle::{Active, Closed, Paused};
        match (self, action) {
            (Active, CaseTransition::Pause) => Ok(Paused),
            (Active, CaseTransition::Close) | (Paused, CaseTransition::Close) => Ok(Closed),
            (Paused, CaseTransition::Reopen) | (Closed, CaseTransition::Reopen) => Ok(Active),
            (Active, CaseTransition::Reopen) => Err(CaseBlocker::CaseAlreadyActive),
            (Paused, CaseTransition::Pause) => Err(CaseBlocker::CaseAlreadyPaused),
            (Closed, CaseTransition::Pause) => Err(CaseBlocker::CaseCannotPauseWhenClosed),
            (Closed, CaseTransition::Close) => Err(CaseBlocker::CaseCannotCloseWhenClosed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionDisposition {
    Open,
    Answered,
    Inconclusive,
    Deferred,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionImportance {
    Low,
    Normal,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionBody {
    #[schemars(length(min = 1, max = 2048))]
    pub wording: String,
    pub context: Option<String>,
    pub scope: Option<Knowledge<String>>,
    pub importance: QuestionImportance,
    /// Accepted body stores a fully resolved exact reference, not a client alias.
    pub parent: Option<RevisionRef>,
    pub disposition: QuestionDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<RevisionRef>,
}

/// Payload for `question.create`. A parent may refer to an earlier operation in
/// the same command; resolve `ClientRef` to an exact revision before persisting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionCreate {
    #[schemars(length(min = 1, max = 2048))]
    pub wording: String,
    pub context: Option<String>,
    pub scope: Option<Knowledge<String>>,
    pub importance: QuestionImportance,
    pub parent: Option<Reference>,
}

impl QuestionCreate {
    pub fn into_body(
        self,
        resolved_parent: Option<RevisionRef>,
    ) -> Result<QuestionBody, QuestionReferenceBlocker> {
        match (&self.parent, resolved_parent) {
            (None, None) => {}
            (None, Some(_)) => return Err(QuestionReferenceBlocker::UnexpectedResolvedParent),
            (Some(Reference::Existing(expected)), Some(actual)) if *expected == actual => {}
            (Some(Reference::Existing(_)), Some(_)) => {
                return Err(QuestionReferenceBlocker::ResolvedParentMismatch);
            }
            (Some(Reference::Existing(_)), None) => {
                return Err(QuestionReferenceBlocker::ExistingParentNotResolved);
            }
            (Some(Reference::Client(_)), Some(_)) => {}
            (Some(Reference::Client(_)), None) => {
                return Err(QuestionReferenceBlocker::ClientParentNotResolved);
            }
        }
        if let Some(parent) = resolved_parent
            && parent.id.expect_kind(Kind::Question).is_err()
        {
            return Err(QuestionReferenceBlocker::ParentWrongKind);
        }
        Ok(QuestionBody {
            wording: self.wording,
            context: self.context,
            scope: self.scope,
            importance: self.importance,
            parent: resolved_parent,
            disposition: QuestionDisposition::Open,
            answer: None,
            resolution_reason: None,
            evidence_refs: Vec::new(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionReferenceBlocker {
    UnexpectedResolvedParent,
    ResolvedParentMismatch,
    ExistingParentNotResolved,
    ClientParentNotResolved,
    ParentWrongKind,
}

impl QuestionReferenceBlocker {
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnexpectedResolvedParent => "QUESTION_PARENT_UNEXPECTED",
            Self::ResolvedParentMismatch => "QUESTION_PARENT_MISMATCH",
            Self::ExistingParentNotResolved => "QUESTION_PARENT_UNRESOLVED",
            Self::ClientParentNotResolved => "QUESTION_CLIENT_PARENT_UNRESOLVED",
            Self::ParentWrongKind => "QUESTION_PARENT_WRONG_KIND",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionResolve {
    pub question_ref: RevisionRef,
    pub disposition: QuestionDisposition,
    #[schemars(length(min = 1, max = 2048))]
    pub reason: String,
    pub answer: Option<String>,
    pub evidence_refs: Vec<Reference>,
}

impl QuestionResolve {
    pub fn validate_from(&self, current: QuestionDisposition) -> Result<(), QuestionBlocker> {
        if self.question_ref.id.expect_kind(Kind::Question).is_err() {
            return Err(QuestionBlocker::WrongQuestionReference);
        }
        if self.disposition == QuestionDisposition::Open {
            return Err(QuestionBlocker::InvalidResolution);
        }
        current.transition(self.disposition)?;
        if self.disposition == QuestionDisposition::Answered {
            if self
                .answer
                .as_deref()
                .is_none_or(|answer| answer.trim().is_empty())
            {
                return Err(QuestionBlocker::AnswerRequired);
            }
            if self.evidence_refs.is_empty() {
                return Err(QuestionBlocker::AnswerNeedsEvidence);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionBlocker {
    InvalidResolution,
    QuestionAlreadyResolved,
    QuestionSuperseded,
    AnswerRequired,
    AnswerNeedsEvidence,
    WrongQuestionReference,
}

impl QuestionBlocker {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidResolution => "QUESTION_INVALID_RESOLUTION",
            Self::QuestionAlreadyResolved => "QUESTION_ALREADY_RESOLVED",
            Self::QuestionSuperseded => "QUESTION_SUPERSEDED",
            Self::AnswerRequired => "QUESTION_ANSWER_REQUIRED",
            Self::AnswerNeedsEvidence => "QUESTION_ANSWER_NEEDS_EVIDENCE",
            Self::WrongQuestionReference => "QUESTION_REFERENCE_WRONG_KIND",
        }
    }
}

impl QuestionDisposition {
    pub fn transition(self, next: Self) -> Result<Self, QuestionBlocker> {
        use QuestionDisposition::{Answered, Deferred, Inconclusive, Open, Superseded};
        match (self, next) {
            (Open, Answered | Inconclusive | Deferred | Superseded) => Ok(next),
            (Deferred, Open | Answered | Inconclusive | Superseded) => Ok(next),
            (_, Superseded) if self != Superseded => Ok(Superseded),
            (Superseded, _) => Err(QuestionBlocker::QuestionSuperseded),
            (Answered | Inconclusive, _) => Err(QuestionBlocker::QuestionAlreadyResolved),
            _ => Err(QuestionBlocker::InvalidResolution),
        }
    }
}

/// Check a durable ID's kind at the operation boundary. Input decoding validates
/// the prefix/UUID syntax; relation-specific kind checks belong here or in the engine.
pub fn require_kind(id: Id, expected: Kind) -> Result<Id, KindMismatch> {
    id.expect_kind(expected).map_err(|_| KindMismatch {
        expected,
        actual: id.kind(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindMismatch {
    pub expected: Kind,
    pub actual: Kind,
}
