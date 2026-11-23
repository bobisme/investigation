//! CLI adapter for deterministic, read-only gap ranking.

use investigation_core::gap_page::GapFilter;
use investigation_core::gap_rules::enumerate_gaps;
use investigation_core::guidance::{self, GUIDANCE_RULE_VERSION, TriggerContext};
use investigation_core::next_rank::{NextResponse, rank_next};
use investigation_store::Store;
use serde_json::{Value, json};

use crate::{
    Global, Output, display_argv,
    frontier::snapshot_argv,
    selection::{select_case, workspace_argv},
};
use investigation_core::catalog::standard_catalog;
use investigation_core::wire::Failure;

pub(crate) fn run(store: &mut Store, global: &Global, limit: usize) -> Result<Output, Failure> {
    let case_id = select_case(store, global.case.as_deref())?;
    let replay = store.case(case_id)?;
    let inventory = enumerate_gaps(&replay.state, &replay.snapshot, case_id)?;
    let response = rank_next(&replay.state, &inventory, limit.min(3));
    let mut value = serde_json::to_value(&response).expect("next response serializes");
    let workspace = workspace_argv(store.workspace());
    // Every query path reads this response's snapshot, even after new writes.
    let frontier =
        |filter: &GapFilter| snapshot_argv(&workspace, case_id, &response.snapshot, filter);
    value["frontier_argv"] = json!(frontier(&GapFilter::default()));
    for (index, entry) in response.not_ranked.iter().enumerate() {
        value["not_ranked"][index]["frontier_argv"] = json!(frontier(&entry.filter));
    }
    for (index, entry) in response.withheld.iter().enumerate() {
        value["withheld"][index]["frontier_argv"] = json!(frontier(&entry.filter));
    }
    for (index, recommendation) in response.recommendations.iter().enumerate() {
        if let Some(group) = &recommendation.group {
            value["recommendations"][index]["group"]["frontier_argv"] =
                json!(frontier(&group.filter));
        }
    }
    // Guidance comes from the built-in pack and the case's available pins only.
    let packs = store.method_packs(&replay.state, case_id, None)?;
    let mode = TriggerContext::evaluate(&replay.state, case_id, &inventory).mode;
    value["guidance_rule_version"] = json!(GUIDANCE_RULE_VERSION);
    let unavailable: Vec<_> = packs.iter().filter(|pack| !pack.is_available()).collect();
    if !unavailable.is_empty() {
        value["unavailable_method_packs"] = json!(unavailable);
    }
    let catalog = standard_catalog();
    // `get_mut`, not indexing: indexing a missing key would insert a null.
    if let Some(alternatives) = value
        .get_mut("stop_or_defer")
        .and_then(|stop| stop.get_mut("alternatives"))
        .and_then(Value::as_array_mut)
    {
        for alternative in alternatives {
            if let Some(operation) = alternative["operation"].as_str()
                && catalog.available_operation(operation).is_ok()
            {
                let mut schema_argv = workspace.clone();
                schema_argv.extend([
                    "schema".into(),
                    operation.into(),
                    "--format".into(),
                    "json".into(),
                ]);
                alternative["schema_argv"] = json!(schema_argv);
            }
        }
    }
    for item in value["recommendations"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        if let (Some(id), Some(revision)) = (
            item["subject_refs"][0]["id"].as_str(),
            item["subject_refs"][0]["revision"].as_u64(),
        ) {
            let mut argv = workspace.clone();
            argv.extend([
                "show".into(),
                id.into(),
                "--case".into(),
                case_id.to_string(),
                "--at".into(),
                revision.to_string(),
                "--depth".into(),
                "full".into(),
                "--format".into(),
                "json".into(),
            ]);
            item["inspect_argv"] = json!(argv);
        }
        if let Some(operation) = item["schema_operation"].as_str()
            && catalog.available_operation(operation).is_ok()
        {
            let mut schema_argv = workspace.clone();
            schema_argv.extend([
                "schema".into(),
                operation.into(),
                "--format".into(),
                "json".into(),
            ]);
            item["schema_argv"] = json!(schema_argv);
        }
        // Each stall move that records names its schema; a read names `show`.
        for stall_move in item
            .get_mut("stall")
            .and_then(|stall| stall.get_mut("moves"))
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            let mut argv = workspace.clone();
            if let Some(operation) = stall_move["operation"].as_str() {
                if catalog.available_operation(operation).is_err() {
                    continue;
                }
                argv.extend(["schema".into(), operation.into()]);
                argv.extend(["--format".into(), "json".into()]);
                stall_move["schema_argv"] = json!(argv);
            } else if stall_move["kind"] == "inspect_raw_data"
                && let Some(target) = stall_move["target_refs"][0]["id"].as_str()
            {
                argv.extend(["show".into(), target.into(), "--case".into()]);
                argv.extend([case_id.to_string(), "--format".into(), "json".into()]);
                stall_move["inspect_argv"] = json!(argv);
            }
        }
        // At most one card per recommendation, chosen by the rule that produced it.
        if let Some(card) = item["rule_id"]
            .as_str()
            .and_then(|rule| guidance::card_for_rule(&packs, mode, rule))
        {
            let mut guide_argv = workspace.clone();
            guide_argv.extend([
                "--case".into(),
                case_id.to_string(),
                "guide".into(),
                card.card.clone(),
                "--pack".into(),
                card.pack_id.clone(),
                "--format".into(),
                "json".into(),
            ]);
            item["guide"] = json!(card);
            item["guide"]["argv"] = json!(guide_argv);
        }
        item["result_caveat"] = json!(
            "This route reads recorded state only; it does not execute or validate an external action."
        );
    }
    let text = render_text(&value, &response);
    Ok(Output::new(value, text))
}

fn argv_text(value: &Value) -> Option<String> {
    let argv = value
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Some(display_argv(&argv))
}

fn render_text(value: &Value, response: &NextResponse) -> String {
    let mut text = format!(
        "case {}  snapshot {}  rules {}\n",
        response.case_id, response.snapshot.sequence, response.rule_version
    );
    for recommendation in &response.recommendations {
        text.push_str(&format!(
            "{}. {}\n   {}\n   unresolved inputs: {}\n",
            recommendation.rank,
            recommendation.purpose,
            recommendation.action_kind,
            if recommendation.unresolved_fields.is_empty() {
                "none recorded".to_owned()
            } else {
                recommendation.unresolved_fields.join(", ")
            }
        ));
        if let Some(argv) =
            value["recommendations"][recommendation.rank - 1]["inspect_argv"].as_array()
        {
            let argv = argv
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            text.push_str(&format!("   Inspect: {}\n", display_argv(&argv)));
        }
        if let Some(argv) =
            value["recommendations"][recommendation.rank - 1]["schema_argv"].as_array()
        {
            let argv = argv
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            text.push_str(&format!("   Schema: {}\n", display_argv(&argv)));
        }
        let guide = &value["recommendations"][recommendation.rank - 1]["guide"];
        if let (Some(card), Some(pack), Some(summary)) = (
            guide["card"].as_str(),
            guide["pack_id"].as_str(),
            guide["summary"].as_str(),
        ) {
            text.push_str(&format!("   Guide: {card} ({pack})  {summary}\n"));
        }
        let decision_value = &recommendation.decision_value;
        text.push_str(&format!(
            "   Value: {}  {}\n",
            decision_value.decision_effect, decision_value.basis
        ));
        if let Some(stall) = &recommendation.stall {
            let thresholds = stall
                .thresholds
                .iter()
                .map(|threshold| format!("{} {}", threshold.name, threshold.value))
                .collect::<Vec<_>>()
                .join(", ");
            text.push_str(&format!(
                "   Stall: {} at sequences {} (threshold {thresholds})\n",
                stall.pattern,
                stall
                    .journal_items
                    .iter()
                    .map(|item| item.sequence.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            for stall_move in &stall.moves {
                let kind = serde_json::to_value(stall_move.kind)
                    .ok()
                    .and_then(|kind| kind.as_str().map(str::to_owned))
                    .unwrap_or_default();
                text.push_str(&format!(
                    "   Move {kind}{}: {}\n",
                    stall_move
                        .operation
                        .as_deref()
                        .map(|operation| format!(" ({operation})"))
                        .unwrap_or_default(),
                    stall_move.purpose
                ));
            }
        }
        if let Some(narrower) = &decision_value.narrower_value {
            text.push_str(&format!(
                "   Every recorded outcome leaves the disposition unchanged; ranked by its narrower value: {narrower}\n"
            ));
        }
        if !recommendation.outcome_branches.is_empty() {
            for branch in &recommendation.outcome_branches {
                // The outcome is a recorded prediction: quoted data, never
                // part of the tool's sentence.
                text.push_str(&format!(
                    "   {} → {}\n",
                    investigation_core::authored_text::quoted(
                        "outcome",
                        investigation_core::authored_text::AUTHORED_DATA,
                        &branch.outcome
                    ),
                    branch.consequence
                ));
            }
        }
        if let Some(group) = &recommendation.group {
            text.push_str(&format!(
                "   Group: {} gaps of {} read criterion {} ({} ranked, {} withheld)\n",
                group.gap_count,
                group.rule_id,
                group.criterion_ref.id,
                group.ranked_count,
                group.withheld_count
            ));
            if let Some(route) = argv_text(
                &value["recommendations"][recommendation.rank - 1]["group"]["frontier_argv"],
            ) {
                text.push_str(&format!("   Group gaps: {route}\n"));
            }
        }
    }
    if response.remaining_recommendations > 0 {
        text.push_str(&format!(
            "{} more ranked recommendation(s); {} ranked gaps in all.\n",
            response.remaining_recommendations, response.ranked_gap_total
        ));
    }
    for (index, withheld) in response.withheld.iter().enumerate() {
        text.push_str(&format!(
            "Withheld: {} soft-criterion gap(s) for disqualified option {}\n",
            withheld.gap_count, withheld.option_ref.id
        ));
        if let Some(route) = argv_text(&value["withheld"][index]["frontier_argv"]) {
            text.push_str(&format!("   {route}\n"));
        }
    }
    if let Some(stop) = &response.stop_or_defer {
        text.push_str(&format!("Stop or defer: {}\n", stop.reason));
        for alternative in &stop.alternatives {
            text.push_str(&format!(
                "   {}  {}\n",
                alternative.operation, alternative.purpose
            ));
        }
    }
    text.push_str(&format!(
        "Counts: source {}, external {}, quality-blocked {}, stale {}, deferred {}\n",
        response.counts.requires_source,
        response.counts.requires_external_action,
        response.counts.blocked_by_quality,
        response.counts.stale_due_to_revision,
        response.counts.deferred,
    ));
    for (index, entry) in response.not_ranked.iter().enumerate() {
        if entry.count > 0
            && let Some(route) = argv_text(&value["not_ranked"][index]["frontier_argv"])
        {
            let status = serde_json::to_value(entry.status)
                .ok()
                .and_then(|status| status.as_str().map(str::to_owned))
                .unwrap_or_default();
            text.push_str(&format!("Not ranked ({status}): {route}\n"));
        }
    }
    if let Some(route) = argv_text(&value["frontier_argv"]) {
        text.push_str(&format!("Frontier at this snapshot: {route}\n"));
    }
    for pack in value["unavailable_method_packs"]
        .as_array()
        .into_iter()
        .flatten()
    {
        text.push_str(&format!(
            "Method pack {} {} is pinned but unavailable ({}); its cards are not used.\n",
            pack["pack_id"].as_str().unwrap_or(""),
            pack["version"].as_str().unwrap_or(""),
            pack["unavailable_reason"].as_str().unwrap_or(""),
        ));
    }
    text.push_str("No detected gaps means no gap was emitted by this rule version; it does not mean the investigation is complete.");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use investigation_core::{identity::Id, wire::Snapshot};

    #[test]
    fn text_output_is_explicit_about_unknowns_and_not_completion() {
        let id: Id = "case_019acd16-0000-7000-8000-000000000001".parse().unwrap();
        let response = NextResponse {
            schema_version: "inv.next.v1".into(),
            snapshot: Snapshot {
                sequence: 3,
                head_hash: format!("sha256:{}", "a".repeat(64)).parse().unwrap(),
            },
            rule_version: investigation_core::next_rank::NEXT_RULE_VERSION.into(),
            case_id: id,
            recommendations: vec![],
            counts: investigation_core::next_rank::GapCounts {
                ready_to_analyze: 0,
                requires_source: 0,
                requires_external_action: 0,
                blocked_by_quality: 1,
                stale_due_to_revision: 0,
                deferred: 1,
                resolved: 0,
            },
            remaining_recommendations: 0,
            ranked_gap_total: 0,
            not_ranked: vec![],
            withheld: vec![],
            stop_or_defer: None,
            limits: vec![],
        };
        let rendered = render_text(&serde_json::to_value(&response).unwrap(), &response);
        assert!(rendered.contains("quality-blocked 1"));
        assert!(rendered.contains("not mean the investigation is complete"));
    }
}
