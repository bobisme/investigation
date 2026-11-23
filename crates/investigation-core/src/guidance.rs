//! Contextual method guidance: trigger evaluation and card choice.
//!
//! Card text is guidance only when it comes from the built-in pack or from a
//! pack pinned to the case and resolved from its retained bytes
//! ([`crate::method_pack_ops::resolve`]). This module never loads a pack from
//! a path, so a pack file that is merely present, or ingested but not adopted,
//! can never supply guidance. Every function is a pure function of recorded
//! state and resolved packs: no clock, process, network or write.
//!
//! A recommendation carries at most one card. The card is chosen by the gap
//! rule that produced the recommendation, in fixed tiers: a card that names
//! the rule as a `gap_rule` trigger first, then, for a stall rule, a card that
//! names the `stalled` case fact. Within a tier, cards from pinned packs (in
//! adoption order) take precedence over the built-in pack; within one pack the
//! card with the fewest triggers (the most specific) wins, then file order.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;

use crate::{
    case::CaseMode,
    gap_rules::{GapInventory, GapStatus},
    identity::{Id, Kind},
    method_pack::{Card, CaseFact, LoadedPack, RecordKind, Trigger},
    method_pack_ops::{PackOrigin, ResolvedPack},
    state::{Record, State},
    wire::Digest,
};

/// Version of the trigger evaluation and card choice rules in this module.
pub const GUIDANCE_RULE_VERSION: &str = "guidance.v1";

/// How card text is marked in every response: method guidance from the binary
/// or a pinned pack, never case evidence and never an instruction to execute.
pub const CONTENT_TRUST: &str = "method_guidance";

/// Gap rules that report repeated inconclusive work. A recommendation from one
/// of these rules is matched to cards that name the `stalled` case fact, and
/// an unresolved gap from one of them makes that fact hold. These are the
/// stall rules of [`crate::stall`].
pub const STALL_GAP_RULES: &[&str] = crate::gap_rules::STALL_RULES;

/// Workflow order used to group cards. Stages not listed sort after these,
/// alphabetically.
pub const STAGE_ORDER: &[&str] = &[
    "start", "frame", "observe", "collect", "explain", "assess", "evaluate", "decide", "review",
    "record", "close",
];

/// The facts a card trigger can test, evaluated once for one case snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TriggerContext {
    pub mode: Option<CaseMode>,
    pub case_facts: BTreeSet<CaseFact>,
    pub record_kinds: BTreeSet<RecordKind>,
    /// Rules with at least one gap that is neither resolved nor deferred.
    pub gap_rules: BTreeSet<String>,
}

impl TriggerContext {
    /// Evaluate every trigger input for one case from recorded fields only.
    /// `inventory` must come from the same snapshot as `state`.
    pub fn evaluate(state: &State, case_id: Id, inventory: &GapInventory) -> Self {
        let gap_rules: BTreeSet<String> = inventory
            .gaps
            .iter()
            .filter(|gap| !matches!(gap.status, GapStatus::Resolved | GapStatus::Deferred))
            .map(|gap| gap.rule_id.clone())
            .collect();
        Self::from_records(state, case_id, gap_rules)
    }

    /// Evaluate case facts and record kinds only; no gap rule holds. Used by
    /// policy gates, which run inside a transaction without a verified
    /// snapshot to enumerate gaps from.
    pub fn evaluate_recorded(state: &State, case_id: Id) -> Self {
        Self::from_records(state, case_id, BTreeSet::new())
    }

    fn from_records(state: &State, case_id: Id, gap_rules: BTreeSet<String>) -> Self {
        let records: Vec<&Record> = state
            .records()
            .filter(|record| record.header.case_id == case_id)
            .collect();
        let mode = state
            .current(case_id)
            .and_then(|case| case.body.get("mode"))
            .and_then(|mode| serde_json::from_value(mode.clone()).ok());
        let record_kinds = ALL_RECORD_KINDS
            .iter()
            .copied()
            .filter(|kind| {
                records
                    .iter()
                    .any(|record| record.header.id.kind() == kind.kind())
            })
            .collect();
        Self {
            mode,
            case_facts: case_facts(&records, &gap_rules),
            record_kinds,
            gap_rules,
        }
    }

    pub fn holds(&self, trigger: &Trigger) -> bool {
        trigger
            .gap_rule
            .as_ref()
            .is_some_and(|rule| self.gap_rules.contains(rule))
            || trigger
                .case_fact
                .is_some_and(|fact| self.case_facts.contains(&fact))
            || trigger
                .record_kind
                .is_some_and(|kind| self.record_kinds.contains(&kind))
    }
}

const ALL_RECORD_KINDS: &[RecordKind] = &[
    RecordKind::Case,
    RecordKind::Question,
    RecordKind::Source,
    RecordKind::Artifact,
    RecordKind::Observation,
    RecordKind::Hypothesis,
    RecordKind::Assumption,
    RecordKind::Option,
    RecordKind::Criterion,
    RecordKind::Comparison,
    RecordKind::Assessment,
    RecordKind::Conclusion,
    RecordKind::Decision,
    RecordKind::Review,
    RecordKind::Objection,
    RecordKind::Annotation,
    RecordKind::Retrospective,
    RecordKind::Inquiry,
    RecordKind::Run,
];

fn case_facts(records: &[&Record], gap_rules: &BTreeSet<String>) -> BTreeSet<CaseFact> {
    let live = |kind: Kind| {
        records
            .iter()
            .copied()
            .filter(move |record| record.header.id.kind() == kind && !withdrawn(&record.body))
    };
    let open_hypotheses = live(Kind::Hypothesis)
        .filter(|record| {
            !matches!(
                text(&record.body, "lifecycle"),
                Some("rejected_within_scope" | "retired" | "rejected" | "superseded")
            )
        })
        .count();
    let open_options = live(Kind::Option)
        .filter(|record| {
            !matches!(
                text(&record.body, "lifecycle"),
                Some("ruled_out" | "withdrawn" | "superseded")
            )
        })
        .count();
    let mut facts = BTreeSet::new();
    if live(Kind::Question).next().is_none() {
        facts.insert(CaseFact::NoQuestion);
    }
    if open_hypotheses == 1 {
        facts.insert(CaseFact::SingleOpenHypothesis);
    }
    if open_hypotheses >= 2 {
        facts.insert(CaseFact::CompetingHypotheses);
    }
    if open_options >= 2 {
        facts.insert(CaseFact::MultipleOptions);
    }
    if live(Kind::Source).count() >= 2 {
        facts.insert(CaseFact::MultipleSources);
    }
    if live(Kind::Observation).any(|record| {
        field(&record.body, "value")
            .and_then(|value| value.get("kind"))
            .and_then(Value::as_str)
            == Some("measurement")
    }) {
        facts.insert(CaseFact::MeasuredObservation);
    }
    if STALL_GAP_RULES.iter().any(|rule| gap_rules.contains(*rule)) {
        facts.insert(CaseFact::Stalled);
    }
    facts
}

fn field<'a>(body: &'a Value, key: &str) -> Option<&'a Value> {
    body.get(key)
        .or_else(|| body.get("content").and_then(|content| content.get(key)))
}

fn text<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    field(body, key).and_then(Value::as_str)
}

fn withdrawn(body: &Value) -> bool {
    ["status", "lifecycle"]
        .iter()
        .any(|key| matches!(text(body, key), Some("retracted" | "withdrawn")))
        || field(body, "retracted").and_then(Value::as_bool) == Some(true)
}

/// A reference to one card, as attached to a recommendation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CardRef {
    pub card: String,
    pub title: String,
    pub stage: String,
    pub summary: String,
    pub pack_id: String,
    pub pack_version: String,
    pub pack_digest: Digest,
    pub origin: PackOrigin,
    /// The trigger that selected the card.
    pub matched_trigger: Trigger,
    pub content_trust: &'static str,
}

impl CardRef {
    fn new(pack: &ResolvedPack, card: &Card, trigger: Trigger) -> Self {
        Self {
            card: card.name.clone(),
            title: card.title.clone(),
            stage: card.stage.clone(),
            summary: card.summary.clone(),
            pack_id: pack.pack_id.clone(),
            pack_version: pack.version.clone(),
            pack_digest: pack.digest.clone(),
            origin: pack.origin,
            matched_trigger: trigger,
            content_trust: CONTENT_TRUST,
        }
    }
}

/// Available packs in precedence order: pinned packs in adoption order, then
/// the built-in pack. Unavailable pins never contribute cards.
pub fn guidance_packs(packs: &[ResolvedPack]) -> Vec<(&ResolvedPack, &LoadedPack)> {
    let available = |origin| {
        packs
            .iter()
            .filter(move |pack| pack.origin == origin && pack.is_available())
            .filter_map(|pack| pack.loaded.as_ref().map(|loaded| (pack, loaded)))
    };
    available(PackOrigin::Pinned)
        .chain(available(PackOrigin::BuiltIn))
        .collect()
}

/// Whether a card applies to a case mode. An unknown mode applies everywhere.
pub fn applies(pack: &LoadedPack, card: &Card, mode: Option<CaseMode>) -> bool {
    mode.is_none_or(|mode| {
        pack.pack().modes.contains(&mode) && (card.modes.is_empty() || card.modes.contains(&mode))
    })
}

/// The trigger tiers a recommendation from `rule_id` matches, in order.
pub fn rule_triggers(rule_id: &str) -> Vec<Trigger> {
    let mut tiers = vec![Trigger {
        gap_rule: Some(rule_id.to_owned()),
        case_fact: None,
        record_kind: None,
    }];
    if STALL_GAP_RULES.contains(&rule_id) {
        tiers.push(Trigger {
            gap_rule: None,
            case_fact: Some(CaseFact::Stalled),
            record_kind: None,
        });
    }
    tiers
}

/// The single card for a recommendation produced by `rule_id`, if any.
pub fn card_for_rule(
    packs: &[ResolvedPack],
    mode: Option<CaseMode>,
    rule_id: &str,
) -> Option<CardRef> {
    card_for_triggers(packs, mode, &rule_triggers(rule_id))
}

/// The first card matching the earliest tier, under the precedence rules in
/// the module documentation. Cards with no triggers are never chosen.
pub fn card_for_triggers(
    packs: &[ResolvedPack],
    mode: Option<CaseMode>,
    tiers: &[Trigger],
) -> Option<CardRef> {
    let ordered = guidance_packs(packs);
    tiers.iter().find_map(|trigger| {
        ordered.iter().find_map(|(resolved, loaded)| {
            loaded
                .pack()
                .cards
                .iter()
                .enumerate()
                .filter(|(_, card)| applies(loaded, card, mode) && card.triggers.contains(trigger))
                .min_by_key(|(index, card)| (card.triggers.len(), *index))
                .map(|(_, card)| CardRef::new(resolved, card, trigger.clone()))
        })
    })
}

/// One line describing when a card is shown.
pub fn trigger_line(card: &Card) -> String {
    if card.triggers.is_empty() {
        return "on request only".into();
    }
    card.triggers
        .iter()
        .map(describe_trigger)
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn describe_trigger(trigger: &Trigger) -> String {
    if let Some(rule) = &trigger.gap_rule {
        format!("gap rule {rule}")
    } else if let Some(fact) = &trigger.case_fact {
        format!("case fact {}", wire_name(fact))
    } else if let Some(kind) = &trigger.record_kind {
        format!("record kind {}", wire_name(kind))
    } else {
        "never".into()
    }
}

fn wire_name(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Rank for grouping cards by workflow stage.
pub fn stage_rank(stage: &str) -> (usize, String) {
    (
        STAGE_ORDER
            .iter()
            .position(|known| *known == stage)
            .unwrap_or(STAGE_ORDER.len()),
        stage.to_owned(),
    )
}

/// Up to five names close to `wanted`: an edit distance within a third of its
/// length (at least two), or one name containing the other.
pub fn close_matches<'a>(wanted: &str, names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let wanted = wanted.to_ascii_lowercase();
    let threshold = (wanted.chars().count() / 3).max(2);
    let mut scored: Vec<(usize, String)> = names
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|name| {
            let distance = edit_distance(&wanted, name);
            let contains = !wanted.is_empty() && (name.contains(&wanted) || wanted.contains(name));
            (distance <= threshold || contains).then(|| (distance, name.to_owned()))
        })
        .collect();
    scored.sort();
    scored.into_iter().take(5).map(|(_, name)| name).collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, left) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, right) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(left != *right);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests;
