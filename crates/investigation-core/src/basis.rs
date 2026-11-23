//! Versioned substantive and set-membership review bases.
//!
//! Field lists below are exhaustive: an unclassified body field is an error,
//! not silently omitted. Lifecycle changes are handled separately from claim
//! and evidence changes. Changing a list requires a basis version migration.
use crate::{
    command::{Reference, RevisionRef},
    evaluate::{CriterionKind, CriterionLifecycle, OptionLifecycle},
    evaluate_ops::{CriterionBody, OptionBody},
    explain::{AssumptionStatus, Materiality},
    header::Knowledge,
    identity::{Id, Kind},
    review::{ObjectionSeverity, ObjectionStatus},
    state::{Record, State},
    wire::{Digest, ErrorCode, Failure},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

/// Canonical objection shape shared with objection operations. Subject identity
/// is stable across revision changes; the exact reviewed revision remains pinned.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectionBody {
    pub subject_ref: RevisionRef,
    pub statement: String,
    pub scope: String,
    pub severity: ObjectionSeverity,
    pub status: ObjectionStatus,
    pub review_ref: Option<RevisionRef>,
    pub evidence_refs: Knowledge<Vec<RevisionRef>>,
    pub resolution_reason: Option<String>,
    pub resolution_evidence_refs: Vec<RevisionRef>,
    /// The fresh review a resolution cites, when one was required or given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_review_ref: Option<RevisionRef>,
}
/// Mirrors the flat canonical assumption body produced by the explanatory lane.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssumptionBody {
    pub claim: String,
    pub consequence: String,
    pub materiality: Materiality,
    pub affected_refs: Vec<Reference>,
    pub status: AssumptionStatus,
    pub rationale: Option<String>,
    pub evidence_refs: Option<Vec<Reference>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MembershipRule {
    ActiveHardCriteria,
    EligibleOptions,
    OpenObjections,
    MaterialAssumptions,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MembershipScope {
    Question {
        case_id: Id,
        question_id: Id,
        option_id: Option<Id>,
    },
    Subject {
        case_id: Id,
        subject_id: Id,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MembershipMember {
    pub id: Id,
    pub basis_digest: Digest,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MembershipBasis {
    pub rule: MembershipRule,
    pub version: u32,
    pub scope: MembershipScope,
    pub members: Vec<MembershipMember>,
    pub digest: Digest,
}

fn failure(message: &str) -> Failure {
    Failure::new(ErrorCode::IntegrityFailure, message)
}
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<_, _> =
                map.iter().map(|(k, v)| (k.clone(), canonical(v))).collect();
            serde_json::to_value(sorted).expect("JSON value serialization")
        }
        Value::Array(v) => Value::Array(v.iter().map(canonical).collect()),
        other => other.clone(),
    }
}
fn hash(value: &Value) -> Result<Digest, Failure> {
    let bytes =
        serde_json::to_vec(&canonical(value)).map_err(|_| failure("Cannot serialize basis."))?;
    format!("sha256:{:x}", Sha256::digest(bytes))
        .try_into()
        .map_err(|_| failure("Cannot form basis digest."))
}

/// Hash only the explicitly classified substantive fields. Do not include CAS
/// revision, timestamps, review pointers or lifecycle-only metadata.
pub fn basis_digest(record: &Record) -> Result<Digest, Failure> {
    let (substantive, lifecycle): (&[&str], &[&str]) = match record.header.id.kind() {
        Kind::Option => (
            &[
                "question_ref",
                "description",
                "context",
                "feasibility",
                "dependencies",
                "tradeoff_notes",
            ],
            &[
                "lifecycle",
                "disposition_reason",
                "amendment_reason",
                "criterion_ref",
            ],
        ),
        Kind::Criterion => (
            &[
                "name",
                "kind",
                "question_ref",
                "option_refs",
                "scale",
                "preferred_direction",
                "threshold",
                "weighting_rationale",
            ],
            &["lifecycle", "change_reason", "material_change"],
        ),
        Kind::Comparison => (
            &[
                "option_ref",
                "criterion_ref",
                "outcome",
                "method",
                "value",
                "basis",
                "evidence_refs",
                "assumption_refs",
                "limitations",
                "uncertainty",
                "rationale",
            ],
            &["retracted", "change_reason"],
        ),
        Kind::Assumption => (
            &[
                "claim",
                "consequence",
                "materiality",
                "affected_refs",
                "status",
                "rationale",
                "evidence_refs",
            ],
            &[],
        ),
        Kind::Objection => (
            &[
                "subject_ref",
                "statement",
                "scope",
                "severity",
                "review_ref",
                "evidence_refs",
            ],
            &[
                "status",
                "resolution_reason",
                "resolution_evidence_refs",
                "resolution_review_ref",
            ],
        ),
        Kind::Assessment => (
            &[
                "subject_ref",
                "evidence_refs",
                "relation",
                "rival_expectations",
                "rationale",
                "assumption_refs",
                "confidence",
                "likelihood",
                "basis_digest",
            ],
            &[
                "status",
                "retraction_reason",
                "superseded_by",
                "supersedes",
                "rationale_for_status",
                "change_reason",
            ],
        ),
        Kind::Conclusion => (
            &[
                "statement",
                "scope",
                "significance",
                "support_refs",
                "contradiction_refs",
                "caveats",
                "causal_factors",
                "causal_test",
                "confidence",
                "likelihood",
            ],
            &[
                "status",
                "lifecycle",
                "review_ref",
                "reviewed_basis",
                "disposition_evidence_refs",
                "successor_ref",
                "change_reason",
                "gate_policy",
            ],
        ),
        Kind::Decision => (
            &[
                "question_ref",
                "choice",
                "compared_alternatives",
                "criteria_set_version",
                "comparison_refs",
                "hard_conditions",
                "evidence_refs",
                "assumption_refs",
                "tradeoffs",
                "consequences",
                "reopen_trigger",
                "expected_outcome",
                "check_when",
                "reversal_conditions",
            ],
            &[
                "status",
                "lifecycle",
                "review_ref",
                "reviewed_basis",
                "supersedes",
                "superseded_by",
                "change_reason",
                "acceptance_challenges",
                "gate_policy",
            ],
        ),
        _ => return Err(failure("Record kind has no defined substantive basis.")),
    };
    let object = record
        .body
        .as_object()
        .ok_or_else(|| failure("Record body is not an object."))?;
    if object
        .keys()
        .any(|k| !substantive.contains(&k.as_str()) && !lifecycle.contains(&k.as_str()))
    {
        return Err(failure("Record contains an unclassified basis field."));
    }
    for key in substantive {
        let optional = matches!(
            (record.header.id.kind(), *key),
            (Kind::Assumption, "rationale" | "evidence_refs") | (Kind::Conclusion, "causal_test")
        );
        if !object.contains_key(*key) && !optional {
            return Err(failure("Record is missing a substantive basis field."));
        }
    }
    let selected: BTreeMap<_, _> = substantive
        .iter()
        .filter_map(|key| object.get(*key).map(|v| ((*key).to_owned(), v.clone())))
        .collect();
    hash(&json!({"version":1,"kind":record.header.id.kind().prefix(),"fields":selected}))
}
fn read<T: serde::de::DeserializeOwned>(r: &Record) -> Result<T, Failure> {
    serde_json::from_value(r.body.clone())
        .map_err(|_| failure("Invalid canonical membership body."))
}
fn exact_ref(r: &Reference) -> Result<RevisionRef, Failure> {
    match r {
        Reference::Existing(v) => Ok(*v),
        Reference::Client(_) => Err(failure("Canonical body contains a client alias.")),
    }
}

/// Evaluate a fixed rule against one case snapshot. Empty membership is hashed
/// with its rule and scope and is therefore a first-class review basis.
pub fn membership(
    state: &State,
    rule: MembershipRule,
    scope: MembershipScope,
) -> Result<MembershipBasis, Failure> {
    let (case_id, target) = match &scope {
        MembershipScope::Question {
            case_id,
            question_id,
            option_id,
        } if matches!(
            rule,
            MembershipRule::ActiveHardCriteria | MembershipRule::EligibleOptions
        ) =>
        {
            if case_id.kind() != Kind::Case
                || question_id.kind() != Kind::Question
                || option_id.is_some_and(|id| id.kind() != Kind::Option)
                || (rule == MembershipRule::EligibleOptions && option_id.is_some())
            {
                return Err(failure("Invalid question membership scope."));
            }
            (*case_id, *question_id)
        }
        MembershipScope::Subject {
            case_id,
            subject_id,
        } if matches!(
            rule,
            MembershipRule::OpenObjections | MembershipRule::MaterialAssumptions
        ) =>
        {
            if case_id.kind() != Kind::Case {
                return Err(failure("Invalid subject membership scope."));
            }
            (*case_id, *subject_id)
        }
        _ => return Err(failure("Membership rule and scope do not match.")),
    };
    let scoped = state
        .current(target)
        .filter(|r| r.header.case_id == case_id)
        .ok_or_else(|| failure("Membership scope target is not in this case."))?;
    if matches!(scope, MembershipScope::Question { .. })
        && scoped.header.id.kind() != Kind::Question
    {
        return Err(failure("Scope target is not a question."));
    }
    if let MembershipScope::Question {
        option_id: Some(id),
        ..
    } = &scope
    {
        let option: OptionBody = read(
            state
                .current(*id)
                .filter(|r| r.header.case_id == case_id)
                .ok_or_else(|| failure("Scoped option is not in this case."))?,
        )?;
        if option.question_ref.id != target {
            return Err(failure("Scoped option belongs to another question."));
        }
    }
    let mut members = Vec::new();
    for record in state.records().filter(|r| r.header.case_id == case_id) {
        let include = match (rule, record.header.id.kind()) {
            (MembershipRule::ActiveHardCriteria, Kind::Criterion) => {
                let c: CriterionBody = read(record)?;
                c.kind == CriterionKind::Hard
                    && c.lifecycle == CriterionLifecycle::Active
                    && c.question_ref.id == target
                    && match (&scope, &c.option_refs) {
                        (
                            MembershipScope::Question {
                                option_id: Some(id),
                                ..
                            },
                            Some(options),
                        ) => options.iter().any(|o| o.id == *id),
                        _ => true,
                    }
            }
            (MembershipRule::EligibleOptions, Kind::Option) => {
                let o: OptionBody = read(record)?;
                o.lifecycle == OptionLifecycle::Eligible && o.question_ref.id == target
            }
            (MembershipRule::OpenObjections, Kind::Objection) => {
                let o: ObjectionBody = read(record)?;
                o.status == ObjectionStatus::Open && o.subject_ref.id == target
            }
            (MembershipRule::MaterialAssumptions, Kind::Assumption) => {
                let a: AssumptionBody = read(record)?;
                if a.materiality != Materiality::Material
                    || a.status == AssumptionStatus::Superseded
                {
                    false
                } else {
                    let mut found = false;
                    for r in &a.affected_refs {
                        if exact_ref(r)?.id == target {
                            found = true;
                        }
                    }
                    found
                }
            }
            _ => false,
        };
        if include {
            members.push(MembershipMember {
                id: record.header.id,
                basis_digest: basis_digest(record)?,
            });
        }
    }
    members.sort_by_key(|m| m.id);
    let digest = hash(&json!({"version":1,"rule":rule,"scope":scope,"members":members}))?;
    Ok(MembershipBasis {
        rule,
        version: 1,
        scope,
        members,
        digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        header::{RecordHeader, Revision, SchemaVersion},
        state::AcceptedTransaction,
        wire::RecordChange,
    };
    fn id(prefix: &str, n: u64) -> Id {
        format!("{prefix}_018d87fc-5234-7000-8000-{n:012x}")
            .parse()
            .unwrap()
    }
    fn record(id: Id, case_id: Id, seq: u64, body: Value) -> Record {
        let actor = "agent:test".parse().unwrap();
        let time = "2026-11-22T08:12:44.000Z".parse().unwrap();
        Record {
            header: RecordHeader {
                id,
                case_id,
                schema_version: SchemaVersion::RecordV0Alpha,
                created_by: actor,
                actor_descriptor_self_reported: None,
                created_at: time,
                recorded_at: "2026-11-22T08:12:44.000Z".parse().unwrap(),
                recorded_by: "agent:test".parse().unwrap(),
                observed_at: None,
                acquired_at: None,
                acceptance_sequence: seq,
                revision: Revision(1),
                history: vec![],
                classification: Default::default(),
                provenance: Default::default(),
                extensions: Default::default(),
            },
            body,
        }
    }
    fn add(s: &mut State, r: Record) {
        let change = RecordChange {
            client_ref: None,
            id: r.header.id,
            revision: 1,
        };
        s.apply(&AcceptedTransaction {
            case_id: r.header.case_id,
            sequence: r.header.acceptance_sequence,
            records: vec![r],
            created: vec![change],
            updated: vec![],
            warnings: vec![],
        })
        .unwrap();
    }
    fn change(s: &mut State, id: Id, body: Value) {
        let mut r = s.current(id).unwrap().clone();
        r.header.acceptance_sequence = s.sequence() + 1;
        r.header.history.push(r.header.revision);
        r.header.revision.0 += 1;
        r.body = body;
        let c = RecordChange {
            client_ref: None,
            id,
            revision: r.header.revision.0,
        };
        s.apply(&AcceptedTransaction {
            case_id: r.header.case_id,
            sequence: r.header.acceptance_sequence,
            records: vec![r],
            created: vec![],
            updated: vec![c],
            warnings: vec![],
        })
        .unwrap();
    }
    fn criterion(q: Id, selected: Option<Id>) -> Value {
        json!({"name":"Latency","kind":"hard","question_ref":{"id":q,"revision":1},"option_refs":selected.map(|x|vec![json!({"id":x,"revision":1})]),"scale":{"unit":"ms","description":"p99","anchors":[]},"preferred_direction":"lower","threshold":{"kind":"measured","operator":"at_most","value":{"decimal":"25","unit":"ms"}},"weighting_rationale":null,"lifecycle":"active","change_reason":null,"material_change":false})
    }
    fn option(q: Id) -> Value {
        json!({"question_ref":{"id":q,"revision":1},"description":"Keep existing","context":null,"feasibility":null,"dependencies":[],"tradeoff_notes":[],"lifecycle":"eligible","disposition_reason":null,"amendment_reason":null,"criterion_ref":null})
    }
    #[test]
    fn scoped_membership_tracks_phantoms_and_not_unrelated_questions() {
        let case = id("case", 1);
        let q = id("q", 2);
        let other = id("q", 3);
        let opt = id("opt", 4);
        let c = id("crit", 5);
        let c2 = id("crit", 6);
        let objection = id("obj", 7);
        let mut s = State::default();
        add(&mut s, record(case, case, 1, json!({})));
        add(&mut s, record(q, case, 2, json!({})));
        add(&mut s, record(other, case, 3, json!({})));
        add(&mut s, record(opt, case, 4, option(q)));
        let scope = MembershipScope::Question {
            case_id: case,
            question_id: q,
            option_id: Some(opt),
        };
        let empty = membership(&s, MembershipRule::ActiveHardCriteria, scope.clone()).unwrap();
        assert!(empty.members.is_empty());
        add(&mut s, record(c2, case, 5, criterion(other, None)));
        assert_eq!(
            empty.digest,
            membership(&s, MembershipRule::ActiveHardCriteria, scope.clone())
                .unwrap()
                .digest
        );
        add(&mut s, record(c, case, 6, criterion(q, Some(opt))));
        let active = membership(&s, MembershipRule::ActiveHardCriteria, scope.clone()).unwrap();
        assert_eq!(active.members.len(), 1);
        assert_ne!(empty.digest, active.digest);
        let mut retired = criterion(q, Some(opt));
        retired["lifecycle"] = json!("retired");
        change(&mut s, c, retired);
        assert_eq!(
            empty.digest,
            membership(&s, MembershipRule::ActiveHardCriteria, scope)
                .unwrap()
                .digest
        );
        let os = MembershipScope::Subject {
            case_id: case,
            subject_id: q,
        };
        let before = membership(&s, MembershipRule::OpenObjections, os.clone()).unwrap();
        add(
            &mut s,
            record(
                objection,
                case,
                8,
                json!({"subject_ref":{"id":q,"revision":1},"statement":"Evidence is missing","scope":"this question","severity":"material","status":"open","review_ref":null,"evidence_refs":{"state":"unknown"},"resolution_reason":null,"resolution_evidence_refs":[]}),
            ),
        );
        let after = membership(&s, MembershipRule::OpenObjections, os).unwrap();
        assert_eq!(after.members.len(), 1);
        assert_ne!(before.digest, after.digest);
    }
    #[test]
    fn lifecycle_is_separate_but_substantive_and_unknown_fields_are_not() {
        let case = id("case", 1);
        let q = id("q", 2);
        let opt = id("opt", 3);
        let mut r = record(opt, case, 1, option(q));
        let first = basis_digest(&r).unwrap();
        assert_eq!(
            first.as_str(),
            "sha256:69266abefa20d85b215bac521c8efb07893a1c6dbd49ff1814d9d115507b174a"
        );
        r.body["lifecycle"] = json!("withdrawn");
        assert_eq!(first, basis_digest(&r).unwrap());
        r.body["description"] = json!("Changed claim");
        assert_ne!(first, basis_digest(&r).unwrap());
        r.body["unclassified"] = json!(true);
        assert!(basis_digest(&r).is_err());
    }
    #[test]
    fn material_assumptions_and_eligible_options_use_explicit_links() {
        let case = id("case", 1);
        let q = id("q", 2);
        let opt = id("opt", 3);
        let a = id("asm", 4);
        let mut s = State::default();
        add(&mut s, record(case, case, 1, json!({})));
        add(&mut s, record(q, case, 2, json!({})));
        add(&mut s, record(opt, case, 3, option(q)));
        let scope = MembershipScope::Question {
            case_id: case,
            question_id: q,
            option_id: None,
        };
        assert_eq!(
            membership(&s, MembershipRule::EligibleOptions, scope.clone())
                .unwrap()
                .members
                .len(),
            1
        );
        let mut withdrawn = option(q);
        withdrawn["lifecycle"] = json!("withdrawn");
        change(&mut s, opt, withdrawn);
        assert!(
            membership(&s, MembershipRule::EligibleOptions, scope)
                .unwrap()
                .members
                .is_empty()
        );
        add(
            &mut s,
            record(
                a,
                case,
                5,
                json!({"claim":"Workload remains stable","consequence":"The comparison remains applicable","materiality":"material","affected_refs":[{"id":q,"revision":1}],"status":"unassessed"}),
            ),
        );
        let subject = MembershipScope::Subject {
            case_id: case,
            subject_id: q,
        };
        assert_eq!(
            membership(&s, MembershipRule::MaterialAssumptions, subject)
                .unwrap()
                .members
                .len(),
            1
        );
    }
    #[test]
    fn conclusion_review_basis_changes_for_scope_and_evidence_not_validation() {
        let case = id("case", 1);
        let mut r = record(
            id("concl", 8),
            case,
            1,
            json!({
                "statement":"Observed a wait", "scope":"test workload", "significance":"investigate",
                "support_refs":[], "contradiction_refs":[], "caveats":["one sample"],
                "causal_factors":[], "confidence":null, "likelihood":null, "status":"candidate"
            }),
        );
        let original = basis_digest(&r).unwrap();
        r.body["status"] = json!("validated");
        assert_eq!(original, basis_digest(&r).unwrap());
        r.body["scope"] = json!("production");
        assert_ne!(original, basis_digest(&r).unwrap());
        r.body["scope"] = json!("test workload");
        r.body["support_refs"] = json!([{"id":id("obs",9),"revision":1}]);
        assert_ne!(original, basis_digest(&r).unwrap());
    }
}
