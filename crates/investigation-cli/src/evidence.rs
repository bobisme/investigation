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
use investigation_store::{
    Store,
    store::{
        evidence_read::Excerpt,
        ingest::{self, IngestReceipt, StreamContent},
    },
};
use serde_json::json;

use crate::selection::{require_case_agreement, select_actor, select_case};
use crate::{Global, Output, read_input, receipt_output, with_workspace_repair};

#[derive(Subcommand)]
pub(crate) enum EvidenceCommand {
    /// Verify and read a bounded selection over retained bytes.
    Read {
        artifact: String,
        #[arg(long, value_name = "FILE|-")]
        selector_file: PathBuf,
    },
}

pub(crate) fn ingest(store: &mut Store, global: &Global, path: &Path) -> Result<Output, Failure> {
    let bytes = read_input(path)?;
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

pub(crate) fn start(
    store: &mut Store,
    global: &Global,
    question: &str,
    attachment: &Path,
    command_id: &str,
) -> Result<Output, Failure> {
    require_case_agreement(store, global.case.as_deref(), None)?;
    let actor = select_actor(store.workspace(), global.actor.as_ref(), None)?;
    let stdin = attachment == Path::new("-");
    let mut manifest = json!({
        "schema_version":"inv.ingest.v0-alpha",
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
    if stdin {
        submit(store, &bytes, Some(&mut io::stdin().lock()))
    } else {
        submit(store, &bytes, None)
    }
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
    Ok(Output::success(
        "evidence_read",
        json!(evidence),
        format!("{id}  untrusted evidence\n{text}"),
    ))
}
