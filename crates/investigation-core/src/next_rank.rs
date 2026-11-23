//! Deterministic, ordinal ranking of the gap inventory for `next` (plan §7.2).
//!
//! Ranking reads recorded structure only: the weight-free Pareto status of the
//! options a cell gap concerns, recorded rival hypotheses and per-rival
//! predictions, the disposition and importance of the question a gap serves,
//! and earlier unknown, retracted, failed or inconclusive attempts on the same
//! subject. The eight factors are compared lexicographically and each one is
//! exposed with its source. Nothing here is a probability, an expected
//! information gain, a cost estimate or a score, and nothing is summed.

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    ops::Deref,
    sync::{Arc, Mutex, OnceLock, PoisonError},
};

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
    /// Recorded advice feedback: a lapsed deferral says why it re-surfaced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advice: Option<crate::advice_feedback::GapAdvice>,
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
    let facts = Facts::build(state, inventory);
    // Every eligible gap gets its rank key and value class. The words and
    // example references that explain a value are written only for the
    // recommendations shown, by the same analysis (`Context::explain`).
    // Each gap's outcome depends on the replay alone, so a large inventory is
    // analyzed on a few threads; the outcomes come back in inventory order
    // and are folded below exactly as one loop would.
    let outcomes = crate::parallel::map_chunks(
        &inventory.gaps,
        inventory.gaps.len().saturating_mul(RANK_WORK_PER_GAP),
        |gaps| {
            let context = Context::new(&facts);
            gaps.iter().map(|gap| context.outcome(gap)).collect()
        },
    );
    let mut ranked = Vec::new();
    // Which inventory positions were ranked, for the group counts.
    let mut is_ranked = vec![false; inventory.gaps.len()];
    let mut withheld = BTreeMap::<Id, WithheldGaps>::new();
    let mut withheld_ids = BTreeSet::new();
    for (index, (gap, outcome)) in inventory.gaps.iter().zip(outcomes).enumerate() {
        match outcome {
            Outcome::NotEligible => {}
            Outcome::Withheld(option_ref, determining_refs) => {
                withheld_ids.insert(gap.gap_id.as_str());
                withheld
                    .entry(option_ref.id)
                    .or_insert_with(|| WithheldGaps {
                        reason_code: "soft_criterion_on_hard_disqualified_option".into(),
                        explanation: "The option already fails a hard criterion on a verified comparison, so no soft-criterion result can change its disposition or Pareto status. Measuring it is not recommended.".into(),
                        option_ref,
                        determining_refs,
                        gap_count: 0,
                        filter: GapFilter {
                            rule_ids: [SOFT_CELL_MISSING.to_owned()].into(),
                            read_ids: [option_ref.id].into(),
                            ..GapFilter::default()
                        },
                    })
                    .gap_count += 1;
            }
            Outcome::Ranked(item) => {
                is_ranked[index] = true;
                ranked.push(item);
            }
        }
    }
    // The key ends with the gap ID, so ties keep the order a sort of full
    // analyses had.
    ranked.sort_by(|a, b| a.key.cmp(&b.key));

    let mut seen_groups = BTreeSet::new();
    let mut representatives = Vec::new();
    for item in &ranked {
        let gap = item.gap;
        if let Some(criterion) = group_criterion(gap)
            && !seen_groups.insert((criterion.id, gap.rule_id.as_str()))
        {
            continue;
        }
        representatives.push(item);
    }
    let context = Context::new(&facts);
    let recommendations = representatives
        .iter()
        .take(limit)
        .enumerate()
        .map(|(index, item)| {
            let gap = item.gap;
            let (analysis, value) = context.explain(gap);
            debug_assert!(analysis.key(gap) == item.key, "explained key differs");
            let group = group_criterion(gap)
                .map(|criterion| {
                    context.group(inventory, gap, criterion, &is_ranked, &withheld_ids)
                })
                .filter(|group| group.gap_count > 1);
            recommendation(gap, &analysis, value, group, index + 1)
        })
        .collect();

    let ranked_effects: Vec<&str> = ranked.iter().map(|item| item.effect).collect();
    let stalls: Vec<&str> = ranked
        .iter()
        .filter(|item| item.stall)
        .map(|item| item.gap.gap_id.as_str())
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

/// The Pareto status of every eligible option of one question, by option.
struct ParetoTable {
    options: BTreeMap<Id, ParetoOption>,
    /// How many options are not disqualified_hard: each is a rival of every
    /// other option of the question.
    not_disqualified: usize,
}

/// One ranked gap: its key, its value class and whether it is a stall.
struct Ranked<'a> {
    key: RankKey<'a>,
    gap: &'a Gap,
    effect: &'a str,
    stall: bool,
}

/// The analysis of one gap, as the fold in `rank_next` takes it.
enum Outcome<'a> {
    NotEligible,
    /// The option and its determining cells.
    Withheld(RevisionRef, Vec<RevisionRef>),
    Ranked(Ranked<'a>),
}

/// The elementary steps the analysis of one gap takes, for
/// `parallel::map_chunks`.
const RANK_WORK_PER_GAP: usize = 4;

/// A Pareto table shared by the threads of one ranking: computed by the
/// first that needs it, read by the others.
type SharedTable = Arc<Result<ParetoTable, String>>;

/// Everything the ranking reads from one replay, gathered in one pass, and
/// shared by the threads that analyze its gaps.
struct Facts<'a> {
    state: &'a State,
    case_id: Id,
    /// Pareto status by question, computed once for each question that a
    /// cell gap reads, whichever thread reads it first.
    pareto: Mutex<BTreeMap<Id, Arc<OnceLock<SharedTable>>>>,
    /// Unknown or retracted comparisons by (option, criterion).
    prior_cells: BTreeMap<(Id, Id), Vec<RevisionRef>>,
    /// Inconclusive or non-diagnostic assessments by subject hypothesis.
    inconclusive_assessments: BTreeMap<Id, Vec<RevisionRef>>,
    /// Failed, cancelled or blocked runs by the plan they executed.
    failed_runs: BTreeMap<Id, Vec<RevisionRef>>,
    /// Live hypotheses by question.
    live_hypotheses: BTreeMap<Id, Vec<RevisionRef>>,
}

/// The analysis of gaps on one thread: the shared facts, and memos of what
/// every gap of a criterion or question reads again. The memos live as long
/// as this one ranking of one replay.
struct Context<'f, 'a> {
    facts: &'f Facts<'a>,
    /// This thread's handles on the shared Pareto tables.
    tables: RefCell<BTreeMap<Id, SharedTable>>,
    /// Memo of `question_ref` of the current record with an ID. Every gap of
    /// a criterion reads the same records; the memo lives as long as this
    /// one ranking of one replay.
    question_refs: RefCell<BTreeMap<Id, Option<RevisionRef>>>,
    /// Memo of what a cell value reads of a criterion.
    criteria: RefCell<BTreeMap<Id, CriterionFacts<'a>>>,
    /// Memo of `question_of(id, 3)`, the question a gap subject serves.
    subject_questions: RefCell<BTreeMap<Id, Option<&'a Record>>>,
}

/// The rank of one gap, and with `Context::explain` the words and example
/// references of its factors.
struct Analysis<'a> {
    /// The value class: `value.decision_effect` when explained.
    effect: &'a str,
    branches: Option<Vec<OutcomeBranch>>,
    /// The option and its determining cells when the gap is withheld.
    withhold: Option<(RevisionRef, Vec<RevisionRef>)>,
    /// Advisory gaps are never consequential blockers.
    advisory: bool,
    /// Stall gaps rank as consequential: repeating the step that stalled is
    /// unlikely to change the picture.
    stall: bool,
    applicability: Factor,
    novelty: Factor,
}

/// One ordinal factor: its key, its priority, and when explained its source
/// and the records it read (empty otherwise).
struct Factor {
    key: u8,
    priority: OrdinalPriority,
    source: String,
    refs: Vec<RevisionRef>,
}

impl Factor {
    fn new(
        key: u8,
        priority: OrdinalPriority,
        explain: bool,
        words: impl FnOnce() -> (String, Vec<RevisionRef>),
    ) -> Self {
        let (source, refs) = if explain {
            words()
        } else {
            (String::new(), Vec::new())
        };
        Self {
            key,
            priority,
            source,
            refs,
        }
    }
}

/// What a cell value reads of the criterion of the cell.
#[derive(Clone, Copy)]
struct CriterionFacts<'a> {
    /// The current criterion record.
    record: Option<&'a Record>,
    /// Whether it is a hard criterion.
    hard: bool,
    /// The question it names.
    question: Option<RevisionRef>,
}

/// A value class, and when explained its value and rewritten outcome
/// branches, if any.
struct Valued<'a> {
    effect: &'a str,
    explained: Option<(DecisionValue, Option<Vec<OutcomeBranch>>)>,
    withhold: Option<(RevisionRef, Vec<RevisionRef>)>,
}

impl<'a> Valued<'a> {
    /// `effect`, with the value `write` gives it when explained.
    fn new(
        effect: &'a str,
        explain: bool,
        write: impl FnOnce(&str) -> (DecisionValue, Option<Vec<OutcomeBranch>>),
    ) -> Self {
        Self {
            effect,
            explained: explain.then(|| write(effect)),
            withhold: None,
        }
    }

    /// The emitting rule's own class, with an optional note on why nothing
    /// narrowed it.
    fn fixed(gap: &'a Gap, explain: bool, note: impl FnOnce() -> Option<String>) -> Self {
        Self::new(&gap.decision_effect, explain, |_| {
            (static_value(gap, note().as_deref()), None)
        })
    }
}

type RankKey<'a> = (u8, u8, u8, u8, u8, u8, u8, u64, &'a str);

impl<'a> Analysis<'a> {
    fn key(&self, gap: &'a Gap) -> RankKey<'a> {
        let integrity = u8::from(!gap.rule_id.starts_with("store.integrity."));
        let preservation = u8::from(!gap.time_sensitive);
        let effect = self.effect;
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
            self.applicability.key,
            cost,
            self.novelty.key,
            gap.creation_sequence,
            gap.gap_id.as_str(),
        )
    }
}

impl<'a> Facts<'a> {
    fn build(state: &'a State, inventory: &GapInventory) -> Self {
        let mut context = Self {
            state,
            case_id: inventory.case_id,
            pareto: Mutex::default(),
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
        context
    }

    /// The Pareto table of a question.
    fn table(&self, question: Id) -> Result<ParetoTable, String> {
        pareto(self.state, self.case_id, question)
            .map(|options| {
                let not_disqualified = options
                    .iter()
                    .filter(|option| option.status != ParetoStatus::DisqualifiedHard)
                    .count();
                ParetoTable {
                    options: options
                        .into_iter()
                        .map(|option| (option.option_ref.id, option))
                        .collect(),
                    not_disqualified,
                }
            })
            .map_err(|failure| failure.error.message)
    }
}

impl<'a> Deref for Context<'_, 'a> {
    type Target = Facts<'a>;

    fn deref(&self) -> &Facts<'a> {
        self.facts
    }
}

impl<'f, 'a> Context<'f, 'a> {
    fn new(facts: &'f Facts<'a>) -> Self {
        Self {
            facts,
            tables: RefCell::default(),
            question_refs: RefCell::default(),
            criteria: RefCell::default(),
            subject_questions: RefCell::default(),
        }
    }

    /// The Pareto table of a question, computed once per ranking.
    fn table(&self, question: Id) -> SharedTable {
        if let Some(table) = self.tables.borrow().get(&question) {
            return Arc::clone(table);
        }
        let cell = Arc::clone(
            self.pareto
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(question)
                .or_default(),
        );
        let table = Arc::clone(cell.get_or_init(|| Arc::new(self.facts.table(question))));
        self.tables
            .borrow_mut()
            .insert(question, Arc::clone(&table));
        table
    }

    /// What the fold in `rank_next` takes of one gap.
    fn outcome<'g>(&self, gap: &'g Gap) -> Outcome<'g> {
        if !eligible(gap.status) {
            return Outcome::NotEligible;
        }
        let analysis = self.analyze(gap);
        match analysis.withhold {
            Some((option_ref, determining)) => Outcome::Withheld(option_ref, determining),
            None => Outcome::Ranked(Ranked {
                key: analysis.key(gap),
                gap,
                effect: analysis.effect,
                stall: analysis.stall,
            }),
        }
    }

    fn current(&self, id: Id) -> Option<&'a Record> {
        self.state.current(id)
    }

    /// `question_ref` of the current record with this ID, read once.
    fn record_question(&self, id: Id) -> Option<RevisionRef> {
        *self
            .question_refs
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| {
                self.current(id)
                    .and_then(|record| question_ref(&record.body))
            })
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

    /// What a cell value reads of a criterion, read once per criterion.
    fn criterion(&self, id: Id) -> CriterionFacts<'a> {
        if let Some(facts) = self.criteria.borrow().get(&id) {
            return *facts;
        }
        let record = self.current(id);
        let facts = CriterionFacts {
            record,
            hard: record.is_some_and(|criterion| {
                field(content(&criterion.body), "kind").and_then(Value::as_str) == Some("hard")
            }),
            question: record.and_then(|record| question_ref(&record.body)),
        };
        self.criteria.borrow_mut().insert(id, facts);
        facts
    }

    /// The current Question a gap subject serves, read once per subject.
    fn subject_question(&self, id: Id) -> Option<&'a Record> {
        if let Some(question) = self.subject_questions.borrow().get(&id) {
            return *question;
        }
        let question = self.question_of(id, 3);
        self.subject_questions.borrow_mut().insert(id, question);
        question
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

    /// The rank of a gap, without the words that explain it.
    fn analyze<'g>(&self, gap: &'g Gap) -> Analysis<'g> {
        self.assess(gap, false).0
    }

    /// The rank of a gap and its explained decision value. The same
    /// analysis as [`Self::analyze`], so the rank is the same.
    fn explain<'g>(&self, gap: &'g Gap) -> (Analysis<'g>, DecisionValue) {
        let (analysis, value) = self.assess(gap, true);
        (
            analysis,
            value.expect("an explained analysis writes its decision value"),
        )
    }

    fn assess<'g>(&self, gap: &'g Gap, explain: bool) -> (Analysis<'g>, Option<DecisionValue>) {
        let cell = self.cell_of(gap);
        let valued = match gap.rule_id.as_str() {
            _ if reads_cell(gap) => self.cell_value(gap, cell, explain),
            NO_ASSESSMENT => self.rival_value(gap, explain),
            UNFROZEN_PLAN => self.prediction_value(gap, explain),
            _ => Valued::fixed(gap, explain, || None),
        };
        let (mut value, branches) = valued.explained.unzip();
        let advisory = is_measurement_flag(gap);
        if advisory && let Some(value) = &mut value {
            value.basis.push_str(" Advisory measurement flag: it points at conditions worth recording and never blocks recording or changes validity.");
        }
        let analysis = Analysis {
            effect: valued.effect,
            branches: branches.flatten(),
            withhold: valued.withhold,
            advisory,
            stall: crate::guidance::STALL_GAP_RULES.contains(&gap.rule_id.as_str()),
            applicability: self.applicability(gap, explain),
            novelty: self.novelty(gap, cell, explain),
        };
        (analysis, value)
    }

    /// `cell` is `self.cell_of(gap)`.
    fn cell_value<'g>(&self, gap: &'g Gap, cell: Option<(Id, Id)>, explain: bool) -> Valued<'g> {
        let Some((option_id, criterion_id)) = cell else {
            return Valued::fixed(gap, explain, || {
                Some("The option and criterion of this cell could not be read.".into())
            });
        };
        let facts = self.criterion(criterion_id);
        // The question of the criterion, else of the option.
        let question = facts.question.or_else(|| self.record_question(option_id));
        let Some(question) = question else {
            return Valued::fixed(gap, explain, || {
                Some("The criterion names no question, so no Pareto status was computed.".into())
            });
        };
        let table = self.table(question.id);
        let table = match table.as_ref() {
            Ok(table) => table,
            Err(message) => {
                return Valued::fixed(gap, explain, || {
                    Some(format!(
                        "Pareto status for question {} is unavailable: {message}",
                        question.id
                    ))
                });
            }
        };
        let Some(standing) = table.options.get(&option_id) else {
            return Valued::fixed(gap, explain, || {
                Some(format!(
                    "Option {option_id} is not an eligible option of question {}, so it has no Pareto status.",
                    question.id
                ))
            });
        };
        let option_ref = standing.option_ref;
        let CriterionFacts {
            record: criterion,
            hard,
            ..
        } = facts;
        let determining: &[RevisionRef] = &standing.determining_cells;
        let revises_determining = determining
            .iter()
            .any(|cell| gap.subject_refs.iter().any(|subject| subject.id == cell.id));
        let refs = || {
            let mut refs = vec![option_ref];
            refs.extend(criterion.map(Record::reference));
            refs
        };
        let basis_refs = || {
            let mut basis_refs = refs();
            basis_refs.extend(determining.iter().copied());
            basis_refs
        };
        let cells = || {
            determining
                .iter()
                .map(|cell| cell.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
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
        let status = || status_name(standing.status);
        match standing.status {
            ParetoStatus::DisqualifiedHard if !revises_determining => {
                if gap.rule_id == SOFT_CELL_MISSING {
                    let mut withheld = Valued::new(CONTEXT, explain, |effect| {
                        let mut withheld = value(
                            effect,
                            format!(
                                "Option {option_id} is disqualified_hard by comparison {}; no soft-criterion result can change that.",
                                cells()
                            ),
                            basis_refs(),
                        );
                        withheld.every_outcome_unchanged = true;
                        (withheld, None)
                    });
                    withheld.withhold = Some((option_ref, determining.to_vec()));
                    return withheld;
                }
                Valued::new(CONTEXT, explain, |effect| {
                    let cells = cells();
                    let mut narrowed = value(
                        effect,
                        format!(
                            "Option {option_id} already fails a hard criterion on verified comparison {cells}. Every outcome of this step leaves it disqualified_hard."
                        ),
                        basis_refs(),
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
                    (narrowed, Some(branches))
                })
            }
            ParetoStatus::StrictlyDominated if !hard && !revises_determining => {
                Valued::new(CONTEXT, explain, |effect| {
                    (
                        value(
                            effect,
                            format!(
                                "Option {option_id} is strictly dominated on recorded cells; soft-criterion measurement on a dominated option is deprioritized."
                            ),
                            basis_refs(),
                        ),
                        None,
                    )
                })
            }
            _ if hard => Valued::new(FLIP, explain, |effect| {
                (
                    value(
                        effect,
                        format!(
                            "The result on this hard criterion decides whether option {option_id} stays eligible; a fail rules it out. Current Pareto status: {}.",
                            status()
                        ),
                        refs(),
                    ),
                    None,
                )
            }),
            _ => {
                // Every option of the question but this one that is not
                // disqualified_hard is a rival; this option is in the table.
                let count = table.not_disqualified
                    - usize::from(standing.status != ParetoStatus::DisqualifiedHard);
                if count == 0 {
                    Valued::new(CONTEXT, explain, |effect| {
                        let mut narrowed = value(
                            effect,
                            format!(
                                "No other option of question {} is eligible and not disqualified, so no result on this criterion can change which option is on the Pareto frontier.",
                                question.id
                            ),
                            refs(),
                        );
                        narrowed.every_outcome_unchanged = true;
                        narrowed.narrower_value = Some("Records the option's standing on this criterion for the decision record and later comparison.".into());
                        (narrowed, None)
                    })
                } else {
                    Valued::new(DISCRIMINATE, explain, |effect| {
                        let rivals = table
                            .options
                            .values()
                            .filter(|other| {
                                other.option_ref.id != option_id
                                    && other.status != ParetoStatus::DisqualifiedHard
                            })
                            .map(|other| other.option_ref)
                            .take(BASIS_REF_LIMIT);
                        let mut refs = refs();
                        refs.extend(rivals);
                        (
                            value(
                                effect,
                                format!(
                                    "A recorded result on this criterion could change option {option_id}'s Pareto status ({}) against {count} other eligible option(s).",
                                    status()
                                ),
                                refs,
                            ),
                            None,
                        )
                    })
                }
            }
        }
    }

    fn rival_value<'g>(&self, gap: &'g Gap, explain: bool) -> Valued<'g> {
        let Some(hypothesis) = gap
            .subject_refs
            .first()
            .and_then(|subject| self.current(subject.id))
        else {
            return Valued::fixed(gap, explain, || None);
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
        let write = |effect: &str, basis: String| {
            let value = DecisionValue {
                decision_effect: effect.into(),
                rule_decision_effect: gap.decision_effect.clone(),
                basis,
                basis_refs: rivals.iter().copied().take(BASIS_REF_LIMIT).collect(),
                every_outcome_unchanged: false,
                narrower_value: None,
                pareto_status: None,
            };
            (value, None)
        };
        if count == 0 {
            return Valued::new(CONTEXT, explain, |effect| {
                write(effect, "No live rival hypothesis is recorded for this hypothesis's question or in its rival_refs. An assessment can change its standing but cannot discriminate rivals.".into())
            });
        }
        Valued::new(DISCRIMINATE, explain, |effect| {
            write(
                effect,
                format!(
                    "An assessment could separate this hypothesis from {count} live rival hypothesis(es) recorded for its question or in its rival_refs."
                ),
            )
        })
    }

    fn prediction_value<'g>(&self, gap: &'g Gap, explain: bool) -> Valued<'g> {
        let Some(inquiry) = gap
            .subject_refs
            .first()
            .and_then(|subject| self.current(subject.id))
        else {
            return Valued::fixed(gap, explain, || None);
        };
        let by_outcome = live_predictions_by_outcome(self.state, inquiry);
        let rivals: BTreeSet<RevisionRef> = by_outcome.values().flatten().copied().collect();
        let value = |effect: &str, basis: String| DecisionValue {
            decision_effect: effect.into(),
            rule_decision_effect: gap.decision_effect.clone(),
            basis,
            basis_refs: rivals.iter().copied().take(BASIS_REF_LIMIT).collect(),
            every_outcome_unchanged: false,
            narrower_value: None,
            pareto_status: None,
        };
        if rivals.is_empty() {
            return Valued::new(UNKNOWN, explain, |effect| {
                (value(effect, "The plan records no prediction for a live rival, so what each outcome would change is unknown. Record a predicted outcome for each live rival.".into()), None)
            });
        }
        if rivals.len() >= 2 && by_outcome.len() >= 2 {
            return Valued::new(DISCRIMINATE, explain, |effect| {
                let value = value(
                    effect,
                    format!(
                        "The plan records {} distinct predicted outcomes across {} live rivals, so the result can discriminate them.",
                        by_outcome.len(),
                        rivals.len()
                    ),
                );
                let branches = by_outcome
                    .iter()
                    .map(|(outcome, rivals)| OutcomeBranch {
                        outcome: outcome.clone(),
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
                (value, Some(branches))
            });
        }
        Valued::new(CONTEXT, explain, |effect| {
            let mut value = value(effect, "The recorded predictions name one live rival, or the same outcome for every live rival, so no recorded outcome discriminates rivals.".into());
            value.every_outcome_unchanged = true;
            value.narrower_value = Some(
                "Freezing still locks the predictions, controls and stop rule before outcomes are seen."
                    .into(),
            );
            (value, None)
        })
    }

    fn applicability(&self, gap: &Gap, explain: bool) -> Factor {
        const DEADLINE: &str = "No structured decision deadline is recorded.";
        let Some(question) = gap
            .subject_refs
            .iter()
            .find_map(|subject| self.subject_question(subject.id))
        else {
            return Factor::new(2, OrdinalPriority::Unknown, explain, || {
                (
                    format!("The gap's subjects name no question. {DEADLINE}"),
                    vec![],
                )
            });
        };
        let body = content(&question.body);
        let disposition = field(body, "disposition")
            .and_then(Value::as_str)
            .unwrap_or("open");
        let importance = field(body, "importance")
            .and_then(Value::as_str)
            .unwrap_or("normal");
        let refs = || vec![question.reference()];
        let id = question.header.id;
        if disposition != "open" {
            return Factor::new(3, OrdinalPriority::Low, explain, || {
                (
                    format!("Question {id} is recorded as {disposition}. {DEADLINE}"),
                    refs(),
                )
            });
        }
        let (key, priority) = match importance {
            "high" => (0, OrdinalPriority::High),
            "low" => (3, OrdinalPriority::Low),
            _ => (1, OrdinalPriority::Medium),
        };
        Factor::new(key, priority, explain, || {
            (
                format!("Question {id} is open with recorded importance {importance}. {DEADLINE}"),
                refs(),
            )
        })
    }

    /// `cell` is `self.cell_of(gap)`.
    fn novelty(&self, gap: &Gap, cell: Option<(Id, Id)>, explain: bool) -> Factor {
        let subject = |kind| {
            gap.subject_refs
                .iter()
                .find(|reference| reference.id.kind() == kind)
                .copied()
        };
        fn recorded(attempts: Option<&Vec<RevisionRef>>) -> Cow<'_, [RevisionRef]> {
            Cow::Borrowed(attempts.map_or(&[][..], Vec::as_slice))
        }
        let attempts: Option<Cow<'_, [RevisionRef]>> = match gap.rule_id.as_str() {
            HARD_CELL_MISSING | HARD_CELL_UNKNOWN | SOFT_CELL_MISSING | STALE_COMPARISON => {
                cell.map(|cell| recorded(self.prior_cells.get(&cell)))
            }
            NO_ASSESSMENT => subject(Kind::Hypothesis)
                .map(|hypothesis| recorded(self.inconclusive_assessments.get(&hypothesis.id))),
            UNFROZEN_PLAN => {
                subject(Kind::Inquiry).map(|inquiry| recorded(self.failed_runs.get(&inquiry.id)))
            }
            RUN_INCOMPLETE => subject(Kind::Run).and_then(|run| {
                let plan = run_plan(self.current(run.id)?)?;
                Some(Cow::Owned(
                    self.failed_runs
                        .get(&plan.id)
                        .into_iter()
                        .flatten()
                        .filter(|other| other.id != run.id)
                        .copied()
                        .collect(),
                ))
            }),
            _ => None,
        };
        match attempts {
            None => Factor::new(1, OrdinalPriority::Unknown, explain, || {
                (
                    "This rule's subject has no attempt history that the ranking reads.".into(),
                    vec![],
                )
            }),
            Some(attempts) if attempts.is_empty() => {
                Factor::new(0, OrdinalPriority::High, explain, || {
                    (
                        "No earlier unknown, retracted, failed or inconclusive attempt is recorded on this subject.".into(),
                        vec![],
                    )
                })
            }
            Some(attempts) => Factor::new(2, OrdinalPriority::Low, explain, || {
                (
                    format!(
                        "{} earlier attempt(s) on this subject ended unknown, retracted, failed or inconclusive; repeating the same step may add little.",
                        attempts.len()
                    ),
                    attempts.iter().copied().take(BASIS_REF_LIMIT).collect(),
                )
            }),
        }
    }

    /// The group a shown recommendation stands for. `is_ranked` marks the
    /// ranked inventory positions; `withheld` names the withheld gaps.
    fn group(
        &self,
        inventory: &GapInventory,
        gap: &Gap,
        criterion: RevisionRef,
        is_ranked: &[bool],
        withheld: &BTreeSet<&str>,
    ) -> GapGroup {
        let filter = GapFilter {
            rule_ids: [gap.rule_id.clone()].into(),
            read_ids: [criterion.id].into(),
            ..GapFilter::default()
        };
        let (mut gap_count, mut ranked_count, mut withheld_count) = (0, 0, 0);
        for (index, member) in inventory.gaps.iter().enumerate() {
            // The rule test first: it is the filter's own first test that
            // most gaps fail, and it is cheap.
            if member.rule_id != gap.rule_id || !filter.matches(member) {
                continue;
            }
            gap_count += 1;
            ranked_count += usize::from(is_ranked[index]);
            withheld_count += usize::from(withheld.contains(member.gap_id.as_str()));
        }
        GapGroup {
            group_id: format!("group:{}:{}", gap.rule_id, criterion.id),
            rule_id: gap.rule_id.clone(),
            criterion_ref: criterion,
            question_ref: self
                .current(criterion.id)
                .and_then(|record| question_ref(&record.body)),
            gap_count,
            ranked_count,
            withheld_count,
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
    value: DecisionValue,
    group: Option<GapGroup>,
    rank: usize,
) -> Recommendation {
    let key = analysis.key(gap);
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
            priority: analysis.applicability.priority,
            source: analysis.applicability.source.clone(),
            source_refs: analysis.applicability.refs.clone(),
        },
        RankReason {
            factor: RankFactor::BoundedCost,
            priority: OrdinalPriority::Unknown,
            source: "no comparable user-supplied cost estimate is recorded".into(),
            source_refs: vec![],
        },
        RankReason {
            factor: RankFactor::Novelty,
            priority: analysis.novelty.priority,
            source: analysis.novelty.source.clone(),
            source_refs: analysis.novelty.refs.clone(),
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
        decision_value: value,
        group,
        uncertainty: "Ordinal deterministic heuristic; not a probability, expected-information calculation, or universal ranking.".into(),
        prerequisites: gap.prerequisites.clone(),
        unresolved_fields: gap.unresolved_fields.clone(),
        estimated_cost: serde_json::json!({"state":"unknown"}),
        external_effects: if external { "consumer_managed" } else { "none" }.into(),
        schema_operation: gap.suggested_operation.clone(),
        outcome_branches: branches,
        deferral_alternative: "Keep the gap visible as deferred or unknown; this tool does not dispatch work.".into(),
        stall: gap.stall.as_deref().cloned(),
        advice: gap.advice.as_deref().cloned(),
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
        let facts = Facts::build(&state, &inventory);
        let context = Context::new(&facts);
        let explained: Vec<_> = inventory
            .gaps
            .iter()
            .map(|gap| context.explain(gap))
            .collect();
        // The cheap analysis that ranks every gap gives the explained key.
        for (gap, (analysis, _)) in inventory.gaps.iter().zip(&explained) {
            assert!(context.analyze(gap).key(gap) == analysis.key(gap));
        }
        let analyses: Vec<_> = explained.iter().map(|(analysis, _)| analysis).collect();
        let values: Vec<_> = explained.iter().map(|(_, value)| value).collect();
        let effect = |index: usize| values[index].decision_effect.as_str();
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
        assert!(values[4].every_outcome_unchanged);
        // No predictions: the branch effects are unknown and must be asked for.
        assert_eq!(effect(5), UNKNOWN);
        assert!(values[5].basis.contains("Record a predicted outcome"));

        // Novelty: an inconclusive assessment and a failed run are earlier attempts.
        assert_eq!(analyses[0].novelty.priority, OrdinalPriority::Low);
        assert_eq!(
            analyses[0].novelty.refs,
            vec![RevisionRef {
                id: id("assess", 20),
                revision: 1
            }]
        );
        assert_eq!(analyses[1].novelty.priority, OrdinalPriority::High);
        assert_eq!(analyses[3].novelty.priority, OrdinalPriority::Low);
        assert_eq!(analyses[4].novelty.priority, OrdinalPriority::High);
        // Applicability: open/normal question vs an answered question.
        assert_eq!(analyses[0].applicability.priority, OrdinalPriority::Medium);
        assert_eq!(analyses[2].applicability.priority, OrdinalPriority::Low);
        assert!(analyses[2].applicability.source.contains("answered"));
        assert_eq!(analyses[3].applicability.priority, OrdinalPriority::Unknown);

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
        let facts = Facts::build(&table.state, &inventory);
        let context = Context::new(&facts);
        let (fresh, fresh_value) = context.explain(&inventory.gaps[0]);
        let (flag, flag_value) = context.explain(&inventory.gaps[1]);
        assert_eq!(fresh_value.decision_effect, FLIP);
        assert_eq!(
            fresh_value.pareto_status,
            Some(ParetoStatus::DisqualifiedHard)
        );
        assert!(fresh_value.basis.contains(&disqualified.to_string()));
        assert!(!fresh.advisory);
        assert_eq!(flag_value.decision_effect, FLIP);
        assert!(flag_value.basis.contains(&eligible.to_string()));
        assert!(flag.advisory && flag_value.basis.contains("Advisory"));
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
