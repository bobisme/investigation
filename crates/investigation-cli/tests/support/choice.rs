//! A small choice case through the public binary, shared by the late
//! criterion and promotion race tests (bn-dla3): one question, two eligible
//! options, one hard criterion with an unknown cell for each option, and a
//! proposed Decision with its validation review, not yet accepted. Every run
//! clears the environment and puts an empty folder on `PATH`.
#![allow(dead_code)]

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

pub const ACTOR: &str = "agent:chooser";
pub const REVIEWER: &str = "agent:reviewer";

pub fn reference(id: &str, revision: u64) -> Value {
    json!({"id": id, "revision": revision})
}

/// The binary on `args` (with `--format json`) on the workspace `ws`.
pub fn command(ws: &Path, args: &[&str]) -> Command {
    let empty_path = ws.parent().unwrap().join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(ws.parent().unwrap())
        .env_clear()
        .env("PATH", &empty_path)
        .arg("--workspace")
        .arg(ws)
        .args(args)
        .args(["--format", "json"]);
    command
}

/// Run `args` with `stdin`: the exit code and the JSON it printed.
pub fn run(ws: &Path, args: &[&str], stdin: Option<&[u8]>) -> (i32, Value) {
    let mut child = command(ws, args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(bytes) = stdin {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "{args:?}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap(), value)
}

/// A one-operation `inv.command.v1` envelope.
pub fn envelope(key: &str, case: Option<&str>, actor: &str, op: &str, payload: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema_version":"inv.command.v1","command_id":key,"case_id":case,"actor":actor,
        "preconditions":[],"operations":[{"op":op,"payload":payload}],
    }))
    .unwrap()
}

pub struct Choice {
    pub ws: PathBuf,
    pub case: String,
    pub question: String,
    pub options: Vec<Value>,
    pub criterion: String,
    pub comparisons: Vec<String>,
    pub decision: String,
    pub review: String,
    keys: std::cell::Cell<usize>,
}

/// A new workspace `casework` in a new folder `root/name`, importing from
/// that folder.
pub fn workspace(root: &Path, name: &str) -> PathBuf {
    let folder = root.join(name);
    fs::create_dir(&folder).unwrap();
    let empty_path = folder.join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let init = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(&folder)
        .env_clear()
        .env("PATH", &empty_path)
        .args(["init", "./casework", "--no-vault", "--import-root", "."])
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert!(init.status.success(), "{init:?}");
    folder.join("casework")
}

/// A hard qualitative criterion for every option of `question`.
pub fn hard_criterion(question: &str, name: &str) -> Value {
    json!({
        "name":name,"kind":"hard","scope":{"kind":"all_options","question_ref":reference(question, 1)},
        "scale":{"unit":null,"description":name,"anchors":[]},"preferred_direction":"lower",
        "threshold":{"kind":"qualitative","requirement":name},"weighting_rationale":null,
    })
}

impl Choice {
    /// The case in a new workspace at `root/name`, up to and including the
    /// review of its proposed Decision.
    pub fn new(root: &Path, name: &str) -> Self {
        let ws = workspace(root, name);
        let mut choice = Self {
            ws,
            case: String::new(),
            question: String::new(),
            options: Vec::new(),
            criterion: String::new(),
            comparisons: Vec::new(),
            decision: String::new(),
            review: String::new(),
            keys: std::cell::Cell::new(0),
        };
        choice.case = choice.create(
            "case.create",
            json!({"title":"Which store should hold the queue?","mode":"evaluate",
                "objective":"Choose a store","decision_relevance":"Pick one store"}),
        );
        choice.question = choice.create(
            "question.create",
            json!({"wording":"Which store should we use?","importance":"high","context":"Two candidates"}),
        );
        for description in ["Keep the current store", "Adopt the new store"] {
            let option = choice.create(
                "option.create",
                json!({"question_ref":reference(&choice.question, 1),"description":description,
                    "dependencies":[],"tradeoff_notes":[]}),
            );
            choice.write(
                "option.disposition",
                json!({"option_ref":reference(&option, 1),"lifecycle":"eligible","reason":"Plausible"}),
            );
            choice.options.push(reference(&option, 2));
        }
        choice.criterion = choice.create(
            "criterion.create",
            hard_criterion(&choice.question, "Keeps every acknowledged write"),
        );
        for option in choice.options.clone() {
            let comparison = choice.create(
                "comparison.add",
                json!({"option_ref":option,"criterion_ref":reference(&choice.criterion, 1),
                    "outcome":"unknown","method":"Not measured","value":null,"evidence_refs":[],
                    "assumption_refs":[],"uncertainty":"No trial","rationale":"No evidence yet"}),
            );
            choice.comparisons.push(comparison);
        }
        let comparisons: Vec<Value> = choice
            .comparisons
            .iter()
            .map(|id| reference(id, 1))
            .collect();
        choice.decision = choice.create(
            "decision.propose",
            json!({"question_ref":reference(&choice.question, 1),
                "choice":{"kind":"select","option_ref":choice.options[0]},
                "compared_alternatives":choice.options,"comparison_refs":comparisons,
                "hard_conditions":[{"criterion_ref":reference(&choice.criterion, 1),
                    "reason":"Verify that no acknowledged write is lost"}],
                "evidence_refs":[],"assumption_refs":[],"tradeoffs":["Nothing is measured yet"],
                "consequences":["Keep the store pending a check"],"reopen_trigger":"A trial loses a write",
                "expected_outcome":"No lost writes","check_when":"After a trial","reversal_conditions":[]}),
        );
        let basis = choice.show(&choice.decision)["review_basis"].clone();
        let key = choice.key();
        let (exit, review) = choice.record(&envelope(
            &key,
            Some(&choice.case),
            REVIEWER,
            "review.submit",
            json!({"kind":"validation","subject_ref":reference(&choice.decision, 1),
                "basis_digest":basis["basis_digest"],"read_set":basis["read_set"],
                "method":"Read the comparisons","materials_examined":[],"shared_dependencies":[],
                "objection_refs":[],"verdict":"supports","caveats":["Nothing is measured yet"],
                "context_isolation":"fresh_context_unblinded","context":{
                    "saw_original_conclusion":{"state":"unknown"},"gathered_new_evidence":{"state":"unknown"},
                    "method_or_source_changes":[],"activity":"Review the choice","dependencies":"unknown",
                    "dependence_basis":"No evidence source"}}),
        ));
        assert_eq!(exit, 0, "{review:#}");
        choice.review = review["created"][0]["id"].as_str().unwrap().to_owned();
        choice
    }

    fn key(&self) -> String {
        self.keys.set(self.keys.get() + 1);
        format!("choice-{}", self.keys.get())
    }

    /// `record --input -` with `bytes`.
    pub fn record(&self, bytes: &[u8]) -> (i32, Value) {
        run(&self.ws, &["record", "--input", "-"], Some(bytes))
    }

    /// One operation by [`ACTOR`] in this case, which must commit.
    pub fn write(&self, op: &str, payload: Value) -> Value {
        let key = self.key();
        let case = (!self.case.is_empty()).then_some(self.case.as_str());
        let (exit, receipt) = self.record(&envelope(&key, case, ACTOR, op, payload));
        assert_eq!(exit, 0, "{op}: {receipt:#}");
        receipt
    }

    /// The ID of the one record `op` creates.
    pub fn create(&self, op: &str, payload: Value) -> String {
        self.write(op, payload)["created"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// The `decision.accept` envelope for the reviewed Decision.
    pub fn accept(&self, key: &str) -> Vec<u8> {
        envelope(
            key,
            Some(&self.case),
            ACTOR,
            "decision.accept",
            json!({"decision_ref":reference(&self.decision, 1),"review_ref":reference(&self.review, 1)}),
        )
    }

    /// `show ID --depth full`.
    pub fn show(&self, id: &str) -> Value {
        let (exit, shown) = run(
            &self.ws,
            &["--case", &self.case, "show", id, "--depth", "full"],
            None,
        );
        assert_eq!(exit, 0, "{shown:#}");
        shown
    }

    /// The Decision's effective state and freshness, as `show` gives them.
    pub fn standing(&self) -> (Value, Value) {
        let shown = self.show(&self.decision);
        (
            shown["effective_status"].clone(),
            shown["freshness"].clone(),
        )
    }
}
