//! Read-only structural blindspot audit and the `inv.audit.v1` envelope.
//!
//! The audit runs a registry of deterministic checks over one committed
//! snapshot of one case. It never calls a model, writes a record, alters
//! evidence or invents a hypothesis, and it never certifies completeness: a
//! result with no finding reads "no structural gap identified under checks X".
//!
//! # Registering checks
//!
//! Every check is one [`CheckSpec`] in a family slice. A family lives in its
//! own submodule (for example `blindspot/causal.rs`) as
//! `pub(super) const CHECKS: &[CheckSpec] = &[...]` and is added to
//! [`FAMILIES`]; nothing else in this module or in the command changes. A spec
//! carries:
//!
//! - a stable `check_id` (dotted, lowercase) and a `version`. Change the
//!   version whenever the condition changes; never reuse an ID for a different
//!   condition. The registry digest in every response changes with them;
//! - the subject kinds it reports on, the recorded fields it reads, and its
//!   condition in words;
//! - a fixed `why` and `severity_reason`. Messages are static so a finding can
//!   never quote record text: withheld content cannot leak and nothing is
//!   fabricated. Per-finding specifics are machine tokens in `facts`;
//! - the operation schema and method card that help resolve it;
//! - its [`CheckSource`]: either gap rules of [`crate::gap_rules`] (then the
//!   finding keeps the frontier's `gap_id`, so `next` and `frontier` surface
//!   the same gap) or a native function over an [`AuditContext`].
//!
//! A native function returns [`Detection`]s: subject revisions, related
//! revisions it read, and fact tokens. The engine adds the IDs, messages,
//! classification placeholders and ordering. The tests in this module check
//! every registered spec (unique IDs, known schemas, cards and gap rules).

mod evaluative;
mod seed;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    classification::{Audience, AudiencePolicy, Classifier, Field},
    command::RevisionRef,
    freshness::{FreshnessIndex, Propagation, dependency_edges},
    gap_rules::{GAP_RULE_VERSION, Gap, GapInventory, enumerate_gaps},
    identity::{Id, Kind},
    state::{Record, State},
    wire::{Digest, ErrorCode, Failure, Snapshot},
};

/// Version of the audit engine contract and of the built-in check registry.
/// The exact set of checks that ran is disclosed per response by
/// `checks_run` and `check_set_digest`.
pub const CHECK_VERSION: &str = "builtin-blindspot.v1";

/// The statement every audit carries, whatever it found.
pub const LIMITS: &str = "Checks cover recorded objects and typed fields only, under the checks listed in checks_run. A result with no identified gap is not proof of completeness: semantic alternatives, unrecorded evidence and problems outside these checks may remain unseen. This audit reads one committed snapshot, writes nothing and records no Review.";

/// Most records the `--subject` read-set walk visits.
pub const MAX_SUBJECT_READ_SET: usize = 4096;

/// Every registered check family. A later family adds one entry here.
const FAMILIES: &[&[CheckSpec]] = &[seed::CHECKS, evaluative::CHECKS];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AuditVersion {
    #[serde(rename = "inv.audit.v1")]
    V1,
}

/// How much a finding bears on reliance. Ordinal, never a score.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// A recorded critical challenge contests a record that is relied on.
    Critical,
    /// The recorded basis of a relied-on or consequential record is incomplete or changed.
    Material,
    /// Worth recording; does not by itself bear on reliance.
    Advisory,
}

/// A finding states only what the recorded structure shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FindingState {
    GapDetectedInRecordedStructure,
}

/// The overall result. Neither value is a completeness or correctness claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    GapsIdentifiedInRecordedStructure,
    NoStructuralGapIdentifiedUnderChecks,
}

/// One machine-readable fact behind a finding, such as
/// `freshness_trigger:withdrawn`, with the exact revisions it concerns.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindingFact {
    pub fact: String,
    pub refs: Vec<RevisionRef>,
}

/// One structural gap a check detected at the audited snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub check_id: String,
    pub check_version: String,
    /// Stable for the same check and subject ID set. A gap-rule finding keeps
    /// the frontier's `gap:` ID; a native one is `blindspot:CHECK:IDS`.
    pub gap_id: String,
    /// The gap rule that emitted a gap-rule finding.
    pub rule_id: Option<String>,
    pub rule_version: Option<String>,
    pub state: FindingState,
    pub severity: Severity,
    pub severity_reason: String,
    pub subject_refs: Vec<RevisionRef>,
    /// Other exact revisions the check read to reach this finding.
    pub related_refs: Vec<RevisionRef>,
    pub facts: Vec<FindingFact>,
    /// Referenced revisions the default audience may not see. They appear as
    /// ID-only placeholders; no text of theirs is in the response.
    pub withheld_refs: Vec<RevisionRef>,
    pub why: String,
    pub suggested_schema: Option<String>,
    pub guide: Option<String>,
    /// Added by the command: inspection, schema and method-card argv.
    pub inspect_argvs: Vec<Vec<String>>,
    pub schema_argv: Option<Vec<String>>,
    pub guide_argv: Option<Vec<String>>,
}

/// Where a check's findings come from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CheckSourceView {
    /// Gaps of the structural gap rules, shared with `next` and `frontier`.
    GapRules {
        rule_ids: Vec<String>,
        rule_prefix: Option<String>,
        rule_versions: Vec<String>,
    },
    /// A blindspot-only check evaluated by this engine.
    Native,
}

/// One check that ran, whether or not it found anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckRun {
    pub check_id: String,
    pub check_version: String,
    pub source: CheckSourceView,
    /// Record kinds the check reports on, by name.
    pub subject_kinds: Vec<String>,
    pub reads: Vec<String>,
    pub condition: String,
    pub severity: Severity,
    pub suggested_schema: Option<String>,
    pub guide: Option<String>,
    /// Findings of this check within the audited scope.
    pub findings: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuditScope {
    pub case_id: Id,
    /// With `--subject`: the conclusion, comparison or decision audited.
    pub subject_ref: Option<RevisionRef>,
    /// With `--subject`: the subject and the exact revisions it cites,
    /// transitively through inherited dependencies. Findings about any of
    /// these records are in scope.
    pub subject_read_set: Vec<RevisionRef>,
    /// The audience whose classification decides what is withheld.
    pub audience: Audience,
}

/// The `inv.audit.v1` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuditResponse {
    pub schema_version: AuditVersion,
    pub snapshot: Snapshot,
    pub check_version: String,
    /// SHA-256 of the sorted `check_id@check_version` lines (gap-rule checks
    /// append their rule versions), so any change to the check set shows.
    pub check_set_digest: Digest,
    pub scope: AuditScope,
    pub outcome: AuditOutcome,
    /// "N structural gap(s) identified under checks X" or "No structural gap
    /// identified under checks X". Never a statement of completeness.
    pub statement: String,
    /// Findings, in `gap_id` order.
    pub checks: Vec<Finding>,
    /// Every check that ran, in `check_id` order.
    pub checks_run: Vec<CheckRun>,
    pub limits: String,
    /// Added by the command: the method card and review schema to continue with.
    pub follow_up_argvs: Vec<Vec<String>>,
}

/// The published schema of [`AuditResponse`].
pub fn audit_schema() -> Schema {
    schema_for!(AuditResponse)
}

/// Gap rules a gap-rule check adopts.
#[derive(Clone, Copy, Debug)]
pub enum GapRuleMatch {
    Exact(&'static [&'static str]),
    Prefix(&'static str),
}

impl GapRuleMatch {
    fn matches(self, rule_id: &str) -> bool {
        match self {
            Self::Exact(ids) => ids.contains(&rule_id),
            Self::Prefix(prefix) => rule_id.starts_with(prefix),
        }
    }
}

pub type NativeCheck = fn(&AuditContext<'_>) -> Result<Vec<Detection>, Failure>;

#[derive(Clone, Copy)]
pub enum CheckSource {
    /// Findings are the matching gaps of the gap inventory. `rule_versions`
    /// lists any rule set besides [`GAP_RULE_VERSION`] that the gaps come from.
    GapRules {
        rules: GapRuleMatch,
        rule_versions: &'static [&'static str],
    },
    Native(NativeCheck),
}

/// One registered check. See the module documentation.
#[derive(Clone, Copy)]
pub struct CheckSpec {
    pub check_id: &'static str,
    pub version: &'static str,
    pub subject_kinds: &'static [Kind],
    pub reads: &'static [&'static str],
    pub condition: &'static str,
    pub why: &'static str,
    pub severity: Severity,
    pub severity_reason: &'static str,
    pub suggested_schema: Option<&'static str>,
    pub guide: Option<&'static str>,
    pub source: CheckSource,
}

/// What a native check found about one subject set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Detection {
    pub subject_refs: Vec<RevisionRef>,
    pub related_refs: Vec<RevisionRef>,
    pub facts: Vec<FindingFact>,
}

/// Read-only inputs of one audit, all from the same snapshot.
pub struct AuditContext<'a> {
    pub state: &'a State,
    pub snapshot: &'a Snapshot,
    pub case_id: Id,
    pub gaps: &'a GapInventory,
    pub freshness: &'a FreshnessIndex,
}

impl AuditContext<'_> {
    /// Current records of one kind in the case, in ID order.
    pub fn current(&self, kind: Kind) -> Vec<&Record> {
        let mut records: Vec<&Record> = self
            .state
            .records()
            .filter(|record| {
                record.header.case_id == self.case_id && record.header.id.kind() == kind
            })
            .collect();
        records.sort_by_key(|record| record.header.id);
        records
    }

    /// Current validated Conclusions and accepted Decisions, in ID order.
    pub fn promoted(&self) -> Vec<&Record> {
        let mut records = self.current(Kind::Conclusion);
        records.extend(self.current(Kind::Decision));
        records.retain(|record| is_promoted(record));
        records.sort_by_key(|record| record.header.id);
        records
    }
}

/// A Conclusion with status validated, or a Decision whose lifecycle or status is accepted.
pub fn is_promoted(record: &Record) -> bool {
    let body = &record.body;
    match record.header.id.kind() {
        Kind::Conclusion => body["status"] == "validated",
        Kind::Decision => body["lifecycle"] == "accepted" || body["status"] == "accepted",
        _ => false,
    }
}

/// Every registered check, in `check_id` order.
pub fn checks() -> Vec<&'static CheckSpec> {
    let mut checks: Vec<&'static CheckSpec> =
        FAMILIES.iter().flat_map(|family| family.iter()).collect();
    checks.sort_by_key(|check| check.check_id);
    checks
}

/// What `--subject` narrows to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuditRequest {
    pub subject: Option<Id>,
}

pub fn kind_name(kind: Kind) -> String {
    format!("{kind:?}").to_ascii_lowercase()
}

fn sorted(mut refs: Vec<RevisionRef>) -> Vec<RevisionRef> {
    refs.sort_by_key(|reference| (reference.id, reference.revision));
    refs.dedup();
    refs
}

fn ids_key(refs: &[RevisionRef]) -> String {
    let ids: BTreeSet<Id> = refs.iter().map(|reference| reference.id).collect();
    ids.iter().map(Id::to_string).collect::<Vec<_>>().join("+")
}

fn source_view(check: &CheckSpec) -> CheckSourceView {
    match check.source {
        CheckSource::GapRules {
            rules,
            rule_versions,
        } => {
            let (rule_ids, rule_prefix) = match rules {
                GapRuleMatch::Exact(ids) => (ids.iter().map(|id| (*id).to_owned()).collect(), None),
                GapRuleMatch::Prefix(prefix) => (Vec::new(), Some(prefix.to_owned())),
            };
            let mut versions = vec![GAP_RULE_VERSION.to_owned()];
            versions.extend(rule_versions.iter().map(|version| (*version).to_owned()));
            CheckSourceView::GapRules {
                rule_ids,
                rule_prefix,
                rule_versions: versions,
            }
        }
        CheckSource::Native(_) => CheckSourceView::Native,
    }
}

/// SHA-256 over one line per check: `check_id@version`, plus `@rule_version`
/// for each rule set a gap-rule check adopts.
pub fn check_set_digest(checks: &[&CheckSpec]) -> Digest {
    let mut lines: Vec<String> = checks
        .iter()
        .map(|check| {
            let mut line = format!("{}@{}", check.check_id, check.version);
            if let CheckSourceView::GapRules { rule_versions, .. } = source_view(check) {
                for version in rule_versions {
                    line.push('@');
                    line.push_str(&version);
                }
            }
            line
        })
        .collect();
    lines.sort();
    let mut hasher = Sha256::new();
    hasher.update(CHECK_VERSION.as_bytes());
    for line in lines {
        hasher.update(b"\n");
        hasher.update(line.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
        .parse()
        .expect("a SHA-256 hex digest is a valid digest")
}

/// The subject and every exact revision it cites, following inherited
/// dependencies transitively. Change-only references (subjects, scopes and
/// questions) are members but are not followed.
pub fn subject_read_set(state: &State, subject: &Record) -> Vec<RevisionRef> {
    let mut members = BTreeMap::<Id, RevisionRef>::new();
    members.insert(subject.header.id, subject.reference());
    let mut queue = vec![subject];
    let mut expanded = BTreeSet::new();
    while let Some(record) = queue.pop() {
        if !expanded.insert(record.header.id) || members.len() >= MAX_SUBJECT_READ_SET {
            continue;
        }
        for edge in dependency_edges(record).unwrap_or_default() {
            members.entry(edge.target.id).or_insert(edge.target);
            if edge.propagation == Propagation::Inherit
                && let Some(next) = state.current(edge.target.id)
                && next.header.case_id == subject.header.case_id
            {
                queue.push(next);
            }
        }
    }
    sorted(members.into_values().collect())
}

fn statement(findings: usize, checks: &[CheckRun]) -> String {
    let names = checks
        .iter()
        .map(|check| format!("{} ({})", check.check_id, check.check_version))
        .collect::<Vec<_>>()
        .join(", ");
    match findings {
        0 => format!("No structural gap identified under checks {names}."),
        1 => format!("1 structural gap identified under checks {names}."),
        count => format!("{count} structural gaps identified under checks {names}."),
    }
}

fn gap_detection(gap: &Gap) -> Detection {
    Detection {
        subject_refs: gap.subject_refs.clone(),
        related_refs: gap.read_set.clone(),
        facts: vec![FindingFact {
            fact: format!(
                "gap_status:{}",
                serde_json::to_value(gap.status)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default()
            ),
            refs: gap.subject_refs.clone(),
        }],
    }
}

fn refuse_subject(message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at("--subject")
}

/// Run every registered check over one verified snapshot of a case. The
/// caller supplies the snapshot of the replay that populated `state`.
pub fn audit(
    state: &State,
    snapshot: &Snapshot,
    case_id: Id,
    request: &AuditRequest,
) -> Result<AuditResponse, Failure> {
    let gaps = enumerate_gaps(state, snapshot, case_id)?;
    let freshness = FreshnessIndex::build(state, case_id);
    let context = AuditContext {
        state,
        snapshot,
        case_id,
        gaps: &gaps,
        freshness: &freshness,
    };
    let (subject_ref, subject_read_set) = match request.subject {
        None => (None, Vec::new()),
        Some(id) => {
            if !matches!(
                id.kind(),
                Kind::Conclusion | Kind::Comparison | Kind::Decision
            ) {
                return Err(refuse_subject(
                    "Narrow the audit to a conclusion, comparison or decision ID.",
                ));
            }
            let record = state
                .current(id)
                .filter(|record| record.header.case_id == case_id)
                .ok_or_else(|| {
                    Failure::new(
                        ErrorCode::UnknownReference,
                        "The subject is absent from this case at the audited snapshot.",
                    )
                    .at("--subject")
                })?;
            (Some(record.reference()), subject_read_set(state, record))
        }
    };
    let in_scope: Option<BTreeSet<Id>> = subject_ref.is_some().then(|| {
        subject_read_set
            .iter()
            .map(|reference| reference.id)
            .collect()
    });
    let classifier = Classifier::new(state);
    let policy = AudiencePolicy::default();
    let withheld = |reference: &RevisionRef| {
        state
            .revision(*reference)
            .or_else(|| state.current(reference.id))
            .is_some_and(|record| !classifier.may_show(record, Field::Title, &policy))
    };

    let registry = checks();
    let mut findings = Vec::new();
    let mut checks_run = Vec::new();
    for check in &registry {
        let detections: Vec<(Option<&Gap>, Detection)> = match check.source {
            CheckSource::GapRules { rules, .. } => gaps
                .gaps
                .iter()
                .filter(|gap| rules.matches(&gap.rule_id))
                .map(|gap| (Some(gap), gap_detection(gap)))
                .collect(),
            CheckSource::Native(evaluate) => evaluate(&context)?
                .into_iter()
                .map(|detection| (None, detection))
                .collect(),
        };
        let mut count = 0;
        for (gap, detection) in detections {
            let subject_refs = sorted(detection.subject_refs);
            if subject_refs.is_empty() {
                continue;
            }
            if let Some(scope) = &in_scope
                && !subject_refs
                    .iter()
                    .any(|reference| scope.contains(&reference.id))
            {
                continue;
            }
            let related_refs: Vec<RevisionRef> = sorted(detection.related_refs)
                .into_iter()
                .filter(|reference| !subject_refs.contains(reference))
                .collect();
            let mut facts = detection.facts;
            for fact in &mut facts {
                fact.refs = sorted(std::mem::take(&mut fact.refs));
            }
            facts.sort();
            facts.dedup();
            let withheld_refs = sorted(
                subject_refs
                    .iter()
                    .chain(&related_refs)
                    .chain(facts.iter().flat_map(|fact| &fact.refs))
                    .copied()
                    .filter(|reference| withheld(reference))
                    .collect(),
            );
            let gap_id = match gap {
                Some(gap) => gap.gap_id.clone(),
                None => format!("blindspot:{}:{}", check.check_id, ids_key(&subject_refs)),
            };
            findings.push(Finding {
                check_id: check.check_id.into(),
                check_version: check.version.into(),
                gap_id,
                rule_id: gap.map(|gap| gap.rule_id.clone()),
                rule_version: gap.map(|gap| gap.rule_version.clone()),
                state: FindingState::GapDetectedInRecordedStructure,
                severity: check.severity,
                severity_reason: check.severity_reason.into(),
                subject_refs,
                related_refs,
                facts,
                withheld_refs,
                why: check.why.into(),
                suggested_schema: check.suggested_schema.map(str::to_owned),
                guide: check.guide.map(str::to_owned),
                inspect_argvs: Vec::new(),
                schema_argv: None,
                guide_argv: None,
            });
            count += 1;
        }
        checks_run.push(CheckRun {
            check_id: check.check_id.into(),
            check_version: check.version.into(),
            source: source_view(check),
            subject_kinds: check
                .subject_kinds
                .iter()
                .map(|kind| kind_name(*kind))
                .collect(),
            reads: check.reads.iter().map(|read| (*read).to_owned()).collect(),
            condition: check.condition.into(),
            severity: check.severity,
            suggested_schema: check.suggested_schema.map(str::to_owned),
            guide: check.guide.map(str::to_owned),
            findings: count,
        });
    }
    findings.sort_by(|left, right| {
        (&left.gap_id, &left.check_id).cmp(&(&right.gap_id, &right.check_id))
    });
    findings.dedup_by(|later, earlier| later.gap_id == earlier.gap_id);
    let outcome = if findings.is_empty() {
        AuditOutcome::NoStructuralGapIdentifiedUnderChecks
    } else {
        AuditOutcome::GapsIdentifiedInRecordedStructure
    };
    Ok(AuditResponse {
        schema_version: AuditVersion::V1,
        snapshot: snapshot.clone(),
        check_version: CHECK_VERSION.into(),
        check_set_digest: check_set_digest(&registry),
        scope: AuditScope {
            case_id,
            subject_ref,
            subject_read_set,
            audience: policy.audience,
        },
        outcome,
        statement: statement(findings.len(), &checks_run),
        checks: findings,
        checks_run,
        limits: LIMITS.into(),
        follow_up_argvs: Vec::new(),
    })
}
