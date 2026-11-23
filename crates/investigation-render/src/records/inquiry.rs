//! Inquiry notes: the plan, whether it was frozen before results, its
//! predictions per rival, and the runs it produced.

use std::fmt::Write as _;

use investigation_core::identity::Kind;
use serde_json::Value;

use super::{Facts, citing, refs};
use crate::{
    RenderError,
    evidence::{MAX_LISTED, or_not_recorded, prose, revision_link},
    note::text_at,
    text::inline_code,
};

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    let body = &f.record.body;
    let mode = text_at(body, "inquiry.mode").unwrap_or("unknown");
    let lifecycle = text_at(body, "lifecycle").unwrap_or("unknown");
    let _ = writeln!(
        f.out,
        "- Mode: {}; lifecycle {}{}",
        inline_code(&super::wire(mode)),
        inline_code(&super::wire(lifecycle)),
        if lifecycle == "frozen" {
            ": the plan was fixed before its results and cannot be edited"
        } else {
            ""
        }
    );
    f.text("Purpose", "purpose");
    f.text("Method", "method");
    f.reference_present("Question", "question_ref")?;
    f.word_present("Plan digest", "plan_digest");
    f.list(
        "Desired observations",
        "desired_observations",
        "none recorded",
    );
    f.list("Controls", "controls", "none recorded");
    f.list("Limits", "limits", "none recorded");
    f.list("Warnings", "warnings", "none");
    f.text_present("Prior exposure", "prior_exposure");
    if mode == "confirmatory" {
        let plan = &body["inquiry"]["confirmatory"];
        let _ = writeln!(
            f.out,
            "- Evidence holdout boundary: {}",
            or_not_recorded(text_at(plan, "evidence_holdout_boundary"))
        );
        let _ = writeln!(
            f.out,
            "- Prior exposure: {}",
            or_not_recorded(text_at(plan, "prior_exposure"))
        );
        let _ = writeln!(
            f.out,
            "- Stop-analysis rule: {}",
            or_not_recorded(text_at(plan, "stop_analysis_rule"))
        );
        f.heading("Predictions");
        let predictions: Vec<&Value> = plan["predictions"]
            .as_array()
            .into_iter()
            .flatten()
            .collect();
        if predictions.is_empty() {
            f.out.push_str("No prediction is recorded.\n");
        }
        for prediction in predictions.iter().take(MAX_LISTED) {
            let rival = match refs(prediction.get("rival_ref")).first() {
                Some(reference) => revision_link(f.notes, *reference)?,
                None => "an unresolved rival".to_owned(),
            };
            let _ = writeln!(
                f.out,
                "- If {rival} holds: {}",
                prose(text_at(prediction, "predicted_outcome").unwrap_or_default())
            );
        }
    }

    f.heading("Runs");
    let runs: Vec<_> = citing(f.notes, f.record, &["intent.plan_ref"])
        .into_iter()
        .filter(|(record, _)| record.header.id.kind() == Kind::Run)
        .collect();
    f.backlinks(&runs, "No run is recorded under this plan.", |_, run| {
        Ok(format!(
            ": execution {}, evidence validity {}",
            inline_code(&super::wire(
                text_at(&run.body, "execution_status").unwrap_or("unknown")
            )),
            inline_code(&super::wire(
                text_at(&run.body, "evidence_validity").unwrap_or("unknown")
            ))
        ))
    })
}
