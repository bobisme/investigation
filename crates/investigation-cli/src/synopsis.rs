//! Authored synopses in `show`, `search` and `brief`.
//!
//! A synopsis is editorial text bound to one subject revision. It never
//! replaces the generated `summary`, caveat or freshness fields, and it is
//! labelled as authored data, never instructions. A stale or replaced synopsis
//! is not shown as the current account of its subject at summary depth: reads
//! fall back to the deterministic projection and say so. `full` depth shows
//! the text with its state, so the change stays inspectable.

use investigation_core::{
    classification::{AudiencePolicy, Classifier, Field},
    freshness::FreshnessIndex,
    state::{Record, State},
    synopsis::{self, SynopsisState, SynopsisStatus},
};
use serde_json::{Value, json};

use crate::views::ViewDepth;

/// Reasons listed below full depth, as in the other views.
const SHOWN_REASONS: usize = 3;

const FALLBACK_NOTICE: &str = "The newest authored synopsis is stale: its subject changed materially or needs review. The summary is the deterministic projection of typed fields; use --depth full to read the synopsis as history.";

fn status_value(status: &SynopsisStatus, depth: ViewDepth) -> Value {
    let mut value = json!(status);
    if depth != ViewDepth::Full
        && let Some(reasons) = value["reasons"].as_array_mut()
        && reasons.len() > SHOWN_REASONS
    {
        let omitted = reasons.len() - SHOWN_REASONS;
        reasons.truncate(SHOWN_REASONS);
        value["reasons_omitted"] = json!(omitted);
    }
    value
}

/// Synopses of a subject that the default audience may read, newest first,
/// and the number withheld.
fn visible_for_subject(
    state: &State,
    index: &FreshnessIndex,
    classifier: &Classifier<'_>,
    policy: &AudiencePolicy,
    subject: &Record,
) -> (Vec<(SynopsisStatus, String)>, usize) {
    let mut shown = Vec::new();
    let mut withheld = 0;
    for status in synopsis::for_subject(state, index, subject.header.id) {
        let Some(record) = state.revision(status.annotation_ref) else {
            continue;
        };
        if !classifier.may_show(record, Field::Content, policy) {
            withheld += 1;
            continue;
        }
        let text = synopsis::text(record).unwrap_or_default().to_owned();
        shown.push((status, text));
    }
    (shown, withheld)
}

/// `authored_synopsis` for a record that has synopses, or `synopsis_binding`
/// for a synopsis annotation itself. Adds nothing for other records.
/// `policy_state` is today's head; it differs from `state` for a historical
/// read, whose disclosure still follows today's classification.
pub(crate) fn show_fields(
    view: &mut Value,
    state: &State,
    policy_state: &State,
    index: &FreshnessIndex,
    record: &Record,
    depth: ViewDepth,
) {
    if let Some(status) = synopsis::status(state, index, record) {
        view["synopsis_binding"] = status_value(&status, depth);
        return;
    }
    let classifier = Classifier::new(policy_state);
    let policy = AudiencePolicy::default();
    let (shown, withheld) = visible_for_subject(state, index, &classifier, &policy, record);
    if shown.is_empty() && withheld == 0 {
        return;
    }
    let mut block = json!({
        "available":!shown.is_empty(),
        "state":shown.first().map_or("withheld", |(status, _)| status.state.as_str()),
        "summary_source":"typed_fields",
        "replaced":shown.iter().filter(|(status, _)| status.state == SynopsisState::Replaced).count(),
        "withheld":withheld,
        "rendering":"quoted_data",
    });
    if let Some((latest, text)) = shown.first() {
        let mut item = status_value(latest, depth);
        let current = latest.state == SynopsisState::Current;
        // Title depth reports availability only; a stale synopsis's text is
        // history, read at full depth.
        if depth == ViewDepth::Full || (depth == ViewDepth::Summary && current) {
            item["text"] = json!(text);
        }
        if !current {
            block["fallback"] = json!("deterministic_projection");
            block["notice"] = json!(FALLBACK_NOTICE);
        }
        block["latest"] = item;
    }
    if depth == ViewDepth::Full {
        block["history"] = json!(
            shown
                .iter()
                .skip(1)
                .map(|(status, text)| {
                    let mut item = status_value(status, depth);
                    item["text"] = json!(text);
                    item
                })
                .collect::<Vec<_>>()
        );
    }
    view["authored_synopsis"] = block;
}

/// One text line for `show`: the synopsis state, and the quoted text only when
/// it is current. Empty when the view has no authored synopsis.
pub(crate) fn text_line(view: &Value) -> String {
    let block = &view["authored_synopsis"];
    if let Some(state) = block["state"].as_str() {
        return match block["latest"]["text"].as_str() {
            // Quoted and escaped as data: nothing inside can end the line.
            Some(text) if state == "current" => format!(
                "\nAuthored synopsis [editorial_summary, current]: {}",
                investigation_core::authored_text::quote(text)
            ),
            _ => format!(
                "\nAuthored synopsis [editorial_summary, {state}]: not shown; the summary above uses typed fields."
            ),
        };
    }
    match view["synopsis_binding"]["state"].as_str() {
        Some(state) => format!(
            "\nSynopsis of {} r{} [editorial_summary, {state}]",
            view["synopsis_binding"]["subject_ref"]["id"]
                .as_str()
                .unwrap_or("?"),
            view["synopsis_binding"]["subject_ref"]["revision"]
        ),
        None => String::new(),
    }
}

/// Search additions: `synopsis_binding` on a synopsis hit, and
/// `synopsis.authored` on a hit that has a visible synopsis.
pub(crate) fn search_fields(
    result: &mut Value,
    state: &State,
    index: &FreshnessIndex,
    record: &Record,
    classifier: &Classifier<'_>,
    policy: &AudiencePolicy,
) {
    if let Some(status) = synopsis::status(state, index, record) {
        result["synopsis_binding"] = status_value(&status, ViewDepth::Summary);
        return;
    }
    let (shown, _) = visible_for_subject(state, index, classifier, policy, record);
    let Some((latest, text)) = shown.first() else {
        return;
    };
    let mut authored = json!({
        "state":latest.state,
        "annotation_ref":latest.annotation_ref,
        "subject_ref":latest.subject_ref,
        "recorded_by":latest.recorded_by,
        "label":latest.label,
        "counts_as_source_hit":false,
        "content_trust":latest.content_trust,
        "rendering":"quoted_data",
    });
    if latest.state == SynopsisState::Current {
        authored["text"] = json!(text);
    }
    result["synopsis"]["authored"] = authored;
}

/// A count-only brief omission for the case's synopses, with a read command
/// for the first stale one (or the newest current one). Synopsis text is never
/// part of a brief. `None` when the case has no visible synopsis. Disclosure
/// follows `policy_state` (today's head); `pin` binds the read argv to the
/// brief's snapshot.
pub(crate) fn brief_omission(
    state: &State,
    policy_state: &State,
    index: &FreshnessIndex,
    workspace: &[String],
    pin: &[String],
) -> Option<Value> {
    let classifier = Classifier::new(policy_state);
    let policy = AudiencePolicy::default();
    let counts = synopsis::case_counts(state, index, |record| {
        classifier.may_show(record, Field::Content, &policy)
    });
    if counts.total() == 0 {
        return None;
    }
    let target = counts.stale_refs.first().copied().or_else(|| {
        state
            .records()
            .filter(|record| record.header.case_id == index.case_id())
            .filter(|record| synopsis::is_synopsis(record))
            .filter(|record| classifier.may_show(record, Field::Content, &policy))
            .max_by_key(|record| (record.header.acceptance_sequence, record.header.id))
            .map(Record::reference)
    })?;
    let mut argv = workspace.to_vec();
    argv.extend([
        "--case".into(),
        index.case_id().to_string(),
        "show".into(),
        target.id.to_string(),
    ]);
    argv.extend(pin.iter().cloned());
    argv.extend(["--format".into(), "json".into()]);
    Some(json!({
        "kind":"authored_synopses",
        "current":counts.current,
        "stale":counts.stale,
        "replaced":counts.replaced,
        "reason":"Authored synopses are editorial, not evidence, and are left out of the brief. A stale synopsis no longer describes its subject.",
        "argv":argv,
    }))
}
