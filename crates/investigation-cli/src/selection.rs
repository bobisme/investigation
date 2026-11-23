use std::{env, io, path::Path};

use investigation_core::{
    PROGRAM_NAME,
    header::{Actor, ActorResolutionError, resolve_actor},
    identity::{Id, Kind, ResolveError, resolve_prefix},
    wire::{ErrorCode, Failure},
};
use investigation_store::{Store, Workspace, workspace::WorkspaceError};
use serde_json::json;

pub(crate) fn open_workspace(path: Option<&Path>) -> Result<Workspace, Failure> {
    match path {
        Some(path) => Workspace::open(path).map_err(|error| {
            if matches!(&error, WorkspaceError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound)
                && !path.join("investigation.toml").is_file()
            {
                return workspace_failure(WorkspaceError::WorkspaceNotFound(path.to_owned()));
            }
            workspace_failure(error)
        }),
        None => {
            let cwd = env::current_dir().map_err(|error| {
                Failure::new(ErrorCode::IoFailure, format!("Read working directory: {error}"))
            })?;
            Workspace::discover(&cwd).map_err(|error| {
                let failure = workspace_failure(error);
                match crate::first_use::nearby_workspaces().as_slice() {
                    [(_, display), ..] if failure.error.code == ErrorCode::WorkspaceRequired => {
                        Failure::new(
                            ErrorCode::WorkspaceRequired,
                            format!(
                                "{} A workspace exists at {display}; pass --workspace {display}.",
                                failure.error.message
                            ),
                        )
                        .with_argv(
                            [PROGRAM_NAME, "--workspace", display, "onboard", "--format", "json"]
                                .map(str::to_owned)
                                .to_vec(),
                        )
                    }
                    _ => failure,
                }
            })
        }
    }
}

pub(crate) fn workspace_failure(error: WorkspaceError) -> Failure {
    let code = match &error {
        WorkspaceError::WorkspaceNotFound(_) => ErrorCode::WorkspaceRequired,
        WorkspaceError::Io { .. }
        | WorkspaceError::FilesystemDetection { .. }
        | WorkspaceError::Serialization(_)
        | WorkspaceError::Identity(_) => ErrorCode::IoFailure,
        WorkspaceError::WorkspaceAlreadyExists(_)
        | WorkspaceError::InvalidRoot(_)
        | WorkspaceError::SymlinkRejected(_)
        | WorkspaceError::InvalidImportRoot(_)
        | WorkspaceError::UnsupportedFilesystem { .. } => ErrorCode::InvalidInput,
        _ => ErrorCode::IntegrityFailure,
    };
    let advice = match &error {
        WorkspaceError::UnsupportedFilesystem { .. } => " Choose a local workspace path.",
        _ if code == ErrorCode::WorkspaceRequired => {
            " Run investigation init PATH or pass --workspace PATH."
        }
        _ => "",
    };
    let failure = Failure::new(code, format!("{error}{advice}"));
    if code == ErrorCode::WorkspaceRequired {
        failure.with_argv(
            [PROGRAM_NAME, "onboard", "--format", "json"]
                .map(str::to_owned)
                .to_vec(),
        )
    } else {
        failure
    }
}

pub(crate) fn select_actor(
    workspace: &Workspace,
    flag: Option<&Actor>,
    envelope: Option<&Actor>,
) -> Result<Actor, Failure> {
    let config = workspace.config().actor.as_ref();
    let environment = if flag.or(envelope).or(config).is_none() {
        env::var_os("AGENT")
            .map(|value| {
                value
                    .to_str()
                    .and_then(|value| value.parse::<Actor>().ok())
                    .ok_or_else(|| {
                        Failure::new(
                            ErrorCode::InvalidInput,
                            "AGENT must be a valid actor identifier. Pass --actor to override it.",
                        )
                        .at("AGENT")
                    })
            })
            .transpose()?
    } else {
        None
    };

    resolve_actor(flag, envelope, config, environment.as_ref()).map_err(|error| match error {
        ActorResolutionError::Conflict => {
            let mut failure = Failure::new(
                ErrorCode::InvalidInput,
                "--actor must match the request actor.",
            )
            .at("/actor");
            failure.error.expected = Some(json!(flag));
            failure.error.actual = Some(json!(envelope));
            failure
        }
        ActorResolutionError::Missing => {
            let failure = Failure::new(
                ErrorCode::InvalidInput,
                "Specify an attributed actor with --actor, the workspace actor setting, or AGENT, for example --actor agent:investigator.",
            )
            .at("/actor");
            match crate::first_use::with_example_actor(env::args_os()) {
                Some(argv) => failure.with_argv(argv),
                None => failure,
            }
        }
    })
}

pub(crate) fn select_case(store: &mut Store, selector: Option<&str>) -> Result<Id, Failure> {
    let heads = store.list_cases()?;
    if let Some(selector) = selector {
        return resolve_case_prefix(selector, heads.iter().map(|head| head.case_id));
    }
    if let [head] = heads.as_slice() {
        return Ok(head.case_id);
    }

    let mut failure = Failure::new(
        ErrorCode::CaseRequired,
        if heads.is_empty() {
            "Create a case with investigation start QUESTION --attach FILE, or with investigation case new TITLE."
        } else {
            "Select a case with --case ID."
        },
    )
    .at("/case_id");
    failure.error.actual = Some(json!(
        heads.iter().map(|head| head.case_id).collect::<Vec<_>>()
    ));
    let mut argv = workspace_argv(store.workspace());
    argv.extend(["onboard", "--format", "json"].map(str::to_owned));
    Err(failure.with_argv(argv))
}

fn resolve_case_prefix(selector: &str, ids: impl Iterator<Item = Id>) -> Result<Id, Failure> {
    if !selector.starts_with("case_") {
        return Err(invalid_case_selector());
    }
    resolve_prefix(selector, ids).map_err(|error| match error {
        ResolveError::InvalidPrefix => invalid_case_selector(),
        ResolveError::NotFound => Failure::new(
            ErrorCode::UnknownReference,
            "No case matches the supplied ID.",
        )
        .at("/case_id"),
        ResolveError::Ambiguous(ids) => {
            let mut failure = Failure::new(
                ErrorCode::AmbiguousReference,
                "The case prefix matches several cases. Use a full case ID.",
            )
            .at("/case_id");
            failure.error.actual = Some(json!(ids));
            failure
        }
    })
}

fn invalid_case_selector() -> Failure {
    Failure::new(
        ErrorCode::InvalidInput,
        "Supply a case ID or a unique case_ prefix.",
    )
    .at("/case_id")
}

pub(crate) fn require_case_agreement(
    store: &mut Store,
    selector: Option<&str>,
    envelope: Option<Id>,
) -> Result<(), Failure> {
    let Some(selector) = selector else {
        return Ok(());
    };
    let selected = match selector.parse::<Id>() {
        Ok(id) if id.kind() == Kind::Case => id,
        _ => select_case(store, Some(selector))?,
    };
    if Some(selected) != envelope {
        let mut failure = Failure::new(
            ErrorCode::InvalidInput,
            "--case must match the request case_id. Omit --case when creating a case.",
        )
        .at("/case_id");
        failure.error.expected = Some(json!(selected));
        failure.error.actual = Some(json!(envelope));
        return Err(failure);
    }
    Ok(())
}

pub(crate) fn workspace_argv(workspace: &Workspace) -> Vec<String> {
    vec![
        PROGRAM_NAME.into(),
        "--workspace".into(),
        workspace.root().to_string_lossy().into_owned(),
    ]
}
