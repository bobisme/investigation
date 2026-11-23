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
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::{
    freshness::dependency_edges,
    identity::{Id, Kind},
    state::Record,
};
use investigation_render::{RenderPlan, VerifiedSnapshot, layout};
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
        if !path.ends_with(".md") {
            continue;
        }
        let note = text(path).unwrap();
        let mut targets = BTreeSet::new();
        for (target, anchor) in links(note) {
            match text(&format!("{target}.md")) {
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
