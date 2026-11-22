//! CLI adapter for deterministic, read-only gap ranking.

use investigation_core::gap_rules::{enumerate_gaps, rank_next};
use investigation_store::Store;
use serde_json::{Value, json};

use crate::{
    Global, Output, display_argv,
    selection::{select_case, workspace_argv},
};
use investigation_core::catalog::standard_catalog;
use investigation_core::wire::Failure;

pub(crate) fn run(store: &mut Store, global: &Global, limit: usize) -> Result<Output, Failure> {
    let case_id = select_case(store, global.case.as_deref())?;
    let replay = store.case(case_id)?;
    let inventory = enumerate_gaps(&replay.state, &replay.snapshot, case_id)?;
    let response = rank_next(&inventory, limit.min(3));
    let mut value = serde_json::to_value(&response).expect("next response serializes");
    let workspace = workspace_argv(store.workspace());
    let catalog = standard_catalog();
    for item in value["recommendations"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        if let Some(id) = item["subject_refs"][0]["id"].as_str() {
            let mut argv = workspace.clone();
            argv.extend([
                "show".into(),
                id.into(),
                "--case".into(),
                case_id.to_string(),
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
        item["result_caveat"] = json!(
            "This route reads recorded state only; it does not execute or validate an external action."
        );
    }
    let text = render_text(&value, &response);
    Ok(Output::new(value, text))
}

fn render_text(value: &Value, response: &investigation_core::gap_rules::NextResponse) -> String {
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
        if !recommendation.outcome_branches.is_empty() {
            for branch in &recommendation.outcome_branches {
                text.push_str(&format!("   {} → {}\n", branch.outcome, branch.consequence));
            }
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
        let response = investigation_core::gap_rules::NextResponse {
            schema_version: "inv.next.v1".into(),
            snapshot: Snapshot {
                sequence: 3,
                head_hash: format!("sha256:{}", "a".repeat(64)).parse().unwrap(),
            },
            rule_version: "slice-next.v1".into(),
            case_id: id,
            recommendations: vec![],
            counts: investigation_core::gap_rules::GapCounts {
                ready_to_analyze: 0,
                requires_source: 0,
                requires_external_action: 0,
                blocked_by_quality: 1,
                stale_due_to_revision: 0,
                deferred: 1,
                resolved: 0,
            },
            remaining_recommendations: 0,
            limits: vec![],
        };
        let rendered = render_text(&serde_json::to_value(&response).unwrap(), &response);
        assert!(rendered.contains("quality-blocked 1"));
        assert!(rendered.contains("not mean the investigation is complete"));
    }
}
