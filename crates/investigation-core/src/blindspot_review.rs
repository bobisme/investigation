//! The blindspot review contract: `review.submit` with `kind: blindspot`.
//!
//! A blindspot review is the recorded form of a semantic challenge. It states
//! exactly what was looked at: the subject, the verified snapshot inspected, the
//! scope, the method and the read-set. It can also name the structural audit
//! (`inv.audit.v1`) it read, by check version and `check_set_digest`, and give
//! each audit gap it considered a disposition. Gaps the reviewer found outside
//! the registered checks are `novel_gaps`; the reviewer may record each one as a
//! Question, Assumption or Objection created earlier in the same transaction and
//! cited by `client_ref`.
//!
//! The review never validates, accepts or otherwise changes a Conclusion or
//! Decision, and no field can assert exhaustive coverage. It goes stale through
//! the freshness index when a record it read changes materially, or when the
//! registered check set differs from the one whose audit it reviewed. The same
//! predicate decides whether a pack policy counts it as a current review.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    blindspot::{AuditRequest, CHECK_VERSION, audit, check_set_digest, checks},
    command::{Reference, RevisionRef},
    freshness::{Freshness, FreshnessIndex},
    identity::{Id, Kind},
    review::BlindspotVerdict,
    state::{OperationContext, Record, State},
    wire::{Digest, ErrorCode, Failure, Snapshot},
};

/// Kinds a novel gap can be recorded as.
const GAP_RECORD_KINDS: &[Kind] = &[Kind::Question, Kind::Assumption, Kind::Objection];

/// The structural audit a blindspot review read. Copy the fields from the
/// `investigation blindspots` response; its snapshot is the review's
/// `audited_snapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewedAudit {
    /// The response's `check_version`.
    pub check_version: String,
    /// The response's `check_set_digest`.
    pub check_set_digest: Digest,
    /// The response's `scope.subject_ref` for an audit narrowed with
    /// `--subject`. Omit it for a whole-case audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_ref: Option<RevisionRef>,
}

/// What the reviewer concluded about one gap the audit reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GapDispositionKind {
    /// The gap is real and still open.
    Accepted,
    /// Recorded work answers the gap; `record_refs` names it.
    Addressed,
    /// The gap does not bear on the reviewed scope, for the stated reason.
    NotApplicable,
}

/// A disposition of one `gap_id` reported by the reviewed audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GapDisposition {
    pub gap_id: String,
    pub disposition: GapDispositionKind,
    pub reason: String,
    #[serde(default)]
    pub uncertainty: Option<String>,
    /// Current revisions the reviewer read for this gap; each is also in `read_set`.
    #[serde(default)]
    pub related_refs: Vec<RevisionRef>,
    /// Records that record or address the gap, including ones created earlier
    /// in the same transaction.
    #[serde(default)]
    pub record_refs: Vec<Reference>,
}

/// A gap the reviewer identified that no registered check reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NovelGap {
    pub statement: String,
    #[serde(default)]
    pub uncertainty: Option<String>,
    /// Current revisions the gap concerns; each is also in `read_set`.
    #[serde(default)]
    pub related_refs: Vec<RevisionRef>,
    /// The Question, Assumption or Objection that records this gap, typically
    /// created earlier in the same transaction and cited by `client_ref`.
    #[serde(default)]
    pub record_refs: Vec<Reference>,
}

/// The blindspot-specific parts of a payload, borrowed for validation.
pub(crate) struct Detail<'a> {
    pub verdict: BlindspotVerdict,
    pub gaps: &'a [String],
    pub no_gap_reason: Option<&'a str>,
    pub audit: Option<&'a ReviewedAudit>,
    pub gap_dispositions: &'a [GapDisposition],
    pub novel_gaps: &'a [NovelGap],
}

fn invalid(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(path)
}

fn gate(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::GateFailed, message).at(path)
}

fn nonblank(value: &str, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() {
        return Err(invalid(path, "Supply nonblank text."));
    }
    Ok(())
}

/// Payload checks that need no case state.
pub(crate) fn shape(detail: &Detail<'_>) -> Result<(), Failure> {
    if !detail.gap_dispositions.is_empty() && detail.audit.is_none() {
        return Err(invalid(
            "/audit",
            "Name the reviewed audit (check_version and check_set_digest) before giving its gaps a disposition.",
        ));
    }
    let mut seen = BTreeSet::new();
    for (index, disposition) in detail.gap_dispositions.iter().enumerate() {
        let path = format!("/gap_dispositions/{index}");
        nonblank(&disposition.gap_id, &format!("{path}/gap_id"))?;
        if !seen.insert(disposition.gap_id.as_str()) {
            return Err(invalid(
                &format!("{path}/gap_id"),
                "Give each audit gap one disposition.",
            ));
        }
        nonblank(&disposition.reason, &format!("{path}/reason"))?;
        if let Some(uncertainty) = &disposition.uncertainty {
            nonblank(uncertainty, &format!("{path}/uncertainty"))?;
        }
        if disposition.disposition == GapDispositionKind::Addressed
            && disposition.record_refs.is_empty()
        {
            return Err(invalid(
                &format!("{path}/record_refs"),
                "Cite the records that address the gap.",
            ));
        }
    }
    for (index, gap) in detail.novel_gaps.iter().enumerate() {
        let path = format!("/novel_gaps/{index}");
        nonblank(&gap.statement, &format!("{path}/statement"))?;
        if let Some(uncertainty) = &gap.uncertainty {
            nonblank(uncertainty, &format!("{path}/uncertainty"))?;
        }
    }
    let identified = !detail.gaps.is_empty()
        || !detail.novel_gaps.is_empty()
        || detail
            .gap_dispositions
            .iter()
            .any(|disposition| disposition.disposition == GapDispositionKind::Accepted);
    match detail.verdict {
        BlindspotVerdict::GapsIdentified if !identified => Err(invalid(
            "/gaps",
            "List an actual gap, or select a bounded verdict.",
        )),
        BlindspotVerdict::NoGapIdentifiedWithinScope if identified => Err(invalid(
            "/verdict",
            "A review that lists gaps, novel gaps or accepted audit gaps cannot report no gap.",
        )),
        BlindspotVerdict::NoGapIdentifiedWithinScope
            if detail
                .no_gap_reason
                .is_none_or(|reason| reason.trim().is_empty()) =>
        {
            Err(invalid(
                "/no_gap_reason",
                "Explain why no gap was identified within the stated scope.",
            ))
        }
        _ => Ok(()),
    }
}

/// The digest of the check set this build registers.
pub fn current_check_set_digest() -> Digest {
    check_set_digest(&checks())
}

/// Resolved blindspot-specific content, ready to be stored on the review.
pub(crate) struct Resolved {
    audit: Option<Value>,
    gap_dispositions: Vec<Value>,
    novel_gaps: Vec<Value>,
    /// Revisions the review must list in its read-set.
    pub read: Vec<RevisionRef>,
    warnings: Vec<String>,
}

fn related(
    ctx: &OperationContext<'_>,
    refs: &[RevisionRef],
    path: &str,
    read: &mut Vec<RevisionRef>,
) -> Result<Vec<RevisionRef>, Failure> {
    let mut seen = BTreeSet::new();
    for (index, reference) in refs.iter().enumerate() {
        let path = format!("{path}/{index}");
        let Some(current) = ctx.current(reference.id) else {
            return Err(
                Failure::new(ErrorCode::UnknownReference, "Cite a record of this case.").at(&path),
            );
        };
        if current.reference() != *reference {
            let mut conflict = Failure::new(
                ErrorCode::RevisionConflict,
                "The record changed. Read its current revision and include it in read_set.",
            )
            .at(&path);
            conflict.error.expected = Some(json!(current.reference()));
            return Err(conflict);
        }
        if !seen.insert(reference.id) {
            return Err(invalid(&path, "Supply one revision per record."));
        }
        read.push(*reference);
    }
    Ok(refs.to_vec())
}

fn records(
    ctx: &OperationContext<'_>,
    refs: &[Reference],
    kinds: Option<&[Kind]>,
    path: &str,
) -> Result<Vec<RevisionRef>, Failure> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (index, reference) in refs.iter().enumerate() {
        let path = format!("{path}/{index}");
        let resolved = match (kinds, reference) {
            (None, _) => ctx.resolve_reference(reference, &path)?,
            (Some(kinds), Reference::Existing(exact)) => {
                if !kinds.contains(&exact.id.kind()) {
                    return Err(invalid(
                        &path,
                        "Record a novel gap as a Question, Assumption or Objection.",
                    ));
                }
                ctx.resolve(reference, exact.id.kind(), &path)?
            }
            (Some(kinds), Reference::Client(_)) => kinds
                .iter()
                .find_map(|kind| ctx.resolve(reference, *kind, &path).ok())
                .ok_or_else(|| {
                    Failure::new(
                        ErrorCode::UnknownReference,
                        "Use a same-case Question, Assumption or Objection, or a preceding alias of one.",
                    )
                    .at(&path)
                })?,
        };
        if !seen.insert(resolved.id) {
            return Err(invalid(&path, "Supply one revision per record."));
        }
        out.push(resolved);
    }
    Ok(out)
}

/// Check the reviewed audit against the verified snapshot and re-run it there,
/// so a disposition can only name a gap that audit really reported.
fn reviewed_audit(
    ctx: &OperationContext<'_>,
    snapshot: &Snapshot,
    reviewed: &ReviewedAudit,
) -> Result<crate::blindspot::AuditResponse, Failure> {
    let digest = current_check_set_digest();
    if reviewed.check_version != CHECK_VERSION || reviewed.check_set_digest != digest {
        let mut failure = gate(
            "/audit/check_set_digest",
            "The audit ran under a different check set. Run investigation blindspots again and review its response.",
        );
        failure.error.expected =
            Some(json!({"check_version": CHECK_VERSION, "check_set_digest": digest}));
        return Err(failure);
    }
    let state = ctx.state();
    if state.verified_snapshot(snapshot.sequence) != Some(&snapshot.head_hash) {
        return Err(gate(
            "/audited_snapshot",
            "Use the snapshot of the audit response, a verified head of this case journal.",
        ));
    }
    let case = ctx
        .case_id()
        .ok_or_else(|| Failure::new(ErrorCode::IntegrityFailure, "A review belongs to a case."))?;
    let past = state.as_of(snapshot.sequence);
    let request = AuditRequest {
        subject: reviewed.subject_ref.map(|reference| reference.id),
    };
    let response = audit(&past, snapshot, case, &request)
        .map_err(|failure| failure.at("/audit/subject_ref"))?;
    if response.scope.subject_ref != reviewed.subject_ref {
        let mut failure = invalid(
            "/audit/subject_ref",
            "Use the audited subject revision from the audit response scope.",
        );
        failure.error.expected = Some(json!(response.scope.subject_ref));
        return Err(failure);
    }
    Ok(response)
}

/// Resolve references and check the audit. `subject` is excluded from the
/// required read-set because the review records it as `subject_ref`.
pub(crate) fn resolve(
    ctx: &OperationContext<'_>,
    subject: Id,
    snapshot: &Snapshot,
    detail: &Detail<'_>,
) -> Result<Resolved, Failure> {
    let mut read = Vec::new();
    let mut warnings = Vec::new();
    let audit = match detail.audit {
        None => {
            if ctx.state().verified_snapshot(snapshot.sequence) != Some(&snapshot.head_hash) {
                warnings.push(
                    "The audited_snapshot is not a verified head of this case journal; the review records it as stated."
                        .to_owned(),
                );
            }
            None
        }
        Some(reviewed) => Some((reviewed, reviewed_audit(ctx, snapshot, reviewed)?)),
    };
    let mut gap_dispositions = Vec::new();
    for (index, disposition) in detail.gap_dispositions.iter().enumerate() {
        let path = format!("/gap_dispositions/{index}");
        let Some((_, response)) = &audit else {
            return Err(invalid("/audit", "Name the reviewed audit."));
        };
        let Some(finding) = response
            .checks
            .iter()
            .find(|finding| finding.gap_id == disposition.gap_id)
        else {
            let mut failure = gate(
                &format!("{path}/gap_id"),
                "Give a disposition only to a gap_id the reviewed audit reported.",
            );
            failure.error.expected = Some(json!(
                response
                    .checks
                    .iter()
                    .map(|finding| &finding.gap_id)
                    .collect::<Vec<_>>()
            ));
            return Err(failure);
        };
        let related_refs = related(
            ctx,
            &disposition.related_refs,
            &format!("{path}/related_refs"),
            &mut read,
        )?;
        let record_refs = records(
            ctx,
            &disposition.record_refs,
            None,
            &format!("{path}/record_refs"),
        )?;
        gap_dispositions.push(json!({
            "gap_id": disposition.gap_id, "check_id": finding.check_id,
            "disposition": disposition.disposition, "reason": disposition.reason,
            "uncertainty": disposition.uncertainty, "audited_subject_refs": finding.subject_refs,
            "related_refs": related_refs, "record_refs": record_refs,
        }));
    }
    let mut novel_gaps = Vec::new();
    for (index, gap) in detail.novel_gaps.iter().enumerate() {
        let path = format!("/novel_gaps/{index}");
        let related_refs = related(
            ctx,
            &gap.related_refs,
            &format!("{path}/related_refs"),
            &mut read,
        )?;
        let record_refs = records(
            ctx,
            &gap.record_refs,
            Some(GAP_RECORD_KINDS),
            &format!("{path}/record_refs"),
        )?;
        novel_gaps.push(json!({
            "origin": "reviewer", "statement": gap.statement, "uncertainty": gap.uncertainty,
            "related_refs": related_refs, "record_refs": record_refs,
        }));
    }
    let audit = audit.map(|(reviewed, response)| {
        let reported: Vec<&String> = response
            .checks
            .iter()
            .map(|finding| &finding.gap_id)
            .collect();
        let dispositioned: BTreeSet<&str> = detail
            .gap_dispositions
            .iter()
            .map(|disposition| disposition.gap_id.as_str())
            .collect();
        let open: Vec<&String> = reported
            .iter()
            .copied()
            .filter(|gap_id| !dispositioned.contains(gap_id.as_str()))
            .collect();
        if !open.is_empty() {
            warnings.push(format!(
                "{} gap(s) reported by the reviewed audit have no disposition in this review: {}.",
                open.len(),
                open.iter()
                    .map(|gap_id| gap_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        json!({
            "check_version": reviewed.check_version,
            "check_set_digest": reviewed.check_set_digest,
            "subject_ref": reviewed.subject_ref,
            "scope": if reviewed.subject_ref.is_some() { "subject" } else { "case" },
            "reported_gap_ids": reported,
            "undispositioned_gap_ids": open,
        })
    });
    read.retain(|reference| reference.id != subject);
    Ok(Resolved {
        audit,
        gap_dispositions,
        novel_gaps,
        read,
        warnings,
    })
}

impl Resolved {
    /// Add the resolved content to the stored review body. A review without
    /// any of it keeps the body shape it had before these fields existed.
    pub(crate) fn store(self, body: &mut Value, ctx: &mut OperationContext<'_>) {
        if let Some(audit) = self.audit {
            body["audit"] = audit;
        }
        if !self.gap_dispositions.is_empty() {
            body["gap_dispositions"] = Value::Array(self.gap_dispositions);
        }
        if !self.novel_gaps.is_empty() {
            body["novel_gaps"] = Value::Array(self.novel_gaps);
        }
        for warning in self.warnings {
            ctx.warn(warning);
        }
    }
}

/// Whether a submitted blindspot review read an audit whose check set differs
/// from the one this build registers. Reviews without an audit never do.
pub fn check_set_changed(record: &Record) -> bool {
    let body = &record.body;
    if record.header.id.kind() != Kind::Review
        || body["kind"] != "blindspot"
        || body["status"] != "submitted"
        || body["audit"].is_null()
    {
        return false;
    }
    body["audit"]["check_version"] != CHECK_VERSION
        || body["audit"]["check_set_digest"] != json!(current_check_set_digest())
}

/// Submitted blindspot reviews of `subject` that are current in the freshness
/// index: no record they read or reviewed changed materially, nothing they
/// read needs review, and their audit's check set is still the registered one.
/// This is the single definition used by pack policy gates and views.
pub fn current_blindspot_reviews(
    state: &State,
    subject: &Record,
) -> Result<Vec<RevisionRef>, Failure> {
    let case = subject.header.case_id;
    let mut reviews = state.records().filter(|record| {
        record.header.case_id == case
            && record.header.id.kind() == Kind::Review
            && record.body["kind"] == "blindspot"
            && record.body["status"] == "submitted"
            && record.body["subject_ref"]["id"] == json!(subject.header.id)
    });
    let Some(first) = reviews.next() else {
        return Ok(Vec::new());
    };
    let index = FreshnessIndex::build(state, case);
    let mut found: Vec<RevisionRef> = std::iter::once(first)
        .chain(reviews)
        .filter(|review| {
            index
                .get(review.header.id)
                .is_some_and(|result| result.freshness == Freshness::Current)
        })
        .map(Record::reference)
        .collect();
    found.sort_by_key(|reference| reference.id);
    Ok(found)
}

#[cfg(test)]
mod tests;
