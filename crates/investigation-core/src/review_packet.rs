//! Blinded review packet (§14.2).
//!
//! A read of a Conclusion, Decision or Comparison that a second reviewer can
//! judge without the originating conclusion. It keeps the question, the
//! evidence and the evaluation criteria and omits the subject's claim, choice
//! or outcome, every confidence and likelihood, interpretive rationale,
//! record status and author identities. Inclusion is positive: each record
//! kind has a fixed list of retained fields, and every other field of a
//! packed record is listed in its `omitted_fields`. The packet also names
//! what it left out as whole records: reviews of and objections to the
//! subject.
//!
//! The packet is built from one snapshot and is deterministic: records are
//! ordered by ID, fields by name, and `packet_digest` hashes the content.
//! Classification is the caller's: a record the audience may not see is
//! listed by reference as `restricted`, with no fields. A packet promises no
//! cognitive independence; a review made from it still declares its own
//! `context_isolation`.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use crate::{
    basis::basis_digest,
    command::RevisionRef,
    identity::{Id, Kind},
    review_ops::required_read_set,
    state::{Record, State},
    wire::{Digest, ErrorCode, Failure},
};

/// Version of the packet layout and field lists.
pub const PACKET_VERSION: &str = "inv.review_packet.v1";

/// Stated with every packet.
pub const NOTICE: &str = "A blinded packet omits the originating conclusion and confidence so a reviewer can judge the evidence first. It does not make a review independent: record the review's context_isolation and what the reviewer saw.";

/// Kinds a packet can be made for: the subjects of a validation review.
pub const SUBJECT_KINDS: &[Kind] = &[Kind::Conclusion, Kind::Decision, Kind::Comparison];

/// Fields retained for the packet's subject. Everything else is the
/// originating conclusion, its rationale or its status.
fn subject_fields(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Conclusion => &["scope"],
        Kind::Decision => &[
            "question_ref",
            "compared_alternatives",
            "criteria_set_version",
            "comparison_refs",
            "hard_conditions",
            "evidence_refs",
            "assumption_refs",
        ],
        Kind::Comparison => &[
            "option_ref",
            "criterion_ref",
            "method",
            "value",
            "evidence_refs",
            "assumption_refs",
        ],
        _ => &[],
    }
}

/// Fields retained for every other packed record. Confidence, likelihood,
/// rationale, status and actor fields are never listed.
fn retained_fields(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Case => &[
            "title",
            "objective",
            "scope",
            "exclusions",
            "decision_relevance",
        ],
        Kind::Question => &["wording", "context", "scope", "importance", "parent"],
        Kind::Hypothesis => &[
            "question_ref",
            "statement",
            "applicability",
            "possible_falsifiers",
            "rival_refs",
            "assumption_refs",
        ],
        Kind::Observation => &[
            "value",
            "basis",
            "extraction_method",
            "validity",
            "quality",
            "limitations",
            "evidence_kind",
            "variables_changed",
            "observed_at",
            "measurement",
        ],
        Kind::Source => &[
            "identity",
            "title",
            "publisher",
            "access",
            "verification_limits",
        ],
        Kind::Artifact => &[
            "digest",
            "size_bytes",
            "format",
            "source",
            "transforms",
            "availability",
            "integrity",
            "role",
        ],
        Kind::Run => &[
            "intent",
            "tool",
            "environment",
            "started_at",
            "finished_at",
            "result_assertion",
            "artifact_refs",
            "manifest_digest",
            "execution_status",
            "evidence_completeness",
            "evidence_kind",
            "variables_changed",
            "search_coverage",
            "measurement",
            "repetition",
        ],
        Kind::Assessment => &[
            "subject_ref",
            "evidence_refs",
            "relation",
            "assumption_refs",
        ],
        Kind::Comparison => &[
            "option_ref",
            "criterion_ref",
            "outcome",
            "method",
            "value",
            "evidence_refs",
            "assumption_refs",
            "limitations",
            "uncertainty",
        ],
        Kind::Criterion => &[
            "name",
            "kind",
            "question_ref",
            "option_refs",
            "scope",
            "scale",
            "preferred_direction",
            "threshold",
        ],
        Kind::Option => &[
            "question_ref",
            "description",
            "context",
            "feasibility",
            "dependencies",
        ],
        Kind::Assumption => &["claim", "consequence", "materiality", "affected_refs"],
        Kind::Conclusion => &[
            "statement",
            "scope",
            "support_refs",
            "contradiction_refs",
            "caveats",
        ],
        _ => &[],
    }
}

/// Whole categories a blinded packet leaves out, always listed.
const CATEGORIES: &[(&str, &str)] = &[
    (
        "originating_conclusion",
        "The subject's claim, choice or outcome, its significance, caveats, tradeoffs and rationale; see subject.omitted_fields.",
    ),
    (
        "confidence",
        "Confidence and likelihood on every packed record.",
    ),
    (
        "author_identities",
        "Recording actors, self-reported descriptors and producer or assessor actor fields of every packed record.",
    ),
    (
        "interpretive_rationale",
        "Rationale text on assessments and comparisons, rival expectations and weighting rationale.",
    ),
    (
        "record_status",
        "Lifecycle, status, disposition and resolution fields, which can state a conclusion.",
    ),
    (
        "reviews_and_objections",
        "Reviews of the subject and objections, listed by reference in omitted_records.",
    ),
];

/// One record in the packet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PacketRecord {
    #[serde(rename = "ref")]
    pub reference: RevisionRef,
    #[serde(rename = "type")]
    pub kind: String,
    /// `included` or `restricted`. A restricted record carries no fields.
    pub content_state: &'static str,
    #[schemars(with = "std::collections::BTreeMap<String, Value>")]
    pub fields: Map<String, Value>,
    /// Body fields of this record left out of the packet, by name.
    pub omitted_fields: Vec<String>,
}

/// A record left out whole, by reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct OmittedRecord {
    #[serde(rename = "ref")]
    pub reference: RevisionRef,
    #[serde(rename = "type")]
    pub kind: String,
    pub reason: &'static str,
}

/// A category of content the packet leaves out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Omission {
    pub what: &'static str,
    pub detail: &'static str,
}

/// What a validation review of the subject must cite.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PacketReviewBasis {
    pub subject_ref: RevisionRef,
    pub basis_digest: Digest,
    pub read_set: Vec<RevisionRef>,
}

/// A blinded review packet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewPacket {
    pub packet_version: &'static str,
    pub blinded: bool,
    pub subject: PacketRecord,
    /// The case and its questions.
    pub question: Vec<PacketRecord>,
    /// Criteria in the subject's dependency closure.
    pub criteria: Vec<PacketRecord>,
    /// Everything else in the subject's dependency closure.
    pub evidence: Vec<PacketRecord>,
    pub omitted_records: Vec<OmittedRecord>,
    pub omissions: Vec<Omission>,
    /// Exact references a review made from this packet cites.
    pub review_basis: PacketReviewBasis,
    /// SHA-256 of the packet content above, for citing what was available.
    pub packet_digest: Digest,
    pub notice: &'static str,
}

fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "Invalid stored state for a review packet.",
    )
}

fn pack(record: &Record, retained: &[&str], visible: bool) -> PacketRecord {
    let body = record.body.as_object();
    let mut fields = Map::new();
    let mut omitted = BTreeSet::new();
    for (key, value) in body.into_iter().flatten() {
        if visible && retained.contains(&key.as_str()) {
            fields.insert(key.clone(), value.clone());
        } else {
            omitted.insert(key.clone());
        }
    }
    PacketRecord {
        reference: record.reference(),
        kind: record.header.id.kind().to_string(),
        content_state: if visible { "included" } else { "restricted" },
        fields,
        omitted_fields: if visible {
            omitted.into_iter().collect()
        } else {
            Vec::new()
        },
    }
}

fn names_subject(record: &Record, subject: Id) -> bool {
    serde_json::from_value::<RevisionRef>(record.body["subject_ref"].clone())
        .is_ok_and(|reference| reference.id == subject)
}

/// Build the blinded packet for `subject` from `state`. `visible` says
/// whether the audience may see a record's content.
pub fn blinded_packet(
    state: &State,
    subject: &Record,
    visible: &dyn Fn(&Record) -> bool,
) -> Result<ReviewPacket, Failure> {
    let kind = subject.header.id.kind();
    if !SUBJECT_KINDS.contains(&kind) {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "A blinded review packet covers a Conclusion, Decision or Comparison.",
        )
        .at("/id"));
    }
    let case = subject.header.case_id;
    let read_set = required_read_set(state, subject)?;
    let mut question = Vec::new();
    let mut criteria = Vec::new();
    let mut evidence = Vec::new();
    let mut omitted_records = Vec::new();
    let mut seen = BTreeSet::new();
    for reference in &read_set {
        let record = state.revision(*reference).ok_or_else(integrity)?;
        seen.insert(record.header.id);
        let kind = record.header.id.kind();
        match kind {
            Kind::Review | Kind::Objection => omitted_records.push(OmittedRecord {
                reference: *reference,
                kind: kind.to_string(),
                reason: "interpretation_of_subject",
            }),
            _ => {
                let packed = pack(record, retained_fields(kind), visible(record));
                match kind {
                    Kind::Case | Kind::Question => question.push(packed),
                    Kind::Criterion => criteria.push(packed),
                    _ => evidence.push(packed),
                }
            }
        }
    }
    for record in state
        .records()
        .filter(|record| record.header.case_id == case && !seen.contains(&record.header.id))
    {
        match record.header.id.kind() {
            Kind::Case | Kind::Question => {
                question.push(pack(
                    record,
                    retained_fields(record.header.id.kind()),
                    visible(record),
                ));
            }
            Kind::Review if names_subject(record, subject.header.id) => {
                omitted_records.push(OmittedRecord {
                    reference: record.reference(),
                    kind: Kind::Review.to_string(),
                    reason: "review_of_subject",
                });
            }
            Kind::Objection if names_subject(record, subject.header.id) => {
                omitted_records.push(OmittedRecord {
                    reference: record.reference(),
                    kind: Kind::Objection.to_string(),
                    reason: "objection_to_subject",
                });
            }
            _ => {}
        }
    }
    for list in [&mut question, &mut criteria, &mut evidence] {
        list.sort_by_key(|item| (item.reference.id, item.reference.revision));
    }
    omitted_records.sort_by_key(|item| (item.reference.id, item.reference.revision));
    let subject_packed = pack(subject, subject_fields(kind), visible(subject));
    let omissions: Vec<Omission> = CATEGORIES
        .iter()
        .map(|(what, detail)| Omission { what, detail })
        .collect();
    let review_basis = PacketReviewBasis {
        subject_ref: subject.reference(),
        basis_digest: basis_digest(subject)?,
        read_set,
    };
    let content = json!({
        "packet_version": PACKET_VERSION, "subject": subject_packed, "question": question,
        "criteria": criteria, "evidence": evidence, "omitted_records": omitted_records,
        "omissions": omissions, "review_basis": review_basis,
    });
    let bytes = serde_json::to_vec(&content).map_err(|_| integrity())?;
    let packet_digest = format!("sha256:{:x}", Sha256::digest(bytes))
        .parse()
        .map_err(|_| integrity())?;
    Ok(ReviewPacket {
        packet_version: PACKET_VERSION,
        blinded: true,
        subject: subject_packed,
        question,
        criteria,
        evidence,
        omitted_records,
        omissions,
        review_basis,
        packet_digest,
        notice: NOTICE,
    })
}
