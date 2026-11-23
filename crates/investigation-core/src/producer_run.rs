//! The neutral producer-run envelope `inv.producer-run.v1`.
//!
//! An external producer describes one completed attempt in a single JSON
//! document: who produced it, what it meant to do, how execution ended, the
//! environment it reports, the original artifacts it left, and its own results,
//! controls, receipts and limitations. Every statement in the envelope is the
//! producer's claim. Ingest retains the envelope bytes and the declared artifact
//! bytes as source content and records one Run that indexes them; it never turns
//! a producer statement into an Observation with validity, an Assessment or a
//! Conclusion.
//!
//! Four things stay separate:
//!
//! - execution: the producer's reported state and exit code;
//! - evidence completeness: which declared artifacts ingest actually retained;
//! - measurement validity: always `unknown` until an attributed judgment says
//!   otherwise, whatever the exit code or control results;
//! - investigative interpretation: only an attributed Assessment or Conclusion.
//!
//! The same assertion and control types are the finalize payload of the
//! recording API, so a producer that records incrementally and one that writes a
//! single envelope make identical statements.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    command::RevisionRef,
    explain::{EvidenceCompleteness, ExecutionStatus},
    header::{ClassificationLevel, ClockQuality},
    wire::Digest,
};

/// Wire tag of the first envelope version.
pub const PRODUCER_RUN_V1: &str = "inv.producer-run.v1";
/// Largest envelope ingest accepts, in bytes.
pub const MAX_ENVELOPE_BYTES: usize = 512 * 1024;
pub const MAX_ARTIFACTS: usize = 32;
pub const MAX_ASSERTIONS: usize = 256;
pub const MAX_CONTROLS: usize = 64;
pub const MAX_RECEIPTS: usize = 32;
pub const MAX_LIMITATIONS: usize = 64;
pub const MAX_ENVIRONMENT_ENTRIES: usize = 64;
/// Bound on the serialized size of one assertion value.
pub const MAX_VALUE_BYTES: usize = 16 * 1024;
const NAME_CHARS: usize = 128;
const TEXT_CHARS: usize = 2048;
const RUN_ID_CHARS: usize = 256;

/// Producer-run envelope versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ProducerRunVersion {
    #[serde(rename = "inv.producer-run.v1")]
    V1,
}

/// One completed producer attempt, described by the producer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProducerRun {
    pub schema_version: ProducerRunVersion,
    pub producer: ProducerDescriptor,
    /// The producer's stable identifier for this attempt. With `producer.name`
    /// it names one logical acquisition: ingesting the same pair again maps onto
    /// the recorded Run, and a changed envelope revises it.
    #[schemars(length(min = 1, max = 256))]
    pub external_run_id: String,
    pub intent: ProducerIntent,
    pub execution: ProducerExecution,
    /// Reported environment facts such as a revision or workload label.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// Original artifacts. Paths resolve beneath the workspace import roots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ProducerArtifact>,
    /// The producer's own results, retained as its attributed claims.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assertions: Vec<ProducerAssertion>,
    /// Controls the producer reports with its own result. Not proof of method.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<ProducerControl>,
    /// References to external receipts about the action, unverified.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receipts: Vec<ProducerReceipt>,
    /// Limitations the producer states.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
    /// Level of the envelope, its Source and the Run. The recorded level is the
    /// highest of this and every artifact's level.
    #[serde(default)]
    pub classification: ClassificationLevel,
    /// Inert namespaced metadata such as `example-org.instrument`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

impl ProducerRun {
    /// Level recorded for the envelope, its Source and the Run.
    pub fn level(&self) -> ClassificationLevel {
        self.artifacts
            .iter()
            .map(|artifact| artifact.classification)
            .fold(self.classification, Ord::max)
    }
}

/// Self-reported producer identity. Attribution, not authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProducerDescriptor {
    /// Producer namespace; it scopes acquisition identity and is never interpreted.
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
    )]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 128))]
    pub version: Option<String>,
    /// Instance label, such as a host-local name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 128))]
    pub instance: Option<String>,
}

/// What the producer meant to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProducerIntent {
    #[schemars(length(min = 1, max = 256))]
    pub label: String,
    /// The producer's own digest of an external plan; retained, not checked.
    #[serde(default)]
    #[schemars(length(min = 1, max = 256))]
    pub external_plan_digest: Option<String>,
}

/// Execution as the producer reports it. A `completed` state or an exit code of
/// 0 says how the action ended; it implies nothing about measurement validity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProducerExecution {
    pub state: ExecutionStatus,
    #[serde(default)]
    pub exit_code: Option<i64>,
    /// Reported start time, verbatim; RFC 3339 is recommended.
    #[serde(default)]
    #[schemars(length(min = 1, max = 64))]
    pub started_at: Option<String>,
    #[serde(default)]
    #[schemars(length(min = 1, max = 64))]
    pub ended_at: Option<String>,
    /// Quality of the producer's clock for both times.
    #[serde(default = "unknown_clock")]
    pub clock_quality: ClockQuality,
    /// Original error text, preserved verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 2048))]
    pub error: Option<String>,
}

fn unknown_clock() -> ClockQuality {
    ClockQuality::Unknown
}

/// One declared original artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProducerArtifact {
    /// Producer vocabulary, such as `original-output`, `log` or `receipt`.
    #[schemars(
        length(min = 1, max = 64),
        regex(pattern = r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
    )]
    pub role: String,
    /// Relative path, resolved beneath a configured import root. `./` prefixes
    /// are ignored; `..`, absolute paths and symlinks are refused.
    #[schemars(length(min = 1, max = 1024))]
    pub path: String,
    /// Lowercase hex SHA-256 the producer expects. A mismatch rejects this item
    /// only; its bytes are not retained.
    #[serde(default)]
    #[schemars(regex(pattern = r"^[0-9a-f]{64}$"))]
    pub expected_sha256: Option<String>,
    #[serde(default)]
    #[schemars(length(min = 1, max = 256))]
    pub media_type: Option<String>,
    #[serde(default)]
    pub classification: ClassificationLevel,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

/// How valid the producer says an assertion is. The only accepted claim is
/// `producer_reported`: a producer cannot declare its own result verified.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ValidityClaim {
    #[default]
    ProducerReported,
}

/// A producer-reported result, such as a metric.
///
/// It is retained as the producer's assertion. Only an attributed Assessment or
/// Conclusion can promote it to an investigative interpretation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ProducerAssertion {
    /// Assertion kind, such as `reported_metric`.
    #[schemars(length(min = 1, max = 128))]
    pub kind: String,
    /// Name, such as `latency.p99`.
    #[schemars(length(min = 1, max = 128))]
    pub name: String,
    /// Reported value, retained verbatim.
    pub value: Value,
    /// Unit, when meaningful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 64))]
    pub unit: Option<String>,
    /// Always `producer_reported`.
    #[serde(default)]
    pub validity_claim: ValidityClaim,
    /// Inert namespaced metadata.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

impl ProducerAssertion {
    /// An assertion without a unit.
    pub fn new(kind: impl Into<String>, name: impl Into<String>, value: Value) -> Self {
        Self {
            kind: kind.into(),
            name: name.into(),
            value,
            unit: None,
            validity_claim: ValidityClaim::ProducerReported,
            metadata: BTreeMap::new(),
        }
    }
}

/// A control the producer reports with its own result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ProducerControl {
    /// Control name.
    #[schemars(length(min = 1, max = 128))]
    pub name: String,
    /// Producer's result, such as `reported_pass`. It is not proof of method.
    #[schemars(length(min = 1, max = 128))]
    pub producer_result: String,
    /// Inert namespaced metadata.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

impl ProducerControl {
    /// A control and its reported result.
    pub fn new(name: impl Into<String>, producer_result: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            producer_result: producer_result.into(),
            metadata: BTreeMap::new(),
        }
    }
}

/// Reference to an external receipt about the action, retained unverified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ProducerReceipt {
    /// Receipt format, as named by the producer.
    #[schemars(length(min = 1, max = 128))]
    pub kind: String,
    /// `path` of the entry in `artifacts` that holds the receipt bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 1024))]
    pub artifact: Option<String>,
    /// External locator, retained as an unverified reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 2048))]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

/// A rejected envelope field: message and JSON pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeError {
    pub message: String,
    pub path: String,
}

fn error(message: impl Into<String>, path: impl Into<String>) -> EnvelopeError {
    EnvelopeError {
        message: message.into(),
        path: path.into(),
    }
}

fn text(value: &str, max: usize, path: &str) -> Result<(), EnvelopeError> {
    if value.trim().is_empty() || value.chars().count() > max || value.chars().any(char::is_control)
    {
        return Err(error(
            format!("Use 1–{max} characters without control characters."),
            path,
        ));
    }
    Ok(())
}

fn optional(value: Option<&String>, max: usize, path: &str) -> Result<(), EnvelopeError> {
    value.map_or(Ok(()), |value| text(value, max, path))
}

fn token(value: &str, max: usize, path: &str) -> Result<(), EnvelopeError> {
    let ok = !value.is_empty()
        && value.len() <= max
        && value.starts_with(|c: char| c.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(error(
            format!(
                "Use 1–{max} ASCII letters, digits, '.', '_' or '-', starting with a letter or digit."
            ),
            path,
        ))
    }
}

fn bounded<T>(items: &[T], max: usize, path: &str) -> Result<(), EnvelopeError> {
    if items.len() > max {
        return Err(error(format!("List at most {max} entries."), path));
    }
    Ok(())
}

/// The relative path an artifact names, without `.` components. Every other
/// component must be a plain name.
pub fn normalized_path(path: &str) -> Option<String> {
    let parts: Vec<&str> = path
        .split('/')
        .filter(|part| *part != "." && !part.is_empty())
        .collect();
    let plain = !path.starts_with('/')
        && !parts.is_empty()
        && parts.iter().all(|part| *part != "..")
        && !path.chars().any(|c| c.is_control() || c == '\\');
    plain.then(|| parts.join("/"))
}

impl ProducerRun {
    /// Check bounds and cross-references that the JSON Schema cannot express.
    /// Metadata bounds are checked by the store's opaque-content policy.
    pub fn validate(&self) -> Result<(), EnvelopeError> {
        token(&self.producer.name, NAME_CHARS, "/producer/name")?;
        optional(
            self.producer.version.as_ref(),
            NAME_CHARS,
            "/producer/version",
        )?;
        optional(
            self.producer.instance.as_ref(),
            NAME_CHARS,
            "/producer/instance",
        )?;
        text(&self.external_run_id, RUN_ID_CHARS, "/external_run_id")?;
        text(&self.intent.label, 256, "/intent/label")?;
        optional(
            self.intent.external_plan_digest.as_ref(),
            256,
            "/intent/external_plan_digest",
        )?;
        optional(
            self.execution.started_at.as_ref(),
            64,
            "/execution/started_at",
        )?;
        optional(self.execution.ended_at.as_ref(), 64, "/execution/ended_at")?;
        if let Some(message) = &self.execution.error
            && (message.is_empty() || message.chars().count() > TEXT_CHARS)
        {
            return Err(error(
                format!("Keep the error text to 1–{TEXT_CHARS} characters."),
                "/execution/error",
            ));
        }
        if self.environment.len() > MAX_ENVIRONMENT_ENTRIES {
            return Err(error(
                format!("Report at most {MAX_ENVIRONMENT_ENTRIES} environment entries."),
                "/environment",
            ));
        }
        for (key, value) in &self.environment {
            let path = format!("/environment/{}", key.replace('~', "~0").replace('/', "~1"));
            token(key, NAME_CHARS, &path)?;
            text(value, 1024, &path)?;
        }
        bounded(&self.artifacts, MAX_ARTIFACTS, "/artifacts")?;
        let mut paths = std::collections::BTreeSet::new();
        for (index, artifact) in self.artifacts.iter().enumerate() {
            let at = |field: &str| format!("/artifacts/{index}/{field}");
            token(&artifact.role, 64, &at("role"))?;
            let normalized = normalized_path(&artifact.path)
                .filter(|_| artifact.path.chars().count() <= 1024)
                .ok_or_else(|| {
                    error(
                        "Use a relative path of plain names beneath an import root, without '..'.",
                        at("path"),
                    )
                })?;
            if !paths.insert(normalized) {
                return Err(error("Declare each artifact path once.", at("path")));
            }
            if let Some(expected) = &artifact.expected_sha256
                && (expected.len() != 64
                    || !expected
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
            {
                return Err(error(
                    "Use 64 lowercase hexadecimal characters.",
                    at("expected_sha256"),
                ));
            }
            optional(artifact.media_type.as_ref(), 256, &at("media_type"))?;
        }
        bounded(&self.assertions, MAX_ASSERTIONS, "/assertions")?;
        for (index, assertion) in self.assertions.iter().enumerate() {
            let at = |field: &str| format!("/assertions/{index}/{field}");
            text(&assertion.kind, NAME_CHARS, &at("kind"))?;
            text(&assertion.name, NAME_CHARS, &at("name"))?;
            optional(assertion.unit.as_ref(), 64, &at("unit"))?;
            let size = serde_json::to_vec(&assertion.value).map_or(usize::MAX, |bytes| bytes.len());
            if size > MAX_VALUE_BYTES {
                return Err(error(
                    format!(
                        "Keep an assertion value under {MAX_VALUE_BYTES} bytes. Retain larger results as an artifact."
                    ),
                    at("value"),
                ));
            }
        }
        bounded(&self.controls, MAX_CONTROLS, "/controls")?;
        for (index, control) in self.controls.iter().enumerate() {
            text(
                &control.name,
                NAME_CHARS,
                &format!("/controls/{index}/name"),
            )?;
            text(
                &control.producer_result,
                NAME_CHARS,
                &format!("/controls/{index}/producer_result"),
            )?;
        }
        bounded(&self.receipts, MAX_RECEIPTS, "/receipts")?;
        for (index, receipt) in self.receipts.iter().enumerate() {
            let at = |field: &str| format!("/receipts/{index}/{field}");
            text(&receipt.kind, NAME_CHARS, &at("kind"))?;
            optional(receipt.reference.as_ref(), TEXT_CHARS, &at("reference"))?;
            if let Some(artifact) = &receipt.artifact
                && !normalized_path(artifact).is_some_and(|path| paths.contains(&path))
            {
                return Err(error(
                    "Name the path of an entry in artifacts.",
                    at("artifact"),
                ));
            }
            if receipt.artifact.is_none() && receipt.reference.is_none() {
                return Err(error(
                    "Give the receipt an artifact path or a reference.",
                    format!("/receipts/{index}"),
                ));
            }
        }
        bounded(&self.limitations, MAX_LIMITATIONS, "/limitations")?;
        for (index, limitation) in self.limitations.iter().enumerate() {
            text(limitation, TEXT_CHARS, &format!("/limitations/{index}"))?;
        }
        Ok(())
    }

    /// Every namespaced metadata map in the envelope with its JSON pointer.
    pub fn metadata_maps(&self) -> Vec<(String, &BTreeMap<String, Value>)> {
        let mut maps = vec![("/metadata".to_owned(), &self.metadata)];
        maps.extend(
            self.artifacts
                .iter()
                .enumerate()
                .map(|(index, item)| (format!("/artifacts/{index}/metadata"), &item.metadata)),
        );
        maps.extend(
            self.assertions
                .iter()
                .enumerate()
                .map(|(index, item)| (format!("/assertions/{index}/metadata"), &item.metadata)),
        );
        maps.extend(
            self.controls
                .iter()
                .enumerate()
                .map(|(index, item)| (format!("/controls/{index}/metadata"), &item.metadata)),
        );
        maps.extend(
            self.receipts
                .iter()
                .enumerate()
                .map(|(index, item)| (format!("/receipts/{index}/metadata"), &item.metadata)),
        );
        maps
    }
}

/// Who a stored producer statement is attributed to. Always the producer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Attribution {
    #[default]
    ProducerReported,
}

/// What ingest did with one declared artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Retention {
    /// The bytes were snapshotted into the content-addressed store.
    Retained {
        source_ref: RevisionRef,
        artifact_ref: RevisionRef,
        digest: Digest,
        size_bytes: u64,
    },
    /// No file exists at the path beneath any import root.
    Missing,
    /// The bytes differ from `expected_sha256`; nothing was retained.
    Rejected { actual: Digest },
}

impl Retention {
    pub fn is_retained(&self) -> bool {
        matches!(self, Self::Retained { .. })
    }
}

/// One declared artifact as recorded on the Run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InventoryEntry {
    /// The artifact entry exactly as the envelope declared it.
    pub declared: ProducerArtifact,
    pub retention: Retention,
}

/// The retained envelope bytes and the Source that records them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeRef {
    pub source_ref: RevisionRef,
    pub artifact_ref: RevisionRef,
    pub digest: Digest,
}

/// The `producer_run` section of a Run ingested from an envelope. Everything
/// here is the producer's attributed statement except `envelope`, the
/// per-artifact `retention` and `evidence_completeness`, which ingest computed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProducerRunRecord {
    pub schema_version: ProducerRunVersion,
    pub attribution: Attribution,
    pub producer: ProducerDescriptor,
    pub external_run_id: String,
    pub intent: ProducerIntent,
    /// Execution exactly as reported.
    pub execution: ProducerExecution,
    pub environment: BTreeMap<String, String>,
    pub envelope: EnvelopeRef,
    pub artifacts: Vec<InventoryEntry>,
    pub assertions: Vec<ProducerAssertion>,
    pub controls: Vec<ProducerControl>,
    pub receipts: Vec<ProducerReceipt>,
    pub limitations: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

/// Run body key that holds the [`ProducerRunRecord`].
pub const PRODUCER_RUN_KEY: &str = "producer_run";

/// Retention completeness of a declared inventory: `complete` when every
/// declared artifact was retained, `missing` when none of a nonempty inventory
/// was, `partial` otherwise. It says nothing about artifacts the producer did
/// not declare.
pub fn inventory_completeness(entries: &[InventoryEntry]) -> EvidenceCompleteness {
    let retained = entries
        .iter()
        .filter(|entry| entry.retention.is_retained())
        .count();
    if retained == entries.len() {
        EvidenceCompleteness::Complete
    } else if retained == 0 {
        EvidenceCompleteness::Missing
    } else {
        EvidenceCompleteness::Partial
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn example() -> Value {
        json!({
            "schema_version": "inv.producer-run.v1",
            "producer": {"name": "example-benchmark", "version": "0.1.0", "instance": "local-a"},
            "external_run_id": "bench-run-002",
            "intent": {"label": "compare two queue settings", "external_plan_digest": null},
            "execution": {"state": "completed", "exit_code": 0, "started_at": null, "ended_at": null, "clock_quality": "unknown"},
            "environment": {"revision": "build-abc123", "workload": "fixture-small", "machine": "controlled-fixture"},
            "artifacts": [
                {"role": "original-output", "path": "./bench-result.json", "expected_sha256": null, "media_type": "application/json", "classification": "internal"}
            ],
            "assertions": [{"kind": "reported_metric", "name": "latency.p99", "value": 18.7, "unit": "ms", "validity_claim": "producer_reported"}],
            "controls": [{"name": "steady-load", "producer_result": "reported_pass"}],
            "receipts": [],
            "limitations": ["Single synthetic workload; representative production profile not established"]
        })
    }

    #[test]
    fn plan_example_decodes_and_validates() {
        let run: ProducerRun = serde_json::from_value(example()).unwrap();
        run.validate().unwrap();
        assert_eq!(
            run.assertions[0].validity_claim,
            ValidityClaim::ProducerReported
        );
        assert_eq!(run.execution.exit_code, Some(0));
    }

    #[test]
    fn a_producer_cannot_claim_its_result_verified() {
        let mut value = example();
        value["assertions"][0]["validity_claim"] = json!("valid");
        assert!(serde_json::from_value::<ProducerRun>(value).is_err());
        let mut value = example();
        value["verdict"] = json!("pass");
        assert!(serde_json::from_value::<ProducerRun>(value).is_err());
    }

    #[test]
    fn paths_and_references_are_checked() {
        for (path, ok) in [
            ("./a/b.json", true),
            ("a//b", true),
            ("../a", false),
            ("/etc/x", false),
            ("./", false),
            ("a/../b", false),
        ] {
            assert_eq!(normalized_path(path).is_some(), ok, "{path}");
        }
        let mut value = example();
        value["artifacts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role": "log", "path": "bench-result.json"}));
        let run: ProducerRun = serde_json::from_value(value).unwrap();
        assert_eq!(run.validate().unwrap_err().path, "/artifacts/1/path");
        let mut value = example();
        value["receipts"] = json!([{"kind": "signed-statement", "artifact": "absent.json"}]);
        let run: ProducerRun = serde_json::from_value(value).unwrap();
        assert_eq!(run.validate().unwrap_err().path, "/receipts/0/artifact");
    }
}
