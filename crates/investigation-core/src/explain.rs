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
    /// Rival hypotheses on the same question, by exact revision or by the
    /// client_ref of an earlier operation in this transaction; an alias of a
    /// later operation is refused. A rivalry recorded once counts in both
    /// directions, so rivals created together each name only those before
    /// them.
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
    /// A practical-equivalence specification stated before results. Freezing
    /// the plan fixes the margin. See `absence_gate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equivalence: Option<crate::absence_gate::EquivalencePlan>,
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
    /// Collections, partitions or sections of the corpus that were searchable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<String>,
    /// Date or edition range the searchable corpus covered, as stated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_range: Option<String>,
    /// Result count per exact query. `hits: 0` is an explicit empty result;
    /// an absent `hits` was not recorded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query_results: Vec<QueryResult>,
    /// What was deliberately or necessarily not searched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_searched: Vec<String>,
}
/// Result count for one exact query, stored as the recorder stated it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryResult {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hits: Option<u64>,
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
    /// Optional typed measurement fields; see `measurement_fields`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<crate::measurement_fields::MeasurementFields>,
    /// Identity of this Run as one repeat of a measurement procedure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repetition: Option<crate::measurement_fields::Repetition>,
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
