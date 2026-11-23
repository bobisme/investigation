//! Transaction input shared by the frontends and the store.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{
    header::{Actor, ActorDescriptor},
    identity::{Id, Kind},
    input::{self, InputError},
    wire::{Digest, ErrorCode, Failure},
};

pub const MAX_OPERATIONS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum CommandVersion {
    #[serde(rename = "inv.command.v1", alias = "inv.command.v0-alpha")]
    V1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub schema_version: CommandVersion,
    #[schemars(length(min = 1, max = 128))]
    pub command_id: String,
    #[serde(deserialize_with = "required_case")]
    pub case_id: Option<Id>,
    pub actor: Actor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_descriptor: Option<ActorDescriptor>,
    pub preconditions: Vec<RevisionRef>,
    #[schemars(length(min = 1, max = 128))]
    pub operations: Vec<Operation>,
}

fn required_case<'de, D: Deserializer<'de>>(decoder: D) -> Result<Option<Id>, D::Error> {
    Option::<Id>::deserialize(decoder)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 128))]
    pub client_ref: Option<String>,
    #[schemars(with = "std::collections::BTreeMap<String, Value>")]
    pub payload: Value,
}

/// An exact revision already issued by the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevisionRef {
    pub id: Id,
    #[schemars(range(min = 1))]
    #[serde(deserialize_with = "positive_revision")]
    pub revision: u64,
}

fn positive_revision<'de, D: Deserializer<'de>>(decoder: D) -> Result<u64, D::Error> {
    let revision = u64::deserialize(decoder)?;
    if revision == 0 {
        return Err(serde::de::Error::custom("Revision starts at 1"));
    }
    Ok(revision)
}

/// An alias of a record created by an earlier operation in this transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClientRef {
    #[schemars(length(min = 1, max = 128))]
    pub client_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Reference {
    Existing(RevisionRef),
    Client(ClientRef),
}

/// Keep the original parsed request alongside typed input. Structural equality
/// compares parsed fields and attachment digests in order, independent of file
/// location, key order, or presentation options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRequest {
    pub input: Value,
    pub attachments: Vec<Digest>,
}

impl Command {
    pub fn parse(bytes: &[u8]) -> Result<(Self, SemanticRequest), Failure> {
        let input = input::parse_request(bytes).map_err(input_failure)?;
        let command: Self = input::decode(&input).map_err(input_failure)?;
        command.validate_shape()?;
        Ok((
            command,
            SemanticRequest {
                input,
                attachments: Vec::new(),
            },
        ))
    }

    /// All entry points share these semantic envelope requirements. The catalog
    /// separately validates operation-specific payloads and cross-record rules.
    pub fn validate_shape(&self) -> Result<(), Failure> {
        if !valid_token(&self.command_id) {
            return Err(field(
                "/command_id",
                "Use a command ID with 1–128 characters and no control characters.",
            ));
        }
        if self.operations.is_empty() {
            return Err(Failure::new(
                ErrorCode::EmptyTransaction,
                "Supply at least one operation.",
            )
            .at("/operations")
            .with_schema("record"));
        }
        if self.operations.len() > MAX_OPERATIONS {
            return Err(field(
                "/operations",
                "Submit at most 128 operations per transaction.",
            ));
        }
        let creates_case = self.operations.len() == 1 && self.operations[0].op == "case.create";
        match self.case_id {
            None if creates_case && self.preconditions.is_empty() => {}
            Some(id) if id.kind() == Kind::Case && !creates_case => {}
            _ => {
                return Err(field(
                    "/case_id",
                    "A standalone case.create requires null case_id and no preconditions. Other transactions require a case ID.",
                ));
            }
        }
        for (index, precondition) in self.preconditions.iter().enumerate() {
            if precondition.revision == 0 {
                return Err(field(
                    format!("/preconditions/{index}/revision"),
                    "Revision starts at 1.",
                ));
            }
        }
        for (index, operation) in self.operations.iter().enumerate() {
            if operation.op == "case.create" && !creates_case {
                return Err(field(
                    format!("/operations/{index}/op"),
                    "Submit case.create as a standalone transaction.",
                ));
            }
            if operation
                .client_ref
                .as_deref()
                .is_some_and(|token| !valid_token(token))
            {
                return Err(field(
                    format!("/operations/{index}/client_ref"),
                    "Use an alias with 1–128 characters and no control characters.",
                ));
            }
            if !operation.payload.is_object() {
                return Err(field(
                    format!("/operations/{index}/payload"),
                    "Supply a JSON object as the operation payload.",
                ));
            }
        }
        Ok(())
    }
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.chars().count() <= 128 && !token.chars().any(char::is_control)
}

fn field(path: impl Into<String>, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message)
        .at(path)
        .with_schema("record")
}

pub fn input_failure(error: InputError) -> Failure {
    Failure::new(ErrorCode::InvalidInput, error.message)
        .at(error.path)
        .with_schema("record")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CASE_ID: &str = "case_019acd16-0000-7000-8000-000000000001";

    fn request() -> Value {
        json!({
            "schema_version": "inv.command.v0-alpha",
            "command_id": "example-create-question-01",
            "case_id": CASE_ID,
            "actor": "agent:investigator-1",
            "preconditions": [],
            "operations": [{
                "op": "question.create",
                "client_ref": "principal-question",
                "payload": {
                    "title": "Why did p99 latency rise?",
                    "decision": "Identify the leading cause.",
                    "scope": "Synthetic workload on two builds."
                }
            }]
        })
    }

    fn parse(value: &Value) -> Result<(Command, SemanticRequest), Failure> {
        Command::parse(&serde_json::to_vec(value).unwrap())
    }

    #[test]
    fn command_scope_and_limits() {
        let mut request = request();
        assert!(parse(&request).is_ok());
        request["operations"][0]["unexpected"] = json!(true);
        assert_eq!(
            parse(&request).unwrap_err().error.path.as_deref(),
            Some("/operations/0/unexpected")
        );
        request["operations"] = json!([]);
        assert_eq!(
            parse(&request).unwrap_err().error.code,
            ErrorCode::EmptyTransaction
        );
        request["operations"] = json!(vec![
            json!({"op":"question.create","payload":{}});
            MAX_OPERATIONS + 1
        ]);
        assert_eq!(
            parse(&request).unwrap_err().error.path.as_deref(),
            Some("/operations")
        );
        request["operations"] = json!([{"op":"case.create","payload":{}}]);
        assert_eq!(
            parse(&request).unwrap_err().error.path.as_deref(),
            Some("/case_id")
        );
        request["case_id"] = Value::Null;
        assert!(parse(&request).is_ok());
        request.as_object_mut().unwrap().remove("case_id");
        assert!(parse(&request).is_err(), "case_id must be explicitly null");
    }

    #[test]
    fn exact_references_exclude_aliases() {
        let revision = json!({"id":CASE_ID,"revision":1});
        let client = json!({"client_ref":"principal-question"});
        assert!(matches!(
            input::decode(&revision).unwrap(),
            Reference::Existing(_)
        ));
        assert!(matches!(
            input::decode(&client).unwrap(),
            Reference::Client(_)
        ));
        let both = json!({"id":CASE_ID,"revision":1,"client_ref":"principal-question"});
        assert!(input::decode::<Reference>(&both).is_err());
        assert!(input::decode::<Reference>(&json!({"id":CASE_ID,"revision":0})).is_err());
    }

    #[test]
    fn semantic_retries_compare_parsed_values_and_ordered_attachment_digests() {
        let input = request();
        let (_, compact) = parse(&input).unwrap();
        let reordered = input
            .as_object()
            .unwrap()
            .iter()
            .rev()
            .map(|(key, value)| {
                format!(
                    "{}: {}",
                    serde_json::to_string(key).unwrap(),
                    serde_json::to_string_pretty(value).unwrap()
                )
            })
            .collect::<Vec<_>>()
            .join(",\n");
        let (_, pretty) = Command::parse(format!("{{\n{reordered}\n}}").as_bytes()).unwrap();
        assert_eq!(compact, pretty);
        let a: Digest = format!("sha256:{}", "1".repeat(64)).parse().unwrap();
        let b: Digest = format!("sha256:{}", "2".repeat(64)).parse().unwrap();
        let a_then_b = SemanticRequest {
            attachments: vec![a.clone(), b.clone()],
            ..compact
        };
        let b_then_a = SemanticRequest {
            attachments: vec![b, a],
            ..pretty
        };
        assert_ne!(a_then_b, b_then_a);
    }
}
