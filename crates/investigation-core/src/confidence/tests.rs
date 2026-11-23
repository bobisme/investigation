use serde_json::json;

use super::*;
use crate::{catalog::standard_catalog, wire::ErrorCode};

const OBSERVATION: &str = "obs_01890f3e-7c00-7000-8000-000000000001";

fn conclusion(confidence: Value) -> Value {
    json!({
        "statement":"The reading is 12.5 units.","scope":"This reading.",
        "significance":"Reuse with care.","support_refs":[{"id":OBSERVATION,"revision":1}],
        "contradiction_refs":[],"caveats":["One reading."],"confidence":confidence
    })
}

fn confidence(level: &str, reasons: Value) -> Value {
    json!({"level":level,"rationale":"One calibrated reading.","limitations":[],
        "what_would_change_it":null,"downgrade_reasons":reasons})
}

fn all(judgment: &str) -> Value {
    json!({"validity":judgment,"consistency":judgment,"directness":judgment,
        "precision":judgment,"completeness":judgment})
}

fn decode_error(op: &str, payload: &Value) -> Failure {
    standard_catalog()
        .get(op)
        .unwrap()
        .validate_payload(payload)
        .unwrap_err()
}

#[test]
fn numeric_confidence_is_rejected_with_a_pointer_to_likelihood() {
    for (op, payload, path) in [
        ("conclusion.create", conclusion(json!(0.8)), "/confidence"),
        (
            "conclusion.create",
            conclusion(json!({"level":0.8,"rationale":"x","limitations":[],
                "what_would_change_it":null,"downgrade_reasons":null})),
            "/confidence/level",
        ),
        (
            "conclusion.create",
            conclusion(confidence("80%", Value::Null)),
            "/confidence/level",
        ),
        (
            "conclusion.amend",
            json!({"conclusion_ref":{"id":"concl_01890f3e-7c00-7000-8000-000000000002","revision":1},
                "replacement":conclusion(json!("0.9")),"reason":"Revised."}),
            "/replacement/confidence",
        ),
    ] {
        let failure = decode_error(op, &payload);
        assert_eq!(failure.error.code, ErrorCode::InvalidInput, "{op}");
        assert_eq!(failure.error.path.as_deref(), Some(path), "{op}");
        assert!(
            failure
                .error
                .message
                .contains("never a number or probability")
        );
        assert!(failure.error.message.contains("likelihood"));
        assert_eq!(
            failure.error.repair.as_ref().unwrap().schema.as_deref(),
            Some(op)
        );
    }
    // Other decode failures keep the generic message.
    let failure = decode_error("conclusion.create", &conclusion(json!({"level":"certain"})));
    assert_eq!(
        failure.error.message,
        "Field does not match the expected schema"
    );
    // Ordinal labels decode.
    for level in ["low", "moderate", "high", "not_assessed"] {
        standard_catalog()
            .get("conclusion.create")
            .unwrap()
            .validate_payload(&conclusion(confidence(level, Value::Null)))
            .unwrap();
    }
}

#[test]
fn moderate_or_high_needs_named_reasons_and_not_assessed_is_always_acceptable() {
    let parse = |value: Value| -> Confidence { serde_json::from_value(value).unwrap() };
    assert_eq!(requirement(None), PromotionRequirement::NotRequired);
    for level in ["low", "not_assessed"] {
        assert_eq!(
            requirement(Some(&parse(confidence(level, Value::Null)))),
            PromotionRequirement::NotRequired
        );
    }
    for level in ["moderate", "high"] {
        assert_eq!(
            requirement(Some(&parse(confidence(level, Value::Null)))),
            PromotionRequirement::MissingDowngradeReasons
        );
        assert_eq!(
            requirement(Some(&parse(confidence(level, all("not_assessed"))))),
            PromotionRequirement::Met
        );
    }
}

#[test]
fn text_names_labels_and_reasons_never_numbers() {
    let view = json!({"confidence":{
        "level":"high","promotion_requirement":"met",
        "concerns":[
            {"reason":"consistency","recorded":"concern","computed":["single_origin"]},
        ],
        "advisories":[{"code":"label_above_named_concerns","ceiling":"moderate","message":"m"}]
    }});
    let line = text(&view);
    assert_eq!(
        line,
        "\nConfidence: high; consistency concern (recorded; computed: single_origin)\nConfidence advisory label_above_named_concerns (ceiling moderate)"
    );
    assert!(!line.chars().any(|c| c.is_ascii_digit() || c == '%'));
    assert_eq!(text(&json!({"title":"no confidence"})), "");
}
