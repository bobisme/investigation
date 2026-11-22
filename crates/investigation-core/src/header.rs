//! Common record metadata. Actor and source clocks are claims, not verified identity or causality.
use crate::identity::Id;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::BTreeMap, fmt, str::FromStr};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Actor(String);
impl JsonSchema for Actor {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Actor".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type":"string", "minLength":1, "maxLength":128, "pattern":"^[A-Za-z0-9:_.-]+$"})
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActorError;
impl fmt::Display for ActorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("actor must be a nonempty, bounded identifier using ASCII letters, digits, ':', '_', '-', or '.'")
    }
}
impl std::error::Error for ActorError {}
impl FromStr for Actor {
    type Err = ActorError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty()
            || s.len() > 128
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-' | b'.'))
        {
            return Err(ActorError);
        }
        Ok(Self(s.to_owned()))
    }
}
impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl Serialize for Actor {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for Actor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    Agent,
}
/// Every field is self-reported, including kind; it is not an authentication claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActorDescriptor {
    pub kind: ActorKind,
    pub model_family: Option<String>,
    pub model_version: Option<String>,
    pub harness: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActorResolutionError {
    Conflict,
    Missing,
}
/// Explicit flag and envelope must agree. Configuration and environment are fallbacks.
pub fn resolve_actor(
    flag: Option<&Actor>,
    envelope: Option<&Actor>,
    config: Option<&Actor>,
    environment: Option<&Actor>,
) -> Result<Actor, ActorResolutionError> {
    if let (Some(a), Some(b)) = (flag, envelope)
        && a != b
    {
        return Err(ActorResolutionError::Conflict);
    }
    flag.or(envelope)
        .or(config)
        .or(environment)
        .cloned()
        .ok_or(ActorResolutionError::Missing)
}

/// Explicit unknown is distinct from a known zero, empty string, or empty list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "state",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Knowledge<T> {
    Unknown,
    Known(T),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClockQuality {
    Unknown,
    Unverified,
    Synchronized,
    Attested,
}
/// The original time and offset are source claims and must not determine ordering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceTime {
    pub timestamp: Knowledge<String>,
    pub offset: Knowledge<String>,
    pub clock_quality: ClockQuality,
}
/// A validated UTC RFC 3339 timestamp with an explicit fractional precision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct UtcTimestamp(String);
impl JsonSchema for UtcTimestamp {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "UtcTimestamp".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type":"string", "format":"date-time", "pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\\.[0-9]{1,9}Z$"})
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimestampError;
impl fmt::Display for TimestampError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected valid UTC RFC 3339 timestamp with fractional precision")
    }
}
impl std::error::Error for TimestampError {}
impl FromStr for UtcTimestamp {
    type Err = TimestampError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((whole, fraction)) = s.strip_suffix('Z').and_then(|s| s.split_once('.')) else {
            return Err(TimestampError);
        };
        let valid_whole = whole.len() == 19
            && whole.bytes().enumerate().all(|(i, b)| match i {
                4 | 7 => b == b'-',
                10 => b == b'T',
                13 | 16 => b == b':',
                _ => b.is_ascii_digit(),
            });
        if !valid_whole
            || !(1..=9).contains(&fraction.len())
            || !fraction.bytes().all(|b| b.is_ascii_digit())
            || time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                .is_err()
        {
            return Err(TimestampError);
        }
        Ok(Self(s.to_owned()))
    }
}
impl fmt::Display for UtcTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for UtcTimestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}
/// Injectable clock for deterministic fixtures.
pub trait Clock {
    fn now_utc(&mut self) -> UtcTimestamp;
}
pub struct SystemClock;
impl Clock for SystemClock {
    fn now_utc(&mut self) -> UtcTimestamp {
        let now = time::OffsetDateTime::now_utc();
        let description = time::format_description::parse(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:9]Z",
        )
        .expect("static timestamp format");
        now.format(&description)
            .expect("UTC timestamp formats")
            .parse()
            .expect("system UTC is valid")
    }
}

#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Revision(pub u64);
/// Header versions use e.g. `inv.record.v0-alpha`; event versions are independent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum SchemaVersion {
    #[serde(rename = "inv.record.v0-alpha")]
    RecordV0Alpha,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationLevel {
    Public,
    #[default]
    Internal,
    Restricted,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Classification {
    pub level: ClassificationLevel,
    pub reason: Option<String>,
    pub tags: Vec<String>,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub references: Vec<Id>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordHeader {
    pub id: Id,
    pub case_id: Id,
    pub schema_version: SchemaVersion,
    pub created_by: Actor,
    pub actor_descriptor_self_reported: Option<ActorDescriptor>,
    pub recorded_at: UtcTimestamp,
    pub observed_at: Option<SourceTime>,
    pub acquired_at: Option<SourceTime>,
    pub acceptance_sequence: u64,
    pub revision: Revision,
    /// Transactions that accepted earlier revisions, in acceptance order.
    pub history: Vec<Id>,
    pub classification: Classification,
    pub provenance: Provenance,
    /// Opaque until a pinned pack supplies a schema. No executable semantics.
    pub extensions: BTreeMap<String, serde_json::Value>,
}

/// Bounded display text, never an identity. Removes control and common markup delimiters.
pub fn display_title(input: &str, max_chars: usize) -> String {
    input
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | '[' | ']' | '*' | '_' | '`' | '#'))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .chars()
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_is_not_zero_or_empty() {
        assert_ne!(
            serde_json::to_string(&Knowledge::<u64>::Unknown).unwrap(),
            serde_json::to_string(&Knowledge::Known(0)).unwrap()
        );
        assert_ne!(
            serde_json::to_string(&Knowledge::<String>::Unknown).unwrap(),
            serde_json::to_string(&Knowledge::Known(String::new())).unwrap()
        );
        assert_eq!(
            Classification::default().level,
            ClassificationLevel::Internal
        );
    }
    #[test]
    fn actors_resolve_without_authentication_claims() {
        let a: Actor = "agent:investigator-1".parse().unwrap();
        let b: Actor = "human:bob".parse().unwrap();
        assert_eq!(
            resolve_actor(Some(&a), Some(&a), Some(&b), None),
            Ok(a.clone())
        );
        assert_eq!(
            resolve_actor(Some(&a), Some(&b), None, None),
            Err(ActorResolutionError::Conflict)
        );
        assert_eq!(resolve_actor(None, None, Some(&b), Some(&a)), Ok(b));
        assert_eq!(
            serde_json::from_str::<Actor>(&serde_json::to_string(&a).unwrap()).unwrap(),
            a
        );
    }
    #[test]
    fn title_is_bounded_and_safe() {
        assert_eq!(display_title("# Hello\n<script>", 8), "Hello sc");
    }
    #[test]
    fn timestamps_validate_and_clock_is_utc() {
        assert!("2026-11-22T15:10:01.123Z".parse::<UtcTimestamp>().is_ok());
        for bad in [
            "2026-11-22T15:10:01Z",
            "2026-02-30T15:10:01.1Z",
            "2026-11-22T15:10:01.1+00:00",
            "2026-11-22t15:10:01.1Z",
        ] {
            assert!(bad.parse::<UtcTimestamp>().is_err());
        }
        let t = SystemClock.now_utc();
        assert_eq!(t.to_string().parse::<UtcTimestamp>().unwrap(), t);
    }
    #[test]
    fn schema_version_is_not_arbitrary() {
        assert_eq!(
            serde_json::to_string(&SchemaVersion::RecordV0Alpha).unwrap(),
            "\"inv.record.v0-alpha\""
        );
        assert!(serde_json::from_str::<SchemaVersion>("\"anything\"").is_err());
    }
}
