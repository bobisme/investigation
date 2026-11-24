//! W11.16: one leak and caveat-preservation suite that every export kind,
//! current and future, must pass (plan §16.3, §20.4).
//!
//! **Fixtures.** Scenarios A, B, C and D (the checked-in workspaces under
//! `fixtures/vault-goldens/`) and a fresh hostile workspace each get the same
//! canary layer, added through the public binary to one case. Every string a
//! restricted record carries holds a token starting [`RESTRICTED`], every
//! internal one [`INTERNAL`], and every public one [`PUBLIC`]; in the hostile
//! fixture each token is wrapped in hostile Markdown, YAML, HTML and control
//! text (`crates/investigation-render/tests/fixtures/hostile-text.json`). The
//! tokens are alphanumeric, so no escaping or quoting can hide one from a
//! byte scan. The layer plants a token in every channel a withheld record can
//! reach ([`Channel`]): titles, statements, rationale, selectors, quotes,
//! file names, attachment bytes, source names, links, namespaced metadata,
//! annotations, lessons, human-note snapshots (W11.14: path and bytes),
//! producer runs (file names, bytes, metadata), derived titles (bn-31an:
//! a public decision whose option is withheld) and the steps and lessons of
//! a public Retrospective that cite a restricted record (bn-91gz: authored
//! text may restate what it cites, so it is held at the level of the
//! highest record it cites). It also leaves a dirty
//! working vault: a conflict copy and a stale note holding [`DIRTY`], which
//! no export may ever contain.
//!
//! **Matrix.** For every kind in the registry (`export::KINDS`), not a list
//! kept here, every audience, with and without `--remap-ids`, on every
//! fixture: export through the binary into a fresh folder and read back
//! every byte of every file and every file name. A kind's audience contract
//! is registered in [`CONTRACTS`]; a kind without one fails
//! `every_export_kind_has_a_registered_contract` and the matrix itself.
//!
//! **Assertions** on every export below `restricted`:
//! - no token above the audience, in any byte or file name;
//! - no withheld record's ID in any form (prefixed, bare, upper case,
//!   32-digit), also behind a remapped value (the publisher's mapping is
//!   checked), and no digest of a withheld artifact;
//! - no absolute path of the machine the export was made on;
//! - a remapped export holds no canonical ID, head hash or creation time;
//! - every copied byte (human notes, attachments, run files) is exactly a
//!   retained blob, so a remap never altered one (W11.14);
//! - each caveated record carries every policy caveat verbatim in every file
//!   that shows it, and the public audience gets no caveat at all;
//! - no audience copy shows a Decision or Conclusion in a stronger effective
//!   state than the full (`restricted`) copy.
//!
//! The workspace archive is the full-recovery product with its own scope: it
//! is made only for `restricted`, refuses `--remap-ids`, and is checked for
//! the dirty-vault canary and copied-byte integrity.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use investigation_core::{
    identity::{Id, Kind},
    state::State,
};
use investigation_render::{
    Audience,
    export::{Caveat, ExportSnapshot, KINDS, Pin, Scope, kind, plan_export},
};
use investigation_store::{Store, Workspace, journal::hash_bytes};
use serde_json::{Value, json};

#[path = "support/fixtures.rs"]
mod fixtures;

const ACTOR: &str = "agent:leak-suite";
const REVIEWER: &str = "human:leak-reviewer";
/// Prefix of every token planted in a restricted record.
const RESTRICTED: &str = "ZRCANARY";
/// Prefix of every token planted in an internal record.
const INTERNAL: &str = "ZICANARY";
/// Prefix of every token planted in a public record.
const PUBLIC: &str = "ZPVISIBLE";
/// Prefix of every token planted in the working vault's generated area: a
/// conflict copy and a stale note. No export may ever hold it.
const DIRTY: &str = "ZDCANARY";
/// Token in the public fields of a Retrospective whose steps and lessons
/// cite a restricted record (bn-91gz).
const CITED_ACCOUNT: &str = "ZPVISIBLEcitedaccount";

// ------------------------------------------------------------ Contracts

/// The channels a withheld record can reach. Each is a token suffix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Channel {
    /// An option's title, shown on its own and in the title derived from it
    /// for the decision that selects it (bn-31an).
    Derived,
    /// Observation and objection statements.
    Statement,
    /// An assessment's rationale.
    Rationale,
    /// A source selector's exact text.
    Selector,
    /// An observation's quoted text.
    Quote,
    /// An ingested file's name.
    FileName,
    /// Retained artifact bytes.
    Bytes,
    /// A source's title.
    Source,
    /// A source's namespaced metadata.
    Metadata,
    /// A URL in a source's context.
    Link,
    /// An annotation's text.
    Annotation,
    /// A retrospective's lesson.
    Lesson,
    /// A step or lesson of a public Retrospective that cites a restricted
    /// record (bn-91gz). The record is shown below `restricted`; these
    /// entries never are.
    Cited,
    /// A human note snapshot's vault path (W11.14).
    NotePath,
    /// A human note snapshot's captured bytes (W11.14).
    NoteBytes,
    /// A producer run's artifact file name.
    RunFile,
    /// A producer run's artifact bytes.
    RunBytes,
    /// A producer run's intent and namespaced metadata.
    RunMeta,
}

impl Channel {
    const ALL: [Self; 18] = [
        Self::Derived,
        Self::Statement,
        Self::Rationale,
        Self::Selector,
        Self::Quote,
        Self::FileName,
        Self::Bytes,
        Self::Source,
        Self::Metadata,
        Self::Link,
        Self::Annotation,
        Self::Lesson,
        Self::Cited,
        Self::NotePath,
        Self::NoteBytes,
        Self::RunFile,
        Self::RunBytes,
        Self::RunMeta,
    ];

    const fn suffix(self) -> &'static str {
        match self {
            Self::Derived => "derived",
            Self::Statement => "statement",
            Self::Rationale => "rationale",
            Self::Selector => "selector",
            Self::Quote => "quote",
            Self::FileName => "filename",
            Self::Bytes => "bytes",
            Self::Source => "source",
            Self::Metadata => "metadata",
            Self::Link => "link",
            Self::Annotation => "annotation",
            Self::Lesson => "lesson",
            Self::Cited => "cited",
            Self::NotePath => "notepath",
            Self::NoteBytes => "notebytes",
            Self::RunFile => "runfile",
            Self::RunBytes => "runbytes",
            Self::RunMeta => "runmeta",
        }
    }

    fn token(self, level: &str) -> String {
        format!("{level}{}", self.suffix())
    }
}

/// What an export kind promises the audience.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Audiences {
    /// Every audience, with and without `--remap-ids`, under the strict
    /// positive-inclusion policy.
    Review,
    /// Full recovery: only `restricted`, canonical IDs, every byte.
    Recovery,
}

/// A kind's registered contract: its audience scope and the restricted
/// channels a `restricted` export of it shows, so the suite proves the
/// tokens actually reach the kind (a kind that shows nothing would pass
/// every leak check vacuously).
struct Contract {
    kind: &'static str,
    audiences: Audiences,
    shows: &'static [Channel],
}

/// Every export kind's contract. A kind added to `export::KINDS` must be
/// added here, with the channels it shows, before the suite passes.
const CONTRACTS: &[Contract] = &[
    Contract {
        kind: "handoff",
        audiences: Audiences::Review,
        shows: &[Channel::Derived, Channel::Lesson, Channel::Statement],
    },
    Contract {
        kind: "case-report",
        audiences: Audiences::Review,
        shows: &[
            Channel::Derived,
            Channel::Statement,
            Channel::Quote,
            Channel::Lesson,
            Channel::Cited,
            Channel::NotePath,
            Channel::NoteBytes,
            Channel::RunMeta,
        ],
    },
    Contract {
        kind: "analysis-report",
        audiences: Audiences::Review,
        shows: &[Channel::Statement, Channel::Lesson, Channel::RunMeta],
    },
    Contract {
        kind: "decision-record",
        audiences: Audiences::Review,
        shows: &[
            Channel::Derived,
            Channel::Statement,
            Channel::NotePath,
            Channel::NoteBytes,
        ],
    },
    Contract {
        kind: "method-retrospective",
        audiences: Audiences::Review,
        shows: &[Channel::Lesson, Channel::RunMeta],
    },
    Contract {
        kind: "review-vault",
        audiences: Audiences::Review,
        shows: &[
            Channel::Derived,
            Channel::Statement,
            Channel::Rationale,
            Channel::Selector,
            Channel::Quote,
            Channel::FileName,
            Channel::Source,
            Channel::Link,
            Channel::Annotation,
            Channel::Lesson,
            Channel::Cited,
            Channel::NotePath,
            Channel::RunFile,
            Channel::RunMeta,
        ],
    },
    Contract {
        kind: "workspace-archive",
        audiences: Audiences::Recovery,
        shows: &Channel::ALL,
    },
    Contract {
        kind: "producer-bundle",
        audiences: Audiences::Review,
        shows: &[Channel::RunFile, Channel::RunBytes, Channel::RunMeta],
    },
];

fn contract(name: &str) -> &'static Contract {
    CONTRACTS
        .iter()
        .find(|contract| contract.kind == name)
        .unwrap_or_else(|| {
            panic!(
                "export kind {name} has no contract in the W11.16 leak suite: add it to \
                 CONTRACTS in crates/investigation-cli/tests/export_leaks.rs"
            )
        })
}

#[test]
fn every_export_kind_has_a_registered_contract() {
    let registered: BTreeSet<&str> = CONTRACTS.iter().map(|contract| contract.kind).collect();
    let kinds: BTreeSet<&str> = KINDS.iter().map(|spec| spec.name).collect();
    assert_eq!(registered, kinds, "CONTRACTS and export::KINDS differ");
    assert_eq!(CONTRACTS.len(), KINDS.len(), "a kind is registered twice");
    for spec in KINDS {
        let contract = contract(spec.name);
        assert!(spec.available(), "{} has no renderer", spec.name);
        assert_eq!(
            contract.audiences == Audiences::Recovery,
            spec.product.resumable(),
            "{}: only a resumable kind has the recovery scope",
            spec.name
        );
        assert_eq!(
            contract.audiences == Audiences::Recovery,
            spec.scope() == Scope::All,
            "{}",
            spec.name
        );
    }
}

// ------------------------------------------------------------- Binary

fn invoke(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> (i32, Value) {
    let mut full: Vec<&str> = args.to_vec();
    full.extend(["--format", "json"]);
    let mut process = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_clear()
        .args(&full)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(stdin) = stdin {
        process.stdin.take().unwrap().write_all(stdin).unwrap();
    }
    let output = process.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value = serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!(
            "{args:?}: {error}\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap(), value)
}

fn ok(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> Value {
    let (code, value) = invoke(cwd, args, stdin);
    assert_eq!(code, 0, "{args:?}: {value:#}");
    value
}

// ------------------------------------------------------------- Fixture

/// One fixture workspace with the canary layer added to one case.
struct Planted {
    _directory: tempfile::TempDir,
    /// The temporary folder everything lives in; no export may name it.
    root: PathBuf,
    workspace: PathBuf,
    case: String,
    /// The case sequence before the withheld options were raised.
    before_raise: u64,
    /// The public decisions whose chosen option is withheld (bn-31an).
    derived: Vec<String>,
    /// The digest of the internal note whose bytes quote a record ID.
    id_note_digest: String,
    /// The restricted observation (bn-91gz).
    withheld_observation: String,
    label: String,
    /// Exports made so far, numbering their folders.
    exports: std::sync::atomic::AtomicUsize,
}

struct Layer<'a> {
    workspace: &'a Path,
    inputs: PathBuf,
    /// The inputs folder relative to the import root, where producer run
    /// envelopes name their artifacts.
    below_root: &'static str,
    case: String,
    wrap: &'a dyn Fn(&str) -> String,
    keys: usize,
    /// Revisions the receipts reported, by ID.
    revisions: std::cell::RefCell<BTreeMap<String, u64>>,
}

impl Layer<'_> {
    fn key(&mut self, what: &str) -> String {
        self.keys += 1;
        format!("leak-{}-{what}", self.keys)
    }

    fn store(&self) -> Store {
        Store::open(Workspace::open(self.workspace).unwrap())
    }

    fn case_id(&self) -> Id {
        self.case.parse().unwrap()
    }

    fn state(&self) -> (State, Value) {
        let mut store = self.store();
        let replay = store.case(self.case_id()).unwrap();
        let snapshot = serde_json::to_value(replay.snapshot).unwrap();
        (replay.state, snapshot)
    }

    /// The current revision of `id`.
    fn current(&self, id: &str) -> Value {
        if let Some(revision) = self.revisions.borrow().get(id) {
            return json!({"id": id, "revision": revision});
        }
        let (state, _) = self.state();
        let revision = state
            .current(id.parse().unwrap())
            .unwrap_or_else(|| panic!("{id} is not in the case"))
            .header
            .revision
            .0;
        json!({"id": id, "revision": revision})
    }

    fn submit(&mut self, actor: &str, op: &str, payload: Value) -> Value {
        let key = self.key(op);
        let command = json!({
            "schema_version": "inv.command.v1", "command_id": key, "case_id": self.case,
            "actor": actor, "preconditions": [], "operations": [{"op": op, "payload": payload}]
        });
        let receipt = ok(
            self.workspace,
            &["record", "--input", "-"],
            Some(command.to_string().as_bytes()),
        );
        assert_eq!(receipt["result"], "committed", "{op}: {receipt:#}");
        self.note(&receipt);
        receipt
    }

    /// Remember the revisions a receipt reports, so [`Self::current`] needs
    /// no replay for them.
    fn note(&self, receipt: &Value) {
        let mut revisions = self.revisions.borrow_mut();
        for change in ["created", "updated"]
            .into_iter()
            .filter_map(|field| receipt[field].as_array())
            .flatten()
        {
            if let (Some(id), Some(revision)) = (change["id"].as_str(), change["revision"].as_u64())
            {
                revisions.insert(id.to_owned(), revision);
            }
        }
    }

    fn create(&mut self, op: &str, payload: Value) -> String {
        let receipt = self.submit(ACTOR, op, payload);
        receipt["created"][0]["id"].as_str().unwrap().to_owned()
    }

    /// Raise a record to restricted, unless it already is (a record citing
    /// restricted evidence inherits its level).
    fn raise(&mut self, id: &str) {
        let subject = self.current(id);
        let reason = (self.wrap)(&format!("{RESTRICTED}statement raise reason"));
        let key = self.key("raise");
        let command = json!({
            "schema_version": "inv.command.v1", "command_id": key, "case_id": self.case,
            "actor": ACTOR, "preconditions": [], "operations": [{"op": "classification.raise",
                "payload": {"subject_ref": subject, "level": "restricted", "reason": reason}}]
        });
        let (code, receipt) = invoke(
            self.workspace,
            &["record", "--input", "-"],
            Some(command.to_string().as_bytes()),
        );
        if code == 0 {
            self.note(&receipt);
        } else {
            assert_eq!(receipt["error"]["actual"], "restricted", "{receipt:#}");
        }
    }

    /// Clear a record for the public audience by a reviewed downgrade.
    fn clear(&mut self, id: &str) {
        let subject = self.current(id);
        self.submit(
            REVIEWER,
            "classification.downgrade",
            json!({
                "subject_ref": subject, "level": "public",
                "reason": "Reviewed for publication.",
                "transformation": {
                    "inputs": [{"input_ref": subject, "digest": null}],
                    "transformation": "manual_review", "version": "1",
                    "redaction_decisions": [{"scope": "Whole record", "action": "retained",
                        "rationale": "Nothing in it is sensitive."}],
                    "output_digest": null
                },
                "review": {"method": "Read the whole record.", "coverage": "whole_output",
                    "examined": [subject]}
            }),
        );
    }

    /// [`Self::wrap`] for a one-line field: no control characters, at most
    /// 256 characters.
    fn line(&self, text: &str) -> String {
        let wrapped: String = (self.wrap)(text)
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        if wrapped.chars().count() > 256 {
            text.to_owned()
        } else {
            wrapped
        }
    }

    fn text(&self, level: &str, channel: Channel, rest: &str) -> String {
        (self.wrap)(&format!("{} {rest}", channel.token(level)))
    }

    fn observation(&mut self, statement: String) -> String {
        self.create(
            "observation.add",
            json!({
                "value": {"kind": "text", "statement": statement},
                "basis": {"kind": "assertion", "assertion": "Asserted for the leak suite.",
                    "context": null},
                "extraction_method": null, "validity": "valid", "quality": [], "limitations": [],
                "evidence_kind": "observational", "variables_changed": [], "observed_at": null
            }),
        )
    }

    fn conclusion(&mut self, statement: String, support: &[&str]) -> String {
        let support: Vec<Value> = support.iter().map(|id| self.current(id)).collect();
        self.create(
            "conclusion.create",
            json!({
                "statement": statement, "scope": "Scope of the statement.",
                "significance": "Significance of the statement.",
                "support_refs": support, "contradiction_refs": [], "caveats": [],
                "causal_factors": [], "confidence": null, "likelihood": null
            }),
        )
    }

    fn option(&mut self, question: &str, description: String) -> String {
        let question = self.current(question);
        self.create(
            "option.create",
            json!({"question_ref": question, "description": description,
                "dependencies": [], "tradeoff_notes": []}),
        )
    }

    fn decision(&mut self, question: &str, option: &str) -> String {
        let question = self.current(question);
        let option = self.current(option);
        let text = |what: &str| (self.wrap)(&format!("{PUBLIC} decision {what}"));
        let payload = json!({
            "question_ref": question,
            "choice": {"kind": "select", "option_ref": option},
            "compared_alternatives": [option],
            "comparison_refs": [], "hard_conditions": [], "evidence_refs": [],
            "assumption_refs": [], "tradeoffs": [text("tradeoff")],
            "consequences": [text("consequence")],
            "reopen_trigger": text("reopen trigger"), "reversal_conditions": []
        });
        self.create("decision.propose", payload)
    }

    /// Write an input file and return its absolute path.
    fn input(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.inputs.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn ingest(&mut self, manifest: &Value) -> Value {
        let key = self.key("ingest-manifest");
        let path = self.input(&format!("{key}.json"), manifest.to_string().as_bytes());
        ok(
            self.workspace,
            &[
                "--case",
                &self.case.clone(),
                "ingest",
                "--input",
                path.to_str().unwrap(),
            ],
            None,
        )
    }

    fn retrospective(&mut self, level: &str, run: &str) -> String {
        let (_, snapshot) = self.state();
        let case_ref = self.current(&self.case.clone());
        let text = |what: &str| self.text(level, Channel::Lesson, what);
        let payload = json!({
            "case_ref": case_ref, "snapshot": snapshot,
            "methods_attempted": [{"name": text("method"), "inquiry_ref": null}],
            "effective_steps": [{"description": text("effective step"), "support_refs": [run]}],
            "invalid_or_misleading_steps": [],
            "none_observed_reason": "None was observed.",
            "costs_or_constraints": text("costs"),
            "residual_unknowns": [text("unknown")],
            "lessons": [{"guidance": text("guidance"), "applies_when": text("applies when"),
                "support_refs": [run], "transfer_limits": text("transfer limits")}],
        });
        self.create("retrospective.record", payload)
    }

    /// A Retrospective whose own fields are public and whose every step and
    /// lesson cites every record in `support` (bn-91gz).
    fn cited_retrospective(&mut self, support: &[&str]) -> String {
        let (_, snapshot) = self.state();
        let case_ref = self.current(&self.case.clone());
        let public = |what: &str| (self.wrap)(&format!("{CITED_ACCOUNT} {what}"));
        let cited = |what: &str| self.text(RESTRICTED, Channel::Cited, what);
        let payload = json!({
            "case_ref": case_ref, "snapshot": snapshot,
            "methods_attempted": [{"name": public("method"), "inquiry_ref": null}],
            "effective_steps": [{"description": cited("effective step"),
                "support_refs": support}],
            "invalid_or_misleading_steps": [{"description": cited("misleading step"),
                "support_refs": support}],
            "none_observed_reason": null,
            "costs_or_constraints": public("costs"),
            "residual_unknowns": [public("unknown")],
            "lessons": [{"guidance": cited("guidance"), "applies_when": cited("applies when"),
                "support_refs": support, "transfer_limits": cited("transfer limits")}],
        });
        self.create("retrospective.record", payload)
    }

    fn snapshot_note(&mut self, path: &str, bytes: &[u8], level: &str) {
        let file = self.workspace.join("vault").join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, bytes).unwrap();
        let key = self.key("note");
        let case = self.case.clone();
        ok(
            self.workspace,
            &[
                "--actor",
                ACTOR,
                "--case",
                &case,
                "vault",
                "snapshot-note",
                path,
                "--on",
                &case,
                "--classification",
                level,
                "--command-id",
                &key,
            ],
            None,
        );
    }

    fn producer_run(&mut self, level: &str, external: &str) -> Value {
        let (file, bytes) = if level == PUBLIC_LEVEL {
            (
                format!("{PUBLIC}runfile-{external}.json"),
                format!("{{\"ok\":\"{PUBLIC}runbytes\"}}\n"),
            )
        } else {
            let marker = marker(level);
            (
                format!("{}-{external}.json", Channel::RunFile.token(marker)),
                format!("{{\"secret\":\"{}\"}}\n", Channel::RunBytes.token(marker)),
            )
        };
        self.input(&file, bytes.as_bytes());
        let meta = Channel::RunMeta.token(marker(level));
        let envelope = json!({
            "schema_version": "inv.producer-run.v1",
            "producer": {"name": "example-benchmark", "instance": format!("{meta}-instance")},
            "external_run_id": format!("{external}-{meta}"),
            "intent": {"label": self.line(&format!("{meta} intent"))},
            "execution": {"state": "completed", "exit_code": 0},
            "environment": {"workload": format!("{meta}-workload")},
            "artifacts": [{"role": "original-output", "path": format!("{}{file}", self.below_root),
                           "classification": level,
                           "metadata": {"example-org.note": format!("{meta} artifact note")}}],
            "assertions": [{"kind": "reported_metric", "name": format!("{meta}.metric"),
                "value": 1}],
            "controls": [{"name": format!("{meta}-control"), "producer_result": "reported_pass"}],
            "limitations": [self.line(&format!("{meta} limitation"))],
            "classification": level,
            "metadata": {"example-org.instrument": {"label": format!("{meta} metadata")}}
        });
        let path = self.input(
            &format!("{external}.envelope.json"),
            envelope.to_string().as_bytes(),
        );
        let case = self.case.clone();
        ok(
            self.workspace,
            &[
                "--actor",
                ACTOR,
                "--case",
                &case,
                "ingest",
                "--input",
                path.to_str().unwrap(),
            ],
            None,
        )
    }
}

const PUBLIC_LEVEL: &str = "public";

/// The token prefix of a classification level.
fn marker(level: &str) -> &'static str {
    match level {
        "restricted" => RESTRICTED,
        "internal" => INTERNAL,
        _ => PUBLIC,
    }
}

/// The hostile texts every token is wrapped in, in the hostile fixture.
fn hostile_texts() -> Vec<String> {
    let path =
        fixtures::repository().join("crates/investigation-render/tests/fixtures/hostile-text.json");
    let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    value["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| case["text"].as_str().unwrap().to_owned())
        // A NUL is refused as authored text; every other case is accepted.
        .filter(|text| !text.contains('\0'))
        .collect()
}

/// Copy a scenario (or create a fresh hostile workspace) and add the canary
/// layer to its first case.
#[allow(clippy::too_many_lines)]
fn planted(fixture: &str) -> Planted {
    // LEAK_SUITE_KEEP=1 keeps the fixture and its exports for inspection.
    let directory = tempfile::Builder::new()
        .disable_cleanup(std::env::var_os("LEAK_SUITE_KEEP").is_some())
        .tempdir()
        .unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    eprintln!("{fixture}: {}", root.display());
    let workspace = root.join("workspace");
    let hostile = fixture == "hostile";
    let (inputs, case) = if hostile {
        let inputs = root.join("inputs");
        fs::create_dir_all(&inputs).unwrap();
        ok(
            &root,
            &[
                "init",
                workspace.to_str().unwrap(),
                "--import-root",
                inputs.to_str().unwrap(),
            ],
            None,
        );
        let command = json!({
            "schema_version": "inv.command.v1", "command_id": "leak-case", "case_id": null,
            "actor": ACTOR, "preconditions": [], "operations": [{"op": "case.create",
                "payload": {"title": format!("{PUBLIC} hostile case"), "mode": "evaluate"}}]
        });
        let receipt = ok(
            &workspace,
            &["record", "--input", "-"],
            Some(command.to_string().as_bytes()),
        );
        (
            inputs,
            receipt["created"][0]["id"].as_str().unwrap().to_owned(),
        )
    } else {
        fixtures::workspace(fixture, &workspace, false);
        // The scenario's import root is the workspace itself.
        let inputs = workspace.join("inputs");
        fs::create_dir_all(&inputs).unwrap();
        let mut cases: Vec<String> = fs::read_dir(workspace.join("state/cases"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        cases.sort();
        (inputs, cases[0].clone())
    };
    let texts = hostile_texts();
    let counter = std::cell::Cell::new(0usize);
    let wrap = |token: &str| -> String {
        if !hostile {
            return token.to_owned();
        }
        let index = counter.get();
        counter.set(index + 1);
        format!(
            "{} {token} {}",
            texts[index % texts.len()],
            texts[(index + 1) % texts.len()]
        )
    };
    let mut layer = Layer {
        workspace: &workspace,
        inputs,
        below_root: if hostile { "" } else { "inputs/" },
        case: case.clone(),
        wrap: &wrap,
        keys: 0,
        revisions: std::cell::RefCell::default(),
    };
    let l = &mut layer;
    // The case itself is cleared, so a public export has something to show.
    l.clear(&case);

    // A public question with public, internal and restricted options, and a
    // public decision for each (bn-31an: a decision's title is derived from
    // its option's).
    let question = l.create(
        "question.create",
        json!({"wording": (l.wrap)(&format!("{PUBLIC} question")), "importance": "normal"}),
    );
    l.clear(&question);
    let open = l.option(&question, (l.wrap)(&format!("{PUBLIC} option")));
    l.clear(&open);
    let mut derived = Vec::new();
    let mut raised_options = Vec::new();
    for level in [INTERNAL, RESTRICTED] {
        let option = l.option(
            &question,
            l.text(level, Channel::Derived, "withheld option"),
        );
        let decision = l.decision(&question, &option);
        l.clear(&decision);
        derived.push(decision);
        raised_options.push((level, option));
    }
    // A public decision whose decisive objection is restricted, and a public
    // conclusion challenged by a restricted assessment.
    let decision = l.decision(&question, &open);
    l.clear(&decision);
    let subject = l.current(&decision);
    let objection = l.create(
        "objection.raise",
        json!({"subject_ref": subject,
            "statement": l.text(RESTRICTED, Channel::Statement, "decisive objection"),
            "scope": l.text(RESTRICTED, Channel::Statement, "objection scope"),
            "severity": "critical", "evidence_refs": {"state": "unknown"}}),
    );
    l.raise(&objection);
    let clean = l.conclusion((l.wrap)(&format!("{PUBLIC} clean conclusion")), &[]);
    l.clear(&clean);
    let challenged = l.conclusion((l.wrap)(&format!("{PUBLIC} challenged conclusion")), &[]);
    l.clear(&challenged);
    let internal_observation = l.observation(l.text(INTERNAL, Channel::Statement, "observation"));
    let subject = l.current(&challenged);
    let support = l.current(&internal_observation);
    let assessment = l.create(
        "assessment.add",
        json!({"subject_ref": subject, "evidence_refs": [support],
            "relation": "inconclusive",
            "rationale": l.text(RESTRICTED, Channel::Rationale, "assessment rationale"),
            "rival_expectations": [], "assumption_refs": [],
            "confidence": null, "likelihood": null}),
    );
    l.raise(&assessment);

    // A restricted source: title, file name, bytes, metadata and a link, an
    // observation quoting it through a selector, and a public conclusion and
    // an internal one resting on that observation.
    let original = format!(
        "{} line\n{}\n{}\n",
        Channel::Bytes.token(RESTRICTED),
        Channel::Selector.token(RESTRICTED),
        Channel::Quote.token(RESTRICTED)
    );
    let file = l.input(
        &format!("{}.txt", Channel::FileName.token(RESTRICTED)),
        original.as_bytes(),
    );
    let key = l.key("source");
    let receipt = l.ingest(&json!({
        "schema_version": "inv.ingest.v1", "command_id": key, "actor": ACTOR, "case_id": case,
        "source": {"title": l.text(RESTRICTED, Channel::Source, "source title"),
            "format": "text/plain",
            "context": l.text(RESTRICTED, Channel::Link,
                &format!("see https://example.invalid/{}", Channel::Link.token(RESTRICTED))),
            "metadata": {"example-org.origin": {"note": Channel::Metadata.token(RESTRICTED)}}},
        "paths": [file], "classification": "restricted", "extract_text": false
    }));
    let item = &receipt["items"][0];
    let (source, artifact) = (
        item["source_id"].as_str().unwrap().to_owned(),
        item["artifact_id"].as_str().unwrap().to_owned(),
    );
    let digest = item["digest"].clone();
    let artifact_ref = l.current(&artifact);
    // A restricted JSON artifact the selector channel points into.
    let data = l.input(
        "data.json",
        format!("{{\"{}\": 1}}\n", Channel::Selector.token(RESTRICTED)).as_bytes(),
    );
    let key = l.key("data");
    let receipt = l.ingest(&json!({
        "schema_version": "inv.ingest.v1", "command_id": key, "actor": ACTOR, "case_id": case,
        "source": {"title": format!("{RESTRICTED}source data"), "format": "application/json"},
        "paths": [data], "classification": "restricted", "extract_text": false
    }));
    let data_ref = l.current(receipt["items"][0]["artifact_id"].as_str().unwrap());
    let data_digest = receipt["items"][0]["digest"].clone();
    let restricted_observation = l.create(
        "observation.add",
        json!({
            "value": {"kind": "text",
                "statement": l.text(RESTRICTED, Channel::Statement, "observation")},
            "basis": {"kind": "source", "artifact_ref": data_ref,
                "selector": {"schema_version": "inv.selector.v1", "artifact_digest": data_digest,
                    "selector": {"type": "json_pointer", "parser": null,
                        "pointer": format!("/{}", Channel::Selector.token(RESTRICTED))}},
                "text_quote": {"exact": "1"}},
            "extraction_method": null, "validity": "valid", "quality": [], "limitations": [],
            "evidence_kind": "observational", "variables_changed": [], "observed_at": null
        }),
    );
    let quoting = l.create(
        "observation.add",
        json!({
            "value": {"kind": "text",
                "statement": l.text(RESTRICTED, Channel::Statement, "quoting observation")},
            "basis": {"kind": "source", "artifact_ref": artifact_ref,
                "selector": {"schema_version": "inv.selector.v1", "artifact_digest": digest,
                    "selector": {"type": "text_lines", "start_line": 3, "end_line": 3,
                        "encoding": "utf-8", "parser_version": "inv.text.utf8.v1"}},
                "text_quote": {"exact": Channel::Quote.token(RESTRICTED)}},
            "extraction_method": null, "validity": "valid", "quality": [], "limitations": [],
            "evidence_kind": "observational", "variables_changed": [], "observed_at": null
        }),
    );
    l.raise(&quoting);
    l.raise(&restricted_observation);
    let rests = l.conclusion(
        (l.wrap)(&format!("{PUBLIC} conclusion on restricted evidence")),
        &[&restricted_observation],
    );
    l.clear(&rests);
    l.conclusion(
        l.text(
            INTERNAL,
            Channel::Statement,
            "conclusion on restricted evidence",
        ),
        &[&restricted_observation],
    );

    // A reviewed public derivative of the restricted original: the only
    // attachment a public review copy may carry.
    let derived_file = l.input("redacted.txt", format!("{PUBLIC} attachment\n").as_bytes());
    let key = l.key("derivative");
    let receipt = l.ingest(&json!({
        "schema_version": "inv.ingest.v1", "command_id": key, "actor": ACTOR, "case_id": case,
        "source": {"title": format!("{PUBLIC} redacted"), "format": "text/plain"},
        "paths": [derived_file], "classification": "public", "extract_text": false,
        "derived_from": {
            "inputs": [{"artifact_ref": artifact_ref, "digest": digest}],
            "transformation": "example-org.redact", "version": "2",
            "redaction_decisions": [{"scope": "Codenames", "action": "removed",
                "rationale": "Not needed."}],
            "review": {"method": "Read the whole output against the original.",
                "coverage": "whole_output"}
        }
    }));
    let derivative = receipt["items"][0]["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let derivative_digest = receipt["items"][0]["digest"].clone();
    let derivative_ref = l.current(&derivative);
    l.submit(
        REVIEWER,
        "classification.downgrade",
        json!({
            "subject_ref": derivative_ref, "level": "public",
            "reason": "The codename was removed and the result reviewed.",
            "transformation": {
                "inputs": [{"input_ref": artifact_ref, "digest": digest},
                    {"input_ref": {"id": source, "revision": 1}, "digest": null}],
                "transformation": "example-org.redact", "version": "2",
                "redaction_decisions": [{"scope": "Codenames", "action": "removed",
                    "rationale": "Not needed."}],
                "output_digest": derivative_digest
            },
            "review": {"method": "Read the whole output.", "coverage": "whole_output",
                "examined": [derivative_ref]}
        }),
    );

    // Annotations on the public decision, internal and restricted.
    for (marker, level) in [(INTERNAL, "internal"), (RESTRICTED, "restricted")] {
        let subject = l.current(&decision);
        l.create(
            "annotation.add",
            json!({"subject_refs": [subject],
                "content": {"kind": "text", "text": l.text(marker, Channel::Annotation, "note")},
                "classification": {"level": level, "reason": null, "tags": []}}),
        );
    }

    // Human note snapshots (W11.14): the path and the captured bytes.
    for (marker, level) in [(INTERNAL, "internal"), (RESTRICTED, "restricted")] {
        let path = format!("Notes/{}.md", Channel::NotePath.token(marker));
        let bytes = format!(
            "# Plan\n\n{} kept here.\n",
            Channel::NoteBytes.token(marker)
        );
        l.snapshot_note(&path, bytes.as_bytes(), level);
    }

    // An internal note whose captured bytes quote a record ID: a remapped
    // export would rewrite it, so it must list it without copying it.
    let id_note = format!("# Links\n\nSee {case} for the plan.\n");
    l.snapshot_note("Notes/ids.md", id_note.as_bytes(), "internal");
    let id_note_digest = hash_bytes(id_note.as_bytes());

    // Producer runs at every level.
    for level in ["public", "internal", "restricted"] {
        l.producer_run(level, &format!("leak-{level}"));
    }

    // Retrospectives: a public one, an internal one, a public one whose
    // steps and lessons cite the restricted observation (bn-91gz), and one
    // raised to restricted after it was recorded. The cited one is the
    // latest an `internal` or `public` audience may see.
    let run = l.create(
        "run.record",
        json!({"intent": {"kind": "exploration", "intent": "Trace one burst."},
            "producer": "agent:producer", "artifact_refs": [], "execution_status": "completed",
            "evidence_completeness": "complete", "evidence_kind": "observational",
            "variables_changed": []}),
    );
    l.clear(&run);
    let public_retrospective = l.retrospective(PUBLIC, &run);
    l.clear(&public_retrospective);
    l.retrospective(INTERNAL, &run);
    let cited = l.cited_retrospective(&[&run, &restricted_observation]);
    l.clear(&cited);
    let secret = l.retrospective(RESTRICTED, &run);
    l.raise(&secret);

    // Raise the restricted option last: the decisions were cleared while it
    // was internal, so a re-export pinned before the raise sees it in the
    // snapshot's own classification and only today's policy withholds it.
    let before_raise = l.state().1["sequence"].as_u64().unwrap();
    for (level, option) in raised_options {
        if level == RESTRICTED {
            l.raise(&option);
        }
    }

    // A dirty working vault: a conflict copy and a stale generated note.
    let notes = workspace.join("vault/Cases").join(&case);
    fs::create_dir_all(notes.join("Conclusions")).unwrap();
    fs::write(
        notes.join("Overview (conflict copy).md"),
        format!("# Overview\n\n{DIRTY}conflict copy\n"),
    )
    .unwrap();
    fs::write(
        notes.join(format!("Conclusions/{DIRTY}stale.md")),
        format!("---\ninv_id: stale\n---\n{DIRTY}stale note\n"),
    )
    .unwrap();
    drop(layer);
    Planted {
        _directory: directory,
        root,
        workspace,
        case,
        before_raise,
        derived,
        id_note_digest: id_note_digest.as_str().to_owned(),
        withheld_observation: restricted_observation,
        label: fixture.to_owned(),
        exports: std::sync::atomic::AtomicUsize::new(0),
    }
}

// ------------------------------------------------------------- Exports

/// One export read back from disk, indexed once for the scans.
struct Exported {
    files: BTreeMap<String, Vec<u8>>,
    receipt: Value,
    /// Each file's path and text, lower case.
    lower: Vec<(String, String)>,
    /// Each file's text with remapped IDs restored to canonical ones, by
    /// its restored path.
    restored: BTreeMap<String, String>,
    /// Every UUID-shaped value and every 32- and 64-digit hex run in any
    /// path or byte, lower case, as written.
    uuids: BTreeSet<String>,
    hex32: BTreeSet<String>,
    hex64: BTreeSet<String>,
}

/// Whether `window` has the shape of a hyphenated UUID.
fn uuid_shaped(window: &[u8]) -> bool {
    window.len() == 36
        && window.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// Replace every UUID-shaped value `map` names.
fn restore_text(text: &str, map: &BTreeMap<String, String>) -> String {
    if map.is_empty() {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut copied = 0;
    while at + 36 <= bytes.len() {
        let window = &bytes[at..at + 36];
        if uuid_shaped(window)
            && let Some(canonical) = map.get(std::str::from_utf8(window).unwrap())
        {
            out.push_str(&text[copied..at]);
            out.push_str(canonical);
            at += 36;
            copied = at;
        } else {
            at += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

impl Exported {
    fn new(files: BTreeMap<String, Vec<u8>>, receipt: Value) -> Self {
        // Exported to canonical, bare UUIDs; the bundle names the case.
        let identifiers = &receipt["publisher_receipt"]["identifiers"];
        let uuid = |value: &Value| {
            let text = value.as_str().unwrap();
            text.split_once('_')
                .map_or(text, |(_, uuid)| uuid)
                .to_owned()
        };
        let mut map: BTreeMap<String, String> = identifiers["ids"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|pair| (uuid(&pair["exported"]), uuid(&pair["canonical"])))
            .collect();
        if !map.is_empty() {
            map.insert(
                uuid(&identifiers["exported_snapshot"]["case_id"]),
                uuid(&identifiers["canonical_snapshot"]["case_id"]),
            );
        }
        let mut lower = Vec::new();
        let mut restored = BTreeMap::new();
        let (mut uuids, mut hex32, mut hex64) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        for (path, bytes) in &files {
            let text = String::from_utf8_lossy(bytes);
            let low = format!("{path}\n{text}").to_ascii_lowercase();
            let raw = low.as_bytes();
            for start in 0..raw.len().saturating_sub(35) {
                if uuid_shaped(&raw[start..start + 36]) {
                    uuids.insert(low[start..start + 36].to_owned());
                }
            }
            let mut run = 0;
            for (index, byte) in raw.iter().chain([&b' ']).enumerate() {
                if byte.is_ascii_hexdigit() {
                    run += 1;
                    continue;
                }
                match run {
                    32 => hex32.insert(low[index - 32..index].to_owned()),
                    64 => hex64.insert(low[index - 64..index].to_owned()),
                    _ => false,
                };
                run = 0;
            }
            lower.push((path.to_ascii_lowercase(), low));
            restored.insert(restore_text(path, &map), restore_text(&text, &map));
        }
        Self {
            files,
            receipt,
            lower,
            restored,
            uuids,
            hex32,
            hex64,
        }
    }

    /// Paths and lines holding `needle`, compared in lower case.
    fn find(&self, needle: &str) -> Vec<String> {
        let needle = needle.to_ascii_lowercase();
        let mut found = Vec::new();
        for (path, text) in &self.lower {
            if !text.contains(&needle) {
                continue;
            }
            for line in text.lines().filter(|line| line.contains(&needle)) {
                let shown: String = line.chars().take(200).collect();
                found.push(format!("{path}: {shown}"));
            }
        }
        found
    }

    /// Whether the record ID appears in any form: prefixed or bare, any
    /// letter case, hyphenated or as 32 digits.
    fn names(&self, id: Id) -> bool {
        let uuid = bare(id);
        self.uuids.contains(&uuid) || self.hex32.contains(&uuid.replace('-', ""))
    }

    /// The (canonical, exported) ID pairs of a remapped export, prefixed.
    fn pairs(&self) -> Vec<(String, String)> {
        self.receipt["publisher_receipt"]["identifiers"]["ids"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .map(|pair| {
                        (
                            pair["canonical"].as_str().unwrap().to_owned(),
                            pair["exported"].as_str().unwrap().to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn remapped(&self) -> bool {
        self.receipt["identifiers"] == "remapped"
    }
}

fn read_tree(at: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![at.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let kind = fs::symlink_metadata(&path).unwrap().file_type();
            assert!(!kind.is_symlink(), "an export holds a link: {path:?}");
            if kind.is_dir() {
                pending.push(path);
            } else {
                found.insert(
                    path.strip_prefix(at).unwrap().to_str().unwrap().to_owned(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    found
}

fn audience_name(audience: Audience) -> &'static str {
    audience.as_str()
}

/// Export through the binary; `Err` holds a refusal's JSON.
fn export(
    planted: &Planted,
    name: &str,
    audience: Audience,
    remap: bool,
    pin: Option<u64>,
) -> Result<Exported, Value> {
    let count = planted
        .exports
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        + 1;
    let out = planted.root.join(format!(
        "out-{count:03}-{name}-{}-{remap}-{}",
        audience_name(audience),
        pin.map_or_else(|| "head".to_owned(), |sequence| sequence.to_string())
    ));
    let mut args: Vec<String> = Vec::new();
    if kind(name).unwrap().scope() == Scope::Case {
        args.extend(["--case".into(), planted.case.clone()]);
    }
    args.extend([
        "export".into(),
        "--kind".into(),
        name.into(),
        "--audience".into(),
        audience_name(audience).into(),
        "--out".into(),
        out.to_str().unwrap().into(),
    ]);
    if remap {
        args.push("--remap-ids".into());
    }
    if let Some(sequence) = pin {
        args.extend(["--snapshot".into(), sequence.to_string()]);
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, receipt) = invoke(&planted.workspace, &args, None);
    if code != 0 {
        assert!(!out.exists(), "a refused export wrote {out:?}");
        return Err(receipt);
    }
    Ok(Exported::new(read_tree(&out), receipt))
}

// ------------------------------------------------------------ Ground truth

/// What the audience policy decides for one export, computed in process.
struct Truth {
    /// Withheld record IDs, the case itself excluded (it names the export).
    withheld: Vec<Id>,
    /// Caveated records and their caveats.
    caveated: Vec<(Id, Vec<Caveat>)>,
    /// Digests of withheld artifacts no shown artifact shares.
    withheld_digests: Vec<String>,
    /// Every record of the case, for remapped exports.
    records: Vec<Id>,
    /// Every verified head hash of the case, hex.
    heads: Vec<String>,
}

fn truth(planted: &Planted, audience: Audience, pin: Option<u64>) -> Truth {
    let mut store = Store::open(Workspace::open(&planted.workspace).unwrap());
    let case: Id = planted.case.parse().unwrap();
    let pin = Pin {
        sequence: pin,
        head_hash: None,
    };
    let snapshot = ExportSnapshot::read(&mut store, case, &pin).unwrap();
    let plan = plan_export(kind("case-report").unwrap(), &snapshot, audience).unwrap();
    let state = snapshot.case().state();
    let withheld: Vec<Id> = plan
        .exclusions()
        .iter()
        .map(|exclusion| exclusion.id)
        .filter(|id| *id != case)
        .collect();
    let digest_of = |id: Id| {
        state.current(id).and_then(|record| {
            investigation_core::derivation::artifact_body(state, record.reference())
                .and_then(|body| body.digest)
        })
    };
    let shown_digests: BTreeSet<String> = state
        .records()
        .filter(|record| record.header.id.kind() == Kind::Artifact)
        .filter(|record| !withheld.contains(&record.header.id))
        .filter_map(|record| digest_of(record.header.id))
        .map(|digest| digest.as_str().to_owned())
        .collect();
    let withheld_digests = withheld
        .iter()
        .filter(|id| id.kind() == Kind::Artifact)
        .filter_map(|id| digest_of(*id))
        .map(|digest| digest.as_str().to_owned())
        .filter(|digest| !shown_digests.contains(digest))
        .map(|digest| digest.trim_start_matches("sha256:").to_owned())
        .collect();
    let replay = store.case(case).unwrap();
    let heads = (0..=replay.snapshot.sequence)
        .filter_map(|sequence| replay.state.verified_snapshot(sequence))
        .map(|digest| digest.as_str().trim_start_matches("sha256:").to_owned())
        .collect();
    Truth {
        withheld,
        caveated: plan
            .caveated()
            .iter()
            .map(|caveated| (caveated.id, caveated.caveats.clone()))
            .collect(),
        withheld_digests,
        records: replay
            .state
            .records()
            .map(|record| record.header.id)
            .collect(),
        heads,
    }
}

fn bare(id: Id) -> String {
    id.to_string().split_once('_').unwrap().1.to_owned()
}

/// Every digest the workspace retains bytes for, hex.
fn retained_digests(workspace: &Path) -> BTreeSet<String> {
    read_tree(&workspace.join("state/blobs/sha256"))
        .into_keys()
        .filter_map(|path| path.rsplit('/').next().map(str::to_owned))
        .filter(|name| name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .collect()
}

/// Whether `path` is a byte-for-byte copy of retained evidence: a human
/// note snapshot, a review attachment, a producer run's file or an archived
/// blob.
fn copied(path: &str) -> bool {
    path.starts_with("human/")
        || path.starts_with("Attachments/")
        || path.starts_with("blobs/")
        || (path.starts_with("runs/") && path.contains("/files/"))
}

fn hex(bytes: &[u8]) -> String {
    hash_bytes(bytes)
        .as_str()
        .trim_start_matches("sha256:")
        .to_owned()
}

/// Every copied byte is exactly a retained blob, and every file named by a
/// digest hashes to it: no remap, filter or rewrite ever altered evidence
/// (W11.14).
fn assert_copies_are_exact(label: &str, exported: &Exported, retained: &BTreeSet<String>) {
    for (path, bytes) in &exported.files {
        let digest = hex(bytes);
        if copied(path) {
            assert!(
                retained.contains(&digest),
                "{label}: {path} is not a retained blob's exact bytes"
            );
        }
        let name = path.rsplit('/').next().unwrap();
        if name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            assert_eq!(name, digest, "{label}: {path} does not hash to its name");
        }
    }
}

/// Lower-case tokens of a level present anywhere in an export, file names
/// included.
fn tokens(exported: &Exported, prefix: &str) -> BTreeSet<String> {
    let prefix = prefix.to_ascii_lowercase();
    let mut found = BTreeSet::new();
    for (path, bytes) in &exported.files {
        let text = format!("{path}\n{}", String::from_utf8_lossy(bytes)).to_ascii_lowercase();
        for (at, _) in text.match_indices(&prefix) {
            let token: String = text[at..]
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            found.insert(token);
        }
    }
    found
}

/// UUID-shaped values in `text`, in order.
fn uuids_in(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    (0..bytes.len().saturating_sub(35))
        .filter(|start| uuid_shaped(&bytes[*start..*start + 36]))
        .map(|start| &text[start..start + 36])
        .collect()
}

/// The effective states an export shows for each record: the backticked
/// word after `effective` or `Effective state:`, attributed to the nearest
/// record ID before it on its line, or else to the record the file is named
/// after. IDs are restored to canonical ones.
fn standings(exported: &Exported, ids: &[Id]) -> BTreeSet<(String, String)> {
    let records: BTreeSet<String> = ids.iter().map(|id| bare(*id)).collect();
    let mut found = BTreeSet::new();
    for (path, text) in &exported.restored {
        let own = uuids_in(path)
            .into_iter()
            .rfind(|uuid| records.contains(*uuid));
        for line in text.lines() {
            for marker in ["effective `", "Effective state: `"] {
                for (at, _) in line.match_indices(marker) {
                    let rest = &line[at + marker.len()..];
                    let Some(word) = rest.split('`').next() else {
                        continue;
                    };
                    let subject = uuids_in(&line[..at])
                        .into_iter()
                        .rfind(|uuid| records.contains(*uuid))
                        .or(own);
                    if let Some(subject) = subject {
                        found.insert((subject.to_owned(), word.to_owned()));
                    }
                }
            }
        }
    }
    found
}

/// Record a failed expectation and go on, so one run reports every leak.
macro_rules! expect {
    ($problems:expr, $condition:expr, $($message:tt)+) => {
        if !$condition {
            $problems.push(format!($($message)+));
        }
    };
}

/// The leak and caveat assertions for one review-product export below or at
/// `restricted`.
#[allow(clippy::too_many_lines)]
fn assert_review_export(
    planted: &Planted,
    label: &str,
    audience: Audience,
    exported: &Exported,
    truth: &Truth,
    full: &BTreeSet<(String, String)>,
    problems: &mut Vec<String>,
) {
    // Nothing from the dirty working vault, ever.
    let dirty = exported.find(DIRTY);
    expect!(problems, dirty.is_empty(), "{label}: {dirty:#?}");
    // No token above the audience.
    let above: &[&str] = match audience {
        Audience::Restricted => &[],
        Audience::Internal => &[RESTRICTED],
        Audience::Public => &[RESTRICTED, INTERNAL],
    };
    for prefix in above {
        let found = exported.find(prefix);
        expect!(
            problems,
            found.is_empty(),
            "{label}: {prefix} leaked: {found:#?}"
        );
    }
    // No path of the machine the export was made on.
    let root = planted.root.to_str().unwrap();
    let found = exported.find(root);
    expect!(
        problems,
        found.is_empty(),
        "{label}: local path leaked: {found:#?}"
    );
    // No withheld record, in any form, also behind a remapped value.
    for id in &truth.withheld {
        expect!(
            problems,
            !exported.names(*id),
            "{label}: withheld {id} is named: {:#?}",
            exported.find(&bare(*id))
        );
    }
    let withheld: BTreeSet<String> = truth.withheld.iter().map(|id| bare(*id)).collect();
    for (canonical, shown) in exported.pairs() {
        let canonical = canonical.split_once('_').unwrap().1.to_owned();
        expect!(
            problems,
            !withheld.contains(&canonical),
            "{label}: withheld {canonical} shown remapped as {shown}"
        );
    }
    for digest in &truth.withheld_digests {
        expect!(
            problems,
            !exported.hex64.contains(digest),
            "{label}: withheld digest {digest}: {:#?}",
            exported.find(digest)
        );
    }
    // A remapped export shows no canonical ID, creation time or head hash.
    if exported.remapped() {
        let times: BTreeSet<String> = exported
            .uuids
            .iter()
            .map(|uuid| uuid[..13].to_owned())
            .collect();
        for id in &truth.records {
            expect!(problems, !exported.names(*id), "{label}: canonical {id}");
            let time = &bare(*id)[..13];
            expect!(
                problems,
                !times.contains(time),
                "{label}: the creation time of {id}: {:#?}",
                exported.find(time)
            );
        }
        for head in &truth.heads {
            expect!(
                problems,
                !exported.hex64.contains(head),
                "{label}: head hash {head}"
            );
        }
    }
    // Caveats: verbatim beside every caveated record wherever it is shown;
    // none at all for the public, which gets no caveated record.
    if audience == Audience::Public {
        expect!(
            problems,
            truth.caveated.is_empty(),
            "{label}: {:?}",
            truth.caveated
        );
        for caveat in [Caveat::ChallengeWithheld, Caveat::BasisWithheld] {
            let found = exported.find(caveat.text());
            expect!(problems, found.is_empty(), "{label}: {found:#?}");
        }
    }
    for (id, caveats) in &truth.caveated {
        let uuid = bare(*id);
        for (path, text) in &exported.restored {
            if path == "export-manifest.json" {
                continue; // It names records only by reference.
            }
            if !text.contains(&uuid) {
                continue;
            }
            for caveat in caveats {
                expect!(
                    problems,
                    text.contains(caveat.text()),
                    "{label}: {path} shows {id} without its {} caveat",
                    caveat.as_str()
                );
            }
        }
    }
    // No record reads stronger than in the full copy.
    for standing in standings(exported, &truth.records) {
        expect!(
            problems,
            full.contains(&standing),
            "{label}: {standing:?} is not a standing the full copy shows"
        );
    }
}

/// Export every kind for every audience, with and without `--remap-ids`,
/// and hold each to its contract.
fn run_matrix(fixture: &str) -> usize {
    let planted = planted(fixture);
    let retained = retained_digests(&planted.workspace);
    // The policy's decisions are the same for every kind.
    let truths: Vec<(Audience, Truth)> =
        [Audience::Restricted, Audience::Internal, Audience::Public]
            .into_iter()
            .map(|audience| (audience, truth(&planted, audience, None)))
            .collect();
    let records = truths[0].1.records.clone();
    // Each kind in its own thread: the exports are separate processes.
    let results: Vec<(usize, Vec<String>, BTreeSet<String>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = KINDS
            .iter()
            .map(|spec| {
                let (planted, retained, truths, records) = (&planted, &retained, &truths, &records);
                scope.spawn(move || check_kind(spec, planted, retained, truths, records))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    let mut exports = 0;
    let mut problems = Vec::new();
    let mut restricted_tokens = BTreeSet::new();
    for (count, found, shown) in results {
        exports += count;
        problems.extend(found);
        restricted_tokens.extend(shown);
    }
    assert!(
        problems.is_empty(),
        "{} problem(s):\n{}",
        problems.len(),
        problems.join("\n")
    );
    // Not vacuous: every channel reached at least one kind.
    for channel in Channel::ALL {
        assert!(
            restricted_tokens.contains(&channel.token(RESTRICTED).to_ascii_lowercase()),
            "{}: no kind shows the {channel:?} channel even to restricted",
            planted.label
        );
    }
    exports
}

/// One kind's part of the matrix: the exports it made, the problems found
/// and the restricted tokens its restricted export shows.
#[allow(clippy::too_many_lines)]
fn check_kind(
    spec: &'static investigation_render::export::KindSpec,
    planted: &Planted,
    retained: &BTreeSet<String>,
    truths: &[(Audience, Truth)],
    records: &[Id],
) -> (usize, Vec<String>, BTreeSet<String>) {
    let mut exports = 0;
    let mut problems = Vec::new();
    let mut restricted_tokens = BTreeSet::new();
    {
        let contract = contract(spec.name);
        let label = |audience: Audience, remap: bool| {
            format!(
                "{} {} {} remap={remap}",
                planted.label,
                spec.name,
                audience.as_str()
            )
        };
        if contract.audiences == Audiences::Recovery {
            let exported = export(planted, spec.name, Audience::Restricted, false, None)
                .unwrap_or_else(|refusal| panic!("{}: {refusal:#}", spec.name));
            exports += 1;
            let found = exported.find(DIRTY);
            assert!(found.is_empty(), "{}: {found:#?}", spec.name);
            assert_copies_are_exact(&label(Audience::Restricted, false), &exported, retained);
            let shown = tokens(&exported, RESTRICTED);
            for channel in contract.shows {
                assert!(
                    shown.contains(&channel.token(RESTRICTED).to_ascii_lowercase()),
                    "{}: {channel:?} missing",
                    spec.name
                );
            }
            restricted_tokens.extend(shown);
            for (audience, remap) in [
                (Audience::Internal, false),
                (Audience::Public, false),
                (Audience::Restricted, true),
            ] {
                let refusal = export(planted, spec.name, audience, remap, None)
                    .err()
                    .unwrap_or_else(|| panic!("{} was written", label(audience, remap)));
                assert_eq!(refusal["ok"], false, "{refusal:#}");
            }
            return (exports, problems, restricted_tokens);
        }
        let full = export(planted, spec.name, Audience::Restricted, false, None)
            .unwrap_or_else(|refusal| panic!("{}: {refusal:#}", spec.name));
        let full_standings = standings(&full, records);
        for audience in [Audience::Restricted, Audience::Internal, Audience::Public] {
            let truth = &truths.iter().find(|(each, _)| *each == audience).unwrap().1;
            // Public is always remapped; --remap-ids changes nothing there.
            let remaps: &[bool] = if audience == Audience::Public {
                &[false]
            } else {
                &[false, true]
            };
            for &remap in remaps {
                let label = label(audience, remap);
                let fresh;
                let exported = if audience == Audience::Restricted && !remap {
                    &full
                } else {
                    fresh = export(planted, spec.name, audience, remap, None)
                        .unwrap_or_else(|refusal| panic!("{label}: {refusal:#}"));
                    &fresh
                };
                exports += 1;
                assert_review_export(
                    planted,
                    &label,
                    audience,
                    exported,
                    truth,
                    &full_standings,
                    &mut problems,
                );
                assert_copies_are_exact(&label, exported, retained);
                // Not vacuous: what the audience may see is there.
                let visible = match audience {
                    Audience::Public => PUBLIC,
                    Audience::Internal => INTERNAL,
                    Audience::Restricted => RESTRICTED,
                };
                assert!(
                    !tokens(exported, visible).is_empty(),
                    "{label}: shows nothing at its own level"
                );
                if audience == Audience::Restricted && !remap {
                    let shown = tokens(exported, RESTRICTED);
                    for channel in contract.shows {
                        assert!(
                            shown.contains(&channel.token(RESTRICTED).to_ascii_lowercase()),
                            "{label}: {channel:?} missing: {shown:?}"
                        );
                    }
                    restricted_tokens.extend(shown);
                }
            }
        }
    }
    (exports, problems, restricted_tokens)
}

#[test]
fn scenario_a_leaks_nothing_through_any_kind_audience_or_identifier_mode() {
    run_matrix("scenario-a");
}

#[test]
fn scenario_b_leaks_nothing_through_any_kind_audience_or_identifier_mode() {
    run_matrix("scenario-b");
}

#[test]
fn scenario_c_leaks_nothing_through_any_kind_audience_or_identifier_mode() {
    run_matrix("scenario-c");
}

#[test]
fn scenario_d_leaks_nothing_through_any_kind_audience_or_identifier_mode() {
    run_matrix("scenario-d");
}

#[test]
fn hostile_text_leaks_nothing_through_any_kind_audience_or_identifier_mode() {
    let exports = run_matrix("hostile");
    // Seven review kinds in five audience modes, and one archive.
    assert_eq!(exports, (KINDS.len() - 1) * 5 + 1);
}

/// The kinds that plan one case, in registry order.
fn case_kinds() -> impl Iterator<Item = &'static str> {
    KINDS
        .iter()
        .filter(|spec| spec.scope() == Scope::Case)
        .map(|spec| spec.name)
}

/// bn-31an, now for every kind: a pinned internal re-export leaked a
/// restricted option's title through the title derived for the public
/// decision that selects it. Each public decision here selects an option
/// that is internal (withheld from `public`) or was raised to restricted
/// after the decision was cleared; the re-export pinned before the raise
/// sees the option as internal in the snapshot's own classification, and
/// only today's policy withholds it.
#[test]
fn a_withheld_option_title_never_reaches_any_kind_through_a_derived_decision_title() {
    let planted = planted("hostile");
    let retained = retained_digests(&planted.workspace);
    let records = truth(&planted, Audience::Restricted, None).records;
    let mut problems = Vec::new();
    let mut shown_neutral = 0;
    for pin in [None, Some(planted.before_raise)] {
        for name in case_kinds() {
            let full = export(&planted, name, Audience::Restricted, false, pin).unwrap();
            let full_standings = standings(&full, &records);
            for audience in [Audience::Internal, Audience::Public] {
                let truth = truth(&planted, audience, pin);
                let label = format!("bn-31an {name} {} pinned={pin:?}", audience.as_str());
                let exported = export(&planted, name, audience, false, pin).unwrap();
                assert_review_export(
                    &planted,
                    &label,
                    audience,
                    &exported,
                    &truth,
                    &full_standings,
                    &mut problems,
                );
                assert_copies_are_exact(&label, &exported, &retained);
                // The decision over the restricted option is still shown to
                // the internal audience, under a title that names nothing.
                let decision: Id = planted.derived[1].parse().unwrap();
                if audience == Audience::Internal && exported.names(decision) {
                    shown_neutral += 1;
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(shown_neutral > 0, "no kind shows the decision at all");
}

/// bn-91gz: case-report highlights, the handoff and the review vault showed a
/// Retrospective's steps and lessons even when they cite a record the
/// audience may not see, though that text may restate it. Here the latest
/// Retrospective, the one a kind showing a single account picks, is public
/// and every step and lesson of it cites the restricted observation. Below
/// `restricted` every kind that shows those entries to `restricted` still
/// shows the account, but none of its entries, and the publisher receipt
/// reports each one left out.
#[test]
fn a_step_or_lesson_citing_a_withheld_record_never_reaches_a_lower_audience_through_any_kind() {
    let planted = planted("hostile");
    let unwrapped = |text: &str| text.to_owned();
    let mut layer = Layer {
        workspace: &planted.workspace,
        inputs: planted.root.join("inputs"),
        below_root: "",
        case: planted.case.clone(),
        wrap: &unwrapped,
        // Past every key the fixture used.
        keys: 1_000_000,
        revisions: std::cell::RefCell::default(),
    };
    let latest = layer.cited_retrospective(&[&planted.withheld_observation]);
    layer.clear(&latest);
    drop(layer);
    let latest: Id = latest.parse().unwrap();
    let retained = retained_digests(&planted.workspace);
    let records = truth(&planted, Audience::Restricted, None).records;
    let entries = Channel::Cited.token(RESTRICTED);
    let mut problems = Vec::new();
    let mut reached = BTreeSet::new();
    for name in case_kinds() {
        let full = export(&planted, name, Audience::Restricted, false, None).unwrap();
        if full.find(&entries).is_empty() {
            continue;
        }
        let full_standings = standings(&full, &records);
        for (audience, remap) in [
            (Audience::Internal, false),
            (Audience::Internal, true),
            (Audience::Public, false),
        ] {
            let truth = truth(&planted, audience, None);
            let label = format!("bn-91gz {name} {} remap={remap}", audience.as_str());
            let exported = export(&planted, name, audience, remap, None).unwrap();
            // No restricted token, the entries' included, at any byte.
            assert_review_export(
                &planted,
                &label,
                audience,
                &exported,
                &truth,
                &full_standings,
                &mut problems,
            );
            assert_copies_are_exact(&label, &exported, &retained);
            if exported.find(CITED_ACCOUNT).is_empty() {
                continue;
            }
            reached.insert(name);
            let omissions = &exported.receipt["publisher_receipt"]["omissions"];
            for (content, part, text) in [
                ("step", "effective_steps[0]", "effective step"),
                ("step", "invalid_or_misleading_steps[0]", "misleading step"),
                ("lesson", "lessons[0]", "guidance"),
            ] {
                // Only an entry this kind shows `restricted` is left out.
                if full.find(&format!("{entries} {text}")).is_empty() {
                    continue;
                }
                let reported = omissions.as_array().into_iter().flatten().any(|omission| {
                    omission["content"] == content
                        && omission["reason"] == "rests_on_withheld"
                        && omission["record"] == latest.to_string()
                        && omission["part"] == part
                });
                if !reported {
                    problems.push(format!("{label}: {part} left out unreported"));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    // Not vacuous: each kind that shows a single account, and each that
    // lists them, shows this one below `restricted`.
    for name in [
        "case-report",
        "handoff",
        "method-retrospective",
        "review-vault",
    ] {
        assert!(
            reached.contains(name),
            "{name} never shows the account: {reached:?}"
        );
    }
}

/// W11.14: a remapped export rewrites every identifier in every file, so it
/// must not copy captured bytes that hold one. The internal note snapshot
/// quoting a record ID is listed by its digest but never copied into a
/// remapped export of any kind, and is copied byte for byte into the
/// canonical one.
#[test]
fn a_remapped_export_never_copies_bytes_the_remap_would_alter() {
    let planted = planted("hostile");
    let retained = retained_digests(&planted.workspace);
    let digest = planted
        .id_note_digest
        .trim_start_matches("sha256:")
        .to_owned();
    let member = format!("human/sha256/{digest}");
    let mut copied_canonically = 0;
    for name in case_kinds() {
        for (audience, remap) in [
            (Audience::Internal, false),
            (Audience::Internal, true),
            (Audience::Restricted, true),
            (Audience::Public, false),
        ] {
            let label = format!("W11.14 {name} {} remap={remap}", audience.as_str());
            let exported = export(&planted, name, audience, remap, None).unwrap();
            assert_copies_are_exact(&label, &exported, &retained);
            let copied = exported
                .files
                .iter()
                .any(|(path, bytes)| copied(path) && hex(bytes) == digest);
            if exported.remapped() {
                assert!(!copied, "{label}: copied bytes the remap rewrites");
                assert!(!exported.files.contains_key(&member), "{label}");
            } else if copied {
                assert_eq!(hex(&exported.files[&member]), digest, "{label}");
                copied_canonically += 1;
            }
        }
    }
    // Not vacuous: a canonical export does copy the note.
    assert!(copied_canonically > 0);
}

/// Normalize an export for comparison across two snapshots of one case: IDs
/// restored to canonical ones, then every digest (head hashes, generations,
/// export and file digests) and every journal position masked. What remains
/// is everything the audience reads, counts included.
fn normalized(exported: &Exported, sequences: &[u64]) -> BTreeMap<String, String> {
    exported
        .restored
        .iter()
        .map(|(path, text)| {
            let mut text = text.clone();
            let mut out = String::with_capacity(text.len());
            let mut run = String::new();
            for character in text.chars().chain(['\n']) {
                if character.is_ascii_hexdigit() {
                    run.push(character);
                    continue;
                }
                if run.len() == 64 {
                    out.push_str("<digest>");
                } else {
                    out.push_str(&run);
                }
                run.clear();
                out.push(character);
            }
            out.pop();
            text = out;
            for sequence in sequences {
                for (from, to) in [
                    (format!("sequence {sequence}"), "sequence <n>".to_owned()),
                    (
                        format!("\"sequence\": {sequence}"),
                        "\"sequence\": <n>".to_owned(),
                    ),
                    (
                        format!("--snapshot {sequence}"),
                        "--snapshot <n>".to_owned(),
                    ),
                    (
                        format!("inv_render_sequence: {sequence}"),
                        "inv_render_sequence: <n>".to_owned(),
                    ),
                ] {
                    text = text.replace(&from, &to);
                }
            }
            (path.clone(), text)
        })
        .collect()
}

/// §20.4 count inference: adding restricted records that bear on nothing
/// shown changes no byte an internal or public audience reads, other than
/// the snapshot the export names; no count, list, completeness or summary
/// moves.
#[test]
fn adding_a_restricted_record_changes_no_count_a_lower_audience_sees() {
    let planted = planted("hostile");
    let audiences = [(Audience::Internal, false), (Audience::Public, false)];
    let mut before = Vec::new();
    let head = |planted: &Planted| -> u64 {
        let mut store = Store::open(Workspace::open(&planted.workspace).unwrap());
        store
            .case(planted.case.parse().unwrap())
            .unwrap()
            .snapshot
            .sequence
    };
    let first = head(&planted);
    for name in case_kinds() {
        for (audience, remap) in audiences {
            before.push(export(&planted, name, audience, remap, None).unwrap());
        }
    }
    // Restricted records of several kinds, none of them bearing on a shown
    // record: an observation, an annotation on the case and a source.
    let wrap = |text: &str| text.to_owned();
    let mut layer = Layer {
        workspace: &planted.workspace,
        inputs: planted.root.join("inputs"),
        below_root: "",
        case: planted.case.clone(),
        wrap: &wrap,
        keys: 100,
        revisions: std::cell::RefCell::default(),
    };
    let observation = layer.observation(format!("{RESTRICTED}statement count probe"));
    layer.raise(&observation);
    let case = layer.current(&planted.case);
    layer.create(
        "annotation.add",
        json!({"subject_refs": [case],
            "content": {"kind": "text", "text": format!("{RESTRICTED}annotation count probe")},
            "classification": {"level": "restricted", "reason": null, "tags": []}}),
    );
    let file = layer.input("count-probe.txt", b"count probe bytes\n");
    layer.ingest(&json!({
        "schema_version": "inv.ingest.v1", "command_id": "leak-count-source", "actor": ACTOR,
        "case_id": planted.case,
        "source": {"title": format!("{RESTRICTED}source count probe"), "format": "text/plain"},
        "paths": [file], "classification": "restricted", "extract_text": false
    }));
    let second = head(&planted);
    assert!(second > first);
    let mut index = 0;
    for name in case_kinds() {
        for (audience, remap) in audiences {
            let after = export(&planted, name, audience, remap, None).unwrap();
            let label = format!("{name} {}", audience.as_str());
            let old = normalized(&before[index], &[first, second]);
            let new = normalized(&after, &[first, second]);
            index += 1;
            assert_eq!(
                old.keys().collect::<Vec<_>>(),
                new.keys().collect::<Vec<_>>(),
                "{label}: the file list changed"
            );
            // Lines are compared as sorted lists: a remapped export orders
            // records by their remapped IDs, which differ per snapshot.
            for (path, text) in &old {
                let sorted = |text: &str| {
                    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
                    lines.sort();
                    lines
                };
                let (was, is) = (sorted(text), sorted(&new[path]));
                let gone: Vec<&String> = was.iter().filter(|line| !is.contains(line)).collect();
                let added: Vec<&String> = is.iter().filter(|line| !was.contains(line)).collect();
                assert!(
                    was == is,
                    "{label}: {path} changed:\n- {gone:#?}\n+ {added:#?}"
                );
            }
        }
    }
}

/// Filtering must not strengthen a recommendation (§16.3), here in the
/// review copy's Overview: the case's leading outcome is computed on the
/// full case. When it is withheld from the audience, the copy says its
/// answer is not stated, rather than promoting the newest outcome it does
/// show, and its counts cover only the records it shows.
#[test]
fn a_review_copy_never_promotes_another_answer_over_a_withheld_leading_outcome() {
    let planted = planted("hostile");
    let overview = |exported: &Exported| -> String {
        let (_, text) = exported
            .restored
            .iter()
            .find(|(path, _)| path.ends_with("/Overview.md"))
            .expect("an Overview");
        text.clone()
    };
    // The leading outcome is the internal conclusion resting on restricted
    // evidence: the internal copy states it, with its caveat.
    let internal = export(&planted, "review-vault", Audience::Internal, false, None).unwrap();
    let text = overview(&internal);
    let answer = text
        .lines()
        .find(|line| line.starts_with("- Answer: "))
        .unwrap();
    assert!(
        answer.contains(&Channel::Statement.token(INTERNAL)),
        "{answer}"
    );
    assert!(text.contains(Caveat::BasisWithheld.text()), "{text}");
    // The public copy does not show it and names no other answer.
    let public = export(&planted, "review-vault", Audience::Public, false, None).unwrap();
    let text = overview(&public);
    assert!(
        text.contains("\n- Answer: not stated in this copy.\n"),
        "{text}"
    );
    assert!(text.contains("Our current answer is not stated in this copy,"));
    assert!(!text.contains("- Answer: [["), "{text}");
}
