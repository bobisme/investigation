//! The vault load test (plan §12.5) works end to end on a tiny workspace:
//! both variants render, status sees the drift the test makes, the dry-run
//! reconcile plans it, and every measurement carries a sample count and a
//! peak-memory figure when GNU `time` is present. Timings are not asserted.
//!
//! Ignored by default because it fsyncs several hundred files. Run it with
//! `TMPDIR=/var/tmp cargo test -p investigation-cli --test vault_load_smoke -- --ignored`.
//! The real size is `just load-test DIR`; see docs/vault-load.md.

use std::path::PathBuf;

use investigation_bench::{
    fixtures::LoadShape,
    load::{LOAD_REPORT_VERSION, LoadConfig, run_load},
};

#[test]
#[ignore = "fsyncs hundreds of files; run with --ignored"]
fn smoke_load_run_reports_both_variants_and_the_drift_it_made() {
    let temporary = tempfile::tempdir().unwrap();
    let mut shape = LoadShape::smoke();
    shape.cases = 2;
    let report = run_load(&LoadConfig {
        binary: PathBuf::from(env!("CARGO_BIN_EXE_investigation")),
        directory: temporary.path().join("load"),
        shape,
        seed: 7,
        repetitions: 2,
    })
    .unwrap();
    assert_eq!(report["schema_version"], LOAD_REPORT_VERSION);
    assert_eq!(report["workspace"]["check_ok"], true);
    let variants = report["variants"].as_array().unwrap();
    let names: Vec<_> = variants
        .iter()
        .map(|v| v["variant"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["baseline", "viewer_1_15_1"]);
    let files = |variant: &serde_json::Value| {
        variant["measurements"][0]["extra"]["vault"]["files"]
            .as_u64()
            .unwrap()
    };
    assert!(
        files(&variants[1]) > files(&variants[0]),
        "the viewer variant adds Bases and the case map"
    );
    for variant in variants {
        let measurements = variant["measurements"].as_array().unwrap();
        for item in measurements {
            assert!(item["stats"]["n"].as_u64().unwrap() >= 1, "{item}");
        }
        let find = |name: &str| {
            measurements
                .iter()
                .find(|item| item["measurement"] == name)
                .unwrap()
        };
        assert_eq!(
            find("vault_status_with_drift")["extra"]["reported"]["drift_count"]
                .as_u64()
                .unwrap(),
            find("vault_status_edited_before_render")["extra"]["edited"]
                .as_u64()
                .unwrap()
                + find("vault_status_edited_before_render")["extra"]["deleted"]
                    .as_u64()
                    .unwrap(),
        );
    }
}
