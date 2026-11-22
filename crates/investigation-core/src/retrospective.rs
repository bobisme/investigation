//! Method accounts and narrative annotations carry attribution on their record
//! headers. Interpretive relations identify the record that establishes them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    command::{Reference, RevisionRef},
    header::Classification,
    identity::Id,
    wire::{Digest, Snapshot},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AnnotationContent {
    Text {
        text: String,
    },
    Artifact {
        artifact_ref: Reference,
        digest: Digest,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnnotationAdd {
    pub subject_refs: Vec<Reference>,
    pub content: AnnotationContent,
    #[serde(default)]
    pub classification: Classification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetrospectiveStatus {
    Recorded,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MethodAttempt {
    pub name: String,
    pub inquiry_ref: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepAccount {
    pub description: String,
    pub support_refs: Vec<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposedLesson {
    pub guidance: String,
    pub applies_when: String,
    pub support_refs: Vec<Id>,
    pub transfer_limits: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetrospectiveRecord {
    pub case_ref: Reference,
    pub snapshot: Snapshot,
    pub methods_attempted: Vec<MethodAttempt>,
    pub effective_steps: Vec<StepAccount>,
    pub invalid_or_misleading_steps: Vec<StepAccount>,
    pub none_observed_reason: Option<String>,
    pub costs_or_constraints: String,
    pub residual_unknowns: Vec<String>,
    pub lessons: Vec<ProposedLesson>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetrospectiveSupersede {
    pub previous: RevisionRef,
    pub reason: String,
    pub successor: Reference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Addresses,
    AlternativeTo,
    CandidateFor,
    EvaluatedBy,
    Compares,
    Satisfies,
    Violates,
    DependsOnAssumption,
    DerivedFrom,
    ObservedIn,
    Tests,
    Supports,
    Contradicts,
    NonDiagnosticFor,
    Reviews,
    Supersedes,
    Retracts,
    InformsDecision,
    ExternalReference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Relation {
    pub source: RevisionRef,
    pub kind: RelationKind,
    pub target: RevisionRef,
    pub attributed_to: RevisionRef,
}

/// Opaque, read-only identifiers. Evaluating a reference never calls the provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExternalWorkItem {
    pub provider: String,
    pub key: String,
    pub url: Option<String>,
}
