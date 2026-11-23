//! Whole-vault checks shared by the renderer's tests and the scenario tests
//! in `investigation-cli` (included there by path).
//!
//! [`check`] answers three questions about one plan, independently of the
//! renderer's own reference walk:
//!
//! 1. Does every current record with a note kind render to its folder?
//! 2. Does every link (and block anchor) resolve to a planned note?
//! 3. Is every reference between two visible notes linked both ways: from
//!    the citing note to the cited one, and back?
//! 4. Is every planned canvas valid JSON Canvas 1.0 whose file nodes all
//!    resolve to planned notes and whose edges all carry a label and no
//!    colour ([`canvas_problems`])?
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::{
    freshness::dependency_edges,
    identity::{Id, Kind},
    state::Record,
    wire::Snapshot,
};
use investigation_render::{
    NoteMarker, PlanEntry, RenderPlan, VerifiedSnapshot, incremental::rebase, layout,
};
use pulldown_cmark::{Event, Options, Parser, Tag};
use serde_json::Value;

pub fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_GFM
        | Options::ENABLE_WIKILINKS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
}

/// Every link a viewer would follow, as (target, block anchor). Parsed, so
/// link-like text inside code is not counted.
pub fn links(note: &str) -> Vec<(String, Option<String>)> {
    Parser::new_ext(note, options())
        .filter_map(|event| match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                // A table cell escapes the label pipe; the parser keeps the backslash.
                let dest = dest_url.trim_end_matches('\\');
                Some(match dest.split_once("#^") {
                    Some((target, anchor)) => (target.to_owned(), Some(anchor.to_owned())),
                    None => (dest.to_owned(), None),
                })
            }
            _ => None,
        })
        .collect()
}

/// A flat frontmatter property, unquoted.
pub fn property(note: &str, key: &str) -> Option<String> {
    let front = note.strip_prefix("---\n")?.split("\n---\n").next()?;
    front
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}: ")))
        .map(|value| value.trim_matches('"').to_owned())
}

/// The text of one `## heading` section, up to the next `## ` heading.
pub fn section<'a>(note: &'a str, heading: &str) -> Option<&'a str> {
    let start = note.find(&format!("\n## {heading}\n"))? + 1;
    let rest = &note[start..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |at| at + 4);
    Some(&rest[..end])
}

/// Every record a body refers to: dependency edges, exact `{id, revision}`
/// objects anywhere, and bare IDs in fields named `*_ref` or `*_refs`.
pub fn references(record: &Record) -> BTreeSet<Id> {
    fn walk(value: &Value, key: Option<&str>, found: &mut BTreeSet<Id>) {
        match value {
            Value::Object(map) => {
                if let (Some(id), Some(_)) = (
                    map.get("id")
                        .and_then(Value::as_str)
                        .and_then(|id| id.parse::<Id>().ok()),
                    map.get("revision").and_then(Value::as_u64),
                ) {
                    found.insert(id);
                    return;
                }
                for (key, child) in map {
                    walk(child, Some(key), found);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, key, found);
                }
            }
            Value::String(text)
                if key.is_some_and(|key| key.ends_with("_ref") || key.ends_with("_refs")) =>
            {
                if let Ok(id) = text.parse::<Id>() {
                    found.insert(id);
                }
            }
            _ => {}
        }
    }
    let mut found: BTreeSet<Id> = dependency_edges(record)
        .unwrap_or_default()
        .into_iter()
        .map(|edge| edge.target.id)
        .collect();
    walk(&record.body, None, &mut found);
    found.remove(&record.header.id);
    found
}

/// How the bytes published at `entry`'s path relate to it: `Ok(None)` when
/// they are the entry, `Ok(Some(basis))` when they are the entry restamped
/// to an earlier head of its case (the basis an unchanged note keeps), and
/// `Err` otherwise. The earlier head must lie on the case's verified chain.
pub fn up_to_basis(
    entry: &PlanEntry,
    bytes: &[u8],
    snapshot: &VerifiedSnapshot,
) -> Result<Option<Snapshot>, String> {
    if bytes == entry.content() {
        return Ok(None);
    }
    let path = entry.path().as_str();
    let case = path
        .strip_prefix("Cases/")
        .and_then(|rest| rest.split('/').next())
        .and_then(|case_id| case_id.parse().ok())
        .and_then(|case_id| snapshot.case(case_id))
        .ok_or_else(|| format!("{path}: differs from the plan outside any case"))?;
    let Some(NoteMarker::Case(basis)) = NoteMarker::read(bytes) else {
        return Err(format!(
            "{path}: differs from the plan and names no case head"
        ));
    };
    let head = case.head();
    if basis.sequence >= head.sequence || case.head_at(basis.sequence).as_ref() != Some(&basis) {
        return Err(format!(
            "{path}: names {basis:?}, not an earlier head of its case (head {head:?})"
        ));
    }
    match rebase(entry.content(), head, &basis) {
        Some(rebased) if rebased == bytes => Ok(Some(basis)),
        _ => Err(format!(
            "{path}: differs from the plan by more than its basis {}",
            basis.sequence
        )),
    }
}

/// What [`check`] saw: the record kinds rendered, and every problem found.
#[derive(Debug, Default)]
pub struct Report {
    pub kinds: BTreeSet<Kind>,
    /// References checked both ways.
    pub pairs: usize,
    pub problems: Vec<String>,
}

pub fn check(plan: &RenderPlan, snapshot: &VerifiedSnapshot) -> Report {
    let mut report = Report::default();
    let text = |path: &str| -> Option<&str> {
        plan.entry(path)
            .map(|entry| std::str::from_utf8(entry.content()).expect("UTF-8 note"))
    };
    // Links of every note, by note path.
    let mut linked: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for entry in plan.entries() {
        let path = entry.path().as_str();
        if path.ends_with(".canvas") {
            report.problems.extend(canvas_problems(plan, path));
            continue;
        }
        if !path.ends_with(".md") {
            continue;
        }
        let note = text(path).unwrap();
        let mut targets = BTreeSet::new();
        for (target, anchor) in links(note) {
            // A link to a non-note file names its extension.
            let file = if target.ends_with(".canvas") {
                target.clone()
            } else {
                format!("{target}.md")
            };
            match text(&file) {
                None => report
                    .problems
                    .push(format!("{path}: link to {target} does not resolve")),
                Some(linked_note) => {
                    if let Some(anchor) = anchor
                        && !linked_note.contains(&format!(" ^{anchor}\n"))
                    {
                        report
                            .problems
                            .push(format!("{path}: block ^{anchor} missing in {target}"));
                    }
                }
            }
            targets.insert(target);
        }
        linked.insert(path.to_owned(), targets);
    }
    for case in snapshot.cases() {
        let state = case.state();
        let visible = |id: Id| -> Option<String> {
            let path = layout::record_note(case.case_id(), id).ok()??;
            let note = text(path.as_str())?;
            (property(note, "inv_withheld").is_none()).then(|| path.to_string())
        };
        for record in state.records() {
            let id = record.header.id;
            let Some(path) = layout::record_note(case.case_id(), id).unwrap() else {
                continue;
            };
            let Some(note) = text(path.as_str()) else {
                report.problems.push(format!("{id} has no note at {path}"));
                continue;
            };
            report.kinds.insert(id.kind());
            if property(note, "inv_id").as_deref() != Some(id.to_string().as_str()) {
                report
                    .problems
                    .push(format!("{path} does not project {id}"));
            }
            // A record whose references are withheld lists none of them.
            let lineage = section(note, "Typed links")
                .is_some_and(|links| !links.contains("\n\nWithheld.\n"));
            let Some(from) = visible(id).filter(|_| lineage) else {
                continue;
            };
            for target in references(record) {
                if target.kind() == Kind::Case || state.current(target).is_none() {
                    continue;
                }
                let Some(to) = visible(target) else {
                    continue;
                };
                report.pairs += 1;
                let forward = to.strip_suffix(".md").unwrap();
                let back = from.strip_suffix(".md").unwrap();
                if !linked[&from].contains(forward) {
                    report
                        .problems
                        .push(format!("{from} names {target} but does not link it"));
                }
                if !linked[&to].contains(back) {
                    report
                        .problems
                        .push(format!("{to} is named by {id} but does not link back"));
                }
            }
        }
    }
    report
}

/// What is wrong with one planned canvas against JSON Canvas 1.0 and the
/// case-map rules: only file nodes, each resolving to a planned note; unique
/// IDs; every edge between two nodes with a non-empty label; no colour; no
/// key the specification does not define.
pub fn canvas_problems(plan: &RenderPlan, path: &str) -> Vec<String> {
    const TOP: &[&str] = &["nodes", "edges"];
    const NODE: &[&str] = &[
        "id", "type", "file", "subpath", "x", "y", "width", "height", "color",
    ];
    const EDGE: &[&str] = &[
        "id", "fromNode", "fromSide", "fromEnd", "toNode", "toSide", "toEnd", "color", "label",
    ];
    const SIDES: &[&str] = &["top", "right", "bottom", "left"];
    let mut problems = Vec::new();
    let Some(entry) = plan.entry(path) else {
        return vec![format!("{path}: not planned")];
    };
    let Ok(Value::Object(canvas)) = serde_json::from_slice::<Value>(entry.content()) else {
        return vec![format!("{path}: not a JSON object")];
    };
    let unknown = |problems: &mut Vec<String>,
                   kind: &str,
                   object: &serde_json::Map<String, Value>,
                   allowed: &[&str]| {
        for key in object.keys() {
            if !allowed.contains(&key.as_str()) {
                problems.push(format!(
                    "{path}: {kind} key {key:?} is not in JSON Canvas 1.0"
                ));
            }
        }
    };
    unknown(&mut problems, "top-level", &canvas, TOP);
    let empty = Vec::new();
    let nodes = canvas
        .get("nodes")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let edges = canvas
        .get("edges")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let mut ids = BTreeSet::new();
    for node in nodes {
        let Some(node) = node.as_object() else {
            problems.push(format!("{path}: a node is not an object"));
            continue;
        };
        unknown(&mut problems, "node", node, NODE);
        let id = node.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() || !ids.insert(id.to_owned()) {
            problems.push(format!("{path}: node id {id:?} empty or repeated"));
        }
        for key in ["x", "y", "width", "height"] {
            if !node.get(key).is_some_and(Value::is_i64) {
                problems.push(format!("{path}: node {id} has no integer {key}"));
            }
        }
        if node.get("type").and_then(Value::as_str) != Some("file") {
            problems.push(format!("{path}: node {id} is not a file node"));
        }
        if node.contains_key("color") {
            problems.push(format!("{path}: node {id} is coloured"));
        }
        match node.get("file").and_then(Value::as_str) {
            Some(file) if file.ends_with(".md") && plan.entry(file).is_some() => {}
            file => problems.push(format!("{path}: node {id} file {file:?} does not resolve")),
        }
    }
    let mut edge_ids = BTreeSet::new();
    for edge in edges {
        let Some(edge) = edge.as_object() else {
            problems.push(format!("{path}: an edge is not an object"));
            continue;
        };
        unknown(&mut problems, "edge", edge, EDGE);
        let id = edge.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() || ids.contains(id) || !edge_ids.insert(id.to_owned()) {
            problems.push(format!("{path}: edge id {id:?} empty or repeated"));
        }
        for end in ["fromNode", "toNode"] {
            if !edge
                .get(end)
                .and_then(Value::as_str)
                .is_some_and(|node| ids.contains(node))
            {
                problems.push(format!("{path}: edge {id} {end} names no node"));
            }
        }
        for side in ["fromSide", "toSide"] {
            if let Some(value) = edge.get(side)
                && !value.as_str().is_some_and(|value| SIDES.contains(&value))
            {
                problems.push(format!("{path}: edge {id} {side} {value}"));
            }
        }
        if !edge
            .get("label")
            .and_then(Value::as_str)
            .is_some_and(|label| !label.trim().is_empty())
        {
            problems.push(format!("{path}: edge {id} has no label"));
        }
        if edge.contains_key("color") {
            problems.push(format!("{path}: edge {id} is coloured"));
        }
    }
    problems
}
