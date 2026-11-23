use std::{collections::BTreeMap, env};

use clap::ArgGroup;
use investigation_core::{
    PROGRAM_NAME,
    catalog::{Catalog, OperationDescriptor, Status, standard_catalog, wire_schema},
    command::CommandVersion,
    identity::{Id, Kind},
    wire::{ErrorCode, Failure},
};
use investigation_store::Store;
use serde_json::{Value, json};

use crate::{
    Global, Output,
    selection::{open_workspace, select_actor, select_case, workspace_argv},
};

const WIRE_SCHEMAS: &[&str] = &[
    "record",
    "receipt",
    "error",
    "ingest",
    "ingest-receipt",
    "evidence-selector",
];
const RESERVED_WIRE_SCHEMAS: &[&str] = &["capture"];

#[derive(clap::Args)]
#[command(group(ArgGroup::new("selection").required(true).args(["operation", "list"])))]
pub(crate) struct Args {
    /// Operation or wire schema name. Use --list to see the available names.
    operation: Option<String>,
    /// List available operations by workflow stage.
    #[arg(long)]
    list: bool,
    /// Include reserved operation and wire names along with the active catalog.
    #[arg(long, requires = "list")]
    include_deferred: bool,
    /// Return the JSON Schema object by itself.
    #[arg(long, requires = "operation")]
    schema_only: bool,
}

pub(crate) fn describe(args: Args, global: &Global) -> Result<Output, Failure> {
    let catalog = standard_catalog();
    if args.list {
        return Ok(list(&catalog, args.include_deferred));
    }
    let name = args
        .operation
        .as_deref()
        .expect("clap requires an operation or --list");
    let wire = match name {
        "ingest" => Some(investigation_store::store::ingest::manifest_schema()),
        "ingest-receipt" => Some(investigation_store::store::ingest::receipt_schema()),
        _ => wire_schema(name),
    };
    if let Some(schema) = wire {
        let schema = serde_json::to_value(schema).expect("wire schema serializes");
        return Ok(if args.schema_only {
            raw(schema)
        } else {
            let purpose = match name {
                "record" => "Submit a transaction with investigation record --input FILE.",
                "receipt" => "Inspect the result of an accepted transaction.",
                "ingest" => "Retain original source bytes with investigation ingest --input FILE.",
                "ingest-receipt" => "Inspect the receipt and retained items from ingest or start.",
                "evidence-selector" => {
                    "Choose a digest-bound selection for investigation evidence read."
                }
                _ => "Inspect the shape of an error and its repair guidance.",
            };
            let text = format!("{name}  {purpose}\n{}", pretty(&schema));
            Output::success(
                "schema",
                json!({"name": name, "purpose": purpose, "schema": schema}),
                text,
            )
        });
    }

    let descriptor = catalog.get(name).ok_or_else(|| unknown(name, &catalog))?;
    if args.schema_only {
        let schema = descriptor.payload_schema().ok_or_else(|| {
            Failure::new(
                ErrorCode::InvalidInput,
                "This operation does not have a published schema yet.",
            )
            .at("operation")
            .with_argv(list_argv())
        })?;
        return Ok(raw(
            serde_json::to_value(schema).expect("payload schema serializes")
        ));
    }

    let mut description = serde_json::to_value(descriptor).expect("descriptor serializes");
    let info = descriptor.info();
    let mut report = format!(
        "{name}  {}\nWhen: {}\nLifecycle: {}\nEffects: {}\nReturns: {}\nRevisions: {}\nRetry: {}\nRepair: {}",
        info.summary,
        info.when_to_use,
        info.lifecycle,
        info.effects,
        info.returns,
        info.revision_rule,
        info.idempotency,
        info.repair,
    );
    let available = catalog.available_operation(name).is_ok();
    description["available"] = json!(available);
    if available && let Some(example) = descriptor.example() {
        let transaction = transaction_form(descriptor, example, global)?;
        report.push_str(&format!(
            "\nTransaction example ({})\n{}\nGuidance: {}",
            transaction["status"].as_str().unwrap_or_default(),
            pretty(&transaction["input"]),
            transaction["guidance"].as_str().unwrap_or_default(),
        ));
        if let Some(fields) = transaction["unresolved_fields"]
            .as_array()
            .filter(|fields| !fields.is_empty())
        {
            report.push_str(&format!(
                "\nResolve: {}",
                fields
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        description["transaction_example"] = transaction;
    } else if !available {
        report.push_str("\nAvailability: deferred");
        if let Some(example) = descriptor.example() {
            report.push_str(&format!("\nPayload example\n{}", pretty(example)));
        }
    }
    if !info.related.is_empty() {
        report.push_str(&format!("\nRelated: {}", info.related.join(", ")));
    }
    if let Some(schema) = descriptor.payload_schema() {
        report.push_str(&format!(
            "\nPayload schema\n{}",
            serde_json::to_string_pretty(schema).expect("schema serializes")
        ));
    }
    Ok(Output::success("schema", description, report))
}

fn list(catalog: &Catalog, include_deferred: bool) -> Output {
    let mut groups = BTreeMap::<&str, Vec<&str>>::new();
    for operation in catalog.available() {
        let info = operation.info();
        groups.entry(info.stage).or_default().push(info.name);
    }
    let mut text: Vec<_> = groups
        .iter()
        .map(|(stage, names)| format!("{stage}  {}", names.join(", ")))
        .collect();
    text.push(format!("wire  {}", WIRE_SCHEMAS.join(", ")));
    text.push("Inspect: investigation schema OPERATION --format json".into());
    let stages: Vec<_> = groups
        .into_iter()
        .map(|(stage, operations)| json!({"stage": stage, "operations": operations}))
        .collect();
    let mut value = json!({"stages": stages, "wire_schemas": WIRE_SCHEMAS});
    if include_deferred {
        let reserved: Vec<_> = catalog
            .operations()
            .filter(|descriptor| descriptor.status() == Status::Deferred)
            .map(|descriptor| descriptor.info().name)
            .collect();
        text.push(format!("reserved operations  {}", reserved.join(", ")));
        text.push(format!(
            "reserved wire  {}",
            RESERVED_WIRE_SCHEMAS.join(", ")
        ));
        value["reserved_operations"] = json!(reserved);
        value["reserved_wire_schemas"] = json!(RESERVED_WIRE_SCHEMAS);
    }
    Output::success("schema_list", value, text.join("\n"))
}

fn raw(schema: Value) -> Output {
    let text = pretty(&schema);
    Output::new(schema, text)
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("JSON value serializes")
}

fn list_argv() -> Vec<String> {
    [PROGRAM_NAME, "schema", "--list", "--format", "json"]
        .map(str::to_owned)
        .to_vec()
}

fn unknown(name: &str, catalog: &Catalog) -> Failure {
    let mut suggestions: Vec<_> = catalog
        .available()
        .map(|operation| operation.info().name)
        .chain(WIRE_SCHEMAS.iter().copied())
        .map(|candidate| (edit_distance(name, candidate), candidate))
        .filter(|(distance, _)| *distance <= 3)
        .collect();
    suggestions.sort_unstable();
    let suggestions: Vec<_> = suggestions
        .into_iter()
        .take(3)
        .map(|(_, name)| name)
        .collect();
    let message = if suggestions.is_empty() {
        "Unknown operation. Run investigation schema --list.".into()
    } else {
        format!(
            "Unknown operation. Did you mean {}?",
            suggestions.join(", ")
        )
    };
    let mut failure = Failure::new(ErrorCode::InvalidInput, message)
        .at("operation")
        .with_argv(list_argv());
    failure.error.expected = Some(json!(suggestions));
    failure
}

fn edit_distance(input: &str, candidate: &str) -> usize {
    let candidate: Vec<_> = candidate.chars().collect();
    let mut row: Vec<_> = (0..=candidate.len()).collect();
    for (index, left) in input.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = index + 1;
        for (column, right) in candidate.iter().enumerate() {
            let above = row[column + 1];
            row[column + 1] = (above + 1)
                .min(row[column] + 1)
                .min(diagonal + usize::from(left != *right));
            diagonal = above;
        }
    }
    row[candidate.len()]
}

fn transaction_form(
    descriptor: &OperationDescriptor,
    example: &Value,
    global: &Global,
) -> Result<Value, Failure> {
    let name = descriptor.info().name;
    let workspace = match open_workspace(global.workspace.as_deref()) {
        Ok(workspace) => Some(workspace),
        Err(failure)
            if global.workspace.is_none() && failure.error.code == ErrorCode::WorkspaceRequired =>
        {
            None
        }
        Err(failure) => return Err(failure),
    };
    let mut unresolved_fields = Vec::new();
    let actor = workspace
        .as_ref()
        .and_then(|workspace| select_actor(workspace, global.actor.as_ref(), None).ok())
        .or_else(|| global.actor.clone())
        .or_else(|| env::var("AGENT").ok().and_then(|actor| actor.parse().ok()));
    if actor.is_none() {
        unresolved_fields.push("/actor".into());
    }

    let mut case = None;
    let mut case_revision = None;
    if name != "case.create" {
        if let Some(workspace) = &workspace {
            let mut store = Store::open(workspace.clone());
            match select_case(&mut store, global.case.as_deref()) {
                Ok(id) => {
                    case = Some(id);
                    let replay = store.case(id)?;
                    case_revision = replay
                        .state
                        .current(id)
                        .map(|record| record.header.revision.0);
                }
                Err(failure) if failure.error.code == ErrorCode::CaseRequired => {}
                Err(failure) => return Err(failure),
            }
        }
        if case.is_none() {
            unresolved_fields.push("/case_id".into());
        }
    }

    let mut payload = example.clone();
    substitute_references(
        &mut payload,
        "/operations/0/payload",
        case.zip(case_revision),
        &mut unresolved_fields,
    );
    if !matches!(name, "case.create" | "question.create") {
        unresolved_fields.push("/preconditions".into());
    }
    let mut argv = workspace
        .as_ref()
        .map(workspace_argv)
        .unwrap_or_else(|| vec![PROGRAM_NAME.into(), "--workspace".into(), "PATH".into()]);
    argv.extend(["record", "--input", "transaction.json", "--format", "json"].map(str::to_owned));
    Ok(json!({
        "status": if unresolved_fields.is_empty() && workspace.is_some() { "runnable_example" } else { "incomplete_form" },
        "workspace_required": workspace.is_none(),
        "unresolved_fields": unresolved_fields,
        "guidance": "Choose a unique command_id for your write. Reuse that ID only with identical input and attribution.",
        "input": {
            "schema_version": CommandVersion::V1,
            "command_id": format!("example-{}", name.replace('.', "-")),
            "case_id": case,
            "actor": actor,
            "preconditions": [],
            "operations": [{"op": name, "payload": payload}],
        },
        "argv": argv,
    }))
}

/// Only the selected case has an unambiguous correspondence with a catalog reference.
fn substitute_references(
    value: &mut Value,
    path: &str,
    case: Option<(Id, u64)>,
    unresolved: &mut Vec<String>,
) {
    match value {
        Value::Object(fields) => {
            if let Some(id) = fields
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| id.parse::<Id>().ok())
                && id.kind() == Kind::Case
                && let Some((case_id, revision)) = case
            {
                fields.insert("id".into(), json!(case_id));
                if fields.contains_key("revision") {
                    fields.insert("revision".into(), json!(revision));
                }
                return;
            }
            for (field, value) in fields {
                let field = field.replace('~', "~0").replace('/', "~1");
                substitute_references(value, &format!("{path}/{field}"), case, unresolved);
            }
        }
        Value::Array(items) => {
            for (index, value) in items.iter_mut().enumerate() {
                substitute_references(value, &format!("{path}/{index}"), case, unresolved);
            }
        }
        Value::String(id) => {
            if let Ok(reference) = id.parse::<Id>() {
                if let Some((case_id, _)) = case
                    && reference.kind() == Kind::Case
                {
                    *id = case_id.to_string();
                } else {
                    unresolved.push(path.into());
                }
            }
        }
        _ => {}
    }
}
