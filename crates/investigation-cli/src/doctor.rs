//! `doctor` and `doctor bundle --preview`: what this build supports and
//! whether the selected workspace can be used, in one read.
//!
//! Every value comes from the code that enforces it: versions from their
//! constants, capture support from [`CaptureCapabilities::current`], selector
//! validation from [`SELECTOR_SUPPORT`], bounds from [`LIMITS`] and [`EXPANSION`], filesystem
//! support from [`FilesystemInfo::support`], index, staging and lock state
//! from [`Store::check`], archive pins from [`Store::archive_pin_status`]
//! and vault state from [`vault_status`]. Doctor writes
//! nothing, probes no external tool and sends nothing anywhere. A refused or
//! broken workspace is a finding, not a failure, so the report always
//! arrives; each finding names its repair.

use std::{
    env,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use investigation_core::{
    PROGRAM_NAME,
    catalog::{Status, standard_catalog},
    command::CommandVersion,
    freshness::FRESHNESS_RULES_VERSION,
    gap_rules::GAP_RULE_VERSION,
    identity::Id,
    local_path::{ELIDED, elide_local_paths},
    method_pack::SCHEMA_VERSION as METHOD_PACK_VERSION,
    next_rank::NEXT_RULE_VERSION,
    wire::{ErrorCode, Failure, ResponseVersion},
};
use investigation_recording::{CAPABILITIES_VERSION, CaptureCapabilities};
use investigation_render::{
    Generation, MANIFEST_VERSION, RENDERER_VERSION, VaultState, VerifiedSnapshot, vault_status,
    working_policy,
};
use investigation_store::{
    Store, Workspace,
    import_paths::{ImportErrorKind, ImportRoot},
    journal::EVENT_VERSION,
    limits::{EXPANSION, LIMITS},
    projection::INDEX_USER_VERSION,
    selectors::SELECTOR_SUPPORT,
    store::{
        Severity as CheckSeverity,
        pins::{PinInfo, PinStatus},
    },
    workspace::{
        FilesystemDetector, FilesystemInfo, NativeFilesystemDetector, RECORD_FAMILY, SupportLevel,
        WORKSPACE_VERSION, WorkspaceError,
    },
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{Global, Output, display_argv, scope_argv};

pub(crate) const AFTER_HELP: &str = "\
doctor reports versions, capabilities, enforced limits, filesystem support and
the selected workspace's index, staging, lock, archive pin and vault state.
Each finding
names a repair. It writes nothing, probes no external tool and uploads nothing;
a workspace it cannot use is reported as a finding with exit status 0.
Examples:
  investigation doctor --format json
  investigation doctor bundle --preview --format json";

const BUNDLE_HELP: &str = "\
Lists exactly what a redacted troubleshooting archive would hold. Evidence
content, credentials and local paths are excluded. This build writes no archive
and uploads nothing; --preview is required.
Example: investigation doctor bundle --preview --format json";

#[derive(clap::Args)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: Option<DoctorCommand>,
}

#[derive(clap::Subcommand)]
pub(crate) enum DoctorCommand {
    /// Preview a redacted troubleshooting archive; nothing is written or sent.
    #[command(after_help = BUNDLE_HELP)]
    Bundle {
        /// List the archive's contents without writing it (required).
        #[arg(long, required = true)]
        preview: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Severity {
    Error,
    Warning,
}

/// One problem and the step that repairs it.
#[derive(Debug, Clone, Serialize)]
struct Finding {
    severity: Severity,
    code: String,
    reason: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    objects: Vec<Id>,
    repair: Repair,
}

#[derive(Debug, Clone, Serialize)]
struct Repair {
    step: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    argv: Option<Vec<String>>,
}

impl Finding {
    fn new(severity: Severity, code: &str, reason: impl Into<String>, step: &str) -> Self {
        Self {
            severity,
            code: code.to_owned(),
            reason: reason.into(),
            objects: Vec::new(),
            repair: Repair {
                step: step.to_owned(),
                argv: None,
            },
        }
    }

    fn argv(mut self, argv: Vec<String>) -> Self {
        self.repair.argv = Some(argv);
        self
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    let mut argv = vec![PROGRAM_NAME.to_owned()];
    argv.extend(parts.iter().map(|part| (*part).to_owned()));
    argv.extend(["--format".to_owned(), "json".to_owned()]);
    argv
}

/// Where doctor looked and what it found there.
struct Report {
    value: Value,
    findings: Vec<Finding>,
    /// Absolute locations withheld from a bundle.
    private_paths: Vec<(PathBuf, &'static str)>,
}

pub(crate) fn run(args: Args, global: &Global) -> Result<Output, Failure> {
    let report = diagnose(global, &NativeFilesystemDetector)?;
    match args.command {
        None => Ok(doctor_output(report)),
        Some(DoctorCommand::Bundle { preview }) => {
            debug_assert!(preview, "clap requires --preview");
            Ok(bundle_preview(report))
        }
    }
}

fn diagnose(global: &Global, detector: &dyn FilesystemDetector) -> Result<Report, Failure> {
    let cwd = env::current_dir().map_err(|error| {
        Failure::new(
            ErrorCode::IoFailure,
            format!("Read working directory: {error}"),
        )
    })?;
    let mut findings = Vec::new();
    let mut private_paths = vec![(cwd.clone(), "<WORKING_DIRECTORY>")];
    if let Some(home) = env::var_os("HOME").filter(|home| !home.is_empty()) {
        private_paths.push((PathBuf::from(home), "<HOME>"));
    }
    let located = match global.workspace.as_deref() {
        Some(path) if path.join("investigation.toml").is_file() => Some(cwd.join(path)),
        Some(_) => {
            findings.push(
                Finding::new(
                    Severity::Error,
                    "WORKSPACE_NOT_FOUND",
                    "The --workspace path holds no investigation.toml.",
                    "Pass the directory that holds investigation.toml, or create a workspace with investigation init PATH.",
                )
                .argv(argv(&["onboard"])),
            );
            None
        }
        None => match Workspace::locate(&cwd) {
            Ok(root) => Some(root),
            Err(_) => {
                findings.push(
                    Finding::new(
                        Severity::Warning,
                        "WORKSPACE_NOT_FOUND",
                        "No investigation.toml was found from the working directory up to the filesystem root, so only this build was diagnosed.",
                        "Run doctor inside a workspace or pass --workspace PATH; create one with investigation init PATH.",
                    )
                    .argv(argv(&["onboard"])),
                );
                None
            }
        },
    };
    let root = located.map(|root| std::fs::canonicalize(&root).unwrap_or(root));
    if let Some(root) = &root {
        private_paths.insert(0, (root.clone(), "<WORKSPACE>"));
    }
    let subject = root.as_deref().unwrap_or(&cwd);
    let (filesystem, refused) = filesystem(
        detector,
        subject,
        if root.is_some() {
            "workspace"
        } else {
            "working_directory"
        },
        &mut findings,
    );
    let workspace = match &root {
        Some(root) if !refused => workspace_health(root, &mut findings, &mut private_paths),
        Some(_) => json!({"opened": false, "reason": "filesystem refused"}),
        None => json!({"opened": false, "reason": "no workspace selected"}),
    };
    let value = json!({
        "versions": versions(),
        "capabilities": capabilities(),
        "limits": limits(),
        "filesystem": filesystem,
        "workspace": workspace,
        "not_checked": NOT_CHECKED,
        "uploads": "none: doctor sends nothing and this build has no telemetry",
    });
    Ok(Report {
        value,
        findings,
        private_paths,
    })
}

/// What doctor cannot establish from a read in this environment.
const NOT_CHECKED: &[&str] = &[
    "power-loss durability: doctor writes nothing, so publication and sync are not exercised; docs/support-matrix.md lists the tested configurations",
    "no-replace rename and directory sync on this mount are not probed; init and the first write fail closed if they are missing",
    "cloud synchronization: a sync agent over a local directory cannot be detected",
    "cache-loss crash qualification runs only in the explicit LazyFS test harness",
    "external tools and viewers are never probed",
];

/// The bounds this build enforces; `integration inspect` reports the same.
pub(crate) fn limits() -> Value {
    json!({
        "enforced": LIMITS,
        "expansion": EXPANSION,
    })
}

/// Versions of every contract this build speaks; shared with `integration inspect`.
pub(crate) fn versions() -> Value {
    let catalog = standard_catalog();
    let operations = catalog.operations().count();
    let available = catalog.available().count();
    let deferred = catalog
        .operations()
        .filter(|operation| operation.status() == Status::Deferred)
        .count();
    json!({
        "program": PROGRAM_NAME,
        "binary": env!("CARGO_PKG_VERSION"),
        "response": ResponseVersion::V1,
        "command": CommandVersion::V1,
        "catalog": {
            "command": CommandVersion::V1,
            "operations_available": available,
            "operations_reserved": deferred,
            "operations_total": operations,
        },
        "workspace_marker": WORKSPACE_VERSION,
        "record_family": RECORD_FAMILY,
        "journal_event": EVENT_VERSION,
        "index_schema": INDEX_USER_VERSION,
        "vault_renderer": RENDERER_VERSION,
        "render_manifest": MANIFEST_VERSION,
        "freshness_rules": FRESHNESS_RULES_VERSION,
        "gap_rules": GAP_RULE_VERSION,
        "next_rules": NEXT_RULE_VERSION,
        "method_pack": METHOD_PACK_VERSION,
        "capture_capabilities": CAPABILITIES_VERSION,
    })
}

/// Capture support, selector validation, readers and built-in method packs;
/// shared with `integration inspect`.
pub(crate) fn capabilities() -> Value {
    let mut readers: Vec<&str> = SELECTOR_SUPPORT
        .iter()
        .flat_map(|support| support.readers.iter().copied())
        .collect();
    readers.sort_unstable();
    readers.dedup();
    let builtin = investigation_core::method_pack_ops::resolve(&[], |_| None);
    json!({
        "capture": CaptureCapabilities::current(),
        "selectors": SELECTOR_SUPPORT,
        "readers": readers,
        "method_packs": {
            "built_in": builtin.iter().map(|pack| json!({
                "pack_id": pack.pack_id,
                "version": pack.version,
                "digest": pack.digest,
            })).collect::<Vec<_>>(),
            "pinned": "per case: pinned packs travel with the case; see workspace.method_packs",
        },
        "durable_writes": {
            "publication": "renameat2 RENAME_NOREPLACE or link(2); an existing target is never overwritten",
            "sync": "fsync of each file and its directory before a write is acknowledged; a failed fsync stops writes",
            "locking": "an advisory lock on state/locks/commit.lock serializes local writers",
            "imports": "openat2 with RESOLVE_BENEATH and RESOLVE_NO_SYMLINKS beneath an import root",
        },
    })
}

/// Classify the filesystem under `path`; returns the section and whether a
/// workspace there is refused.
fn filesystem(
    detector: &dyn FilesystemDetector,
    path: &Path,
    subject: &str,
    findings: &mut Vec<Finding>,
) -> (Value, bool) {
    let symlink_free_open = match ImportRoot::open(path) {
        Ok(_) => json!("supported"),
        Err(error) if matches!(error.reason, ImportErrorKind::Unsupported) => {
            findings.push(Finding::new(
                Severity::Error,
                "OPENAT2_UNSUPPORTED",
                "This kernel lacks openat2, so file imports are refused rather than opened without symlink protection.",
                "Run on a Linux kernel with openat2 (5.6 or later).",
            ));
            json!("unsupported")
        }
        Err(_) => json!("not_determined"),
    };
    let info = match detector.detect(path) {
        Ok(info) => info,
        Err(error) => {
            findings.push(Finding::new(
                Severity::Warning,
                "FILESYSTEM_UNIDENTIFIED",
                format!("The filesystem could not be identified: {error}."),
                "Check that the directory exists and is readable; init and open refuse a filesystem they cannot identify.",
            ));
            return (
                json!({"subject": subject, "identified": false, "symlink_free_open": symlink_free_open}),
                false,
            );
        }
    };
    let support = info.support();
    let refused = support.level == SupportLevel::Refused;
    if refused {
        findings.push(refused_finding(&info, subject));
    }
    (
        json!({
            "subject": subject,
            "identified": true,
            "kind": kind_name(&info),
            "name": info.name,
            "magic": info.magic.map(|magic| format!("{magic:#x}")),
            "support": support.level,
            "reason": support.reason,
            "symlink_free_open": symlink_free_open,
        }),
        refused,
    )
}

fn refused_finding(info: &FilesystemInfo, subject: &str) -> Finding {
    let support = info.support();
    let (code, place) = if subject == "workspace" {
        (
            "FILESYSTEM_REFUSED",
            "The workspace is on a filesystem that init and open refuse",
        )
    } else {
        (
            "FILESYSTEM_REFUSED_FOR_INIT",
            "The working directory is on a filesystem where init would refuse a workspace",
        )
    };
    Finding::new(
        Severity::Error,
        code,
        format!("{place}: {} is {}.", info.name, support.reason),
        "Keep canonical state on a local disk: create a workspace on a local filesystem with investigation init PATH and import the sources again there. Do not copy a live workspace through a network share.",
    )
}

fn kind_name(info: &FilesystemInfo) -> &'static str {
    use investigation_store::workspace::FilesystemKind;
    match info.kind {
        FilesystemKind::Local => "local",
        FilesystemKind::Network => "network",
        FilesystemKind::Fuse => "fuse",
        FilesystemKind::Unknown => "unknown",
        FilesystemKind::UnsupportedPlatform => "unsupported_platform",
    }
}

fn workspace_health(
    root: &Path,
    findings: &mut Vec<Finding>,
    private_paths: &mut Vec<(PathBuf, &'static str)>,
) -> Value {
    let workspace = match Workspace::open(root) {
        Ok(workspace) => workspace,
        Err(error) => {
            let (code, step) = match &error {
                WorkspaceError::InvalidConfig(_) => (
                    "WORKSPACE_CONFIG_INVALID",
                    "Correct investigation.toml; unknown top-level keys are errors and extensions must be namespaced.",
                ),
                WorkspaceError::FormatTooNew { .. } => (
                    "WORKSPACE_FORMAT_NEWER",
                    "A newer program wrote this workspace at a format this one cannot read; use that program. Nothing was changed.",
                ),
                WorkspaceError::UnsupportedWorkspaceVersion
                | WorkspaceError::UnsupportedRecordSchemaFamily => (
                    "WORKSPACE_VERSION_UNSUPPORTED",
                    "Open the workspace with the build that created it; this build does not migrate it.",
                ),
                _ => (
                    "WORKSPACE_UNUSABLE",
                    "Restore the workspace layout from a verified copy; doctor changes nothing.",
                ),
            };
            findings.push(Finding::new(Severity::Error, code, error.to_string(), step));
            return json!({"opened": false, "reason": code});
        }
    };
    // Longest first, so a root nested in another keeps its own marker.
    let mut roots = workspace.config().import_roots.clone();
    roots.sort_by_key(|root| std::cmp::Reverse(root.as_os_str().len()));
    let at = private_paths.len().min(1);
    private_paths.splice(
        at..at,
        roots.into_iter().map(|root| (root, "<IMPORT_ROOT>")),
    );
    let import_roots = import_roots(&workspace, findings);
    let vault_enabled = workspace.config().vault_policy.enabled;
    let workspace_id = workspace.id();
    let mut store = Store::open(workspace);
    let mut value = json!({
        "opened": true,
        "workspace_id": workspace_id,
        "import_roots": import_roots,
    });
    if let Ok((format, _)) = store.format_status() {
        value["format"] = json!(format);
    }
    match store.check() {
        Ok(report) => {
            let unsettled =
                report.staging.active + report.staging.sealed + report.staging.unallocated;
            value["lock"] = json!("free");
            value["cases"] = json!(report.heads.len());
            value["structurally_valid"] = json!(report.structurally_valid);
            value["verified_blobs"] = json!(report.verified_blobs);
            value["reference_only_artifacts"] = json!(report.reference_only_artifacts);
            value["index"] = json!(report.index);
            value["staging"] = json!(report.staging);
            value["unsettled_capture_handles"] = json!(unsettled);
            for issue in report.issues {
                let severity = match issue.severity {
                    CheckSeverity::Error => Severity::Error,
                    CheckSeverity::Warning => Severity::Warning,
                };
                let mut finding = Finding::new(
                    severity,
                    &issue.code,
                    issue.reason,
                    check_step(&issue.code, issue.repair.is_some()),
                );
                finding.objects = issue.objects;
                finding.repair.argv = Some(issue.repair.unwrap_or_else(|| argv(&["check"])));
                findings.push(finding);
            }
            if report.structurally_valid {
                value["method_packs"] = method_packs(&mut store, findings);
            }
            value["archive_pins"] = archive_pins(&mut store, findings, unix_now());
        }
        Err(failure) if failure.error.code == ErrorCode::WorkspaceBusy => {
            value["lock"] = json!("busy");
            findings.push(
                Finding::new(
                    Severity::Warning,
                    "WORKSPACE_BUSY",
                    failure.error.message,
                    "Another process holds the commit lock. Wait for it to finish and run doctor again.",
                )
                .argv(argv(&["doctor"])),
            );
        }
        Err(failure) => {
            findings.push(
                Finding::new(
                    Severity::Error,
                    "CHECK_FAILED",
                    failure.error.message,
                    "Run investigation check for the integrity details; doctor changes nothing.",
                )
                .argv(argv(&["check"])),
            );
        }
    }
    value["vault"] = if vault_enabled {
        vault(&mut store, findings)
    } else {
        json!({"enabled": false})
    };
    for finding in findings.iter_mut() {
        if let Some(argv) = &mut finding.repair.argv {
            scope_argv(store.workspace(), argv);
        }
    }
    value
}

fn check_step(code: &str, has_argv: bool) -> &'static str {
    match code {
        "INDEX_STALE" => {
            "Rebuild the disposable index from the verified journal; canonical state is unchanged."
        }
        "PRIOR_SYNC_FAILURE" => {
            "Retry any uncertain command with the same actor and command ID; check verified the current heads."
        }
        "MIGRATION_INCOMPLETE" => {
            "Run investigation migrate to see the plan, then finish it with migrate --apply and the same plan ID, or restore the backup it names with archive restore."
        }
        "MIGRATION_AVAILABLE" => {
            "Nothing migrates on its own. Run investigation migrate for the dry run before you decide."
        }
        "WORKSPACE_FORMAT_NEWER" => {
            "A newer program wrote this workspace; this one reads it and refuses writes. Use the newer program to write here."
        }
        _ if has_argv => "Run the repair command shown.",
        _ => {
            "Inspect with investigation check. Missing or changed canonical bytes need a restore from a verified copy or a fresh import of the original; doctor changes nothing."
        }
    }
}

fn import_roots(workspace: &Workspace, findings: &mut Vec<Finding>) -> Value {
    let roots = &workspace.config().import_roots;
    let mut unavailable = 0;
    for root in roots {
        if ImportRoot::open(root).is_err() {
            unavailable += 1;
            findings.push(Finding::new(
                Severity::Warning,
                "IMPORT_ROOT_UNAVAILABLE",
                format!(
                    "Import root {} cannot be opened without following a symlink, so files beneath it are refused.",
                    root.display()
                ),
                "Restore the directory, or add a reachable one with investigation init WORKSPACE --import-root DIR.",
            ));
        }
    }
    json!({"configured": roots.len(), "unavailable": unavailable})
}

fn method_packs(store: &mut Store, findings: &mut Vec<Finding>) -> Value {
    let Ok(cases) = store.cases() else {
        return json!(null);
    };
    let mut pinned = 0;
    let mut unavailable = 0;
    for (case, replay) in &cases {
        let Ok(packs) = store.method_packs(&replay.state, *case, None) else {
            continue;
        };
        for pack in packs.iter().filter(|pack| pack.pin.is_some()) {
            pinned += 1;
            if pack.is_available() {
                continue;
            }
            unavailable += 1;
            let mut finding = Finding::new(
                Severity::Warning,
                "METHOD_PACK_UNAVAILABLE",
                format!(
                    "Pinned method pack {} {} cannot be loaded from its retained bytes; its cards are not offered.",
                    pack.pack_id, pack.version
                ),
                "List the case's packs and the reason; adopt the pack again from its original document.",
            )
            .argv(argv(&["--case", &case.to_string(), "method", "list"]));
            finding.objects = vec![*case];
            findings.push(finding);
        }
    }
    json!({"pinned": pinned, "unavailable": unavailable})
}

/// Digests listed per pin; the counts are always complete.
const MAX_HELD_LISTED: usize = 50;

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Every archive pin, live or stale, with its heads, blob count, age and
/// what it holds back from reclamation; stale pins are one finding.
fn archive_pins(store: &mut Store, findings: &mut Vec<Finding>, now: i64) -> Value {
    // The holder view needs verified heads; without them, list the pins.
    let status = store.archive_pin_status().or_else(|_| {
        store.archive_pins().map(|pins| {
            pins.into_iter()
                .map(|pin| PinStatus {
                    holds_back: (!pin.live).then(Vec::new),
                    pin,
                })
                .collect::<Vec<_>>()
        })
    });
    let Ok(status) = status else {
        return json!(null);
    };
    let pins: Vec<Value> = status.iter().map(|entry| pin_value(entry, now)).collect();
    let stale: Vec<&PinInfo> = status
        .iter()
        .map(|entry| &entry.pin)
        .filter(|pin| !pin.live)
        .collect();
    if !stale.is_empty() {
        let listed: Vec<String> = stale
            .iter()
            .map(|pin| {
                let held = if pin.readable {
                    format!("{} case head(s), {} blob(s)", pin.heads.len(), pin.blobs)
                } else {
                    "record unreadable".to_owned()
                };
                format!("{} ({held}, {})", pin.id, age_text(pin.recorded_at, now))
            })
            .collect();
        findings.push(
            Finding::new(
                Severity::Warning,
                "ARCHIVE_PIN_STALE",
                format!(
                    "{} archive pin record(s) are stale: {}. No process holds them, so they protect no blob and hold nothing back from reclamation; an archive stopped without releasing its pin.",
                    stale.len(),
                    listed.join("; ")
                ),
                "Release the stale records; a live pin is never released. The next archive or reclamation would also remove them.",
            )
            .argv(argv(&["archive", "pins", "release", "--stale"])),
        );
    }
    json!({
        "live": status.iter().filter(|entry| entry.pin.live).count(),
        "stale": stale.len(),
        "pins": pins,
    })
}

fn pin_value(entry: &PinStatus, now: i64) -> Value {
    let pin = &entry.pin;
    let holds_back = entry.holds_back.as_ref().map_or_else(
        || json!({"every_blob": true, "reason": "the live pin's record cannot be read, so it holds every blob"}),
        |held| {
            json!({
                "blobs": held.len(),
                "bytes": held.iter().filter_map(|blob| blob.size_bytes).sum::<u64>(),
                "digests": held.iter().take(MAX_HELD_LISTED).map(|blob| &blob.digest).collect::<Vec<_>>(),
                "lists_capped_at": MAX_HELD_LISTED,
            })
        },
    );
    json!({
        "id": pin.id,
        "state": if pin.live { "live" } else { "stale" },
        "heads": pin.heads,
        "blobs": pin.blobs,
        "readable": pin.readable,
        "recorded_at_unix_seconds": pin.recorded_at,
        "age_seconds": pin.recorded_at.map(|at| now.saturating_sub(at).max(0)),
        "holds_back": holds_back,
    })
}

fn age_text(recorded_at: Option<i64>, now: i64) -> String {
    let Some(at) = recorded_at else {
        return "age unknown".to_owned();
    };
    let seconds = now.saturating_sub(at).max(0);
    match seconds {
        0..60 => format!("{seconds}s old"),
        60..3600 => format!("{}m old", seconds / 60),
        3600..86_400 => format!("{}h old", seconds / 3600),
        _ => format!("{}d old", seconds / 86_400),
    }
}

fn vault(store: &mut Store, findings: &mut Vec<Finding>) -> Value {
    let status = match vault_status(store.workspace()) {
        Ok(status) => status,
        Err(error) => {
            findings.push(
                Finding::new(
                    Severity::Warning,
                    "VAULT_UNREADABLE",
                    error.to_string(),
                    "Inspect the vault state; render again to rebuild its manifest.",
                )
                .argv(argv(&["vault", "status"])),
            );
            return json!({"enabled": true, "state": null});
        }
    };
    let Ok(snapshot) = VerifiedSnapshot::read(store) else {
        return json!({"enabled": true, "state": status.state.as_str()});
    };
    let current = Generation::new(
        RENDERER_VERSION,
        &snapshot,
        &working_policy(store.workspace()),
    );
    let is_current = status.is_current(&current);
    let has_cases = !current.heads().is_empty();
    let freshness = match (&status.state, is_current) {
        (_, true) => "current",
        (VaultState::Unrendered, false) => "unrendered",
        (_, false) => "stale",
    };
    match &status.state {
        VaultState::Updating { .. } => {}
        VaultState::Partial { reason, .. } => findings.push(
            Finding::new(
                Severity::Warning,
                "VAULT_PARTIAL",
                format!("The vault mixes render generations: {reason}."),
                "Render again to complete the generation; canonical state is unaffected.",
            )
            .argv(argv(&["vault", "render"])),
        ),
        _ if !is_current && has_cases => findings.push(
            Finding::new(
                Severity::Warning,
                "VAULT_STALE",
                "The vault does not show the current verified snapshot.",
                "Render the vault from the current snapshot; canonical reads are unaffected.",
            )
            .argv(argv(&["vault", "render"])),
        ),
        _ => {}
    }
    if !status.drift.is_empty() {
        findings.push(
            Finding::new(
                Severity::Warning,
                "VAULT_DRIFT",
                format!(
                    "{} generated vault file(s) were changed outside the renderer and are left as found.",
                    status.drift.len()
                ),
                "List the drifted files and where each withheld version is kept.",
            )
            .argv(argv(&["vault", "conflicts"])),
        );
    }
    json!({
        "enabled": true,
        "state": status.state.as_str(),
        "freshness": freshness,
        "drift": status.drift.len(),
    })
}

fn summary(findings: &[Finding]) -> Value {
    let count = |severity| {
        findings
            .iter()
            .filter(|finding| finding.severity == severity)
            .count()
    };
    json!({"errors": count(Severity::Error), "warnings": count(Severity::Warning)})
}

fn next(findings: &[Finding]) -> Vec<Vec<String>> {
    let mut next: Vec<Vec<String>> = Vec::new();
    for argv in findings
        .iter()
        .filter_map(|finding| finding.repair.argv.clone())
    {
        if !next.contains(&argv) {
            next.push(argv);
        }
    }
    next
}

fn doctor_output(report: Report) -> Output {
    let Report {
        mut value,
        findings,
        ..
    } = report;
    let healthy = findings.is_empty();
    let mut text = format!(
        "doctor  {}\nbinary  {PROGRAM_NAME} {}",
        if healthy {
            "healthy".to_owned()
        } else {
            format!("{} finding(s)", findings.len())
        },
        env!("CARGO_PKG_VERSION"),
    );
    let filesystem = &value["filesystem"];
    if filesystem["identified"] == true {
        text.push_str(&format!(
            "\nfilesystem  {}  {}  {}",
            filesystem["subject"].as_str().unwrap_or(""),
            filesystem["name"].as_str().unwrap_or(""),
            filesystem["support"].as_str().unwrap_or(""),
        ));
    }
    let workspace = &value["workspace"];
    if workspace["opened"] == true {
        text.push_str(&format!(
            "\nworkspace  {}  {} case(s)  index {}  lock {}",
            workspace["workspace_id"].as_str().unwrap_or(""),
            workspace["cases"],
            workspace["index"].as_str().unwrap_or("unknown"),
            workspace["lock"].as_str().unwrap_or("unknown"),
        ));
        let pins = &workspace["archive_pins"];
        if pins["pins"].as_array().is_some_and(|pins| !pins.is_empty()) {
            text.push_str(&format!(
                "\narchive pins  {} live  {} stale",
                pins["live"], pins["stale"]
            ));
        }
        if let Some(state) = workspace["vault"]["state"].as_str() {
            text.push_str(&format!(
                "\nvault  {state}  {}",
                workspace["vault"]["freshness"].as_str().unwrap_or("")
            ));
        }
    }
    for finding in &findings {
        text.push_str(&format!(
            "\n{}  {}  {}\nRepair: {}",
            serde_json::to_value(finding.severity)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            finding.code,
            finding.reason,
            finding.repair.step,
        ));
        if let Some(argv) = &finding.repair.argv {
            text.push_str(&format!("\n  {}", display_argv(argv)));
        }
    }
    value["healthy"] = json!(healthy);
    value["summary"] = summary(&findings);
    value["next"] = json!(next(&findings));
    value["findings"] = json!(findings);
    Output::success("doctor", value, text)
}

/// What the archive would contain and what it leaves out, with the report it
/// would carry already redacted.
fn bundle_preview(report: Report) -> Output {
    let Report {
        mut value,
        findings,
        private_paths,
    } = report;
    value["healthy"] = json!(findings.is_empty());
    value["summary"] = summary(&findings);
    value["findings"] = json!(findings);
    redact(&mut value, &private_paths);
    let entries = json!([
        {
            "name": "doctor.json",
            "contents": "the redacted doctor report shown under report: versions, capabilities, limits, filesystem classification, workspace health counts and findings",
        },
    ]);
    let excluded = json!([
        {"category": "evidence_content", "reason": "retained bytes, excerpts, quotes, observation text and every other recorded or authored text stay out; the report holds only IDs, digests, counts, versions, diagnostic codes and fixed diagnostic text"},
        {"category": "credentials", "reason": "environment variables, actor settings and investigation.toml are not read into the archive"},
        {"category": "local_paths", "reason": "the workspace location, the working directory, the home directory, import roots and every other absolute local path are replaced with markers"},
        {"category": "artifact_paths_and_names", "reason": "artifact names, source paths and locators stay out"},
        {"category": "journal_and_blobs", "reason": "canonical events and blobs carry recorded content"},
        {"category": "index_and_vault", "reason": "the disposable index and vault notes hold record text"},
    ]);
    let markers: Vec<_> = private_paths
        .iter()
        .map(|(_, marker)| *marker)
        .chain([ELIDED])
        .collect();
    let text = format!(
        "bundle preview  nothing written, nothing sent\ninclude  doctor.json  redacted doctor report ({} finding(s))\nexclude  evidence content, credentials, local paths, artifact names, journal, blobs, index, vault",
        value["findings"].as_array().map_or(0, Vec::len),
    );
    Output::success(
        "doctor_bundle_preview",
        json!({
            "archive": {
                "written": false,
                "uploaded": false,
                "selection": "explicit: only doctor bundle selects the archive, for the person who runs it; nothing is sent to any consumer",
                "status": "preview only: this build lists the archive and does not write it",
            },
            "entries": entries,
            "excluded": excluded,
            "redaction_markers": markers,
            "report": value,
        }),
        text,
    )
}

/// Elide every remaining whitespace-delimited token that is an absolute path,
/// wherever it is rooted.
fn elide_absolute_paths(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|token| {
            let word = token.trim_end();
            let core = word.trim_start_matches(['(', '\'', '"', '`', '[']);
            if core.len() > 1 && core.starts_with('/') {
                format!(
                    "{}{ELIDED}{}",
                    &word[..word.len() - core.len()],
                    &token[word.len()..]
                )
            } else {
                token.to_owned()
            }
        })
        .collect()
}

/// Replace each private location with its marker, then elide any remaining
/// absolute local path, in every string of `value`.
fn redact(value: &mut Value, private_paths: &[(PathBuf, &'static str)]) {
    match value {
        Value::String(text) => {
            let mut redacted = text.clone();
            for (path, marker) in private_paths {
                let path = path.to_string_lossy();
                if !path.is_empty() && path != "/" {
                    redacted = redacted.replace(path.as_ref(), marker);
                }
            }
            *text = elide_absolute_paths(&elide_local_paths(&redacted));
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| redact(item, private_paths)),
        Value::Object(map) => map
            .values_mut()
            .for_each(|item| redact(item, private_paths)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use investigation_store::workspace::FilesystemKind;

    use super::*;

    struct Fixed(FilesystemKind, &'static str);

    impl FilesystemDetector for Fixed {
        fn detect(&self, _: &Path) -> io::Result<FilesystemInfo> {
            Ok(FilesystemInfo {
                kind: self.0,
                name: self.1.to_owned(),
                magic: Some(0x6969),
            })
        }
    }

    #[test]
    fn a_network_workspace_is_a_refusal_finding_with_a_repair_not_a_failure() {
        let directory = tempfile::tempdir().unwrap();
        let mut findings = Vec::new();
        let (section, refused) = filesystem(
            &Fixed(FilesystemKind::Network, "nfs"),
            directory.path(),
            "workspace",
            &mut findings,
        );
        assert!(refused);
        assert_eq!(section["support"], "refused");
        assert_eq!(section["kind"], "network");
        let finding = findings
            .iter()
            .find(|finding| finding.code == "FILESYSTEM_REFUSED")
            .expect("refusal finding");
        assert_eq!(finding.severity, Severity::Error);
        assert!(
            finding.reason.contains("network filesystem"),
            "{}",
            finding.reason
        );
        assert!(finding.reason.contains("locking"), "{}", finding.reason);
        assert!(finding.repair.step.contains("local filesystem"));
    }

    #[test]
    fn fuse_and_unknown_filesystems_are_refused_and_tmpfs_is_test_use_without_a_finding() {
        let directory = tempfile::tempdir().unwrap();
        for (kind, name, refused) in [
            (FilesystemKind::Fuse, "fuse", true),
            (FilesystemKind::Unknown, "unknown", true),
            (FilesystemKind::Local, "tmpfs", false),
            (FilesystemKind::Local, "ext", false),
        ] {
            let mut findings = Vec::new();
            let (_, was_refused) = filesystem(
                &Fixed(kind, name),
                directory.path(),
                "working_directory",
                &mut findings,
            );
            assert_eq!(was_refused, refused, "{name}");
            let codes: Vec<_> = findings
                .iter()
                .map(|finding| finding.code.as_str())
                .collect();
            if refused {
                assert!(
                    codes.contains(&"FILESYSTEM_REFUSED_FOR_INIT"),
                    "{name}: {codes:?}"
                );
            } else {
                assert!(
                    !codes.iter().any(|code| code.starts_with("FILESYSTEM")),
                    "{codes:?}"
                );
            }
        }
    }

    #[test]
    fn redaction_replaces_private_locations_and_elides_other_local_paths() {
        let mut value = json!({
            "argv": ["investigation", "--workspace", "/srv/cases/w", "check"],
            "reason": "cannot read /home/someone/notes.txt or /srv/cases/w/state",
        });
        redact(
            &mut value,
            &[(PathBuf::from("/srv/cases/w"), "<WORKSPACE>")],
        );
        assert_eq!(value["argv"][2], "<WORKSPACE>");
        assert_eq!(
            value["reason"],
            format!("cannot read {ELIDED} or <WORKSPACE>/state")
        );
        // `/scratch/` is a known root, elided with its punctuation kept;
        // `/data2/` is not, and the bundle's catch-all elides the token.
        let mut value = json!("under /scratch/x/y, see state/locks and (/data2/z)");
        redact(&mut value, &[]);
        assert_eq!(
            value,
            format!("under {ELIDED}, see state/locks and ({ELIDED}")
        );
    }
}
