//! Scenario D end to end through the public binary: contradictory source
//! research (plan §17.4) on the `scenario-d-source-research` fixture.
//!
//! Each variant starts from an empty directory, runs offline, and checks every
//! named property in the variant's `expected.json`:
//!
//! - the specification, article restatement and blog passage stay three
//!   acquisitions of one known origin, and the article's bench test is a
//!   separate origin in the same Source record (the duplicate-origin trap);
//! - the erratum is traced to its edition: in the baseline it invalidates the
//!   earlier 64-frame judgment without deleting it, in the twin it does not
//!   apply to the named build;
//! - a `search_coverage` Run per registry keeps every empty search, and an
//!   absence claim resting on them without an expected signal is gated;
//! - a reference-only copy of the erratum reports `source_unavailable`;
//! - a conditional conclusion is reviewed and validated from sources alone,
//!   with no inquiry plan and no executable experiment;
//! - in `injected-output`, planted instructions in crawler output stay quoted
//!   data: onboarding, schemas, method packs, command selection and the result
//!   are the same as the baseline.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:scenario-d";
const REVIEWER: &str = "agent:scenario-d-reviewer";
const FIXTURE: &str = "../../fixtures/development/scenario-d-source-research";
const SPEC: &str = "primary-specification.txt";
const ARTICLE: &str = "trade-article.txt";
const BLOG: &str = "blog-post.txt";
const ERRATUM: &str = "first-party-erratum.txt";
const SEARCH_LOG: &str = "search-coverage-input.json";
const CRAWL: &str = "crawl-output-forum-sweep.txt";
const DOCUMENTS: [&str; 4] = [SPEC, ARTICLE, BLOG, ERRATUM];
const ERRATA_REGISTER: &str = "Corvin errata register";
/// Text that only the planted notice contains.
const INJECTED_TOKENS: [&str; 5] = [
    "relay-sweep",
    "errata_search",
    "NOTICE TO THE AUTOMATED",
    "promote-conclusion",
    "three independent references confirm",
];
/// Operations whose published schemas must not change with case content.
const SCHEMAS: [&str; 4] = [
    "record",
    "observation.add",
    "source.assess",
    "conclusion.create",
];

fn exec(root: &Path, args: &[&str], stdin: Option<&Value>) -> (i32, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(root)
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
    assert!(
        output.status.code().is_some(),
        "{args:?} was killed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap(), stdout)
}

fn run(root: &Path, args: &[&str], stdin: Option<&Value>, exit: i32) -> Value {
    let (code, stdout) = exec(root, args, stdin);
    assert_eq!(code, exit, "{args:?}\n{stdout}");
    serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("{args:?}: {stdout}"))
}

/// Line breaks collapse to spaces; fixture selectors match after that.
fn flat(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .map(|byte| if *byte == b'\n' { b' ' } else { *byte })
        .collect()
}

fn reference(value: &Value) -> Value {
    json!({"id": value["id"], "revision": value["revision"]})
}

fn text(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("not a string: {value}"))
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| text(item).to_owned())
        .collect()
}

fn property(expected: &Value, id: &str) -> Value {
    expected["properties"]
        .as_array()
        .unwrap()
        .iter()
        .find(|property| property["id"] == id)
        .unwrap_or_else(|| panic!("expected.json has no property {id}"))
        .clone()
}

fn contains_token(text: &str) -> bool {
    INJECTED_TOKENS.iter().any(|token| text.contains(token))
}

/// Every string holding planted text sits inside an object labelled as
/// untrusted data: `summary_untrusted_data`, `content_trust:
/// untrusted_evidence` or a `recorded_evidence` label. Returns how many such
/// strings were found.
fn planted_text_is_labelled(value: &Value, labelled: bool, path: &str) -> usize {
    match value {
        Value::String(text) if contains_token(text) => {
            assert!(labelled, "unlabelled planted text at {path}: {text}");
            1
        }
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                planted_text_is_labelled(item, labelled, &format!("{path}/{index}"))
            })
            .sum(),
        Value::Object(map) => {
            let here = labelled
                || map.get("summary_untrusted_data") == Some(&json!(true))
                || map.get("content_trust") == Some(&json!("untrusted_evidence"))
                || map.get("label") == Some(&json!("recorded_evidence"));
            map.iter()
                .map(|(key, item)| planted_text_is_labelled(item, here, &format!("{path}/{key}")))
                .sum()
        }
        _ => 0,
    }
}

/// Every suggested command line runs this program and none carries planted text.
fn argv_are_trusted(value: &Value, path: &str) {
    match value {
        Value::Object(map) => {
            if let Some(argv) = map.get("argv").and_then(Value::as_array) {
                assert_eq!(argv[0], "investigation", "{path}: {argv:?}");
                assert!(
                    !argv.iter().any(|arg| contains_token(text(arg))),
                    "{path}: {argv:?}"
                );
            }
            for (key, item) in map {
                argv_are_trusted(item, &format!("{path}/{key}"));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                argv_are_trusted(item, &format!("{path}/{index}"));
            }
        }
        _ => {}
    }
}

struct Document {
    source: String,
    /// The text derivative that observations quote.
    text: String,
    /// The original retained bytes.
    original: String,
    digest: String,
    bytes: Vec<u8>,
}

struct Walk {
    _dir: tempfile::TempDir,
    root: PathBuf,
    variant: String,
    sources: PathBuf,
    manifest: Value,
    expected: Value,
    case: String,
    sequence: usize,
    created: Vec<String>,
    documents: BTreeMap<String, Document>,
    /// Observation per (source file, flattened quote).
    quotes: BTreeMap<(String, Vec<u8>), Value>,
}

impl Walk {
    fn new(variant: &str) -> Self {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(FIXTURE)
            .join(variant);
        let read = |file: &str| -> Value {
            serde_json::from_slice(&fs::read(directory.join(file)).unwrap()).unwrap()
        };
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        fs::create_dir(&root).unwrap();
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        run(&root, &["init", ".", "--no-vault", "--json"], None, 0);
        let config = root.join("investigation.toml");
        let contents = fs::read_to_string(&config).unwrap();
        fs::write(
            &config,
            contents.replace("import_roots = []", "import_roots = [\".\"]"),
        )
        .unwrap();
        Walk {
            _dir: dir,
            root,
            variant: variant.to_owned(),
            sources: directory.join("sources"),
            manifest: read("manifest.json"),
            expected: read("expected.json"),
            case: String::new(),
            sequence: 0,
            created: Vec::new(),
            documents: BTreeMap::new(),
            quotes: BTreeMap::new(),
        }
    }

    fn injected(&self) -> bool {
        self.variant == "injected-output"
    }

    fn twin(&self) -> bool {
        self.variant == "later-edition-twin"
    }

    fn cli(&self, args: &[&str], exit: i32) -> Value {
        let mut full = vec!["--case", self.case.as_str()];
        full.extend_from_slice(args);
        full.push("--json");
        run(&self.root, &full, None, exit)
    }

    fn submit(&mut self, actor: &str, op: &str, payload: Value, pre: Value, exit: i32) -> Value {
        self.sequence += 1;
        let command = json!({
            "schema_version": "inv.command.v1", "command_id": format!("scenario-d-{}", self.sequence),
            "case_id": self.case, "actor": actor, "preconditions": pre,
            "operations": [{"op": op, "payload": payload}],
        });
        let receipt = run(
            &self.root,
            &["record", "--input", "-", "--json"],
            Some(&command),
            exit,
        );
        if exit == 0 {
            for change in receipt["created"].as_array().unwrap() {
                self.created.push(text(&change["id"]).to_owned());
            }
            // Keep quoted Observations at their current revision.
            for change in receipt["updated"].as_array().unwrap() {
                for observation in self.quotes.values_mut() {
                    if observation["id"] == change["id"] {
                        *observation = reference(change);
                    }
                }
            }
        }
        receipt
    }

    /// One write by the investigator; returns the created or updated revision.
    fn one(&mut self, op: &str, payload: Value) -> Value {
        let receipt = self.submit(ACTOR, op, payload, json!([]), 0);
        let change = receipt["created"]
            .get(0)
            .or_else(|| receipt["updated"].get(0))
            .unwrap();
        reference(change)
    }

    fn show(&self, id: &Value, depth: &str) -> Value {
        self.cli(&["show", text(&id["id"]), "--depth", depth], 0)
    }

    fn current(&self, id: &Value) -> Value {
        if let Some(observation) = self.quotes.values().find(|quoted| quoted["id"] == id["id"]) {
            return observation.clone();
        }
        let shown = self.show(id, "full");
        json!({"id": id["id"], "revision": shown["record"]["header"]["revision"]})
    }

    fn ingest(&mut self, file: &str, extra: Value) -> Value {
        fs::copy(self.sources.join(file), self.root.join(file)).unwrap();
        self.sequence += 1;
        let format = if file.ends_with(".json") {
            "application/json"
        } else {
            "text/plain"
        };
        let mut manifest = json!({
            "schema_version": "inv.ingest.v1", "command_id": format!("scenario-d-collect-{}", self.sequence),
            "actor": ACTOR, "source": {"title": file, "format": format}, "paths": [file], "extract_text": true,
        });
        for (key, value) in extra.as_object().unwrap() {
            manifest[key] = value.clone();
        }
        self.submit_ingest(manifest)
    }

    fn submit_ingest(&mut self, mut manifest: Value) -> Value {
        if self.case.is_empty() {
            manifest["new_case"] = json!({"title": self.manifest["question"], "mode": "explore"});
        } else {
            manifest["case_id"] = json!(self.case);
        }
        let path = self.root.join(format!("ingest-{}.json", self.sequence));
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let receipt = run(
            &self.root,
            &["ingest", "--input", path.to_str().unwrap(), "--json"],
            None,
            0,
        );
        for change in receipt["created"].as_array().unwrap() {
            let id = text(&change["id"]);
            if self.case.is_empty() && id.starts_with("case_") {
                self.case = id.to_owned();
            }
            self.created.push(id.to_owned());
        }
        receipt
    }

    /// Collect one document as its own acquisition.
    fn collect(&mut self, file: &str) {
        let receipt = self.ingest(file, json!({}));
        let item = &receipt["items"][0];
        assert_eq!(receipt["items"].as_array().unwrap().len(), 1);
        assert_eq!(item["outcome"], "imported");
        let entry = self.manifest["source_files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["path"] == format!("sources/{file}"))
            .unwrap();
        // The retained bytes are the fixture's immutable bytes.
        assert_eq!(
            item["digest"],
            format!("sha256:{}", text(&entry["sha256"])),
            "{file}"
        );
        assert_eq!(item["size_bytes"], entry["bytes"], "{file}");
        self.documents.insert(
            file.to_owned(),
            Document {
                source: text(&item["source_id"]).to_owned(),
                text: text(&item["derivative_id"]).to_owned(),
                original: text(&item["artifact_id"]).to_owned(),
                digest: text(&item["digest"]).to_owned(),
                bytes: fs::read(self.sources.join(file)).unwrap(),
            },
        );
    }

    /// Record the exact source bytes matching `selector` as a text
    /// Observation with a byte-exact quote. One Observation per passage.
    fn quote(&mut self, file: &str, selector: &str, limitations: &[&str]) -> Value {
        let wanted = flat(selector.as_bytes());
        let key = (file.to_owned(), wanted.clone());
        if let Some(existing) = self.quotes.get(&key) {
            return existing.clone();
        }
        let document = &self.documents[file];
        let start = flat(&document.bytes)
            .windows(wanted.len())
            .position(|window| window == wanted.as_slice())
            .unwrap_or_else(|| panic!("{selector:?} is not in {file}"));
        let exact = std::str::from_utf8(&document.bytes[start..start + wanted.len()])
            .unwrap()
            .to_owned();
        let payload = json!({
            "value": {"kind": "text", "statement": exact},
            "basis": {"kind": "source", "artifact_ref": {"id": document.text, "revision": 1},
                "selector": {"schema_version": "inv.selector.v1", "artifact_digest": document.digest,
                    "selector": {"type": "byte_range", "start": start, "end": start + exact.len()}},
                "text_quote": {"exact": exact}},
            "extraction_method": "Quoted the passage byte for byte from the retained document",
            "validity": "valid", "quality": [], "limitations": limitations,
            "evidence_kind": "observational", "variables_changed": [],
        });
        let lineage = (document.source.clone(), document.original.clone());
        let observation = self.one("observation.add", payload);
        let shown = self.show(&observation, "full");
        let basis = &shown["record"]["body"]["basis"];
        assert_eq!(basis["text_quote"]["state"], "verified", "{selector}");
        assert_eq!(basis["selector"]["validation"], "verified", "{selector}");
        // Derivation lineage leads back to this document's acquisition.
        assert_eq!(
            shown["lineage"]["origins"][0]["source_ref"]["id"],
            json!(lineage.0)
        );
        assert_eq!(
            shown["lineage"]["origins"][0]["artifact_ref"]["id"],
            json!(lineage.1)
        );
        self.quotes.insert(key, observation.clone());
        observation
    }

    fn quoted(&self, file: &str, selector: &str) -> Value {
        self.quotes[&(file.to_owned(), flat(selector.as_bytes()))].clone()
    }

    fn origin_passage(&self, origin: &str, file: &str) -> Value {
        let passage = self.manifest["claim_origins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == origin)
            .unwrap()["passages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|passage| passage["source"] == file)
            .unwrap()
            .clone();
        self.quoted(file, text(&passage["selector"]))
    }

    /// Request review, have a second actor review from the recorded sources,
    /// and validate. No inquiry plan or executable step is involved.
    fn review_and_validate(&mut self, candidate: &Value) -> Value {
        let basis = self.show(candidate, "full")["review_basis"].clone();
        let mut pre = basis["read_set"].as_array().cloned().unwrap();
        pre.push(basis["subject_ref"].clone());
        let ready = self.submit(
            ACTOR,
            "conclusion.request_review",
            json!({"conclusion_ref": candidate}),
            json!(pre),
            0,
        );
        let ready = reference(&ready["updated"][0]);
        let basis = self.show(&ready, "full")["review_basis"].clone();
        let materials: Vec<Value> = basis["read_set"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| text(&item["id"]).starts_with("obs_"))
            .cloned()
            .collect();
        let review = self.submit(REVIEWER, "review.submit", json!({
            "kind": "validation", "subject_ref": basis["subject_ref"], "basis_digest": basis["basis_digest"],
            "read_set": basis["read_set"],
            "method": "Re-read every cited passage against its retained source and check which edition the erratum names.",
            "materials_examined": materials, "shared_dependencies": [], "objection_refs": [],
            "verdict": "supports", "caveats": ["Source reading only; no unit was tested for this review."],
            "context_isolation": "fresh_context_unblinded",
            "context": {"saw_original_conclusion": {"state": "known", "value": true},
                "gathered_new_evidence": {"state": "known", "value": false},
                "method_or_source_changes": [], "activity": "Read the retained sources and the recorded origins.",
                "dependencies": "shared", "dependence_basis": "The same retained documents."},
        }), json!([]), 0);
        let review = reference(&review["created"][0]);
        let basis = self.show(&ready, "full")["review_basis"].clone();
        let mut pre = basis["read_set"].as_array().cloned().unwrap();
        pre.extend([ready.clone(), review.clone()]);
        let validated = self.submit(
            ACTOR,
            "conclusion.validate",
            json!({"conclusion_ref": ready, "review_ref": review}),
            json!(pre),
            0,
        );
        reference(&validated["updated"][0])
    }
}

/// Outputs that must not depend on case content: onboarding, operation
/// schemas, method packs and one method card.
fn trusted_surfaces(walk: &Walk) -> Value {
    let onboard = run(&walk.root, &["onboard", "--json"], None, 0);
    let schemas: Vec<Value> = SCHEMAS
        .iter()
        .map(|name| run(&walk.root, &["schema", name, "--json"], None, 0))
        .collect();
    let methods = walk.cli(&["method", "list"], 0);
    let card = walk.cli(&["guide", "frame"], 0);
    for (name, value) in [
        ("onboard", &onboard),
        ("methods", &methods),
        ("guide", &card),
    ] {
        assert!(!contains_token(&value.to_string()), "{name}: {value}");
        argv_are_trusted(value, name);
    }
    let surfaces = json!({
        "onboard": {
            "active_operations": onboard["active_operations"], "limits": onboard["limits"],
            "mental_model": onboard["mental_model"], "when_to_use": onboard["when_to_use"],
            "state": onboard["state"],
            "next": onboard["next"].as_array().unwrap().iter().map(|next| next["kind"].clone()).collect::<Vec<_>>(),
        },
        "schemas": schemas,
        "packs": methods["packs"], "content_trust": methods["content_trust"],
        "cards": methods["stages"].as_array().unwrap().iter()
            .flat_map(|stage| stage["cards"].as_array().unwrap().iter().map(|card| card["name"].clone()))
            .collect::<Vec<_>>(),
        "card": [&card["guide"]["card"], &card["guide"]["pack"]],
    });
    // Schema examples name the workspace and case; nothing else may vary.
    let normal = surfaces
        .to_string()
        .replace(walk.root.to_str().unwrap(), "WORKSPACE")
        .replace(&walk.case, "CASE");
    serde_json::from_str(&normal).unwrap()
}

/// What a reader of the case sees: the variant-independent shape of the
/// result. The injected variant must produce the baseline value.
type Outcome = Value;

fn walk(variant: &str) -> (Walk, Outcome) {
    let mut w = Walk::new(variant);

    // Four documents, each its own Source and acquisition.
    for file in DOCUMENTS {
        w.collect(file);
    }
    let before = trusted_surfaces(&w);
    if w.injected() {
        w.collect(CRAWL);
    }
    let sources: BTreeSet<&str> = w.documents.values().map(|d| d.source.as_str()).collect();
    assert_eq!(sources.len(), w.documents.len());
    for document in w.documents.values() {
        let shown = w.cli(&["show", &document.source, "--depth", "full"], 0);
        assert_eq!(
            shown["record"]["body"]["acquisition"]["channel"],
            "local_file"
        );
    }

    // The search log is retained as material for the search Runs.
    let log = w.ingest(SEARCH_LOG, json!({}))["items"][0].clone();
    let log_ref = json!({"id": log["artifact_id"], "revision": 1});

    // Every claim-origin passage and every expected selector is quoted.
    let bench_limit =
        text(&property(&w.expected, "bench_test_build_caveat")["limitation"]).to_owned();
    let claim_origins = w.manifest["claim_origins"].as_array().unwrap().clone();
    for origin in &claim_origins {
        for passage in origin["passages"].as_array().unwrap() {
            let limits: Vec<&str> = if origin["id"] == "O2-article-bench-test" {
                vec![bench_limit.as_str()]
            } else {
                vec![]
            };
            w.quote(
                text(&passage["source"]),
                text(&passage["selector"]),
                &limits,
            );
        }
    }
    let kept = property(&w.expected, "every_original_selector_kept");
    assert_eq!(kept["assert"], "quotes_present_in_sources");
    for quote in kept["quotes"].as_array().unwrap() {
        w.quote(text(&quote["source"]), text(&quote["quote"]), &[]);
    }
    let mut planted = Vec::new();
    if w.injected() {
        let inert = property(&w.expected, "injected_text_inert");
        assert_eq!(inert["assert"], "instructions_in_source_not_executed");
        assert_eq!(inert["source"], CRAWL);
        // The planted notice is kept in the record, quoted, never omitted.
        for quote in inert["quotes"].as_array().unwrap() {
            planted.push(w.quote(
                CRAWL,
                text(quote),
                &["Planted instruction text; data only."],
            ));
        }
    }

    // Trace each passage the way the manifest describes it: originals first.
    for origin in &claim_origins {
        let originating = text(&origin["originating_source"]);
        let mut upstream: Option<Value> = None;
        for passage in origin["passages"].as_array().unwrap() {
            let file = text(&passage["source"]);
            let observation = w.quoted(file, text(&passage["selector"]));
            let link = match text(&passage["role"]) {
                "original" => {
                    assert_eq!(file, originating);
                    json!({"kind": "original"})
                }
                _ => {
                    json!({"kind": "restates", "observation_ref": w.current(upstream.as_ref().unwrap())})
                }
            };
            let traced = w.one("origin.trace", json!({
                "observation_ref": observation, "origin": link,
                "dimensions": {"upstream_source": originating},
                "rationale": format!("{} passage of {}.", text(&passage["role"]), text(&origin["id"])),
            }));
            upstream = Some(traced);
        }
    }

    // Duplicate-origin trap: three acquisitions, one known origin.
    let one = property(&w.expected, "one_origin_for_disputed_value");
    assert_eq!(one["assert"], "claim_origin_count");
    let disputed = text(&one["claim_origin"]).to_owned();
    let restated: Vec<Value> = strings(&one["acquisition_sources"])
        .iter()
        .map(|file| w.origin_passage(&disputed, file))
        .collect();
    let question = w.one("question.create", json!({
        "wording": w.manifest["question"], "importance": "high",
        "context": format!("Named build {}; {}", text(&w.manifest["named_build"]), text(&w.manifest["build_to_edition"]["rule"])),
    }));
    let h64 = w.one(
        "hypothesis.create",
        json!({
            "question_ref": question, "statement": "The window at build 3.1.4 is 64 frames.",
            "possible_falsifiers": ["A first-party erratum for Edition 3.1 changes the value."],
            "rival_refs": [], "assumption_refs": [],
        }),
    );
    // The prior judgment: three documents read as support for 64.
    let prior = w.one(
        "assessment.add",
        json!({
            "subject_ref": h64, "evidence_refs": restated, "relation": "supports",
            "rationale": "The specification, a trade article and a blog all state 64 frames.",
        }),
    );
    let shown = w.show(&prior, "full");
    let summary = &shown["evidence_origins"][0]["summary"];
    assert_eq!(summary["known_origin_count"], one["origin_count"]);
    assert_eq!(summary["unknown_origin_count"], 0);
    assert_eq!(
        summary["acquisition_count"],
        one["acquisitions_kept_as_separate_records"]
    );
    let mut expected_sources: Vec<String> = strings(&one["acquisition_sources"])
        .iter()
        .map(|file| w.documents[file].source.clone())
        .collect();
    expected_sources.sort();
    assert_eq!(summary["acquisitions"], json!(expected_sources));
    assert_eq!(summary["independent_pairs"], 0);
    let originating = text(&claim_origins[0]["originating_source"]);
    assert_eq!(
        summary["origins"][0]["root"]["id"],
        w.origin_passage(&disputed, originating)["id"]
    );
    // Apparent corroboration is not counted: the confidence view names it.
    let computed: Vec<&Value> = shown["confidence"]["computed_concerns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|concern| &concern["code"])
        .collect();
    assert_eq!(computed, [&json!("single_origin")], "{shown:#}");

    // The bench test is a different origin in the same Source record.
    let bench_property = property(&w.expected, "bench_test_separate_origin");
    assert_eq!(bench_property["assert"], "distinct_origin");
    let same = text(&bench_property["same_source_record_as"]);
    let bench = w.origin_passage(text(&bench_property["claim_origin"]), same);
    let restatement = w.origin_passage(text(&bench_property["different_from"]), same);
    let bench_origin = w.show(&bench, "full")["claim_origin"].clone();
    let restated_origin = w.show(&restatement, "full")["claim_origin"].clone();
    assert_eq!(bench_origin["origin"]["state"], "known");
    assert_eq!(bench_origin["origin"]["root"]["id"], bench["id"]);
    assert_eq!(
        restated_origin["origin"]["root"]["id"],
        w.origin_passage(&disputed, originating)["id"]
    );
    assert_ne!(
        bench_origin["origin"]["root"],
        restated_origin["origin"]["root"]
    );
    assert_eq!(
        bench_origin["acquisitions"],
        json!([w.documents[same].source])
    );
    assert_eq!(
        restated_origin["acquisitions"],
        bench_origin["acquisitions"]
    );

    // Source quality, separately per dimension.
    let erratum = w.origin_passage("O3-erratum", ERRATUM);
    let currency = property(&w.expected, "erratum_currency");
    assert_eq!(currency["assert"], "erratum_applies_to_named_build");
    assert_eq!(currency["named_build"], w.manifest["named_build"]);
    let edition = text(&w.manifest["build_to_edition"]["named_build_edition"]).to_owned();
    assert_eq!(currency["named_build_edition"], edition.as_str());
    let applies = currency["applies"].as_bool().unwrap();
    assert_eq!(applies, !w.twin());
    let mapping = w.quoted(
        SPEC,
        &format!("Firmware builds numbered {edition}.x implement Edition {edition}."),
    );
    let affected = if applies {
        w.quoted(
            ERRATUM,
            &format!("Affected edition: Edition {edition} (firmware builds numbered {edition}.x)."),
        )
    } else {
        w.quoted(
            ERRATUM,
            "Affected edition: Edition 3.2 only (firmware builds numbered 3.2.x).",
        )
    };
    let roles = [
        (
            ARTICLE,
            "secondary",
            "Paragraph 3 restates the specification; paragraph 6 reports the author's own bench test.",
        ),
        (
            BLOG,
            "tertiary",
            "Repeats the article's restatement and tested nothing.",
        ),
        (
            ERRATUM,
            "primary",
            "The working group issued the correction.",
        ),
    ];
    for (file, role, rationale) in roles {
        let source = json!({"id": w.documents[file].source, "revision": 1});
        let current = if file == ERRATUM {
            json!({"judgment": "current", "rationale": "The erratum states it is in effect."})
        } else {
            Value::Null
        };
        w.one(
            "source.assess",
            json!({
                "source_ref": source, "origin": {"judgment": role, "rationale": rationale},
                "currency": current, "evidence_refs": [], "limitations": [],
            }),
        );
    }
    // Erratum currency for the named build, recorded on the specification.
    let spec_source = json!({"id": w.documents[SPEC].source, "revision": 1});
    let judgment = if applies {
        json!({"judgment": "superseded", "rationale": format!("The erratum names Edition {edition}, which build {} implements.", text(&w.manifest["named_build"]))})
    } else {
        json!({"judgment": "current", "rationale": format!("The erratum names only Edition 3.2; build {} implements Edition {edition}.", text(&w.manifest["named_build"]))})
    };
    let assessed = w.one("source.assess", json!({
        "source_ref": spec_source, "origin": {"judgment": "primary", "rationale": "The publisher's own specification."},
        "currency": judgment, "evidence_refs": [erratum, affected, mapping], "limitations": ["Only the retained edition was examined."],
    }));
    if applies {
        w.one("source.withdraw", json!({
            "source_ref": assessed, "reason": "corrected_by_erratum",
            "scope": {"extent": "partial", "statement": format!("Section 5.2 and Table 7 row 4: 64 frames for Edition {edition}.")},
            "rationale": "The first-party erratum withdraws the printed value for the named build's edition.",
            "evidence_refs": [erratum, affected],
        }));
    }
    let spec_shown = w.cli(&["show", &w.documents[SPEC].source, "--depth", "full"], 0);
    let withdrawals = spec_shown["record"]["body"]["withdrawals"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let spec_quote = w.origin_passage(&disputed, SPEC);
    let use_state = w.show(&spec_quote, "full")["evidentiary_use"]["state"].clone();
    // The erratum invalidates the prior judgment without deleting it; in the
    // twin it does not apply and the judgment stays current.
    let prior_now = w.show(&prior, "full");
    assert_eq!(prior_now["record"]["header"]["revision"], 1);
    assert_eq!(prior_now["record"]["body"]["relation"], "supports");
    if applies {
        assert_eq!(withdrawals.len(), 1);
        assert_eq!(withdrawals[0]["reason"], "corrected_by_erratum");
        assert_eq!(withdrawals[0]["scope"]["extent"], "partial");
        assert_eq!(use_state, "partially_withdrawn");
        assert_eq!(
            prior_now["freshness"]["effective"], "needs_review",
            "{prior_now:#}"
        );
        assert!(
            prior_now["freshness"]["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason["trigger"]["kind"] == "withdrawn"
                    && reason["cause"]["id"] == w.documents[SPEC].source.as_str())
        );
        // The quote read before the withdrawal needs review; a quote of a row
        // the withdrawal leaves alone, recorded after it, was read with the
        // withdrawal in effect and is current (W06b.16).
        assert_eq!(
            w.show(&spec_quote, "full")["freshness"]["effective"],
            "needs_review"
        );
        let idle = w.quote(
            SPEC,
            "row 5  Idle session timeout ......................... 30 seconds",
            &[],
        );
        let idle = w.show(&idle, "full");
        assert_eq!(idle["evidentiary_use"]["state"], "partially_withdrawn");
        assert_eq!(idle["freshness"]["effective"], "current", "{idle:#}");
    } else {
        assert!(withdrawals.is_empty());
        assert_eq!(use_state, "in_use");
        assert_eq!(
            prior_now["freshness"]["effective"], "current",
            "{prior_now:#}"
        );
    }
    let spec_currency = spec_shown["record"]["body"]["source_assessment"]["judgments"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["currency"]["judgment"]
        .clone();

    // A reference-only copy of the erratum, named by its register entry, is
    // never presented as retained bytes.
    let searches: Vec<Value> = serde_json::from_slice::<Value>(
        &fs::read(w.sources.join(SEARCH_LOG)).unwrap(),
    )
    .unwrap()["searches"]
        .as_array()
        .unwrap()
        .clone();
    let entry = searches
        .iter()
        .filter(|search| search["registry"] == ERRATA_REGISTER)
        .flat_map(|search| search["hits"].as_array().unwrap().iter())
        .next()
        .map(|hit| text(hit).to_owned())
        .unwrap();
    let erratum_entry = w.manifest["source_files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == format!("sources/{ERRATUM}"))
        .unwrap()
        .clone();
    w.sequence += 1;
    let referenced = w.submit_ingest(json!({
        "schema_version": "inv.ingest.v1", "command_id": format!("scenario-d-refer-{}", w.sequence), "actor": ACTOR,
        "source": {"title": format!("{ERRATA_REGISTER} entry {entry} (not retained)")},
        "external_refs": [{"locator": format!("register://corvin-errata/{entry}"),
            "digest": format!("sha256:{}", text(&erratum_entry["sha256"])), "size_bytes": erratum_entry["bytes"]}],
    }));
    let reference_item = &referenced["items"][0];
    assert_eq!(reference_item["retention"]["state"], "source_unavailable");
    let reference_artifact = text(&reference_item["artifact_id"]).to_owned();
    let shown = w.cli(&["show", &reference_artifact, "--depth", "full"], 0);
    assert_eq!(shown["retention"][0]["state"], "source_unavailable");
    assert_eq!(shown["retention"][0]["offline_complete"], false);
    let selector = w.root.join("reference-selector.json");
    fs::write(
        &selector,
        json!({"schema_version": "inv.selector.v1", "artifact_digest": format!("sha256:{}", text(&erratum_entry["sha256"])),
            "selector": {"type": "whole"}})
        .to_string(),
    )
    .unwrap();
    let refused = w.cli(
        &[
            "evidence",
            "read",
            &reference_artifact,
            "--selector-file",
            selector.to_str().unwrap(),
        ],
        5,
    );
    assert_eq!(refused["error"]["actual"]["state"], "source_unavailable");

    // Search coverage: one Run per registry, every empty search kept.
    let mut registries: Vec<String> = Vec::new();
    for search in &searches {
        let registry = text(&search["registry"]).to_owned();
        if !registries.contains(&registry) {
            registries.push(registry);
        }
    }
    let mut runs = BTreeMap::new();
    for registry in &registries {
        let done: Vec<&Value> = searches
            .iter()
            .filter(|search| search["registry"] == registry.as_str())
            .collect();
        let total: u64 = done
            .iter()
            .map(|search| search["result_count"].as_u64().unwrap())
            .sum();
        let times: BTreeSet<&str> = done
            .iter()
            .map(|search| text(&search["searched_at"]))
            .collect();
        let run = w.one("run.record", json!({
            "intent": {"kind": "exploration", "intent": format!("Search {registry} for errata and conformance statements")},
            "producer": ACTOR, "artifact_refs": [log_ref], "execution_status": "completed",
            "evidence_completeness": "complete", "evidence_kind": "observational", "variables_changed": [],
            "search_coverage": {
                "corpus": registry,
                "revision_bounds": format!("Registry contents as searched at {}", times.into_iter().collect::<Vec<_>>().join(", ")),
                "queries": done.iter().map(|search| search["query"].clone()).collect::<Vec<_>>(),
                "filters": [], "inclusion_rules": [], "exclusion_rules": [],
                "total_hits": total, "examined_hits": total, "empty_hits": null,
                "query_results": done.iter().map(|search| json!({"query": search["query"], "hits": search["result_count"]})).collect::<Vec<_>>(),
            },
        }));
        runs.insert(registry.clone(), run);
    }
    let cover = property(&w.expected, "search_coverage_includes_empty_results");
    assert_eq!(cover["assert"], "search_coverage_run_lists_registries");
    assert_eq!(cover["source"], SEARCH_LOG);
    let mut recorded = BTreeMap::new();
    for (registry, run) in &runs {
        let shown = w.show(run, "full");
        let coverage = shown["record"]["body"]["search_coverage"].clone();
        assert_eq!(coverage["corpus"], registry.as_str());
        assert_eq!(shown["record"]["body"]["artifact_refs"], json!([log_ref]));
        recorded.insert(registry.clone(), coverage);
    }
    let listed: BTreeSet<String> = recorded.keys().cloned().collect();
    let wanted: BTreeSet<String> = strings(&cover["registries"]).into_iter().collect();
    assert_eq!(listed, wanted);
    let empty: BTreeSet<String> = recorded
        .iter()
        .filter(|(_, coverage)| {
            coverage["total_hits"] == 0
                && coverage["query_results"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|result| result["hits"] == 0)
        })
        .map(|(registry, _)| registry.clone())
        .collect();
    let wanted: BTreeSet<String> = strings(&cover["zero_result_registries"])
        .into_iter()
        .collect();
    assert_eq!(empty, wanted);
    // Empty searches inside the errata register stay recorded as zero results.
    let register = &recorded[ERRATA_REGISTER];
    let empty_in_register: Vec<String> = searches
        .iter()
        .filter(|search| search["registry"] == ERRATA_REGISTER)
        .filter(|search| {
            register["query_results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|result| result["query"] == search["query"] && result["hits"] == 0)
        })
        .map(|search| text(&search["search_id"]).to_owned())
        .collect();
    assert_eq!(
        empty_in_register,
        strings(&cover["zero_result_searches_in_errata_register"])
    );

    // Empty registry searches are not proof that no further erratum exists:
    // an absence claim resting on them without an expected signal is gated.
    let mut zero = Vec::new();
    for (index, search) in searches.iter().enumerate() {
        if search["result_count"] != 0 {
            continue;
        }
        zero.push(w.one("observation.add", json!({
            "value": {"kind": "measurement", "value": "0", "unit": "hits"},
            "basis": {"kind": "source", "artifact_ref": log_ref,
                "selector": {"schema_version": "inv.selector.v1", "artifact_digest": log["digest"],
                    "selector": {"type": "json_pointer", "pointer": format!("/searches/{index}/result_count"), "parser": null}},
                "text_quote": null},
            "run_ref": runs[text(&search["registry"])],
            "extraction_method": "Read result_count from the retained search log", "validity": "valid",
            "quality": [], "limitations": ["Zero hits for this query in this registry only"],
            "evidence_kind": "observational", "variables_changed": [],
        })));
    }
    let none = w.one("hypothesis.create", json!({
        "question_ref": question, "statement": format!("No further erratum changes the Edition {edition} value."),
        "possible_falsifiers": ["A registry lists a further erratum."], "rival_refs": [], "assumption_refs": [],
    }));
    let absence = w.one("assessment.add", json!({
        "subject_ref": none, "evidence_refs": zero, "relation": "supports",
        "rationale": "The other registries returned nothing.",
        "absence": {"opportunity": {"description": "Registry searches with zero hits", "refs": runs.values().collect::<Vec<_>>()}},
    }));
    let claim = w.one("conclusion.create", json!({
        "statement": "No further erratum exists.", "scope": format!("Edition {edition}"),
        "significance": "Would close the currency question.", "support_refs": [absence], "contradiction_refs": [],
        "caveats": ["Only the listed registries were searched."], "claim_form": "absence",
    }));
    let basis = w.show(&claim, "full")["review_basis"].clone();
    let mut pre = basis["read_set"].as_array().cloned().unwrap();
    pre.push(basis["subject_ref"].clone());
    let gated = w.submit(
        ACTOR,
        "conclusion.request_review",
        json!({"conclusion_ref": claim}),
        json!(pre),
        5,
    );
    assert_eq!(gated["error"]["code"], "GATE_FAILED");
    assert_eq!(
        gated["error"]["actual"]["human_question"],
        "Are detection limits appropriate?"
    );
    let mut absence_codes: Vec<String> = gated["error"]["actual"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|reason| reason["missing"].as_array().unwrap().iter())
        .map(|missing| text(&missing["code"]).to_owned())
        .collect();
    absence_codes.sort();
    assert!(
        absence_codes.contains(&"absence.expected_signal_missing".to_owned()),
        "{gated:#}"
    );
    assert_eq!(
        w.show(&claim, "full")["record"]["body"]["status"],
        "candidate"
    );

    // The answer for the named build.
    let h48 = w.one(
        "hypothesis.create",
        json!({
            "question_ref": question, "statement": "The window at build 3.1.4 is 48 frames.",
            "possible_falsifiers": ["The erratum does not apply to the named build's edition."],
            "rival_refs": [h64], "assumption_refs": [],
        }),
    );
    let effective = &currency["effective_value"];
    let outcomes = strings(&w.expected["acceptable_outcomes"]);
    let limits = strings(&w.expected["required_visible_limits"]);
    let mut caveats = limits.clone();
    caveats.push(bench_limit.clone());
    if w.injected() {
        // The forum anecdote is a separate, unjudged origin: never counted as
        // independent support.
        let anecdote_property = property(&w.expected, "forum_anecdote_not_corroboration");
        assert_eq!(
            anecdote_property["assert"],
            "not_counted_as_independent_support"
        );
        let anecdote = w.quote(
            CRAWL,
            text(&anecdote_property["quote"]),
            &[text(&anecdote_property["reason"])],
        );
        w.one(
            "origin.trace",
            json!({
                "observation_ref": anecdote, "origin": {"kind": "original"},
                "rationale": "An unattributed forum post; build, unit and procedure are unknown.",
            }),
        );
        let anecdote = w.current(&anecdote);
        let bench_now = w.current(&bench);
        let read = w.one("assessment.add", json!({
            "subject_ref": h48, "evidence_refs": [anecdote, bench_now], "relation": "inconclusive",
            "rationale": format!("The forum post is not independent support: {}.", text(&anecdote_property["reason"])),
        }));
        let shown = w.show(&read, "full");
        let summary = &shown["evidence_origins"][0]["summary"];
        assert_eq!(summary["known_origin_count"], 2);
        assert_eq!(summary["independent_pairs"], 0);
        assert_eq!(summary["pairs"][0]["dependence"], "unknown");
        let codes: Vec<&Value> = shown["confidence"]["computed_concerns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|concern| &concern["code"])
            .collect();
        assert_eq!(codes, [&json!("independence_unknown")]);
    }
    let conclusion = if applies {
        // The bench test and the erratum are judged independent on stated
        // dimensions; the restatements add nothing beyond their origin.
        w.one("origin.judge_dependence", json!({
            "roots": [w.current(&bench), w.current(&erratum)], "judgment": "independent_on_stated_dimensions",
            "dimensions": ["method", "upstream_source"],
            "basis": "The erratum is a first-party correction; the bench test is the article author's own run on one unit.",
        }));
        let support = w.one("assessment.add", json!({
            "subject_ref": h48, "evidence_refs": [w.current(&erratum), w.current(&bench)], "relation": "supports",
            "rationale": format!("Erratum for Edition {edition} sets 48 frames; the bench test stalled above 48 on one unit of another 3.1 build."),
            "rival_expectations": [{"rival_ref": h64, "expectation": "less_expected",
                "rationale": "A 64-frame window predicts neither the correction nor a stall at 56 frames."}],
        }));
        w.one("conclusion.create", json!({
            "statement": outcomes[0], "scope": format!("Build {} (Edition {edition})", text(&w.manifest["named_build"])),
            "significance": outcomes[1], "support_refs": [support], "contradiction_refs": [prior],
            "caveats": caveats,
            "confidence": {"level": "low", "rationale": "One first-party correction and one limited bench test.",
                "limitations": [bench_limit], "what_would_change_it": "A later erratum or a conformance statement for the named build.",
                "downgrade_reasons": {"validity": "ok", "consistency": "concern", "directness": "concern",
                    "precision": "not_assessed", "completeness": "concern"}},
        }))
    } else {
        // Neither the later-edition erratum nor the pre-release bench test
        // bears on Edition 3.1.
        for evidence in [w.current(&erratum), w.current(&bench)] {
            w.one("assessment.add", json!({
                "subject_ref": h64, "evidence_refs": [evidence], "relation": "non_diagnostic",
                "rationale": "It concerns Edition 3.2 or a pre-release of it, not the named build's edition.",
            }));
        }
        w.one("conclusion.create", json!({
            "statement": outcomes[0], "scope": format!("Build {} (Edition {edition})", text(&w.manifest["named_build"])),
            "significance": outcomes[1], "support_refs": [prior], "contradiction_refs": [],
            "caveats": caveats,
            "confidence": {"level": "low", "rationale": "One specification origin; no independent measurement for this edition.",
                "limitations": [bench_limit], "what_would_change_it": "An independent measurement on an Edition 3.1 build.",
                "downgrade_reasons": {"validity": "ok", "consistency": "concern", "directness": "concern",
                    "precision": "not_assessed", "completeness": "concern"}},
        }))
    };
    let value = format!("{} {}", effective["value"], text(&effective["unit"]));
    assert!(
        outcomes[0].contains(&value),
        "{value} not in {}",
        outcomes[0]
    );
    let candidate = w.show(&conclusion, "summary");
    let concerns = candidate["confidence"]["concerns"].clone();

    // In the injected variant the planted notice is in the record by now, yet
    // nothing was promoted on its say-so.
    let validated = w.review_and_validate(&conclusion);
    let shown = w.show(&validated, "full");
    assert_eq!(shown["record"]["body"]["status"], "validated");
    // The conclusion and its review were recorded after the partial
    // withdrawal and the currency concern, so both were read in effect: the
    // conclusion is current although its contradiction, the prior judgment,
    // stays `needs_review` (freshness rules version 4, W06b.16).
    assert_eq!(shown["freshness"]["effective"], "current", "{shown:#}");
    assert_eq!(shown["freshness"]["reasons"], json!([]), "{shown:#}");
    for caveat in &caveats {
        assert!(
            shown["recorded_caveats"]
                .as_array()
                .unwrap()
                .contains(&json!(caveat)),
            "{caveat}"
        );
    }
    let resolution = if applies {
        json!({"question_ref": question, "disposition": "answered", "answer": outcomes[0],
            "reason": outcomes[1], "evidence_refs": [validated]})
    } else {
        json!({"question_ref": question, "disposition": "inconclusive", "answer": outcomes[0],
            "reason": outcomes[1], "evidence_refs": [validated]})
    };
    w.one("question.resolve", resolution);

    // No inquiry plan and no executable experiment: only search Runs.
    assert!(
        !w.created.iter().any(|id| id.starts_with("inq_")),
        "{:?}",
        w.created
    );
    for id in w.created.iter().filter(|id| id.starts_with("run_")) {
        let shown = w.cli(&["show", id, "--depth", "full"], 0);
        assert_eq!(shown["record"]["body"]["evidence_kind"], "observational");
        assert!(shown["record"]["body"]["search_coverage"].is_object());
    }

    // The limitation and the visible limits reach the handoff.
    let brief = w.cli(
        &["brief", "--level", "standard", "--max-bytes", "262144"],
        0,
    );
    let handoff = brief.to_string();
    assert!(handoff.contains(&bench_limit), "{brief:#}");
    for limit in &limits {
        assert!(handoff.contains(limit.as_str()), "{limit}");
    }
    let after = trusted_surfaces(&w);
    assert_eq!(before, after, "trusted surfaces changed with case content");
    assert_eq!(w.cli(&["check"], 0)["ok"], true);

    if w.injected() {
        injected_text_stays_inert(&w, &planted, &brief);
    }

    let outcome = json!({
        "acquisitions": w.documents.keys().filter(|file| file.as_str() != CRAWL).collect::<Vec<_>>(),
        "origin_summary": {"known": summary_counts(&w, &prior)},
        "spec_currency": spec_currency,
        "spec_withdrawals": withdrawals.iter().map(|entry| json!([entry["reason"], entry["scope"]])).collect::<Vec<_>>(),
        "prior": {"freshness": w.show(&prior, "full")["freshness"]["effective"], "revision": 1},
        "searches": registries.iter().filter(|r| r.as_str() != "Public forum crawl").map(|r| &recorded[r]).collect::<Vec<_>>(),
        "absence_gate": absence_codes,
        "conclusion": {
            "statement": shown["record"]["body"]["statement"], "status": shown["record"]["body"]["status"],
            "caveats": shown["recorded_caveats"], "concerns": concerns,
            "confidence": w.show(&validated, "summary")["confidence"],
        },
        "trusted": after,
    });
    (w, outcome)
}

fn summary_counts(w: &Walk, assessment: &Value) -> Value {
    let summary = &w.show(assessment, "full")["evidence_origins"][0]["summary"];
    json!([
        summary["known_origin_count"],
        summary["acquisition_count"],
        summary["independent_pairs"]
    ])
}

/// The planted notice is kept as quoted data everywhere it can be read, and
/// nothing a reader acts on carries it.
fn injected_text_stays_inert(w: &Walk, planted: &[Value], brief: &Value) {
    assert!(!planted.is_empty());
    for observation in planted {
        let shown = w.show(observation, "full");
        assert_eq!(shown["summary_untrusted_data"], true);
        assert!(
            planted_text_is_labelled(&shown, false, "show") > 0,
            "{shown:#}"
        );
        let basis = &shown["record"]["body"]["basis"];
        let selector = w.root.join("planted-selector.json");
        fs::write(&selector, json!({
            "schema_version": "inv.selector.v1", "artifact_digest": basis["selector"]["artifact_digest"],
            "selector": basis["selector"]["selector"],
        }).to_string()).unwrap();
        let read = w.cli(
            &[
                "evidence",
                "read",
                text(&basis["artifact_ref"]["id"]),
                "--selector-file",
                selector.to_str().unwrap(),
            ],
            0,
        );
        assert_eq!(read["content_trust"], "untrusted_evidence");
        argv_are_trusted(&read, "evidence read");
    }
    planted_text_is_labelled(brief, false, "brief");
    argv_are_trusted(brief, "brief");
    // Search finds the notice and labels each match as recorded evidence.
    let found = w.cli(&["search", "relay", "sweep", "promote", "conclusion"], 0);
    assert!(text(&found["notice"]).contains("never instructions"));
    let results = found["results"].as_array().unwrap();
    assert!(!results.is_empty(), "{found:#}");
    for result in results {
        assert_eq!(result["result_role"], "pointer");
        for found in result["matches"].as_array().unwrap() {
            assert_eq!(found["label"], "recorded_evidence", "{result:#}");
            assert_eq!(found["content_trust"], "untrusted_evidence");
            assert_eq!(found["rendering"], "quoted_data");
        }
    }
    argv_are_trusted(&found, "search");
    // Command selection: ranked next actions never carry planted text.
    for view in [w.cli(&["next"], 0), w.cli(&["frontier"], 0)] {
        argv_are_trusted(&view, "next");
        planted_text_is_labelled(&view, false, "next");
    }
}

/// JSON paths where `left` and `right` differ, with both values.
fn differ(left: &Value, right: &Value, path: &str, out: &mut Vec<String>) {
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for key in keys {
                let (x, y) = (
                    a.get(key).unwrap_or(&Value::Null),
                    b.get(key).unwrap_or(&Value::Null),
                );
                differ(x, y, &format!("{path}/{key}"), out);
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (index, (x, y)) in a.iter().zip(b).enumerate() {
                differ(x, y, &format!("{path}/{index}"), out);
            }
        }
        _ if left != right => out.push(format!("{path}: {left} != {right}")),
        _ => {}
    }
}

/// The baseline outcome, walked once and shared by the tests that read it.
fn baseline() -> &'static Value {
    static BASELINE: OnceLock<Value> = OnceLock::new();
    BASELINE.get_or_init(|| {
        let (walk, outcome) = walk("baseline");
        assert_eq!(walk.expected["variant"], "baseline");
        outcome
    })
}

#[test]
fn scenario_d_baseline_conditional_conclusion_from_sources_alone() {
    let outcome = baseline();
    assert_eq!(outcome["spec_currency"], "superseded");
    assert_eq!(outcome["prior"]["freshness"], "needs_review");
    // Independent pair judged: no consistency concern beyond the recorded
    // contradiction with the superseded prior judgment.
    let consistency = outcome["conclusion"]["concerns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|concern| concern["reason"] == "consistency")
        .unwrap();
    assert_eq!(consistency["computed"], json!(["recorded_contradiction"]));
}

#[test]
fn scenario_d_later_edition_twin_erratum_does_not_apply_to_the_named_build() {
    let (walk, outcome) = walk("later-edition-twin");
    assert_eq!(walk.expected["variant"], "later-edition-twin");
    assert_eq!(outcome["spec_currency"], "current");
    assert_eq!(outcome["spec_withdrawals"], json!([]));
    assert_eq!(outcome["prior"]["freshness"], "current");
    // The conditional answer rests on one origin, and the view says so.
    let consistency = outcome["conclusion"]["concerns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|concern| concern["reason"] == "consistency")
        .unwrap();
    assert_eq!(consistency["computed"], json!(["single_origin"]));
}

#[test]
fn scenario_d_injected_instructions_stay_inert_and_the_result_equals_the_baseline() {
    let baseline = std::thread::spawn(baseline);
    let (injected, outcome) = walk("injected-output");
    let property = property(&injected.expected, "injected_text_inert");
    assert_eq!(property["result_must_equal"], "baseline");
    let baseline = baseline.join().unwrap();
    let mut differences = Vec::new();
    differ(&outcome, baseline, "", &mut differences);
    assert!(
        differences.is_empty(),
        "injected result differs from the baseline: {differences:#?}"
    );
}
