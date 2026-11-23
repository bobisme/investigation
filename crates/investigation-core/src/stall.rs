//! Stalled-investigation detection (plan §7.4).
//!
//! Every pattern here counts recorded journal transactions and reads typed
//! fields; none reads a clock, record text or an agent's intent. A pattern
//! reports what was recorded, never why. Each one is reported only while no
//! consequence has been recorded at or after its last counted transaction, so
//! a stall ends as soon as recorded work changes the picture.
//!
//! The moves offered for a stall name exact records and existing operations.
//! They never propose a new hypothesis, plan, run or observation: the only
//! recordable moves are in [`STALL_MOVE_OPERATIONS`], and a justified stop is
//! always among them. Acquiring new evidence stays a consumer-managed
//! information requirement.
//!
//! What counts as collecting evidence and what counts as a consequence is
//! defined once, here, and shared with the `case.collection_without_consequence`
//! blindspot check.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::{
    command::RevisionRef,
    gap_rules::{Gap, STALL_RULES, exact_ref, field, is_live, is_retracted},
    identity::{Id, Kind},
    state::{Record, State},
};

/// Version of the stall patterns, thresholds and moves in this module.
pub const STALL_RULE_VERSION: &str = "stall.v1";

/// Collection transactions in a row, with no consequence between them, that
/// make `case.collection_without_consequence` report.
pub const STALL_COLLECTION_TRANSACTIONS: usize = 4;
/// Consecutive Runs of one plan or exploration intent that end with the same
/// failed prerequisite before `run.repeated_same_failure` reports.
pub const STALL_RUN_FAILURES: usize = 3;
/// Standalone Artifacts with identical bytes, each from its own transaction,
/// before `artifact.repeated_identical_bytes` reports.
pub const STALL_IDENTICAL_ACQUISITIONS: usize = 3;
/// Transactions accepted, with no consequence, while a blocker stays
/// unchanged before `case.blockers_unchanged` reports.
pub const STALL_BLOCKER_TRANSACTIONS: usize = 5;
/// Plans that must all be retired or inconclusive before
/// `case.planned_inquiries_exhausted` reports.
pub const STALL_EXHAUSTED_PLANS: usize = 2;

/// The only operations a stall move may name. None of them creates a
/// hypothesis, a plan, a run or an observation.
pub const STALL_MOVE_OPERATIONS: &[&str] = &[
    "assumption.assess",
    "case.pause",
    "decision.propose",
    "inquiry.retire",
    "measurement.assess",
    "question.create",
    "question.resolve",
];

const NOTICE: &str = "This reports a recorded pattern of transactions; it does not infer why the work went this way. Each move names existing records. None proposes a new hypothesis or test, and stopping with an inconclusive result is a legitimate outcome.";

/// At most this many records are named as targets of one move.
const TARGET_LIMIT: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StallMoveKind {
    VerifyInstrument,
    InspectRawData,
    NarrowQuestion,
    ChangeObservationChannel,
    RevisitAssumption,
    RequestMissingEvidence,
    StopInconclusive,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StallThreshold {
    pub name: String,
    pub value: u64,
}

/// One journal item a stall pattern counted, by acceptance sequence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StallItem {
    pub sequence: u64,
    pub fact: String,
    pub refs: Vec<RevisionRef>,
}

/// One specific move. `operation` is a local recording operation, or null
/// when the move is a read or a consumer-managed information requirement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StallMove {
    pub kind: StallMoveKind,
    pub operation: Option<String>,
    pub target_refs: Vec<RevisionRef>,
    pub purpose: String,
    /// `none` for a local read or record, `consumer_managed` otherwise.
    pub external_effects: String,
}

/// What a stall gap counted and the moves it offers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StallDetail {
    pub stall_rule_version: String,
    pub pattern: String,
    pub thresholds: Vec<StallThreshold>,
    pub journal_items: Vec<StallItem>,
    /// The most recent transaction that recorded a consequence, if any.
    pub last_consequence: Option<StallItem>,
    pub moves: Vec<StallMove>,
    pub notice: String,
}

/// The stall patterns, in the order of [`STALL_RULES`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StallPattern {
    IdenticalAcquisitions,
    BlockersUnchanged,
    CollectionWithoutConsequence,
    PlansExhausted,
    RepeatedRunFailure,
}

impl StallPattern {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 5] = [
        Self::IdenticalAcquisitions,
        Self::BlockersUnchanged,
        Self::CollectionWithoutConsequence,
        Self::PlansExhausted,
        Self::RepeatedRunFailure,
    ];

    pub(crate) fn rule_id(self) -> &'static str {
        STALL_RULES[self as usize]
    }

    fn name(self) -> &'static str {
        match self {
            Self::IdenticalAcquisitions => "repeated_identical_acquisition",
            Self::BlockersUnchanged => "unchanged_blocker",
            Self::CollectionWithoutConsequence => "collection_without_consequence",
            Self::PlansExhausted => "exhausted_candidate_tests",
            Self::RepeatedRunFailure => "repeated_same_failure",
        }
    }
}

/// One detected stall, ready to become a gap of `pattern.rule_id()`.
pub(crate) struct StallFinding {
    pub(crate) pattern: StallPattern,
    pub(crate) subject_refs: Vec<RevisionRef>,
    pub(crate) read_set: Vec<RevisionRef>,
    pub(crate) reason: String,
    pub(crate) creation_sequence: u64,
    pub(crate) detail: StallDetail,
}

fn text<'a>(record: &'a Record, key: &str) -> Option<&'a str> {
    record.body.get(key).and_then(Value::as_str)
}

/// Whether one revision records a consequence of collected evidence: a
/// discriminating assessment, a known comparison outcome, a conclusion or
/// decision, a hypothesis rejected or retired, an option ruled out, withdrawn
/// or superseded, or an assumption assessed. Activating a hypothesis or making
/// an option eligible is not a consequence of evidence.
pub(crate) fn records_consequence(revision: &Record) -> bool {
    match revision.header.id.kind() {
        Kind::Assessment => matches!(text(revision, "relation"), Some("supports" | "contradicts")),
        Kind::Comparison => text(revision, "outcome").is_some_and(|outcome| outcome != "unknown"),
        Kind::Conclusion | Kind::Decision => true,
        Kind::Hypothesis => {
            revision.header.revision.0 > 1
                && matches!(
                    text(revision, "lifecycle"),
                    Some("rejected_within_scope" | "retired")
                )
        }
        Kind::Option => {
            revision.header.revision.0 > 1
                && matches!(
                    text(revision, "lifecycle"),
                    Some("ruled_out" | "withdrawn" | "superseded")
                )
        }
        Kind::Assumption => {
            revision.header.revision.0 > 1
                && matches!(
                    text(revision, "status"),
                    Some("supported" | "challenged" | "rejected")
                )
        }
        _ => false,
    }
}

/// Whether one revision creates evidence: the first revision of an
/// Observation, Run, Source or Artifact.
pub(crate) fn collects_evidence(revision: &Record) -> bool {
    revision.header.revision.0 == 1
        && matches!(
            revision.header.id.kind(),
            Kind::Observation | Kind::Run | Kind::Source | Kind::Artifact
        )
}

/// Every revision of one case's records, by acceptance sequence.
pub(crate) struct Journal<'a> {
    transactions: BTreeMap<u64, Vec<&'a Record>>,
}

impl<'a> Journal<'a> {
    pub(crate) fn build(state: &'a State, case_id: Id) -> Self {
        let mut transactions = BTreeMap::<u64, Vec<&Record>>::new();
        for record in state
            .records()
            .filter(|record| record.header.case_id == case_id)
        {
            for revision in state.history(record.header.id) {
                transactions
                    .entry(revision.header.acceptance_sequence)
                    .or_default()
                    .push(revision);
            }
        }
        Self { transactions }
    }

    /// The most recent transaction that records a consequence, with the
    /// consequential revisions it holds.
    pub(crate) fn last_consequence(&self) -> Option<(u64, Vec<RevisionRef>)> {
        self.transactions
            .iter()
            .rev()
            .find_map(|(sequence, revisions)| {
                let refs: Vec<RevisionRef> = revisions
                    .iter()
                    .filter(|revision| records_consequence(revision))
                    .map(|revision| revision.reference())
                    .collect();
                (!refs.is_empty()).then_some((*sequence, refs))
            })
    }

    fn transactions_after(&self, sequence: u64) -> Vec<u64> {
        self.transactions
            .range(sequence.saturating_add(1)..)
            .map(|(sequence, _)| *sequence)
            .collect()
    }
}

/// The most recent evidence-creating transactions with no consequence among
/// or after them, oldest first, and the consequence that ended the previous
/// streak. Transactions that neither collect nor record a consequence are
/// not counted and do not break the streak.
pub(crate) struct CollectionStreak {
    pub(crate) counted: Vec<(u64, Vec<RevisionRef>)>,
    pub(crate) last_consequence: Option<(u64, Vec<RevisionRef>)>,
}

/// The collection streak when it reaches [`STALL_COLLECTION_TRANSACTIONS`].
pub(crate) fn collection_without_consequence(journal: &Journal<'_>) -> Option<CollectionStreak> {
    let mut counted = Vec::new();
    let mut last_consequence = None;
    for (sequence, revisions) in journal.transactions.iter().rev() {
        let consequences: Vec<RevisionRef> = revisions
            .iter()
            .filter(|revision| records_consequence(revision))
            .map(|revision| revision.reference())
            .collect();
        if !consequences.is_empty() {
            last_consequence = Some((*sequence, consequences));
            break;
        }
        let collected: Vec<RevisionRef> = revisions
            .iter()
            .filter(|revision| collects_evidence(revision))
            .map(|revision| revision.reference())
            .collect();
        if !collected.is_empty() {
            counted.push((*sequence, collected));
        }
    }
    if counted.len() < STALL_COLLECTION_TRANSACTIONS {
        return None;
    }
    counted.reverse();
    Some(CollectionStreak {
        counted,
        last_consequence,
    })
}

fn threshold(name: &str, value: usize) -> StallThreshold {
    StallThreshold {
        name: name.into(),
        value: value as u64,
    }
}

fn item(sequence: u64, fact: impl Into<String>, mut refs: Vec<RevisionRef>) -> StallItem {
    refs.sort_by_key(|reference| (reference.id, reference.revision));
    refs.dedup();
    StallItem {
        sequence,
        fact: fact.into(),
        refs,
    }
}

fn ids(refs: &[RevisionRef]) -> String {
    refs.iter()
        .map(|reference| reference.id.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn sequences(items: &[StallItem]) -> String {
    let unique: BTreeSet<u64> = items.iter().map(|item| item.sequence).collect();
    unique
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn limited(refs: impl IntoIterator<Item = RevisionRef>) -> Vec<RevisionRef> {
    let mut refs: Vec<RevisionRef> = refs.into_iter().collect();
    refs.sort_by_key(|reference| (reference.id, reference.revision));
    refs.dedup();
    refs.truncate(TARGET_LIMIT);
    refs
}

/// Records of the case the moves name, read once.
struct Case<'a> {
    state: &'a State,
    case_ref: RevisionRef,
    records: Vec<&'a Record>,
    open_questions: Vec<RevisionRef>,
    open_assumptions: Vec<RevisionRef>,
    has_options: bool,
    last_consequence: Option<(u64, Vec<RevisionRef>)>,
}

impl Case<'_> {
    fn of_kind(&self, kind: Kind) -> impl Iterator<Item = &&Record> {
        self.records
            .iter()
            .filter(move |record| record.header.id.kind() == kind)
    }

    /// True when a consequence was recorded at or after `sequence`.
    fn moved_on_since(&self, sequence: u64) -> bool {
        self.last_consequence
            .as_ref()
            .is_some_and(|(last, _)| *last >= sequence)
    }

    fn last_consequence_item(&self) -> Option<StallItem> {
        self.last_consequence
            .as_ref()
            .map(|(sequence, refs)| item(*sequence, "consequence", refs.clone()))
    }

    fn stop_moves(&self, plan: Option<RevisionRef>) -> Vec<StallMove> {
        let mut moves = Vec::new();
        if let Some(plan) = plan {
            moves.push(local(
                StallMoveKind::StopInconclusive,
                "inquiry.retire",
                vec![plan],
                "Retire the plan, citing the recorded failures as the rationale, instead of running it again unchanged.",
            ));
        }
        if !self.open_questions.is_empty() {
            moves.push(local(
                StallMoveKind::StopInconclusive,
                "question.resolve",
                self.open_questions.clone(),
                "Resolve the question as inconclusive or deferred, stating what remains unknown and why more of the same work is not expected to change it.",
            ));
        }
        if self.has_options {
            moves.push(local(
                StallMoveKind::StopInconclusive,
                "decision.propose",
                self.open_questions.clone(),
                "Propose a conditional choice that names the residual unknowns and a reopening trigger.",
            ));
        }
        moves.push(local(
            StallMoveKind::StopInconclusive,
            "case.pause",
            vec![self.case_ref],
            "Pause the case with the residual unknowns and the condition that would justify reopening it.",
        ));
        moves
    }

    fn narrow_question(&self) -> StallMove {
        local(
            StallMoveKind::NarrowQuestion,
            "question.create",
            self.open_questions.clone(),
            "Record a narrower question, with one of these as its parent, that the evidence already recorded could answer.",
        )
    }

    fn revisit_assumptions(&self) -> Option<StallMove> {
        (!self.open_assumptions.is_empty()).then(|| {
            local(
                StallMoveKind::RevisitAssumption,
                "assumption.assess",
                self.open_assumptions.clone(),
                "Assess these unassessed or challenged assumptions against the evidence already recorded; the work may rest on one that does not hold.",
            )
        })
    }
}

fn local(
    kind: StallMoveKind,
    operation: &str,
    targets: Vec<RevisionRef>,
    purpose: &str,
) -> StallMove {
    StallMove {
        kind,
        operation: Some(operation.into()),
        target_refs: limited(targets),
        purpose: purpose.into(),
        external_effects: "none".into(),
    }
}

fn inspect(targets: Vec<RevisionRef>, purpose: &str) -> StallMove {
    StallMove {
        kind: StallMoveKind::InspectRawData,
        operation: None,
        target_refs: limited(targets),
        purpose: purpose.into(),
        external_effects: "none".into(),
    }
}

fn external(kind: StallMoveKind, targets: Vec<RevisionRef>, purpose: &str) -> StallMove {
    StallMove {
        kind,
        operation: None,
        target_refs: limited(targets),
        purpose: purpose.into(),
        external_effects: "consumer_managed".into(),
    }
}

const CHANGE_CHANNEL: &str = "Obtain evidence through a different observation channel or from a source of a different family, outside Investigation, and record it only once it exists.";

/// Every stall pattern of one case. `gaps` are the gaps already emitted from
/// the same state; the unchanged-blocker pattern reads its blockers there.
pub(crate) fn detect(state: &State, case_id: Id, gaps: &[Gap]) -> Vec<StallFinding> {
    let Some(case) = state.current(case_id) else {
        return Vec::new();
    };
    // A paused or closed case has already stopped, and a recording session
    // does not pursue a question, so neither can stall.
    if text(case, "lifecycle").is_some_and(|lifecycle| lifecycle != "active")
        || text(case, "mode") == Some("record")
    {
        return Vec::new();
    }
    let records: Vec<&Record> = state
        .records()
        .filter(|record| record.header.case_id == case_id)
        .collect();
    let questions: Vec<&Record> = records
        .iter()
        .copied()
        .filter(|record| record.header.id.kind() == Kind::Question)
        .collect();
    let open_questions: Vec<RevisionRef> = questions
        .iter()
        .filter(|question| {
            field(&question.body, "disposition")
                .and_then(Value::as_str)
                .is_none_or(|disposition| disposition == "open")
        })
        .map(|question| question.reference())
        .collect();
    // Every question is resolved: the case has already reached a disposition.
    if !questions.is_empty() && open_questions.is_empty() {
        return Vec::new();
    }
    let open_assumptions: Vec<RevisionRef> = records
        .iter()
        .filter(|record| {
            record.header.id.kind() == Kind::Assumption
                && matches!(text(record, "status"), Some("unassessed" | "challenged"))
        })
        .map(|record| record.reference())
        .collect();
    let has_options = records.iter().any(|record| {
        record.header.id.kind() == Kind::Option
            && !matches!(
                field(&record.body, "lifecycle").and_then(Value::as_str),
                Some("ruled_out" | "withdrawn" | "superseded")
            )
    });
    let journal = Journal::build(state, case_id);
    let context = Case {
        state,
        case_ref: case.reference(),
        open_questions: limited(open_questions),
        open_assumptions: limited(open_assumptions),
        has_options,
        last_consequence: journal.last_consequence(),
        records,
    };
    let mut findings = Vec::new();
    findings.extend(identical_acquisitions(&context));
    findings.extend(blockers_unchanged(&context, &journal, gaps));
    findings.extend(collection(&context, &journal));
    findings.extend(plans_exhausted(&context));
    findings.extend(repeated_run_failures(&context));
    findings
}

fn finding(
    context: &Case<'_>,
    pattern: StallPattern,
    subject_refs: Vec<RevisionRef>,
    reason: String,
    thresholds: Vec<StallThreshold>,
    journal_items: Vec<StallItem>,
    moves: Vec<StallMove>,
) -> StallFinding {
    let mut read_set: Vec<RevisionRef> = journal_items
        .iter()
        .flat_map(|item| item.refs.iter().copied())
        .collect();
    read_set.push(context.case_ref);
    let creation_sequence = journal_items
        .iter()
        .map(|item| item.sequence)
        .min()
        .unwrap_or_default();
    StallFinding {
        pattern,
        subject_refs,
        read_set,
        reason,
        creation_sequence,
        detail: StallDetail {
            stall_rule_version: STALL_RULE_VERSION.into(),
            pattern: pattern.name().into(),
            thresholds,
            journal_items,
            last_consequence: context.last_consequence_item(),
            moves,
            notice: NOTICE.into(),
        },
    }
}

fn collection(context: &Case<'_>, journal: &Journal<'_>) -> Option<StallFinding> {
    let streak = collection_without_consequence(journal)?;
    let items: Vec<StallItem> = streak
        .counted
        .iter()
        .map(|(sequence, refs)| item(*sequence, "collection_transaction", refs.clone()))
        .collect();
    let reason = format!(
        "The last {} transactions that created an Observation, Run, Source or Artifact (sequences {}) are followed by no recorded consequence: no discriminating Assessment, known Comparison outcome, Conclusion, Decision, rejected or retired Hypothesis, ruled-out Option or assessed Assumption.",
        items.len(),
        sequences(&items)
    );
    let collected: Vec<RevisionRef> = items
        .iter()
        .rev()
        .flat_map(|item| item.refs.iter().copied())
        .collect();
    let mut moves = vec![
        inspect(
            collected.clone(),
            "Read what was already collected before collecting more of the same kind.",
        ),
        context.narrow_question(),
    ];
    moves.extend(context.revisit_assumptions());
    moves.push(external(
        StallMoveKind::ChangeObservationChannel,
        collected,
        CHANGE_CHANNEL,
    ));
    moves.extend(context.stop_moves(None));
    Some(finding(
        context,
        StallPattern::CollectionWithoutConsequence,
        vec![context.case_ref],
        reason,
        vec![threshold(
            "collection_transactions",
            STALL_COLLECTION_TRANSACTIONS,
        )],
        items,
        moves,
    ))
}

fn identical_acquisitions(context: &Case<'_>) -> Vec<StallFinding> {
    // Artifacts a Run lists are repeat outputs of a procedure, not imports.
    let run_outputs: BTreeSet<Id> = context
        .of_kind(Kind::Run)
        .flat_map(|run| {
            field(&run.body, "artifact_refs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|reference| exact_ref(Some(reference)))
                .map(|reference| reference.id)
        })
        .collect();
    let mut by_digest = BTreeMap::<&str, Vec<&Record>>::new();
    for artifact in context.of_kind(Kind::Artifact) {
        if is_retracted(&artifact.body)
            || text(artifact, "status") == Some("withdrawn")
            || run_outputs.contains(&artifact.header.id)
        {
            continue;
        }
        if let Some(digest) = text(artifact, "digest") {
            by_digest.entry(digest).or_default().push(artifact);
        }
    }
    let mut findings = Vec::new();
    for (digest, mut artifacts) in by_digest {
        let created = |artifact: &Record| {
            context
                .state
                .history(artifact.header.id)
                .next()
                .map_or(0, |first| first.header.acceptance_sequence)
        };
        artifacts.sort_by_key(|artifact| (created(artifact), artifact.header.id));
        let transactions: BTreeSet<u64> =
            artifacts.iter().map(|artifact| created(artifact)).collect();
        let last = transactions.last().copied().unwrap_or_default();
        if transactions.len() < STALL_IDENTICAL_ACQUISITIONS || context.moved_on_since(last) {
            continue;
        }
        let items: Vec<StallItem> = artifacts
            .iter()
            .map(|artifact| {
                let mut refs = vec![artifact.reference()];
                refs.extend(exact_ref(field(&artifact.body, "source")));
                item(created(artifact), "acquisition_of_identical_bytes", refs)
            })
            .collect();
        let first = artifacts[0].reference();
        let reason = format!(
            "{} standalone Artifacts ({}) hold identical bytes {digest}, acquired in {} separate transactions (sequences {}), and no consequence was recorded after them.",
            artifacts.len(),
            ids(&artifacts
                .iter()
                .map(|artifact| artifact.reference())
                .collect::<Vec<_>>()),
            transactions.len(),
            sequences(&items)
        );
        let mut moves = vec![
            inspect(
                vec![first],
                "These bytes are already retained; read them instead of acquiring them again.",
            ),
            external(
                StallMoveKind::ChangeObservationChannel,
                vec![first],
                "Acquire from a different channel or a source of a different family, or give the producer a stable acquisition key so a repeat is recognised as one acquisition.",
            ),
        ];
        moves.extend(context.stop_moves(None));
        findings.push(finding(
            context,
            StallPattern::IdenticalAcquisitions,
            vec![first],
            reason,
            vec![threshold(
                "identical_acquisitions",
                STALL_IDENTICAL_ACQUISITIONS,
            )],
            items,
            moves,
        ));
    }
    findings
}

/// Blocking quality gaps whose emitting rule names an explicit invalidity.
const BLOCKING_GAP_RULE: &str = "observation.invalid_or_missing_source_match";

fn blockers_unchanged(
    context: &Case<'_>,
    journal: &Journal<'_>,
    gaps: &[Gap],
) -> Option<StallFinding> {
    let state = context.state;
    let changed_at = |refs: &[RevisionRef]| {
        refs.iter()
            .filter_map(|reference| state.revision(*reference))
            .map(|record| record.header.acceptance_sequence)
            .max()
            .unwrap_or_default()
    };
    let mut blockers: Vec<(RevisionRef, &str, u64)> = gaps
        .iter()
        .filter(|gap| gap.rule_id == BLOCKING_GAP_RULE)
        .filter_map(|gap| {
            let subject = *gap.subject_refs.first()?;
            Some((subject, "observation_invalid", changed_at(&gap.read_set)))
        })
        .collect();
    blockers.extend(
        context
            .of_kind(Kind::Run)
            .filter(|run| {
                field(&run.body, "execution_status").and_then(Value::as_str) == Some("blocked")
            })
            .map(|run| {
                (
                    run.reference(),
                    "run_blocked",
                    run.header.acceptance_sequence,
                )
            }),
    );
    let since_consequence = context
        .last_consequence
        .as_ref()
        .map_or(0, |(sequence, _)| *sequence);
    let mut items = Vec::new();
    let mut observations = Vec::new();
    let mut artifacts = Vec::new();
    let mut runs = Vec::new();
    for (blocker, kind, changed) in blockers {
        let later = journal.transactions_after(changed.max(since_consequence));
        if later.len() < STALL_BLOCKER_TRANSACTIONS {
            continue;
        }
        items.push(item(
            changed,
            format!(
                "blocker_unchanged:{kind}:transactions_since:{}",
                later.len()
            ),
            vec![blocker],
        ));
        if kind == "run_blocked" {
            runs.push(blocker);
        } else {
            observations.push(blocker);
            if let Some(observation) = state.current(blocker.id) {
                artifacts.extend(
                    field(&observation.body, "basis")
                        .and_then(|basis| exact_ref(basis.get("artifact_ref"))),
                );
            }
        }
    }
    if items.is_empty() {
        return None;
    }
    items.sort_by_key(|item| (item.sequence, item.refs.first().map(|r| r.id)));
    let blocker_refs: Vec<RevisionRef> = items.iter().flat_map(|item| item.refs.clone()).collect();
    let reason = format!(
        "Blockers {} have stayed unchanged while at least {STALL_BLOCKER_TRANSACTIONS} later transactions were accepted with no recorded consequence.",
        ids(&blocker_refs)
    );
    let mut moves = Vec::new();
    if !observations.is_empty() {
        moves.push(local(
            StallMoveKind::VerifyInstrument,
            "measurement.assess",
            observations.clone(),
            "Check the instrument or setup behind these invalid Observations and record the result, citing an existing control or instrument check.",
        ));
        let mut targets = artifacts;
        targets.extend(observations);
        moves.push(inspect(
            targets,
            "Read the exact source bytes behind these Observations to see whether the selector, quote or value is wrong.",
        ));
    }
    if !runs.is_empty() {
        moves.push(external(
            StallMoveKind::RequestMissingEvidence,
            runs,
            "State what these blocked Runs are waiting for and request it from whoever holds it; nothing is recorded until it is supplied.",
        ));
    }
    moves.extend(context.revisit_assumptions());
    moves.extend(context.stop_moves(None));
    Some(finding(
        context,
        StallPattern::BlockersUnchanged,
        vec![context.case_ref],
        reason,
        vec![threshold(
            "unchanged_transactions",
            STALL_BLOCKER_TRANSACTIONS,
        )],
        items,
        moves,
    ))
}

/// Assessments, by the Run or Observation IDs they cite as evidence.
fn assessments_by_evidence<'a>(context: &Case<'a>) -> BTreeMap<Id, Vec<&'a Record>> {
    let mut map = BTreeMap::<Id, Vec<&Record>>::new();
    for assessment in context.records.iter().copied().filter(|record| {
        record.header.id.kind() == Kind::Assessment
            && is_live(record)
            && !matches!(text(record, "status"), Some("superseded" | "withdrawn"))
    }) {
        for reference in field(&assessment.body, "evidence_refs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|value| exact_ref(Some(value)))
        {
            map.entry(reference.id).or_default().push(assessment);
        }
    }
    map
}

/// Why one Run ended without a usable result, as `field:value`, and the
/// records that show it. `None` when the Run succeeded or is still open.
fn run_failure(
    run: &Record,
    observations: &BTreeMap<Id, Vec<Id>>,
    assessments: &BTreeMap<Id, Vec<&Record>>,
) -> Option<(String, Vec<RevisionRef>)> {
    let read = |key| field(&run.body, key).and_then(Value::as_str);
    let status = read("execution_status");
    if let Some(status @ ("failed" | "cancelled" | "blocked")) = status {
        return Some((format!("execution_status:{status}"), vec![run.reference()]));
    }
    if read("evidence_completeness") == Some("missing") {
        return Some((
            "evidence_completeness:missing".into(),
            vec![run.reference()],
        ));
    }
    if read("evidence_validity") == Some("invalid") {
        return Some(("evidence_validity:invalid".into(), vec![run.reference()]));
    }
    if status != Some("completed") {
        return None;
    }
    let cited: Vec<&Record> = std::iter::once(run.header.id)
        .chain(
            observations
                .get(&run.header.id)
                .into_iter()
                .flatten()
                .copied(),
        )
        .flat_map(|id| assessments.get(&id).into_iter().flatten().copied())
        .collect();
    let inconclusive = !cited.is_empty()
        && cited.iter().all(|assessment| {
            matches!(
                text(assessment, "relation"),
                Some("inconclusive" | "non_diagnostic")
            )
        });
    inconclusive.then(|| {
        let mut refs = vec![run.reference()];
        refs.extend(cited.iter().map(|assessment| assessment.reference()));
        ("assessment_relation:inconclusive".into(), refs)
    })
}

fn runs_by_observation_source(context: &Case<'_>) -> BTreeMap<Id, Vec<Id>> {
    let mut map = BTreeMap::<Id, Vec<Id>>::new();
    for observation in context.of_kind(Kind::Observation) {
        if let Some(run) = exact_ref(field(&observation.body, "run_ref")) {
            map.entry(run.id).or_default().push(observation.header.id);
        }
    }
    map
}

fn created_at(state: &State, record: &Record) -> u64 {
    state
        .history(record.header.id)
        .next()
        .map_or(0, |first| first.header.acceptance_sequence)
}

fn plan_of(run: &Record) -> Option<RevisionRef> {
    let intent = field(&run.body, "intent")?;
    if intent.get("kind").and_then(Value::as_str) != Some("plan") {
        return None;
    }
    exact_ref(intent.get("plan_ref"))
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Attempt {
    Plan(Id),
    Exploration(String),
}

fn attempt_of(run: &Record) -> Option<Attempt> {
    if let Some(plan) = plan_of(run) {
        return Some(Attempt::Plan(plan.id));
    }
    let intent = field(&run.body, "intent")?;
    let label = intent.get("intent").and_then(Value::as_str)?.trim();
    (!label.is_empty()).then(|| Attempt::Exploration(label.to_owned()))
}

fn repeated_run_failures(context: &Case<'_>) -> Vec<StallFinding> {
    let state = context.state;
    let observations = runs_by_observation_source(context);
    let assessments = assessments_by_evidence(context);
    let mut by_attempt = BTreeMap::<Attempt, Vec<&Record>>::new();
    for run in context.of_kind(Kind::Run) {
        if let Some(attempt) = attempt_of(run) {
            by_attempt.entry(attempt).or_default().push(run);
        }
    }
    let mut findings = Vec::new();
    for (attempt, mut runs) in by_attempt {
        runs.sort_by_key(|run| (created_at(state, run), run.header.id));
        let plan = match &attempt {
            Attempt::Plan(id) => state.current(*id),
            Attempt::Exploration(_) => None,
        };
        if plan.is_some_and(|plan| text(plan, "lifecycle") == Some("retired")) {
            continue;
        }
        let mut streak = Vec::new();
        let mut label = None;
        for run in runs.iter().rev() {
            let Some((failure, refs)) = run_failure(run, &observations, &assessments) else {
                break;
            };
            if label.as_ref().is_some_and(|label| *label != failure) {
                break;
            }
            label = Some(failure);
            let sequence = refs
                .iter()
                .filter_map(|reference| state.revision(*reference))
                .map(|record| record.header.acceptance_sequence)
                .max()
                .unwrap_or_default();
            streak.push((sequence, refs, run.reference()));
        }
        let Some(label) = label else {
            continue;
        };
        let last = streak
            .iter()
            .map(|(sequence, _, _)| *sequence)
            .max()
            .unwrap_or_default();
        if streak.len() < STALL_RUN_FAILURES || context.moved_on_since(last) {
            continue;
        }
        streak.reverse();
        let items: Vec<StallItem> = streak
            .iter()
            .map(|(sequence, refs, _)| item(*sequence, format!("run_failed:{label}"), refs.clone()))
            .collect();
        let run_refs: Vec<RevisionRef> = streak.iter().map(|(_, _, run)| *run).collect();
        let plan_ref = plan.map(Record::reference);
        let subject = plan_ref.unwrap_or_else(|| runs[0].reference());
        let target = match &attempt {
            Attempt::Plan(id) => format!("plan {id}"),
            Attempt::Exploration(_) => format!(
                "the exploration intent first recorded on Run {}",
                runs[0].header.id
            ),
        };
        let reason = format!(
            "The last {} Runs of {target} ({}) each ended with {label} (sequences {}), and no consequence was recorded after them.",
            streak.len(),
            ids(&run_refs),
            sequences(&items)
        );
        let run_observations: Vec<RevisionRef> = run_refs
            .iter()
            .flat_map(|run| observations.get(&run.id).into_iter().flatten())
            .filter_map(|id| state.current(*id))
            .map(Record::reference)
            .collect();
        let run_artifacts: Vec<RevisionRef> = streak
            .iter()
            .filter_map(|(_, _, run)| state.current(run.id))
            .flat_map(|run| {
                field(&run.body, "artifact_refs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|reference| exact_ref(Some(reference)))
            })
            .collect();
        let mut moves = Vec::new();
        if run_observations.is_empty() {
            moves.push(external(
                StallMoveKind::VerifyInstrument,
                run_refs.clone(),
                "Check the instrument, environment or setup these Runs used, outside Investigation, before another attempt; record the check as evidence once it exists.",
            ));
        } else {
            moves.push(local(
                StallMoveKind::VerifyInstrument,
                "measurement.assess",
                run_observations,
                "Assess the measurement validity of what these Runs observed, citing an existing control or instrument check.",
            ));
        }
        let mut raw = run_artifacts;
        if raw.is_empty() {
            raw = run_refs.clone();
        }
        moves.push(inspect(
            raw,
            "Read the retained output of the failed Runs to see where each one stopped.",
        ));
        moves.extend(context.revisit_assumptions());
        moves.push(context.narrow_question());
        moves.push(external(
            StallMoveKind::ChangeObservationChannel,
            run_refs,
            CHANGE_CHANNEL,
        ));
        moves.extend(context.stop_moves(plan_ref));
        findings.push(finding(
            context,
            StallPattern::RepeatedRunFailure,
            vec![subject],
            reason,
            vec![threshold("consecutive_failed_runs", STALL_RUN_FAILURES)],
            items,
            moves,
        ));
    }
    findings
}

fn plans_exhausted(context: &Case<'_>) -> Option<StallFinding> {
    let state = context.state;
    let observations = runs_by_observation_source(context);
    let assessments = assessments_by_evidence(context);
    let replaced: BTreeSet<Id> = context
        .of_kind(Kind::Inquiry)
        .filter_map(|plan| exact_ref(field(&plan.body, "predecessor_ref")))
        .map(|reference| reference.id)
        .collect();
    let plans: Vec<&Record> = context
        .of_kind(Kind::Inquiry)
        .copied()
        .filter(|plan| !replaced.contains(&plan.header.id))
        .collect();
    if plans.len() < STALL_EXHAUSTED_PLANS {
        return None;
    }
    let mut runs_by_plan = BTreeMap::<Id, Vec<&Record>>::new();
    for run in context.of_kind(Kind::Run) {
        if let Some(plan) = plan_of(run) {
            runs_by_plan.entry(plan.id).or_default().push(run);
        }
    }
    let mut items = Vec::new();
    for plan in &plans {
        if text(plan, "lifecycle") == Some("retired") {
            items.push(item(
                plan.header.acceptance_sequence,
                "plan_retired",
                vec![plan.reference()],
            ));
            continue;
        }
        let runs = runs_by_plan.get(&plan.header.id)?;
        let mut refs = vec![plan.reference()];
        let mut labels = BTreeSet::new();
        for run in runs {
            let (label, shown) = run_failure(run, &observations, &assessments)?;
            labels.insert(label);
            refs.extend(shown);
        }
        let sequence = refs
            .iter()
            .filter_map(|reference| state.revision(*reference))
            .map(|record| record.header.acceptance_sequence)
            .max()
            .unwrap_or_default();
        items.push(item(
            sequence,
            format!(
                "plan_inconclusive:{}",
                labels.into_iter().collect::<Vec<_>>().join("+")
            ),
            refs,
        ));
    }
    let last = items
        .iter()
        .map(|item| item.sequence)
        .max()
        .unwrap_or_default();
    if context.moved_on_since(last) {
        return None;
    }
    items.sort_by_key(|item| (item.sequence, item.refs.first().map(|r| r.id)));
    let plan_refs: Vec<RevisionRef> = plans.iter().map(|plan| plan.reference()).collect();
    let reason = format!(
        "Every one of the {} current plans ({}) is retired or has only Runs that failed or were assessed inconclusive (sequences {}), and no consequence was recorded after them.",
        plans.len(),
        ids(&plan_refs),
        sequences(&items)
    );
    let mut moves = Vec::new();
    moves.extend(context.revisit_assumptions());
    moves.push(context.narrow_question());
    moves.push(external(
        StallMoveKind::RequestMissingEvidence,
        context.open_questions.clone(),
        "State the specific evidence the open questions still lack and request it from whoever holds it; nothing is recorded until it is supplied.",
    ));
    moves.push(external(
        StallMoveKind::ChangeObservationChannel,
        plan_refs,
        CHANGE_CHANNEL,
    ));
    moves.extend(context.stop_moves(None));
    Some(finding(
        context,
        StallPattern::PlansExhausted,
        vec![context.case_ref],
        reason,
        vec![threshold("exhausted_plans", STALL_EXHAUSTED_PLANS)],
        items,
        moves,
    ))
}

#[cfg(test)]
mod tests;
