//! Receipt structure checks and the chronology rule.
//!
//! `testdata/timestamp-token.der` and `testdata/timestamp-response.der` were
//! made offline with OpenSSL 3.6 (`openssl ts -query -digest HEX -sha256
//! -cert -no_nonce`, then `openssl ts -reply`, with and without `-token_out`)
//! by a throwaway self-signed test TSA whose key was not kept. Their message
//! imprint is the SHA-256 of `example frozen plan\n`.

use super::*;

const TOKEN: &[u8] = include_bytes!("testdata/timestamp-token.der");
const RESPONSE: &[u8] = include_bytes!("testdata/timestamp-response.der");
const IMPRINT: &str = "sha256:01ed774d4686cf4d74a26d8dfd351cf4b4f856deebc2a0285aaa5ed6ae657464";
const OTHER: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn digest(text: &str) -> Digest {
    text.parse().unwrap()
}

fn encode64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, byte)| n | u32::from(*byte) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
        for _ in chunk.len()..3 {
            out.push('=');
        }
    }
    out
}

/// A DSSE envelope over an in-toto Statement naming `subjects`.
pub(crate) fn envelope(subjects: &[&str]) -> Vec<u8> {
    let statement = json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": subjects.iter().map(|digest| json!({
            "name": "inquiry-plan",
            "digest": {"sha256": digest.strip_prefix("sha256:").unwrap()},
        })).collect::<Vec<_>>(),
        "predicateType": "https://example.org/executor-receipt/v1",
        "predicate": {"stage": "before-execution"},
    });
    serde_json::to_vec(&json!({
        "payloadType": IN_TOTO_PAYLOAD_TYPE,
        "payload": encode64(&serde_json::to_vec(&statement).unwrap()),
        "signatures": [{"keyid": "test", "sig": encode64(b"not a real signature")}],
    }))
    .unwrap()
}

fn input(format: ReceiptFormat, bytes: &[u8]) -> ReceiptInput<'_> {
    ReceiptInput {
        kind: "executor-receipt",
        format: Some(format),
        artifact_ref: None,
        digest: None,
        bytes: ReceiptBytes::Retained(bytes),
    }
}

fn planned_run(plan: &str) -> Value {
    json!({"chronology": "recorded_before_run_declaration", "plan_digest": plan})
}

#[test]
fn base64_accepts_both_alphabets_with_or_without_padding() {
    assert_eq!(base64("aGk="), Some(b"hi".to_vec()));
    assert_eq!(base64("aGk"), Some(b"hi".to_vec()));
    assert_eq!(base64("-_8"), base64("+/8"));
    assert_eq!(base64("a"), None);
    assert_eq!(base64("aGk==="), None);
    assert_eq!(base64("a b="), None);
    let bytes: Vec<u8> = (0..=255).collect();
    assert_eq!(base64(&encode64(&bytes)), Some(bytes));
}

#[test]
fn dsse_statement_subjects_are_read() {
    let structure =
        check_structure(ReceiptFormat::InTotoDsse, &envelope(&[IMPRINT, OTHER])).unwrap();
    assert_eq!(structure.subjects, vec![digest(IMPRINT), digest(OTHER)]);
    assert_eq!(structure.signatures, 1);
    assert_eq!(
        structure.statement_type.as_deref(),
        Some("https://in-toto.io/Statement/v1")
    );
    assert_eq!(
        structure.predicate_type.as_deref(),
        Some("https://example.org/executor-receipt/v1")
    );
}

#[test]
fn malformed_dsse_envelopes_are_refused_with_a_reason() {
    let good: Value = serde_json::from_slice(&envelope(&[IMPRINT])).unwrap();
    let mutate = |change: &dyn Fn(&mut Value)| {
        let mut value = good.clone();
        change(&mut value);
        check_structure(
            ReceiptFormat::InTotoDsse,
            &serde_json::to_vec(&value).unwrap(),
        )
        .unwrap_err()
    };
    assert!(mutate(&|v| v["payloadType"] = json!("text/plain")).contains("payloadType"));
    assert!(mutate(&|v| v["signatures"] = json!([])).contains("no signature"));
    assert!(mutate(&|v| v["payload"] = json!("%%%")).contains("base64"));
    assert!(mutate(&|v| v["payload"] = json!(encode64(b"{\"_type\":\"x\"}"))).contains("_type"));
    let statement = |subject: Value| {
        encode64(
            &serde_json::to_vec(&json!({
                "_type": "https://in-toto.io/Statement/v1",
                "subject": subject,
                "predicateType": "p",
            }))
            .unwrap(),
        )
    };
    assert!(mutate(&|v| v["payload"] = json!(statement(json!([])))).contains("no subject"));
    assert!(
        mutate(&|v| v["payload"] = json!(statement(json!([{"digest": {"sha256": "abc"}}]))))
            .contains("64 hexadecimal")
    );
    assert!(
        mutate(&|v| v["payload"] = json!(statement(json!([{"name": "x"}])))).contains("digest")
    );
    assert!(
        check_structure(ReceiptFormat::InTotoDsse, b"not json")
            .unwrap_err()
            .contains("JSON")
    );
}

#[test]
fn a_predicate_specific_payload_type_and_other_algorithms_are_accepted() {
    assert!(in_toto_payload_type(
        "application/vnd.in-toto.provenance+json"
    ));
    assert!(!in_toto_payload_type("application/vnd.in-toto.+json"));
    let statement = json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{"digest": {"sha512": "ab"}}],
        "predicateType": "p",
    });
    let bytes = serde_json::to_vec(&json!({
        "payloadType": "application/vnd.in-toto+json",
        "payload": encode64(&serde_json::to_vec(&statement).unwrap()),
        "signatures": [{"sig": "AA"}],
    }))
    .unwrap();
    let checks = check_receipts(
        &[input(ReceiptFormat::InTotoDsse, &bytes)],
        Some(&digest(IMPRINT)),
    );
    assert_eq!(
        checks[0].verification,
        ReceiptVerification::StructureChecked
    );
    assert_eq!(checks[0].binding, ReceiptBinding::NotCompared);
    assert_eq!(checks[0].chronology_effect, ChronologyEffect::None);
}

#[test]
fn rfc3161_token_and_response_yield_the_message_imprint() {
    for bytes in [TOKEN, RESPONSE] {
        let structure = check_structure(ReceiptFormat::Rfc3161, bytes).unwrap();
        assert_eq!(structure.subjects, vec![digest(IMPRINT)]);
        assert_eq!(
            structure.hash_algorithm.as_deref(),
            Some("2.16.840.1.101.3.4.2.1")
        );
        assert_eq!(structure.signatures, 1);
        let time = structure.gen_time.unwrap();
        assert!(time.starts_with("2026") && time.ends_with('Z'), "{time}");
    }
}

#[test]
fn damaged_der_is_refused_never_panics() {
    // Every truncation and every single-byte change parses or is refused.
    for end in 0..TOKEN.len() {
        assert!(check_structure(ReceiptFormat::Rfc3161, &TOKEN[..end]).is_err());
    }
    for index in 0..TOKEN.len() {
        let mut bytes = TOKEN.to_vec();
        bytes[index] ^= 0xff;
        let _ = check_structure(ReceiptFormat::Rfc3161, &bytes);
    }
    let mut trailing = TOKEN.to_vec();
    trailing.push(0);
    assert!(
        check_structure(ReceiptFormat::Rfc3161, &trailing)
            .unwrap_err()
            .contains("after")
    );
    // Indefinite and oversized lengths.
    assert!(
        check_structure(ReceiptFormat::Rfc3161, &[0x30, 0x80, 0, 0])
            .unwrap_err()
            .contains("indefinite")
    );
    assert!(
        check_structure(ReceiptFormat::Rfc3161, &[0x30, 0x85, 1, 1, 1, 1, 1])
            .unwrap_err()
            .contains("too long")
    );
    // A DSSE envelope is not a timestamp token, and the reverse.
    assert!(check_structure(ReceiptFormat::Rfc3161, &envelope(&[IMPRINT])).is_err());
    assert!(check_structure(ReceiptFormat::InTotoDsse, TOKEN).is_err());
}

#[test]
fn oversized_receipts_are_not_parsed() {
    let big = vec![b' '; MAX_RECEIPT_BYTES + 1];
    assert!(
        check_structure(ReceiptFormat::InTotoDsse, &big)
            .unwrap_err()
            .contains("limit")
    );
}

#[test]
fn oids_print_in_dotted_form() {
    assert_eq!(
        dotted(OID_SHA256).as_deref(),
        Some("2.16.840.1.101.3.4.2.1")
    );
    assert_eq!(
        dotted(OID_TST_INFO).as_deref(),
        Some("1.2.840.113549.1.9.16.1.4")
    );
    assert_eq!(dotted(&[0x2a, 0x86]), None);
    assert_eq!(dotted(&[]), None);
}

#[test]
fn a_matching_in_toto_subject_makes_the_run_executor_bound() {
    let bytes = envelope(&[OTHER, IMPRINT]);
    let mut body = planned_run(IMPRINT);
    let warnings = record_receipts(&mut body, &[input(ReceiptFormat::InTotoDsse, &bytes)]);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(body["chronology"], "executor_bound");
    assert_eq!(body[CHRONOLOGY_VERIFICATION_KEY], "structure_checked");
    let check = &body[RECEIPT_CHECKS_KEY][0];
    assert_eq!(check["verification"], "structure_checked");
    assert_eq!(check["binding"], "plan_digest_bound");
    assert_eq!(check["chronology_effect"], "executor_bound");
    let view = chronology_view(&body).unwrap();
    assert_eq!(view["level"], "executor_bound");
    assert_eq!(view["verification"], "structure_checked");
    assert_eq!(view["receipts_checked"], 1);
}

#[test]
fn a_mismatching_subject_never_upgrades_chronology() {
    let bytes = envelope(&[OTHER]);
    let mut body = planned_run(IMPRINT);
    let warnings = record_receipts(&mut body, &[input(ReceiptFormat::InTotoDsse, &bytes)]);
    assert_eq!(body["chronology"], "recorded_before_run_declaration");
    assert!(body.get(CHRONOLOGY_VERIFICATION_KEY).is_none());
    assert_eq!(
        body[RECEIPT_CHECKS_KEY][0]["binding"],
        "plan_digest_not_bound"
    );
    assert_eq!(body[RECEIPT_CHECKS_KEY][0]["chronology_effect"], "none");
    assert!(warnings[0].contains("no subject equal"), "{warnings:?}");
    assert_eq!(
        chronology_view(&body).unwrap()["verification"],
        "journal_order"
    );
}

#[test]
fn a_timestamp_over_the_plan_only_dates_the_plan() {
    let mut body = planned_run(IMPRINT);
    let warnings = record_receipts(&mut body, &[input(ReceiptFormat::Rfc3161, TOKEN)]);
    assert!(warnings.is_empty());
    assert_eq!(body["chronology"], "recorded_before_run_declaration");
    let check = &body[RECEIPT_CHECKS_KEY][0];
    assert_eq!(check["binding"], "plan_digest_bound");
    assert_eq!(check["chronology_effect"], "none");
    assert!(
        check["note"]
            .as_str()
            .unwrap()
            .contains("only dates the plan")
    );
}

#[test]
fn only_a_run_that_pinned_its_plan_at_declaration_is_upgraded() {
    let bytes = envelope(&[IMPRINT]);
    for chronology in ["retrospective", "order_unknown"] {
        let mut body = json!({"chronology": chronology, "plan_digest": IMPRINT});
        record_receipts(&mut body, &[input(ReceiptFormat::InTotoDsse, &bytes)]);
        assert_eq!(body["chronology"], chronology);
    }
    // No frozen plan: checked, never compared.
    let mut body = json!({"chronology": "order_unknown", "plan_digest": null});
    record_receipts(&mut body, &[input(ReceiptFormat::InTotoDsse, &bytes)]);
    assert_eq!(body[RECEIPT_CHECKS_KEY][0]["binding"], "no_frozen_plan");
    assert_eq!(body["chronology"], "order_unknown");
}

#[test]
fn undeclared_unretained_oversized_and_invalid_receipts_do_not_count() {
    let bytes = envelope(&[IMPRINT]);
    let mut undeclared = input(ReceiptFormat::InTotoDsse, &bytes);
    undeclared.format = None;
    let mut unavailable = input(ReceiptFormat::InTotoDsse, &bytes);
    unavailable.bytes = ReceiptBytes::Unavailable;
    let mut large = input(ReceiptFormat::InTotoDsse, &bytes);
    large.bytes = ReceiptBytes::TooLarge;
    let invalid = input(ReceiptFormat::InTotoDsse, b"{}");
    let mut body = planned_run(IMPRINT);
    let warnings = record_receipts(&mut body, &[undeclared, unavailable, large, invalid]);
    assert_eq!(body["chronology"], "recorded_before_run_declaration");
    let checks = body[RECEIPT_CHECKS_KEY].as_array().unwrap();
    let verification: Vec<_> = checks.iter().map(|c| c["verification"].clone()).collect();
    assert_eq!(
        verification,
        vec![
            json!("not_checked"),
            json!("not_checked"),
            json!("not_checked"),
            json!("structure_invalid")
        ]
    );
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].starts_with("Receipt 3"), "{warnings:?}");
    // The recorded checks read back.
    let _: Vec<ReceiptCheck> = serde_json::from_value(body[RECEIPT_CHECKS_KEY].clone()).unwrap();
}

#[test]
fn no_receipts_leave_the_body_alone() {
    let mut body = planned_run(IMPRINT);
    let before = body.clone();
    assert!(record_receipts(&mut body, &[]).is_empty());
    assert_eq!(body, before);
}

#[test]
fn every_chronology_shows_a_verification_level() {
    for (chronology, verification) in [
        ("retrospective", "journal_order"),
        ("order_unknown", "journal_order"),
        ("recorded_before_run_declaration", "journal_order"),
        ("executor_bound", "unverified"),
        ("externally_attested", "unverified"),
    ] {
        let view = chronology_view(&json!({"chronology": chronology})).unwrap();
        assert_eq!(view["verification"], verification, "{chronology}");
    }
    assert!(chronology_view(&json!({})).is_none());
}
