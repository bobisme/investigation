//! Identity, idempotency and ordering values shared by every capture call.

use std::{collections::BTreeMap, fmt, str::FromStr};

use investigation_core::{
    attempt,
    identity::{Id, Kind},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Inert producer metadata keyed by a namespaced name such as `example-org.instrument`.
///
/// The recorder retains the values opaquely. They never change canonical facts,
/// methods or authority.
pub type Metadata = BTreeMap<String, Value>;

/// Version tag of the capture protocol that the Rust API and the JSON-CLI share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum CaptureVersion {
    /// `inv.capture.v1`.
    #[serde(rename = "inv.capture.v1")]
    V1,
}

impl CaptureVersion {
    /// Every protocol version this build accepts, newest first.
    pub const SUPPORTED: &'static [Self] = &[Self::V1];

    /// The wire tag, for example `inv.capture.v1`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "inv.capture.v1",
        }
    }
}

impl fmt::Display for CaptureVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The five capture operations. Their names are part of the stable contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Operation {
    /// [`crate::Recorder::begin`], wire name `capture.begin`.
    Begin,
    /// [`crate::Recorder::append`], wire name `capture.append`.
    Append,
    /// [`crate::Recorder::attach`], wire name `capture.attach`.
    Attach,
    /// [`crate::Recorder::finalize`], wire name `capture.finalize`.
    Finalize,
    /// [`crate::Recorder::recover`], wire name `capture.recover`.
    Recover,
}

impl Operation {
    /// All operations in lifecycle order.
    pub const ALL: &'static [Self] = &[
        Self::Begin,
        Self::Append,
        Self::Attach,
        Self::Finalize,
        Self::Recover,
    ];

    /// The operation's wire name, for example `capture.begin`.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Begin => "capture.begin",
            Self::Append => "capture.append",
            Self::Attach => "capture.attach",
            Self::Finalize => "capture.finalize",
            Self::Recover => "capture.recover",
        }
    }
}

impl fmt::Display for Operation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.wire_name())
    }
}

/// A rejected identity or key value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidValue {
    field: &'static str,
    rule: &'static str,
}

impl InvalidValue {
    /// The field that failed validation.
    pub fn field(&self) -> &'static str {
        self.field
    }
}

impl fmt::Display for InvalidValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} must be {}", self.field, self.rule)
    }
}

impl std::error::Error for InvalidValue {}

const KEY_RULE: &str = "1 to 128 characters without control characters";
const RUN_RULE: &str = "1 to 256 characters without control characters";

fn bounded_text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.chars().count() <= max && !value.chars().any(char::is_control)
}

macro_rules! validated_string {
    ($(#[$meta:meta])* $name:ident, $field:literal, $max:literal, $rule:expr, $check:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Validate and wrap a value.
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidValue> {
                let value = value.into();
                if ($check)(value.as_str()) {
                    Ok(Self(value))
                } else {
                    Err(InvalidValue { field: $field, rule: $rule })
                }
            }

            /// The validated value.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = InvalidValue;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl FromStr for $name {
            type Err = InvalidValue;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({
                    "type": "string",
                    "minLength": 1,
                    "maxLength": $max,
                    "description": $rule,
                })
            }
        }
    };
}

validated_string!(
    /// Caller-chosen idempotency key for one call.
    ///
    /// Retrying a call with the same key and identical content returns the original
    /// receipt. Reusing a key with different content is a conflict, never an update.
    /// For `begin` and `finalize` the key is the journal command ID; for `append` and
    /// `attach` it is the producer's item key, unique within one handle.
    IdempotencyKey,
    "idempotency key",
    128,
    KEY_RULE,
    |value| bounded_text(value, 128)
);

validated_string!(
    /// The producer's own identifier for one logical attempt.
    ///
    /// Together with the producer name it identifies the attempt; a retried `begin`
    /// for the same pair resumes the same Run rather than declaring a new one.
    ExternalRunId,
    "external run ID",
    256,
    RUN_RULE,
    |value| bounded_text(value, 256)
);

/// A producer namespace such as `example-benchmark`.
///
/// The recorder never interprets the name. It only scopes idempotency and
/// attribution, and the core contains no knowledge of any particular producer.
///
/// The rule is [`attempt::is_producer_name`], the same one the producer-run
/// envelope's `producer.name` and an ingest acquisition `namespace` follow, so
/// one producer can record an attempt by either path under one name. `:` and
/// `/` are refused. Deserialization also accepts the `:` and `/` names that
/// capture recordings made before the rules were aligned may hold
/// ([`attempt::is_recorded_producer_name`]), so stored requests replay,
/// recover and finalize; `capture.begin` refuses such a name for any new
/// declaration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(into = "String")]
pub struct ProducerName(String);

impl ProducerName {
    /// Validate and wrap a value.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidValue> {
        let value = value.into();
        if attempt::is_producer_name(&value) {
            Ok(Self(value))
        } else {
            Err(Self::invalid())
        }
    }

    /// The validated value.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether a new attempt may use this name. Only a name read back from a
    /// recording made before the rules were aligned can fail this.
    pub fn is_current(&self) -> bool {
        attempt::is_producer_name(&self.0)
    }

    fn invalid() -> InvalidValue {
        InvalidValue {
            field: "producer name",
            rule: attempt::PRODUCER_NAME_RULE,
        }
    }
}

impl<'de> Deserialize<'de> for ProducerName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if attempt::is_recorded_producer_name(&value) {
            Ok(Self(value))
        } else {
            Err(serde::de::Error::custom(Self::invalid()))
        }
    }
}

impl TryFrom<String> for ProducerName {
    type Error = InvalidValue;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl FromStr for ProducerName {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl From<ProducerName> for String {
    fn from(value: ProducerName) -> Self {
        value.0
    }
}

impl fmt::Display for ProducerName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The published v1 capture schemas froze this schema without a `pattern`, so
/// the character rule is stated in `description` and enforced at runtime.
impl JsonSchema for ProducerName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ProducerName".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": attempt::PRODUCER_NAME_MAX_CHARS,
            "description": attempt::PRODUCER_NAME_RULE,
        })
    }
}

/// Self-reported producer identity. It is attribution, not authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ProducerIdentity {
    /// Producer namespace.
    pub name: ProducerName,
    /// Producer version, when the producer reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Producer instance, such as a host-local label, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

impl ProducerIdentity {
    /// Identity with only a name. Set `version` and `instance` as fields.
    pub fn new(name: ProducerName) -> Self {
        Self {
            name,
            version: None,
            instance: None,
        }
    }
}

/// One-based position of an accepted item within a capture handle.
///
/// Appended facts and attachment descriptors share one sequence. The recorder
/// accepts only the next expected position, so a lost middle item is detectable.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "u64", into = "u64")]
pub struct Position(#[schemars(range(min = 1))] u64);

impl Position {
    /// The first position on every handle.
    pub const FIRST: Self = Self(1);

    /// A position, or `None` for zero.
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// The numeric position.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The following position, or `None` on overflow.
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

impl TryFrom<u64> for Position {
    type Error = InvalidValue;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(InvalidValue {
            field: "position",
            rule: "at least 1",
        })
    }
}

impl From<Position> for u64 {
    fn from(position: Position) -> Self {
        position.0
    }
}

impl fmt::Display for Position {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Opaque capability naming one capture, returned by `begin`.
///
/// It is an Investigation `handle_…` identifier. Treat it as a token: persist it
/// with the producer's own run state so a restarted producer can call `recover`.
/// A handle grants no authority outside this recorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "Id", into = "Id")]
pub struct CaptureHandle(Id);

impl CaptureHandle {
    /// Wrap an identifier of kind `handle`.
    pub fn from_id(id: Id) -> Result<Self, InvalidValue> {
        id.expect_kind(Kind::Handle)
            .map(Self)
            .map_err(|_| InvalidValue {
                field: "capture handle",
                rule: "an identifier with the handle_ prefix",
            })
    }

    /// The underlying identifier.
    pub const fn id(self) -> Id {
        self.0
    }
}

impl TryFrom<Id> for CaptureHandle {
    type Error = InvalidValue;

    fn try_from(id: Id) -> Result<Self, Self::Error> {
        Self::from_id(id)
    }
}

impl From<CaptureHandle> for Id {
    fn from(handle: CaptureHandle) -> Self {
        handle.0
    }
}

impl FromStr for CaptureHandle {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let id = value.parse::<Id>().map_err(|_| InvalidValue {
            field: "capture handle",
            rule: "an identifier with the handle_ prefix",
        })?;
        Self::from_id(id)
    }
}

impl fmt::Display for CaptureHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl JsonSchema for CaptureHandle {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CaptureHandle".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^handle_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
        })
    }
}

/// Whether a receipt reports a first acceptance or a replay of an earlier one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReceiptResult {
    /// This call made the item durable.
    Accepted,
    /// An identical earlier call made it durable; this is that call's receipt.
    AlreadyAccepted,
}
