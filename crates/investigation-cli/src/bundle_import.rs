//! `ingest --input DIR/producer-bundle.json`: re-import every run of a
//! producer bundle (W11.10).
//!
//! The bundle is first checked offline against its own manifest, exactly as
//! `archive verify` checks it, so no altered, missing or extra file is read.
//! Then each run's `inv.producer-run.v1` envelope is ingested through the
//! ordinary producer-run path with the run's own `files` folder as its
//! artifact folder. That folder must lie beneath a configured import root:
//! the bundle widens nothing that ingest may read. Identity follows the
//! acquisition rules: the producer name and external run ID name the
//! attempt, so a run already recorded with the same envelope is deduplicated
//! and never a new trial, and a changed one is revised.
//!
//! Each run is its own transaction, in bundle order, and each is idempotent,
//! so an interrupted import is completed by running it again. Runs go into
//! `--case` when given, otherwise into the case the first run lands in.

use std::{
    env,
    path::{Path, PathBuf},
};

use std::collections::BTreeMap;

use investigation_core::{
    command::RevisionRef,
    identity::{Id, Kind},
    producer_run::PRODUCER_RUN_KEY,
    wire::{Digest, ErrorCode, Failure},
};
use investigation_render::export::{
    producer_bundle::{BUNDLE_FORMAT, INDEX_FILE, KIND, ProducerBundle},
    verify::verify_export,
};
use investigation_store::{
    Store, journal::hash_bytes, store::ingest::producer_run::ProducerRunSubmission,
};
use serde_json::json;

use crate::{
    Global, Output, display_argv,
    selection::{select_actor, select_case, workspace_argv},
    with_workspace_repair,
};

/// Whether `bytes` are a producer bundle index.
pub(crate) fn is_bundle(bytes: &[u8]) -> bool {
    #[derive(serde::Deserialize)]
    struct Version {
        schema_version: Option<String>,
    }
    serde_json::from_slice::<Version>(bytes)
        .ok()
        .and_then(|version| version.schema_version)
        .is_some_and(|version| version == BUNDLE_FORMAT)
}

fn refuse(code: ErrorCode, message: impl Into<String>) -> Failure {
    Failure::new(code, message)
        .at("--input")
        .with_schema("producer-bundle")
}

/// Re-import every run of the bundle whose index is at `path`.
pub(crate) fn ingest(store: &mut Store, global: &Global, path: &Path) -> Result<Output, Failure> {
    if path == Path::new("-") {
        return Err(refuse(
            ErrorCode::InvalidInput,
            "A producer bundle is read from its directory. Name DIR/producer-bundle.json, not stdin.",
        ));
    }
    if store.is_dry_run() {
        return Err(refuse(
            ErrorCode::InvalidInput,
            "A producer bundle is re-imported run by run, so --dry-run is refused for it. \
             Dry-run one run with --input DIR/runs/NNNN/producer-run.json.",
        ));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| Failure::new(ErrorCode::IoFailure, error.to_string()))?
            .join(path)
    };
    if absolute.file_name().and_then(|name| name.to_str()) != Some(INDEX_FILE) {
        return Err(refuse(
            ErrorCode::InvalidInput,
            format!("Name the bundle's {INDEX_FILE}, in the directory the export wrote."),
        ));
    }
    let directory = absolute
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| refuse(ErrorCode::InvalidInput, "The bundle has no directory."))?;
    let scratch = store.workspace().root().join("state").join("tmp");
    let report = verify_export(&directory, &scratch)
        .map_err(|error| refuse(ErrorCode::IoFailure, error.to_string()))?;
    if !report.ok() || report.kind.as_deref() != Some(KIND) {
        let mut failure = refuse(
            ErrorCode::IntegrityFailure,
            "The bundle does not match its export-manifest.json, or is not a producer bundle, \
             so nothing was ingested. Run archive verify on its directory for the findings.",
        );
        failure.error.actual = Some(json!({"kind": report.kind, "problems": report.problems}));
        return Err(failure);
    }
    let bundle: ProducerBundle =
        serde_json::from_slice(&read(&directory, INDEX_FILE)?).map_err(|error| {
            refuse(
                ErrorCode::InvalidInput,
                format!("{INDEX_FILE} does not follow the producer-bundle schema: {error}."),
            )
        })?;
    let prefix = import_prefix(store, &directory)?;
    let actor = select_actor(store.workspace(), global.actor.as_ref(), None)?;
    let mut case_id = match global.case.as_deref() {
        Some(selector) => Some(select_case(store, Some(selector))?),
        None => None,
    };

    let recorded = recorded_envelopes(store)?;
    let mut runs = Vec::new();
    let mut skipped = Vec::new();
    for run in &bundle.runs {
        let Some(reingest) = &run.reingest else {
            skipped.push(json!({
                "folder": run.folder,
                "run": run.run,
                "not_reingestible": run.not_reingestible,
            }));
            continue;
        };
        let bytes = read(&directory, &reingest.envelope)?;
        if hash_bytes(&bytes) != reingest.sha256 {
            return Err(refuse(
                ErrorCode::IntegrityFailure,
                format!(
                    "{} does not match its digest in {INDEX_FILE}.",
                    reingest.envelope
                ),
            ));
        }
        // This exact envelope already describes a Run here: the same
        // attempt, recorded, whatever bytes the bundle could not carry
        // (a rejected artifact's were never retained).
        if let Some((case, recorded_run)) = recorded.get(&reingest.sha256) {
            case_id.get_or_insert(*case);
            runs.push(json!({
                "folder": run.folder,
                "bundle_run": run.run,
                "run": recorded_run,
                "run_outcome": "already_recorded",
                "result": "not_committed",
            }));
            continue;
        }
        let mut submission = ProducerRunSubmission::new(actor.clone());
        submission.case_id = case_id;
        submission.artifact_folder = Some(join(&prefix, &reingest.artifact_folder));
        let receipt = match store.ingest_producer_run(&bytes, &submission) {
            Ok(receipt) => receipt,
            Err(failure) => {
                // The same attempt, already recorded here through the
                // capture protocol: it is not a new trial, and there is
                // nothing to add to it from a description of it.
                if let Some(captured) = captured_attempt(&failure) {
                    if case_id.is_none() {
                        case_id = captured["case_id"].as_str().and_then(|id| id.parse().ok());
                    }
                    runs.push(json!({
                        "folder": run.folder,
                        "bundle_run": run.run,
                        "run": {"id": captured["run_id"]},
                        "run_outcome": "already_captured",
                        "result": "not_committed",
                    }));
                    continue;
                }
                let mut failure = with_workspace_repair(store.workspace(), failure);
                failure.error.actual = Some(json!({
                    "failed_run": run.folder,
                    "reimported": runs,
                }));
                return Err(failure);
            }
        };
        case_id.get_or_insert(receipt.case_id);
        runs.push(json!({
            "folder": run.folder,
            "bundle_run": run.run,
            "run": receipt.run,
            "run_outcome": receipt.run_outcome,
            "transaction_id": receipt.receipt.transaction_id,
            "result": receipt.receipt.result,
            "envelope": receipt.envelope.outcome,
            "artifacts": receipt.artifacts.iter().map(|item| json!({
                "item": item.item,
                "outcome": item.outcome,
                "digest": item.digest,
            })).collect::<Vec<_>>(),
            "execution": receipt.execution,
            "evidence_completeness": receipt.evidence_completeness,
            "measurement_validity": receipt.measurement_validity,
        }));
    }

    let mut next = workspace_argv(store.workspace());
    if let Some(case_id) = case_id {
        next.extend(["--case".into(), case_id.to_string()]);
    }
    next.extend(["brief".into(), "--format".into(), "json".into()]);
    let mut text = format!(
        "ingested producer bundle  {} run(s)  {} skipped",
        runs.len(),
        skipped.len()
    );
    for run in &runs {
        text.push_str(&format!(
            "\n{}  {}  {}  {}",
            run["folder"].as_str().unwrap_or_default(),
            run["run"]["id"].as_str().unwrap_or_default(),
            run["run_outcome"].as_str().unwrap_or_default(),
            run["result"].as_str().unwrap_or_default(),
        ));
    }
    for run in &skipped {
        text.push_str(&format!(
            "\n{}  not re-importable: {}",
            run["folder"].as_str().unwrap_or_default(),
            run["not_reingestible"].as_str().unwrap_or_default(),
        ));
    }
    text.push_str(&format!("\nNext: {}", display_argv(&next)));
    let value = json!({
        "bundle": {
            "case_id": bundle.case_id,
            "audience": bundle.audience,
            "export_id": report.export_id,
            "runs": bundle.runs.len(),
        },
        "case_id": case_id,
        "runs": runs,
        "skipped": skipped,
        "next": [{"kind": "local_read", "argv": next}],
    });
    Ok(Output::success("ingested", value, text))
}

/// Runs recorded here from an envelope, by the envelope's digest: the
/// envelope Source of a Run revision is the exact description it was
/// ingested from.
fn recorded_envelopes(store: &mut Store) -> Result<BTreeMap<Digest, (Id, RevisionRef)>, Failure> {
    let mut recorded = BTreeMap::new();
    for (case_id, replay) in store.cases()? {
        for record in replay.state.records_of_kind(Kind::Run) {
            let digest = record.body[PRODUCER_RUN_KEY]["envelope"]["digest"].clone();
            if let Ok(digest) = serde_json::from_value::<Digest>(digest) {
                recorded.insert(digest, (case_id, record.reference()));
            }
        }
    }
    Ok(recorded)
}

/// The run and case of a refusal that names the attempt as already
/// recorded through `capture.begin`.
fn captured_attempt(failure: &Failure) -> Option<&serde_json::Value> {
    let expected = failure.error.expected.as_ref()?;
    (failure.error.code == ErrorCode::IdempotencyKeyReused
        && failure.error.path.as_deref() == Some("/external_run_id")
        && expected["recorded_by"] == "capture.begin")
        .then_some(expected)
}

/// A bundle member, at most the size ingest accepts for an envelope.
fn read(directory: &Path, member: &str) -> Result<Vec<u8>, Failure> {
    std::fs::read(directory.join(member))
        .map_err(|error| refuse(ErrorCode::IoFailure, format!("{member}: {error}")))
}

/// The bundle directory relative to the configured import root that holds
/// it; empty when it is the root itself.
fn import_prefix(store: &Store, directory: &Path) -> Result<String, Failure> {
    let workspace = store.workspace();
    let directory = directory
        .canonicalize()
        .map_err(|error| refuse(ErrorCode::IoFailure, error.to_string()))?;
    for configured in &workspace.config().import_roots {
        let root: PathBuf = if configured.is_absolute() {
            configured.clone()
        } else {
            workspace.root().join(configured)
        };
        let Ok(root) = root.canonicalize() else {
            continue;
        };
        if let Ok(relative) = directory.strip_prefix(&root) {
            let parts: Option<Vec<&str>> = relative
                .components()
                .map(|component| component.as_os_str().to_str())
                .collect();
            if let Some(parts) = parts {
                return Ok(parts.join("/"));
            }
        }
    }
    let mut failure = refuse(
        ErrorCode::InvalidInput,
        "The bundle directory is not beneath an import root of this workspace, and ingest \
         reads artifact bytes only beneath import roots. Move the bundle below one, or \
         initialize a workspace with --import-root naming a folder that holds it.",
    );
    failure.error.expected = Some(json!({"import_roots": workspace.config().import_roots}));
    Err(failure)
}

fn join(prefix: &str, folder: &str) -> String {
    if prefix.is_empty() {
        folder.to_owned()
    } else {
        format!("{prefix}/{folder}")
    }
}
