//! Deterministic descriptions of the structural gap rules, and explanations of
//! individual gaps by stable `gap_id`.
//!
//! A gap ID spells the rule that emitted it and the subject IDs it covers, so
//! any well-formed gap ID can be explained, including one that the current
//! snapshot no longer emits. A description reports what a rule inspects; it is
//! not evidence, and a changed rule version is not newly acquired evidence.

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;

use crate::{
    gap_rules::{GAP_RULE_VERSION, Gap, GapInventory, GapStatus},
    identity::{Id, Kind},
    state::State,
    wire::{ErrorCode, Failure, Snapshot},
};

pub const GAP_EXPLANATION_VERSION: &str = "inv.gap_explanation.v1";

struct RuleSpec {
    rule_id: &'static str,
    subject_kinds: &'static [Kind],
    status: GapStatus,
    condition: &'static str,
    inspected_fields: &'static [&'static str],
    suggested_operation: Option<&'static str>,
    persistence: &'static str,
}

/// Every rule emitted by `GAP_RULE_VERSION`, in rule ID order. A test checks
/// this table against the rule engine source so the two cannot drift.
const RULES: &[RuleSpec] = &[
    RuleSpec {
        rule_id: "case.preserve_declared_time_sensitive_source",
        subject_kinds: &[Kind::Case],
        status: GapStatus::RequiresSource,
        condition: "The case header sets extension inv.time_sensitive_preservation to true or sets a non-null inv.preservation_deadline.",
        inspected_fields: &[
            "header.extensions.inv.time_sensitive_preservation",
            "header.extensions.inv.preservation_deadline",
        ],
        suggested_operation: Some("source.add"),
        persistence: "Emitted while the declaration stands; this rule version reads only the declaration and does not check for a linked source.",
    },
    RuleSpec {
        rule_id: "case.ready_to_close_without_retrospective",
        subject_kinds: &[Kind::Case],
        status: GapStatus::ReadyToAnalyze,
        condition: "The case header sets extension inv.closure_ready or inv.ready_to_close to true and the case has no Retrospective record.",
        inspected_fields: &[
            "header.extensions.inv.closure_ready",
            "header.extensions.inv.ready_to_close",
            "retrospective records in the case",
        ],
        suggested_operation: Some("retrospective.record"),
        persistence: "Stops when the case records any Retrospective or withdraws the closure flag.",
    },
    RuleSpec {
        rule_id: "comparison.stale_dependency_revision",
        subject_kinds: &[Kind::Comparison],
        status: GapStatus::StaleDueToRevision,
        condition: "A non-retracted Comparison names an option_ref or criterion_ref revision older than the current revision of that record.",
        inspected_fields: &["retracted", "option_ref", "criterion_ref"],
        suggested_operation: Some("comparison.revise"),
        persistence: "Stops when the Comparison names the current Option and Criterion revisions or is retracted.",
    },
    RuleSpec {
        rule_id: "conclusion.causal_claim_without_rival_account",
        subject_kinds: &[Kind::Conclusion],
        status: GapStatus::RequiresSource,
        condition: "A live Conclusion lists causal_factors, no Hypothesis named by those factors records rival_refs, and the Conclusion gives no alternatives_not_useful_reason.",
        inspected_fields: &[
            "lifecycle",
            "causal_factors[].hypothesis_ref",
            "rival_refs of each named Hypothesis",
            "alternatives_not_useful_reason",
        ],
        suggested_operation: Some("hypothesis.create"),
        persistence: "Stops when a named Hypothesis records a rival, the Conclusion records a bounded reason, or the Conclusion is no longer live.",
    },
    RuleSpec {
        rule_id: "conclusion.disputed",
        subject_kinds: &[Kind::Conclusion],
        status: GapStatus::StaleDueToRevision,
        condition: "A live Conclusion has effective_disposition or status disputed, or an open Objection names it as subject_ref.",
        inspected_fields: &[
            "lifecycle",
            "effective_disposition",
            "status",
            "subject_ref and status of each Objection",
        ],
        suggested_operation: Some("review.submit"),
        persistence: "Stops when no disputed disposition or open Objection remains, or the Conclusion is no longer live.",
    },
    RuleSpec {
        rule_id: "decision.accepted_with_unresolved_hard_cell",
        subject_kinds: &[Kind::Decision],
        status: GapStatus::StaleDueToRevision,
        condition: "A Decision has lifecycle accepted while the same rule version emits any decision.hard_criterion_cell_missing or decision.hard_criterion_cell_unknown gap in the case.",
        inspected_fields: &["lifecycle", "hard-criterion cell gaps in the case"],
        suggested_operation: Some("decision.propose"),
        persistence: "Stops when no hard-criterion cell gap remains in the case or the Decision is no longer accepted.",
    },
    RuleSpec {
        rule_id: "decision.hard_criterion_cell_missing",
        subject_kinds: &[Kind::Option, Kind::Criterion],
        status: GapStatus::RequiresSource,
        condition: "An active hard Criterion and an applicable Option (same question when the Criterion names one, inside the Criterion's option_refs when present, and not ruled_out, withdrawn or superseded) have no non-retracted Comparison for the pair.",
        inspected_fields: &[
            "Criterion kind, lifecycle, question_ref, scope.question_ref and option_refs",
            "Option question_ref and lifecycle",
            "option_ref, criterion_ref and retracted of each Comparison",
        ],
        suggested_operation: Some("comparison.add"),
        persistence: "Stops when a Comparison covers the pair, or the Option or Criterion leaves scope.",
    },
    RuleSpec {
        rule_id: "decision.hard_criterion_cell_unknown",
        subject_kinds: &[Kind::Comparison],
        status: GapStatus::RequiresSource,
        condition: "A non-retracted Comparison whose dependencies are current refers to a hard Criterion and records outcome unknown.",
        inspected_fields: &["retracted", "criterion_ref", "Criterion kind", "outcome"],
        suggested_operation: Some("comparison.revise"),
        persistence: "Stops when the Comparison records a known outcome or is retracted. An unknown outcome is never read as a pass or a fail.",
    },
    RuleSpec {
        rule_id: "decision.option_criterion_cell_missing",
        subject_kinds: &[Kind::Option, Kind::Criterion],
        status: GapStatus::RequiresSource,
        condition: "An active Criterion that is not hard and an applicable Option (same question when the Criterion names one, inside the Criterion's option_refs when present, and not ruled_out, withdrawn or superseded) have no non-retracted Comparison for the pair.",
        inspected_fields: &[
            "Criterion kind, lifecycle, question_ref, scope.question_ref and option_refs",
            "Option question_ref and lifecycle",
            "option_ref, criterion_ref and retracted of each Comparison",
        ],
        suggested_operation: Some("comparison.add"),
        persistence: "Stops when a Comparison covers the pair, or the Option or Criterion leaves scope.",
    },
    RuleSpec {
        rule_id: "freshness.needs_review",
        subject_kinds: &[
            Kind::Assessment,
            Kind::Comparison,
            Kind::Conclusion,
            Kind::Decision,
        ],
        status: GapStatus::StaleDueToRevision,
        condition: "A live Assessment, Comparison, Conclusion or Decision has effective freshness needs_review in the dependency index: a record it relies on, directly or transitively, was withdrawn, retracted, retired, rejected or superseded, was materially revised, became invalid or less valid, or was newly judged doubtful, or a reviewed membership or closure changed. Lifecycle-only and benign judgment revisions do not emit it.",
        inspected_fields: &[
            "every exact revision reference in the record body and the records it cites",
            "substantive basis of each cited record",
            "withdrawals, source_assessment and effective measurement validity of cited records",
            "validation review memberships and read_set",
        ],
        suggested_operation: None,
        persistence: "Stops when the record is replaced, retracted or superseded, or the change it relies on is reverted; the reason lists each cause and the dependency path to it.",
    },
    RuleSpec {
        rule_id: "hypothesis.no_assessment",
        subject_kinds: &[Kind::Hypothesis],
        status: GapStatus::RequiresSource,
        condition: "A live Hypothesis (not retired, rejected, retracted or superseded) is not the exact subject_ref revision of any Assessment.",
        inspected_fields: &["lifecycle", "status", "subject_ref of each Assessment"],
        suggested_operation: Some("assessment.add"),
        persistence: "Stops when an Assessment names the current Hypothesis revision or the Hypothesis is no longer live. Amending the Hypothesis reopens it.",
    },
    RuleSpec {
        rule_id: "inquiry.confirmatory_plan_unfrozen",
        subject_kinds: &[Kind::Inquiry],
        status: GapStatus::ReadyToAnalyze,
        condition: "An Inquiry in confirmatory mode has a lifecycle other than frozen or retired; a missing lifecycle reads as draft.",
        inspected_fields: &["inquiry.mode", "lifecycle"],
        suggested_operation: Some("inquiry.freeze"),
        persistence: "Stops when the Inquiry is frozen or retired.",
    },
    RuleSpec {
        rule_id: "observation.invalid_or_missing_source_match",
        subject_kinds: &[Kind::Observation],
        status: GapStatus::BlockedByQuality,
        condition: "A non-retracted Observation has effective measurement validity invalid, or its basis selector validation or text quote state is not_found.",
        inspected_fields: &[
            "status, retracted and lifecycle",
            "effective measurement validity",
            "basis.selector.validation",
            "basis.text_quote.state",
        ],
        suggested_operation: Some("observation.correct"),
        persistence: "Stops when the Observation is retracted, or its effective validity is no longer invalid and neither source match is not_found.",
    },
    RuleSpec {
        rule_id: "observation.validity_context_missing",
        subject_kinds: &[Kind::Observation],
        status: GapStatus::BlockedByQuality,
        condition: "A non-retracted Observation not flagged by observation.invalid_or_missing_source_match has unknown effective measurement validity or no measurement_assessment.",
        inspected_fields: &[
            "status, retracted and lifecycle",
            "effective measurement validity",
            "measurement_assessment",
        ],
        suggested_operation: Some("measurement.assess"),
        persistence: "Stops when the Observation has a measurement_assessment and a known effective validity, or it is retracted.",
    },
    RuleSpec {
        rule_id: "review.stale_read_set",
        subject_kinds: &[Kind::Review],
        status: GapStatus::StaleDueToRevision,
        condition: "A Review's read_set names a revision older than the current revision of that record, and the change since is not benign under the freshness index's predicate (lifecycle metadata, a concern-free Source judgment or a measurement judgment that keeps effective validity are benign).",
        inspected_fields: &["read_set"],
        suggested_operation: Some("review.submit"),
        persistence: "Emitted while this Review's read_set names revisions with non-benign successors; a fresh Review over current revisions does not carry it.",
    },
    RuleSpec {
        rule_id: "run.outcome_incomplete_or_unknown",
        subject_kinds: &[Kind::Run],
        status: GapStatus::RequiresExternalAction,
        condition: "A Run has execution_status declared, running, unknown or absent, or evidence_completeness partial, missing, unknown or absent.",
        inspected_fields: &["execution_status", "evidence_completeness"],
        suggested_operation: Some("run.reconcile"),
        persistence: "Stops when the Run records a terminal status with complete evidence. A process status is never read as evidence validity.",
    },
];

/// What one rule inspects and when it emits a gap.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GapRuleDescription {
    pub rule_id: String,
    pub rule_version: String,
    /// Record kind prefixes of the subjects a gap from this rule covers.
    pub subject_kinds: Vec<String>,
    pub emitted_status: GapStatus,
    pub condition: String,
    pub inspected_fields: Vec<String>,
    pub suggested_operation: Option<String>,
    /// When the rule stops emitting the gap.
    pub persistence: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GapSubjectState {
    pub id: Id,
    /// The subject's revision at the snapshot, or null when the case has no such record.
    pub current_revision: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GapExplanation {
    pub schema_version: String,
    pub case_id: Id,
    pub snapshot: Snapshot,
    pub rule_version: String,
    pub gap_id: String,
    /// Whether the rule emits this gap at `snapshot`.
    pub emitted: bool,
    pub rule: GapRuleDescription,
    pub subjects: Vec<GapSubjectState>,
    /// The emitted gap, when `emitted` is true.
    pub gap: Option<Gap>,
    pub limits: Vec<String>,
}

fn describe(spec: &RuleSpec) -> GapRuleDescription {
    GapRuleDescription {
        rule_id: spec.rule_id.into(),
        rule_version: GAP_RULE_VERSION.into(),
        subject_kinds: spec
            .subject_kinds
            .iter()
            .map(|kind| kind.prefix().to_owned())
            .collect(),
        emitted_status: spec.status,
        condition: spec.condition.into(),
        inspected_fields: spec
            .inspected_fields
            .iter()
            .map(|field| (*field).to_owned())
            .collect(),
        suggested_operation: spec.suggested_operation.map(str::to_owned),
        persistence: spec.persistence.into(),
    }
}

/// Every rule of the current gap rule version, in rule ID order.
pub fn gap_rules() -> Vec<GapRuleDescription> {
    RULES.iter().map(describe).collect()
}

pub fn gap_rule(rule_id: &str) -> Option<GapRuleDescription> {
    RULES
        .iter()
        .find(|spec| spec.rule_id == rule_id)
        .map(describe)
}

pub fn gap_rule_ids() -> impl Iterator<Item = &'static str> {
    RULES.iter().map(|spec| spec.rule_id)
}

fn unknown_rule(rule_id: &str, path: &str) -> Failure {
    let mut failure = Failure::new(
        ErrorCode::UnknownReference,
        format!(
            "Gap rule version {GAP_RULE_VERSION} has no rule {rule_id:?}. A rule from another version is not evidence about this case."
        ),
    )
    .at(path);
    failure.error.expected = Some(json!(gap_rule_ids().collect::<Vec<_>>()));
    failure.error.actual = Some(json!(rule_id));
    failure
}

/// Reject a rule ID that the current rule version does not define.
pub fn require_rule(rule_id: &str, path: &str) -> Result<(), Failure> {
    if RULES.iter().any(|spec| spec.rule_id == rule_id) {
        Ok(())
    } else {
        Err(unknown_rule(rule_id, path))
    }
}

/// Split a gap ID into its rule ID and subject IDs. The spelling must be the
/// canonical one the rule engine emits: `gap:RULE_ID:ID[+ID...]` with subject
/// IDs ascending and distinct.
pub fn parse_gap_id(gap_id: &str) -> Result<(String, Vec<Id>), Failure> {
    let malformed = || {
        let mut failure = Failure::new(
            ErrorCode::InvalidInput,
            "Supply a gap ID as emitted by frontier or next: gap:RULE_ID:SUBJECT_ID[+SUBJECT_ID...].",
        )
        .at("/gap_id");
        failure.error.actual = Some(json!(gap_id));
        failure
    };
    let rest = gap_id.strip_prefix("gap:").ok_or_else(malformed)?;
    let (rule_id, subjects) = rest.split_once(':').ok_or_else(malformed)?;
    if rule_id.is_empty() || subjects.is_empty() {
        return Err(malformed());
    }
    let ids = subjects
        .split('+')
        .map(str::parse::<Id>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| malformed())?;
    let mut canonical = ids.clone();
    canonical.sort();
    canonical.dedup();
    if canonical != ids {
        let mut failure = malformed();
        failure.error.message = "The gap ID lists its subject IDs out of canonical order or more than once; use the spelling frontier emits.".into();
        failure.error.expected = Some(json!(format!(
            "gap:{rule_id}:{}",
            canonical
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("+")
        )));
        return Err(failure);
    }
    Ok((rule_id.to_owned(), ids))
}

/// Explain `gap_id` against an inventory enumerated from `state`. The rule is
/// named for any well-formed gap ID of the current rule version, whether or
/// not the inventory emits it.
pub fn explain_gap(
    state: &State,
    inventory: &GapInventory,
    gap_id: &str,
) -> Result<GapExplanation, Failure> {
    let (rule_id, subject_ids) = parse_gap_id(gap_id)?;
    let rule = gap_rule(&rule_id).ok_or_else(|| unknown_rule(&rule_id, "/gap_id"))?;
    let gap = inventory
        .gaps
        .iter()
        .find(|gap| gap.gap_id == gap_id)
        .cloned();
    let subjects = subject_ids
        .iter()
        .map(|id| GapSubjectState {
            id: *id,
            current_revision: state
                .current(*id)
                .filter(|record| record.header.case_id == inventory.case_id)
                .map(|record| record.reference().revision),
        })
        .collect();
    let mut limits = vec![
        "The description reports which committed fields the rule reads; it is not evidence and does not judge whether the suggested operation is a good method.".to_owned(),
        "A gap is identified by its rule and subject IDs, so a changed rule version can rename or retire a gap without any new evidence.".to_owned(),
    ];
    if gap.is_none() {
        limits.push(
            "The rule does not emit this gap at this snapshot: its condition does not hold for these subjects, or a subject is absent from the case. Absence is not proof that the underlying question is settled.".into(),
        );
    }
    Ok(GapExplanation {
        schema_version: GAP_EXPLANATION_VERSION.into(),
        case_id: inventory.case_id,
        snapshot: inventory.snapshot.clone(),
        rule_version: inventory.rule_version.clone(),
        gap_id: gap_id.to_owned(),
        emitted: gap.is_some(),
        rule,
        subjects,
        gap,
        limits,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// Rule IDs spelled in the rule engine: literals on a `rule_id:` line, or a
    /// bare literal on its own line (the hard/soft cell branch).
    fn engine_rule_ids() -> BTreeSet<String> {
        let source = include_str!("gap_rules.rs");
        let engine = source
            .split("#[cfg(test)]")
            .next()
            .expect("rule engine source");
        let mut ids = BTreeSet::new();
        for line in engine.lines() {
            let trimmed = line.trim().trim_end_matches(',');
            let literal = if let Some(rest) = trimmed.strip_prefix("rule_id: ") {
                rest.trim_end_matches(',')
            } else if trimmed.starts_with('"') && trimmed.ends_with('"') {
                trimmed
            } else {
                continue;
            };
            let Some(text) = literal
                .strip_prefix('"')
                .and_then(|text| text.strip_suffix('"'))
            else {
                continue;
            };
            let shaped = text.matches('.').count() == 1
                && !text.ends_with('.')
                && !text.ends_with('_')
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || matches!(byte, b'.' | b'_'));
            if shaped {
                ids.insert(text.to_owned());
            }
        }
        ids
    }

    #[test]
    fn the_rule_table_matches_every_rule_the_engine_emits() {
        let table: BTreeSet<String> = gap_rule_ids().map(str::to_owned).collect();
        assert_eq!(table.len(), RULES.len(), "rule IDs are unique");
        assert_eq!(engine_rule_ids(), table);
        let ordered: Vec<_> = gap_rule_ids().collect();
        let mut sorted = ordered.clone();
        sorted.sort_unstable();
        assert_eq!(ordered, sorted, "the table is in rule ID order");
        for rule in gap_rules() {
            assert_eq!(rule.rule_version, GAP_RULE_VERSION);
            assert!(!rule.subject_kinds.is_empty());
            assert!(!rule.condition.is_empty() && !rule.persistence.is_empty());
            assert!(!rule.inspected_fields.is_empty());
        }
    }

    #[test]
    fn gap_ids_parse_only_in_their_canonical_spelling() {
        let low = "hyp_019acd16-0000-7000-8000-000000000001";
        let high = "hyp_019acd16-0000-7000-8000-000000000002";
        let (rule, ids) = parse_gap_id(&format!("gap:hypothesis.no_assessment:{low}")).unwrap();
        assert_eq!(rule, "hypothesis.no_assessment");
        assert_eq!(ids.len(), 1);
        assert_eq!(
            parse_gap_id(&format!("gap:x.y:{low}+{high}"))
                .unwrap()
                .1
                .len(),
            2
        );
        let reversed = parse_gap_id(&format!("gap:x.y:{high}+{low}")).unwrap_err();
        assert_eq!(reversed.error.code, ErrorCode::InvalidInput);
        assert_eq!(
            reversed.error.expected,
            Some(json!(format!("gap:x.y:{low}+{high}")))
        );
        for malformed in [
            "",
            "gap:",
            "gap:rule",
            "gap::hyp_019acd16-0000-7000-8000-000000000001",
            "gap:rule:",
            "gap:rule:not-an-id",
            "hypothesis.no_assessment:hyp_019acd16-0000-7000-8000-000000000001",
        ] {
            let failure = parse_gap_id(malformed).unwrap_err();
            assert_eq!(
                failure.error.path.as_deref(),
                Some("/gap_id"),
                "{malformed}"
            );
        }
    }

    #[test]
    fn unknown_rules_are_rejected_with_the_known_list() {
        let failure = require_rule("hypothesis.no_such_rule", "--rule").unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::UnknownReference);
        assert!(failure.error.message.contains(GAP_RULE_VERSION));
        assert_eq!(
            failure.error.expected.unwrap().as_array().unwrap().len(),
            RULES.len()
        );
        assert!(require_rule("review.stale_read_set", "--rule").is_ok());
    }
}
