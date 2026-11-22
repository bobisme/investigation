//! Evaluation payloads. Runtime validity and acceptance gates belong to W04.
use crate::{
    command::{Reference, RevisionRef},
    header::{Actor, RecordHeader},
    identity::Id,
    input::Measurement,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OptionLifecycle {
    Proposed,
    Eligible,
    RuledOut,
    Withdrawn,
    Superseded,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OptionCreate {
    pub question_ref: Reference,
    pub description: String,
    pub context: Option<String>,
    pub feasibility: Option<String>,
    pub dependencies: Vec<String>,
    pub tradeoff_notes: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OptionDisposition {
    pub option_ref: RevisionRef,
    pub lifecycle: OptionLifecycle,
    pub reason: String,
    pub criterion_ref: Option<Reference>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OptionRecord {
    pub header: RecordHeader,
    pub content: OptionCreate,
    pub lifecycle: OptionLifecycle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CriterionKind {
    Hard,
    Soft,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CriterionLifecycle {
    Draft,
    Active,
    Retired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreferredDirection {
    Higher,
    Lower,
    Target,
    Unordered,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScaleAnchor {
    pub label: String,
    pub meaning: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CriterionScale {
    pub unit: Option<String>,
    pub description: String,
    pub anchors: Vec<ScaleAnchor>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CriterionScope {
    AllOptions {
        question_ref: Reference,
    },
    SelectedOptions {
        question_ref: Reference,
        option_refs: Vec<Reference>,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdOperator {
    LessThan,
    AtMost,
    Equal,
    AtLeast,
    GreaterThan,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Threshold {
    Measured {
        operator: ThresholdOperator,
        value: Measurement,
    },
    Qualitative {
        requirement: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CriterionCreate {
    pub name: String,
    pub kind: CriterionKind,
    pub scope: CriterionScope,
    pub scale: CriterionScale,
    pub preferred_direction: PreferredDirection,
    pub threshold: Option<Threshold>,
    pub weighting_rationale: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Criterion {
    pub header: RecordHeader,
    pub content: CriterionCreate,
    pub lifecycle: CriterionLifecycle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonOutcome {
    Meets,
    Fails,
    PartiallyMeets,
    Unknown,
    NotApplicable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonLifecycle {
    Draft,
    Assessed,
    Challenged,
    Superseded,
}
/// An explicit qualitative preference is not an objective measurement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComparisonBasis {
    Preference { statement: String },
    Qualitative { statement: String },
    Unknown { reason: String },
}
/// Retains the flat §9.4 wire example. Validate value/basis combinations at the W04 gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparisonAdd {
    pub option_ref: RevisionRef,
    pub criterion_ref: RevisionRef,
    pub outcome: ComparisonOutcome,
    pub method: String,
    pub value: Option<Measurement>,
    pub basis: Option<ComparisonBasis>,
    pub evidence_refs: Vec<Id>,
    pub assumption_refs: Vec<Id>,
    pub limitations: Option<String>,
    pub actor: Option<Actor>,
    pub uncertainty: String,
    pub rationale: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Comparison {
    pub header: RecordHeader,
    pub content: ComparisonAdd,
    pub lifecycle: ComparisonLifecycle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionLifecycle {
    Proposed,
    Accepted,
    Superseded,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionChoice {
    Select { option_ref: Reference },
    Defer { reason: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionPropose {
    pub question_ref: Reference,
    pub choice: DecisionChoice,
    pub compared_alternatives: Vec<Reference>,
    pub criteria_set_version: String,
    pub evidence_refs: Vec<Reference>,
    pub assumption_refs: Vec<Reference>,
    pub tradeoffs: Vec<String>,
    pub consequences: Vec<String>,
    pub reopen_trigger: String,
    pub expected_outcome: Option<String>,
    pub check_when: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub header: RecordHeader,
    pub content: DecisionPropose,
    pub lifecycle: DecisionLifecycle,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn plan_comparison_example_decodes() {
        let p = json!({"option_ref":{"id":"opt_01890f3e-7c00-7000-8000-000000000001","revision":2},"criterion_ref":{"id":"crit_01890f3e-7c00-7000-8000-000000000002","revision":1},"outcome":"partially_meets","method":"controlled_benchmark","value":{"decimal":"18.7","unit":"ms","statistic":"p99","sample_count":1200},"evidence_refs":["obs_01890f3e-7c00-7000-8000-000000000003"],"assumption_refs":[],"uncertainty":"Measured on the seeded workload; production mix unknown","rationale":"Below the 25ms preferred target in this fixture"});
        let parsed: ComparisonAdd = serde_json::from_value(p.clone()).unwrap();
        assert_eq!(parsed.value.unwrap().decimal.as_str(), "18.7");
        let mut invalid = p;
        invalid["surprise"] = json!(true);
        assert!(serde_json::from_value::<ComparisonAdd>(invalid).is_err());
        let schema = serde_json::to_string(&schemars::schema_for!(ComparisonAdd)).unwrap();
        assert!(schema.contains("option_ref") && schema.contains("criterion_ref"));
    }
    #[test]
    fn defer_and_preference_are_explicit() {
        assert!(
            serde_json::from_value::<DecisionChoice>(json!({"kind":"defer","reason":"await data"}))
                .is_ok()
        );
        assert!(
            serde_json::from_value::<ComparisonBasis>(
                json!({"kind":"preference","statement":"prefer familiar tools"})
            )
            .is_ok()
        );
        assert!(
            serde_json::from_value::<ComparisonBasis>(
                json!({"kind":"unknown","reason":"not measured"})
            )
            .is_ok()
        );
    }
}
