//! Promotion evidence is hashed outside the commit lock (W06b.09b, §10.2
//! step 1). A promotion re-hashes every retained artifact in its read set;
//! with the commit lock held, one promotion over a large artifact would make
//! every other writer wait for that read.
//!
//! The store runs on the lock-probing filesystem of the streaming ingest
//! tests: every read of a blob first checks that the commit lock is free, so
//! a full read under the lock fails the test wherever it happens.
use std::{
    cell::Cell,
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use investigation_core::{
    basis::basis_digest,
    catalog::standard_catalog,
    command::RevisionRef,
    identity::{Id, Kind},
    review_ops::required_read_set,
    wire::{CommitStatus, ErrorCode, Failure, Receipt},
};
use serde_json::{Value, json};

use super::{DEFERRED_ATTEMPTS, PromotionBlobs};
use crate::{
    durable::{CommitLock, WriteGate},
    store::{Store, diag_tests, ingest::streaming_tests::Probing},
    workspace::Workspace,
};

const MIB: u64 = 1024 * 1024;
const ACTOR: &str = "agent:promotion-test";
const COMMIT_LOCK: &str = "state/locks/commit.lock";
/// The read buffer of a blob hash.
const BUFFER: u64 = 64 * 1024;

fn op(name: &str, payload: Value) -> Value {
    json!({"op": name, "payload": payload})
}

/// A sparse file of `size` bytes: zeros except a marker every 4 MiB, so the
/// artifact takes little disk however large it is.
fn sparse(path: &Path, size: u64) {
    let mut file = fs::File::create(path).unwrap();
    file.set_len(size).unwrap();
    for offset in (0..size.saturating_sub(8)).step_by(4 * MIB as usize) {
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&offset.to_le_bytes()).unwrap();
    }
}

/// Replace a blob with a file of the same size and modification time whose
/// bytes differ: what a check of size and mtime alone would miss.
fn replace(blob: &Path, size: u64) {
    let modified = fs::metadata(blob).unwrap().modified().unwrap();
    let forged = blob.with_extension("forged");
    sparse(&forged, size);
    let mut file = fs::OpenOptions::new().write(true).open(&forged).unwrap();
    file.write_all(b"forged").unwrap();
    file.set_modified(modified).unwrap();
    drop(file);
    fs::rename(&forged, blob).unwrap();
}

/// A case whose retained artifact backs one observation, on a probed store.
struct Flow {
    _temporary: tempfile::TempDir,
    workspace: Workspace,
    store: Store<Probing>,
    reads: Rc<Cell<usize>>,
    case: Id,
    artifact: Id,
    blob: PathBuf,
    size: u64,
    observation: Id,
    commands: usize,
}

impl Flow {
    fn new(size: u64) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        Workspace::init(&root, false).unwrap();
        let config = root.join("investigation.toml");
        let contents = fs::read_to_string(&config).unwrap();
        fs::write(
            &config,
            contents.replace("import_roots = []", "import_roots = [\".\"]"),
        )
        .unwrap();
        sparse(&root.join("capture.bin"), size);
        let workspace = Workspace::open(&root).unwrap();
        let reads = Rc::new(Cell::new(0));
        let mut store = Store {
            workspace: workspace.clone(),
            fs: Probing::new(workspace.root(), reads.clone()),
            gate: WriteGate::isolated(),
            catalog: standard_catalog(),
        };
        let manifest = json!({
            "schema_version": "inv.ingest.v1",
            "command_id": "ingest",
            "actor": ACTOR,
            "new_case": {"title": "Large capture"},
            "source": {"title": "Generated capture"},
            "paths": ["capture.bin"],
        });
        let receipt = store
            .ingest(&serde_json::to_vec(&manifest).unwrap(), None)
            .unwrap();
        let case = receipt
            .receipt
            .created
            .iter()
            .find(|change| change.id.kind() == Kind::Case)
            .unwrap()
            .id;
        let item = &receipt.items[0];
        assert_eq!(item.size_bytes, size);
        let digest = item.digest.to_string();
        let hex = digest.trim_start_matches("sha256:");
        let blob = workspace
            .root()
            .join("state/blobs/sha256")
            .join(&hex[..2])
            .join(hex);
        let mut flow = Self {
            _temporary: temporary,
            workspace,
            store,
            reads,
            case,
            artifact: item.artifact_id,
            blob,
            size,
            observation: case,
            commands: 0,
        };
        let artifact = flow.cur(flow.artifact);
        flow.observation = flow.must(vec![op(
            "observation.add",
            json!({
                "value":{"kind":"measurement","value":"18","unit":"ms"},
                "basis":{"kind":"source","artifact_ref":artifact,
                    "selector":{"schema_version":"inv.selector.v1","artifact_digest":digest,"selector":{"type":"whole"}},
                    "text_quote":null},
                "extraction_method":"p99 over the retained samples","validity":"valid","quality":[],
                "limitations":[],"evidence_kind":"observational","variables_changed":[]
            }),
        )])
        .created[0]
            .id;
        flow
    }

    /// Replace the filesystem hooks.
    fn probe(&mut self, hooks: impl FnOnce(Probing) -> Probing) {
        self.store.fs = hooks(Probing::new(self.workspace.root(), self.reads.clone()));
    }

    fn cur(&mut self, id: Id) -> RevisionRef {
        self.store
            .case(self.case)
            .unwrap()
            .state
            .current(id)
            .unwrap()
            .reference()
    }

    /// Record one command. A gate that asks for preconditions is retried once
    /// with exactly the revisions it names.
    fn submit(&mut self, ops: &[Value]) -> Result<Receipt, Failure> {
        let mut command = json!({
            "schema_version":"inv.command.v1","case_id":self.case,"actor":ACTOR,
            "preconditions":[],"operations":ops,
        });
        for attempt in 0..2 {
            self.commands += 1;
            command["command_id"] = json!(format!("step-{}", self.commands));
            match self.store.record(&serde_json::to_vec(&command).unwrap()) {
                Err(failure)
                    if attempt == 0
                        && failure
                            .error
                            .message
                            .contains("as a transaction precondition") =>
                {
                    command["preconditions"] = failure.error.expected.clone().unwrap_or_default();
                }
                other => return other,
            }
        }
        unreachable!()
    }

    fn must(&mut self, ops: Vec<Value>) -> Receipt {
        self.submit(&ops)
            .unwrap_or_else(|failure| panic!("rejected: {:?}\n{ops:#?}", failure.error))
    }

    fn validation_review(&mut self, subject: RevisionRef) -> Value {
        let replay = self.store.case(self.case).unwrap();
        let record = replay.state.revision(subject).unwrap();
        let digest = basis_digest(record).unwrap();
        let reads = required_read_set(&replay.state, record).unwrap();
        op(
            "review.submit",
            json!({
                "kind":"validation","subject_ref":subject,"basis_digest":digest,"read_set":reads,
                "method":"Inspect the cited measurements and sources.","materials_examined":[],
                "shared_dependencies":[],"objection_refs":[],"verdict":"supports","caveats":["Generated."],
                "context_isolation":"fresh_context_unblinded","context":{
                    "saw_original_conclusion":{"state":"known","value":true},
                    "gathered_new_evidence":{"state":"known","value":false},
                    "method_or_source_changes":[],"activity":"Read the recorded basis.",
                    "dependencies":"shared","dependence_basis":"Same recorded data."}
            }),
        )
    }

    /// A conclusion supported by the observation.
    fn conclusion(&mut self) -> Id {
        let observation = self.cur(self.observation);
        self.must(vec![op(
            "conclusion.create",
            json!({
                "statement":"The managed queue met the latency limit in the benchmark.",
                "scope":"The recorded benchmark.","significance":"Informs the queue choice.",
                "support_refs":[observation],"contradiction_refs":[],"caveats":["Generated."]
            }),
        )])
        .created[0]
            .id
    }

    fn request_review(&mut self, conclusion: Id) -> Result<Receipt, Failure> {
        let subject = self.cur(conclusion);
        self.submit(&[op(
            "conclusion.request_review",
            json!({"conclusion_ref": subject}),
        )])
    }

    fn replace_blob(&self) {
        replace(&self.blob, self.size);
    }

    /// Hash reads the whole blob takes.
    fn whole_reads(&self) -> usize {
        (self.size / BUFFER) as usize
    }
}

/// Each promotion step, from requesting review of a conclusion to accepting
/// a decision, hashes its retained evidence: the whole 32 MiB blob is read,
/// every read finds the commit lock free, and another writer commits in the
/// middle of the hash.
#[test]
fn another_writer_commits_while_a_promotion_hashes_its_evidence() {
    let mut flow = Flow::new(32 * MIB);
    let observation = flow.cur(flow.observation);
    let question = flow
        .must(vec![op(
            "question.create",
            json!({"importance":"normal","wording":"Which queue should carry the jobs?"}),
        )])
        .created[0]
        .id;
    let q = flow.cur(question);
    let options: Vec<Id> = flow
        .must(
            ["Managed queue", "Broker"]
                .into_iter()
                .map(|description| {
                    op(
                        "option.create",
                        json!({"question_ref":q,"description":description,"dependencies":[],"tradeoff_notes":[]}),
                    )
                })
                .collect(),
        )
        .created
        .iter()
        .map(|change| change.id)
        .collect();
    let dispositions = options
        .iter()
        .map(|option| {
            op(
                "option.disposition",
                json!({"option_ref":flow.cur(*option),"lifecycle":"eligible","reason":"Feasible"}),
            )
        })
        .collect();
    flow.must(dispositions);
    let criterion = flow.must(vec![op(
        "criterion.create",
        json!({
            "name":"Latency","kind":"hard","preferred_direction":"lower",
            "scale":{"anchors":[],"description":"p99 latency","unit":"ms"},
            "scope":{"kind":"all_options","question_ref":q},
            "threshold":{"kind":"measured","operator":"at_most","value":{"decimal":"25","unit":"ms"}}
        }),
    )])
    .created[0]
        .id;
    let criterion = flow.cur(criterion);
    let comparisons = options
        .iter()
        .map(|option| {
            op(
                "comparison.add",
                json!({
                    "option_ref":flow.cur(*option),"criterion_ref":criterion,
                    "outcome":"meets","method":"Benchmark","value":{"decimal":"18","unit":"ms"},
                    "evidence_refs":[observation],"assumption_refs":[],
                    "uncertainty":"Few runs.","rationale":"Measured p99 is below the limit."
                }),
            )
        })
        .collect();
    let comparisons: Vec<Id> = flow
        .must(comparisons)
        .created
        .iter()
        .map(|change| change.id)
        .collect();
    let conclusion = flow.conclusion();

    // Each armed hash commits one case elsewhere, through its own store, on
    // its first read.
    let armed = Rc::new(Cell::new(false));
    let elsewhere = Rc::new(Cell::new(0));
    let workspace = flow.workspace.clone();
    let (hook_armed, hook_elsewhere) = (armed.clone(), elsewhere.clone());
    flow.probe(|probing| {
        probing.on_read(move || {
            if !hook_armed.replace(false) {
                return;
            }
            let command = json!({
                "schema_version": "inv.command.v1",
                "command_id": format!("concurrent-{}", hook_elsewhere.get()),
                "actor": "agent:another-writer", "case_id": null, "preconditions": [],
                "operations": [{"op": "case.create",
                    "payload": {"title": "Concurrent work", "mode": "explore"}}],
            });
            hook_elsewhere.set(hook_elsewhere.get() + 1);
            Store::open(workspace.clone())
                .record(&serde_json::to_vec(&command).unwrap())
                .unwrap();
        })
    });
    let promote = |flow: &mut Flow, what: &str, step: &dyn Fn(&mut Flow) -> Value| {
        let operation = step(flow);
        let before = flow.reads.get();
        armed.set(true);
        let receipt = flow.must(vec![operation]);
        assert!(!armed.get(), "{what}: no hash ran");
        assert!(
            flow.reads.get() - before > flow.whole_reads(),
            "{what}: {} reads",
            flow.reads.get() - before
        );
        receipt
    };

    let subject = flow.cur(conclusion);
    promote(&mut flow, "conclusion.request_review", &|_| {
        op(
            "conclusion.request_review",
            json!({"conclusion_ref": subject}),
        )
    });
    let subject = flow.cur(conclusion);
    let review = promote(&mut flow, "conclusion review", &|flow| {
        flow.validation_review(subject)
    })
    .created[0]
        .id;
    let review = flow.cur(review);
    let subject = flow.cur(conclusion);
    promote(&mut flow, "conclusion.validate", &|_| {
        op(
            "conclusion.validate",
            json!({"conclusion_ref": subject, "review_ref": review}),
        )
    });
    let chosen = flow.cur(options[0]);
    let compared: Vec<_> = vec![flow.cur(comparisons[0])];
    let decision = flow.must(vec![op("decision.propose", json!({
        "question_ref":q,"choice":{"kind":"select","option_ref":chosen},
        "compared_alternatives":[chosen],"comparison_refs":compared,"hard_conditions":[],
        "evidence_refs":[],"assumption_refs":[],"tradeoffs":[],"consequences":["Adopt the managed queue."],
        "reopen_trigger":"Latency evidence changes.","reversal_conditions":[]
    }))])
    .created[0]
        .id;
    let subject = flow.cur(decision);
    let review = promote(&mut flow, "decision review", &|flow| {
        flow.validation_review(subject)
    })
    .created[0]
        .id;
    let review = flow.cur(review);
    let subject = flow.cur(decision);
    promote(&mut flow, "decision.accept", &|_| {
        op(
            "decision.accept",
            json!({"decision_ref": subject, "review_ref": review}),
        )
    });

    let state = flow.store.case(flow.case).unwrap().state;
    assert_eq!(
        state.current(conclusion).unwrap().body["status"],
        "validated"
    );
    assert_eq!(
        state.current(decision).unwrap().body["lifecycle"],
        "accepted"
    );
    assert_eq!(elsewhere.get(), 5);
    let lock = flow.workspace.root().join(COMMIT_LOCK);
    assert!(CommitLock::acquire(&lock, Duration::ZERO).is_ok());
    let mut store = Store::open(flow.workspace.clone());
    assert_eq!(store.list_cases().unwrap().len(), 6);
    assert!(store.check().unwrap().integrity_ok());
}

/// A blob whose bytes no longer match its digest fails the promotion before
/// anything is written, as it did when the hash ran under the lock.
#[test]
fn a_tampered_blob_fails_the_promotion_with_an_integrity_failure() {
    let mut flow = Flow::new(MIB);
    let conclusion = flow.conclusion();
    let head = flow.store.case(flow.case).unwrap().snapshot;
    flow.replace_blob();
    let failure = flow.request_review(conclusion).unwrap_err();
    assert_eq!(failure.error.code, ErrorCode::IntegrityFailure);
    assert_eq!(failure.commit_status, CommitStatus::NotCommitted);
    assert_eq!(
        failure.error.actual.unwrap()["unavailable_artifacts"][0]["id"],
        json!(flow.artifact)
    );
    assert_eq!(flow.store.case(flow.case).unwrap().snapshot, head);
}

/// The blob is intact when it is hashed and replaced, with the same size and
/// modification time, once the lock is taken again. Its identity changed, so
/// the attempt hashes it again outside the lock, finds the forgery and fails.
#[test]
fn a_blob_replaced_between_its_hash_and_the_lock_fails_the_promotion() {
    let mut flow = Flow::new(MIB);
    let conclusion = flow.conclusion();
    let head = flow.store.case(flow.case).unwrap().snapshot;
    let baseline = flow.reads.get();
    let tampered = Rc::new(Cell::new(false));
    let (reads, blob, size) = (flow.reads.clone(), flow.blob.clone(), flow.size);
    let hook_tampered = tampered.clone();
    flow.probe(|probing| {
        probing.on_lock(move || {
            // The first hash of the blob has run: forge it now.
            if reads.get() > baseline && !hook_tampered.replace(true) {
                replace(&blob, size);
            }
        })
    });
    let failure = flow.request_review(conclusion).unwrap_err();
    assert!(tampered.get());
    assert_eq!(failure.error.code, ErrorCode::IntegrityFailure);
    assert_eq!(failure.commit_status, CommitStatus::NotCommitted);
    assert_eq!(
        failure.error.actual.unwrap()["unavailable_artifacts"][0]["id"],
        json!(flow.artifact)
    );
    // Hashed twice, both times without the lock.
    assert!(flow.reads.get() - baseline > 2 * flow.whole_reads());
    assert_eq!(flow.store.case(flow.case).unwrap().snapshot, head);
}

/// The debug log names the hash as its own stage between two locked
/// attempts, without paths or request text.
#[test]
fn a_deferred_promotion_logs_the_verify_artifacts_stage() {
    let mut flow = Flow::new(MIB);
    let conclusion = flow.conclusion();
    let mut result = None;
    let log = diag_tests::capture(tracing::Level::DEBUG, || {
        result = Some(flow.request_review(conclusion));
    });
    result.unwrap().unwrap();
    // The first submission only learned its preconditions.
    let command = format!("step-{}", flow.commands);
    let log: String = log
        .lines()
        .filter(|line| diag_tests::field(line, "command_id") == Some(command.as_str()))
        .map(|line| format!("{line}\n"))
        .collect();
    let order: Vec<_> = log
        .lines()
        .filter_map(|line| diag_tests::field(line, "stage"))
        .collect();
    let verify = order
        .iter()
        .position(|stage| *stage == "verify_artifacts")
        .unwrap_or_else(|| panic!("{order:?}"));
    assert!(order[..verify].contains(&"validate"), "{order:?}");
    assert!(order[verify..].contains(&"validate"), "{order:?}");
    assert!(order[verify..].contains(&"journal_publish"), "{order:?}");
    assert_eq!(log.matches("commit complete").count(), 1, "{log}");
    assert!(!log.contains("commit failed"), "{log}");
    assert!(!log.contains("latency limit"), "request text leaked: {log}");
    let root = flow.workspace.root().display().to_string();
    assert!(!log.contains(&root), "path leaked: {log}");
}

/// Deferral is bounded: after [`DEFERRED_ATTEMPTS`] attempts the next one
/// hashes under the lock, so a command always finishes.
#[test]
fn deferral_is_bounded() {
    let mut promotion = PromotionBlobs::default();
    for _ in 0..DEFERRED_ATTEMPTS {
        promotion.next_attempt();
        assert!(promotion.defer);
    }
    promotion.next_attempt();
    assert!(!promotion.defer);
    assert!(!PromotionBlobs::under_lock().defer);
}
