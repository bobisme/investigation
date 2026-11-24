//! Checks of provenance and review quality (§8.5, §5.6, §14.2): quotes,
//! absence claims, claim origins, retention, invalidated sources and the
//! independence of the reviews behind a promoted record.
//!
//! Every condition reads recorded fields through the readers that own them,
//! and re-derives none of their rules:
//!
//! - quote state is the `text_quote.state` the store verified at acceptance;
//! - claim origins and dependence come from [`crate::origin_ops`]: an absent
//!   dependence judgment is `unknown`, never independent, and only recorded
//!   common provenance reports a finding;
//! - absence claims are read by [`crate::absence_gate`] and search Runs by
//!   [`crate::search_coverage`];
//! - retention is [`crate::external_ref::artifact_retention`];
//! - an invalidated source reaches a record through the freshness index, so
//!   a record accepted with a partial withdrawal or source concern already in
//!   effect acknowledges it (freshness rules version 4) and is not reported;
//! - review independence is [`crate::review_independence::review_entry`] and
//!   its dependence groups.
//!
//! What a promoted record "rests on" is its subject read set: the record and
//! every exact revision it cites, transitively through inherited dependencies.
//! No check uses a clock, reads record text into a finding, or proposes
//! evidence. Promoted records whose own freshness changed stay with
//! `promoted.basis_not_current`; `source.invalidated_basis_unreviewed` and
//! `review.source_invalidated` cover the records that are not promoted.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::{
    absence_gate::{AbsenceBasis, evaluate_absence},
    command::RevisionRef,
    evidence::ArtifactAvailability,
    external_ref::artifact_retention,
    freshness::{Freshness, Trigger},
    identity::{Id, Kind},
    input,
    measurement_fields::cited_run,
    origin_ops::{Dependence, OriginDimension, OriginState, cited_origins},
    review_independence::{LowIndependence, dependence_group, review_entry},
    search_coverage::{run_coverage, states_bounds, states_empty_result},
    state::{Record, State},
    wire::Failure,
};

use super::{
    AuditContext, CheckSource, CheckSpec, Detection, FindingFact, Severity, is_promoted,
    subject_read_set,
};

/// Gate codes about the opportunity's search or adequacy standard. They are
/// reported by `source.absence_without_search_log`; the other unmet absence
/// requirements by `source.absence_basis_incomplete`.
const SEARCH_CODES: &[&str] = &[
    "absence.opportunity_missing",
    "absence.opportunity_refs_missing",
    "absence.adequacy_standard_missing",
];

/// Recorded dimensions that name a common upstream provenance. A shared
/// acquisition alone is not one: one document may carry a restatement and an
/// independent bench test.
const PROVENANCE_DIMENSIONS: &[OriginDimension] = &[
    OriginDimension::UpstreamSource,
    OriginDimension::CollectionRun,
    OriginDimension::DependenceGroup,
];

pub(super) const CHECKS: &[CheckSpec] = &[
    CheckSpec {
        check_id: "origin.shared_origin_citations",
        version: "v1",
        subject_kinds: &[
            Kind::Assessment,
            Kind::Comparison,
            Kind::Conclusion,
            Kind::Decision,
        ],
        reads: &[
            "evidence_refs, support_refs and contradiction_refs of each live Assessment, Comparison, Conclusion and Decision (Assessments with relation supports or contradicts)",
            "claim_origin traces and dependence judgments of each cited Observation and its origin root (origin_ops)",
            "acquisitions of each cited Observation through its derivation lineage",
        ],
        condition: "One evidence field cites two or more Observations that share recorded original provenance: they resolve to one known origin root, a current attributed judgment says their roots are shared or partly_shared, or their roots record the same upstream source, collection run or dependence group without an undisputed independence judgment. An absent dependence judgment is unknown, never independent; unknown pairs are counted in the facts and do not by themselves report a finding.",
        why: "Several citations are presented together but share one original provenance, so they are not independent corroboration.",
        severity: Severity::Material,
        severity_reason: "Counting restatements of one origin as separate support overstates how much independent evidence there is.",
        suggested_schema: Some("origin.judge_dependence"),
        guide: Some("independence"),
        source: CheckSource::Native(shared_origin_citations),
    },
    CheckSpec {
        check_id: "quote.promoted_basis_unverified",
        version: "v1",
        subject_kinds: &[Kind::Conclusion, Kind::Decision],
        reads: &[
            "Conclusion status and Decision lifecycle or status",
            "the subject read set of the promoted record",
            "value.kind, basis.kind and basis.text_quote.state of each Observation revision in it",
        ],
        condition: "A validated Conclusion or accepted Decision rests on a text Observation read from a source whose quote state is not verified: absent (not_applicable), ambiguous, unverified or unavailable.",
        why: "A promoted record rests on quoted text whose presence in the retained source was not established exactly once.",
        severity: Severity::Material,
        severity_reason: "An unchecked or ambiguous quote may not say what the promoted record relies on it for.",
        suggested_schema: Some("observation.correct"),
        guide: Some("source-quality"),
        source: CheckSource::Native(promoted_quote_unverified),
    },
    CheckSpec {
        check_id: "review.promoted_self_review_only",
        version: "v1",
        subject_kinds: &[Kind::Conclusion, Kind::Decision],
        reads: &[
            "Conclusion status and Decision lifecycle or status",
            "kind, status, subject_ref and context_isolation of each Review (review_independence.v1)",
        ],
        condition: "A validated Conclusion or accepted Decision has at least one submitted validation or blindspot Review, and every one declares context_isolation same_session_self_review.",
        why: "The only recorded challenge of a promoted record is its author rereading the work in the same session.",
        severity: Severity::Material,
        severity_reason: "Same-session self-review is low-independence: without new information it often fails to find errors.",
        suggested_schema: Some("review.submit"),
        guide: Some("independence"),
        source: CheckSource::Native(promoted_self_review_only),
    },
    CheckSpec {
        check_id: "review.promoted_single_dependence_group",
        version: "v1",
        subject_kinds: &[Kind::Conclusion, Kind::Decision],
        reads: &[
            "Conclusion status and Decision lifecycle or status",
            "kind, status and subject_ref of each Review",
            "recorded_by and the self-reported actor descriptor of each Review (review_independence.v1 dependence groups)",
        ],
        condition: "A validated Conclusion or accepted Decision has two or more submitted validation or blindspot Reviews and all of them fall in one dependence group: one self-reported model family, or one actor where no family is declared.",
        why: "Several reviews of a promoted record come from one dependence group, so they count as one judgment, not several.",
        severity: Severity::Advisory,
        severity_reason: "Review count is not independence; reviewers of one model family share errors.",
        suggested_schema: Some("review.submit"),
        guide: Some("independence"),
        source: CheckSource::Native(promoted_single_dependence_group),
    },
    CheckSpec {
        check_id: "review.source_invalidated",
        version: "v1",
        subject_kinds: &[Kind::Review],
        reads: &[
            "status and subject_ref of each Review",
            "effective freshness and its reasons (freshness rules v4)",
        ],
        condition: "A submitted Review whose subject is current has effective freshness needs_review or disputed because a record it read was withdrawn or retracted, a cited Source was newly judged doubtful, unreliable or outdated, or reverified bytes mismatched. A Review accepted with the change in effect acknowledges it and is not reported.",
        why: "A review read material that has since been withdrawn, retracted or put in doubt, and has not been redone.",
        severity: Severity::Material,
        severity_reason: "The review's judgment may not hold without the invalidated material.",
        suggested_schema: Some("review.submit"),
        guide: Some("review"),
        source: CheckSource::Native(review_source_invalidated),
    },
    CheckSpec {
        check_id: "source.absence_basis_incomplete",
        version: "v1",
        subject_kinds: &[Kind::Assessment, Kind::Comparison, Kind::Conclusion],
        reads: &[
            "absence block and evidence_refs of each live Assessment and Comparison (absence-gate.v1)",
            "claim_form and support_refs of each live Conclusion",
        ],
        condition: "An absence claim does not meet the absence gate for a reason other than its search or adequacy standard: no expected signal, an unusable or retracted opportunity record, a window or sample count below or without its stated minimum, or no valid instrument; or a Conclusion with claim_form absence cites no supporting record with an absence block. The facts name each unmet gate code.",
        why: "A claim that something is absent does not state what would have been seen, or rests on an opportunity that could not have shown it.",
        severity: Severity::Advisory,
        severity_reason: "The absence gate already reads the claim as inconclusive and holds promotion; empty results are not proof of absence.",
        suggested_schema: Some("assessment.add"),
        guide: Some("negative-result"),
        source: CheckSource::Native(absence_basis_incomplete),
    },
    CheckSpec {
        check_id: "source.absence_without_search_log",
        version: "v1",
        subject_kinds: &[Kind::Assessment, Kind::Comparison, Kind::Conclusion],
        reads: &[
            "absence block and evidence_refs of each live Assessment and Comparison",
            "claim_form and support_refs of each live Conclusion",
            "run_ref of each cited Observation and search_coverage of each linked Run",
        ],
        condition: "An absence claim has no linked search Run whose search_coverage records its queries, a bound (revision bounds, date range or collections) and an explicit empty result, and states no minimum observation window or sample count instead. Linked Runs are opportunity references and the Runs that cited Observations name. For a Conclusion with claim_form absence, no supporting record with an absence block has one.",
        why: "A claim that something does not exist has no recorded search behind it.",
        severity: Severity::Material,
        severity_reason: "Without the corpus, queries and empty results, nobody can tell what was looked at; nothing found is not nothing there.",
        suggested_schema: Some("run.record"),
        guide: Some("negative-result"),
        source: CheckSource::Native(absence_without_search_log),
    },
    CheckSpec {
        check_id: "source.claim_without_selector_or_attribution",
        version: "v1",
        subject_kinds: &[Kind::Conclusion, Kind::Decision],
        reads: &[
            "Conclusion status and Decision lifecycle or status",
            "the subject read set of the promoted record",
            "basis.kind and run_ref of each Observation revision in it",
        ],
        condition: "A validated Conclusion or accepted Decision rests on an Observation whose basis is an assertion (no retained or external source selector) and that names no Run, so no producer is attributed.",
        why: "A claimed fact a promoted record relies on has neither a source selector nor a producer attribution.",
        severity: Severity::Material,
        severity_reason: "Nobody can check where an unsourced, unattributed fact came from.",
        suggested_schema: Some("observation.add"),
        guide: Some("source-quality"),
        source: CheckSource::Native(claim_without_selector_or_attribution),
    },
    CheckSpec {
        check_id: "source.invalidated_basis_unreviewed",
        version: "v1",
        subject_kinds: &[
            Kind::Assessment,
            Kind::Comparison,
            Kind::Conclusion,
            Kind::Decision,
        ],
        reads: &[
            "status, lifecycle and retracted of each Assessment, Comparison, Conclusion and Decision",
            "effective freshness and its reasons (freshness rules v4)",
        ],
        condition: "A live Assessment or Comparison, or a Conclusion or Decision that is not promoted, has effective freshness needs_review or disputed because a record it rests on was withdrawn or retracted, a cited Source was newly judged doubtful, unreliable or outdated, or reverified bytes mismatched. A record accepted with the change in effect acknowledges it and is not reported. Promoted records are reported by promoted.basis_not_current.",
        why: "A judgment still rests on a source that has since been withdrawn, retracted or put in doubt.",
        severity: Severity::Material,
        severity_reason: "The judgment may no longer follow from its evidence until it is reassessed; the earlier judgment stays on record.",
        suggested_schema: Some("assessment.supersede"),
        guide: Some("source-quality"),
        source: CheckSource::Native(invalidated_basis_unreviewed),
    },
    CheckSpec {
        check_id: "source.reference_only_unverified",
        version: "v1",
        subject_kinds: &[Kind::Conclusion, Kind::Decision],
        reads: &[
            "Conclusion status and Decision lifecycle or status",
            "the subject read set of the promoted record",
            "availability and external verifications of the current revision of each Artifact in it, and of each Artifact a cited Observation reads",
        ],
        condition: "A validated Conclusion or accepted Decision rests on a reference-only Artifact (availability external, retention source_unavailable) with no recorded reverification of its bytes.",
        why: "A promoted record rests on bytes that are not retained here and were never checked against their recorded digest.",
        severity: Severity::Material,
        severity_reason: "The cited bytes cannot be read or verified offline; they may differ from what was described.",
        suggested_schema: None,
        guide: Some("source-quality"),
        source: CheckSource::Native(reference_only_unverified),
    },
];

fn fact(fact: impl Into<String>, refs: Vec<RevisionRef>) -> FindingFact {
    FindingFact {
        fact: fact.into(),
        refs,
    }
}

fn token(value: impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn refs(value: &Value) -> Vec<RevisionRef> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| input::decode::<RevisionRef>(item).ok())
        .collect()
}

/// Retracted, withdrawn, superseded, retired or rejected: out of evidentiary use.
fn ended(record: &Record) -> bool {
    let body = &record.body;
    let ended = |field: &str| {
        matches!(
            body[field].as_str(),
            Some(
                "retracted"
                    | "withdrawn"
                    | "superseded"
                    | "retired"
                    | "rejected"
                    | "rejected_within_scope"
            )
        )
    };
    body["retracted"] == true || ended("status") || ended("lifecycle")
}

/// The exact revisions a promoted record rests on, as records: the cited
/// revision where it exists, else the current one.
fn rests_on<'a>(state: &'a State, record: &Record) -> Vec<&'a Record> {
    subject_read_set(state, record)
        .into_iter()
        .filter(|reference| reference.id != record.header.id)
        .filter_map(|reference| {
            state
                .revision(reference)
                .or_else(|| state.current(reference.id))
        })
        .filter(|found| found.header.case_id == record.header.case_id)
        .collect()
}

/// Group revisions by fact token, in token order.
fn grouped(found: BTreeMap<String, Vec<RevisionRef>>) -> Vec<FindingFact> {
    found
        .into_iter()
        .map(|(name, refs)| fact(name, refs))
        .collect()
}

fn shared_origin_citations(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let mut kinds = Vec::new();
    for kind in [
        Kind::Assessment,
        Kind::Comparison,
        Kind::Conclusion,
        Kind::Decision,
    ] {
        kinds.extend(context.current(kind));
    }
    let mut detections = Vec::new();
    for record in kinds {
        if ended(record) {
            continue;
        }
        if record.header.id.kind() == Kind::Assessment
            && !matches!(
                record.body["relation"].as_str(),
                Some("supports" | "contradicts")
            )
        {
            continue;
        }
        let mut detection = Detection::default();
        for field in cited_origins(context.state, record) {
            let summary = &field.summary;
            let mut facts = Vec::new();
            for group in &summary.origins {
                if group.state == OriginState::Known && group.observations.len() >= 2 {
                    let mut cited = group.observations.clone();
                    cited.extend(group.root);
                    facts.push(fact("shared_origin:one_known_root", cited));
                    facts.push(fact(
                        format!("acquisitions_of_one_origin:{}", group.acquisitions.len()),
                        group.observations.clone(),
                    ));
                }
            }
            let members = |index: usize| -> Vec<RevisionRef> {
                let group = &summary.origins[index];
                let mut members = group.observations.clone();
                members.extend(group.root);
                members
            };
            for pair in &summary.pairs {
                let mut both = members(pair.origins[0]);
                both.extend(members(pair.origins[1]));
                if matches!(
                    pair.dependence,
                    Dependence::Shared | Dependence::PartlyShared
                ) {
                    facts.push(fact(
                        format!("dependence_judged:{}", token(pair.dependence)),
                        both.clone(),
                    ));
                }
                if !pair.independent_corroboration {
                    for dimension in &pair.recorded_shared {
                        if PROVENANCE_DIMENSIONS.contains(dimension) {
                            facts.push(fact(
                                format!("recorded_shared:{}", token(dimension)),
                                both.clone(),
                            ));
                        }
                    }
                }
                for conflict in &pair.conflicts {
                    facts.push(fact(
                        format!("independence_conflicts_recorded:{}", token(conflict)),
                        both.clone(),
                    ));
                }
            }
            if facts.is_empty() {
                continue;
            }
            let cited: Vec<RevisionRef> = summary
                .origins
                .iter()
                .flat_map(|group| group.observations.iter().chain(&group.root))
                .copied()
                .collect();
            facts.push(fact(format!("cited_field:{}", field.field), cited.clone()));
            facts.push(fact(
                format!("known_origin_count:{}", summary.known_origin_count),
                Vec::new(),
            ));
            facts.push(fact(
                format!("acquisition_count:{}", summary.acquisition_count),
                Vec::new(),
            ));
            facts.push(fact(
                format!("independent_pairs:{}", summary.independent_pairs),
                Vec::new(),
            ));
            if summary.unknown_pairs > 0 {
                facts.push(fact(
                    format!("dependence_unknown_pairs:{}", summary.unknown_pairs),
                    Vec::new(),
                ));
            }
            detection.related_refs.extend(cited);
            detection.facts.extend(facts);
        }
        if !detection.facts.is_empty() {
            detection.subject_refs = vec![record.reference()];
            detections.push(detection);
        }
    }
    Ok(detections)
}

/// Promoted records and what each rests on, with one detection per record
/// whose basis yields facts.
fn over_promoted(
    context: &AuditContext<'_>,
    mut facts_of: impl FnMut(&Record) -> Option<(String, Vec<RevisionRef>)>,
) -> Vec<Detection> {
    let mut detections = Vec::new();
    for record in context.promoted() {
        let mut found = BTreeMap::<String, Vec<RevisionRef>>::new();
        for basis in rests_on(context.state, record) {
            if let Some((name, refs)) = facts_of(basis) {
                found.entry(name).or_default().extend(refs);
            }
        }
        if found.is_empty() {
            continue;
        }
        let related = found.values().flatten().copied().collect();
        detections.push(Detection {
            subject_refs: vec![record.reference()],
            related_refs: related,
            facts: grouped(found),
        });
    }
    detections
}

fn promoted_quote_unverified(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    Ok(over_promoted(context, |basis| {
        let body = &basis.body;
        if basis.header.id.kind() != Kind::Observation
            || body["value"]["kind"] != "text"
            || body["basis"]["kind"] != "source"
        {
            return None;
        }
        let state = match body["basis"]["text_quote"]["state"].as_str() {
            Some("verified") => return None,
            Some("not_applicable") | None => "absent",
            Some(other) => other,
        };
        Some((format!("text_quote:{state}"), vec![basis.reference()]))
    }))
}

fn claim_without_selector_or_attribution(
    context: &AuditContext<'_>,
) -> Result<Vec<Detection>, Failure> {
    Ok(over_promoted(context, |basis| {
        let body = &basis.body;
        (basis.header.id.kind() == Kind::Observation
            && body["basis"]["kind"] == "assertion"
            && body["run_ref"].is_null())
        .then(|| {
            (
                "observation_basis:assertion_without_run".to_owned(),
                vec![basis.reference()],
            )
        })
    }))
}

fn reference_only_unverified(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let state = context.state;
    Ok(over_promoted(context, |basis| {
        let artifact = match basis.header.id.kind() {
            Kind::Artifact => state.current(basis.header.id),
            Kind::Observation => input::decode::<RevisionRef>(&basis.body["basis"]["artifact_ref"])
                .ok()
                .and_then(|cited| state.current(cited.id)),
            _ => None,
        }
        .filter(|artifact| artifact.header.case_id == basis.header.case_id)?;
        let retention = artifact_retention(artifact)?;
        (retention.availability == ArtifactAvailability::External
            && retention.verification_count == 0)
            .then(|| {
                let mut refs = vec![retention.artifact_ref];
                if basis.header.id.kind() == Kind::Observation {
                    refs.push(basis.reference());
                }
                ("retention:external_unverified".to_owned(), refs)
            })
    }))
}

/// Submitted validation or blindspot Reviews of `subject`, in ID order.
fn reviews_of<'a>(context: &'a AuditContext<'_>, subject: Id) -> Vec<&'a Record> {
    context
        .current(Kind::Review)
        .into_iter()
        .filter(|review| {
            review.body["status"] == "submitted"
                && matches!(
                    review.body["kind"].as_str(),
                    Some("validation" | "blindspot")
                )
                && input::decode::<RevisionRef>(&review.body["subject_ref"])
                    .is_ok_and(|reference| reference.id == subject)
        })
        .collect()
}

fn promoted_self_review_only(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let mut detections = Vec::new();
    for record in context.promoted() {
        let reviews = reviews_of(context, record.header.id);
        if reviews.is_empty()
            || !reviews.iter().all(|review| {
                review_entry(context.state, review).is_some_and(|entry| {
                    entry
                        .low_independence_reasons
                        .contains(&LowIndependence::SameSessionSelfReview)
                })
            })
        {
            continue;
        }
        let refs: Vec<RevisionRef> = reviews.iter().map(|review| review.reference()).collect();
        detections.push(Detection {
            subject_refs: vec![record.reference()],
            related_refs: refs.clone(),
            facts: vec![
                fact("low_independence:same_session_self_review", refs),
                fact(format!("review_count:{}", reviews.len()), Vec::new()),
            ],
        });
    }
    Ok(detections)
}

fn promoted_single_dependence_group(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let mut detections = Vec::new();
    for record in context.promoted() {
        let reviews = reviews_of(context, record.header.id);
        if reviews.len() < 2 {
            continue;
        }
        let groups: BTreeMap<String, _> = reviews
            .iter()
            .map(|review| {
                dependence_group(
                    &review.header.recorded_by,
                    review.header.actor_descriptor_self_reported.as_ref(),
                )
            })
            .collect();
        let Some((_, basis)) = groups.iter().next().filter(|_| groups.len() == 1) else {
            continue;
        };
        let refs: Vec<RevisionRef> = reviews.iter().map(|review| review.reference()).collect();
        detections.push(Detection {
            subject_refs: vec![record.reference()],
            related_refs: refs.clone(),
            facts: vec![
                fact(format!("dependence_group_basis:{}", token(basis)), refs),
                fact(format!("review_count:{}", reviews.len()), Vec::new()),
            ],
        });
    }
    Ok(detections)
}

/// Freshness triggers that invalidate a cited source or claim.
fn source_trigger(trigger: &Trigger) -> bool {
    matches!(
        trigger,
        Trigger::Withdrawn
            | Trigger::AvailabilityRemoved
            | Trigger::Retracted
            | Trigger::SourceConcern { .. }
            | Trigger::ExternalMismatch
    )
}

/// The source-level reasons a record is not current, as a detection.
fn invalidation(context: &AuditContext<'_>, record: &Record) -> Option<Detection> {
    let freshness = context.freshness.get(record.header.id)?;
    if freshness.freshness == Freshness::Current {
        return None;
    }
    let mut detection = Detection::default();
    let mut found = BTreeMap::<String, Vec<RevisionRef>>::new();
    for reason in &freshness.reasons {
        if !source_trigger(&reason.trigger) {
            continue;
        }
        detection.related_refs.push(reason.cause);
        detection
            .related_refs
            .extend(reason.path.iter().map(|step| step.cites));
        found
            .entry(format!(
                "freshness_trigger:{}",
                serde_json::to_value(&reason.trigger)
                    .ok()
                    .and_then(|value| value["kind"].as_str().map(str::to_owned))
                    .unwrap_or_default()
            ))
            .or_default()
            .push(reason.cause);
    }
    if found.is_empty() {
        return None;
    }
    detection.facts = grouped(found);
    detection.facts.push(fact(
        format!("freshness:{}", freshness.freshness.as_str()),
        vec![freshness.record],
    ));
    detection.subject_refs = vec![record.reference()];
    Some(detection)
}

fn invalidated_basis_unreviewed(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let mut detections = Vec::new();
    for kind in [
        Kind::Assessment,
        Kind::Comparison,
        Kind::Conclusion,
        Kind::Decision,
    ] {
        for record in context.current(kind) {
            if ended(record) || is_promoted(record) {
                continue;
            }
            detections.extend(invalidation(context, record));
        }
    }
    Ok(detections)
}

fn review_source_invalidated(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    Ok(context
        .current(Kind::Review)
        .into_iter()
        .filter(|review| review.body["status"] == "submitted")
        .filter(|review| {
            input::decode::<RevisionRef>(&review.body["subject_ref"])
                .ok()
                .and_then(|subject| context.state.current(subject.id))
                .is_some_and(|subject| !ended(subject))
        })
        .filter_map(|review| invalidation(context, review))
        .collect())
}

/// One record that carries an absence block, read through the absence gate.
struct AbsenceRecord {
    reference: RevisionRef,
    /// Linked search Runs whose coverage states bounds and an empty result.
    searched: Vec<RevisionRef>,
    /// A minimum observation window or sample count is stated instead.
    sampled: bool,
    /// Unmet gate codes with their records.
    missing: Vec<(String, Vec<RevisionRef>)>,
}

fn absence_record(state: &State, record: &Record) -> Option<AbsenceRecord> {
    let block = record
        .body
        .get("absence")
        .filter(|block| !block.is_null())?;
    let basis: AbsenceBasis = input::decode(block).ok()?;
    let evidence = refs(&record.body["evidence_refs"]);
    let opportunity = refs(&block["opportunity"]["refs"]);
    let mut searched = Vec::new();
    for reference in opportunity.iter().chain(&evidence) {
        let Some(cited) = state
            .revision(*reference)
            .filter(|cited| cited.header.case_id == record.header.case_id)
        else {
            continue;
        };
        let run = match cited.header.id.kind() {
            Kind::Run => Some(cited),
            Kind::Observation => cited_run(state, cited),
            _ => None,
        };
        if let Some(run) = run
            && run_coverage(run)
                .is_some_and(|coverage| states_bounds(&coverage) && states_empty_result(&coverage))
        {
            searched.push(run.reference());
        }
    }
    let sampled = basis.opportunity.as_ref().is_some_and(|opportunity| {
        opportunity.minimum_observation_window.is_some()
            || opportunity.minimum_sample_count.is_some()
    });
    let evaluation = evaluate_absence(state, record.header.case_id, &basis, &evidence);
    Some(AbsenceRecord {
        reference: record.reference(),
        searched,
        sampled,
        missing: evaluation
            .missing
            .into_iter()
            .map(|item| (item.code, item.refs))
            .collect(),
    })
}

/// Absence claims of the case: live Assessments and Comparisons with an
/// absence block, and live Conclusions declaring `claim_form: absence` with
/// the absence-bearing records among their direct support.
enum AbsenceClaim<'a> {
    Block(&'a Record, AbsenceRecord),
    Conclusion(&'a Record, Vec<AbsenceRecord>),
}

fn absence_claims<'a>(context: &'a AuditContext<'_>) -> Vec<AbsenceClaim<'a>> {
    let state = context.state;
    let mut claims = Vec::new();
    for kind in [Kind::Assessment, Kind::Comparison] {
        for record in context.current(kind) {
            if ended(record) {
                continue;
            }
            if let Some(absence) = absence_record(state, record) {
                claims.push(AbsenceClaim::Block(record, absence));
            }
        }
    }
    for record in context.current(Kind::Conclusion) {
        if ended(record) || record.body["claim_form"] != "absence" {
            continue;
        }
        let support = refs(&record.body["support_refs"])
            .into_iter()
            .filter_map(|reference| state.revision(reference))
            .filter(|cited| cited.header.case_id == record.header.case_id)
            .filter_map(|cited| absence_record(state, cited))
            .collect();
        claims.push(AbsenceClaim::Conclusion(record, support));
    }
    claims
}

fn absence_without_search_log(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let unsearched = |absence: &AbsenceRecord| {
        absence.searched.is_empty()
            && (!absence.sampled
                || absence
                    .missing
                    .iter()
                    .any(|(code, _)| SEARCH_CODES.contains(&code.as_str())))
    };
    let mut detections = Vec::new();
    for claim in absence_claims(context) {
        match claim {
            AbsenceClaim::Block(record, absence) if unsearched(&absence) => {
                let mut facts = vec![fact("search_log:none_linked", vec![absence.reference])];
                facts.extend(
                    absence
                        .missing
                        .iter()
                        .filter(|(code, _)| SEARCH_CODES.contains(&code.as_str()))
                        .map(|(code, refs)| fact(format!("absence_gate:{code}"), refs.clone())),
                );
                detections.push(Detection {
                    subject_refs: vec![record.reference()],
                    related_refs: Vec::new(),
                    facts,
                });
            }
            // A Conclusion with no absence basis at all is reported by
            // source.absence_basis_incomplete.
            AbsenceClaim::Conclusion(record, support)
                if !support.is_empty() && support.iter().all(unsearched) =>
            {
                let related: Vec<RevisionRef> =
                    support.iter().map(|absence| absence.reference).collect();
                detections.push(Detection {
                    subject_refs: vec![record.reference()],
                    related_refs: related.clone(),
                    facts: vec![fact("search_log:none_linked", related)],
                });
            }
            _ => {}
        }
    }
    Ok(detections)
}

fn absence_basis_incomplete(context: &AuditContext<'_>) -> Result<Vec<Detection>, Failure> {
    let other_codes = |absence: &AbsenceRecord| -> Vec<FindingFact> {
        let mut found = BTreeMap::<String, Vec<RevisionRef>>::new();
        for (code, refs) in &absence.missing {
            if !SEARCH_CODES.contains(&code.as_str()) {
                found
                    .entry(format!("absence_gate:{code}"))
                    .or_default()
                    .extend(refs.iter().copied().chain([absence.reference]));
            }
        }
        grouped(found)
    };
    let mut detections = Vec::new();
    for claim in absence_claims(context) {
        match claim {
            AbsenceClaim::Block(record, absence) => {
                let facts = other_codes(&absence);
                if !facts.is_empty() {
                    detections.push(Detection {
                        subject_refs: vec![record.reference()],
                        related_refs: facts.iter().flat_map(|f| f.refs.clone()).collect(),
                        facts,
                    });
                }
            }
            AbsenceClaim::Conclusion(record, support) => {
                if support.iter().any(|absence| absence.missing.is_empty()) {
                    continue;
                }
                let facts: Vec<FindingFact> = if support.is_empty() {
                    vec![fact("absence_gate:absence.claim_without_basis", Vec::new())]
                } else {
                    support.iter().flat_map(other_codes).collect()
                };
                if !facts.is_empty() {
                    detections.push(Detection {
                        subject_refs: vec![record.reference()],
                        related_refs: facts.iter().flat_map(|f| f.refs.clone()).collect(),
                        facts,
                    });
                }
            }
        }
    }
    Ok(detections)
}

#[cfg(test)]
mod tests;
