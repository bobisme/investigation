//! Deterministic, ordinal ranking of the gap inventory for `next` (plan §7.2).
//!
//! Ranking reads recorded structure only: the weight-free Pareto status of the
//! options a cell gap concerns, recorded rival hypotheses and per-rival
//! predictions, the disposition and importance of the question a gap serves,
//! and earlier unknown, retracted, failed or inconclusive attempts on the same
//! subject. The eight factors are compared lexicographically and each one is
//! exposed with its source. Nothing here is a probability, an expected
//! information gain, a cost estimate or a score, and nothing is summed.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::{
    command::RevisionRef,
    decision_ops::{ParetoOption, ParetoStatus, pareto},
    gap_page::GapFilter,
    gap_rules::{
        Gap, GapInventory, GapStatus, OutcomeBranch, Prerequisite, content, exact_ref, field,
        is_live, is_retracted,
    },
    identity::{Id, Kind},
    state::{Record, State},
    wire::Snapshot,
};

pub const NEXT_RULE_VERSION: &str = "slice-next.v2";
pub const DEFAULT_NEXT_LIMIT: usize = 3;

const FLIP: &str = "could_flip_choice";
const DISCRIMINATE: &str = "could_discriminate";
const CONTEXT: &str = "context_only";
const UNKNOWN: &str = "unknown";

const HARD_CELL_MISSING: &str = "decision.hard_criterion_cell_missing";
const HARD_CELL_UNKNOWN: &str = "decision.hard_criterion_cell_unknown";
const SOFT_CELL_MISSING: &str = "decision.option_criterion_cell_missing";
const STALE_COMPARISON: &str = "comparison.stale_dependency_revision";
const NO_ASSESSMENT: &str = "hypothesis.no_assessment";
const UNFROZEN_PLAN: &str = "inquiry.confirmatory_plan_unfrozen";
const RUN_INCOMPLETE: &str = "run.outcome_incomplete_or_unknown";
const FRESHNESS: &str = "freshness.needs_review";
/// Measurement flags use their flag ID as the rule ID; all are advisory.
const MEASUREMENT_FLAG_PREFIX: &str = "measurement.";

fn is_measurement_flag(gap: &Gap) -> bool {
    gap.rule_id.starts_with(MEASUREMENT_FLAG_PREFIX)
}

/// The per-rival predictions of a confirmatory InquiryPlan, grouped by
/// predicted outcome text (trimmed): each outcome maps to the current live
/// rival hypotheses that predict it. Predictions naming a rival that is
/// absent or no longer live, or with blank outcome text, are left out. An
/// exploratory plan has none. Shared by `next` ranking and the
/// `inquiry.non_discriminating_plan` blindspot check.
pub(crate) fn live_predictions_by_outcome(
    state: &State,
    inquiry: &Record,
) -> BTreeMap<String, BTreeSet<RevisionRef>> {
    let mut by_outcome = BTreeMap::<String, BTreeSet<RevisionRef>>::new();
    let predictions = field(content(&inquiry.body), "inquiry")
        .and_then(|mode| mode.get("confirmatory"))
        .and_then(|plan| plan.get("predictions"))
        .and_then(Value::as_array);
    for prediction in predictions.into_iter().flatten() {
        let Some(rival) = exact_ref(prediction.get("rival_ref"))
            .and_then(|reference| state.current(reference.id))
            .filter(|rival| rival.header.case_id == inquiry.header.case_id && is_live(rival))
        else {
            continue;
        };
        let Some(outcome) = prediction
            .get("predicted_outcome")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|outcome| !outcome.is_empty())
        else {
            continue;
        };
        by_outcome
            .entry(outcome.to_owned())
            .or_default()
            .insert(rival.reference());
    }
    by_outcome
}

/// Cell rules, plus flags and freshness gaps whose subject is a Comparison:
/// the value of rechecking a comparison depends on the option it describes.
fn reads_cell(gap: &Gap) -> bool {
    matches!(
        gap.rule_id.as_str(),
        HARD_CELL_MISSING | HARD_CELL_UNKNOWN | SOFT_CELL_MISSING | STALE_COMPARISON
    ) || ((is_measurement_flag(gap) || gap.rule_id == FRESHNESS)
        && gap
            .subject_refs
            .iter()
            .any(|subject| subject.id.kind() == Kind::Comparison))
}

/// Rules whose repeated gaps are grouped by criterion, and so by question.
/// Each of these gaps reads exactly one Criterion, so `--rule R --reads C`
/// selects exactly one group in `frontier`.
const GROUPED_RULES: [&str; 3] = [HARD_CELL_MISSING, HARD_CELL_UNKNOWN, SOFT_CELL_MISSING];

/// At most this many example records are listed as the basis of one value.
const BASIS_REF_LIMIT: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RankFactor {
    CanonicalIntegrity,
    TimeSensitivePreservation,
    ConsequentialBlocker,
    DecisionChangingValue,
    Applicability,
    BoundedCost,
    Novelty,
    StableIdCreationOrder,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrdinalPriority {
    Highest,
    High,
    Medium,
    Low,
    Unknown,
    NotApplicable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RankReason {
    pub factor: RankFactor,
    pub priority: OrdinalPriority,
    /// What the factor read, in words.
    pub source: String,
    /// The exact records the factor read; empty when the source is a rule or
    /// a stated absence.
    pub source_refs: Vec<RevisionRef>,
}

/// How much the step could change the choice or the standing of live rivals,
/// derived from recorded structure. It is a class, not an estimate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionValue {
    /// `could_flip_choice`, `could_discriminate`, `context_only` or `unknown`.
    pub decision_effect: String,
    /// The emitting rule's own class, before this ranking read the structure.
    pub rule_decision_effect: String,
    pub basis: String,
    pub basis_refs: Vec<RevisionRef>,
    /// True when every recorded outcome leaves the disposition unchanged; the
    /// step is then ranked by `narrower_value`.
    pub every_outcome_unchanged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub narrower_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pareto_status: Option<ParetoStatus>,
}

/// Gaps of one rule that read one Criterion, shown as one recommendation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GapGroup {
    pub group_id: String,
    pub rule_id: String,
    pub criterion_ref: RevisionRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_ref: Option<RevisionRef>,
    /// Every gap in the inventory the drill-down returns.
    pub gap_count: usize,
    pub ranked_count: usize,
    pub withheld_count: usize,
    /// The frontier filter that returns exactly this group.
    #[serde(skip)]
    pub filter: GapFilter,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Recommendation {
    pub rank: usize,
    pub action_id: String,
    pub gap_id: String,
    pub rule_id: String,
    pub action_kind: String,
    pub subject_refs: Vec<RevisionRef>,
    pub purpose: String,
    pub expected_useful_result: String,
    pub rank_reasons: Vec<RankReason>,
    pub decision_value: DecisionValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<GapGroup>,
    pub uncertainty: String,
    pub prerequisites: Vec<Prerequisite>,
    pub unresolved_fields: Vec<String>,
    pub estimated_cost: Value,
    pub external_effects: String,
    pub schema_operation: Option<String>,
    pub outcome_branches: Vec<OutcomeBranch>,
    pub deferral_alternative: String,
    /// What a stall rule counted and the moves it offers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stall: Option<crate::stall::StallDetail>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GapCounts {
    pub ready_to_analyze: usize,
    pub requires_source: usize,
    pub requires_external_action: usize,
    pub blocked_by_quality: usize,
    pub stale_due_to_revision: usize,
    pub deferred: usize,
    pub resolved: usize,
}

/// Gaps of one status that are never ranked as actions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotRanked {
    pub status: GapStatus,
    pub count: usize,
    pub reason: String,
    #[serde(skip)]
    pub filter: GapFilter,
}

/// Eligible gaps that are not recommended because no outcome could matter.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WithheldGaps {
    pub reason_code: String,
    pub explanation: String,
    pub option_ref: RevisionRef,
    pub determining_refs: Vec<RevisionRef>,
    pub gap_count: usize,
    #[serde(skip)]
    pub filter: GapFilter,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StopAlternative {
    pub operation: String,
    pub purpose: String,
}

/// Offered when no ranked step is recorded as able to change a choice or
/// discriminate rivals, or when a stall rule reports repeated work without a
/// consequence. It is an alternative, never an automatic closure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StopOrDefer {
    pub reason: String,
    pub basis: Vec<String>,
    pub alternatives: Vec<StopAlternative>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NextResponse {
    pub schema_version: String,
    pub snapshot: Snapshot,
    pub rule_version: String,
    pub case_id: Id,
    pub recommendations: Vec<Recommendation>,
    pub counts: GapCounts,
    /// Ranked recommendations (a group counts once) after the ones shown.
    pub remaining_recommendations: usize,
    /// Eligible gaps that were ranked, counting every member of a group.
    pub ranked_gap_total: usize,
    pub not_ranked: Vec<NotRanked>,
    pub withheld: Vec<WithheldGaps>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_or_defer: Option<StopOrDefer>,
    pub limits: Vec<String>,
}

/// Rank feasible recommendations lexicographically by the eight §7.2 factors.
/// `state` must be the replay that produced `inventory`. Ordinal factors are
/// exposed; they are never summed into a pseudo-score.
pub fn rank_next(state: &State, inventory: &GapInventory, requested_limit: usize) -> NextResponse {
    let limit = requested_limit.min(DEFAULT_NEXT_LIMIT);
    let context = Context::build(state, inventory);
    let mut ranked = Vec::new();
    let mut withheld = BTreeMap::<Id, WithheldGaps>::new();
    let mut withheld_ids = BTreeSet::new();
    for gap in inventory.gaps.iter().filter(|gap| eligible(gap.status)) {
        let analysis = context.analyze(gap);
        if let Some((option_ref, determining_refs)) = &analysis.withhold {
            withheld_ids.insert(gap.gap_id.as_str());
            withheld
                .entry(option_ref.id)
                .or_insert_with(|| WithheldGaps {
                    reason_code: "soft_criterion_on_hard_disqualified_option".into(),
                    explanation: "The option already fails a hard criterion on a verified comparison, so no soft-criterion result can change its disposition or Pareto status. Measuring it is not recommended.".into(),
                    option_ref: *option_ref,
                    determining_refs: determining_refs.clone(),
                    gap_count: 0,
                    filter: GapFilter {
                        rule_ids: [SOFT_CELL_MISSING.to_owned()].into(),
                        read_ids: [option_ref.id].into(),
                        ..GapFilter::default()
                    },
                })
                .gap_count += 1;
            continue;
        }
        ranked.push((analysis.key(gap), gap, analysis));
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0));

    let mut seen_groups = BTreeSet::new();
    let mut representatives = Vec::new();
    for (_, gap, analysis) in &ranked {
        if let Some(criterion) = group_criterion(gap)
            && !seen_groups.insert((gap.rule_id.as_str(), criterion.id))
        {
            continue;
        }
        representatives.push((*gap, analysis));
    }
    let recommendations = representatives
        .iter()
        .take(limit)
        .enumerate()
        .map(|(index, (gap, analysis))| {
            let group = group_criterion(gap)
                .map(|criterion| context.group(inventory, gap, criterion, &ranked, &withheld_ids))
                .filter(|group| group.gap_count > 1);
            recommendation(gap, analysis, group, index + 1)
        })
        .collect();

    let ranked_effects: Vec<&str> = ranked
        .iter()
        .map(|(_, _, analysis)| analysis.value.decision_effect.as_str())
        .collect();
    let stalls: Vec<&str> = ranked
        .iter()
        .filter(|(_, _, analysis)| analysis.stall)
        .map(|(_, gap, _)| gap.gap_id.as_str())
        .collect();
    let stop_or_defer = stop_or_defer(&ranked_effects, withheld_ids.len(), &stalls);
    NextResponse {
        schema_version: "inv.next.v1".into(),
        snapshot: inventory.snapshot.clone(),
        rule_version: NEXT_RULE_VERSION.into(),
        case_id: inventory.case_id,
        recommendations,
        counts: count_gaps(&inventory.gaps),
        remaining_recommendations: representatives.len().saturating_sub(limit),
        ranked_gap_total: ranked.len(),
        not_ranked: not_ranked(&inventory.gaps),
        withheld: withheld.into_values().collect(),
        stop_or_defer,
        limits: inventory.limits.clone(),
    }
}

fn eligible(status: GapStatus) -> bool {
    matches!(
        status,
        GapStatus::ReadyToAnalyze
            | GapStatus::RequiresSource
            | GapStatus::RequiresExternalAction
            | GapStatus::StaleDueToRevision
    )
}

fn group_criterion(gap: &Gap) -> Option<RevisionRef> {
    if !GROUPED_RULES.contains(&gap.rule_id.as_str()) {
        return None;
    }
    let mut criteria = gap
        .read_set
        .iter()
        .filter(|reference| reference.id.kind() == Kind::Criterion);
    let criterion = criteria.next().copied();
    criterion.filter(|_| criteria.next().is_none())
}

fn not_ranked(gaps: &[Gap]) -> Vec<NotRanked> {
    [
        (
            GapStatus::BlockedByQuality,
            "Blocked until the quality of the recorded evidence is repaired; listed, not ranked.",
        ),
        (
            GapStatus::Deferred,
            "Deferred by recorded advice feedback; listed, not ranked, until resumed or materially changed.",
        ),
        (
            GapStatus::Resolved,
            "Resolved under this rule version; listed for audit only.",
        ),
    ]
    .into_iter()
    .map(|(status, reason)| NotRanked {
        status,
        count: gaps.iter().filter(|gap| gap.status == status).count(),
        reason: reason.into(),
        filter: GapFilter {
            statuses: [status].into(),
            ..GapFilter::default()
        },
    })
    .collect()
}

fn stop_or_defer(ranked_effects: &[&str], withheld: usize, stalls: &[&str]) -> Option<StopOrDefer> {
    // A stall is itself the reason to offer a stop, whatever else is ranked.
    if stalls.is_empty() {
        if ranked_effects.is_empty() && withheld == 0 {
            return None;
        }
        // A step whose effect is unknown might still matter, so it blocks the offer.
        if ranked_effects.iter().any(|effect| *effect != CONTEXT) {
            return None;
        }
    }
    let mut basis = Vec::new();
    if !stalls.is_empty() {
        basis.push(format!(
            "Stall gap(s) {} report repeated work with no recorded consequence.",
            stalls.join(", ")
        ));
    }
    if ranked_effects.iter().all(|effect| *effect == CONTEXT) {
        basis.push(format!(
            "{} ranked gap(s) are context_only: no recorded outcome changes a hard-criterion disposition or an option's Pareto status, or discriminates live rivals.",
            ranked_effects.len()
        ));
    }
    if withheld > 0 {
        basis.push(format!(
            "{withheld} soft-criterion gap(s) concern options already disqualified by a hard criterion and are withheld."
        ));
    }
    basis.push("This is a statement about recorded structure under this rule version, not proof that the investigation is complete.".into());
    let reason = if stalls.is_empty() {
        "No ranked step is recorded as able to change the choice or discriminate rivals; a justified stop or deferral is an alternative to further measurement."
    } else {
        "Recorded work has repeated without changing a recorded judgment; a justified stop or deferral is an alternative to more of the same work."
    };
    Some(StopOrDefer {
        reason: reason.into(),
        basis,
        alternatives: vec![
            StopAlternative {
                operation: "decision.propose".into(),
                purpose: "Propose a conditional choice that names the residual unknowns and a reopening trigger.".into(),
            },
            StopAlternative {
                operation: "question.resolve".into(),
                purpose: "Resolve the question as answered, inconclusive or deferred, with the reason recorded.".into(),
            },
            StopAlternative {
                operation: "case.pause".into(),
                purpose: "Pause the case with residual risks and reopening conditions.".into(),
            },
        ],
    })
}

fn count_gaps(gaps: &[Gap]) -> GapCounts {
    let count = |status| gaps.iter().filter(|gap| gap.status == status).count();
    GapCounts {
        ready_to_analyze: count(GapStatus::ReadyToAnalyze),
        requires_source: count(GapStatus::RequiresSource),
        requires_external_action: count(GapStatus::RequiresExternalAction),
        blocked_by_quality: count(GapStatus::BlockedByQuality),
        stale_due_to_revision: count(GapStatus::StaleDueToRevision),
        deferred: count(GapStatus::Deferred),
        resolved: count(GapStatus::Resolved),
    }
}

type ParetoTable = Result<BTreeMap<Id, ParetoOption>, String>;

/// A value class, rewritten outcome branches if any, and the option and
/// determining cells when the gap is withheld.
type Valued = (
    DecisionValue,
    Option<Vec<OutcomeBranch>>,
    Option<(RevisionRef, Vec<RevisionRef>)>,
);

/// Everything the ranking reads from one replay, gathered in one pass.
struct Context<'a> {
    state: &'a State,
    /// Pareto status by question, computed only for questions with cell gaps.
    pareto: BTreeMap<Id, ParetoTable>,
    /// Unknown or retracted comparisons by (option, criterion).
    prior_cells: BTreeMap<(Id, Id), Vec<RevisionRef>>,
    /// Inconclusive or non-diagnostic assessments by subject hypothesis.
    inconclusive_assessments: BTreeMap<Id, Vec<RevisionRef>>,
    /// Failed, cancelled or blocked runs by the plan they executed.
    failed_runs: BTreeMap<Id, Vec<RevisionRef>>,
    /// Live hypotheses by question.
    live_hypotheses: BTreeMap<Id, Vec<RevisionRef>>,
}

struct Analysis {
    value: DecisionValue,
    branches: Option<Vec<OutcomeBranch>>,
    withhold: Option<(RevisionRef, Vec<RevisionRef>)>,
    /// Advisory gaps are never consequential blockers.
    advisory: bool,
    /// Stall gaps rank as consequential: repeating the step that stalled is
    /// unlikely to change the picture.
    stall: bool,
    applicability: (u8, OrdinalPriority, String, Vec<RevisionRef>),
    novelty: (u8, OrdinalPriority, String, Vec<RevisionRef>),
}

type RankKey = (u8, u8, u8, u8, u8, u8, u8, u64, String);

impl Analysis {
    fn key(&self, gap: &Gap) -> RankKey {
        let integrity = u8::from(!gap.rule_id.starts_with("store.integrity."));
        let preservation = u8::from(!gap.time_sensitive);
        let effect = self.value.decision_effect.as_str();
        let consequential =
            u8::from(!self.stall && (self.advisory || !matches!(effect, FLIP | DISCRIMINATE)));
        let decision_value = match effect {
            FLIP => 0,
            DISCRIMINATE => 1,
            CONTEXT => 2,
            _ => 3,
        };
        // No comparable user-supplied cost is stored, so cost never reorders.
        let cost = 1;
        (
            integrity,
            preservation,
            consequential,
            decision_value,
            self.applicability.0,
            cost,
            self.novelty.0,
            gap.creation_sequence,
            gap.gap_id.clone(),
        )
    }
}

impl<'a> Context<'a> {
    fn build(state: &'a State, inventory: &GapInventory) -> Self {
        let mut context = Self {
            state,
            pareto: BTreeMap::new(),
            prior_cells: BTreeMap::new(),
            inconclusive_assessments: BTreeMap::new(),
            failed_runs: BTreeMap::new(),
            live_hypotheses: BTreeMap::new(),
        };
        let case_id = inventory.case_id;
        for record in state
            .records()
            .filter(|record| record.header.case_id == case_id)
        {
            let reference = record.reference();
            let body = &record.body;
            match record.header.id.kind() {
                Kind::Comparison => {
                    let unknown = field(body, "outcome").and_then(Value::as_str) == Some("unknown");
                    if let (Some(option), Some(criterion)) = (
                        exact_ref(field(body, "option_ref")),
                        exact_ref(field(body, "criterion_ref")),
                    ) && (unknown || is_retracted(body))
                    {
                        context
                            .prior_cells
                            .entry((option.id, criterion.id))
                            .or_default()
                            .push(reference);
                    }
                }
                Kind::Assessment => {
                    let relation = field(body, "relation").and_then(Value::as_str);
                    if let Some(subject) = exact_ref(field(body, "subject_ref"))
                        && matches!(relation, Some("inconclusive" | "non_diagnostic"))
                        && !is_retracted(body)
                    {
                        context
                            .inconclusive_assessments
                            .entry(subject.id)
                            .or_default()
                            .push(reference);
                    }
                }
                Kind::Run => {
                    let status = field(body, "execution_status").and_then(Value::as_str);
                    if let Some(plan) = run_plan(record)
                        && matches!(status, Some("failed" | "cancelled" | "blocked"))
                    {
                        context
                            .failed_runs
                            .entry(plan.id)
                            .or_default()
                            .push(reference);
                    }
                }
                Kind::Hypothesis if is_live(record) => {
                    if let Some(question) = exact_ref(field(content(body), "question_ref")) {
                        context
                            .live_hypotheses
                            .entry(question.id)
                            .or_default()
                            .push(reference);
                    }
                }
                _ => {}
            }
        }
        let questions: BTreeSet<Id> = inventory
            .gaps
            .iter()
            .filter(|gap| eligible(gap.status))
            .filter_map(|gap| context.cell_of(gap))
            .filter_map(|(option, criterion)| context.cell_question(option, criterion))
            .map(|question| question.id)
            .collect();
        for question in questions {
            let table = pareto(state, case_id, question)
                .map(|options| {
                    options
                        .into_iter()
                        .map(|option| (option.option_ref.id, option))
                        .collect()
                })
                .map_err(|failure| failure.error.message);
            context.pareto.insert(question, table);
        }
        context
    }

    fn current(&self, id: Id) -> Option<&'a Record> {
        self.state.current(id)
    }

    /// The (option, criterion) pair a cell gap concerns.
    fn cell_of(&self, gap: &Gap) -> Option<(Id, Id)> {
        if !reads_cell(gap) {
            return None;
        }
        let find = |kind| {
            gap.subject_refs
                .iter()
                .find(|reference| reference.id.kind() == kind)
                .map(|reference| reference.id)
        };
        if let (Some(option), Some(criterion)) = (find(Kind::Option), find(Kind::Criterion)) {
            return Some((option, criterion));
        }
        let comparison = self.current(find(Kind::Comparison)?)?;
        Some((
            exact_ref(field(&comparison.body, "option_ref"))?.id,
            exact_ref(field(&comparison.body, "criterion_ref"))?.id,
        ))
    }

    fn cell_question(&self, option: Id, criterion: Id) -> Option<RevisionRef> {
        [criterion, option]
            .into_iter()
            .filter_map(|id| self.current(id))
            .find_map(|record| question_ref(&record.body))
    }

    /// The current Question a record serves, following option, subject and
    /// decision references a few steps.
    fn question_of(&self, id: Id, depth: u8) -> Option<&'a Record> {
        let record = self.current(id)?;
        if id.kind() == Kind::Question {
            return Some(record);
        }
        if let Some(question) = question_ref(&record.body) {
            return self
                .current(question.id)
                .filter(|question| question.header.id.kind() == Kind::Question);
        }
        if depth == 0 {
            return None;
        }
        ["option_ref", "subject_ref", "decision_ref"]
            .into_iter()
            .filter_map(|key| exact_ref(field(&record.body, key)))
            .find_map(|reference| self.question_of(reference.id, depth - 1))
    }

    fn analyze(&self, gap: &Gap) -> Analysis {
        let (mut value, branches, withhold) = match gap.rule_id.as_str() {
            _ if reads_cell(gap) => self.cell_value(gap),
            NO_ASSESSMENT => (self.rival_value(gap), None, None),
            UNFROZEN_PLAN => self.prediction_value(gap),
            _ => (static_value(gap, None), None, None),
        };
        let advisory = is_measurement_flag(gap);
        if advisory {
            value.basis.push_str(" Advisory measurement flag: it points at conditions worth recording and never blocks recording or changes validity.");
        }
        Analysis {
            value,
            branches,
            withhold,
            advisory,
            stall: crate::guidance::STALL_GAP_RULES.contains(&gap.rule_id.as_str()),
            applicability: self.applicability(gap),
            novelty: self.novelty(gap),
        }
    }

    fn cell_value(&self, gap: &Gap) -> Valued {
        let Some((option_id, criterion_id)) = self.cell_of(gap) else {
            return (
                static_value(
                    gap,
                    Some("The option and criterion of this cell could not be read."),
                ),
                None,
                None,
            );
        };
        let Some(question) = self.cell_question(option_id, criterion_id) else {
            return (
                static_value(
                    gap,
                    Some("The criterion names no question, so no Pareto status was computed."),
                ),
                None,
                None,
            );
        };
        let table = match self.pareto.get(&question.id) {
            Some(Ok(table)) => table,
            Some(Err(message)) => {
                let note = format!(
                    "Pareto status for question {} is unavailable: {message}",
                    question.id
                );
                return (static_value(gap, Some(&note)), None, None);
            }
            None => return (static_value(gap, None), None, None),
        };
        let Some(standing) = table.get(&option_id) else {
            let note = format!(
                "Option {option_id} is not an eligible option of question {}, so it has no Pareto status.",
                question.id
            );
            return (static_value(gap, Some(&note)), None, None);
        };
        let option_ref = standing.option_ref;
        let criterion = self.current(criterion_id);
        let criterion_ref = criterion.map(Record::reference);
        let hard = criterion.is_some_and(|criterion| {
            field(content(&criterion.body), "kind").and_then(Value::as_str) == Some("hard")
        });
        let subjects: BTreeSet<Id> = gap.subject_refs.iter().map(|subject| subject.id).collect();
        let determining: Vec<RevisionRef> = standing.determining_cells.clone();
        let revises_determining = determining.iter().any(|cell| subjects.contains(&cell.id));
        let mut refs = vec![option_ref];
        refs.extend(criterion_ref);
        let rule_effect = gap.decision_effect.clone();
        let value = |effect: &str, basis: String, refs: Vec<RevisionRef>| DecisionValue {
            decision_effect: effect.into(),
            rule_decision_effect: rule_effect.clone(),
            basis,
            basis_refs: refs,
            every_outcome_unchanged: false,
            narrower_value: None,
            pareto_status: Some(standing.status),
        };
        let status = status_name(standing.status);
        match standing.status {
            ParetoStatus::DisqualifiedHard if !revises_determining => {
                let mut basis_refs = refs.clone();
                basis_refs.extend(determining.iter().copied());
                let cells = determining
                    .iter()
                    .map(|cell| cell.id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                if gap.rule_id == SOFT_CELL_MISSING {
                    let mut withheld = value(
                        CONTEXT,
                        format!(
                            "Option {option_id} is disqualified_hard by comparison {cells}; no soft-criterion result can change that."
                        ),
                        basis_refs,
                    );
                    withheld.every_outcome_unchanged = true;
                    return (withheld, None, Some((option_ref, determining)));
                }
                let mut narrowed = value(
                    CONTEXT,
                    format!(
                        "Option {option_id} already fails a hard criterion on verified comparison {cells}. Every outcome of this step leaves it disqualified_hard."
                    ),
                    basis_refs,
                );
                narrowed.every_outcome_unchanged = true;
                narrowed.narrower_value = Some("Completes or rechecks the record for an option that is already ruled out; it cannot restore eligibility or change the choice.".into());
                let branches = gap
                    .outcome_branches
                    .iter()
                    .map(|branch| OutcomeBranch {
                        outcome: branch.outcome.clone(),
                        consequence: format!(
                            "The option stays disqualified_hard by comparison {cells}."
                        ),
                        basis: "recorded failed hard criterion".into(),
                    })
                    .collect();
                (narrowed, Some(branches), None)
            }
            ParetoStatus::StrictlyDominated if !hard && !revises_determining => {
                let mut basis_refs = refs.clone();
                basis_refs.extend(determining.iter().copied());
                (
                    value(
                        CONTEXT,
                        format!(
                            "Option {option_id} is strictly dominated on recorded cells; soft-criterion measurement on a dominated option is deprioritized."
                        ),
                        basis_refs,
                    ),
                    None,
                    None,
                )
            }
            _ if hard => (
                value(
                    FLIP,
                    format!(
                        "The result on this hard criterion decides whether option {option_id} stays eligible; a fail rules it out. Current Pareto status: {status}."
                    ),
                    refs,
                ),
                None,
                None,
            ),
            _ => {
                let rivals: Vec<RevisionRef> = table
                    .values()
                    .filter(|other| {
                        other.option_ref.id != option_id
                            && other.status != ParetoStatus::DisqualifiedHard
                    })
                    .map(|other| other.option_ref)
                    .collect();
                if rivals.is_empty() {
                    let mut narrowed = value(
                        CONTEXT,
                        format!(
                            "No other option of question {} is eligible and not disqualified, so no result on this criterion can change which option is on the Pareto frontier.",
                            question.id
                        ),
                        refs,
                    );
                    narrowed.every_outcome_unchanged = true;
                    narrowed.narrower_value = Some("Records the option's standing on this criterion for the decision record and later comparison.".into());
                    (narrowed, None, None)
                } else {
                    let count = rivals.len();
                    refs.extend(rivals.into_iter().take(BASIS_REF_LIMIT));
                    (
                        value(
                            DISCRIMINATE,
                            format!(
                                "A recorded result on this criterion could change option {option_id}'s Pareto status ({status}) against {count} other eligible option(s)."
                            ),
                            refs,
                        ),
                        None,
                        None,
                    )
                }
            }
        }
    }

    fn rival_value(&self, gap: &Gap) -> DecisionValue {
        let Some(hypothesis) = gap
            .subject_refs
            .first()
            .and_then(|subject| self.current(subject.id))
        else {
            return static_value(gap, None);
        };
        let own = hypothesis.header.id;
        let mut rivals = BTreeSet::new();
        if let Some(question) = exact_ref(field(content(&hypothesis.body), "question_ref")) {
            rivals.extend(
                self.live_hypotheses
                    .get(&question.id)
                    .into_iter()
                    .flatten()
                    .filter(|rival| rival.id != own)
                    .copied(),
            );
        }
        for reference in field(content(&hypothesis.body), "rival_refs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|value| exact_ref(Some(value)))
        {
            if let Some(rival) = self.current(reference.id).filter(|rival| is_live(rival)) {
                rivals.insert(rival.reference());
            }
        }
        let count = rivals.len();
        let mut value = DecisionValue {
            decision_effect: DISCRIMINATE.into(),
            rule_decision_effect: gap.decision_effect.clone(),
            basis: format!(
                "An assessment could separate this hypothesis from {count} live rival hypothesis(es) recorded for its question or in its rival_refs."
            ),
            basis_refs: rivals.into_iter().take(BASIS_REF_LIMIT).collect(),
            every_outcome_unchanged: false,
            narrower_value: None,
            pareto_status: None,
        };
        if count == 0 {
            value.decision_effect = CONTEXT.into();
            value.basis = "No live rival hypothesis is recorded for this hypothesis's question or in its rival_refs. An assessment can change its standing but cannot discriminate rivals.".into();
        }
        value
    }

    fn prediction_value(&self, gap: &Gap) -> Valued {
        let Some(inquiry) = gap
            .subject_refs
            .first()
            .and_then(|subject| self.current(subject.id))
        else {
            return (static_value(gap, None), None, None);
        };
        let by_outcome = live_predictions_by_outcome(self.state, inquiry);
        let rivals: BTreeSet<RevisionRef> = by_outcome.values().flatten().copied().collect();
        let mut value = DecisionValue {
            decision_effect: UNKNOWN.into(),
            rule_decision_effect: gap.decision_effect.clone(),
            basis: "The plan records no prediction for a live rival, so what each outcome would change is unknown. Record a predicted outcome for each live rival.".into(),
            basis_refs: rivals.iter().copied().take(BASIS_REF_LIMIT).collect(),
            every_outcome_unchanged: false,
            narrower_value: None,
            pareto_status: None,
        };
        if rivals.is_empty() {
            return (value, None, None);
        }
        if rivals.len() >= 2 && by_outcome.len() >= 2 {
            value.decision_effect = DISCRIMINATE.into();
            value.basis = format!(
                "The plan records {} distinct predicted outcomes across {} live rivals, so the result can discriminate them.",
                by_outcome.len(),
                rivals.len()
            );
            let branches = by_outcome
                .into_iter()
                .map(|(outcome, rivals)| OutcomeBranch {
                    outcome,
                    consequence: format!(
                        "Consistent with the prediction recorded for {}.",
                        rivals
                            .iter()
                            .map(|rival| rival.id.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    basis: "recorded per-rival prediction (authored text)".into(),
                })
                .collect();
            return (value, Some(branches), None);
        }
        value.decision_effect = CONTEXT.into();
        value.every_outcome_unchanged = true;
        value.basis = "The recorded predictions name one live rival, or the same outcome for every live rival, so no recorded outcome discriminates rivals.".into();
        value.narrower_value = Some(
            "Freezing still locks the predictions, controls and stop rule before outcomes are seen."
                .into(),
        );
        (value, None, None)
    }

    fn applicability(&self, gap: &Gap) -> (u8, OrdinalPriority, String, Vec<RevisionRef>) {
        const DEADLINE: &str = "No structured decision deadline is recorded.";
        let Some(question) = gap
            .subject_refs
            .iter()
            .find_map(|subject| self.question_of(subject.id, 3))
        else {
            return (
                2,
                OrdinalPriority::Unknown,
                format!("The gap's subjects name no question. {DEADLINE}"),
                vec![],
            );
        };
        let body = content(&question.body);
        let disposition = field(body, "disposition")
            .and_then(Value::as_str)
            .unwrap_or("open");
        let importance = field(body, "importance")
            .and_then(Value::as_str)
            .unwrap_or("normal");
        let refs = vec![question.reference()];
        let id = question.header.id;
        if disposition != "open" {
            return (
                3,
                OrdinalPriority::Low,
                format!("Question {id} is recorded as {disposition}. {DEADLINE}"),
                refs,
            );
        }
        let (key, priority) = match importance {
            "high" => (0, OrdinalPriority::High),
            "low" => (3, OrdinalPriority::Low),
            _ => (1, OrdinalPriority::Medium),
        };
        (
            key,
            priority,
            format!("Question {id} is open with recorded importance {importance}. {DEADLINE}"),
            refs,
        )
    }

    fn novelty(&self, gap: &Gap) -> (u8, OrdinalPriority, String, Vec<RevisionRef>) {
        let subject = |kind| {
            gap.subject_refs
                .iter()
                .find(|reference| reference.id.kind() == kind)
                .copied()
        };
        let attempts: Option<Vec<RevisionRef>> = match gap.rule_id.as_str() {
            HARD_CELL_MISSING | HARD_CELL_UNKNOWN | SOFT_CELL_MISSING | STALE_COMPARISON => self
                .cell_of(gap)
                .map(|cell| self.prior_cells.get(&cell).cloned().unwrap_or_default()),
            NO_ASSESSMENT => subject(Kind::Hypothesis).map(|hypothesis| {
                self.inconclusive_assessments
                    .get(&hypothesis.id)
                    .cloned()
                    .unwrap_or_default()
            }),
            UNFROZEN_PLAN => subject(Kind::Inquiry).map(|inquiry| {
                self.failed_runs
                    .get(&inquiry.id)
                    .cloned()
                    .unwrap_or_default()
            }),
            RUN_INCOMPLETE => subject(Kind::Run).and_then(|run| {
                let plan = run_plan(self.current(run.id)?)?;
                Some(
                    self.failed_runs
                        .get(&plan.id)
                        .into_iter()
                        .flatten()
                        .filter(|other| other.id != run.id)
                        .copied()
                        .collect(),
                )
            }),
            _ => None,
        };
        match attempts {
            None => (
                1,
                OrdinalPriority::Unknown,
                "This rule's subject has no attempt history that the ranking reads.".into(),
                vec![],
            ),
            Some(attempts) if attempts.is_empty() => (
                0,
                OrdinalPriority::High,
                "No earlier unknown, retracted, failed or inconclusive attempt is recorded on this subject.".into(),
                vec![],
            ),
            Some(attempts) => (
                2,
                OrdinalPriority::Low,
                format!(
                    "{} earlier attempt(s) on this subject ended unknown, retracted, failed or inconclusive; repeating the same step may add little.",
                    attempts.len()
                ),
                attempts.into_iter().take(BASIS_REF_LIMIT).collect(),
            ),
        }
    }

    fn group(
        &self,
        inventory: &GapInventory,
        gap: &Gap,
        criterion: RevisionRef,
        ranked: &[(RankKey, &Gap, Analysis)],
        withheld: &BTreeSet<&str>,
    ) -> GapGroup {
        let filter = GapFilter {
            rule_ids: [gap.rule_id.clone()].into(),
            read_ids: [criterion.id].into(),
            ..GapFilter::default()
        };
        GapGroup {
            group_id: format!("group:{}:{}", gap.rule_id, criterion.id),
            rule_id: gap.rule_id.clone(),
            criterion_ref: criterion,
            question_ref: self
                .current(criterion.id)
                .and_then(|record| question_ref(&record.body)),
            gap_count: inventory
                .gaps
                .iter()
                .filter(|gap| filter.matches(gap))
                .count(),
            ranked_count: ranked
                .iter()
                .filter(|(_, gap, _)| filter.matches(gap))
                .count(),
            withheld_count: inventory
                .gaps
                .iter()
                .filter(|gap| withheld.contains(gap.gap_id.as_str()) && filter.matches(gap))
                .count(),
            filter,
        }
    }
}

fn question_ref(body: &Value) -> Option<RevisionRef> {
    exact_ref(field(body, "question_ref"))
        .or_else(|| field(body, "scope").and_then(|scope| exact_ref(field(scope, "question_ref"))))
}

fn run_plan(run: &Record) -> Option<RevisionRef> {
    let intent = field(&run.body, "intent")?;
    if intent.get("kind").and_then(Value::as_str) != Some("plan") {
        return None;
    }
    exact_ref(intent.get("plan_ref"))
}

fn status_name(status: ParetoStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn static_value(gap: &Gap, note: Option<&str>) -> DecisionValue {
    let mut basis = format!(
        "Rule {} classifies this gap as {}; no recorded structure read by this ranking narrows it.",
        gap.rule_id, gap.decision_effect
    );
    if let Some(note) = note {
        basis = format!("{note} {basis}");
    }
    DecisionValue {
        decision_effect: gap.decision_effect.clone(),
        rule_decision_effect: gap.decision_effect.clone(),
        basis,
        basis_refs: vec![],
        every_outcome_unchanged: false,
        narrower_value: None,
        pareto_status: None,
    }
}

fn recommendation(
    gap: &Gap,
    analysis: &Analysis,
    group: Option<GapGroup>,
    rank: usize,
) -> Recommendation {
    let key = analysis.key(gap);
    let value = &analysis.value;
    let effect = value.decision_effect.as_str();
    let mut value_source = format!("{effect}: {}", value.basis);
    if let Some(narrower) = &value.narrower_value {
        value_source.push_str(&format!(" Ranked by its narrower value: {narrower}"));
    }
    let reasons = vec![
        RankReason {
            factor: RankFactor::CanonicalIntegrity,
            priority: if key.0 == 0 {
                OrdinalPriority::Highest
            } else {
                OrdinalPriority::NotApplicable
            },
            source: "the case was read from a verified snapshot; no corruption gap was emitted"
                .into(),
            source_refs: vec![],
        },
        RankReason {
            factor: RankFactor::TimeSensitivePreservation,
            priority: if key.1 == 0 {
                OrdinalPriority::Highest
            } else {
                OrdinalPriority::NotApplicable
            },
            source: if gap.time_sensitive {
                "explicit time-sensitive preservation field".into()
            } else {
                "no explicit preservation deadline on this gap".into()
            },
            source_refs: if gap.time_sensitive {
                gap.subject_refs.clone()
            } else {
                vec![]
            },
        },
        RankReason {
            factor: RankFactor::ConsequentialBlocker,
            priority: if key.2 == 0 {
                OrdinalPriority::High
            } else {
                OrdinalPriority::NotApplicable
            },
            source: if analysis.advisory {
                format!(
                    "rule {} is an advisory measurement flag, never a blocker",
                    gap.rule_id
                )
            } else if analysis.stall {
                format!(
                    "stall rule {}: recorded work repeated with no consequence, so more of the same step is unlikely to change the picture",
                    gap.rule_id
                )
            } else {
                format!("rule {} with decision effect {effect}", gap.rule_id)
            },
            source_refs: if analysis.stall {
                gap.subject_refs.clone()
            } else if key.2 == 0 {
                value.basis_refs.clone()
            } else {
                vec![]
            },
        },
        RankReason {
            factor: RankFactor::DecisionChangingValue,
            priority: match key.3 {
                0 => OrdinalPriority::High,
                1 => OrdinalPriority::Medium,
                2 => OrdinalPriority::Low,
                _ => OrdinalPriority::Unknown,
            },
            source: value_source,
            source_refs: value.basis_refs.clone(),
        },
        RankReason {
            factor: RankFactor::Applicability,
            priority: analysis.applicability.1,
            source: analysis.applicability.2.clone(),
            source_refs: analysis.applicability.3.clone(),
        },
        RankReason {
            factor: RankFactor::BoundedCost,
            priority: OrdinalPriority::Unknown,
            source: "no comparable user-supplied cost estimate is recorded".into(),
            source_refs: vec![],
        },
        RankReason {
            factor: RankFactor::Novelty,
            priority: analysis.novelty.1,
            source: analysis.novelty.2.clone(),
            source_refs: analysis.novelty.3.clone(),
        },
        RankReason {
            factor: RankFactor::StableIdCreationOrder,
            priority: OrdinalPriority::NotApplicable,
            source: format!(
                "creation sequence {}, then stable gap_id",
                gap.creation_sequence
            ),
            source_refs: vec![],
        },
    ];
    let branches = analysis
        .branches
        .clone()
        .unwrap_or_else(|| gap.outcome_branches.clone());
    let expected_useful_result = if gap.stall.is_some() {
        "Record one of the offered moves and why, or a justified stop; none of the moves invents a hypothesis, test or evidence.".into()
    } else if let Some(narrower) = &value.narrower_value {
        format!("Every recorded outcome leaves the disposition unchanged. {narrower}")
    } else if branches.is_empty() {
        "Resolve the named prerequisite or keep its unknown state explicit.".into()
    } else {
        "Record which declared branch is supported; do not infer a result before evidence is supplied.".into()
    };
    let external = gap.status == GapStatus::RequiresExternalAction;
    Recommendation {
        rank,
        action_id: format!("action:{}", gap.gap_id),
        gap_id: gap.gap_id.clone(),
        rule_id: gap.rule_id.clone(),
        action_kind: if external {
            "consumer_information"
        } else {
            "local_record_or_read"
        }
        .into(),
        subject_refs: gap.subject_refs.clone(),
        purpose: gap.reason.clone(),
        expected_useful_result,
        rank_reasons: reasons,
        decision_value: value.clone(),
        group,
        uncertainty: "Ordinal deterministic heuristic; not a probability, expected-information calculation, or universal ranking.".into(),
        prerequisites: gap.prerequisites.clone(),
        unresolved_fields: gap.unresolved_fields.clone(),
        estimated_cost: serde_json::json!({"state":"unknown"}),
        external_effects: if external { "consumer_managed" } else { "none" }.into(),
        schema_operation: gap.suggested_operation.clone(),
        outcome_branches: branches,
        deferral_alternative: "Keep the gap visible as deferred or unknown; this tool does not dispatch work.".into(),
        stall: gap.stall.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        gap_rules::{GAP_RULE_VERSION, GapFields, enumerate_gaps, make_gap},
        header::{RecordHeader, Revision, SchemaVersion},
        state::AcceptedTransaction,
        wire::RecordChange,
    };
    use serde_json::json;

    fn id(prefix: &str, n: u64) -> Id {
        format!("{prefix}_018d87fc-5234-7000-8000-{n:012x}")
            .parse()
            .unwrap()
    }

    fn reference(id: Id) -> Value {
        json!({"id": id, "revision": 1})
    }

    fn add(state: &mut State, id: Id, case: Id, body: Value) {
        let sequence = state.sequence() + 1;
        let record = Record {
            header: RecordHeader {
                id,
                case_id: case,
                schema_version: SchemaVersion::RecordV0Alpha,
                created_by: "agent:test".parse().unwrap(),
                actor_descriptor_self_reported: None,
                created_at: "2026-11-22T08:12:44.000Z".parse().unwrap(),
                recorded_at: "2026-11-22T08:12:44.000Z".parse().unwrap(),
                recorded_by: "agent:test".parse().unwrap(),
                observed_at: None,
                acquired_at: None,
                acceptance_sequence: sequence,
                revision: Revision(1),
                history: vec![],
                classification: Default::default(),
                provenance: Default::default(),
                extensions: Default::default(),
            },
            body,
        };
        state
            .apply(&AcceptedTransaction {
                case_id: case,
                sequence,
                records: vec![record],
                created: vec![RecordChange {
                    client_ref: None,
                    id,
                    revision: 1,
                }],
                updated: vec![],
                warnings: vec![],
            })
            .unwrap();
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            sequence: 1,
            head_hash: format!("sha256:{}", "a".repeat(64)).parse().unwrap(),
        }
    }

    fn criterion(question: Id, kind: &str) -> Value {
        json!({"name":"Latency","kind":kind,"question_ref":reference(question),"option_refs":null,
            "scale":{"unit":"ms","description":"p99","anchors":[]},"preferred_direction":"lower",
            "threshold": if kind == "hard" {
                json!({"kind":"measured","operator":"at_most","value":{"decimal":"25","unit":"ms"}})
            } else { Value::Null },
            "weighting_rationale":null,"lifecycle":"active","change_reason":null,"material_change":false})
    }

    struct Table {
        state: State,
        case: Id,
        options: [Id; 3],
        hard: Id,
        second_hard: Id,
        soft: [Id; 2],
        failing_cell: Id,
    }

    /// Options A, B, C on one question. A fails hard criterion H on a
    /// verified comparison, B meets it, C is unmeasured. A second hard
    /// criterion and two soft criteria have no comparisons at all.
    fn table() -> Table {
        let case = id("case", 1);
        let question = id("q", 2);
        let source = id("src", 3);
        let artifact = id("art", 4);
        let digest = format!("sha256:{}", "a".repeat(64));
        let mut state = State::default();
        add(&mut state, case, case, json!({"title":"Choice"}));
        add(
            &mut state,
            question,
            case,
            json!({"wording":"Which queue?","importance":"high","disposition":"open"}),
        );
        add(&mut state, source, case, json!({}));
        add(
            &mut state,
            artifact,
            case,
            json!({"source":reference(source),"digest":digest,"size_bytes":12,"format":"application/json","transforms":[],"availability":"retained","integrity":"verified","sensitivity":"internal"}),
        );
        let hard = id("crit", 10);
        let second_hard = id("crit", 11);
        let soft = [id("crit", 12), id("crit", 13)];
        add(&mut state, hard, case, criterion(question, "hard"));
        add(&mut state, second_hard, case, criterion(question, "hard"));
        for soft in soft {
            add(&mut state, soft, case, criterion(question, "soft"));
        }
        let options = [id("opt", 20), id("opt", 21), id("opt", 22)];
        for option in options {
            add(
                &mut state,
                option,
                case,
                json!({"question_ref":reference(question),"description":"Option","context":null,"feasibility":null,"dependencies":[],"tradeoff_notes":[],"lifecycle":"eligible","disposition_reason":null,"amendment_reason":null,"criterion_ref":null}),
            );
        }
        for (index, (option, value)) in [(options[0], 40), (options[1], 10)].into_iter().enumerate()
        {
            let observation = id("obs", 30 + index as u64);
            add(
                &mut state,
                observation,
                case,
                json!({
                    "value":{"kind":"measurement","value":value.to_string(),"unit":"ms"},
                    "basis":{"kind":"source","artifact_ref":reference(artifact),"selector":{"schema_version":"inv.selector.v1","artifact_digest":digest,"selector":{"type":"whole"},"validation":"verified"},"text_quote":{"exact":null,"prefix":null,"suffix":null,"state":"not_applicable"}},
                    "validity":"valid","quality":[],"limitations":[],"variables_changed":[],
                }),
            );
            add(
                &mut state,
                id("cmp", 40 + index as u64),
                case,
                json!({"option_ref":reference(option),"criterion_ref":reference(hard),
                    "outcome": if value > 25 { "fails" } else { "meets" },"method":"measured",
                    "value":{"decimal":value.to_string(),"unit":"ms"},"basis":null,
                    "evidence_refs":[reference(observation)],"assumption_refs":[],"limitations":null,
                    "uncertainty":"Fixture","rationale":"Fixture","retracted":false,"change_reason":null}),
            );
        }
        Table {
            state,
            case,
            options,
            hard,
            second_hard,
            soft,
            failing_cell: id("cmp", 40),
        }
    }

    fn inventory(table: &Table) -> GapInventory {
        enumerate_gaps(&table.state, &snapshot(), table.case).unwrap()
    }

    fn reads(gap: &Gap, id: Id) -> bool {
        gap.read_set.iter().any(|read| read.id == id)
    }

    #[test]
    fn soft_cells_on_a_hard_disqualified_option_are_withheld_not_suggested() {
        let table = table();
        let inventory = inventory(&table);
        let [disqualified, eligible, unmeasured] = table.options;
        let soft_on_disqualified = inventory
            .gaps
            .iter()
            .filter(|gap| gap.rule_id == SOFT_CELL_MISSING && reads(gap, disqualified))
            .count();
        assert_eq!(soft_on_disqualified, 2, "the inventory still lists them");

        let next = rank_next(&table.state, &inventory, DEFAULT_NEXT_LIMIT);
        assert_eq!(next.rule_version, NEXT_RULE_VERSION);
        for recommendation in &next.recommendations {
            assert!(
                !(recommendation.rule_id == SOFT_CELL_MISSING
                    && recommendation
                        .subject_refs
                        .iter()
                        .any(|subject| subject.id == disqualified)),
                "{recommendation:?}"
            );
        }
        assert_eq!(next.withheld.len(), 1);
        let withheld = &next.withheld[0];
        assert_eq!(withheld.option_ref.id, disqualified);
        assert_eq!(withheld.gap_count, 2);
        assert_eq!(
            withheld.determining_refs,
            vec![RevisionRef {
                id: table.failing_cell,
                revision: 1
            }]
        );
        // The withheld filter selects exactly the withheld gaps.
        assert_eq!(
            inventory
                .gaps
                .iter()
                .filter(|gap| withheld.filter.matches(gap))
                .count(),
            2
        );

        // Hard cells come first; the soft group for the live options is next.
        let effects: Vec<_> = next
            .recommendations
            .iter()
            .map(|item| item.decision_value.decision_effect.as_str())
            .collect();
        assert_eq!(effects, [FLIP, FLIP, DISCRIMINATE]);
        let soft = &next.recommendations[2];
        assert_eq!(soft.rule_id, SOFT_CELL_MISSING);
        let group = soft.group.as_ref().expect("soft cells are grouped");
        assert!(table.soft.contains(&group.criterion_ref.id));
        assert_eq!(
            (group.gap_count, group.ranked_count, group.withheld_count),
            (3, 2, 1)
        );
        assert!(
            soft.subject_refs
                .iter()
                .any(|subject| [eligible, unmeasured].contains(&subject.id))
        );
        let second = next
            .recommendations
            .iter()
            .find_map(|item| {
                item.group
                    .as_ref()
                    .filter(|group| group.criterion_ref.id == table.second_hard)
            })
            .expect("the second hard criterion is one grouped recommendation");
        assert_eq!(second.gap_count, 3);
        // Two soft groups and three hard recommendations exist; three are shown.
        assert_eq!(next.remaining_recommendations, 1);
        assert_eq!(
            next.ranked_gap_total,
            inventory
                .gaps
                .iter()
                .filter(|gap| eligible_status(gap))
                .count()
                - 2
        );
        assert!(next.stop_or_defer.is_none());
        assert_eq!(
            rank_next(&table.state, &inventory, DEFAULT_NEXT_LIMIT),
            next,
            "deterministic"
        );
    }

    fn eligible_status(gap: &Gap) -> bool {
        eligible(gap.status)
    }

    #[test]
    fn a_step_that_cannot_change_the_disposition_is_ranked_by_its_narrower_value() {
        let table = table();
        let [disqualified, _, _] = table.options;
        let mut inventory = inventory(&table);
        // Only the gaps about the disqualified option.
        inventory
            .gaps
            .retain(|gap| reads(gap, disqualified) && gap.rule_id.starts_with("decision."));
        let next = rank_next(&table.state, &inventory, DEFAULT_NEXT_LIMIT);
        assert_eq!(next.recommendations.len(), 1);
        let hard = &next.recommendations[0];
        assert_eq!(hard.rule_id, HARD_CELL_MISSING);
        assert!(reads_ref(&hard.subject_refs, table.second_hard));
        let value = &hard.decision_value;
        assert_eq!(value.rule_decision_effect, FLIP);
        assert_eq!(value.decision_effect, CONTEXT);
        assert!(value.every_outcome_unchanged);
        assert_eq!(value.pareto_status, Some(ParetoStatus::DisqualifiedHard));
        assert!(value.narrower_value.is_some());
        assert!(
            value
                .basis_refs
                .iter()
                .any(|reference| reference.id == table.failing_cell)
        );
        assert_eq!(hard.outcome_branches.len(), 3);
        assert!(
            hard.outcome_branches
                .iter()
                .all(|branch| branch.consequence.contains("stays disqualified_hard"))
        );
        assert!(
            hard.expected_useful_result
                .contains("leaves the disposition unchanged")
        );
        let value_reason = hard
            .rank_reasons
            .iter()
            .find(|reason| reason.factor == RankFactor::DecisionChangingValue)
            .unwrap();
        assert_eq!(value_reason.priority, OrdinalPriority::Low);
        assert!(value_reason.source.contains("narrower value"));
        assert_eq!(next.withheld[0].gap_count, 2);
        let stop = next
            .stop_or_defer
            .expect("nothing left can change the choice");
        assert!(
            stop.alternatives
                .iter()
                .any(|alternative| alternative.operation == "decision.propose")
        );
        assert!(stop.basis.iter().any(|line| line.contains("withheld")));
    }

    fn reads_ref(refs: &[RevisionRef], id: Id) -> bool {
        refs.iter().any(|reference| reference.id == id)
    }

    #[test]
    fn every_recommendation_names_all_eight_factors_and_their_sources() {
        let table = table();
        let inventory = inventory(&table);
        let next = rank_next(&table.state, &inventory, DEFAULT_NEXT_LIMIT);
        assert!(!next.recommendations.is_empty());
        for recommendation in &next.recommendations {
            let factors: Vec<_> = recommendation
                .rank_reasons
                .iter()
                .map(|reason| reason.factor)
                .collect();
            assert_eq!(
                factors,
                [
                    RankFactor::CanonicalIntegrity,
                    RankFactor::TimeSensitivePreservation,
                    RankFactor::ConsequentialBlocker,
                    RankFactor::DecisionChangingValue,
                    RankFactor::Applicability,
                    RankFactor::BoundedCost,
                    RankFactor::Novelty,
                    RankFactor::StableIdCreationOrder,
                ]
            );
            assert!(
                recommendation
                    .rank_reasons
                    .iter()
                    .all(|reason| !reason.source.trim().is_empty())
            );
            assert!(!recommendation.decision_value.basis.is_empty());
            let applicability = &recommendation.rank_reasons[4];
            assert_eq!(applicability.priority, OrdinalPriority::High);
            assert!(applicability.source.contains("importance high"));
            assert_eq!(applicability.source_refs[0].id, id("q", 2));
        }
        let text = serde_json::to_string(&next).unwrap();
        assert!(!text.contains("\"score"));
        assert!(!text.contains("\"probability"));
        // Not-ranked statuses are always reported with their counts.
        let statuses: Vec<_> = next.not_ranked.iter().map(|entry| entry.status).collect();
        assert_eq!(
            statuses,
            [
                GapStatus::BlockedByQuality,
                GapStatus::Deferred,
                GapStatus::Resolved
            ]
        );
        assert_eq!(next.not_ranked[0].count, next.counts.blocked_by_quality);
        let _ = table.hard;
    }

    fn gap(rule_id: &str, subject: Id, effect: &str, status: GapStatus, sequence: u64) -> Gap {
        make_gap(GapFields {
            rule_id,
            subject_refs: vec![RevisionRef {
                id: subject,
                revision: 1,
            }],
            read_set: vec![],
            status,
            reason: "fixture",
            prerequisites: vec![],
            suggested_operation: Some("assessment.add"),
            unresolved_fields: vec![],
            outcome_branches: vec![],
            decision_effect: effect,
            time_sensitive: false,
            creation_sequence: sequence,
        })
    }

    fn bare_inventory(case: Id, gaps: Vec<Gap>) -> GapInventory {
        GapInventory {
            schema_version: "inv.gaps.v1".into(),
            snapshot: snapshot(),
            rule_version: GAP_RULE_VERSION.into(),
            case_id: case,
            gaps,
            limits: vec!["not completion".into()],
        }
    }

    #[test]
    fn rivals_predictions_and_prior_attempts_are_read_from_recorded_structure() {
        let case = id("case", 1);
        let question = id("q", 2);
        let lone_question = id("q", 3);
        let mut state = State::default();
        add(&mut state, case, case, json!({"title":"Explain"}));
        add(
            &mut state,
            question,
            case,
            json!({"wording":"Why?","importance":"normal","disposition":"open"}),
        );
        add(
            &mut state,
            lone_question,
            case,
            json!({"wording":"What?","importance":"low","disposition":"answered"}),
        );
        let hypothesis = |n| id("hyp", n);
        for (n, q) in [(10, question), (11, question), (12, lone_question)] {
            add(
                &mut state,
                hypothesis(n),
                case,
                json!({"question_ref":reference(q),"statement":"Account","rival_refs":[]}),
            );
        }
        add(
            &mut state,
            id("assess", 20),
            case,
            json!({"subject_ref":reference(hypothesis(10)),"relation":"inconclusive","evidence_refs":[]}),
        );
        let plan = |predictions: Value| json!({"purpose":"Discriminate","inquiry":{"mode":"confirmatory","confirmatory":{"predictions":predictions}},"lifecycle":"draft"});
        let discriminating = id("inq", 30);
        let same = id("inq", 31);
        let empty = id("inq", 32);
        add(
            &mut state,
            discriminating,
            case,
            plan(json!([
                {"rival_ref":reference(hypothesis(10)),"predicted_outcome":"Latency drops"},
                {"rival_ref":reference(hypothesis(11)),"predicted_outcome":"Latency holds"},
            ])),
        );
        add(
            &mut state,
            same,
            case,
            plan(json!([
                {"rival_ref":reference(hypothesis(10)),"predicted_outcome":"Latency drops"},
                {"rival_ref":reference(hypothesis(11)),"predicted_outcome":"Latency drops"},
            ])),
        );
        add(&mut state, empty, case, plan(json!([])));
        add(
            &mut state,
            id("run", 40),
            case,
            json!({"intent":{"kind":"plan","plan_ref":reference(discriminating)},"execution_status":"failed","evidence_completeness":"missing"}),
        );

        let inventory = bare_inventory(
            case,
            vec![
                gap(
                    NO_ASSESSMENT,
                    hypothesis(10),
                    DISCRIMINATE,
                    GapStatus::RequiresSource,
                    1,
                ),
                gap(
                    NO_ASSESSMENT,
                    hypothesis(11),
                    DISCRIMINATE,
                    GapStatus::RequiresSource,
                    2,
                ),
                gap(
                    NO_ASSESSMENT,
                    hypothesis(12),
                    DISCRIMINATE,
                    GapStatus::RequiresSource,
                    3,
                ),
                gap(
                    UNFROZEN_PLAN,
                    discriminating,
                    DISCRIMINATE,
                    GapStatus::ReadyToAnalyze,
                    4,
                ),
                gap(
                    UNFROZEN_PLAN,
                    same,
                    DISCRIMINATE,
                    GapStatus::ReadyToAnalyze,
                    5,
                ),
                gap(
                    UNFROZEN_PLAN,
                    empty,
                    DISCRIMINATE,
                    GapStatus::ReadyToAnalyze,
                    6,
                ),
            ],
        );
        let context = Context::build(&state, &inventory);
        let analyses: Vec<_> = inventory
            .gaps
            .iter()
            .map(|gap| context.analyze(gap))
            .collect();
        let effect = |index: usize| analyses[index].value.decision_effect.as_str();
        // Two live rivals on one question discriminate; a lone account does not.
        assert_eq!(effect(0), DISCRIMINATE);
        assert_eq!(effect(1), DISCRIMINATE);
        assert_eq!(effect(2), CONTEXT);
        // Distinct per-rival predictions discriminate and become the branches.
        assert_eq!(effect(3), DISCRIMINATE);
        let branches = analyses[3].branches.as_ref().unwrap();
        assert_eq!(
            branches
                .iter()
                .map(|branch| branch.outcome.as_str())
                .collect::<Vec<_>>(),
            ["Latency drops", "Latency holds"]
        );
        // Identical predictions leave every outcome unchanged.
        assert_eq!(effect(4), CONTEXT);
        assert!(analyses[4].value.every_outcome_unchanged);
        // No predictions: the branch effects are unknown and must be asked for.
        assert_eq!(effect(5), UNKNOWN);
        assert!(
            analyses[5]
                .value
                .basis
                .contains("Record a predicted outcome")
        );

        // Novelty: an inconclusive assessment and a failed run are earlier attempts.
        assert_eq!(analyses[0].novelty.1, OrdinalPriority::Low);
        assert_eq!(
            analyses[0].novelty.3,
            vec![RevisionRef {
                id: id("assess", 20),
                revision: 1
            }]
        );
        assert_eq!(analyses[1].novelty.1, OrdinalPriority::High);
        assert_eq!(analyses[3].novelty.1, OrdinalPriority::Low);
        assert_eq!(analyses[4].novelty.1, OrdinalPriority::High);
        // Applicability: open/normal question vs an answered question.
        assert_eq!(analyses[0].applicability.1, OrdinalPriority::Medium);
        assert_eq!(analyses[2].applicability.1, OrdinalPriority::Low);
        assert!(analyses[2].applicability.2.contains("answered"));
        assert_eq!(analyses[3].applicability.1, OrdinalPriority::Unknown);

        let next = rank_next(&state, &inventory, DEFAULT_NEXT_LIMIT);
        let order: Vec<_> = next
            .recommendations
            .iter()
            .map(|item| item.subject_refs[0].id)
            .collect();
        // Same value and applicability: the novel hypothesis outranks the
        // repeated one; the discriminating plan follows on applicability.
        assert_eq!(order, [hypothesis(11), hypothesis(10), discriminating]);
        assert!(next.stop_or_defer.is_none());
    }

    #[test]
    fn freshness_and_advisory_flag_gaps_on_comparisons_read_the_cell_they_describe() {
        let table = table();
        let [disqualified, eligible, _] = table.options;
        let comparison_gap = |rule: &str, comparison: Id, effect: &str, status| {
            gap(rule, comparison, effect, status, 1)
        };
        let inventory = bare_inventory(
            table.case,
            vec![
                // Rechecking the comparison that disqualifies A could restore it.
                comparison_gap(
                    FRESHNESS,
                    table.failing_cell,
                    FLIP,
                    GapStatus::StaleDueToRevision,
                ),
                // An advisory flag on B's hard-criterion comparison.
                comparison_gap(
                    "measurement.flat_samples",
                    id("cmp", 41),
                    FLIP,
                    GapStatus::ReadyToAnalyze,
                ),
            ],
        );
        let context = Context::build(&table.state, &inventory);
        let fresh = context.analyze(&inventory.gaps[0]);
        let flag = context.analyze(&inventory.gaps[1]);
        assert_eq!(fresh.value.decision_effect, FLIP);
        assert_eq!(
            fresh.value.pareto_status,
            Some(ParetoStatus::DisqualifiedHard)
        );
        assert!(fresh.value.basis.contains(&disqualified.to_string()));
        assert!(!fresh.advisory);
        assert_eq!(flag.value.decision_effect, FLIP);
        assert!(flag.value.basis.contains(&eligible.to_string()));
        assert!(flag.advisory && flag.value.basis.contains("Advisory"));
        // Same decision value, but an advisory flag is never a consequential blocker.
        assert_eq!(fresh.key(&inventory.gaps[0]).2, 0);
        assert_eq!(flag.key(&inventory.gaps[1]).2, 1);
        let next = rank_next(&table.state, &inventory, DEFAULT_NEXT_LIMIT);
        assert_eq!(next.recommendations[0].rule_id, FRESHNESS);
        let blocker = &next.recommendations[1].rank_reasons[2];
        assert_eq!(blocker.priority, OrdinalPriority::NotApplicable);
        assert!(blocker.source.contains("advisory"));
        let _ = table.hard;
    }

    #[test]
    fn next_is_stable_lexicographic_and_never_emits_a_score_or_shell_command() {
        let case = id("case", 1);
        let mut later = gap(
            "z.rule",
            id("hyp", 2),
            CONTEXT,
            GapStatus::RequiresSource,
            2,
        );
        later.subject_refs.clear();
        let mut earlier = gap("a.rule", id("hyp", 3), FLIP, GapStatus::RequiresSource, 1);
        earlier.subject_refs.clear();
        let inventory = bare_inventory(case, vec![later, earlier]);
        let first = rank_next(&State::default(), &inventory, 3);
        let second = rank_next(&State::default(), &inventory, 3);
        assert_eq!(first, second);
        assert_eq!(first.recommendations[0].rule_id, "a.rule");
        assert_eq!(first.recommendations.len(), 2);
        assert!(
            first
                .recommendations
                .iter()
                .all(|item| item.estimated_cost["state"] == "unknown")
        );
        assert!(
            !serde_json::to_value(&first)
                .unwrap()
                .to_string()
                .contains("score")
        );
    }

    #[test]
    fn blockers_and_deferred_items_are_counted_but_not_ranked_as_actions() {
        let case = id("case", 1);
        let inventory = bare_inventory(
            case,
            vec![
                gap(
                    "blocked",
                    id("obs", 2),
                    UNKNOWN,
                    GapStatus::BlockedByQuality,
                    1,
                ),
                gap("deferred", id("hyp", 3), UNKNOWN, GapStatus::Deferred, 2),
            ],
        );
        let next = rank_next(&State::default(), &inventory, DEFAULT_NEXT_LIMIT);
        assert!(next.recommendations.is_empty());
        assert_eq!(next.counts.blocked_by_quality, 1);
        assert_eq!(next.counts.deferred, 1);
        assert_eq!(next.not_ranked[0].count, 1);
        assert_eq!(next.not_ranked[1].count, 1);
        assert!(inventory.gaps.iter().all(|gap| {
            next.not_ranked
                .iter()
                .filter(|entry| entry.filter.matches(gap))
                .count()
                == 1
        }));
        // Nothing is ranked, so there is no basis for a stop offer either.
        assert!(next.stop_or_defer.is_none());
    }
}
