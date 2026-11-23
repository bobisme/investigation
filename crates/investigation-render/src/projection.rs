//! Where the projection lives: the workspace's own `vault/` (the dedicated
//! vault), or one managed prefix folder inside a vault a person already has
//! (existing-vault mode).
//!
//! Existing-vault mode is configured in the workspace's `investigation.toml`:
//!
//! ```toml
//! [vault_policy]
//! enabled = true
//! existing_vault = "/home/me/Notes"
//! managed_prefix = "Investigation"
//! ```
//!
//! The publisher's root is then `<existing_vault>/<managed_prefix>` instead
//! of `vault/`. Every plan path, manifest path, drift record and reconcile
//! destination is relative to that root, and every directory below it is
//! opened from its handle without following links, so no write, rename or
//! removal can reach a path outside the prefix. Inside the prefix the
//! ownership table is the dedicated vault's ([`crate::layout::VAULT_AREAS`]):
//! `Notes/`, `Boards/` and `Inbox/` there are the person's, and
//! `preserve-as-note` moves an edit to `Notes/Reconciled/` there.
//!
//! The rules that keep the rest of the vault untouched:
//!
//! - The vault must be an absolute, canonical path to an existing directory
//!   (no symbolic link anywhere on it), outside the workspace, with the
//!   workspace outside it, so canonical state is never synchronized as notes.
//! - The prefix's parent folders must already exist as real directories; the
//!   renderer creates only the prefix folder itself, and only to render.
//! - Nothing outside the prefix is opened for writing, listed or read, not
//!   even the viewer's configuration folder: settings, themes, plugins and
//!   property types are never changed. Generated properties are `inv_`
//!   prefixed and Bases filter on the exact `inv_case_id`, so a person's
//!   notes never appear in a generated view, and every generated link and
//!   Canvas file path starts with the prefix ([`crate::layout::LinkRoot`]),
//!   so none resolves to a person's note with the same name.
//! - The first render into a prefix that already holds files refuses unless
//!   each is owned or already holds exactly the bytes planned for it
//!   ([`check_first_render`]). The renderer never takes over a folder that is
//!   already someone's.
//! - The manifest records the root it was written for ([`Projection`]); a
//!   render, reconcile or removal under a different root refuses, so a moved
//!   setting never lets the record of one folder act on another.
//! - [`crate::remove`] deletes only owned files whose bytes are still the
//!   ones generated, then the folders that leaves empty, the prefix last.

use std::{
    fs,
    os::fd::OwnedFd,
    path::{Path, PathBuf},
};

use investigation_store::Workspace;
use rustix::{
    fs::{CWD, Mode, OFlags, fsync, mkdirat, openat},
    io::Errno,
};

use crate::{
    OnDisk, PlanPath, Publisher, RenderError, RenderPlan,
    publish::{inspect_named, remove_empty_directory},
};

const DIRECTORY: OFlags = OFlags::DIRECTORY
    .union(OFlags::RDONLY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Files named in a refusal; the count is always complete.
const MAX_NAMED: usize = 5;

/// The projection root a workspace renders into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Projection {
    /// The workspace's own `vault/`.
    Dedicated,
    /// The managed `prefix` folder of the person's vault at `vault`.
    Existing { vault: PathBuf, prefix: String },
}

impl Projection {
    /// The projection the workspace's vault policy configures.
    pub fn of(workspace: &Workspace) -> Self {
        match workspace.config().vault_policy.existing() {
            Some((vault, prefix)) => Self::Existing {
                vault: vault.to_owned(),
                prefix: prefix.to_owned(),
            },
            None => Self::Dedicated,
        }
    }

    /// The managed prefix, in existing-vault mode.
    pub fn prefix(&self) -> Option<&str> {
        match self {
            Self::Dedicated => None,
            Self::Existing { prefix, .. } => Some(prefix),
        }
    }

    /// The root in words, for messages.
    pub fn describe(&self) -> String {
        match self {
            Self::Dedicated => "the workspace's vault/".to_owned(),
            Self::Existing { vault, prefix } => {
                format!("{prefix}/ in the existing vault {}", vault.display())
            }
        }
    }

    /// The prefix's components, in existing-vault mode.
    fn components(&self) -> Vec<&str> {
        self.prefix()
            .map(|prefix| prefix.split('/').collect())
            .unwrap_or_default()
    }
}

fn refuse(reason: impl Into<String>) -> RenderError {
    RenderError::Projection {
        reason: reason.into(),
    }
}

/// Open the projection root without following links. `create` makes the
/// managed prefix folder when it is absent (only a render asks); otherwise an
/// absent prefix is `None`. The workspace's own `vault/` is never created
/// here.
pub(crate) fn open_root(
    workspace: &Workspace,
    create: bool,
) -> Result<Option<OwnedFd>, RenderError> {
    if !workspace.config().vault_policy.enabled {
        return Err(RenderError::VaultDisabled);
    }
    let projection = Projection::of(workspace);
    let Projection::Existing { vault, .. } = &projection else {
        let root = openat(
            CWD,
            workspace.root(),
            OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| RenderError::io(".", error))?;
        return openat(&root, "vault", DIRECTORY, Mode::empty())
            .map(Some)
            .map_err(|error| match error {
                Errno::LOOP | Errno::NOTDIR => RenderError::UnsafeDirectory("vault"),
                error => RenderError::io("vault", error),
            });
    };
    let mut directory = open_vault(workspace, vault)?;
    let components = projection.components();
    let (last, parents) = components
        .split_last()
        .expect("a validated prefix has a component");
    for (depth, component) in parents.iter().enumerate() {
        directory = match openat(&directory, *component, DIRECTORY, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => {
                return Err(refuse(format!(
                    "the folder {}/ that holds the managed prefix does not exist in the vault; \
                     the renderer creates only the prefix folder itself, so create it first",
                    components[..=depth].join("/")
                )));
            }
            Err(Errno::LOOP | Errno::NOTDIR) => {
                return Err(refuse(format!(
                    "{}/ in the vault is a symbolic link or not a folder; the renderer never \
                     follows links",
                    components[..=depth].join("/")
                )));
            }
            Err(error) => return Err(RenderError::io(components[..=depth].join("/"), error)),
        };
    }
    let prefix = components.join("/");
    match openat(&directory, *last, DIRECTORY, Mode::empty()) {
        Ok(fd) => Ok(Some(fd)),
        Err(Errno::NOENT) if create => {
            match mkdirat(&directory, *last, Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(error) => return Err(RenderError::io(prefix, error)),
            }
            fsync(&directory).map_err(|error| RenderError::io(prefix.as_str(), error))?;
            openat(&directory, *last, DIRECTORY, Mode::empty())
                .map(Some)
                .map_err(|error| unsafe_prefix(&prefix, error))
        }
        Err(Errno::NOENT) => Ok(None),
        Err(error) => Err(unsafe_prefix(&prefix, error)),
    }
}

fn unsafe_prefix(prefix: &str, error: Errno) -> RenderError {
    match error {
        Errno::LOOP | Errno::NOTDIR => refuse(format!(
            "the managed prefix {prefix}/ is a symbolic link or a file, not a folder; the \
             renderer never follows links"
        )),
        error => RenderError::io(prefix, error),
    }
}

/// Open the person's vault, after checking it is a canonical directory that
/// neither contains nor lies inside the workspace.
fn open_vault(workspace: &Workspace, vault: &Path) -> Result<OwnedFd, RenderError> {
    let shown = vault.display();
    match fs::canonicalize(vault) {
        Ok(canonical) if canonical == vault => {}
        Ok(_) => {
            return Err(refuse(format!(
                "the existing vault {shown} must be a canonical path, without symbolic links"
            )));
        }
        Err(error) => {
            return Err(refuse(format!(
                "the existing vault {shown} cannot be opened: {error}"
            )));
        }
    }
    let root = workspace.root();
    if vault.starts_with(root) || root.starts_with(vault) {
        return Err(refuse(format!(
            "the existing vault {shown} must be outside the workspace, and the workspace \
             outside it"
        )));
    }
    openat(CWD, vault, DIRECTORY, Mode::empty()).map_err(|error| match error {
        Errno::LOOP | Errno::NOTDIR => refuse(format!(
            "the existing vault {shown} is not a folder; the renderer never follows links"
        )),
        error => refuse(format!(
            "the existing vault {shown} cannot be opened: {}",
            std::io::Error::from(error)
        )),
    })
}

/// Remove the managed prefix folder when it is empty, after a removal. One
/// non-recursive `rmdir`, which the kernel refuses while anything remains.
/// Best effort: `true` when it was removed.
pub(crate) fn remove_root_if_empty(workspace: &Workspace) -> bool {
    let projection = Projection::of(workspace);
    let Projection::Existing { vault, .. } = &projection else {
        return false;
    };
    let Ok(mut directory) = open_vault(workspace, vault) else {
        return false;
    };
    let components = projection.components();
    let Some((last, parents)) = components.split_last() else {
        return false;
    };
    for component in parents {
        let Ok(next) = openat(&directory, *component, DIRECTORY, Mode::empty()) else {
            return false;
        };
        directory = next;
    }
    remove_empty_directory(&directory, last)
}

/// The first render into a managed prefix: refuse when the prefix holds any
/// file that is neither claimed by `claims` nor already holding exactly the
/// bytes `plan` puts at its path. Hidden entries are never read, adopted or
/// changed, so they do not count; empty folders do not count either.
pub(crate) fn check_first_render(
    publisher: &Publisher<'_>,
    plan: &RenderPlan,
    claims: &dyn Fn(&str) -> bool,
) -> Result<(), RenderError> {
    let mut foreign = Vec::new();
    publisher.visit(true, &mut |relative, directory, name, _| {
        if claims(relative) {
            return Ok(());
        }
        let planned = PlanPath::new(relative)
            .ok()
            .and_then(|path| plan.entry(path.as_str()));
        let identical = match planned {
            Some(entry) => {
                let name = name.to_string_lossy();
                matches!(
                    inspect_named(directory, relative, &name)?,
                    OnDisk::Regular { sha256 } if &sha256 == entry.sha256()
                )
            }
            None => false,
        };
        if !identical {
            foreign.push(relative.to_owned());
        }
        Ok(())
    })?;
    if foreign.is_empty() {
        return Ok(());
    }
    foreign.sort();
    let named: Vec<&str> = foreign.iter().take(MAX_NAMED).map(String::as_str).collect();
    Err(refuse(format!(
        "the managed prefix already holds {} file(s) the renderer does not own, such as {}; \
         choose an empty or new managed_prefix, or move those files out of it",
        foreign.len(),
        named.join(", ")
    )))
}
