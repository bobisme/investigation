//! Advisory measurement flags through the public binary: recording is never
//! blocked, the receipt warns, and `show`, `brief` and `frontier` carry the
//! same stable flag IDs.

use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut process = command.spawn().unwrap();
    if let Some(stdin) = stdin {
        let mut pipe = process.stdin.take().unwrap();
        pipe.write_all(&serde_json::to_vec(stdin).unwrap()).unwrap();
    }
    let output = process.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(exit), "{args:?}\n{stdout}");
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    serde_json::from_str(&stdout).expect(&stdout)
}

fn known(value: Value) -> Value {
    json!({"state": "known", "value": value})
}

fn flag_ids(view: &Value) -> Vec<String> {
    view["measurement_flags"]["flags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|flag| flag["flag_id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn flags_are_advisory_on_receipts_and_shown_in_every_read() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    run(root, &["init", ".", "--no-vault", "--json"], None, 0);
    let mut keys = 0;
    let mut submit = |case: &Value, op: &str, payload: Value| {
        keys += 1;
        let input = json!({
            "schema_version": "inv.command.v1", "command_id": format!("flags-{keys}"),
            "case_id": case, "actor": "agent:flags", "preconditions": [],
            "operations": [{"op": op, "payload": payload}],
        });
        run(root, &["record", "--input", "-", "--json"], Some(&input), 0)
    };
    let receipt = submit(
        &Value::Null,
        "case.create",
        json!({"title": "Flags", "mode": "record"}),
    );
    let case = receipt["created"][0]["id"].clone();
    let receipt = submit(
        &case,
        "observation.add",
        json!({
            "value": {"kind": "measurement", "value": "48.0", "unit": "ms"},
            "basis": {"kind": "assertion", "assertion": "Reported by the recorder.", "context": null},
            "validity": "valid", "quality": [], "limitations": [],
            "evidence_kind": "observational", "variables_changed": [],
            "measurement": {
                "statistic": known(json!({"kind": "quantile", "quantile": "0.99"})),
                "sample_count": known(json!(120)),
                "trial_order": known(json!("sequential")),
                "load_model": known(json!({
                    "loop": known(json!("closed")),
                    "timed_from_intended_send": known(json!(false))
                }))
            }
        }),
    );
    let observation = receipt["created"][0]["id"].as_str().unwrap().to_owned();
    // Three advisory flags, and the note that a recorded `valid` is the
    // recorder's own statement (ADR 0023).
    let (warnings, recorded): (Vec<_>, Vec<_>) = receipt["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .partition(|warning| {
            !warning
                .as_str()
                .unwrap()
                .contains("validity basis recorded_only")
        });
    assert_eq!(warnings.len(), 3, "{receipt}");
    assert_eq!(recorded.len(), 1, "{receipt}");
    assert!(warnings.iter().all(|warning| {
        warning
            .as_str()
            .unwrap()
            .contains("does not block recording")
    }));

    let expected = [
        "measurement.quantile_below_order_statistic_bound",
        "measurement.sequential_trial_order",
        "measurement.closed_loop_without_intended_send_timing",
    ];
    let shown = run(root, &["show", &observation, "--json"], None, 0);
    assert_eq!(flag_ids(&shown), expected);
    assert_eq!(
        shown["measurement_flags"]["rule_version"],
        "measurement-flags.v1"
    );
    assert!(
        shown["measurement_flags"]["notice"]
            .as_str()
            .unwrap()
            .starts_with("Advisory")
    );
    for flag in shown["measurement_flags"]["flags"].as_array().unwrap() {
        assert_eq!(flag["advisory"], true);
        assert!(!flag["fields"].as_array().unwrap().is_empty());
    }
    // Validity stays as recorded.
    assert_eq!(shown["measurement_validity"]["state"], "valid");

    let case_id = case.as_str().unwrap();
    let brief = run(
        root,
        &["--case", case_id, "brief", "--level", "standard", "--json"],
        None,
        0,
    );
    let evidence = brief["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == observation.as_str())
        .unwrap();
    assert_eq!(flag_ids(evidence), expected);

    let frontier = run(root, &["--case", case_id, "frontier", "--json"], None, 0);
    let rules: Vec<_> = frontier["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|gap| gap["rule_id"].as_str())
        .filter(|rule| rule.starts_with("measurement."))
        .collect();
    for id in expected {
        assert!(rules.contains(&id), "{frontier}");
    }
}
