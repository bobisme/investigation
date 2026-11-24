//! Sensitivity classification: defaults, propagation and reviewed downgrades.
//!
//! Every record carries a recorded label (`public | internal | restricted`,
//! default `internal`). Renderers and exporters must not read that label
//! directly. They ask a [`Classifier`] for the *effective* level, which also
//! accounts for:
//!
//! - derivation inputs: a derivative is at least as sensitive as what it was
//!   derived from (artifact transforms and source, an observation's artifact,
//!   an annotation's subjects and note artifact, and header provenance);
//! - a case raised above the default level, which raises every record in it;
//! - explicit `classification.raise` changes, which apply to every revision of
//!   the subject;
//! - a reviewed `classification.downgrade`, which applies only to the exact
//!   reviewed subject revision and stops applying if anything it covered later
//!   becomes stricter than the level the reviewer examined.
//!
//! Classification is handling policy for this tool's own outputs. It is not an
//! access-control boundary: anyone with the workspace can read the journal.
//! Only a reviewed transformation can lower a level, and pattern matching alone
//! never counts as that review.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    command::RevisionRef,
    evidence::{ArtifactSensitivity, SelectorRequest},
    identity::{Id, Kind},
    state::{Record, State},
    wire::Digest,
};

pub use crate::header::{Classification, ClassificationLevel};

/// Level applied to new cases and records unless a stricter one is requested.
pub const DEFAULT_LEVEL: ClassificationLevel = ClassificationLevel::Internal;

/// Annotation role that marks a classification change record.
pub const CHANGE_ROLE: &str = "classification";

/// The most sensitive level a reader may see. `internal` is the default
/// audience of `show`, `brief` and the projected vault.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    Public,
    #[default]
    Internal,
    Restricted,
}

impl Audience {
    pub const fn clearance(self) -> ClassificationLevel {
        match self {
            Self::Public => ClassificationLevel::Public,
            Self::Internal => ClassificationLevel::Internal,
            Self::Restricted => ClassificationLevel::Restricted,
        }
    }

    pub fn permits(self, level: ClassificationLevel) -> bool {
        level <= self.clearance()
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Internal => "internal",
            Self::Restricted => "restricted",
        }
    }
}

/// How an output channel treats records the audience may not see.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AudiencePolicy {
    pub audience: Audience,
    /// When true, a withheld record may still appear as an ID-only placeholder.
    /// Audience-facing exports set this to false: IDs and the existence of an
    /// omission need permission too.
    pub placeholders: bool,
}

impl Default for AudiencePolicy {
    fn default() -> Self {
        Self {
            audience: Audience::default(),
            placeholders: true,
        }
    }
}

impl AudiencePolicy {
    /// Policy for an audience-facing bundle: nothing withheld is mentioned.
    pub const fn strict(audience: Audience) -> Self {
        Self {
            audience,
            placeholders: false,
        }
    }
}

/// Channels of one record that a renderer may emit. A sensitive title, path,
/// label or lineage can leak even when the bytes are omitted, so each channel
/// is asked for separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    /// The record ID and type, alone.
    Identity,
    /// Title, aliases, labels, filenames derived from text.
    Title,
    /// Bounded synopsis and typed summary fields.
    Summary,
    /// Full body, excerpts and retained bytes.
    Content,
    /// Source names, producer metadata, digests, sizes and paths.
    Metadata,
    /// References to the records this one was derived from.
    Lineage,
}

/// Which factor decided a record's effective level.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClassificationBasis {
    Recorded,
    CaseFloor { case_ref: RevisionRef },
    Inherited { input_ref: RevisionRef },
    Raised { change_ref: RevisionRef },
    ReviewedDowngrade { change_ref: RevisionRef },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectiveClassification {
    pub level: ClassificationLevel,
    /// The record's own label (for an artifact, the stricter of its header
    /// label and recorded sensitivity).
    pub recorded: ClassificationLevel,
    pub basis: ClassificationBasis,
    /// A reviewed downgrade of this revision that no longer applies because
    /// something it covered became stricter after the review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_downgrade: Option<RevisionRef>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeDirection {
    Raise,
    Downgrade,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RedactionAction {
    Removed,
    Masked,
    Generalized,
    /// Kept unchanged after review; the rationale says why it is safe.
    Retained,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactionDecision {
    /// What the decision covers, e.g. a field, region or kind of content.
    #[schemars(length(min = 1, max = 512))]
    pub scope: String,
    pub action: RedactionAction,
    #[schemars(length(min = 1, max = 2048))]
    pub rationale: String,
}

/// How thoroughly the reviewer examined the transformed output.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewCoverage {
    /// The reviewer examined the whole transformed output.
    WholeOutput,
    /// Only automated pattern matching ran. Never sufficient for a downgrade;
    /// listed so producers can report it honestly and receive a repair.
    PatternMatchingOnly,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransformationInputRecord {
    pub input_ref: RevisionRef,
    pub digest: Option<Digest>,
    /// Effective level of the input when the review was accepted.
    pub level: ClassificationLevel,
}

/// Persisted reviewed transformation with exact, resolved references.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewedTransformationRecord {
    pub inputs: Vec<TransformationInputRecord>,
    pub transformation: String,
    pub version: String,
    pub selectors: Vec<SelectorRequest>,
    pub redaction_decisions: Vec<RedactionDecision>,
    pub output_digest: Option<Digest>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DowngradeReviewRecord {
    pub method: String,
    pub coverage: ReviewCoverage,
    pub examined: Vec<RevisionRef>,
    pub limitations: Vec<String>,
}

/// Body of a classification change, stored under `classification_change` in
/// an annotation with role `classification`. The reviewer is the change
/// record's attributed actor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassificationChange {
    pub direction: ChangeDirection,
    pub subject_ref: RevisionRef,
    /// Effective level of the subject when the change was accepted.
    pub from: ClassificationLevel,
    pub to: ClassificationLevel,
    /// Level from the subject's own label, case floor and inputs, before any
    /// change, when this change was accepted. A downgrade lapses once that
    /// level rises above this value.
    pub base_level: ClassificationLevel,
    pub reason: String,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transformation: Option<ReviewedTransformationRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<DowngradeReviewRecord>,
    /// A downgrade: the write-side guardrails it satisfied, when a pinned
    /// pack declares any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardrail: Option<crate::guardrails::GuardrailRecord>,
}

impl ClassificationChange {
    /// Decode a classification change record, or `None` for any other record.
    pub fn from_record(record: &Record) -> Option<Self> {
        if record.header.id.kind() != Kind::Annotation
            || record.body.get("role").and_then(Value::as_str) != Some(CHANGE_ROLE)
        {
            return None;
        }
        serde_json::from_value(record.body.get("classification_change")?.clone()).ok()
    }
}

/// Exact records a record was derived from. Classification flows from each of
/// them to the record. Claims that merely cite evidence (conclusions,
/// decisions, comparisons) are not derivatives; their audience treatment is a
/// caveat decision made on the full snapshot before filtering.
pub fn derivation_inputs(state: &State, record: &Record) -> Vec<RevisionRef> {
    let mut inputs = BTreeSet::new();
    let mut add = |value: &Value| {
        if let Ok(reference) = serde_json::from_value::<RevisionRef>(value.clone()) {
            inputs.insert(reference);
        }
    };
    let body = &record.body;
    match record.header.id.kind() {
        Kind::Artifact => {
            add(&body["source"]);
            for transform in body["transforms"].as_array().into_iter().flatten() {
                add(&transform["input"]);
            }
        }
        Kind::Observation => add(&body["basis"]["artifact_ref"]),
        Kind::Annotation => {
            for subject in body["subject_refs"].as_array().into_iter().flatten() {
                add(subject);
            }
            add(&body["content"]["artifact_ref"]);
        }
        _ => {}
    }
    for id in &record.header.provenance.references {
        if let Some(input) = state.record_at(*id, record.header.acceptance_sequence) {
            inputs.insert(input.reference());
        }
    }
    inputs.remove(&record.reference());
    inputs.into_iter().collect()
}

/// The recorded label: header level, and for artifacts also body sensitivity.
pub fn recorded_level(record: &Record) -> ClassificationLevel {
    let header = record.header.classification.level;
    if record.header.id.kind() != Kind::Artifact {
        return header;
    }
    let sensitivity = serde_json::from_value::<ArtifactSensitivity>(
        record
            .body
            .get("sensitivity")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map(|sensitivity| match sensitivity {
        ArtifactSensitivity::Public => ClassificationLevel::Public,
        ArtifactSensitivity::Internal => ClassificationLevel::Internal,
        ArtifactSensitivity::Restricted => ClassificationLevel::Restricted,
    })
    // An unreadable label is treated as the strictest, never the most open.
    .unwrap_or(ClassificationLevel::Restricted);
    header.max(sensitivity)
}

struct ChangeEntry {
    sequence: u64,
    change_ref: RevisionRef,
    change: ClassificationChange,
}

/// The classification changes of one snapshot, by subject, oldest first.
/// A change counts only when its subject is in the change's own case.
pub(crate) struct ClassificationChanges(BTreeMap<Id, Vec<ChangeEntry>>);

impl ClassificationChanges {
    pub(crate) fn index(state: &State) -> Self {
        let mut changes: BTreeMap<Id, Vec<ChangeEntry>> = BTreeMap::new();
        for record in state.records() {
            if let Some(change) = ClassificationChange::from_record(record)
                && state
                    .current(change.subject_ref.id)
                    .is_some_and(|subject| subject.header.case_id == record.header.case_id)
            {
                changes
                    .entry(change.subject_ref.id)
                    .or_default()
                    .push(ChangeEntry {
                        sequence: record.header.acceptance_sequence,
                        change_ref: record.reference(),
                        change,
                    });
            }
        }
        for entries in changes.values_mut() {
            entries.sort_by_key(|entry| (entry.sequence, entry.change_ref.id));
        }
        Self(changes)
    }
}

/// Answers classification queries over one state snapshot. Build it once per
/// rendering pass; results are memoized per exact revision.
pub struct Classifier<'a> {
    state: &'a State,
    changes: std::sync::Arc<ClassificationChanges>,
    memo: RefCell<BTreeMap<RevisionRef, EffectiveClassification>>,
    visiting: RefCell<BTreeSet<RevisionRef>>,
}

impl<'a> Classifier<'a> {
    /// Cheap: the classification changes of the snapshot are indexed once
    /// per state ([`State::classification_changes`]) and shared.
    pub fn new(state: &'a State) -> Self {
        Self {
            state,
            changes: state.classification_changes(),
            memo: RefCell::default(),
            visiting: RefCell::default(),
        }
    }

    pub fn state(&self) -> &'a State {
        self.state
    }

    /// Effective classification of this exact record revision.
    pub fn effective(&self, record: &Record) -> EffectiveClassification {
        let reference = record.reference();
        if let Some(known) = self.memo.borrow().get(&reference) {
            return known.clone();
        }
        let recorded = recorded_level(record);
        if !self.visiting.borrow_mut().insert(reference) {
            // References point backwards in acceptance order; a cycle means a
            // corrupt image. Fail closed for this path.
            return EffectiveClassification {
                level: ClassificationLevel::Restricted,
                recorded,
                basis: ClassificationBasis::Recorded,
                stale_downgrade: None,
            };
        }
        let result = self.compute(record, recorded);
        self.visiting.borrow_mut().remove(&reference);
        self.memo.borrow_mut().insert(reference, result.clone());
        result
    }

    /// Effective classification of an exact revision, if it exists.
    pub fn effective_ref(&self, reference: RevisionRef) -> Option<EffectiveClassification> {
        self.state
            .revision(reference)
            .map(|record| self.effective(record))
    }

    /// Level from the record's own label, a raised case and its derivation
    /// inputs, before classification changes of the record itself.
    pub fn base_level(&self, record: &Record) -> ClassificationLevel {
        self.base(record, recorded_level(record)).0
    }

    fn base(
        &self,
        record: &Record,
        recorded: ClassificationLevel,
    ) -> (ClassificationLevel, ClassificationBasis) {
        let mut level = recorded;
        let mut basis = ClassificationBasis::Recorded;
        let mut raise = |candidate: ClassificationLevel, why: ClassificationBasis| {
            if candidate > level {
                level = candidate;
                basis = why;
            }
        };
        if record.header.id.kind() != Kind::Case
            && let Some(case) = self.state.current(record.header.case_id)
        {
            let floor = self.effective(case).level;
            if floor > DEFAULT_LEVEL {
                raise(
                    floor,
                    ClassificationBasis::CaseFloor {
                        case_ref: case.reference(),
                    },
                );
            }
        }
        for input in derivation_inputs(self.state, record) {
            if let Some(effective) = self.effective_ref(input) {
                raise(
                    effective.level,
                    ClassificationBasis::Inherited { input_ref: input },
                );
            }
        }
        (level, basis)
    }

    fn compute(&self, record: &Record, recorded: ClassificationLevel) -> EffectiveClassification {
        let (mut level, mut basis) = self.base(record, recorded);
        let entries = self
            .changes
            .0
            .get(&record.header.id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let reference = record.reference();
        let downgrade = entries.iter().rev().find(|entry| {
            entry.change.direction == ChangeDirection::Downgrade
                && entry.change.subject_ref == reference
        });
        let mut stale_downgrade = None;
        let mut raises_after = 0;
        if let Some(entry) = downgrade {
            if level <= entry.change.base_level {
                level = entry.change.to;
                basis = ClassificationBasis::ReviewedDowngrade {
                    change_ref: entry.change_ref,
                };
                raises_after = entry.sequence;
            } else {
                stale_downgrade = Some(entry.change_ref);
            }
        }
        for entry in entries {
            if entry.change.direction == ChangeDirection::Raise
                && entry.sequence > raises_after
                && entry.change.to > level
            {
                level = entry.change.to;
                basis = ClassificationBasis::Raised {
                    change_ref: entry.change_ref,
                };
            }
        }
        EffectiveClassification {
            level,
            recorded,
            basis,
            stale_downgrade,
        }
    }

    /// Whether the record's whole content may be shown to this audience.
    pub fn permits(&self, record: &Record, audience: Audience) -> bool {
        audience.permits(self.effective(record).level)
    }

    /// Answer "may this field of this record be shown to this audience?".
    /// A record also counts as withheld while its current revision is
    /// withheld, so history cannot leak what the head hides.
    pub fn may_show(&self, record: &Record, field: Field, policy: &AudiencePolicy) -> bool {
        let visible = self.permits(record, policy.audience)
            && self
                .state
                .current(record.header.id)
                .is_none_or(|current| self.permits(current, policy.audience));
        match field {
            Field::Identity => visible || policy.placeholders,
            Field::Title | Field::Summary | Field::Content | Field::Metadata => visible,
            Field::Lineage => {
                visible
                    && (policy.placeholders
                        || derivation_inputs(self.state, record)
                            .into_iter()
                            .all(|input| {
                                self.state
                                    .revision(input)
                                    .is_some_and(|input| self.permits(input, policy.audience))
                            }))
            }
        }
    }

    /// Records whose effective level is above the given level, among the
    /// derivation inputs of `record`. A reviewed downgrade to `level` must
    /// list every one of them.
    pub fn inputs_above(&self, record: &Record, level: ClassificationLevel) -> Vec<RevisionRef> {
        derivation_inputs(self.state, record)
            .into_iter()
            .filter(|input| {
                self.effective_ref(*input)
                    .is_some_and(|effective| effective.level > level)
            })
            .collect()
    }

    /// Classification changes recorded for a record ID, oldest first.
    pub fn changes(&self, id: Id) -> impl Iterator<Item = (RevisionRef, &ClassificationChange)> {
        self.changes
            .0
            .get(&id)
            .into_iter()
            .flatten()
            .map(|entry| (entry.change_ref, &entry.change))
    }
}

/// Convenience for one-off queries. Prefer one [`Classifier`] per pass.
pub fn effective_classification(state: &State, record: &Record) -> EffectiveClassification {
    Classifier::new(state).effective(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_order_from_open_to_strict_and_default_is_internal() {
        assert!(ClassificationLevel::Public < ClassificationLevel::Internal);
        assert!(ClassificationLevel::Internal < ClassificationLevel::Restricted);
        assert_eq!(ClassificationLevel::default(), DEFAULT_LEVEL);
        assert_eq!(Audience::default(), Audience::Internal);
        assert!(Audience::Internal.permits(ClassificationLevel::Public));
        assert!(!Audience::Internal.permits(ClassificationLevel::Restricted));
        assert!(!Audience::Public.permits(ClassificationLevel::Internal));
        assert!(AudiencePolicy::default().placeholders);
        assert!(!AudiencePolicy::strict(Audience::Public).placeholders);
    }
}
