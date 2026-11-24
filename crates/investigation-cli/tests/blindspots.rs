//! The read-only structural blindspot audit through the public binary: the
//! `inv.audit.v1` envelope validates against its published schema, a clean
//! audit never reads as complete, identical snapshots give identical audits,
//! and restricted records never leak text.
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:cli-test";
const FORBIDDEN: &[&str] = &[
    "all blindspots resolved",
    "verified correct",
    "investigation complete",
];

fn output(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> String {
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
    stdout
}

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let stdout = output(cwd, args, stdin, exit);
    serde_json::from_str(&stdout).expect(&stdout)
}

fn published_schema() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/v1/audit.schema.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn assert_valid(response: &Value) {
    let schema = published_schema();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(response)
        .map(|error| format!("{} at {}", error, error.instance_path))
        .collect();
    assert!(errors.is_empty(), "{errors:#?}\n{response:#}");
}

fn assert_bounded(text: &str) {
    let lower = text.to_ascii_lowercase();
    for phrase in FORBIDDEN {
        assert!(!lower.contains(phrase), "{phrase:?} in {text}");
    }
}

struct Case<'a> {
    root: &'a Path,
    id: String,
}

impl Case<'_> {
    fn submit(&self, key: &str, op: &str, payload: Value, pre: Value) -> Value {
        let command = json!({
            "schema_version":"inv.command.v1", "command_id":key, "case_id":self.id,
            "actor":ACTOR, "preconditions":pre, "operations":[{"op":op,"payload":payload}]
        });
        run(
            self.root,
            &["record", "--input", "-", "--json"],
            Some(&command),
            0,
        )
    }

    fn one(&self, key: &str, op: &str, payload: Value) -> Value {
        let receipt = self.submit(key, op, payload, json!([]));
        let change = receipt["created"]
            .get(0)
            .or_else(|| receipt["updated"].get(0))
            .unwrap();
        json!({"id":change["id"],"revision":change["revision"]})
    }

    fn show(&self, id: &Value) -> Value {
        run(
            self.root,
            &[
                "show",
                id["id"].as_str().unwrap(),
                "--depth",
                "full",
                "--json",
            ],
            None,
            0,
        )
    }

    fn review(&self, key: &str, subject: &Value) -> Value {
        let basis = self.show(subject)["review_basis"].clone();
        self.one(
            key,
            "review.submit",
            json!({
                "kind":"validation","subject_ref":basis["subject_ref"],"basis_digest":basis["basis_digest"],
                "read_set":basis["read_set"],"method":"Read the recorded basis.","materials_examined":[],
                "shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":["One reading."],
                "context_isolation":"fresh_context_unblinded","context":{
                    "saw_original_conclusion":{"state":"known","value":true},
                    "gathered_new_evidence":{"state":"known","value":false},
                    "method_or_source_changes":[],"activity":"Read the recorded basis.",
                    "dependencies":"shared","dependence_basis":"Same recorded data."}
            }),
        )
    }

    fn audit(&self, extra: &[&str]) -> Value {
        let mut args = vec!["--case", &self.id, "blindspots"];
        args.extend_from_slice(extra);
        args.push("--json");
        let response = run(self.root, &args, None, 0);
        assert_valid(&response);
        assert_bounded(&response.to_string());
        response
    }

    fn audit_text(&self, extra: &[&str]) -> String {
        let mut args = vec!["--case", &self.id, "blindspots"];
        args.extend_from_slice(extra);
        let text = output(self.root, &args, None, 0);
        assert_bounded(&text);
        text
    }
}

fn start(root: &Path) -> (String, Value) {
    run(root, &["init", ".", "--no-vault", "--json"], None, 0);
    let config = root.join("investigation.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    fs::write(root.join("reading.txt"), "Reading 12.5 units\n").unwrap();
    let start = run(
        root,
        &[
            "start",
            "Can the reading be interpreted?",
            "--attach",
            "reading.txt",
            "--command-id",
            "blindspot-case",
            "--actor",
            ACTOR,
            "--json",
        ],
        None,
        0,
    );
    (
        start["created"][0]["id"].as_str().unwrap().to_owned(),
        start,
    )
}

fn check_ids(response: &Value) -> Vec<String> {
    response["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["check_id"].as_str().unwrap().to_owned())
        .collect()
}

fn finding<'a>(response: &'a Value, check_id: &str) -> &'a Value {
    response["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["check_id"] == check_id)
        .unwrap_or_else(|| panic!("no {check_id} in {response:#}"))
}

#[test]
fn audits_are_bounded_deterministic_pinned_and_schema_valid() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (id, started) = start(root);
    let case = Case { root, id };

    // A case with nothing relied on yet: no structural gap under the named checks.
    let clean = case.audit(&[]);
    assert_eq!(clean["schema_version"], "inv.audit.v1");
    assert_eq!(clean["check_version"], "builtin-blindspot.v1");
    assert_eq!(
        clean["outcome"],
        "no_structural_gap_identified_under_checks"
    );
    assert!(clean["checks"].as_array().unwrap().is_empty());
    let statement = clean["statement"].as_str().unwrap();
    assert!(
        statement.starts_with("No structural gap identified under checks "),
        "{statement}"
    );
    let ran: Vec<&str> = clean["checks_run"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| check["check_id"].as_str().unwrap())
        .collect();
    assert!(ran.contains(&"promoted.open_critical_objection"));
    for check in &ran {
        assert!(statement.contains(check), "{statement}");
    }
    assert!(
        clean["limits"]
            .as_str()
            .unwrap()
            .contains("not proof of completeness")
    );
    let text = case.audit_text(&[]);
    assert!(text.contains(statement), "{text}");
    assert!(text.contains("not proof of completeness"), "{text}");

    // A validated conclusion over one checked reading.
    let artifact = started["items"][0]["derivative_id"].as_str().unwrap();
    let selector = json!({"schema_version":"inv.selector.v1","artifact_digest":started["items"][0]["digest"],"selector":{"type":"text_lines","start_line":1,"end_line":1,"encoding":"utf-8","parser_version":"inv.text.utf8.v1"}});
    let observation = case.one(
        "observe",
        "observation.add",
        json!({
            "value":{"kind":"text","statement":"The recorded reading is 12.5 units."},
            "basis":{"kind":"source","artifact_ref":{"id":artifact,"revision":1},"selector":selector,
                "text_quote":{"exact":"Reading 12.5 units"}},
            "validity":"unknown","quality":[],"limitations":[],"evidence_kind":"observational","variables_changed":[]
        }),
    );
    let observation = case.one("assess", "measurement.assess", json!({
        "observation_ref":observation,"channel":null,"validity":"valid",
        "verification":[{"kind":"equivalent","justification":"The setup reproduced a known reference value."}],
        "rationale":"Judged from the recorded setup.","limits":[]
    }));
    let candidate = case.one(
        "claim",
        "conclusion.create",
        json!({
            "statement":"The reading is 12.5 units.","scope":"This recorded reading.",
            "significance":"Check the setup before reuse.","support_refs":[observation],
            "contradiction_refs":[],"caveats":["One reading."]
        }),
    );
    let basis = case.show(&candidate)["review_basis"].clone();
    let mut pre = basis["read_set"].as_array().cloned().unwrap();
    pre.push(basis["subject_ref"].clone());
    case.submit(
        "ready",
        "conclusion.request_review",
        json!({"conclusion_ref":candidate}),
        json!(pre),
    );
    let ready = json!({"id":candidate["id"],"revision":2});
    let review = case.review("review", &ready);
    let basis = case.show(&ready)["review_basis"].clone();
    let mut pre = basis["read_set"].as_array().cloned().unwrap();
    pre.extend([ready.clone(), review.clone()]);
    case.submit(
        "validate",
        "conclusion.validate",
        json!({"conclusion_ref":ready,"review_ref":review}),
        json!(pre),
    );
    let validated = json!({"id":candidate["id"],"revision":3});
    assert_eq!(
        case.audit(&[])["outcome"],
        "no_structural_gap_identified_under_checks"
    );

    // An unrelated flagged measurement: advisory findings keep the frontier's gap IDs.
    let known = |value: Value| json!({"state":"known","value":value});
    case.one(
        "flagged",
        "observation.add",
        json!({
            "value": {"kind": "measurement", "value": "48.0", "unit": "ms"},
            "basis": {"kind": "assertion", "assertion": "Reported by the recorder.", "context": null},
            "validity": "valid", "quality": [], "limitations": [],
            "evidence_kind": "observational", "variables_changed": [],
            "measurement": {"trial_order": known(json!("sequential"))}
        }),
    );
    let flagged = case.audit(&[]);
    let advisory = finding(&flagged, "measurement.advisory_flags");
    assert_eq!(advisory["rule_id"], "measurement.sequential_trial_order");
    assert_eq!(advisory["severity"], "advisory");
    let frontier = run(root, &["--case", &case.id, "frontier", "--json"], None, 0);
    assert!(
        frontier["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap["gap_id"] == advisory["gap_id"]),
        "{frontier:#}"
    );

    // A critical objection after validation contests the relied-on conclusion.
    let objection = case.one(
        "critical",
        "objection.raise",
        json!({
            "subject_ref":validated,"statement":"A secret critical concern.",
            "scope":"The recorded reading.","severity":"critical","evidence_refs":{"state":"unknown"}
        }),
    );
    let contested = case.audit(&[]);
    assert_eq!(
        contested["outcome"],
        "gaps_identified_in_recorded_structure"
    );
    let critical = finding(&contested, "promoted.open_critical_objection");
    assert_eq!(critical["severity"], "critical");
    assert_eq!(critical["state"], "gap_detected_in_recorded_structure");
    assert_eq!(critical["subject_refs"], json!([validated]));
    assert_eq!(
        critical["gap_id"],
        format!(
            "blindspot:promoted.open_critical_objection:{}",
            validated["id"].as_str().unwrap()
        )
    );
    assert!(
        critical["related_refs"]
            .as_array()
            .unwrap()
            .contains(&objection)
    );
    assert_eq!(critical["suggested_schema"], "objection.resolve");
    assert!(critical["schema_argv"].as_array().is_some());
    assert!(critical["guide_argv"].as_array().is_some());
    // The critical objection alone does not also count as a changed basis.
    assert!(
        !check_ids(&contested).contains(&"promoted.basis_not_current".to_owned()),
        "{contested:#}"
    );
    let text = case.audit_text(&[]);
    assert!(
        text.contains(contested["statement"].as_str().unwrap()),
        "{text}"
    );
    assert!(
        text.contains(critical["gap_id"].as_str().unwrap()),
        "{text}"
    );

    // Identical snapshots give identical audits, byte for byte.
    let again = output(root, &["--case", &case.id, "blindspots", "--json"], None, 0);
    let first = output(root, &["--case", &case.id, "blindspots", "--json"], None, 0);
    assert_eq!(first, again);
    let pinned_at = contested["snapshot"]["sequence"]
        .as_u64()
        .unwrap()
        .to_string();
    let head_hash = contested["snapshot"]["head_hash"]
        .as_str()
        .unwrap()
        .to_owned();

    // Withdrawing the cited artifact changes the conclusion's basis.
    case.one(
        "withdraw",
        "artifact.withdraw",
        json!({"artifact_ref":{"id":artifact,"revision":1},"reason":"collection_error",
            "scope":{"extent":"entire"},"rationale":"Imported from the wrong bench."}),
    );
    let changed = case.audit(&[]);
    let stale = finding(&changed, "promoted.basis_not_current");
    assert!(
        stale["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|fact| fact["fact"] == "freshness_trigger:withdrawn"),
        "{stale:#}"
    );

    // The earlier snapshot still gives exactly the earlier audit.
    let pinned = output(
        root,
        &[
            "--case",
            &case.id,
            "blindspots",
            "--snapshot",
            &pinned_at,
            "--head-hash",
            &head_hash,
            "--json",
        ],
        None,
        0,
    );
    // The earlier audit, labelled historical; the head's audit has no label.
    let mut pinned: Value = serde_json::from_str(&pinned).unwrap();
    assert_eq!(pinned["historical"], true, "{pinned}");
    assert_eq!(
        pinned["historical_read"]["snapshot"]["sequence"].to_string(),
        pinned_at
    );
    let first: Value = serde_json::from_str(&first).unwrap();
    assert!(first.get("historical").is_none(), "{first}");
    let fields = pinned.as_object_mut().unwrap();
    fields.remove("historical");
    fields.remove("historical_read");
    assert_eq!(pinned, first);
    let unknown = run(
        root,
        &[
            "--case",
            &case.id,
            "blindspots",
            "--snapshot",
            "9999",
            "--json",
        ],
        None,
        3,
    );
    assert_eq!(unknown["error"]["code"], "UNKNOWN_REFERENCE");

    // --subject narrows to the conclusion and what it cites.
    let narrowed = case.audit(&["--subject", candidate["id"].as_str().unwrap()]);
    assert_eq!(narrowed["scope"]["subject_ref"], validated);
    let read_set = narrowed["scope"]["subject_read_set"].as_array().unwrap();
    assert!(read_set.contains(&validated));
    assert!(!check_ids(&narrowed).contains(&"measurement.advisory_flags".to_owned()));
    assert!(check_ids(&narrowed).contains(&"promoted.open_critical_objection".to_owned()));
    let refused = run(
        root,
        &[
            "--case",
            &case.id,
            "blindspots",
            "--subject",
            observation["id"].as_str().unwrap(),
            "--json",
        ],
        None,
        2,
    );
    assert_eq!(refused["error"]["path"], "--subject");

    // A restricted objection appears as an ID-only placeholder; its text never does.
    case.one(
        "restrict",
        "classification.raise",
        json!({"subject_ref":objection,"level":"restricted","reason":"Sensitive challenge."}),
    );
    let restricted = case.audit(&[]);
    let critical = finding(&restricted, "promoted.open_critical_objection");
    assert!(
        critical["withheld_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reference| reference["id"] == objection["id"]),
        "{critical:#}"
    );
    let serialized = restricted.to_string();
    assert!(!serialized.contains("secret critical concern"));
    assert!(!case.audit_text(&[]).contains("secret critical concern"));

    // The audit wrote nothing.
    let head = run(root, &["--case", &case.id, "blindspots", "--json"], None, 0);
    assert_eq!(head["snapshot"], restricted["snapshot"]);
}
