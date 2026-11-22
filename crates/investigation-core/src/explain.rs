//! Explanatory payloads. Runtime gates and transitions belong to W03.
pub use crate::evidence::EvidenceKind;
use crate::{
    command::{Reference, RevisionRef},
    header::{Actor, RecordHeader, SourceTime},
    wire::Digest,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HypothesisCreate {
    pub question_ref: Reference,
    pub statement: String,
    pub applicability: Option<String>,
    pub possible_falsifiers: Vec<String>,
    pub rival_refs: Vec<Reference>,
    pub assumption_refs: Vec<Reference>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisLifecycle {
    Proposed,
    Active,
    RejectedWithinScope,
    Retired,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Hypothesis {
    pub header: RecordHeader,
    pub content: HypothesisCreate,
    pub lifecycle: HypothesisLifecycle,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HypothesisDisposition {
    pub hypothesis_ref: RevisionRef,
    pub lifecycle: HypothesisLifecycle,
    pub rationale: String,
    pub evidence_refs: Vec<Reference>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssumptionStatus {
    Unassessed,
    Supported,
    Challenged,
    Rejected,
    Superseded,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Materiality {
    Material,
    Nonmaterial,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssumptionCreate {
    pub claim: String,
    pub consequence: String,
    pub materiality: Materiality,
    pub affected_refs: Vec<Reference>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Assumption {
    pub header: RecordHeader,
    pub content: AssumptionCreate,
    pub status: AssumptionStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RivalPrediction {
    pub rival_ref: Reference,
    pub predicted_outcome: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmatoryPlan {
    pub predictions: Vec<RivalPrediction>,
    pub evidence_holdout_boundary: String,
    pub prior_exposure: String,
    pub stop_analysis_rule: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "mode",
    content = "confirmatory",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum InquiryMode {
    Exploratory,
    Confirmatory(ConfirmatoryPlan),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InquiryLifecycle {
    Draft,
    Frozen,
    Retired,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InquiryCreate {
    pub purpose: String,
    pub method: String,
    pub controls: Vec<String>,
    pub limits: Vec<String>,
    pub desired_observations: Vec<String>,
    pub inquiry: InquiryMode,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InquiryPlan {
    pub header: RecordHeader,
    pub content: InquiryCreate,
    pub lifecycle: InquiryLifecycle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Declared,
    Running,
    Completed,
    Failed,
    Cancelled,
    Blocked,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceCompleteness {
    Complete,
    Partial,
    Missing,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunIntent {
    Plan { plan_ref: RevisionRef },
    Exploration { intent: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchCoverage {
    pub corpus: String,
    pub revision_bounds: Option<String>,
    pub queries: Vec<String>,
    pub filters: Vec<String>,
    pub inclusion_rules: Vec<String>,
    pub exclusion_rules: Vec<String>,
    pub total_hits: Option<u64>,
    pub examined_hits: Option<u64>,
    pub empty_hits: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub intent: RunIntent,
    pub producer: Actor,
    pub tool: Option<String>,
    pub environment: Option<String>,
    pub started_at: Option<SourceTime>,
    pub finished_at: Option<SourceTime>,
    pub result_assertion: Option<String>,
    pub artifact_refs: Vec<Reference>,
    pub manifest_digest: Option<Digest>,
    pub execution_status: ExecutionStatus,
    pub evidence_completeness: EvidenceCompleteness,
    pub evidence_kind: EvidenceKind,
    pub variables_changed: Vec<String>,
    pub search_coverage: Option<SearchCoverage>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub header: RecordHeader,
    pub content: RunRecord,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn confirmatory_requires_prediction_table() {
        let mut p = json!({"mode":"confirmatory", "confirmatory":{"evidence_holdout_boundary":"held out", "prior_exposure":"none", "stop_analysis_rule":"stop at ten"}});
        assert!(serde_json::from_value::<InquiryMode>(p.clone()).is_err());
        p["confirmatory"]["predictions"] = json!([]);
        assert!(serde_json::from_value::<InquiryMode>(p).is_ok());
        let schema = serde_json::to_string(&schemars::schema_for!(InquiryMode)).unwrap();
        assert!(schema.contains("predictions"));
    }
    #[test]
    fn execution_and_completeness_are_independent() {
        for (execution, evidence) in [("completed", "partial"), ("failed", "complete")] {
            let p = json!({"intent":{"kind":"exploration","intent":"inspect"},"producer":"agent:test","tool":null,"environment":null,"started_at":null,"finished_at":null,"result_assertion":null,"artifact_refs":[],"manifest_digest":null,"execution_status":execution,"evidence_completeness":evidence,"evidence_kind":"observational","variables_changed":[],"search_coverage":null});
            assert!(serde_json::from_value::<RunRecord>(p).is_ok());
        }
    }
}
