//! `init --existing-vault VAULT [--managed-prefix FOLDER]`: write the
//! existing-vault settings (`[vault_policy] enabled`, `existing_vault`,
//! `managed_prefix`) that W12.06 documented as hand edits, after the checks a
//! render would make anyway, so a bad setting is refused with a repair before
//! anything is written.
//!
//! What it checks, and writes nothing to any vault:
//!
//! - the vault is an existing directory; its canonical path (no links) is
//!   what is stored, and it must not contain the workspace or lie inside it;
//! - the prefix passes [`check_managed_prefix`]; a refusal repairs to a
//!   sanitized prefix;
//! - the prefix's parent folders exist in the vault as real folders (the
//!   renderer creates only the prefix folder itself), and the prefix, when it
//!   exists, is a real folder;
//! - on an existing workspace, a change of projection root is allowed only
//!   while the render manifest owns no generated path
//!   ([`projection::check_change`]); otherwise the repair is `vault remove`
//!   under the current setting first. This is also the route for changing
//!   the prefix: remove, `init --existing-vault VAULT --managed-prefix NEW`,
//!   render. Files of the person under the old prefix are never moved.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use investigation_core::wire::{ErrorCode, Failure};
use investigation_render::{
    RENDER_LOCK_WAIT, RenderError,
    lock::RenderLock,
    projection::{self, Projection},
    remove::preview,
};
use investigation_store::{
    Workspace,
    workspace::{MAX_PREFIX_COMPONENTS, check_managed_prefix},
};
use serde_json::{Value, json};

use crate::{first_use, vault::argv};

/// The prefix used when `--managed-prefix` is not given.
pub(crate) const DEFAULT_PREFIX: &str = "Investigation";

/// A checked existing-vault setting: the canonical vault and the prefix.
pub(crate) struct Setting {
    pub(crate) vault: PathBuf,
    pub(crate) prefix: String,
    /// Whether the prefix folder already exists in the vault.
    pub(crate) prefix_exists: bool,
}

fn replaced(flag: &str, old: &str, new: &str) -> Option<Vec<String>> {
    first_use::with_replaced_value(env::args_os(), flag, Path::new(old), Path::new(new))
}

fn with_repair(failure: Failure, argv: Option<Vec<String>>) -> Failure {
    match argv {
        Some(argv) => failure.with_argv(argv),
        None => failure,
    }
}

/// A prefix that passes [`check_managed_prefix`], made from `prefix`: unsafe
/// characters become `-`, hidden, empty and `..` folders are dropped, at most
/// the last four folders are kept. Falls back to [`DEFAULT_PREFIX`].
pub(crate) fn sanitized_prefix(prefix: &str) -> String {
    let folders: Vec<String> = prefix
        .split(['/', '\\'])
        .map(|folder| {
            let cleaned: String = folder
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                        c
                    } else {
                        '-'
                    }
                })
                .collect();
            let cleaned = cleaned.trim_start_matches(['.', ' ', '-']);
            let cleaned = cleaned.trim_end_matches(['.', ' ', '-']);
            cleaned.chars().take(64).collect::<String>()
        })
        .filter(|folder| !folder.is_empty())
        .collect();
    let start = folders.len().saturating_sub(MAX_PREFIX_COMPONENTS);
    let candidate = folders[start..].join("/");
    if check_managed_prefix(&candidate).is_ok() {
        candidate
    } else {
        DEFAULT_PREFIX.to_owned()
    }
}

/// Check `--existing-vault` and `--managed-prefix` for a workspace at
/// `workspace` (which may not exist yet). Reads the vault's folders on the
/// prefix's path only; writes nothing.
pub(crate) fn check(
    vault: &Path,
    prefix: Option<&str>,
    workspace: &Path,
) -> Result<Setting, Failure> {
    let given = prefix.is_some();
    let prefix = prefix.unwrap_or(DEFAULT_PREFIX);
    let canonical = fs::canonicalize(vault)
        .ok()
        .filter(|path| path.is_dir())
        .ok_or_else(|| {
            Failure::new(
                ErrorCode::InvalidInput,
                format!(
                    "Existing vault {} is not an existing folder. Name the folder that holds your \
                     notes; init creates nothing in it.",
                    vault.display()
                ),
            )
            .at("--existing-vault")
        })?;
    let target = std::path::absolute(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let target = fs::canonicalize(&target).unwrap_or(target);
    if canonical.starts_with(&target) || target.starts_with(&canonical) {
        return Err(Failure::new(
            ErrorCode::InvalidInput,
            format!(
                "The workspace and the existing vault {} must be apart: neither may lie inside \
                 the other, so canonical state is never synchronized as notes. Put the \
                 workspace outside the vault.",
                canonical.display()
            ),
        )
        .at("--existing-vault"));
    }
    if let Err(reason) = check_managed_prefix(prefix) {
        let suggestion = sanitized_prefix(prefix);
        let failure = Failure::new(
            ErrorCode::InvalidInput,
            format!(
                "Managed prefix {prefix:?} {reason}. Use one to four folder names of ASCII \
                 letters, digits, spaces, -, _ and ., none hidden, such as {suggestion:?}."
            ),
        )
        .at("--managed-prefix");
        return Err(with_repair(
            failure,
            replaced("--managed-prefix", prefix, &suggestion),
        ));
    }
    let folders: Vec<&str> = prefix.split('/').collect();
    let mut path = canonical.clone();
    for (index, folder) in folders.iter().enumerate() {
        path.push(folder);
        let last = index + 1 == folders.len();
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                let suggestion = format!("{} Casebook", folders[..=index].join("/"));
                let suggestion = sanitized_prefix(&suggestion);
                let failure = Failure::new(
                    ErrorCode::InvalidInput,
                    format!(
                        "{} in the existing vault is a file or a link, not a folder; the renderer \
                         never follows a link or replaces a file. Choose another prefix, such as \
                         {suggestion:?}.",
                        folders[..=index].join("/")
                    ),
                )
                .at("--managed-prefix");
                let argv = if given {
                    replaced("--managed-prefix", prefix, &suggestion)
                } else {
                    None
                };
                return Err(with_repair(failure, argv));
            }
            Err(_) if last => {
                return Ok(Setting {
                    vault: canonical,
                    prefix: prefix.to_owned(),
                    prefix_exists: false,
                });
            }
            Err(_) => {
                let suggestion = folders[index + 1..].join("/");
                let failure = Failure::new(
                    ErrorCode::InvalidInput,
                    format!(
                        "The folder {} does not exist in the existing vault. The renderer \
                         creates only the prefix folder itself: create {} in your vault first, \
                         or use a prefix whose parent folders exist, such as {suggestion:?}.",
                        folders[..=index].join("/"),
                        folders[..=index].join("/")
                    ),
                )
                .at("--managed-prefix");
                return Err(with_repair(
                    failure,
                    replaced("--managed-prefix", prefix, &suggestion),
                ));
            }
        }
    }
    Ok(Setting {
        vault: canonical,
        prefix: prefix.to_owned(),
        prefix_exists: true,
    })
}

/// Write `setting` to an existing workspace's config. Refused while the
/// render manifest owns generated paths under another root. Returns whether
/// the config changed.
pub(crate) fn apply(workspace: &mut Workspace, setting: &Setting) -> Result<bool, Failure> {
    let to = Projection::Existing {
        vault: setting.vault.clone(),
        prefix: setting.prefix.clone(),
    };
    // Held across the check and the config write, so no render, reconcile,
    // copy-map or removal adds an owned path between them. Lock order: this
    // render lock first, then the commit lock that set_existing_vault takes,
    // the order a render uses ([`projection::check_change`]).
    let lock = match RenderLock::acquire(workspace, RENDER_LOCK_WAIT) {
        Ok(lock) => lock,
        Err(error @ RenderError::Busy { .. }) => {
            let mut failure = error.to_failure();
            failure.error.message = format!(
                "{} The projection setting can change only between renders; nothing was \
                 written. Run this init again when the render finishes.",
                failure.error.message
            );
            return Err(failure.at("--existing-vault"));
        }
        Err(error) => return Err(error.to_failure().at("--existing-vault")),
    };
    if let Err(error) = projection::check_change(workspace, &to, &lock) {
        // After a removal, only generated files someone edited stay owned:
        // they need a reconcile decision, not another removal.
        drop(lock);
        let only_edited =
            preview(workspace).is_ok_and(|report| report.removed() == 0 && report.kept() > 0);
        let (step, repair): (&str, &[&str]) = if only_edited {
            (
                "Generated files you edited stay owned after vault remove: give each one up with \
                 vault reconcile PATH --disposition release-path (it stays where it is, as yours) \
                 or preserve-as-note, as vault conflicts lists them, then run vault remove and \
                 this init again",
                &["vault", "conflicts"],
            )
        } else {
            (
                "Run vault remove under the current setting first (it deletes only unedited \
                 generated files and never your notes), then run this init again",
                &["vault", "remove", "--dry-run"],
            )
        };
        let mut failure = error.to_failure();
        failure.error.message = format!(
            "The projection cannot move to {} yet: {}. {step}. Nothing was written.",
            to.describe(),
            error.to_string().trim_end_matches('.')
        );
        return Err(failure
            .at("--existing-vault")
            .with_argv(argv(workspace, repair)));
    }
    let changed = workspace
        .set_existing_vault(&setting.vault, &setting.prefix)
        .map_err(|error| crate::selection::workspace_failure(error).at("--existing-vault"));
    drop(lock);
    changed
}

/// The `projection` value and text line of the init response. The vault's
/// path is not echoed, as in `vault status`.
pub(crate) fn describe(setting: &Setting, changed: Option<bool>) -> (Value, String) {
    let folder = if setting.prefix_exists {
        "exists; a first render refuses it while it holds files the renderer does not own"
    } else {
        "created by the first vault render"
    };
    let mut value = json!({
        "mode": "existing_vault",
        "managed_prefix": setting.prefix,
        "prefix_folder": if setting.prefix_exists { "exists" } else { "absent" },
    });
    if let Some(changed) = changed {
        value["setting"] = json!(if changed { "written" } else { "unchanged" });
    }
    let text = format!(
        "projection  {}/ in the existing vault ({folder}); nothing outside it is written or read\n",
        setting.prefix
    );
    (value, text)
}
