//! Namespaced extension properties declared by a pinned method pack (§2.4,
//! §4.3, §20.3).
//!
//! A pack may declare optional typed properties (`[[properties]]`) for some
//! record kinds. A transaction operation can then carry
//! `extensions: {"<pack id>": {"<property>": value}}` beside its payload. The
//! values are stored on the header of the record the operation creates (or
//! of the one record it revises), under `header.extensions.<pack id>`, so a
//! pack adds fields without a change to any Rust record type.
//!
//! Values are checked once, at write time, against the closed schema subset
//! of the pack revision the case pins, resolved from its retained bytes. A
//! namespace the case has not adopted is refused with a repair that names
//! `method_pack.adopt`; producer metadata that no pack declares stays in the
//! opaque `metadata` path instead. Replay never consults a pack: an accepted
//! revision is canonical data, so a record keeps its extensions when the pack
//! file is deleted, edited or its retained bytes become unavailable.
//!
//! Extensions are inert. No gate, freshness rule, gap rule, ranking,
//! blindspot check or basis digest reads them; the only consumer in the core
//! is a pack's own declared `required_fields` policy input (W09.07). Reads
//! show them as labeled, uninterpreted pack data.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::{
    identity::{Id, Kind},
    input::pointer,
    method_pack::{PropertyDefinition, ValueSchema, ValueType},
    method_pack_ops::{ADOPT, BUILTIN_PACK_ID, ResolvedPack, adopted_packs, resolve_pin},
    pack_policy::PackSource,
    state::{Record, State},
    wire::{ErrorCode, Failure},
};

/// Upper bound on the serialized extension map of one record.
pub const MAX_EXTENSION_BYTES: usize = 64 * 1024;
/// Upper bound on a `decimal` property value, in characters.
pub const MAX_DECIMAL_CHARS: usize = 64;
/// How a read labels extension values: pack data, never instructions.
pub const INTERPRETATION: &str = "pack_data_uninterpreted";

/// The record an operation's extensions attach to: the record it created,
/// or else the single record it revised.
pub(crate) fn target(creation: Option<Id>, images: &[Record]) -> Result<Id, Failure> {
    if let Some(id) = creation {
        return Ok(id);
    }
    let mut ids = images.iter().map(|record| record.header.id);
    let first = ids.next().ok_or_else(|| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Operation handler produced no record changes.",
        )
    })?;
    if ids.all(|id| id == first) {
        Ok(first)
    } else {
        Err(Failure::new(
            ErrorCode::InvalidInput,
            "This operation revises several records and creates none, so its extensions have no single record to attach to. Attach them to an operation that creates or revises exactly one record.",
        ))
    }
}

/// Validate `requested` for `record` against the case's pinned packs and
/// return the record's new extension map. Paths are relative to the
/// operation's `extensions` object. `state` must include earlier operations
/// of the same transaction, so a pack adopted earlier in it applies.
pub(crate) fn apply(
    state: &State,
    case_id: Id,
    packs: Option<&dyn PackSource>,
    record: &Record,
    requested: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, Value>, Failure> {
    let mut extensions = record.header.extensions.clone();
    if requested.is_empty() {
        return Ok(extensions);
    }
    let pins = adopted_packs(state, case_id, None)?;
    let kind = record.header.id.kind();
    for (namespace, value) in requested {
        let at = |rest: &[&str]| {
            pointer(std::iter::once(namespace.as_str()).chain(rest.iter().copied()))
        };
        if value.is_null() {
            if extensions.remove(namespace).is_none() {
                return Err(Failure::new(
                    ErrorCode::InvalidInput,
                    "The record carries no extension fields for this pack; null only removes existing ones.",
                )
                .at(at(&[])));
            }
            continue;
        }
        let resolved = resolve(namespace, &pins, packs).map_err(|failure| failure.at(at(&[])))?;
        let Some(loaded) = resolved.loaded.as_ref() else {
            let mut failure = Failure::new(
                ErrorCode::GateFailed,
                "The pinned pack's retained bytes are unavailable, so its extension schema cannot be checked. Restore the pack (retain it again and adopt it with replaces), or omit these extensions.",
            )
            .at(at(&[]))
            .with_schema(ADOPT);
            failure.error.actual = Some(json!({
                "pack_id": resolved.pack_id, "version": resolved.version,
                "digest": resolved.digest, "unavailable_reason": resolved.unavailable_reason,
            }));
            return Err(failure);
        };
        let Some(fields) = value.as_object().filter(|fields| !fields.is_empty()) else {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                "Supply an object of the pack's declared properties, or null to remove this pack's fields.",
            )
            .at(at(&[])));
        };
        let declared = &loaded.pack().properties;
        for (name, field) in fields {
            let property = declared
                .iter()
                .find(|property| &property.name == name)
                .ok_or_else(|| undeclared(&resolved, declared, kind).at(at(&[name])))?;
            if !property
                .record_kinds
                .iter()
                .any(|declared| declared.kind() == kind)
            {
                let mut failure = Failure::new(
                    ErrorCode::InvalidInput,
                    format!("The pack declares this property for other record kinds, not {kind}."),
                )
                .at(at(&[name]));
                failure.error.expected = Some(json!(property.record_kinds));
                return Err(failure);
            }
            let mut segments = vec![namespace.clone(), name.clone()];
            check_value(&property.schema, field, &mut segments)?;
        }
        extensions.insert(namespace.clone(), value.clone());
    }
    let size = serde_json::to_vec(&extensions).map_or(usize::MAX, |bytes| bytes.len());
    if size > MAX_EXTENSION_BYTES {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "The record's extension fields exceed 64 KiB once serialized.",
        ));
    }
    Ok(extensions)
}

/// The pin for `namespace`, resolved from retained bytes. The built-in pack
/// is always available and needs no pin.
fn resolve(
    namespace: &str,
    pins: &[crate::method_pack_ops::MethodPackPin],
    packs: Option<&dyn PackSource>,
) -> Result<ResolvedPack, Failure> {
    if namespace == BUILTIN_PACK_ID {
        return Ok(crate::method_pack_ops::resolve(&[], |_| None)
            .into_iter()
            .next()
            .expect("the built-in pack resolves"));
    }
    let pin = pins
        .iter()
        .find(|pin| pin.pack_id == namespace)
        .ok_or_else(|| {
            let mut failure = Failure::new(
                ErrorCode::UnknownReference,
                "Extension namespaces are method pack IDs this case has adopted. Retain the pack file with ingest and pin it with method_pack.adopt first; producer fields that no pack declares belong in the record's opaque metadata.",
            )
            .with_schema(ADOPT);
            failure.error.expected = Some(json!(
                pins.iter().map(|pin| pin.pack_id.as_str()).collect::<Vec<_>>()
            ));
            failure
        })?;
    let bytes = packs.and_then(|source| source.pack_bytes(&pin.digest));
    Ok(resolve_pin(pin, bytes.as_deref()))
}

fn undeclared(pack: &ResolvedPack, declared: &[PropertyDefinition], kind: Kind) -> Failure {
    let mut failure = Failure::new(
        ErrorCode::InvalidInput,
        format!(
            "{}@{} declares no such property. Use one of the properties it declares for {kind}.",
            pack.pack_id, pack.version
        ),
    );
    failure.error.expected = Some(json!(
        declared
            .iter()
            .filter(|property| property.record_kinds.iter().any(|k| k.kind() == kind))
            .map(|property| property.name.as_str())
            .collect::<Vec<_>>()
    ));
    failure
}

/// Check one value against the closed schema subset. The failure path is the
/// JSON Pointer of the offending value inside `extensions`.
fn check_value(
    schema: &ValueSchema,
    value: &Value,
    segments: &mut Vec<String>,
) -> Result<(), Failure> {
    let fail = |segments: &[String], message: &str| {
        let mut failure = Failure::new(ErrorCode::InvalidInput, message).at(pointer(segments));
        failure.error.expected = Some(json!(schema));
        failure
    };
    match schema.value_type {
        ValueType::String => {
            let text = value
                .as_str()
                .ok_or_else(|| fail(segments, "Expected a string."))?;
            if schema
                .max_length
                .is_some_and(|max| text.len() > max as usize)
            {
                return Err(fail(
                    segments,
                    "String exceeds the property's max_length in bytes.",
                ));
            }
            if let Some(allowed) = &schema.allowed
                && !allowed.iter().any(|allowed| allowed == text)
            {
                return Err(fail(
                    segments,
                    "String is not one of the property's enum values.",
                ));
            }
        }
        ValueType::Integer => {
            let number = value
                .as_i64()
                .ok_or_else(|| fail(segments, "Expected an integer."))?;
            if schema.minimum.is_some_and(|minimum| number < minimum)
                || schema.maximum.is_some_and(|maximum| number > maximum)
            {
                return Err(fail(segments, "Integer is outside the property's range."));
            }
        }
        ValueType::Decimal => {
            if !value.as_str().is_some_and(decimal) {
                return Err(fail(
                    segments,
                    "Expected a base-10 decimal string such as \"18.70\".",
                ));
            }
        }
        ValueType::Boolean => {
            if !value.is_boolean() {
                return Err(fail(segments, "Expected true or false."));
            }
        }
        ValueType::Array => {
            let items = value
                .as_array()
                .ok_or_else(|| fail(segments, "Expected an array."))?;
            if schema
                .max_items
                .is_some_and(|max| items.len() > max as usize)
            {
                return Err(fail(segments, "Array exceeds the property's max_items."));
            }
            let item_schema = schema.items.as_deref().ok_or_else(|| {
                Failure::new(ErrorCode::IntegrityFailure, "Array property lacks items.")
            })?;
            for (index, item) in items.iter().enumerate() {
                segments.push(index.to_string());
                check_value(item_schema, item, segments)?;
                segments.pop();
            }
        }
        ValueType::Object => {
            let object = value
                .as_object()
                .ok_or_else(|| fail(segments, "Expected an object."))?;
            let fields = schema.fields.as_deref().unwrap_or_default();
            for name in schema.required.iter().flatten() {
                if !object.contains_key(name) {
                    let mut path = segments.clone();
                    path.push(name.clone());
                    return Err(fail(&path, "Required field is missing."));
                }
            }
            for (name, item) in object {
                segments.push(name.clone());
                let field = fields
                    .iter()
                    .find(|field| &field.name == name)
                    .ok_or_else(|| {
                        fail(segments, "Field is not declared by the property schema.")
                    })?;
                check_value(&field.schema, item, segments)?;
                segments.pop();
            }
        }
    }
    Ok(())
}

fn decimal(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, "0"));
    text.chars().count() <= MAX_DECIMAL_CHARS
        && !whole.is_empty()
        && !fraction.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && fraction.bytes().all(|b| b.is_ascii_digit())
}

/// Whether a header extension key is a pack namespace (a pack ID). Keys with
/// a dot, such as the core's own `inv.*` keys, are not.
pub fn is_pack_namespace(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// One stored pack field, in namespace then field order.
pub struct PackField<'a> {
    pub pack_id: &'a str,
    pub field: &'a str,
    pub value: &'a Value,
}

/// The pack fields a record carries. Values are returned exactly as stored.
pub fn fields(record: &Record) -> Vec<PackField<'_>> {
    record
        .header
        .extensions
        .iter()
        .filter(|(key, _)| is_pack_namespace(key))
        .filter_map(|(pack_id, value)| Some((pack_id, value.as_object()?)))
        .flat_map(|(pack_id, object)| {
            object.iter().map(move |(field, value)| PackField {
                pack_id,
                field,
                value,
            })
        })
        .collect()
}

/// A `full` read's labeled view of the record's pack fields, or `None` when
/// it has none. The values are pack data the core never interprets.
pub fn projection(record: &Record) -> Option<Value> {
    let fields = fields(record);
    if fields.is_empty() {
        return None;
    }
    Some(json!({
        "interpretation": INTERPRETATION,
        "note": "Optional fields declared by a pinned method pack. Checked against the pack's schema when written; never read by gates, freshness, ranking or audits.",
        "fields": fields.iter().map(|field| {
            let mut entry = Map::new();
            entry.insert("pack_id".into(), json!(field.pack_id));
            entry.insert("field".into(), json!(field.field));
            entry.insert("value".into(), field.value.clone());
            Value::Object(entry)
        }).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests;
