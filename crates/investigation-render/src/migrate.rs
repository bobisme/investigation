//! `migrate`: move a workspace to a later format, dry run first, with a
//! verified backup before any change and an exact reverse from that backup
//! (plan §10.7, W11.15, ADR 0014). The migrations themselves, and what they
//! may and may not change, are in `investigation_store::migration`.
//!
//! [`plan`] is the dry run. It runs `check`, lists the pending migrations
//! in order with the areas each changes and what none ever changes, and
//! names the plan with an ID: a digest of the workspace, the formats, the
//! migrations and the recovery head over every case head. It writes
//! nothing.
//!
//! [`apply`] runs only the plan its ID names, in these steps:
//!
//! 1. **Backup.** A workspace archive (W11.08) of every case at the
//!    planned heads, written to a new directory (by default
//!    `exports/migration-<from>-to-<to>-<recovery head>`), then verified as
//!    `archive verify` does, which restores a private copy and runs `check`
//!    on it. A backup that does not verify, or that holds other heads, stops
//!    the migration before any change.
//! 2. **Mark.** Under the coordination lock, only while every case head is
//!    still the backup's, the marker's format gains `migrating`, naming the
//!    plan and the backup. From here every write is refused, by every
//!    program that reads the section, and `check` refuses the workspace as
//!    `MIGRATION_INCOMPLETE`.
//! 3. **Apply** each migration in order. Each is idempotent.
//! 4. **Rebuild** the disposable index from the journal.
//! 5. **Check.** `check` must pass, every case head must equal the
//!    backup's, and the owner configuration must equal the backup's bytes.
//! 6. **Publish.** Under the lock, with the heads unchanged, the marker's
//!    format becomes the new one, without `migrating`.
//!
//! A kill before step 2 leaves the workspace as it was, which `check`
//! accepts; a kill from step 2 to 6 leaves `migrating`, which `check`
//! refuses with the command that finishes it. Either way `apply` with the
//! same plan ID finishes: the plan ID does not depend on the marker, the
//! backup is found again by its export ID and verified again, and every
//! migration runs again. The backup is never removed.
//!
//! **Reverse.** `archive restore BACKUP PATH` restores the workspace as it
//! was before step 2, byte for byte in every canonical file, the marker
//! included (W11.18). Use it in place of the migrated workspace, never
//! beside it: they share a workspace ID.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use investigation_core::{
    PROGRAM_NAME,
    identity::Id,
    wire::{Digest, ErrorCode, Failure},
};
use investigation_store::{
    Store,
    journal::hash_bytes,
    migration::{Area, NEVER_CHANGED, Registry},
    store::{CaseHead, CheckReport},
    workspace::{Migrating, WorkspaceFormat},
};
use serde::Serialize;
use serde_json::json;

use crate::export::{
    self, ExportManifest, MANIFEST_FILE,
    archive::{ARCHIVE_AUDIENCE, canonical_member, plan_archive, recovery_head},
    source::read_archive_source,
    verify::verify_export,
    write_export,
};

/// Version of the plan ID's input.
pub const PLAN_VERSION: &str = "inv.migration-plan.v1";
/// Folder under the workspace that default backups go to.
const BACKUP_FOLDER: &str = "exports";
/// Backups tried at the default name before giving up: the name, then
/// `-2` and so on, past directories an interrupted backup left.
const MAX_BACKUP_NAMES: u32 = 100;
/// `check` codes that a migration in progress raises by design.
const MIGRATION_CODES: [&str; 1] = ["MIGRATION_INCOMPLETE"];

/// One pending migration, as the dry run shows it.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedMigration {
    pub version: u32,
    pub name: &'static str,
    pub summary: &'static str,
    /// The areas it changes.
    pub touches: Vec<&'static str>,
    /// What it would change here, described without writing.
    pub changes: Vec<String>,
    pub read_requires: u32,
    pub write_requires: u32,
}

/// Where the workspace stands and what [`apply`] would do.
#[derive(Debug, Clone, Serialize)]
pub struct MigrationPlan {
    pub workspace_id: Id,
    /// The format section on disk now.
    pub format: WorkspaceFormat,
    /// `current`, `migration_available`, `migration_incomplete` or `newer`.
    pub status: &'static str,
    pub from: u32,
    pub to: u32,
    pub migrations: Vec<PlannedMigration>,
    /// The areas the pending migrations change, with what each holds.
    pub areas: Vec<(&'static str, &'static str)>,
    /// What no migration changes.
    pub never_changed: &'static [&'static str],
    pub heads: Vec<CaseHead>,
    /// The digest a workspace archive of these heads records.
    pub recovery_head: Digest,
    /// The ID [`apply`] needs; `None` when there is nothing to apply.
    pub plan_id: Option<String>,
    /// Whether the plan finishes a migration that started earlier.
    pub resume: bool,
    /// Where the backup goes unless `--backup` names a directory.
    pub default_backup: PathBuf,
    /// Why nothing can be applied, when nothing can.
    pub refusal: Option<String>,
}

/// Points a test can stop [`apply`] at, as a kill there would.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The backup is written, not yet verified.
    BackedUp,
    /// The backup is verified; nothing in the workspace has changed.
    Verified,
    /// The marker holds `migrating`.
    Marked,
    /// The migration to this format has run.
    Applied(u32),
    /// The index is rebuilt.
    Rebuilt,
    /// `check` passed; the marker still holds `migrating`.
    Checked,
    /// The new format is published.
    Published,
}

/// A finished migration.
#[derive(Debug, Clone)]
pub struct Applied {
    pub plan: MigrationPlan,
    pub plan_id: String,
    /// The verified workspace archive taken before any change.
    pub backup: PathBuf,
    pub backup_export_id: String,
    /// Whether an earlier run's backup was found and verified again.
    pub backup_reused: bool,
    /// The format published.
    pub format: WorkspaceFormat,
    /// The `check` that passed before publication.
    pub check: CheckReport,
}

/// Why [`apply`] stopped.
#[derive(Debug)]
pub enum Stopped {
    /// Refused or failed, with the reason and what to do.
    Failed(Failure),
    /// Stopped at a [`Step`] by a test, with no cleanup.
    #[doc(hidden)]
    Interrupted(Step),
}

impl From<Failure> for Stopped {
    fn from(failure: Failure) -> Self {
        Self::Failed(failure)
    }
}

fn migrate_argv(root: &Path, extra: &[&str]) -> Vec<String> {
    let mut argv = vec![
        PROGRAM_NAME.to_owned(),
        "--workspace".to_owned(),
        root.to_string_lossy().into_owned(),
        "migrate".to_owned(),
    ];
    argv.extend(extra.iter().map(|part| (*part).to_owned()));
    argv.extend(["--format".to_owned(), "json".to_owned()]);
    argv
}

fn refused(message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message.into())
}

/// The dry run: where the workspace stands and what [`apply`] would do.
/// Writes nothing.
pub fn plan(store: &mut Store, registry: &Registry) -> Result<MigrationPlan, Failure> {
    let workspace_id = store.workspace().id();
    let root = store.workspace().root().to_path_buf();
    let (format, _) = store.format_status()?;
    let check = store.check()?;
    let heads = check.heads.clone();
    let recovery = recovery_head(
        workspace_id,
        heads.iter().map(|head| {
            (
                head.case_id,
                head.snapshot.sequence,
                &head.snapshot.head_hash,
            )
        }),
    );
    let (status, from, to, resume, mut refusal) = match &format.migrating {
        Some(migrating) if registry.get(migrating.to).is_none() => (
            "migration_incomplete",
            migrating.from,
            migrating.to,
            true,
            Some(format!(
                "A migration to format {} started with a program that has other migrations, and \
                 did not finish. Finish it with that program, or restore its backup (export {}) \
                 with archive restore.",
                migrating.to, migrating.backup_export_id
            )),
        ),
        Some(migrating) => (
            "migration_incomplete",
            migrating.from,
            migrating.to,
            true,
            None,
        ),
        None if format.version > registry.latest() => (
            "newer",
            format.version,
            format.version,
            false,
            Some(format!(
                "The workspace is at format {}, later than any this program migrates to ({}). \
                 There is nothing to migrate.",
                format.version,
                registry.latest()
            )),
        ),
        None if format.version == registry.latest() => {
            ("current", format.version, format.version, false, None)
        }
        None => (
            "migration_available",
            format.version,
            registry.latest(),
            false,
            None,
        ),
    };
    let failing: Vec<&str> = check
        .issues
        .iter()
        .filter(|issue| {
            issue.severity == investigation_store::store::Severity::Error
                && !MIGRATION_CODES.contains(&issue.code.as_str())
        })
        .map(|issue| issue.code.as_str())
        .collect();
    if refusal.is_none() && from < to && (!check.structurally_valid || !failing.is_empty()) {
        refusal = Some(format!(
            "check fails ({}); a workspace is migrated only when its journals verify. Run check \
             and repair what it reports first.",
            failing.join(", ")
        ));
    }
    let mut migrations = Vec::new();
    let mut areas: Vec<Area> = Vec::new();
    if from < to {
        for migration in registry.pending(from) {
            if migration.version() > to {
                break;
            }
            let changes = migration
                .describe(store.workspace())
                .unwrap_or_else(|error| vec![format!("cannot describe the change: {error}")]);
            let (read_requires, write_requires) = migration.requires();
            areas.extend(migration.touches());
            migrations.push(PlannedMigration {
                version: migration.version(),
                name: migration.name(),
                summary: migration.summary(),
                touches: migration
                    .touches()
                    .iter()
                    .map(|area| area.as_str())
                    .collect(),
                changes,
                read_requires,
                write_requires,
            });
        }
    }
    areas.sort();
    areas.dedup();
    let plan_id = (refusal.is_none() && from < to).then(|| {
        let mut text = format!("{PLAN_VERSION}\nworkspace {workspace_id}\nfrom {from}\nto {to}\n");
        for migration in &migrations {
            text.push_str(&format!(
                "migration {} {} {} {}\n",
                migration.version,
                migration.name,
                migration.read_requires,
                migration.write_requires
            ));
        }
        text.push_str(&format!("recovery {recovery}\n"));
        hash_bytes(text.as_bytes())
            .as_str()
            .replacen("sha256:", "mig_", 1)
    });
    let default_backup = root.join(BACKUP_FOLDER).join(format!(
        "migration-{from}-to-{to}-{}",
        &recovery.as_str().trim_start_matches("sha256:")[..12]
    ));
    Ok(MigrationPlan {
        workspace_id,
        format,
        status,
        from,
        to,
        migrations,
        areas: areas
            .into_iter()
            .map(|area| (area.as_str(), area.description()))
            .collect(),
        never_changed: &NEVER_CHANGED,
        heads,
        recovery_head: recovery,
        plan_id,
        resume,
        default_backup,
        refusal,
    })
}

/// Run the plan `plan_id` names; see the module documentation. `backup`
/// is the backup directory, by default the plan's `default_backup`.
/// `schemas` are the published contracts the backup carries, as for
/// `export --kind workspace-archive`.
pub fn apply(
    store: &mut Store,
    registry: &Registry,
    plan_id: &str,
    backup: Option<&Path>,
    schemas: Vec<(String, Vec<u8>)>,
) -> Result<Applied, Stopped> {
    apply_until(store, registry, plan_id, backup, schemas, None)
}

/// [`apply`], stopped at `stop` as a kill there would stop it.
#[doc(hidden)]
pub fn apply_until(
    store: &mut Store,
    registry: &Registry,
    plan_id: &str,
    backup: Option<&Path>,
    schemas: Vec<(String, Vec<u8>)>,
    stop: Option<Step>,
) -> Result<Applied, Stopped> {
    let at = |step: Step| -> Result<(), Stopped> {
        if stop == Some(step) {
            return Err(Stopped::Interrupted(step));
        }
        Ok(())
    };
    let root = store.workspace().root().to_path_buf();
    let plan = plan(store, registry)?;
    if let Some(reason) = &plan.refusal {
        return Err(refused(format!("{reason} Nothing was changed.")).into());
    }
    let Some(planned) = plan.plan_id.clone() else {
        return Err(refused(format!(
            "The workspace is at format {}, the latest this program migrates to. There is \
             nothing to migrate and nothing was changed.",
            plan.format.version
        ))
        .into());
    };
    if planned != plan_id {
        let mut failure = refused(format!(
            "The plan changed since the dry run that printed {plan_id}: the workspace, its case \
             heads or the migrations differ. Nothing was changed. Read the new plan and apply \
             its ID."
        ))
        .at("--plan")
        .with_argv(migrate_argv(&root, &[]));
        failure.error.expected = Some(json!({ "plan_id": plan_id }));
        failure.error.actual = Some(json!({ "plan_id": planned }));
        return Err(failure.into());
    }

    // 1. Backup, written and verified before any change.
    let (backup_dir, backup_export_id, backup_reused, config_digest) =
        take_backup(store, &plan, backup, schemas, &at)?;
    at(Step::Verified)?;

    // 2. Mark, under the lock, while the heads are the backup's.
    let heads = head_triples(&plan.heads);
    let marked = match &plan.format.migrating {
        Some(migrating) => {
            if migrating.backup_export_id != backup_export_id {
                return Err(refused(format!(
                    "The migration in progress took the backup {}, and {} holds {}. Pass \
                     --backup with the directory of {}. Nothing was changed.",
                    migrating.backup_export_id,
                    backup_dir.display(),
                    backup_export_id,
                    migrating.backup_export_id
                ))
                .at("--backup")
                .into());
            }
            plan.format.clone()
        }
        None => {
            let marked = WorkspaceFormat {
                migrating: Some(Migrating {
                    from: plan.from,
                    to: plan.to,
                    plan_id: planned.clone(),
                    backup_export_id: backup_export_id.clone(),
                }),
                ..plan.format.clone()
            };
            store.publish_format(&plan.format, marked.clone(), &heads)?;
            marked
        }
    };
    at(Step::Marked)?;

    // 3. Apply every migration, in order.
    let failed = |step: &str, reason: String| {
        refused(format!(
            "The migration stopped while {step}: {reason} The workspace is marked as migrating, \
             so writes are refused and check reports it. Run migrate --apply --plan {planned} \
             again to finish, or restore the backup with archive restore {}.",
            backup_dir.display()
        ))
        .with_argv(migrate_argv(&root, &["--apply", "--plan", &planned]))
    };
    let mut last = (plan.format.read_requires, plan.format.write_requires);
    for migration in registry.pending(plan.from) {
        if migration.version() > plan.to {
            break;
        }
        migration
            .apply(store.workspace())
            .map_err(|error| failed(&format!("migration {} ran", migration.name()), error.0))?;
        last = migration.requires();
        at(Step::Applied(migration.version()))?;
    }

    // 4. Rebuild the disposable index.
    store
        .repair_index()
        .map_err(|failure| failed("the index was rebuilt", failure.error.message))?;
    at(Step::Rebuilt)?;

    // 5. Check: journals, heads and configuration exactly as backed up.
    let check = store
        .check()
        .map_err(|failure| failed("check ran", failure.error.message))?;
    let failing: Vec<&str> = check
        .issues
        .iter()
        .filter(|issue| {
            issue.severity == investigation_store::store::Severity::Error
                && !MIGRATION_CODES.contains(&issue.code.as_str())
        })
        .map(|issue| issue.code.as_str())
        .collect();
    if !check.structurally_valid || !failing.is_empty() {
        return Err(failed(
            "check ran",
            format!("check fails ({}).", failing.join(", ")),
        )
        .into());
    }
    if head_triples(&check.heads) != heads {
        return Err(failed(
            "the case heads were compared",
            "a case head differs from the backup's.".to_owned(),
        )
        .into());
    }
    let config = fs::read(root.join("investigation.toml"))
        .map_err(|error| failed("the configuration was read", error.to_string()))?;
    if Some(hash_bytes(&config)) != config_digest {
        return Err(failed(
            "the configuration was compared",
            "investigation.toml differs from the backup's; no migration may change it.".to_owned(),
        )
        .into());
    }
    at(Step::Checked)?;

    // 6. Publish the new format.
    let format = WorkspaceFormat {
        version: plan.to,
        read_requires: last.0,
        write_requires: last.1,
        migrating: None,
    };
    store.publish_format(&marked, format.clone(), &heads)?;
    at(Step::Published)?;
    Ok(Applied {
        plan,
        plan_id: planned,
        backup: backup_dir,
        backup_export_id,
        backup_reused,
        format,
        check,
    })
}

fn head_triples(heads: &[CaseHead]) -> Vec<(Id, u64, Digest)> {
    heads
        .iter()
        .map(|head| {
            (
                head.case_id,
                head.snapshot.sequence,
                head.snapshot.head_hash.clone(),
            )
        })
        .collect()
}

/// What a directory holds, for choosing a backup.
enum Found {
    Absent,
    Empty,
    /// A finished export, with its manifest.
    Export(Box<ExportManifest>),
    /// Files and no manifest: an unfinished export, or something else.
    Other,
}

fn inspect(dir: &Path) -> Result<Found, Failure> {
    let unreadable = |error: io::Error| {
        refused(format!("{} cannot be read ({error}).", dir.display())).at("--backup")
    };
    match fs::symlink_metadata(dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Found::Absent),
        Err(error) => return Err(unreadable(error)),
        Ok(metadata) if !metadata.is_dir() => return Ok(Found::Other),
        Ok(_) => {}
    }
    if fs::read_dir(dir).map_err(unreadable)?.next().is_none() {
        return Ok(Found::Empty);
    }
    match fs::read(dir.join(MANIFEST_FILE)) {
        Ok(bytes) => Ok(serde_json::from_slice::<ExportManifest>(&bytes)
            .map(|manifest| Found::Export(Box::new(manifest)))
            .unwrap_or(Found::Other)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Found::Other),
        Err(error) => Err(unreadable(error)),
    }
}

/// Whether `manifest` is a workspace archive of exactly the planned heads.
fn is_backup_of(manifest: &ExportManifest, plan: &MigrationPlan) -> bool {
    manifest.kind == "workspace-archive"
        && manifest.snapshot.case_id == plan.workspace_id
        && manifest.snapshot.head_hash == plan.recovery_head
}

/// Find or write the backup, then verify it. Returns its directory, export
/// ID, whether it was reused, and the digest of the configuration it holds.
fn take_backup(
    store: &mut Store,
    plan: &MigrationPlan,
    requested: Option<&Path>,
    schemas: Vec<(String, Vec<u8>)>,
    at: &dyn Fn(Step) -> Result<(), Stopped>,
) -> Result<(PathBuf, String, bool, Option<Digest>), Stopped> {
    let candidates: Vec<PathBuf> = match requested {
        Some(dir) => vec![dir.to_path_buf()],
        None => (1..=MAX_BACKUP_NAMES)
            .map(|n| match n {
                1 => plan.default_backup.clone(),
                n => {
                    let mut name = plan.default_backup.clone().into_os_string();
                    name.push(format!("-{n}"));
                    PathBuf::from(name)
                }
            })
            .collect(),
    };
    let resuming = plan.format.migrating.as_ref();
    let mut chosen = None;
    for dir in &candidates {
        match inspect(dir)? {
            Found::Export(manifest) if is_backup_of(&manifest, plan) => {
                if resuming.is_none_or(|m| m.backup_export_id == manifest.export_id) {
                    chosen = Some((dir.clone(), true));
                    break;
                }
            }
            Found::Absent | Found::Empty if resuming.is_none() => {
                chosen = Some((dir.clone(), false));
                break;
            }
            _ if requested.is_some() => {
                return Err(refused(format!(
                    "{} holds something other than a backup of these case heads{}. A backup is \
                     written only into a new or empty directory; choose another --backup. \
                     Nothing was changed.",
                    dir.display(),
                    resuming.map_or(String::new(), |m| format!(
                        " (the migration in progress took export {})",
                        m.backup_export_id
                    ))
                ))
                .at("--backup")
                .into());
            }
            _ => {}
        }
    }
    let Some((dir, reused)) = chosen else {
        let message = match resuming {
            Some(migrating) => format!(
                "The backup the migration in progress took (export {}) is not at {} or a \
                 numbered name beside it. Pass --backup with its directory. Nothing was changed.",
                migrating.backup_export_id,
                plan.default_backup.display()
            ),
            None => format!(
                "No free backup directory beside {}; pass --backup DIR. Nothing was changed.",
                plan.default_backup.display()
            ),
        };
        return Err(refused(message).at("--backup").into());
    };
    if !reused {
        write_backup(store, plan, &dir, schemas)?;
        at(Step::BackedUp)?;
    }
    // Verified now, a reused one too: the scratch copy is made beside the
    // backup and removed afterwards.
    let scratch = dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let report = verify_export(&dir, &scratch).map_err(|error| {
        refused(format!(
            "The backup {} cannot be read ({error}).",
            dir.display()
        ))
    })?;
    let verified = report.ok()
        && report.archive.as_ref().is_some_and(|archive| {
            archive.recovery_head == plan.recovery_head
                && archive.workspace_id == plan.workspace_id
                && archive.rehearsal.as_ref().is_some_and(|r| r.check_ok)
        });
    if !verified {
        let codes: Vec<&str> = report.problems.iter().map(|p| p.code.as_str()).collect();
        let mut failure = Failure::new(
            ErrorCode::IntegrityFailure,
            format!(
                "The backup {} did not verify ({}). Nothing in the workspace was changed; the \
                 backup is left for inspection.",
                dir.display(),
                if codes.is_empty() {
                    "it holds other heads".to_owned()
                } else {
                    codes.join(", ")
                }
            ),
        );
        failure.error.actual = Some(json!({ "problems": report.problems }));
        return Err(failure.into());
    }
    let export_id = report.export_id.clone().unwrap_or_default();
    let manifest = match inspect(&dir)? {
        Found::Export(manifest) => manifest,
        _ => return Err(refused("The verified backup lost its manifest.").into()),
    };
    let member = canonical_member("investigation.toml");
    let config = manifest
        .files
        .iter()
        .find(|file| file.path == member)
        .map(|file| file.sha256.clone());
    Ok((dir, export_id, reused, config))
}

/// Write a workspace archive of `store` into `dir`, which must be absent
/// or empty. Refused when its heads are not the plan's.
fn write_backup(
    store: &mut Store,
    plan: &MigrationPlan,
    dir: &Path,
    schemas: Vec<(String, Vec<u8>)>,
) -> Result<(), Stopped> {
    if let Some(parent) = dir.parent()
        && !parent.exists()
    {
        // Only the default backup folder, exports/, is ever created here.
        fs::create_dir_all(parent)
            .map_err(|error| refused(format!("{} cannot be made ({error}).", parent.display())))?;
    }
    let spec = export::kind("workspace-archive").expect("the workspace archive kind is registered");
    let (source, pin) = read_archive_source(store, schemas)?;
    let archive =
        plan_archive(spec, &source, ARCHIVE_AUDIENCE).map_err(|error| error.to_failure())?;
    if archive.manifest().snapshot.head_hash != plan.recovery_head {
        return Err(refused(
            "The workspace changed since the dry run: a case head moved. Nothing was changed. \
             Run migrate again for a new plan.",
        )
        .into());
    }
    write_export(&archive, dir).map_err(|error| error.to_failure().at("--backup"))?;
    let _ = store.release_pin(pin);
    Ok(())
}
