//! Optional coverage plans (W10.21) through the public CLI: a case without a
//! plan shows no coverage section in show, brief, explain or the vault; with
//! a plan, every percentage names its plan revision and denominator, and an
//! invalidated linked measurement moves its cell to `needs_retest`.
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use investigation_render::{RenderPlan, VerifiedSnapshot, ViewPolicy, render};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const ACTOR: &str = "agent:coverage-test";

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
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let stdout = output(cwd, args, stdin, exit);
    serde_json::from_str::<Value>(&stdout).expect(&stdout)
}

struct Case<'a> {
    root: &'a Path,
    id: String,
}

impl Case<'_> {
    fn one(&self, key: &str, op: &str, payload: Value) -> Value {
        let command = json!({
            "schema_version":"inv.command.v1", "command_id":key, "case_id":self.id,
            "actor":ACTOR, "preconditions":[], "operations":[{"op":op,"payload":payload}]
        });
        let receipt = run(
            self.root,
            &["record", "--input", "-", "--json"],
            Some(&command),
            0,
        );
        let change = receipt["created"]
            .get(0)
            .or_else(|| receipt["updated"].get(0))
            .unwrap();
        json!({"id":change["id"],"revision":change["revision"]})
    }

    fn read(&self, args: &[&str]) -> Value {
        let mut argv = vec!["--case", self.id.as_str()];
        argv.extend(args);
        argv.push("--json");
        run(self.root, &argv, None, 0)
    }

    fn show(&self, id: &str, depth: &str) -> Value {
        self.read(&["show", id, "--depth", depth])
    }

    fn show_text(&self, id: &str) -> String {
        output(
            self.root,
            &["--case", &self.id, "show", id, "--format", "text"],
            None,
            0,
        )
    }

    fn explain(&self, id: &str) -> Value {
        self.read(&["explain", id, "--max-bytes", "1000000"])
    }

    fn brief(&self) -> Value {
        self.read(&["brief", "--max-bytes", "1000000"])
    }

    fn vault(&self) -> RenderPlan {
        let mut store = Store::open(Workspace::open(self.root).unwrap());
        let snapshot = VerifiedSnapshot::read(&mut store).unwrap();
        render(&snapshot, &ViewPolicy::default()).unwrap()
    }
}

fn start(root: &Path) -> Case<'_> {
    run(root, &["init", ".", "--no-vault", "--json"], None, 0);
    let command = json!({
        "schema_version":"inv.command.v1", "command_id":"start", "case_id":null, "actor":ACTOR,
        "preconditions":[], "operations":[{"op":"case.create","payload":{
            "title":"Where does the failure reproduce?","mode":"record"}}]
    });
    let started = run(
        root,
        &["record", "--input", "-", "--json"],
        Some(&command),
        0,
    );
    Case {
        root,
        id: started["created"][0]["id"].as_str().unwrap().to_owned(),
    }
}

/// Every object key in `value`, with its path.
fn keys(value: &Value, path: &str, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                let here = format!("{path}/{key}");
                found.push(here.clone());
                keys(item, &here, found);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                keys(item, &format!("{path}/{index}"), found);
            }
        }
        _ => {}
    }
}

fn coverage_keys(value: &Value) -> Vec<String> {
    let mut found = Vec::new();
    keys(value, "", &mut found);
    found
        .into_iter()
        .filter(|path| path.ends_with("/coverage") || path.contains("/coverage/"))
        .collect()
}

/// A percentage is never serialized without its plan revision and denominator.
fn assert_qualified(value: &Value) {
    match value {
        Value::Object(map) => {
            if map.contains_key("percent") {
                for key in ["plan_revision", "denominator", "plan_ref", "statement"] {
                    assert!(map.contains_key(key), "{key} missing: {value}");
                }
            }
            map.values().for_each(assert_qualified);
        }
        Value::Array(items) => items.iter().for_each(assert_qualified),
        _ => {}
    }
}

/// Every text line with a percent sign names the plan revision and the
/// denominator ("N of M").
fn assert_text_qualified(text: &str) {
    for line in text.lines().filter(|line| line.contains('%')) {
        assert!(
            line.contains("plan revision") && line.contains(" of "),
            "unqualified percentage: {line}"
        );
    }
}

fn vault_text(plan: &RenderPlan) -> String {
    plan.entries()
        .iter()
        .map(|entry| String::from_utf8_lossy(entry.content()).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn coverage_appears_only_with_a_plan_and_always_with_its_denominator() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let case = start(root);

    // No plan: no coverage section in show, brief, explain or the vault.
    for view in [
        case.show(&case.id, "full"),
        case.show(&case.id, "summary"),
        case.brief(),
        case.explain(&case.id),
    ] {
        assert!(coverage_keys(&view).is_empty(), "{view}");
    }
    assert!(!case.show_text(&case.id).contains("Coverage"));
    assert!(!vault_text(&case.vault()).contains("Coverage"));

    let plan = case.one(
        "define",
        "coverage.define",
        json!({
            "title": "Conditions by component",
            "plan_kind": "general",
            "applicability": "Current branch on the recorded hosts.",
            "dimensions": [
                {"name": "condition", "values": [{"key": "cold"}, {"key": "warm"}]},
                {"name": "component", "values": [{"key": "parser"}, {"key": "cache"}, {"key": "writer"}]}
            ],
            "predicate": "The failure occurs under the condition in the component.",
            "polarity": {"positive": "The failure occurred.", "negative": "The failure did not occur within scope."},
            "exclusions": [{"matching": {"condition": "cold", "component": "writer"}, "reason": "Not loaded cold."}]
        }),
    );
    let observation = case.one(
        "observe",
        "observation.add",
        json!({
            "value": {"kind": "measurement", "value": "12.5", "unit": "unit"},
            "basis": {"kind": "assertion", "assertion": "Reported by the recorder.", "context": null},
            "extraction_method": null, "validity": "unknown", "quality": [], "limitations": [],
            "evidence_kind": "observational", "variables_changed": [], "observed_at": null, "run_ref": null
        }),
    );
    let plan = case.one(
        "link",
        "coverage.link",
        json!({"plan_ref": plan, "links": [{
            "cell": {"condition": "warm", "component": "cache"},
            "state": "observed_negative_within_scope",
            "record_refs": [observation],
            "rationale": "Ten warm restarts without the failure."
        }]}),
    );
    let plan_id = plan["id"].as_str().unwrap();
    let statement =
        format!("1 of 5 applicable cells assessed (20.0%) under plan revision 1 of {plan_id}");

    let case_view = case.show(&case.id, "summary");
    let listed = &case_view["coverage"]["plans"][0];
    assert_eq!(listed["percentage"]["statement"], json!(statement));
    assert_eq!(listed["denominator"], 5);
    assert_eq!(listed["plan_revision"], 1);
    let plan_view = case.show(plan_id, "full");
    assert_eq!(
        plan_view["coverage"]["counts"]["observed_negative_within_scope"],
        1
    );
    assert_eq!(
        plan_view["coverage"]["untested_sample"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let brief = case.brief();
    assert_eq!(
        brief["case"]["coverage"]["plans"][0]["percentage"]["statement"],
        json!(statement)
    );
    let explained = case.explain(plan_id);
    assert_eq!(
        explained["subject"]["coverage"]["percentage"]["statement"],
        json!(statement),
        "{explained}"
    );
    for view in [&case_view, &plan_view, &brief, &explained] {
        assert_qualified(view);
    }
    let case_text = case.show_text(&case.id);
    let plan_text = case.show_text(plan_id);
    assert!(
        case_text.contains(&format!("Coverage: {statement}")),
        "{case_text}"
    );
    assert!(
        plan_text.contains(&format!("Coverage: {statement}")),
        "{plan_text}"
    );
    // The vault escapes Markdown punctuation; compare the unescaped text.
    let vault = vault_text(&case.vault()).replace('\\', "");
    assert!(
        vault.contains(&format!("- Coverage: {statement}")),
        "{vault}"
    );
    for text in [&case_text, &plan_text, &vault] {
        assert_text_qualified(text);
    }

    // Invalidating the linked measurement moves the cell to needs_retest.
    case.one(
        "invalidate",
        "measurement.assess",
        json!({
            "observation_ref": observation, "channel": null, "validity": "invalid",
            "verification": [{"kind": "equivalent", "justification": "Dropped samples in the window."}],
            "rationale": "The value cannot be interpreted.", "limits": []
        }),
    );
    let plan_view = case.show(plan_id, "full");
    let coverage = &plan_view["coverage"];
    assert_eq!(coverage["counts"]["needs_retest"], 1);
    assert_eq!(coverage["assessed"], 0);
    let cell = coverage["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cell| cell["at"] == json!({"condition": "warm", "component": "cache"}))
        .unwrap();
    assert_eq!(cell["state"], "needs_retest");
    assert_eq!(cell["recorded_state"], "observed_negative_within_scope");
    assert_eq!(cell["retest"][0]["trigger"]["kind"], "invalid_measurement");
    assert_eq!(
        coverage["percentage"]["statement"],
        json!(format!(
            "0 of 5 applicable cells assessed (0.0%) under plan revision 1 of {plan_id}"
        ))
    );
    assert_qualified(&plan_view);
}
