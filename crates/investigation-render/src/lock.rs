//! The renderer lock: an advisory lock on `local/render.lock`.
//!
//! It is a different file from the store's commit lock in `state/locks/`, so
//! a render in progress never blocks a commit, and a commit never blocks a
//! render beyond the brief verified read. It serializes renders only; it does
//! not coordinate editors or sync clients writing into the vault.

use std::{
    fs::File,
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use investigation_store::Workspace;
use rustix::{
    fs::{CWD, Mode, OFlags, openat},
    io::Errno,
};

use crate::RenderError;

/// Workspace-relative path of the renderer lock file.
pub const RENDER_LOCK: &str = "local/render.lock";

/// Held for a whole render. Dropping it releases the lock; the file persists.
#[derive(Debug)]
pub struct RenderLock {
    _file: File,
    root: PathBuf,
    pub waited: Duration,
}

impl RenderLock {
    /// Take the lock, waiting up to `timeout` for another render to finish.
    /// The lock file is created if absent, never through a symbolic link.
    pub fn acquire(workspace: &Workspace, timeout: Duration) -> Result<Self, RenderError> {
        let root = workspace.root();
        let local = open_local(root)?;
        let fd = openat(
            &local,
            "render.lock",
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| RenderError::io(RENDER_LOCK, error))?;
        let file = File::from(fd);
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => {
                    return Ok(Self {
                        _file: file,
                        root: root.to_owned(),
                        waited: start.elapsed(),
                    });
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    let elapsed = start.elapsed();
                    if elapsed >= timeout {
                        return Err(RenderError::Busy { waited: elapsed });
                    }
                    std::thread::sleep(Duration::from_millis(5).min(timeout - elapsed));
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(RenderError::io(RENDER_LOCK, error));
                }
            }
        }
    }

    /// The workspace root this lock serializes.
    pub fn workspace_root(&self) -> &Path {
        &self.root
    }
}

/// What [`probe`] found.
pub(crate) enum Probe {
    /// No render is running. The guard, when present, holds the lock until
    /// dropped, so no render can start while the caller reads.
    Idle { _guard: Option<File> },
    /// Another render holds the lock.
    Running,
}

/// Whether a render is running, without waiting and without creating the
/// lock file. A free lock is taken and returned, so the caller's reads are
/// not interleaved with a render that starts meanwhile.
pub(crate) fn probe(workspace: &Workspace) -> Result<Probe, RenderError> {
    let local = open_local(workspace.root())?;
    let fd = match openat(
        &local,
        "render.lock",
        OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        // No render has ever run here.
        Err(Errno::NOENT) => return Ok(Probe::Idle { _guard: None }),
        Err(error) => return Err(RenderError::io(RENDER_LOCK, error)),
    };
    let file = File::from(fd);
    match file.try_lock() {
        Ok(()) => Ok(Probe::Idle { _guard: Some(file) }),
        Err(std::fs::TryLockError::WouldBlock) => Ok(Probe::Running),
        Err(std::fs::TryLockError::Error(error)) => Err(RenderError::io(RENDER_LOCK, error)),
    }
}

/// Open the workspace's `local/` directory, refusing a symbolic link.
pub(crate) fn open_local(root: &Path) -> Result<OwnedFd, RenderError> {
    let directory = OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC;
    let root_fd =
        openat(CWD, root, directory, Mode::empty()).map_err(|error| RenderError::io(".", error))?;
    openat(
        &root_fd,
        "local",
        directory | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| match error {
        Errno::LOOP | Errno::NOTDIR => RenderError::UnsafeDirectory("workspace local/"),
        error => RenderError::io("local", error),
    })
}
