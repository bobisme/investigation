//! The language-neutral recording conformance suite (fixtures/conformance/v1)
//! run through scripts/run-conformance.py against the built binary. The
//! reference producer and the Rust and Python examples pass every case and
//! record equal normalized state. The deliberately non-idempotent
//! producer fails exactly the lost-response cases, so its defect is what the
//! suite detects.

use std::{path::Path, process::Command};

use serde_json::Value;

fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
}

fn producer(script: &str) -> String {
    let path = repository().join("scripts/conformance").join(script);
    format!("python3 {}", path.display())
}

/// Run the suite against `producers`; return the exit status and the report.
fn run_suite(producers: &[String], cases: &[&str]) -> (bool, Value) {
    let mut command = Command::new("python3");
    command
        .arg(repository().join("scripts/run-conformance.py"))
        .arg("--bin")
        .arg(env!("CARGO_BIN_EXE_investigation"))
        .arg("--json")
        .current_dir(repository());
    for producer in producers {
        command.arg("--producer").arg(producer);
    }
    for case in cases {
        command.arg("--case").arg(case);
    }
    let output = command
        .output()
        .expect("python3 runs the conformance runner");
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "runner printed no report ({error}): {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.success(), report)
}

fn failed_cases(report: &Value) -> Vec<String> {
    report["producers"][0]["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["passed"] != true)
        .map(|case| case["case"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn suite_maps_every_plan_item_to_a_case() {
    let output = Command::new("python3")
        .arg(repository().join("scripts/run-conformance.py"))
        .arg("--bin")
        .arg(env!("CARGO_BIN_EXE_investigation"))
        .arg("--validate-suite")
        .current_dir(repository())
        .output()
        .expect("python3 runs the conformance runner");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let suite: Value = serde_json::from_slice(
        &std::fs::read(repository().join("fixtures/conformance/v1/suite.json")).unwrap(),
    )
    .unwrap();
    let items: Vec<&str> = suite["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    for required in [
        "equivalent-hashes-ids",
        "idempotent-begin-append-attach-finalize",
        "lost-responses",
        "truncated-streams",
        "changed-manifest",
        "exact-acquisition-mapping",
        "incomplete-controls",
        "namespaced-metadata",
        "protocol-version",
        "crash-restart-recovery",
        "positions",
        "attachments",
        "missing-attachments",
        "interrupted-runs",
        "late-items",
        "finalize-retry",
        "recover",
        "producer-name-attempt-identity",
    ] {
        assert!(items.contains(&required), "no coverage item {required}");
    }
}

/// The Rust example's adapter binary. `cargo test --workspace` builds every
/// package's binaries before any test runs, so it sits beside this package's.
fn rust_example() -> String {
    let binary = Path::new(env!("CARGO_BIN_EXE_investigation")).with_file_name(format!(
        "example-rust-producer{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        binary.is_file(),
        "{} is missing: run `cargo build -p example-rust-producer` or test the whole workspace",
        binary.display()
    );
    binary.display().to_string()
}

#[test]
fn reference_and_both_examples_pass_and_record_equal_state() {
    let python = format!(
        "python3 -B {}",
        repository()
            .join("examples/python-producer/producer.py")
            .display()
    );
    let producers = [producer("reference_producer.py"), python, rust_example()];
    let (passed, report) = run_suite(&producers, &[]);
    assert!(passed, "{}", serde_json::to_string_pretty(&report).unwrap());
    for entry in report["producers"].as_array().unwrap() {
        assert_eq!(entry["describe"], Value::Array(Vec::new()));
        assert_eq!(entry["cases"].as_array().unwrap().len(), 20);
        assert!(
            entry["cases"]
                .as_array()
                .unwrap()
                .iter()
                .all(|case| case["passed"] == true)
        );
    }
    // Each example is compared with the reference producer on every case.
    let equivalence = report["equivalence"].as_array().unwrap();
    assert_eq!(equivalence.len(), 40);
    assert!(equivalence.iter().all(|entry| entry["equal"] == true));
}

#[test]
fn non_idempotent_producer_fails_the_suite() {
    let (passed, report) = run_suite(&[producer("non_idempotent_producer.py")], &[]);
    assert!(
        !passed,
        "a producer that re-records on a lost response passed"
    );
    assert_eq!(report["passed"], false);
    assert_eq!(
        failed_cases(&report),
        [
            "lost-response-append",
            "lost-response-attach",
            "lost-response-begin",
            "lost-response-finalize",
        ]
    );
}

#[test]
fn two_producers_are_compared_case_by_case() {
    // The defect only shows on a lost response, so on these cases the two
    // producers must record equal normalized canonical state.
    let cases = ["baseline", "partial-evidence", "crash-resume-attach"];
    let (passed, report) = run_suite(
        &[
            producer("reference_producer.py"),
            producer("non_idempotent_producer.py"),
        ],
        &cases,
    );
    assert!(passed, "{}", serde_json::to_string_pretty(&report).unwrap());
    let equivalence = report["equivalence"].as_array().unwrap();
    assert_eq!(equivalence.len(), cases.len());
    assert!(equivalence.iter().all(|entry| entry["equal"] == true));
}
