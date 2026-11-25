//! Promotions racing a retraction or a criterion change, as two real
//! processes (bn-dla3; plan §20.2 P5).
//!
//! The binary is built with debug assertions, so the commit pause of
//! `investigation_store::test_pause` is compiled in. The promoting process
//! (`conclusion.validate` or `decision.accept`, with the read set its caller
//! read) is started and paused at the start of its commit, before it takes
//! the commit lock. A second process then commits a change to what the
//! promotion rests on, and the first is let go. The promotion is refused
//! with nothing committed: it is validated against the state after the
//! change, never against the state its caller read. A control run pauses
//! and resumes the same promotion with no change in between, and it
//! commits.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Stdio},
};

use investigation_store::test_pause::{PAUSE_COMMIT, PAUSE_READY};
use serde_json::{Value, json};

#[path = "support/choice.rs"]
mod choice;
#[path = "support/pause.rs"]
mod pause;

use choice::{ACTOR, Choice, REVIEWER, command, envelope, hard_criterion, reference, run};

/// A promotion of `bytes` paused at the start of its commit.
struct Paused {
    child: Child,
    ready: PathBuf,
}

impl Paused {
    fn start(ws: &Path, bytes: &[u8]) -> Self {
        let ready = ws.parent().unwrap().join(".promotion-paused");
        let _ = fs::remove_file(&ready);
        let mut child = command(ws, &["record", "--input", "-"])
            .env(PAUSE_COMMIT, "lock")
            .env(PAUSE_READY, &ready)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(bytes).unwrap();
        pause::wait_for_ready(&mut child, &ready, "lock", "the promotion");
        Self { child, ready }
    }

    /// Let it go on; its exit code and response.
    fn resume(self) -> (i32, Value) {
        fs::remove_file(&self.ready).unwrap();
        let output = self.child.wait_with_output().unwrap();
        let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!(
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.code().unwrap(), value)
    }
}

/// The journal sequence of `case`.
fn sequence(ws: &Path, case: &str) -> u64 {
    let (exit, brief) = run(ws, &["--case", case, "brief", "--level", "compact"], None);
    assert_eq!(exit, 0, "{brief:#}");
    brief["snapshot"]["sequence"].as_u64().unwrap()
}

/// The race: start `promotion` paused, commit `change` from another process,
/// let the promotion go. Returns the promotion's exit code and response.
fn race(ws: &Path, case: &str, promotion: &[u8], change: &[u8]) -> (i32, Value) {
    let paused = Paused::start(ws, promotion);
    let before = sequence(ws, case);
    let (exit, changed) = run(ws, &["record", "--input", "-"], Some(change));
    assert_eq!(
        exit, 0,
        "the change commits while the promotion waits: {changed:#}"
    );
    assert_eq!(sequence(ws, case), before + 1);
    let raced = paused.resume();
    assert_ne!(raced.0, 0, "{:#}", raced.1);
    assert_eq!(raced.1["commit_status"], "not_committed", "{:#}", raced.1);
    assert_eq!(
        sequence(ws, case),
        before + 1,
        "the promotion committed nothing"
    );
    raced
}

#[test]
fn an_acceptance_racing_a_new_hard_criterion_is_refused_against_the_new_set() {
    let directory = tempfile::tempdir().unwrap();
    let choice = Choice::new(directory.path(), "criterion");
    let change = envelope(
        "late-criterion",
        Some(&choice.case),
        ACTOR,
        "criterion.create",
        hard_criterion(&choice.question, "Replicates to a second site"),
    );
    let (_, refused) = race(&choice.ws, &choice.case, &choice.accept("accept"), &change);
    assert_eq!(refused["error"]["code"], "GATE_FAILED", "{refused:#}");
    assert_eq!(
        refused["error"]["path"], "/operations/0/payload/review_ref",
        "{refused:#}"
    );
    let (status, _) = choice.standing();
    assert_eq!(status["recorded"], "proposed", "{status:#}");
}

#[test]
fn an_acceptance_racing_a_retraction_of_a_cited_comparison_is_a_revision_conflict() {
    let directory = tempfile::tempdir().unwrap();
    let choice = Choice::new(directory.path(), "retraction");
    let change = envelope(
        "retract",
        Some(&choice.case),
        ACTOR,
        "comparison.retract",
        json!({"comparison_ref":reference(&choice.comparisons[1], 1),
            "reason":"The cell was recorded against the wrong option"}),
    );
    let (_, refused) = race(&choice.ws, &choice.case, &choice.accept("accept"), &change);
    assert_eq!(refused["error"]["code"], "REVISION_CONFLICT", "{refused:#}");
    assert_eq!(
        refused["error"]["path"], "/operations/0/payload/read_set",
        "{refused:#}"
    );
    let (status, _) = choice.standing();
    assert_eq!(status["recorded"], "proposed", "{status:#}");
}

#[test]
fn an_acceptance_paused_and_resumed_with_no_change_commits() {
    let directory = tempfile::tempdir().unwrap();
    let choice = Choice::new(directory.path(), "control");
    let (exit, accepted) = Paused::start(&choice.ws, &choice.accept("accept")).resume();
    assert_eq!(exit, 0, "{accepted:#}");
    assert_eq!(accepted["result"], "committed", "{accepted:#}");
    let (status, _) = choice.standing();
    assert_eq!(status["recorded"], "accepted", "{status:#}");
}

/// A Conclusion on one assessed Observation of an attached reading,
/// requested for review and with a supporting validation review; returns
/// the workspace, the case, the Observation and the `conclusion.validate`
/// envelope with the read set a caller reads before promoting.
fn ready_conclusion(root: &Path) -> (PathBuf, String, Value, Vec<u8>) {
    let ws = choice::workspace(root, "conclusion");
    let folder = ws.parent().unwrap();
    fs::write(folder.join("reading.txt"), "Reading 12.5 units\n").unwrap();
    let (exit, started) = run(
        &ws,
        &[
            "start",
            "Can the reading be interpreted?",
            "--attach",
            "reading.txt",
            "--command-id",
            "start",
            "--actor",
            ACTOR,
        ],
        None,
    );
    assert_eq!(exit, 0, "{started:#}");
    let case = started["created"][0]["id"].as_str().unwrap().to_owned();
    let attachment = &started["items"][0];
    let keys = std::cell::Cell::new(0);
    let write = |actor: &str, op: &str, payload: Value, pre: Value| {
        keys.set(keys.get() + 1);
        let bytes = serde_json::to_vec(&json!({
            "schema_version":"inv.command.v1","command_id":format!("claim-{}", keys.get()),
            "case_id":case,"actor":actor,"preconditions":pre,"operations":[{"op":op,"payload":payload}],
        }))
        .unwrap();
        let (exit, receipt) = run(&ws, &["record", "--input", "-"], Some(&bytes));
        assert_eq!(exit, 0, "{op}: {receipt:#}");
        let change = receipt["created"]
            .get(0)
            .or_else(|| receipt["updated"].get(0))
            .unwrap();
        json!({"id":change["id"],"revision":change["revision"]})
    };
    let show = |id: &Value| {
        let (exit, shown) = run(
            &ws,
            &[
                "--case",
                &case,
                "show",
                id["id"].as_str().unwrap(),
                "--depth",
                "full",
            ],
            None,
        );
        assert_eq!(exit, 0, "{shown:#}");
        shown
    };
    let selector = json!({"schema_version":"inv.selector.v1","artifact_digest":attachment["digest"],
        "selector":{"type":"text_lines","start_line":1,"end_line":1,"encoding":"utf-8","parser_version":"inv.text.utf8.v1"}});
    let observation = write(
        ACTOR,
        "observation.add",
        json!({
            "value":{"kind":"text","statement":"The recorded reading is 12.5 units."},
            "basis":{"kind":"source","artifact_ref":{"id":attachment["derivative_id"],"revision":1},
                "selector":selector,"text_quote":{"exact":"Reading 12.5 units"}},
            "validity":"unknown","quality":[],"limitations":[],"evidence_kind":"observational","variables_changed":[]
        }),
        json!([]),
    );
    let assessed = write(
        ACTOR,
        "measurement.assess",
        json!({"observation_ref":observation,"channel":null,"validity":"valid",
            "verification":[{"kind":"equivalent","justification":"The setup reproduced a known reference value."}],
            "rationale":"Judged from the recorded setup.","limits":[]}),
        json!([]),
    );
    let candidate = write(
        ACTOR,
        "conclusion.create",
        json!({"statement":"The reading is 12.5 units.","scope":"This recorded reading.",
            "significance":"Check the setup before reuse.","support_refs":[assessed],
            "contradiction_refs":[],"caveats":["One reading."],"confidence":null}),
        json!([]),
    );
    let basis = show(&candidate)["review_basis"].clone();
    let mut pre = basis["read_set"].as_array().cloned().unwrap();
    pre.push(basis["subject_ref"].clone());
    let ready = write(
        ACTOR,
        "conclusion.request_review",
        json!({"conclusion_ref":candidate}),
        json!(pre),
    );
    let basis = show(&ready)["review_basis"].clone();
    let review = write(
        REVIEWER,
        "review.submit",
        json!({"kind":"validation","subject_ref":basis["subject_ref"],"basis_digest":basis["basis_digest"],
            "read_set":basis["read_set"],"method":"Read the recorded basis.","materials_examined":[],
            "shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":["One reading."],
            "context_isolation":"fresh_context_unblinded","context":{
                "saw_original_conclusion":{"state":"known","value":true},
                "gathered_new_evidence":{"state":"known","value":false},
                "method_or_source_changes":[],"activity":"Read the recorded basis.",
                "dependencies":"shared","dependence_basis":"Same recorded data."}}),
        json!([]),
    );
    // The read set a caller reads just before promoting.
    let basis = show(&ready)["review_basis"].clone();
    let mut pre = basis["read_set"].as_array().cloned().unwrap();
    pre.extend([ready.clone(), review.clone()]);
    let validate = serde_json::to_vec(&json!({
        "schema_version":"inv.command.v1","command_id":"validate","case_id":case,"actor":ACTOR,
        "preconditions":pre,"operations":[{"op":"conclusion.validate",
            "payload":{"conclusion_ref":ready,"review_ref":review}}],
    }))
    .unwrap();
    (ws, case, assessed, validate)
}

#[test]
fn a_validation_racing_a_retraction_of_its_observation_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let (ws, case, observation, validate) = ready_conclusion(directory.path());
    let change = envelope(
        "retract",
        Some(&case),
        ACTOR,
        "observation.retract",
        json!({"observation_ref":observation,
            "revision_reason":"The reading came from the wrong instrument."}),
    );
    let (_, refused) = race(&ws, &case, &validate, &change);
    assert_eq!(refused["error"]["code"], "REVISION_CONFLICT", "{refused:#}");
}

#[test]
fn a_validation_paused_and_resumed_with_no_change_commits() {
    let directory = tempfile::tempdir().unwrap();
    let (ws, _, _, validate) = ready_conclusion(directory.path());
    let (exit, validated) = Paused::start(&ws, &validate).resume();
    assert_eq!(exit, 0, "{validated:#}");
    assert_eq!(validated["result"], "committed", "{validated:#}");
}
