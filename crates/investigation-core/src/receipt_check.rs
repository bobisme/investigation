//! Executor receipts and the chronology they can support (plan §6.5).
//!
//! A producer may name receipts beside a captured or ingested Run. A receipt
//! that declares a `format` and whose bytes were retained is structure
//! checked here:
//!
//! - `in_toto_dsse`: a DSSE envelope whose payload is an in-toto Statement.
//!   The subject digests are read from the decoded Statement.
//! - `rfc3161`: an RFC 3161 `TimeStampToken`, or a `TimeStampResp` that holds
//!   one. A bounded DER walk follows the fixed path to the `TSTInfo` and reads
//!   its `messageImprint` and `genTime`.
//!
//! Nothing here verifies a signature, a certificate or a trust anchor. The
//! highest verification level this build records is `structure_checked`.
//!
//! The chronology rule: a Run that pinned a frozen plan at its declaration
//! (`recorded_before_run_declaration`) becomes `executor_bound` only when a
//! structure-checked in-toto receipt names a SHA-256 subject digest equal to
//! the Run's frozen plan digest. A receipt whose subject differs never changes
//! chronology. An RFC 3161 token never changes chronology: a timestamp over
//! the plan only dates the plan. This build never records
//! `externally_attested`, which needs independently verifiable evidence of
//! both the plan and the ordering of the execution event.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{command::RevisionRef, wire::Digest};

/// Largest receipt this build parses. Larger receipts stay retained and are
/// recorded `not_checked`.
pub const MAX_RECEIPT_BYTES: usize = 256 * 1024;
/// Deepest JSON nesting read from a DSSE envelope or its decoded Statement.
pub const MAX_RECEIPT_JSON_DEPTH: usize = 32;
/// Most subjects read from one in-toto Statement.
pub const MAX_STATEMENT_SUBJECTS: usize = 256;
/// Most length octets in one DER length field (lengths below 4 GiB).
pub const MAX_DER_LENGTH_OCTETS: usize = 4;
/// Longest receipt text (payload type, predicate type, time) copied onto the Run.
const MAX_COPIED_CHARS: usize = 256;

/// Run body key holding one [`ReceiptCheck`] per receipt.
pub const RECEIPT_CHECKS_KEY: &str = "receipt_checks";
/// Run body key holding the verification level of an upgraded chronology.
pub const CHRONOLOGY_VERIFICATION_KEY: &str = "chronology_verification";

/// DSSE payload type of an in-toto Statement.
pub const IN_TOTO_PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";
/// Statement types whose `subject` this build reads.
pub const IN_TOTO_STATEMENT_TYPES: &[&str] = &[
    "https://in-toto.io/Statement/v1",
    "https://in-toto.io/Statement/v0.1",
];

/// Declared receipt format. No format is inferred from bytes, a kind or a
/// media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptFormat {
    /// A DSSE envelope carrying an in-toto Statement.
    InTotoDsse,
    /// An RFC 3161 TimeStampToken or TimeStampResp, DER encoded.
    Rfc3161,
}

impl ReceiptFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InTotoDsse => "in_toto_dsse",
            Self::Rfc3161 => "rfc3161",
        }
    }
}

/// How far a receipt was checked. `signature_verified` is reserved for a
/// later build; this one never records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptVerification {
    /// The declared structure parsed and its subject digests were read.
    /// No signature was verified.
    StructureChecked,
    /// The bytes do not have the declared structure.
    StructureInvalid,
    /// Not parsed: no declared format, no retained bytes, or over the size limit.
    NotChecked,
}

/// How the receipt's subject compares with the Run's frozen plan digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptBinding {
    /// A SHA-256 subject digest equals the frozen plan digest.
    PlanDigestBound,
    /// SHA-256 subject digests were read; none equals the frozen plan digest.
    PlanDigestNotBound,
    /// The Run pins no frozen plan, so there is nothing to compare.
    NoFrozenPlan,
    /// No SHA-256 subject digest could be compared.
    NotCompared,
}

/// What one receipt contributes to the Run's chronology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChronologyEffect {
    /// Supports `executor_bound`.
    ExecutorBound,
    None,
}

/// Verification level of a Run's displayed chronology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChronologyVerification {
    /// Local journal order only.
    JournalOrder,
    /// A receipt's structure and subject digest were checked; no signature.
    StructureChecked,
    /// A receipt-backed level with no recorded check.
    Unverified,
}

impl ChronologyVerification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::JournalOrder => "journal_order",
            Self::StructureChecked => "structure_checked",
            Self::Unverified => "unverified",
        }
    }
}

/// The recorded result for one receipt, on the Run under
/// [`RECEIPT_CHECKS_KEY`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptCheck {
    /// Position in the producer's `receipts`.
    pub index: usize,
    /// The producer's kind, verbatim.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<ReceiptFormat>,
    /// The retained Artifact that holds the receipt bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<RevisionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_digest: Option<Digest>,
    pub verification: ReceiptVerification,
    /// SHA-256 subject digests read from the receipt: the in-toto subjects,
    /// or the RFC 3161 message imprint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subject_digests: Vec<Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate_type: Option<String>,
    /// DSSE signatures or CMS signer infos present. None is verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signatures_present: Option<usize>,
    /// RFC 3161 imprint hash algorithm, as a dotted OID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_algorithm: Option<String>,
    /// RFC 3161 `genTime`, verbatim and unverified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gen_time: Option<String>,
    pub binding: ReceiptBinding,
    pub chronology_effect: ChronologyEffect,
    /// Why the receipt has this result.
    pub note: String,
}

/// The bytes of one receipt, as the store could provide them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptBytes<'a> {
    Retained(&'a [u8]),
    /// Retained, but larger than [`MAX_RECEIPT_BYTES`]; not read.
    TooLarge,
    /// No retained bytes: a reference only, or a missing artifact.
    Unavailable,
}

/// One receipt to check.
#[derive(Debug, Clone)]
pub struct ReceiptInput<'a> {
    pub kind: &'a str,
    pub format: Option<ReceiptFormat>,
    pub artifact_ref: Option<RevisionRef>,
    pub digest: Option<Digest>,
    pub bytes: ReceiptBytes<'a>,
}

/// What a receipt's structure yields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Structure {
    pub subjects: Vec<Digest>,
    pub payload_type: Option<String>,
    pub statement_type: Option<String>,
    pub predicate_type: Option<String>,
    pub signatures: usize,
    pub hash_algorithm: Option<String>,
    pub gen_time: Option<String>,
}

/// Parse `bytes` as `format`, or say why they do not have that structure.
pub fn check_structure(format: ReceiptFormat, bytes: &[u8]) -> Result<Structure, String> {
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(format!(
            "larger than the {MAX_RECEIPT_BYTES}-byte receipt limit"
        ));
    }
    match format {
        ReceiptFormat::InTotoDsse => dsse(bytes),
        ReceiptFormat::Rfc3161 => rfc3161(bytes),
    }
}

/// Check every receipt against the Run's frozen plan digest.
pub fn check_receipts(receipts: &[ReceiptInput<'_>], plan: Option<&Digest>) -> Vec<ReceiptCheck> {
    receipts
        .iter()
        .enumerate()
        .map(|(index, receipt)| check_one(index, receipt, plan))
        .collect()
}

fn check_one(index: usize, receipt: &ReceiptInput<'_>, plan: Option<&Digest>) -> ReceiptCheck {
    let mut check = ReceiptCheck {
        index,
        kind: receipt.kind.to_owned(),
        format: receipt.format,
        artifact_ref: receipt.artifact_ref,
        receipt_digest: receipt.digest.clone(),
        verification: ReceiptVerification::NotChecked,
        subject_digests: Vec::new(),
        payload_type: None,
        statement_type: None,
        predicate_type: None,
        signatures_present: None,
        hash_algorithm: None,
        gen_time: None,
        binding: ReceiptBinding::NotCompared,
        chronology_effect: ChronologyEffect::None,
        note: String::new(),
    };
    let Some(format) = receipt.format else {
        check.note = "No format was declared, so the receipt is retained and not parsed.".into();
        return check;
    };
    let bytes = match receipt.bytes {
        ReceiptBytes::Retained(bytes) => bytes,
        ReceiptBytes::TooLarge => {
            check.note = format!(
                "The retained receipt is larger than the {MAX_RECEIPT_BYTES}-byte receipt limit and was not parsed."
            );
            return check;
        }
        ReceiptBytes::Unavailable => {
            check.note = "No receipt bytes were retained, so the receipt was not parsed.".into();
            return check;
        }
    };
    let structure = match check_structure(format, bytes) {
        Ok(structure) => structure,
        Err(reason) => {
            check.verification = ReceiptVerification::StructureInvalid;
            check.note = format!("Not a well-formed {} receipt: {reason}.", format.as_str());
            return check;
        }
    };
    check.verification = ReceiptVerification::StructureChecked;
    check.signatures_present = Some(structure.signatures);
    check.payload_type = structure.payload_type;
    check.statement_type = structure.statement_type;
    check.predicate_type = structure.predicate_type;
    check.hash_algorithm = structure.hash_algorithm;
    check.gen_time = structure.gen_time;
    check.subject_digests = structure.subjects;
    check.binding = match plan {
        None => ReceiptBinding::NoFrozenPlan,
        Some(_) if check.subject_digests.is_empty() => ReceiptBinding::NotCompared,
        Some(plan) if check.subject_digests.contains(plan) => ReceiptBinding::PlanDigestBound,
        Some(_) => ReceiptBinding::PlanDigestNotBound,
    };
    let unsigned = "Structure checked; no signature was verified.";
    check.note = match (format, check.binding) {
        (ReceiptFormat::InTotoDsse, ReceiptBinding::PlanDigestBound) => {
            check.chronology_effect = ChronologyEffect::ExecutorBound;
            format!(
                "{unsigned} A Statement subject equals the frozen plan digest, which supports executor_bound."
            )
        }
        (ReceiptFormat::Rfc3161, ReceiptBinding::PlanDigestBound) => format!(
            "{unsigned} The message imprint equals the frozen plan digest. A timestamp over the plan only dates the plan; chronology is unchanged."
        ),
        (_, ReceiptBinding::PlanDigestNotBound) => {
            format!("{unsigned} No subject equals the frozen plan digest; chronology is unchanged.")
        }
        (_, ReceiptBinding::NoFrozenPlan) => {
            format!("{unsigned} The Run pins no frozen plan; chronology is unchanged.")
        }
        _ => format!(
            "{unsigned} No SHA-256 subject digest could be compared; chronology is unchanged."
        ),
    };
    check
}

/// Record the checks on a Run body and apply the chronology rule. Returns
/// receipt warnings. Nothing changes when `receipts` is empty.
pub fn record_receipts(body: &mut Value, receipts: &[ReceiptInput<'_>]) -> Vec<String> {
    if receipts.is_empty() {
        return Vec::new();
    }
    let plan: Option<Digest> = body
        .get("plan_digest")
        .and_then(Value::as_str)
        .and_then(|text| text.parse().ok());
    let checks = check_receipts(receipts, plan.as_ref());
    let mut warnings = Vec::new();
    for check in &checks {
        match (check.verification, check.binding) {
            (ReceiptVerification::StructureInvalid, _) => warnings.push(format!(
                "Receipt {} ({}): {} It does not change chronology.",
                check.index, check.kind, check.note
            )),
            (ReceiptVerification::StructureChecked, ReceiptBinding::PlanDigestNotBound) => {
                warnings.push(format!(
                    "Receipt {} ({}) names no subject equal to the Run's frozen plan digest; it does not change chronology.",
                    check.index, check.kind
                ));
            }
            _ => {}
        }
    }
    let supported = checks
        .iter()
        .any(|check| check.chronology_effect == ChronologyEffect::ExecutorBound);
    if supported
        && body.get("chronology").and_then(Value::as_str) == Some("recorded_before_run_declaration")
    {
        body["chronology"] = json!("executor_bound");
        body[CHRONOLOGY_VERIFICATION_KEY] = json!(ChronologyVerification::StructureChecked);
    }
    body[RECEIPT_CHECKS_KEY] = json!(checks);
    warnings
}

/// Verification level of a Run's chronology: the recorded one, or the level
/// implied by a local chronology. `None` when the Run records no chronology.
pub fn chronology_verification(body: &Value) -> Option<ChronologyVerification> {
    if let Some(recorded) = body
        .get(CHRONOLOGY_VERIFICATION_KEY)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
    {
        return Some(recorded);
    }
    match body.get("chronology").and_then(Value::as_str)? {
        "retrospective" | "order_unknown" | "recorded_before_run_declaration" => {
            Some(ChronologyVerification::JournalOrder)
        }
        _ => Some(ChronologyVerification::Unverified),
    }
}

/// The chronology of a Run with its verification level, for display.
pub fn chronology_view(body: &Value) -> Option<Value> {
    let level = body.get("chronology").filter(|value| !value.is_null())?;
    Some(json!({
        "level": level,
        "verification": chronology_verification(body),
        "receipts_checked": body
            .get(RECEIPT_CHECKS_KEY)
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
    }))
}

// DSSE and in-toto

fn dsse(bytes: &[u8]) -> Result<Structure, String> {
    let envelope = crate::input::parse_json(bytes, MAX_RECEIPT_BYTES, MAX_RECEIPT_JSON_DEPTH)
        .map_err(|_| "the envelope is not bounded JSON".to_owned())?;
    let field = |name: &str| {
        envelope
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("the envelope has no {name} string"))
    };
    let payload_type = field("payloadType")?;
    if !in_toto_payload_type(payload_type) {
        return Err("the payloadType is not an in-toto media type".into());
    }
    let signatures = envelope
        .get("signatures")
        .and_then(Value::as_array)
        .ok_or("the envelope has no signatures array")?;
    if signatures.is_empty() {
        return Err("the envelope has no signature".into());
    }
    for signature in signatures {
        let sig = signature
            .get("sig")
            .and_then(Value::as_str)
            .ok_or("a signature has no sig string")?;
        if sig.is_empty() || base64(sig).is_none() {
            return Err("a signature's sig is not base64".into());
        }
    }
    let payload = base64(field("payload")?).ok_or("the payload is not base64")?;
    let statement = crate::input::parse_json(&payload, MAX_RECEIPT_BYTES, MAX_RECEIPT_JSON_DEPTH)
        .map_err(|_| "the payload is not a bounded JSON Statement".to_owned())?;
    let statement_type = statement
        .get("_type")
        .and_then(Value::as_str)
        .ok_or("the Statement has no _type")?;
    if !IN_TOTO_STATEMENT_TYPES.contains(&statement_type) {
        return Err("the Statement _type is not a supported in-toto Statement".into());
    }
    let predicate_type = statement
        .get("predicateType")
        .and_then(Value::as_str)
        .ok_or("the Statement has no predicateType")?;
    let subjects = statement
        .get("subject")
        .and_then(Value::as_array)
        .filter(|subjects| !subjects.is_empty())
        .ok_or("the Statement has no subject")?;
    if subjects.len() > MAX_STATEMENT_SUBJECTS {
        return Err(format!(
            "the Statement has more than {MAX_STATEMENT_SUBJECTS} subjects"
        ));
    }
    let mut digests = Vec::new();
    for subject in subjects {
        let set = subject
            .get("digest")
            .and_then(Value::as_object)
            .filter(|set| !set.is_empty())
            .ok_or("a subject has no digest")?;
        if let Some(sha256) = set.get("sha256") {
            let hex = sha256
                .as_str()
                .ok_or("a subject sha256 digest is not a string")?;
            let digest: Digest = format!("sha256:{}", hex.to_ascii_lowercase())
                .parse()
                .map_err(|_| "a subject sha256 digest is not 64 hexadecimal digits".to_owned())?;
            if !digests.contains(&digest) {
                digests.push(digest);
            }
        }
    }
    Ok(Structure {
        subjects: digests,
        payload_type: copied(payload_type),
        statement_type: copied(statement_type),
        predicate_type: copied(predicate_type),
        signatures: signatures.len(),
        hash_algorithm: None,
        gen_time: None,
    })
}

fn in_toto_payload_type(payload_type: &str) -> bool {
    payload_type == IN_TOTO_PAYLOAD_TYPE
        || payload_type
            .strip_prefix("application/vnd.in-toto.")
            .and_then(|rest| rest.strip_suffix("+json"))
            .is_some_and(|predicate| !predicate.is_empty())
}

/// Text copied from a receipt onto the Run: bounded and free of controls.
fn copied(text: &str) -> Option<String> {
    (text.chars().count() <= MAX_COPIED_CHARS && !text.chars().any(char::is_control))
        .then(|| text.to_owned())
}

/// Standard or URL-safe base64, padded or not (DSSE allows either alphabet).
fn base64(text: &str) -> Option<Vec<u8>> {
    let data = text.trim_end_matches('=');
    if text.len() - data.len() > 2 || data.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 3 + 2);
    let mut accumulator: u32 = 0;
    let mut bits = 0;
    for byte in data.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((accumulator >> bits) & 0xff).ok()?);
            accumulator &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// RFC 3161

const TAG_INTEGER: u8 = 0x02;
const TAG_OCTET_STRING: u8 = 0x04;
const TAG_OID: u8 = 0x06;
const TAG_GENERALIZED_TIME: u8 = 0x18;
const TAG_SEQUENCE: u8 = 0x30;
const TAG_SET: u8 = 0x31;
const TAG_CONTEXT_0: u8 = 0xa0;
const TAG_CONTEXT_1: u8 = 0xa1;
/// 1.2.840.113549.1.7.2 id-signedData (RFC 5652).
const OID_SIGNED_DATA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02];
/// 1.2.840.113549.1.9.16.1.4 id-ct-TSTInfo (RFC 3161).
const OID_TST_INFO: &[u8] = &[
    0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x10, 0x01, 0x04,
];
/// 2.16.840.1.101.3.4.2.1 id-sha256.
const OID_SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];

/// One DER element: its tag and contents.
struct Element<'a> {
    tag: u8,
    contents: &'a [u8],
}

/// Reads consecutive DER elements from a byte slice. Definite lengths only;
/// single-byte tags only.
struct Der<'a> {
    rest: &'a [u8],
}

impl<'a> Der<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    fn peek_tag(&self) -> Option<u8> {
        self.rest.first().copied()
    }

    fn next(&mut self) -> Result<Element<'a>, String> {
        let (&tag, rest) = self.rest.split_first().ok_or("a DER element is missing")?;
        if tag & 0x1f == 0x1f {
            return Err("a DER tag uses the multi-byte form".into());
        }
        let (&first, mut rest) = rest.split_first().ok_or("a DER length is missing")?;
        let length = if first < 0x80 {
            usize::from(first)
        } else {
            let octets = usize::from(first & 0x7f);
            if octets == 0 {
                return Err("a DER length is indefinite".into());
            }
            if octets > MAX_DER_LENGTH_OCTETS || octets > rest.len() {
                return Err("a DER length is too long".into());
            }
            let (digits, after) = rest.split_at(octets);
            rest = after;
            digits
                .iter()
                .fold(0usize, |length, digit| (length << 8) | usize::from(*digit))
        };
        if length > rest.len() {
            return Err("a DER element runs past its container".into());
        }
        let (contents, after) = rest.split_at(length);
        self.rest = after;
        Ok(Element { tag, contents })
    }

    fn expect(&mut self, tag: u8, what: &str) -> Result<&'a [u8], String> {
        let element = self.next()?;
        if element.tag == tag {
            Ok(element.contents)
        } else {
            Err(format!("expected {what}"))
        }
    }

    fn end(&self, what: &str) -> Result<(), String> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(format!("unexpected bytes after {what}"))
        }
    }
}

fn rfc3161(bytes: &[u8]) -> Result<Structure, String> {
    let mut outer = Der::new(bytes);
    let top = outer.expect(TAG_SEQUENCE, "a SEQUENCE")?;
    outer.end("the receipt")?;
    let mut fields = Der::new(top);
    // A TimeStampResp starts with its PKIStatusInfo SEQUENCE; a
    // TimeStampToken (ContentInfo) starts with its contentType OID.
    let content_info = if fields.peek_tag() == Some(TAG_SEQUENCE) {
        let mut status = Der::new(fields.expect(TAG_SEQUENCE, "a PKIStatusInfo")?);
        let code = status.expect(TAG_INTEGER, "a PKIStatus")?;
        if !matches!(code, [0] | [1]) {
            return Err("the TimeStampResp status is not granted".into());
        }
        let token = fields.expect(TAG_SEQUENCE, "a timeStampToken")?;
        fields.end("the TimeStampResp")?;
        token
    } else {
        top
    };
    let mut content_info = Der::new(content_info);
    if content_info.expect(TAG_OID, "a contentType")? != OID_SIGNED_DATA {
        return Err("the token's contentType is not id-signedData".into());
    }
    let mut explicit = Der::new(content_info.expect(TAG_CONTEXT_0, "the [0] content")?);
    content_info.end("the ContentInfo")?;
    let signed_data = explicit.expect(TAG_SEQUENCE, "a SignedData")?;
    explicit.end("the SignedData")?;

    let mut signed = Der::new(signed_data);
    signed.expect(TAG_INTEGER, "the SignedData version")?;
    signed.expect(TAG_SET, "the digestAlgorithms")?;
    let mut encapsulated = Der::new(signed.expect(TAG_SEQUENCE, "an EncapsulatedContentInfo")?);
    if encapsulated.expect(TAG_OID, "an eContentType")? != OID_TST_INFO {
        return Err("the eContentType is not id-ct-TSTInfo".into());
    }
    let mut econtent = Der::new(encapsulated.expect(TAG_CONTEXT_0, "the [0] eContent")?);
    encapsulated.end("the EncapsulatedContentInfo")?;
    let tst_info = econtent.expect(TAG_OCTET_STRING, "a primitive OCTET STRING eContent")?;
    econtent.end("the eContent")?;
    if signed.peek_tag() == Some(TAG_CONTEXT_0) {
        signed.next()?;
    }
    if signed.peek_tag() == Some(TAG_CONTEXT_1) {
        signed.next()?;
    }
    let mut signer_infos = Der::new(signed.expect(TAG_SET, "the signerInfos")?);
    signed.end("the signerInfos")?;
    let mut signatures = 0;
    while !signer_infos.is_empty() {
        signer_infos.expect(TAG_SEQUENCE, "a SignerInfo")?;
        signatures += 1;
    }
    if signatures == 0 {
        return Err("the token has no SignerInfo".into());
    }

    let mut outer = Der::new(tst_info);
    let mut tst = Der::new(outer.expect(TAG_SEQUENCE, "a TSTInfo")?);
    outer.end("the TSTInfo")?;
    if tst.expect(TAG_INTEGER, "the TSTInfo version")? != [1] {
        return Err("the TSTInfo version is not 1".into());
    }
    tst.expect(TAG_OID, "the TSA policy")?;
    let mut imprint = Der::new(tst.expect(TAG_SEQUENCE, "a MessageImprint")?);
    let mut algorithm = Der::new(imprint.expect(TAG_SEQUENCE, "an AlgorithmIdentifier")?);
    let oid = algorithm.expect(TAG_OID, "a hash algorithm OID")?;
    let hashed = imprint.expect(TAG_OCTET_STRING, "a hashedMessage")?;
    imprint.end("the MessageImprint")?;
    tst.expect(TAG_INTEGER, "the serialNumber")?;
    let time = tst.expect(TAG_GENERALIZED_TIME, "the genTime")?;
    let mut subjects = Vec::new();
    if oid == OID_SHA256 {
        if hashed.len() != 32 {
            return Err("the SHA-256 hashedMessage is not 32 bytes".into());
        }
        let hex: String = hashed.iter().map(|byte| format!("{byte:02x}")).collect();
        subjects.push(
            format!("sha256:{hex}")
                .parse()
                .map_err(|_| "the hashedMessage is not a digest".to_owned())?,
        );
    }
    Ok(Structure {
        subjects,
        payload_type: None,
        statement_type: None,
        predicate_type: None,
        signatures,
        hash_algorithm: Some(dotted(oid).ok_or("the hash algorithm OID is malformed")?),
        gen_time: std::str::from_utf8(time).ok().and_then(copied),
    })
}

/// A DER OBJECT IDENTIFIER's contents in dotted form.
fn dotted(oid: &[u8]) -> Option<String> {
    if oid.last()? & 0x80 != 0 || oid.len() > 64 {
        return None;
    }
    let mut arcs: Vec<u64> = Vec::new();
    let mut value: u64 = 0;
    for &byte in oid {
        value = value.checked_mul(128)? | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            arcs.push(value);
            value = 0;
        }
    }
    let head = arcs.first().copied()?;
    let (a, b) = match head {
        0..=39 => (0, head),
        40..=79 => (1, head - 40),
        _ => (2, head - 80),
    };
    let mut text = format!("{a}.{b}");
    for arc in &arcs[1..] {
        text.push('.');
        text.push_str(&arc.to_string());
    }
    Some(text)
}

#[cfg(test)]
mod tests;
