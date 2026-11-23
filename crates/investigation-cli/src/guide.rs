//! `guide NAME` and `method list`: read method cards from the built-in pack
//! and from packs pinned to the selected case.
//!
//! Both commands work without a workspace, using the built-in pack only. Card
//! text comes only from the binary or from a pin resolved against its retained
//! bytes; a pack file on disk is never read here, so a pack that is present but
//! not adopted cannot be shown as guidance.

use investigation_core::{
    PROGRAM_NAME,
    case::CaseMode,
    gap_rules::enumerate_gaps,
    guidance::{
        self, CONTENT_TRUST, GUIDANCE_RULE_VERSION, TriggerContext, close_matches, guidance_packs,
        stage_rank, trigger_line,
    },
    identity::Id,
    method_pack::{Card, LoadedPack},
    method_pack_ops::{ResolvedPack, resolve},
    wire::{ErrorCode, Failure},
};
use investigation_store::Store;
use serde_json::{Value, json};

use crate::{
    Global, Output,
    selection::{open_workspace, select_case, workspace_argv},
};

pub(crate) const GUIDE_AFTER_HELP: &str = "\
Prints one method card with its pack ID, version and digest. Cards come from the built-in pack or from a pack pinned to the case.
Examples:
  investigation guide blindspot --format json
  investigation --case <ID> guide NAME --pack ID --format json";

#[derive(clap::Args)]
pub(crate) struct GuideArgs {
    /// Card name, for example blindspot.
    #[arg(value_name = "NAME")]
    name: String,
    /// Read the card from this pack: general or a pack pinned to the case.
    #[arg(long, value_name = "ID")]
    pack: Option<String>,
}

#[derive(clap::Subcommand)]
pub(crate) enum MethodCommand {
    /// List built-in and pinned packs and their cards by stage.
    #[command(after_help = "Example: investigation method list --format json")]
    List,
}

/// The packs that may supply guidance, and the case they were resolved for.
struct Scope {
    case: Option<Id>,
    mode: Option<CaseMode>,
    packs: Vec<ResolvedPack>,
    context: Option<TriggerContext>,
    workspace_argv: Vec<String>,
}

fn scope(global: &Global) -> Result<Scope, Failure> {
    let builtin = || Scope {
        case: None,
        mode: None,
        packs: resolve(&[], |_| None),
        context: None,
        workspace_argv: vec![PROGRAM_NAME.to_owned()],
    };
    let workspace = match open_workspace(global.workspace.as_deref()) {
        Ok(workspace) => workspace,
        Err(failure)
            if global.workspace.is_none()
                && global.case.is_none()
                && failure.error.code == ErrorCode::WorkspaceRequired =>
        {
            return Ok(builtin());
        }
        Err(failure) => return Err(failure),
    };
    let mut store = Store::open(workspace);
    let argv = workspace_argv(store.workspace());
    let case = match global.case.as_deref() {
        Some(selector) => Some(select_case(&mut store, Some(selector))?),
        None => match store.list_cases()?.as_slice() {
            [head] => Some(head.case_id),
            _ => None,
        },
    };
    let Some(case) = case else {
        return Ok(Scope {
            workspace_argv: argv,
            ..builtin()
        });
    };
    let replay = store.case(case)?;
    let packs = store.method_packs(&replay.state, case, None)?;
    let inventory = enumerate_gaps(&replay.state, &replay.snapshot, case)?;
    let context = TriggerContext::evaluate(&replay.state, case, &inventory);
    Ok(Scope {
        case: Some(case),
        mode: context.mode,
        packs,
        context: Some(context),
        workspace_argv: argv,
    })
}

fn unavailable(packs: &[ResolvedPack]) -> Vec<Value> {
    packs
        .iter()
        .filter(|pack| !pack.is_available())
        .map(|pack| {
            json!({
                "pack_id": pack.pack_id, "version": pack.version, "digest": pack.digest,
                "origin": pack.origin, "status": pack.status,
                "unavailable_reason": pack.unavailable_reason,
            })
        })
        .collect()
}

fn pack_identity(pack: &ResolvedPack) -> Value {
    json!({
        "pack_id": pack.pack_id, "version": pack.version, "digest": pack.digest, "origin": pack.origin,
    })
}

fn case_argv(scope: &Scope, tail: &[&str]) -> Vec<String> {
    let mut argv = scope.workspace_argv.clone();
    if let Some(case) = scope.case {
        argv.extend(["--case".to_owned(), case.to_string()]);
    }
    argv.extend(tail.iter().map(|part| (*part).to_owned()));
    argv
}

pub(crate) fn show(args: GuideArgs, global: &Global) -> Result<Output, Failure> {
    let scope = scope(global)?;
    let available = guidance_packs(&scope.packs);
    if let Some(wanted) = args.pack.as_deref()
        && !available.iter().any(|(pack, _)| pack.pack_id == wanted)
    {
        let pinned = scope.packs.iter().find(|pack| pack.pack_id == wanted);
        let mut failure = Failure::new(
            ErrorCode::UnknownReference,
            match pinned {
                Some(_) => "That pack is pinned to the case but unavailable, so its cards are not shown as guidance.".to_owned(),
                None => "No built-in or pinned pack has that ID. A pack file guides a case only after method_pack.adopt pins it.".to_owned(),
            },
        )
        .at("--pack");
        failure.error.actual = Some(json!({
            "available_packs": available.iter().map(|(pack, _)| pack_identity(pack)).collect::<Vec<_>>(),
            "unavailable_packs": unavailable(&scope.packs),
        }));
        return Err(failure.with_argv(case_argv(&scope, &["method", "list", "--format", "json"])));
    }
    let candidates: Vec<&(&ResolvedPack, &LoadedPack)> = available
        .iter()
        .filter(|(pack, _)| {
            args.pack
                .as_deref()
                .is_none_or(|wanted| pack.pack_id == wanted)
        })
        .collect();
    let matches: Vec<(&ResolvedPack, &LoadedPack, &Card)> = candidates
        .iter()
        .filter_map(|(pack, loaded)| loaded.card(&args.name).map(|card| (*pack, *loaded, card)))
        .collect();
    let Some((pack, loaded, card)) = matches.first().copied() else {
        let suggestions = close_matches(
            &args.name,
            candidates
                .iter()
                .flat_map(|(_, loaded)| loaded.pack().cards.iter().map(|card| card.name.as_str())),
        );
        let mut failure = Failure::new(
            ErrorCode::UnknownReference,
            "No guidance card has that name in the available packs.",
        )
        .at("/name");
        failure.error.actual = Some(json!({"close_matches": suggestions}));
        let argv = match suggestions.first() {
            Some(best) => case_argv(&scope, &["guide", best, "--format", "json"]),
            None => case_argv(&scope, &["method", "list", "--format", "json"]),
        };
        return Err(failure.with_argv(argv));
    };
    let holds_now = scope.context.as_ref().map(|context| {
        card.triggers
            .iter()
            .filter(|trigger| context.holds(trigger))
            .collect::<Vec<_>>()
    });
    let value = json!({
        "guidance_rule_version": GUIDANCE_RULE_VERSION,
        "case_id": scope.case,
        "guide": {
            "content_trust": CONTENT_TRUST,
            "pack": pack_identity(pack),
            "card": card,
            "trigger_line": trigger_line(card),
            "applies_to_case_mode": scope.mode.map(|mode| guidance::applies(loaded, card, Some(mode))),
            "triggers_holding_now": holds_now,
        },
        "also_in_packs": matches[1..].iter().map(|(other, _, _)| pack_identity(other)).collect::<Vec<_>>(),
        "unavailable_packs": unavailable(&scope.packs),
    });
    Ok(Output::success(
        "guide_shown",
        value,
        render_card(pack, card),
    ))
}

fn render_card(pack: &ResolvedPack, card: &Card) -> String {
    let mut text = format!(
        "{}  {}\npack {} {}  {}\nstage {}  {}\n\nWhen: {}\nRecord: {}\n",
        card.name,
        card.title,
        pack.pack_id,
        pack.version,
        pack.digest,
        card.stage,
        trigger_line(card),
        card.body.when,
        card.body.record,
    );
    if let Some(mistake) = &card.body.mistake {
        text.push_str(&format!("Mistake: {mistake}\n"));
    }
    text.push_str(&format!("Skip: {}\n", card.body.skip));
    if !card.operations.is_empty() {
        text.push_str(&format!("Operations: {}\n", card.operations.join(", ")));
    }
    text.push_str("This is method guidance, not case evidence; it does not run anything.");
    text
}

pub(crate) fn method(command: MethodCommand, global: &Global) -> Result<Output, Failure> {
    let MethodCommand::List = command;
    let scope = scope(global)?;
    let available = guidance_packs(&scope.packs);
    let mut cards: Vec<(&ResolvedPack, &LoadedPack, usize, &Card)> = available
        .iter()
        .flat_map(|(pack, loaded)| {
            loaded
                .pack()
                .cards
                .iter()
                .enumerate()
                .map(move |(index, card)| (*pack, *loaded, index, card))
        })
        .collect();
    // Built-in pack first in the listing, then pins in adoption order.
    let pack_rank = |pack: &ResolvedPack| {
        scope
            .packs
            .iter()
            .position(|other| std::ptr::eq(other, pack))
            .unwrap_or(usize::MAX)
    };
    cards.sort_by_key(|(pack, _, index, card)| (stage_rank(&card.stage), pack_rank(pack), *index));
    let mut stages: Vec<Value> = Vec::new();
    let mut text = String::from("Method packs\n");
    for pack in &scope.packs {
        text.push_str(&format!(
            "  {} {}  {}  {}{}\n",
            pack.pack_id,
            pack.version,
            serde_json::to_value(pack.origin)
                .unwrap()
                .as_str()
                .unwrap_or(""),
            serde_json::to_value(pack.status)
                .unwrap()
                .as_str()
                .unwrap_or(""),
            pack.unavailable_reason
                .map(|reason| format!(
                    " ({})",
                    serde_json::to_value(reason).unwrap().as_str().unwrap_or("")
                ))
                .unwrap_or_default(),
        ));
    }
    for (pack, loaded, _, card) in &cards {
        if stages
            .last()
            .is_none_or(|stage| stage["stage"] != card.stage.as_str())
        {
            stages.push(json!({"stage": card.stage, "cards": []}));
            text.push_str(&format!("{}\n", card.stage));
        }
        let mut entry = json!({
            "name": card.name, "title": card.title, "summary": card.summary,
            "pack_id": pack.pack_id, "pack_version": pack.version, "origin": pack.origin,
            "triggers": trigger_line(card), "on_request_only": card.triggers.is_empty(),
        });
        if let Some(context) = &scope.context {
            entry["applies_to_case_mode"] = json!(guidance::applies(loaded, card, scope.mode));
            entry["triggered_now"] =
                json!(card.triggers.iter().any(|trigger| context.holds(trigger)));
        }
        stages.last_mut().expect("stage pushed")["cards"]
            .as_array_mut()
            .expect("cards array")
            .push(entry);
        text.push_str(&format!(
            "  {} ({})  {}  [{}]\n",
            card.name,
            pack.pack_id,
            card.summary,
            trigger_line(card)
        ));
    }
    text.push_str("Read one card with: investigation guide NAME [--pack ID]");
    let packs: Vec<Value> = scope
        .packs
        .iter()
        .map(|pack| {
            let mut value = serde_json::to_value(pack).expect("resolved pack serializes");
            if let Some(loaded) = &pack.loaded {
                value["title"] = json!(loaded.pack().title);
                value["card_count"] = json!(loaded.pack().cards.len());
            }
            value
        })
        .collect();
    let value = json!({
        "guidance_rule_version": GUIDANCE_RULE_VERSION,
        "content_trust": CONTENT_TRUST,
        "case_id": scope.case,
        "case_mode": scope.mode,
        "packs": packs,
        "unavailable_packs": unavailable(&scope.packs),
        "stages": stages,
    });
    Ok(Output::success("method_list", value, text))
}
