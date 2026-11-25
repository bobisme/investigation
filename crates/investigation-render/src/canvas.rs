//! JSON Canvas files in the layout the viewer writes them, and their content
//! digest (ADR 0019).
//!
//! The viewer (release 1.15.1, the pinned release of the compatibility
//! profile) re-serializes a `.canvas` file when it opens it, in its own
//! layout: `{`, then each top-level key on its own line indented by one tab;
//! an array of nodes or edges opens on that line, lists each element as one
//! compact JSON object on its own line indented by two tabs, separated by
//! `,` and a line break, and closes on its own line indented by one tab; `}`
//! ends the file with no line break after it. The compact objects are
//! `JSON.stringify` output: no spaces, characters outside ASCII written as
//! they are, `/` not escaped, and the control characters `JSON.stringify`
//! escapes escaped the same way (serde_json's compact writer agrees with it on
//! every string). [`to_bytes`] writes exactly that, so a generated canvas the
//! viewer opens keeps its bytes. Both checked-in scenario maps, opened and
//! saved by that release, are reproduced byte for byte.
//!
//! Keys are written in a fixed order, the one the viewer's own files show for
//! file nodes and edges ([`NODE_KEYS`], [`EDGE_KEYS`]); keys outside those
//! lists follow in sorted order. An empty array is written `[]`, as
//! `JSON.stringify` writes one (no sample shows the viewer's choice here).
//!
//! [`digest`] is how the renderer knows a `.canvas` file: the SHA-256 of
//! [`to_bytes`] of the file's parsed JSON. Layout, key order, escaping and a
//! trailing line break do not change it; any changed value, added or removed
//! node, edge or key does. A file that is not JSON is known by its bytes.
//! Every other file is known by its bytes, as before.

use investigation_core::wire::Digest;
use investigation_store::journal::hash_bytes;
use serde_json::{Map, Value};

/// Top-level keys in the order the viewer writes them.
const TOP_KEYS: &[&str] = &["nodes", "edges"];
/// Node keys in the order the viewer writes them. The file-node keys (`id`,
/// `type`, `file`, `x`, `y`, `width`, `height`) are the order of the viewer's
/// own files; the others are placed by the JSON Canvas 1.0 field order.
pub(crate) const NODE_KEYS: &[&str] = &[
    "id",
    "type",
    "text",
    "file",
    "subpath",
    "url",
    "label",
    "background",
    "backgroundStyle",
    "x",
    "y",
    "width",
    "height",
    "color",
];
/// Edge keys in the order the viewer writes them; `id`, `fromNode`,
/// `fromSide`, `toNode`, `toSide`, `label` are the order of its own files.
pub(crate) const EDGE_KEYS: &[&str] = &[
    "id", "fromNode", "fromSide", "fromEnd", "toNode", "toSide", "toEnd", "color", "label",
];

/// Whether `name` (a file name or path) is a JSON Canvas file.
pub(crate) fn is_canvas(name: &str) -> bool {
    name.ends_with(".canvas")
}

/// The canvas `document` in the viewer's layout. See the module
/// documentation.
pub(crate) fn to_bytes(document: &Value) -> Vec<u8> {
    let Value::Object(top) = document else {
        return compact(document, &[]).into_bytes();
    };
    if top.is_empty() {
        return b"{}".to_vec();
    }
    let keys = ordered(top, TOP_KEYS);
    let mut out = String::from("{");
    for (index, key) in keys.iter().enumerate() {
        out.push_str("\n\t");
        out.push_str(&string(key));
        out.push(':');
        let value = &top[*key];
        let order = match *key {
            "nodes" => NODE_KEYS,
            "edges" => EDGE_KEYS,
            _ => &[],
        };
        match value {
            Value::Array(items) if !items.is_empty() => {
                out.push('[');
                for (at, item) in items.iter().enumerate() {
                    out.push_str(if at == 0 { "\n\t\t" } else { ",\n\t\t" });
                    out.push_str(&compact(item, order));
                }
                out.push_str("\n\t]");
            }
            _ => out.push_str(&compact(value, order)),
        }
        if index + 1 < keys.len() {
            out.push(',');
        }
    }
    out.push_str("\n}");
    out.into_bytes()
}

/// How the renderer knows a file's bytes: for a `.canvas` file that parses as
/// JSON, the SHA-256 of [`to_bytes`] of its content; otherwise the SHA-256
/// of the bytes.
pub(crate) fn digest(name: &str, bytes: &[u8]) -> Digest {
    if is_canvas(name)
        && let Ok(document) = serde_json::from_slice::<Value>(bytes)
    {
        return hash_bytes(&to_bytes(&document));
    }
    hash_bytes(bytes)
}

/// `map`'s keys: those in `order` first, in that order, then the rest sorted.
fn ordered<'a>(map: &'a Map<String, Value>, order: &[&'a str]) -> Vec<&'a str> {
    let mut keys: Vec<&str> = order
        .iter()
        .copied()
        .filter(|key| map.contains_key(*key))
        .collect();
    keys.extend(
        map.keys()
            .map(String::as_str)
            .filter(|key| !order.contains(key)),
    );
    keys
}

/// `value` as `JSON.stringify` writes it, with an object's keys in `order`
/// first; nested objects keep sorted key order.
fn compact(value: &Value, order: &[&str]) -> String {
    match value {
        Value::Object(map) => {
            let mut out = String::from("{");
            for (index, key) in ordered(map, order).iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&string(key));
                out.push(':');
                out.push_str(&compact(&map[*key], &[]));
            }
            out.push('}');
            out
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(|item| compact(item, &[])).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

fn string(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_layout_is_tabs_and_one_compact_object_per_line_without_a_final_newline() {
        let document = json!({
            "edges": [{"label": "supports", "toSide": "left", "toNode": "b", "fromSide": "right", "fromNode": "a", "id": "a--b"}],
            "nodes": [
                {"height": 240, "width": 400, "y": 0, "x": 0, "file": "Cases/c/a.md", "type": "file", "id": "a"},
                {"type": "text", "id": "t", "text": "Café / ü \"q\"\n\u{1}", "x": 1, "y": 2, "width": 3, "height": 4},
            ],
        });
        let text = String::from_utf8(to_bytes(&document)).unwrap();
        assert_eq!(
            text,
            "{\n\t\"nodes\":[\n\
             \t\t{\"id\":\"a\",\"type\":\"file\",\"file\":\"Cases/c/a.md\",\"x\":0,\"y\":0,\"width\":400,\"height\":240},\n\
             \t\t{\"id\":\"t\",\"type\":\"text\",\"text\":\"Café / ü \\\"q\\\"\\n\\u0001\",\"x\":1,\"y\":2,\"width\":3,\"height\":4}\n\
             \t],\n\
             \t\"edges\":[\n\
             \t\t{\"id\":\"a--b\",\"fromNode\":\"a\",\"fromSide\":\"right\",\"toNode\":\"b\",\"toSide\":\"left\",\"label\":\"supports\"}\n\
             \t]\n}"
        );
        assert_eq!(
            to_bytes(&json!({"nodes": [], "edges": []})),
            b"{\n\t\"nodes\":[],\n\t\"edges\":[]\n}"
        );
    }

    #[test]
    fn a_canvas_is_known_by_its_content_and_any_other_file_by_its_bytes() {
        let ours = to_bytes(
            &json!({"nodes": [{"id": "a", "type": "file", "file": "x/.md", "x": 0, "y": 0, "width": 1, "height": 1}], "edges": []}),
        );
        let pretty = b"{\n  \"edges\": [],\n  \"nodes\": [\n    {\"type\": \"file\", \"id\": \"a\", \"file\": \"x\\/\\u002emd\", \"x\": 0, \"y\": 0, \"width\": 1, \"height\": 1}\n  ]\n}\n";
        assert_eq!(digest("Map.canvas", &ours), hash_bytes(&ours));
        assert_eq!(digest("Map.canvas", pretty), hash_bytes(&ours));
        let moved = String::from_utf8_lossy(pretty).replace("\"x\": 0", "\"x\": 10");
        assert_ne!(digest("Map.canvas", moved.as_bytes()), hash_bytes(&ours));
        assert_eq!(digest("Map.md", pretty), hash_bytes(pretty));
        assert_eq!(digest("Map.canvas", b"not json"), hash_bytes(b"not json"));
    }
}
