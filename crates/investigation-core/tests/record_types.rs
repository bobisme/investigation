use investigation_core::{
    case::{CaseBlocker, CaseClose, CaseMode},
    evidence::{ArtifactAvailability, ArtifactIntegrity, EvidenceSelector, SelectorValidation},
};
use serde_json::{Value, json};

#[test]
fn all_selector_types_are_versioned_and_keep_unavailable_distinct() {
    let digest = format!("sha256:{}", "1".repeat(64));
    let selectors = [
        json!({"type":"whole"}),
        json!({"type":"byte_range","start":0,"end":3}),
        json!({"type":"text_lines","start_line":1,"end_line":3,"encoding":"utf-8","parser_version":"test-v1"}),
        json!({"type":"text_quote","exact":"workload"}),
        json!({"type":"json_pointer","pointer":"/observation"}),
        json!({"type":"table_cell","table":"requests","row_index":1,"column":"elapsed","extraction_version":"test-v1"}),
        json!({"type":"time_window","start":"-0.1","end":"8.00","unit":"ms","coordinate_origin":"capture-start","channel":"time"}),
        json!({"type":"code_span","repository":"synthetic","commit":"fixture-revision","blob_digest":digest,"path":"sample.txt","start_line":1,"end_line":1}),
        json!({"type":"external_selector","namespace":"test.example","schema_uri":"urn:test:sample","schema_version":"1","value":{"sample":3}}),
    ];

    for selector in selectors {
        let kind = selector["type"].clone();
        let value = json!({
            "schema_version":"inv.selector.v1",
            "artifact_digest":digest,
            "selector":selector,
            "validation":"unavailable"
        });
        let accepted: EvidenceSelector = serde_json::from_value(value).unwrap();
        accepted.selector.validate_shape().unwrap();
        assert_eq!(accepted.validation, SelectorValidation::Unavailable);
        let serialized = serde_json::to_value(accepted).unwrap();
        assert_eq!(serialized["schema_version"], "inv.selector.v1");
        assert_eq!(serialized["selector"]["type"], kind);
    }
    assert_eq!(
        serde_json::to_value(ArtifactAvailability::Missing).unwrap(),
        "missing"
    );
    assert_eq!(
        serde_json::to_value(ArtifactIntegrity::Unavailable).unwrap(),
        "unavailable"
    );
}

#[test]
fn material_closure_requires_a_retrospective_in_every_mode() {
    let mut request = json!({
        "outcome":"inconclusive",
        "reason":"No competing explanations remain testable",
        "unresolved_questions":[],
        "reopen_condition":"New source material arrives",
        "retrospective_waived":"No material conclusion, decision, or run"
    });
    let close: CaseClose = serde_json::from_value(request.clone()).unwrap();
    assert!(close.validate_materiality(CaseMode::Explain, false).is_ok());
    for mode in [CaseMode::Explain, CaseMode::Evaluate, CaseMode::Record] {
        assert_eq!(
            close.validate_materiality(mode, true),
            Err(CaseBlocker::RetrospectiveRequired)
        );
    }

    request["retrospective_ref"] = json!({"client_ref":"this-batch-retrospective"});
    let close: CaseClose = serde_json::from_value(request.clone()).unwrap();
    assert_eq!(
        close.validate_materiality(CaseMode::Explain, true),
        Err(CaseBlocker::RetrospectiveDispositionConflict)
    );

    request["retrospective_waived"] = Value::Null;
    let close: CaseClose = serde_json::from_value(request).unwrap();
    assert!(close.validate_materiality(CaseMode::Explain, true).is_ok());
}
