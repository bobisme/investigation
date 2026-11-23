//! Local command-line frontend.

mod blindspots;
mod brief;
mod brief_delta;
mod calibration;
mod capture;
mod capture_schema;
mod config;
mod docs;
mod doctor;
mod dry_run;
mod evidence;
mod explain;
mod export;
mod exposure;
mod first_use;
mod frontier;
mod guide;
#[cfg(test)]
mod help_lint;
mod help_teach;
mod inherited_trust;
mod integration;
mod lessons;
mod lineage;
mod logging;
mod next;
mod onboard;
mod read_only;
mod retrospective;
mod review_packet;
mod schema;
mod search;
mod selection;
mod shortcuts;
mod snapshot;
mod synopsis;
mod vault;
mod vault_copy_map;
mod vault_reconcile;
mod vault_remove;
mod view_text;
mod views;

use std::{
    env,
    ffi::OsString,
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{ColorChoice, FromArgMatches, Parser, Subcommand, ValueEnum, error::ErrorKind};
use investigation_core::{
    PROGRAM_NAME,
    authored_text::escape_for_terminal,
    command::Command as RecordCommand,
    command::RevisionRef,
    freshness::FreshnessIndex,
    gap_rules::{enumerate_gaps, rank_next},
    header::Actor,
    identity::{ResolveError, resolve_prefix},
    input::MAX_REQUEST_BYTES,
    wire::{
        CommitResult, CommitStatus, ErrorCode, Failure, Guidance, GuidanceKind, Receipt,
        ResponseVersion,
    },
};
use investigation_store::{Store, Workspace, store::ReceiptLookup, workspace::WorkspaceError};
use serde::Serialize;
use serde_json::{Value, json};

use brief::BriefInputs;
use selection::{
    open_workspace, require_case_agreement, select_actor, workspace_argv, workspace_failure,
};
use views::ViewDepth;

const FIRST_STEPS: &str = "\
Use investigation when a question needs multiple observations, a comparison, or a handoff.
The workspace preserves evidence and attributed revisions. Writes return a durable receipt.

Examples:
Start a case from a local file in the working directory:
  investigation init ./casework --actor agent:investigator --no-vault --import-root . --format json
  investigation --workspace ./casework --actor agent:investigator start 'Why did requests time out?' --attach ./results.log --format json
On this machine, later writes that omit --actor are attributed to the actor init --actor recorded.

Continue, record and hand off:
  investigation onboard --format json
  investigation docs agents
  investigation --workspace ./casework brief --format json
  investigation --workspace ./casework next --format json
  investigation --workspace ./casework case new 'Request latency' --format json
  investigation --workspace ./casework source add ./results.log --case <ID> --format json
  investigation schema case.create --format json
  investigation --workspace ./casework observe 'The log reports 28 seconds' --source <ARTIFACT_ID> --quote 'waited 28 seconds' --format json
  investigation --workspace ./casework note 'Check the clock source' --on <RECORD_ID> --format json
  investigation --workspace ./casework record --input case.json --format json
  investigation --workspace ./casework receipt KEY --actor ACTOR --format json

Use schema record for the transaction envelope and schema --list for operations.
Generated shortcut command IDs appear on stderr before writes; reconcile with receipt KEY.
Use --print-envelope to inspect a shortcut request without writing.
Exit codes: 0 success, 2 invalid input, 3 missing workspace, case or reference, 4 conflict,
5 gate refused, 6 integrity failure, 7 busy, IO or unknown durability. With --format json
the failure names its cause in error.code and its retry in error.repair.";

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
    /// Self-reported session label for evidence reads and writes, or INVESTIGATION_SESSION.
    #[arg(long, global = true, value_name = "LABEL")]
    pub(crate) session: Option<investigation_core::inherited_trust::Session>,
    /// Select text or a single machine-readable JSON object; pretty prints the text form.
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
    /// Reject commands that write or export; INVESTIGATION_READ_ONLY=1 does the same.
    // Enforced from the parsed matches by `read_only::refusal` before dispatch.
    #[arg(long, global = true)]
    #[allow(dead_code)]
    read_only: bool,
    /// Accepted for compatibility; output is never colored.
    #[arg(long, global = true)]
    no_color: bool,
    /// Accepted for compatibility; output is never paged.
    #[arg(long, global = true)]
    no_pager: bool,
    /// Diagnostic logs on stderr: -v info, -vv debug, -vvv trace. INVESTIGATION_LOG sets a level.
    #[arg(short = 'v', long = "verbose", global = true, action = clap::ArgAction::Count)]
    #[allow(dead_code)]
    verbose: u8,
    /// Only log errors on stderr.
    #[arg(short = 'q', long, global = true)]
    #[allow(dead_code)]
    quiet: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Text,
    Json,
    // Renders as text: it is already the human-readable form and carries no color.
    Pretty,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a local workspace and an optional Markdown vault.
    #[command(
        after_help = "Example: investigation init ./casework --actor agent:investigator --no-vault --import-root . --format json\n--actor records ACTOR in local/actor.toml as this machine's actor for writes in the workspace that name no actor. --actor on a write and AGENT take precedence."
    )]
    Init {
        /// New or empty directory for the workspace, or an existing workspace; --workspace PATH also works.
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
        /// Skip creation of the optional Markdown vault.
        #[arg(long)]
        no_vault: bool,
        /// Let the workspace retain copies of files beneath DIR (read-only; repeatable).
        /// On an existing workspace PATH, adds DIR to its import roots.
        #[arg(long = "import-root", value_name = "DIR")]
        import_roots: Vec<PathBuf>,
    },
    /// Submit one atomic transaction using the operation catalog.
    #[command(
        after_help = "Run investigation schema record for the envelope. Run investigation schema --list for operations.\nExamples:\n  investigation record --input transaction.json --format json\n  investigation record --input transaction.json --dry-run --format json\n--dry-run checks the transaction against the current snapshot like a commit and writes nothing."
    )]
    Record {
        /// Read from this file, or use - to read stdin.
        #[arg(long, value_name = "FILE|-")]
        input: PathBuf,
        #[arg(long, help = dry_run::HELP)]
        dry_run: bool,
    },
    /// Preserve original local bytes and return their acquisition IDs.
    Ingest {
        /// Read the JSON manifest from FILE, or - for stdin; investigation schema ingest shows it.
        #[arg(long, value_name = "FILE|-")]
        input: PathBuf,
        #[arg(long, help = dry_run::HELP)]
        dry_run: bool,
    },
    /// Open an exploratory case and retain one attachment atomically.
    #[command(
        after_help = "Examples:\n  investigation --workspace ./casework --actor agent:investigator start 'Why did requests time out?' --attach ./results.log --format json\n  investigation --workspace ./casework --actor agent:investigator start 'Why did requests time out?' --attach ./results.log --dry-run --format json\nFILE must lie beneath an import root of the workspace; use - to read the attachment from stdin.\nWithout --actor or AGENT, the case is attributed to the actor init --actor recorded on this machine."
    )]
    Start {
        /// The question the case investigates.
        question: String,
        /// Local file to retain as the first source, or - for stdin.
        #[arg(long, value_name = "FILE|-")]
        attach: PathBuf,
        /// Stable idempotency key; generated if omitted.
        #[arg(long, value_name = "KEY")]
        command_id: Option<String>,
        #[arg(long, help = dry_run::HELP)]
        dry_run: bool,
    },
    /// Inspect retained source material explicitly.
    #[command(subcommand)]
    Evidence(evidence::EvidenceCommand),
    /// Record an external producer's run through the inv.capture.v1 protocol.
    #[command(subcommand)]
    Capture(capture::CaptureCommand),
    /// Discover operation schemas, examples, and repairs.
    #[command(
        after_help = "Examples:\n  investigation schema --list --format json\n  investigation schema case.create --format json\n  investigation schema record --schema-only --format json"
    )]
    Schema(schema::Args),
    /// Learn the model and inspect the next valid local action.
    Onboard,
    /// Print embedded offline docs: the agent instructions or the optional skill.
    #[command(after_help = docs::AFTER_HELP)]
    Docs(docs::Args),
    /// Rank up to three snapshot-bound investigative gaps.
    Next {
        /// Most gaps to return, from 1 to 3.
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=3))]
        limit: u8,
    },
    /// List every snapshot-bound gap with filters, paging and rule explanations.
    #[command(after_help = frontier::AFTER_HELP)]
    Frontier(frontier::Args),
    /// Audit recorded structure for gaps; never a completeness certificate.
    #[command(after_help = blindspots::AFTER_HELP)]
    Blindspots(blindspots::Args),
    /// Print one method card from the built-in pack or a pinned pack.
    #[command(after_help = guide::GUIDE_AFTER_HELP)]
    Guide(guide::GuideArgs),
    /// List method packs and their cards.
    #[command(subcommand)]
    Method(guide::MethodCommand),
    /// Look up a command without resubmitting its original input.
    #[command(
        after_help = "Use the original command ID and actor.\nExample: investigation receipt create-case-1 --actor agent:investigator --format json"
    )]
    Receipt {
        /// Command ID (key) of the original write.
        #[arg(value_name = "KEY")]
        key: String,
    },
    /// Read a case record at title, summary, or full depth.
    Show {
        /// Record to read: a full ID or a unique typed prefix.
        id: String,
        /// How much of the record to print.
        #[arg(long, value_enum, default_value = "summary")]
        depth: ViewDepth,
        /// Inspect an exact historical revision.
        #[arg(long, value_name = "REVISION")]
        at: Option<u64>,
        #[command(flatten)]
        snapshot: snapshot::SnapshotArgs,
    },
    /// Read a bounded handoff from the verified case snapshot.
    #[command(after_help = brief::AFTER_HELP)]
    Brief(brief::Args),
    /// Walk the recorded evidence and assessment chain behind one record.
    #[command(after_help = explain::AFTER_HELP)]
    Explain(explain::Args),
    /// Read a blinded review packet: question, evidence and criteria, no conclusion.
    #[command(after_help = review_packet::AFTER_HELP)]
    ReviewPacket(review_packet::Args),
    /// Read a case's retrospectives and lessons, its process digest, or admitted lessons.
    #[command(after_help = retrospective::AFTER_HELP)]
    Retrospective(retrospective::Args),
    /// Brier calibration of resolved likelihoods by self-reported model family.
    #[command(after_help = calibration::AFTER_HELP)]
    Calibration(calibration::Args),
    /// Find records by text; results are labelled pointers, not evidence.
    #[command(after_help = search::AFTER_HELP)]
    Search(search::Args),
    /// Diagnose where exposed text lives in this workspace; removes nothing.
    #[command(after_help = exposure::AFTER_HELP)]
    Exposure(exposure::Args),
    /// Verify the journal, blobs, and index without repairing them.
    #[command(after_help = "Example: investigation check --format json")]
    Check,
    /// Rebuild derived workspace data or retire settled capture staging.
    #[command(subcommand)]
    Repair(RepairCommand),
    /// Publish the working vault and inspect its health and drift.
    #[command(subcommand, after_help = vault::AFTER_HELP)]
    Vault(vault::VaultCommand),
    /// Write an immutable report or bundle of one case at one verified snapshot to a new directory.
    #[command(after_help = export::after_help())]
    Export(export::Args),
    /// Read the effective workspace settings and where each value comes from; never writes.
    #[command(subcommand, after_help = config::AFTER_HELP)]
    Config(config::ConfigCommand),
    /// Report versions, capabilities, limits, filesystem support and workspace health.
    #[command(after_help = doctor::AFTER_HELP)]
    Doctor(doctor::Args),
    /// Show, apply or remove the agent instructions in a file you name; never implicit.
    #[command(subcommand, after_help = integration::AFTER_HELP)]
    Integration(integration::IntegrationCommand),
    /// Print the program version; the same as --version.
    // Hidden: agents guess `version` before `--version`, so accept it quietly.
    #[command(hide = true)]
    Version,
}

#[derive(Subcommand)]
enum RepairCommand {
    /// Rebuild the disposable index from the verified journal.
    #[command(
        after_help = "Run investigation check before and after rebuilding.\nExample: investigation repair index --format json"
    )]
    Index,
    /// Remove capture staging once its Run is finalized and its bytes are canonical.
    #[command(after_help = capture::RETIRE_HELP)]
    Staging {
        /// Retire this handle's staging and report it in any state.
        #[arg(long, value_name = "HANDLE")]
        handle: Option<String>,
    },
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
        object.insert("schema_version".into(), json!(ResponseVersion::V1));
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
    // `pretty` renders as text: there is no separate terminal rendering, and
    // a terminal and a pipe therefore receive the same output.
    let mut format = match env::var("FORMAT").as_deref() {
        Ok("json") => Format::Json,
        _ => Format::Text,
    };
    let mut args = args.iter().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--") => break,
            Some("--json" | "--format=json") => format = Format::Json,
            Some("--format=text" | "--format=pretty") => format = Format::Text,
            Some("--format") => match args.peek().and_then(|arg| arg.to_str()) {
                Some("json") => {
                    args.next();
                    format = Format::Json;
                }
                Some("text" | "pretty") => {
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
    match cli.command {
        Commands::Schema(args) => schema::describe(args, &global),
        Commands::Init {
            path,
            no_vault,
            import_roots,
        } => init(path.as_deref(), !no_vault, &import_roots, &global),
        Commands::Onboard => onboard::run(&global),
        Commands::Docs(args) => docs::run(args),
        Commands::Guide(args) => guide::show(args, &global),
        Commands::Method(command) => guide::method(command, &global),
        Commands::Doctor(args) => doctor::run(args, &global),
        Commands::Config(command) => config::run(&global, command),
        Commands::Integration(command) => integration::run(command),
        Commands::Version => Ok(version_output(format!(
            "{PROGRAM_NAME} {}",
            env!("CARGO_PKG_VERSION")
        ))),
        command => {
            let mut store = Store::open(open_workspace(global.workspace.as_deref())?);
            match command {
                Commands::Record { input, dry_run } => {
                    dry_run::run(&mut store, dry_run, |store| record(store, &global, &input))
                }
                Commands::Ingest { input, dry_run } => dry_run::run(&mut store, dry_run, |store| {
                    evidence::ingest(store, &global, &input)
                }),
                Commands::Start {
                    question,
                    attach,
                    command_id,
                    dry_run,
                } => dry_run::run(&mut store, dry_run, |store| {
                    evidence::start(store, &global, &question, &attach, command_id.as_deref())
                }),
                Commands::Evidence(command) => evidence::read(&mut store, &global, command),
                Commands::Capture(command) => capture::run(&mut store, &global, command),
                Commands::Receipt { key } => lookup_receipt(&mut store, &global, &key),
                Commands::Show {
                    id,
                    depth,
                    at,
                    snapshot,
                } => show(&mut store, &global, &id, depth, at, &snapshot),
                Commands::Brief(args) => brief(&mut store, &global, args),
                Commands::Explain(args) => explain::run(&mut store, &global, args),
                Commands::Next { limit } => next::run(&mut store, &global, usize::from(limit)),
                Commands::Frontier(args) => frontier::run(&mut store, &global, args),
                Commands::Blindspots(args) => blindspots::run(&mut store, &global, args),
                Commands::Search(args) => search::run(&mut store, &global, args),
                Commands::Retrospective(args) => retrospective::run(&mut store, &global, args),
                Commands::Calibration(args) => calibration::run(&mut store, &global, args),
                Commands::ReviewPacket(args) => review_packet::run(&mut store, &global, args),
                Commands::Check => check(&mut store),
                Commands::Exposure(args) => exposure::run(&mut store, &global, args),
                Commands::Vault(command) => vault::run(&mut store, &global, command),
                Commands::Export(args) => export::run(&mut store, &global, args),
                Commands::Repair(RepairCommand::Staging { handle }) => {
                    capture::retire(&mut store, handle.as_deref())
                }
                Commands::Repair(RepairCommand::Index) => {
                    let cases = store.repair_index()?;
                    let text = format!("index  rebuilt  {} case(s)", cases.len());
                    Ok(Output::success(
                        "index_rebuilt",
                        json!({ "cases": cases }),
                        text,
                    ))
                }
                Commands::Schema(_)
                | Commands::Init { .. }
                | Commands::Onboard
                | Commands::Docs(_)
                | Commands::Guide(_)
                | Commands::Method(_)
                | Commands::Doctor(_)
                | Commands::Config(_)
                | Commands::Integration(_)
                | Commands::Version => {
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
    pin: &snapshot::SnapshotArgs,
) -> Result<Output, Failure> {
    let case = selection::select_case(store, global.case.as_deref())?;
    let mut retry = workspace_argv(store.workspace());
    retry.extend([
        "--case".into(),
        case.to_string(),
        "show".into(),
        selector.into(),
        "--format".into(),
        "json".into(),
    ]);
    let pinned = snapshot::resolve(store.case(case)?, pin, retry)?;
    let replay = pinned.read();
    let today = pinned.today();
    let mut prefix = workspace_argv(store.workspace());
    prefix.extend(["--case".into(), case.to_string()]);
    let records = brief::follow_up(&prefix, &["brief"]);
    let id = resolve_prefix(
        selector,
        replay.state.records().map(|record| record.header.id),
    )
    .map_err(|error| match error {
        ResolveError::InvalidPrefix => Failure::new(
            ErrorCode::InvalidInput,
            "Supply an ID or unique typed prefix.",
        )
        .at("/id")
        .with_argv(records),
        ResolveError::NotFound => Failure::new(
            ErrorCode::UnknownReference,
            "No record in this case matches that ID.",
        )
        .at("/id")
        .with_argv(records),
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
    let index = FreshnessIndex::build(&replay.state, case);
    let effective = record
        .map(|record| views::effective_state_as_of(store, replay, &index, record, today))
        .unwrap_or_default();
    let mut view = views::show_record_revision(replay, today, id, at, depth, &effective)
        .map_err(|error| Failure::new(ErrorCode::UnknownReference, error.to_string()))?;
    if let Some(label) = pinned.label() {
        view["historical_read"] = label;
    }
    if view["content_state"] != "restricted"
        && let Some((key, origins)) =
            record.and_then(|record| views::origin_projection(replay, record))
    {
        view[key] = origins;
    }
    if view["content_state"] != "restricted"
        && at.is_none()
        && let Some(record) = record
    {
        synopsis::show_fields(
            &mut view,
            &replay.state,
            today.unwrap_or(&replay.state),
            &index,
            record,
            depth,
        );
        inherited_trust::synopsis_fields(&mut view, replay, depth);
    }
    if depth == ViewDepth::Full
        && at.is_none()
        && let Some(record) = record
    {
        if view["content_state"] != "restricted"
            && let Some(basis) = views::review_basis(replay, record)
        {
            view["review_basis"] = basis;
        }
        if id.kind() == investigation_core::identity::Kind::Question {
            view["pareto"] = json!(investigation_core::decision_ops::pareto(
                &replay.state,
                case,
                id
            )?);
            if let Some(analysis) =
                investigation_core::weighting::weighted_analysis(&replay.state, case, id)?
            {
                view["weighted_analysis"] = json!(analysis);
            }
        }
        if id.kind() == investigation_core::identity::Kind::Case {
            view["method_packs"] = json!(store.method_packs(&replay.state, case, None)?);
        }
    }
    let text = views::authored_text_lines(&mut view);
    let mut text = format!("{id}  {text}");
    text.push_str(&views::challenges_text(&view));
    text.push_str(&synopsis::text_line(&view));
    text.push_str(&inherited_trust::text(&view));
    text.push_str(&investigation_core::confidence::text(&view));
    text.push_str(&investigation_core::likelihood::text(&view));
    text.push_str(&investigation_core::review_independence::text(&view));
    text.push_str(&investigation_core::coverage::text(&view));
    if depth == ViewDepth::Full {
        let next = view_text::show_full_argv(&prefix, id, at, &replay.snapshot);
        let text = view_text::show_full(&view, &text, &next);
        return Ok(Output::success("record_shown", view, text));
    }
    Ok(Output::success("record_shown", view, text))
}

fn brief(store: &mut Store, global: &Global, args: brief::Args) -> Result<Output, Failure> {
    let case = selection::select_case(store, global.case.as_deref())?;
    let mut prefix = workspace_argv(store.workspace());
    prefix.extend(["--case".into(), case.to_string()]);
    let retry = brief::follow_up(&prefix, &["brief"]);
    let pinned = snapshot::resolve(store.case(case)?, &args.snapshot, retry)?;
    let replay = pinned.read();
    let today = pinned.today();
    let mut argv = prefix.clone();
    argv.extend([
        "show".into(),
        case.to_string(),
        "--depth".into(),
        "full".into(),
    ]);
    argv.extend(snapshot::pin_argv(&replay.snapshot));
    argv.extend(["--format".into(), "json".into()]);
    let inventory = enumerate_gaps(&replay.state, &replay.snapshot, case)?;
    let next = rank_next(&replay.state, &inventory, 3);
    let index = FreshnessIndex::build(&replay.state, case);
    let mut effective = replay
        .state
        .current(case)
        .map(|record| views::effective_state_as_of(store, replay, &index, record, today))
        .unwrap_or_default();
    let outcomes: Vec<_> = replay
        .state
        .records()
        .filter(|record| {
            matches!(
                record.header.id.kind(),
                investigation_core::identity::Kind::Conclusion
                    | investigation_core::identity::Kind::Decision
            )
        })
        .collect();
    let evidence = replay.state.records().filter(|record| {
        matches!(
            record.header.id.kind(),
            investigation_core::identity::Kind::Observation
                | investigation_core::identity::Kind::Comparison
        )
    });
    let record_effective: std::collections::BTreeMap<_, _> = outcomes
        .iter()
        .copied()
        .chain(evidence)
        .map(|record| {
            (
                record.header.id,
                views::effective_state_as_of(store, replay, &index, record, today),
            )
        })
        .collect();
    let primary = outcomes
        .iter()
        .filter(|record| {
            record.body["lifecycle"] != "superseded" && record.body["status"] != "superseded"
        })
        .max_by_key(|record| (record.header.acceptance_sequence, record.header.id));
    if let Some(primary) = primary {
        let overlay = &record_effective[&primary.header.id];
        effective.material_caveat = overlay.material_caveat.clone();
        effective.contradiction = overlay.contradiction.clone();
    }
    effective.gaps.push(json!({
        "snapshot":next.snapshot,
        "rule_version":next.rule_version,
        "counts":next.counts,
        "recommendations":next.recommendations.iter().map(|item| json!({
            "action_id":item.action_id,"gap_id":item.gap_id,"rule_id":item.rule_id,
            "subject_refs":item.subject_refs,"purpose":item.purpose,
            "status_prerequisites":item.prerequisites,"unresolved_fields":item.unresolved_fields,
            "schema_operation":item.schema_operation,"external_effects":item.external_effects,
            "outcome_branches":item.outcome_branches,"deferral_alternative":item.deferral_alternative
        })).collect::<Vec<_>>(),
        "remaining_recommendations":next.remaining_recommendations,
        "limits":next.limits
    }));
    let inputs = BriefInputs {
        effective,
        next_actions: vec![json!({"kind":"local_read","argv":argv,"snapshot":replay.snapshot})],
        record_effective,
        omissions: synopsis::brief_omission(
            &replay.state,
            today.unwrap_or(&replay.state),
            &index,
            &workspace_argv(store.workspace()),
            &snapshot::pin_argv(&replay.snapshot),
        )
        .into_iter()
        .collect(),
        primary_outcome: primary.map(|record| record.header.id),
        historical: pinned.label(),
        read_prefix: prefix.clone(),
        ..BriefInputs::default()
    };
    let budget = args.max_bytes.saturating_sub(1);
    let view = match &args.since {
        Some(since) => {
            let since = brief_delta::resolve_since(replay, since, &prefix)?;
            brief_delta::delta(store, replay, today, &since, args.level, budget, &inputs)
        }
        None => brief::brief(replay, today, args.level, budget, &inputs),
    }
    .map_err(|error| {
        let mut failure =
            Failure::new(ErrorCode::BudgetTooSmall, error.to_string()).at("/max_bytes");
        let minimum = error.minimum_bytes + 1;
        failure.error.expected = Some(json!({"minimum_bytes":minimum}));
        failure.error.actual = Some(json!({"requested_bytes":args.max_bytes}));
        let minimum = minimum.to_string();
        let mut tail = vec![
            "brief",
            "--level",
            args.level.as_str(),
            "--max-bytes",
            &minimum,
        ];
        if let Some(since) = &args.since {
            tail.extend(["--since", since]);
        }
        let pin = snapshot::pin_argv(&replay.snapshot);
        tail.extend(pin.iter().map(String::as_str));
        failure.with_argv(brief::follow_up(&prefix, &tail))
    })?;
    let text = view_text::brief(&view);
    Ok(Output::new(view, text))
}

fn init(
    path: Option<&Path>,
    vault: bool,
    import_roots: &[PathBuf],
    global: &Global,
) -> Result<Output, Failure> {
    let (path, flag) = first_use::init_destination(path, global.workspace.as_deref())?;
    let import_roots = import_roots
        .iter()
        .map(|root| {
            std::fs::canonicalize(root)
                .ok()
                .filter(|root| root.is_dir())
                .ok_or_else(|| {
                    let failure = Failure::new(
                        ErrorCode::InvalidInput,
                        format!(
                            "Import root {} is not an existing directory. Name the directory that holds the files.",
                            root.display()
                        ),
                    )
                    .at("--import-root");
                    match first_use::import_root_repair(root).and_then(|parent| {
                        first_use::with_replaced_value(env::args_os(), "--import-root", root, &parent)
                    }) {
                        Some(argv) => failure.with_argv(argv),
                        None => failure,
                    }
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut added = Vec::new();
    let (workspace, result) = match Workspace::init_with_import_roots(path, vault, &import_roots) {
        Ok(workspace) => (workspace, "initialized"),
        Err(error @ WorkspaceError::WorkspaceAlreadyExists(_)) => {
            if path.join("investigation.toml").is_file() {
                let mut workspace = Workspace::open(path).map_err(workspace_failure)?;
                added = workspace
                    .add_import_roots(&import_roots)
                    .map_err(|error| workspace_failure(error).at("--import-root"))?;
                let result = if added.is_empty() {
                    "already_initialized"
                } else {
                    "import_roots_added"
                };
                (workspace, result)
            } else {
                return Err(first_use::nonempty_init_repair(
                    workspace_failure(error),
                    path,
                    flag,
                    import_roots.is_empty(),
                ));
            }
        }
        Err(error @ WorkspaceError::InvalidImportRoot(_)) => {
            return Err(workspace_failure(error).at("--import-root"));
        }
        Err(error) => return Err(workspace_failure(error)),
    };
    let actor_setting = match &global.actor {
        Some(actor) => {
            let previous = workspace
                .set_local_actor(actor)
                .map_err(|error| workspace_failure(error).at("--actor"))?;
            Some((actor.clone(), previous))
        }
        None => None,
    };
    let local_actor = match &actor_setting {
        Some((actor, _)) => Some(actor.clone()),
        None => workspace.local_actor().map_err(workspace_failure)?,
    };
    let vault = workspace.config().vault_policy.enabled;
    let path = workspace.root().to_string_lossy();
    let configured_roots = &workspace.config().import_roots;
    let start = first_use::start_step(&path, local_actor.as_ref());
    let mut text = format!(
        "{}  {result}  {path}\nstate/  canonical\ncache/  disposable\nlocal/  machine-local\n{}",
        workspace.id(),
        if vault {
            "vault/  optional projection\n"
        } else {
            ""
        },
    );
    for root in configured_roots {
        let state = if added.contains(root) { "  added" } else { "" };
        text.push_str(&format!("import root  {}{state}\n", root.display()));
    }
    if let Some((actor, previous)) = &actor_setting {
        text.push_str(&first_use::actor_setting_text(actor, previous.as_ref()));
    }
    text.push_str(&first_use::start_text(&path, false, local_actor.as_ref()));
    if configured_roots.is_empty() {
        text.push_str(&format!(
            "\nNo import roots are configured, so FILE paths are refused. {}",
            first_use::import_root_advice(&workspace)
        ));
    }
    let mut value = json!({
        "workspace": path,
        "workspace_id": workspace.id(),
        "genesis_sequence": workspace.sequence(),
        "layout": {
            "canonical": "state/",
            "disposable": "cache/",
            "machine_local": "local/",
            "projection": vault.then_some("vault/"),
        },
        "import_roots": configured_roots,
        "added_import_roots": added,
        "next": [start]
    });
    if let Some((actor, previous)) = &actor_setting {
        value["actor_setting"] = first_use::actor_setting_value(actor, previous.as_ref());
    }
    Ok(Output::success(result, value, text))
}

pub(crate) fn read_input(path: &Path) -> Result<Vec<u8>, Failure> {
    let read = |error| {
        let hint = if path.to_string_lossy().trim_start().starts_with(['{', '[']) {
            " --input takes a file path; pass - and pipe inline JSON on stdin."
        } else {
            ""
        };
        Failure::new(
            ErrorCode::IoFailure,
            format!("Read input {}: {error}.{hint}", path.display()),
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
    let (mut command, _) = RecordCommand::parse(&input)?;
    let input = inherited_trust::declare_session(global, &mut command, input)?;
    select_actor(
        store.workspace(),
        global.actor.as_ref(),
        Some(&command.actor),
    )?;
    require_case_agreement(store, global.case.as_deref(), command.case_id)?;
    let mut receipt = store
        .record(&input)
        .map_err(|failure| record_repair(store, &command, failure))?;
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

fn is_extension_failure(failure: &Failure) -> bool {
    failure.error.path.as_deref().is_some_and(|path| {
        path.strip_prefix("/operations/")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(_, rest)| rest.starts_with("extensions"))
    }) && failure.error.repair.is_some()
}

fn record_repair(store: &mut Store, command: &RecordCommand, mut failure: Failure) -> Failure {
    let operation = failure
        .error
        .path
        .as_deref()
        .and_then(|path| {
            let operation = path
                .strip_prefix("/operations/")?
                .split('/')
                .next()?
                .parse::<usize>()
                .ok()?;
            command.operations.get(operation)
        })
        .or_else(|| command.operations.first());
    // A refused pack extension already names its repair (method pack adoption);
    // it is not about the operation's payload or a case-local reference.
    if is_extension_failure(&failure) {
        return with_workspace_repair(store.workspace(), failure);
    }
    if matches!(
        failure.error.code,
        ErrorCode::GateFailed | ErrorCode::UnknownReference
    ) && let Some(operation) = operation
    {
        failure = failure.with_schema(&operation.op);
    }
    if failure.error.code == ErrorCode::UnknownReference
        && let Some(case) = command.case_id
        && let Ok(replay) = store.case(case)
    {
        let expected_kind = failure.error.path.as_deref().and_then(|path| {
            let (_, pointer) = path.split_once("/payload")?;
            let value = operation?.payload.pointer(pointer)?;
            let id = value.as_str().or_else(|| value.get("id")?.as_str())?;
            id.parse::<investigation_core::identity::Id>()
                .ok()
                .map(|id| id.kind())
        });
        let refs: Vec<_> = replay
            .state
            .records()
            .filter(|record| expected_kind.is_none_or(|kind| record.header.id.kind() == kind))
            .take(3)
            .map(|record| record.reference())
            .collect();
        if let Some(first) = refs.first() {
            failure = failure.with_argv(vec![
                PROGRAM_NAME.into(),
                "show".into(),
                first.id.to_string(),
                "--case".into(),
                case.to_string(),
                "--format".into(),
                "json".into(),
            ]);
        }
        failure.error.actual =
            Some(json!({"case_local_references":refs,"snapshot":replay.snapshot}));
    }
    with_workspace_repair(store.workspace(), failure)
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
    if let Some(warning) = selection::local_actor_warning() {
        receipt.warnings.push(warning);
    }
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
    // A retry says so: its lines are otherwise those of the first commit.
    if receipt.result == CommitResult::AlreadyCommitted {
        text.push_str("  already_committed");
    }
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

pub(crate) fn scope_argv(workspace: &Workspace, argv: &mut Vec<String>) {
    if argv.first().is_some_and(|program| program == PROGRAM_NAME)
        && !argv
            .get(1)
            .is_some_and(|arg| arg == "--workspace" || arg == "init")
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

pub(crate) fn display_argv(argv: &[String]) -> String {
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
    if report.reference_only_artifacts > 0 {
        text.push_str(&format!(
            "  {} reference-only artifact(s) not retained or checked",
            report.reference_only_artifacts
        ));
    }
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
    logging::init(&args);
    let format = requested_format(&args);
    let parser = help_teach::command_tree();
    let help = help_argv(&parser, &args);
    let parsed = parser.try_get_matches_from(args);
    let output = match parsed {
        Ok(mut matches) => {
            if let Some(refusal) = read_only::refusal(&matches) {
                Output::failure(refusal)
            } else if let Some(shortcut) = shortcuts::from_matches(&mut matches) {
                dispatch_shortcut(shortcut).unwrap_or_else(Output::failure)
            } else {
                match Cli::from_arg_matches_mut(&mut matches) {
                    Ok(cli) => dispatch(cli).unwrap_or_else(Output::failure),
                    Err(error) => parse_error(error, help),
                }
            }
        }
        Err(error) => parse_error(error, help),
    };

    // Authored and recorded text never reaches a terminal with a literal
    // control, bidirectional override or invisible character.
    let text = escape_for_terminal(output.text.trim_end());
    let result = match format {
        Format::Json => writeln!(
            io::stdout().lock(),
            "{}",
            escape_for_terminal(&output.value.to_string())
        ),
        Format::Text | Format::Pretty
            if output.exit != 0 && output.value.get("error").is_some() =>
        {
            writeln!(io::stderr().lock(), "{text}")
        }
        Format::Text | Format::Pretty => writeln!(io::stdout().lock(), "{text}"),
    };
    match result {
        Ok(()) => ExitCode::from(output.exit),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::from(output.exit),
        Err(_) => ExitCode::from(ErrorCode::IoFailure.exit_code()),
    }
}

/// The help argv for the deepest subcommand named on the command line, so a
/// refused flag points at the help that lists the flags of that subcommand.
fn help_argv(root: &clap::Command, args: &[OsString]) -> Vec<String> {
    let takes_value = |command: &clap::Command, long: &str| {
        command
            .get_arguments()
            .chain(root.get_arguments())
            .find(|arg| arg.get_long() == Some(long))
            .is_some_and(|arg| arg.get_action().takes_values())
    };
    let mut argv = vec![PROGRAM_NAME.to_owned()];
    let mut command = root;
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next().and_then(|arg| arg.to_str()) {
        if arg == "--" {
            break;
        } else if let Some(long) = arg.strip_prefix("--") {
            if !long.contains('=') && takes_value(command, long) {
                args.next();
            }
        } else if arg.starts_with('-') {
            continue;
        } else if let Some(subcommand) = command.find_subcommand(arg) {
            argv.push(subcommand.get_name().to_owned());
            command = subcommand;
        } else {
            break;
        }
    }
    argv.extend(["--help", "--format", "json"].map(str::to_owned));
    argv
}

fn parse_error(error: clap::Error, help: Vec<String>) -> Output {
    match error.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            let text = error.to_string();
            Output::success("help", json!({ "help": text }), text)
        }
        ErrorKind::DisplayVersion => version_output(error.to_string()),
        ErrorKind::InvalidSubcommand if help.len() == 4 => {
            // An unknown word in the command position of the root. In the
            // W17.03 help-only runs agents passed prose here, as if the CLI
            // were a note taker, then spent a call on --help.
            let word = error
                .get(clap::error::ContextKind::InvalidSubcommand)
                .map(ToString::to_string)
                .unwrap_or_default();
            let (advice, repair) = if word.contains(char::is_whitespace) {
                (
                    ROOT_PROSE_ADVICE,
                    [PROGRAM_NAME, "onboard", "--format", "json"]
                        .map(str::to_owned)
                        .to_vec(),
                )
            } else {
                (ROOT_WORD_ADVICE, help)
            };
            let message = format!("{}\n{advice}", error.to_string().trim_end());
            Output::failure(Failure::new(ErrorCode::InvalidInput, message).with_argv(repair))
        }
        _ => Output::failure(
            Failure::new(ErrorCode::InvalidInput, error.to_string()).with_argv(help),
        ),
    }
}

const ROOT_PROSE_ADVICE: &str = "\
investigation takes a command first, not free text. To start a case from a local file:
  investigation init ./casework --actor agent:investigator --no-vault --import-root .
  investigation --workspace ./casework --actor agent:investigator start 'QUESTION' --attach FILE";

const ROOT_WORD_ADVICE: &str = "\
investigation --help lists the commands. Typed records such as assessment.add are written with
investigation record --input FILE; investigation schema --list lists the operations.";

fn version_output(text: String) -> Output {
    Output::success(
        "version",
        json!({ "program": PROGRAM_NAME, "version": env!("CARGO_PKG_VERSION") }),
        text,
    )
}

fn dispatch_shortcut(shortcut: shortcuts::ParsedShortcut) -> Result<Output, Failure> {
    let workspace = open_workspace(shortcut.global.workspace.as_deref())?;
    let mut store = Store::open(workspace);
    shortcuts::run(&mut store, shortcut)
}
