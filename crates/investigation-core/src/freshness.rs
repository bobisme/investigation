//! Dependency index and effective freshness propagation.
//!
//! The index is derived only from exact revision references in canonical
//! record bodies, so it can always be rebuilt from the journal. Freshness is a
//! read-time judgment over one snapshot: lifecycle history is never rewritten.
//!
//! A dependency is affected by a change to the record it cites when that
//! record's *substantive* basis differs (see [`crate::basis::basis_digest`]),
//! or when the record became withdrawn, retracted, retired, rejected or
//! superseded. Lifecycle-only revisions, such as validating a conclusion, do
//! not invalidate dependents. Through `Inherit` edges a dependent also inherits
//! the freshness of the records it relies on, with a reason chain that says why.
//!
//! [`FreshnessIndex::build`] recomputes everything from scratch.
//! [`FreshnessIndex::update`] recomputes only what a newer snapshot can affect.
//! The two must produce identical indexes.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    basis::{MembershipBasis, MembershipRule, basis_digest},
    command::RevisionRef,
    evidence::MeasurementValidity,
    identity::{Id, Kind},
    measurement_ops::{MEASUREMENT_ASSESSMENT_KEY, effective_validity, validity_rank},
    review_ops::{promotion_memberships, required_reads},
    state::{Record, State},
    wire::{ErrorCode, Failure},
};

/// Version of the trigger, relevance and propagation rules in this module.
/// Version 2 judges Observations by effective validity and keeps
/// classification changes out of the Overview. Version 3 makes a blindspot
/// review that read a structural audit stale when the check set changes.
/// Version 4 acknowledges a partial withdrawal or a raised source concern for
/// a record whose current revision was accepted at or after it: that record
/// read the Source or Artifact with the change in effect. Version 5 checks a
/// validation review for new required reads whatever else changed in its
/// subject's closure: behind a benign later revision (such as a restated
/// criterion), which the promotion closure now reads through, and where a
/// promotion would refuse the walk (such as a rejected material assumption).
/// Version 6 tracks an objection's `resolution_evidence_refs` and an option's
/// disposition `criterion_ref` as inherited dependencies (the option's in the
/// promotion closure), and keeps a validation review's membership checks
/// after its subject changes in substance, so an objection raised after the
/// review disputes it rather than leaving it only `needs_review`.
pub const FRESHNESS_RULES_VERSION: u32 = 6;

/// Effective freshness, ordered by severity.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Current,
    NeedsReview,
    Disputed,
}

impl Freshness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::NeedsReview => "needs_review",
            Self::Disputed => "disputed",
        }
    }
}

/// How a change below an edge reaches the dependent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Propagation {
    /// The dependent relies on the cited record's own basis and inherits its
    /// effective freshness, in addition to reacting to changes of the record.
    Inherit,
    /// Only a change to the cited record itself affects the dependent. Used for
    /// subjects, scopes and questions, which would otherwise form cycles.
    ChangeOnly,
}

impl Propagation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::ChangeOnly => "change_only",
        }
    }
}

/// One exact revision cited by a record body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DependencyEdge {
    /// Body field (dotted for nested lists) that holds the reference.
    pub field: &'static str,
    pub target: RevisionRef,
    pub propagation: Propagation,
    /// Part of the promotion dependency closure used by review gates.
    #[serde(skip)]
    closure: bool,
}

/// Why a record is not current.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    /// A source, artifact or option was withdrawn, or its bytes were declared missing.
    Withdrawn,
    /// An observation, assessment, comparison or conclusion was retracted.
    Retracted,
    /// A criterion, inquiry or hypothesis was retired.
    Retired,
    /// A hypothesis or cited claim was rejected.
    Rejected,
    /// A cited record was superseded.
    Superseded,
    /// The substantive basis of a cited record changed: a material correction,
    /// criterion or option revision, or claim amendment.
    MaterialRevision,
    /// A cited measurement is now assessed as invalid.
    InvalidMeasurement,
    /// A cited Observation's effective validity got worse without becoming
    /// invalid, for example `valid` to `partial`. Judgments that keep or raise
    /// effective validity do not change freshness.
    ValidityLowered {
        from: MeasurementValidity,
        to: MeasurementValidity,
    },
    /// Bytes supplied to reverify a cited reference-only Artifact differ
    /// from its recorded digest or size. The recorded digest is unchanged.
    ExternalMismatch,
    /// A cited source is newly judged doubtful or not authentic, unreliable,
    /// or outdated or superseded (`source.assess`).
    SourceConcern { dimensions: Vec<String> },
    /// A cited assumption's disposition changed.
    AssumptionDisposition {
        from: Option<String>,
        to: Option<String>,
    },
    /// An open critical objection names the record.
    CriticalObjection,
    /// A scoped set captured by a validation review changed membership.
    MembershipChanged { rule: MembershipRule },
    /// An objection to the reviewed subject was raised after its validation
    /// review. The cause is the objection; it disputes the subject.
    ObjectionRaisedAfterReview,
    /// The current dependency closure includes a record the review did not read.
    BasisExpanded,
    /// A blindspot review read a structural audit whose check set differs
    /// from the registered one. The cause is the review itself.
    CheckSetChanged,
    /// A cited record is absent from the case.
    MissingDependency,
    /// The record's inherited basis is circular.
    CircularBasis,
    /// The record's stored references cannot be read.
    UnreadableBasis,
}

impl Trigger {
    fn key(&self) -> String {
        match self {
            Self::AssumptionDisposition { from, to } => format!(
                "assumption_disposition:{}:{}",
                from.as_deref().unwrap_or("-"),
                to.as_deref().unwrap_or("-")
            ),
            Self::ValidityLowered { from, to } => format!(
                "validity_lowered:{}:{}",
                wire(from).unwrap_or_default(),
                wire(to).unwrap_or_default()
            ),
            Self::MembershipChanged { rule } => format!(
                "membership_changed:{}",
                serde_json::to_value(rule)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default()
            ),
            other => trigger_kind(other),
        }
    }

    /// A short, stable description of the trigger for text output.
    pub fn describe(&self) -> String {
        match self {
            Self::SourceConcern { dimensions } => {
                format!("source concern raised ({})", dimensions.join(", "))
            }
            Self::AssumptionDisposition { from, to } => format!(
                "assumption disposition changed ({} to {})",
                from.as_deref().unwrap_or("none"),
                to.as_deref().unwrap_or("none")
            ),
            Self::ValidityLowered { from, to } => format!(
                "measurement validity lowered ({} to {})",
                wire(from).unwrap_or_default(),
                wire(to).unwrap_or_default()
            ),
            Self::MembershipChanged { rule } => format!(
                "reviewed membership changed ({})",
                wire(rule).unwrap_or_default()
            ),
            other => trigger_kind(other).replace('_', " "),
        }
    }

    fn severity(&self) -> Freshness {
        match self {
            Self::CriticalObjection | Self::ObjectionRaisedAfterReview => Freshness::Disputed,
            _ => Freshness::NeedsReview,
        }
    }
}

fn wire(value: &impl Serialize) -> Option<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
}

fn trigger_kind(trigger: &Trigger) -> String {
    serde_json::to_value(trigger)
        .ok()
        .and_then(|value| value["kind"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// One link in a reason chain: `record` cites `cites` through `field`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DependencyStep {
    pub record: Id,
    pub field: String,
    pub cites: RevisionRef,
}

/// A root cause and the dependency path from the assessed record to it. An
/// empty path means the trigger applies to the record itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FreshnessReason {
    pub freshness: Freshness,
    pub trigger: Trigger,
    /// The current revision of the record whose change or state is the cause.
    pub cause: RevisionRef,
    pub path: Vec<DependencyStep>,
    /// Acceptance sequence of the evidentiary-use change behind the trigger
    /// (`source_ops::UseChange::since`). A record accepted at or after it read
    /// the cited material with the change in effect and acknowledges the
    /// reason. `None` for every other trigger.
    #[serde(skip)]
    since: Option<u64>,
}

type ReasonKey = (Id, u64, String);

impl FreshnessReason {
    fn local(trigger: Trigger, cause: RevisionRef, path: Vec<DependencyStep>) -> Self {
        Self {
            freshness: trigger.severity(),
            trigger,
            cause,
            path,
            since: None,
        }
    }

    /// Whether a record whose current revision was accepted at `read_at` read
    /// the cause with this change already in effect.
    fn acknowledged_at(&self, read_at: u64) -> bool {
        self.since.is_some_and(|since| since <= read_at)
    }

    fn key(&self) -> ReasonKey {
        (self.cause.id, self.cause.revision, self.trigger.key())
    }

    /// One line for text output: the trigger, its cause and the dependency
    /// path, for example `withdrawn: src_… r2 via obs_… basis.artifact_ref -> art_… source`.
    pub fn describe(&self) -> String {
        let mut text = format!(
            "{}: {} r{}",
            self.trigger.describe(),
            self.cause.id,
            self.cause.revision
        );
        for (index, step) in self.path.iter().enumerate() {
            text.push_str(if index == 0 { " via " } else { " -> " });
            text.push_str(&format!("{} {}", step.record, step.field));
        }
        text
    }

    /// Shorter chains win; ties break on the chain's IDs, fields and revisions.
    fn precedes(&self, other: &Self) -> bool {
        let chain = |reason: &Self| -> Vec<(Id, String, Id, u64)> {
            reason
                .path
                .iter()
                .map(|step| {
                    (
                        step.record,
                        step.field.clone(),
                        step.cites.id,
                        step.cites.revision,
                    )
                })
                .collect()
        };
        (self.path.len(), chain(self)) < (other.path.len(), chain(other))
    }
}

/// Effective freshness of one current record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordFreshness {
    pub record: RevisionRef,
    /// Historical lifecycle status as recorded; never rewritten.
    pub lifecycle: Option<String>,
    pub freshness: Freshness,
    pub reasons: Vec<FreshnessReason>,
    /// Reasons this record acknowledges because it was accepted with the
    /// change in effect. They do not affect its freshness but still reach its
    /// dependents, which judge them against their own acceptance.
    #[serde(skip)]
    acknowledged: Vec<FreshnessReason>,
}

impl RecordFreshness {
    /// Lifecycle and freshness together, for example `validated / needs_review`.
    pub fn label(&self) -> String {
        format!(
            "{} / {}",
            self.lifecycle.as_deref().unwrap_or("recorded"),
            self.freshness.as_str()
        )
    }

    /// Whether any reason asks for review again, whatever else disputes it.
    pub fn needs_review(&self) -> bool {
        self.reasons
            .iter()
            .any(|reason| reason.freshness == Freshness::NeedsReview)
    }
}

/// Aggregate freshness for a case Overview or a report over a set of records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FreshnessSummary {
    pub case_id: Id,
    pub sequence: u64,
    pub rules_version: u32,
    pub freshness: Freshness,
    pub current: usize,
    pub needs_review: usize,
    pub disputed: usize,
    /// Records that are not current, in ID order.
    pub affected: Vec<RevisionRef>,
}

fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "Invalid stored review dependency.",
    )
}

fn decode_refs(value: &Value) -> Result<Vec<RevisionRef>, Failure> {
    match value {
        Value::Null => Ok(Vec::new()),
        value @ Value::Array(_) => crate::input::decode(value).map_err(|_| integrity()),
        value => Ok(vec![crate::input::decode(value).map_err(|_| integrity())?]),
    }
}

struct Edges {
    edges: Vec<DependencyEdge>,
    closure: bool,
}

impl Edges {
    fn add(
        &mut self,
        body: &Value,
        key: &str,
        field: &'static str,
        propagation: Propagation,
    ) -> Result<(), Failure> {
        for target in decode_refs(&body[key])? {
            self.edges.push(DependencyEdge {
                field,
                target,
                propagation,
                closure: self.closure,
            });
        }
        Ok(())
    }

    fn push(&mut self, targets: Vec<RevisionRef>, field: &'static str, propagation: Propagation) {
        for target in targets {
            self.edges.push(DependencyEdge {
                field,
                target,
                propagation,
                closure: self.closure,
            });
        }
    }

    fn each(
        &mut self,
        list: &Value,
        keys: &[(&str, &'static str)],
        propagation: Propagation,
    ) -> Result<(), Failure> {
        if let Some(items) = list.as_array() {
            for item in items {
                for (key, field) in keys {
                    self.add(item, key, field, propagation)?;
                }
            }
        }
        Ok(())
    }
}

/// Every exact revision a record body cites, in a stable order. The promotion
/// closure (`review_ops::required_read_set`) walks the subset of these edges
/// that existed before freshness tracking; the remaining edges let reviews,
/// objections, annotations, retrospectives and questions track their bases.
pub fn dependency_edges(record: &Record) -> Result<Vec<DependencyEdge>, Failure> {
    use Propagation::{ChangeOnly, Inherit};
    let body = &record.body;
    let mut out = Edges {
        edges: Vec::new(),
        closure: true,
    };
    match record.header.id.kind() {
        Kind::Conclusion => {
            out.each(
                &body["causal_factors"],
                &[("hypothesis_ref", "causal_factors.hypothesis_ref")],
                Inherit,
            )?;
            let test = &body["causal_test"];
            out.add(test, "factor_ref", "causal_test.factor_ref", Inherit)?;
            out.add(test, "baseline", "causal_test.baseline", Inherit)?;
            out.add(test, "treatment", "causal_test.treatment", Inherit)?;
            out.add(test, "reversal", "causal_test.reversal", Inherit)?;
            out.add(body, "support_refs", "support_refs", Inherit)?;
            out.add(body, "contradiction_refs", "contradiction_refs", Inherit)?;
            out.closure = false;
            out.add(body, "review_ref", "review_ref", Inherit)?;
        }
        Kind::Assessment => {
            out.each(
                &body["rival_expectations"],
                &[("rival_ref", "rival_expectations.rival_ref")],
                ChangeOnly,
            )?;
            out.add(body, "subject_ref", "subject_ref", ChangeOnly)?;
            out.add(body, "evidence_refs", "evidence_refs", Inherit)?;
            out.add(body, "assumption_refs", "assumption_refs", Inherit)?;
            out.add(
                &body["absence"]["opportunity"],
                "refs",
                "absence.opportunity.refs",
                Inherit,
            )?;
            out.add(
                &body["equivalence"],
                "inquiry_ref",
                "equivalence.inquiry_ref",
                Inherit,
            )?;
            out.add(
                &body["equivalence"]["interval"],
                "run_ref",
                "equivalence.interval.run_ref",
                Inherit,
            )?;
            // Frozen-basis links (W10.06): outside the promotion closure.
            out.closure = false;
            let frozen = &body["frozen_basis"];
            out.add(frozen, "plan_ref", "frozen_basis.plan_ref", ChangeOnly)?;
            out.each(
                &frozen["actual_inputs"],
                &[("input_ref", "frozen_basis.actual_inputs.input_ref")],
                Inherit,
            )?;
            out.each(
                &frozen["exclusions"],
                &[("input_ref", "frozen_basis.exclusions.input_ref")],
                ChangeOnly,
            )?;
        }
        Kind::Observation => {
            out.add(
                &body["basis"],
                "artifact_ref",
                "basis.artifact_ref",
                Inherit,
            )?;
            out.add(body, "run_ref", "run_ref", Inherit)?;
            // Claim origin judgments: change-only, outside the promotion closure.
            out.closure = false;
            let (upstream, roots) = crate::origin_ops::origin_references(record)?;
            out.push(
                upstream,
                "claim_origin.traces.origin.observation_ref",
                ChangeOnly,
            );
            out.push(roots, "claim_origin.dependence.roots", ChangeOnly);
        }
        Kind::Artifact => {
            out.each(
                &body["transforms"],
                &[("input", "transforms.input")],
                Inherit,
            )?;
            out.add(body, "source", "source", Inherit)?;
        }
        Kind::Run => {
            out.add(&body["intent"], "plan_ref", "intent.plan_ref", ChangeOnly)?;
            out.add(body, "artifact_refs", "artifact_refs", Inherit)?;
            out.add(
                &body["repetition"],
                "setup_refs",
                "repetition.setup_refs",
                Inherit,
            )?;
            out.add(
                body,
                "retrospective_plan_ref",
                "retrospective_plan_ref",
                ChangeOnly,
            )?;
        }
        Kind::Inquiry => {
            out.each(
                &body["inquiry"]["confirmatory"]["predictions"],
                &[("rival_ref", "inquiry.confirmatory.predictions.rival_ref")],
                ChangeOnly,
            )?;
            out.add(body, "question_ref", "question_ref", ChangeOnly)?;
        }
        Kind::Hypothesis => {
            out.add(body, "question_ref", "question_ref", ChangeOnly)?;
            out.add(body, "assumption_refs", "assumption_refs", Inherit)?;
            out.add(body, "evidence_refs", "evidence_refs", Inherit)?;
        }
        Kind::Assumption => out.add(body, "evidence_refs", "evidence_refs", Inherit)?,
        Kind::Option => {
            out.add(body, "question_ref", "question_ref", ChangeOnly)?;
            out.add(body, "dependencies", "dependencies", Inherit)?;
            // A disposition rests on the criterion it names: an option ruled
            // out against a criterion that later changes needs review, and so
            // does what relies on it (rules version 6).
            out.add(body, "criterion_ref", "criterion_ref", Inherit)?;
        }
        Kind::Criterion => {
            out.add(body, "question_ref", "question_ref", ChangeOnly)?;
            out.add(body, "option_refs", "option_refs", ChangeOnly)?;
            out.add(
                &body["weight"],
                "source_refs",
                "weight.source_refs",
                Inherit,
            )?;
        }
        Kind::Comparison => {
            out.add(body, "option_ref", "option_ref", Inherit)?;
            out.add(body, "criterion_ref", "criterion_ref", Inherit)?;
            out.add(body, "evidence_refs", "evidence_refs", Inherit)?;
            out.add(body, "assumption_refs", "assumption_refs", Inherit)?;
            out.add(
                &body["absence"]["opportunity"],
                "refs",
                "absence.opportunity.refs",
                Inherit,
            )?;
        }
        Kind::Decision => {
            out.add(&body["choice"], "option_ref", "choice.option_ref", Inherit)?;
            out.each(
                &body["hard_conditions"],
                &[("criterion_ref", "hard_conditions.criterion_ref")],
                Inherit,
            )?;
            out.each(
                &body["reversal_conditions"],
                &[
                    ("assumption_ref", "reversal_conditions.assumption_ref"),
                    ("criterion_ref", "reversal_conditions.criterion_ref"),
                    ("comparison_ref", "reversal_conditions.comparison_ref"),
                    ("option_ref", "reversal_conditions.option_ref"),
                ],
                Inherit,
            )?;
            out.add(body, "question_ref", "question_ref", ChangeOnly)?;
            out.add(
                body,
                "compared_alternatives",
                "compared_alternatives",
                Inherit,
            )?;
            out.add(body, "comparison_refs", "comparison_refs", Inherit)?;
            out.add(body, "evidence_refs", "evidence_refs", Inherit)?;
            out.add(body, "assumption_refs", "assumption_refs", Inherit)?;
            out.closure = false;
            out.add(body, "review_ref", "review_ref", Inherit)?;
        }
        Kind::Review => {
            out.closure = false;
            out.add(body, "subject_ref", "subject_ref", ChangeOnly)?;
            out.add(body, "read_set", "read_set", Inherit)?;
            out.add(body, "materials_examined", "materials_examined", Inherit)?;
            out.add(body, "evidence_refs", "evidence_refs", Inherit)?;
            out.add(body, "assumption_refs", "assumption_refs", Inherit)?;
            out.add(body, "objection_refs", "objection_refs", ChangeOnly)?;
        }
        Kind::Objection => {
            out.closure = false;
            out.add(body, "subject_ref", "subject_ref", ChangeOnly)?;
            if body["evidence_refs"]["state"] == "known" {
                out.add(&body["evidence_refs"], "value", "evidence_refs", Inherit)?;
            }
            out.add(body, "review_ref", "review_ref", ChangeOnly)?;
            // A resolution rests on the evidence it cites (rules version 6).
            out.add(
                body,
                "resolution_evidence_refs",
                "resolution_evidence_refs",
                Inherit,
            )?;
        }
        Kind::Annotation => {
            out.closure = false;
            out.add(body, "subject_refs", "subject_refs", Inherit)?;
            out.add(
                &body["content"],
                "artifact_ref",
                "content.artifact_ref",
                Inherit,
            )?;
        }
        Kind::Retrospective => {
            out.closure = false;
            out.add(body, "case_ref", "case_ref", ChangeOnly)?;
            out.add(body, "read_set", "read_set", Inherit)?;
        }
        Kind::Question => {
            out.closure = false;
            out.add(body, "parent", "parent", ChangeOnly)?;
            out.add(body, "evidence_refs", "evidence_refs", Inherit)?;
        }
        _ => {}
    }
    Ok(out.edges)
}

/// References walked by the promotion dependency closure, in its historic order.
pub(crate) fn closure_links(record: &Record) -> Result<Vec<RevisionRef>, Failure> {
    Ok(dependency_edges(record)?
        .into_iter()
        .filter(|edge| edge.closure)
        .map(|edge| edge.target)
        .collect())
}

fn text(value: &Value) -> Option<&str> {
    value.as_str()
}

/// Withdrawal, retraction and the other end states that remove a record from
/// evidentiary use. Source withdrawal uses the same `status`/`lifecycle` words.
fn end_state(record: &Record) -> Option<Trigger> {
    let body = &record.body;
    let status = text(&body["status"]);
    let lifecycle = text(&body["lifecycle"]);
    let either = |words: &[&str]| {
        status.is_some_and(|s| words.contains(&s)) || lifecycle.is_some_and(|l| words.contains(&l))
    };
    if body["retracted"] == true || either(&["retracted"]) {
        Some(Trigger::Retracted)
    } else if either(&["withdrawn"])
        || matches!(
            text(&body["availability"]),
            Some("missing" | "intentionally_removed")
        )
    {
        Some(Trigger::Withdrawn)
    } else if either(&["retired"]) {
        Some(Trigger::Retired)
    } else if either(&["rejected", "rejected_within_scope"]) {
        Some(Trigger::Rejected)
    } else if either(&["superseded"]) {
        Some(Trigger::Superseded)
    } else {
        None
    }
}

/// A measurement trigger between the cited and current revision. Observation
/// validity is the effective validity computed by `measurement_ops`, so an
/// attributed judgment affects dependents only when it makes validity worse.
fn measurement_change(state: &State, cited: &Record, current: &Record) -> Option<Trigger> {
    match current.header.id.kind() {
        Kind::Observation => {
            let before = effective_validity(state, cited);
            let after = effective_validity(state, current);
            if after == MeasurementValidity::Invalid && before != MeasurementValidity::Invalid {
                Some(Trigger::InvalidMeasurement)
            } else if validity_rank(after) < validity_rank(before) {
                Some(Trigger::ValidityLowered {
                    from: before,
                    to: after,
                })
            } else {
                None
            }
        }
        Kind::Run => (current.body["evidence_validity"] == "invalid"
            && cited.body["evidence_validity"] != "invalid")
            .then_some(Trigger::InvalidMeasurement),
        _ => None,
    }
}

/// Observations whose effective validity can change without a new revision of
/// their own, because a verification they rest on changed.
fn judged(record: &Record) -> bool {
    record.header.id.kind() == Kind::Observation
        && record
            .body
            .get(MEASUREMENT_ASSESSMENT_KEY)
            .is_some_and(|log| !log.is_null())
}

/// Ended records are history, and classification changes are handling
/// metadata: both are tracked but are not part of the case Overview.
fn out_of_use(record: &Record) -> bool {
    end_state(record).is_some()
        || (record.header.id.kind() == Kind::Annotation
            && record.body["role"] == crate::classification::CHANGE_ROLE)
}

/// Lifecycle and attribution fields for kinds without a versioned review basis.
/// Every other body field is treated as substantive, which errs toward review.
fn lifecycle_fields(kind: Kind) -> &'static [&'static str] {
    match kind {
        // Attributed validity judgments are compared through effective
        // validity in `measurement_change`, not as part of the basis.
        Kind::Observation => &[
            "status",
            "lifecycle",
            "retracted",
            "revision_reason",
            "retraction_reason",
            "correction_reason",
            "rationale",
            "corrects",
            MEASUREMENT_ASSESSMENT_KEY,
            crate::origin_ops::CLAIM_ORIGIN_KEY,
            crate::observation_ops::GUARDRAIL_KEY,
        ],
        Kind::Artifact | Kind::Source => &[
            "status",
            "lifecycle",
            "rationale",
            "withdrawal_reason",
            "withdrawals",
            "source_assessment",
        ],
        Kind::Hypothesis => &["lifecycle", "rationale", "alternative_to"],
        Kind::Inquiry => &[
            "lifecycle",
            "rationale",
            "predecessor_ref",
            "plan_digest",
            "prior_exposure",
            "warnings",
        ],
        Kind::Run => &[
            "reconciliation_reason",
            "ordering_receipt",
            "external_ref",
            "retrospective_plan_ref",
        ],
        Kind::Question => &["disposition", "answer", "resolution_reason"],
        Kind::Case => &[
            "lifecycle",
            "closure_outcome",
            "closure",
            "transition_reason",
        ],
        Kind::Review | Kind::Annotation => &["status"],
        Kind::Retrospective => &["status", "successor_ref", "superseded_reason"],
        _ => &[],
    }
}

/// The part of a record a dependent relies on. `scope_option` narrows a
/// criterion's option scope to whether it still covers the dependent's option,
/// so re-scoping to other options is demonstrably irrelevant.
fn substantive(record: &Record, scope_option: Option<Id>) -> Value {
    let mut record = record.clone();
    if record.header.id.kind() == Kind::Criterion
        && let Some(option) = scope_option
    {
        let covers = match &record.body["option_refs"] {
            Value::Null => true,
            value => decode_refs(value)
                .map(|refs| refs.iter().any(|reference| reference.id == option))
                .unwrap_or(false),
        };
        record.body["option_refs"] = json!({ "covers_dependent_option": covers });
    }
    if let Ok(digest) = basis_digest(&record) {
        return json!(digest);
    }
    let mut body = record.body;
    if let Some(object) = body.as_object_mut() {
        for field in lifecycle_fields(record.header.id.kind()) {
            object.remove(*field);
        }
    }
    body
}

fn scope_option(record: &Record) -> Option<Id> {
    let reference = match record.header.id.kind() {
        Kind::Comparison => &record.body["option_ref"],
        Kind::Decision => &record.body["choice"]["option_ref"],
        _ => return None,
    };
    decode_refs(reference)
        .ok()
        .and_then(|refs| refs.first().map(|reference| reference.id))
}

/// Decide whether the change from the cited revision to the current one
/// affects a dependent. `None` means the cited basis is unchanged.
fn edge_change(
    state: &State,
    case: Id,
    scope: Option<Id>,
    edge: &DependencyEdge,
) -> Option<(Change, RevisionRef)> {
    let current = state
        .current(edge.target.id)
        .filter(|record| record.header.case_id == case);
    let (Some(current), Some(cited)) = (current, state.revision(edge.target)) else {
        return Some((Change::from(Trigger::MissingDependency), edge.target));
    };
    // A judgment about a subject stays a record of that subject when the
    // subject is later rejected or superseded; only its claim matters.
    let subject = edge.propagation == Propagation::ChangeOnly && edge.field == "subject_ref";
    revision_change(state, cited, current, scope, subject)
        .map(|change| (change, current.reference()))
}

/// A trigger and, for an evidentiary-use change, when it happened.
#[derive(Clone, Debug, PartialEq)]
struct Change {
    trigger: Trigger,
    since: Option<u64>,
}

impl From<Trigger> for Change {
    fn from(trigger: Trigger) -> Self {
        Self {
            trigger,
            since: None,
        }
    }
}

/// The trigger, if any, that the change from `cited` to `current` (two
/// revisions of one record) raises for a record that cites `cited`, with the
/// sequence of an evidentiary-use change (see [`FreshnessReason`]).
fn revision_change(
    state: &State,
    cited: &Record,
    current: &Record,
    scope: Option<Id>,
    subject: bool,
) -> Option<Change> {
    if current.header.revision == cited.header.revision {
        return None;
    }
    if current.header.id.kind() == Kind::Assumption
        && cited.body["status"] != current.body["status"]
    {
        return Some(Change::from(Trigger::AssumptionDisposition {
            from: text(&cited.body["status"]).map(str::to_owned),
            to: text(&current.body["status"]).map(str::to_owned),
        }));
    }
    // Withdrawals and source judgments are use fields, outside the substantive
    // basis; each new withdrawal or newly raised concern is its own trigger.
    if !subject && let Some(change) = crate::source_ops::use_change(cited, current) {
        return Some(Change {
            trigger: change.trigger,
            since: change.since,
        });
    }
    // Reverifying a reference: a mismatch is its own trigger, and a match
    // that only records the verification (or retains the same bytes) is not a
    // change to what dependents cite.
    if let Some(change) = crate::external_ref::verification_change(cited, current) {
        return change.map(Change::from);
    }
    if let Some(trigger) = end_state(current)
        && end_state(cited).is_none()
        && !subject
    {
        return Some(Change::from(trigger));
    }
    if let Some(trigger) = measurement_change(state, cited, current) {
        return Some(Change::from(trigger));
    }
    if substantive(cited, scope) != substantive(current, scope) {
        return Some(Change::from(Trigger::MaterialRevision));
    }
    None
}

/// The freshness trigger a plain citation of `cited` sees in the record's
/// current same-case revision, or `None` when that later revision is benign
/// to it: lifecycle or attribution metadata, a matching reverification, a
/// Source or Artifact judgment that raises no concern, or a measurement
/// judgment that keeps or raises effective validity. A missing record or
/// revision is [`Trigger::MissingDependency`]. A plain citation was read while
/// the cited revision was current, so every later withdrawal or raised concern
/// is new to it; only the index knows when a dependent was accepted.
pub fn successor_change(state: &State, case: Id, cited: RevisionRef) -> Option<Trigger> {
    let current = state
        .current(cited.id)
        .filter(|record| record.header.case_id == case);
    let (Some(current), Some(historic)) = (current, state.revision(cited)) else {
        return Some(Trigger::MissingDependency);
    };
    revision_change(state, historic, current, None, false).map(|change| change.trigger)
}

/// The current same-case revision of a cited record when every change since
/// the cited revision is benign to a reader of it ([`successor_change`]).
///
/// This is the one "later revision is benign" predicate: the index judges
/// plain dependency edges with it, the promotion gates accept a reviewed or
/// cited revision whose successor it admits, and the `review.stale_read_set`
/// gap rule flags only read-set revisions it rejects.
pub fn benign_successor(state: &State, case: Id, cited: RevisionRef) -> Option<&Record> {
    if successor_change(state, case, cited).is_some() {
        return None;
    }
    state
        .current(cited.id)
        .filter(|record| record.header.case_id == case)
}

/// Checks that a validation review's computed scoped sets and closure still
/// match. Exact-revision changes are handled by the review's edges.
fn review_basis(state: &State, case: Id, record: &Record) -> Vec<FreshnessReason> {
    let body = &record.body;
    if record.header.id.kind() != Kind::Review
        || body["kind"] != "validation"
        || body["status"] != "submitted"
    {
        return Vec::new();
    }
    let unreadable = || {
        vec![FreshnessReason::local(
            Trigger::UnreadableBasis,
            record.reference(),
            Vec::new(),
        )]
    };
    let (Ok(subject), Ok(stored), Ok(read_set)) = (
        crate::input::decode::<RevisionRef>(&body["subject_ref"]),
        crate::input::decode::<Vec<MembershipBasis>>(&body["memberships"]),
        crate::input::decode::<Vec<RevisionRef>>(&body["read_set"]),
    ) else {
        return unreadable();
    };
    let Some(target) = state
        .current(subject.id)
        .filter(|target| target.header.case_id == case)
    else {
        return Vec::new();
    };
    // A substantive subject change is reported by the subject_ref edge. The
    // checks below still apply (rules version 6): an objection raised after
    // the review disputes it whatever else changed since.
    let Ok(now) = promotion_memberships(state, target) else {
        return unreadable();
    };
    let mut reasons = Vec::new();
    let rules: BTreeSet<_> = stored
        .iter()
        .chain(&now)
        .map(|membership| serde_json::to_string(&membership.rule).unwrap_or_default())
        .collect();
    for rule in rules {
        let find = |list: &[MembershipBasis]| {
            list.iter()
                .find(|m| serde_json::to_string(&m.rule).unwrap_or_default() == rule)
                .cloned()
        };
        let (before, after) = (find(&stored), find(&now));
        // A newly open objection disputes the subject (objection policy); a
        // resolved one only means the review read a different set.
        if let (Some(before), Some(after)) = (&before, &after)
            && after.rule == MembershipRule::OpenObjections
        {
            let raised: Vec<_> = after
                .members
                .iter()
                .filter(|member| !before.members.iter().any(|old| old.id == member.id))
                .filter_map(|member| state.current(member.id))
                .collect();
            if !raised.is_empty() {
                reasons.extend(raised.into_iter().map(|objection| {
                    FreshnessReason::local(
                        Trigger::ObjectionRaisedAfterReview,
                        objection.reference(),
                        Vec::new(),
                    )
                }));
                continue;
            }
        }
        if before.as_ref().map(|m| (&m.scope, &m.digest))
            != after.as_ref().map(|m| (&m.scope, &m.digest))
            && let Some(membership) = before.or(after)
        {
            reasons.push(FreshnessReason::local(
                Trigger::MembershipChanged {
                    rule: membership.rule,
                },
                target.reference(),
                Vec::new(),
            ));
        }
    }
    // A record now required to read the subject that the review did not
    // read, whatever else changed in the closure: the walk reads through
    // benign successors and does not stop where a promotion would refuse.
    if let Ok(required) = required_reads(state, target) {
        let read: BTreeSet<Id> = read_set.iter().map(|reference| reference.id).collect();
        for reference in required {
            if reference.id != target.header.id && !read.contains(&reference.id) {
                // The tolerant walk may stand a cited revision in for a
                // changed one; the cause names the current revision.
                let cause = state
                    .current(reference.id)
                    .map_or(reference, Record::reference);
                reasons.push(FreshnessReason::local(
                    Trigger::BasisExpanded,
                    cause,
                    Vec::new(),
                ));
            }
        }
    }
    reasons
}

/// Open critical objections by subject ID.
fn critical_objections(state: &State, case: Id) -> BTreeMap<Id, Vec<RevisionRef>> {
    let mut found: BTreeMap<Id, Vec<RevisionRef>> = BTreeMap::new();
    for record in state.records().filter(|record| {
        record.header.case_id == case && record.header.id.kind() == Kind::Objection
    }) {
        if record.body["status"] == "open"
            && record.body["severity"] == "critical"
            && let Ok(subject) = crate::input::decode::<RevisionRef>(&record.body["subject_ref"])
        {
            found
                .entry(subject.id)
                .or_default()
                .push(record.reference());
        }
    }
    found
}

fn lifecycle(record: &Record) -> Option<String> {
    let body = &record.body;
    text(&body["status"])
        .or_else(|| text(&body["lifecycle"]))
        .or_else(|| text(&body["disposition"]))
        .map(str::to_owned)
}

#[derive(Clone, Debug, PartialEq)]
struct Node {
    reference: RevisionRef,
    /// Acceptance sequence of the assessed revision: when its author read
    /// what it cites.
    read_at: u64,
    lifecycle: Option<String>,
    inactive: bool,
    targets: BTreeSet<Id>,
    local: Vec<FreshnessReason>,
    inherit: Vec<(&'static str, RevisionRef)>,
}

impl Node {
    fn assess(
        state: &State,
        case: Id,
        record: &Record,
        objections: &BTreeMap<Id, Vec<RevisionRef>>,
    ) -> Self {
        let mut local = Vec::new();
        let mut inherit = Vec::new();
        let mut targets = BTreeSet::new();
        let read_at = record.header.acceptance_sequence;
        match dependency_edges(record) {
            Ok(edges) => {
                let scope = scope_option(record);
                for edge in edges {
                    targets.insert(edge.target.id);
                    match edge_change(state, case, scope, &edge) {
                        Some((change, cause)) => {
                            let reason = FreshnessReason {
                                since: change.since,
                                ..FreshnessReason::local(
                                    change.trigger,
                                    cause,
                                    vec![DependencyStep {
                                        record: record.header.id,
                                        field: edge.field.to_owned(),
                                        cites: edge.target,
                                    }],
                                )
                            };
                            // An acknowledged use change leaves the cited
                            // record's own basis to judge.
                            if reason.acknowledged_at(read_at)
                                && edge.propagation == Propagation::Inherit
                            {
                                inherit.push((edge.field, edge.target));
                            }
                            local.push(reason);
                        }
                        None if edge.propagation == Propagation::Inherit => {
                            inherit.push((edge.field, edge.target));
                        }
                        None => {}
                    }
                }
            }
            Err(_) => local.push(FreshnessReason::local(
                Trigger::UnreadableBasis,
                record.reference(),
                Vec::new(),
            )),
        }
        for objection in objections.get(&record.header.id).into_iter().flatten() {
            local.push(FreshnessReason::local(
                Trigger::CriticalObjection,
                *objection,
                Vec::new(),
            ));
        }
        local.extend(review_basis(state, case, record));
        if crate::blindspot_review::check_set_changed(record) {
            local.push(FreshnessReason::local(
                Trigger::CheckSetChanged,
                record.reference(),
                Vec::new(),
            ));
        }
        Self {
            reference: record.reference(),
            read_at,
            lifecycle: lifecycle(record),
            inactive: out_of_use(record),
            targets,
            local,
            inherit,
        }
    }
}

fn merge(reasons: &mut BTreeMap<ReasonKey, FreshnessReason>, reason: FreshnessReason) {
    match reasons.get(&reason.key()) {
        Some(existing) if !reason.precedes(existing) => {}
        _ => {
            reasons.insert(reason.key(), reason);
        }
    }
}

/// Strongly connected components of the inheritance graph, emitted so that
/// every component follows the components it inherits from.
fn components(nodes: &BTreeMap<Id, Node>) -> (Vec<Vec<Id>>, BTreeMap<Id, Vec<Id>>) {
    struct Tarjan<'a> {
        nodes: &'a BTreeMap<Id, Node>,
        index: BTreeMap<Id, usize>,
        low: BTreeMap<Id, usize>,
        stack: Vec<Id>,
        on_stack: BTreeSet<Id>,
        order: Vec<Vec<Id>>,
    }
    impl Tarjan<'_> {
        fn successors(&self, id: Id) -> BTreeSet<Id> {
            self.nodes[&id]
                .inherit
                .iter()
                .map(|(_, target)| target.id)
                .filter(|target| self.nodes.contains_key(target))
                .collect()
        }
        fn visit(&mut self, id: Id) {
            let next = self.index.len();
            self.index.insert(id, next);
            self.low.insert(id, next);
            self.stack.push(id);
            self.on_stack.insert(id);
            for successor in self.successors(id) {
                if !self.index.contains_key(&successor) {
                    self.visit(successor);
                    let low = self.low[&id].min(self.low[&successor]);
                    self.low.insert(id, low);
                } else if self.on_stack.contains(&successor) {
                    let low = self.low[&id].min(self.index[&successor]);
                    self.low.insert(id, low);
                }
            }
            if self.low[&id] == self.index[&id] {
                let mut component = Vec::new();
                while let Some(member) = self.stack.pop() {
                    self.on_stack.remove(&member);
                    component.push(member);
                    if member == id {
                        break;
                    }
                }
                component.sort();
                self.order.push(component);
            }
        }
    }
    let mut tarjan = Tarjan {
        nodes,
        index: BTreeMap::new(),
        low: BTreeMap::new(),
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        order: Vec::new(),
    };
    for id in nodes.keys() {
        if !tarjan.index.contains_key(id) {
            tarjan.visit(*id);
        }
    }
    let mut cyclic = BTreeMap::new();
    for component in &tarjan.order {
        let looped = component.len() > 1
            || nodes[&component[0]]
                .inherit
                .iter()
                .any(|(_, target)| target.id == component[0]);
        if looped {
            for member in component {
                cyclic.insert(*member, component.clone());
            }
        }
    }
    (tarjan.order, cyclic)
}

fn resolve(
    node: &Node,
    cycle: Option<&Vec<Id>>,
    results: &BTreeMap<Id, RecordFreshness>,
) -> RecordFreshness {
    let mut reasons = BTreeMap::new();
    for reason in &node.local {
        merge(&mut reasons, reason.clone());
    }
    if cycle.is_some() {
        merge(
            &mut reasons,
            FreshnessReason::local(Trigger::CircularBasis, node.reference, Vec::new()),
        );
    }
    for (field, cited) in &node.inherit {
        if cycle.is_some_and(|members| members.contains(&cited.id)) {
            continue;
        }
        let inherited = results
            .get(&cited.id)
            .into_iter()
            .flat_map(|result| result.reasons.iter().chain(&result.acknowledged));
        for inherited in inherited {
            let mut path = vec![DependencyStep {
                record: node.reference.id,
                field: (*field).to_owned(),
                cites: *cited,
            }];
            path.extend(inherited.path.iter().cloned());
            merge(
                &mut reasons,
                FreshnessReason {
                    path,
                    ..inherited.clone()
                },
            );
        }
    }
    let (acknowledged, reasons): (Vec<_>, Vec<_>) = reasons
        .into_values()
        .partition(|reason| reason.acknowledged_at(node.read_at));
    RecordFreshness {
        record: node.reference,
        lifecycle: node.lifecycle.clone(),
        freshness: reasons
            .iter()
            .map(|reason| reason.freshness)
            .max()
            .unwrap_or(Freshness::Current),
        reasons,
        acknowledged,
    }
}

/// Dependency index and effective freshness for every current record of one
/// case at one snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct FreshnessIndex {
    case_id: Id,
    sequence: u64,
    nodes: BTreeMap<Id, Node>,
    dependents: BTreeMap<Id, BTreeSet<Id>>,
    objections: BTreeMap<Id, Vec<RevisionRef>>,
    cyclic: BTreeMap<Id, Vec<Id>>,
    results: BTreeMap<Id, RecordFreshness>,
}

impl FreshnessIndex {
    /// Full recomputation from the snapshot, independent of any prior index.
    pub fn build(state: &State, case_id: Id) -> Self {
        let objections = critical_objections(state, case_id);
        let nodes: BTreeMap<Id, Node> = state
            .records()
            .filter(|record| record.header.case_id == case_id)
            .map(|record| {
                (
                    record.header.id,
                    Node::assess(state, case_id, record, &objections),
                )
            })
            .collect();
        let mut dependents: BTreeMap<Id, BTreeSet<Id>> = BTreeMap::new();
        for (id, node) in &nodes {
            for target in &node.targets {
                dependents.entry(*target).or_default().insert(*id);
            }
        }
        let (order, cyclic) = components(&nodes);
        let mut results = BTreeMap::new();
        for id in order.iter().flatten() {
            let result = resolve(&nodes[id], cyclic.get(id), &results);
            results.insert(*id, result);
        }
        Self {
            case_id,
            sequence: state.sequence(),
            nodes,
            dependents,
            objections,
            cyclic,
            results,
        }
    }

    /// Incremental recomputation for a later snapshot of the same case. Only
    /// records changed since the indexed sequence, records that cite them,
    /// validation reviews, objection subjects and their transitive dependents
    /// are reassessed. The result equals `build(state, case_id)`.
    pub fn update(&mut self, state: &State) -> Result<(), Failure> {
        if state.sequence() < self.sequence {
            return Err(Failure::new(
                ErrorCode::IntegrityFailure,
                "The freshness index is newer than the supplied snapshot.",
            ));
        }
        let case = self.case_id;
        let changed: BTreeSet<Id> = state
            .records()
            .filter(|record| {
                record.header.case_id == case && record.header.acceptance_sequence > self.sequence
            })
            .map(|record| record.header.id)
            .collect();
        self.sequence = state.sequence();
        if changed.is_empty() {
            return Ok(());
        }
        let objections = critical_objections(state, case);
        let mut dirty = changed.clone();
        for subject in self.objections.keys().chain(objections.keys()) {
            if self.objections.get(subject) != objections.get(subject) {
                dirty.insert(*subject);
            }
        }
        self.objections = objections;
        for id in &changed {
            if let Some(old) = self.nodes.get(id) {
                for target in &old.targets {
                    if let Some(set) = self.dependents.get_mut(target) {
                        set.remove(id);
                        if set.is_empty() {
                            self.dependents.remove(target);
                        }
                    }
                }
            }
            let record = state.current(*id).ok_or_else(|| {
                Failure::new(ErrorCode::IntegrityFailure, "Changed record disappeared.")
            })?;
            let node = Node::assess(state, case, record, &self.objections);
            for target in &node.targets {
                self.dependents.entry(*target).or_default().insert(*id);
            }
            self.nodes.insert(*id, node);
        }
        for id in &changed {
            dirty.extend(self.dependents.get(id).into_iter().flatten().copied());
        }
        // Effective validity reads the current state of cited verifications, so
        // dependents of judged Observations are reassessed on every change.
        for record in state
            .records()
            .filter(|record| record.header.case_id == case && judged(record))
        {
            dirty.extend(self.dependents(record.header.id).collect::<Vec<_>>());
        }
        dirty.extend(
            self.nodes
                .keys()
                .filter(|id| id.kind() == Kind::Review)
                .copied(),
        );
        let mut seeds = changed;
        for id in dirty {
            if seeds.contains(&id) {
                continue;
            }
            let Some(record) = state
                .current(id)
                .filter(|record| record.header.case_id == case)
            else {
                continue;
            };
            let node = Node::assess(state, case, record, &self.objections);
            if self.nodes.get(&id) != Some(&node) {
                seeds.insert(id);
                self.nodes.insert(id, node);
            }
        }
        let (order, cyclic) = components(&self.nodes);
        for id in self.cyclic.keys().chain(cyclic.keys()) {
            if self.cyclic.get(id) != cyclic.get(id) {
                seeds.insert(*id);
            }
        }
        self.cyclic = cyclic;
        let mut affected = BTreeSet::new();
        let mut pending: Vec<Id> = seeds.into_iter().collect();
        while let Some(id) = pending.pop() {
            if affected.insert(id) {
                pending.extend(self.dependents.get(&id).into_iter().flatten().copied());
            }
        }
        for id in order.iter().flatten().filter(|id| affected.contains(id)) {
            let result = resolve(&self.nodes[id], self.cyclic.get(id), &self.results);
            self.results.insert(*id, result);
        }
        Ok(())
    }

    pub fn case_id(&self) -> Id {
        self.case_id
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Effective freshness of one current record.
    pub fn get(&self, id: Id) -> Option<&RecordFreshness> {
        self.results.get(&id)
    }

    /// Effective freshness of any revision of a record of this case: the
    /// indexed result for a current revision, otherwise the historical revision
    /// assessed against this snapshot. History is not rewritten either way.
    pub fn assess(&self, state: &State, record: &Record) -> RecordFreshness {
        if let Some(result) = self.results.get(&record.header.id)
            && result.record == record.reference()
        {
            return result.clone();
        }
        let node = Node::assess(state, self.case_id, record, &self.objections);
        resolve(&node, None, &self.results)
    }

    /// Every current record, in ID order.
    pub fn records(&self) -> impl Iterator<Item = &RecordFreshness> {
        self.results.values()
    }

    /// Current records citing `id` through any edge.
    pub fn dependents(&self, id: Id) -> impl Iterator<Item = Id> + '_ {
        self.dependents.get(&id).into_iter().flatten().copied()
    }

    /// Aggregate over the named records, for example those included in a report.
    pub fn summarize(&self, ids: impl IntoIterator<Item = Id>) -> FreshnessSummary {
        let mut summary = FreshnessSummary {
            case_id: self.case_id,
            sequence: self.sequence,
            rules_version: FRESHNESS_RULES_VERSION,
            freshness: Freshness::Current,
            current: 0,
            needs_review: 0,
            disputed: 0,
            affected: Vec::new(),
        };
        let ids: BTreeSet<Id> = ids.into_iter().collect();
        for result in ids.iter().filter_map(|id| self.results.get(id)) {
            match result.freshness {
                Freshness::Current => summary.current += 1,
                Freshness::NeedsReview => summary.needs_review += 1,
                Freshness::Disputed => summary.disputed += 1,
            }
            if result.freshness != Freshness::Current {
                summary.affected.push(result.record);
            }
            summary.freshness = summary.freshness.max(result.freshness);
        }
        summary
    }

    /// Case Overview: every current record still in evidentiary use.
    /// Withdrawn, retracted, retired and superseded records are history.
    pub fn overview(&self) -> FreshnessSummary {
        self.summarize(
            self.nodes
                .iter()
                .filter(|(_, node)| !node.inactive)
                .map(|(id, _)| *id),
        )
    }
}

#[cfg(test)]
mod tests {
    mod blindspot_review;
    mod judgments;
    mod promotion_agreement;
    mod source_use;

    use super::*;
    use crate::{
        catalog::standard_catalog,
        command::Command,
        header::RecordHeader,
        header::SystemClock,
        identity::{IdSource, SystemIdSource},
        review_ops::required_read_set,
        state::{Acceptance, AcceptedTransaction, validate_and_apply},
        wire::RecordChange,
    };

    /// A case built through real operations where the core owns them, and
    /// through accepted images where ingest belongs to the store.
    struct Case {
        state: State,
        case: Id,
        index: FreshnessIndex,
    }

    struct Fixture {
        source: RevisionRef,
        artifact: RevisionRef,
        observation: RevisionRef,
        question: RevisionRef,
        hypothesis: RevisionRef,
        assessment: RevisionRef,
        conclusion: RevisionRef,
        conclusion_review: RevisionRef,
        note: RevisionRef,
        option: RevisionRef,
        criterion: RevisionRef,
        comparison: RevisionRef,
        decision: RevisionRef,
        decision_review: RevisionRef,
        assumption: RevisionRef,
        other_criterion: RevisionRef,
    }

    impl Case {
        fn new() -> Self {
            let mut state = State::default();
            let accepted = apply(
                &mut state,
                None,
                vec![op(
                    "case.create",
                    json!({"title":"Queue choice","mode":"record"}),
                )],
                &[],
            )
            .unwrap();
            let case = accepted.case_id;
            let index = FreshnessIndex::build(&state, case);
            Self { state, case, index }
        }

        /// Apply, then require the incremental index to equal a full rebuild.
        fn settle(&mut self) {
            self.index.update(&self.state).unwrap();
            assert_eq!(self.index, FreshnessIndex::build(&self.state, self.case));
        }

        fn run(&mut self, ops: Vec<Value>, preconditions: &[RevisionRef]) -> AcceptedTransaction {
            let accepted = apply(&mut self.state, Some(self.case), ops, preconditions).unwrap();
            self.settle();
            accepted
        }

        fn one(&mut self, name: &str, payload: Value) -> RevisionRef {
            self.run(vec![op(name, payload)], &[]).records[0].reference()
        }

        fn image(&mut self, kind: Kind, body: Value, replaces: Option<RevisionRef>) -> RevisionRef {
            let sequence = self.state.sequence() + 1;
            let mut header = self
                .state
                .current(replaces.map_or(self.case, |reference| reference.id))
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
                    case_id: self.case,
                    sequence,
                    records: vec![record],
                    created,
                    updated,
                    warnings: vec![],
                })
                .unwrap();
            self.settle();
            reference
        }

        fn current(&self, reference: RevisionRef) -> RevisionRef {
            self.state.current(reference.id).unwrap().reference()
        }

        fn freshness(&self, reference: RevisionRef) -> &RecordFreshness {
            self.index.get(reference.id).unwrap()
        }

        fn validate(&mut self, subject: RevisionRef) -> RevisionRef {
            let record = self.state.revision(subject).unwrap();
            let basis = basis_digest(record).unwrap();
            let reads = required_read_set(&self.state, record).unwrap();
            self.run(vec![op("review.submit", json!({
                "kind":"validation","subject_ref":subject,"basis_digest":basis,"read_set":reads,
                "method":"Inspect the cited measurements and sources.","materials_examined":[],
                "shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":["One fixture."],
                "context_isolation":"fresh_context_unblinded","context":{
                    "saw_original_conclusion":{"state":"known","value":true},
                    "gathered_new_evidence":{"state":"known","value":false},
                    "method_or_source_changes":[],"activity":"Read the recorded basis.",
                    "dependencies":"shared","dependence_basis":"Same recorded data."}
            }))], &[]).records[0].reference()
        }

        fn fixture(&mut self) -> Fixture {
            let source = self.image(Kind::Source, json!({
                "producer":null,"publisher":null,"identity":"bench.log",
                "acquisition":{"channel":"local_file","acquired_at":null,"method":null,"context":null,"environment":null},
                "access":"local_only","verification_limits":[]
            }), None);
            let digest = format!("sha256:{}", "1".repeat(64));
            let artifact = self.image(Kind::Artifact, json!({
                "digest":digest,"size_bytes":64,"format":"text/plain","source":source,
                "transforms":[],"availability":"retained","integrity":"verified","sensitivity":"internal"
            }), None);
            let observation = self.image(Kind::Observation, json!({
                "value":{"kind":"measurement","value":"20","unit":"ms"},
                "basis":{"kind":"source","artifact_ref":artifact,
                    "selector":{"schema_version":"inv.selector.v1","artifact_digest":digest,"selector":{"type":"whole"},"validation":"verified"},
                    "text_quote":{"exact":null,"prefix":null,"suffix":null,"state":"not_applicable"}},
                "extraction_method":"p99 over the retained samples","validity":"valid","quality":[],"limitations":[],
                "evidence_kind":"observational","variables_changed":[],"observed_at":null,
            }), None);
            let question = self.one(
                "question.create",
                json!({"importance":"normal","wording":"Which queue should carry the jobs?"}),
            );
            let hypothesis = self.one("hypothesis.create", json!({
                "question_ref":question,"statement":"The managed queue meets the latency limit.",
                "applicability":"Projected load","assumption_refs":[],"possible_falsifiers":["p99 above 25 ms"],"rival_refs":[]
            }));
            let assessment = self.one(
                "assessment.add",
                json!({
                    "subject_ref":hypothesis,"evidence_refs":[observation],"relation":"supports",
                    "rationale":"The retained p99 is below the limit."
                }),
            );
            let candidate = self.one("conclusion.create", json!({
                "statement":"The managed queue met the latency limit in the benchmark.",
                "scope":"The recorded benchmark.","significance":"Informs the queue choice.",
                "support_refs":[observation],"contradiction_refs":[],"caveats":["One benchmark."]
            }));
            let mut reads =
                required_read_set(&self.state, self.state.revision(candidate).unwrap()).unwrap();
            reads.push(candidate);
            let ready = self
                .run(
                    vec![op(
                        "conclusion.request_review",
                        json!({"conclusion_ref":candidate}),
                    )],
                    &reads,
                )
                .records[0]
                .reference();
            let conclusion_review = self.validate(ready);
            let mut reads =
                required_read_set(&self.state, self.state.revision(ready).unwrap()).unwrap();
            reads.extend([ready, conclusion_review]);
            let conclusion = self
                .run(
                    vec![op(
                        "conclusion.validate",
                        json!({"conclusion_ref":ready,"review_ref":conclusion_review}),
                    )],
                    &reads,
                )
                .records[0]
                .reference();
            let note = self.one("annotation.add", json!({
                "subject_refs":[conclusion],"content":{"kind":"text","text":"Summary: the managed queue met the limit."}
            }));
            let option = self.one("option.create", json!({
                "question_ref":question,"description":"Managed queue","dependencies":[],"tradeoff_notes":[]
            }));
            let option = self.one(
                "option.disposition",
                json!({"option_ref":option,"lifecycle":"eligible","reason":"Feasible"}),
            );
            let other = self.one("option.create", json!({
                "question_ref":question,"description":"Broker","dependencies":[],"tradeoff_notes":[]
            }));
            let other = self.one(
                "option.disposition",
                json!({"option_ref":other,"lifecycle":"eligible","reason":"Feasible"}),
            );
            let mut scoped = criterion(question, "2", "hours");
            scoped["name"] = json!("Operator effort");
            scoped["scope"] =
                json!({"kind":"selected_options","question_ref":question,"option_refs":[other]});
            let other_criterion = self.one("criterion.create", scoped);
            let criterion = self.one("criterion.create", criterion(question, "25", "ms"));
            let assumption = self.one(
                "assumption.create",
                json!({
                    "affected_refs":[question],"claim":"The benchmark load matches production.",
                    "consequence":"Latency results transfer.","materiality":"nonmaterial"
                }),
            );
            let comparison = self.one("comparison.add", json!({
                "option_ref":option,"criterion_ref":criterion,"outcome":"meets","method":"Benchmark",
                "value":{"decimal":"20","unit":"ms"},"evidence_refs":[observation],"assumption_refs":[assumption],
                "uncertainty":"One run.","rationale":"Measured p99 is below the limit."
            }));
            let proposed = self.one("decision.propose", json!({
                "question_ref":question,"choice":{"kind":"select","option_ref":option},
                "compared_alternatives":[option],"comparison_refs":[comparison],"hard_conditions":[],
                "evidence_refs":[],"assumption_refs":[],"tradeoffs":[],"consequences":["Adopt the managed queue."],
                "reopen_trigger":"Latency evidence changes.","reversal_conditions":[]
            }));
            let decision_review = self.validate(proposed);
            let decision = self.one(
                "decision.accept",
                json!({"decision_ref":proposed,"review_ref":decision_review}),
            );
            Fixture {
                source,
                artifact,
                observation,
                question,
                hypothesis,
                assessment,
                conclusion,
                conclusion_review,
                note,
                option,
                criterion,
                comparison,
                decision,
                decision_review,
                assumption,
                other_criterion,
            }
        }
    }

    fn op(name: &str, payload: Value) -> Value {
        json!({"op":name,"payload":payload})
    }

    fn criterion(question: RevisionRef, limit: &str, unit: &str) -> Value {
        json!({
            "name":"Latency","kind":"hard","preferred_direction":"lower",
            "scale":{"anchors":[],"description":"p99 latency","unit":unit},
            "scope":{"kind":"all_options","question_ref":question},
            "threshold":{"kind":"measured","operator":"at_most","value":{"decimal":limit,"unit":unit}}
        })
    }

    fn apply(
        state: &mut State,
        case: Option<Id>,
        ops: Vec<Value>,
        preconditions: &[RevisionRef],
    ) -> Result<AcceptedTransaction, Failure> {
        let sequence = state.sequence() + 1;
        let command: Command = serde_json::from_value(json!({
            "schema_version":"inv.command.v1","case_id":case,"actor":"agent:test",
            "command_id":format!("fresh-{sequence}"),"preconditions":preconditions,"operations":ops,
        }))
        .unwrap();
        let accepted = validate_and_apply(
            state,
            &command,
            &standard_catalog(),
            Acceptance {
                sequence,
                clock: &mut SystemClock,
                ids: &mut SystemIdSource,
            },
        )?;
        state.apply(&accepted)?;
        Ok(accepted)
    }

    #[test]
    fn fixture_starts_current_and_lifecycle_only_validation_keeps_reviews_current() {
        let mut case = Case::new();
        let f = case.fixture();
        for result in case.index.records() {
            assert_eq!(result.freshness, Freshness::Current, "{result:?}");
        }
        assert_eq!(case.freshness(f.conclusion).label(), "validated / current");
        assert_eq!(case.freshness(f.decision).label(), "accepted / current");
        assert_eq!(case.index.overview().freshness, Freshness::Current);
    }

    fn snapshot(case: &Case) -> BTreeMap<Id, RecordFreshness> {
        case.index
            .records()
            .map(|result| (result.record.id, result.clone()))
            .collect()
    }

    fn only_reason(result: &RecordFreshness) -> &FreshnessReason {
        assert_eq!(result.reasons.len(), 1, "{result:#?}");
        &result.reasons[0]
    }

    #[test]
    fn source_withdrawal_reaches_every_dependent_layer_with_a_reason_chain() {
        let mut case = Case::new();
        let f = case.fixture();
        let withdrawn = case.one(
            "source.withdraw",
            json!({
                "source_ref": f.source, "reason": "retracted_by_origin", "scope": {"extent":"entire"},
                "rationale": "The publisher retracted the benchmark.", "evidence_refs": []
            }),
        );

        let affected = [
            f.artifact,
            f.observation,
            f.assessment,
            f.conclusion,
            f.conclusion_review,
            f.note,
            f.comparison,
            f.decision,
            f.decision_review,
        ];
        for reference in affected {
            let result = case.freshness(reference);
            assert_eq!(result.freshness, Freshness::NeedsReview, "{result:#?}");
            let reason = only_reason(result);
            assert_eq!(reason.trigger, Trigger::Withdrawn);
            assert_eq!(reason.cause, withdrawn);
            assert_eq!(reason.path[0].record, reference.id);
            assert_eq!(reason.path.last().unwrap().cites, f.source);
        }
        let observation = only_reason(case.freshness(f.observation));
        assert_eq!(
            observation
                .path
                .iter()
                .map(|step| (step.record, step.field.as_str(), step.cites))
                .collect::<Vec<_>>(),
            vec![
                (f.observation.id, "basis.artifact_ref", f.artifact),
                (f.artifact.id, "source", f.source),
            ]
        );
        let assessment = only_reason(case.freshness(f.assessment));
        assert_eq!(assessment.path[0].field, "evidence_refs");
        assert_eq!(assessment.path.len(), 3);
        assert_eq!(
            case.freshness(f.conclusion).label(),
            "validated / needs_review"
        );
        assert_eq!(
            case.freshness(f.decision).label(),
            "accepted / needs_review"
        );
        for unaffected in [
            f.question,
            f.hypothesis,
            f.option,
            f.criterion,
            f.assumption,
        ] {
            assert_eq!(case.freshness(unaffected).freshness, Freshness::Current);
        }
        // History is not rewritten: the withdrawn Source keeps its own record.
        assert_eq!(
            case.freshness(f.source).lifecycle.as_deref(),
            Some("withdrawn")
        );

        let overview = case.index.overview();
        assert_eq!(overview.freshness, Freshness::NeedsReview);
        assert_eq!(overview.needs_review, affected.len());
        assert!(!overview.affected.contains(&withdrawn));
        let report = case
            .index
            .summarize([f.conclusion.id, f.decision.id, f.question.id]);
        assert_eq!(
            (report.freshness, report.current, report.needs_review),
            (Freshness::NeedsReview, 1, 2)
        );
        assert_eq!(report.affected, {
            let mut expected = vec![case.current(f.conclusion), case.current(f.decision)];
            expected.sort_by_key(|reference| reference.id);
            expected
        });
    }

    #[test]
    fn observation_retraction_and_invalid_measurement_name_their_triggers() {
        let mut case = Case::new();
        let f = case.fixture();
        let mut body = case.state.revision(f.observation).unwrap().body.clone();
        body["validity"] = json!("invalid");
        let invalid = case.image(Kind::Observation, body, Some(f.observation));
        let reason = only_reason(case.freshness(f.comparison));
        assert_eq!(reason.trigger, Trigger::InvalidMeasurement);
        assert_eq!(reason.cause, invalid);
        let retracted = case.one(
            "observation.retract",
            json!({"observation_ref":invalid,"revision_reason":"Outside the declared scope."}),
        );
        let reason = only_reason(case.freshness(f.conclusion));
        assert_eq!(reason.cause, retracted);
        assert_eq!(reason.trigger, Trigger::Retracted);
        assert_eq!(reason.path.len(), 1);
        assert_eq!(case.freshness(f.artifact).freshness, Freshness::Current);
        assert_eq!(case.freshness(f.decision).freshness, Freshness::NeedsReview);
    }

    fn amend(case: &mut Case, criterion: RevisionRef, replacement: Value) -> RevisionRef {
        case.one(
            "criterion.amend",
            json!({"criterion_ref":criterion,"reason":"Revised requirement","replacement":replacement}),
        )
    }

    #[test]
    fn criterion_threshold_weight_and_unit_changes_require_review() {
        for (limit, unit, weighting) in [
            ("30", "ms", None),
            ("25", "s", None),
            ("25", "ms", Some("Latency dominates.")),
        ] {
            let mut case = Case::new();
            let f = case.fixture();
            let mut replacement = criterion(f.question, limit, unit);
            replacement["weighting_rationale"] = json!(weighting);
            let revised = amend(&mut case, f.criterion, replacement);
            let reason = only_reason(case.freshness(f.comparison));
            assert_eq!(reason.trigger, Trigger::MaterialRevision);
            assert_eq!(reason.cause, revised);
            assert_eq!(reason.path.len(), 1);
            assert_eq!(reason.path[0].field, "criterion_ref");
            assert_eq!(reason.path[0].cites, f.criterion);
            for reference in [f.decision, f.decision_review] {
                assert_eq!(case.freshness(reference).freshness, Freshness::NeedsReview);
            }
            assert_eq!(
                case.freshness(f.decision).label(),
                "accepted / needs_review"
            );
            for reference in [f.conclusion, f.conclusion_review, f.assessment, f.note] {
                assert_eq!(case.freshness(reference).freshness, Freshness::Current);
            }
        }
    }

    #[test]
    fn criterion_changes_outside_the_dependency_map_are_irrelevant() {
        let mut case = Case::new();
        let f = case.fixture();
        // A criterion scoped to another option is outside the decision's map.
        let mut replacement = criterion(f.question, "3", "hours");
        replacement["name"] = json!("Operator effort");
        let other = case.state.revision(f.other_criterion).unwrap().body["option_refs"].clone();
        replacement["scope"] =
            json!({"kind":"selected_options","question_ref":f.question,"option_refs":other});
        amend(&mut case, f.other_criterion, replacement);
        for result in case.index.records() {
            assert_eq!(result.freshness, Freshness::Current, "{result:#?}");
        }
        // Re-scoping the compared criterion still covers the compared option.
        let mut scoped = criterion(f.question, "25", "ms");
        let mut options =
            case.state.revision(f.other_criterion).unwrap().body["option_refs"].clone();
        options.as_array_mut().unwrap().push(json!(f.option));
        scoped["scope"] =
            json!({"kind":"selected_options","question_ref":f.question,"option_refs":options});
        amend(&mut case, f.criterion, scoped);
        assert_eq!(case.freshness(f.comparison).freshness, Freshness::Current);
    }

    #[test]
    fn unrelated_mutations_leave_existing_reviews_current() {
        let mut case = Case::new();
        let f = case.fixture();
        let before = snapshot(&case);
        let question = case.one(
            "question.create",
            json!({"importance":"normal","wording":"Which region hosts the queue?"}),
        );
        let hypothesis = case.one("hypothesis.create", json!({
            "question_ref":question,"statement":"The nearest region suffices.",
            "applicability":"Current users","assumption_refs":[],"possible_falsifiers":["Latency above limit"],"rival_refs":[]
        }));
        case.one(
            "assumption.create",
            json!({
                "affected_refs":[hypothesis],"claim":"Users stay in one region.",
                "consequence":"Region choice holds.","materiality":"material"
            }),
        );
        case.one("objection.raise", json!({
            "subject_ref":hypothesis,"statement":"Users are spread out.","scope":"Region choice.",
            "severity":"critical","evidence_refs":{"state":"unknown"}
        }));
        case.one(
            "annotation.add",
            json!({"subject_refs":[question],"content":{"kind":"text","text":"Pending."}}),
        );
        let mut body = case.state.revision(f.observation).unwrap().body.clone();
        body["value"] = json!({"kind":"measurement","value":"31","unit":"ms"});
        case.image(Kind::Observation, body, None);
        let after = snapshot(&case);
        for (id, result) in &before {
            assert_eq!(after.get(id), Some(result));
        }
        for reference in [f.conclusion_review, f.decision_review] {
            assert_eq!(case.freshness(reference).freshness, Freshness::Current);
        }
        assert_eq!(case.index.overview().disputed, 1);
    }

    #[test]
    fn assumption_disposition_changes_reach_comparisons_and_decisions() {
        let mut case = Case::new();
        let f = case.fixture();
        let assessed = case.one(
            "assumption.assess",
            json!({"assumption_ref":f.assumption,"evidence_refs":[],"rationale":"Production load is burstier.","status":"challenged"}),
        );
        let reason = only_reason(case.freshness(f.comparison));
        assert_eq!(
            reason.trigger,
            Trigger::AssumptionDisposition {
                from: Some("unassessed".into()),
                to: Some("challenged".into())
            }
        );
        assert_eq!(reason.cause, assessed);
        assert_eq!(case.freshness(f.decision).freshness, Freshness::NeedsReview);
        assert_eq!(case.freshness(f.conclusion).freshness, Freshness::Current);
    }

    #[test]
    fn objections_dispute_dependents_and_material_objections_reopen_reviews() {
        let mut case = Case::new();
        let f = case.fixture();
        let objection = case.one("objection.raise", json!({
            "subject_ref":f.comparison,"statement":"The samples exclude the warm-up.","scope":"The measurement.",
            "severity":"critical","evidence_refs":{"state":"unknown"}
        }));
        for reference in [f.comparison, f.decision, f.decision_review] {
            let result = case.freshness(reference);
            assert_eq!(result.freshness, Freshness::Disputed, "{result:#?}");
            assert!(
                result
                    .reasons
                    .iter()
                    .all(|reason| reason.cause == objection)
            );
        }
        assert!(only_reason(case.freshness(f.comparison)).path.is_empty());
        assert_eq!(case.freshness(f.decision).label(), "accepted / disputed");
        assert_eq!(case.freshness(f.conclusion).freshness, Freshness::Current);
        case.one("objection.resolve", json!({
            "objection_ref":objection,"reason":"Warm-up samples were excluded by design.","evidence_refs":[f.observation]
        }));
        for result in case.index.records() {
            assert_eq!(result.freshness, Freshness::Current, "{result:#?}");
        }
        // An objection raised after the validation review disputes the
        // validated conclusion (objection policy), not merely its review.
        let raised = case.one("objection.raise", json!({
            "subject_ref":f.conclusion,"statement":"One benchmark is thin.","scope":"The conclusion.",
            "severity":"material","evidence_refs":{"state":"unknown"}
        }));
        let review = only_reason(case.freshness(f.conclusion_review));
        assert_eq!(review.trigger, Trigger::ObjectionRaisedAfterReview);
        assert_eq!(review.cause, raised);
        let conclusion = only_reason(case.freshness(f.conclusion));
        assert_eq!(conclusion.path[0].field, "review_ref");
        assert_eq!(case.freshness(f.conclusion).label(), "validated / disputed");
        assert_eq!(
            crate::review_ops::effective_disposition(
                &case.state,
                case.state.current(f.conclusion.id).unwrap()
            ),
            crate::review::EffectiveDisposition::Disputed
        );
        assert_eq!(case.freshness(f.note).freshness, Freshness::Disputed);
        assert_eq!(case.freshness(f.decision).freshness, Freshness::Current);
        assert_eq!(case.index.overview().disputed, 3);
        // Resolving it restores the open set the review captured.
        case.one("objection.resolve", json!({
            "objection_ref":raised,"reason":"A second benchmark agrees.","evidence_refs":[f.observation]
        }));
        assert_eq!(case.freshness(f.conclusion).label(), "validated / current");
    }

    #[test]
    fn one_update_across_many_transactions_equals_a_full_build() {
        let mut case = Case::new();
        let f = case.fixture();
        let mut lagging = case.index.clone();
        amend(&mut case, f.criterion, criterion(f.question, "30", "ms"));
        case.one("objection.raise", json!({
            "subject_ref":f.conclusion,"statement":"One benchmark is thin.","scope":"The conclusion.",
            "severity":"critical","evidence_refs":{"state":"unknown"}
        }));
        case.one(
            "observation.retract",
            json!({"observation_ref":f.observation,"revision_reason":"Wrong host."}),
        );
        lagging.update(&case.state).unwrap();
        assert_eq!(lagging, FreshnessIndex::build(&case.state, case.case));
        assert_eq!(lagging.sequence(), case.state.sequence());
        let mut earlier = State::default();
        apply(
            &mut earlier,
            None,
            vec![op("case.create", json!({"title":"Other","mode":"record"}))],
            &[],
        )
        .unwrap();
        assert!(lagging.update(&earlier).is_err());
    }

    #[test]
    fn ending_a_subject_does_not_invalidate_judgments_about_it() {
        let mut case = Case::new();
        let f = case.fixture();
        let mut body = case.state.revision(f.hypothesis).unwrap().body.clone();
        body["lifecycle"] = json!("rejected_within_scope");
        case.image(Kind::Hypothesis, body, Some(f.hypothesis));
        assert_eq!(case.freshness(f.assessment).freshness, Freshness::Current);
        let mut body = case.state.revision(f.hypothesis).unwrap().body.clone();
        body["statement"] = json!("A different claim.");
        let amended = case.image(Kind::Hypothesis, body, Some(case.current(f.hypothesis)));
        let reason = only_reason(case.freshness(f.assessment));
        assert_eq!(
            (&reason.trigger, reason.cause),
            (&Trigger::MaterialRevision, amended)
        );
    }

    #[test]
    fn circular_inheritance_is_reported_deterministically() {
        let mut case = Case::new();
        let first = SystemIdSource.next_id(Kind::Annotation).unwrap();
        let second = SystemIdSource.next_id(Kind::Annotation).unwrap();
        let note = |subject: Id| json!({"subject_refs":[{"id":subject,"revision":1}],"content":{"kind":"text","text":"Loop."}});
        let mut header = case.state.current(case.case).unwrap().header.clone();
        header.acceptance_sequence = case.state.sequence() + 1;
        let records = [(first, second), (second, first)].map(|(id, subject)| Record {
            header: RecordHeader {
                id,
                ..header.clone()
            },
            body: note(subject),
        });
        let created = records
            .iter()
            .map(|record| RecordChange {
                client_ref: None,
                id: record.header.id,
                revision: 1,
            })
            .collect();
        case.state
            .apply(&AcceptedTransaction {
                case_id: case.case,
                sequence: header.acceptance_sequence,
                records: records.to_vec(),
                created,
                updated: vec![],
                warnings: vec![],
            })
            .unwrap();
        case.settle();
        for id in [first, second] {
            let reason = only_reason(case.index.get(id).unwrap());
            assert_eq!(reason.trigger, Trigger::CircularBasis);
            assert!(reason.path.is_empty());
        }
    }

    #[test]
    fn index_edges_match_the_promotion_closure_and_reverse_lookups() {
        let mut case = Case::new();
        let f = case.fixture();
        let dependents: BTreeSet<Id> = case.index.dependents(f.observation.id).collect();
        for reference in [
            f.assessment,
            f.conclusion,
            f.comparison,
            f.conclusion_review,
            f.decision_review,
        ] {
            assert!(dependents.contains(&reference.id), "{reference:?}");
        }
        let decision = case.state.current(f.decision.id).unwrap();
        let edges = dependency_edges(decision).unwrap();
        assert!(
            edges
                .iter()
                .any(|edge| edge.field == "review_ref" && edge.target == f.decision_review)
        );
        assert!(
            !closure_links(decision)
                .unwrap()
                .contains(&f.decision_review)
        );
        assert_eq!(
            serde_json::to_value(edges[0]).unwrap()["propagation"],
            "inherit"
        );
    }
}
