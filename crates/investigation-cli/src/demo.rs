//! `demo PATH --scenario NAME`: a reproducible tutorial workspace.
//!
//! A scenario is one of the checked-in fixture workspaces under
//! `fixtures/vault-goldens/`, built by the scenario proofs
//! (`scripts/vault-goldens.py --rebuild-workspaces`) and embedded by
//! `build.rs`. `demo` does not copy their files: it creates a new workspace
//! and submits each committed transaction again, in commit order, through the
//! same store entry points `record` and `ingest` use. A replay seam
//! ([`investigation_core::replay_seams`]) holds the clock at each
//! transaction's recorded time and draws IDs from the scenario's seed, so
//! identical fixtures always give identical canonical bytes.
//!
//! The new IDs differ from the fixture's, so every later request is rewritten
//! before it is submitted: each record ID, journal head hash, manifest digest
//! and review basis digest the fixture's journal holds is mapped to the one
//! the replay produced at the same place. A request the current core rejects
//! stops the demo with the transaction it stopped at.
//!
//! The replay stops before the first transaction that closes a case, so the
//! tutorial case keeps one deliberate step open. The tutorial path that
//! follows (plan §17.5) lives in [`tutorial`], the one place it is written:
//! `demo --help`, the command's output and the tests all read it.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    ops::Range,
    path::{Component, Path, PathBuf},
};

use investigation_core::{
    PROGRAM_NAME,
    basis::basis_digest,
    header::{Actor, UtcTimestamp},
    identity::{Id, Kind},
    replay_seams::Replay,
    state::Record,
    wire::{ErrorCode, Failure, Receipt},
};
use investigation_store::{
    Store, Workspace,
    journal::hash_bytes,
    store::ingest::producer_run::ProducerRunSubmission,
    workspace::{DemoMarker, WorkspaceError},
};
use serde_json::{Value, json};

use crate::{Output, display_argv, selection::workspace_failure};

mod fixtures {
    include!(concat!(env!("OUT_DIR"), "/demo_fixtures.rs"));
}

/// One tutorial scenario and the fixture it replays.
pub(crate) struct Scenario {
    pub(crate) name: &'static str,
    title: &'static str,
    /// The fixture directory, relative to the repository.
    fixture: &'static str,
    files: &'static [(&'static str, &'static [u8])],
    seed: u64,
    /// The operation the open step needs; the tutorial prints its schema.
    pub(crate) operation: &'static str,
    /// The export the tutorial ends with.
    pub(crate) export: &'static str,
    open_step: &'static str,
}

pub(crate) const SCENARIOS: [Scenario; 4] = [
    Scenario {
        name: "debug",
        title: "intermittent request timeout (plan §17.1)",
        fixture: "fixtures/vault-goldens/scenario-a",
        files: fixtures::SCENARIO_A,
        seed: 0x5eed_0a17,
        operation: "retrospective.record",
        export: "case-report",
        open_step: "The validated root cause awaits its attributed retrospective and case closure.",
    },
    Scenario {
        name: "performance",
        title: "p99 latency regression (plan §17.2)",
        fixture: "fixtures/vault-goldens/scenario-b",
        files: fixtures::SCENARIO_B,
        seed: 0x5eed_0b17,
        operation: "retrospective.record",
        export: "case-report",
        open_step: "The bounded conclusions await the attributed retrospective and case closure.",
    },
    Scenario {
        name: "research",
        title: "contradictory source research (plan §17.4)",
        fixture: "fixtures/vault-goldens/scenario-d",
        files: fixtures::SCENARIO_D,
        seed: 0x5eed_0d17,
        operation: "retrospective.record",
        export: "case-report",
        open_step: "The resolved question awaits a retrospective on why downstream reports added no independent support.",
    },
    Scenario {
        name: "choice",
        title: "architecture choice (plan §17.3)",
        fixture: "fixtures/vault-goldens/scenario-c",
        files: fixtures::SCENARIO_C,
        seed: 0x5eed_0c17,
        operation: "comparison.add",
        export: "decision-record",
        open_step: "The superseding decision after the changed hard threshold awaits the retrospective and case closure; unknown comparison cells stay unknown.",
    },
];

fn scenario(name: &str) -> &'static Scenario {
    SCENARIOS
        .iter()
        .find(|scenario| scenario.name == name)
        .expect("clap accepts only scenario names")
}

#[derive(clap::Args)]
pub(crate) struct Args {
    /// New or empty directory for the tutorial workspace; its parent must exist.
    #[arg(value_name = "PATH")]
    path: PathBuf,
    /// Which worked case to generate.
    #[arg(long, value_name = "NAME", value_parser = clap::builder::PossibleValuesParser::new(SCENARIOS.map(|s| s.name)))]
    scenario: String,
}

/// The tutorial path for `scenario` in the workspace `path`: `demo` itself,
/// then the read-only and owned-output commands that explore it. For
/// `--scenario choice` in `./demo-case` this is plan §17.5 word for word.
pub(crate) fn tutorial(path: &str, scenario: &Scenario) -> Vec<Vec<String>> {
    let at = |rest: &[&str]| {
        let mut argv = vec![PROGRAM_NAME.to_owned(), "--workspace".into(), path.into()];
        argv.extend(rest.iter().map(|part| (*part).to_owned()));
        argv
    };
    vec![
        vec![
            PROGRAM_NAME.into(),
            "demo".into(),
            path.into(),
            "--scenario".into(),
            scenario.name.into(),
        ],
        at(&["onboard", "--format", "json"]),
        at(&["brief", "--format", "json"]),
        at(&["schema", scenario.operation, "--format", "json"]),
        at(&["next", "--format", "json"]),
        at(&["check", "--format", "json"]),
        at(&["vault", "render", "--format", "json"]),
        at(&[
            "export",
            "--kind",
            scenario.export,
            "--audience",
            "internal",
        ]),
    ]
}

/// `demo --help` text after the options.
pub(crate) fn after_help() -> String {
    let mut text = String::from(
        "\
Replays a checked-in scenario's committed transactions through the normal
commit path, with the clock held at each transaction's recorded time and IDs
drawn from the scenario's seed, so the same scenario always gives the same
canonical bytes. The case has real IDs and one deliberate step left open.
PATH must be new or empty and its parent must exist; nothing is written
outside PATH. The workspace marker records that it is a demo.

Scenarios:
",
    );
    for scenario in &SCENARIOS {
        text.push_str(&format!(
            "  {:<13}{}, from {}\n",
            scenario.name, scenario.title, scenario.fixture
        ));
    }
    text.push_str("\nExamples: the shipped tutorial path (plan §17.5), run in order:\n");
    let steps = tutorial("./demo-case", scenario("choice"));
    for argv in &steps {
        text.push_str(&format!("  {}\n", display_argv(argv)));
    }
    text.push_str(
        "Every step after demo reads, or writes only the owned vault or a new export;\n\
         none records, validates or promotes anything.\n\n",
    );
    text.push_str(&format!("Next: {}", display_argv(&steps[1])));
    text
}

pub(crate) fn run(args: Args) -> Result<Output, Failure> {
    let scenario = scenario(&args.scenario);
    let fixture = Fixture::load(scenario)?;
    let shown = args.path.to_string_lossy().into_owned();
    let first = fixture
        .events
        .first()
        .ok_or_else(|| integrity(scenario, "The fixture has no transactions."))?;
    let replay = Replay::install(scenario.seed, first.recorded_at.clone())
        .map_err(|_| integrity(scenario, "A replay is already running on this thread."))?;
    let marker = DemoMarker {
        scenario: scenario.name.into(),
        fixture: scenario.fixture.into(),
        seed: scenario.seed,
    };
    let workspace = Workspace::init_demo(&args.path, fixture.config, marker).map_err(|error| {
        match error {
            WorkspaceError::WorkspaceAlreadyExists(_) => Failure::new(
                ErrorCode::InvalidInput,
                format!(
                    "{shown} is not empty. demo writes only a new or empty directory; choose another PATH."
                ),
            ),
            WorkspaceError::InvalidRoot(_) => Failure::new(
                ErrorCode::InvalidInput,
                format!(
                    "The parent directory of {shown} does not exist. demo writes nothing outside PATH; create the parent first."
                ),
            ),
            error => workspace_failure(error),
        }
        .at("/path")
    })?;
    let root = workspace.root().to_path_buf();
    let mut map = Mapping::default();
    map.learn(&fixture.workspace_id, &workspace.id().to_string())
        .map_err(|message| integrity(scenario, &message))?;
    let mut store = Store::open(workspace);
    let mut replayed = 0;
    let mut first_case = None;
    for event in fixture
        .events
        .iter()
        .take_while(|event| !event.closes_case())
    {
        replay.set_now(event.recorded_at.clone());
        let stopped = |message: String| {
            integrity(
                scenario,
                &format!(
                    "Replay stopped at transaction {} ({}): {message}",
                    replayed + 1,
                    event.command_id
                ),
            )
        };
        let input = map.rewrite(&event.value["request"]["input"]);
        let (receipt, case) = submit(&mut store, &root, &fixture, event, &input)
            .map_err(|failure| stopped(failure.error.message))?;
        let case = match map.get(&event.case_id) {
            Some(case) => case
                .parse::<Id>()
                .map_err(|error| stopped(error.to_string()))?,
            None => case
                .or_else(|| {
                    receipt
                        .created
                        .iter()
                        .map(|change| change.id)
                        .find(|id| id.kind() == Kind::Case)
                })
                .ok_or_else(|| stopped("no case for the transaction".into()))?,
        };
        first_case.get_or_insert(case);
        let path = root.join(format!(
            "state/cases/{case}/events/{:012}.json",
            receipt.snapshot.sequence
        ));
        let bytes = fs::read(&path).map_err(|error| stopped(error.to_string()))?;
        let committed: Value =
            serde_json::from_slice(&bytes).map_err(|error| stopped(error.to_string()))?;
        map.learn_event(event, &committed, &bytes)
            .map_err(stopped)?;
        replayed += 1;
    }
    drop(replay);
    let case = first_case.ok_or_else(|| integrity(scenario, "No transaction was replayed."))?;
    let steps = tutorial(&shown, scenario);
    let mut text = format!(
        "demo  {}  {}\nfixture  {}  replayed {replayed} transactions  seed {:#x}\ncase  {case}\nopen step  {}\n\nTutorial:\n",
        scenario.name,
        root.display(),
        scenario.fixture,
        scenario.seed,
        scenario.open_step,
    );
    for argv in &steps[1..] {
        text.push_str(&format!("  {}\n", display_argv(argv)));
    }
    text.push_str(&format!("Next: {}", display_argv(&steps[1])));
    let value = json!({
        "workspace": root,
        "workspace_id": store.workspace().id(),
        "scenario": scenario.name,
        "fixture": scenario.fixture,
        "seed": scenario.seed,
        "transactions": replayed,
        "case_id": case,
        "open_step": scenario.open_step,
        "tutorial": &steps[1..],
        "next": [{"kind": "local_read", "argv": &steps[1]}],
    });
    Ok(Output::success("demo_created", value, text))
}

fn integrity(scenario: &Scenario, message: &str) -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        format!(
            "The {} demo could not be built from {}. {message}",
            scenario.name, scenario.fixture
        ),
    )
}

/// One committed transaction of the fixture.
struct Event {
    value: Value,
    bytes: &'static [u8],
    recorded_at: UtcTimestamp,
    case_id: String,
    sequence: u64,
    command_id: String,
}

impl Event {
    fn closes_case(&self) -> bool {
        self.value["request"]["input"]["operations"]
            .as_array()
            .is_some_and(|operations| {
                operations
                    .iter()
                    .any(|operation| operation["op"] == "case.close")
            })
    }
}

struct Fixture {
    config: &'static str,
    workspace_id: String,
    events: Vec<Event>,
    blobs: HashMap<String, &'static [u8]>,
}

impl Fixture {
    fn load(scenario: &Scenario) -> Result<Self, Failure> {
        let broken = |message: &str| integrity(scenario, message);
        let mut config = None;
        let mut workspace_id = None;
        let mut events = Vec::new();
        let mut blobs = HashMap::new();
        for (path, bytes) in scenario.files {
            if *path == "investigation.toml" {
                config = std::str::from_utf8(bytes).ok();
            } else if *path == "state/workspace.json" {
                let marker: Value = serde_json::from_slice(bytes)
                    .map_err(|_| broken("Its workspace marker is not JSON."))?;
                workspace_id = marker["workspace_id"].as_str().map(str::to_owned);
            } else if let Some(name) = path.strip_prefix("state/blobs/sha256/") {
                let hex = name.rsplit('/').next().unwrap_or_default();
                blobs.insert(format!("sha256:{hex}"), *bytes);
            } else if path.contains("/events/") {
                let value: Value = serde_json::from_slice(bytes)
                    .map_err(|_| broken(&format!("{path} is not JSON.")))?;
                let case_id = path.split('/').nth(2).unwrap_or_default().to_owned();
                let recorded_at = value["recorded_at"]
                    .as_str()
                    .and_then(|text| text.parse().ok())
                    .ok_or_else(|| broken(&format!("{path} has no recorded_at.")))?;
                events.push(Event {
                    sequence: value["sequence"].as_u64().unwrap_or_default(),
                    command_id: value["command_id"].as_str().unwrap_or_default().to_owned(),
                    value,
                    bytes,
                    recorded_at,
                    case_id,
                });
            }
        }
        // Commit order across cases: recorded time, then the case's own order.
        events.sort_by(|a, b| {
            (a.recorded_at.to_string(), &a.case_id, a.sequence).cmp(&(
                b.recorded_at.to_string(),
                &b.case_id,
                b.sequence,
            ))
        });
        Ok(Self {
            config: config.ok_or_else(|| broken("It has no investigation.toml."))?,
            workspace_id: workspace_id.ok_or_else(|| broken("It has no workspace marker."))?,
            events,
            blobs,
        })
    }

    fn blob(&self, digest: &str) -> Result<&'static [u8], Failure> {
        self.blobs.get(digest).copied().ok_or_else(|| {
            Failure::new(
                ErrorCode::IntegrityFailure,
                format!("The fixture retains no blob {digest}."),
            )
        })
    }
}

/// Submit one rewritten request through the entry point its command used.
/// Returns the receipt and, for a producer run, the case it names.
fn submit(
    store: &mut Store,
    root: &Path,
    fixture: &Fixture,
    event: &Event,
    input: &Value,
) -> Result<(Receipt, Option<Id>), Failure> {
    if input["schema_version"] == "inv.command.v1" {
        let bytes = serde_json::to_vec(input).expect("JSON serializes");
        return store.record(&bytes).map(|receipt| (receipt, None));
    }
    let mut staged = Staged::new(root);
    let attachments: Vec<&str> = event.value["request"]["attachments"]
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if let Some(envelope) = input.get("producer_run") {
        // The declared artifacts are read from the workspace, as the
        // scenario proof placed them; only retained ones can be replayed.
        for artifact in envelope["artifacts"].as_array().into_iter().flatten() {
            let (Some(path), Some(hex)) = (
                artifact["path"].as_str(),
                artifact["expected_sha256"].as_str(),
            ) else {
                continue;
            };
            let digest = format!("sha256:{hex}");
            if attachments.contains(&digest.as_str()) {
                staged.write(path, fixture.blob(&digest)?)?;
            }
        }
        let actor: Actor = serde_json::from_value(input["actor"].clone())
            .map_err(|error| invalid(&error.to_string()))?;
        let mut submission = ProducerRunSubmission::new(actor);
        submission.case_id = input["case_id"]
            .as_str()
            .map(str::parse)
            .transpose()
            .map_err(|_| invalid("The producer run names no valid case."))?;
        let bytes = serde_json::to_vec(envelope).expect("JSON serializes");
        let receipt = store.ingest_producer_run(&bytes, &submission)?;
        return Ok((receipt.receipt, Some(receipt.case_id)));
    }
    if input.get("content").is_some() {
        return Err(invalid(
            "Replaying a standard-input ingest is not supported.",
        ));
    }
    // Retained paths, in order, are the transaction's attachments.
    let paths = input["paths"].as_array().cloned().unwrap_or_default();
    if input["retention"] != "reference" {
        for (path, digest) in paths.iter().zip(&attachments) {
            let path = path
                .as_str()
                .ok_or_else(|| invalid("An ingest path is not text."))?;
            staged.write(path, fixture.blob(digest)?)?;
        }
    }
    let bytes = serde_json::to_vec(input).expect("JSON serializes");
    store
        .ingest(&bytes, None)
        .map(|receipt| (receipt.receipt, None))
}

fn invalid(message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message.to_owned())
}

/// Source files placed in the new workspace for one ingest and removed
/// afterwards, whatever its outcome.
struct Staged<'a> {
    root: &'a Path,
    files: Vec<PathBuf>,
}

impl<'a> Staged<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            files: Vec::new(),
        }
    }

    fn write(&mut self, relative: &str, bytes: &[u8]) -> Result<(), Failure> {
        let relative = Path::new(relative);
        let plain = relative
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
            && relative.components().count() > 0;
        let mut components = relative.components().filter(|c| *c != Component::CurDir);
        let single = components.next().is_some() && components.next().is_none();
        if !plain || !single {
            return Err(invalid(&format!(
                "Fixture path {} is not a file name in the workspace.",
                relative.display()
            )));
        }
        let path = self.root.join(relative);
        let io = |error: std::io::Error| {
            Failure::new(
                ErrorCode::IoFailure,
                format!("Stage {}: {error}", path.display()),
            )
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(io)?;
        self.files.push(path.clone());
        std::io::Write::write_all(&mut file, bytes).map_err(io)
    }
}

impl Drop for Staged<'_> {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = fs::remove_file(file);
        }
    }
}

/// Fixture identity to replayed identity: record IDs, journal hashes and
/// digests, learned from each committed transaction.
#[derive(Default)]
struct Mapping(HashMap<String, String>);

impl Mapping {
    fn get(&self, token: &str) -> Option<&String> {
        self.0.get(token)
    }

    fn learn(&mut self, fixture: &str, replayed: &str) -> Result<(), String> {
        if fixture == replayed {
            return Ok(());
        }
        match self.0.get(fixture) {
            Some(known) if known != replayed => Err(format!(
                "the fixture's {fixture} replayed as both {known} and {replayed}"
            )),
            _ => {
                self.0.insert(fixture.to_owned(), replayed.to_owned());
                Ok(())
            }
        }
    }

    /// Learn from one committed transaction: every token at the same place
    /// in the two events, the event's own hash, and the review basis digest
    /// of every record it accepted.
    fn learn_event(
        &mut self,
        event: &Event,
        committed: &Value,
        bytes: &[u8],
    ) -> Result<(), String> {
        self.align(&event.value, committed)?;
        self.learn(hash_bytes(event.bytes).as_str(), hash_bytes(bytes).as_str())?;
        let records = |value: &Value| {
            value["accepted_operations"]["records"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        };
        for (fixture, replayed) in records(&event.value).iter().zip(records(committed).iter()) {
            let digest = |value: &Value| {
                serde_json::from_value::<Record>(value.clone())
                    .ok()
                    .and_then(|record| basis_digest(&record).ok())
            };
            if let (Some(fixture), Some(replayed)) = (digest(fixture), digest(replayed)) {
                self.learn(fixture.as_str(), replayed.as_str())?;
            }
        }
        Ok(())
    }

    fn align(&mut self, fixture: &Value, replayed: &Value) -> Result<(), String> {
        match (fixture, replayed) {
            (Value::String(fixture), Value::String(replayed)) => {
                let (a, b) = (tokens(fixture), tokens(replayed));
                if a.len() == b.len() {
                    for (a, b) in a.into_iter().zip(b) {
                        self.learn(&fixture[a], &replayed[b])?;
                    }
                }
                Ok(())
            }
            (Value::Array(fixture), Value::Array(replayed)) if fixture.len() == replayed.len() => {
                fixture
                    .iter()
                    .zip(replayed)
                    .try_for_each(|(a, b)| self.align(a, b))
            }
            (Value::Object(fixture), Value::Object(replayed)) => {
                let mut unmatched: BTreeMap<&String, &Value> = BTreeMap::new();
                let mut left = Vec::new();
                for (key, value) in fixture {
                    match replayed.get(&self.rewrite_text(key)) {
                        Some(other) => self.align(value, other)?,
                        None => left.push((key, value)),
                    }
                }
                for (key, value) in replayed {
                    if !fixture.keys().any(|k| self.rewrite_text(k) == *key) {
                        unmatched.insert(key, value);
                    }
                }
                // Keys that are themselves new IDs pair up in order.
                if left.len() == unmatched.len() {
                    for ((key, value), (other_key, other)) in left.into_iter().zip(unmatched) {
                        self.align(
                            &Value::String(key.clone()),
                            &Value::String(other_key.clone()),
                        )?;
                        self.align(value, other)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn rewrite(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.rewrite_text(text)),
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| self.rewrite(item)).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| (self.rewrite_text(key), self.rewrite(value)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn rewrite_text(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for range in tokens(text) {
            out.push_str(&text[at..range.start]);
            let token = &text[range.clone()];
            out.push_str(self.0.get(token).map_or(token, String::as_str));
            at = range.end;
        }
        out.push_str(&text[at..]);
        out
    }
}

/// Byte ranges of the identity tokens in `text`: record IDs such as
/// `obs_<uuid>` and digests such as `sha256:<64 hex>`.
fn tokens(text: &str) -> Vec<Range<usize>> {
    const DIGEST: &[u8] = b"sha256:";
    let bytes = text.as_bytes();
    let hex = |b: &u8| b.is_ascii_digit() || (b'a'..=b'f').contains(b);
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let boundary = i == 0 || !word(bytes[i - 1]);
        if boundary && bytes[i..].starts_with(DIGEST) {
            let end = i + DIGEST.len() + 64;
            if end <= bytes.len()
                && bytes[i + DIGEST.len()..end].iter().all(hex)
                && bytes.get(end).is_none_or(|b| !word(*b))
            {
                found.push(i..end);
                i = end;
                continue;
            }
        }
        if boundary && bytes[i].is_ascii_lowercase() {
            let prefix = bytes[i..]
                .iter()
                .take_while(|b| b.is_ascii_lowercase())
                .count();
            let end = i + prefix + 1 + 36;
            if bytes.get(i + prefix) == Some(&b'_')
                && end <= bytes.len()
                && bytes.get(end).is_none_or(|b| !word(*b))
                && text.get(i..end).is_some_and(|id| id.parse::<Id>().is_ok())
            {
                found.push(i..end);
                i = end;
                continue;
            }
            i += prefix;
            continue;
        }
        i += 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_only::{EFFECTS, Effect};

    #[test]
    fn tokens_find_ids_and_digests_inside_text() {
        let id = "obs_01a20751-8ff0-71b3-8533-fa18bea0abb8";
        let other = "assess_01a20a5f-d58a-73c2-8ace-4e610366c5d3";
        let digest = format!("sha256:{}", "ab".repeat(32));
        let text = format!("gap:rule:{id}+{other} {digest} environment_differs xobs_1");
        let found: Vec<_> = tokens(&text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(found, [id, other, digest.as_str()]);
        let mut map = Mapping::default();
        map.learn(id, "obs_01a20751-8ff0-71b3-8533-000000000001")
            .unwrap();
        assert!(
            map.learn(id, "obs_01a20751-8ff0-71b3-8533-000000000002")
                .is_err()
        );
        assert_eq!(
            map.rewrite_text(&format!("{id}+{other}")),
            format!("obs_01a20751-8ff0-71b3-8533-000000000001+{other}")
        );
    }

    #[test]
    fn choice_tutorial_in_demo_case_is_plan_section_17_5() {
        let plan = include_str!("../../../notes/plan.md");
        let section = plan
            .split("### 17.5 Shipped tutorial command path")
            .nth(1)
            .and_then(|rest| rest.split("```sh").nth(1))
            .and_then(|rest| rest.split("```").next())
            .expect("plan §17.5 has a shell block");
        let planned: Vec<String> = section
            .lines()
            .filter_map(|line| line.strip_prefix("inv "))
            // A trailing shell comment (an erratum note) is not part of the command.
            .map(|line| {
                format!(
                    "{PROGRAM_NAME} {}",
                    line.split(" #").next().unwrap_or(line).trim_end()
                )
            })
            .collect();
        let shipped: Vec<String> = tutorial("./demo-case", scenario("choice"))
            .iter()
            .map(|argv| display_argv(argv))
            .collect();
        assert_eq!(planned, shipped);
    }

    /// No tutorial step after `demo` can record, validate or promote
    /// anything: each is a read, or writes only the owned vault projection or
    /// a new export directory, neither of which changes canonical state.
    #[test]
    fn no_tutorial_step_is_a_domain_write() {
        let owned_outputs = ["vault render", "export"];
        let commands = crate::help_teach::command_tree();
        for scenario in &SCENARIOS {
            for argv in &tutorial("./demo-case", scenario)[1..] {
                let matches = commands
                    .clone()
                    .try_get_matches_from(argv)
                    .unwrap_or_else(|error| panic!("{argv:?}: {error}"));
                let mut path = Vec::new();
                let mut current = &matches;
                while let Some((name, sub)) = current.subcommand() {
                    path.push(name);
                    current = sub;
                }
                let path = path.join(" ");
                let effect = EFFECTS
                    .iter()
                    .find(|(name, _)| *name == path)
                    .map(|(_, effect)| *effect)
                    .unwrap_or_else(|| panic!("{path} is unclassified"));
                assert!(
                    effect == Effect::Read || owned_outputs.contains(&path.as_str()),
                    "{}: tutorial step {path} is {effect:?}",
                    scenario.name
                );
            }
        }
    }
}
