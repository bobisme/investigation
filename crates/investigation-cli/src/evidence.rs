use std::{
    io,
    path::{Path, PathBuf},
};

use clap::Subcommand;
use investigation_core::{
    evidence::SelectorRequest,
    identity::{Kind, resolve_prefix},
    input,
    wire::{ErrorCode, Failure},
};
use investigation_store::opaque::{EnvelopeProbe, probe_envelope};
use investigation_store::{
    Store,
    store::{
        evidence_read::Excerpt,
        ingest::{
            self, IngestReceipt, StreamContent,
            producer_run::{ProducerRunReceipt, ProducerRunSubmission},
        },
    },
};
use serde_json::json;

use crate::selection::{require_case_agreement, select_actor, select_case};
use crate::{Global, Output, read_input, receipt_output, with_workspace_repair};

#[derive(Subcommand)]
pub(crate) enum EvidenceCommand {
    /// Verify and read a bounded selection over retained bytes.
    Read {
        /// Artifact ID from a source or ingest receipt; a unique typed prefix works.
        artifact: String,
        /// Read the selector (byte, line or text range) from FILE, or - for stdin.
        #[arg(long, value_name = "FILE|-")]
        selector_file: PathBuf,
    },
}

pub(crate) fn ingest(store: &mut Store, global: &Global, path: &Path) -> Result<Output, Failure> {
    let bytes = read_input(path)?;
    if crate::bundle_import::is_bundle(&bytes) {
        return crate::bundle_import::ingest(store, global, path);
    }
    if probe_envelope(&bytes) != EnvelopeProbe::NotAnEnvelope {
        return producer_run(store, global, &bytes);
    }
    let (manifest, _) = ingest::parse_manifest(&bytes)?;
    select_actor(
        store.workspace(),
        global.actor.as_ref(),
        Some(&manifest.actor),
    )?;
    require_case_agreement(store, global.case.as_deref(), manifest.case_id)?;
    if matches!(manifest.content, Some(StreamContent::Stdin { .. })) {
        if path == Path::new("-") {
            return Err(Failure::new(
                ErrorCode::InvalidInput,
                "Use a manifest file when the source bytes arrive on stdin.",
            )
            .at("--input")
            .with_schema("ingest"));
        }
        return submit(store, &bytes, Some(&mut io::stdin().lock()));
    }
    submit(store, &bytes, None)
}

/// Ingest an `inv.producer-run.v1` envelope. The envelope names no actor,
/// case or command: the actor comes from --actor or settings, --case selects an
/// existing case (else a minimal record case is created), and the command ID is
/// derived from the request content.
fn producer_run(store: &mut Store, global: &Global, bytes: &[u8]) -> Result<Output, Failure> {
    let mut submission = ProducerRunSubmission::new(select_actor(
        store.workspace(),
        global.actor.as_ref(),
        None,
    )?);
    if let Some(selector) = global.case.as_deref() {
        submission.case_id = Some(select_case(store, Some(selector))?);
    }
    let ProducerRunReceipt {
        receipt,
        case_id,
        run,
        run_outcome,
        envelope,
        artifacts,
        execution,
        evidence_completeness,
        measurement_validity,
    } = store
        .ingest_producer_run(bytes, &submission)
        .map_err(|failure| with_workspace_repair(store.workspace(), failure))?;
    let mut out = receipt_output(store.workspace(), receipt);
    out.text.push_str(&format!(
        "
run {}  revision {}  {}  execution {}  evidence {}  validity {}",
        run.id,
        run.revision,
        json!(run_outcome).as_str().unwrap_or_default(),
        json!(execution).as_str().unwrap_or_default(),
        json!(evidence_completeness).as_str().unwrap_or_default(),
        json!(measurement_validity).as_str().unwrap_or_default(),
    ));
    for item in &artifacts {
        out.text.push_str(&format!(
            "
{}  {}",
            item.item,
            json!(item.outcome).as_str().unwrap_or_default()
        ));
    }
    out.value["case_id"] = json!(case_id);
    out.value["run"] = json!(run);
    out.value["run_outcome"] = json!(run_outcome);
    out.value["envelope"] = json!(envelope);
    out.value["artifacts"] = json!(artifacts);
    out.value["execution"] = json!(execution);
    out.value["evidence_completeness"] = json!(evidence_completeness);
    out.value["measurement_validity"] = json!(measurement_validity);
    Ok(out)
}

pub(crate) fn start(
    store: &mut Store,
    global: &Global,
    question: &str,
    attachment: &Path,
    command_id: Option<&str>,
) -> Result<Output, Failure> {
    require_case_agreement(store, global.case.as_deref(), None)?;
    let actor = select_actor(store.workspace(), global.actor.as_ref(), None)?;
    let stdin = attachment == Path::new("-");
    let attachment =
        &crate::first_use::check_attachment(store.workspace(), attachment, Some("--attach"))?;
    let command_id = match command_id {
        Some(command_id) => command_id.to_owned(),
        None => {
            let generated = crate::shortcuts::generated_command_id();
            if !store.is_dry_run() {
                crate::shortcuts::announce_generated_id(&generated)?;
            }
            generated
        }
    };
    let mut manifest = json!({
        "schema_version":ingest::IngestVersion::V1,
        "command_id":command_id,
        "actor":actor,
        "new_case":{"title":question,"mode":"explore","objective":question},
        "source":{"title":"Initial attachment"},
        "extract_text":true,
    });
    if stdin {
        manifest["content"] = json!({"kind":"stdin","name":"Initial attachment"});
    } else {
        manifest["paths"] = json!([attachment]);
    }
    let bytes = serde_json::to_vec(&manifest).expect("start manifest serializes");
    let mut out = if stdin {
        submit(store, &bytes, Some(&mut io::stdin().lock()))
    } else {
        submit(store, &bytes, None)
    }?;
    let created = |prefix: &str| {
        out.value["created"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|change| change["id"].as_str())
            .find(|id| id.starts_with(prefix))
            .map(str::to_owned)
    };
    // A dry run's records do not exist yet; its next write is the commit.
    if let (false, Some(case), Some(source)) =
        (store.is_dry_run(), created("case_"), created("src_"))
    {
        // The v1 receipt `next` list holds local reads only, so the write
        // that should follow is published beside it as `next_write`.
        let step =
            crate::first_use::observe_step(store.workspace(), &actor.to_string(), &case, &source);
        let argv: Vec<String> = step["argv"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| part.as_str().map(str::to_owned))
            .collect();
        out.text.push_str(&format!(
            "\nRecord what the source shows, quoting it exactly: {}",
            crate::display_argv(&argv)
        ));
        out.value["next_write"] = step;
    }
    Ok(out)
}

fn submit(
    store: &mut Store,
    bytes: &[u8],
    stdin: Option<&mut dyn io::Read>,
) -> Result<Output, Failure> {
    let IngestReceipt {
        receipt,
        items,
        manifest_artifact_id,
    } = store
        .ingest(bytes, stdin)
        .map_err(|failure| with_workspace_repair(store.workspace(), failure))?;
    let mut out = receipt_output(store.workspace(), receipt);
    out.value["items"] = json!(items);
    out.value["manifest_artifact_id"] = json!(manifest_artifact_id);
    Ok(out)
}

pub(crate) fn submit_shortcut_manifest(store: &mut Store, bytes: &[u8]) -> Result<Output, Failure> {
    submit(store, bytes, None)
}

pub(crate) fn read(
    store: &mut Store,
    global: &Global,
    command: EvidenceCommand,
) -> Result<Output, Failure> {
    let EvidenceCommand::Read {
        artifact,
        selector_file,
    } = command;
    let case = select_case(store, global.case.as_deref())?;
    let replay = store.case(case)?;
    let id = resolve_prefix(
        &artifact,
        replay
            .state
            .records()
            .map(|record| record.header.id)
            .filter(|id| id.kind() == Kind::Artifact),
    )
    .map_err(|error| {
        use investigation_core::identity::ResolveError;
        let code = match error {
            ResolveError::InvalidPrefix => ErrorCode::InvalidInput,
            ResolveError::NotFound => ErrorCode::UnknownReference,
            ResolveError::Ambiguous(_) => ErrorCode::AmbiguousReference,
        };
        Failure::new(code, "Supply a unique Artifact ID from this case.").at("/artifact")
    })?;
    let selector = input::parse_request(&read_input(&selector_file)?)
        .and_then(|value| input::decode::<SelectorRequest>(&value))
        .map_err(|error| {
            Failure::new(ErrorCode::InvalidInput, error.message)
                .at(error.path)
                .with_schema("evidence-selector")
        })?;
    selector.selector.validate_shape().map_err(|error| {
        Failure::new(ErrorCode::InvalidInput, error.code())
            .at("/selector")
            .with_schema("evidence-selector")
    })?;
    let record = replay.state.current(id).expect("resolved current artifact");
    let evidence = store
        .evidence_read(&replay, record, selector)
        .map_err(|failure| with_workspace_repair(store.workspace(), failure))?;
    let session_read =
        crate::inherited_trust::record_read(store, global, case, record.reference())?;
    let text = match &evidence.excerpt {
        Excerpt::Text { text, .. } => text
            .chars()
            .flat_map(|character| {
                if character.is_control() && !matches!(character, '\n' | '\t') {
                    character
                        .escape_default()
                        .to_string()
                        .chars()
                        .collect::<Vec<_>>()
                } else {
                    vec![character]
                }
            })
            .collect(),
        Excerpt::JsonValue { value } => {
            serde_json::to_string_pretty(value).expect("selected JSON serializes")
        }
        Excerpt::Binary { .. } => "Binary content is retained in the workspace.".into(),
    };
    // The excerpt is delimited by a fence longer than any backtick run inside
    // it, so no excerpt line can close it.
    let fence = "`".repeat((longest_backtick_run(&text) + 1).max(3));
    let mut value = json!(evidence);
    value["session_read"] = session_read;
    Ok(Output::success(
        "evidence_read",
        value,
        format!(
            "{id}  untrusted evidence, quoted data between the fences\n{fence}text\n{}\n{fence}",
            text.strip_suffix('\n').unwrap_or(&text)
        ),
    ))
}

fn longest_backtick_run(text: &str) -> usize {
    let (mut longest, mut run) = (0, 0);
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}
