//! Local command-line frontend.

mod evidence;
mod schema;
mod selection;
mod views;

use std::{
    env,
    ffi::OsString,
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{ColorChoice, Parser, Subcommand, ValueEnum, error::ErrorKind};
use investigation_core::{
    PROGRAM_NAME,
    command::Command as RecordCommand,
    command::RevisionRef,
    header::Actor,
    identity::{ResolveError, resolve_prefix},
    input::MAX_REQUEST_BYTES,
    wire::{CommitStatus, ErrorCode, Failure, Guidance, GuidanceKind, Receipt, ResponseVersion},
};
use investigation_store::{Store, Workspace, store::ReceiptLookup, workspace::WorkspaceError};
use serde::Serialize;
use serde_json::{Value, json};

use selection::{
    open_workspace, require_case_agreement, select_actor, workspace_argv, workspace_failure,
};
use views::{BriefInputs, BriefLevel, ViewDepth};

const FIRST_STEPS: &str = "\
Use investigation when a question needs multiple observations, a comparison, or a handoff.
The workspace preserves evidence and attributed revisions. Writes return a durable receipt.

  investigation init ./casework --format json
  investigation schema case.create --format json
  investigation --workspace ./casework record --input case.json --format json
  investigation --workspace ./casework receipt KEY --actor ACTOR --format json

Use schema record for the transaction envelope and schema --list for operations.";

#[derive(Parser)]
#[command(
    name = PROGRAM_NAME,
    version,
    about = "Preserve evidence, test explanations, and evaluate options.",
    after_help = FIRST_STEPS,
    color = ColorChoice::Never,
    arg_required_else_help = true
)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Args)]
pub(crate) struct Global {
    /// Open this workspace instead of discovering the nearest investigation.toml.
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) workspace: Option<PathBuf>,
    /// Select a case by full ID or unique case_ prefix.
    #[arg(long, global = true, value_name = "ID")]
    pub(crate) case: Option<String>,
    /// Attribute actions to this actor, or use workspace settings or AGENT.
    #[arg(long, global = true, value_name = "ACTOR")]
    pub(crate) actor: Option<Actor>,
    /// Select text or a single machine-readable JSON object.
    #[arg(
        long,
        global = true,
        value_enum,
        value_name = "FORMAT",
        overrides_with = "json"
    )]
    format: Option<Format>,
    #[arg(long, global = true, hide = true, overrides_with = "format")]
    json: bool,
    /// Reject commands that write workspace data.
    #[arg(long, global = true)]
    read_only: bool,
    /// Suppress terminal colors.
    #[arg(long, global = true)]
    no_color: bool,
    /// Write directly to stdout.
    #[arg(long, global = true)]
    no_pager: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Text,
    Json,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a local workspace and an optional Obsidian vault.
    #[command(after_help = "Example: investigation init ./casework --format json")]
    Init {
        #[arg(value_name = "PATH")]
        path: PathBuf,
        /// Skip creation of the optional Obsidian vault.
        #[arg(long)]
        no_vault: bool,
    },
    /// Submit one atomic transaction using the operation catalog.
    #[command(
        after_help = "Run investigation schema record for the envelope. Run investigation schema --list for operations.\nExample: investigation record --input transaction.json --format json"
    )]
    Record {
        /// Read from this file, or use - to read stdin.
        #[arg(long, value_name = "FILE|-")]
        input: PathBuf,
    },
    /// Preserve original local bytes and return their acquisition IDs.
    Ingest {
        /// Read the manifest from FILE, or - for a path-only manifest on stdin.
        #[arg(long, value_name = "FILE|-")]
        input: PathBuf,
    },
    /// Open an exploratory case and retain one attachment atomically.
    Start {
        question: String,
        #[arg(long, value_name = "FILE|-")]
        attach: PathBuf,
        #[arg(long, value_name = "KEY")]
        command_id: String,
    },
    /// Inspect retained source material explicitly.
    #[command(subcommand)]
    Evidence(evidence::EvidenceCommand),
    /// Discover operation schemas, examples, and repairs.
    #[command(
        after_help = "Examples:\n  investigation schema --list --format json\n  investigation schema case.create --format json\n  investigation schema record --schema-only --format json"
    )]
    Schema(schema::Args),
    /// Look up a command without resubmitting its original input.
    #[command(
        after_help = "Use the original command ID and actor.\nExample: investigation receipt create-case-1 --actor agent:investigator --format json"
    )]
    Receipt {
        #[arg(value_name = "KEY")]
        key: String,
    },
    /// Read a case record at title, summary, or full depth.
    Show {
        id: String,
        #[arg(long, value_enum, default_value = "summary")]
        depth: ViewDepth,
        /// Inspect an exact historical revision.
        #[arg(long, value_name = "REVISION")]
        at: Option<u64>,
    },
    /// Read a bounded handoff from the verified case snapshot.
    Brief {
        #[arg(long, value_enum, default_value = "standard")]
        level: BriefLevel,
        #[arg(long, default_value_t = 16_384)]
        max_bytes: usize,
    },
    /// Verify the journal, blobs, and index without repairing them.
    #[command(after_help = "Example: investigation check --format json")]
    Check,
    /// Rebuild derived workspace data.
    #[command(subcommand)]
    Repair(RepairCommand),
}

#[derive(Subcommand)]
enum RepairCommand {
    /// Rebuild the disposable index from the verified journal.
    #[command(
        after_help = "Run investigation check before and after rebuilding.\nExample: investigation repair index --format json"
    )]
    Index,
}

pub(crate) struct Output {
    value: Value,
    text: String,
    exit: u8,
}

impl Output {
    pub(crate) fn new(value: impl Serialize, text: String) -> Self {
        Self {
            value: serde_json::to_value(value).expect("CLI response serializes"),
            text,
            exit: 0,
        }
    }

    pub(crate) fn success(result: &str, mut value: Value, text: String) -> Self {
        let object = value.as_object_mut().expect("CLI response is an object");
        object.insert("schema_version".into(), json!(ResponseVersion::Alpha));
        object.insert("ok".into(), json!(true));
        object.insert("result".into(), json!(result));
        Self::new(value, text)
    }

    fn failure(failure: Failure) -> Self {
        let exit = failure.exit_code();
        let mut text = failure.to_string();
        if let Some(repair) = &failure.error.repair {
            if let Some(argv) = &repair.argv {
                text.push_str(&format!("\nRetry or inspect with {}", display_argv(argv)));
            }
            if let Some(schema) = &repair.schema {
                text.push_str(&format!("\nSee investigation schema {schema}"));
            }
        }
        Self {
            exit,
            ..Self::new(failure, text)
        }
    }
}

/// Read presentation switches before clap validates the rest of the command.
/// Stop at the positional separator so an operand named --json stays an operand.
fn requested_format(args: &[OsString]) -> Format {
    let mut format = match env::var("FORMAT").as_deref() {
        Ok("json") => Format::Json,
        _ => Format::Text,
    };
    let mut args = args.iter().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--") => break,
            Some("--json" | "--format=json") => format = Format::Json,
            Some("--format=text") => format = Format::Text,
            Some("--format") => match args.peek().and_then(|arg| arg.to_str()) {
                Some("json") => {
                    args.next();
                    format = Format::Json;
                }
                Some("text") => {
                    args.next();
                    format = Format::Text;
                }
                _ => {}
            },
            _ => {}
        }
    }
    format
}

fn dispatch(cli: Cli) -> Result<Output, Failure> {
    let global = cli.global;
    if global.read_only
        && matches!(
            cli.command,
            Commands::Init { .. }
                | Commands::Record { .. }
                | Commands::Ingest { .. }
                | Commands::Start { .. }
                | Commands::Repair(_)
        )
    {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "This command requires write access. Remove --read-only to run it.",
        )
        .at("--read-only"));
    }
    match cli.command {
        Commands::Schema(args) => schema::describe(args, &global),
        Commands::Init { path, no_vault } => init(&path, !no_vault, global.workspace.as_deref()),
        command => {
            let mut store = Store::open(open_workspace(global.workspace.as_deref())?);
            match command {
                Commands::Record { input } => record(&mut store, &global, &input),
                Commands::Ingest { input } => evidence::ingest(&mut store, &global, &input),
                Commands::Start {
                    question,
                    attach,
                    command_id,
                } => evidence::start(&mut store, &global, &question, &attach, &command_id),
                Commands::Evidence(command) => evidence::read(&mut store, &global, command),
                Commands::Receipt { key } => lookup_receipt(&mut store, &global, &key),
                Commands::Show { id, depth, at } => show(&mut store, &global, &id, depth, at),
                Commands::Brief { level, max_bytes } => {
                    brief(&mut store, &global, level, max_bytes)
                }
                Commands::Check => check(&mut store),
                Commands::Repair(RepairCommand::Index) => {
                    let cases = store.repair_index()?;
                    let text = format!("index  rebuilt  {} case(s)", cases.len());
                    Ok(Output::success(
                        "index_rebuilt",
                        json!({ "cases": cases }),
                        text,
                    ))
                }
                Commands::Schema(_) | Commands::Init { .. } => {
                    unreachable!("workspace-independent command")
                }
            }
        }
    }
}

fn show(
    store: &mut Store,
    global: &Global,
    selector: &str,
    depth: ViewDepth,
    at: Option<u64>,
) -> Result<Output, Failure> {
    let case = selection::select_case(store, global.case.as_deref())?;
    let replay = store.case(case)?;
    let id = resolve_prefix(
        selector,
        replay.state.records().map(|record| record.header.id),
    )
    .map_err(|error| match error {
        ResolveError::InvalidPrefix => Failure::new(
            ErrorCode::InvalidInput,
            "Supply an ID or unique typed prefix.",
        )
        .at("/id"),
        ResolveError::NotFound => Failure::new(
            ErrorCode::UnknownReference,
            "No record in this case matches that ID.",
        )
        .at("/id"),
        ResolveError::Ambiguous(candidates) => {
            let mut failure = Failure::new(
                ErrorCode::AmbiguousReference,
                "Several records match. Use a full ID.",
            )
            .at("/id");
            failure.error.actual = Some(json!(candidates));
            failure
        }
    })?;
    let record = match at {
        Some(revision) => replay.state.revision(RevisionRef { id, revision }),
        None => replay.state.current(id),
    };
    let effective = record
        .map(|record| views::effective_state(&replay, record))
        .unwrap_or_default();
    let projected = if at.is_some() {
        views::show_record_revision(&replay, id, at, depth, &effective)
    } else {
        views::show_record(&replay, id, depth, &effective)
    };
    let mut view =
        projected.map_err(|error| Failure::new(ErrorCode::UnknownReference, error.to_string()))?;
    if depth == ViewDepth::Full
        && at.is_none()
        && let Some(record) = record
    {
        if let Some(basis) = views::review_basis(&replay, record) {
            view["review_basis"] = basis;
        }
        if id.kind() == investigation_core::identity::Kind::Question {
            view["pareto"] = json!(investigation_core::decision_ops::pareto(
                &replay.state,
                case,
                id
            )?);
        }
    }
    if depth == ViewDepth::Full {
        let text = serde_json::to_string_pretty(&view).expect("full view serializes");
        return Ok(Output::success("record_shown", view, text));
    }
    let text = format!(
        "{}  {}\n{}",
        id,
        view["title"].as_str().unwrap_or(""),
        view["summary"].as_str().unwrap_or("")
    );
    Ok(Output::success("record_shown", view, text))
}

fn brief(
    store: &mut Store,
    global: &Global,
    level: BriefLevel,
    max_bytes: usize,
) -> Result<Output, Failure> {
    let case = selection::select_case(store, global.case.as_deref())?;
    let replay = store.case(case)?;
    let mut argv = workspace_argv(store.workspace());
    argv.extend([
        "--case".into(),
        case.to_string(),
        "show".into(),
        case.to_string(),
        "--depth".into(),
        "full".into(),
        "--format".into(),
        "json".into(),
    ]);
    let inputs = BriefInputs {
        next_actions: vec![json!({"kind":"local_read","argv":argv})],
        effective: replay
            .state
            .current(case)
            .map(|record| views::effective_state(&replay, record))
            .unwrap_or_default(),
        record_effective: replay
            .state
            .records()
            .filter(|record| {
                matches!(
                    record.header.id.kind(),
                    investigation_core::identity::Kind::Conclusion
                        | investigation_core::identity::Kind::Decision
                )
            })
            .map(|record| (record.header.id, views::effective_state(&replay, record)))
            .collect(),
        ..BriefInputs::default()
    };
    let view =
        views::brief(&replay, level, max_bytes.saturating_sub(1), &inputs).map_err(|error| {
            let mut failure =
                Failure::new(ErrorCode::BudgetTooSmall, error.to_string()).at("/max_bytes");
            failure.error.expected = Some(json!({"minimum_bytes":error.minimum_bytes + 1}));
            failure.error.actual = Some(json!({"requested_bytes":max_bytes}));
            failure
        })?;
    let text = serde_json::to_string_pretty(&view).expect("brief view serializes");
    Ok(Output::new(view, text))
}

fn init(path: &Path, vault: bool, override_path: Option<&Path>) -> Result<Output, Failure> {
    if override_path.is_some() {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "Pass the init destination as PATH.",
        )
        .at("--workspace"));
    }
    let (workspace, result) = match Workspace::init(path, vault) {
        Ok(workspace) => (workspace, "initialized"),
        Err(error @ WorkspaceError::WorkspaceAlreadyExists(_)) => {
            if path.join("investigation.toml").is_file() {
                (
                    Workspace::open(path).map_err(workspace_failure)?,
                    "already_initialized",
                )
            } else {
                return Err(workspace_failure(error));
            }
        }
        Err(error) => return Err(workspace_failure(error)),
    };
    let vault = workspace.config().vault_policy.enabled;
    let path = workspace.root().to_string_lossy();
    let mut next = workspace_argv(&workspace);
    next.extend(["schema", "case.create", "--format", "json"].map(str::to_owned));
    let text = format!(
        "{}  {result}  {path}\nstate/  canonical\ncache/  disposable\nlocal/  machine-local\n{}Next: {}",
        workspace.id(),
        if vault {
            "vault/  optional projection\n"
        } else {
            ""
        },
        display_argv(&next)
    );
    Ok(Output::success(
        result,
        json!({
            "workspace": path,
            "workspace_id": workspace.id(),
            "genesis_sequence": workspace.sequence(),
            "layout": {
                "canonical": "state/",
                "disposable": "cache/",
                "machine_local": "local/",
                "projection": vault.then_some("vault/"),
            },
            "next": [{ "kind": "local_read", "argv": next }]
        }),
        text,
    ))
}

pub(crate) fn read_input(path: &Path) -> Result<Vec<u8>, Failure> {
    let read = |error| {
        Failure::new(
            ErrorCode::IoFailure,
            format!("Read input {}: {error}", path.display()),
        )
        .at("--input")
    };
    let mut bytes = Vec::new();
    let limit = MAX_REQUEST_BYTES as u64 + 1;
    if path == Path::new("-") {
        io::stdin()
            .lock()
            .take(limit)
            .read_to_end(&mut bytes)
            .map_err(read)?;
    } else {
        File::open(path)
            .map_err(read)?
            .take(limit)
            .read_to_end(&mut bytes)
            .map_err(read)?;
    }
    Ok(bytes)
}

fn record(store: &mut Store, global: &Global, input: &Path) -> Result<Output, Failure> {
    let input = read_input(input)?;
    let (command, _) = RecordCommand::parse(&input)?;
    select_actor(
        store.workspace(),
        global.actor.as_ref(),
        Some(&command.actor),
    )?;
    require_case_agreement(store, global.case.as_deref(), command.case_id)?;
    let mut receipt = store
        .record(&input)
        .map_err(|failure| with_workspace_repair(store.workspace(), failure))?;
    if receipt.next.is_empty()
        && let (Some(case), Some(change)) = (command.case_id, receipt.updated.first())
    {
        receipt.next.push(Guidance {
            kind: GuidanceKind::LocalRead,
            argv: [
                PROGRAM_NAME.to_owned(),
                "show".to_owned(),
                change.id.to_string(),
                "--case".to_owned(),
                case.to_string(),
                "--format".to_owned(),
                "json".to_owned(),
            ]
            .into(),
        });
    }
    Ok(receipt_output(store.workspace(), receipt))
}

fn lookup_receipt(store: &mut Store, global: &Global, key: &str) -> Result<Output, Failure> {
    let actor = select_actor(store.workspace(), global.actor.as_ref(), None)?;
    if key.is_empty() || key.chars().count() > 128 || key.chars().any(char::is_control) {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            "Use a command ID with 1–128 characters and no control characters.",
        )
        .at("/command_id"));
    }
    let selected = global
        .case
        .as_deref()
        .map(|selector| selection::select_case(store, Some(selector)))
        .transpose()
        .map_err(|failure| receipt_read_failure(store.workspace(), failure))?;
    match store
        .receipt(&actor, key)
        .map_err(|failure| receipt_read_failure(store.workspace(), failure))?
    {
        ReceiptLookup::Found { receipt } => {
            if let Some(case) = selected {
                let replay = store
                    .case(case)
                    .map_err(|failure| receipt_read_failure(store.workspace(), failure))?;
                if !replay
                    .events
                    .iter()
                    .any(|event| event.event.transaction_id == receipt.transaction_id)
                {
                    return Err(Failure::new(
                        ErrorCode::UnknownReference,
                        "The command receipt belongs to a different case.",
                    )
                    .at("/case_id"));
                }
            }
            Ok(receipt_output(store.workspace(), *receipt))
        }
        ReceiptLookup::NotFoundAtSnapshot { cases } => Ok(Output::success(
            "not_found_at_snapshot",
            json!({ "command_id": key, "actor": actor, "cases": cases }),
            format!("{key}  not_found_at_snapshot"),
        )),
    }
}

fn receipt_read_failure(workspace: &Workspace, mut failure: Failure) -> Failure {
    if failure.exit_code() >= 6 {
        failure.commit_status = CommitStatus::Unknown;
    }
    with_workspace_repair(workspace, failure)
}

pub(crate) fn receipt_output(workspace: &Workspace, mut receipt: Receipt) -> Output {
    if receipt.next.is_empty() {
        receipt.next.push(Guidance {
            kind: GuidanceKind::LocalRead,
            argv: [PROGRAM_NAME, "check", "--format", "json"]
                .map(str::to_owned)
                .to_vec(),
        });
    }
    for next in &mut receipt.next {
        scope_argv(workspace, &mut next.argv);
    }
    let mut text = receipt.transaction_id.to_string();
    for change in receipt.created.iter().chain(&receipt.updated) {
        text.push_str(&format!("\n{}  revision {}", change.id, change.revision));
    }
    for warning in &receipt.warnings {
        text.push_str(&format!("\nWarning: {warning}"));
    }
    if let Some(next) = receipt.next.first() {
        text.push_str(&format!("\nNext: {}", display_argv(&next.argv)));
    }
    Output::new(receipt, text)
}

fn with_workspace_repair(workspace: &Workspace, mut failure: Failure) -> Failure {
    if let Some(argv) = failure
        .error
        .repair
        .as_mut()
        .and_then(|repair| repair.argv.as_mut())
    {
        scope_argv(workspace, argv);
    }
    failure
}

fn scope_argv(workspace: &Workspace, argv: &mut Vec<String>) {
    if argv.first().is_some_and(|program| program == PROGRAM_NAME)
        && !argv.get(1).is_some_and(|arg| arg == "--workspace")
    {
        if argv.get(1).is_some_and(|command| command == "receipt")
            && argv.get(2).is_some_and(|key| key.starts_with('-'))
        {
            let key = argv.remove(2);
            argv.extend(["--".to_owned(), key]);
        }
        argv.splice(0..1, workspace_argv(workspace));
    }
}

fn display_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|part| {
            if !part.is_empty()
                && part.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'_' | b'/' | b'-')
                })
            {
                part.clone()
            } else {
                format!("'{}'", part.replace('\'', "'\"'\"'"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn check(store: &mut Store) -> Result<Output, Failure> {
    let mut report = store.check()?;
    let integrity_ok = report.integrity_ok();
    let mut text = format!(
        "{}  {} case(s)  {} verified blob(s)",
        if integrity_ok {
            "verified"
        } else {
            "integrity failure"
        },
        report.heads.len(),
        report.verified_blobs,
    );
    for issue in &mut report.issues {
        text.push_str(&format!("\n{}  {}", issue.code, issue.reason));
        if let Some(argv) = &mut issue.repair {
            scope_argv(store.workspace(), argv);
            text.push_str(&format!("\nRepair: {}", display_argv(argv)));
        }
    }
    let report = serde_json::to_value(report).expect("check report serializes");
    let mut output = Output::success("checked", report, text);
    output.value["ok"] = json!(integrity_ok);
    if !integrity_ok {
        output.exit = ErrorCode::IntegrityFailure.exit_code();
    }
    Ok(output)
}

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().collect();
    let format = requested_format(&args);
    let output = match Cli::try_parse_from(args) {
        Ok(cli) => dispatch(cli).unwrap_or_else(Output::failure),
        Err(error) => match error.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                let text = error.to_string();
                Output::success("help", json!({ "help": text }), text)
            }
            ErrorKind::DisplayVersion => Output::success(
                "version",
                json!({ "program": PROGRAM_NAME, "version": env!("CARGO_PKG_VERSION") }),
                error.to_string(),
            ),
            _ => Output::failure(
                Failure::new(ErrorCode::InvalidInput, error.to_string()).with_argv(
                    [PROGRAM_NAME, "--help", "--format", "json"]
                        .map(str::to_owned)
                        .to_vec(),
                ),
            ),
        },
    };

    let text = output.text.trim_end();
    let result = match format {
        Format::Json => writeln!(io::stdout().lock(), "{}", output.value),
        Format::Text if output.exit != 0 && output.value.get("error").is_some() => {
            writeln!(io::stderr().lock(), "{text}")
        }
        Format::Text => writeln!(io::stdout().lock(), "{text}"),
    };
    match result {
        Ok(()) => ExitCode::from(output.exit),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::from(output.exit),
        Err(_) => ExitCode::from(ErrorCode::IoFailure.exit_code()),
    }
}
