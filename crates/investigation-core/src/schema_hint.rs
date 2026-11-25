//! What a payload field should have been, read from the operation's own
//! JSON Schema (bn-29t7). Pure.
//!
//! A decode failure names a path. This module follows that path through the
//! payload schema, choosing the `oneOf`/`anyOf` branch the payload's own tag
//! selects, and describes the schema at the end of it: an enum's allowed
//! values, a const, or an object's `$defs` type with its required fields, the
//! required fields the payload lacks and the fields it may not have. It adds
//! one valid example for the field, from the operation's example when it has
//! the field and otherwise built from the schema. Every word comes from the
//! schema; nothing of the submitted value is echoed except its field names,
//! which are bounded like a path.

use serde_json::{Map, Value, json};

/// The expected shape at a failing path.
#[derive(Debug, Clone, PartialEq)]
pub struct Hint {
    /// A sentence fragment: what the field must be.
    pub expected: String,
    /// The `$defs` type of the field, when it has one.
    pub type_name: Option<String>,
    /// Machine-readable: `type`, `enum`, `required`, `missing`, `unknown`,
    /// `example`.
    pub detail: Value,
}

const MAX_LISTED: usize = 24;
const MAX_FIELD_NAME: usize = 64;

/// Describe the schema at `path` (a JSON Pointer into `payload`).
pub fn describe(
    schema: &Value,
    payload: &Value,
    path: &str,
    example: Option<&Value>,
) -> Option<Hint> {
    let mut node = schema;
    let mut instance = Some(payload);
    let mut name: Option<String> = None;
    for segment in segments(path) {
        let (resolved, _) = select(schema, node, instance);
        let next = if let Some(child) = resolved
            .get("properties")
            .and_then(|properties| properties.get(&segment))
        {
            child
        } else if let (Some(items), Ok(_)) = (resolved.get("items"), segment.parse::<usize>()) {
            items
        } else {
            return None;
        };
        node = next;
        instance = instance.and_then(|value| match value {
            Value::Object(map) => map.get(&segment),
            Value::Array(items) => segment.parse::<usize>().ok().and_then(|i| items.get(i)),
            _ => None,
        });
        name = None;
    }
    let (resolved, def) = select(schema, node, instance);
    if def.is_some() {
        name = def;
    }
    let mut detail = Map::new();
    let expected = phrase(schema, node, instance, &mut detail, &mut name);
    // An object that has every required field and no unknown one failed
    // inside a field the decoder could not name: describing the object would
    // mislead, so give no hint.
    if instance.is_some_and(Value::is_object)
        && detail.contains_key("required")
        && !detail.contains_key("missing")
        && !detail.contains_key("unknown")
    {
        return None;
    }
    if let Some(type_name) = &name {
        detail.insert("type".into(), json!(type_name));
    }
    let example = example
        .and_then(|example| example.pointer(path))
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| synthesize(schema, resolved, 0));
    detail.insert("example".into(), example);
    Some(Hint {
        expected,
        type_name: name,
        detail: Value::Object(detail),
    })
}

impl Hint {
    /// The full refusal message, starting with the generic decode message so
    /// readers that match on it keep working.
    pub fn message(&self, operation: &str) -> String {
        let mut message = format!(
            "Field does not match the expected schema: expected {}.",
            self.expected
        );
        if let Some(example) = self.detail.get("example") {
            let text = example.to_string();
            if text.chars().count() <= 400 {
                message.push_str(&format!(" Example: {text}."));
            }
        }
        if let Some(name) = &self.type_name {
            message.push_str(&format!(
                " See $defs/{name} in `investigation schema {operation}`."
            ));
        }
        message
    }
}

fn segments(path: &str) -> Vec<String> {
    path.split('/')
        .skip(1)
        .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
        .collect()
}

/// Follow `$ref`s, returning the target and the last `$defs` name.
fn deref<'a>(root: &'a Value, mut node: &'a Value) -> (&'a Value, Option<String>) {
    let mut name = None;
    for _ in 0..16 {
        let Some(target) = node.get("$ref").and_then(Value::as_str) else {
            break;
        };
        let Some(def) = target.strip_prefix("#/$defs/") else {
            break;
        };
        let Some(next) = root.get("$defs").and_then(|defs| defs.get(def)) else {
            break;
        };
        name = Some(def.to_owned());
        node = next;
    }
    (node, name)
}

fn is_null_schema(node: &Value) -> bool {
    node.get("type").and_then(Value::as_str) == Some("null")
}

fn branches(node: &Value) -> Option<&Vec<Value>> {
    node.get("oneOf")
        .or_else(|| node.get("anyOf"))
        .and_then(Value::as_array)
}

/// The schema node that applies to `instance`: `$ref`s followed and, for a
/// `oneOf`/`anyOf`, the branch the instance's tag (a `const` property) picks,
/// else the only non-null branch.
fn select<'a>(
    root: &'a Value,
    node: &'a Value,
    instance: Option<&Value>,
) -> (&'a Value, Option<String>) {
    let (mut node, mut name) = deref(root, node);
    for _ in 0..8 {
        let Some(options) = branches(node) else {
            break;
        };
        let live: Vec<(&Value, Option<String>)> = options
            .iter()
            .map(|branch| deref(root, branch))
            .filter(|(branch, _)| !is_null_schema(branch))
            .collect();
        let chosen = live
            .iter()
            .find(|(branch, _)| tag_matches(branch, instance) == Some(true))
            .or_else(|| (live.len() == 1).then(|| &live[0]));
        let Some((branch, def)) = chosen else {
            break;
        };
        node = branch;
        if def.is_some() {
            name.clone_from(def);
        }
    }
    (node, name)
}

/// Whether every `const` property of an object branch equals the instance's.
fn tag_matches(branch: &Value, instance: Option<&Value>) -> Option<bool> {
    let properties = branch.get("properties")?.as_object()?;
    let instance = instance?.as_object()?;
    let mut any = false;
    for (key, property) in properties {
        if let Some(constant) = property.get("const") {
            any = true;
            if instance.get(key) != Some(constant) {
                return Some(false);
            }
        }
    }
    any.then_some(true)
}

fn bounded(name: &str) -> String {
    name.chars().take(MAX_FIELD_NAME).collect()
}

fn list(values: &[String]) -> String {
    let mut out: Vec<String> = values.iter().take(MAX_LISTED).cloned().collect();
    if values.len() > MAX_LISTED {
        out.push(format!("and {} more", values.len() - MAX_LISTED));
    }
    out.join(", ")
}

/// The tag property and value of a tagged object branch, such as `kind`
/// `"select"`.
fn tag_of(branch: &Value) -> Option<(String, Value)> {
    branch
        .get("properties")?
        .as_object()?
        .iter()
        .find_map(|(key, property)| Some((key.clone(), property.get("const")?.clone())))
}

fn phrase(
    root: &Value,
    raw: &Value,
    instance: Option<&Value>,
    detail: &mut Map<String, Value>,
    name: &mut Option<String>,
) -> String {
    let (node, def) = deref(root, raw);
    if def.is_some() {
        *name = def;
    }
    if let Some(options) = branches(node) {
        let nullable = options
            .iter()
            .any(|branch| is_null_schema(deref(root, branch).0));
        let live: Vec<(&Value, Option<String>)> = options
            .iter()
            .map(|branch| deref(root, branch))
            .filter(|(branch, _)| !is_null_schema(branch))
            .collect();
        if let Some((branch, _)) = live
            .iter()
            .find(|(branch, _)| tag_matches(branch, instance) == Some(true))
            .or_else(|| (live.len() == 1).then(|| &live[0]))
        {
            let inner = phrase(root, branch, instance, detail, name);
            return if nullable {
                format!("{inner}, or null")
            } else {
                inner
            };
        }
        let forms: Vec<String> = live
            .iter()
            .map(|(branch, def)| match (tag_of(branch), def) {
                (Some((key, value)), _) => {
                    let required = required_of(branch)
                        .into_iter()
                        .filter(|field| *field != key)
                        .collect::<Vec<_>>();
                    if required.is_empty() {
                        format!("{key} {value}")
                    } else {
                        format!("{key} {value} with {}", list(&required))
                    }
                }
                (None, Some(def)) => format!("a {def}"),
                (None, None) => short(branch),
            })
            .collect();
        detail.insert("one_of".into(), json!(forms));
        let prefix = name
            .as_ref()
            .map(|name| format!("a {name}: "))
            .unwrap_or_default();
        let nullable = if nullable { ", or null" } else { "" };
        return format!("{prefix}one of {}{nullable}", list(&forms));
    }
    if let Some(values) = node.get("enum").and_then(Value::as_array) {
        let shown: Vec<String> = values.iter().map(value_text).collect();
        detail.insert("enum".into(), Value::Array(values.clone()));
        return format!("one of {}", list(&shown));
    }
    if let Some(constant) = node.get("const") {
        detail.insert("const".into(), constant.clone());
        return format!("the value {constant}");
    }
    if type_is(node, "object") || node.get("properties").is_some() {
        let required = required_of(node);
        let properties: Vec<String> = node
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| properties.keys().cloned().collect())
            .unwrap_or_default();
        let mut out = match name {
            Some(name) => format!("a {name} object"),
            None => "an object".to_owned(),
        };
        if !required.is_empty() {
            out.push_str(&format!(" with required fields {}", list(&required)));
            detail.insert("required".into(), json!(required));
        }
        if let Some(Value::Object(map)) = instance {
            let missing: Vec<String> = required
                .iter()
                .filter(|field| !map.contains_key(field.as_str()))
                .cloned()
                .collect();
            if !missing.is_empty() {
                out.push_str(&format!("; missing {}", list(&missing)));
                detail.insert("missing".into(), json!(missing));
            }
            if node.get("additionalProperties") == Some(&Value::Bool(false)) {
                let unknown: Vec<String> = map
                    .keys()
                    .filter(|key| !properties.contains(key))
                    .map(|key| bounded(key))
                    .collect();
                if !unknown.is_empty() {
                    out.push_str(&format!("; not allowed: {}", list(&unknown)));
                    detail.insert("unknown".into(), json!(unknown));
                }
            }
        }
        return out;
    }
    short(node)
}

fn required_of(node: &Value) -> Vec<String> {
    node.get("required")
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn type_is(node: &Value, kind: &str) -> bool {
    match node.get("type") {
        Some(Value::String(value)) => value == kind,
        Some(Value::Array(values)) => values.iter().any(|value| value == kind),
        _ => false,
    }
}

fn value_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

/// A short phrase for a scalar or array schema.
fn short(node: &Value) -> String {
    let nullable = type_is(node, "null");
    let base = if type_is(node, "string") {
        if node
            .get("pattern")
            .and_then(Value::as_str)
            .is_some_and(|p| p.contains("case|q|src"))
        {
            "a record ID".to_owned()
        } else if node.get("pattern").is_some() {
            "a string of the documented form".to_owned()
        } else {
            let mut out = "a string".to_owned();
            if node
                .get("minLength")
                .and_then(Value::as_u64)
                .is_some_and(|min| min > 0)
            {
                out = "a nonblank string".to_owned();
            }
            if let Some(max) = node.get("maxLength").and_then(Value::as_u64) {
                out.push_str(&format!(" of at most {max} characters"));
            }
            out
        }
    } else if type_is(node, "integer") {
        match node.get("minimum").and_then(Value::as_u64) {
            Some(min) => format!("an integer of at least {min}"),
            None => "an integer".to_owned(),
        }
    } else if type_is(node, "number") {
        "a number".to_owned()
    } else if type_is(node, "boolean") {
        "true or false".to_owned()
    } else if type_is(node, "array") {
        "an array".to_owned()
    } else {
        "a value of the documented form".to_owned()
    };
    if nullable {
        format!("{base}, or null")
    } else {
        base
    }
}

/// A minimal value valid for `node`, built from the schema.
fn synthesize(root: &Value, raw: &Value, depth: usize) -> Value {
    if depth > 6 {
        return Value::Null;
    }
    let (node, _) = deref(root, raw);
    if let Some(options) = branches(node) {
        let first = options
            .iter()
            .map(|branch| deref(root, branch).0)
            .find(|branch| !is_null_schema(branch))
            .or_else(|| options.first());
        return first.map_or(Value::Null, |branch| synthesize(root, branch, depth + 1));
    }
    if let Some(values) = node.get("enum").and_then(Value::as_array) {
        return values.first().cloned().unwrap_or(Value::Null);
    }
    if let Some(constant) = node.get("const") {
        return constant.clone();
    }
    if type_is(node, "object") || node.get("properties").is_some() {
        let mut out = Map::new();
        let properties = node.get("properties").and_then(Value::as_object);
        for field in required_of(node) {
            let value = properties
                .and_then(|properties| properties.get(&field))
                .map_or(Value::Null, |child| synthesize(root, child, depth + 1));
            out.insert(field, value);
        }
        return Value::Object(out);
    }
    if type_is(node, "null") && !type_is(node, "string") && !type_is(node, "array") {
        return Value::Null;
    }
    if type_is(node, "string") {
        return if node.get("pattern").is_some() {
            json!("<ID>")
        } else {
            json!("text")
        };
    }
    if type_is(node, "integer") || type_is(node, "number") {
        return json!(node.get("minimum").and_then(Value::as_u64).unwrap_or(1));
    }
    if type_is(node, "boolean") {
        return json!(false);
    }
    if type_is(node, "array") {
        return json!([]);
    }
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        json!({
            "$defs": {
                "Materiality": {"type": "string", "enum": ["material", "nonmaterial", "unknown"]},
                "StopCondition": {
                    "type": "object", "additionalProperties": false,
                    "properties": {"description": {"type": "string", "minLength": 1},
                        "reasons": {"type": "array", "items": {"type": "string"}}},
                    "required": ["description", "reasons"]
                },
                "Knowledge": {"oneOf": [
                    {"type": "object", "properties": {"state": {"const": "unknown"}}, "required": ["state"]},
                    {"type": "object", "properties": {"state": {"const": "known"},
                        "value": {"$ref": "#/$defs/StopCondition"}}, "required": ["state", "value"]}
                ]}
            },
            "type": "object", "additionalProperties": false,
            "properties": {
                "materiality": {"$ref": "#/$defs/Materiality"},
                "stop_condition": {"anyOf": [{"$ref": "#/$defs/Knowledge"}, {"type": "null"}]}
            },
            "required": ["materiality"]
        })
    }

    #[test]
    fn an_enum_lists_its_values_and_gives_one() {
        let hint = describe(
            &schema(),
            &json!({"materiality": "high"}),
            "/materiality",
            None,
        )
        .unwrap();
        assert_eq!(hint.expected, "one of material, nonmaterial, unknown");
        assert_eq!(hint.detail["example"], "material");
        assert_eq!(hint.type_name.as_deref(), Some("Materiality"));
    }

    #[test]
    fn a_nested_object_is_found_through_the_tagged_branch() {
        let payload = json!({"materiality": "unknown",
            "stop_condition": {"state": "known", "value": {"text": "done"}}});
        let hint = describe(&schema(), &payload, "/stop_condition/value", None).unwrap();
        assert_eq!(hint.type_name.as_deref(), Some("StopCondition"));
        assert!(
            hint.expected
                .contains("required fields description, reasons"),
            "{hint:?}"
        );
        assert!(
            hint.expected.contains("missing description, reasons"),
            "{hint:?}"
        );
        assert!(hint.expected.contains("not allowed: text"), "{hint:?}");
        assert_eq!(
            hint.detail["example"],
            json!({"description": "text", "reasons": []})
        );
        assert!(hint.message("case.amend").contains("$defs/StopCondition"));
    }

    #[test]
    fn untagged_forms_are_listed_and_the_example_comes_from_the_operation() {
        let payload = json!({"materiality": "unknown", "stop_condition": "soon"});
        let example = json!({"stop_condition": {"state": "unknown"}});
        let hint = describe(&schema(), &payload, "/stop_condition", Some(&example)).unwrap();
        assert!(hint.expected.contains("state \"unknown\""), "{hint:?}");
        assert!(
            hint.expected.contains("state \"known\" with value"),
            "{hint:?}"
        );
        assert!(hint.expected.ends_with("or null"), "{hint:?}");
        assert_eq!(hint.detail["example"], json!({"state": "unknown"}));
    }

    #[test]
    fn a_complete_object_gives_no_hint_since_the_fault_is_inside_it() {
        let payload = json!({"materiality": "unknown",
            "stop_condition": {"state": "known", "value": {"description": "x", "reasons": [7]}}});
        assert!(describe(&schema(), &payload, "/stop_condition/value", None).is_none());
    }

    #[test]
    fn a_path_the_schema_does_not_have_gives_no_hint() {
        assert!(describe(&schema(), &json!({}), "/nowhere/else", None).is_none());
    }
}
