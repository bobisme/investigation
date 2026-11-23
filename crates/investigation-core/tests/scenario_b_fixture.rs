//! Every Scenario B input parses with the real envelope and measurement types.

use std::path::PathBuf;

use investigation_core::{measurement_fields::MeasurementFields, producer_run::ProducerRun};

fn family() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/development/scenario-b-latency-regression")
}

#[test]
fn every_run_envelope_and_measurement_block_is_accepted() {
    let mut runs = 0;
    for variant in std::fs::read_dir(family()).unwrap() {
        let variant = variant.unwrap().path();
        let runs_dir = variant.join("runs");
        if !runs_dir.is_dir() {
            continue;
        }
        for run in std::fs::read_dir(runs_dir).unwrap() {
            let run = run.unwrap().path();
            let envelope: ProducerRun =
                serde_json::from_slice(&std::fs::read(run.join("envelope.json")).unwrap())
                    .unwrap_or_else(|e| panic!("{}: {e}", run.display()));
            envelope.validate().unwrap();
            let measurement: MeasurementFields =
                serde_json::from_slice(&std::fs::read(run.join("measurement.json")).unwrap())
                    .unwrap_or_else(|e| panic!("{}: {e}", run.display()));
            measurement.validate("/measurement").unwrap();
            runs += 1;
        }
    }
    assert_eq!(runs, 12);
}
