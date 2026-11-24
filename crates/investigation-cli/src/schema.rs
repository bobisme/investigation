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
    "producer-run",
    "producer-run-receipt",
    "audit",
    "method-pack",
    "export-manifest",
    "dry-run",
    "producer-bundle",
];
const RESERVED_WIRE_SCHEMAS: &[&str] = &[];

fn wire_names() -> Vec<&'static str> {
    WIRE_SCHEMAS
        .iter()
        .chain(crate::capture_schema::NAMES)
        .copied()
        .collect()
}

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
    if let Some(schema) = crate::capture_schema::schema(name) {
        return Ok(if args.schema_only {
            raw(schema)
        } else {
            crate::capture_schema::describe(name)
        });
    }
    if let Some(schema) = wire_value(name) {
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
                "producer-run" => {
                    "Record one completed producer run with investigation ingest --input ENVELOPE."
                }
                "producer-run-receipt" => {
                    "Inspect the Run, retained items and outcomes from a producer-run ingest."
                }
                "audit" => {
                    "Read the response of investigation blindspots, a read-only structural audit."
                }
                "export-manifest" => {
                    "Read export-manifest.json, written last into every directory investigation export creates."
                }
                "producer-bundle" => {
                    "Read producer-bundle.json from investigation export --kind producer-bundle, or re-import it with investigation ingest --input."
                }
                "dry-run" => {
                    "Read the would-be receipt that a write given --dry-run returns without committing."
                }
                "method-pack" => {
                    "Author an inv.method-pack.v1 TOML file (tables decode to this shape); retain it with ingest and pin it with method_pack.adopt."
                }
                _ => "Inspect the shape of an error and its repair guidance.",
            };
            let mut text = format!("{name}  {purpose}\n{}", pretty(&schema));
            let mut value = json!({"name": name, "purpose": purpose, "schema": schema});
            if name == "producer-run" {
                let (example, behavior) = producer_run_example();
                text.push_str(&format!(
                    "\nSchematic example (paths resolve beneath configured import roots)\n{}\nBehavior: {}",
                    pretty(&example),
                    behavior.join(" ")
                ));
                value["example"] = example;
                value["behavior"] = json!(behavior);
            }
            Output::success("schema", value, text)
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

/// A wire schema served by name, other than the capture schemas.
fn wire_value(name: &str) -> Option<Value> {
    let wire = match name {
        "ingest" => Some(investigation_store::store::ingest::manifest_schema()),
        "ingest-receipt" => Some(investigation_store::store::ingest::receipt_schema()),
        "producer-run" => Some(investigation_store::store::ingest::producer_run::envelope_schema()),
        "producer-run-receipt" => {
            Some(investigation_store::store::ingest::producer_run::receipt_schema())
        }
        "audit" => Some(investigation_core::blindspot::audit_schema()),
        "method-pack" => Some(investigation_core::method_pack::schema()),
        "export-manifest" => Some(investigation_render::export::manifest_schema()),
        "dry-run" => Some(crate::dry_run::schema()),
        "producer-bundle" => Some(investigation_render::export::producer_bundle::bundle_schema()),
        _ => wire_schema(name),
    }?;
    Some(serde_json::to_value(wire).expect("wire schema serializes"))
}

/// The published v1 contracts as `scripts/check-v1-schemas.py` lays them out
/// under `schemas/v1`: file name and JSON bytes, `catalog.json` included. A
/// workspace archive carries them so its journal can be read without this
/// program.
pub(crate) fn published_documents() -> Vec<(String, Vec<u8>)> {
    let catalog = standard_catalog();
    let mut documents = BTreeMap::<String, Value>::new();
    let mut wire = serde_json::Map::new();
    for name in wire_names() {
        let file = format!(
            "{}.schema.json",
            if name == "record" { "command" } else { name }
        );
        let mut schema = crate::capture_schema::schema(name)
            .or_else(|| wire_value(name))
            .expect("every wire name has a schema");
        schema["$id"] = json!(format!("urn:investigation:schema:v1:wire:{name}"));
        wire.insert(name.to_owned(), json!(file));
        documents.insert(file, schema);
    }
    let mut groups = BTreeMap::<&str, Vec<&str>>::new();
    for operation in catalog.available() {
        let info = operation.info();
        groups.entry(info.stage).or_default().push(info.name);
    }
    let mut operations = serde_json::Map::new();
    for operation in catalog.available() {
        let name = operation.info().name;
        let Some(payload) = operation.payload_schema() else {
            continue;
        };
        let file = format!("operations/{name}.schema.json");
        let mut schema = serde_json::to_value(payload).expect("payload schema serializes");
        schema["$id"] = json!(format!("urn:investigation:schema:v1:operation:{name}"));
        operations.insert(name.to_owned(), json!(file));
        documents.insert(file, schema);
    }
    let reserved: Vec<_> = catalog
        .operations()
        .filter(|descriptor| descriptor.status() == Status::Deferred)
        .map(|descriptor| descriptor.info().name)
        .collect();
    let stages: Vec<_> = groups
        .into_iter()
        .map(|(stage, operations)| json!({"stage": stage, "operations": operations}))
        .collect();
    documents.insert(
        "catalog.json".to_owned(),
        json!({
            "schema_version": "inv.catalog.v1",
            "wire_schemas": wire,
            "operations": operations,
            "stages": stages,
            "reserved_operations": reserved,
            "reserved_wire_schemas": RESERVED_WIRE_SCHEMAS,
        }),
    );
    documents
        .into_iter()
        .map(|(file, value)| {
            let mut bytes = serde_json::to_vec_pretty(&value).expect("schema serializes");
            bytes.push(b'\n');
            (file, bytes)
        })
        .collect()
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
    let wire_schemas = wire_names();
    text.push(format!("wire  {}", wire_schemas.join(", ")));
    text.push("Inspect: investigation schema OPERATION --format json".into());
    let stages: Vec<_> = groups
        .into_iter()
        .map(|(stage, operations)| json!({"stage": stage, "operations": operations}))
        .collect();
    let mut value = json!({"stages": stages, "wire_schemas": wire_schemas});
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

/// The plan's schematic envelope. It is relative to an existing fixture
/// directory: put the directory in `import_roots` and `ingest` resolves the path.
fn producer_run_example() -> (Value, [&'static str; 6]) {
    (
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
        }),
        [
            "Records one Run plus a Source and Artifact for the envelope bytes and for each retained artifact, in one transaction.",
            "Without --case it creates a minimal record-mode case. The actor comes from --actor or settings; the command ID is derived from the content.",
            "producer.name and external_run_id identify the attempt: identical content records nothing, changed content revises the same Run and Sources.",
            "Assertions, controls and receipts stay attributed producer claims on the Run; no Observation, Assessment or Conclusion is created.",
            "Execution state and exit code set no measurement validity; the Run's evidence_validity stays unknown.",
            "A missing artifact or an expected_sha256 mismatch affects that item only and makes the Run's evidence completeness partial.",
        ],
    )
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
        .chain(wire_names())
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
