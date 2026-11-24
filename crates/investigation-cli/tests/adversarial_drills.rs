//! Adversarial drills 1 and 2 of plan §17.7, through the public binary, in an
//! explanatory and an evaluative variant each.
//!
//! Drill 1, title-only cold start: a case of at least 40 mixed records is
//! read through `brief --level compact`, `frontier` and `show --depth title`
//! only. That path names the open gap and the question it serves without
//! reading retained bytes, costs far fewer bytes than reading every record at
//! full depth, and shows each live Conclusion and Decision with its material
//! caveat and effective freshness whether or not an authored synopsis exists.
//!
//! Drill 2, stale synopsis: a once-good authored synopsis says a criterion
//! passed. A source below it is withdrawn (explanatory) or the criterion's
//! threshold changes (evaluative). The synopsis stays in history, is marked
//! stale, and no default read presents it as the current answer: brief at
//! every level and in text, the delta, show, search, and the vault Overview
//! synopsis ladder. Full-depth reads, `explain` and `evidence read` of the
//! cited bytes show the change.
//!
//! Inputs are the committed fixtures under
//! `fixtures/adversarial-drills/drills-1-2-*`; expected read sets, summaries,
//! rule IDs and evidence excerpts are pinned in each fixture's
//! `expected.json`. Every command runs with a cleared environment and an
//! empty `PATH`: no agent runtime or other program is needed. Byte counts
//! are printed (`cargo test --test adversarial_drills -- --nocapture`) and
//! recorded in `fixtures/README.md`.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

#[path = "support/acceptance.rs"]
mod acceptance;

const ACTOR: &str = "agent:drill";

fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
}

fn fixture(name: &str) -> PathBuf {
    repository().join("fixtures/adversarial-drills").join(name)
}

fn exact(id: &str, revision: u64) -> Value {
    json!({"id":id,"revision":revision})
}

fn ids(receipt: &Value) -> Vec<String> {
    receipt["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["id"].as_str().unwrap().to_owned())
        .collect()
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap())
        .collect()
}

/// One retained source: its Source, text derivative and the digest
/// selectors bind to.
struct Retained {
    source: String,
    /// The decoded text derivative that text quotes are verified against.
    derivative: String,
    digest: Value,
}

/// A workspace driven only through the binary.
struct Drill {
    _dir: tempfile::TempDir,
    root: PathBuf,
    empty_path: PathBuf,
    case: String,
    /// Every record the drill created, the case first.
    records: Vec<String>,
    step: usize,
}

impl Drill {
    /// A new workspace whose import root holds the fixture's files.
    fn new(name: &str, vault: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().to_owned();
        let empty_path = parent.join("empty-path");
        fs::create_dir(&empty_path).unwrap();
        for file in ["source.txt", "update.txt"] {
            let path = fixture(name).join(file);
            if path.exists() {
                fs::copy(&path, parent.join(file)).unwrap();
            }
        }
        let drill = Self {
            root: parent.join("casework"),
            empty_path,
            case: String::new(),
            records: Vec::new(),
            step: 0,
            _dir: dir,
        };
        let root = drill.root.to_str().unwrap().to_owned();
        let mut args = vec!["init", &root, "--import-root", parent.to_str().unwrap()];
        if !vault {
            args.push("--no-vault");
        }
        drill.call_in(&parent, &args, None, 0);
        drill
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
        command
            .current_dir(cwd)
            .env_clear()
            .env("PATH", &self.empty_path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn call_in(
        &self,
        cwd: &Path,
        args: &[&str],
        stdin: Option<&Value>,
        exit: i32,
    ) -> (Value, usize) {
        let mut args = args.to_vec();
        args.extend(["--format", "json"]);
        let mut command = self.command(cwd, &args);
        command.stdin(if stdin.is_some() {
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
            "{args:?}\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value =
            serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("{error}: {stdout}"));
        (value, stdout.len())
    }

    /// Run a read or shortcut in the workspace; the JSON and its exact byte count.
    fn call(&self, args: &[&str]) -> (Value, usize) {
        self.call_in(&self.root, args, None, 0)
    }

    fn json(&self, args: &[&str]) -> Value {
        self.call(args).0
    }

    /// The same read as human text.
    fn text(&self, args: &[&str]) -> String {
        let mut args = args.to_vec();
        args.extend(["--format", "text"]);
        let output = self.command(&self.root, &args).output().unwrap();
        assert!(output.status.success(), "{args:?} {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn create_case(&mut self, title: &str, mode: &str) {
        let receipt = self.json(&["--actor", ACTOR, "case", "new", title, "--mode", mode]);
        self.case = ids(&receipt)[0].clone();
        self.records.push(self.case.clone());
    }

    fn retain(&mut self, file: &str, title: &str) -> Retained {
        let path = self.root.parent().unwrap().join(file);
        let receipt = self.json(&[
            "--actor",
            ACTOR,
            "--case",
            &self.case.clone(),
            "source",
            "add",
            path.to_str().unwrap(),
            "--title",
            title,
        ]);
        self.records.extend(ids(&receipt));
        let item = &receipt["items"][0];
        Retained {
            source: item["source_id"].as_str().unwrap().to_owned(),
            derivative: item["derivative_id"].as_str().unwrap().to_owned(),
            digest: item["digest"].clone(),
        }
    }

    /// Commit one transaction; the receipt.
    fn record(&mut self, operations: Vec<(&str, Value)>) -> Value {
        self.step += 1;
        let operations: Vec<Value> = operations
            .into_iter()
            .map(|(op, payload)| json!({"op":op,"payload":payload}))
            .collect();
        let envelope = json!({
            "schema_version":"inv.command.v1","command_id":format!("drill-{}", self.step),
            "case_id":self.case,"actor":ACTOR,"preconditions":[],"operations":operations
        });
        let receipt = self
            .call_in(&self.root, &["record", "--input", "-"], Some(&envelope), 0)
            .0;
        self.records.extend(ids(&receipt));
        receipt
    }

    fn created(&mut self, operations: Vec<(&str, Value)>) -> Vec<String> {
        ids(&self.record(operations))
    }

    fn show(&self, id: &str, depth: &str) -> (Value, usize) {
        self.call(&["show", id, "--depth", depth])
    }

    fn brief(&self, args: &[&str]) -> (Value, usize) {
        let mut argv = vec!["brief"];
        argv.extend(args);
        self.call(&argv)
    }

    /// Every frontier page at the head, followed to the end.
    fn frontier(&self, extra: &[&str]) -> (Vec<Value>, usize) {
        let mut gaps = Vec::new();
        let mut bytes = 0;
        let mut cursor: Option<String> = None;
        loop {
            let mut argv = vec!["frontier"];
            argv.extend(extra);
            if let Some(cursor) = &cursor {
                argv.extend(["--cursor", cursor]);
            }
            let (page, size) = self.call(&argv);
            bytes += size;
            gaps.extend(page["gaps"].as_array().unwrap().iter().cloned());
            match page["next_cursor"].as_str() {
                Some(next) => cursor = Some(next.to_owned()),
                None => return (gaps, bytes),
            }
        }
    }

    fn vault_overview(&self) -> String {
        let render = self.json(&["vault", "render"]);
        assert_eq!(render["ok"], true, "{render}");
        fs::read_to_string(
            self.root
                .join("vault/Cases")
                .join(&self.case)
                .join("Overview.md"),
        )
        .unwrap()
    }
}

fn text_lines(line: usize) -> Value {
    json!({"type":"text_lines","start_line":line,"end_line":line,"encoding":"utf-8",
        "parser_version":"inv.text.utf8.v1"})
}

fn synopsis(subject: &str, text: &str) -> (&'static str, Value) {
    (
        "annotation.add",
        json!({"subject_refs":[exact(subject, 1)],"role":"synopsis","content":{"kind":"text","text":text}}),
    )
}

/// A built fixture case.
struct Built {
    drill: Drill,
    expected: Value,
    /// Conclusion or Decision IDs by their title.
    outcomes: BTreeMap<String, String>,
    /// Records named by the expectations, by their title.
    named: BTreeMap<String, String>,
    /// The source or criterion drill 2 changes, and the receipt it needs.
    retained: Vec<Retained>,
}

impl Built {
    fn outcome(&self, title: &str) -> &str {
        self.outcomes
            .get(title)
            .unwrap_or_else(|| panic!("no outcome titled {title}"))
    }

    fn named(&self, title: &str) -> &str {
        self.named
            .get(title)
            .unwrap_or_else(|| panic!("no record titled {title}"))
    }

    /// Record every authored synopsis the fixture expects, in order; their
    /// IDs and the receipt.
    fn write_synopses(&mut self) -> (Vec<String>, Value) {
        let entries: Vec<(String, String)> = self.expected["synopses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    self.outcome(entry["subject"].as_str().unwrap()).to_owned(),
                    entry["text"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let operations = entries
            .iter()
            .map(|(subject, text)| synopsis(subject, text))
            .collect();
        let receipt = self.drill.record(operations);
        (ids(&receipt), receipt)
    }
}

fn expected(name: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(fixture(name).join("expected.json")).unwrap()).unwrap()
}

/// A planned Conclusion: statement, support, contradiction and caveats.
type Planned<'a> = (&'a str, Vec<Value>, Vec<Value>, Vec<&'a str>);

/// The explanatory fixture: a week of export batches, a log after a change,
/// three questions, rival hypotheses and five conclusions.
fn explanatory(vault: bool) -> Built {
    const NAME: &str = "drills-1-2-explanatory";
    let expected = expected(NAME);
    let mut drill = Drill::new(NAME, vault);
    drill.create_case("Nightly export stalls", "explain");
    let week = drill.retain("source.txt", "Export log, week of 1 November");
    let after = drill.retain("update.txt", "Export log after the lock_timeout change");
    let parent = drill.root.parent().unwrap().to_owned();
    let mut observations = Vec::new();
    for retained in [&week, &after] {
        let file = if retained.source == week.source {
            "source.txt"
        } else {
            "update.txt"
        };
        let text = fs::read_to_string(parent.join(file)).unwrap();
        let operations = text
            .lines()
            .enumerate()
            .map(|(index, line)| {
                let quote = &line[line.find("export batch").unwrap()..];
                (
                    "observation.add",
                    json!({
                        "value":{"kind":"text","statement":format!("Log line: {quote}.")},
                        "basis":{"kind":"source","artifact_ref":exact(&retained.derivative, 1),
                            "selector":{"schema_version":"inv.selector.v1","artifact_digest":retained.digest,
                                "selector":text_lines(index + 1)},
                            "text_quote":{"exact":quote}},
                        "validity":"valid","quality":[],
                        "limitations":["One log line; the log does not name the lock holder."],
                        "evidence_kind":"observational","variables_changed":[]
                    }),
                )
            })
            .collect();
        observations.extend(drill.created(operations));
    }
    let questions = drill.created(
        [
            "Why do nightly report exports stall?",
            "How many export batches stalled in the week?",
            "Do stalls cluster after midnight?",
        ]
        .into_iter()
        .map(|wording| {
            (
                "question.create",
                json!({"importance":"high","wording":wording}),
            )
        })
        .collect(),
    );
    let hypotheses = [
        (
            "A concurrent maintenance job holds the table lock.",
            "Stalls occur on nights without maintenance.",
        ),
        (
            "The export host runs out of disk.",
            "Free disk stays above 10 GB during a stall.",
        ),
        (
            "A slow upstream query blocks the batch.",
            "The upstream query finishes before the stall.",
        ),
    ];
    let hypothesis_ids = drill.created(
        hypotheses
            .iter()
            .map(|(statement, falsifier)| {
                (
                    "hypothesis.create",
                    json!({"applicability":"The nightly export job","assumption_refs":[],
                        "possible_falsifiers":[falsifier],"question_ref":exact(&questions[0], 1),
                        "rival_refs":[],"statement":statement}),
                )
            })
            .collect(),
    );
    // The third hypothesis is left without an assessment: the open gap.
    drill.record(
        [
            (0, 0, "supports", "The batch waited for the table lock."),
            (0, 3, "supports", "The batch waited for the table lock."),
            (
                0,
                4,
                "inconclusive",
                "A completed batch says nothing about the lock.",
            ),
            (1, 1, "inconclusive", "The log does not report disk use."),
            (1, 2, "inconclusive", "The log does not report disk use."),
        ]
        .into_iter()
        .map(|(hypothesis, observation, relation, rationale)| {
            (
                "assessment.add",
                json!({"evidence_refs":[exact(&observations[observation], 1)],"rationale":rationale,
                    "relation":relation,"subject_ref":exact(&hypothesis_ids[hypothesis], 1)}),
            )
        })
        .collect(),
    );
    let refs = |indices: &[usize]| -> Vec<Value> {
        indices
            .iter()
            .map(|index| exact(&observations[*index], 1))
            .collect()
    };
    let conclusions: Vec<Planned> = vec![
        (
            "Four of twelve export batches stalled in the week of 1 November.",
            refs(&[0, 3, 6, 9]),
            vec![],
            vec!["One week of logs from one host."],
        ),
        (
            "Every stalled batch waited for the table lock.",
            refs(&[0, 3, 6, 9]),
            vec![],
            vec!["The log does not name the lock holder."],
        ),
        (
            "Stalls cluster after midnight.",
            refs(&[0, 6]),
            refs(&[3, 9]),
            vec!["Timestamps are wall-clock without attestation."],
        ),
        (
            "Completed batches finish in about three seconds.",
            refs(&[1, 2, 4]),
            vec![],
            vec![],
        ),
        (
            "No batch stalled in the four runs after the lock_timeout change.",
            refs(&[12, 13, 14, 15]),
            vec![],
            vec!["Four runs on one night."],
        ),
    ];
    let conclusion_ids = drill.created(
        conclusions
            .iter()
            .map(|(statement, support, contradiction, caveats)| {
                (
                    "conclusion.create",
                    json!({"statement":statement,"scope":"The logged export batches.",
                        "significance":"Bounds the export stall.","support_refs":support,
                        "contradiction_refs":contradiction,"caveats":caveats}),
                )
            })
            .collect(),
    );
    drill.record(vec![
        (
            "assumption.create",
            json!({"affected_refs":[exact(&hypothesis_ids[0], 1)],
                "claim":"The export log records every batch.",
                "consequence":"Counts of stalled batches depend on it.","materiality":"material"}),
        ),
        (
            "inquiry.create",
            json!({"controls":[],"desired_observations":["The lock holder for each stalled batch."],
                "inquiry":{"mode":"exploratory"},"limits":["One week of logs."],
                "method":"Join the lock wait log to the maintenance schedule.",
                "purpose":"Find the lock holder."}),
        ),
        (
            "annotation.add",
            json!({"subject_refs":[exact(&hypothesis_ids[2], 1)],
                "content":{"kind":"text","text":"Check the upstream query timings."}}),
        ),
    ]);
    let mut named = BTreeMap::new();
    for (index, wording) in [
        "Why do nightly report exports stall?",
        "How many export batches stalled in the week?",
        "Do stalls cluster after midnight?",
    ]
    .into_iter()
    .enumerate()
    {
        named.insert(wording.to_owned(), questions[index].clone());
    }
    for (index, (statement, _)) in hypotheses.iter().enumerate() {
        named.insert((*statement).to_owned(), hypothesis_ids[index].clone());
    }
    Built {
        drill,
        expected,
        outcomes: conclusions
            .iter()
            .zip(conclusion_ids)
            .map(|(conclusion, id)| (conclusion.0.to_owned(), id))
            .collect(),
        named,
        retained: vec![week, after],
    }
}

/// The evaluative fixture: three queue options against one hard and three
/// soft criteria, a proposed decision and one conclusion. One hard cell is
/// left uncompared although its measurement is recorded.
fn evaluative(vault: bool) -> Built {
    const NAME: &str = "drills-1-2-evaluative";
    const OPTIONS: [&str; 3] = ["database queue", "managed queue", "dedicated broker"];
    const UNITS: [&str; 4] = ["seconds", "milliseconds", "USD/month", "engineer-weeks"];
    let expected = expected(NAME);
    let mut drill = Drill::new(NAME, vault);
    drill.create_case("Queue choice", "evaluate");
    let trials = drill.retain("source.txt", "Synthetic queue trials");
    let text = fs::read_to_string(drill.root.parent().unwrap().join("source.txt")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    // The measured number on each line: the first integer after the colon.
    let values: Vec<String> = lines
        .iter()
        .map(|line| {
            line.split(':')
                .nth(1)
                .unwrap()
                .split_whitespace()
                .find(|word| word.parse::<u64>().is_ok())
                .unwrap()
                .to_owned()
        })
        .collect();
    let observations = drill.created(
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                (
                    "observation.add",
                    json!({
                        "value":{"kind":"measurement","value":value,"unit":UNITS[index % 4]},
                        "basis":{"kind":"source","artifact_ref":exact(&trials.derivative, 1),
                            "selector":{"schema_version":"inv.selector.v1","artifact_digest":trials.digest,
                                "selector":text_lines(index + 1)},
                            "text_quote":null},
                        "validity":"valid","quality":[],
                        "limitations":["Synthetic trial; not production traffic."],
                        "evidence_kind":"observational","variables_changed":[]
                    }),
                )
            })
            .collect(),
    );
    let questions = drill.created(vec![
        (
            "question.create",
            json!({"importance":"high","wording":"Which queue should the application use?"}),
        ),
        (
            "question.create",
            json!({"importance":"normal","wording":"Is the synthetic recovery trial representative?"}),
        ),
    ]);
    let question = questions[0].clone();
    let options = drill.created(
        OPTIONS
            .iter()
            .map(|description| {
                (
                    "option.create",
                    json!({"question_ref":exact(&question, 1),"description":description,
                        "dependencies":[],"tradeoff_notes":[]}),
                )
            })
            .collect(),
    );
    drill.record(
        options
            .iter()
            .map(|option| {
                (
                    "option.disposition",
                    json!({"option_ref":exact(option, 1),"lifecycle":"eligible",
                        "reason":"Plausible candidate for the stated application"}),
                )
            })
            .collect(),
    );
    let criterion = |name: &str, kind: &str, unit: &str, threshold: Value| {
        (
            "criterion.create",
            json!({"name":name,"kind":kind,"scope":{"kind":"all_options","question_ref":exact(&question, 1)},
                "scale":{"unit":unit,"description":name,"anchors":[]},"preferred_direction":"lower",
                "threshold":threshold,"weighting_rationale":null}),
        )
    };
    let criteria = drill.created(vec![
        criterion(
            "Recovery time",
            "hard",
            "seconds",
            json!({"kind":"measured","operator":"at_most","value":{"decimal":"300","unit":"seconds"}}),
        ),
        criterion("Enqueue latency", "soft", "milliseconds", Value::Null),
        criterion("Monthly cost", "soft", "USD/month", Value::Null),
        criterion("Migration effort", "soft", "engineer-weeks", Value::Null),
    ]);
    // Every cell but the dedicated broker's recovery time is compared.
    let mut cells = Vec::new();
    for (option_index, option) in options.iter().enumerate() {
        for (criterion_index, criterion) in criteria.iter().enumerate() {
            if option_index == 2 && criterion_index == 0 {
                continue;
            }
            let line = option_index * 4 + criterion_index;
            cells.push((
                "comparison.add",
                json!({"option_ref":exact(option, 2),"criterion_ref":exact(criterion, 1),
                    "outcome":if criterion_index == 0 { "meets" } else { "partially_meets" },
                    "method":"Read from the synthetic trial log",
                    "value":{"decimal":values[line],"unit":UNITS[criterion_index]},
                    "evidence_refs":[exact(&observations[line], 1)],"assumption_refs":[],
                    "uncertainty":"Synthetic trials only","rationale":"A scoped comparison, not a ranking"}),
            ));
        }
    }
    let comparisons = drill.created(cells);
    let assumption = drill.created(vec![(
        "assumption.create",
        json!({"claim":"Traffic stays below 2500000 jobs per day",
            "consequence":"The cost ordering may change above it","materiality":"material","affected_refs":[]}),
    )])[0]
        .clone();
    let conclusion = drill.created(vec![(
        "conclusion.create",
        json!({"statement":"The dedicated broker had the lowest enqueue p99 in the trials.",
            "scope":"Twelve synthetic samples per option.","significance":"Informs the latency trade-off.",
            "support_refs":[exact(&observations[9], 1)],
            "contradiction_refs":[],"caveats":["Twelve samples do not establish a production p99."]}),
    )])[0]
        .clone();
    let decision = drill.created(vec![(
        "decision.propose",
        json!({"question_ref":exact(&question, 1),"choice":{"kind":"select","option_ref":exact(&options[0], 2)},
            "compared_alternatives":options.iter().map(|option| exact(option, 2)).collect::<Vec<_>>(),
            "comparison_refs":comparisons.iter().map(|id| exact(id, 1)).collect::<Vec<_>>(),
            "hard_conditions":[{"criterion_ref":exact(&criteria[0], 1),
                "reason":"Independently verify the reported recovery p95"}],
            "evidence_refs":[exact(&observations[0], 1)],"assumption_refs":[exact(&assumption, 1)],
            "tradeoffs":["Higher enqueue latency than the broker"],
            "consequences":["Keep the database queue"],
            "reopen_trigger":"Retest recovery if the threshold changes",
            "expected_outcome":"A maintainable queue","check_when":"After an independent failure test",
            "reversal_conditions":[]}),
    )])[0]
        .clone();
    drill.record(vec![(
        "annotation.add",
        json!({"subject_refs":[exact(&questions[1], 1)],
            "content":{"kind":"text","text":"Ask whether the failure model matches production."}}),
    )]);
    let mut named: BTreeMap<String, String> = OPTIONS
        .iter()
        .zip(&options)
        .map(|(description, id)| ((*description).to_owned(), id.clone()))
        .collect();
    named.insert("Recovery time".to_owned(), criteria[0].clone());
    named.insert(
        "Which queue should the application use?".to_owned(),
        question,
    );
    Built {
        drill,
        expected,
        outcomes: BTreeMap::from([
            ("database queue".to_owned(), decision),
            (
                "The dedicated broker had the lowest enqueue p99 in the trials.".to_owned(),
                conclusion,
            ),
        ]),
        named,
        retained: vec![trials],
    }
}

/// The compact index entry of one outcome.
fn entry<'a>(brief: &'a Value, id: &str) -> &'a Value {
    brief["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("{id} not indexed in {}", brief["outcomes"]))
}

/// Byte counts of the layered reads of one case.
#[derive(Debug)]
struct Bytes {
    records: usize,
    compact: usize,
    standard: usize,
    frontier: usize,
    titles: usize,
    cold_path: usize,
    full_traversal: usize,
}

/// Drill 1 on a built fixture. Returns the byte counts it measured.
fn cold_start(built: &mut Built, variant: &str) -> Bytes {
    let expected = built.expected["cold_start"].clone();
    let outcome_ids: Vec<String> = built.outcomes.values().cloned().collect();

    // The index before any synopsis exists, for the independence check.
    let (bare, _) = built.drill.brief(&["--level", "compact"]);
    let (synopses, _) = built.write_synopses();
    let drill = &built.drill;
    assert!(
        drill.records.len() >= 40,
        "{variant}: only {} records",
        drill.records.len()
    );

    // ---- the cold path: compact brief, frontier, then titles only
    let (compact, compact_bytes) = drill.brief(&["--level", "compact"]);
    assert_eq!(compact["level"], "compact");
    assert_eq!(compact["omitted_items"]["outcomes"], 0, "{variant}");
    assert_eq!(compact["omitted_items"]["evidence"], 0, "{variant}");
    let indexed = compact["outcomes"].as_array().unwrap();
    assert_eq!(indexed.len(), outcome_ids.len(), "{variant}");
    // Every indexed outcome is pinned below.
    assert_eq!(
        expected["outcomes"].as_array().unwrap().len(),
        indexed.len(),
        "{variant}"
    );
    // Every current Conclusion and Decision carries its status, material
    // caveat, contradiction and effective freshness in the index, the same
    // with or without an authored synopsis, and never the synopsis text.
    for pinned in expected["outcomes"].as_array().unwrap() {
        let title = pinned["title"].as_str().unwrap();
        let id = built.outcome(title);
        let shown = entry(&compact, id);
        assert_eq!(shown["title"], title, "{variant}");
        let full = drill.show(id, "full").0;
        assert_eq!(
            shown["effective_status"]["state"], pinned["effective_status"],
            "{variant} {title}"
        );
        assert_eq!(
            shown["freshness"]["label"], pinned["freshness"],
            "{variant} {title}"
        );
        assert_eq!(shown["freshness"]["state"], "current", "{variant} {title}");
        assert_eq!(
            shown["freshness"]["label"], full["freshness"]["label"],
            "{variant} {title}"
        );
        assert_eq!(
            shown["contradiction"]["state"], pinned["contradiction"],
            "{variant} {title}"
        );
        let caveat = &shown["material_caveat"];
        match &pinned["material_caveat"] {
            Value::Null => assert_eq!(caveat, &json!({"state":"unknown"}), "{variant} {title}"),
            Value::String(text) => {
                assert_eq!(caveat["state"], "known", "{variant} {title}");
                let reason = caveat
                    .get("value")
                    .or_else(|| caveat.get("reason"))
                    .unwrap();
                assert_eq!(reason, text.as_str(), "{variant} {title}");
            }
            other => panic!("unexpected pin {other}"),
        }
        assert!(shown.get("summary").is_none(), "{variant} {title}");
        let before = entry(&bare, id);
        for key in [
            "material_caveat",
            "contradiction",
            "effective_status",
            "freshness",
        ] {
            assert_eq!(shown[key], before[key], "{variant} {title}: {key}");
        }
    }
    let serialized = compact.to_string();
    for synopsis in built.expected["synopses"].as_array().unwrap() {
        let text = synopsis["text"].as_str().unwrap();
        assert!(!serialized.contains(text), "{variant}: {text}");
    }
    let counted = compact["omissions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "authored_synopses")
        .unwrap_or_else(|| panic!("{variant}: synopses are not counted"));
    assert_eq!(counted["current"], synopses.len(), "{variant}");
    assert_eq!(counted["stale"], 0, "{variant}");
    // The case-level caveat is the leading outcome's.
    let lead = built.outcome(expected["leading_outcome"].as_str().unwrap());
    assert_eq!(compact["outcomes"][0]["id"], lead, "{variant}");
    assert_eq!(
        compact["material_caveat"], compact["outcomes"][0]["material_caveat"],
        "{variant}"
    );

    // The compact brief names the gap to read next; the frontier, filtered
    // to that rule, lists it with its read set.
    let pinned_gap = &expected["lead_gap"];
    let recommendation = &compact["gaps"][0]["recommendations"][0];
    let rule = recommendation["rule_id"].as_str().unwrap();
    assert_eq!(rule, pinned_gap["rule_id"], "{variant}");
    let (filtered, filtered_bytes) = drill.frontier(&["--rule", rule]);
    assert_eq!(filtered.len(), 1, "{variant}");
    let gap = &filtered[0];
    assert_eq!(gap["status"], pinned_gap["status"], "{variant}");
    assert_eq!(recommendation["gap_id"], gap["gap_id"], "{variant}");

    // The whole inventory, unranked: pinned rule IDs and counts.
    let (gaps, frontier_bytes) = drill.frontier(&[]);
    let mut rules: BTreeMap<String, u64> = BTreeMap::new();
    for gap in &gaps {
        *rules
            .entry(gap["rule_id"].as_str().unwrap().to_owned())
            .or_default() += 1;
    }
    let pinned_rules: BTreeMap<String, u64> =
        serde_json::from_value(expected["frontier_rules"].clone()).unwrap();
    assert_eq!(rules, pinned_rules, "{variant}");

    // Titles resolve the gap's subjects and read set.
    let mut path_bytes = compact_bytes + filtered_bytes;
    let mut reads = 0;
    let mut title_of = |id: &str| -> String {
        let (shown, bytes) = drill.show(id, "title");
        path_bytes += bytes;
        reads += 1;
        assert_eq!(shown["depth"], "title");
        assert!(shown["freshness"]["label"].is_string(), "{shown}");
        shown["title"].as_str().unwrap().to_owned()
    };
    let subjects: Vec<String> = gap["subject_refs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|subject| title_of(subject["id"].as_str().unwrap()))
        .collect();
    assert_eq!(
        subjects,
        strings(&pinned_gap["subject_titles"]),
        "{variant}"
    );
    let read_set: Vec<String> = gap["read_set"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| title_of(item["id"].as_str().unwrap()))
        .collect();
    assert_eq!(
        read_set,
        strings(&pinned_gap["read_set_titles"]),
        "{variant}"
    );
    // The gap serves the pinned question: its first subject's typed
    // question link, from one full record, never from retained bytes.
    let (subject, bytes) = drill.show(gap["subject_refs"][0]["id"].as_str().unwrap(), "full");
    let question = subject["record"]["body"]["question_ref"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{variant}: {subject}"))
        .to_owned();
    let wording = title_of(&question);
    path_bytes += bytes;
    reads += 1;
    assert_eq!(wording, expected["right_question"], "{variant}");
    assert_eq!(question, built.named(&wording), "{variant}");
    assert!(
        compact["unresolved_questions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == question.as_str() && item["title"] == wording.as_str()),
        "{variant}: {}",
        compact["unresolved_questions"]
    );

    // ---- the comparison reads
    let (standard, standard_bytes) = drill.brief(&["--level", "standard"]);
    // Depth and budget are orthogonal (plan §8.3): every level fills the
    // same --max-bytes with whole items. A compact entry is never larger
    // than the standard one, so unbounded the compact brief is the smaller;
    // within the default budget it can be larger only because standard left
    // out items that did not fit (bn-1xnl).
    let unbounded = |level: &str| drill.brief(&["--level", level, "--max-bytes", "1000000"]);
    let ((_, compact_all), (_, standard_all)) = (unbounded("compact"), unbounded("standard"));
    assert!(
        compact_all < standard_all,
        "{variant}: {compact_all} >= {standard_all}"
    );
    if compact_bytes > standard_bytes {
        assert!(
            standard["omitted"]
                .as_array()
                .unwrap()
                .iter()
                .any(|section| section["cause"] == "budget"),
            "{variant}: {}",
            standard["omitted"]
        );
    }
    let mut full_traversal = 0;
    let mut titles = 0;
    for id in &drill.records {
        full_traversal += drill.show(id, "full").1;
        titles += drill.show(id, "title").1;
    }
    let bytes = Bytes {
        records: drill.records.len(),
        compact: compact_bytes,
        standard: standard_bytes,
        frontier: frontier_bytes,
        titles,
        cold_path: path_bytes,
        full_traversal,
    };
    println!(
        "drill 1 {variant}: {} records; compact brief {} B; standard brief {} B; frontier inventory (all pages) {} B; \
         cold path (compact brief, frontier for its gap, {} record reads) {} B; title sweep of every record {} B; \
         full traversal of every record {} B; full / compact = {:.1}",
        bytes.records,
        bytes.compact,
        bytes.standard,
        bytes.frontier,
        reads,
        bytes.cold_path,
        bytes.titles,
        bytes.full_traversal,
        bytes.full_traversal as f64 / bytes.compact as f64,
    );
    // For the deterministic acceptance report (plan §20.5, context
    // efficiency): the compact brief against a full-record traversal.
    acceptance::observe(
        "context_efficiency",
        &format!("drill-1-{variant}"),
        &json!({"records": bytes.records, "compact_brief_bytes": bytes.compact,
                "standard_brief_bytes": bytes.standard, "title_sweep_bytes": bytes.titles,
                "cold_path_bytes": bytes.cold_path, "full_traversal_bytes": bytes.full_traversal,
                "compact_to_full_ratio": bytes.compact as f64 / bytes.full_traversal as f64}),
    );
    // Significantly smaller: at least a fifth of the full traversal, and
    // within the default budget whatever the case size.
    assert!(
        bytes.compact * 5 <= bytes.full_traversal,
        "{variant}: {bytes:?}"
    );
    assert!(bytes.compact <= 16_384, "{variant}: {bytes:?}");
    assert!(
        bytes.cold_path < bytes.full_traversal,
        "{variant}: {bytes:?}"
    );
    bytes
}

#[test]
fn drill_1_title_only_cold_start_explanatory() {
    let mut built = explanatory(false);
    cold_start(&mut built, "explanatory");
}

#[test]
fn drill_1_title_only_cold_start_evaluative() {
    let mut built = evaluative(false);
    cold_start(&mut built, "evaluative");
}

/// What drill 2 changes below the synopsis.
#[derive(Clone, Copy)]
enum Change {
    /// Withdraw the second retained source entirely.
    WithdrawSource,
    /// Tighten the hard criterion's threshold to this many seconds.
    Threshold(&'static str),
}

/// The search hit for one record.
fn hit<'a>(search: &'a Value, id: &str) -> &'a Value {
    search["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|result| result["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing from {search}"))
}

fn synopsis_omission(brief: &Value) -> (Value, Value) {
    let omission = brief["omissions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "authored_synopses")
        .unwrap_or_else(|| panic!("synopses are not counted: {brief}"));
    (omission["current"].clone(), omission["stale"].clone())
}

/// Drill 2 on a built fixture.
fn stale_synopsis(mut built: Built, variant: &str, change: Change) {
    let pinned = built.expected["stale_synopsis"].clone();
    let (synopses, checkpoint) = built.write_synopses();
    let texts: Vec<String> = built.expected["synopses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["text"].as_str().unwrap().to_owned())
        .collect();
    // The first synopsis is the once-good summary; the second is about a
    // record the change does not reach.
    let stale_text = texts[0].as_str();
    let stale = synopses[0].clone();
    let kept = synopses[1].clone();
    let subject = built
        .outcome(pinned["subject"].as_str().unwrap())
        .to_owned();
    let untouched = built
        .outcome(pinned["untouched_subject"].as_str().unwrap())
        .to_owned();
    let since = checkpoint["transaction_id"].as_str().unwrap().to_owned();
    let drill = &built.drill;

    // ---- before: the synopsis is current and the answer reads well
    let before = drill.show(&subject, "summary").0;
    assert_eq!(before["authored_synopsis"]["state"], "current", "{variant}");
    assert_eq!(before["authored_synopsis"]["latest"]["text"], stale_text);
    let overview = drill.vault_overview();
    assert!(
        overview.contains("- Summary: `summary_current`: the authored synopsis [["),
        "{variant}\n{overview}"
    );
    assert!(
        overview.contains("> [!quote] Authored synopsis (editorial, not evidence)"),
        "{variant}"
    );
    let brief = drill.brief(&[]).0;
    assert_eq!(brief["outcomes"][0]["id"], subject.as_str(), "{variant}");
    assert_eq!(
        synopsis_omission(&brief),
        (json!(texts.len()), json!(0)),
        "{variant}"
    );

    // ---- the change below the synopsis
    let cause = match change {
        Change::WithdrawSource => {
            let source = built.retained[1].source.clone();
            built.drill.record(vec![(
                "source.withdraw",
                json!({"source_ref":exact(&source, 1),"reason":"collection_error",
                    "scope":{"extent":"entire"},
                    "rationale":"The after-change log was captured on a staging host, not the export host.",
                    "evidence_refs":[]}),
            )]);
            source
        }
        Change::Threshold(seconds) => {
            let criterion = built.named("Recovery time").to_owned();
            let question = built
                .named("Which queue should the application use?")
                .to_owned();
            built.drill.record(vec![(
                "criterion.amend",
                json!({"criterion_ref":exact(&criterion, 1),
                    "reason":"The service owner tightened the recovery limit.",
                    "replacement":{"kind":"hard","name":"Recovery time","preferred_direction":"lower",
                        "scale":{"anchors":[],"description":"Recovery time","unit":"seconds"},
                        "scope":{"kind":"all_options","question_ref":exact(&question, 1)},
                        "threshold":{"kind":"measured","operator":"at_most",
                            "value":{"decimal":seconds,"unit":"seconds"}}}}),
            )]);
            criterion
        }
    };
    let drill = &built.drill;
    let absent = |channel: &str, output: &str| {
        assert!(
            !output.contains(stale_text),
            "{variant}: {channel} presents the stale synopsis\n{output}"
        );
    };

    // ---- the default brief, and every level, never carries it
    let (brief, bytes) = drill.brief(&[]);
    println!("drill 2 {variant}: default brief after the change {bytes} B");
    absent("brief", &brief.to_string());
    let lead = &brief["outcomes"][0];
    assert_eq!(lead["id"], subject.as_str(), "{variant}");
    assert_eq!(
        lead["freshness"]["effective"], "needs_review",
        "{variant} {lead}"
    );
    assert_eq!(
        lead["freshness"]["label"], pinned["freshness_after"],
        "{variant}"
    );
    assert_eq!(
        lead["freshness"]["reasons"][0]["cause"]["id"],
        cause.as_str(),
        "{variant}"
    );
    assert_eq!(brief["freshness"]["effective"], "needs_review", "{variant}");
    assert_eq!(
        synopsis_omission(&brief),
        (json!(texts.len() - 1), json!(1)),
        "{variant}"
    );
    assert_eq!(
        entry(&brief, &untouched)["freshness"]["state"],
        "current",
        "{variant}"
    );
    for level in ["compact", "standard", "deep"] {
        let (output, _) = drill.brief(&["--level", level, "--max-bytes", "1000000"]);
        absent(level, &output.to_string());
        let shown = &entry(&output, &subject)["freshness"];
        assert_eq!(
            shown["label"], pinned["freshness_after"],
            "{variant} {level}"
        );
        assert_eq!(shown["state"], "stale", "{variant} {level}");
    }
    absent("brief text", &drill.text(&["brief"]));
    let delta = drill.brief(&["--since", &since]).0;
    absent("brief --since", &delta.to_string());
    let reported = &delta["stale_synopses"][0];
    assert_eq!(reported["id"], stale.as_str(), "{variant} {delta}");
    assert_eq!(reported["state"], "stale");
    assert_eq!(reported["before"], "current");
    assert_eq!(
        delta["stale_synopses"].as_array().unwrap().len(),
        1,
        "{variant}"
    );

    // ---- show falls back to the projection of typed fields
    let summary = drill.show(&subject, "summary").0;
    absent("show summary", &summary.to_string());
    let block = &summary["authored_synopsis"];
    assert_eq!(block["state"], "stale", "{variant}");
    assert_eq!(block["fallback"], "deterministic_projection");
    assert_eq!(block["latest"]["annotation_ref"], exact(&stale, 1));
    assert_eq!(summary["summary_basis"]["kind"], "typed_fields");
    assert_eq!(summary["summary"], pinned["summary"], "{variant}");
    absent("show title", &drill.show(&subject, "title").0.to_string());
    let text = drill.text(&["show", &subject]);
    absent("show text", &text);
    assert!(
        text.contains("Authored synopsis [editorial_summary, stale]: not shown"),
        "{variant}\n{text}"
    );
    assert_eq!(
        drill.show(&untouched, "summary").0["authored_synopsis"]["state"],
        "current",
        "{variant}"
    );

    // ---- full depth keeps the history and names the change
    let full = drill.show(&subject, "full").0;
    let block = &full["authored_synopsis"];
    assert_eq!(block["state"], "stale");
    assert_eq!(block["latest"]["text"], stale_text);
    assert_eq!(
        block["latest"]["reasons"][0]["cause"]["id"],
        cause.as_str(),
        "{variant} {block}"
    );
    assert_eq!(
        full["freshness"]["reasons"][0]["cause"]["id"],
        cause.as_str(),
        "{variant}"
    );
    let annotation = drill.show(&stale, "full").0;
    assert_eq!(annotation["record"]["body"]["content"]["text"], stale_text);
    assert_eq!(
        annotation["synopsis_binding"]["state"], "stale",
        "{variant}"
    );
    assert_eq!(
        drill.show(&kept, "summary").0["synopsis_binding"]["state"],
        "current"
    );
    // ---- the evidence layer: the recorded chain names the change, and the
    // exact retained bytes stay readable with their current standing
    let explained = drill.json(&["explain", &subject, "--max-bytes", "1000000"]);
    assert!(
        explained["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["section"] != "nodes"),
        "{variant}: {}",
        explained["omitted"]
    );
    let root = &explained["subject"];
    assert_eq!(
        root["freshness"]["reasons"][0]["cause"]["id"],
        cause.as_str(),
        "{variant}"
    );
    let nodes = explained["nodes"].as_array().unwrap();
    let node = |id: &str| -> &Value {
        nodes
            .iter()
            .find(|node| node["id"] == id)
            .unwrap_or_else(|| panic!("{variant}: {id} not walked"))
    };
    let body = &full["record"]["body"];
    let cited = body
        .get("support_refs")
        .unwrap_or(&body["evidence_refs"])
        .as_array()
        .unwrap();
    match change {
        Change::WithdrawSource => {
            // Every observation the subject rests on is no longer usable.
            for reference in cited {
                let use_ = &node(reference["id"].as_str().unwrap())["evidentiary_use"];
                assert_eq!(use_["state"], "withdrawn", "{variant} {use_}");
                assert_eq!(
                    use_["withdrawals"][0]["record"]["id"],
                    cause.as_str(),
                    "{variant}"
                );
            }
        }
        Change::Threshold(seconds) => {
            // The decision cites the criterion's superseded revision, and the
            // walked criterion is the amended one.
            let link = root["links"]
                .as_array()
                .unwrap()
                .iter()
                .find(|link| link["field"] == "hard_conditions.criterion_ref")
                .unwrap_or_else(|| panic!("{variant}: {root}"));
            assert_eq!(link["ref"], exact(&cause, 1), "{variant}");
            assert_eq!(link["link_state"], "stale", "{variant}");
            assert_eq!(link["current_revision"], 2, "{variant}");
            assert_eq!(node(&cause)["basis_revision"], 2, "{variant}");
            let amended = drill.show(&cause, "full").0;
            assert_eq!(
                amended["record"]["body"]["threshold"]["value"]["decimal"], seconds,
                "{variant}"
            );
        }
    }
    let first = node(cited[0]["id"].as_str().unwrap());
    let request = &first["evidence"]["evidence_read"];
    let argv: Vec<&str> = request["argv"].as_array().unwrap()[1..]
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let format = argv.iter().position(|arg| *arg == "--format").unwrap();
    let read = drill
        .call_in(&drill.root, &argv[..format], Some(&request["stdin"]), 0)
        .0;
    assert_eq!(read["excerpt"]["availability"], "text", "{variant}");
    assert_eq!(
        read["excerpt"]["text"].as_str().unwrap().trim_end(),
        pinned["evidence_excerpt"],
        "{variant}"
    );
    assert_eq!(
        read["evidentiary_use"]["state"], pinned["evidence_use"],
        "{variant} {read}"
    );
    absent("explain", &explained.to_string());

    // ---- search labels it stale and never counts it as a source hit
    let word = pinned["search_term"].as_str().unwrap();
    let found = drill.json(&["search", word]);
    assert_eq!(found["counts"]["source_hits"], 0, "{variant}");
    let result = hit(&found, &stale);
    assert_eq!(result["synopsis_binding"]["state"], "stale", "{variant}");

    // ---- the vault Overview synopsis ladder falls back and says why
    let overview = drill.vault_overview();
    let answer = overview
        .split("\n## Answer and caveat\n")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .unwrap();
    assert!(!answer.contains("`summary_current`"), "{variant}\n{answer}");
    assert!(
        answer.contains("- Summary: `summary_stale`: the newest authored synopsis [["),
        "{variant}\n{answer}"
    );
    assert!(
        answer.contains("falls back to the deterministic projection of typed fields"),
        "{variant}"
    );
    assert!(
        !overview.contains("[!quote] Authored synopsis"),
        "{variant}\n{overview}"
    );
    assert!(
        answer.contains(pinned["overview_reason"].as_str().unwrap()),
        "{variant}\n{answer}"
    );
    if std::env::var_os("DRILL_DUMP").is_some() {
        println!("{overview}");
    }
}

#[test]
fn drill_2_stale_synopsis_after_a_source_withdrawal_explanatory() {
    stale_synopsis(explanatory(true), "explanatory", Change::WithdrawSource);
}

#[test]
fn drill_2_stale_synopsis_after_a_threshold_change_evaluative() {
    stale_synopsis(evaluative(true), "evaluative", Change::Threshold("100"));
}
