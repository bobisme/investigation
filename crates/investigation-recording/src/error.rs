//! Errors returned by recorder calls.

use std::fmt;

use investigation_core::wire::{CommitStatus, ErrorCode, Failure};

use crate::key::Operation;

/// Why a recorder call returned no receipt.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RecordingError {
    /// The recorder refused or could not complete the call.
    ///
    /// This is the same structured failure the JSON-CLI prints, including its
    /// commit status and repair guidance. When the commit status is `unknown`,
    /// retry the identical request with the same key or call `recover`; never
    /// assume the call had no effect.
    Rejected(Failure),
    /// This build does not implement the operation. Nothing was recorded.
    Unsupported {
        /// The operation that was called.
        operation: Operation,
    },
}

impl RecordingError {
    /// The structured error code, for a rejection.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::Rejected(failure) => Some(failure.error.code),
            Self::Unsupported { .. } => None,
        }
    }

    /// Whether anything may have been recorded.
    pub fn commit_status(&self) -> CommitStatus {
        match self {
            Self::Rejected(failure) => failure.commit_status,
            Self::Unsupported { .. } => CommitStatus::NotCommitted,
        }
    }

    /// The structured failure, for a rejection.
    pub fn failure(&self) -> Option<&Failure> {
        match self {
            Self::Rejected(failure) => Some(failure),
            Self::Unsupported { .. } => None,
        }
    }
}

impl From<Failure> for RecordingError {
    fn from(failure: Failure) -> Self {
        Self::Rejected(failure)
    }
}

impl fmt::Display for RecordingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(failure) => failure.fmt(formatter),
            Self::Unsupported { operation } => write!(
                formatter,
                "{operation} is not implemented in this build; nothing was recorded"
            ),
        }
    }
}

impl std::error::Error for RecordingError {}
