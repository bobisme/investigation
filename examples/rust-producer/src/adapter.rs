//! Argument parsing, the synthetic-run scenario and the call log.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use investigation_recording::{
    ArtifactRole, BeginCapture, CaptureCompletion, CaptureEvent, CaptureHandle, CaptureIntent,
    CaseTarget, ChannelEvent, ChannelInventory, CommitStatus, EvidenceCompleteness,
    ExecutionReport, ExecutionStatus, ExternalReceipt, ExternalRunId, FinalizeCapture,
    FinalizeReceipt, IdempotencyKey, ItemReceipt, LifecycleFact, LocalRecorder, Metadata, Position,
    ProducerAssertion, ProducerControl, ProducerIdentity, ProducerName, Recorder, RecordingError,
    RecoverScope, StableArtifact,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const SCHEMA: &str = "inv.producer-adapter.v1";
const SCENARIOS: [&str; 4] = [
    "synthetic-run",
    "partial-evidence",
    "interrupted-run",
    "late-item",
];
const CRASH_STATUS: u8 = 75;
/// The exact bytes the synthetic action fabricates.
const RESULT: &str = "{\"metric\":\"latency.p99\",\"value\":18.7,\"unit\":\"ms\"}\n";

pub fn main(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("describe") if args.len() == 1 => {
            println!(
                "{}",
                json!({
                    "schema_version": SCHEMA,
                    "protocols": ["inv.capture.v1"],
                    "scenarios": SCENARIOS,
                    "faults": ["drop-response", "crash-after"],
                })
            );
            ExitCode::SUCCESS
        }
        Some(mode @ ("run" | "resume")) => match Options::parse(mode == "resume", &args[1..]) {
            Ok(options) => execute(&options),
            Err(message) => usage(&message),
        },
        _ => usage("expected `describe`, `run` or `resume`"),
    }
}

fn usage(message: &str) -> ExitCode {
    eprintln!("usage error: {message}");
    eprintln!(
        "usage: example-rust-producer describe | run|resume --workspace DIR --output-dir OUT \
         --scenario NAME --producer NAME --external-run-id ID [--protocol VERSION] [--fault KIND:OP:N]..."
    );
    ExitCode::from(2)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    DropResponse,
    CrashAfter,
}

struct Fault {
    kind: Kind,
    operation: &'static str,
    nth: u64,
}

struct Options {
    resume: bool,
    scenario: &'static str,
    protocol: Option<String>,
    workspace: PathBuf,
    output_dir: PathBuf,
    /// Checked against the producer name rule when `begin` is built, so a
    /// refused name is a rejected `begin`, not a usage error.
    producer: String,
    external_run_id: ExternalRunId,
    faults: Vec<Fault>,
}

impl Options {
    fn parse(resume: bool, args: &[String]) -> Result<Self, String> {
        let (mut workspace, mut output_dir, mut scenario) = (None, None, None);
        let (mut producer, mut external_run_id) = (None, None);
        let (mut faults, mut protocol) = (Vec::new(), None);
        let mut iter = args.iter();
        while let Some(flag) = iter.next() {
            let value = iter
                .next()
                .ok_or_else(|| format!("{flag} needs a value"))?
                .clone();
            match flag.as_str() {
                "--workspace" => workspace = Some(PathBuf::from(value)),
                "--output-dir" => output_dir = Some(PathBuf::from(value)),
                "--scenario" => scenario = Some(value),
                "--producer" => producer = Some(value),
                "--external-run-id" => external_run_id = Some(value),
                "--protocol" => protocol = Some(value),
                "--fault" if !resume => faults.push(parse_fault(&value)?),
                other => return Err(format!("unknown argument {other}")),
            }
        }
        let need = |name: &str| format!("missing {name}");
        let scenario = scenario.ok_or_else(|| need("--scenario"))?;
        let Some(scenario) = SCENARIOS.iter().copied().find(|name| *name == scenario) else {
            return Err(format!("unknown scenario {scenario}"));
        };
        let absolute = |path: PathBuf| std::path::absolute(&path).map_err(|e| e.to_string());
        Ok(Self {
            resume,
            scenario,
            protocol,
            workspace: absolute(workspace.ok_or_else(|| need("--workspace"))?)?,
            output_dir: absolute(output_dir.ok_or_else(|| need("--output-dir"))?)?,
            producer: producer.ok_or_else(|| need("--producer"))?,
            external_run_id: ExternalRunId::new(
                external_run_id.ok_or_else(|| need("--external-run-id"))?,
            )
            .map_err(|e| e.to_string())?,
            faults,
        })
    }
}

fn parse_fault(text: &str) -> Result<Fault, String> {
    let mut parts = text.split(':');
    let kind = match parts.next() {
        Some("drop-response") => Kind::DropResponse,
        Some("crash-after") => Kind::CrashAfter,
        _ => return Err(format!("unknown fault {text}")),
    };
    let operation = match parts.next() {
        Some("begin") => "begin",
        Some("append") => "append",
        Some("attach") => "attach",
        Some("finalize") => "finalize",
        _ => return Err(format!("unknown operation in fault {text}")),
    };
    let nth = parts
        .next()
        .and_then(|n| n.parse::<u64>().ok())
        .filter(|n| *n >= 1)
        .ok_or_else(|| format!("fault {text} needs a call number of at least 1"))?;
    if parts.next().is_some() {
        return Err(format!("malformed fault {text}"));
    }
    Ok(Fault {
        kind,
        operation,
        nth,
    })
}

/// How a scenario stopped before finalizing.
enum Stop {
    Crash,
    Rejected,
}

/// Records every call as sent and applies injected faults.
struct Driver<'a> {
    recorder: &'a LocalRecorder,
    faults: &'a [Fault],
    counts: BTreeMap<&'static str, u64>,
    calls: Vec<Value>,
}

impl Driver<'_> {
    /// One recorder call under the producer retry rule: an ambiguous failure
    /// (commit status `unknown`) resends the identical request with the same key.
    /// `drop-response` forces that same resend after a success.
    fn call<T>(
        &mut self,
        operation: &'static str,
        key: &str,
        position: Option<u64>,
        send: impl Fn(&LocalRecorder) -> Result<T, RecordingError>,
        result_of: impl Fn(&T) -> Value,
    ) -> Result<T, Stop> {
        let n = {
            let count = self.counts.entry(operation).or_insert(0);
            *count += 1;
            *count
        };
        let matching = |kind| {
            self.faults
                .iter()
                .any(|f| f.kind == kind && f.operation == operation && f.nth == n)
        };
        let drop = matching(Kind::DropResponse);
        let crash = matching(Kind::CrashAfter);

        let mut outcome = self.once(operation, key, position, &send, &result_of);
        let ambiguous = matches!(&outcome, Err(e) if e.commit_status() == CommitStatus::Unknown);
        if drop || ambiguous {
            // The response is discarded; resend the identical request.
            outcome = self.once(operation, key, position, &send, &result_of);
        }
        let value = outcome.map_err(|error| {
            eprintln!("{operation} rejected: {error}");
            Stop::Rejected
        })?;
        if crash {
            return Err(Stop::Crash);
        }
        Ok(value)
    }

    fn once<T>(
        &mut self,
        operation: &'static str,
        key: &str,
        position: Option<u64>,
        send: &impl Fn(&LocalRecorder) -> Result<T, RecordingError>,
        result_of: &impl Fn(&T) -> Value,
    ) -> Result<T, RecordingError> {
        let outcome = send(self.recorder);
        let (result, error_code) = match &outcome {
            Ok(value) => (result_of(value), Value::Null),
            Err(error) => (
                json!("rejected"),
                error
                    .code()
                    .and_then(|code| serde_json::to_value(code).ok())
                    .unwrap_or(Value::Null),
            ),
        };
        self.calls.push(json!({
            "operation": format!("capture.{operation}"),
            "key": key,
            "position": position,
            "result": result,
            "error_code": error_code,
        }));
        outcome
    }
}

fn execute(options: &Options) -> ExitCode {
    let recorder = match LocalRecorder::open(&options.workspace) {
        Ok(recorder) => recorder,
        Err(error) => {
            eprintln!("cannot open workspace: {error}");
            return ExitCode::from(2);
        }
    };
    let mut driver = Driver {
        recorder: &recorder,
        faults: &options.faults,
        counts: BTreeMap::new(),
        calls: Vec::new(),
    };
    let ids = RefCell::new((None, None));
    let outcome = synthetic_run(options, &mut driver, &ids);
    let ids = ids.into_inner();
    let (outcome_name, finalize, code) = match outcome {
        Ok(receipt) => ("finalized", serde_json::to_value(receipt).ok(), 0),
        Err(Stop::Crash) => ("crashed", None, CRASH_STATUS),
        Err(Stop::Rejected) => ("rejected", None, 1),
    };
    println!(
        "{}",
        json!({
            "schema_version": SCHEMA,
            "scenario": options.scenario,
            "external_run_id": options.external_run_id.as_str(),
            "outcome": outcome_name,
            "run_id": ids.0,
            "handle": ids.1,
            "calls": driver.calls,
            "finalize": finalize,
        })
    );
    ExitCode::from(code)
}

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).expect("fixed keys are valid")
}

fn at(step: u64) -> Position {
    Position::new(step).expect("positions start at 1")
}

fn commit_result(receipt: &investigation_recording::CommitReceipt) -> Value {
    serde_json::to_value(receipt.result).unwrap_or(Value::Null)
}

fn item_result(receipt: &ItemReceipt) -> Value {
    serde_json::to_value(receipt.result).unwrap_or(Value::Null)
}

/// Write the synthetic action's output atomically, so a file that exists is
/// complete. The action is pure, so writing it again after a crash is safe.
///
/// Returns `None` when the file holds bytes this action did not write. The
/// producer cannot vouch for them and never overwrites them.
fn fabricate(output_dir: &Path) -> Result<Option<PathBuf>, Stop> {
    let target = output_dir.join("result.json");
    let io = |error: std::io::Error| {
        eprintln!("cannot write {}: {error}", target.display());
        Stop::Rejected
    };
    if let Ok(existing) = fs::read_to_string(&target) {
        if existing == RESULT {
            return Ok(Some(target));
        }
        eprintln!("{} holds unexpected bytes", target.display());
        return Ok(None);
    }
    let temporary = output_dir.join("result.json.part");
    fs::write(&temporary, RESULT).map_err(io)?;
    fs::rename(&temporary, &target).map_err(io)?;
    Ok(Some(target))
}

/// One request of a scenario, at the position it takes.
enum Step {
    Append(CaptureEvent),
    Attach(StableArtifact),
}

fn host_metadata() -> Metadata {
    Metadata::from([("example-org.host".to_owned(), json!({"slot": 3}))])
}

fn fixture_metadata() -> Metadata {
    Metadata::from([("example-org.fixture".to_owned(), json!({"seed": 7}))])
}

fn phase(handle: CaptureHandle, name: &str, position: u64, phase: &str) -> Step {
    Step::Append(CaptureEvent::new(
        handle,
        key(name),
        at(position),
        LifecycleFact::PhaseChanged {
            phase: phase.into(),
        },
    ))
}

fn started(handle: CaptureHandle) -> CaptureEvent {
    CaptureEvent::new(handle, key("started"), at(1), LifecycleFact::Started)
}

fn result_artifact(handle: CaptureHandle, path: PathBuf) -> StableArtifact {
    let mut result = StableArtifact::file(
        handle,
        key("result"),
        at(2),
        ArtifactRole::OriginalOutput,
        path,
    );
    result.media_type = Some("application/json".into());
    result
}

/// The requests of a scenario before finalization, in position order.
fn steps(scenario: &str, handle: CaptureHandle, result: &Path) -> Vec<Step> {
    let result_path = result.join("result.json");
    match scenario {
        "partial-evidence" => {
            let mut first = started(handle);
            first.metadata = host_metadata();
            let mut original = result_artifact(handle, result_path);
            original.channel = Some("results".into());
            original.expected_digest = Some(
                format!("sha256:{:x}", Sha256::digest(RESULT.as_bytes()))
                    .parse()
                    .expect("a sha256 digest"),
            );
            original.expected_size_bytes = Some(RESULT.len() as u64);
            original.metadata = host_metadata();
            let mut trace = StableArtifact::missing(
                handle,
                key("trace"),
                at(3),
                ArtifactRole::Log,
                "trace collector exited before flush",
            );
            trace.channel = Some("trace".into());
            let gap = CaptureEvent::new(
                handle,
                key("trace-gap"),
                at(4),
                LifecycleFact::Channel {
                    channel: "trace".into(),
                    state: ChannelEvent::Gap {
                        detail: Some("collector restarted".into()),
                    },
                },
            );
            vec![
                Step::Append(first),
                Step::Attach(original),
                Step::Attach(trace),
                Step::Append(gap),
            ]
        }
        "interrupted-run" => vec![
            Step::Append(started(handle)),
            Step::Attach(result_artifact(handle, result_path)),
        ],
        // synthetic-run and late-item
        _ => vec![
            Step::Append(started(handle)),
            Step::Attach(result_artifact(handle, result_path)),
            phase(handle, "phase-summarize", 3, "summarize"),
        ],
    }
}

fn metric() -> ProducerAssertion {
    let mut metric = ProducerAssertion::new("reported_metric", "latency.p99", json!(18.7));
    metric.unit = Some("ms".into());
    metric
}

/// The finalize request of a scenario that ran every step.
fn finalize_request(scenario: &str, handle: CaptureHandle, id: &str) -> FinalizeCapture {
    let finalize_key = key(&format!("{id}/finalize"));
    let mut completed = ExecutionReport::new(ExecutionStatus::Completed);
    completed.exit_code = Some(0);
    match scenario {
        "partial-evidence" => {
            let mut summary = FinalizeCapture::new(
                handle,
                finalize_key,
                Some(at(4)),
                CaptureCompletion::Complete,
                completed,
            );
            summary.channels = vec![
                ChannelInventory::new("results", EvidenceCompleteness::Complete),
                ChannelInventory::new("trace", EvidenceCompleteness::Complete),
            ];
            summary.declared_artifacts = vec![key("result"), key("trace"), key("summary")];
            summary.assertions.push(metric());
            summary.controls = vec![
                ProducerControl::new("steady-load", "reported_pass"),
                ProducerControl::new("warm-cache", "not_run"),
            ];
            let mut statement = ExternalReceipt::new("signed-statement");
            statement.reference = Some("urn:example:statement:1".into());
            summary.receipts.push(statement);
            summary.limitations = vec!["Single synthetic workload".into()];
            summary.metadata = fixture_metadata();
            summary
        }
        "interrupted-run" => {
            let mut summary = FinalizeCapture::new(
                handle,
                finalize_key,
                Some(at(4)),
                CaptureCompletion::Interrupted,
                ExecutionReport::new(ExecutionStatus::Unknown),
            );
            summary.declared_artifacts = vec![key("result")];
            summary.limitations = vec!["Producer cannot vouch for positions after 2".into()];
            summary
        }
        _ => {
            let mut summary = FinalizeCapture::new(
                handle,
                finalize_key,
                Some(at(3)),
                CaptureCompletion::Complete,
                completed,
            );
            summary.assertions.push(metric());
            summary
        }
    }
}

fn begin_request(options: &Options, driver: &mut Driver<'_>) -> Result<BeginCapture, Stop> {
    let id = options.external_run_id.as_str();
    let producer = match ProducerName::new(options.producer.as_str()) {
        Ok(producer) => producer,
        Err(error) => {
            // The recording crate refuses the name before any call. Report it as
            // the recorder would: a rejected `begin` with `INVALID_INPUT`.
            eprintln!("begin rejected: {error}");
            driver.calls.push(json!({
                "operation": "capture.begin",
                "key": format!("{id}/begin"),
                "position": null,
                "result": "rejected",
                "error_code": "INVALID_INPUT",
            }));
            return Err(Stop::Rejected);
        }
    };
    let mut begin = BeginCapture::new(
        key(&format!("{id}/begin")),
        format!("agent:{producer}")
            .parse()
            .map_err(|_| Stop::Rejected)?,
        ProducerIdentity::new(producer),
        options.external_run_id.clone(),
        CaptureIntent::new("synthetic run"),
        CaseTarget::NewRecordCase {
            title: "Synthetic runs".into(),
        },
    );
    if options.scenario == "partial-evidence" {
        begin.metadata = fixture_metadata();
    }
    begin.protocol.clone_from(&options.protocol);
    Ok(begin)
}

/// Begin, the scenario's steps, finalize. `run` sends every request of the
/// scenario, so a rerun resends them all and gets the original receipts. `resume`
/// resends the same `begin`, asks `recover` where it stopped, and continues with
/// the first step after `last_position`.
fn synthetic_run(
    options: &Options,
    driver: &mut Driver<'_>,
    ids: &RefCell<(Option<String>, Option<String>)>,
) -> Result<FinalizeReceipt, Stop> {
    let id = options.external_run_id.as_str();
    let begin = begin_request(options, driver)?;
    let begun = driver.call(
        "begin",
        begin.key.as_str(),
        None,
        |r| r.begin(&begin),
        |receipt| {
            // Report the IDs even if an injected crash fires right after this call.
            *ids.borrow_mut() = (
                Some(receipt.run_id.to_string()),
                Some(receipt.handle.to_string()),
            );
            commit_result(&receipt.commit)
        },
    )?;
    let handle: CaptureHandle = begun.handle;
    if !options.resume {
        let receipt = run_steps(options, driver, handle, id, 0)?;
        if options.scenario == "late-item" {
            late_item(driver, handle)?;
        }
        return Ok(receipt);
    }

    let state = driver
        .recorder
        .recover(&RecoverScope::Handle { handle })
        .map_err(|error| {
            eprintln!("recover failed: {error}");
            Stop::Rejected
        })?;
    let recovered = state.handles.first().ok_or(Stop::Rejected)?;
    let receipt = match &recovered.finalized {
        Some(finalized) => finalized.clone(),
        None => {
            let done = recovered.last_position.map_or(0, Position::get);
            run_steps(options, driver, handle, id, done)?
        }
    };
    if options.scenario == "late-item" {
        late_item(driver, handle)?;
    }
    Ok(receipt)
}

fn run_steps(
    options: &Options,
    driver: &mut Driver<'_>,
    handle: CaptureHandle,
    id: &str,
    done: u64,
) -> Result<FinalizeReceipt, Stop> {
    for step in steps(options.scenario, handle, &options.output_dir) {
        match step {
            Step::Append(event) if event.position.get() > done => {
                driver.call(
                    "append",
                    event.key.as_str(),
                    Some(event.position.get()),
                    |r| r.append(&event),
                    item_result,
                )?;
            }
            Step::Attach(artifact) if artifact.position.get() > done => {
                if artifact.key.as_str() == "result" && fabricate(&options.output_dir)?.is_none() {
                    // Cannot vouch for the step that did not finish: settle as interrupted.
                    let mut execution = ExecutionReport::new(ExecutionStatus::Unknown);
                    execution.error = Some("the result file could not be vouched for".into());
                    let summary = FinalizeCapture::new(
                        handle,
                        key(&format!("{id}/finalize")),
                        Position::new(done),
                        CaptureCompletion::Interrupted,
                        execution,
                    );
                    return finalize(driver, &summary);
                }
                driver.call(
                    "attach",
                    artifact.key.as_str(),
                    Some(artifact.position.get()),
                    |r| r.attach(&artifact),
                    |attached| item_result(&attached.receipt),
                )?;
            }
            _ => {}
        }
    }
    finalize(driver, &finalize_request(options.scenario, handle, id))
}

/// Appending to a sealed handle must be refused with `REVISION_CONFLICT`. The
/// refusal is the expected outcome; an acceptance is a failure.
fn late_item(driver: &mut Driver<'_>, handle: CaptureHandle) -> Result<(), Stop> {
    let late = CaptureEvent::new(
        handle,
        key("late-note"),
        at(4),
        LifecycleFact::PhaseChanged {
            phase: "late".into(),
        },
    );
    match driver.call(
        "append",
        "late-note",
        Some(4),
        |r| r.append(&late),
        item_result,
    ) {
        Err(Stop::Rejected)
            if driver
                .calls
                .last()
                .is_some_and(|call| call["error_code"] == "REVISION_CONFLICT") =>
        {
            Ok(())
        }
        Err(stop) => Err(stop),
        Ok(_) => {
            eprintln!("the late item was accepted after finalization");
            Err(Stop::Rejected)
        }
    }
}

fn finalize(driver: &mut Driver<'_>, summary: &FinalizeCapture) -> Result<FinalizeReceipt, Stop> {
    driver.call(
        "finalize",
        summary.key.as_str(),
        None,
        |r| r.finalize(summary),
        |receipt| commit_result(&receipt.commit),
    )
}
