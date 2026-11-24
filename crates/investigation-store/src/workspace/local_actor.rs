//! The machine-local actor setting in `local/actor.toml`.
//!
//! `init DIR --actor ACTOR` writes it. A write that names no actor any other
//! way (`--actor`, a request envelope, the `actor` key of `investigation.toml`
//! or `AGENT`) is attributed to it instead of being refused. It lives under
//! `local/`, which is machine-specific and noncanonical, so it does not travel
//! in the canonical state, and nothing infers it: only an explicit `--actor`
//! on `init` sets it.

use std::time::Duration;

use super::{
    Actor, Deserialize, LOCK_FILE, Serialize, Workspace, WorkspaceError, ensure_regular_file, fs,
    io, io_at, read_text, sync_directory, write_new_file,
};
use crate::durable::{CommitLock, LockError};

/// Path of the setting, relative to the workspace root.
pub const LOCAL_ACTOR_FILE: &str = "local/actor.toml";
const VERSION: &str = "inv.local-actor.v1";
const STAGE_FILE: &str = "local/.actor.toml.stage";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Setting {
    schema_version: String,
    actor: Actor,
}

impl Workspace {
    /// The actor recorded by `init --actor` on this machine, if any. A file
    /// that exists but does not parse is an error, never a silent absence.
    pub fn local_actor(&self) -> Result<Option<Actor>, WorkspaceError> {
        let path = self.root.join(LOCAL_ACTOR_FILE);
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_at(&path, error)),
        }
        ensure_regular_file(&path)?;
        let setting: Setting = toml::from_str(&read_text(&path)?).map_err(|error| {
            WorkspaceError::InvalidConfig(format!("{LOCAL_ACTOR_FILE}: {error}"))
        })?;
        if setting.schema_version != VERSION {
            return Err(WorkspaceError::InvalidConfig(format!(
                "{LOCAL_ACTOR_FILE}: schema_version must be {VERSION}"
            )));
        }
        Ok(Some(setting.actor))
    }

    /// Record `actor` as this machine's actor for the workspace and return
    /// the actor it replaces. The file is replaced atomically under the
    /// commit lock, so the fixed stage name is never shared.
    pub fn set_local_actor(&self, actor: &Actor) -> Result<Option<Actor>, WorkspaceError> {
        let lock_path = self.root.join(LOCK_FILE);
        let _lock =
            CommitLock::acquire_preferred(&lock_path, Duration::from_secs(2)).map_err(|error| {
                match error {
                    LockError::Io(error) => io_at(&lock_path, error),
                    LockError::Busy { .. } => io_at(
                        &lock_path,
                        io::Error::new(io::ErrorKind::WouldBlock, "workspace is busy"),
                    ),
                }
            })?;
        let previous = self.local_actor()?;
        if previous.as_ref() == Some(actor) {
            return Ok(previous);
        }
        let body = toml::to_string(&Setting {
            schema_version: VERSION.to_owned(),
            actor: actor.clone(),
        })
        .map_err(|error| WorkspaceError::Serialization(error.to_string()))?;
        let text = format!(
            "# Machine-local actor for writes in this workspace that name no other actor.\n\
             # Set it with {} init DIR --actor ACTOR. --actor, a request's actor, the\n\
             # investigation.toml actor and AGENT take precedence. It is not canonical state.\n\
             {body}",
            investigation_core::PROGRAM_NAME
        );
        let target = self.root.join(LOCAL_ACTOR_FILE);
        let stage = self.root.join(STAGE_FILE);
        match fs::remove_file(&stage) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_at(&stage, error)),
        }
        write_new_file(&stage, text.as_bytes())?;
        fs::rename(&stage, &target).map_err(|source| io_at(&target, source))?;
        sync_directory(&self.root.join("local"))?;
        Ok(previous)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, Workspace) {
        let base = tempfile::tempdir().unwrap();
        let workspace = Workspace::init(&base.path().join("ws"), false).unwrap();
        (base, workspace)
    }

    #[test]
    fn the_setting_is_absent_until_set_and_round_trips_through_reopen() {
        let (base, workspace) = workspace();
        assert_eq!(workspace.local_actor().unwrap(), None);
        let first: Actor = "agent:first".parse().unwrap();
        assert_eq!(workspace.set_local_actor(&first).unwrap(), None);
        let reopened = Workspace::open(workspace.root()).unwrap();
        assert_eq!(reopened.local_actor().unwrap(), Some(first.clone()));
        let second: Actor = "agent:second".parse().unwrap();
        assert_eq!(reopened.set_local_actor(&second).unwrap(), Some(first));
        assert_eq!(reopened.local_actor().unwrap(), Some(second));
        let text = fs::read_to_string(reopened.root().join(LOCAL_ACTOR_FILE)).unwrap();
        assert!(text.contains("actor = \"agent:second\""), "{text}");
        assert!(
            fs::read_dir(reopened.root().join("local"))
                .unwrap()
                .all(|entry| entry.unwrap().file_name() == "actor.toml"),
            "no stage file is left behind"
        );
        drop(base);
    }

    #[test]
    fn a_malformed_setting_is_an_error_not_an_absent_actor() {
        let (base, workspace) = workspace();
        let path = workspace.root().join(LOCAL_ACTOR_FILE);
        fs::write(&path, "actor = \"not valid!\"\n").unwrap();
        assert!(matches!(
            workspace.local_actor(),
            Err(WorkspaceError::InvalidConfig(_))
        ));
        fs::write(
            &path,
            "schema_version = \"inv.local-actor.v0\"\nactor = \"agent:x\"\n",
        )
        .unwrap();
        assert!(matches!(
            workspace.local_actor(),
            Err(WorkspaceError::InvalidConfig(_))
        ));
        drop(base);
    }
}
