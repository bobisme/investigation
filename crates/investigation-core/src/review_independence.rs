//! Review independence as declared and attributed (§14.2, §8.5).
//!
//! Nothing here infers independence. A Review declares its `context_isolation`
//! and its context: whether the reviewer saw the original conclusion, gathered
//! new evidence, what kind of work it was, prior involvement and whether it
//! read other reviews. An undeclared attribute stays `undeclared`; an unknown
//! one stays `unknown`. This module reads those declarations back and adds
//! three things computed from the journal:
//!
//! - **Low independence.** A `same_session_self_review`, a declared
//!   `self_review`, and a reviewer who recorded the subject's current content
//!   are flagged. The last is a procedural actor-string check: a different
//!   actor string never makes a review independent.
//! - **Dependence groups.** Reviews whose self-reported actor descriptors name
//!   one model family form one group. A human reviewer, or an agent whose
//!   family is not declared, is grouped by actor string alone. Descriptors are
//!   claims, like the actor string, and a different family is not independent
//!   on that basis alone.
//! - **Pair order.** For two reviews of one subject, journal order and recorded
//!   reads show whether the later one could have read the earlier one. Only a
//!   later review whose audited snapshot precedes the earlier review, with no
//!   recorded or declared read, is shown as recorded independently of it.
//!
//! Recording is never refused for any of this. Views flag; a pinned pack may
//! require a review with no low-independence flag at a promotion gate
//! through the `isolated_review` policy input.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    command::RevisionRef,
    header::{Actor, ActorDescriptor, ActorKind, Knowledge},
    identity::{Id, Kind},
    pack_policy::content_authors,
    review::{ContextIsolation, Dependence, ReviewContext, ReviewWorkKind},
    review_ops::{ValidationReviewBody, validation_review_current},
    state::{Record, State},
    wire::Failure,
};

/// Version of the rules in this module.
pub const RULE_VERSION: &str = "review-independence.v1";

/// Stated with every independence view.
pub const NOTICE: &str = "Independence attributes are declared by the reviewer and attributed to the recording actor; actor descriptors are self-reported claims. Reviews from one model family form one dependence group, and a different family is not independent on that basis alone. Actor-ID inequality is merely a procedural check.";

/// Most review pairs listed in one view.
const MAX_PAIRS: usize = 64;

/// Subjects that reviews can name and whose views carry an independence summary.
const SUBJECTS: &[Kind] = &[
    Kind::Case,
    Kind::Question,
    Kind::Conclusion,
    Kind::Decision,
    Kind::Hypothesis,
    Kind::Comparison,
];

/// A declaration as read back: absent from the record, explicitly unknown, or
/// known.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum Declared<T> {
    Undeclared,
    Unknown,
    Known(T),
}

impl<T: Clone> Declared<T> {
    fn from(value: Option<&Knowledge<T>>) -> Self {
        match value {
            None => Self::Undeclared,
            Some(Knowledge::Unknown) => Self::Unknown,
            Some(Knowledge::Known(value)) => Self::Known(value.clone()),
        }
    }
}

/// The reviewer's declarations, as recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Declarations {
    pub context_isolation: ContextIsolation,
    pub saw_original_conclusion: Declared<bool>,
    pub gathered_new_evidence: Declared<bool>,
    pub work_kind: Declared<ReviewWorkKind>,
    pub prior_involvement: Declared<Vec<String>>,
    pub read_other_reviews: Declared<bool>,
    pub dependencies: Dependence,
}

/// Why a review is flagged as low-independence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LowIndependence {
    /// Declared `context_isolation: same_session_self_review`.
    SameSessionSelfReview,
    /// Declared `work_kind: self_review`.
    DeclaredSelfReview,
    /// The recording actor string also recorded the subject's current
    /// substantive content. A procedural check only.
    ReviewerRecordedSubject,
}

/// What a dependence group was formed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupBasis {
    /// A self-reported agent model family.
    ModelFamily,
    /// A self-reported human reviewer, grouped by actor string.
    HumanActor,
    /// No declared model family; grouped by actor string alone.
    ActorFamilyUndeclared,
}

/// One review of a subject with its declarations and computed flags.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewEntry {
    pub review_ref: RevisionRef,
    /// `validation` or `blindspot`.
    pub kind: String,
    pub verdict: String,
    /// The subject revision the review names.
    pub subject_revision: u64,
    /// Acceptance sequence of the review's first revision: its journal order.
    pub recorded_at_sequence: u64,
    /// For a blindspot review, the snapshot sequence the reviewer audited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audited_sequence: Option<u64>,
    pub recorded_by: Actor,
    /// Self-reported; a claim like the actor string.
    pub actor_descriptor_self_reported: Option<ActorDescriptor>,
    pub declarations: Declarations,
    /// Declarations of this review that are absent from its record.
    pub undeclared: Vec<&'static str>,
    pub low_independence: bool,
    pub low_independence_reasons: Vec<LowIndependence>,
    /// The dependence group this review belongs to.
    pub dependence_group: String,
    /// Other reviews of the same subject named in this review's read set.
    pub reviews_read: Vec<RevisionRef>,
}

/// Reviews whose self-reported descriptors place them in one group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DependenceGroup {
    pub group: String,
    pub basis: GroupBasis,
    pub reviews: Vec<RevisionRef>,
}

/// What journal order and recorded reads show for two reviews of one subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PairOrder {
    /// Both were recorded in one transaction by one actor.
    RecordedTogether,
    /// The later review names the earlier one in its read set.
    LaterReadEarlier,
    /// The later review declares it read another review of the subject.
    LaterDeclaredReadingReviews,
    /// The later review audited a snapshot from before the earlier review
    /// was recorded, and records and declares no read of it.
    LaterBasisPredatesEarlier,
    /// The later review declares it read no other review, but was recorded
    /// after the earlier one existed: journal order does not show it held.
    LaterDeclaredNoRead,
    /// The later review was recorded after the earlier one existed and does
    /// not declare whether it read it.
    LaterCouldHaveReadEarlier,
}

/// Two reviews of one subject in journal order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewPair {
    pub earlier: RevisionRef,
    pub later: RevisionRef,
    pub order: PairOrder,
    /// True only when journal order shows the later judgment was formed
    /// before the earlier one could be read. Never a claim of cognitive
    /// independence.
    pub recorded_independently: bool,
    pub same_dependence_group: bool,
}

/// Counts shown beside the review count wherever a subject is read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct IndependenceSummary {
    /// Reviews the audience may see; only these are grouped and paired.
    pub review_count: usize,
    /// Reviews withheld by classification: counted, never described.
    pub restricted_review_count: usize,
    pub dependence_group_count: usize,
    /// Groups formed by actor string because no model family was declared.
    pub groups_without_declared_family: usize,
    pub low_independence_count: usize,
    pub low_independence_reviews: Vec<RevisionRef>,
    /// Reviews missing at least one independence declaration.
    pub reviews_with_undeclared_attributes: usize,
    /// Pairs whose later review recorded or declared reading another review.
    pub pairs_with_recorded_read: usize,
    /// Pairs journal order shows were recorded independently.
    pub pairs_recorded_independently: usize,
}

/// Independence of the reviews of one subject.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct IndependenceView {
    pub rule_version: &'static str,
    pub summary: IndependenceSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviews: Option<Vec<ReviewEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependence_groups: Option<Vec<DependenceGroup>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pairs: Option<Vec<ReviewPair>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pairs_omitted: Option<usize>,
    pub notice: &'static str,
}

/// The part of a stored review body this module reads.
#[derive(Deserialize)]
struct StoredReview {
    kind: String,
    status: String,
    subject_ref: RevisionRef,
    #[serde(default)]
    read_set: Vec<RevisionRef>,
    verdict: Value,
    context_isolation: ContextIsolation,
    context: ReviewContext,
    #[serde(default)]
    audited_snapshot: Option<crate::wire::Snapshot>,
}

fn stored(record: &Record) -> Option<StoredReview> {
    if record.header.id.kind() != Kind::Review {
        return None;
    }
    serde_json::from_value(record.body.clone()).ok()
}

fn family_key(family: &str) -> String {
    family.trim().to_lowercase()
}

fn group_of(record: &Record) -> (String, GroupBasis) {
    dependence_group(
        &record.header.recorded_by,
        record.header.actor_descriptor_self_reported.as_ref(),
    )
}

/// The dependence group of an actor and its self-reported descriptor:
/// `model_family:<lowercased family>` for an agent that declares a family,
/// `human:<actor>` for a declared human, otherwise `actor:<actor>`. Also used
/// to group likelihood calibration (see `likelihood`).
pub fn dependence_group(
    actor: &Actor,
    descriptor: Option<&ActorDescriptor>,
) -> (String, GroupBasis) {
    match descriptor {
        Some(ActorDescriptor {
            kind: ActorKind::Agent,
            model_family: Some(family),
            ..
        }) if !family.trim().is_empty() => (
            format!("model_family:{}", family_key(family)),
            GroupBasis::ModelFamily,
        ),
        Some(ActorDescriptor {
            kind: ActorKind::Human,
            ..
        }) => (format!("human:{actor}"), GroupBasis::HumanActor),
        _ => (format!("actor:{actor}"), GroupBasis::ActorFamilyUndeclared),
    }
}

/// Declarations absent from `context`, by field name.
pub fn undeclared(context: &ReviewContext) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if context.work_kind.is_none() {
        missing.push("work_kind");
    }
    if context.prior_involvement.is_none() {
        missing.push("prior_involvement");
    }
    if context.read_other_reviews.is_none() {
        missing.push("read_other_reviews");
    }
    missing
}

/// Receipt warnings for a review being recorded. Never a refusal.
pub fn submission_warnings(isolation: ContextIsolation, context: &ReviewContext) -> Vec<String> {
    let mut warnings = Vec::new();
    if isolation == ContextIsolation::SameSessionSelfReview {
        warnings.push(
            "The review declares same_session_self_review; reads and briefs flag it as low-independence."
                .to_owned(),
        );
    }
    let missing = undeclared(context);
    if !missing.is_empty() {
        warnings.push(format!(
            "The review does not declare context {}; each reads as undeclared. Declare it, as unknown if need be.",
            missing.join(", ")
        ));
    }
    warnings
}

fn entry(state: &State, record: &Record, review: &StoredReview) -> ReviewEntry {
    let context = &review.context;
    let declarations = Declarations {
        context_isolation: review.context_isolation,
        saw_original_conclusion: Declared::from(Some(&context.saw_original_conclusion)),
        gathered_new_evidence: Declared::from(Some(&context.gathered_new_evidence)),
        work_kind: Declared::from(context.work_kind.as_ref()),
        prior_involvement: Declared::from(context.prior_involvement.as_ref()),
        read_other_reviews: Declared::from(context.read_other_reviews.as_ref()),
        dependencies: context.dependencies,
    };
    let mut reasons = Vec::new();
    if review.context_isolation == ContextIsolation::SameSessionSelfReview {
        reasons.push(LowIndependence::SameSessionSelfReview);
    }
    if declarations.work_kind == Declared::Known(ReviewWorkKind::SelfReview) {
        reasons.push(LowIndependence::DeclaredSelfReview);
    }
    if let Some(subject) = state.revision(review.subject_ref)
        && content_authors(state, subject)
            .is_ok_and(|authors| authors.contains(&record.header.recorded_by))
    {
        reasons.push(LowIndependence::ReviewerRecordedSubject);
    }
    let reviews_read = review
        .read_set
        .iter()
        .copied()
        .filter(|reference| {
            reference.id.kind() == Kind::Review
                && state
                    .revision(*reference)
                    .and_then(stored)
                    .is_some_and(|other| other.subject_ref.id == review.subject_ref.id)
        })
        .collect();
    ReviewEntry {
        review_ref: record.reference(),
        kind: review.kind.clone(),
        verdict: review.verdict.as_str().unwrap_or("unknown").to_owned(),
        subject_revision: review.subject_ref.revision,
        recorded_at_sequence: first_sequence(state, record),
        audited_sequence: review.audited_snapshot.as_ref().map(|s| s.sequence),
        recorded_by: record.header.recorded_by.clone(),
        actor_descriptor_self_reported: record.header.actor_descriptor_self_reported.clone(),
        undeclared: undeclared(context),
        low_independence: !reasons.is_empty(),
        low_independence_reasons: reasons,
        dependence_group: group_of(record).0,
        reviews_read,
        declarations,
    }
}

fn first_sequence(state: &State, record: &Record) -> u64 {
    state
        .history(record.header.id)
        .next()
        .map_or(record.header.acceptance_sequence, |first| {
            first.header.acceptance_sequence
        })
}

/// Submitted reviews of `subject`, in journal order.
fn reviews_of(state: &State, subject: Id) -> Vec<(&Record, StoredReview)> {
    let index = state.review_index();
    index
        .0
        .get(&subject)
        .into_iter()
        .flatten()
        .filter_map(|id| {
            let record = state.current(*id)?;
            Some((record, stored(record)?))
        })
        .collect()
}

/// The IDs of the submitted Reviews of `subject`, from [`reviews_of`] (the
/// index), for [`crate::derived_check`].
pub(crate) fn review_ids_of(state: &State, subject: Id) -> Vec<Id> {
    reviews_of(state, subject)
        .into_iter()
        .map(|(record, _)| record.header.id)
        .collect()
}

/// [`reviews_of`] as it was before [`ReviewIndex`]: a walk over every record
/// for each subject. The reference for [`crate::derived_check`].
pub(crate) fn reviews_of_by_scan(state: &State, subject: Id) -> Vec<Id> {
    let mut reviews: Vec<_> = state
        .records()
        .filter_map(|record| Some((record, stored(record)?)))
        .filter(|(_, review)| review.subject_ref.id == subject && review.status == "submitted")
        .collect();
    reviews.sort_by_key(|(record, _)| (first_sequence(state, record), record.header.id));
    reviews
        .into_iter()
        .map(|(record, _)| record.header.id)
        .collect()
}

/// The submitted Reviews of each subject, in journal order: by the sequence
/// of each Review's first revision, then by ID. Built once per snapshot by
/// [`State::review_index`] instead of once per subject.
pub(crate) struct ReviewIndex(BTreeMap<Id, Vec<Id>>);

impl ReviewIndex {
    pub(crate) fn index(state: &State) -> Self {
        let mut index: BTreeMap<Id, Vec<(u64, Id)>> = BTreeMap::new();
        for record in state.records() {
            let Some(review) = stored(record) else {
                continue;
            };
            if review.status == "submitted" {
                index
                    .entry(review.subject_ref.id)
                    .or_default()
                    .push((first_sequence(state, record), record.header.id));
            }
        }
        Self(
            index
                .into_iter()
                .map(|(subject, mut reviews)| {
                    reviews.sort_unstable();
                    (subject, reviews.into_iter().map(|(_, id)| id).collect())
                })
                .collect(),
        )
    }
}

fn order(earlier: &ReviewEntry, later: &ReviewEntry) -> PairOrder {
    if later
        .reviews_read
        .iter()
        .any(|reference| reference.id == earlier.review_ref.id)
    {
        return PairOrder::LaterReadEarlier;
    }
    if later.declarations.read_other_reviews == Declared::Known(true) {
        return PairOrder::LaterDeclaredReadingReviews;
    }
    if later.recorded_at_sequence == earlier.recorded_at_sequence {
        return PairOrder::RecordedTogether;
    }
    if later
        .audited_sequence
        .is_some_and(|sequence| sequence < earlier.recorded_at_sequence)
    {
        return PairOrder::LaterBasisPredatesEarlier;
    }
    if later.declarations.read_other_reviews == Declared::Known(false) {
        return PairOrder::LaterDeclaredNoRead;
    }
    PairOrder::LaterCouldHaveReadEarlier
}

fn view(
    state: &State,
    subject: Id,
    full: bool,
    visible: &dyn Fn(&Record) -> bool,
) -> Option<IndependenceView> {
    let (reviews, restricted): (Vec<_>, Vec<_>) = reviews_of(state, subject)
        .into_iter()
        .partition(|(record, _)| visible(record));
    if reviews.is_empty() && restricted.is_empty() {
        return None;
    }
    let entries: Vec<ReviewEntry> = reviews
        .iter()
        .map(|(record, review)| entry(state, record, review))
        .collect();
    let mut groups: BTreeMap<String, DependenceGroup> = BTreeMap::new();
    for (record, _) in &reviews {
        let (key, basis) = group_of(record);
        groups
            .entry(key.clone())
            .or_insert_with(|| DependenceGroup {
                group: key,
                basis,
                reviews: Vec::new(),
            })
            .reviews
            .push(record.reference());
    }
    let mut pairs = Vec::new();
    for (index, later) in entries.iter().enumerate() {
        for earlier in &entries[..index] {
            let order = order(earlier, later);
            pairs.push(ReviewPair {
                earlier: earlier.review_ref,
                later: later.review_ref,
                order,
                recorded_independently: order == PairOrder::LaterBasisPredatesEarlier,
                same_dependence_group: earlier.dependence_group == later.dependence_group,
            });
        }
    }
    let low: Vec<RevisionRef> = entries
        .iter()
        .filter(|entry| entry.low_independence)
        .map(|entry| entry.review_ref)
        .collect();
    let summary = IndependenceSummary {
        review_count: entries.len(),
        restricted_review_count: restricted.len(),
        dependence_group_count: groups.len(),
        groups_without_declared_family: groups
            .values()
            .filter(|group| group.basis == GroupBasis::ActorFamilyUndeclared)
            .count(),
        low_independence_count: low.len(),
        low_independence_reviews: low,
        reviews_with_undeclared_attributes: entries
            .iter()
            .filter(|entry| !entry.undeclared.is_empty())
            .count(),
        pairs_with_recorded_read: pairs
            .iter()
            .filter(|pair| {
                matches!(
                    pair.order,
                    PairOrder::LaterReadEarlier | PairOrder::LaterDeclaredReadingReviews
                )
            })
            .count(),
        pairs_recorded_independently: pairs
            .iter()
            .filter(|pair| pair.recorded_independently)
            .count(),
    };
    let omitted = pairs.len().saturating_sub(MAX_PAIRS);
    pairs.truncate(MAX_PAIRS);
    Some(IndependenceView {
        rule_version: RULE_VERSION,
        summary,
        reviews: full.then_some(entries),
        dependence_groups: full.then(|| groups.into_values().collect()),
        pairs: full.then_some(pairs),
        pairs_omitted: (full && omitted > 0).then_some(omitted),
        notice: NOTICE,
    })
}

/// Independence of the reviews of `subject`. `None` when nothing has
/// reviewed it. The summary always; reviews, groups and pairs when `full`.
/// `visible` says whether the audience may see a review; a hidden review is
/// only counted.
pub fn subject_view(
    state: &State,
    subject: &Record,
    full: bool,
    visible: &dyn Fn(&Record) -> bool,
) -> Option<IndependenceView> {
    if !SUBJECTS.contains(&subject.header.id.kind()) {
        return None;
    }
    view(state, subject.header.id, full, visible)
}

/// The independence entry of one Review record, read beside its subject's
/// other reviews. `None` for any other record.
pub fn review_entry(state: &State, record: &Record) -> Option<ReviewEntry> {
    let review = stored(record)?;
    Some(entry(state, record, &review))
}

/// Current reviews of `subject` with no low-independence flag, for the
/// `isolated_review` pack policy input. A blindspot review is current as the
/// blindspot review contract defines it; a validation review as promotion
/// does. Restricted reviews count: policy is not disclosure.
pub fn isolated_reviews(state: &State, subject: &Record) -> Result<Vec<RevisionRef>, Failure> {
    let blindspot = crate::blindspot_review::current_blindspot_reviews(state, subject)?;
    let mut isolated = Vec::new();
    for (record, review) in reviews_of(state, subject.header.id) {
        let current = match review.kind.as_str() {
            "blindspot" => blindspot.contains(&record.reference()),
            _ => serde_json::from_value::<ValidationReviewBody>(record.body.clone())
                .is_ok_and(|body| validation_review_current(state, subject, &body).is_ok()),
        };
        if current && !entry(state, record, &review).low_independence {
            isolated.push(record.reference());
        }
    }
    Ok(isolated)
}

/// What a view of `record` shows: the subject summary (or full view), or a
/// Review's own entry.
pub fn projection(
    state: &State,
    record: &Record,
    full: bool,
    visible: &dyn Fn(&Record) -> bool,
) -> Option<Value> {
    if record.header.id.kind() == Kind::Review {
        return review_entry(state, record).and_then(|entry| serde_json::to_value(entry).ok());
    }
    subject_view(state, record, full, visible).and_then(|view| serde_json::to_value(view).ok())
}

/// One line for text output, or empty.
pub fn text(view: &Value) -> String {
    let summary = &view["review_independence"]["summary"];
    let Some(count) = summary["review_count"].as_u64() else {
        return String::new();
    };
    let mut line = format!(
        "\nReviews: {count} in {} dependence group(s) by self-reported model family",
        summary["dependence_group_count"]
            .as_u64()
            .unwrap_or_default()
    );
    let low = summary["low_independence_count"]
        .as_u64()
        .unwrap_or_default();
    if low > 0 {
        line.push_str(&format!("; {low} low-independence"));
    }
    line
}

#[cfg(test)]
mod tests;
