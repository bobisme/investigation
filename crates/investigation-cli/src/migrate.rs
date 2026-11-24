//! `migrate`: the dry run of a workspace format migration by default, and
//! with `--apply --plan ID` the migration itself (plan §10.7, W11.15,
//! ADR 0014). The steps, backup and reverse are in
//! `investigation_render::migrate`; the migrations, and what they may and
//! may not change, in `investigation_store::migration`.
//!
//! This program ships no migration: every format written so far is read as
//! it is. So the dry run reports format 1 as current, and `--apply` has
//! nothing to do. The command exists so that the first real migration
//! arrives with its procedure already tested.

use std::{fmt::Write as _, path::PathBuf};

use investigation_core::{
    PROGRAM_NAME,
    wire::{ErrorCode, Failure},
};
use investigation_render::{
    VaultRenderer,
    migrate::{self, Applied, MigrationPlan, Stopped},
    render_vault, working_policy,
};
use investigation_store::{Store, migration::Registry, workspace::FORMAT_VERSION};
use serde_json::{Value, json};

use crate::Output;

pub(crate) const AFTER_HELP: &str = "\
Without --apply, migrate is a dry run: it runs check, reports the workspace
format and the migrations this program has for it, in order, with the areas
each changes and what no migration ever changes, and prints a plan ID. It
writes nothing. Reads never migrate a workspace.

With --apply --plan ID it runs that plan and no other: a verified workspace
archive as the backup first (by default under exports/), then the workspace is
marked as migrating, so writes are refused and check reports it, then each
migration, a rebuilt index and check, and last the new format. A run stopped
at any point is finished by the same command. archive restore BACKUP PATH
reverses it exactly. Case journals, blobs, record and command IDs,
investigation.toml and the human vault folders are never changed.

This program has no migration yet: format 1 is current for every workspace
made so far.

Examples:
  investigation migrate --format json
  investigation migrate --apply --plan mig_0123abcd --format json
  investigation migrate --apply --plan mig_0123abcd --backup ../pre-migration --format json

Next: investigation check --format json";

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Run the plan the dry run printed; without it, migrate only reports.
    #[arg(long, requires = "plan")]
    apply: bool,
    /// The plan ID the dry run printed; --apply runs only that plan.
    #[arg(long, value_name = "ID", requires = "apply")]
    plan: Option<String>,
    /// The backup directory, new or empty; default exports/migration-<from>-to-<to>-<head>.
    #[arg(long, value_name = "DIR", requires = "apply")]
    backup: Option<PathBuf>,
}

pub(crate) fn run(store: &mut Store, args: &Args) -> Result<Output, Failure> {
    let registry = Registry::builtin();
    if !args.apply {
        let plan = migrate::plan(store, &registry)?;
        return Ok(plan_output(store, &plan));
    }
    let plan_id = args.plan.as_deref().unwrap_or_default();
    let applied = migrate::apply(
        store,
        &registry,
        plan_id,
        args.backup.as_deref(),
        crate::schema::published_documents(),
    )
    .map_err(|stopped| match stopped {
        Stopped::Failed(failure) => failure,
        Stopped::Interrupted(step) => Failure::new(
            ErrorCode::IoFailure,
            format!("The migration was stopped at {step:?}."),
        ),
    })?;
    let vault = render_after(store);
    Ok(applied_output(store, &applied, vault))
}

fn argv(store: &Store, tail: &[&str]) -> Vec<String> {
    let mut argv = vec![
        PROGRAM_NAME.to_owned(),
        "--workspace".to_owned(),
        store.workspace().root().to_string_lossy().into_owned(),
    ];
    argv.extend(tail.iter().map(|part| (*part).to_owned()));
    argv.extend(["--format".to_owned(), "json".to_owned()]);
    argv
}

fn plan_output(store: &Store, plan: &MigrationPlan) -> Output {
    let next = match &plan.plan_id {
        Some(id) => argv(store, &["migrate", "--apply", "--plan", id]),
        None => argv(store, &["check"]),
    };
    let value = json!({
        "dry_run": true,
        "workspace_id": plan.workspace_id,
        "status": plan.status,
        "format": plan.format,
        "this_program_writes_format": FORMAT_VERSION,
        "from": plan.from,
        "to": plan.to,
        "resume": plan.resume,
        "migrations": plan.migrations,
        "areas": plan.areas.iter().map(|(area, holds)| json!({"area": area, "holds": holds})).collect::<Vec<_>>(),
        "never_changed": plan.never_changed,
        "heads": plan.heads,
        "recovery_head": plan.recovery_head,
        "plan_id": plan.plan_id,
        "default_backup": plan.default_backup,
        "refusal": plan.refusal,
        "next": [{"kind": if plan.plan_id.is_some() { "write" } else { "local_read" }, "argv": next}],
    });
    let mut text = format!(
        "{}  format {}  this program writes format {FORMAT_VERSION}",
        plan.status, plan.format.version
    );
    if plan.migrations.is_empty() {
        text.push_str("\nno migration to apply; nothing was changed");
    }
    for migration in &plan.migrations {
        let _ = write!(
            text,
            "\n{}  {}  changes {}",
            migration.version,
            migration.summary,
            migration.touches.join(", ")
        );
        for change in &migration.changes {
            let _ = write!(text, "\n  {change}");
        }
    }
    if let Some(reason) = &plan.refusal {
        let _ = write!(text, "\nrefused: {reason}");
    }
    if let Some(id) = &plan.plan_id {
        let _ = write!(
            text,
            "\nplan {id}\nbackup first to {}\nnever changed: {}",
            plan.default_backup.display(),
            plan.never_changed.join("; ")
        );
    }
    let _ = write!(text, "\nNext: {}", crate::display_argv(&next));
    Output::success("migration_planned", value, text)
}

/// Render the vault after a migration, like a restore: the projection is
/// rebuilt, never migrated in place by hand.
fn render_after(store: &mut Store) -> Value {
    let vault = &store.workspace().config().vault_policy;
    if !vault.enabled {
        return json!({"rendered": false, "reason": "The vault is disabled in investigation.toml."});
    }
    let policy = working_policy(store.workspace());
    match render_vault(store, &VaultRenderer, &policy, &mut |_| {}) {
        Ok(report) => json!({"rendered": true, "generation": report.publish.generation.id()}),
        Err(error) => json!({"rendered": false, "reason": error.to_string()}),
    }
}

fn applied_output(store: &Store, applied: &Applied, vault: Value) -> Output {
    let backup = applied.backup.to_string_lossy().into_owned();
    let reverse = [
        PROGRAM_NAME,
        "archive",
        "restore",
        backup.as_str(),
        "<NEW_PATH>",
        "--format",
        "json",
    ]
    .map(str::to_owned)
    .to_vec();
    let check = argv(store, &["check"]);
    let value = json!({
        "workspace_id": applied.plan.workspace_id,
        "plan_id": applied.plan_id,
        "from": applied.plan.from,
        "to": applied.plan.to,
        "migrations": applied.plan.migrations,
        "format": applied.format,
        "backup": {
            "directory": backup,
            "export_id": applied.backup_export_id,
            "reused": applied.backup_reused,
            "recovery_head": applied.plan.recovery_head,
        },
        "check": {
            "ok": true,
            "heads": applied.check.heads,
            "verified_blobs": applied.check.verified_blobs,
            "index": applied.check.index,
        },
        "vault": vault,
        "reverse": {
            "argv": reverse,
            "note": "Restores the workspace as it was before the migration, byte for byte. Use it in place of this workspace, never beside it: they share a workspace ID.",
        },
        "next": [{"kind": "local_read", "argv": check}],
    });
    let text = format!(
        "migrated  format {} -> {}  {}\nbackup {}  {}\ncheck passed; every case head and investigation.toml equal the backup's\nreverse: {}\nNext: {}",
        applied.plan.from,
        applied.plan.to,
        applied.plan_id,
        backup,
        applied.backup_export_id,
        crate::display_argv(&reverse),
        crate::display_argv(&check)
    );
    Output::success("migrated", value, text)
}
