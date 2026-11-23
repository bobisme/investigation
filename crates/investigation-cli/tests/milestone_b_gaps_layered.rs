//! Milestone B §20.1 Y15, through the public binary: every supported record
//! type yields stable `title`, `summary` and `full` views; evidence is
//! separately opt-in; `full` does not implicitly load a secret-bearing blob.
//!
//! One workspace holds at least one record of every case-record kind. The
//! kind list comes from `investigation_core::identity::Kind`: an exhaustive
//! match sorts every variant into case records or non-record identifiers, so
//! a new kind fails to compile here until it is classified, and a new
//! record kind fails the coverage assertion until the fixture records one.
//!
//! For every record and every depth (plan §8.3):
//!
//! - two reads give byte-identical output, which satisfies the depth's
//!   contract schema (no `show` schema is published, so the test builds one
//!   from §8.3 over the published `Id`, `Snapshot` and `Digest` definitions);
//! - a write to another case leaves the output byte-identical, and an
//!   unrelated write to the same case changes nothing but `snapshot` (and
//!   the Case's own record-count overview);
//! - the depths nest: `title` ⊂ `summary` ⊂ `full`, field by field;
//! - no depth returns retained bytes: a line of the attached file that no
//!   record quotes appears only through `evidence read`.
//!
//! A second case holds an Artifact raised to `restricted` through
//! `classification.raise`. Every depth shows it, and its text derivative, as
//! a restricted stub with no record, digest or bytes, and `evidence read`
//! refuses it on classification. The probe: every retained blob in the
//! workspace is then overwritten. No view at any depth shows the new bytes
//! and a view changes only in its integrity verdict, while `evidence read`
//! and `check` report the corruption.
//!
//! The strict property, that no depth reads a restricted blob, has its own
//! test: a same-size overwrite of the restricted bytes, which only a read
//! could notice, leaves every view byte-identical. `check` still hashes
//! them, and once it has found the forgery the views report it. A non-
//! restricted blob overwritten in place is caught by the next `show`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use investigation_core::identity::Kind;
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const ACTOR: &str = "agent:layered-test";
const DEPTHS: [&str; 3] = ["title", "summary", "full"];
/// A line of the attached file that no record quotes or restates.
const RETAINED_ONLY: &str = "retained-only marker 7c1e9a: never quoted, never typed";
/// The only line of the restricted attachment.
const SECRET: &str = "secret-bearing marker 4b2d6f: restricted bytes";

/// Every identifier kind, sorted once. A new `Kind` variant fails to compile
/// here until it is classified; a case record kind must then be recorded by
/// [`build`] or the coverage assertion fails.
fn is_case_record(kind: Kind) -> bool {
    match kind {
        Kind::Case
        | Kind::Question
        | Kind::Source
        | Kind::Artifact
        | Kind::Observation
        | Kind::Hypothesis
        | Kind::Assumption
        | Kind::Option
        | Kind::Criterion
        | Kind::Comparison
        | Kind::Assessment
        | Kind::Conclusion
        | Kind::Decision
        | Kind::Review
        | Kind::Objection
        | Kind::Annotation
        | Kind::Retrospective
        | Kind::Inquiry
        | Kind::Run => true,
        // Journal transactions, capture handles, checkpoints, imports and
        // the workspace are identifiers, not records `show` reads.
        Kind::Transaction | Kind::Handle | Kind::Checkpoint | Kind::Import | Kind::Workspace => {
            false
        }
    }
}

fn record_kinds() -> BTreeSet<Kind> {
    Kind::ALL
        .iter()
        .copied()
        .filter(|kind| is_case_record(*kind))
        .collect()
}

fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
}

/// Run the binary; return the parsed JSON and the exact stdout.
fn call(cwd: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> (Value, String) {
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
    // A negative `exit` accepts any status; the caller inspects `ok`.
    if exit >= 0 {
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{args:?}\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    (serde_json::from_str(&stdout).expect(&stdout), stdout)
}

fn run(cwd: &Path, args: &[&str], exit: i32) -> Value {
    call(cwd, args, None, exit).0
}

fn exact(id: &str, revision: u64) -> Value {
    json!({"id":id,"revision":revision})
}

fn example(root: &Path, operation: &str) -> Value {
    run(root, &["schema", operation, "--json"], 0)["transaction_example"]["input"]["operations"][0]
        ["payload"]
        .clone()
}

fn created(receipt: &Value) -> Vec<String> {
    receipt["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["id"].as_str().unwrap().to_owned())
        .collect()
}

/// The workspace: a case with a record of every kind, and a second case
/// whose only attachment is restricted.
struct Fixture {
    dir: tempfile::TempDir,
    case: String,
    text: String,
    digest: Value,
    secret_case: String,
    secret_raw: String,
    secret_text: String,
    secret_digest: Value,
    transaction: String,
    step: usize,
}

impl Fixture {
    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn record(&mut self, case: &str, operations: Vec<(&str, Value)>) -> Value {
        self.step += 1;
        let operations: Vec<Value> = operations
            .into_iter()
            .map(|(op, payload)| json!({"op":op,"payload":payload}))
            .collect();
        run_record(
            self.root(),
            &json!({
                "schema_version":"inv.command.v1","command_id":format!("layered-{}", self.step),
                "case_id":case,"actor":ACTOR,"preconditions":[],"operations":operations
            }),
        )
    }

    fn one(&mut self, op: &str, payload: Value) -> String {
        let case = self.case.clone();
        created(&self.record(&case, vec![(op, payload)]))[0].clone()
    }

    fn show(&self, case: &str, id: &str, depth: &str) -> (Value, String) {
        call(
            self.root(),
            &["--case", case, "show", id, "--depth", depth, "--json"],
            None,
            0,
        )
    }

    fn evidence_read(&self, case: &str, artifact: &str, digest: &Value, line: u64) -> Value {
        let selector = self.root().join(format!("selector-{artifact}.json"));
        fs::write(
            &selector,
            serde_json::to_vec(&json!({
                "schema_version":"inv.selector.v1","artifact_digest":digest,
                "selector":{"type":"text_lines","start_line":line,"end_line":line,
                    "encoding":"utf-8","parser_version":"inv.text.utf8.v1"}
            }))
            .unwrap(),
        )
        .unwrap();
        let (value, _) = call(
            self.root(),
            &[
                "--case",
                case,
                "evidence",
                "read",
                artifact,
                "--selector-file",
                selector.to_str().unwrap(),
                "--json",
            ],
            None,
            // Exit codes differ by failure; callers inspect `ok`.
            -1,
        );
        value
    }
}

fn run_record(root: &Path, command: &Value) -> Value {
    call(
        root,
        &["record", "--input", "-", "--json"],
        Some(command),
        0,
    )
    .0
}

fn start(root: &Path, question: &str, file: &str, key: &str) -> Value {
    run(
        root,
        &[
            "start",
            question,
            "--attach",
            file,
            "--command-id",
            key,
            "--actor",
            ACTOR,
            "--json",
        ],
        0,
    )
}

fn build() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    run(root, &["init", ".", "--no-vault", "--json"], 0);
    let config = root.join("investigation.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    fs::write(
        root.join("bench.txt"),
        format!("p99 18.7 ms\np99 31.2 ms\n{RETAINED_ONLY}\n"),
    )
    .unwrap();
    fs::write(root.join("secret.txt"), format!("{SECRET}\n")).unwrap();
    let started = start(
        root,
        "Does the queue meet the latency limit?",
        "bench.txt",
        "a",
    );
    let secret = start(
        root,
        "What does the restricted note say?",
        "secret.txt",
        "b",
    );
    let mut fixture = Fixture {
        case: started["created"][0]["id"].as_str().unwrap().to_owned(),
        text: started["items"][0]["derivative_id"]
            .as_str()
            .unwrap()
            .to_owned(),
        digest: started["items"][0]["digest"].clone(),
        secret_case: secret["created"][0]["id"].as_str().unwrap().to_owned(),
        secret_raw: secret["items"][0]["artifact_id"]
            .as_str()
            .unwrap()
            .to_owned(),
        secret_text: secret["items"][0]["derivative_id"]
            .as_str()
            .unwrap()
            .to_owned(),
        secret_digest: secret["items"][0]["digest"].clone(),
        transaction: started["transaction_id"].as_str().unwrap().to_owned(),
        dir,
        step: 0,
    };
    let root = fixture.root().to_owned();
    let case = fixture.case.clone();

    // Explanatory records.
    let question = fixture.one("question.create", example(&root, "question.create"));
    let mut hypothesis = example(&root, "hypothesis.create");
    hypothesis["question_ref"] = exact(&question, 1);
    let hypothesis = fixture.one("hypothesis.create", hypothesis);
    let observation = fixture.one(
        "observation.add",
        json!({
            "value":{"kind":"text","statement":"The benchmark reports p99 18.7 ms."},
            "basis":{"kind":"source","artifact_ref":exact(&fixture.text, 1),
                "selector":{"schema_version":"inv.selector.v1","artifact_digest":fixture.digest,
                    "selector":{"type":"text_lines","start_line":1,"end_line":1,
                        "encoding":"utf-8","parser_version":"inv.text.utf8.v1"}},
                "text_quote":{"exact":"p99 18.7 ms"}},
            "validity":"unknown","quality":[],"limitations":["One run."],
            "evidence_kind":"observational","variables_changed":[]
        }),
    );
    fixture.record(
        &case,
        vec![(
            "measurement.assess",
            json!({"observation_ref":exact(&observation, 1),"channel":null,"validity":"valid",
                "verification":[{"kind":"equivalent",
                    "justification":"The setup reproduced a known reference value."}],
                "rationale":"Judged from the recorded setup.","limits":[]}),
        )],
    );
    let mut assessment = example(&root, "assessment.add");
    assessment["subject_ref"] = exact(&hypothesis, 1);
    assessment["evidence_refs"] = json!([exact(&observation, 2)]);
    assessment["relation"] = json!("supports");
    assessment["rationale"] = json!("The run stays under the limit.");
    let assessment = fixture.one("assessment.add", assessment);
    let mut assumption = example(&root, "assumption.create");
    assumption["affected_refs"] = json!([exact(&hypothesis, 1)]);
    fixture.one("assumption.create", assumption);
    let conclusion = fixture.one(
        "conclusion.create",
        json!({
            "statement":"The queue met the latency limit in the recorded run.",
            "scope":"The recorded benchmark.","significance":"Informs the queue choice.",
            "support_refs":[exact(&observation, 2), exact(&assessment, 1)],
            "contradiction_refs":[],"caveats":["One synthetic workload."]
        }),
    );
    let mut objection = example(&root, "objection.raise");
    objection["subject_ref"] = exact(&conclusion, 1);
    objection["severity"] = json!("material");
    fixture.one("objection.raise", objection);
    fixture.one(
        "annotation.add",
        json!({"subject_refs":[exact(&conclusion, 1)],"role":"synopsis",
            "content":{"kind":"text","text":"One run met the limit."}}),
    );
    let (full, _) = fixture.show(&case, &conclusion, "full");
    let basis = full["review_basis"].clone();
    fixture.one(
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
    );
    fixture.one("inquiry.create", example(&root, "inquiry.create"));
    fixture.one("run.start", example(&root, "run.start"));

    // Evaluative records.
    let mut option = example(&root, "option.create");
    option["question_ref"] = exact(&question, 1);
    let option = fixture.one("option.create", option);
    let mut criterion = example(&root, "criterion.create");
    criterion["scope"]["question_ref"] = exact(&question, 1);
    let criterion = fixture.one("criterion.create", criterion);
    let mut comparison = example(&root, "comparison.add");
    comparison["option_ref"] = exact(&option, 1);
    comparison["criterion_ref"] = exact(&criterion, 1);
    comparison["evidence_refs"] = json!([observation]);
    fixture.one("comparison.add", comparison);
    let mut decision = example(&root, "decision.propose");
    decision["question_ref"] = exact(&question, 1);
    let receipt = fixture.record(&case, vec![("decision.propose", decision)]);

    let mut retrospective = example(&root, "retrospective.record");
    retrospective["case_ref"] = exact(&case, 1);
    retrospective["snapshot"] = receipt["snapshot"].clone();
    fixture.one("retrospective.record", retrospective);

    // The restricted Artifact; its text derivative inherits the level.
    let raise = json!({"subject_ref":exact(&fixture.secret_raw, 1),"level":"restricted",
        "reason":"Holds unreleased figures.","tags":[]});
    let secret_case = fixture.secret_case.clone();
    fixture.record(&secret_case, vec![("classification.raise", raise)]);
    fixture
}

/// Every current record of the workspace, by case.
fn records(root: &Path) -> Vec<(String, String, Kind)> {
    let mut store = Store::open(Workspace::open(root).unwrap());
    let mut found = Vec::new();
    for head in store.list_cases().unwrap() {
        let replay = store.case(head.case_id).unwrap();
        for record in replay.state.records() {
            found.push((
                head.case_id.to_string(),
                record.header.id.to_string(),
                record.header.id.kind(),
            ));
        }
    }
    found.sort();
    found
}

/// The output of `show` for every record at every depth, by (case, id, depth).
type Views = BTreeMap<(String, String, &'static str), (Value, String)>;

fn views(fixture: &Fixture) -> Views {
    let mut out = Views::new();
    for (case, id, _) in records(fixture.root()) {
        for depth in DEPTHS {
            out.insert(
                (case.clone(), id.clone(), depth),
                fixture.show(&case, &id, depth),
            );
        }
    }
    out
}

/// The contract schema of one depth (plan §8.3), over the published `Id`,
/// `Snapshot` and `Digest` definitions.
fn contract(depth: &str) -> Value {
    let published: Value = serde_json::from_slice(
        &fs::read(repository().join("schemas/v1/receipt.schema.json")).unwrap(),
    )
    .unwrap();
    let defs = &published["$defs"];
    let mut required = vec![
        "schema_version",
        "ok",
        "result",
        "id",
        "type",
        "title",
        "snapshot",
        "basis_revision",
        "freshness",
        "depth",
        "effective_status",
        "summary_available",
        "historical",
    ];
    let mut properties = json!({
        "schema_version":{"const":"inv.response.v1"},
        "ok":{"const":true},
        "result":{"const":"record_shown"},
        "id":{"$ref":"#/$defs/Id"},
        "type":{"type":"string"},
        "title":{"type":"string","minLength":1},
        "snapshot":{"$ref":"#/$defs/Snapshot"},
        "basis_revision":{"type":"integer","minimum":1},
        "freshness":{"type":"object","required":["state"]},
        "depth":{"const":depth},
        "effective_status":{"type":"object","required":["state"]},
        "summary_available":{"type":"boolean"},
        "historical":{"const":false},
        "evidence_reference_ids":{"type":"array","items":{"$ref":"#/$defs/Id"}},
        "record":false
    });
    // A restricted stub carries only the header fields.
    let restricted = json!({"properties":{"content_state":{"const":"restricted"}},
        "required":["content_state"]});
    let mut open = vec![
        "material_caveat",
        "contradiction",
        "gaps",
        "recorded_caveats",
        "evidence_reference_ids",
    ];
    if depth != "title" {
        open.extend(["summary", "summary_basis"]);
        properties["summary"] = json!({"type":"string"});
        properties["summary_basis"] = json!({"type":"object","required":["revision","kind"],"properties":{"revision":{"type":"integer"}}});
    }
    if depth == "full" {
        open.push("record");
        properties["record"] = json!({"type":"object","required":["header","body"],
            "properties":{"header":{"type":"object","required":["id","revision","case_id"],
                "properties":{"id":{"$ref":"#/$defs/Id"},"revision":{"type":"integer","minimum":1},
                    "case_id":{"$ref":"#/$defs/Id"}}},
                "body":{"type":"object"}}});
    }
    required.sort_unstable();
    json!({
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "$defs":{"Id":defs["Id"],"Snapshot":defs["Snapshot"],"Digest":defs["Digest"]},
        "type":"object",
        "required":required,
        "properties":properties,
        "if":restricted,
        "then":{},
        "else":{"required":open},
    })
}

#[test]
fn every_record_type_has_stable_layered_views_and_evidence_stays_opt_in() {
    let mut fixture = build();
    let root = fixture.root().to_owned();
    let all = records(&root);

    // Coverage: one record of every case-record kind, none of any other.
    let kinds: BTreeSet<Kind> = all.iter().map(|(_, _, kind)| *kind).collect();
    assert_eq!(kinds, record_kinds(), "kinds recorded vs case-record kinds");
    // A non-record identifier is not something `show` reads.
    let refused = run(
        &root,
        &[
            "--case",
            &fixture.case,
            "show",
            &fixture.transaction,
            "--json",
        ],
        3,
    );
    assert_eq!(refused["error"]["code"], "UNKNOWN_REFERENCE", "{refused}");

    // Determinism and the contract schema.
    let first = views(&fixture);
    let second = views(&fixture);
    assert_eq!(first.len(), all.len() * DEPTHS.len());
    let validators: BTreeMap<&str, _> = DEPTHS
        .iter()
        .map(|depth| (*depth, jsonschema::validator_for(&contract(depth)).unwrap()))
        .collect();
    for (key, (value, stdout)) in &first {
        assert_eq!(stdout, &second[key].1, "{key:?} differs between two reads");
        let errors: Vec<String> = validators[key.2]
            .iter_errors(value)
            .map(|error| format!("{} at {}", error, error.instance_path))
            .collect();
        assert!(errors.is_empty(), "{key:?}: {errors:#?}\n{value:#}");
        assert_eq!(value["id"], key.1.as_str());
        // §8.3: a summary is at most ~700 UTF-8 bytes.
        let summary = value["summary"].as_str().unwrap_or_default();
        assert!(summary.len() <= 700, "{key:?}: {} bytes", summary.len());
    }

    // Depths nest: title ⊂ summary ⊂ full.
    let mut violations = Vec::new();
    for (case, id, kind) in &all {
        let at = |depth: &'static str| &first[&(case.clone(), id.clone(), depth)].0;
        for (low, high) in [("title", "summary"), ("summary", "full")] {
            let mut found = Vec::new();
            nesting_violations("", at(low), at(high), &mut found);
            violations.extend(
                found
                    .into_iter()
                    .map(|v| format!("{kind} {id} {low}->{high} {v}")),
            );
        }
    }
    assert!(violations.is_empty(), "{violations:#?}");

    // Retained bytes: never in a view; only `evidence read` returns them.
    for (key, (_, stdout)) in &first {
        assert!(
            !stdout.contains(RETAINED_ONLY),
            "{key:?} returned retained bytes"
        );
        assert!(
            !stdout.contains(SECRET),
            "{key:?} returned restricted bytes"
        );
    }
    let read = fixture.evidence_read(&fixture.case, &fixture.text, &fixture.digest, 3);
    assert_eq!(read["content_trust"], "untrusted_evidence", "{read}");
    assert_eq!(
        read["excerpt"]["text"],
        format!("{RETAINED_ONLY}\n"),
        "{read}"
    );

    // The restricted Artifact and its derivative are stubs at every depth,
    // and `evidence read` refuses them on classification.
    for artifact in [&fixture.secret_raw, &fixture.secret_text] {
        for depth in DEPTHS {
            let (view, stdout) = &first[&(fixture.secret_case.clone(), artifact.clone(), depth)];
            assert_eq!(
                view["content_state"], "restricted",
                "{artifact} {depth}: {view}"
            );
            assert!(view.get("record").is_none());
            let digest = fixture.secret_digest.as_str().unwrap();
            assert!(!stdout.contains(digest.trim_start_matches("sha256:")));
        }
        let refused =
            fixture.evidence_read(&fixture.secret_case, artifact, &fixture.secret_digest, 1);
        assert_eq!(refused["ok"], false, "{refused}");
        assert_eq!(refused["error"]["code"], "GATE_FAILED", "{refused}");
        assert!(!refused.to_string().contains(SECRET));
    }

    // A write to another case changes no byte of this case's views; an
    // unrelated write to the same case changes only `snapshot`.
    let secret_case = fixture.secret_case.clone();
    fixture.record(
        &secret_case,
        vec![(
            "question.create",
            json!({"importance":"normal","wording":"Is the note current?"}),
        )],
    );
    let other_case = views(&fixture);
    for (key, (_, stdout)) in first.iter().filter(|(key, _)| key.0 == fixture.case) {
        assert_eq!(
            stdout, &other_case[key].1,
            "{key:?} changed after another case's write"
        );
    }
    let case = fixture.case.clone();
    let mut unrelated = example(&root, "question.create");
    unrelated["wording"] = json!("Does the release notes page load?");
    fixture.record(&case, vec![("question.create", unrelated)]);
    let same_case = views(&fixture);
    for (key, (value, _)) in first.iter().filter(|(key, _)| key.0 == fixture.case) {
        let mut before = value.clone();
        let mut after = same_case[key].0.clone();
        assert_ne!(before["snapshot"], after["snapshot"]);
        before["snapshot"] = Value::Null;
        after["snapshot"] = Value::Null;
        // The Case's own freshness overview counts the case's records, so
        // a new record in the case is not unrelated to it.
        if key.1 == fixture.case {
            assert_ne!(
                before["freshness"]["overview"],
                after["freshness"]["overview"]
            );
            before["freshness"]["overview"] = Value::Null;
            after["freshness"]["overview"] = Value::Null;
        }
        assert_eq!(before, after, "{key:?} changed after an unrelated write");
    }

    // The probe: overwrite every retained blob. No view at any depth shows
    // the new bytes; a view changes only in its integrity verdict (the
    // effective state and the caveat naming the unavailable Artifacts).
    // `evidence read` and `check` notice; a restricted Artifact is still
    // refused on classification, before any byte is read.
    let probed = probe(&fixture, |_| true);
    let mut changed = Vec::new();
    for (key, (value, _)) in &same_case {
        let (after, probed_stdout) = &probed[key];
        assert!(
            !probed_stdout.contains(PROBE),
            "{key:?} returned blob bytes"
        );
        let mut paths = Vec::new();
        differences("", value, after, &mut paths);
        paths.retain(|path| {
            !INTEGRITY_VERDICT
                .iter()
                .any(|field| path.starts_with(field))
        });
        if !paths.is_empty() {
            changed.push(format!("{key:?} {paths:?}"));
        }
    }
    assert!(changed.is_empty(), "{changed:#?}");
    for artifact in [&fixture.secret_raw, &fixture.secret_text] {
        for depth in DEPTHS {
            let view = &probed[&(fixture.secret_case.clone(), artifact.clone(), depth)].0;
            assert_eq!(
                view["content_state"], "restricted",
                "{artifact} {depth}: {view}"
            );
        }
    }
    let corrupt = fixture.evidence_read(&fixture.case, &fixture.text, &fixture.digest, 3);
    assert_eq!(corrupt["ok"], false, "{corrupt}");
    assert!(!corrupt.to_string().contains(PROBE), "{corrupt}");
    let checked = run(&root, &["check", "--json"], -1);
    assert_eq!(checked["ok"], false, "{checked}");
    let refused = fixture.evidence_read(
        &fixture.secret_case,
        &fixture.secret_raw,
        &fixture.secret_digest,
        1,
    );
    assert_eq!(refused["error"]["code"], "GATE_FAILED", "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Restricted"),
        "{refused}"
    );
}

/// The strict reading of Y15: no depth reads a restricted blob at all. The
/// probe overwrites only the restricted bytes and keeps their size, which a
/// stat cannot tell from the original: a view that never opened them is
/// byte-identical afterwards. `check` reads every blob and finds the
/// forgery; from then on the views report it, still without reading.
#[test]
fn no_depth_reads_a_restricted_blob() {
    let fixture = build();
    let secret = fixture
        .secret_digest
        .as_str()
        .unwrap()
        .trim_start_matches("sha256:")
        .to_owned();
    let before = views(&fixture);
    let after = probe_keeping_size(&fixture, |blob| blob.ends_with(&secret));
    let changed: Vec<_> = before
        .iter()
        .filter(|(key, (_, stdout))| stdout != &after[*key].1)
        .map(|(key, (value, _))| {
            let mut paths = Vec::new();
            differences("", value, &after[key].0, &mut paths);
            format!("{key:?} {paths:?}")
        })
        .collect();
    assert!(changed.is_empty(), "{changed:#?}");

    let checked = run(fixture.root(), &["check", "--json"], -1);
    assert_eq!(checked["ok"], false, "{checked}");
    let checked_views = views(&fixture);
    let mut reported = BTreeSet::new();
    for (key, (value, stdout)) in &after {
        let (later, later_stdout) = &checked_views[key];
        assert!(!later_stdout.contains(PROBE), "{key:?} returned blob bytes");
        if stdout == later_stdout {
            continue;
        }
        assert_eq!(key.0, fixture.secret_case, "{key:?} changed");
        let mut paths = Vec::new();
        differences("", value, later, &mut paths);
        assert!(
            paths.iter().all(|path| INTEGRITY_VERDICT
                .iter()
                .any(|field| path.starts_with(field))),
            "{key:?} {paths:?}"
        );
        assert_eq!(later["content_state"], "restricted", "{key:?}");
        reported.insert(key.1.clone());
    }
    assert!(
        reported.contains(&fixture.secret_raw) && reported.contains(&fixture.secret_text),
        "{reported:?}"
    );
}

/// A non-restricted blob overwritten in place, with its size and
/// modification time kept, is caught by the next `show`: the read that
/// follows a change of the file hashes it again.
#[test]
fn an_overwrite_in_place_is_caught_by_the_next_show() {
    let fixture = build();
    let hex = fixture
        .digest
        .as_str()
        .unwrap()
        .trim_start_matches("sha256:")
        .to_owned();
    let (first, _) = fixture.show(&fixture.case, &fixture.text, "title");
    assert_eq!(first["freshness"]["state"], "current", "{first}");
    let (second, _) = fixture.show(&fixture.case, &fixture.text, "title");
    assert_eq!(first, second);
    let blob = blob_files(&fixture.root().join("state/blobs/sha256"))
        .into_iter()
        .find(|blob| blob.ends_with(&hex))
        .unwrap();
    overwrite_in_place(&blob);
    let (third, stdout) = fixture.show(&fixture.case, &fixture.text, "title");
    assert!(!stdout.contains(PROBE));
    assert_eq!(
        third["freshness"]["state"], "evidence_unavailable",
        "{third}"
    );
}

/// View fields that report whether retained bytes verify.
const INTEGRITY_VERDICT: [&str; 4] = [
    "/effective_status/state",
    "/freshness/state",
    "/material_caveat/",
    "/lineage/inspectability/",
];
/// What the probe writes over a blob.
const PROBE: &str = "overwritten by the probe";

/// Overwrite the retained blobs `select` picks with [`PROBE`], then read
/// every view again.
fn probe(fixture: &Fixture, select: impl Fn(&str) -> bool) -> Views {
    let blobs: Vec<PathBuf> = blob_files(&fixture.root().join("state/blobs/sha256"))
        .into_iter()
        .filter(|blob| select(blob.to_str().unwrap()))
        .collect();
    assert!(!blobs.is_empty());
    for blob in &blobs {
        // Blobs are read-only: replace each file instead.
        fs::remove_file(blob).unwrap();
        fs::write(blob, format!("{PROBE}\n")).unwrap();
    }
    views(fixture)
}

/// Overwrite the retained blobs `select` picks with bytes of the same
/// length, then read every view again.
fn probe_keeping_size(fixture: &Fixture, select: impl Fn(&str) -> bool) -> Views {
    let blobs: Vec<PathBuf> = blob_files(&fixture.root().join("state/blobs/sha256"))
        .into_iter()
        .filter(|blob| select(blob.to_str().unwrap()))
        .collect();
    assert!(!blobs.is_empty());
    for blob in &blobs {
        let length = fs::metadata(blob).unwrap().len() as usize;
        let forged: Vec<u8> = PROBE.bytes().cycle().take(length).collect();
        fs::remove_file(blob).unwrap();
        fs::write(blob, forged).unwrap();
    }
    views(fixture)
}

/// Change the first bytes of a blob in place, keeping its inode, size and
/// modification time.
fn overwrite_in_place(blob: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(blob).unwrap();
    let modified = metadata.modified().unwrap();
    let mut permissions = metadata.permissions();
    let mode = permissions.mode();
    permissions.set_mode(0o600);
    fs::set_permissions(blob, permissions.clone()).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(blob).unwrap();
    file.write_all(&PROBE.as_bytes()[..4]).unwrap();
    file.set_modified(modified).unwrap();
    drop(file);
    permissions.set_mode(mode);
    fs::set_permissions(blob, permissions).unwrap();
}

/// Where a shallower view is not contained in a deeper one: every field of
/// the shallower view is in the deeper one with the same value, an object
/// recursively and an array as a prefix. Only `depth` differs, and a
/// `*_omitted` count may stand for entries the deeper view lists.
fn nesting_violations(path: &str, low: &Value, high: &Value, found: &mut Vec<String>) {
    match (low, high) {
        (Value::Object(low), Value::Object(high)) => {
            for (key, value) in low {
                let at = format!("{path}/{key}");
                match high.get(key) {
                    _ if at == "/depth" || key.ends_with("_omitted") => {}
                    Some(deeper) => nesting_violations(&at, value, deeper, found),
                    None => found.push(format!("{at} missing deeper")),
                }
            }
        }
        (Value::Array(low), Value::Array(high)) if low.len() <= high.len() => {
            for (index, (value, deeper)) in low.iter().zip(high).enumerate() {
                nesting_violations(&format!("{path}/{index}"), value, deeper, found);
            }
        }
        _ if low == high => {}
        _ => found.push(format!("{path}: {low} vs {high}")),
    }
}

fn differences(path: &str, before: &Value, after: &Value, found: &mut Vec<String>) {
    match (before, after) {
        (Value::Object(before), Value::Object(after)) => {
            let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
            for key in keys {
                differences(
                    &format!("{path}/{key}"),
                    before.get(key).unwrap_or(&Value::Null),
                    after.get(key).unwrap_or(&Value::Null),
                    found,
                );
            }
        }
        _ if before == after => {}
        _ => found.push(path.to_owned()),
    }
}

fn blob_files(directory: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(blob_files(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}
