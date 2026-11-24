//! The fork marker: what `archive fork` records in a new workspace's
//! immutable `state/workspace.json` (plan §10.7, W11.18, ADR 0013).
//!
//! A fork keeps every case journal byte for byte, so its record, transaction
//! and command IDs, receipts and case heads at the fork are the origin's. It
//! gets a new workspace ID, and every event it commits names that ID. The
//! marker names the origin workspace, the archive it was forked from, and,
//! for each inherited case, the head the fork started at and which
//! workspaces wrote the journal up to there. The journal verifier accepts
//! the origin's identity only for those bytes (see
//! [`crate::journal::JournalWriters`]), so a fork can never pass for its
//! origin, and two forks of one archive never pass for one writer.
//!
//! The marker is additive and optional: a workspace without it is an
//! ordinary workspace, and its absence changes nothing for one.

use std::{fs, path::Path};

use investigation_core::{
    identity::{Id, IdSource, Kind, SystemIdSource},
    wire::Digest,
};
use serde::{Deserialize, Serialize};

use super::{GENESIS_FILE, Workspace, WorkspaceError, WorkspaceGenesis, io_at, sync_directory};
use crate::journal::{InheritedCase, JournalWriters};

/// Most inherited cases one marker lists; the marker is read on every open.
pub const MAX_FORK_CASES: usize = 100_000;
/// Longest `origin_export_id` a marker holds.
const MAX_EXPORT_ID_CHARS: usize = 128;
/// Where the replacement marker is staged before it replaces the copy's.
const STAGED_MARKER: &str = "state/tmp/fork-marker";

/// Where a fork came from. Written once, when the fork is made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkMarker {
    /// The workspace the archive was made from.
    pub origin_workspace_id: Id,
    /// The archive's recovery head: a digest over every case head it held.
    pub origin_recovery_head: Digest,
    /// The `export_id` of the archive the fork was made from.
    pub origin_export_id: String,
    /// Every case the fork inherited, in case ID order.
    pub cases: Vec<InheritedCase>,
}

impl ForkMarker {
    /// Whether the marker is well formed for a workspace whose ID is `own`.
    pub(super) fn validate(&self, own: Id) -> Result<(), WorkspaceError> {
        let invalid = |reason: &str| Err(WorkspaceError::InvalidGenesis(format!("fork: {reason}")));
        if self.origin_workspace_id.kind() != Kind::Workspace || self.origin_workspace_id == own {
            return invalid("the origin must be another workspace");
        }
        if self.origin_export_id.is_empty()
            || self.origin_export_id.chars().count() > MAX_EXPORT_ID_CHARS
            || self.origin_export_id.chars().any(char::is_control)
        {
            return invalid("the origin export ID is empty, too long or has a control character");
        }
        if self.cases.len() > MAX_FORK_CASES {
            return invalid("too many inherited cases");
        }
        if self
            .cases
            .windows(2)
            .any(|pair| pair[0].case_id >= pair[1].case_id)
        {
            return invalid("inherited cases must be unique and in case ID order");
        }
        if let Some(case) = self.cases.iter().find(|case| !case.is_well_formed(own)) {
            return Err(WorkspaceError::InvalidGenesis(format!(
                "fork: the inherited head of case {} is malformed",
                case.case_id
            )));
        }
        Ok(())
    }

    /// The inherited head of `case_id`, if the fork inherited it.
    pub fn case(&self, case_id: Id) -> Option<&InheritedCase> {
        self.cases
            .binary_search_by_key(&case_id, |case| case.case_id)
            .ok()
            .map(|index| &self.cases[index])
    }
}

impl Workspace {
    /// The fork marker, when `archive fork` created this workspace.
    pub fn fork(&self) -> Option<&ForkMarker> {
        self.fork.as_ref()
    }

    /// Which workspaces wrote the journal of `case_id`: this one, after any
    /// prefix the fork inherited.
    pub fn writers(&self, case_id: Id) -> JournalWriters<'_> {
        JournalWriters {
            workspace_id: self.id,
            inherited: self.fork.as_ref().and_then(|fork| fork.case(case_id)),
        }
    }

    /// Give the unpublished workspace copy at `root` a new identity: a new
    /// workspace ID and `fork` in its marker, keeping the rest of the marker
    /// (a demo marker stays). Only for a copy `archive fork` staged and has
    /// not published; the marker of any other workspace is immutable.
    pub fn give_fork_identity(root: &Path, fork: ForkMarker) -> Result<Self, WorkspaceError> {
        let staged = Self::open(root)?;
        if staged.id != fork.origin_workspace_id {
            return Err(WorkspaceError::InvalidGenesis(
                "fork: the copy is not of the origin workspace".to_owned(),
            ));
        }
        let id = SystemIdSource
            .next_id(Kind::Workspace)
            .map_err(|error| WorkspaceError::Identity(error.to_string()))?;
        fork.validate(id)?;
        let marker = WorkspaceGenesis {
            schema_version: super::WORKSPACE_VERSION,
            schema_family: super::RECORD_FAMILY,
            workspace_id: id,
            genesis_sequence: 0,
            demo: staged.demo,
            fork: Some(fork),
            format: (staged.format != super::WorkspaceFormat::BASELINE).then_some(staged.format),
        };
        let mut bytes = serde_json::to_vec_pretty(&marker)
            .map_err(|error| WorkspaceError::Serialization(error.to_string()))?;
        bytes.push(b'\n');
        let stage = staged.root.join(STAGED_MARKER);
        let target = staged.root.join(GENESIS_FILE);
        super::write_new_file(&stage, &bytes)?;
        fs::rename(&stage, &target).map_err(|source| io_at(&target, source))?;
        sync_directory(&staged.state_dir)?;
        Self::open(&staged.root)
    }
}

#[cfg(test)]
mod tests {
    use investigation_core::identity::{Id, IdSource, Kind, SystemIdSource};

    use crate::journal::{InheritedCase, JournalWriter, JournalWriters};

    fn id(kind: Kind) -> Id {
        SystemIdSource.next_id(kind).unwrap()
    }

    #[test]
    fn each_sequence_names_the_workspace_that_wrote_it() {
        let (origin, middle, own) = (
            id(Kind::Workspace),
            id(Kind::Workspace),
            id(Kind::Workspace),
        );
        let case = InheritedCase {
            case_id: id(Kind::Case),
            sequence: 9,
            head_hash: crate::journal::hash_bytes(b"head"),
            writers: vec![
                JournalWriter {
                    workspace_id: origin,
                    through_sequence: 4,
                },
                JournalWriter {
                    workspace_id: middle,
                    through_sequence: 9,
                },
            ],
        };
        assert!(case.is_well_formed(own));
        let writers = JournalWriters {
            workspace_id: own,
            inherited: Some(&case),
        };
        let expected = [
            (0, origin),
            (1, origin),
            (4, origin),
            (5, middle),
            (9, middle),
            (10, own),
        ];
        for (sequence, workspace) in expected {
            assert_eq!(writers.at(sequence), workspace, "{sequence}");
        }
        assert_eq!(JournalWriters::from(own).at(3), own);

        // Malformed: the fork itself as a writer, runs out of order, a
        // repeated writer, or a last run that does not end at the head.
        assert!(!case.is_well_formed(middle));
        let mut broken = case.clone();
        broken.writers[1].through_sequence = 4;
        assert!(!broken.is_well_formed(own));
        let mut broken = case.clone();
        broken.writers[1].workspace_id = origin;
        assert!(!broken.is_well_formed(own));
        let mut broken = case.clone();
        broken.sequence = 10;
        assert!(!broken.is_well_formed(own));
        let mut broken = case;
        broken.writers.clear();
        assert!(!broken.is_well_formed(own));
    }
}
