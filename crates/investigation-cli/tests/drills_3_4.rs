//! Adversarial drills 3 and 4 (plan §17.7, §8.5, §20.1) through the public
//! binary, replayed from the scripted fixtures in `fixtures/adversarial-drills/`.
//!
//! Drill 3, a blindspot that changes the answer: a Scenario A variant whose
//! validated root cause rests on an unassessed clock-alignment assumption, and
//! a Scenario C variant whose queue choice ignores an unmeasured recovery
//! guarantee. `blindspots` returns the pinned check IDs, and a scripted
//! reviewer records a blindspot Review with a Question and an Assumption in one
//! transaction, never a replacement Conclusion or Decision.
//!
//! Drill 4, a semantic gap outside rule coverage: on a case whose audit is
//! clean, a scripted fresh reviewer adds a rival explanation (or a rival
//! option) through the review. `next` and `frontier` then show it, the
//! subject's effective state shows the open challenge, and the later audit
//! neither rewrites the earlier clean audit nor reports its absence as an
//! error.
//!
//! The responses are scripted transactions, not model runs. Each fixture is a
//! list of steps; the harness here interprets them, and the invariants below
//! hold for every drill:
//!
//! - every audit validates against `schemas/v1/audit.schema.json`, and neither
//!   its JSON nor its text ever says "complete" or claims a verified result;
//! - `next` and `frontier` mention completeness only to deny it;
//! - a step marked `response` updates no record, creates no Conclusion or
//!   Decision, and leaves every Conclusion and Decision of the case exactly as
//!   it was (same revision, same body).
//!
//! # Fixture steps
//!
//! A step has one action key. Values may hold placeholders, resolved when the
//! step runs: `{"$": "name/segment/..."}` reads a binding (a segment is an
//! object key, an array index, `*` for every element, or `[key=value]` for the
//! one element whose `key` equals `value`); `{"$concat": [..]}` joins arrays
//! and single values.
//!
//! - `find`: bind each name to the current revision of the one record of an ID
//!   kind (`kind` is the ID prefix) whose body matches `where` (JSON pointer to
//!   value), or with `all` to every such record in ID order. Once `case` is
//!   bound, only its records are searched.
//! - `record`: submit one `inv.command.v1` transaction (`case`, `actor`,
//!   `preconditions`, `operations`); `bind` names receipt pointers.
//! - `cli`: run a command with `--format json`, optionally with `stdin`.
//! - `audit`: run `blindspots`; `expect` pins `outcome`, the sorted
//!   `check_ids` and `findings`; `replays` re-runs an earlier audit at its
//!   snapshot and requires identical bytes; `cites_any_of` requires every
//!   finding to cite one of the given IDs.
//! - `surface`: run `next` or `frontier`; `expect.contains` lists gaps by rule
//!   and subject ID.
//!
//! `as` keeps a step's JSON response as a binding, and `expect` maps JSON
//! pointers of it to values.
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use investigation_render::VerifiedSnapshot;
use serde_json::{Value, json};

#[path = "support/fixtures.rs"]
mod fixtures;

const FORBIDDEN: &[&str] = &[
    "all blindspots resolved",
    "verified correct",
    "investigation complete",
    "coverage complete",
];

fn drill_dir() -> PathBuf {
    fixtures::repository().join("fixtures/adversarial-drills")
}

/// Whether `text` holds `word` delimited by non-word characters.
fn has_word(text: &str, word: &str) -> bool {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// An audit never says "complete" and never claims a verified result.
fn assert_audit_bounded(text: &str) {
    let lower = text.to_ascii_lowercase();
    assert!(!has_word(&lower, "complete"), "\"complete\" in {text}");
    for phrase in FORBIDDEN {
        assert!(!lower.contains(phrase), "{phrase:?} in {text}");
    }
}

/// Guidance may mention completeness only to deny it.
fn assert_guidance_bounded(text: &str) {
    let lower = text.to_ascii_lowercase();
    for phrase in FORBIDDEN {
        assert!(!lower.contains(phrase), "{phrase:?} in {text}");
    }
    for fragment in lower.split(['.', '"', '\n']) {
        if has_word(fragment, "complete") {
            assert!(has_word(fragment, "not"), "{fragment:?} in {text}");
        }
    }
}

fn walk(value: &Value, segments: &[&str], path: &str) -> Value {
    let Some((segment, rest)) = segments.split_first() else {
        return value.clone();
    };
    fn missing(path: &str, segment: &str, value: &Value) -> ! {
        panic!("{path}: no {segment:?} in {value:#}")
    }
    if *segment == "*" {
        let items = value
            .as_array()
            .unwrap_or_else(|| missing(path, segment, value));
        return Value::Array(items.iter().map(|item| walk(item, rest, path)).collect());
    }
    if let Some(filter) = segment
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
    {
        let (key, wanted) = filter.split_once('=').unwrap();
        let items = value
            .as_array()
            .unwrap_or_else(|| missing(path, segment, value));
        let found: Vec<&Value> = items
            .iter()
            .filter(|item| item[key].as_str() == Some(wanted))
            .collect();
        assert_eq!(found.len(), 1, "{path}: {segment} in {value:#}");
        return walk(found[0], rest, path);
    }
    let next = match value {
        Value::Array(items) => segment.parse::<usize>().ok().and_then(|i| items.get(i)),
        Value::Object(map) => map.get(*segment),
        _ => None,
    };
    walk(
        next.unwrap_or_else(|| missing(path, segment, value)),
        rest,
        path,
    )
}

/// `{"id", "revision"}` of a receipt entry or record reference.
fn exact(value: Value) -> Value {
    match (&value["id"], &value["revision"]) {
        (Value::String(_), Value::Number(_)) => {
            json!({"id": value["id"], "revision": value["revision"]})
        }
        _ => value,
    }
}

struct Drill {
    name: String,
    root: PathBuf,
    bindings: BTreeMap<String, Value>,
    /// Raw JSON bytes of each named audit, for replay comparisons.
    audits: BTreeMap<String, String>,
    step: usize,
}

impl Drill {
    fn output(&self, args: &[String], stdin: Option<&Value>) -> (Option<i32>, String) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
        command
            .current_dir(&self.root)
            .env_remove("AGENT")
            .env_remove("FORMAT")
            .env_remove("INVESTIGATION_SESSION")
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
        (output.status.code(), stdout)
    }

    fn json(&self, args: &[String], stdin: Option<&Value>, exit: i64) -> (Value, String) {
        let mut args = args.to_vec();
        args.extend(["--format".to_owned(), "json".to_owned()]);
        let (code, stdout) = self.output(&args, stdin);
        let value: Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|_| panic!("{}: {args:?}\n{stdout}", self.context()));
        assert_eq!(
            code,
            Some(i32::try_from(exit).unwrap()),
            "{}: {args:?}\n{value:#}",
            self.context()
        );
        (value, stdout)
    }

    fn context(&self) -> String {
        format!("{} step {}", self.name, self.step)
    }

    fn lookup(&self, path: &str) -> Value {
        let segments: Vec<&str> = path.split('/').collect();
        let binding = self
            .bindings
            .get(segments[0])
            .unwrap_or_else(|| panic!("{}: unbound {path}", self.context()));
        walk(binding, &segments[1..], path)
    }

    fn resolve(&self, template: &Value) -> Value {
        match template {
            Value::Object(map) if map.len() == 1 && map.contains_key("$") => {
                self.lookup(map["$"].as_str().unwrap())
            }
            Value::Object(map) if map.len() == 1 && map.contains_key("$concat") => {
                let mut joined = Vec::new();
                for part in map["$concat"].as_array().unwrap() {
                    match self.resolve(part) {
                        Value::Array(items) => joined.extend(items),
                        item => joined.push(item),
                    }
                }
                Value::Array(joined)
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| (key.clone(), self.resolve(value)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.resolve(v)).collect()),
            other => other.clone(),
        }
    }

    fn args(&self, template: &Value) -> Vec<String> {
        self.resolve(template)
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap().to_owned())
            .collect()
    }

    fn case_id(&self) -> String {
        self.lookup("case/id").as_str().unwrap().to_owned()
    }

    /// Current records of the verified snapshot, restricted to the bound case.
    fn records(&self) -> Vec<(String, Value, Value)> {
        let snapshot = VerifiedSnapshot::read(&mut fixtures::open(&self.root)).unwrap();
        let case = self
            .bindings
            .get("case")
            .map(|case| case["id"].as_str().unwrap().to_owned());
        let mut found = Vec::new();
        for state in snapshot.cases() {
            if case
                .as_deref()
                .is_some_and(|case| state.case_id().to_string() != case)
            {
                continue;
            }
            for record in state.state().records() {
                found.push((
                    record.header.id.to_string(),
                    serde_json::to_value(record.reference()).unwrap(),
                    record.body.clone(),
                ));
            }
        }
        found.sort_by(|left, right| left.0.cmp(&right.0));
        found
    }

    /// Every Conclusion and Decision of the case: exact revision and body.
    fn promotable(&self) -> Vec<(String, Value, Value)> {
        self.records()
            .into_iter()
            .filter(|(id, _, _)| id.starts_with("concl_") || id.starts_with("dec_"))
            .collect()
    }

    fn expect(&self, response: &Value, expect: Option<&Value>) {
        let Some(expect) = expect else { return };
        for (pointer, wanted) in expect.as_object().unwrap() {
            let wanted = self.resolve(wanted);
            assert_eq!(
                response.pointer(pointer),
                Some(&wanted),
                "{}: {pointer} in {response:#}",
                self.context()
            );
        }
    }

    fn keep(&mut self, step: &Value, value: Value) {
        if let Some(name) = step.get("as").and_then(Value::as_str) {
            self.bindings.insert(name.to_owned(), value);
        }
    }

    fn find(&mut self, specs: &Value) {
        let records = self.records();
        for (name, spec) in specs.as_object().unwrap() {
            let prefix = format!("{}_", spec["kind"].as_str().unwrap());
            let conditions: Vec<(String, Value)> = spec
                .get("where")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(pointer, value)| (pointer.clone(), self.resolve(value)))
                .collect();
            let matches: Vec<Value> = records
                .iter()
                .filter(|(id, _, body)| {
                    id.starts_with(&prefix)
                        && conditions
                            .iter()
                            .all(|(pointer, value)| body.pointer(pointer) == Some(value))
                })
                .map(|(_, reference, _)| reference.clone())
                .collect();
            let bound = if spec["all"] == true {
                assert!(!matches.is_empty(), "{}: none for {name}", self.context());
                Value::Array(matches)
            } else {
                assert_eq!(
                    matches.len(),
                    1,
                    "{}: {name} matched {matches:?}",
                    self.context()
                );
                matches[0].clone()
            };
            self.bindings.insert(name.clone(), bound);
        }
    }

    fn record(&mut self, step: &Value) {
        let request = &step["record"];
        let case = match request.get("case") {
            Some(case) => self.resolve(case),
            None => json!(self.case_id()),
        };
        let command = json!({
            "schema_version": "inv.command.v1",
            "command_id": format!("{}-{}", self.name, self.step),
            "case_id": case,
            "actor": request["actor"],
            "preconditions": self.resolve(request.get("preconditions").unwrap_or(&json!([]))),
            "operations": self.resolve(&request["operations"]),
        });
        let response = request["response"] == true;
        let before = response.then(|| self.promotable());
        let args = vec!["record".to_owned(), "--input".to_owned(), "-".to_owned()];
        let (receipt, _) = self.json(&args, Some(&command), 0);
        assert_eq!(receipt["result"], "committed", "{receipt:#}");
        if let Some(before) = before {
            // A scripted review response changes no Conclusion or Decision.
            assert_eq!(
                receipt["updated"],
                json!([]),
                "{}: {receipt:#}",
                self.context()
            );
            for created in receipt["created"].as_array().unwrap() {
                let id = created["id"].as_str().unwrap();
                assert!(
                    !id.starts_with("concl_") && !id.starts_with("dec_"),
                    "{}: the response created {id}",
                    self.context()
                );
            }
            assert_eq!(self.promotable(), before, "{}", self.context());
        }
        if let Some(binds) = step.get("bind").and_then(Value::as_object) {
            for (name, pointer) in binds {
                let value = receipt
                    .pointer(pointer.as_str().unwrap())
                    .unwrap_or_else(|| panic!("{}: {pointer} in {receipt:#}", self.context()));
                self.bindings.insert(name.clone(), exact(value.clone()));
            }
        }
        self.expect(&receipt, step.get("expect"));
        self.keep(step, receipt);
    }

    fn cli(&mut self, step: &Value) {
        let args = self.args(&step["cli"]);
        let stdin = step.get("stdin").map(|stdin| self.resolve(stdin));
        let exit = step.get("exit").and_then(Value::as_i64).unwrap_or(0);
        let (response, _) = self.json(&args, stdin.as_ref(), exit);
        self.expect(&response, step.get("expect"));
        self.keep(step, response);
    }

    fn audit(&mut self, step: &Value) {
        let mut args = vec!["--case".to_owned(), self.case_id(), "blindspots".to_owned()];
        if let Some(subject) = step["audit"].get("subject") {
            args.push("--subject".to_owned());
            args.push(self.resolve(subject).as_str().unwrap().to_owned());
        }
        let (response, raw) = self.json(&args, None, 0);
        assert_eq!(response["schema_version"], "inv.audit.v1");
        let schema: Value = serde_json::from_str(
            &fs::read_to_string(fixtures::repository().join("schemas/v1/audit.schema.json"))
                .unwrap(),
        )
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&response)
            .map(|error| format!("{error} at {}", error.instance_path))
            .collect();
        assert!(errors.is_empty(), "{errors:#?}");
        assert_audit_bounded(&raw);
        let (code, text) = self.output(&args, None);
        assert_eq!(code, Some(0), "{text}");
        assert_audit_bounded(&text);
        assert!(
            text.contains(response["statement"].as_str().unwrap()),
            "{text}"
        );
        assert!(
            response["limits"]
                .as_str()
                .unwrap()
                .contains("not proof of completeness")
        );

        let findings = response["checks"].as_array().unwrap();
        let expect = step.get("expect").cloned().unwrap_or(json!({}));
        if let Some(outcome) = expect.get("outcome") {
            assert_eq!(&response["outcome"], outcome, "{}", self.context());
        }
        if let Some(pinned) = expect.get("check_ids") {
            let mut found: Vec<&str> = findings
                .iter()
                .map(|finding| finding["check_id"].as_str().unwrap())
                .collect();
            found.sort_unstable();
            assert_eq!(json!(found), *pinned, "{}: {response:#}", self.context());
        }
        for wanted in expect
            .get("findings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let wanted = self.resolve(wanted);
            let matching: Vec<&Value> = findings
                .iter()
                .filter(|finding| {
                    finding["check_id"] == wanted["check_id"]
                        && wanted
                            .get("subject_refs")
                            .is_none_or(|subjects| &finding["subject_refs"] == subjects)
                        && wanted
                            .get("rule_id")
                            .is_none_or(|rule| &finding["rule_id"] == rule)
                })
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "{}: {wanted:#} in {response:#}",
                self.context()
            );
            for fact in wanted
                .get("facts_include")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                assert!(
                    matching[0]["facts"].as_array().unwrap().contains(fact),
                    "{}: {fact} in {:#}",
                    self.context(),
                    matching[0]
                );
            }
        }
        if let Some(cited) = step.get("cites_any_of") {
            let ids = self.resolve(cited);
            let ids: Vec<&str> = ids
                .as_array()
                .unwrap()
                .iter()
                .map(|id| id.as_str().unwrap())
                .collect();
            for finding in findings {
                let text = finding.to_string();
                assert!(
                    ids.iter().any(|id| text.contains(id)),
                    "{}: a finding cites none of {ids:?}: {finding:#}",
                    self.context()
                );
            }
        }
        if let Some(earlier) = step.get("replays").and_then(Value::as_str) {
            // The earlier audit, re-run at its own snapshot, is byte-identical:
            // what was recorded later is never reported against it.
            let snapshot = &self.bindings[earlier]["snapshot"];
            let mut replay = vec![
                "--case".to_owned(),
                self.case_id(),
                "blindspots".to_owned(),
                "--snapshot".to_owned(),
                snapshot["sequence"].to_string(),
                "--head-hash".to_owned(),
                snapshot["head_hash"].as_str().unwrap().to_owned(),
            ];
            replay.extend(["--format".to_owned(), "json".to_owned()]);
            let (code, bytes) = self.output(&replay, None);
            assert_eq!(code, Some(0), "{bytes}");
            assert_eq!(bytes, self.audits[earlier], "{}", self.context());
        }
        if let Some(name) = step.get("as").and_then(Value::as_str) {
            self.audits.insert(name.to_owned(), raw);
        }
        self.keep(step, response);
    }

    fn surface(&mut self, step: &Value) {
        let command = step["surface"].as_str().unwrap();
        let list = match command {
            "next" => "recommendations",
            "frontier" => "gaps",
            other => panic!("unknown surface {other}"),
        };
        let args = vec!["--case".to_owned(), self.case_id(), command.to_owned()];
        let (response, raw) = self.json(&args, None, 0);
        assert_guidance_bounded(&raw);
        let (code, text) = self.output(&args, None);
        assert_eq!(code, Some(0), "{text}");
        assert_guidance_bounded(&text);
        let gaps = response[list].as_array().unwrap();
        for wanted in step
            .pointer("/expect/contains")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let wanted = self.resolve(wanted);
            assert!(
                gaps.iter().any(|gap| {
                    gap["rule_id"] == wanted["rule_id"]
                        && gap["subject_refs"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|subject| subject["id"] == wanted["subject"])
                }),
                "{}: {wanted} not in {command}: {response:#}",
                self.context()
            );
            // Text output names the same gap.
            assert!(
                text.contains(wanted["subject"].as_str().unwrap()),
                "{}: {text}",
                self.context()
            );
        }
        if let Some(count) = step.pointer("/expect/count") {
            assert_eq!(&json!(gaps.len()), count, "{response:#}");
        }
        self.keep(step, response);
    }
}

fn base(fixture: &Value, root: &Path) {
    let base = &fixture["base"];
    if let Some(scenario) = base.get("workspace").and_then(Value::as_str) {
        fixtures::workspace(scenario, root, false);
        return;
    }
    fs::create_dir_all(root).unwrap();
    let init = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(root)
        .env_remove("AGENT")
        .args(["init", ".", "--no-vault", "--format", "json"])
        .output()
        .unwrap();
    assert!(init.status.success(), "{init:?}");
    let config = root.join("investigation.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    for source in base["sources"].as_array().unwrap() {
        let from = fixtures::repository().join(source["from"].as_str().unwrap());
        fs::copy(&from, root.join(source["as"].as_str().unwrap())).unwrap();
    }
}

/// Replay one drill fixture and return its final bindings.
fn replay(file: &str) -> BTreeMap<String, Value> {
    let fixture: Value =
        serde_json::from_str(&fs::read_to_string(drill_dir().join(file)).unwrap()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("workspace");
    base(&fixture, &root);
    let mut drill = Drill {
        name: file.trim_end_matches(".json").to_owned(),
        root,
        bindings: BTreeMap::new(),
        audits: BTreeMap::new(),
        step: 0,
    };
    for (index, step) in fixture["steps"].as_array().unwrap().iter().enumerate() {
        drill.step = index;
        if let Some(specs) = step.get("find") {
            drill.find(specs);
        } else if step.get("record").is_some() {
            drill.record(step);
        } else if step.get("cli").is_some() {
            drill.cli(step);
        } else if step.get("audit").is_some() {
            drill.audit(step);
        } else if step.get("surface").is_some() {
            drill.surface(step);
        } else {
            panic!("{}: unknown step {step}", drill.context());
        }
    }
    drill.bindings
}

#[test]
fn drill_3_explanatory_root_cause_ignores_a_corrupted_clock() {
    replay("drill-3-explanatory.json");
}

#[test]
fn drill_3_evaluative_queue_choice_ignores_an_unmeasured_recovery_guarantee() {
    replay("drill-3-evaluative.json");
}

#[test]
fn drill_4_explanatory_reviewer_adds_a_rival_explanation_no_check_names() {
    replay("drill-4-explanatory.json");
}

#[test]
fn drill_4_evaluative_reviewer_adds_a_rival_option_no_check_names() {
    replay("drill-4-evaluative.json");
}

#[test]
fn every_drill_fixture_is_replayed_by_a_test() {
    let mut files: Vec<String> = fs::read_dir(drill_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".json"))
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "drill-3-evaluative.json",
            "drill-3-explanatory.json",
            "drill-4-evaluative.json",
            "drill-4-explanatory.json",
        ]
    );
}

#[test]
fn completeness_language_is_detected_as_a_word() {
    assert!(has_word("the investigation is complete.", "complete"));
    assert!(!has_word("not proof of completeness", "complete"));
    assert!(!has_word("evidence_complete", "complete"));
}
