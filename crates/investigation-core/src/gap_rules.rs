//! Versioned, deterministic structural gaps. Ranking lives in `next_rank`.
//!
//! These rules inspect recorded fields only. They do not infer a domain test,
//! calculate a score, execute a process, or treat source text as instructions.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    command::RevisionRef,
    identity::{Id, Kind},
    state::{Record, State},
    wire::{ErrorCode, Failure, Snapshot},
};

pub const GAP_RULE_VERSION: &str = "slice-gaps.v1";

/// Gap rules that report a stalled investigation (plan §7.4), in rule ID
/// order. `stall::StallPattern` indexes this table; the patterns, thresholds
/// and moves live in [`crate::stall`].
pub const STALL_RULES: &[&str] = &[
    "artifact.repeated_identical_bytes",
    "case.blockers_unchanged",
    "case.collection_without_consequence",
    "case.planned_inquiries_exhausted",
    "run.repeated_same_failure",
];

// Ranking lives in `next_rank`; these re-exports keep older import paths.
pub use crate::next_rank::{
    DEFAULT_NEXT_LIMIT, GapCounts, NEXT_RULE_VERSION, NextResponse, OrdinalPriority, RankFactor,
    RankReason, Recommendation, rank_next,
};

#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GapStatus {
    ReadyToAnalyze,
    RequiresSource,
    RequiresExternalAction,
    BlockedByQuality,
    StaleDueToRevision,
    Deferred,
    Resolved,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrerequisiteState {
    Met,
    Unmet,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Prerequisite {
    pub name: String,
    pub state: PrerequisiteState,
    pub reason: String,
    pub subject_refs: Vec<RevisionRef>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutcomeBranch {
    pub outcome: String,
    pub consequence: String,
    pub basis: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Gap {
    /// Stable for the same rule and subject ID set; revisions appear in the read set.
    pub gap_id: String,
    pub rule_id: String,
    pub rule_version: String,
    pub subject_refs: Vec<RevisionRef>,
    pub read_set: Vec<RevisionRef>,
    pub status: GapStatus,
    pub reason: String,
    pub prerequisites: Vec<Prerequisite>,
    pub suggested_operation: Option<String>,
    pub unresolved_fields: Vec<String>,
    pub outcome_branches: Vec<OutcomeBranch>,
    pub decision_effect: String,
    pub time_sensitive: bool,
    pub creation_sequence: u64,
    /// The journal items and moves of a stall rule; absent on other rules.
    /// Boxed, like `advice`, so the hundreds of thousands of gaps of a large
    /// matrix, which carry neither, stay small.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stall: Option<Box<crate::stall::StallDetail>>,
    /// The recorded advice feedback that governs this gap, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advice: Option<Box<crate::advice_feedback::GapAdvice>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GapInventory {
    pub schema_version: String,
    pub snapshot: Snapshot,
    pub rule_version: String,
    pub case_id: Id,
    pub gaps: Vec<Gap>,
    pub limits: Vec<String>,
}

/// Enumerate structural gaps for one verified state snapshot. The caller must
/// supply the snapshot from the same replay used to populate `state`.
pub fn enumerate_gaps(
    state: &State,
    snapshot: &Snapshot,
    case_id: Id,
) -> Result<GapInventory, Failure> {
    let gaps = case_gaps(state, case_id)?;
    Ok(GapInventory {
        schema_version: "inv.gaps.v1".into(),
        snapshot: snapshot.clone(),
        rule_version: GAP_RULE_VERSION.into(),
        case_id,
        gaps,
        limits: vec![
            "A detected structural gap is not proof that an unrecorded explanation exists.".into(),
            "Zero detected gaps means no gap was emitted by this rule version; it does not mean the investigation is complete.".into(),
            "Rules inspect committed typed fields only and never emit tests, scores, shell commands, or external execution.".into(),
        ],
    })
}

/// The gaps of [`enumerate_gaps`] for the state as it stands, with recorded
/// advice feedback applied. Write handlers use it to name an emitted gap.
pub(crate) fn case_gaps(state: &State, case_id: Id) -> Result<Vec<Gap>, Failure> {
    if case_id.kind() != Kind::Case {
        return Err(Failure::new(ErrorCode::InvalidInput, "Select a case ID.").at("/case_id"));
    }
    let case = state
        .current(case_id)
        .filter(|record| record.header.case_id == case_id)
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::UnknownReference,
                "The selected case is absent from this snapshot.",
            )
            .at("/case_id")
        })?;
    let mut records: Vec<&Record> = state
        .records()
        .filter(|record| record.header.case_id == case_id)
        .collect();
    records.sort_by_key(|record| (record.header.acceptance_sequence, record.header.id));
    let by_kind = |kind| {
        records
            .iter()
            .copied()
            .filter(move |record| record.header.id.kind() == kind)
            .collect::<Vec<_>>()
    };
    let observations = by_kind(Kind::Observation);
    let hypotheses = by_kind(Kind::Hypothesis);
    let assessments = by_kind(Kind::Assessment);
    let inquiries = by_kind(Kind::Inquiry);
    let runs = by_kind(Kind::Run);
    let conclusions = by_kind(Kind::Conclusion);
    let options = by_kind(Kind::Option);
    let criteria = by_kind(Kind::Criterion);
    let comparisons = by_kind(Kind::Comparison);
    let reviews = by_kind(Kind::Review);
    let objections = by_kind(Kind::Objection);
    let retrospectives = by_kind(Kind::Retrospective);
    let mut gaps = Vec::new();
    let case_ref = reference(case);

    // A time-sensitive preservation suggestion is emitted only when the
    // investigator explicitly recorded the flag/deadline in structured data.
    if is_time_sensitive(case) {
        gaps.push(make_gap(GapFields {
    rule_id: "case.preserve_declared_time_sensitive_source",
    subject_refs: vec![case_ref],
    read_set: vec![case_ref],
    status: GapStatus::RequiresSource,
    reason: "The case explicitly declares time-sensitive evidence that has not been retained or linked.",
    prerequisites: vec![prerequisite("source_identity", PrerequisiteState::Unknown, "The source and retention window require investigator input.", vec![case_ref])],
    suggested_operation: Some("source.add"),
    unresolved_fields: vec!["source_ref".into(), "artifact_ref".into(), "retention_window".into()],
    outcome_branches: vec![],
    decision_effect: "context_only",
    time_sensitive: true,
    creation_sequence: case.header.acceptance_sequence,
}));
    }

    for observation in &observations {
        if is_retracted(&observation.body) {
            continue;
        }
        let observation_ref = reference(observation);
        let validity = crate::measurement_ops::effective_validity(state, observation);
        if validity == crate::evidence::MeasurementValidity::Invalid
            || selector_not_found(&observation.body)
        {
            gaps.push(make_gap(GapFields {
    rule_id: "observation.invalid_or_missing_source_match",
    subject_refs: vec![observation_ref],
    read_set: vec![observation_ref],
    status: GapStatus::BlockedByQuality,
    reason: "The recorded Observation has invalid measurement validity or its selector/quote is marked not_found.",
    prerequisites: vec![prerequisite("valid_source_match", PrerequisiteState::Unmet, "Record a corrected Observation only after checking the exact source bytes.", vec![observation_ref])],
    suggested_operation: Some("observation.correct"),
    unresolved_fields: vec!["corrected_value_or_retraction_reason".into()],
    outcome_branches: vec![],
    decision_effect: "unknown",
    time_sensitive: false,
    creation_sequence: observation.header.acceptance_sequence,
}));
        } else if validity == crate::evidence::MeasurementValidity::Unknown
            || observation.body.get("measurement_assessment").is_none()
        {
            gaps.push(make_gap(GapFields {
    rule_id: "observation.validity_context_missing",
    subject_refs: vec![observation_ref],
    read_set: vec![observation_ref],
    status: GapStatus::BlockedByQuality,
    reason: "The Observation has no recorded validity context; presence of a quote or value alone does not establish measurement validity.",
    prerequisites: vec![prerequisite("validity_assessment", PrerequisiteState::Unmet, "An attributed measurement assessment or explicit limitation is needed.", vec![observation_ref])],
    suggested_operation: Some("measurement.assess"),
    unresolved_fields: vec!["instrument_or_channel_validity".into(), "control_or_limitations".into()],
    outcome_branches: vec![],
    decision_effect: "could_discriminate",
    time_sensitive: false,
    creation_sequence: observation.header.acceptance_sequence,
}));
        }
    }

    // Read once: every hypothesis is checked against every assessment.
    let assessed: BTreeSet<RevisionRef> = assessments
        .iter()
        .filter_map(|assessment| exact_ref(field(&assessment.body, "subject_ref")))
        .collect();
    for hypothesis in &hypotheses {
        if !is_live(hypothesis) {
            continue;
        }
        let hypothesis_ref = reference(hypothesis);
        let has_assessment = assessed.contains(&hypothesis_ref);
        if !has_assessment {
            gaps.push(make_gap(GapFields {
                rule_id: "hypothesis.no_assessment",
                subject_refs: vec![hypothesis_ref],
                read_set: vec![hypothesis_ref],
                status: GapStatus::RequiresSource,
                reason: "No current Assessment interprets evidence against this live Hypothesis.",
                prerequisites: vec![prerequisite(
                    "evidence_refs",
                    PrerequisiteState::Unknown,
                    "Select exact observations or runs that bear on the hypothesis.",
                    vec![hypothesis_ref],
                )],
                suggested_operation: Some("assessment.add"),
                unresolved_fields: vec![
                    "evidence_refs".into(),
                    "relation".into(),
                    "rationale".into(),
                ],
                outcome_branches: vec![],
                decision_effect: "could_discriminate",
                time_sensitive: false,
                creation_sequence: hypothesis.header.acceptance_sequence,
            }));
        }
    }

    for inquiry in &inquiries {
        let body = content(&inquiry.body);
        let confirmatory = field(body, "inquiry")
            .and_then(|mode| mode.get("mode"))
            .and_then(Value::as_str)
            == Some("confirmatory");
        let lifecycle = field(&inquiry.body, "lifecycle")
            .and_then(Value::as_str)
            .unwrap_or("draft");
        if confirmatory && lifecycle != "frozen" && lifecycle != "retired" {
            let inquiry_ref = reference(inquiry);
            gaps.push(make_gap(GapFields {
    rule_id: "inquiry.confirmatory_plan_unfrozen",
    subject_refs: vec![inquiry_ref],
    read_set: vec![inquiry_ref],
    status: GapStatus::ReadyToAnalyze,
    reason: "A confirmatory Inquiry remains unfrozen, so its predictions and controls are not locked before outcomes.",
    prerequisites: vec![prerequisite("predictions_and_controls", PrerequisiteState::Unknown, "Review the declared controls, holdout, prior exposure, and stop rule.", vec![inquiry_ref])],
    suggested_operation: Some("inquiry.freeze"),
    unresolved_fields: vec!["question_ref".into(), "predictions".into()],
    outcome_branches: vec![],
    decision_effect: "could_discriminate",
    time_sensitive: false,
    creation_sequence: inquiry.header.acceptance_sequence,
}));
        }
    }

    for run in &runs {
        let status = field(&run.body, "execution_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let completeness = field(&run.body, "evidence_completeness")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if matches!(status, "declared" | "running" | "unknown")
            || matches!(completeness, "partial" | "missing" | "unknown")
        {
            let run_ref = reference(run);
            gaps.push(make_gap(GapFields {
    rule_id: "run.outcome_incomplete_or_unknown",
    subject_refs: vec![run_ref],
    read_set: vec![run_ref],
    status: GapStatus::RequiresExternalAction,
    reason: "The Run has a partial or unknown outcome; reconcile recorded facts without treating a process status as evidence validity.",
    prerequisites: vec![prerequisite("terminal_outcome", PrerequisiteState::Unmet, "A consumer must supply a terminal outcome or explicitly retain uncertainty.", vec![run_ref])],
    suggested_operation: Some("run.reconcile"),
    unresolved_fields: vec!["outcome".into(), "evidence_validity".into(), "rationale".into()],
    outcome_branches: vec![],
    decision_effect: "could_discriminate",
    time_sensitive: false,
    creation_sequence: run.header.acceptance_sequence,
}));
        }
    }

    for conclusion in &conclusions {
        if !is_live(conclusion) {
            continue;
        }
        let conclusion_ref = reference(conclusion);
        let body = content(&conclusion.body);
        let factors = field(body, "causal_factors")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let has_rival_record = factors
            .iter()
            .filter_map(|factor| exact_ref(field(factor, "hypothesis_ref")))
            .any(|hypothesis_ref| {
                state.current(hypothesis_ref.id).is_some_and(|hypothesis| {
                    let rival_refs =
                        field(content(&hypothesis.body), "rival_refs").and_then(Value::as_array);
                    rival_refs.is_some_and(|refs| !refs.is_empty())
                })
            });
        let bounded_alternative_reason = field(body, "alternatives_not_useful_reason")
            .and_then(Value::as_str)
            .is_some_and(|reason| !reason.trim().is_empty());
        if !factors.is_empty() && !has_rival_record && !bounded_alternative_reason {
            gaps.push(make_gap(GapFields {
    rule_id: "conclusion.causal_claim_without_rival_account",
    subject_refs: vec![conclusion_ref],
    read_set: vec![conclusion_ref],
    status: GapStatus::RequiresSource,
    reason: "A causal Conclusion names causal factors but records no acknowledged rival account or contradiction reference.",
    prerequisites: vec![prerequisite("rival_account", PrerequisiteState::Unmet, "Record a live alternative or a bounded reason alternatives were not useful.", vec![conclusion_ref])],
    suggested_operation: Some("hypothesis.create"),
    unresolved_fields: vec!["rival_account_or_bounded_reason".into()],
    outcome_branches: vec![],
    decision_effect: "could_discriminate",
    time_sensitive: false,
    creation_sequence: conclusion.header.acceptance_sequence,
}));
        }
        let disputed = field(&conclusion.body, "effective_disposition").and_then(Value::as_str)
            == Some("disputed")
            || field(&conclusion.body, "status").and_then(Value::as_str) == Some("disputed")
            || objections.iter().any(|objection| {
                exact_ref(field(&objection.body, "subject_ref"))
                    .is_some_and(|r| r.id == conclusion_ref.id)
                    && !is_retracted(&objection.body)
                    // Resolved or withdrawn objections stay on record but no
                    // longer dispute the conclusion.
                    && field(&objection.body, "status").and_then(Value::as_str) == Some("open")
            });
        if disputed {
            gaps.push(make_gap(GapFields {
    rule_id: "conclusion.disputed",
    subject_refs: vec![conclusion_ref],
    read_set: vec![conclusion_ref],
    status: GapStatus::StaleDueToRevision,
    reason: "A material Objection or recorded disposition disputes this Conclusion and requires review before unqualified use.",
    prerequisites: vec![prerequisite("objection_disposition", PrerequisiteState::Unmet, "Resolve the linked Objection or record a scoped review.", vec![conclusion_ref])],
    suggested_operation: Some("review.submit"),
    unresolved_fields: vec!["review_scope".into(), "verdict".into(), "read_set".into()],
    outcome_branches: vec![],
    decision_effect: "could_flip_choice",
    time_sensitive: false,
    creation_sequence: conclusion.header.acceptance_sequence,
}));
        }
    }

    for comparison in &comparisons {
        if field(&comparison.body, "retracted").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let comparison_ref = reference(comparison);
        let option_ref = exact_ref(field(&comparison.body, "option_ref"));
        let criterion_ref = exact_ref(field(&comparison.body, "criterion_ref"));
        let stale_refs: Vec<RevisionRef> = [option_ref, criterion_ref]
            .into_iter()
            .flatten()
            .filter(|reference| {
                state
                    .current(reference.id)
                    .is_some_and(|current| current.reference() != *reference)
            })
            .collect();
        if !stale_refs.is_empty() {
            gaps.push(make_gap(GapFields {
                rule_id: "comparison.stale_dependency_revision",
                subject_refs: vec![comparison_ref],
                read_set: join_refs(vec![comparison_ref], stale_refs.clone()),
                status: GapStatus::StaleDueToRevision,
                reason: "This Comparison refers to an earlier Option or Criterion revision.",
                prerequisites: vec![prerequisite(
                    "current_dependencies",
                    PrerequisiteState::Unmet,
                    "Reassess against the current exact Option and Criterion revisions.",
                    stale_refs.clone(),
                )],
                suggested_operation: Some("comparison.revise"),
                unresolved_fields: vec![
                    "current_option_ref".into(),
                    "current_criterion_ref".into(),
                    "rationale".into(),
                ],
                outcome_branches: vec![],
                decision_effect: "could_flip_choice",
                time_sensitive: false,
                creation_sequence: comparison.header.acceptance_sequence,
            }));
        } else if criterion_ref
            .and_then(|reference| state.current(reference.id))
            .is_some_and(|criterion| {
                field(content(&criterion.body), "kind").and_then(Value::as_str) == Some("hard")
            })
            && field(&comparison.body, "outcome").and_then(Value::as_str) == Some("unknown")
        {
            gaps.push(make_gap(GapFields {
    rule_id: "decision.hard_criterion_cell_unknown",
    subject_refs: vec![comparison_ref],
    read_set: join_refs(vec![comparison_ref], [option_ref, criterion_ref].into_iter().flatten().collect()),
    status: GapStatus::RequiresSource,
    reason: "A hard-criterion Comparison is explicitly unknown; do not treat it as a pass or a fail.",
    prerequisites: vec![prerequisite("hard_cell_evidence", PrerequisiteState::Unknown, "Obtain the specific evidence required to assess the hard criterion.", vec![comparison_ref])],
    suggested_operation: Some("comparison.revise"),
    unresolved_fields: vec!["evidence_refs".into(), "measured_or_qualitative_result".into(), "uncertainty".into()],
    outcome_branches: vec![branch("meets", "The option remains eligible under this criterion.", "typed hard threshold"), branch("fails", "The option is ruled out for the declared scope.", "typed hard threshold"), branch("unknown", "Eligibility remains unresolved.", "insufficient evidence")],
    decision_effect: "could_flip_choice",
    time_sensitive: false,
    creation_sequence: comparison.header.acceptance_sequence,
}));
        }
    }

    let mut cells = BTreeMap::<(Id, Id), RevisionRef>::new();
    for comparison in &comparisons {
        if field(&comparison.body, "retracted").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let (Some(option_ref), Some(criterion_ref)) = (
            exact_ref(field(&comparison.body, "option_ref")),
            exact_ref(field(&comparison.body, "criterion_ref")),
        ) {
            cells.insert((option_ref.id, criterion_ref.id), reference(comparison));
        }
    }
    // What the cell rule reads of each option, read once rather than once per
    // criterion: its question and whether its lifecycle takes it out.
    let option_facts: Vec<(&Record, Option<Id>, bool)> = options
        .iter()
        .map(|option| {
            let option_body = content(&option.body);
            let question =
                exact_ref(field(option_body, "question_ref")).map(|reference| reference.id);
            let withdrawn = field(option_body, "lifecycle")
                .and_then(Value::as_str)
                .is_some_and(|lifecycle| {
                    matches!(lifecycle, "ruled_out" | "withdrawn" | "superseded")
                });
            (*option, question, withdrawn)
        })
        .collect();
    // Each criterion's cells are independent of every other criterion's, so a
    // large matrix is split across threads by criterion; the gaps come back
    // in criterion order, as one loop would emit them.
    let cell_gaps = crate::parallel::map_chunks(
        &criteria,
        criteria.len().saturating_mul(option_facts.len()),
        |criteria| {
            let mut gaps = Vec::new();
            for criterion in criteria {
                let criterion_body = content(&criterion.body);
                if field(criterion_body, "lifecycle")
                    .and_then(Value::as_str)
                    .is_some_and(|lifecycle| lifecycle != "active")
                {
                    continue;
                }
                let criterion_question = exact_ref(field(criterion_body, "question_ref"))
                    .map(|reference| reference.id)
                    .or_else(|| {
                        field(criterion_body, "scope")
                            .and_then(|scope| exact_ref(field(scope, "question_ref")))
                            .map(|reference| reference.id)
                    });
                let is_hard = field(criterion_body, "kind").and_then(Value::as_str) == Some("hard");
                let criterion_ref = reference(criterion);
                // The IDs of the exact references in `option_refs`, read once.
                let scoped_options: Option<Vec<Id>> = field(criterion_body, "option_refs")
                    .and_then(Value::as_array)
                    .map(|refs| {
                        refs.iter()
                            .filter_map(|reference| exact_ref(Some(reference)))
                            .map(|reference| reference.id)
                            .collect()
                    });
                for (option, option_question, withdrawn) in &option_facts {
                    if criterion_question.is_some() && *option_question != criterion_question {
                        continue;
                    }
                    if scoped_options
                        .as_ref()
                        .is_some_and(|ids| !ids.contains(&option.header.id))
                    {
                        continue;
                    }
                    if *withdrawn {
                        continue;
                    }
                    if !cells.contains_key(&(option.header.id, criterion.header.id)) {
                        let option_ref = reference(option);
                        let status = GapStatus::RequiresSource;
                        gaps.push(make_gap(GapFields {
                    rule_id: if is_hard {
                        "decision.hard_criterion_cell_missing"
                    } else {
                        "decision.option_criterion_cell_missing"
                    },
                    subject_refs: vec![option_ref, criterion_ref],
                    read_set: vec![option_ref, criterion_ref],
                    status,
                    reason: if is_hard {
                        "An applicable Option has no Comparison for a hard Criterion."
                    } else {
                        "An applicable Option has no Comparison for this Criterion."
                    },
                    prerequisites: vec![prerequisite(
                        "comparison_evidence",
                        PrerequisiteState::Unknown,
                        "Supply evidence and rationale, or record an explicit unknown result.",
                        vec![option_ref, criterion_ref],
                    )],
                    suggested_operation: Some("comparison.add"),
                    unresolved_fields: vec![
                        "method".into(),
                        "evidence_refs".into(),
                        "outcome".into(),
                        "uncertainty".into(),
                    ],
                    outcome_branches: if is_hard {
                        vec![
                            branch(
                                "meets",
                                "The option remains eligible under this criterion.",
                                "typed hard threshold",
                            ),
                            branch(
                                "fails",
                                "The option is ruled out for this scope.",
                                "typed hard threshold",
                            ),
                            branch(
                                "unknown",
                                "Eligibility remains unresolved.",
                                "insufficient evidence",
                            ),
                        ]
                    } else {
                        vec![]
                    },
                    decision_effect: if is_hard {
                        "could_flip_choice"
                    } else {
                        "context_only"
                    },
                    time_sensitive: false,
                    creation_sequence: option
                        .header
                        .acceptance_sequence
                        .min(criterion.header.acceptance_sequence),
                }));
                    }
                }
            }
            gaps
        },
    );
    gaps.extend(cell_gaps);

    for review in &reviews {
        let review_ref = reference(review);
        let mut stale = Vec::new();
        if let Some(read_set) = field(&review.body, "read_set").and_then(Value::as_array) {
            for value in read_set {
                // The freshness index's predicate for the review's own edge:
                // a benign later revision, such as a valid measurement
                // judgment or a question answered with the reviewed subject,
                // leaves the read current.
                if let Some(reference) = exact_ref(Some(value))
                    && state
                        .current(reference.id)
                        .is_some_and(|current| current.reference() != reference)
                    && crate::freshness::benign_successor_for(state, case_id, reference, review)
                        .is_none()
                {
                    stale.push(reference);
                }
            }
        }
        if !stale.is_empty() {
            gaps.push(make_gap(GapFields {
                rule_id: "review.stale_read_set",
                subject_refs: vec![review_ref],
                read_set: join_refs(vec![review_ref], stale.clone()),
                status: GapStatus::StaleDueToRevision,
                reason: "A Review read-set contains revisions that have since changed in a way its reader must recheck.",
                prerequisites: vec![prerequisite(
                    "current_read_set",
                    PrerequisiteState::Unmet,
                    "Recheck the changed records and submit a fresh scoped review.",
                    stale.clone(),
                )],
                suggested_operation: Some("review.submit"),
                unresolved_fields: vec![
                    "audited_snapshot".into(),
                    "read_set".into(),
                    "scope_examined".into(),
                ],
                outcome_branches: vec![],
                decision_effect: "could_flip_choice",
                time_sensitive: false,
                creation_sequence: review.header.acceptance_sequence,
            }));
        }
    }

    freshness_gaps(state, case_id, &records, &mut gaps);

    let closure_ready =
        extension_true(case, "inv.closure_ready") || extension_true(case, "inv.ready_to_close");
    if closure_ready && retrospectives.is_empty() {
        gaps.push(make_gap(GapFields {
    rule_id: "case.ready_to_close_without_retrospective",
    subject_refs: vec![case_ref],
    read_set: vec![case_ref],
    status: GapStatus::ReadyToAnalyze,
    reason: "The case explicitly marks itself ready to close but has no recorded Retrospective.",
    prerequisites: vec![prerequisite("retrospective", PrerequisiteState::Unmet, "Record bounded methods, limits, residual uncertainty, and lessons or an allowed waiver.", vec![case_ref])],
    suggested_operation: Some("retrospective.record"),
    unresolved_fields: vec!["methods_attempted".into(), "residual_unknowns".into(), "lessons_or_waiver".into()],
    outcome_branches: vec![],
    decision_effect: "context_only",
    time_sensitive: false,
    creation_sequence: case.header.acceptance_sequence,
}));
    }

    // Older accepted Decisions should surface any unresolved active hard cell
    // in the same question, but the engine never chooses among options.
    let unresolved_hard = gaps
        .iter()
        .any(|gap| gap.rule_id.starts_with("decision.hard_criterion_cell_"));
    for decision in records.iter().copied().filter(|record| {
        record.header.id.kind() == Kind::Decision
            && field(&record.body, "lifecycle").and_then(Value::as_str) == Some("accepted")
    }) {
        if unresolved_hard {
            let decision_ref = reference(decision);
            gaps.push(make_gap(GapFields {
                rule_id: "decision.accepted_with_unresolved_hard_cell",
                subject_refs: vec![decision_ref],
                read_set: vec![decision_ref],
                status: GapStatus::StaleDueToRevision,
                reason: "An accepted Decision has an unresolved hard-criterion cell in the case.",
                prerequisites: vec![prerequisite(
                    "hard_criterion_cells",
                    PrerequisiteState::Unmet,
                    "Resolve the applicable hard cells or keep the choice explicitly conditional.",
                    vec![decision_ref],
                )],
                suggested_operation: Some("decision.propose"),
                unresolved_fields: vec![
                    "updated_comparisons".into(),
                    "conditional_reopen_trigger".into(),
                ],
                outcome_branches: vec![],
                decision_effect: "could_flip_choice",
                time_sensitive: false,
                creation_sequence: decision.header.acceptance_sequence,
            }));
        }
    }

    measurement_flag_gaps(state, case_id, &mut gaps);
    stall_gaps(state, case_id, &mut gaps);
    deduplicate_and_sort(&mut gaps);
    // Feedback changes only how a gap is presented, after every rule ran.
    crate::advice_feedback::apply(state, case_id, &mut gaps);
    Ok(gaps)
}

/// Live judgments the freshness index marks `needs_review`: a cited record was
/// withdrawn, retracted, materially revised or newly judged doubtful, directly
/// or through the records it relies on. The reason names each cause and path.
fn freshness_gaps(state: &State, case_id: Id, records: &[&Record], gaps: &mut Vec<Gap>) {
    const SHOWN: usize = 3;
    let index = crate::freshness::FreshnessIndex::build(state, case_id);
    for record in records.iter().copied().filter(|record| {
        matches!(
            record.header.id.kind(),
            Kind::Assessment | Kind::Comparison | Kind::Conclusion | Kind::Decision
        ) && is_live(record)
            && !matches!(
                field(&record.body, "status").and_then(Value::as_str),
                Some("superseded" | "withdrawn" | "rejected")
            )
    }) {
        let Some(result) = index
            .get(record.header.id)
            .filter(|result| result.needs_review())
        else {
            continue;
        };
        let reasons: Vec<_> = result
            .reasons
            .iter()
            .filter(|reason| reason.freshness == crate::freshness::Freshness::NeedsReview)
            .collect();
        let mut text = format!(
            "Effective freshness is {}: {}",
            result.label(),
            reasons
                .iter()
                .take(SHOWN)
                .map(|reason| reason.describe())
                .collect::<Vec<_>>()
                .join("; ")
        );
        if reasons.len() > SHOWN {
            text.push_str(&format!("; and {} more", reasons.len() - SHOWN));
        }
        text.push('.');
        let mut read_set = Vec::new();
        let prerequisites = reasons
            .iter()
            .map(|reason| {
                let mut refs = vec![reason.cause];
                refs.extend(reason.path.iter().map(|step| step.cites));
                read_set.extend(refs.iter().copied());
                prerequisite(
                    "current_basis",
                    PrerequisiteState::Unmet,
                    &format!("Recheck against {}.", reason.describe()),
                    refs,
                )
            })
            .collect();
        let (operation, effect) = match record.header.id.kind() {
            Kind::Assessment => ("assessment.supersede", "unknown"),
            Kind::Comparison => ("comparison.revise", "could_flip_choice"),
            Kind::Conclusion => ("review.submit", "unknown"),
            _ => ("decision.propose", "could_flip_choice"),
        };
        let subject = reference(record);
        gaps.push(make_gap(GapFields {
            rule_id: "freshness.needs_review",
            subject_refs: vec![subject],
            read_set: join_refs(vec![subject], read_set),
            status: GapStatus::StaleDueToRevision,
            reason: &text,
            prerequisites,
            suggested_operation: Some(operation),
            unresolved_fields: vec!["current_basis".into(), "rationale".into()],
            outcome_branches: vec![],
            decision_effect: effect,
            time_sensitive: false,
            creation_sequence: record.header.acceptance_sequence,
        }));
    }
}

pub(crate) struct GapFields<'a> {
    pub(crate) rule_id: &'a str,
    pub(crate) subject_refs: Vec<RevisionRef>,
    pub(crate) read_set: Vec<RevisionRef>,
    pub(crate) status: GapStatus,
    pub(crate) reason: &'a str,
    pub(crate) prerequisites: Vec<Prerequisite>,
    pub(crate) suggested_operation: Option<&'a str>,
    pub(crate) unresolved_fields: Vec<String>,
    pub(crate) outcome_branches: Vec<OutcomeBranch>,
    pub(crate) decision_effect: &'a str,
    pub(crate) time_sensitive: bool,
    pub(crate) creation_sequence: u64,
}

pub(crate) fn make_gap(fields: GapFields<'_>) -> Gap {
    let GapFields {
        rule_id,
        mut subject_refs,
        mut read_set,
        status,
        reason,
        prerequisites,
        suggested_operation,
        unresolved_fields,
        outcome_branches,
        decision_effect,
        time_sensitive,
        creation_sequence,
    } = fields;
    subject_refs.sort_by_key(|reference| (reference.id, reference.revision));
    subject_refs.dedup();
    // The read set covers the subjects; most rules already list them.
    for subject in &subject_refs {
        if !read_set.contains(subject) {
            read_set.push(*subject);
        }
    }
    read_set.sort_by_key(|reference| (reference.id, reference.revision));
    read_set.dedup();
    // gap:{rule_id}:{subject ID}+{subject ID}..., written in one buffer.
    let mut gap_id = String::with_capacity(5 + rule_id.len() + 48 * subject_refs.len());
    gap_id.push_str("gap:");
    gap_id.push_str(rule_id);
    gap_id.push(':');
    for (index, reference) in subject_refs.iter().enumerate() {
        if index > 0 {
            gap_id.push('+');
        }
        write!(gap_id, "{}", reference.id).expect("writing to a String cannot fail");
    }
    Gap {
        gap_id,
        rule_id: rule_id.into(),
        rule_version: GAP_RULE_VERSION.into(),
        subject_refs,
        read_set,
        status,
        reason: reason.into(),
        prerequisites,
        suggested_operation: suggested_operation.map(str::to_owned),
        unresolved_fields,
        outcome_branches,
        decision_effect: decision_effect.into(),
        time_sensitive,
        creation_sequence,
        stall: None,
        advice: None,
    }
}

fn prerequisite(
    name: &str,
    state: PrerequisiteState,
    reason: &str,
    mut subject_refs: Vec<RevisionRef>,
) -> Prerequisite {
    subject_refs.sort_by_key(|reference| (reference.id, reference.revision));
    subject_refs.dedup();
    Prerequisite {
        name: name.into(),
        state,
        reason: reason.into(),
        subject_refs,
    }
}

fn branch(outcome: &str, consequence: &str, basis: &str) -> OutcomeBranch {
    OutcomeBranch {
        outcome: outcome.into(),
        consequence: consequence.into(),
        basis: basis.into(),
    }
}

fn reference(record: &Record) -> RevisionRef {
    record.reference()
}

pub(crate) fn content(body: &Value) -> &Value {
    body.get("content").unwrap_or(body)
}

pub(crate) fn field<'a>(body: &'a Value, key: &str) -> Option<&'a Value> {
    body.get(key)
        .or_else(|| body.get("content").and_then(|content| content.get(key)))
}

/// An exact revision reference, read from the value in place.
pub(crate) fn exact_ref(value: Option<&Value>) -> Option<RevisionRef> {
    value.and_then(|value| RevisionRef::deserialize(value).ok())
}

fn join_refs(mut refs: Vec<RevisionRef>, extra: Vec<RevisionRef>) -> Vec<RevisionRef> {
    refs.extend(extra);
    refs.sort_by_key(|reference| (reference.id, reference.revision));
    refs.dedup();
    refs
}

pub(crate) fn is_retracted(body: &Value) -> bool {
    field(body, "status").and_then(Value::as_str) == Some("retracted")
        || field(body, "retracted").and_then(Value::as_bool) == Some(true)
        || field(body, "lifecycle").and_then(Value::as_str) == Some("retracted")
}

pub(crate) fn is_live(record: &Record) -> bool {
    !matches!(
        field(&record.body, "lifecycle").and_then(Value::as_str),
        Some("retired" | "rejected_within_scope" | "rejected" | "retracted" | "superseded")
    ) && !is_retracted(&record.body)
}

fn selector_not_found(body: &Value) -> bool {
    let basis = field(body, "basis");
    let selector = basis.and_then(|basis| basis.get("selector"));
    selector
        .and_then(|selector| selector.get("validation"))
        .and_then(Value::as_str)
        == Some("not_found")
        || basis
            .and_then(|basis| basis.get("text_quote"))
            .and_then(|quote| quote.get("state"))
            .and_then(Value::as_str)
            == Some("not_found")
}

fn extension_true(record: &Record, key: &str) -> bool {
    record.header.extensions.get(key).and_then(Value::as_bool) == Some(true)
}

fn is_time_sensitive(record: &Record) -> bool {
    extension_true(record, "inv.time_sensitive_preservation")
        || record
            .header
            .extensions
            .get("inv.preservation_deadline")
            .is_some_and(|value| !value.is_null())
}

/// One advisory gap per measurement flag, keyed by the flag ID and the records
/// it is about. These never block; they point at fields worth recording.
fn measurement_flag_gaps(state: &State, case_id: Id, gaps: &mut Vec<Gap>) {
    for flag in crate::measurement_flags::case_flags(state, case_id) {
        let subject = flag.subject_ref;
        let comparison = subject.id.kind() == Kind::Comparison;
        let mut read_set = flag.related_refs.clone();
        read_set.extend(flag.fields.iter().map(|field| field.record_ref));
        gaps.push(make_gap(GapFields {
            rule_id: &flag.flag_id,
            subject_refs: std::iter::once(subject)
                .chain(flag.related_refs.iter().copied())
                .collect(),
            read_set,
            status: GapStatus::ReadyToAnalyze,
            reason: &flag.reason,
            prerequisites: vec![prerequisite(
                "measurement_conditions",
                PrerequisiteState::Unknown,
                crate::measurement_flags::ADVISORY_NOTICE,
                vec![subject],
            )],
            suggested_operation: match subject.id.kind() {
                Kind::Observation => Some("measurement.assess"),
                Kind::Comparison => Some("comparison.revise"),
                _ => None,
            },
            unresolved_fields: flag.fields.iter().map(|field| field.path.clone()).collect(),
            outcome_branches: vec![],
            decision_effect: if comparison {
                "could_flip_choice"
            } else {
                "context_only"
            },
            time_sensitive: false,
            creation_sequence: state
                .revision(subject)
                .map_or(0, |record| record.header.acceptance_sequence),
        }));
    }
}

/// One gap per detected stall pattern. A stall gap proposes no single
/// operation: its moves name the exact records and operations to choose from,
/// and a justified stop is always among them.
fn stall_gaps(state: &State, case_id: Id, gaps: &mut Vec<Gap>) {
    for stall in crate::stall::detect(state, case_id, gaps) {
        let mut gap = make_gap(GapFields {
            rule_id: stall.pattern.rule_id(),
            subject_refs: stall.subject_refs.clone(),
            read_set: stall.read_set,
            status: GapStatus::ReadyToAnalyze,
            reason: &stall.reason,
            prerequisites: vec![prerequisite(
                "stall_response",
                PrerequisiteState::Unknown,
                "Choose one of the offered moves and record why; stopping with an inconclusive result is a legitimate choice.",
                stall.subject_refs,
            )],
            suggested_operation: None,
            unresolved_fields: vec!["chosen_move".into(), "rationale".into()],
            outcome_branches: vec![],
            decision_effect: "context_only",
            time_sensitive: false,
            creation_sequence: stall.creation_sequence,
        });
        gap.stall = Some(Box::new(stall.detail));
        gaps.push(gap);
    }
}

/// Keep the first emitted gap of each gap ID, in gap ID order. The order is
/// worked out on (ID, emission position) pairs: unique keys, so an unstable
/// sort gives the order a stable sort by ID gives, and equal IDs stay in
/// emission order. The gaps are then moved into that order in place, the
/// later duplicates last, and the duplicates dropped.
fn deduplicate_and_sort(gaps: &mut Vec<Gap>) {
    let mut order: Vec<(&str, usize)> = gaps
        .iter()
        .enumerate()
        .map(|(position, gap)| (gap.gap_id.as_str(), position))
        .collect();
    order.sort_unstable();
    let mut kept = Vec::with_capacity(order.len());
    let mut later = Vec::new();
    for (index, &(id, position)) in order.iter().enumerate() {
        if index > 0 && order[index - 1].0 == id {
            later.push(position);
        } else {
            kept.push(position);
        }
    }
    let unique = kept.len();
    kept.extend(later);
    permute(gaps, &kept);
    gaps.truncate(unique);
}

/// Reorder `items` in place so that position `i` holds the item that was at
/// `sources[i]`. `sources` must be a permutation of the positions.
fn permute<T>(items: &mut [T], sources: &[usize]) {
    debug_assert_eq!(items.len(), sources.len());
    let mut placed = vec![false; items.len()];
    for start in 0..items.len() {
        // Follow the cycle through `start`: each swap puts one item in its
        // place, and the item from `start` travels to the cycle's end.
        let mut target = start;
        while !placed[target] {
            placed[target] = true;
            let source = sources[target];
            if source == start {
                break;
            }
            items.swap(target, source);
            target = source;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::{
        Actor, Classification, Provenance, RecordHeader, Revision, SchemaVersion, UtcTimestamp,
    };
    use serde_json::json;

    fn id(value: &str) -> Id {
        value.parse().unwrap()
    }

    fn record(id: Id, case_id: Id, seq: u64, body: Value) -> Record {
        let actor: Actor = "agent:test".parse().unwrap();
        let time: UtcTimestamp = "2026-11-22T00:00:00.0Z".parse().unwrap();
        Record {
            header: RecordHeader {
                id,
                case_id,
                schema_version: SchemaVersion::RecordV0Alpha,
                created_by: actor.clone(),
                actor_descriptor_self_reported: None,
                created_at: time.clone(),
                recorded_at: time,
                recorded_by: actor,
                observed_at: None,
                acquired_at: None,
                acceptance_sequence: seq,
                revision: Revision(1),
                history: vec![],
                classification: Classification::default(),
                provenance: Provenance::default(),
                extensions: Default::default(),
            },
            body,
        }
    }

    fn state(records: Vec<Record>) -> State {
        let case_id = records[0].header.case_id;
        let transaction = crate::state::AcceptedTransaction {
            case_id,
            sequence: 1,
            records: records.clone(),
            created: records
                .iter()
                .map(|record| crate::wire::RecordChange {
                    client_ref: None,
                    id: record.header.id,
                    revision: 1,
                })
                .collect(),
            updated: vec![],
            warnings: vec![],
        };
        let mut state = State::default();
        state.apply(&transaction).unwrap();
        state
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            sequence: 1,
            head_hash: format!("sha256:{}", "a".repeat(64)).parse().unwrap(),
        }
    }

    #[test]
    fn permute_moves_each_item_to_its_target() {
        // Every permutation of five positions, and a long one made of
        // several cycles and fixed points.
        fn permutations(items: Vec<usize>) -> Vec<Vec<usize>> {
            if items.len() <= 1 {
                return vec![items];
            }
            (0..items.len())
                .flat_map(|index| {
                    let mut rest = items.clone();
                    let first = rest.remove(index);
                    permutations(rest).into_iter().map(move |mut tail| {
                        tail.insert(0, first);
                        tail
                    })
                })
                .collect()
        }
        let mut cases = permutations((0..5).collect());
        cases.push((0..1000).map(|i| (i * 7 + 3) % 1000).collect());
        for sources in cases {
            let mut items: Vec<String> = (0..sources.len()).map(|i| format!("item {i}")).collect();
            let expected: Vec<String> = sources.iter().map(|&i| items[i].clone()).collect();
            permute(&mut items, &sources);
            assert_eq!(items, expected, "{sources:?}");
        }
    }

    #[test]
    fn duplicate_gap_ids_keep_the_first_emitted_in_gap_id_order() {
        let gap = |id: &str, reason: &str| Gap {
            gap_id: id.into(),
            rule_id: "r".into(),
            rule_version: GAP_RULE_VERSION.into(),
            subject_refs: vec![],
            read_set: vec![],
            status: GapStatus::ReadyToAnalyze,
            reason: reason.into(),
            prerequisites: vec![],
            suggested_operation: None,
            unresolved_fields: vec![],
            outcome_branches: vec![],
            decision_effect: "unknown".into(),
            time_sensitive: false,
            creation_sequence: 0,
            stall: None,
            advice: None,
        };
        let mut gaps = vec![
            gap("gap:c", "first c"),
            gap("gap:a", "first a"),
            gap("gap:c", "second c"),
            gap("gap:b", "only b"),
            gap("gap:a", "second a"),
            gap("gap:c", "third c"),
        ];
        deduplicate_and_sort(&mut gaps);
        let kept: Vec<(&str, &str)> = gaps
            .iter()
            .map(|gap| (gap.gap_id.as_str(), gap.reason.as_str()))
            .collect();
        assert_eq!(
            kept,
            [
                ("gap:a", "first a"),
                ("gap:b", "only b"),
                ("gap:c", "first c")
            ]
        );
    }

    #[test]
    fn inventory_is_deterministic_and_gap_ids_survive_record_order_changes() {
        let case_id = id("case_019acd16-0000-7000-8000-000000000001");
        let q = id("q_019acd16-0000-7000-8000-000000000002");
        let opt = id("opt_019acd16-0000-7000-8000-000000000003");
        let crit = id("crit_019acd16-0000-7000-8000-000000000004");
        let mut case = record(
            case_id,
            case_id,
            1,
            json!({"title":"fixture","lifecycle":"active"}),
        );
        case.header
            .extensions
            .insert("inv.time_sensitive_preservation".into(), json!(true));
        let question = record(
            q,
            case_id,
            1,
            json!({"wording":"Which option meets the hard requirement?"}),
        );
        let option = record(
            opt,
            case_id,
            1,
            json!({"question_ref":{"id":q,"revision":1},"lifecycle":"eligible"}),
        );
        let criterion = record(
            crit,
            case_id,
            1,
            json!({"name":"recovery","kind":"hard","question_ref":{"id":q,"revision":1},"lifecycle":"active"}),
        );
        let state = state(vec![
            case.clone(),
            question.clone(),
            option.clone(),
            criterion.clone(),
        ]);
        let first = enumerate_gaps(&state, &snapshot(), case_id).unwrap();
        let second = enumerate_gaps(&state, &snapshot(), case_id).unwrap();
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
        assert!(
            first
                .gaps
                .iter()
                .any(|gap| gap.rule_id == "decision.hard_criterion_cell_missing")
        );
        assert!(
            first
                .gaps
                .iter()
                .any(|gap| gap.rule_id == "case.preserve_declared_time_sensitive_source")
        );
        assert!(
            first
                .gaps
                .iter()
                .all(|gap| gap.gap_id.contains(&gap.rule_id))
        );
    }
}
