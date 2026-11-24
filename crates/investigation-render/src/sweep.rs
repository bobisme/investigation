//! Cleanup of temporary files that a killed render left behind.
//!
//! The publisher writes through hidden temporaries beside each target:
//! `.{name}.{tag}-{n}.inv-tmp` before a rename into place, and
//! `.{name}.{tag}-{n}.inv-del` for a retired file moved aside before it is
//! unlinked. A render killed between those steps leaves them behind; enough
//! of them block later temporaries with the same tag.
//!
//! The sweep runs under the render lock, before a render publishes, and
//! touches only names that match that pattern exactly for a target the
//! manifest or the plan manages, in that target's own directory. Directories
//! are opened from the vault handle without following links; a leftover that
//! is not a regular file is left alone. Then:
//!
//! - an `.inv-tmp` file is unlinked: it never reached its target, so it holds
//!   no bytes anyone else could rely on;
//! - an `.inv-del` file is unlinked only when its bytes are proven generated
//!   ([`crate::Manifest::accepts`] for its target); otherwise it is renamed
//!   back to its target without replacing anything, or kept where it is when
//!   the target exists again. Unproven bytes are never deleted.
//!
//! A same-user process that swaps a different file in under such a name
//! between the check and the unlink can still lose that file; the renderer
//! lock does not coordinate other writers.

use std::{
    collections::{BTreeMap, BTreeSet},
    os::fd::{AsFd, OwnedFd},
};

use investigation_core::wire::Digest;
use investigation_store::Workspace;
use rustix::{
    fs::{AtFlags, Dir, FileType, RenameFlags, fsync, renameat_with, statat, unlinkat},
    io::Errno,
};

use crate::{
    OnDisk, PlanPath, Publisher, RenderError, layout,
    lock::open_local,
    publish::{TEMP_ATTEMPTS, Walk, inspect},
};

/// Suffix of a temporary written before its rename into place.
pub const TEMP_SUFFIX: &str = "inv-tmp";
/// Suffix of a retired file moved aside before it is unlinked.
pub const ASIDE_SUFFIX: &str = "inv-del";

const MANIFEST_NAME: &str = "render-manifest.json";

/// One leftover the sweep handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Swept {
    /// Path of the leftover: workspace-relative, such as
    /// `vault/Cases/<id>/Questions/.<id>.md.<tag>-0.inv-tmp` or
    /// `local/.render-manifest.json.<tag>-0.inv-tmp`; in existing-vault mode
    /// a projection leftover is vault-relative and starts with the managed
    /// prefix (`Investigation/Cases/...`).
    pub path: String,
    pub action: SweptAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweptAction {
    /// Unlinked: an unpublished temporary, or a moved-aside file whose bytes
    /// were proven generated.
    Removed,
    /// A moved-aside file with unproven bytes, renamed back to its target.
    Restored,
    /// A moved-aside file with unproven bytes whose target exists again. It
    /// stays where it is for a human to reconcile.
    Kept,
}

/// The target and suffix of a leftover name, when `name` is exactly
/// `.{target}.{tag}-{n}.{suffix}` with a tag and attempt the publisher uses.
pub(crate) fn leftover_target(name: &str) -> Option<(&str, &'static str)> {
    let rest = name.strip_prefix('.')?;
    let (rest, suffix) = [TEMP_SUFFIX, ASIDE_SUFFIX].into_iter().find_map(|suffix| {
        rest.strip_suffix(suffix)
            .and_then(|rest| rest.strip_suffix('.'))
            .map(|rest| (rest, suffix))
    })?;
    let (target, attempt) = rest.rsplit_once('.')?;
    let (tag, attempt) = attempt.rsplit_once('-')?;
    let tag_ok = tag == "render"
        || tag == "manifest"
        || (tag.len() == 12
            && tag
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    let attempt_ok = attempt.len() == 1
        && attempt
            .parse::<usize>()
            .is_ok_and(|attempt| attempt < TEMP_ATTEMPTS);
    (tag_ok && attempt_ok && !target.is_empty()).then_some((target, suffix))
}

/// Names in `directory` that are leftovers for one of `targets`, sorted.
fn leftovers<'t>(
    directory: &OwnedFd,
    label: &str,
    targets: &'t BTreeMap<String, PlanPath>,
) -> Result<Vec<(String, &'t PlanPath, &'static str)>, RenderError> {
    let mut found = Vec::new();
    let mut listing = Dir::read_from(directory).map_err(|error| RenderError::io(label, error))?;
    while let Some(item) = listing.read() {
        let item = item.map_err(|error| RenderError::io(label, error))?;
        let Ok(name) = item.file_name().to_str() else {
            continue;
        };
        let Some((target, suffix)) = leftover_target(name) else {
            continue;
        };
        if let Some(path) = targets.get(target) {
            found.push((name.to_owned(), path, suffix));
        }
    }
    found.sort();
    Ok(found)
}

fn is_regular(directory: &OwnedFd, name: &str, label: &str) -> Result<bool, RenderError> {
    match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile),
        Err(Errno::NOENT) => Ok(false),
        Err(error) => Err(RenderError::io(label, error)),
    }
}

fn unlink(directory: &OwnedFd, name: &str, label: &str) -> Result<bool, RenderError> {
    match unlinkat(directory, name, AtFlags::empty()) {
        Ok(()) => Ok(true),
        Err(Errno::NOENT) => Ok(false),
        Err(error) => Err(RenderError::io(label, error)),
    }
}

impl Publisher<'_> {
    /// Clean leftovers beside every path in `targets`. `accepts` says whether
    /// bytes at a path are proven generated. Directories that are missing or
    /// reached through a link are skipped.
    pub fn sweep(
        &self,
        targets: &BTreeSet<PlanPath>,
        accepts: &dyn Fn(&PlanPath, &Digest) -> bool,
    ) -> Result<Vec<Swept>, RenderError> {
        let mut by_directory: BTreeMap<Vec<&str>, BTreeMap<String, PlanPath>> = BTreeMap::new();
        // A human-owned area is never swept, even for a target a faulty plan
        // named there.
        for path in targets.iter().filter(|path| !layout::is_human_owned(path)) {
            let mut components: Vec<&str> = path.components().collect();
            let name = components.pop().expect("plan paths have a component");
            by_directory
                .entry(components)
                .or_default()
                .insert(name.to_owned(), path.clone());
        }
        let mut swept = Vec::new();
        for (parents, names) in &by_directory {
            let Some(first) = names.values().next() else {
                continue;
            };
            let Walk::Directory(directory) = self.walk(first, parents, false)? else {
                continue;
            };
            let prefix = self.folder_label(parents);
            let mut changed = false;
            for (name, target, suffix) in leftovers(&directory, &prefix, names)? {
                let label = format!("{prefix}/{name}");
                if !is_regular(&directory, &name, &label)? {
                    continue;
                }
                let action = if suffix == TEMP_SUFFIX {
                    if !unlink(&directory, &name, &label)? {
                        continue;
                    }
                    SweptAction::Removed
                } else {
                    let proven = match inspect(&directory, target, &name)? {
                        OnDisk::Regular { sha256 } => accepts(target, &sha256),
                        OnDisk::Absent => continue,
                        OnDisk::Other => false,
                    };
                    if proven {
                        if !unlink(&directory, &name, &label)? {
                            continue;
                        }
                        SweptAction::Removed
                    } else {
                        match renameat_with(
                            &directory,
                            name.as_str(),
                            &directory,
                            target.file_name(),
                            RenameFlags::NOREPLACE,
                        ) {
                            Ok(()) => SweptAction::Restored,
                            Err(Errno::EXIST) => SweptAction::Kept,
                            Err(Errno::NOENT) => continue,
                            Err(error) => return Err(RenderError::io(label, error)),
                        }
                    }
                };
                changed |= action != SweptAction::Kept;
                swept.push(Swept {
                    path: label,
                    action,
                });
            }
            if changed {
                fsync(directory.as_fd()).map_err(|error| RenderError::io(prefix, error))?;
            }
        }
        Ok(swept)
    }
}

/// Unlink unpublished manifest temporaries in `local/`.
pub(crate) fn sweep_local(workspace: &Workspace) -> Result<Vec<Swept>, RenderError> {
    let local = open_local(workspace.root())?;
    let mut targets = BTreeMap::new();
    targets.insert(
        MANIFEST_NAME.to_owned(),
        PlanPath::new(MANIFEST_NAME).expect("a valid name"),
    );
    let mut swept = Vec::new();
    for (name, _, suffix) in leftovers(&local, "local", &targets)? {
        let label = format!("local/{name}");
        if suffix != TEMP_SUFFIX || !is_regular(&local, &name, &label)? {
            continue;
        }
        if unlink(&local, &name, &label)? {
            swept.push(Swept {
                path: label,
                action: SweptAction::Removed,
            });
        }
    }
    if !swept.is_empty() {
        fsync(local.as_fd()).map_err(|error| RenderError::io("local", error))?;
    }
    Ok(swept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_publishers_exact_names_are_leftovers() {
        let tag = "0123456789ab";
        assert_eq!(
            leftover_target(&format!(".Start Here.md.{tag}-0.inv-tmp")),
            Some(("Start Here.md", TEMP_SUFFIX))
        );
        assert_eq!(
            leftover_target(".q_1.md.render-7.inv-del"),
            Some(("q_1.md", ASIDE_SUFFIX))
        );
        assert_eq!(
            leftover_target(".render-manifest.json.manifest-3.inv-tmp"),
            Some(("render-manifest.json", TEMP_SUFFIX))
        );
        for other in [
            "q_1.md",
            ".q_1.md.inv-tmp",
            &format!("q_1.md.{tag}-0.inv-tmp"),
            &format!(".q_1.md.{tag}-8.inv-tmp"),
            &format!(".q_1.md.{tag}-00.inv-tmp"),
            ".q_1.md.0123456789AB-0.inv-tmp",
            ".q_1.md.0123456789a-0.inv-tmp",
            ".q_1.md.mine-0.inv-tmp",
            &format!(".q_1.md.{tag}-0.inv-tmp.bak"),
            &format!(".q_1.md.{tag}-0.tmp"),
            &format!("..{tag}-0.inv-tmp"),
        ] {
            assert_eq!(leftover_target(other), None, "{other}");
        }
    }
}
