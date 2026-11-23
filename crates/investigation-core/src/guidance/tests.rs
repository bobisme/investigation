use serde_json::json;
use sha2::{Digest as _, Sha256};

use super::*;
use crate::{
    command::RevisionRef,
    gap_rules::enumerate_gaps,
    header::{
        Actor, Classification, Provenance, RecordHeader, Revision, SchemaVersion, UtcTimestamp,
    },
    method_pack::GAP_RULE_TRIGGERS,
    method_pack_ops::{MethodPackPin, resolve, resolve_pin},
    state::AcceptedTransaction,
    wire::{RecordChange, Snapshot},
};

const LOCAL_PACK: &str = r#"schema_version = "inv.method-pack.v1"
id = "local-notes"
version = "1.0.0"
title = "Local notes"
summary = "Optional guidance for one team."
modes = ["explore"]

[[cards]]
name = "weigh"
title = "Weigh the open account"
stage = "assess"
summary = "An open account has no recorded assessment."
triggers = [{ gap_rule = "hypothesis.no_assessment" }]

[cards.body]
when = "An account is open and nothing interprets evidence against it."
record = "An assessment that cites the exact observations."
skip = "The account was withdrawn."
"#;

fn id(value: &str) -> Id {
    value.parse().unwrap()
}

fn case_id() -> Id {
    id("case_019acd16-0000-7000-8000-000000000001")
}

fn record(id: Id, body: serde_json::Value) -> Record {
    let actor: Actor = "agent:test".parse().unwrap();
    let time: UtcTimestamp = "2026-11-22T00:00:00.0Z".parse().unwrap();
    Record {
        header: RecordHeader {
            id,
            case_id: case_id(),
            schema_version: SchemaVersion::RecordV0Alpha,
            created_by: actor.clone(),
            actor_descriptor_self_reported: None,
            created_at: time.clone(),
            recorded_at: time,
            recorded_by: actor,
            observed_at: None,
            acquired_at: None,
            acceptance_sequence: 1,
            revision: Revision(1),
            history: vec![],
            classification: Classification::default(),
            provenance: Provenance::default(),
            extensions: Default::default(),
        },
        body,
    }
}

fn state(records: Vec<Record>) -> State {
    let transaction = AcceptedTransaction {
        case_id: case_id(),
        sequence: 1,
        created: records
            .iter()
            .map(|record| RecordChange {
                client_ref: None,
                id: record.header.id,
                revision: 1,
            })
            .collect(),
        records,
        updated: vec![],
        warnings: vec![],
    };
    let mut state = State::default();
    state.apply(&transaction).unwrap();
    state
}

fn context(records: Vec<Record>) -> TriggerContext {
    let state = state(records);
    let snapshot = Snapshot {
        sequence: 1,
        head_hash: format!("sha256:{}", "a".repeat(64)).parse().unwrap(),
    };
    let inventory = enumerate_gaps(&state, &snapshot, case_id()).unwrap();
    TriggerContext::evaluate(&state, case_id(), &inventory)
}

fn case(mode: &str) -> Record {
    record(
        case_id(),
        json!({"title":"fixture","mode":mode,"lifecycle":"active"}),
    )
}

fn pin(bytes: &[u8]) -> MethodPackPin {
    MethodPackPin {
        pack_id: "local-notes".into(),
        version: "1.0.0".into(),
        digest: format!("sha256:{:x}", Sha256::digest(bytes))
            .parse()
            .unwrap(),
        artifact_ref: RevisionRef {
            id: id("art_019acd16-0000-7000-8000-000000000009"),
            revision: 1,
        },
        reason: "Team guidance.".into(),
        adopted_at_sequence: 2,
        replaces: None,
    }
}

fn builtin() -> Vec<ResolvedPack> {
    resolve(&[], |_| None)
}

fn with_pin(bytes: Option<&[u8]>) -> Vec<ResolvedPack> {
    let mut packs = builtin();
    packs.push(resolve_pin(&pin(LOCAL_PACK.as_bytes()), bytes));
    packs
}

#[test]
fn each_gap_rule_maps_to_at_most_one_built_in_card_deterministically() {
    let packs = builtin();
    let chosen = |rule| card_for_rule(&packs, Some(CaseMode::Explain), rule);
    assert_eq!(
        chosen("hypothesis.no_assessment").unwrap().card,
        "discriminate"
    );
    assert_eq!(
        chosen("conclusion.causal_claim_without_rival_account")
            .unwrap()
            .card,
        "alternatives"
    );
    assert_eq!(
        chosen("run.outcome_incomplete_or_unknown").unwrap().card,
        "negative-result"
    );
    assert!(chosen("store.integrity.unknown").is_none());
    for rule in GAP_RULE_TRIGGERS {
        let first = chosen(rule);
        assert_eq!(first, chosen(rule), "{rule}");
        if let Some(card) = first {
            assert_eq!(card.pack_id, "general");
            assert_eq!(card.origin, PackOrigin::BuiltIn);
            assert_eq!(card.content_trust, CONTENT_TRUST);
            assert_eq!(card.matched_trigger.gap_rule.as_deref(), Some(*rule));
            assert_ne!(card.card, "handoff");
        }
    }
}

#[test]
fn the_stalled_fact_selects_the_stalled_card_over_broader_cards() {
    let stalled = Trigger {
        gap_rule: None,
        case_fact: Some(CaseFact::Stalled),
        record_kind: None,
    };
    let card = card_for_triggers(&builtin(), Some(CaseMode::Explore), &[stalled]).unwrap();
    assert_eq!(card.card, "stalled");
    for rule in STALL_GAP_RULES {
        assert_eq!(rule_triggers(rule).len(), 2);
    }
    assert_eq!(rule_triggers("hypothesis.no_assessment").len(), 1);
}

#[test]
fn an_available_pinned_card_takes_precedence_for_the_same_trigger() {
    let rule = "hypothesis.no_assessment";
    let pinned = with_pin(Some(LOCAL_PACK.as_bytes()));
    let card = card_for_rule(&pinned, Some(CaseMode::Explore), rule).unwrap();
    assert_eq!(
        (card.card.as_str(), card.pack_id.as_str()),
        ("weigh", "local-notes")
    );
    assert_eq!(card.origin, PackOrigin::Pinned);
    assert_eq!(card.pack_digest, pin(LOCAL_PACK.as_bytes()).digest);

    // The pinned pack does not apply to this mode, so general is used.
    let card = card_for_rule(&pinned, Some(CaseMode::Explain), rule).unwrap();
    assert_eq!(card.pack_id, "general");

    // Missing or altered bytes leave the pin unavailable: never guidance.
    for packs in [
        with_pin(None),
        with_pin(Some(LOCAL_PACK.replace("Weigh", "Edited").as_bytes())),
    ] {
        assert!(!packs[1].is_available());
        assert!(card_for_rule(&packs, Some(CaseMode::Explore), rule).is_none());
        let card = card_for_rule(&packs, Some(CaseMode::Explain), rule).unwrap();
        assert_eq!(
            (card.card.as_str(), card.pack_id.as_str()),
            ("discriminate", "general")
        );
        assert_eq!(guidance_packs(&packs).len(), 1);
    }
}

#[test]
fn case_facts_come_from_recorded_fields_only() {
    let question = record(
        id("q_019acd16-0000-7000-8000-000000000002"),
        json!({"wording":"Why?","importance":"normal"}),
    );
    let hypothesis = |n: u32, lifecycle: &str| {
        record(
            id(&format!("hyp_019acd16-0000-7000-8000-00000000001{n}")),
            json!({"statement":"An account.","lifecycle":lifecycle}),
        )
    };
    let source = |n: u32| {
        record(
            id(&format!("src_019acd16-0000-7000-8000-00000000002{n}")),
            json!({"title":"A source"}),
        )
    };
    let option = |n: u32, lifecycle: &str| {
        record(
            id(&format!("opt_019acd16-0000-7000-8000-00000000003{n}")),
            json!({"description":"An option.","lifecycle":lifecycle}),
        )
    };
    let measured = record(
        id("obs_019acd16-0000-7000-8000-000000000041"),
        json!({"value":{"kind":"measurement","value":"28","unit":"s"}}),
    );

    let empty = context(vec![case("explore")]);
    assert_eq!(empty.mode, Some(CaseMode::Explore));
    assert_eq!(empty.case_facts, BTreeSet::from([CaseFact::NoQuestion]));
    assert_eq!(empty.record_kinds, BTreeSet::from([RecordKind::Case]));

    let single = context(vec![
        case("explain"),
        question.clone(),
        hypothesis(1, "active"),
        hypothesis(2, "retired"),
        source(1),
        option(1, "eligible"),
        option(2, "ruled_out"),
    ]);
    assert_eq!(
        single.case_facts,
        BTreeSet::from([CaseFact::SingleOpenHypothesis])
    );
    assert!(single.gap_rules.contains("hypothesis.no_assessment"));
    assert!(single.holds(&Trigger {
        gap_rule: None,
        case_fact: None,
        record_kind: Some(RecordKind::Question),
    }));

    let busy = context(vec![
        case("evaluate"),
        question,
        hypothesis(1, "active"),
        hypothesis(2, "proposed"),
        source(1),
        source(2),
        option(1, "eligible"),
        option(2, "proposed"),
        measured,
    ]);
    assert_eq!(
        busy.case_facts,
        BTreeSet::from([
            CaseFact::CompetingHypotheses,
            CaseFact::MultipleOptions,
            CaseFact::MultipleSources,
            CaseFact::MeasuredObservation,
        ])
    );
    assert!(!busy.case_facts.contains(&CaseFact::Stalled));
}

#[test]
fn lines_and_close_matches_are_stable() {
    let general = crate::method_pack::builtin_general();
    assert_eq!(
        trigger_line(general.card("handoff").unwrap()),
        "on request only"
    );
    assert_eq!(
        trigger_line(general.card("stalled").unwrap()),
        "case fact stalled"
    );
    let names: Vec<&str> = general
        .pack()
        .cards
        .iter()
        .map(|card| card.name.as_str())
        .collect();
    assert_eq!(close_matches("blindspt", names.clone())[0], "blindspot");
    assert_eq!(close_matches("Review", names.clone())[0], "review");
    assert!(close_matches("zzzzzzzzzz", names).is_empty());
    assert!(stage_rank("frame") < stage_rank("close"));
    assert!(stage_rank("close") < stage_rank("unlisted"));
}
