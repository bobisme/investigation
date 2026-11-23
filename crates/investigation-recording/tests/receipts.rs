//! Executor receipts at `capture.finalize`: structure checks recorded on the
//! Run, and the chronology rule. Only a structure-checked in-toto receipt
//! whose subject is the frozen plan digest makes a Run `executor_bound`; a
//! mismatching subject or a timestamp never does.

use std::{
    fs,
    path::{Path, PathBuf},
};

use investigation_recording::{
    ArtifactRole, BeginCapture, CaptureCompletion, CaptureIntent, CaseTarget, ExecutionReport,
    ExecutionStatus, ExternalReceipt, ExternalRunId, FinalizeCapture, FinalizeReceipt, Id,
    IdempotencyKey, LocalRecorder, Position, ProducerIdentity, ProducerName, ReceiptFormat,
    Recorder, RevisionRef, StableArtifact,
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const TOKEN: &[u8] =
    include_bytes!("../../investigation-core/src/receipt_check/testdata/timestamp-token.der");

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    outputs: PathBuf,
    recorder: LocalRecorder,
    case_id: Id,
    plan: RevisionRef,
}

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).unwrap()
}

fn store(root: &Path) -> Store {
    Store::open(Workspace::open(root).unwrap())
}

fn begin(name: &str, case: CaseTarget) -> BeginCapture {
    BeginCapture::new(
        key(&format!("{name}/begin")),
        "agent:example-executor".parse().unwrap(),
        ProducerIdentity::new(ProducerName::new("example-executor").unwrap()),
        ExternalRunId::new(name).unwrap(),
        CaptureIntent::new("planned run"),
        case,
    )
}

/// A workspace with one case holding a frozen inquiry plan.
fn fixture() -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let outputs = temporary.path().join("outputs");
    fs::create_dir(&outputs).unwrap();
    Workspace::init(&root, false).unwrap();
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        contents.replace(
            "import_roots = []",
            &format!("import_roots = [{:?}]", outputs.display().to_string()),
        ),
    )
    .unwrap();
    let recorder = LocalRecorder::open(&root).unwrap();
    let first = recorder
        .begin(&begin(
            "setup",
            CaseTarget::NewRecordCase {
                title: "Planned runs".into(),
            },
        ))
        .unwrap();
    let submit = |command_id: &str, op: &str, payload: Value| {
        let command = json!({
            "schema_version": "inv.command.v1", "command_id": command_id,
            "actor": "agent:planner", "case_id": first.case_id, "preconditions": [],
            "operations": [{"op": op, "payload": payload}],
        });
        store(&root)
            .record(&serde_json::to_vec(&command).unwrap())
            .unwrap()
    };
    let example = investigation_core::catalog::standard_catalog()
        .get("inquiry.create")
        .unwrap()
        .example()
        .unwrap()
        .clone();
    let draft = submit("plan", "inquiry.create", example).created[0].id;
    let frozen = submit(
        "freeze",
        "inquiry.freeze",
        json!({"inquiry_ref": {"id": draft, "revision": 1}}),
    )
    .updated[0]
        .clone();
    Fixture {
        _temporary: temporary,
        root,
        outputs,
        recorder,
        case_id: first.case_id,
        plan: RevisionRef {
            id: frozen.id,
            revision: frozen.revision,
        },
    }
}

fn encode64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, byte)| n | u32::from(*byte) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
        for _ in chunk.len()..3 {
            out.push('=');
        }
    }
    out
}

/// A DSSE envelope over an in-toto Statement whose subject is `digest`.
/// The signature is a placeholder: no signature is verified.
fn statement_receipt(digest: &str) -> Vec<u8> {
    let statement = json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{"name": "inquiry-plan", "digest": {"sha256": digest.strip_prefix("sha256:").unwrap()}}],
        "predicateType": "https://example.org/executor-receipt/v1",
        "predicate": {"bound": "before-execution"},
    });
    serde_json::to_vec(&json!({
        "payloadType": "application/vnd.in-toto+json",
        "payload": encode64(&serde_json::to_vec(&statement).unwrap()),
        "signatures": [{"keyid": "executor", "sig": encode64(b"placeholder")}],
    }))
    .unwrap()
}

impl Fixture {
    /// Capture one planned Run whose receipts are `receipts` (format, bytes),
    /// and return the finalize receipt and the Run body.
    fn capture(
        &self,
        name: &str,
        receipts: &[(ReceiptFormat, Vec<u8>)],
    ) -> (FinalizeReceipt, Value) {
        let mut request = begin(
            name,
            CaseTarget::Existing {
                case_id: self.case_id,
            },
        );
        request.intent.inquiry_plan = Some(self.plan);
        let handle = self.recorder.begin(&request).unwrap().handle;
        let mut summary = FinalizeCapture::new(
            handle,
            key(&format!("{name}/finalize")),
            Position::new(receipts.len() as u64),
            CaptureCompletion::Complete,
            ExecutionReport::new(ExecutionStatus::Completed),
        );
        for (index, (format, bytes)) in receipts.iter().enumerate() {
            let item = format!("receipt-{index}");
            let file = format!("{name}-{item}");
            fs::write(self.outputs.join(&file), bytes).unwrap();
            self.recorder
                .attach(&StableArtifact::file(
                    handle,
                    key(&item),
                    Position::new(index as u64 + 1).unwrap(),
                    ArtifactRole::Receipt,
                    file,
                ))
                .unwrap();
            let mut receipt = ExternalReceipt::new("executor-receipt");
            receipt.format = Some(*format);
            receipt.artifact = Some(key(&item));
            summary.receipts.push(receipt);
        }
        let finalized = self.recorder.finalize(&summary).unwrap();
        let case = store(&self.root).case(self.case_id).unwrap();
        let body = case.state.current(finalized.run_id).unwrap().body.clone();
        (finalized, body)
    }

    fn plan_digest(&self) -> String {
        let case = store(&self.root).case(self.case_id).unwrap();
        case.state.current(self.plan.id).unwrap().body["plan_digest"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

#[test]
fn a_statement_over_the_frozen_plan_makes_the_run_executor_bound() {
    let fixture = fixture();
    let plan = fixture.plan_digest();
    let (finalized, run) = fixture.capture(
        "bound",
        &[(ReceiptFormat::InTotoDsse, statement_receipt(&plan))],
    );
    assert!(
        finalized.commit.warnings.is_empty(),
        "{:?}",
        finalized.commit.warnings
    );
    assert_eq!(run["plan_digest"], plan.as_str());
    assert_eq!(run["chronology"], "executor_bound");
    assert_eq!(run["chronology_verification"], "structure_checked");
    let check = &run["receipt_checks"][0];
    assert_eq!(check["verification"], "structure_checked");
    assert_eq!(check["binding"], "plan_digest_bound");
    assert_eq!(check["chronology_effect"], "executor_bound");
    assert_eq!(check["signatures_present"], 1);
    // The receipt bytes are an Artifact of the Run.
    let artifact = check["artifact_ref"].clone();
    assert!(
        run["artifact_refs"].as_array().unwrap().contains(&artifact),
        "{run}"
    );
}

#[test]
fn a_mismatching_subject_or_a_timestamp_never_upgrades_chronology() {
    let fixture = fixture();
    let other = format!("sha256:{}", "a".repeat(64));
    let (finalized, run) = fixture.capture(
        "mismatch",
        &[
            (ReceiptFormat::InTotoDsse, statement_receipt(&other)),
            (ReceiptFormat::Rfc3161, TOKEN.to_vec()),
            (ReceiptFormat::InTotoDsse, b"{\"payload\":\"\"}".to_vec()),
        ],
    );
    assert_eq!(run["chronology"], "recorded_before_run_declaration");
    assert!(run.get("chronology_verification").is_none());
    let checks = run["receipt_checks"].as_array().unwrap();
    assert_eq!(checks[0]["binding"], "plan_digest_not_bound");
    assert_eq!(checks[1]["verification"], "structure_checked");
    assert_eq!(checks[1]["binding"], "plan_digest_not_bound");
    assert_eq!(checks[2]["verification"], "structure_invalid");
    for check in checks {
        assert_eq!(check["chronology_effect"], "none");
    }
    let warnings = &finalized.commit.warnings;
    assert!(
        warnings.iter().any(|w| w.starts_with("Receipt 0")),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(|w| w.starts_with("Receipt 2")),
        "{warnings:?}"
    );
}

#[test]
fn a_receipt_without_a_declared_format_is_retained_and_not_parsed() {
    let fixture = fixture();
    let plan = fixture.plan_digest();
    let mut request = begin(
        "undeclared",
        CaseTarget::Existing {
            case_id: fixture.case_id,
        },
    );
    request.intent.inquiry_plan = Some(fixture.plan);
    let handle = fixture.recorder.begin(&request).unwrap().handle;
    fs::write(fixture.outputs.join("statement"), statement_receipt(&plan)).unwrap();
    fixture
        .recorder
        .attach(&StableArtifact::file(
            handle,
            key("statement"),
            Position::new(1).unwrap(),
            ArtifactRole::Receipt,
            "statement",
        ))
        .unwrap();
    let mut summary = FinalizeCapture::new(
        handle,
        key("undeclared/finalize"),
        Position::new(1),
        CaptureCompletion::Complete,
        ExecutionReport::new(ExecutionStatus::Completed),
    );
    let mut receipt = ExternalReceipt::new("executor-receipt");
    receipt.artifact = Some(key("statement"));
    summary.receipts.push(receipt);
    let finalized = fixture.recorder.finalize(&summary).unwrap();
    let case = store(&fixture.root).case(fixture.case_id).unwrap();
    let run = &case.state.current(finalized.run_id).unwrap().body;
    assert_eq!(run["chronology"], "recorded_before_run_declaration");
    assert_eq!(run["receipt_checks"][0]["verification"], "not_checked");
    assert!(run["receipt_checks"][0].get("format").is_none());
}
