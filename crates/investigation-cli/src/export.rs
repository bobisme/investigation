//! `export --kind KIND`: write an immutable export of one case at one verified
//! snapshot into a fresh directory.
//!
//! The pipeline lives in `investigation_render::export`; this module selects
//! the case and the destination and reports the result. The response is the
//! publisher's receipt: unlike the export's own manifest it names the exact
//! records the audience policy withheld, the records it shows with caveats,
//! the case's current head and, when IDs are remapped, the canonical
//! snapshot and every ID pair.

use std::{
    env,
    fmt::Write as _,
    fs,
    io::{self, Write as _},
    os::unix::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf},
};

use clap::builder::PossibleValuesParser;
use investigation_core::wire::{Digest, ErrorCode, Failure};
use investigation_render::{
    Audience,
    export::{
        self, ExportError, ExportPlan, ExportSnapshot, IdentifierMode, KINDS, KindSpec,
        MANIFEST_FILE, Pin, Product, Scope,
        archive::{ARCHIVE_AUDIENCE, plan_archive},
        plan_export_remap, producer_bundle, review_vault,
        source::{read_archive_source, read_retained},
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
    /// New or empty directory to write (a new one for review-vault); default exports/<kind>-<case>-<sequence>-<id> in the workspace.
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
    /// Fail with the exact exclusions instead of writing an incomplete export.
    #[arg(long)]
    require_complete: bool,
    /// Replace record IDs and the snapshot with opaque ones for this export; always on for public.
    #[arg(long)]
    remap_ids: bool,
    /// Also write the canonical-to-exported ID map of a remapped export to FILE: a new owner-only file outside the export directory.
    #[arg(long, value_name = "FILE")]
    id_map: Option<PathBuf>,
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
export is marked incomplete. A record that rests on, or is challenged by, a
left-out record carries a fixed caveat for the internal and restricted
audiences and is left out for public. The exact records left out or caveated
are listed only in this command's output, never in the export.
--require-complete refuses instead. --snapshot pins an earlier verified head;
disclosure still follows today's classifications.

Record IDs are UUIDv7 values that embed their creation time, and a journal
sequence tells how many events a case holds. A public export, and any export
with --remap-ids, replaces every UUIDv7 value with an opaque one for this
export only, and its manifest names the bundle (sequence 0, a bundle digest)
instead of the case snapshot. The mapping back is listed only in this
command's output, and with --id-map FILE also in FILE: created new and
owner-only (0600), never overwritten, never inside the export directory or
among the workspace's state, vault or configuration (local/ and exports/ are
allowed), and refused for an export that is not remapped. A remapped export
cannot restore canonical state. Record times (recorded, acquired) show
exactly in a canonical export, to the UTC day in a remapped one, and not at
all in a public one.

--kind review-vault writes a browsable copy of the case for the audience:
the vault's notes and plain Markdown table views planned from the records
the audience may see, never copied from vault/, with Start Here.md, Omitted
Data.md and reviewed derivative previews under Attachments/. Human notes and
boards shown records rely on appear as captured (Human Files.md, bytes under
human/sha256/), never as they are now. It is written only into a directory
that does not exist yet, and cannot resume the case.

--kind workspace-archive copies the canonical state of every case instead:
each journal to its verified head, the blobs they require, the workspace
identity, investigation.toml, the vault's human folders and the v1 schemas.
It takes no --case or --snapshot, is made only for --audience restricted
because it holds every byte, and is refused while capture staging holds
evidence the journal does not reference. It keeps canonical IDs and refuses
--remap-ids. Check it with archive verify.

--kind producer-bundle writes the case's recorded Runs for another tool:
producer-bundle.json (schema producer-bundle), each run's inv.producer-run.v1
envelope and its retained artifact bytes. Producer statements stay labelled as
the producer's claims. With the bundle beneath an import root, investigation
ingest --input DIR/producer-bundle.json re-imports every run; the same run maps
onto the recorded one and is never a new trial.

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
  investigation export --kind case-report --audience internal --remap-ids --case <ID> --format json
  investigation export --kind case-report --audience public --case <ID> --out ./report --id-map ./report-ids.json --format json
  investigation export --kind review-vault --audience internal --case <ID> --out ./review --format json
  investigation export --kind workspace-archive --audience restricted --out ./backup --format json
  investigation export --kind producer-bundle --audience internal --case <ID> --out ./runs --format json

Read DIR/export-manifest.json for completeness, versions and file digests, then
share DIR yourself; the export never leaves this machine on its own.

Next: investigation brief --case <ID> --format json",
    );
    text
}

pub(crate) fn run(store: &mut Store, global: &Global, args: Args) -> Result<Output, Failure> {
    let spec = export::kind(&args.kind).expect("clap accepts registered kinds only");
    if spec.scope() == Scope::All {
        return run_archive(store, global, spec, &args);
    }
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
    if args.id_map.is_some()
        && IdentifierMode::for_audience(audience, args.remap_ids) == IdentifierMode::Canonical
    {
        let mut repair = retry.clone();
        repair.push("--remap-ids".into());
        return Err(id_map_canonical().with_argv(repair));
    }
    let snapshot = ExportSnapshot::read(store, case, &pin)
        .map_err(|failure| failure.with_argv(retry.clone()))?;
    // The captured bytes of the human files records rely on (W11.14), for a
    // review copy the reviewed derivatives it may attach, and for a producer
    // bundle the retained artifacts its Runs name: read exactly those bytes,
    // checked against their digests, before planning. The planner copies
    // only those the audience may see.
    let mut digests = snapshot_digests(&snapshot);
    if spec.product == Product::Review {
        digests.extend(review_vault::attachment_digests(&snapshot, audience));
    }
    if spec.name == producer_bundle::KIND {
        digests.extend(producer_bundle::required_digests(
            snapshot.case().state(),
            snapshot.today(),
        ));
    }
    digests.sort();
    digests.dedup();
    let retained =
        read_retained(store, &digests).map_err(|failure| failure.with_argv(retry.clone()))?;
    let plan = plan_export_remap(spec, &snapshot, audience, &retained, args.remap_ids).map_err(
        |error| {
            let failure = error.to_failure();
            match error {
                ExportError::KindNotAvailable { .. } => {
                    let mut failure = failure.at("--kind");
                    failure.error.expected = Some(json!({"available": export::available_kinds()}));
                    failure
                }
                ExportError::RemapResumable { .. } => failure.at("--remap-ids"),
                _ => failure,
            }
        },
    )?;
    if args.require_complete && !plan.complete() {
        let error = ExportError::Incomplete {
            exclusions: plan.exclusions().to_vec(),
            omissions: plan.omissions().to_vec(),
        };
        let mut failure = error.to_failure().at("--require-complete");
        failure.error.actual = Some(exclusions_json(&plan));
        return Err(failure);
    }
    let (out, shown) = destination(store, &args, &plan);
    let id_map = args
        .id_map
        .as_deref()
        .map(|file| {
            check_id_map(store.workspace().root(), file, &out)
                .map_err(|failure| failure.with_argv(retry.clone()))
        })
        .transpose()?;
    // The map is created first, so an existing file refuses the export
    // before anything is written; it is removed again if the export fails.
    if let Some(file) = &id_map {
        write_id_map(file, &plan, audience)?;
    }
    let written = write(store, &plan, &out, &shown).inspect_err(|_| {
        if let Some(file) = &id_map {
            let _ = fs::remove_file(file);
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
        "identifiers": plan.identifiers().mode(),
        "snapshot": plan.identifiers().canonical_snapshot(),
        "historical": snapshot.historical(),
        "latest_snapshot": snapshot.latest(),
        "publisher_receipt": receipt_json(&plan),
        "id_map": args.id_map.as_deref().map(|file| file.to_string_lossy().into_owned()),
        "next": [{"kind": "local_read", "argv": next_argv}],
    });
    let canonical = plan.identifiers().canonical_snapshot();
    let mut text = format!(
        "exported  {}  {}  {}\n{}  sequence {}  {} file(s)",
        manifest.kind,
        manifest.completeness.as_str(),
        shown,
        canonical.case_id,
        canonical.sequence,
        written.files
    );
    if plan.identifiers().mode() == IdentifierMode::Remapped {
        let _ = write!(
            text,
            "\nremapped {} ID(s); the export names bundle {} and the mapping is in --format json output only",
            plan.identifiers().len(),
            manifest.snapshot.case_id
        );
        if let Some(file) = &args.id_map {
            let _ = write!(
                text,
                ", and in {} (owner-only; keep it private)",
                file.to_string_lossy()
            );
        }
    }
    if !plan.exclusions().is_empty() {
        let _ = write!(
            text,
            "\nwithheld {} record(s) from audience {}; they are listed in --format json output only",
            plan.exclusions().len(),
            audience.as_str()
        );
    }
    if !plan.omissions().is_empty() {
        let _ = write!(
            text,
            "\nleft out {} further item(s) the kind could not show or copy exactly; they are listed in --format json output only",
            plan.omissions().len()
        );
    }
    if !plan.caveated().is_empty() {
        let _ = write!(
            text,
            "\ncaveated {} shown record(s) that rest on or are challenged by withheld material",
            plan.caveated().len()
        );
    }
    let _ = write!(
        text,
        "\nNext: read {shown}/{MANIFEST_FILE}, then share the directory yourself.\nRead the case at this snapshot: {}",
        display_argv(&next_argv)
    );
    Ok(Output::success("exported", value, text))
}

/// Where to write `plan`: `--out`, or `exports/<default name>`. Returns the
/// path and how to show it.
fn destination(store: &Store, args: &Args, plan: &ExportPlan) -> (PathBuf, String) {
    match &args.out {
        Some(out) => (out.clone(), out.to_string_lossy().into_owned()),
        None => {
            let name = default_name(plan);
            (
                store.workspace().root().join("exports").join(&name),
                format!("exports/{name}"),
            )
        }
    }
}

fn write(
    store: &Store,
    plan: &ExportPlan,
    out: &Path,
    shown: &str,
) -> Result<export::Written, Failure> {
    refuse_workspace_internals(store.workspace().root(), out)?;
    write_export(plan, out).map_err(|error| {
        let failure = error.to_failure().at("--out");
        match error {
            ExportError::DestinationNotEmpty
            | ExportError::DestinationExists
            | ExportError::DestinationUnsafe(_) => {
                let mut failure = failure;
                failure.error.actual = Some(json!({"destination": shown}));
                failure
            }
            _ => failure,
        }
    })
}

/// `export --kind workspace-archive`: every case at its verified head.
fn run_archive(
    store: &mut Store,
    global: &Global,
    spec: &'static KindSpec,
    args: &Args,
) -> Result<Output, Failure> {
    let refuse = |path: &str, message: &str| {
        Failure::new(ErrorCode::InvalidInput, message.to_owned()).at(path)
    };
    if global.case.is_some() {
        return Err(refuse(
            "--case",
            "A workspace archive holds every case of the workspace; omit --case.",
        ));
    }
    if args.snapshot.snapshot.is_some() {
        return Err(refuse(
            "--snapshot",
            "A workspace archive copies every case to its current verified head; omit --snapshot.",
        ));
    }
    if args.remap_ids {
        return Err(ExportError::RemapResumable { kind: spec.name }
            .to_failure()
            .at("--remap-ids"));
    }
    if args.id_map.is_some() {
        return Err(id_map_canonical());
    }
    let audience = args.audience.audience();
    if audience != ARCHIVE_AUDIENCE {
        let mut failure = ExportError::ArchiveAudience {
            required: ARCHIVE_AUDIENCE.as_str(),
        }
        .to_failure()
        .at("--audience");
        failure.error.expected = Some(json!({"audience": ARCHIVE_AUDIENCE.as_str()}));
        return Err(failure);
    }
    let (source, pin) = read_archive_source(store, crate::schema::published_documents())?;
    let plan = plan_archive(spec, &source, audience).map_err(|error| {
        let failure = error.to_failure();
        match &error {
            ExportError::Incomplete { .. } | ExportError::Caveated { .. } => {
                let mut failure = failure.at("--audience");
                failure.error.actual = Some(exclusions_json_of(&error));
                failure
            }
            _ => failure,
        }
    })?;
    let (out, shown) = destination(store, args, &plan);
    // The pin lives until the archive is written; an error here drops it.
    let written = write(store, &plan, &out, &shown)?;
    // A failed release leaves a record no process holds; the next pin or
    // reclamation expires it, so it does not fail a finished archive.
    let _ = store.release_pin(pin);
    let manifest = plan.manifest();
    let canonical = &source.canonical;
    let verify_argv: Vec<String> = [
        investigation_core::PROGRAM_NAME,
        "archive",
        "verify",
        &out.to_string_lossy(),
        "--format",
        "json",
    ]
    .map(str::to_owned)
    .to_vec();
    let value = json!({
        "export_id": manifest.export_id,
        "kind": manifest.kind,
        "product": manifest.product,
        "audience": audience.as_str(),
        "destination": shown,
        "created_directory": written.created_directory,
        "manifest": MANIFEST_FILE,
        "index": export::archive::INDEX_FILE,
        "files_written": written.files,
        "bytes_written": written.bytes,
        "completeness": manifest.completeness,
        "identifiers": plan.identifiers().mode(),
        "snapshot": manifest.snapshot,
        "recovery_head": manifest.snapshot.head_hash,
        "cases": canonical.heads(),
        "blobs": canonical.blobs.len(),
        "not_retained": canonical.not_retained.len(),
        "human_notes": source.human_notes.len(),
        "human_notes_skipped": source.skipped_notes,
        "next": [{"kind": "local_read", "argv": verify_argv}],
    });
    let mut text = format!(
        "exported  {}  complete  {}\n{} case(s)  {} blob(s)  {} human note(s)  {} file(s)\nrecovery head {}",
        manifest.kind,
        shown,
        canonical.cases.len(),
        canonical.blobs.len(),
        source.human_notes.len(),
        written.files,
        manifest.snapshot.head_hash,
    );
    if !source.skipped_notes.is_empty() {
        let _ = write!(
            text,
            "\nskipped {} human-folder entr(ies); they are listed in --format json output",
            source.skipped_notes.len()
        );
    }
    let _ = write!(
        text,
        "\nThe archive holds restricted records and bytes and is not encrypted.\nNext: {}",
        display_argv(&verify_argv)
    );
    Ok(Output::success("exported", value, text))
}

fn exclusions_json_of(error: &ExportError) -> Value {
    if let ExportError::Caveated { caveated } = error {
        return json!({
            "caveated_count": caveated.len(),
            "caveated": caveated.iter().take(MAX_LISTED).collect::<Vec<_>>(),
            "lists_capped_at": MAX_LISTED,
        });
    }
    let ExportError::Incomplete {
        exclusions,
        omissions,
    } = error
    else {
        return Value::Null;
    };
    json!({
        "exclusion_count": exclusions.len(),
        "exclusions": exclusions.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "omission_count": omissions.len(),
        "omissions": omissions.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "lists_capped_at": MAX_LISTED,
    })
}

/// Everything the publisher sees that the export does not hold.
fn receipt_json(plan: &ExportPlan) -> Value {
    let mut receipt = exclusions_json(plan);
    receipt["identifiers"] = identifiers_json(plan);
    receipt
}

/// The canonical snapshot behind the export and, when remapped, every ID
/// pair. Not capped: the mapping is needed whole to read the export back.
fn identifiers_json(plan: &ExportPlan) -> Value {
    let identifiers = plan.identifiers();
    let mut value = json!({
        "mode": identifiers.mode(),
        "exported_snapshot": identifiers.exported_snapshot(),
        "canonical_snapshot": identifiers.canonical_snapshot(),
    });
    if identifiers.mode() == IdentifierMode::Remapped {
        value["note"] = json!(
            "For the publisher only: the export names the bundle and shows these remapped IDs; \
             this is the only mapping back to the case snapshot and its records."
        );
        value["remap"] = json!(export::REMAP_VERSION);
        value["id_count"] = json!(identifiers.len());
        value["ids"] = identifiers
            .pairs()
            .map(|(canonical, exported)| json!({"canonical": canonical, "exported": exported}))
            .collect();
    }
    value
}

/// The publisher-only list of withheld records, content the kind left out
/// and caveated records.
fn exclusions_json(plan: &ExportPlan) -> Value {
    let exclusions = plan.exclusions();
    let omissions = plan.omissions();
    let caveated = plan.caveated();
    json!({
        "note": "For the publisher only: these records were withheld by the audience policy and are not named in the export; the omissions are content the kind left out beyond those records (what, why, the record it belongs to and which part), which the export does not name or count either; the caveated records are shown with the policy's fixed caveats.",
        "policy": export::POLICY_VERSION,
        "exclusion_count": exclusions.len(),
        "exclusions": exclusions.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "omission_count": omissions.len(),
        "omissions": omissions.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "caveated_count": caveated.len(),
        "caveated": caveated.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "lists_capped_at": MAX_LISTED,
    })
}

/// `<kind>-<case>-<sequence>-<12 hex of the export ID>`, or
/// `<kind>-<remapped case>-<12 hex>` when remapped, so the name discloses no
/// more than the export: equal exports get equal names, so a repeat finds
/// the first one and is refused.
fn default_name(plan: &ExportPlan) -> String {
    let manifest = plan.manifest();
    let short = manifest
        .export_id
        .trim_start_matches("exp_")
        .get(..12)
        .unwrap_or_default();
    match plan.identifiers().mode() {
        IdentifierMode::Canonical => format!(
            "{}-{}-{}-{short}",
            manifest.kind, manifest.snapshot.case_id, manifest.snapshot.sequence
        ),
        IdentifierMode::Remapped => {
            format!("{}-{}-{short}", manifest.kind, manifest.snapshot.case_id)
        }
    }
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

/// `--id-map` given for an export that keeps canonical IDs, so has no map.
fn id_map_canonical() -> Failure {
    Failure::new(
        ErrorCode::InvalidInput,
        "--id-map is written only for a remapped export (--audience public, or --remap-ids); \
         this export keeps canonical IDs and has no map.",
    )
    .at("--id-map")
}

/// Where `--id-map FILE` may go: a new file in an existing directory, not
/// inside the export directory `out`, and inside the workspace only under
/// `local/` or `exports/`, never among its state, vault (which is synced
/// and archived) or configuration. Returns the absolute path to create.
fn check_id_map(root: &Path, file: &Path, out: &Path) -> Result<PathBuf, Failure> {
    let refuse = |message: &str| {
        let mut failure = Failure::new(ErrorCode::InvalidInput, message.to_owned()).at("--id-map");
        failure.error.actual = Some(json!({"id_map": file.to_string_lossy()}));
        failure
    };
    let absolute = |path: &Path| -> Result<PathBuf, Failure> {
        if path.is_absolute() {
            return Ok(path.to_path_buf());
        }
        env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| Failure::new(ErrorCode::IoFailure, error.to_string()))
    };
    let absolute_file = absolute(file)?;
    let (Some(parent), Some(Component::Normal(name))) = (
        absolute_file.parent(),
        absolute_file.components().next_back(),
    ) else {
        return Err(refuse("--id-map names a file, not a directory or `..`."));
    };
    let Ok(parent) = parent.canonicalize() else {
        return Err(refuse(
            "The --id-map file's directory does not exist; create it first.",
        ));
    };
    let target = parent.join(name);
    if fs::symlink_metadata(&target).is_ok() {
        return Err(refuse(
            "The --id-map file already exists. The map is never overwritten; name a new file.",
        ));
    }
    if target.starts_with(resolved(&absolute(out)?)) {
        return Err(refuse(
            "The --id-map file may not be inside the export directory: the map is for the \
             publisher only, and the export is shared.",
        ));
    }
    if let Ok(root) = root.canonicalize()
        && target.starts_with(&root)
        && !target.starts_with(root.join("local"))
        && !target.starts_with(root.join("exports"))
    {
        let mut failure = refuse(
            "Inside the workspace the --id-map file may go only under local/ or exports/, \
             never among its state, vault or configuration.",
        );
        failure.error.expected = Some(json!({"under": ["local/", "exports/"]}));
        return Err(failure);
    }
    Ok(target)
}

/// `path` with its longest existing ancestor resolved through links, so a
/// containment check sees through them; the rest is kept as written.
fn resolved(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            return rest.into_iter().rev().fold(real, |at, part| at.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Create `file` new and owner-only (0600) and write the export's ID map
/// into it: the publisher receipt's `identifiers` object with the export it
/// belongs to.
fn write_id_map(file: &Path, plan: &ExportPlan, audience: Audience) -> Result<(), Failure> {
    let mut map = identifiers_json(plan);
    map["note"] = json!(
        "For the publisher only: the canonical snapshot and record IDs behind this remapped \
         export. Keep it private: with it the export's IDs, and the creation times they embed, \
         can be read back."
    );
    map["export_id"] = json!(plan.manifest().export_id);
    map["kind"] = json!(plan.manifest().kind);
    map["audience"] = json!(audience.as_str());
    let mut bytes = serde_json::to_vec_pretty(&map).expect("the ID map serializes");
    bytes.push(b'\n');
    let failed = |error: io::Error| {
        let code = match error.kind() {
            io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound => ErrorCode::InvalidInput,
            _ => ErrorCode::IoFailure,
        };
        let mut failure =
            Failure::new(code, format!("Could not create the --id-map file: {error}"))
                .at("--id-map");
        failure.error.actual = Some(json!({"id_map": file.to_string_lossy()}));
        failure
    };
    let mut handle = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(file)
        .map_err(failed)?;
    handle
        .write_all(&bytes)
        .and_then(|()| handle.sync_all())
        .inspect_err(|_| {
            let _ = fs::remove_file(file);
        })
        .map_err(failed)
}

/// The digests of the human-file snapshots of the pinned case whose bytes
/// the workspace still retains, in order.
fn snapshot_digests(snapshot: &ExportSnapshot) -> Vec<Digest> {
    use investigation_core::{
        derivation::artifact_body, evidence::ArtifactAvailability, human_file::NoteSnapshot,
    };
    let state = snapshot.case().state();
    let mut digests: Vec<Digest> = NoteSnapshot::all(state)
        .into_iter()
        .filter(|note| {
            !investigation_core::retention_ops::bytes_withdrawn(
                state,
                snapshot.today(),
                note.artifact.id,
            )
        })
        .filter(|note| {
            state.current(note.artifact.id).is_some_and(|current| {
                artifact_body(state, current.reference()).is_some_and(|body| {
                    body.availability == ArtifactAvailability::Retained
                        && body.digest.as_ref() == Some(&note.sha256)
                })
            })
        })
        .map(|note| note.sha256)
        .collect();
    digests.sort();
    digests.dedup();
    digests
}
