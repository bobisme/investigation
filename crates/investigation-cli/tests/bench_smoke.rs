//! The §10.5 benchmark harness works end to end on tiny fixtures: it
//! generates valid workspaces, finds grouped `next` output complete against
//! its ground truth, times every measured command and writes a report with
//! the environment and targets. Timings are not asserted; `just bench` runs
//! the real sizes on a release build.

use std::path::PathBuf;

use investigation_bench::{Config, REPORT_VERSION, run};

#[test]
fn smoke_run_reports_every_measurement_with_environment_and_targets() {
    let temporary = tempfile::tempdir().unwrap();
    let config = Config::smoke(
        PathBuf::from(env!("CARGO_BIN_EXE_investigation")),
        temporary.path().join("fixtures"),
    );
    let report = run(&config).unwrap();
    assert_eq!(report["schema_version"], REPORT_VERSION);
    assert_eq!(report["complete"], true, "{}", report["completeness"]);

    let fixtures: Vec<_> = report["fixtures"].as_array().unwrap().iter().collect();
    let names: Vec<_> = fixtures
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["exhaustive_small", "sparse_choice", "large"]);
    for fixture in &fixtures {
        assert_eq!(fixture["check"]["ok"], true, "{fixture}");
        assert!(fixture["objects"].as_u64().unwrap() > 0);
        assert!(fixture["relations"].as_u64().unwrap() > 0);
    }
    for completeness in report["completeness"].as_array().unwrap() {
        assert!(
            !completeness["groups_checked"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    assert!(
        report["completeness"][0]["cli_full_frontier"]["pages"]
            .as_u64()
            .unwrap()
            >= 1
    );

    let environment = &report["environment"];
    for key in [
        "cpu_model",
        "logical_cpus",
        "binary_profile",
        "load_average_1_5_15_at_start",
    ] {
        assert!(environment.get(key).is_some(), "{key}");
    }
    assert!(report["method"]["trial_order"].is_string());

    let measurements = report["measurements"].as_array().unwrap();
    for fixture in ["sparse_choice", "large"] {
        for name in [
            "brief",
            "next_top_three",
            "frontier_first_page",
            "record_single_operation",
            "index_rebuild",
            "vault_render_first",
            "vault_render_unchanged",
            "vault_render_after_one_mutation",
        ] {
            let item = measurements
                .iter()
                .find(|item| item["fixture"] == fixture && item["measurement"] == name)
                .unwrap_or_else(|| panic!("{fixture} {name}"));
            let n = item["stats"]["n"].as_u64().unwrap();
            assert!(n >= 1, "{item}");
            assert_eq!(
                item["stats"]["samples_ms"].as_array().unwrap().len() as u64,
                n
            );
            assert!(item["stats"]["p95_ms"].as_f64().unwrap() > 0.0);
            assert!(item["stopped_by_time_budget"].is_boolean(), "{item}");
            assert!(n <= item["requested_samples"].as_u64().unwrap());
            assert!(["met", "missed", "no_target"].contains(&item["verdict"].as_str().unwrap()));
        }
    }
    let targeted = measurements
        .iter()
        .filter(|item| item["target_ms"].is_number())
        .count();
    assert!(targeted >= 8);
    for profile in report["profiles"].as_array().unwrap() {
        assert!(profile["pareto_ms"]["n"].as_u64().unwrap() >= 1);
    }
}
