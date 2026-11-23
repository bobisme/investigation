//! Search coverage recorded on a search Run.
//!
//! A Run that searched a corpus may carry a [`SearchCoverage`] block: the corpus
//! identity and bounds, the exact queries and filters, inclusion and exclusion
//! rules, hit counts, per-query results and what was not searched. The core
//! stores the block exactly as the recorder stated it. It never runs a search,
//! never infers a count from another and never decides that a search was
//! adequate; readers such as the absence gate make that judgement.
//!
//! An empty result is evidence of absence only within the stated bounds. A
//! recorded zero (`total_hits: 0` or `hits: 0`) is a stated empty result; an
//! absent count was not recorded and is not treated as zero.
//!
//! Readers: [`run_coverage`] for a Run record, [`search_coverage`] for the
//! current revision of a Run by ID, and [`cited_coverage`] for the exact Run an
//! Observation cites through `run_ref`.

use serde_json::Value;

use crate::{
    command::RevisionRef,
    explain::SearchCoverage,
    identity::{Id, Kind},
    input,
    state::{Record, State},
    wire::{ErrorCode, Failure},
};

/// Run body key that holds the [`SearchCoverage`] block.
pub const SEARCH_COVERAGE_KEY: &str = "search_coverage";

const MAX_ITEMS: usize = 256;
const TEXT_LIMIT: usize = 2048;

fn invalid(path: &str, message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(path)
}

fn text(value: &str, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() {
        return Err(invalid(path, "Supply nonblank text describing this field."));
    }
    if value.len() > TEXT_LIMIT {
        return Err(invalid(path, "Shorten this text to at most 2048 bytes."));
    }
    Ok(())
}

fn list(items: &[String], path: &str) -> Result<(), Failure> {
    if items.len() > MAX_ITEMS {
        return Err(invalid(path, "List at most 256 entries."));
    }
    items
        .iter()
        .enumerate()
        .try_for_each(|(index, item)| text(item, &format!("{path}/{index}")))
}

/// Shape checks only. Counts are never compared with each other here beyond
/// the consistency checks of the Run shape, and nothing is inferred.
pub fn validate(coverage: &SearchCoverage, path: &str) -> Result<(), Failure> {
    text(&coverage.corpus, &format!("{path}/corpus"))?;
    if let Some(bounds) = &coverage.revision_bounds {
        text(bounds, &format!("{path}/revision_bounds"))?;
    }
    if let Some(range) = &coverage.date_range {
        text(range, &format!("{path}/date_range"))?;
    }
    list(&coverage.queries, &format!("{path}/queries"))?;
    list(&coverage.filters, &format!("{path}/filters"))?;
    list(
        &coverage.inclusion_rules,
        &format!("{path}/inclusion_rules"),
    )?;
    list(
        &coverage.exclusion_rules,
        &format!("{path}/exclusion_rules"),
    )?;
    list(&coverage.collections, &format!("{path}/collections"))?;
    list(&coverage.not_searched, &format!("{path}/not_searched"))?;
    if coverage.query_results.len() > MAX_ITEMS {
        return Err(invalid(
            &format!("{path}/query_results"),
            "List at most 256 entries.",
        ));
    }
    for (index, result) in coverage.query_results.iter().enumerate() {
        text(
            &result.query,
            &format!("{path}/query_results/{index}/query"),
        )?;
    }
    Ok(())
}

/// The coverage block stored on a Run body, if present and well formed.
pub fn recorded_coverage(body: &Value) -> Option<SearchCoverage> {
    match body.get(SEARCH_COVERAGE_KEY) {
        None | Some(Value::Null) => None,
        Some(value) => input::decode(value).ok(),
    }
}

/// The coverage of a Run record. `None` for any other record kind.
pub fn run_coverage(run: &Record) -> Option<SearchCoverage> {
    (run.header.id.kind() == Kind::Run)
        .then(|| recorded_coverage(&run.body))
        .flatten()
}

/// The coverage of the current revision of Run `run`.
pub fn search_coverage(state: &State, run: Id) -> Option<SearchCoverage> {
    state.current(run).and_then(run_coverage)
}

/// The coverage of the exact Run revision an Observation cites through
/// `run_ref`, in the same case.
pub fn cited_coverage(state: &State, observation: &Record) -> Option<SearchCoverage> {
    let run: RevisionRef = input::decode(observation.body.get("run_ref")?).ok()?;
    state
        .revision(run)
        .filter(|record| record.header.case_id == observation.header.case_id)
        .and_then(run_coverage)
}

/// True when the recorder stated an explicit empty result: a zero total or a
/// query with zero hits, and no query with hits. Absent counts never qualify.
pub fn states_empty_result(coverage: &SearchCoverage) -> bool {
    let zero_total = coverage.total_hits == Some(0);
    let zero_query = coverage
        .query_results
        .iter()
        .any(|result| result.hits == Some(0));
    let any_hits = coverage.total_hits.is_some_and(|total| total > 0)
        || coverage
            .query_results
            .iter()
            .any(|result| result.hits.is_some_and(|hits| hits > 0));
    (zero_total || zero_query) && !any_hits
}

/// True when the block states what was searched in bounded terms: a corpus,
/// at least one query, and some recorded bound (revision, date range or
/// collections). It says nothing about adequacy.
pub fn states_bounds(coverage: &SearchCoverage) -> bool {
    !coverage.queries.is_empty()
        && (coverage.revision_bounds.is_some()
            || coverage.date_range.is_some()
            || !coverage.collections.is_empty())
}

fn count(value: Option<u64>) -> String {
    value.map_or_else(|| "not recorded".into(), |count| count.to_string())
}

/// One-line coverage summary for views. Counts are shown as stored.
pub fn summary(coverage: &SearchCoverage) -> String {
    let mut out = format!(
        "Search coverage: corpus {}; {} queries; hits total {}, examined {}, empty {}",
        coverage.corpus.trim(),
        coverage.queries.len(),
        count(coverage.total_hits),
        count(coverage.examined_hits),
        count(coverage.empty_hits),
    );
    if let Some(bounds) = &coverage.revision_bounds {
        out.push_str(&format!("; revision bounds {}", bounds.trim()));
    }
    if let Some(range) = &coverage.date_range {
        out.push_str(&format!("; date range {}", range.trim()));
    }
    if !coverage.not_searched.is_empty() {
        out.push_str(&format!(
            "; {} items not searched",
            coverage.not_searched.len()
        ));
    }
    out.push('.');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn coverage(extra: Value) -> SearchCoverage {
        let mut value = json!({
            "corpus": "Registry", "queries": ["q"], "filters": [],
            "inclusion_rules": [], "exclusion_rules": []
        });
        value.as_object_mut().unwrap().extend(
            extra
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn legacy_block_without_new_fields_still_decodes() {
        let c = coverage(json!({}));
        assert!(c.query_results.is_empty() && c.not_searched.is_empty());
        assert!(!states_empty_result(&c));
    }

    #[test]
    fn explicit_zero_is_empty_but_absent_count_is_not() {
        assert!(states_empty_result(&coverage(json!({"total_hits": 0}))));
        assert!(states_empty_result(&coverage(
            json!({"query_results": [{"query": "q", "hits": 0}]})
        )));
        assert!(!states_empty_result(&coverage(
            json!({"query_results": [{"query": "q"}]})
        )));
        assert!(!states_empty_result(&coverage(
            json!({"total_hits": 3, "query_results": [{"query": "q", "hits": 0}]})
        )));
    }

    #[test]
    fn summary_shows_counts_as_stored() {
        let line = summary(&coverage(json!({"total_hits": 0, "not_searched": ["x"]})));
        assert!(line.contains("total 0, examined not recorded, empty not recorded"));
        assert!(line.contains("1 items not searched"));
    }

    #[test]
    fn validation_rejects_blank_entries() {
        assert!(validate(&coverage(json!({"not_searched": [" "]})), "/c").is_err());
        assert!(validate(&coverage(json!({"query_results": [{"query": ""}]})), "/c").is_err());
        assert!(validate(&coverage(json!({"total_hits": 0})), "/c").is_ok());
    }
}

#[cfg(test)]
mod operation_tests {
    use super::*;
    use crate::{
        catalog::standard_catalog,
        command::Command,
        header::SystemClock,
        identity::SystemIdSource,
        state::{Acceptance, AcceptedTransaction, validate_and_apply},
    };
    use serde_json::json;

    struct Case {
        state: State,
        case: Option<Id>,
    }

    impl Case {
        fn new() -> Self {
            let mut test = Self {
                state: State::default(),
                case: None,
            };
            let accepted = test
                .submit("case.create", json!({"title": "Search", "mode": "record"}))
                .unwrap();
            test.case = Some(accepted.case_id);
            test
        }

        fn submit(&mut self, op: &str, payload: Value) -> Result<AcceptedTransaction, Failure> {
            let sequence = self.state.sequence() + 1;
            let command: Command = serde_json::from_value(json!({
                "schema_version": "inv.command.v1",
                "command_id": format!("command-{sequence}"),
                "case_id": self.case,
                "actor": "agent:test",
                "preconditions": [],
                "operations": [{"op": op, "payload": payload}],
            }))
            .unwrap();
            let accepted = validate_and_apply(
                &self.state,
                &command,
                &standard_catalog(),
                Acceptance {
                    sequence,
                    clock: &mut SystemClock,
                    ids: &mut SystemIdSource,
                },
            )?;
            self.state.apply(&accepted)?;
            Ok(accepted)
        }
    }

    fn search_run(coverage: Value) -> Value {
        json!({
            "intent": {"kind": "exploration", "intent": "Search the registry."},
            "producer": "agent:producer",
            "artifact_refs": [],
            "execution_status": "completed",
            "evidence_completeness": "partial",
            "evidence_kind": "observational",
            "variables_changed": [],
            "search_coverage": coverage,
        })
    }

    fn coverage() -> Value {
        json!({
            "corpus": "Errata registry", "revision_bounds": "snapshot 12",
            "queries": ["part 7"], "filters": [], "inclusion_rules": [],
            "exclusion_rules": [], "total_hits": 0, "examined_hits": 0,
            "empty_hits": 0, "collections": ["errata"], "date_range": "2020-2026",
            "query_results": [{"query": "part 7", "hits": 0}],
            "not_searched": ["drafts"]
        })
    }

    #[test]
    fn empty_search_run_validates_and_an_absence_observation_links_to_it() {
        let mut case = Case::new();
        let accepted = case.submit("run.record", search_run(coverage())).unwrap();
        let run = accepted.records.last().unwrap().reference();
        let stored = search_coverage(&case.state, run.id).unwrap();
        assert_eq!(serde_json::to_value(&stored).unwrap(), coverage());
        assert!(states_empty_result(&stored) && states_bounds(&stored));

        let observation = case
            .submit(
                "observation.add",
                json!({
                    "value": {"kind": "measurement", "value": "0", "unit": "hits"},
                    "basis": {"kind": "assertion", "assertion": "No erratum found.", "context": null},
                    "extraction_method": null, "validity": "valid", "quality": [],
                    "limitations": [], "evidence_kind": "observational",
                    "variables_changed": [], "observed_at": null, "run_ref": run,
                }),
            )
            .unwrap();
        let observation = case
            .state
            .current(observation.records.last().unwrap().header.id)
            .unwrap();
        assert_eq!(cited_coverage(&case.state, observation), Some(stored));
    }

    #[test]
    fn counts_are_stored_as_given_and_inconsistent_counts_are_rejected() {
        let mut case = Case::new();
        let mut partial = coverage();
        partial["total_hits"] = Value::Null;
        partial["query_results"] = json!([{"query": "part 7"}]);
        let accepted = case
            .submit("run.record", search_run(partial.clone()))
            .unwrap();
        let stored =
            search_coverage(&case.state, accepted.records.last().unwrap().header.id).unwrap();
        assert_eq!(stored.total_hits, None);
        assert_eq!(stored.query_results[0].hits, None);
        assert!(!states_empty_result(&stored));

        let mut bad = coverage();
        bad["examined_hits"] = json!(5);
        assert!(case.submit("run.record", search_run(bad)).is_err());
        let mut blank = coverage();
        blank["not_searched"] = json!([""]);
        assert!(case.submit("run.record", search_run(blank)).is_err());
    }
}
