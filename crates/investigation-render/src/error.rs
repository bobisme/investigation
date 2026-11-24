//! Render failures. A render failure never changes canonical state.

use std::{fmt, io, time::Duration};

use investigation_core::wire::{ErrorCode, Failure};

#[derive(Debug)]
pub enum RenderError {
    /// A planner produced a path outside the allowed shape.
    InvalidPath { path: String, reason: &'static str },
    /// A planner produced the same path twice, or two paths that differ only by case.
    DuplicatePath(String),
    /// A planned note could not carry its generation marker.
    Marker { path: String, reason: &'static str },
    /// The canonical snapshot could not be read or verified.
    Snapshot(Failure),
    /// The workspace has no vault; its vault policy is disabled.
    VaultDisabled,
    /// The vault root or the workspace's `local/` directory is not a real directory.
    UnsafeDirectory(&'static str),
    /// Another render holds the renderer lock.
    Busy { waited: Duration },
    /// A lock from one workspace was offered to a publisher for another.
    WrongWorkspace,
    /// `local/render-manifest.json` declares a format this build does not
    /// know. It is left untouched rather than replaced with an older format.
    UnsupportedManifest { version: String },
    /// A filesystem operation failed. `path` is relative to the vault or workspace.
    Io { path: String, error: io::Error },
    /// `vault reconcile` refused a listed path. `changed` when the path no
    /// longer holds what the last render diagnosed. Nothing was changed for
    /// the path; `reason` says what earlier paths of the same request did.
    Reconcile {
        path: String,
        reason: String,
        changed: bool,
    },
    /// Existing-vault mode refused to touch the person's vault: the vault or
    /// the managed prefix is unsafe, the prefix holds files the renderer
    /// does not own, or the manifest records a projection somewhere else.
    /// Nothing in the vault changed.
    Projection { reason: String },
    /// A review export planned a file naming a record its audience policy
    /// withheld. Nothing is written; the planner must leave the record out.
    Withheld { path: String },
}

impl RenderError {
    pub(crate) fn io(path: impl Into<String>, error: impl Into<io::Error>) -> Self {
        Self::Io {
            path: path.into(),
            error: error.into(),
        }
    }

    /// Machine-readable failure for frontends. Canonical state is unaffected.
    pub fn to_failure(&self) -> Failure {
        match self {
            Self::Snapshot(failure) => failure.clone(),
            Self::Busy { .. } => Failure::new(ErrorCode::WorkspaceBusy, self.to_string()),
            Self::InvalidPath { .. }
            | Self::DuplicatePath(_)
            | Self::Marker { .. }
            | Self::UnsupportedManifest { .. } => {
                Failure::new(ErrorCode::IntegrityFailure, self.to_string())
            }
            Self::VaultDisabled => Failure::new(ErrorCode::InvalidInput, self.to_string()),
            Self::UnsafeDirectory(_) | Self::WrongWorkspace | Self::Io { .. } => {
                Failure::new(ErrorCode::IoFailure, self.to_string())
            }
            Self::Reconcile { changed: true, .. } => {
                Failure::new(ErrorCode::RevisionConflict, self.to_string()).at("/paths")
            }
            Self::Reconcile { .. } => {
                Failure::new(ErrorCode::InvalidInput, self.to_string()).at("/paths")
            }
            Self::Projection { .. } => Failure::new(ErrorCode::InvalidInput, self.to_string()),
            Self::Withheld { .. } => Failure::new(ErrorCode::GateFailed, self.to_string()),
        }
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path, reason } => {
                write!(
                    formatter,
                    "Renderer planned an invalid path {path:?}: {reason}."
                )
            }
            Self::DuplicatePath(path) => {
                write!(formatter, "Renderer planned the path {path:?} twice.")
            }
            Self::Marker { path, reason } => write!(
                formatter,
                "Renderer planned the note {path:?} without a valid generation marker: {reason}."
            ),
            Self::Snapshot(failure) => write!(formatter, "{failure}"),
            Self::VaultDisabled => formatter.write_str(
                "The workspace vault policy is disabled, so there is no vault to render.",
            ),
            Self::UnsafeDirectory(what) => write!(
                formatter,
                "The {what} is not a real directory; the renderer does not follow symbolic links."
            ),
            Self::Busy { waited } => write!(
                formatter,
                "Timed out after {} ms waiting for another render to finish. Commits are not affected; retry the render.",
                waited.as_millis()
            ),
            Self::WrongWorkspace => {
                formatter.write_str("The render lock belongs to a different workspace.")
            }
            Self::UnsupportedManifest { version } => write!(
                formatter,
                "local/render-manifest.json has the unsupported format {version:?}; it was left untouched. Render with a build that supports it."
            ),
            Self::Io { path, error } => write!(formatter, "{path}: {error}"),
            Self::Reconcile { path, reason, .. } if path.is_empty() => {
                write!(formatter, "vault reconcile: {reason}.")
            }
            Self::Reconcile { path, reason, .. } => write!(formatter, "{path}: {reason}."),
            Self::Projection { reason } => write!(
                formatter,
                "Existing-vault projection refused: {reason}. Nothing in the vault changed."
            ),
            Self::Withheld { path } => write!(
                formatter,
                "The review export was refused: {path} would name a record the audience policy \
                 withheld. Nothing was written."
            ),
        }
    }
}

impl std::error::Error for RenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            _ => None,
        }
    }
}
