//! CLI adapter for the read-only Brier calibration of resolved likelihoods.

use investigation_core::{
    classification::{AudiencePolicy, Classifier, Field},
    likelihood::{calibration, calibration_text, resolved_forecasts, unresolved},
    state::Record,
    wire::Failure,
};
use investigation_store::Store;
use serde_json::json;

use crate::{Global, Output, selection::select_case};

pub(crate) const AFTER_HELP: &str = "\
Scores each resolved likelihood with (probability - outcome)^2 and groups the
scores by the forecaster's self-reported model family and version. A mean is
shown only from the stated minimum of scored forecasts; smaller groups show
their count alone. Groups are listed by key, never ranked. Writes nothing.
Example: investigation --case <ID> calibration --format json
         investigation calibration --all-cases --format json";

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Read every case in the workspace instead of the selected case.
    #[arg(long)]
    all_cases: bool,
}

pub(crate) fn run(store: &mut Store, global: &Global, args: Args) -> Result<Output, Failure> {
    let replays = if args.all_cases {
        store.cases()?.into_values().collect::<Vec<_>>()
    } else {
        let case = select_case(store, global.case.as_deref())?;
        vec![store.case(case)?]
    };
    let policy = AudiencePolicy::default();
    let mut forecasts = Vec::new();
    let mut withheld = 0;
    let mut pending = 0;
    let mut snapshots = Vec::new();
    for replay in &replays {
        let classifier = Classifier::new(&replay.state);
        let visible = |record: &Record| classifier.may_show(record, Field::Title, &policy);
        let (found, hidden) = resolved_forecasts(&replay.state, &visible);
        forecasts.extend(found);
        withheld += hidden;
        pending += unresolved(&replay.state, &visible);
        snapshots.push(replay.snapshot.clone());
    }
    let view = calibration(&forecasts, pending, withheld);
    let mut value = serde_json::to_value(&view).expect("calibration serializes");
    value["scope"] = json!(if args.all_cases { "workspace" } else { "case" });
    value["snapshots"] = json!(snapshots);
    let text = calibration_text(&view);
    Ok(Output::new(value, text))
}
