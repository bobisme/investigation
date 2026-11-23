//! Inherited trust of authored text in CLI reads and `evidence read` (§16.1).
//!
//! `evidence read` under a declared session (`--session` or
//! `INVESTIGATION_SESSION`) records the read for the actor; a later write that
//! declares the same session inherits the read's level when it commits. Reads
//! show the result as `inherited_trust` on every authored record, always
//! beside the stated limit: only declared inputs and reads made through this
//! tool are known.

use std::{collections::BTreeMap, env};

use investigation_core::{
    command::RevisionRef,
    identity::Id,
    inherited_trust::{
        InheritedTrust, LIMITS, Resolver, Session, SessionRead, TrustLevel, is_authored,
    },
    state::Record,
    wire::{ErrorCode, Failure},
};
use investigation_store::{Store, journal::CaseReplay};
use serde_json::{Value, json};

use crate::{Global, brief::Section, selection::select_actor, views::ViewDepth};

/// Environment fallback for `--session`.
pub(crate) const SESSION_ENV: &str = "INVESTIGATION_SESSION";

/// The brief field that states the levels shown and the limit once.
const BRIEF_FIELD: &str = "inherited_trust";
/// Inputs named at summary depth: the ones that set the lowest level.
const LOWEST_SHOWN: usize = 3;
/// Inputs and session reads listed at full depth.
const LISTED: usize = 16;

/// The declared session: `--session`, else a nonempty `INVESTIGATION_SESSION`.
pub(crate) fn select_session(global: &Global) -> Result<Option<Session>, Failure> {
    if let Some(session) = &global.session {
        return Ok(Some(session.clone()));
    }
    let Some(value) = env::var_os(SESSION_ENV).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .map(Some)
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::InvalidInput,
                "INVESTIGATION_SESSION must be 1-128 ASCII letters, digits, ':', '_', '-' or '.'. Pass --session to override it.",
            )
            .at(SESSION_ENV)
        })
}

/// Record an `evidence read` for the declared session and describe it.
pub(crate) fn record_read(
    store: &mut Store,
    global: &Global,
    case_id: Id,
    artifact: RevisionRef,
) -> Result<Value, Failure> {
    let Some(session) = select_session(global)? else {
        return Ok(json!({
            "recorded":false,"session":null,"reason":"no_session_declared",
            "notice":"This read is not recorded. A write inherits the level of evidence reads only when the read and the write declare the same session (--session or INVESTIGATION_SESSION).",
        }));
    };
    if global.read_only {
        return Ok(json!({
            "recorded":false,"session":session,"reason":"read_only",
            "notice":"--read-only writes nothing, so this read is not recorded for the session.",
        }));
    }
    // Reading never requires an actor; recording a read for a session does.
    let Ok(actor) = select_actor(store.workspace(), global.actor.as_ref(), None) else {
        return Ok(json!({
            "recorded":false,"session":session,"reason":"no_actor",
            "notice":"A session read is recorded for an actor. Pass --actor, set the workspace actor or AGENT to record it.",
        }));
    };
    store.record_session_read(&actor, &session, case_id, artifact)?;
    Ok(json!({
        "recorded":true,"session":session,"actor":actor,
        "level":TrustLevel::UntrustedExternal,"limits":LIMITS,
    }))
}

/// Fill the envelope's session from `--session` or the environment, or
/// refuse a disagreement. Returns the bytes to submit.
pub(crate) fn declare_session(
    global: &Global,
    command: &mut investigation_core::command::Command,
    input: Vec<u8>,
) -> Result<Vec<u8>, Failure> {
    let Some(selected) = select_session(global)? else {
        return Ok(input);
    };
    match &command.session {
        Some(declared) if declared == &selected => Ok(input),
        Some(declared) => {
            let mut failure = Failure::new(
                ErrorCode::InvalidInput,
                "--session or INVESTIGATION_SESSION must match the request session.",
            )
            .at("/session");
            failure.error.expected = Some(json!(selected));
            failure.error.actual = Some(json!(declared));
            Err(failure)
        }
        None => {
            command.session = Some(selected);
            Ok(serde_json::to_vec(&command).expect("command serializes"))
        }
    }
}

fn input_ref(id: Id, revision: Option<u64>, via: &str) -> Value {
    let mut value = json!({"id":id,"via":via});
    if let Some(revision) = revision {
        value["revision"] = json!(revision);
    }
    value
}

/// Shape one computed level for a view at `depth`.
fn trust_value(trust: &InheritedTrust, case_id: Id, depth: ViewDepth) -> Value {
    let (here, elsewhere): (Vec<&SessionRead>, Vec<&SessionRead>) = trust
        .session_reads
        .iter()
        .partition(|read| read.case_id == case_id);
    let mut lowest: Vec<Value> = trust
        .inputs
        .iter()
        .filter(|input| input.level == trust.level && !input.outside_case)
        .map(|input| input_ref(input.id, input.revision, "declared"))
        .collect();
    if trust.level == TrustLevel::UntrustedExternal {
        lowest.extend(here.iter().map(|read| {
            input_ref(
                read.artifact.id,
                Some(read.artifact.revision),
                "session_read",
            )
        }));
    }
    let lowest_count = lowest.len();
    lowest.truncate(LOWEST_SHOWN);
    let mut value = json!({
        "level":trust.level,
        "lowest_inputs":lowest,
        "declared_inputs":trust.inputs.len(),
        "session_reads":trust.session_reads_total(),
        "revisions_without_session":trust.undeclared_sessions(),
        "limits":LIMITS,
    });
    if lowest_count > LOWEST_SHOWN {
        value["lowest_inputs_omitted"] = json!(lowest_count - LOWEST_SHOWN);
    }
    if depth == ViewDepth::Full {
        value["inputs"] = json!(trust.inputs.iter().take(LISTED).collect::<Vec<_>>());
        value["inputs_omitted"] = json!(trust.inputs.len().saturating_sub(LISTED));
        value["sessions"] = json!(trust.sessions);
        value["session_read_refs"] = json!(
            here.iter()
                .take(LISTED)
                .map(|read| read.artifact)
                .collect::<Vec<_>>()
        );
        value["session_read_refs_omitted"] = json!(here.len().saturating_sub(LISTED));
        // Another case's Artifacts are counted, never named here.
        value["other_case_session_reads"] = json!(elsewhere.len());
    }
    value
}

/// `inherited_trust` for an authored record, or `None` for recorded source
/// content and at title depth.
pub(crate) fn view(record: &Record, replay: &CaseReplay, depth: ViewDepth) -> Option<Value> {
    if depth == ViewDepth::Title || !is_authored(record.header.id.kind()) {
        return None;
    }
    let trust = Resolver::new(&replay.state, replay).inherited(record);
    Some(trust_value(&trust, record.header.case_id, depth))
}

/// A lesson's level: its Retrospective's, with the limit.
pub(crate) fn lesson_view(retrospective: &Record, replay: &CaseReplay) -> Value {
    let trust = Resolver::new(&replay.state, replay).inherited(retrospective);
    json!({"level":trust.level,"source":"retrospective","limits":LIMITS})
}

/// The level of the synopsis `show` reports under `authored_synopsis`.
pub(crate) fn synopsis_fields(view: &mut Value, replay: &CaseReplay, depth: ViewDepth) {
    let latest = &view["authored_synopsis"]["latest"];
    let Some(reference) = serde_json::from_value::<RevisionRef>(latest["annotation_ref"].clone())
        .ok()
        .and_then(|reference| replay.state.revision(reference))
    else {
        return;
    };
    if let Some(trust) = self::view(reference, replay, depth) {
        view["authored_synopsis"]["latest"]["inherited_trust"] = trust;
    }
}

/// Room a complete brief read reserves for [`brief_limits`].
pub(crate) const BRIEF_MARGIN: usize = 1024;

/// Brief, before filling: every item keeps only its level. The minimum
/// envelope carries none; the limit is stated once by [`brief_limits`].
pub(crate) fn brief_compact(sections: &mut [Section]) {
    for item in sections
        .iter_mut()
        .flat_map(|section| section.items.iter_mut())
    {
        compact(item);
    }
}

/// Keep only the level of an item's `inherited_trust`.
pub(crate) fn compact(item: &mut Value) {
    if let Some(level) = item["inherited_trust"]["level"].as_str().map(str::to_owned) {
        item["inherited_trust"] = json!({"level":level});
    }
}

/// Top-level items of a bounded output: list entries and single objects.
fn items(output: &mut Value) -> impl Iterator<Item = &mut Value> {
    output
        .as_object_mut()
        .into_iter()
        .flat_map(|object| object.values_mut())
        .flat_map(|value| match value {
            Value::Array(items) => items.iter_mut().collect::<Vec<_>>(),
            other => vec![other],
        })
}

/// Brief, after filling: when kept items show a level, state the count per
/// level and the limit once in `inherited_trust`. A level is never shown
/// without its limit: when the statement does not fit the budget, the items'
/// levels are removed instead.
pub(crate) fn brief_limits(mut output: Value, max_bytes: usize) -> Value {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for item in items(&mut output) {
        if let Some(level) = item["inherited_trust"]["level"].as_str() {
            *counts.entry(level.to_owned()).or_default() += 1;
        }
    }
    if counts.is_empty() {
        return output;
    }
    output[BRIEF_FIELD] = json!({"items_by_level":counts,"limits":LIMITS});
    let encoded = serde_json::to_vec(&output).map_or(usize::MAX, |bytes| bytes.len() + 1);
    if encoded <= max_bytes {
        return output;
    }
    if let Some(object) = output.as_object_mut() {
        object.remove(BRIEF_FIELD);
    }
    for item in items(&mut output) {
        if let Some(item) = item.as_object_mut() {
            item.remove("inherited_trust");
        }
    }
    output
}

/// A text-mode line for a view's level and its limit; empty without one.
pub(crate) fn text(view: &Value) -> String {
    let mut text = String::new();
    for (label, trust) in [
        ("Inherited trust", &view["inherited_trust"]),
        (
            "Synopsis inherited trust",
            &view["authored_synopsis"]["latest"]["inherited_trust"],
        ),
    ] {
        if let Some(level) = trust["level"].as_str() {
            text.push_str(&format!(
                "\n{label}: {level} (declared inputs {}, session reads {}, revisions without a session {}).",
                trust["declared_inputs"], trust["session_reads"], trust["revisions_without_session"]
            ));
        }
    }
    if !text.is_empty() {
        text.push_str(&format!("\nTrust limits: {LIMITS}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use investigation_core::inherited_trust::{DeclaredInput, RevisionSession};

    fn id(value: &str) -> Id {
        value.parse().unwrap()
    }

    #[test]
    fn other_case_reads_are_counted_never_named() {
        let case = id("case_018d87fc-5234-7000-8000-000000000001");
        let other = id("case_018d87fc-5234-7000-8000-000000000002");
        let artifact = id("art_018d87fc-5234-7000-8000-000000000003");
        let foreign = id("art_018d87fc-5234-7000-8000-000000000004");
        let question = id("q_018d87fc-5234-7000-8000-000000000005");
        let read = |case_id, id| SessionRead {
            case_id,
            artifact: RevisionRef { id, revision: 1 },
        };
        let trust = InheritedTrust {
            level: TrustLevel::UntrustedExternal,
            inputs: vec![DeclaredInput {
                id: question,
                revision: Some(1),
                level: TrustLevel::Unknown,
                outside_case: false,
            }],
            sessions: vec![RevisionSession {
                revision: 1,
                acceptance_sequence: 4,
                session: Some("s-1".parse().unwrap()),
                reads_total: 2,
            }],
            session_reads: vec![read(case, artifact), read(other, foreign)],
        };
        let summary = trust_value(&trust, case, ViewDepth::Summary);
        assert_eq!(summary["level"], "untrusted_external");
        assert_eq!(summary["limits"], LIMITS);
        assert_eq!(summary["lowest_inputs"][0]["id"], json!(artifact));
        assert!(!summary.to_string().contains(&foreign.to_string()));
        let full = trust_value(&trust, case, ViewDepth::Full);
        assert_eq!(full["other_case_session_reads"], 1);
        assert!(!full.to_string().contains(&foreign.to_string()));
        assert!(text(&json!({"inherited_trust":summary})).contains(LIMITS));
    }

    #[test]
    fn a_bounded_output_never_keeps_a_level_without_its_limit() {
        let output = json!({
            "subject":{"id":"x","inherited_trust":{"level":"unknown"}},
            "outcomes":[{"id":"y","inherited_trust":{"level":"untrusted_external"}},{"id":"z"}],
        });
        let roomy = brief_limits(output.clone(), 1 << 20);
        assert_eq!(roomy["inherited_trust"]["limits"], LIMITS);
        assert_eq!(
            roomy["inherited_trust"]["items_by_level"],
            json!({"unknown":1,"untrusted_external":1})
        );
        let tight = brief_limits(output.clone(), output.to_string().len() + 1);
        assert!(tight.get("inherited_trust").is_none());
        assert!(!tight.to_string().contains("level"), "{tight}");
        let plain = json!({"outcomes":[{"id":"z"}]});
        assert_eq!(brief_limits(plain.clone(), 1), plain);
    }
}
