use std::{env, io, path::Path};

use investigation_core::{
    PROGRAM_NAME,
    classification::{AudiencePolicy, Classifier, Field},
    header::{Actor, ActorResolutionError, resolve_actor},
    identity::{Id, Kind, ResolveError, resolve_prefix},
    wire::{ErrorCode, Failure},
};
use investigation_store::{Store, Workspace, workspace::WorkspaceError};
use serde_json::json;

thread_local! {
    /// The command this invocation runs when it writes, so opening the
    /// workspace can refuse a write the workspace's format forbids.
    static WRITE_INTENT: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Writing commands the format gate lets through: an export only reads the
/// workspace, and migrate applies the format rules itself.
const FORMAT_EXEMPT: [&str; 2] = ["export", "migrate"];

/// Record which command this invocation runs, when it writes.
pub(crate) fn note_write_intent(command: Option<String>) {
    WRITE_INTENT.with(|intent| *intent.borrow_mut() = command);
}

/// Refuse a writing command on a workspace whose format this program must
/// not write: a newer format, a migration in progress, or a required
/// migration (W11.15, ADR 0014). Reads pass. Every commit checks the format
/// again under the coordination lock.
pub(crate) fn refuse_unwritable(workspace: &Workspace) -> Result<(), Failure> {
    let Some(command) = WRITE_INTENT.with(|intent| intent.borrow().clone()) else {
        return Ok(());
    };
    if FORMAT_EXEMPT.contains(&command.as_str()) {
        return Ok(());
    }
    let format = workspace.format();
    match investigation_store::migration::write_refusal(format) {
        None => Ok(()),
        Some(refusal) => {
            let mut failure = investigation_store::store::format_failure(&refusal, format);
            if let Some(actual) = failure.error.actual.as_mut() {
                actual["command"] = json!(command);
            }
            Err(failure)
        }
    }
}

pub(crate) fn open_workspace(path: Option<&Path>) -> Result<Workspace, Failure> {
    let workspace = open_workspace_ungated(path)?;
    refuse_unwritable(&workspace)?;
    Ok(workspace)
}

fn open_workspace_ungated(path: Option<&Path>) -> Result<Workspace, Failure> {
    match path {
        Some(path) => Workspace::open(path).map_err(|error| {
            if matches!(&error, WorkspaceError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound)
                && !path.join("investigation.toml").is_file()
            {
                return enclosing_workspace_hint(path).unwrap_or_else(|| {
                    workspace_failure(WorkspaceError::WorkspaceNotFound(path.to_owned()))
                });
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

/// `--workspace PATH` named no workspace, but the working directory is inside
/// one, typically after `cd` into it with a relative PATH: name that one
/// instead of suggesting `init`, which would nest a second workspace.
fn enclosing_workspace_hint(path: &Path) -> Option<Failure> {
    let workspace = Workspace::discover(&env::current_dir().ok()?).ok()?;
    let root = workspace.root().display().to_string();
    let missing = WorkspaceError::WorkspaceNotFound(path.to_owned());
    Some(
        Failure::new(
            ErrorCode::WorkspaceRequired,
            format!(
                "{missing}. The working directory is inside the workspace {root}; \
                 pass --workspace {root}. Do not run init here."
            ),
        )
        .with_argv(
            [
                PROGRAM_NAME,
                "--workspace",
                &root,
                "onboard",
                "--format",
                "json",
            ]
            .map(str::to_owned)
            .to_vec(),
        ),
    )
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
    let mut message = error.to_string();
    if !advice.is_empty() && !message.ends_with(['.', '!', '?']) {
        message.push('.');
    }
    let failure = Failure::new(code, format!("{message}{advice}"));
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

thread_local! {
    /// The actor this invocation took from the machine-local actor setting,
    /// so receipts can say where their attribution came from.
    static LOCAL_ACTOR_USED: std::cell::RefCell<Option<Actor>> =
        const { std::cell::RefCell::new(None) };
}

/// A receipt warning when this invocation attributed a write to the actor in
/// `local/actor.toml`, because no flag, request, workspace setting or AGENT
/// named one.
pub(crate) fn local_actor_warning() -> Option<String> {
    LOCAL_ACTOR_USED.with(|used| {
        used.borrow().as_ref().map(|actor| {
            format!(
                "Attributed to {actor}, the actor that init --actor recorded for this machine \
                 ({}). Pass --actor to attribute work to someone else.",
                investigation_store::workspace::LOCAL_ACTOR_FILE
            )
        })
    })
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

    // The machine-local setting is the last resort: it applies only where
    // the call would otherwise be refused for a missing actor.
    let local = if flag
        .or(envelope)
        .or(config)
        .or(environment.as_ref())
        .is_none()
    {
        workspace.local_actor().map_err(workspace_failure)?
    } else {
        None
    };
    if let Some(actor) = &local {
        LOCAL_ACTOR_USED.with(|used| *used.borrow_mut() = Some(actor.clone()));
    }

    resolve_actor(
        flag,
        envelope,
        config,
        environment.as_ref().or(local.as_ref()),
    )
    .map_err(|error| match error {
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
                "Specify an attributed actor with --actor or AGENT, for example --actor agent:investigator. To name it once for this machine, run investigation init DIR --actor ACTOR.",
            )
            .at("/actor");
            match crate::first_use::with_example_actor(env::args_os()) {
                Some(argv) => failure.with_argv(argv),
                None => failure,
            }
        }
    })
}

/// Resolve `--case` (or the only case) from the published case IDs. Nothing
/// is verified here; the command verifies the case it then reads.
pub(crate) fn select_case(store: &mut Store, selector: Option<&str>) -> Result<Id, Failure> {
    let cases = store.case_ids()?;
    if let Some(selector) = selector {
        return resolve_case_prefix(selector, cases.iter().copied());
    }
    if let [case] = cases.as_slice() {
        return Ok(*case);
    }

    let message = if cases.is_empty() {
        "Create a case with investigation start QUESTION --attach FILE, or with investigation case new TITLE.".to_owned()
    } else {
        format!(
            "Select a case with --case ID. The workspace has {} cases: {}.",
            cases.len(),
            listed_cases(store, &cases)
        )
    };
    let mut failure = Failure::new(ErrorCode::CaseRequired, message).at("/case_id");
    failure.error.actual = Some(json!(cases));
    let argv = workspace_argv(store.workspace());
    if cases.is_empty()
        && let Some(start) = crate::first_use::start_for_source_add(argv.clone())
    {
        return Err(failure.with_argv(start));
    }
    let mut argv = argv;
    argv.extend(["onboard", "--format", "json"].map(str::to_owned));
    Err(failure.with_argv(argv))
}

/// [`select_case`] for a read that names a record by `id`, a full ID or a
/// typed prefix (bn-1lau). Without `--case`, in a workspace with several
/// cases, the case is the one that holds the record: an ID names its case,
/// so this is not a guess. A prefix that matches records in several cases is
/// refused with `CASE_REQUIRED`, which names those cases and repeats this
/// invocation with `--case` set to the first. A typed ID that no case holds
/// is `UNKNOWN_REFERENCE`. Text that is not a typed ID selects as
/// [`select_case`] does.
pub(crate) fn select_case_for(
    store: &mut Store,
    selector: Option<&str>,
    id: &str,
) -> Result<Id, Failure> {
    if selector.is_some() {
        return select_case(store, selector);
    }
    let cases = store.case_ids()?;
    if cases.len() < 2 || resolve_prefix(id, std::iter::empty()) == Err(ResolveError::InvalidPrefix)
    {
        return select_case(store, selector);
    }
    let holders: Vec<Id> = store.locate_records(id)?.into_keys().collect();
    match holders.as_slice() {
        [case] => Ok(*case),
        [] => {
            let mut failure = Failure::new(
                ErrorCode::UnknownReference,
                format!(
                    "No case in this workspace holds a record that matches {id}. List a case's outcomes and evidence with investigation --case ID brief."
                ),
            )
            .at("/id");
            failure.error.actual = Some(json!({"id": id, "cases": cases}));
            Err(failure)
        }
        [first, ..] => {
            let mut failure = Failure::new(
                ErrorCode::CaseRequired,
                format!(
                    "{id} matches records in {} cases: {}. Select one with --case ID, or give a longer ID.",
                    holders.len(),
                    listed_cases(store, &holders)
                ),
            )
            .at("/case_id");
            failure.error.actual = Some(json!(holders));
            match with_case(env::args_os(), *first) {
                Some(argv) => Err(failure.with_argv(argv)),
                None => Err(failure),
            }
        }
    }
}

/// This invocation again with `--case CASE` before its first argument.
/// `None` when an argument is not UTF-8 or the invocation has no command.
fn with_case(args: impl Iterator<Item = std::ffi::OsString>, case: Id) -> Option<Vec<String>> {
    let rest: Option<Vec<String>> = args.skip(1).map(|arg| arg.into_string().ok()).collect();
    let rest = rest.filter(|rest| !rest.is_empty())?;
    let mut argv = vec![
        PROGRAM_NAME.to_owned(),
        "--case".to_owned(),
        case.to_string(),
    ];
    argv.extend(rest);
    Some(argv)
}

/// Cases a `CASE_REQUIRED` message lists by ID; `error.actual` holds all.
const MAX_LISTED_CASES: usize = 8;
/// Characters of a case title the list shows.
const MAX_LISTED_TITLE_CHARS: usize = 60;

/// The first [`MAX_LISTED_CASES`] case IDs, each with its title when the
/// default audience may see it, and how many were omitted. A case that does
/// not verify is listed by ID alone; selecting it reports why.
fn listed_cases(store: &mut Store, cases: &[Id]) -> String {
    let policy = AudiencePolicy::default();
    let mut parts: Vec<String> = cases
        .iter()
        .take(MAX_LISTED_CASES)
        .map(|&case| {
            let Ok(replay) = store.case(case) else {
                return case.to_string();
            };
            let Some(record) = replay.state.current(case) else {
                return case.to_string();
            };
            if !Classifier::new(&replay.state).may_show(record, Field::Title, &policy) {
                return format!("{case} (title restricted)");
            }
            match record.body.get("title").and_then(|title| title.as_str()) {
                Some(title) => {
                    let mut shown: String = title.chars().take(MAX_LISTED_TITLE_CHARS).collect();
                    if title.chars().count() > MAX_LISTED_TITLE_CHARS {
                        shown.push('…');
                    }
                    format!("{case} {shown:?}")
                }
                None => case.to_string(),
            }
        })
        .collect();
    if let Some(omitted) = cases.len().checked_sub(MAX_LISTED_CASES).filter(|n| *n > 0) {
        parts.push(format!(
            "and {omitted} more (error.actual lists every case ID)"
        ));
    }
    parts.join(", ")
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

/// A `--case` given to a command whose input already names its case (a
/// capture handle, a vault path) must select that case: a mismatch is
/// refused, never silently overridden. `owner` says what named the case.
pub(crate) fn require_selected_case(
    store: &mut Store,
    selector: Option<&str>,
    case: Id,
    owner: &str,
) -> Result<(), Failure> {
    let Some(selector) = selector else {
        return Ok(());
    };
    let selected = match selector.parse::<Id>() {
        Ok(id) if id.kind() == Kind::Case => id,
        _ => select_case(store, Some(selector))?,
    };
    if selected != case {
        let mut failure = Failure::new(
            ErrorCode::InvalidInput,
            format!(
                "--case must name {case}, the case of {owner}. Omit --case: {owner} names its case."
            ),
        )
        .at("--case");
        failure.error.expected = Some(json!(case));
        failure.error.actual = Some(json!(selected));
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
