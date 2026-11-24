//! The citation of each Observation a brief shows: the line span of its
//! quote in the source it cites, the exact recorded quote and the
//! quote state the core recorded. A successor cites from these fields.
//!
//! The quote state is shown exactly as recorded; only `verified` means the
//! core found the quote in the retained bytes. A line span is given only for
//! a verified quote or a verified `text_lines` selector, located in the same
//! bytes (see `investigation_store::store::cited_lines`). Content that the
//! effective classification hides is never shown or read: such an
//! Observation gets no citation.

use std::collections::BTreeMap;

use investigation_core::{
    brief_select,
    classification::{AudiencePolicy, Classifier, Field},
    evidence::{ObservationBasis, ObservationBody},
    identity::{Id, Kind},
    state::State,
};
use investigation_store::{
    Store,
    journal::CaseReplay,
    store::cited_lines::{CitedLines, CitedSpan},
};
use serde_json::{Map, Value, json};

/// How to cite, stated once at the top of a brief that shows citations.
pub(crate) const GUIDE: &str = "Cite evidence as FILE line N \"QUOTE\" from an observation's \
source title and citation. Only quote_state verified was found in the retained source bytes.";

/// The citation of every Observation `brief` can list, keyed by its ID.
pub(crate) fn citations(
    store: &Store,
    replay: &CaseReplay,
    today: Option<&State>,
) -> BTreeMap<Id, Value> {
    let state = &replay.state;
    let classification = today.unwrap_or(state);
    let classifier = Classifier::new(classification);
    let policy = AudiencePolicy::default();
    let bodies: Vec<(Id, ObservationBody)> = brief_select::evidence(state)
        .into_iter()
        .filter(|record| record.header.id.kind() == Kind::Observation)
        .filter(|record| may_cite(&record.body))
        .filter(|record| classifier.may_show(record, Field::Content, &policy))
        .filter_map(|record| {
            ObservationBody::from_stored(&record.body)
                .ok()
                .map(|body| (record.header.id, body))
        })
        .collect();
    let spans: Vec<(Id, CitedSpan<'_>)> = bodies
        .iter()
        .filter_map(|(id, body)| match &body.basis {
            ObservationBasis::Source {
                artifact_ref,
                selector,
                text_quote,
            } => Some((
                *id,
                CitedSpan {
                    artifact: *artifact_ref,
                    selector,
                    quote: text_quote,
                },
            )),
            ObservationBasis::Assertion { .. } => None,
        })
        .collect();
    let mut lines = store.cited_lines(state, classification, &spans);
    let mut out = BTreeMap::new();
    for (id, span) in spans {
        let Some(located) = lines.remove(&id) else {
            continue;
        };
        let mut citation = Map::new();
        match located {
            CitedLines::Located { start, end } => {
                citation.insert("lines".into(), json!({"start":start,"end":end}));
            }
            CitedLines::NotLocated(reason) => {
                citation.insert("lines_unavailable".into(), json!(reason));
            }
            CitedLines::NotApplicable => {}
        }
        let quote = span
            .quote
            .exact
            .as_deref()
            .filter(|exact| !exact.is_empty());
        if quote.is_none() && !citation.contains_key("lines") {
            // Nothing to cite by line or by quote.
            continue;
        }
        if let Some(exact) = quote {
            citation.insert("quote".into(), json!(exact));
        }
        citation.insert("quote_state".into(), json!(span.quote.state));
        out.insert(id, Value::Object(citation));
    }
    out
}

/// Whether a stored Observation body can have a citation: a source basis
/// with a recorded quote or a line selector. Checked before the body is
/// decoded, so a case of unquoted measurements costs no decoding.
fn may_cite(body: &Value) -> bool {
    let basis = &body["basis"];
    basis["kind"] == "source"
        && (basis["text_quote"]["exact"].is_string()
            || basis["selector"]["selector"]["type"] == "text_lines")
}

/// The text lines of one citation: the state and location, then the quote.
pub(crate) fn text_lines(item: &Value) -> Vec<String> {
    use investigation_core::authored_text::{AUTHORED_DATA, UNTRUSTED_EVIDENCE, quoted};
    let citation = &item["citation"];
    if !citation.is_object() {
        return Vec::new();
    }
    let mut head = vec!["citation".to_owned()];
    let state = citation["quote_state"].as_str().unwrap_or("unknown");
    if citation.get("quote").is_some() || state != "not_applicable" {
        head.push(format!("quote {state}"));
    }
    match (
        citation["lines"]["start"].as_u64(),
        citation["lines"]["end"].as_u64(),
    ) {
        (Some(start), Some(end)) if start == end => head.push(format!("line {start}")),
        (Some(start), Some(end)) => head.push(format!("lines {start}-{end}")),
        _ => {
            if let Some(reason) = citation["lines_unavailable"].as_str() {
                head.push(format!("line not located: {reason}"));
            }
        }
    }
    // A citation of a record that is no longer current, or no longer in
    // evidentiary use, says so on the same line.
    if let Some(freshness) = item["freshness"]["effective"]
        .as_str()
        .or_else(|| item["freshness"]["state"].as_str())
        .filter(|state| *state != "current")
    {
        head.push(format!("freshness {freshness}"));
    }
    if let Some(usage) = item["evidentiary_use"]["state"]
        .as_str()
        .filter(|state| *state != "in_use")
    {
        head.push(format!("use {usage}"));
    }
    // The source to cite is the item's own source title.
    if let Some(source) = item["source_title"].as_str() {
        head.push(quoted("source", AUTHORED_DATA, source));
    }
    let mut out = vec![
        head.into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("  "),
    ];
    if let Some(quote) = citation["quote"].as_str() {
        out.push(quoted("quote", UNTRUSTED_EVIDENCE, quote));
    }
    out
}
