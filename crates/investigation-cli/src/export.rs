//! `export --kind KIND`: write an immutable export of one case at one verified
//! snapshot into a fresh directory.
//!
//! The pipeline lives in `investigation_render::export`; this module selects
//! the case and the destination and reports the result. The response is the
//! publisher's receipt: unlike the export's own manifest it names the exact
//! records the audience policy withheld and the case's current head.

use std::{
    env,
    fmt::Write as _,
    path::{Path, PathBuf},
};

use clap::builder::PossibleValuesParser;
use investigation_core::wire::{Digest, ErrorCode, Failure};
use investigation_render::{
    Audience,
    export::{
        self, ExportError, ExportPlan, ExportSnapshot, KINDS, MANIFEST_FILE, Pin, plan_export,
        write_export,
    },
};
use investigation_store::Store;
use serde_json::{Value, json};

use crate::{
    Global, Output, display_argv,
    selection::{select_case, workspace_argv},
    snapshot::{SnapshotArgs, pin_argv},
};

/// Exclusions listed in one response; the count is always complete.
const MAX_LISTED: usize = 50;

/// The audience an export is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum AudienceArg {
    Public,
    Internal,
    Restricted,
}

impl AudienceArg {
    const fn audience(self) -> Audience {
        match self {
            Self::Public => Audience::Public,
            Self::Internal => Audience::Internal,
            Self::Restricted => Audience::Restricted,
        }
    }
}

#[derive(clap::Args)]
pub(crate) struct Args {
    /// What to export; kinds that are planned but not built yet are refused.
    #[arg(long, value_name = "KIND", value_parser = PossibleValuesParser::new(export::kind_names()))]
    kind: String,
    /// Who the export is for; records classified above it are withheld.
    #[arg(long, value_enum, value_name = "AUDIENCE")]
    audience: AudienceArg,
    /// New or empty directory to write; default exports/<kind>-<case>-<sequence>-<id> in the workspace.
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
    /// Fail with the exact exclusions instead of writing an incomplete export.
    #[arg(long)]
    require_complete: bool,
    #[command(flatten)]
    snapshot: SnapshotArgs,
}

/// `export --help` text after the options: what it does, the kinds, examples
/// and the next step.
pub(crate) fn after_help() -> String {
    let mut text = String::from(
        "\
Writes one case, as recorded at one verified snapshot, into a new or empty
directory, and export-manifest.json last. The same snapshot, kind and audience
always give the same bytes. Nothing is overwritten, sent or uploaded, and no
model is called. Records classified above --audience are left out and the
export is marked incomplete; the exact records left out are listed only in
this command's output, never in the export. --require-complete refuses
instead. --snapshot pins an earlier verified head; disclosure still follows
today's classifications.

Kinds:
",
    );
    for spec in KINDS {
        let _ = writeln!(
            text,
            "  {:<22}{}{}",
            spec.name,
            spec.purpose,
            if spec.available() {
                ""
            } else {
                " (planned; refused until available)"
            }
        );
    }
    text.push_str(
        "
Examples:
  investigation export --kind case-report --audience internal --case <ID> --format json
  investigation export --kind case-report --audience public --case <ID> --snapshot 12 --out ./report-12 --format json

Read DIR/export-manifest.json for completeness, versions and file digests, then
share DIR yourself; the export never leaves this machine on its own.

Next: investigation brief --case <ID> --format json",
    );
    text
}

pub(crate) fn run(store: &mut Store, global: &Global, args: Args) -> Result<Output, Failure> {
    let case = select_case(store, global.case.as_deref())?;
    let mut prefix = workspace_argv(store.workspace());
    prefix.extend(["--case".into(), case.to_string()]);
    let audience = args.audience.audience();
    let retry = {
        let mut argv = prefix.clone();
        argv.extend([
            "export".into(),
            "--kind".into(),
            args.kind.clone(),
            "--audience".into(),
            audience.as_str().into(),
            "--format".into(),
            "json".into(),
        ]);
        argv
    };
    let head_hash = args
        .snapshot
        .head_hash
        .as_deref()
        .map(str::parse::<Digest>)
        .transpose()
        .map_err(|error| Failure::new(ErrorCode::InvalidInput, error).at("--head-hash"))?;
    let pin = Pin {
        sequence: args.snapshot.snapshot,
        head_hash,
    };
    let snapshot = ExportSnapshot::read(store, case, &pin)
        .map_err(|failure| failure.with_argv(retry.clone()))?;
    let spec = export::kind(&args.kind).expect("clap accepts registered kinds only");
    let plan = plan_export(spec, &snapshot, audience).map_err(|error| {
        let failure = error.to_failure();
        match error {
            ExportError::KindNotAvailable { .. } => {
                let mut failure = failure.at("--kind");
                failure.error.expected = Some(json!({"available": export::available_kinds()}));
                failure
            }
            _ => failure,
        }
    })?;
    if args.require_complete && !plan.complete() {
        let error = ExportError::Incomplete {
            exclusions: plan.exclusions().to_vec(),
        };
        let mut failure = error.to_failure().at("--require-complete");
        failure.error.actual = Some(exclusions_json(&plan));
        return Err(failure);
    }
    let workspace = store.workspace();
    let (out, shown) = match &args.out {
        Some(out) => (out.clone(), out.to_string_lossy().into_owned()),
        None => {
            let name = default_name(&plan);
            (
                workspace.root().join("exports").join(&name),
                format!("exports/{name}"),
            )
        }
    };
    refuse_workspace_internals(workspace.root(), &out)?;
    let written = write_export(&plan, &out).map_err(|error| {
        let failure = error.to_failure().at("--out");
        match error {
            ExportError::DestinationNotEmpty | ExportError::DestinationUnsafe(_) => {
                let mut failure = failure;
                failure.error.actual = Some(json!({"destination": shown}));
                failure
            }
            _ => failure,
        }
    })?;

    let manifest = plan.manifest();
    let mut next_argv = prefix;
    next_argv.push("brief".into());
    next_argv.extend(pin_argv(snapshot.case().head()));
    next_argv.extend(["--format".into(), "json".into()]);
    let files: Vec<&str> = plan
        .files()
        .entries()
        .iter()
        .map(|entry| entry.path().as_str())
        .chain([MANIFEST_FILE])
        .collect();
    let value = json!({
        "export_id": manifest.export_id,
        "kind": manifest.kind,
        "product": manifest.product,
        "audience": audience.as_str(),
        "destination": shown,
        "created_directory": written.created_directory,
        "manifest": MANIFEST_FILE,
        "files": files,
        "files_written": written.files,
        "bytes_written": written.bytes,
        "completeness": manifest.completeness,
        "snapshot": manifest.snapshot,
        "historical": snapshot.historical(),
        "latest_snapshot": snapshot.latest(),
        "publisher_receipt": exclusions_json(&plan),
        "next": [{"kind": "local_read", "argv": next_argv}],
    });
    let mut text = format!(
        "exported  {}  {}  {}\n{}  sequence {}  {} file(s)",
        manifest.kind,
        manifest.completeness.as_str(),
        shown,
        manifest.snapshot.case_id,
        manifest.snapshot.sequence,
        written.files
    );
    if !plan.complete() {
        let _ = write!(
            text,
            "\nwithheld {} record(s) from audience {}; they are listed in --format json output only",
            plan.exclusions().len(),
            audience.as_str()
        );
    }
    let _ = write!(
        text,
        "\nNext: read {shown}/{MANIFEST_FILE}, then share the directory yourself.\nRead the case at this snapshot: {}",
        display_argv(&next_argv)
    );
    Ok(Output::success("exported", value, text))
}

/// The publisher-only list of withheld records.
fn exclusions_json(plan: &ExportPlan) -> Value {
    let exclusions = plan.exclusions();
    json!({
        "note": "For the publisher only: these records were withheld by the audience policy and are not named in the export.",
        "exclusion_count": exclusions.len(),
        "exclusions": exclusions.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "lists_capped_at": MAX_LISTED,
    })
}

/// `<kind>-<case>-<sequence>-<12 hex of the export ID>`: equal exports get
/// equal names, so a repeat finds the first one and is refused.
fn default_name(plan: &ExportPlan) -> String {
    let manifest = plan.manifest();
    let short = manifest
        .export_id
        .trim_start_matches("exp_")
        .get(..12)
        .unwrap_or_default();
    format!(
        "{}-{}-{}-{short}",
        manifest.kind, manifest.snapshot.case_id, manifest.snapshot.sequence
    )
}

/// An export may go anywhere outside the workspace, or under its
/// `exports/` folder, but never among its canonical state, cache, vault or
/// configuration.
fn refuse_workspace_internals(root: &Path, out: &Path) -> Result<(), Failure> {
    let absolute = if out.is_absolute() {
        out.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| Failure::new(ErrorCode::IoFailure, error.to_string()))?
            .join(out)
    };
    let (Some(parent), Ok(root)) = (absolute.parent(), root.canonicalize()) else {
        return Ok(());
    };
    // A missing parent is reported by the writer.
    let Ok(parent) = parent.canonicalize() else {
        return Ok(());
    };
    let inside = parent.starts_with(&root);
    let under_exports = parent.starts_with(root.join("exports"));
    if inside && !under_exports {
        let mut failure = Failure::new(
            ErrorCode::InvalidInput,
            "An export may be written outside the workspace or under its exports/ folder, \
             not among its state, cache, vault or configuration.",
        )
        .at("--out");
        failure.error.expected = Some(json!({"under": "exports/"}));
        return Err(failure);
    }
    Ok(())
}
