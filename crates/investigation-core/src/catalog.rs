//! Single source of operation discovery, schemas, examples and dispatch.

use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, schema_for};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{
    case::{CaseCreate, QuestionCreate},
    command::{Command, input_failure},
    identity::Kind,
    input,
    state::OperationContext,
    wire::{ErrorCode, Failure, Receipt},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Deferred,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct OperationInfo {
    pub name: &'static str,
    pub stage: &'static str,
    pub summary: &'static str,
    pub when_to_use: &'static str,
    pub lifecycle: &'static str,
    pub effects: &'static str,
    pub returns: &'static str,
    pub revision_rule: &'static str,
    pub idempotency: &'static str,
    pub repair: &'static str,
    pub related: &'static [&'static str],
}

type PayloadValidator = Box<dyn Fn(&Value) -> Result<(), Failure>>;
type Handler = Box<dyn Fn(&Value, &mut OperationContext<'_>) -> Result<(), Failure>>;

#[derive(Serialize)]
pub struct OperationDescriptor {
    #[serde(flatten)]
    info: OperationInfo,
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_schema: Option<Schema>,
    #[serde(skip_serializing_if = "Option::is_none")]
    example: Option<Value>,
    #[serde(skip)]
    validate: Option<PayloadValidator>,
    #[serde(skip)]
    handler: Option<Handler>,
}

impl OperationDescriptor {
    pub fn info(&self) -> &OperationInfo {
        &self.info
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn payload_schema(&self) -> Option<&Schema> {
        self.payload_schema.as_ref()
    }

    pub fn example(&self) -> Option<&Value> {
        self.example.as_ref()
    }

    pub fn active<P: DeserializeOwned + JsonSchema + 'static>(
        info: OperationInfo,
        example: Value,
        validate: fn(&P) -> Result<(), Failure>,
        handler: fn(P, &mut OperationContext<'_>) -> Result<(), Failure>,
    ) -> Self {
        let execute = move |value: &Value, context: &mut OperationContext<'_>| {
            let payload = decode(value, info.name, validate)?;
            handler(payload, context)
        };
        Self {
            status: Status::Active,
            handler: Some(Box::new(execute)),
            ..Self::deferred::<P>(info, example, validate)
        }
    }

    pub fn deferred<P: DeserializeOwned + JsonSchema + 'static>(
        info: OperationInfo,
        example: Value,
        validate: fn(&P) -> Result<(), Failure>,
    ) -> Self {
        Self {
            info,
            status: Status::Deferred,
            payload_schema: Some(schema_for!(P)),
            example: Some(example),
            validate: Some(Box::new(move |value| {
                decode(value, info.name, validate).map(drop)
            })),
            handler: None,
        }
    }

    /// Reserve a name before the corresponding wire payload is stable.
    pub fn reserved(name: &'static str) -> Self {
        Self {
            info: OperationInfo {
                name,
                stage: "deferred",
                summary: "Reserved for a later contract.",
                when_to_use: "Discover the current operation set before submitting a transaction.",
                lifecycle: "Deferred.",
                effects: "None.",
                returns: "None.",
                revision_rule: "Deferred.",
                idempotency: "Deferred.",
                repair: "Choose an available operation.",
                related: &[],
            },
            status: Status::Deferred,
            payload_schema: None,
            example: None,
            validate: None,
            handler: None,
        }
    }

    /// Check an embedded example using the same typed decoder as execution.
    /// Deferred contracts can publish examples without exposing an executable handler.
    pub fn validate_example(&self) -> Result<(), Failure> {
        match (&self.validate, &self.example) {
            (Some(validate), Some(example)) => validate(example),
            (None, None) => Ok(()),
            _ => Err(Failure::new(
                ErrorCode::IntegrityFailure,
                "Incomplete operation example.",
            )),
        }
    }

    pub fn validate_payload(&self, value: &Value) -> Result<(), Failure> {
        self.available()?;
        let validate = self.validate.as_ref().ok_or_else(|| {
            Failure::new(
                ErrorCode::IntegrityFailure,
                "Available operation has no decoder.",
            )
        })?;
        validate(value)
    }

    pub(crate) fn execute(
        &self,
        value: &Value,
        context: &mut OperationContext<'_>,
    ) -> Result<(), Failure> {
        self.available()?(value, context)
    }

    fn available(&self) -> Result<&Handler, Failure> {
        self.handler.as_ref().ok_or_else(|| {
            Failure::new(
                ErrorCode::InvalidInput,
                format!("Operation {:?} is deferred.", self.info.name),
            )
            .with_schema("list")
        })
    }
}

#[derive(Default)]
pub struct Catalog {
    descriptors: BTreeMap<&'static str, OperationDescriptor>,
}

impl Catalog {
    pub fn new(
        descriptors: impl IntoIterator<Item = OperationDescriptor>,
    ) -> Result<Self, Failure> {
        let mut catalog = Self::default();
        for descriptor in descriptors {
            let name = descriptor.info.name;
            if catalog.descriptors.insert(name, descriptor).is_some() {
                return Err(Failure::new(
                    ErrorCode::IntegrityFailure,
                    format!("Duplicate operation name {name:?}."),
                ));
            }
        }
        Ok(catalog)
    }

    pub fn get(&self, name: &str) -> Option<&OperationDescriptor> {
        self.descriptors.get(name)
    }

    pub fn operations(&self) -> impl Iterator<Item = &OperationDescriptor> {
        self.descriptors.values()
    }

    pub fn available(&self) -> impl Iterator<Item = &OperationDescriptor> {
        self.operations()
            .filter(|descriptor| descriptor.status == Status::Active)
    }

    pub fn available_operation(&self, name: &str) -> Result<&OperationDescriptor, Failure> {
        let descriptor = self.get(name).ok_or_else(|| {
            Failure::new(
                ErrorCode::InvalidInput,
                "Unknown operation. Inspect the list of available operations.",
            )
            .with_schema("list")
        })?;
        descriptor.available()?;
        Ok(descriptor)
    }
}

fn decode<P: DeserializeOwned>(
    value: &Value,
    operation: &str,
    validate: fn(&P) -> Result<(), Failure>,
) -> Result<P, Failure> {
    let payload =
        input::decode(value).map_err(|error| input_failure(error).with_schema(operation))?;
    validate(&payload).map_err(|error| error.with_schema(operation))?;
    Ok(payload)
}

const COMMAND_IDEMPOTENCY: &str =
    "Retry the identical command with its original actor and command ID.";

/// Wire schemas that apply across the operation catalog.
pub fn wire_schema(name: &str) -> Option<Schema> {
    match name {
        "record" => Some(schema_for!(Command)),
        "receipt" => Some(schema_for!(Receipt)),
        "error" => Some(schema_for!(Failure)),
        "evidence-selector" => Some(schema_for!(crate::evidence::SelectorRequest)),
        _ => None,
    }
}

/// Public record operations for the alpha schema.
pub fn standard_catalog() -> Catalog {
    Catalog::new(
        [
            OperationDescriptor::active(
                OperationInfo {
                    name: "case.create",
                    stage: "start",
                    summary: "Open a bounded investigation or recording session.",
                    when_to_use: "Start a new case to collect evidence or compare options.",
                    lifecycle: "Standalone transaction with null case_id.",
                    effects: "Create one case record.",
                    returns: "Case ID and revision 1.",
                    revision_rule: "No preconditions.",
                    idempotency: COMMAND_IDEMPOTENCY,
                    repair: "Use a standalone transaction with a nonempty title and case mode.",
                    related: &["question.create"],
                },
                json!({"title": "Request latency notes", "mode": "record"}),
                validate_case_create,
                |payload: CaseCreate, context| {
                    context.create(Kind::Case, record_body(payload.into_body())?)?;
                    Ok(())
                },
            ),
            OperationDescriptor::active(
                OperationInfo {
                    name: "question.create",
                    stage: "frame",
                    summary: "Add a question with an answer that matters to the case.",
                    when_to_use: "Record a question that should guide evidence collection.",
                    lifecycle: "Existing case.",
                    effects: "Create one question record.",
                    returns: "Question ID and revision 1.",
                    revision_rule: "If supplied, the parent identifies an exact question revision.",
                    idempotency: COMMAND_IDEMPOTENCY,
                    repair: "Provide answerable wording and either omit the parent or name an exact question revision in this case.",
                    related: &["question.resolve"],
                },
                json!({"wording": "Which phase accounts for variable request latency?",
                    "importance": "normal"}),
                validate_question_create,
                |payload: QuestionCreate, context| {
                    let parent = payload.parent.as_ref()
                        .map(|reference| context.resolve(reference, Kind::Question, "/parent"))
                        .transpose()?;
                    let body = payload.into_body(parent).map_err(|blocker| {
                        Failure::new(ErrorCode::UnknownReference, blocker.code()).at("/parent")
                    })?;
                    context.create(Kind::Question, record_body(body)?)?;
                    Ok(())
                },
            ),
        ]
        .into_iter()
        .chain(crate::case_ops::operations())
        .chain(crate::evaluate_ops::operations())
        .chain(crate::explain_ops::operations())
        .chain(crate::review_ops::operations())
        .chain(crate::decision_ops::operations())
        .chain(crate::retrospective_ops::operations())
        .chain(crate::observation_ops::operations())
        .chain(RESERVED_OPERATIONS.iter().copied().map(OperationDescriptor::reserved)),
    )
    .expect("standard operation names are unique")
}

const RESERVED_OPERATIONS: &[&str] = &[
    "artifact.withdraw",
    "coverage.define",
    "coverage.link",
    "coverage.revise",
    "measurement.assess",
    "source.assess",
    "source.withdraw",
];

fn record_body(payload: impl Serialize) -> Result<Value, Failure> {
    serde_json::to_value(payload).map_err(|error| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            format!("Could not encode record: {error}"),
        )
    })
}

fn validate_case_create(payload: &CaseCreate) -> Result<(), Failure> {
    validate_text(&payload.title, 256, "/title")
}

fn validate_question_create(payload: &QuestionCreate) -> Result<(), Failure> {
    validate_text(&payload.wording, 2048, "/wording")
}

fn validate_text(value: &str, max: usize, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() || value.chars().count() > max {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            format!("Supply nonempty text no longer than {max} characters."),
        )
        .at(path));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_match_the_same_decoder_and_the_published_schema() {
        let catalog = standard_catalog();
        for descriptor in catalog.operations() {
            descriptor.validate_example().unwrap();
            if let Some(schema) = descriptor.payload_schema() {
                let schema = serde_json::to_value(schema).unwrap();
                let validator = jsonschema::validator_for(&schema).unwrap();
                validator
                    .validate(descriptor.example().unwrap())
                    .unwrap_or_else(|error| {
                        panic!(
                            "{} example does not match its schema: {error}",
                            descriptor.info().name
                        )
                    });
            }
        }
        assert!(
            catalog
                .get("case.pause")
                .unwrap()
                .payload_schema()
                .is_some()
        );
        assert_eq!(
            catalog.get("coverage.define").unwrap().status(),
            Status::Deferred
        );
        assert!(
            catalog
                .available()
                .all(|descriptor| descriptor.info().name != "coverage.define")
        );
        assert!(
            catalog
                .available_operation("coverage.define")
                .err()
                .unwrap()
                .error
                .message
                .contains("deferred")
        );
    }

    #[test]
    fn malformed_examples_and_duplicate_names_are_detected() {
        let info = *standard_catalog().get("case.pause").unwrap().info();
        let malformed = OperationDescriptor::deferred(
            info,
            json!({"reason": 3}),
            |_: &crate::case::CasePause| Ok(()),
        );
        assert!(malformed.validate_example().is_err());

        let too_short = OperationDescriptor::deferred(
            info,
            json!({"reason": ""}),
            |_: &crate::case::CasePause| Ok(()),
        );
        assert!(too_short.validate_example().is_ok());
        let schema = serde_json::to_value(too_short.payload_schema().unwrap()).unwrap();
        assert!(!jsonschema::is_valid(&schema, too_short.example().unwrap()));

        let duplicate = Catalog::new([
            OperationDescriptor::reserved("case.pause"),
            OperationDescriptor::reserved("case.pause"),
        ]);
        assert_eq!(
            duplicate.err().unwrap().error.code,
            ErrorCode::IntegrityFailure
        );
    }
}
