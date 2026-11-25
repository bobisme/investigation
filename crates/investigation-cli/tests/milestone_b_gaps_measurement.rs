//! Milestone B §20.1 measurement gaps, through the public binary.
//!
//! - X18: intermittent failures recorded as k of n before and after a change
//!   keep their counts, evidence kind and changed variables, and are shown in
//!   `show` and the vault's Measurements view. A causal Conclusion that rests
//!   only on the observational count stays `candidate`.
//! - X27: a percentile combined from pooled raw samples or merged histograms
//!   carries no averaged-percentile flag; one averaged across runs does.
//! - X30: two percentiles taken under different load models are flagged as not
//!   comparable as they stand, on the receipt and in every read.

use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const AVERAGED: &str = "measurement.quantiles_combined_across_runs";
const LOAD_MODEL: &str = "measurement.compared_load_model_differs";

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

/// One workspace and one case, recorded through `record --input -`.
struct Workspace {
    root: tempfile::TempDir,
    case: Value,
    commands: usize,
}

impl Workspace {
    fn new(vault: bool, mode: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let init: &[&str] = if vault {
            &["init", ".", "--json"]
        } else {
            &["init", ".", "--no-vault", "--json"]
        };
        run(root.path(), init, None, 0);
        let mut workspace = Self {
            root,
            case: Value::Null,
            commands: 0,
        };
        let receipt = workspace.submit("case.create", json!({"title": "Gaps", "mode": mode}), 0);
        workspace.case = receipt["created"][0]["id"].clone();
        workspace
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn case(&self) -> &str {
        self.case.as_str().unwrap()
    }

    fn submit(&mut self, op: &str, payload: Value, exit: i32) -> Value {
        self.commands += 1;
        let input = json!({
            "schema_version": "inv.command.v1", "command_id": format!("gaps-{}", self.commands),
            "case_id": self.case, "actor": "agent:gaps", "preconditions": [],
            "operations": [{"op": op, "payload": payload}],
        });
        run(
            self.path(),
            &["record", "--input", "-", "--json"],
            Some(&input),
            exit,
        )
    }

    /// The exact reference of the one record an operation created.
    fn create(&mut self, op: &str, payload: Value) -> Value {
        let receipt = self.submit(op, payload, 0);
        receipt["created"][0].clone()
    }

    fn read(&self, args: &[&str]) -> Value {
        let mut argv = vec!["--case", self.case()];
        argv.extend_from_slice(args);
        argv.push("--json");
        run(self.path(), &argv, None, 0)
    }

    fn show(&self, reference: &Value) -> Value {
        self.read(&["show", reference["id"].as_str().unwrap(), "--depth", "full"])
    }

    fn question(&mut self) -> Value {
        self.create(
            "question.create",
            json!({"wording": "Which factor explains the recorded behaviour?", "importance": "normal"}),
        )
    }

    fn hypothesis(&mut self, question: &Value, statement: &str) -> Value {
        self.create(
            "hypothesis.create",
            json!({
                "question_ref": question, "statement": statement, "assumption_refs": [],
                "rival_refs": [], "applicability": "The fixture workload only.",
                "possible_falsifiers": ["The failures persist with the factor removed."]
            }),
        )
    }

    /// An asserted measurement Observation with the given measurement block.
    fn measured(&mut self, value: &str, measurement: Value) -> Value {
        self.create(
            "observation.add",
            json!({
                "value": {"kind": "measurement", "value": value, "unit": "ms"},
                "basis": {"kind": "assertion", "assertion": "Reported by the recorder.", "context": null},
                "validity": "valid", "quality": [], "limitations": [],
                "evidence_kind": "observational", "variables_changed": [],
                "measurement": measurement
            }),
        )
    }
}

fn flags(view: &Value) -> &Vec<Value> {
    view["measurement_flags"]["flags"].as_array().unwrap()
}

fn flag_ids(view: &Value) -> Vec<&str> {
    flags(view)
        .iter()
        .map(|flag| flag["flag_id"].as_str().unwrap())
        .collect()
}

/// Measurement rule IDs the frontier reports, with the record each is about.
fn frontier_rules(workspace: &Workspace) -> Vec<(String, String)> {
    let frontier = workspace.read(&["frontier"]);
    frontier["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|gap| {
            let rule = gap["rule_id"].as_str()?;
            rule.starts_with("measurement.").then(|| {
                let subject = gap.to_string();
                (rule.to_owned(), subject)
            })
        })
        .collect()
}

fn warnings_with(receipt: &Value, id: &str) -> usize {
    receipt["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|warning| warning.as_str().unwrap().contains(id))
        .count()
}

// ------------------------------------------------------------------ X18

/// Retain a JSON capture and return its ingest item.
fn capture(workspace: &Workspace, name: &str, data: &Value) -> Value {
    std::fs::write(workspace.path().join(name), data.to_string()).unwrap();
    let manifest = json!({
        "schema_version": "inv.ingest.v1", "command_id": format!("capture-{name}"),
        "actor": "agent:gaps", "case_id": workspace.case,
        "source": {"title": name, "format": "application/json"},
        "paths": [name], "extract_text": true,
    });
    std::fs::write(workspace.path().join("manifest.json"), manifest.to_string()).unwrap();
    let report = run(
        workspace.path(),
        &["ingest", "--input", "manifest.json", "--json"],
        None,
        0,
    );
    report["items"][0].clone()
}

/// `k` of `n` requests failed, read from a retained capture.
fn k_of_n(
    workspace: &mut Workspace,
    capture: &Value,
    k: u64,
    n: u64,
    evidence_kind: &str,
    variables_changed: &[&str],
) -> Value {
    let count = |value: u64, definition: &str| json!({"count": known(json!(value)), "definition": known(json!(definition))});
    workspace.create(
        "observation.add",
        json!({
            "value": {"kind": "measurement", "value": k.to_string(), "unit": "failed requests"},
            "basis": {
                "kind": "source",
                "artifact_ref": {"id": capture["artifact_id"], "revision": 1},
                "selector": {"schema_version": "inv.selector.v1",
                             "artifact_digest": capture["digest"], "selector": {"type": "whole"}}
            },
            "extraction_method": "Read the started and timed-out request counts.",
            "validity": "valid", "quality": [], "limitations": ["Twenty requests per condition."],
            "evidence_kind": evidence_kind, "variables_changed": variables_changed,
            "measurement": {
                "workload": known(json!("burst-20")),
                "sample_count": known(json!(n)),
                "instrumentation_changed": known(json!(false)),
                "attempts": {
                    "started": count(n, "requests sent"),
                    "observed": count(n - k, "requests that completed"),
                    "timed_out": count(k, "requests unfinished at 25 s")
                }
            }
        }),
    )
}

#[test]
fn x18_k_of_n_before_and_after_a_change_is_preserved_shown_and_observational_support_stays_candidate()
 {
    let mut workspace = Workspace::new(true, "explain");
    let config = workspace.path().join("investigation.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("import_roots = []"), "{text}");
    std::fs::write(
        &config,
        text.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();

    let question = workspace.question();
    let factor = workspace.hypothesis(&question, "A connection is held across the wait.");
    let rival = workspace.hypothesis(&question, "Scheduler delay alone explains the timeouts.");
    let before_capture = capture(
        &workspace,
        "before.json",
        &json!({"started": 20, "timed_out": 7}),
    );
    let after_capture = capture(
        &workspace,
        "after.json",
        &json!({"started": 20, "timed_out": 0, "release_before_await": true}),
    );
    let before = k_of_n(&mut workspace, &before_capture, 7, 20, "observational", &[]);
    let after = k_of_n(
        &mut workspace,
        &after_capture,
        0,
        20,
        "interventional",
        &["release_before_await"],
    );

    // Both counts, the evidence kind and the changed variable are kept as recorded.
    for (reference, k, kind, variables) in [
        (&before, 7, "observational", json!([])),
        (&after, 0, "interventional", json!(["release_before_await"])),
    ] {
        let shown = workspace.show(reference);
        let body = &shown["record"]["body"];
        assert_eq!(body["value"]["value"], k.to_string(), "{body}");
        assert_eq!(body["value"]["unit"], "failed requests");
        assert_eq!(body["evidence_kind"], kind);
        assert_eq!(body["variables_changed"], variables);
        let measurement = &body["measurement"];
        assert_eq!(measurement["sample_count"], known(json!(20)));
        assert_eq!(measurement["instrumentation_changed"], known(json!(false)));
        let attempts = &measurement["attempts"];
        assert_eq!(attempts["started"]["count"], known(json!(20)));
        assert_eq!(attempts["timed_out"]["count"], known(json!(k)));
        assert_eq!(attempts["observed"]["count"], known(json!(20 - k)));
        assert_eq!(
            attempts["timed_out"]["definition"],
            known(json!("requests unfinished at 25 s"))
        );
        // Recording counts never changes validity.
        assert_eq!(shown["measurement_validity"]["state"], "valid");
    }

    // The vault's Measurements view shows k, n and the changed variable per row.
    run(
        workspace.path(),
        &["--case", workspace.case(), "vault", "render", "--json"],
        None,
        0,
    );
    let view = std::fs::read_to_string(
        workspace
            .path()
            .join("vault/Cases")
            .join(workspace.case())
            .join("Views/Measurements.md"),
    )
    .unwrap();
    let row = |id: &Value| {
        view.lines()
            .find(|line| line.contains(id["id"].as_str().unwrap()))
            .unwrap_or_else(|| panic!("{view}"))
            .to_owned()
    };
    let before_row = row(&before);
    assert!(before_row.contains("`7` `failed requests`"), "{before_row}");
    assert!(before_row.contains("`20` samples"), "{before_row}");
    assert!(!before_row.contains("variable changed"), "{before_row}");
    let after_row = row(&after);
    assert!(after_row.contains("`0` `failed requests`"), "{after_row}");
    assert!(after_row.contains("`20` samples"), "{after_row}");
    assert!(after_row.contains("1 variable changed"), "{after_row}");

    // A causal claim resting only on the observational count stays a candidate.
    let mut conclusion = |support: Value| {
        workspace.create(
            "conclusion.create",
            json!({
                "statement": "Holding the connection across the wait causes the timeouts.",
                "scope": "The burst-20 fixture workload only.",
                "significance": "Release the connection before the wait in this workload.",
                "support_refs": support, "contradiction_refs": [rival],
                "caveats": ["Twenty requests per condition."],
                "causal_factors": [{"hypothesis_ref": factor, "role": "trigger"}]
            }),
        )
    };
    let observational = conclusion(json!([before]));
    let with_intervention = conclusion(json!([before, after]));
    let snapshot = workspace.show(&observational)["snapshot"].clone();
    let refused = workspace.submit(
        "conclusion.request_review",
        json!({"conclusion_ref": observational}),
        5,
    );
    assert_eq!(refused["error"]["code"], "GATE_FAILED", "{refused}");
    assert!(
        refused["error"]["path"]
            .as_str()
            .unwrap()
            .ends_with("/causal_test"),
        "{refused}"
    );
    let shown = workspace.show(&observational);
    assert_eq!(shown["snapshot"], snapshot);
    assert_eq!(shown["effective_status"]["state"], "candidate");

    let audit = workspace.read(&["blindspots"]);
    let only_observational: Vec<&Value> = audit["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|check| check["check_id"] == "causal.observational_only_support")
        .collect();
    assert_eq!(only_observational.len(), 1, "{audit}");
    let finding = only_observational[0];
    assert_eq!(finding["subject_refs"], json!([observational]));
    let facts: Vec<&str> = finding["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|fact| fact["fact"].as_str().unwrap())
        .collect();
    assert!(facts.contains(&"causal_standing:candidate"), "{finding}");
    assert!(
        !facts
            .iter()
            .any(|fact| fact.starts_with("causal_standing:confirmed")),
        "{finding}"
    );
    let observational_only = finding["facts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|fact| fact["fact"] == "evidence_kind:observational_only")
        .unwrap();
    assert_eq!(observational_only["refs"], json!([before]));
    // The conclusion that also cites the interventional count is not reported.
    assert!(
        !finding
            .to_string()
            .contains(with_intervention["id"].as_str().unwrap()),
        "{finding}"
    );
}

// ------------------------------------------------------------------ X27

#[test]
fn x27_pooled_samples_and_merged_histograms_pass_while_averaged_run_percentiles_are_flagged() {
    let mut workspace = Workspace::new(false, "explain");
    let p99 = |combination: &str| {
        // 3,000 samples clear the 2,995 order-statistic bound for any level up
        // to p99.9, so only the combination rule can fire.
        json!({
            "statistic": known(json!({"kind": "quantile", "quantile": "0.99",
                                      "combination": known(json!(combination))})),
            "sample_count": known(json!(3000))
        })
    };
    // Pooled raw samples from two runs, each sample with its repetition level.
    let mut pooled_fields = p99("pooled_samples");
    pooled_fields["samples"] = json!({
        "shape": "leveled", "unit": "ms",
        "samples": [
            {"value": "41.0", "order": known(json!(0)), "process": known(json!("run-1")), "iteration": known(json!(0))},
            {"value": "47.5", "order": known(json!(1)), "process": known(json!("run-2")), "iteration": known(json!(0))},
            {"value": "44.2", "order": known(json!(2)), "process": known(json!("run-1")), "iteration": known(json!(1))},
            {"value": "52.9", "order": known(json!(3)), "process": known(json!("run-2")), "iteration": known(json!(1))}
        ]
    });
    let mut passing = Vec::new();
    for fields in [pooled_fields, p99("merged_histograms"), p99("single_run")] {
        let receipt = workspace.submit(
            "observation.add",
            json!({
                "value": {"kind": "measurement", "value": "48.0", "unit": "ms"},
                "basis": {"kind": "assertion", "assertion": "Reported by the recorder.", "context": null},
                "validity": "valid", "quality": [], "limitations": [],
                "evidence_kind": "observational", "variables_changed": [],
                "measurement": fields.clone()
            }),
            0,
        );
        // No flag; the one warning is that `valid` is the recorder's own
        // statement (ADR 0023).
        let warnings = receipt["warnings"].as_array().unwrap();
        assert!(
            warnings.iter().all(|warning| warning
                .as_str()
                .unwrap()
                .contains("validity basis recorded_only")),
            "{fields}\n{receipt}"
        );
        let reference = receipt["created"][0].clone();
        let shown = workspace.show(&reference);
        assert!(flag_ids(&shown).is_empty(), "{fields}\n{shown}");
        assert_eq!(
            shown["record"]["body"]["measurement"]["statistic"],
            fields["statistic"]
        );
        passing.push(reference);
    }

    let mut flagged = Vec::new();
    for combination in ["averaged_run_values", "other"] {
        let receipt = workspace.submit(
            "observation.add",
            json!({
                "value": {"kind": "measurement", "value": "48.0", "unit": "ms"},
                "basis": {"kind": "assertion", "assertion": "Reported by the recorder.", "context": null},
                "validity": "valid", "quality": [], "limitations": [],
                "evidence_kind": "observational", "variables_changed": [],
                "measurement": p99(combination)
            }),
            0,
        );
        // The flag, and the note that `valid` is the recorder's own
        // statement (ADR 0023).
        assert_eq!(
            receipt["warnings"].as_array().unwrap().len(),
            2,
            "{receipt}"
        );
        assert_eq!(warnings_with(&receipt, AVERAGED), 1, "{receipt}");
        let reference = receipt["created"][0].clone();
        let shown = workspace.show(&reference);
        assert_eq!(flag_ids(&shown), [AVERAGED], "{combination}");
        let flag = &flags(&shown)[0];
        assert_eq!(flag["advisory"], true);
        assert_eq!(flag["fields"][0]["path"], "/measurement/statistic");
        assert!(
            flag["reason"]
                .as_str()
                .unwrap()
                .contains("needs the raw samples or mergeable histograms"),
            "{flag}"
        );
        // The flag never changes validity.
        assert_eq!(shown["measurement_validity"]["state"], "valid");
        flagged.push(reference);
    }

    // A record citing the pooled and merged percentiles together carries no
    // combination flag; one citing the averaged percentile carries it.
    let conclusion = |workspace: &mut Workspace, support: Value| {
        workspace.create(
            "conclusion.create",
            json!({
                "statement": "The p99 is about 48 ms on this workload.",
                "scope": "The fixture workload only.", "significance": "A bounded description.",
                "support_refs": support, "contradiction_refs": [],
                "caveats": ["Recorded values only."]
            }),
        )
    };
    let combined = conclusion(&mut workspace, json!([passing[0], passing[1]]));
    let shown = workspace.show(&combined);
    assert!(!flag_ids(&shown).contains(&AVERAGED), "{shown}");
    let averaged = conclusion(&mut workspace, json!([passing[0], flagged[0]]));
    let shown = workspace.show(&averaged);
    assert_eq!(
        flag_ids(&shown)
            .iter()
            .filter(|id| **id == AVERAGED)
            .count(),
        1,
        "{shown}"
    );
    assert_eq!(flags(&shown)[0]["subject_ref"], flagged[0]);

    // Case-wide, the frontier reports the combination rule for exactly the
    // two percentiles combined from per-run values.
    let rules = frontier_rules(&workspace);
    let averaged_gaps: Vec<_> = rules.iter().filter(|(rule, _)| rule == AVERAGED).collect();
    assert_eq!(averaged_gaps.len(), 2, "{rules:?}");
    for reference in &flagged {
        let id = reference["id"].as_str().unwrap();
        assert!(
            averaged_gaps.iter().any(|(_, gap)| gap.contains(id)),
            "{rules:?}"
        );
    }
    for reference in &passing {
        let id = reference["id"].as_str().unwrap();
        assert!(
            !averaged_gaps.iter().any(|(_, gap)| gap.contains(id)),
            "{rules:?}"
        );
    }
}

// ------------------------------------------------------------------ X30

#[test]
fn x30_percentiles_under_different_load_models_are_not_silently_compared() {
    let mut workspace = Workspace::new(false, "explain");
    let p99 = |load_loop: Option<&str>| {
        let mut fields = json!({
            "statistic": known(json!({"kind": "quantile", "quantile": "0.99"})),
            "sample_count": known(json!(3000)),
            "workload": known(json!("mixed-requests"))
        });
        // Both loops are timed from the intended send time, so the
        // coordinated-omission rule stays quiet and only the comparison fires.
        if let Some(load_loop) = load_loop {
            fields["load_model"] = known(json!({
                "loop": known(json!(load_loop)),
                "timed_from_intended_send": known(json!(true))
            }));
        }
        fields
    };
    let open = workspace.measured("48.0", p99(Some("open")));
    let closed = workspace.measured("31.0", p99(Some("closed")));
    let open_again = workspace.measured("47.0", p99(Some("open")));
    let unrecorded = workspace.measured("40.0", p99(None));
    for reference in [&open, &closed, &open_again, &unrecorded] {
        assert!(flag_ids(&workspace.show(reference)).is_empty());
    }

    let question = workspace.question();
    let criterion = workspace.create(
        "criterion.create",
        json!({
            "name": "p99 within 50 ms", "kind": "soft",
            "scope": {"kind": "all_options", "question_ref": question},
            "scale": {"unit": "ms", "description": "p99 latency", "anchors": []},
            "preferred_direction": "lower",
            "threshold": {"kind": "measured", "operator": "at_most", "value": {"decimal": "50", "unit": "ms"}},
            "weighting_rationale": null
        }),
    );
    let compare = |workspace: &mut Workspace, description: &str, evidence: Value, exit: i32| {
        let option = workspace.create(
            "option.create",
            json!({"question_ref": question, "description": description,
                   "dependencies": [], "tradeoff_notes": []}),
        );
        workspace.submit(
            "comparison.add",
            json!({
                "option_ref": option, "criterion_ref": criterion, "outcome": "meets",
                "method": "Read the two recorded p99 values.",
                "value": {"decimal": "31.0", "unit": "ms"}, "evidence_refs": evidence,
                "assumption_refs": [], "uncertainty": "One capture each.",
                "rationale": "The candidate's p99 is lower."
            }),
            exit,
        )
    };

    // Open loop against closed loop: recorded, but warned and flagged.
    let receipt = compare(&mut workspace, "Candidate build", json!([open, closed]), 0);
    assert_eq!(receipt["result"], "committed");
    assert_eq!(
        receipt["warnings"].as_array().unwrap().len(),
        1,
        "{receipt}"
    );
    assert_eq!(warnings_with(&receipt, LOAD_MODEL), 1, "{receipt}");
    let differing = receipt["created"][0].clone();
    let shown = workspace.show(&differing);
    assert_eq!(flag_ids(&shown), [LOAD_MODEL], "{shown}");
    let flag = &flags(&shown)[0];
    assert_eq!(flag["advisory"], true);
    assert_eq!(flag["subject_ref"], differing);
    assert_eq!(flag["related_refs"], json!([open, closed]));
    let loops: Vec<&Value> = flag["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| {
            assert_eq!(field["path"], "/measurement/load_model");
            &field["recorded"]["value"]["loop"]["value"]
        })
        .collect();
    assert_eq!(loops, [&json!("open"), &json!("closed")]);
    let reason = flag["reason"].as_str().unwrap();
    assert!(reason.contains("differ in load model"), "{reason}");
    assert!(reason.contains("not comparable as they stand"), "{reason}");

    // Matching load models raise nothing.
    let receipt = compare(
        &mut workspace,
        "Baseline build",
        json!([open, open_again]),
        0,
    );
    assert_eq!(receipt["warnings"], json!([]), "{receipt}");
    let matching = receipt["created"][0].clone();
    assert!(flag_ids(&workspace.show(&matching)).is_empty());

    // A control recorded by `measurement.assess` is compared the same way,
    // and a load model only one side records is not assumed to match.
    let assessed = workspace.submit(
        "measurement.assess",
        json!({
            "observation_ref": unrecorded, "channel": null, "validity": "partial",
            "verification": [
                {"kind": "equivalent", "justification": "Checked by hand.", "evidence_refs": []},
                {"kind": "control", "observation_ref": open, "expected_environment": null,
                 "rationale": "Same workload."}
            ],
            "rationale": "Judged against the open-loop control.", "limits": []
        }),
        0,
    );
    let assessed = assessed["updated"]
        .as_array()
        .unwrap()
        .iter()
        .chain(assessed["created"].as_array().unwrap())
        .find(|reference| reference["id"] == unrecorded["id"])
        .cloned()
        .unwrap_or_else(|| panic!("{assessed}"));
    let shown = workspace.show(&assessed);
    assert_eq!(flag_ids(&shown), [LOAD_MODEL], "{shown}");
    let flag = &flags(&shown)[0];
    assert!(
        flag["reason"]
            .as_str()
            .unwrap()
            .contains("do not both record load model"),
        "{flag}"
    );
    assert_eq!(flag["fields"][0]["recorded"], Value::Null);

    // The case frontier reports the rule for the differing comparison and the
    // assessed observation, and not for the matching comparison.
    let rules = frontier_rules(&workspace);
    let gaps: Vec<_> = rules
        .iter()
        .filter(|(rule, _)| rule == LOAD_MODEL)
        .collect();
    for reference in [&differing, &assessed] {
        let id = reference["id"].as_str().unwrap();
        assert!(gaps.iter().any(|(_, gap)| gap.contains(id)), "{rules:?}");
    }
    let id = matching["id"].as_str().unwrap();
    assert!(!gaps.iter().any(|(_, gap)| gap.contains(id)), "{rules:?}");

    // The brief shows the same flag on the comparison it lists.
    let brief = workspace.read(&["brief", "--level", "standard"]);
    let listed = brief["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == differing["id"])
        .unwrap_or_else(|| panic!("{brief}"));
    assert_eq!(flag_ids(listed), [LOAD_MODEL]);
}
