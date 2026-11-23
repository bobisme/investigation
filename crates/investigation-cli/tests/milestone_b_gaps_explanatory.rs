//! Milestone B gaps in the explanatory and cross-domain rows of §20.1, driven
//! through the public CLI:
//!
//! - X11: times a producer envelope or an observation claims (`started_at`,
//!   `ended_at`, `observed_at`, even with a claimed `attested` clock) are kept
//!   verbatim and shown as claims. They never order records and never lift a
//!   Run's chronology above journal order.
//! - X14: an absence claim resting on a failed or a blocked Run is refused at
//!   `conclusion.request_review` and reported by `blindspots`; the same claim
//!   resting on a separate valid observation from a completed Run passes.
//! - X21: an inconclusive close stores its reopen condition, and `show`,
//!   `brief` and the vault read it back. A blank one is refused; a missing one
//!   is committed with a warning and shown as missing.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:gaps";

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_SESSION")
        .args(args)
        .args(["--format", "json"])
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = command.spawn().unwrap();
    if let Some(stdin) = stdin {
        let mut pipe = process.stdin.take().unwrap();
        pipe.write_all(&serde_json::to_vec(stdin).unwrap()).unwrap();
    }
    let output = process.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{args:?}\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&stdout).expect(&stdout)
}

/// One workspace and the case its commands target.
struct Workspace {
    root: PathBuf,
    case: Value,
    commands: usize,
}

impl Workspace {
    fn submit(&mut self, ops: Value, preconditions: Value, exit: i32) -> Value {
        self.commands += 1;
        let command = json!({
            "schema_version":"inv.command.v1","command_id":format!("gaps-{}", self.commands),
            "case_id":self.case,"actor":ACTOR,"preconditions":preconditions,"operations":ops,
        });
        run(
            &self.root,
            &["record", "--input", "-"],
            Some(&command),
            exit,
        )
    }

    fn op(&mut self, name: &str, payload: Value) -> Value {
        self.submit(json!([{"op":name,"payload":payload}]), json!([]), 0)
    }

    fn one(&mut self, name: &str, payload: Value) -> Value {
        let created = self.op(name, payload)["created"][0].clone();
        json!({"id":created["id"],"revision":created["revision"]})
    }

    fn show(&self, id: &Value) -> Value {
        let id = id.as_str().unwrap();
        run(&self.root, &["show", id, "--depth", "full"], None, 0)
    }

    fn case_ref(&self) -> Value {
        let revision = self.show(&self.case)["record"]["header"]["revision"].clone();
        json!({"id":self.case,"revision":revision})
    }

    /// Render the vault and return the note whose file name is `id`.
    fn note(&self, id: &Value) -> String {
        let id = id.as_str().unwrap();
        let path = find(&self.root.join("vault"), &format!("{id}.md"))
            .unwrap_or_else(|| panic!("no note for {id}"));
        fs::read_to_string(path).unwrap()
    }

    fn render(&self) {
        let rendered = run(&self.root, &["vault", "render"], None, 0);
        assert_eq!(rendered["complete"], true, "{rendered}");
    }

    fn case_note(&self, name: &str) -> String {
        let case = self.case.as_str().unwrap();
        fs::read_to_string(self.root.join(format!("vault/Cases/{case}/{name}"))).unwrap()
    }
}

fn find(directory: &Path, name: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(directory).ok()? {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if let Some(found) = find(&path, name) {
                return Some(found);
            }
        } else if path.file_name().and_then(|file| file.to_str()) == Some(name) {
            return Some(path);
        }
    }
    None
}

fn new_case(root: &Path, mode: &str, title: &str) -> Workspace {
    let mut workspace = Workspace {
        root: root.to_owned(),
        case: Value::Null,
        commands: 0,
    };
    workspace.case = workspace.one("case.create", json!({"title":title,"mode":mode}))["id"].clone();
    workspace
}

/// The section of a note under `## heading`, up to the next `## `.
fn section<'a>(note: &'a str, heading: &str) -> Option<&'a str> {
    let start = note.find(&format!("\n## {heading}\n"))? + 1;
    let rest = &note[start..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |end| end + 4);
    Some(&rest[..end])
}

// ------------------------------------------------------------------- X11

fn envelope(run_id: &str, started: &str, ended: &str, clock: &str) -> Value {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/producer-run/v1/envelope.json");
    let mut envelope: Value = serde_json::from_slice(&fs::read(fixture).unwrap()).unwrap();
    envelope["external_run_id"] = json!(run_id);
    envelope["execution"]["started_at"] = json!(started);
    envelope["execution"]["ended_at"] = json!(ended);
    envelope["execution"]["clock_quality"] = json!(clock);
    envelope
}

#[test]
fn x11_claimed_source_times_are_kept_as_claims_and_never_order_or_attest() {
    let directory = tempfile::tempdir().unwrap();
    let inputs = directory.path().join("inputs");
    fs::create_dir(&inputs).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/producer-run/v1/bench-result.json"),
        inputs.join("bench-result.json"),
    )
    .unwrap();
    // Ingested first, the late run claims a time decades after the early one,
    // on a clock the producer calls attested. Nothing checks either claim.
    let late = "2099-06-01T12:00:00.000Z";
    let early = "1999-06-01T12:00:00.000Z";
    for (name, value) in [
        (
            "late",
            envelope("late-claim", late, "2099-06-01T12:05:00.000Z", "attested"),
        ),
        (
            "early",
            envelope(
                "early-claim",
                early,
                "1999-06-01T12:05:00.000+02:00",
                "synchronized",
            ),
        ),
    ] {
        fs::write(
            inputs.join(format!("{name}.json")),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }
    run(
        directory.path(),
        &["init", "ws", "--import-root", inputs.to_str().unwrap()],
        None,
        0,
    );
    let root = directory.path().join("ws");
    let ingest = |name: &str, case: Option<&str>| {
        let path = inputs.join(format!("{name}.json"));
        let mut args = vec![
            "ingest",
            "--input",
            path.to_str().unwrap(),
            "--actor",
            ACTOR,
        ];
        if let Some(case) = case {
            args.extend(["--case", case]);
        }
        let receipt = run(&root, &args, None, 0);
        assert_eq!(receipt["result"], "committed", "{receipt}");
        receipt
    };
    let first = ingest("late", None);
    let case = first["case_id"].as_str().unwrap().to_owned();
    let second = ingest("early", Some(&case));
    let mut workspace = Workspace {
        root: root.clone(),
        case: json!(case),
        commands: 0,
    };
    let late_run = first["run"]["id"].clone();
    let early_run = second["run"]["id"].clone();

    // An observation claims a time on an attested clock as well.
    let observation = workspace.one(
        "observation.add",
        json!({
            "value":{"kind":"measurement","value":"18.7","unit":"ms"},
            "basis":{"kind":"assertion","assertion":"The producer printed the p99.","context":"Copied from the console."},
            "observed_at":{"timestamp":{"state":"known","value":late},"offset":{"state":"known","value":"+00:00"},"clock_quality":"attested"},
            "run_ref":second["run"],"extraction_method":"Read from the console","validity":"unknown",
            "quality":[],"limitations":[],"evidence_kind":"observational","variables_changed":[],
        }),
    );

    // The claims are kept verbatim, clock quality included, as source times.
    let mut sequences = Vec::new();
    for (run_id, started, ended, clock) in [
        (&late_run, late, "2099-06-01T12:05:00.000Z", "attested"),
        (
            &early_run,
            early,
            "1999-06-01T12:05:00.000+02:00",
            "synchronized",
        ),
    ] {
        let record = workspace.show(run_id)["record"].clone();
        let body = &record["body"];
        assert_eq!(body["started_at"]["timestamp"]["value"], started, "{body}");
        assert_eq!(body["finished_at"]["timestamp"]["value"], ended, "{body}");
        assert_eq!(body["started_at"]["clock_quality"], clock);
        assert_eq!(body["producer_run"]["execution"]["started_at"], started);
        // An exploration Run without a plan has no order to prove, and a
        // claimed time or clock never supplies one.
        assert_eq!(body["chronology"], "order_unknown", "{body}");
        assert!(body.get("chronology_verification").is_none(), "{body}");
        assert!(body.get("receipt_checks").is_none(), "{body}");
        let text = body.to_string();
        for strong in ["executor_bound", "externally_attested"] {
            assert!(!text.contains(strong), "{strong} in {text}");
        }
        assert_eq!(body["evidence_validity"], "unknown");
        sequences.push(record["header"]["acceptance_sequence"].as_u64().unwrap());
    }
    // Journal order, not the claimed times, says which came first.
    assert!(sequences[0] < sequences[1], "{sequences:?}");
    let shown = workspace.show(&observation["id"])["record"].clone();
    let observed = if shown["body"]["observed_at"].is_null() {
        shown["header"]["observed_at"].clone()
    } else {
        shown["body"]["observed_at"].clone()
    };
    assert_eq!(observed["timestamp"]["value"], late, "{shown}");
    assert_eq!(observed["clock_quality"], "attested");

    // The vault labels every such time a claim and keeps chronology at
    // journal order.
    workspace.render();
    for (run_id, started, clock) in [
        (&late_run, late, "attested"),
        (&early_run, early, "synchronized"),
    ] {
        let note = workspace.note(run_id);
        assert!(
            note.contains(&format!(
                "- Started at (claimed): `{started}` (offset `+00:00`, clock `{clock}`)"
            )),
            "{note}"
        );
        assert!(note.contains("- Finished at (claimed): `"), "{note}");
        assert!(note.contains("- Chronology: `order_unknown`"), "{note}");
        assert!(
            note.contains("- Chronology verification: `journal_order`"),
            "{note}"
        );
        for strong in ["executor_bound", "externally_attested"] {
            assert!(!note.contains(strong), "{strong} in {note}");
        }
    }
    let note = workspace.note(&observation["id"]);
    assert!(
        note.contains(&format!(
            "- Observed at (claimed): `{late}` (offset `+00:00`, clock `attested`)"
        )),
        "{note}"
    );
    let timeline = workspace.case_note("Views/Timeline.md");
    assert!(timeline.contains("it alone decides order"), "{timeline}");
    assert!(
        timeline.contains("| Observed, started or acquired (claimed) |"),
        "{timeline}"
    );
    let row = |id: &Value| {
        let id = id.as_str().unwrap();
        timeline
            .lines()
            .position(|line| line.contains(&format!("/Runs/{id}\\|")))
            .unwrap_or_else(|| panic!("no row for {id} in {timeline}"))
    };
    // The run claiming 2099 was accepted first and is listed first.
    assert!(row(&late_run) < row(&early_run), "{timeline}");
    let lines: Vec<&str> = timeline.lines().collect();
    assert!(
        lines[row(&late_run)].contains(&format!("started `{late}` (clock `attested`)")),
        "{timeline}"
    );
    assert!(
        lines[row(&early_run)].contains(&format!("started `{early}` (clock `synchronized`)")),
        "{timeline}"
    );
}

// ------------------------------------------------------------------- X14

/// A case with one retained replay log, one question and a hypothesis that
/// rests on nothing being seen.
fn absence_case(root: &Path) -> (Workspace, Value, Value) {
    run(root, &["init", ".", "--no-vault"], None, 0);
    let config = root.join("investigation.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    fs::write(
        root.join("replay.log"),
        "replay finished: 0 rejected requests\n",
    )
    .unwrap();
    let started = run(
        root,
        &[
            "start",
            "Does the service drop requests under load?",
            "--attach",
            "replay.log",
            "--command-id",
            "start",
            "--actor",
            ACTOR,
        ],
        None,
        0,
    );
    let mut workspace = Workspace {
        root: root.to_owned(),
        case: started["created"][0]["id"].clone(),
        commands: 0,
    };
    let question = workspace.one(
        "question.create",
        json!({"importance":"normal","wording":"Does the service drop requests under load?"}),
    );
    let hypothesis = workspace.one(
        "hypothesis.create",
        json!({
            "question_ref":question,"statement":"The service drops no request under the replayed load.",
            "possible_falsifiers":["An error line for a rejected request."],
            "applicability":"The replayed load","rival_refs":[],"assumption_refs":[],
        }),
    );
    (workspace, started["items"][0].clone(), hypothesis)
}

/// A Run in the given execution state, with a recorded sample count so that
/// only its execution state can disqualify it.
fn replay(workspace: &mut Workspace, status: &str) -> Value {
    workspace.one(
        "run.record",
        json!({
            "intent":{"kind":"exploration","intent":"Replay the recorded load once."},
            "producer":"agent:replayer","artifact_refs":[],"execution_status":status,
            "evidence_completeness":"complete",
            "measurement":{"sample_count":{"state":"known","value":200}},
            "evidence_kind":"observational","variables_changed":[],
        }),
    )
}

/// An absence conclusion on the replay log read under `run`. Returns the
/// assessment, the conclusion and the `request_review` response.
fn absence_claim(
    workspace: &mut Workspace,
    log: &Value,
    hypothesis: &Value,
    run_ref: &Value,
    exit: i32,
) -> (Value, Value, Value) {
    let selector = json!({"schema_version":"inv.selector.v1","artifact_digest":log["digest"],
        "selector":{"type":"text_lines","start_line":1,"end_line":1,"encoding":"utf-8","parser_version":"inv.text.utf8.v1"}});
    let observation = workspace.one(
        "observation.add",
        json!({
            "value":{"kind":"text","statement":"The replay log reports no rejected request."},
            "basis":{"kind":"source","artifact_ref":{"id":log["derivative_id"],"revision":1},
                "selector":selector,"text_quote":{"exact":"0 rejected requests"}},
            "run_ref":run_ref,"validity":"valid","quality":[],"limitations":[],
            "evidence_kind":"observational","variables_changed":[],
        }),
    );
    let assessment = workspace.one(
        "assessment.add",
        json!({
            "subject_ref":hypothesis,"evidence_refs":[observation],"relation":"supports",
            "rationale":"No rejected request was seen in the replay.",
            "absence":{
                "expected_signal":"An error line for each rejected request.",
                "opportunity":{"description":"The replay of the recorded load.","refs":[run_ref],"minimum_sample_count":100},
                "justified_equivalent":"The service's error log is the instrument.",
            },
        }),
    );
    let conclusion = workspace.one(
        "conclusion.create",
        json!({
            "statement":"The service drops no request under the replayed load.",
            "scope":"The replayed load","significance":"Would answer the question.",
            "support_refs":[assessment],"contradiction_refs":[],"caveats":["One replay."],
            "claim_form":"absence",
        }),
    );
    let basis = workspace.show(&conclusion["id"])["review_basis"].clone();
    let mut preconditions = basis["read_set"].as_array().cloned().unwrap();
    preconditions.push(basis["subject_ref"].clone());
    let response = workspace.submit(
        json!([{"op":"conclusion.request_review","payload":{"conclusion_ref":conclusion}}]),
        json!(preconditions),
        exit,
    );
    (assessment, conclusion, response)
}

fn missing_codes(response: &Value) -> Vec<(String, Value)> {
    response["error"]["actual"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|reason| reason["missing"].as_array().unwrap().iter())
        .map(|missing| {
            (
                missing["code"].as_str().unwrap().to_owned(),
                missing["refs"].clone(),
            )
        })
        .collect()
}

#[test]
fn x14_a_failed_or_blocked_run_is_not_negative_evidence_through_the_cli() {
    let directory = tempfile::tempdir().unwrap();
    let (mut workspace, log, hypothesis) = absence_case(directory.path());

    for status in ["failed", "blocked"] {
        let unusable = replay(&mut workspace, status);
        let (assessment, conclusion, refused) =
            absence_claim(&mut workspace, &log, &hypothesis, &unusable, 5);
        assert_eq!(
            refused["error"]["code"], "GATE_FAILED",
            "{status}: {refused}"
        );
        assert_eq!(refused["commit_status"], "not_committed");
        assert_eq!(
            refused["error"]["path"],
            "/operations/0/payload/support_refs"
        );
        let actual = &refused["error"]["actual"];
        assert_eq!(actual["ceiling"], "inconclusive");
        assert_eq!(
            actual["human_question"],
            "Are detection limits appropriate?"
        );
        let codes = missing_codes(&refused);
        // The Run's execution state is the only defect in its opportunity.
        let opportunity: Vec<&(String, Value)> = codes
            .iter()
            .filter(|(code, _)| {
                code.starts_with("absence.") && code != "absence.claim_without_basis"
            })
            .collect();
        assert_eq!(opportunity.len(), 1, "{status}: {codes:?}");
        assert_eq!(opportunity[0].0, "absence.opportunity_run_not_usable");
        assert_eq!(opportunity[0].1, json!([unusable]), "{status}");
        assert!(
            refused.to_string().contains("is not negative evidence"),
            "{refused}"
        );
        // Nothing moved: the claim stays a candidate.
        let shown = workspace.show(&conclusion["id"])["record"].clone();
        assert_eq!(shown["header"]["revision"], 1);
        assert_eq!(shown["body"]["status"], "candidate", "{shown}");

        // The structural audit says the same from typed fields.
        let audit = run(&workspace.root, &["blindspots"], None, 0);
        let reported = audit["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|check| check["check_id"] == "source.absence_basis_incomplete")
            .any(|check| {
                check
                    .to_string()
                    .contains(assessment["id"].as_str().unwrap())
            });
        assert!(reported, "{status}: {audit:#}");
    }

    // A separate valid observation, read under a completed Run with the same
    // recorded sample count, carries the same claim through the gate.
    let completed = replay(&mut workspace, "completed");
    let (_, conclusion, accepted) = absence_claim(&mut workspace, &log, &hypothesis, &completed, 0);
    assert_eq!(accepted["result"], "committed", "{accepted}");
    assert_eq!(accepted["updated"][0]["id"], conclusion["id"]);
}

// ------------------------------------------------------------------- X21

fn close(workspace: &mut Workspace, extra: Value, exit: i32) -> Value {
    let mut payload = json!({
        "outcome":"inconclusive","reason":"No recorded observation separates the two causes.",
        "unresolved_questions":[],"retrospective_waived":"No material work was recorded.",
    });
    payload
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let case = workspace.case_ref();
    workspace.submit(
        json!([{"op":"case.close","payload":payload}]),
        json!([case]),
        exit,
    )
}

fn warnings(response: &Value) -> Vec<String> {
    response["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn x21_an_inconclusive_close_keeps_a_readable_reopen_condition() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    run(root, &["init", "."], None, 0);
    let mut workspace = new_case(root, "explore", "Why does the nightly job stall?");
    let question = workspace.one(
        "question.create",
        json!({"importance":"normal","wording":"Which lock does the job wait on?"}),
    );

    // A blank or unbounded condition is refused whole.
    for (condition, why) in [
        (json!("   \n\t"), "blank"),
        (json!("x".repeat(2049)), "too long"),
    ] {
        let refused = close(&mut workspace, json!({"reopen_condition":condition}), 2);
        assert_eq!(
            refused["error"]["code"], "INVALID_INPUT",
            "{why}: {refused}"
        );
        assert_eq!(
            refused["error"]["path"], "/operations/0/payload/reopen_condition",
            "{why}"
        );
        assert_eq!(refused["commit_status"], "not_committed");
        assert_eq!(
            workspace.show(&workspace.case.clone())["record"]["body"]["lifecycle"],
            "active",
            "{why}"
        );
    }

    // A precise condition is stored and read back by show, brief and vault.
    let condition = "A stall reproduces under the tracing build with lock waits logged";
    let closed = close(
        &mut workspace,
        json!({"reopen_condition":condition,"unresolved_questions":[question]}),
        0,
    );
    assert!(
        !warnings(&closed)
            .iter()
            .any(|warning| warning.starts_with("case.reopen_condition_missing")),
        "{closed}"
    );
    let shown = workspace.show(&workspace.case.clone())["record"]["body"].clone();
    assert_eq!(shown["lifecycle"], "closed");
    assert_eq!(shown["closure_outcome"], "inconclusive");
    assert_eq!(shown["closure"]["reopen_condition"], condition);
    assert_eq!(shown["closure"]["unresolved_questions"], json!([question]));
    let case = workspace.case.as_str().unwrap().to_owned();
    let brief = run(root, &["--case", &case, "brief"], None, 0);
    assert_eq!(brief["case"]["closure_outcome"], "inconclusive", "{brief}");
    assert_eq!(brief["case"]["closure"]["reopen_condition"], condition);
    let compact = run(
        root,
        &["--case", &case, "brief", "--level", "compact"],
        None,
        0,
    );
    assert!(compact.to_string().contains(condition), "{compact}");
    workspace.render();
    let overview = workspace.case_note("Overview.md");
    let closure = section(&overview, "Closure").unwrap_or_else(|| panic!("{overview}"));
    assert!(closure.contains("- Outcome: `inconclusive`"), "{closure}");
    assert!(
        closure.contains(&format!("- Reopen condition: {condition}")),
        "{closure}"
    );
    assert!(
        closure.contains(&format!("/Questions/{}|", question["id"].as_str().unwrap())),
        "{closure}"
    );
    assert!(closure.contains("- Retrospective: waived: "), "{closure}");

    // Reopening clears the closure; the vault drops the section.
    let reopened = workspace.case_ref();
    workspace.submit(
        json!([{"op":"case.reopen","payload":{"reason":"A tracing build is now available."}}]),
        json!([reopened]),
        0,
    );
    workspace.render();
    assert!(section(&workspace.case_note("Overview.md"), "Closure").is_none());

    // Without a condition the close is recorded, flagged, and the vault says
    // it is missing.
    let closed = close(&mut workspace, json!({}), 0);
    let flagged = warnings(&closed);
    assert!(
        flagged
            .iter()
            .any(|warning| warning.starts_with("case.reopen_condition_missing")),
        "{flagged:?}"
    );
    assert!(
        workspace.show(&workspace.case.clone())["record"]["body"]["closure"]["reopen_condition"]
            .is_null()
    );
    workspace.render();
    let overview = workspace.case_note("Overview.md");
    let closure = section(&overview, "Closure").unwrap_or_else(|| panic!("{overview}"));
    assert!(
        closure.contains(
            "- Reopen condition: none recorded; an inconclusive close without one states no way back"
        ),
        "{closure}"
    );
}
