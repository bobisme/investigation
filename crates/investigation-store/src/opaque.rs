//! Policy for source and producer content the core does not understand.
//!
//! Unknown optional metadata is retained as raw data within bounds and is never
//! interpreted. Content that declares an unsupported required schema version is
//! retained as opaque bytes with a diagnostic; nothing is mapped into typed
//! facts. Unknown keys in this project's own versioned payloads stay errors.

use std::collections::BTreeMap;

use serde_json::Value;

/// Maximum serialized size of one source's opaque metadata.
pub const MAX_METADATA_BYTES: usize = 64 * 1024;
pub const MAX_METADATA_DEPTH: usize = 16;
pub const MAX_METADATA_KEY_CHARS: usize = 128;
/// Content larger than this is retained without a version probe.
pub const MAX_PROBE_BYTES: u64 = 1024 * 1024;
const ENVELOPE_FAMILY: &str = "inv.producer-run.";
/// Producer envelope versions this build can interpret.
pub const SUPPORTED_ENVELOPE_VERSIONS: &[&str] =
    &[investigation_core::producer_run::PRODUCER_RUN_V1];

/// Return a message and JSON pointer suffix when metadata exceeds a bound.
pub fn validate_metadata(metadata: &BTreeMap<String, Value>) -> Result<(), (String, String)> {
    let mut total = 2;
    for (key, value) in metadata {
        let pointer = format!("/{}", key.replace('~', "~0").replace('/', "~1"));
        if key.is_empty() || key.chars().count() > MAX_METADATA_KEY_CHARS {
            return Err((
                format!("Use metadata keys of 1–{MAX_METADATA_KEY_CHARS} characters."),
                pointer,
            ));
        }
        if depth(value) > MAX_METADATA_DEPTH {
            return Err((
                format!("Nest metadata at most {MAX_METADATA_DEPTH} levels deep."),
                pointer,
            ));
        }
        total += key.len() + serde_json::to_string(value).map_or(usize::MAX / 4, |s| s.len()) + 4;
        if total > MAX_METADATA_BYTES {
            return Err((
                format!(
                    "Opaque metadata exceeds {MAX_METADATA_BYTES} bytes. Retain large producer output as an artifact instead."
                ),
                String::new(),
            ));
        }
    }
    Ok(())
}

fn depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
        Value::Object(map) => 1 + map.values().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeProbe {
    /// Not a recognizable producer envelope; retained bytes stay as they are.
    NotAnEnvelope,
    Supported,
    UnsupportedVersion(String),
}

/// Look only at the declared schema version of a producer envelope.
///
/// The probe reads a top-level object's keys and skips every value except
/// `schema_version` without building it, so nesting costs one byte of parser
/// state per level and no recursion. Any other shape is not an envelope.
pub fn probe_envelope(bytes: &[u8]) -> EnvelopeProbe {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let Ok(version) = serde::Deserializer::deserialize_map(&mut decoder, VersionOnly) else {
        return EnvelopeProbe::NotAnEnvelope;
    };
    if decoder.end().is_err() {
        return EnvelopeProbe::NotAnEnvelope;
    }
    match version.as_deref() {
        Some(version) if SUPPORTED_ENVELOPE_VERSIONS.contains(&version) => EnvelopeProbe::Supported,
        Some(version) if version.starts_with(ENVELOPE_FAMILY) => {
            EnvelopeProbe::UnsupportedVersion(version.chars().take(64).collect())
        }
        _ => EnvelopeProbe::NotAnEnvelope,
    }
}

/// The last top-level `schema_version` string of a JSON object, if any.
struct VersionOnly;

impl<'de> serde::de::Visitor<'de> for VersionOnly {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut version = None;
        while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
            if key == "schema_version" {
                // A non-string version is not an envelope, as before.
                version = Some(map.next_value::<String>()?);
            } else {
                map.next_value::<serde::de::IgnoredAny>()?;
            }
        }
        Ok(version)
    }
}

/// Diagnostic stored with the Source and returned in the receipt.
pub fn unsupported_version_diagnostic(version: &str) -> String {
    format!(
        "Unsupported producer schema version {version:?}: retained as opaque bytes; no fields were interpreted or mapped into facts."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bounds_and_probe() {
        let ok = BTreeMap::from([("a.b".to_string(), json!({"x": [1, null]}))]);
        assert!(validate_metadata(&ok).is_ok());
        let big = BTreeMap::from([("a.b".to_string(), json!("x".repeat(MAX_METADATA_BYTES)))]);
        assert!(validate_metadata(&big).is_err());
        let mut deep = json!(1);
        for _ in 0..20 {
            deep = json!([deep]);
        }
        assert!(validate_metadata(&BTreeMap::from([("a.b".into(), deep)])).is_err());
        assert_eq!(
            probe_envelope(br#"{"schema_version":"inv.producer-run.v1"}"#),
            EnvelopeProbe::Supported
        );
        assert_eq!(
            probe_envelope(br#"{"schema_version":"inv.producer-run.v9"}"#),
            EnvelopeProbe::UnsupportedVersion("inv.producer-run.v9".into())
        );
        assert_eq!(probe_envelope(b"not json"), EnvelopeProbe::NotAnEnvelope);
        assert_eq!(
            probe_envelope(br#"{"schema_version":"other.v2"}"#),
            EnvelopeProbe::NotAnEnvelope
        );
    }
}
