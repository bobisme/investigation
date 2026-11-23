//! Catalog-generated single-operation flag shortcuts.

use std::{collections::BTreeMap, io::Write, path::PathBuf};

use clap::{Arg, ArgAction, ArgMatches, Command, FromArgMatches, builder::ValueParser};
use investigation_core::{
    catalog::{ShortcutDescriptor, ShortcutTarget, shortcuts as shortcut_specs, standard_catalog},
    command::{Command as RecordCommand, Operation},
    header::Actor,
    identity::{Id, IdSource, Kind, ResolveError, SystemIdSource, resolve_prefix},
    wire::{ErrorCode, Failure},
};
use investigation_store::{Store, store::ingest};
use schemars::schema_for;
use serde_json::{Value, json};

use crate::{
    Global, Output, evidence, receipt_output,
    selection::{select_actor, select_case},
    with_workspace_repair,
};

pub(crate) struct ParsedShortcut {
    pub(crate) global: Global,
    pub(crate) path: Vec<String>,
    pub(crate) values: BTreeMap<String, String>,
    pub(crate) payload_overrides: BTreeMap<String, Value>,
    pub(crate) command_id: Option<String>,
    pub(crate) print_envelope: bool,
}

/// Add catalog-declared shortcuts to the regular CLI command tree.
pub(crate) fn augment(command: Command) -> Command {
    let catalog = standard_catalog();
    let mut root = command;
    for spec in shortcut_specs().iter().filter(|spec| spec.path.len() == 1) {
        root = root.subcommand(shortcut_leaf(spec, &catalog));
    }
    let parents: std::collections::BTreeSet<_> = shortcut_specs()
        .iter()
        .filter(|spec| spec.path.len() > 1)
        .map(|spec| spec.path[0])
        .collect();
    for parent_name in parents {
        let description = match parent_name {
            "case" => "Create and inspect case-level items.",
            "source" => "Add source material through verified ingest.",
            _ => "Shortcut commands.",
        };
        let mut parent = Command::new(parent_name).about(description);
        for spec in shortcut_specs()
            .iter()
            .filter(|spec| spec.path.len() > 1 && spec.path[0] == parent_name)
        {
            parent = parent.subcommand(shortcut_leaf(spec, &catalog));
        }
        root = root.subcommand(parent);
    }
    root
}

fn shortcut_leaf(
    spec: &ShortcutDescriptor,
    catalog: &investigation_core::catalog::Catalog,
) -> Command {
    let schema = shortcut_schema(spec, catalog);
    let example = match spec.target {
        ShortcutTarget::Operation(name) => catalog
            .get(name)
            .and_then(|descriptor| descriptor.example())
            .cloned(),
        ShortcutTarget::Ingest => None,
    };
    let leaf_name = spec.path.last().expect("shortcut path is nonempty");
    let mut leaf = Command::new(*leaf_name)
        .about(spec.summary)
        .arg(
            Arg::new("command_id")
                .long("command-id")
                .value_name("KEY")
                .help("Stable idempotency key; generated if omitted."),
        )
        .arg(
            Arg::new("print_envelope")
                .long("print-envelope")
                .action(ArgAction::SetTrue)
                .help("Print the exact one-operation request without submitting it."),
        );
    for argument in spec.arguments {
        assert!(
            argument
                .payload_paths
                .iter()
                .all(|path| has_schema_path(&schema, path)),
            "shortcut {} argument {} maps to absent payload field",
            spec.path.join(" "),
            argument.id
        );
        let mut arg = Arg::new(argument.id)
            .value_name(argument.value_name)
            .help(argument.help)
            .required(argument.required);
        if argument.positional {
            arg = arg.index(positional_index(spec, argument.id));
        } else if let Some(long) = argument.long {
            arg = arg.long(long);
        }
        if argument.id == "mode"
            && let ShortcutTarget::Operation(name) = spec.target
        {
            let schema =
                serde_json::to_value(catalog.get(name).unwrap().payload_schema().unwrap()).unwrap();
            if let Some(values) = enum_values_for_path(&schema, "mode") {
                let help = format!("{} Choices: {}.", argument.help, values.join(", "));
                arg = arg.help(help);
                arg = arg.value_parser(ValueParser::new(move |input: &str| {
                    if values.iter().any(|candidate| candidate == input) {
                        Ok(input.to_owned())
                    } else {
                        Err(format!("expected one of: {}", values.join(", ")))
                    }
                }));
            }
        }
        leaf = leaf.arg(arg);
    }
    for field in generated_fields(spec, &schema, example.as_ref()) {
        let field_id: &'static str = Box::leak(generated_id(&field.path).into_boxed_str());
        let long: &'static str = Box::leak(field.flag.clone().into_boxed_str());
        let enum_choices = enum_values(&schema, dereference(&schema, &field.schema));
        let description = if let Some(choices) = &enum_choices {
            format!(
                "Catalog payload field {}. Choices: {}.",
                field.path,
                choices.join(", ")
            )
        } else if is_string_schema(&schema, &field.schema) {
            format!("Catalog payload field {}.", field.path)
        } else {
            format!(
                "Catalog payload field {} (structured values use JSON).",
                field.path
            )
        };
        let mut arg = Arg::new(field_id)
            .long(long)
            .value_name("VALUE")
            .help(description)
            .required(field.required);
        let property = field.schema.clone();
        let root_schema = schema.clone();
        arg = arg.value_parser(ValueParser::new(move |input: &str| {
            parse_cli_value(input, &property, &root_schema)
        }));
        leaf = leaf.arg(arg);
    }
    leaf
}

#[derive(Clone)]
struct GeneratedField {
    path: String,
    flag: String,
    schema: Value,
    required: bool,
}

fn shortcut_schema(
    spec: &ShortcutDescriptor,
    catalog: &investigation_core::catalog::Catalog,
) -> Value {
    match spec.target {
        ShortcutTarget::Operation(name) => serde_json::to_value(
            catalog
                .get(name)
                .expect("shortcut target exists")
                .payload_schema()
                .expect("payload schema"),
        )
        .expect("operation schema serializes"),
        ShortcutTarget::Ingest => {
            serde_json::to_value(schema_for!(investigation_store::store::ingest::Manifest))
                .expect("manifest schema serializes")
        }
    }
}

fn generated_fields(
    spec: &ShortcutDescriptor,
    schema: &Value,
    example: Option<&Value>,
) -> Vec<GeneratedField> {
    let mapped_paths: std::collections::BTreeSet<_> = spec
        .arguments
        .iter()
        .flat_map(|arg| arg.payload_paths.iter().copied())
        .collect();
    let mapped_roots: std::collections::BTreeSet<_> = mapped_paths
        .iter()
        .filter_map(|path| path.split('.').next())
        .collect();
    let mut fields = Vec::new();
    if spec.target == ShortcutTarget::Ingest {
        fields.extend(fields_at(
            schema,
            "",
            &mapped_roots,
            &[
                "schema_version",
                "command_id",
                "actor",
                "actor_descriptor",
                "case_id",
                "new_case",
                "paths",
                "content",
            ],
            example,
        ));
        let mapped_source: std::collections::BTreeSet<_> = mapped_paths
            .iter()
            .filter_map(|path| path.strip_prefix("source."))
            .collect();
        fields.extend(fields_at(schema, "source", &mapped_source, &[], None));
    } else {
        fields.extend(fields_at(schema, "", &mapped_roots, &["role"], example));
    }
    fields
}

fn fields_at(
    schema: &Value,
    prefix: &str,
    mapped: &std::collections::BTreeSet<&str>,
    excluded: &[&str],
    example: Option<&Value>,
) -> Vec<GeneratedField> {
    let Some(properties) = schema_properties(schema, prefix) else {
        return Vec::new();
    };
    let required: std::collections::BTreeSet<_> = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    properties
        .iter()
        .filter_map(|(name, property)| {
            if excluded.contains(&name.as_str()) || mapped.contains(name.as_str()) {
                return None;
            }
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            let example_has_value = example
                .and_then(|value| path.split('.').try_fold(value, |value, key| value.get(key)))
                .is_some();
            Some(GeneratedField {
                flag: name.replace('_', "-"),
                path,
                schema: property.clone(),
                required: required.contains(name.as_str()) && !example_has_value,
            })
        })
        .collect()
}

fn schema_properties<'a>(
    schema: &'a Value,
    path: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    let current = if path.is_empty() {
        schema
    } else {
        let mut current = schema;
        for segment in path.split('.') {
            current = property_schema(schema, current, segment)?;
        }
        current
    };
    dereference(schema, current)
        .get("properties")
        .and_then(Value::as_object)
}

fn dereference<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && let Some(name) = reference.strip_prefix("#/$defs/")
        && let Some(target) = root.get("$defs").and_then(|defs| defs.get(name))
    {
        return dereference(root, target);
    }
    schema
}

fn property_schema<'a>(root: &'a Value, schema: &'a Value, name: &str) -> Option<&'a Value> {
    if let Some(child) = schema.get("properties").and_then(|props| props.get(name)) {
        return Some(child);
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && let Some(definition) = reference.strip_prefix("#/$defs/")
        && let Some(target) = root.get("$defs").and_then(|defs| defs.get(definition))
    {
        return property_schema(root, target, name);
    }
    for key in ["oneOf", "anyOf", "allOf"] {
        if let Some(variants) = schema.get(key).and_then(Value::as_array)
            && let Some(child) = variants
                .iter()
                .find_map(|variant| property_schema(root, variant, name))
        {
            return Some(child);
        }
    }
    None
}

fn generated_id(path: &str) -> String {
    format!("generated__{}", path.replace('.', "__"))
}

fn parse_cli_value(input: &str, schema: &Value, root: &Value) -> Result<Value, String> {
    let schema = dereference(root, schema);
    let enum_values = enum_values(root, schema);
    if let Some(values) = enum_values
        && values.iter().any(|value| value == input)
    {
        return Ok(Value::String(input.to_owned()));
    }
    let kind = schema.get("type").and_then(Value::as_str).or_else(|| {
        schema
            .get("type")
            .and_then(Value::as_array)
            .and_then(|types| {
                types
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|kind| *kind != "null")
            })
    });
    if kind == Some("string") {
        return Ok(Value::String(input.to_owned()));
    }
    serde_json::from_str(input)
        .map_err(|error| format!("value must be valid JSON for this catalog field: {error}"))
}

fn enum_values(root: &Value, schema: &Value) -> Option<Vec<String>> {
    if let Some(values) = schema.get("enum").and_then(Value::as_array).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    }) {
        return Some(values);
    }
    for key in ["oneOf", "anyOf", "allOf"] {
        if let Some(items) = schema.get(key).and_then(Value::as_array)
            && let Some(values) = items
                .iter()
                .find_map(|item| enum_values(root, dereference(root, item)))
        {
            return Some(values);
        }
    }
    None
}

fn is_string_schema(root: &Value, schema: &Value) -> bool {
    let schema = dereference(root, schema);
    schema.get("type").and_then(Value::as_str) == Some("string")
        || schema
            .get("type")
            .and_then(Value::as_array)
            .is_some_and(|types| {
                types.iter().any(|kind| kind.as_str() == Some("string"))
                    && types
                        .iter()
                        .all(|kind| matches!(kind.as_str(), Some("string" | "null")))
            })
}

fn positional_index(spec: &ShortcutDescriptor, arg_id: &str) -> usize {
    spec.arguments
        .iter()
        .take_while(|argument| argument.id != arg_id)
        .filter(|argument| argument.positional)
        .count()
        + 1
}

fn has_schema_path(schema: &Value, path: &str) -> bool {
    path.split('.')
        .all(|part| has_schema_property(schema, part))
}

fn has_schema_property(schema: &Value, property: &str) -> bool {
    if let Some(properties) = schema.get("properties").and_then(Value::as_object)
        && properties.contains_key(property)
    {
        return true;
    }
    match schema {
        Value::Object(map) => map
            .values()
            .any(|value| has_schema_property(value, property)),
        Value::Array(items) => items
            .iter()
            .any(|value| has_schema_property(value, property)),
        _ => false,
    }
}

fn enum_values_for_path(schema: &Value, name: &str) -> Option<Vec<String>> {
    fn find(root: &Value, value: &Value, name: &str) -> Option<Vec<String>> {
        if let Some(property) = value.get("properties").and_then(|v| v.get(name)) {
            return enum_values(root, dereference(root, property));
        }
        match value {
            Value::Object(map) => map.values().find_map(|child| find(root, child, name)),
            Value::Array(items) => items.iter().find_map(|child| find(root, child, name)),
            _ => None,
        }
    }
    find(schema, schema, name)
}

pub(crate) fn from_matches(matches: &mut ArgMatches) -> Option<ParsedShortcut> {
    let (first, first_matches) = matches.subcommand()?;
    let (path, values_matches) = if let Some((second, second_matches)) = first_matches.subcommand()
    {
        (vec![first.to_owned(), second.to_owned()], second_matches)
    } else {
        (vec![first.to_owned()], first_matches)
    };
    let spec = shortcut_specs().iter().find(|spec| {
        spec.path
            == path
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
    })?;
    let catalog = standard_catalog();
    let schema = shortcut_schema(spec, &catalog);
    let example = match spec.target {
        ShortcutTarget::Operation(name) => catalog
            .get(name)
            .and_then(|descriptor| descriptor.example())
            .cloned(),
        ShortcutTarget::Ingest => None,
    };
    let mut values = BTreeMap::new();
    for argument in spec.arguments {
        if let Some(value) = values_matches.get_one::<String>(argument.id) {
            values.insert(argument.id.to_owned(), value.clone());
        }
    }
    let mut payload_overrides = BTreeMap::new();
    for field in generated_fields(spec, &schema, example.as_ref()) {
        if let Some(value) = values_matches.get_one::<Value>(&generated_id(&field.path)) {
            payload_overrides.insert(field.path, value.clone());
        }
    }
    let command_id = values_matches.get_one::<String>("command_id").cloned();
    let print_envelope = values_matches.get_flag("print_envelope");
    let global = crate::Global::from_arg_matches_mut(matches).ok()?;
    Some(ParsedShortcut {
        global,
        path,
        values,
        payload_overrides,
        command_id,
        print_envelope,
    })
}

pub(crate) fn run(store: &mut Store, shortcut: ParsedShortcut) -> Result<Output, Failure> {
    let spec = shortcut_specs()
        .iter()
        .find(|spec| {
            spec.path
                == shortcut
                    .path
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice()
        })
        .expect("parsed shortcut has catalog descriptor");
    let generated = shortcut.command_id.is_none();
    let command_id = shortcut
        .command_id
        .clone()
        .unwrap_or_else(generated_command_id);
    let actor = select_actor(store.workspace(), shortcut.global.actor.as_ref(), None)?;
    match spec.target {
        ShortcutTarget::Operation(operation) => {
            run_operation(store, operation, &shortcut, &actor, &command_id, generated)
        }
        ShortcutTarget::Ingest => {
            run_ingest(store, spec, &shortcut, &actor, &command_id, generated)
        }
    }
}

fn generated_command_id() -> String {
    SystemIdSource
        .next_id(Kind::Transaction)
        .expect("transaction id generation")
        .to_string()
}

fn announce_generated_id(command_id: &str) -> Result<(), Failure> {
    writeln!(std::io::stderr().lock(), "command_id: {command_id}").map_err(|error| {
        Failure::new(
            ErrorCode::IoFailure,
            format!("Could not announce command ID before write: {error}"),
        )
    })
}

fn run_operation(
    store: &mut Store,
    operation: &str,
    shortcut: &ParsedShortcut,
    actor: &Actor,
    command_id: &str,
    generated: bool,
) -> Result<Output, Failure> {
    let case_id = if operation == "case.create" {
        if shortcut.global.case.is_some() {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                "case new creates a standalone case; omit --case.",
            )
            .at("--case"));
        }
        None
    } else {
        Some(select_case(store, shortcut.global.case.as_deref())?)
    };
    let payload = build_operation_payload(store, operation, shortcut)?;
    let catalog = standard_catalog();
    let descriptor = catalog.get(operation).expect("target operation exists");
    descriptor.validate_payload(&payload)?;
    let request = RecordCommand {
        schema_version: investigation_core::command::CommandVersion::V1,
        command_id: command_id.to_owned(),
        case_id,
        actor: actor.clone(),
        actor_descriptor: None,
        preconditions: vec![],
        operations: vec![Operation {
            op: operation.to_owned(),
            client_ref: None,
            payload: payload.clone(),
        }],
    };
    let envelope = serde_json::to_value(&request).expect("shortcut envelope serializes");
    let bytes = serde_json::to_vec(&envelope).expect("shortcut envelope serializes");
    let _ = RecordCommand::parse(&bytes)?;
    if shortcut.print_envelope {
        return Ok(Output::new(
            envelope.clone(),
            serde_json::to_string_pretty(&envelope).expect("envelope serializes"),
        ));
    }
    if generated {
        announce_generated_id(command_id)?;
    }
    let receipt = store
        .record(&bytes)
        .map_err(|failure| with_workspace_repair(store.workspace(), failure))?;
    Ok(receipt_output(store.workspace(), receipt))
}

fn build_operation_payload(
    store: &mut Store,
    operation: &str,
    shortcut: &ParsedShortcut,
) -> Result<Value, Failure> {
    let catalog = standard_catalog();
    let mut payload = catalog
        .get(operation)
        .and_then(|descriptor| descriptor.example())
        .cloned()
        .unwrap_or_else(|| json!({}));
    match operation {
        "case.create" => {
            payload["title"] = json!(value(shortcut, "title"));
            payload["mode"] = json!(
                shortcut
                    .values
                    .get("mode")
                    .map(String::as_str)
                    .unwrap_or("explore")
            );
        }
        "observation.add" => {
            let case = select_case(store, shortcut.global.case.as_deref())?;
            let replay = store.case(case)?;
            let source_id = resolve_text_artifact(value(shortcut, "source"), &replay.state)?;
            let source = replay
                .state
                .current(source_id)
                .expect("resolved current artifact");
            let artifact: investigation_core::evidence::ArtifactBody =
                serde_json::from_value(source.body.clone()).map_err(|error| {
                    Failure::new(
                        ErrorCode::IntegrityFailure,
                        format!("Stored Artifact body is invalid: {error}"),
                    )
                })?;
            let digest = artifact.digest.ok_or_else(|| Failure::new(ErrorCode::GateFailed, "The selected decoded text Artifact has no digest; choose a retained derivative.").at("--source"))?;
            let quote = Some(
                shortcut
                    .values
                    .get("quote")
                    .cloned()
                    .unwrap_or_else(|| value(shortcut, "text").to_owned()),
            );
            let selector = json!({"type":"whole"});
            payload = json!({
                "value":{"kind":"text","statement":value(shortcut,"text")},
                "basis":{"kind":"source","artifact_ref":{"id":source_id,"revision":source.header.revision.0},
                    "selector":{"schema_version":"inv.selector.v1","artifact_digest":digest,"selector":selector},
                    "text_quote":quote.map(|exact| json!({"exact":exact,"prefix":null,"suffix":null}))},
                "extraction_method":"direct_read","validity":"unknown","quality":[],"limitations":[],
                "evidence_kind":"observational","variables_changed":[],"observed_at":null,"run_ref":null
            });
        }
        "annotation.add" => {
            let case = select_case(store, shortcut.global.case.as_deref())?;
            let replay = store.case(case)?;
            let subject_id =
                resolve_kind_prefix(value(shortcut, "on"), &replay.state, Kind::Case, "/on")
                    .or_else(|_| resolve_any_subject(value(shortcut, "on"), &replay.state))?;
            let subject = replay
                .state
                .current(subject_id)
                .expect("resolved current subject");
            payload["subject_refs"] =
                json!([{"id":subject_id,"revision":subject.header.revision.0}]);
            payload["content"] = json!({"kind":"text","text":value(shortcut,"text")});
            payload["role"] = json!("note");
        }
        _ => {
            return Err(Failure::new(
                ErrorCode::IntegrityFailure,
                format!("Unsupported shortcut target {operation:?}."),
            ));
        }
    }
    for (path, value) in &shortcut.payload_overrides {
        set_json_path(&mut payload, path, value.clone());
    }
    Ok(payload)
}

fn run_ingest(
    store: &mut Store,
    _spec: &ShortcutDescriptor,
    shortcut: &ParsedShortcut,
    actor: &Actor,
    command_id: &str,
    generated: bool,
) -> Result<Output, Failure> {
    let path = PathBuf::from(value(shortcut, "path"));
    if path.as_os_str() == std::ffi::OsStr::new("-") {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "source add PATH accepts a local file; use ingest --input for streamed source content.",
        )
        .at("PATH"));
    }
    let case = select_case(store, shortcut.global.case.as_deref())?;
    let title = shortcut
        .values
        .get("title")
        .cloned()
        .or_else(|| path.file_name().and_then(|s| s.to_str()).map(str::to_owned))
        .unwrap_or_else(|| "Attached source".into());
    let mut source = json!({"title":title,"producer":null,"publisher":null,"identity":null,"format":null,"context":null,"verification_limits":[]});
    for key in [
        "producer",
        "publisher",
        "identity",
        "source_format",
        "context",
    ] {
        if let Some(value) = shortcut.values.get(key) {
            let manifest_key = if key == "source_format" {
                "format"
            } else {
                key
            };
            source[manifest_key] = json!(value);
        }
    }
    let mut manifest = json!({"schema_version":ingest::IngestVersion::V1,"command_id":command_id,"actor":actor,"case_id":case,"source":source,"paths":[path],"extract_text":true});
    for (path, value) in &shortcut.payload_overrides {
        set_json_path(&mut manifest, path, value.clone());
    }
    let bytes = serde_json::to_vec(&manifest).expect("shortcut ingest manifest serializes");
    ingest::parse_manifest(&bytes)?;
    if shortcut.print_envelope {
        return Ok(Output::new(
            manifest.clone(),
            serde_json::to_string_pretty(&manifest).expect("manifest serializes"),
        ));
    }
    if generated {
        announce_generated_id(command_id)?;
    }
    // Delegate byte retention, source/artifact creation, hashing and proof to the canonical ingest implementation.
    let output = evidence::submit_shortcut_manifest(store, &bytes)?;
    Ok(output)
}

fn value<'a>(shortcut: &'a ParsedShortcut, key: &str) -> &'a str {
    shortcut
        .values
        .get(key)
        .map(String::as_str)
        .unwrap_or_default()
}

fn set_json_path(value: &mut Value, path: &str, replacement: Value) {
    let mut parts = path.split('.').peekable();
    let mut current = value;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            current[part] = replacement;
            return;
        }
        if !current.get(part).is_some_and(Value::is_object) {
            current[part] = json!({});
        }
        current = &mut current[part];
    }
}

fn resolve_text_artifact(
    selector: &str,
    state: &investigation_core::state::State,
) -> Result<Id, Failure> {
    let requested = resolve_prefix(
        selector,
        state
            .records()
            .map(|record| record.header.id)
            .filter(|id| matches!(id.kind(), Kind::Source | Kind::Artifact)),
    )
    .map_err(|error| match error {
        ResolveError::InvalidPrefix => Failure::new(
            ErrorCode::InvalidInput,
            "Supply a Source ID, Artifact ID, or unique typed prefix.",
        )
        .at("--source"),
        ResolveError::NotFound => Failure::new(
            ErrorCode::UnknownReference,
            "No Source or Artifact matches in the selected case.",
        )
        .at("--source"),
        ResolveError::Ambiguous(ids) => {
            let mut failure = Failure::new(
                ErrorCode::AmbiguousReference,
                "Several Sources or Artifacts match. Use the exact decoded-text Artifact ID.",
            )
            .at("--source");
            failure.error.actual = Some(json!(ids));
            failure
        }
    })?;
    if requested.kind() == Kind::Source {
        let derivatives = state
            .records()
            .filter(|record| record.header.id.kind() == Kind::Artifact)
            .filter_map(|record| {
                artifact_body(record)
                    .ok()
                    .map(|body| (record.header.id, body))
            })
            .filter(|(_, body)| body.source.id == requested && is_decoded_text(body))
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        return unique_text_artifact(
            derivatives,
            "The Source has no decoded UTF-8 text Artifact. Use a Source with a retained text derivative or pass a specific decoded-text Artifact ID.",
        );
    }

    let artifact_record = state.current(requested).expect("resolved current Artifact");
    let body = artifact_body(artifact_record)?;
    if is_decoded_text(&body) {
        return Ok(requested);
    }
    let derivatives = state
        .records()
        .filter(|record| record.header.id.kind() == Kind::Artifact)
        .filter_map(|record| {
            artifact_body(record)
                .ok()
                .map(|body| (record.header.id, body))
        })
        .filter(|(_, candidate)| {
            candidate.source.id == body.source.id
                && candidate.transforms.iter().any(|transform| {
                    transform.input.id == requested
                        && transform.input.revision == artifact_record.header.revision.0
                        && transform.transformation == "decode_utf8"
                        && transform.version
                            == investigation_store::store::ingest::TEXT_PARSER_VERSION
                })
        })
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    unique_text_artifact(
        derivatives,
        "The Artifact has no decoded UTF-8 text derivative. Use the derivative Artifact ID returned by source add or ingest.",
    )
}

fn artifact_body(
    record: &investigation_core::state::Record,
) -> Result<investigation_core::evidence::ArtifactBody, Failure> {
    serde_json::from_value(record.body.clone()).map_err(|error| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            format!("Stored Artifact body is invalid: {error}"),
        )
    })
}

fn is_decoded_text(body: &investigation_core::evidence::ArtifactBody) -> bool {
    body.transforms.iter().any(|transform| {
        transform.transformation == "decode_utf8"
            && transform.version == investigation_store::store::ingest::TEXT_PARSER_VERSION
    })
}

fn unique_text_artifact(ids: Vec<Id>, missing_message: &str) -> Result<Id, Failure> {
    match ids.as_slice() {
        [id] => Ok(*id),
        [] => Err(Failure::new(ErrorCode::GateFailed, missing_message).at("--source")),
        _ => {
            let mut failure = Failure::new(
                ErrorCode::AmbiguousReference,
                "Several decoded UTF-8 text Artifacts match. Pass the exact derivative Artifact ID with --source.",
            )
            .at("--source");
            failure.error.actual = Some(json!(ids));
            Err(failure)
        }
    }
}

fn resolve_kind_prefix(
    selector: &str,
    state: &investigation_core::state::State,
    kind: Kind,
    path: &str,
) -> Result<Id, Failure> {
    resolve_prefix(
        selector,
        state
            .records()
            .map(|r| r.header.id)
            .filter(|id| id.kind() == kind),
    )
    .map_err(|error| match error {
        ResolveError::InvalidPrefix => Failure::new(
            ErrorCode::InvalidInput,
            "Supply a typed record ID or unique prefix.",
        )
        .at(path),
        ResolveError::NotFound => Failure::new(
            ErrorCode::UnknownReference,
            "No matching record exists in the selected case.",
        )
        .at(path),
        ResolveError::Ambiguous(ids) => {
            let mut failure = Failure::new(
                ErrorCode::AmbiguousReference,
                "Several records match. Use a longer ID prefix.",
            )
            .at(path);
            failure.error.actual = Some(json!(ids));
            failure
        }
    })
}

fn resolve_any_subject(
    selector: &str,
    state: &investigation_core::state::State,
) -> Result<Id, Failure> {
    resolve_prefix(selector, state.records().map(|r| r.header.id)).map_err(|error| match error {
        ResolveError::InvalidPrefix => Failure::new(
            ErrorCode::InvalidInput,
            "Supply a typed record ID or unique prefix.",
        )
        .at("/on"),
        ResolveError::NotFound => Failure::new(
            ErrorCode::UnknownReference,
            "No matching record exists in the selected case.",
        )
        .at("/on"),
        ResolveError::Ambiguous(ids) => {
            let mut failure = Failure::new(
                ErrorCode::AmbiguousReference,
                "Several records match. Use a longer ID prefix.",
            )
            .at("/on");
            failure.error.actual = Some(json!(ids));
            failure
        }
    })
}
