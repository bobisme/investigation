//! Claim-level origin over the Scenario D source-research fixture: each
//! document is its own acquisition, the restated specification value is one
//! known origin, and the article's bench test and the erratum are separate
//! origins whose dependence stays `unknown` until someone judges it.

use std::{collections::BTreeMap, fs, path::Path};

use investigation_core::{
    command::RevisionRef,
    identity::{Id, Kind},
    origin_ops::{self, Dependence, OriginState},
};
use investigation_store::{Store, Workspace, store::ingest::IngestReceipt};
use serde_json::{Value, json};

const ACTOR: &str = "agent:origin-test";
const FIXTURE: &str = "../../fixtures/development/scenario-d-source-research";

fn workspace() -> (tempfile::TempDir, std::path::PathBuf, Store) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        config,
        contents.replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    let store = Store::open(Workspace::open(&root).unwrap());
    (temporary, root, store)
}

fn variant(name: &str) -> (Value, Value, std::path::PathBuf) {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURE)
        .join(name);
    let read = |file: &str| -> Value {
        serde_json::from_slice(&fs::read(directory.join(file)).unwrap()).unwrap()
    };
    (
        read("manifest.json"),
        read("expected.json"),
        directory.join("sources"),
    )
}

struct Document {
    source: Id,
    artifact: Id,
    digest: Value,
    bytes: Vec<u8>,
}

struct Recorded {
    _temporary: tempfile::TempDir,
    store: Store,
    case: Id,
    documents: BTreeMap<String, Document>,
    /// Observation per (claim origin ID, source file).
    passages: BTreeMap<(String, String), RevisionRef>,
    sequence: usize,
}

impl Recorded {
    fn submit(&mut self, op: &str, payload: Value) -> RevisionRef {
        self.sequence += 1;
        let command = json!({
            "schema_version": "inv.command.v1", "command_id": format!("origin-{}", self.sequence),
            "case_id": self.case, "actor": ACTOR, "preconditions": [],
            "operations": [{"op": op, "payload": payload}],
        });
        let receipt = self
            .store
            .record(&serde_json::to_vec(&command).unwrap())
            .unwrap_or_else(|failure| panic!("{op}: {failure:?}"));
        let change = receipt.created.first().or(receipt.updated.first()).unwrap();
        RevisionRef {
            id: change.id,
            revision: change.revision,
        }
    }

    fn current(&mut self, id: Id) -> RevisionRef {
        self.store
            .case(self.case)
            .unwrap()
            .state
            .current(id)
            .unwrap()
            .reference()
    }

    fn passage(&self, origin: &str, source: &str) -> RevisionRef {
        self.passages[&(origin.to_owned(), source.to_owned())]
    }

    fn summary(&mut self, cited: &[RevisionRef]) -> origin_ops::OriginSummary {
        let state = self.store.case(self.case).unwrap().state;
        origin_ops::origin_summary(&state, self.case, cited)
    }
}

fn ingest(
    store: &mut Store,
    root: &Path,
    sources: &Path,
    file: &str,
    case: Option<Id>,
) -> IngestReceipt {
    fs::copy(sources.join(file), root.join(file)).unwrap();
    let mut manifest = json!({
        "schema_version": "inv.ingest.v1", "command_id": format!("collect-{file}"), "actor": ACTOR,
        "source": {"title": file, "format": "text/plain"}, "paths": [file], "extract_text": true,
    });
    match case {
        Some(case) => manifest["case_id"] = json!(case),
        None => {
            manifest["new_case"] = json!({"title": "Do three references corroborate one value?"})
        }
    }
    store
        .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
        .unwrap()
}

/// Collect every document as its own acquisition, record every claim-origin
/// passage as a text Observation with a byte-exact quote, and trace each
/// passage the way the manifest's `claim_origins` describe it.
fn record(name: &str) -> (Recorded, Value, Value) {
    let (manifest, expected, sources) = variant(name);
    let (temporary, root, mut store) = workspace();
    let files = [
        "primary-specification.txt",
        "trade-article.txt",
        "blog-post.txt",
        "first-party-erratum.txt",
    ];
    let mut case = None;
    let mut documents = BTreeMap::new();
    for file in files {
        let receipt = ingest(&mut store, &root, &sources, file, case);
        case = case.or_else(|| {
            receipt
                .receipt
                .created
                .iter()
                .find(|change| change.id.kind() == Kind::Case)
                .map(|change| change.id)
        });
        let item = &receipt.items[0];
        documents.insert(
            file.to_owned(),
            Document {
                source: item.source_id,
                artifact: item.derivative_id.unwrap_or(item.artifact_id),
                digest: json!(item.digest),
                bytes: fs::read(sources.join(file)).unwrap(),
            },
        );
    }
    let mut recorded = Recorded {
        _temporary: temporary,
        store,
        case: case.unwrap(),
        documents,
        passages: BTreeMap::new(),
        sequence: 0,
    };
    let origins = manifest["claim_origins"].as_array().unwrap();
    for origin in origins {
        for passage in origin["passages"].as_array().unwrap() {
            let source = passage["source"].as_str().unwrap();
            let selector = passage["selector"].as_str().unwrap();
            let document = &recorded.documents[source];
            // Fixture selectors match after collapsing line breaks to spaces;
            // the recorded quote is the exact source bytes.
            let flat = |bytes: &[u8]| -> Vec<u8> {
                bytes
                    .iter()
                    .map(|b| if *b == b'\n' { b' ' } else { *b })
                    .collect()
            };
            let wanted = flat(selector.as_bytes());
            let start = flat(&document.bytes)
                .windows(wanted.len())
                .position(|window| window == wanted.as_slice())
                .unwrap_or_else(|| panic!("{selector:?} is not in {source}"));
            let quote = std::str::from_utf8(&document.bytes[start..start + wanted.len()])
                .unwrap()
                .to_owned();
            let quote = quote.as_str();
            let payload = json!({
                "value": {"kind": "text", "statement": quote},
                "basis": {"kind": "source", "artifact_ref": {"id": document.artifact, "revision": 1},
                    "selector": {"schema_version": "inv.selector.v1", "artifact_digest": document.digest,
                        "selector": {"type": "byte_range", "start": start, "end": start + quote.len()}},
                    "text_quote": {"exact": quote}},
                "extraction_method": "direct_read", "validity": "unknown", "quality": [], "limitations": [],
                "evidence_kind": "observational", "variables_changed": [],
            });
            let observation = recorded.submit("observation.add", payload);
            recorded.passages.insert(
                (origin["id"].as_str().unwrap().to_owned(), source.to_owned()),
                observation,
            );
        }
    }
    // Trace originals first, then restatements in passage order.
    for origin in origins {
        let id = origin["id"].as_str().unwrap();
        let originating = origin["originating_source"].as_str().unwrap();
        let mut upstream = None;
        for passage in origin["passages"].as_array().unwrap() {
            let source = passage["source"].as_str().unwrap();
            let observation = recorded.passage(id, source);
            let observation = recorded.current(observation.id);
            let link = match passage["role"].as_str().unwrap() {
                "original" => {
                    assert_eq!(source, originating);
                    json!({"kind": "original"})
                }
                _ => {
                    let upstream: RevisionRef = upstream.unwrap();
                    let upstream = recorded.current(upstream.id);
                    json!({"kind": "restates", "observation_ref": upstream})
                }
            };
            recorded.submit(
                "origin.trace",
                json!({
                    "observation_ref": observation, "origin": link,
                    "dimensions": {"upstream_source": originating},
                    "rationale": format!("{} passage of {id}.", passage["role"].as_str().unwrap()),
                }),
            );
            upstream = Some(observation);
        }
    }
    (recorded, manifest, expected)
}

fn property<'a>(expected: &'a Value, id: &str) -> &'a Value {
    expected["properties"]
        .as_array()
        .unwrap()
        .iter()
        .find(|property| property["id"] == id)
        .unwrap()
}

fn assert_scenario(name: &str) {
    let (mut recorded, manifest, expected) = record(name);
    let one = property(&expected, "one_origin_for_disputed_value");
    let disputed = one["claim_origin"].as_str().unwrap();
    let restated: Vec<RevisionRef> = one["acquisition_sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| recorded.passage(disputed, source.as_str().unwrap()))
        .collect();
    let summary = recorded.summary(&restated);
    assert_eq!(json!(summary.known_origin_count), one["origin_count"]);
    assert_eq!(
        json!(summary.acquisition_count),
        one["acquisitions_kept_as_separate_records"]
    );
    let mut sources: Vec<Id> = one["acquisition_sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| recorded.documents[file.as_str().unwrap()].source)
        .collect();
    sources.sort();
    assert_eq!(summary.acquisitions, sources);
    assert_eq!(summary.unknown_origin_count, 0);
    let root = summary.origins[0].root.unwrap();
    let originating = manifest["claim_origins"][0]["originating_source"]
        .as_str()
        .unwrap();
    assert_eq!(root.id, recorded.passage(disputed, originating).id);

    // The bench test is a different origin in the same Source record as one
    // of the restatements.
    let bench = property(&expected, "bench_test_separate_origin");
    let same = bench["same_source_record_as"].as_str().unwrap();
    let bench_ref = recorded.passage(bench["claim_origin"].as_str().unwrap(), same);
    let restatement = recorded.passage(disputed, same);
    let summary = recorded.summary(&[restatement, bench_ref]);
    assert_eq!(summary.acquisition_count, 1);
    assert_eq!(summary.acquisitions, vec![recorded.documents[same].source]);
    assert_eq!(summary.known_origin_count, 2);
    assert_ne!(summary.origins[0].root, summary.origins[1].root);
    assert_eq!(summary.pairs[0].dependence, Dependence::Unknown);
    assert!(!summary.pairs[0].independent_corroboration);

    // Every passage together: four acquisitions, three origins, and no pair
    // judged, so no independent corroboration is reported.
    let every: Vec<RevisionRef> = recorded.passages.values().copied().collect();
    let summary = recorded.summary(&every);
    assert_eq!(summary.observations, 5);
    assert_eq!(summary.acquisition_count, 4);
    assert_eq!(summary.known_origin_count, 3);
    assert_eq!(summary.unknown_pairs, 3);
    assert_eq!(summary.independent_pairs, 0);
    assert!(
        summary
            .origins
            .iter()
            .all(|group| group.state == OriginState::Known)
    );

    // A reviewer's attributed judgment is the only way a pair becomes
    // independent; the erratum stays a separate origin either way.
    let erratum = recorded.passage("O3-erratum", "first-party-erratum.txt");
    let roots = [bench_ref, erratum].map(|item| recorded.current(item.id));
    recorded.submit(
        "origin.judge_dependence",
        json!({
            "roots": roots, "judgment": "independent_on_stated_dimensions",
            "dimensions": ["method", "upstream_source"],
            "basis": "The erratum is a first-party correction; the bench test is the article author's own run on one unit.",
        }),
    );
    let summary = recorded.summary(&every);
    assert_eq!(summary.independent_pairs, 1);
    assert_eq!(summary.unknown_pairs, 2);
    let state = recorded.store.case(recorded.case).unwrap().state;
    let view = origin_ops::observation_origin(&state, state.current(erratum.id).unwrap()).unwrap();
    assert_eq!(view.origin.state, OriginState::Known);
    assert_eq!(view.dependence.len(), 1);
    assert_eq!(view.dependence[0].assessor.to_string(), ACTOR);
    // Every passage keeps its byte-exact, verified quote after tracing.
    for passage in recorded.passages.values() {
        let body = &state.current(passage.id).unwrap().body;
        assert_eq!(body["basis"]["text_quote"]["state"], "verified", "{body}");
    }
    // Every acquisition stays its own Source record.
    assert_eq!(
        state
            .records()
            .filter(|record| record.header.id.kind() == Kind::Source)
            .count(),
        4
    );
}

#[test]
fn scenario_d_baseline_counts_one_origin_for_three_acquisitions() {
    assert_scenario("baseline");
}

#[test]
fn scenario_d_later_edition_twin_keeps_the_same_origin_structure() {
    assert_scenario("later-edition-twin");
}
