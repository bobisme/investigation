//! Deterministic acceptance measures of plan §20.5 that earlier tests proved
//! only piecemeal, through the public binary. `scripts/acceptance-measures.py`
//! runs these with the earlier tests behind each measure and writes
//! `target/acceptance/deterministic-report.json`.
//!
//! - Targeting: a seeded fuzz sends every case-targeting write command with
//!   the right, another, an unknown, an ambiguous, a malformed or no case
//!   selector, and with objects of the named case, of another case or of no
//!   case. An independent journal oracle checks each call: a refusal writes
//!   nothing, and an accepted write lands only in the case it named (or, for
//!   a creating command, in a new case) and cites no record of another case.
//!   `INV_TARGETING_SEED=<u64>` replays one seed.
//! - Lost and duplicate writes: every write command is sent, its response
//!   dropped, and sent again; the journal holds it exactly once, the retry
//!   returns the IDs the journal holds, and a changed request under the same
//!   key writes nothing.
//! - Repairability: each ordinary error code is produced, shown to change
//!   nothing, and resolved by following the repair it returns.
//! - Domain neutrality: the scripted architecture and research walkthroughs
//!   use no security abstraction, method pack or test-run ceremony.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

#[path = "support/acceptance.rs"]
mod acceptance;
#[path = "support/fixtures.rs"]
mod fixtures;

const ACTOR: &str = "agent:acceptance";
const DEFAULT_SEED: u64 = 0x2026_1124_5eed;

// ---------------------------------------------------------------- harness

struct Call {
    exit: i32,
    body: Value,
}

impl Call {
    fn accepted(&self) -> bool {
        self.exit == 0 && self.body.get("ok") != Some(&Value::Bool(false))
    }

    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or("")
    }
}

fn invoke(cwd: &Path, args: &[String], stdin: Option<&[u8]>) -> Call {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_READ_ONLY")
        .env_remove("INVESTIGATION_SESSION")
        .env_remove("INVESTIGATION_LOG")
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    if let Some(bytes) = stdin {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let body = serde_json::from_str(stdout.trim()).unwrap_or_else(
        |_| json!({"unparsed": stdout, "stderr": String::from_utf8_lossy(&output.stderr)}),
    );
    Call {
        exit: output.status.code().unwrap_or(-1),
        body,
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

/// A workspace with one import root, driven only through the binary.
struct Casework {
    _directory: tempfile::TempDir,
    root: PathBuf,
    sources: PathBuf,
    files: usize,
}

impl Casework {
    fn new(vault: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let sources = directory.path().join("sources");
        fs::create_dir(&sources).unwrap();
        let mut args = strings(&["init", "ws", "--import-root"]);
        args.push(sources.display().to_string());
        if !vault {
            args.push("--no-vault".to_owned());
        }
        args.extend(strings(&["--format", "json"]));
        let init = invoke(directory.path(), &args, None);
        assert!(init.accepted(), "{}", init.body);
        Self {
            root: directory.path().join("ws"),
            sources,
            _directory: directory,
            files: 0,
        }
    }

    /// Run one command with this workspace, the test actor and JSON output.
    fn call(&self, args: &[String], stdin: Option<&[u8]>) -> Call {
        let mut full = vec![
            "--workspace".to_owned(),
            self.root.display().to_string(),
            "--actor".to_owned(),
            ACTOR.to_owned(),
        ];
        full.extend_from_slice(args);
        full.extend(strings(&["--format", "json"]));
        invoke(&self.root, &full, stdin)
    }

    fn ok(&self, args: &[&str], stdin: Option<&Value>) -> Value {
        let bytes = stdin.map(|value| serde_json::to_vec(value).unwrap());
        let call = self.call(&strings(args), bytes.as_deref());
        assert!(call.accepted(), "{args:?}: {}", call.body);
        call.body
    }

    /// A new file with unique bytes beneath the import root.
    fn source_file(&mut self, first_line: &str) -> PathBuf {
        self.files += 1;
        let path = self.sources.join(format!("source-{}.txt", self.files));
        fs::write(
            &path,
            format!("{first_line}\nshared line\nunique line {}\n", self.files),
        )
        .unwrap();
        path
    }

    fn case_ids(&self) -> Vec<String> {
        let mut cases: Vec<String> = fs::read_dir(self.root.join("state/cases"))
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                    .collect()
            })
            .unwrap_or_default();
        cases.sort();
        cases
    }

    /// Every case's event file names.
    fn journals(&self) -> BTreeMap<String, BTreeSet<String>> {
        self.case_ids()
            .into_iter()
            .map(|case| {
                let events = self.root.join("state/cases").join(&case).join("events");
                let names = fs::read_dir(events)
                    .map(|entries| {
                        entries
                            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                            .collect()
                    })
                    .unwrap_or_default();
                (case, names)
            })
            .collect()
    }

    fn event(&self, case: &str, name: &str) -> Value {
        let path = self
            .root
            .join("state/cases")
            .join(case)
            .join("events")
            .join(name);
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    /// Each record ID with the case whose journal created it.
    fn record_cases(&self) -> BTreeMap<String, String> {
        let mut found = BTreeMap::new();
        for (case, names) in self.journals() {
            for name in names {
                let event = self.event(&case, &name);
                for record in event["accepted_operations"]["records"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    if let Some(id) = record["header"]["id"].as_str() {
                        found.insert(id.to_owned(), case.clone());
                    }
                }
            }
        }
        found
    }

    /// Every file below the workspace except the disposable cache, with its
    /// length and bytes.
    fn files_snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut found = BTreeMap::new();
        let mut pending = vec![self.root.clone()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(&self.root).unwrap().to_path_buf();
                if relative.starts_with("cache") {
                    continue;
                }
                if path.is_dir() {
                    pending.push(path);
                } else {
                    found.insert(relative, fs::read(&path).unwrap());
                }
            }
        }
        found
    }
}

/// Every string in `value` shaped like a record ID: a lowercase kind, `_`,
/// and a hyphenated 36-character UUID.
fn ids_in(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::String(text) => {
            if let Some((kind, uuid)) = text.split_once('_') {
                let shaped = !kind.is_empty()
                    && kind.bytes().all(|byte| byte.is_ascii_lowercase())
                    && uuid.len() == 36
                    && uuid.char_indices().all(|(index, character)| {
                        if [8, 13, 18, 23].contains(&index) {
                            character == '-'
                        } else {
                            character.is_ascii_hexdigit()
                        }
                    });
                if shaped {
                    out.insert(text.clone());
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| ids_in(item, out)),
        Value::Object(map) => map.values().for_each(|item| ids_in(item, out)),
        _ => {}
    }
}

/// splitmix64: a fixed, dependency-free sequence for a seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % bound as u64).unwrap()
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            items.swap(index, self.below(index + 1));
        }
    }
}

// ---------------------------------------------------------------- targeting

/// What the `--case` flag says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Selector {
    Omitted,
    Named,
    NamedPrefix,
    Other,
    Unknown,
    Ambiguous,
    Malformed,
}

const SELECTORS: [Selector; 7] = [
    Selector::Omitted,
    Selector::Named,
    Selector::NamedPrefix,
    Selector::Other,
    Selector::Unknown,
    Selector::Ambiguous,
    Selector::Malformed,
];

/// Which record a call refers to.
#[derive(Clone, Copy, Debug)]
enum Object {
    Named,
    Other,
    Unknown,
}

const OBJECTS: [Object; 3] = [Object::Named, Object::Other, Object::Unknown];

const UNKNOWN_CASE: &str = "case_00000000-0000-7000-8000-000000000000";
const UNKNOWN_OBSERVATION: &str = "obs_00000000-0000-7000-8000-000000000000";

/// The write commands the fuzz sends, by `read_only::EFFECTS` path.
const FUZZED: &[&str] = &[
    "observe",
    "note",
    "record",
    "ingest",
    "source add",
    "start",
    "case new",
    "capture begin",
    "capture append",
    "capture attach",
    "capture finalize",
    "vault snapshot-note",
    "vault reconcile",
];

/// Commands that may write but name no case to write into, with the reason.
const NOT_CASE_TARGETED: &[(&str, &str)] = &[
    ("init", "creates a new workspace, which holds no case yet"),
    (
        "demo",
        "replays a fixture into a new workspace at an empty path",
    ),
    (
        "archive restore",
        "publishes a verified archive as a new workspace",
    ),
    (
        "archive fork",
        "publishes a verified archive as a new workspace",
    ),
    (
        "repair index",
        "rebuilds the disposable index and records nothing; the fuzz runs it and checks every journal is unchanged",
    ),
    (
        "repair staging",
        "retires finalized capture staging and records nothing",
    ),
    (
        "repair journal",
        "restores the witnessed bytes of reformatted journal files across every case and records nothing; it takes no case and changes no content (canonical_journal.rs)",
    ),
    (
        "archive pins release",
        "removes stale archive pin records below local/pins, machine-local bookkeeping; it records nothing and names no case",
    ),
    (
        "vault render",
        "regenerates the vault projection and records nothing; the fuzz runs it and checks every journal is unchanged",
    ),
    (
        "vault copy-map",
        "creates one human-owned board file and records nothing",
    ),
    (
        "vault remove",
        "deletes generated vault files and records nothing",
    ),
    (
        "vault unrelease",
        "ends a release in the render manifest and renders the vault; records nothing",
    ),
    (
        "vault clean-conflicts",
        "--apply deletes set-aside edits under local/conflicts/, machine-local bookkeeping; records nothing",
    ),
    (
        "config set",
        "rewrites vault_policy.viewer_release in investigation.toml; records nothing",
    ),
    (
        "config unset",
        "removes vault_policy.viewer_release from investigation.toml; records nothing",
    ),
    (
        "export",
        "writes files outside canonical state and records nothing",
    ),
    (
        "migrate",
        "--apply changes the workspace format; case journals, blobs and IDs are never changed",
    ),
    (
        "integration apply",
        "changes one file the user names outside any workspace",
    ),
    (
        "integration remove",
        "changes one file the user names outside any workspace",
    ),
    (
        "obsidian setup",
        "--apply changes the viewer configuration folder and records nothing",
    ),
    (
        "retrospective",
        "--consult records a consultation of another case's admitted lesson in --case; lessons::lessons_cross_cases_only_by_admission_and_explicit_consultation covers it",
    ),
];

/// Commands `read_only::EFFECTS` declares able to write.
fn declared_writes() -> BTreeSet<String> {
    let source = fs::read_to_string(
        fixtures::repository().join("crates/investigation-cli/src/read_only.rs"),
    )
    .unwrap();
    let table = source
        .split("pub(crate) const EFFECTS")
        .nth(1)
        .and_then(|rest| rest.split("\n];").next())
        .expect("EFFECTS table");
    // Entries may wrap: join the table and split it at each tuple.
    let flat: String = table
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("( ", "(");
    let mut writes = BTreeSet::new();
    for entry in flat.split("(\"").skip(1) {
        let (name, rest) = entry.split_once('"').unwrap();
        let effect = rest.trim_start_matches([',', ' ']);
        if effect.starts_with("Effect::Write") || effect.starts_with("Effect::ReadUnlessFlag") {
            writes.insert(name.to_owned());
        }
    }
    assert!(writes.contains("record") && writes.contains("vault reconcile"));
    writes
}

/// What a call may do, from what it names.
#[derive(Debug, PartialEq, Eq)]
enum Allowed {
    Refuse,
    Case(String),
    NewCase,
}

struct Probe {
    family: &'static str,
    args: Vec<String>,
    stdin: Option<Vec<u8>>,
    selector: Selector,
    /// The case the request body or handle names.
    request_case: Option<String>,
    /// The case of the record the call refers to; `Some(None)` when that
    /// record exists in no case.
    object_case: Option<Option<String>>,
    creates_case: bool,
    /// A capture handle whose position advances when the call is accepted.
    handle: Option<String>,
}

struct Targeting {
    casework: Casework,
    rng: Rng,
    /// Per named case: its Source and one Observation.
    sources: BTreeMap<String, String>,
    observations: BTreeMap<String, String>,
    /// Open capture handles: handle, case, next position, attached keys.
    handles: Vec<(String, String, u64, Vec<String>)>,
    question: Value,
    counter: usize,
    probes: usize,
    accepted: usize,
    landed: usize,
    misdirected: usize,
    cross_case: usize,
    refusals_that_wrote: usize,
    by_family: BTreeMap<&'static str, (usize, usize)>,
    violations: Vec<String>,
}

impl Targeting {
    fn new(seed: u64) -> Self {
        let mut casework = Casework::new(true);
        let mut sources = BTreeMap::new();
        let mut observations = BTreeMap::new();
        for title in ["First question", "Second question"] {
            let file = casework.source_file(title);
            let started = casework.ok(
                &["start", title, "--attach", &file.display().to_string()],
                None,
            );
            let created = started["created"].as_array().unwrap();
            let find = |prefix: &str| {
                created
                    .iter()
                    .map(|item| item["id"].as_str().unwrap())
                    .find(|id| id.starts_with(prefix))
                    .unwrap()
                    .to_owned()
            };
            let case = find("case_");
            let source = find("src_");
            let observed = casework.ok(
                &[
                    "--case",
                    &case,
                    "observe",
                    "shared line",
                    "--source",
                    &source,
                ],
                None,
            );
            observations.insert(
                case.clone(),
                observed["created"][0]["id"].as_str().unwrap().to_owned(),
            );
            sources.insert(case, source);
        }
        let mut handles = Vec::new();
        for case in sources.keys() {
            let begun = casework.ok(
                &["capture", "begin", "--input", "-"],
                Some(&begin_request(&format!("handle-{case}"), Some(case))),
            );
            handles.push((
                begun["handle"].as_str().unwrap().to_owned(),
                case.clone(),
                1,
                Vec::new(),
            ));
        }
        let schema = casework.ok(&["schema", "question.create"], None);
        let question = schema["transaction_example"]["input"]["operations"][0]["payload"].clone();
        assert!(question.is_object(), "{schema}");
        Self {
            casework,
            rng: Rng(seed),
            sources,
            observations,
            handles,
            question,
            counter: 0,
            probes: 0,
            accepted: 0,
            landed: 0,
            misdirected: 0,
            cross_case: 0,
            refusals_that_wrote: 0,
            by_family: BTreeMap::new(),
            violations: Vec::new(),
        }
    }

    fn named_cases(&self) -> Vec<String> {
        self.sources.keys().cloned().collect()
    }

    fn key(&mut self) -> String {
        self.counter += 1;
        format!("targeting-{}", self.counter)
    }

    fn selector_value(&self, selector: Selector, named: &str, other: &str) -> Option<String> {
        let cases = self.casework.case_ids();
        match selector {
            Selector::Omitted => None,
            Selector::Named => Some(named.to_owned()),
            Selector::Other => Some(other.to_owned()),
            Selector::Unknown => Some(UNKNOWN_CASE.to_owned()),
            Selector::Malformed => Some("obs_12345".to_owned()),
            Selector::NamedPrefix => (named.len().min(12)..=named.len())
                .map(|end| &named[..end])
                .find(|prefix| cases.iter().filter(|case| case.starts_with(prefix)).count() == 1)
                .map(str::to_owned),
            Selector::Ambiguous => {
                let mut prefix = named.to_owned();
                while cases
                    .iter()
                    .filter(|case| case.starts_with(&prefix))
                    .count()
                    < 2
                {
                    prefix.pop();
                }
                Some(prefix)
            }
        }
    }

    fn object(&self, object: Object, named: &str, other: &str) -> (String, Option<String>) {
        match object {
            Object::Named => (self.observations[named].clone(), Some(named.to_owned())),
            Object::Other => (self.observations[other].clone(), Some(other.to_owned())),
            Object::Unknown => (UNKNOWN_OBSERVATION.to_owned(), None),
        }
    }

    /// One probe of `family` naming case `named`.
    fn build(
        &mut self,
        family: &'static str,
        selector: Selector,
        variant: usize,
        named: &str,
        other: &str,
    ) -> Probe {
        let mut probe = Probe {
            family,
            args: Vec::new(),
            stdin: None,
            selector,
            request_case: None,
            object_case: None,
            creates_case: false,
            handle: None,
        };
        let key = self.key();
        match family {
            "observe" => {
                let (source, case) = if variant == 0 {
                    (self.sources[named].clone(), named.to_owned())
                } else {
                    (self.sources[other].clone(), other.to_owned())
                };
                probe.args = strings(&[
                    "observe",
                    "shared line",
                    "--source",
                    &source,
                    "--command-id",
                    &key,
                ]);
                probe.object_case = Some(Some(case));
            }
            "note --on" => {
                let choice = OBJECTS[variant];
                let (id, case) = self.object(choice, named, other);
                probe.args = strings(&[
                    "note",
                    &format!("Probe {key}"),
                    "--on",
                    &id,
                    "--command-id",
                    &key,
                ]);
                probe.object_case = Some(case);
            }
            "note" => {
                probe.args = strings(&["note", &format!("Probe {key}"), "--command-id", &key]);
            }
            "record question.create" => {
                let case = [named, other][variant];
                let request = json!({
                    "schema_version": "inv.command.v1", "command_id": key, "case_id": case,
                    "actor": ACTOR, "preconditions": [],
                    "operations": [{"op": "question.create", "payload": self.question}],
                });
                probe.args = strings(&["record", "--input", "-"]);
                probe.stdin = Some(serde_json::to_vec(&request).unwrap());
                probe.request_case = Some(case.to_owned());
            }
            "record annotation.add" => {
                let choice = OBJECTS[variant];
                let (id, case) = self.object(choice, named, other);
                let request = json!({
                    "schema_version": "inv.command.v1", "command_id": key, "case_id": named,
                    "actor": ACTOR, "preconditions": [],
                    "operations": [{"op": "annotation.add", "payload": {
                        "content": {"kind": "text", "text": format!("Probe {key}")},
                        "subject_refs": [{"id": id, "revision": 1}]}}],
                });
                probe.args = strings(&["record", "--input", "-"]);
                probe.stdin = Some(serde_json::to_vec(&request).unwrap());
                probe.request_case = Some(named.to_owned());
                probe.object_case = Some(case);
            }
            "ingest" => {
                let case = [named, other][variant];
                let file = self.casework.source_file(&key);
                let manifest = json!({
                    "schema_version": "inv.ingest.v1", "command_id": key, "actor": ACTOR,
                    "case_id": case, "source": {"title": format!("Probe {key}")},
                    "paths": [file.display().to_string()],
                });
                probe.args = strings(&["ingest", "--input", "-"]);
                probe.stdin = Some(serde_json::to_vec(&manifest).unwrap());
                probe.request_case = Some(case.to_owned());
            }
            "source add" => {
                let file = self.casework.source_file(&key);
                probe.args = strings(&[
                    "source",
                    "add",
                    &file.display().to_string(),
                    "--command-id",
                    &key,
                ]);
            }
            "start" => {
                let file = self.casework.source_file(&key);
                probe.args = strings(&[
                    "start",
                    &format!("Probe {key}?"),
                    "--attach",
                    &file.display().to_string(),
                    "--command-id",
                    &key,
                ]);
                probe.creates_case = true;
            }
            "case new" => {
                probe.args =
                    strings(&["case", "new", &format!("Probe {key}"), "--command-id", &key]);
                probe.creates_case = true;
            }
            "capture begin" => {
                let case = [named, other][variant];
                probe.args = strings(&["capture", "begin", "--input", "-"]);
                probe.stdin = Some(serde_json::to_vec(&begin_request(&key, Some(case))).unwrap());
                probe.request_case = Some(case.to_owned());
            }
            "capture begin new case" => {
                probe.args = strings(&["capture", "begin", "--input", "-"]);
                probe.stdin = Some(serde_json::to_vec(&begin_request(&key, None)).unwrap());
                probe.creates_case = true;
            }
            "capture append" | "capture attach" => {
                let (handle, case, position, _) = self
                    .handles
                    .iter()
                    .find(|(_, case, _, _)| case == named)
                    .cloned()
                    .unwrap();
                let request = if family == "capture append" {
                    json!({"handle": handle, "key": key, "position": position,
                           "fact": {"kind": "phase_changed", "phase": format!("Probe {key}")}})
                } else {
                    let file = self.casework.source_file(&key);
                    json!({"handle": handle, "key": key, "position": position,
                           "media_type": "text/plain", "role": "original-output",
                           "source": {"kind": "file", "path": file.display().to_string()}})
                };
                probe.args = strings(&[
                    "capture",
                    family.trim_start_matches("capture "),
                    "--input",
                    "-",
                ]);
                probe.stdin = Some(serde_json::to_vec(&request).unwrap());
                probe.request_case = Some(case);
                probe.handle = Some(handle);
            }
            "vault snapshot-note" => {
                let choice = OBJECTS[variant];
                let (id, case) = self.object(choice, named, other);
                let note = format!("Notes/{key}.md");
                fs::write(
                    self.casework.root.join("vault").join(&note),
                    format!("Probe {key}\n"),
                )
                .unwrap();
                probe.args = strings(&[
                    "vault",
                    "snapshot-note",
                    &note,
                    "--on",
                    &id,
                    "--command-id",
                    &key,
                ]);
                probe.object_case = Some(case);
            }
            _ => unreachable!("{family}"),
        }
        if let Some(value) = self.selector_value(selector, named, other) {
            probe.args.splice(0..0, ["--case".to_owned(), value]);
        }
        probe
    }

    fn allowed(&self, probe: &Probe, named: &str, other: &str) -> Allowed {
        let explicit = match probe.selector {
            Selector::Omitted => None,
            Selector::Named | Selector::NamedPrefix => Some(named.to_owned()),
            Selector::Other => Some(other.to_owned()),
            Selector::Unknown | Selector::Ambiguous | Selector::Malformed => {
                return Allowed::Refuse;
            }
        };
        if probe.creates_case {
            return if explicit.is_some() {
                Allowed::Refuse
            } else {
                Allowed::NewCase
            };
        }
        if let (Some(explicit), Some(request)) = (&explicit, &probe.request_case)
            && explicit != request
        {
            return Allowed::Refuse;
        }
        let target = explicit.or_else(|| probe.request_case.clone());
        match (&probe.object_case, target) {
            (Some(None), _) => Allowed::Refuse,
            (Some(Some(object)), Some(target)) if *object != target => Allowed::Refuse,
            (Some(Some(object)), _) => Allowed::Case(object.clone()),
            (None, Some(target)) => Allowed::Case(target),
            (None, None) => Allowed::Refuse,
        }
    }

    /// Send one probe and check it against the journal oracle.
    fn send(&mut self, probe: &Probe, named: &str, other: &str) {
        let allowed = self.allowed(probe, named, other);
        let journals = self.casework.journals();
        let records = self.casework.record_cases();
        let files = self.casework.files_snapshot();
        let call = self.casework.call(&probe.args, probe.stdin.as_deref());
        let after = self.casework.journals();
        let label = format!("{} {:?} {:?}", probe.family, probe.selector, probe.args);
        self.probes += 1;
        let entry = self.by_family.entry(probe.family).or_default();
        entry.0 += 1;
        let mut new_events = Vec::new();
        for (case, names) in &after {
            for name in names {
                if !journals.get(case).is_some_and(|known| known.contains(name)) {
                    new_events.push((case.clone(), name.clone()));
                }
            }
        }
        if !call.accepted() {
            if !new_events.is_empty() || self.casework.files_snapshot() != files {
                self.refusals_that_wrote += 1;
                self.violations
                    .push(format!("{label}: refused ({}) but wrote", call.code()));
            }
            return;
        }
        entry.1 += 1;
        self.accepted += 1;
        if let Some(handle) = &probe.handle
            && let Some(open) = self.handles.iter_mut().find(|(id, ..)| id == handle)
        {
            open.2 += 1;
            if probe.family == "capture attach" {
                let key = serde_json::from_slice::<Value>(probe.stdin.as_deref().unwrap()).unwrap()
                    ["key"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                open.3.push(key);
            }
        }
        let landed: BTreeSet<&String> = new_events.iter().map(|(case, _)| case).collect();
        let correct = match &allowed {
            Allowed::Refuse => false,
            Allowed::Case(case) => landed.iter().all(|landed| *landed == case),
            Allowed::NewCase => {
                landed.len() == 1 && landed.iter().all(|case| !journals.contains_key(*case))
            }
        };
        if !correct {
            self.misdirected += 1;
            self.violations.push(format!(
                "{label}: accepted into {landed:?}, allowed {allowed:?}: {}",
                call.body
            ));
            return;
        }
        if !new_events.is_empty() {
            self.landed += 1;
        }
        for (case, name) in &new_events {
            let event = self.casework.event(case, name);
            if event["accepted_operations"]["case_id"] != json!(case) {
                self.violations
                    .push(format!("{label}: event {case}/{name} names another case"));
            }
            let mut cited = BTreeSet::new();
            ids_in(&event, &mut cited);
            for id in cited {
                if let Some(home) = records.get(&id)
                    && home != case
                {
                    self.cross_case += 1;
                    self.violations
                        .push(format!("{label}: event in {case} cites {id} of {home}"));
                }
            }
        }
    }

    /// A call that must leave every journal unchanged.
    fn journal_neutral(&mut self, args: &[&str]) {
        let before = self.casework.journals();
        self.casework.ok(args, None);
        if self.casework.journals() != before {
            self.violations.push(format!("{args:?} changed a journal"));
        }
    }
}

fn begin_request(key: &str, case: Option<&str>) -> Value {
    let target = match case {
        Some(case) => json!({"kind": "existing", "case_id": case}),
        None => json!({"kind": "new_record_case", "title": format!("Probe {key}")}),
    };
    json!({
        "key": format!("{key}/begin"), "actor": ACTOR,
        "producer": {"name": "acceptance-probe", "version": "1"},
        "external_run_id": key, "intent": {"label": format!("Probe {key}")},
        "case": target,
    })
}

#[test]
fn every_write_command_is_fuzzed_for_targeting_or_names_no_case() {
    let declared = declared_writes();
    let classified: BTreeSet<String> = FUZZED
        .iter()
        .copied()
        .chain(NOT_CASE_TARGETED.iter().map(|(name, _)| *name))
        .chain(TARGETED_WITHOUT_A_JOURNAL.iter().map(|(name, _)| *name))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        declared, classified,
        "classify every write command of read_only::EFFECTS as fuzzed, targeted without a journal or not case-targeted"
    );
    let shards: BTreeSet<&str> = SHARDS
        .iter()
        .flat_map(|families| families.iter().map(|(family, _)| *family))
        .collect();
    for command in FUZZED {
        assert!(
            shards.iter().any(|family| family.starts_with(command))
                || ["capture finalize", "vault reconcile"].contains(command),
            "{command} is classified as fuzzed but no shard sends it"
        );
    }
}

/// The families each shard sends, with their number of variants: which
/// source, object or request case a call names. Shard 0 also finalizes its
/// capture handles and shard 1 reconciles vault paths. The shards run in
/// parallel, each in its own workspace.
const SHARDS: [&[(&str, usize)]; 3] = [
    &[
        ("observe", 2),
        ("note --on", OBJECTS.len()),
        ("note", 1),
        ("source add", 1),
        ("start", 1),
        ("capture append", 1),
    ],
    &[
        ("record question.create", 2),
        ("record annotation.add", OBJECTS.len()),
        ("ingest", 2),
        ("case new", 1),
    ],
    &[
        ("capture begin", 2),
        ("capture begin new case", 1),
        ("capture attach", 1),
        ("vault snapshot-note", OBJECTS.len()),
    ],
];

fn targeting_shard(shard: usize) {
    let seed = std::env::var("INV_TARGETING_SEED")
        .ok()
        .map_or(DEFAULT_SEED, |value| value.parse().unwrap());
    let mut fuzz = Targeting::new(seed.wrapping_add(shard as u64));
    let mut plan: Vec<(&'static str, Selector, usize)> = SHARDS[shard]
        .iter()
        .flat_map(|(family, variants)| {
            SELECTORS.iter().flat_map(move |selector| {
                (0..*variants).map(move |variant| (*family, *selector, variant))
            })
        })
        .collect();
    // The seed decides the order and which of the two cases each call names.
    fuzz.rng.shuffle(&mut plan);
    for (family, selector, variant) in plan {
        let cases = fuzz.named_cases();
        let flip = fuzz.rng.below(2);
        let (named, other) = (cases[flip].clone(), cases[1 - flip].clone());
        let probe = fuzz.build(family, selector, variant, &named, &other);
        fuzz.send(&probe, &named, &other);
    }
    match shard {
        0 => finalize_phase(&mut fuzz),
        1 => reconcile_phase(&mut fuzz),
        _ => {}
    }
    report(&fuzz, seed, shard);
}

/// `capture finalize`: the handle names the case. Wrong selectors first, so
/// the first accepted call finalizes and the later ones retry it.
fn finalize_phase(fuzz: &mut Targeting) {
    for (handle, case, position, attached) in fuzz.handles.clone() {
        let other = fuzz
            .named_cases()
            .into_iter()
            .find(|named| *named != case)
            .unwrap();
        let request = json!({
            "handle": handle, "key": format!("{handle}/finalize"), "completion": "complete",
            "declared_artifacts": attached, "execution": {"exit_code": 0, "state": "completed"},
            "expected_last_position": position - 1, "strict_artifact_inventory": true,
        });
        for selector in [
            Selector::Other,
            Selector::Unknown,
            Selector::Ambiguous,
            Selector::Malformed,
            Selector::Named,
            Selector::NamedPrefix,
            Selector::Omitted,
        ] {
            let mut probe = Probe {
                family: "capture finalize",
                args: strings(&["capture", "finalize", "--input", "-"]),
                stdin: Some(serde_json::to_vec(&request).unwrap()),
                selector,
                request_case: Some(case.clone()),
                object_case: None,
                creates_case: false,
                handle: None,
            };
            if let Some(value) = fuzz.selector_value(selector, &case, &other) {
                probe.args.splice(0..0, ["--case".to_owned(), value]);
            }
            fuzz.send(&probe, &case, &other);
        }
    }
}

/// `vault reconcile --disposition propose-change`: the path names the case.
/// `repair index` and `vault render`, which name no case, leave every
/// journal alone.
fn reconcile_phase(fuzz: &mut Targeting) {
    fuzz.journal_neutral(&["repair", "index"]);
    let mut selectors = SELECTORS.to_vec();
    fuzz.rng.shuffle(&mut selectors);
    for (index, selector) in selectors.into_iter().enumerate() {
        let cases = fuzz.named_cases();
        let (named, other) = (cases[index % 2].clone(), cases[1 - index % 2].clone());
        let overview = format!("Cases/{named}/Overview.md");
        let path = fuzz.casework.root.join("vault").join(&overview);
        if !path.is_file() {
            fuzz.journal_neutral(&["vault", "render"]);
        }
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str(&format!("\nEdited by probe {index}.\n"));
        fs::write(&path, text).unwrap();
        fuzz.journal_neutral(&["vault", "render"]);
        let key = fuzz.key();
        let mut probe = Probe {
            family: "vault reconcile",
            args: strings(&[
                "vault",
                "reconcile",
                &overview,
                "--disposition",
                "propose-change",
                "--field",
                "title",
                "--value",
                &format!("Retitled by {key}"),
                "--command-id",
                &key,
            ]),
            stdin: None,
            selector,
            request_case: Some(named.clone()),
            object_case: None,
            creates_case: false,
            handle: None,
        };
        if let Some(value) = fuzz.selector_value(selector, &named, &other) {
            probe.args.splice(0..0, ["--case".to_owned(), value]);
        }
        fuzz.send(&probe, &named, &other);
    }
}

fn report(fuzz: &Targeting, seed: u64, shard: usize) {
    let observation = json!({
        "seed": seed,
        "shard": shard,
        "probes": fuzz.probes,
        "accepted": fuzz.accepted,
        "refused": fuzz.probes - fuzz.accepted,
        "accepted_writes_with_events": fuzz.landed,
        "misdirected": fuzz.misdirected,
        "cross_case_citations": fuzz.cross_case,
        "refusals_that_wrote": fuzz.refusals_that_wrote,
        "by_family": fuzz.by_family.iter().map(|(family, (probes, accepted))| {
            ((*family).to_owned(), json!({"probes": probes, "accepted": accepted}))
        }).collect::<serde_json::Map<_, _>>(),
        "commands_fuzzed": FUZZED,
        "commands_not_case_targeted": NOT_CASE_TARGETED.iter()
            .map(|(name, reason)| ((*name).to_owned(), json!(reason)))
            .collect::<serde_json::Map<_, _>>(),
        "violations": fuzz.violations,
    });
    acceptance::observe("targeting", &format!("fuzz-shard-{shard}"), &observation);
    assert!(
        fuzz.violations.is_empty(),
        "seed {seed} shard {shard}: {:#?}",
        fuzz.violations
    );
    // Every family reached both outcomes.
    for (family, (probes, accepted)) in &fuzz.by_family {
        assert!(
            *accepted > 0 && accepted < probes,
            "{family}: {accepted} of {probes} accepted; the probes do not exercise both outcomes"
        );
    }
}

#[test]
fn targeting_fuzz_shard_0_lands_every_write_in_its_named_case_or_refuses_it() {
    targeting_shard(0);
}

#[test]
fn targeting_fuzz_shard_1_lands_every_write_in_its_named_case_or_refuses_it() {
    targeting_shard(1);
}

#[test]
fn targeting_fuzz_shard_2_lands_every_write_in_its_named_case_or_refuses_it() {
    targeting_shard(2);
}

/// Write commands that name their case but record nothing in a journal, so the
/// journal oracle of the fuzz cannot see where they land; each has its own
/// targeting test, named here. `retention reclaim --all` is the one form that
/// names no case: it sweeps the workspace, refuses `--case` (probed below) and
/// names no record; retention::a_workspace_sweep_is_a_dry_run_until_applied_and_names_no_record
/// covers it.
const TARGETED_WITHOUT_A_JOURNAL: &[(&str, &str)] = &[(
    "retention reclaim",
    "retention_reclaim_removes_only_bytes_of_the_case_it_names",
)];

fn blob_path(root: &Path, digest: &str) -> PathBuf {
    let hex = digest.strip_prefix("sha256:").unwrap();
    root.join("state/blobs/sha256").join(&hex[..2]).join(hex)
}

#[test]
fn retention_reclaim_removes_only_bytes_of_the_case_it_names() {
    let mut work = Casework::new(false);
    let mut started = Vec::new();
    for case in ["a", "b"] {
        let file = work.source_file(&format!("capture of case {case}"));
        let body = work.ok(
            &[
                "start",
                "Does the queue meet the latency limit?",
                "--attach",
                &file.display().to_string(),
                "--command-id",
                &format!("reclaim-start-{case}"),
            ],
            None,
        );
        let item = &body["items"][0];
        started.push((
            body["created"][0]["id"].as_str().unwrap().to_owned(),
            item["artifact_id"].as_str().unwrap().to_owned(),
            item["digest"].as_str().unwrap().to_owned(),
        ));
    }
    let [(case_a, art_a, digest_a), (case_b, art_b, digest_b)] =
        <[_; 2]>::try_from(started).unwrap();
    // Withdraw every reference of case A to its bytes, as retention.rs does.
    let previewed = work.ok(&["--case", &case_a, "retention", "preview", &art_a], None);
    let mut references = vec![previewed["references"][0]["artifact_ref"].clone()];
    for held in previewed["held_for_others"]["digests"].as_array().unwrap() {
        references.extend(held["references"].as_array().unwrap().iter().cloned());
    }
    let operations: Vec<Value> = references
        .iter()
        .map(|reference| {
            json!({"op": "artifact.expire", "payload": {
                "artifact_ref": reference, "reason": "retention_period_ended",
                "rationale": "The case keeps raw captures for 90 days."}})
        })
        .collect();
    work.ok(
        &["record", "--input", "-"],
        Some(&json!({
            "schema_version": "inv.command.v1", "command_id": "reclaim-expire-a",
            "case_id": case_a, "actor": ACTOR, "preconditions": [],
            "operations": operations
        })),
    );
    let shared = case_a
        .chars()
        .zip(case_b.chars())
        .take_while(|(left, right)| left == right)
        .map(|(character, _)| character)
        .collect::<String>();
    let mut probes: Vec<(String, Vec<String>)> = vec![
        (
            "no --case among two cases".into(),
            strings(&["retention", "reclaim", &art_a]),
        ),
        (
            "the other case".into(),
            strings(&["--case", &case_b, "retention", "reclaim", &art_a]),
        ),
        (
            "an unknown case".into(),
            strings(&["--case", UNKNOWN_CASE, "retention", "reclaim", &art_a]),
        ),
        (
            "a malformed case".into(),
            strings(&["--case", "obs_12345", "retention", "reclaim", &art_a]),
        ),
        (
            "the other case's Artifact".into(),
            strings(&["--case", &case_a, "retention", "reclaim", &art_b]),
        ),
        (
            "one Artifact of each case".into(),
            strings(&["--case", &case_a, "retention", "reclaim", &art_a, &art_b]),
        ),
        (
            "a workspace sweep given a case".into(),
            strings(&[
                "--case",
                &case_a,
                "retention",
                "reclaim",
                "--all",
                "--apply",
            ]),
        ),
    ];
    if shared.len() > "case_".len() {
        probes.push((
            "an ambiguous prefix".into(),
            strings(&["--case", &shared, "retention", "reclaim", &art_a]),
        ));
    }
    let mut refusals_that_wrote = Vec::new();
    for (label, args) in &probes {
        let before = work.files_snapshot();
        let call = work.call(args, None);
        assert!(!call.accepted(), "{label}: {}", call.body);
        if work.files_snapshot() != before {
            refusals_that_wrote.push(label.clone());
        }
    }
    let journals = work.journals();
    let before = work.files_snapshot();
    let reclaimed = work.ok(&["--case", &case_a, "retention", "reclaim", &art_a], None);
    assert_eq!(reclaimed["result"], "retention_reclaimed");
    let after = work.files_snapshot();
    let changed: Vec<&PathBuf> = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .collect();
    let mut misdirected = Vec::new();
    if blob_path(&work.root, &digest_a).exists() {
        misdirected.push(format!(
            "the named case's withdrawn bytes were kept: {reclaimed}"
        ));
    }
    if !blob_path(&work.root, &digest_b).is_file() {
        misdirected.push("the other case's bytes were removed".to_owned());
    }
    if work.journals() != journals {
        misdirected.push("a journal changed".to_owned());
    }
    let case_b_state = Path::new("state/cases").join(&case_b);
    misdirected.extend(
        changed
            .iter()
            .filter(|path| path.starts_with(&case_b_state))
            .map(|path| format!("{} changed", path.display())),
    );
    acceptance::observe(
        "targeting",
        "retention-reclaim",
        &json!({"probes": probes.len() + 1, "accepted": 1, "refused": probes.len(),
                "misdirected": misdirected.len(), "cross_case_citations": 0,
                "refusals_that_wrote": refusals_that_wrote.len(),
                "details": {"misdirected": misdirected, "refusals_that_wrote": refusals_that_wrote}}),
    );
    assert!(misdirected.is_empty(), "{misdirected:#?}");
    assert!(refusals_that_wrote.is_empty(), "{refusals_that_wrote:#?}");
}

// ---------------------------------------------------------------- lost responses

/// One write sent twice with its first response dropped, then sent changed
/// under the same key.
struct Retried {
    command: &'static str,
    key: String,
    args: Vec<String>,
    stdin: Option<Value>,
    changed_args: Vec<String>,
    changed_stdin: Option<Value>,
    /// A staged capture item, checked through `capture recover`, not the
    /// journal.
    staged_on: Option<String>,
}

fn new_events(
    before: &BTreeMap<String, BTreeSet<String>>,
    after: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for (case, names) in after {
        for name in names {
            if !before.get(case).is_some_and(|known| known.contains(name)) {
                found.push((case.clone(), name.clone()));
            }
        }
    }
    found
}

fn staged_items(casework: &Casework, handle: &str) -> usize {
    let recovered = casework.ok(&["capture", "recover", "--handle", handle], None);
    let entry = &recovered["handles"][0];
    entry["items"].as_array().map_or(0, Vec::len)
        + entry["artifacts"].as_array().map_or(0, Vec::len)
}

#[test]
fn every_write_command_retried_after_a_lost_response_is_recorded_exactly_once() {
    let mut casework = Casework::new(true);
    let first = casework.source_file("First question");
    let started = casework.ok(
        &[
            "start",
            "First question",
            "--attach",
            &first.display().to_string(),
        ],
        None,
    );
    let case = started["created"][0]["id"].as_str().unwrap().to_owned();
    let source = started["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .find(|id| id.starts_with("src_"))
        .unwrap()
        .to_owned();
    let observed = casework.ok(
        &[
            "--case",
            &case,
            "observe",
            "shared line",
            "--source",
            &source,
        ],
        None,
    );
    let observation = observed["created"][0]["id"].as_str().unwrap().to_owned();
    let question = casework.ok(&["schema", "question.create"], None)["transaction_example"]
        ["input"]["operations"][0]["payload"]
        .clone();
    let begun = casework.ok(
        &["capture", "begin", "--input", "-"],
        Some(&begin_request("retry-handle", Some(&case))),
    );
    let handle = begun["handle"].as_str().unwrap().to_owned();
    let vault_note = casework.root.join("vault/Notes/retry.md");
    fs::write(&vault_note, "A human note the records rely on.\n").unwrap();

    let with_case = |args: &[&str]| {
        let mut full = strings(&["--case", &case]);
        full.extend(strings(args));
        full
    };
    let command = |key: &str, wording: &Value| {
        json!({"schema_version": "inv.command.v1", "command_id": key, "case_id": case,
               "actor": ACTOR, "preconditions": [],
               "operations": [{"op": "question.create", "payload": wording}]})
    };
    let mut changed_question = question.clone();
    changed_question["wording"] = json!("A changed question?");
    let ingest_file = casework.source_file("Ingested");
    let manifest = |title: &str| {
        json!({"schema_version": "inv.ingest.v1", "command_id": "retry-ingest", "actor": ACTOR,
               "case_id": case, "source": {"title": title},
               "paths": [ingest_file.display().to_string()]})
    };
    let added = casework.source_file("Added").display().to_string();
    let attach_file = casework.source_file("Attached").display().to_string();
    let attached = |media: &str| {
        json!({"handle": handle, "key": "retry-attach", "position": 2, "media_type": media,
               "role": "original-output", "source": {"kind": "file", "path": attach_file}})
    };
    let finalize = |code: i64| {
        json!({"handle": handle, "key": "retry-finalize", "completion": "complete",
               "declared_artifacts": ["retry-attach"],
               "execution": {"exit_code": code, "state": "completed"},
               "expected_last_position": 2, "strict_artifact_inventory": true})
    };
    let new_question_file = casework
        .source_file("Second question")
        .display()
        .to_string();
    // A drifted generated note for vault reconcile.
    casework.ok(&["vault", "render"], None);
    let overview = format!("Cases/{case}/Overview.md");
    let overview_path = casework.root.join("vault").join(&overview);
    let edited = fs::read_to_string(&overview_path).unwrap() + "\nEdited by hand.\n";
    fs::write(&overview_path, edited).unwrap();
    casework.ok(&["vault", "render"], None);

    let plan = vec![
        Retried {
            command: "record",
            key: "retry-record".into(),
            args: strings(&["record", "--input", "-"]),
            stdin: Some(command("retry-record", &question)),
            changed_args: strings(&["record", "--input", "-"]),
            changed_stdin: Some(command("retry-record", &changed_question)),
            staged_on: None,
        },
        Retried {
            command: "ingest",
            key: "retry-ingest".into(),
            args: strings(&["ingest", "--input", "-"]),
            stdin: Some(manifest("Retried source")),
            changed_args: strings(&["ingest", "--input", "-"]),
            changed_stdin: Some(manifest("Changed source")),
            staged_on: None,
        },
        Retried {
            command: "observe",
            key: "retry-observe".into(),
            args: with_case(&[
                "observe",
                "shared line",
                "--source",
                &source,
                "--command-id",
                "retry-observe",
            ]),
            stdin: None,
            changed_args: with_case(&[
                "observe",
                "First question",
                "--source",
                &source,
                "--command-id",
                "retry-observe",
            ]),
            changed_stdin: None,
            staged_on: None,
        },
        Retried {
            command: "note",
            key: "retry-note".into(),
            args: with_case(&[
                "note",
                "Check the clock",
                "--on",
                &observation,
                "--command-id",
                "retry-note",
            ]),
            stdin: None,
            changed_args: with_case(&[
                "note",
                "Check the setpoint",
                "--on",
                &observation,
                "--command-id",
                "retry-note",
            ]),
            changed_stdin: None,
            staged_on: None,
        },
        Retried {
            command: "source add",
            key: "retry-source".into(),
            args: with_case(&["source", "add", &added, "--command-id", "retry-source"]),
            stdin: None,
            changed_args: with_case(&[
                "source",
                "add",
                &ingest_file.display().to_string(),
                "--command-id",
                "retry-source",
            ]),
            changed_stdin: None,
            staged_on: None,
        },
        Retried {
            command: "start",
            key: "retry-start".into(),
            args: strings(&[
                "start",
                "Second question?",
                "--attach",
                &new_question_file,
                "--command-id",
                "retry-start",
            ]),
            stdin: None,
            changed_args: strings(&[
                "start",
                "Another question?",
                "--attach",
                &new_question_file,
                "--command-id",
                "retry-start",
            ]),
            changed_stdin: None,
            staged_on: None,
        },
        Retried {
            command: "case new",
            key: "retry-case".into(),
            args: strings(&["case", "new", "Retried case", "--command-id", "retry-case"]),
            stdin: None,
            changed_args: strings(&["case", "new", "Changed case", "--command-id", "retry-case"]),
            changed_stdin: None,
            staged_on: None,
        },
        Retried {
            command: "capture begin",
            key: "retry-begin/begin".into(),
            args: strings(&["capture", "begin", "--input", "-"]),
            stdin: Some(begin_request("retry-begin", Some(&case))),
            changed_args: strings(&["capture", "begin", "--input", "-"]),
            changed_stdin: Some({
                let mut changed = begin_request("retry-begin", Some(&case));
                changed["intent"]["label"] = json!("Changed intent");
                changed
            }),
            staged_on: None,
        },
        Retried {
            command: "capture append",
            key: "retry-append".into(),
            args: strings(&["capture", "append", "--input", "-"]),
            stdin: Some(
                json!({"handle": handle, "key": "retry-append", "position": 1,
                               "fact": {"kind": "phase_changed", "phase": "warmup"}}),
            ),
            changed_args: strings(&["capture", "append", "--input", "-"]),
            changed_stdin: Some(
                json!({"handle": handle, "key": "retry-append", "position": 1,
                                       "fact": {"kind": "phase_changed", "phase": "measure"}}),
            ),
            staged_on: Some(handle.clone()),
        },
        Retried {
            command: "capture attach",
            key: "retry-attach".into(),
            args: strings(&["capture", "attach", "--input", "-"]),
            stdin: Some(attached("text/plain")),
            changed_args: strings(&["capture", "attach", "--input", "-"]),
            changed_stdin: Some(attached("application/json")),
            staged_on: Some(handle.clone()),
        },
        Retried {
            command: "capture finalize",
            key: "retry-finalize".into(),
            args: strings(&["capture", "finalize", "--input", "-"]),
            stdin: Some(finalize(0)),
            changed_args: strings(&["capture", "finalize", "--input", "-"]),
            changed_stdin: Some(finalize(1)),
            staged_on: None,
        },
        Retried {
            command: "vault snapshot-note",
            key: "retry-snapshot".into(),
            args: with_case(&[
                "vault",
                "snapshot-note",
                "Notes/retry.md",
                "--on",
                &observation,
                "--command-id",
                "retry-snapshot",
            ]),
            stdin: None,
            changed_args: with_case(&[
                "vault",
                "snapshot-note",
                "Notes/retry.md",
                "--on",
                &case,
                "--command-id",
                "retry-snapshot",
            ]),
            changed_stdin: None,
            staged_on: None,
        },
        Retried {
            command: "vault reconcile",
            key: "retry-reconcile".into(),
            args: strings(&[
                "vault",
                "reconcile",
                &overview,
                "--disposition",
                "propose-change",
                "--field",
                "title",
                "--value",
                "Retitled once",
                "--command-id",
                "retry-reconcile",
            ]),
            stdin: None,
            changed_args: strings(&[
                "vault",
                "reconcile",
                &overview,
                "--disposition",
                "propose-change",
                "--field",
                "title",
                "--value",
                "Retitled twice",
                "--command-id",
                "retry-reconcile",
            ]),
            changed_stdin: None,
            staged_on: None,
        },
    ];
    let mut lost = Vec::new();
    let mut duplicated = Vec::new();
    let mut unrefused_changes = Vec::new();
    let mut per_command = serde_json::Map::new();
    for retried in &plan {
        let bytes = |value: &Option<Value>| {
            value
                .as_ref()
                .map(|value| serde_json::to_vec(value).unwrap())
        };
        let before = casework.journals();
        let staged_before = retried
            .staged_on
            .as_deref()
            .map(|handle| staged_items(&casework, handle));
        // The first response is lost: the caller never reads it. The oracle
        // keeps it only to compare the retry's answer with it.
        let dropped = casework.call(&retried.args, bytes(&retried.stdin).as_deref());
        let after_first = casework.journals();
        let written = new_events(&before, &after_first);
        let staged_after = retried
            .staged_on
            .as_deref()
            .map(|handle| staged_items(&casework, handle));
        let recorded = match (staged_before, staged_after) {
            (Some(before), Some(after)) => after - before,
            _ => written.len(),
        };
        if recorded == 0 {
            lost.push(format!("{}: the first send left nothing", retried.command));
        }
        let retry = casework.call(&retried.args, bytes(&retried.stdin).as_deref());
        let after_retry = casework.journals();
        let restaged = retried
            .staged_on
            .as_deref()
            .map(|handle| staged_items(&casework, handle));
        if !retry.accepted() {
            lost.push(format!(
                "{}: the identical retry was refused: {}",
                retried.command, retry.body
            ));
        }
        if !new_events(&after_first, &after_retry).is_empty() || restaged != staged_after {
            duplicated.push(format!(
                "{}: the identical retry wrote again",
                retried.command
            ));
        }
        // The retry answers with the IDs the lost response carried, and
        // every record ID it names is in the journal.
        let mut original = BTreeSet::new();
        ids_in(&dropped.body, &mut original);
        let mut answered = BTreeSet::new();
        ids_in(&retry.body, &mut answered);
        let known = casework.record_cases();
        let phantom: Vec<&String> = answered
            .iter()
            .filter(|id| !known.contains_key(*id))
            .filter(|id| !id.starts_with("tx_") && !id.starts_with("handle_"))
            .collect();
        if answered != original || !phantom.is_empty() {
            lost.push(format!(
                "{}: the retry answered {answered:?}, the lost response {original:?}",
                retried.command
            ));
        }
        // A changed request under the same key writes nothing.
        let changed = casework.call(
            &retried.changed_args,
            bytes(&retried.changed_stdin).as_deref(),
        );
        let after_changed = casework.journals();
        let changed_staged = retried
            .staged_on
            .as_deref()
            .map(|handle| staged_items(&casework, handle));
        if changed.accepted()
            || !new_events(&after_retry, &after_changed).is_empty()
            || changed_staged != restaged
        {
            unrefused_changes.push(format!("{}: {}", retried.command, changed.body));
        }
        per_command.insert(
            retried.command.to_owned(),
            json!({"key": retried.key, "recorded": recorded, "retry": retry.body["proposal"]["result"]
                       .as_str().or(retry.body["result"].as_str()).or(retry.body["commit"]["result"].as_str())
                       .or(retry.body["receipt"]["result"].as_str()),
                   "retry_code": retry.code(), "changed_retry_code": changed.code()}),
        );
    }
    let details = [lost.clone(), duplicated.clone(), unrefused_changes.clone()].concat();
    let observation = json!({
        "commands": plan.len(),
        "lost": lost.len(),
        "duplicated": duplicated.len(),
        "changed_requests_not_refused": unrefused_changes.len(),
        "per_command": per_command,
        "details": details,
    });
    acceptance::observe("lost_duplicate_writes", "cli-lost-response", &observation);
    assert!(
        lost.is_empty() && duplicated.is_empty() && unrefused_changes.is_empty(),
        "{observation:#}"
    );
}

// ---------------------------------------------------------------- repairability

/// Error codes resolved elsewhere, with the test that follows their repair.
const REPAIRED_ELSEWHERE: &[(&str, &str)] = &[
    (
        "RETROSPECTIVE_REQUIRED",
        "drill_closure::a_material_close_is_refused_with_a_repair_that_leads_to_one_idempotent_batch",
    ),
    (
        "RETROSPECTIVE_STALE",
        "drill_closure::new_material_evidence_after_the_account_requires_an_updated_one",
    ),
    (
        "OBJECT_REQUIRED",
        "investigation_store store::capture::tests::a_retry_allocates_staging_that_a_crash_left_unallocated (only a crash inside begin produces it)",
    ),
];

/// Error codes that are not ordinary schema or CAS errors: the store or the
/// machine failed, so the remedy is outside the request.
const NOT_ORDINARY: &[(&str, &str)] = &[
    (
        "INTEGRITY_FAILURE",
        "canonical bytes failed verification; check and doctor report it",
    ),
    (
        "WORKSPACE_BUSY",
        "another writer holds the lock; the identical retry later is the repair",
    ),
    ("IO_FAILURE", "the filesystem failed the call"),
    (
        "DURABILITY_UNKNOWN",
        "the commit outcome is unknown; the identical retry returns the original receipt, as the lost-response test checks, and receipt KEY looks it up",
    ),
];

/// Run a repair's argv (the program name dropped) or schema lookup.
fn follow(casework: &Casework, cwd: &Path, repair: &Value) -> Value {
    let call = if let Some(argv) = repair["argv"].as_array() {
        let args: Vec<String> = argv
            .iter()
            .skip(1)
            .map(|word| word.as_str().unwrap().to_owned())
            .collect();
        invoke(cwd, &args, None)
    } else if let Some(name) = repair["schema"].as_str() {
        let args = if name == "list" {
            strings(&["schema", "--list"])
        } else {
            strings(&["schema", name])
        };
        casework.call(&args, None)
    } else {
        panic!("no repair to follow: {repair}");
    };
    assert!(call.accepted(), "the repair {repair} failed: {}", call.body);
    call.body
}

struct Rejection {
    body: Value,
}

impl Rejection {
    /// Send a call that must be refused with `code` and change nothing.
    fn expect(
        casework: &Casework,
        cwd: &Path,
        args: &[String],
        stdin: Option<&Value>,
        code: &str,
    ) -> Self {
        let files = casework.files_snapshot();
        let bytes = stdin.map(|value| serde_json::to_vec(value).unwrap());
        let call = invoke(cwd, args, bytes.as_deref());
        assert_eq!(call.code(), code, "{args:?}: {}", call.body);
        assert_eq!(call.body["commit_status"], "not_committed", "{}", call.body);
        assert_eq!(
            casework.files_snapshot(),
            files,
            "{code}: the rejection changed the workspace"
        );
        assert!(
            call.body["error"]["repair"].is_object(),
            "{code} names no repair: {}",
            call.body
        );
        Self { body: call.body }
    }

    fn repair(&self) -> &Value {
        &self.body["error"]["repair"]
    }
}

fn workspace_args(casework: &Casework, args: &[&str]) -> Vec<String> {
    let mut full = vec![
        "--workspace".to_owned(),
        casework.root.display().to_string(),
        "--actor".to_owned(),
        ACTOR.to_owned(),
    ];
    full.extend(strings(args));
    full.extend(strings(&["--format", "json"]));
    full
}

#[test]
fn each_ordinary_error_is_resolved_by_following_the_repair_it_returns() {
    let mut casework = Casework::new(false);
    let mut cases = Vec::new();
    let mut observations = Vec::new();
    for title in ["First question", "Second question"] {
        let file = casework.source_file(title);
        let started = casework.ok(
            &["start", title, "--attach", &file.display().to_string()],
            None,
        );
        let created = started["created"].as_array().unwrap();
        let case = created[0]["id"].as_str().unwrap().to_owned();
        let source = created
            .iter()
            .map(|item| item["id"].as_str().unwrap())
            .find(|id| id.starts_with("src_"))
            .unwrap()
            .to_owned();
        let observed = casework.ok(
            &[
                "--case",
                &case,
                "observe",
                "shared line",
                "--source",
                &source,
            ],
            None,
        );
        observations.push(observed["created"][0]["id"].as_str().unwrap().to_owned());
        cases.push(case);
    }
    let case = cases[0].clone();
    let root = casework.root.clone();
    let envelope = |key: &str, operations: Value, preconditions: Value| {
        json!({"schema_version": "inv.command.v1", "command_id": key, "case_id": case,
               "actor": ACTOR, "preconditions": preconditions, "operations": operations})
    };
    let record = workspace_args(&casework, &["record", "--input", "-"]);
    let commit = |request: &Value| {
        let call = casework.call(
            &strings(&["record", "--input", "-"]),
            Some(&serde_json::to_vec(request).unwrap()),
        );
        assert!(
            call.accepted(),
            "the repaired request failed: {}",
            call.body
        );
        call.body
    };
    let mut resolved: BTreeMap<&str, String> = BTreeMap::new();

    // INVALID_INPUT: an unknown payload field. The repair names the
    // operation's schema, whose example payload commits.
    let mut payload = casework.ok(&["schema", "question.create"], None)["transaction_example"]["input"]
        ["operations"][0]["payload"]
        .clone();
    payload["unknown_field"] = json!(true);
    let request = envelope(
        "repair-invalid",
        json!([{"op": "question.create", "payload": payload}]),
        json!([]),
    );
    let rejected = Rejection::expect(&casework, &root, &record, Some(&request), "INVALID_INPUT");
    assert_eq!(
        rejected.body["error"]["path"],
        "/operations/0/payload/unknown_field"
    );
    let schema = follow(&casework, &root, rejected.repair());
    let example = schema["transaction_example"]["input"]["operations"].clone();
    commit(&envelope("repair-invalid", example.clone(), json!([])));
    resolved.insert("INVALID_INPUT", "schema example payload committed".into());

    // EMPTY_TRANSACTION: the repair names the envelope schema, which requires
    // an operation; the example operation commits.
    let rejected = Rejection::expect(
        &casework,
        &root,
        &record,
        Some(&envelope("repair-empty", json!([]), json!([]))),
        "EMPTY_TRANSACTION",
    );
    let schema = follow(&casework, &root, rejected.repair());
    assert_eq!(
        schema["schema"]["properties"]["operations"]["minItems"], 1,
        "{schema}"
    );
    commit(&envelope("repair-empty", example.clone(), json!([])));
    resolved.insert(
        "EMPTY_TRANSACTION",
        "envelope schema followed; one operation committed".into(),
    );

    // UNKNOWN_REFERENCE: the error lists case-local candidates and the
    // repair shows one; citing it commits.
    let annotation = |id: &str| {
        json!([{"op": "annotation.add", "payload": {"content": {"kind": "text", "text": "Check this."},
               "subject_refs": [{"id": id, "revision": 1}]}}])
    };
    let rejected = Rejection::expect(
        &casework,
        &root,
        &record,
        Some(&envelope(
            "repair-unknown",
            annotation(UNKNOWN_OBSERVATION),
            json!([]),
        )),
        "UNKNOWN_REFERENCE",
    );
    let candidate = rejected.body["error"]["actual"]["case_local_references"][0]["id"]
        .as_str()
        .expect("the rejection lists case-local candidates")
        .to_owned();
    assert_eq!(candidate, observations[0]);
    let shown = follow(&casework, &root, rejected.repair());
    assert_eq!(shown["id"], json!(candidate));
    commit(&envelope(
        "repair-unknown",
        annotation(&candidate),
        json!([]),
    ));
    resolved.insert(
        "UNKNOWN_REFERENCE",
        "a listed candidate cited and committed".into(),
    );

    // REVISION_CONFLICT: the repair shows the current record and the error
    // names its revision; the reassessed request against it commits.
    let amend = |title: &str| json!([{"op": "case.amend", "payload": {"title": title, "reason": "Narrowed."}}]);
    commit(&envelope(
        "repair-amend-1",
        amend("First question, narrowed"),
        json!([{"id": case, "revision": 1}]),
    ));
    let rejected = Rejection::expect(
        &casework,
        &root,
        &record,
        Some(&envelope(
            "repair-amend-2",
            amend("First question, renamed"),
            json!([{"id": case, "revision": 1}]),
        )),
        "REVISION_CONFLICT",
    );
    let shown = follow(&casework, &root, rejected.repair());
    assert_eq!(shown["id"], json!(case));
    let current = rejected.body["error"]["actual"]["revision"].clone();
    assert_eq!(current, 2);
    commit(&envelope(
        "repair-amend-2",
        amend("First question, renamed"),
        json!([{"id": case, "revision": current}]),
    ));
    resolved.insert(
        "REVISION_CONFLICT",
        "current revision read; reassessed request committed".into(),
    );

    // IDEMPOTENCY_KEY_REUSED: the repair reads the receipt of the committed
    // request under that key; a new key commits the different request.
    let rejected = Rejection::expect(
        &casework,
        &root,
        &record,
        Some(&envelope(
            "repair-invalid",
            amend("Another title"),
            json!([{"id": case, "revision": 3}]),
        )),
        "IDEMPOTENCY_KEY_REUSED",
    );
    let receipt = follow(&casework, &root, rejected.repair());
    assert_eq!(receipt["command_id"], "repair-invalid", "{receipt}");
    assert!(receipt["transaction_id"].is_string(), "{receipt}");
    commit(&envelope(
        "repair-reused-new-key",
        amend("Another title"),
        json!([{"id": case, "revision": 3}]),
    ));
    resolved.insert(
        "IDEMPOTENCY_KEY_REUSED",
        "receipt read; a new key committed".into(),
    );

    // CASE_REQUIRED: two cases and no --case. The error lists them; the
    // repair onboards; the call with one of them succeeds.
    let note = |selector: Option<&str>| {
        let mut args = Vec::new();
        if let Some(selector) = selector {
            args.extend(["--case", selector]);
        }
        args.extend(["note", "Resume with the controlled build."]);
        workspace_args(&casework, &args)
    };
    let rejected = Rejection::expect(&casework, &root, &note(None), None, "CASE_REQUIRED");
    follow(&casework, &root, rejected.repair());
    let listed: Vec<String> =
        serde_json::from_value(rejected.body["error"]["actual"].clone()).unwrap();
    assert_eq!(listed, casework.case_ids());
    assert!(invoke(&root, &note(Some(&listed[0])), None).accepted());
    resolved.insert("CASE_REQUIRED", "a listed case selected".into());

    // AMBIGUOUS_REFERENCE: a case prefix both cases share. The error lists
    // the matches; a full ID succeeds.
    let shared: String = cases[0]
        .chars()
        .zip(cases[1].chars())
        .take_while(|(left, right)| left == right)
        .map(|(left, _)| left)
        .collect();
    let brief = |selector: &str| workspace_args(&casework, &["--case", selector, "brief"]);
    let files = casework.files_snapshot();
    let call = invoke(&root, &brief(&shared), None);
    assert_eq!(call.code(), "AMBIGUOUS_REFERENCE", "{}", call.body);
    assert_eq!(casework.files_snapshot(), files);
    let matches: Vec<String> =
        serde_json::from_value(call.body["error"]["actual"].clone()).unwrap();
    assert_eq!(matches.len(), 2, "{}", call.body);
    assert!(invoke(&root, &brief(&matches[1]), None).accepted());
    resolved.insert(
        "AMBIGUOUS_REFERENCE",
        "a listed full ID selected (the error lists the matches)".into(),
    );

    // BUDGET_TOO_SMALL: the repair argv asks for the smallest budget that fits.
    let rejected = Rejection::expect(
        &casework,
        &root,
        &workspace_args(&casework, &["--case", &case, "brief", "--max-bytes", "1"]),
        None,
        "BUDGET_TOO_SMALL",
    );
    follow(&casework, &root, rejected.repair());
    resolved.insert(
        "BUDGET_TOO_SMALL",
        "the repair argv returned a bounded brief".into(),
    );

    // WORKSPACE_REQUIRED: no workspace here. The repair onboards, and its
    // first step initializes one; the call then gets past the workspace.
    let empty = tempfile::tempdir().unwrap();
    let call = invoke(empty.path(), &strings(&["next", "--format", "json"]), None);
    assert_eq!(call.code(), "WORKSPACE_REQUIRED", "{}", call.body);
    let onboard = follow(&casework, empty.path(), &call.body["error"]["repair"]);
    let initialized = follow(&casework, empty.path(), &onboard["next"][0]);
    assert_eq!(initialized["result"], "initialized", "{initialized}");
    let again = invoke(
        &empty.path().join("casework"),
        &strings(&["next", "--format", "json"]),
        None,
    );
    assert_ne!(again.code(), "WORKSPACE_REQUIRED", "{}", again.body);
    resolved.insert(
        "WORKSPACE_REQUIRED",
        "onboard then its init step; the workspace is found".into(),
    );

    // GATE_FAILED: support of unknown validity cannot reach review. The
    // repair shows the conclusion and names the operation's schema; once
    // the measurement is assessed, the same request passes the gate.
    let created = commit(&envelope(
        "repair-claim",
        json!([{"op": "conclusion.create", "payload": {
        "statement": "The shared line is recorded.", "scope": "This source.",
        "significance": "Check before reuse.", "support_refs": [{"id": observations[0], "revision": 1}],
        "contradiction_refs": [], "caveats": ["One source."]}}]),
        json!([]),
    ));
    let conclusion = created["created"][0]["id"].as_str().unwrap().to_owned();
    let review = |casework: &Casework| {
        let shown = casework.ok(
            &["--case", &case, "show", &conclusion, "--depth", "full"],
            None,
        );
        let basis = &shown["review_basis"];
        let mut preconditions = basis["read_set"].as_array().cloned().unwrap_or_default();
        preconditions.push(basis["subject_ref"].clone());
        envelope(
            "repair-review",
            json!([{"op": "conclusion.request_review",
            "payload": {"conclusion_ref": basis["subject_ref"]}}]),
            json!(preconditions),
        )
    };
    let rejected = Rejection::expect(
        &casework,
        &root,
        &record,
        Some(&review(&casework)),
        "GATE_FAILED",
    );
    assert_eq!(
        rejected.body["error"]["path"],
        "/operations/0/payload/support_refs"
    );
    let shown = follow(&casework, &root, rejected.repair());
    assert_eq!(shown["id"], json!(conclusion));
    commit(&envelope(
        "repair-assess",
        json!([{"op": "measurement.assess", "payload": {
        "observation_ref": {"id": observations[0], "revision": 1}, "channel": null, "validity": "valid",
        "verification": [{"kind": "equivalent", "justification": "The line matches the retained bytes."}],
        "rationale": "Read from the retained source.", "limits": []}}]),
        json!([]),
    ));
    commit(&review(&casework));
    resolved.insert(
        "GATE_FAILED",
        "the named support assessed; the same request passed".into(),
    );

    // Every published error code is resolved here, resolved by a named test,
    // or not an ordinary schema or CAS error.
    let schema: Value = serde_json::from_slice(
        &fs::read(fixtures::repository().join("schemas/v1/error.schema.json")).unwrap(),
    )
    .unwrap();
    let published: BTreeSet<String> = schema["$defs"]["ErrorCode"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|code| code.as_str().unwrap().to_owned())
        .collect();
    let accounted: BTreeSet<String> = resolved
        .keys()
        .copied()
        .chain(REPAIRED_ELSEWHERE.iter().map(|(code, _)| *code))
        .chain(NOT_ORDINARY.iter().map(|(code, _)| *code))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        published, accounted,
        "account for every published error code"
    );
    acceptance::observe(
        "repairability",
        "error-table",
        &json!({
            "codes_published": published.len(),
            "resolved_by_following_the_repair": resolved,
            "resolved_in_other_tests": REPAIRED_ELSEWHERE.iter()
                .map(|(code, test)| ((*code).to_owned(), json!(test))).collect::<serde_json::Map<_, _>>(),
            "not_ordinary": NOT_ORDINARY.iter()
                .map(|(code, why)| ((*code).to_owned(), json!(why))).collect::<serde_json::Map<_, _>>(),
            "unresolved": 0,
        }),
    );
}

// ---------------------------------------------------------------- domain neutrality

/// Security concepts the architecture and research walkthroughs must not
/// need, beyond `scripts/neutrality-denylist.txt`: the list the Milestone B
/// proof scans the default surfaces for.
const SECURITY_TERMS: &[&str] = &[
    "cve",
    "cwe",
    "cvss",
    "owasp",
    "exploit",
    "vulnerability",
    "vulnerabilities",
    "attack",
    "attacker",
    "threat model",
    "malware",
    "pentest",
    "penetration test",
    "privilege escalation",
    "xss",
    "csrf",
    "sql injection",
    "red team",
    "security",
    "credential",
];

fn contains_word(text: &str, term: &str) -> bool {
    let lower = text.to_lowercase();
    let mut start = 0;
    while let Some(found) = lower[start..].find(term) {
        let at = start + found;
        let before = lower[..at].chars().next_back();
        let after = lower[at + term.len()..].chars().next();
        let boundary = |character: Option<char>| !character.is_some_and(char::is_alphanumeric);
        if boundary(before) && boundary(after) {
            return true;
        }
        start = at + term.len();
    }
    false
}

fn texts(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| texts(item, out)),
        Value::Object(map) => {
            for (key, item) in map {
                out.push(key.clone());
                texts(item, out);
            }
        }
        _ => {}
    }
}

#[test]
fn architecture_and_research_walkthroughs_need_no_security_abstraction_or_test_run_ceremony() {
    let denylist =
        fs::read_to_string(fixtures::repository().join("scripts/neutrality-denylist.txt")).unwrap();
    let terms: Vec<String> = denylist
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_lowercase)
        .chain(SECURITY_TERMS.iter().map(|term| (*term).to_owned()))
        .collect();
    let scratch = tempfile::tempdir().unwrap();
    let mut summary = serde_json::Map::new();
    let mut findings = Vec::new();
    // Scenario C is the architecture choice, Scenario D the source research.
    for (scenario, domain) in [("scenario-c", "architecture"), ("scenario-d", "research")] {
        let root = fixtures::workspace(scenario, &scratch.path().join(scenario), false);
        let mut operations: BTreeMap<String, usize> = BTreeMap::new();
        let mut authored = Vec::new();
        let mut runs = 0;
        let mut cases = Vec::new();
        for case in fs::read_dir(root.join("state/cases")).unwrap() {
            let case = case.unwrap().path();
            cases.push(case.file_name().unwrap().to_string_lossy().into_owned());
            for event in fs::read_dir(case.join("events")).unwrap() {
                let event: Value =
                    serde_json::from_slice(&fs::read(event.unwrap().path()).unwrap()).unwrap();
                for operation in event["request"]["input"]["operations"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    let name = operation["op"].as_str().unwrap().to_owned();
                    if name.starts_with("run.") {
                        runs += 1;
                        // A search log is research method, not a test run.
                        if operation["payload"]["search_coverage"].is_null() {
                            findings.push(format!(
                                "{scenario}: {name} records a run that is not a search log"
                            ));
                        }
                    }
                    if name.starts_with("method_pack.") {
                        findings.push(format!("{scenario}: {name} adopts a method pack"));
                    }
                    *operations.entry(name).or_default() += 1;
                    texts(&operation["payload"], &mut authored);
                }
            }
        }
        // What a successor reads first.
        let mut read = Vec::new();
        for case in &cases {
            for command in ["brief", "next", "blindspots"] {
                let args = strings(&[
                    "--workspace",
                    &root.display().to_string(),
                    "--case",
                    case,
                    command,
                    "--format",
                    "json",
                ]);
                let call = invoke(&root, &args, None);
                assert!(call.accepted(), "{scenario} {command}: {}", call.body);
                texts(&call.body, &mut read);
            }
        }
        for (surface, scanned) in [
            ("journal", &authored),
            ("brief, next and blindspots", &read),
        ] {
            for text in scanned {
                for term in &terms {
                    if contains_word(text, term) {
                        findings.push(format!("{scenario} {surface}: {term:?} in {text:?}"));
                    }
                }
            }
        }
        summary.insert(
            domain.to_owned(),
            json!({"scenario": scenario, "operations": operations, "runs": runs,
                   "texts_scanned": authored.len() + read.len()}),
        );
    }
    acceptance::observe(
        "domain_neutrality",
        "walkthroughs",
        &json!({"terms": terms.len(), "walkthroughs": summary, "findings": findings}),
    );
    assert!(findings.is_empty(), "{findings:#?}");
}
