//! Attributed interpretations and scoped reviews. These payloads record judgments;
//! promotion and freshness are evaluated against the case state.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    command::{Reference, RevisionRef},
    header::Knowledge,
    wire::{Digest, Snapshot},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentRelation {
    Supports,
    Contradicts,
    NonDiagnostic,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Expectation {
    MoreExpected,
    EquallyExpected,
    LessExpected,
    NotAssessed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RivalExpectation {
    pub rival_ref: Reference,
    pub expectation: Expectation,
    pub rationale: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceLevel {
    Low,
    Moderate,
    High,
    NotAssessed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DowngradeJudgment {
    Ok,
    Concern,
    NotAssessed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DowngradeReasons {
    pub validity: DowngradeJudgment,
    pub consistency: DowngradeJudgment,
    pub directness: DowngradeJudgment,
    pub precision: DowngradeJudgment,
    pub completeness: DowngradeJudgment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Confidence {
    pub level: ConfidenceLevel,
    pub rationale: String,
    pub limitations: Vec<String>,
    pub what_would_change_it: Option<String>,
    pub downgrade_reasons: Option<DowngradeReasons>,
}

/// Ordinal labels from the first row of ICD 203 §D.6.e(2)(a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LikelihoodTerm {
    AlmostNoChance,
    VeryUnlikely,
    Unlikely,
    RoughlyEvenChance,
    Likely,
    VeryLikely,
    AlmostCertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Probability(f64);

impl TryFrom<f64> for Probability {
    type Error = &'static str;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        if (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err("Probability must be between 0 and 1 inclusive")
        }
    }
}

impl From<Probability> for f64 {
    fn from(value: Probability) -> Self {
        value.0
    }
}

impl JsonSchema for Probability {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Probability".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type":"number", "minimum":0, "maximum":1})
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ResolutionCondition(String);

impl TryFrom<String> for ResolutionCondition {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.trim().is_empty() {
            Err("Likelihood requires a stated resolution condition")
        } else {
            Ok(Self(value))
        }
    }
}

impl From<ResolutionCondition> for String {
    fn from(value: ResolutionCondition) -> Self {
        value.0
    }
}

impl JsonSchema for ResolutionCondition {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ResolutionCondition".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type":"string", "pattern":"\\S"})
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum LikelihoodEstimate {
    Number(Probability),
    Ordinal(LikelihoodTerm),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Likelihood {
    pub estimate: LikelihoodEstimate,
    pub resolution_condition: ResolutionCondition,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssessmentAdd {
    pub subject_ref: Reference,
    pub evidence_refs: Vec<Reference>,
    pub relation: AssessmentRelation,
    #[serde(default)]
    pub rival_expectations: Vec<RivalExpectation>,
    pub rationale: String,
    #[serde(default)]
    pub assumption_refs: Vec<Reference>,
    pub confidence: Option<Confidence>,
    pub likelihood: Option<Likelihood>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConclusionStatus {
    Candidate,
    ReviewReady,
    Validated,
    Rejected,
    Retracted,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveDisposition {
    Current,
    NeedsReview,
    Disputed,
    Conditional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FactorRole {
    Trigger,
    LatentCondition,
    Contributor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CausalFactor {
    pub hypothesis_ref: Reference,
    pub role: FactorRole,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConclusionCreate {
    pub statement: String,
    pub scope: String,
    pub significance: String,
    pub support_refs: Vec<Reference>,
    pub contradiction_refs: Vec<Reference>,
    pub caveats: Vec<String>,
    #[serde(default)]
    pub causal_factors: Vec<CausalFactor>,
    pub confidence: Option<Confidence>,
    pub likelihood: Option<Likelihood>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextIsolation {
    SameSessionSelfReview,
    FreshContextUnblinded,
    FreshContextBlinded,
    ExternalEvidenceBacked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Dependence {
    Shared,
    PartlyShared,
    IndependentOnStatedDimensions,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewContext {
    pub saw_original_conclusion: Knowledge<bool>,
    pub gathered_new_evidence: Knowledge<bool>,
    pub method_or_source_changes: Vec<String>,
    pub activity: String,
    pub dependencies: Dependence,
    pub dependence_basis: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Submitted,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValidationVerdict {
    Supports,
    Objects,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlindspotVerdict {
    GapsIdentified,
    NoGapIdentifiedWithinScope,
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewSubmit {
    Validation {
        subject_ref: Reference,
        basis_digest: Digest,
        read_set: Vec<RevisionRef>,
        method: String,
        materials_examined: Vec<Reference>,
        shared_dependencies: Vec<String>,
        objection_refs: Vec<Reference>,
        verdict: ValidationVerdict,
        caveats: Vec<String>,
        context_isolation: ContextIsolation,
        context: ReviewContext,
    },
    Blindspot {
        subject_ref: Reference,
        audited_snapshot: Snapshot,
        read_set: Vec<RevisionRef>,
        method: String,
        scope_examined: String,
        checks_considered: Vec<String>,
        gaps: Vec<String>,
        no_gap_reason: Option<String>,
        evidence_refs: Vec<Reference>,
        assumption_refs: Vec<Reference>,
        limitations: Vec<String>,
        verdict: BlindspotVerdict,
        context_isolation: ContextIsolation,
        context: ReviewContext,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObjectionSeverity {
    Critical,
    Material,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObjectionStatus {
    Open,
    Resolved,
    Withdrawn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectionRaise {
    pub subject_ref: Reference,
    pub statement: String,
    pub scope: String,
    pub severity: ObjectionSeverity,
    pub review_ref: Option<Reference>,
    pub evidence_refs: Knowledge<Vec<Reference>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectionChange {
    pub objection_ref: RevisionRef,
    pub reason: String,
    pub evidence_refs: Vec<Reference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConclusionValidate {
    pub conclusion_ref: RevisionRef,
    pub review_ref: Reference,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn likelihood_requires_a_nonempty_resolution_condition() {
        for value in [
            json!({"estimate":0.7}),
            json!({"estimate":0.7,"resolution_condition":"  \n"}),
            json!({"estimate":1.01,"resolution_condition":"Outcome observed"}),
        ] {
            assert!(serde_json::from_value::<Likelihood>(value).is_err());
        }
        assert!(
            serde_json::from_value::<Likelihood>(json!({
                "estimate":"roughly_even_chance",
                "resolution_condition":"The next predeclared check settles the claim"
            }))
            .is_ok()
        );
    }

    #[test]
    fn blindspot_review_cannot_use_a_validation_verdict() {
        let mut value = json!({
            "kind":"blindspot",
            "subject_ref":{"id":"case_019acd16-0000-7000-8000-000000000001","revision":1},
            "audited_snapshot":{"sequence":2,"head_hash":format!("sha256:{}","1".repeat(64))},
            "read_set":[],
            "method":"Trace the cited inputs",
            "scope_examined":"Only the stated workload",
            "checks_considered":["workload coverage"],
            "gaps":["No passive workload samples"],
            "evidence_refs":[],
            "assumption_refs":[],
            "limitations":[],
            "verdict":"gaps_identified",
            "context_isolation":"fresh_context_blinded",
            "context":{
                "saw_original_conclusion":{"state":"known","value":false},
                "gathered_new_evidence":{"state":"known","value":false},
                "method_or_source_changes":[],
                "activity":"Review",
                "dependencies":"unknown",
                "dependence_basis":"No comparison of investigators' prior context"
            }
        });
        assert!(serde_json::from_value::<ReviewSubmit>(value.clone()).is_ok());
        value["verdict"] = json!("supports");
        assert!(serde_json::from_value::<ReviewSubmit>(value).is_err());
    }
}
