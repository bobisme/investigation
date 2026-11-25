//! Shared receipts, source digests, and structured failures.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    command::RevisionRef,
    evidence::{SelectorValidation, TextQuoteState},
    header::Actor,
    identity::Id,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ResponseVersion {
    #[serde(rename = "inv.response.v1", alias = "inv.response.v0-alpha")]
    V1,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);

impl JsonSchema for Digest {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Digest".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "pattern": r"^sha256:[0-9a-f]{64}$" })
    }
}

impl Digest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::str::FromStr for Digest {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.to_owned().try_into()
    }
}

impl TryFrom<String> for Digest {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        }) {
            Ok(Self(value))
        } else {
            Err("Expected a SHA-256 digest as sha256: followed by 64 lowercase hexadecimal digits")
        }
    }
}

impl From<Digest> for String {
    fn from(digest: Digest) -> Self {
        digest.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub sequence: u64,
    pub head_hash: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordChange {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_ref: Option<String>,
    pub id: Id,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommitResult {
    Committed,
    AlreadyCommitted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStatus {
    Current,
    Pending,
    Degraded,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    pub index: ProjectionStatus,
    pub vault: ProjectionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GuidanceKind {
    LocalRead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Guidance {
    pub kind: GuidanceKind,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiptArtifact {
    pub artifact: RevisionRef,
    pub digest: Digest,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiptObservation {
    pub observation: RevisionRef,
    pub selector_validation: SelectorValidation,
    pub quote_state: TextQuoteState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema_version: ResponseVersion,
    pub ok: bool,
    pub result: CommitResult,
    pub command_id: String,
    pub transaction_id: Id,
    pub snapshot: Snapshot,
    pub created: Vec<RecordChange>,
    pub updated: Vec<RecordChange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ReceiptArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observation_checks: Vec<ReceiptObservation>,
    pub projection: Projection,
    pub warnings: Vec<String>,
    pub next: Vec<Guidance>,
    /// The actor this write was attributed to and where that actor came from (bn-2wct). The
    /// CLI sets it on every write it answers; the journal does not keep it, so a receipt read
    /// back from the store has none. Absent from receipts of earlier v1 builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<Attribution>,
}

/// Which actor a write used and why (bn-2wct). An attribution is a claim, never a verified
/// identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Attribution {
    pub actor: Actor,
    pub source: ActorSource,
    /// The actor that `init --actor` recorded for this workspace (`local/actor.toml`) when
    /// `AGENT` named a different one and took precedence. The receipt then also carries the
    /// warning `actor.agent_overrides_local`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides_local_actor: Option<Actor>,
}

/// Where a write's actor came from, in precedence order: the first that names an actor wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActorSource {
    /// `--actor`.
    Flag,
    /// The request's own `actor` field (a command, manifest or capture request).
    Request,
    /// The `actor` setting in the workspace's `investigation.toml`.
    WorkspaceConfig,
    /// The `AGENT` environment variable.
    Environment,
    /// `local/actor.toml`, written by `init --actor` for this machine.
    LocalActor,
}

/// Leave `attribution` unconstrained in a schema that flattens [`Receipt`] into an open object
/// (the ingest and producer-run receipts). There a new property must accept any value (ADR
/// 0007); its shape is the one `receipt.schema.json` publishes.
pub fn unconstrain_attribution(schema: &mut schemars::Schema) {
    if let Some(property) = schema
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .and_then(|properties| properties.get_mut("attribution"))
    {
        *property = Value::Bool(true);
    }
    if let Some(definitions) = schema.get_mut("$defs").and_then(Value::as_object_mut) {
        definitions.remove("Attribution");
        definitions.remove("ActorSource");
    }
    // `Actor` too, unless something else in the schema still refers to it.
    let referenced = Value::from(schema.clone())
        .to_string()
        .contains("\"#/$defs/Actor\"");
    if !referenced && let Some(definitions) = schema.get_mut("$defs").and_then(Value::as_object_mut)
    {
        definitions.remove("Actor");
    }
}

impl ActorSource {
    /// How text output names the source.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Flag => "--actor",
            Self::Request => "the request's actor",
            Self::WorkspaceConfig => "investigation.toml",
            Self::Environment => "AGENT",
            Self::LocalActor => "local/actor.toml, written by init --actor",
        }
    }
}

impl Receipt {
    /// Preserve the durable identity and result details on a replay.
    pub fn as_retry(&self) -> Self {
        Self {
            result: CommitResult::AlreadyCommitted,
            ..self.clone()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommitStatus {
    NotCommitted,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidInput,
    EmptyTransaction,
    BudgetTooSmall,
    WorkspaceRequired,
    CaseRequired,
    ObjectRequired,
    UnknownReference,
    AmbiguousReference,
    RevisionConflict,
    IdempotencyKeyReused,
    GateFailed,
    RetrospectiveRequired,
    RetrospectiveStale,
    IntegrityFailure,
    WorkspaceBusy,
    IoFailure,
    DurabilityUnknown,
}

impl ErrorCode {
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::InvalidInput | Self::EmptyTransaction | Self::BudgetTooSmall => 2,
            Self::WorkspaceRequired
            | Self::CaseRequired
            | Self::ObjectRequired
            | Self::UnknownReference
            | Self::AmbiguousReference => 3,
            Self::RevisionConflict | Self::IdempotencyKeyReused => 4,
            Self::GateFailed | Self::RetrospectiveRequired | Self::RetrospectiveStale => 5,
            Self::IntegrityFailure => 6,
            Self::WorkspaceBusy | Self::IoFailure | Self::DurabilityUnknown => 7,
        }
    }

    pub const fn commit_status(self) -> CommitStatus {
        match self {
            Self::DurabilityUnknown => CommitStatus::Unknown,
            _ => CommitStatus::NotCommitted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RejectionResult {
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Repair {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorDetail {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair: Option<Repair>,
    /// Further refusals of the same transaction, each with its own path and repair, found in the
    /// same validation pass (bn-29t7). The fields above describe the first one. An operation
    /// that depends on a refused operation's `client_ref` is not reported again. Absent when
    /// there is only one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_errors: Vec<ErrorDetail>,
}

/// The most refusals one transaction reports beside the first.
pub const MAX_ADDITIONAL_ERRORS: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub schema_version: ResponseVersion,
    pub ok: bool,
    pub result: RejectionResult,
    pub commit_status: CommitStatus,
    // Keep the error side of Result small even with structured repair values.
    pub error: Box<ErrorDetail>,
}

impl Failure {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            schema_version: ResponseVersion::V1,
            ok: false,
            result: RejectionResult::Rejected,
            commit_status: code.commit_status(),
            error: Box::new(ErrorDetail {
                code,
                message: message.into(),
                path: None,
                expected: None,
                actual: None,
                retry: None,
                repair: None,
                additional_errors: Vec::new(),
            }),
        }
    }

    pub fn at(mut self, path: impl Into<String>) -> Self {
        self.error.path = Some(path.into());
        self
    }

    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.error
            .repair
            .get_or_insert(Repair {
                argv: None,
                schema: None,
            })
            .schema = Some(schema.into());
        self
    }

    pub fn with_argv(mut self, argv: Vec<String>) -> Self {
        self.error
            .repair
            .get_or_insert(Repair {
                argv: None,
                schema: None,
            })
            .argv = Some(argv);
        self
    }

    pub fn exit_code(&self) -> u8 {
        self.error.code.exit_code()
    }

    /// A reference of a kind the field does not accept (bn-29t7): the
    /// message, `expected.kinds` and `actual.kind` name the accepted kinds and
    /// the given one by ID prefix.
    pub fn wrong_kind(
        code: ErrorCode,
        path: &str,
        accepted: &[crate::identity::Kind],
        given: crate::identity::Kind,
    ) -> Self {
        let names: Vec<String> = accepted
            .iter()
            .map(|kind| format!("{kind:?} ({}_)", kind.prefix()))
            .collect();
        let mut failure = Self::new(
            code,
            format!(
                "This reference accepts {}; it names a {given:?} ({}_).",
                match names.len() {
                    1 => format!("only a {}", names[0]),
                    _ => format!("one of {}", names.join(", ")),
                },
                given.prefix()
            ),
        )
        .at(path);
        failure.error.expected = Some(serde_json::json!({
            "kinds": accepted.iter().map(|kind| kind.prefix()).collect::<Vec<_>>()
        }));
        failure.error.actual = Some(serde_json::json!({"kind": given.prefix()}));
        failure
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // An empty path is the document root: no ": " prefix without a name.
        if let Some(path) = self.error.path.as_deref().filter(|path| !path.is_empty()) {
            write!(formatter, "{path}: ")?;
        }
        formatter.write_str(&self.error.message)
    }
}

impl std::error::Error for Failure {}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> Id {
        value.parse().unwrap()
    }

    #[test]
    fn a_degraded_view_keeps_the_commit_and_retry_ids() {
        let receipt = Receipt {
            schema_version: ResponseVersion::V1,
            ok: true,
            result: CommitResult::Committed,
            command_id: "first-case".into(),
            transaction_id: id("tx_019acd16-0000-7000-8000-000000000001"),
            snapshot: Snapshot {
                sequence: 1,
                head_hash: format!("sha256:{}", "a".repeat(64)).parse().unwrap(),
            },
            created: vec![RecordChange {
                client_ref: Some("first".into()),
                id: id("case_019acd16-0000-7000-8000-000000000002"),
                revision: 1,
            }],
            updated: Vec::new(),
            artifacts: Vec::new(),
            observation_checks: Vec::new(),
            projection: Projection {
                index: ProjectionStatus::Current,
                vault: ProjectionStatus::Degraded,
            },
            warnings: vec!["Vault projection failed after the journal commit".into()],
            next: Vec::new(),
            attribution: None,
        };
        let serialized = serde_json::to_value(&receipt).unwrap();
        let retry = serde_json::to_value(receipt.as_retry()).unwrap();
        assert_eq!(serialized["result"], "committed");
        assert_eq!(serialized["ok"], true);
        assert_eq!(retry["result"], "already_committed");
        for field in ["snapshot", "transaction_id", "created", "updated"] {
            assert_eq!(serialized[field], retry[field], "{field}");
        }
    }

    #[test]
    fn rejection_distinguishes_invalid_input_from_uncertain_commit_status() {
        let rejection = Failure::new(ErrorCode::RevisionConflict, "Read the latest revision.")
            .at("/preconditions/0/revision")
            .with_schema("record");
        assert_eq!(rejection.exit_code(), 4);
        assert_eq!(rejection.commit_status, CommitStatus::NotCommitted);
        let uncertain = Failure::new(
            ErrorCode::DurabilityUnknown,
            "Recover before retrying with the same ID.",
        );
        assert_eq!(uncertain.exit_code(), 7);
        assert_eq!(uncertain.commit_status, CommitStatus::Unknown);
    }
}
