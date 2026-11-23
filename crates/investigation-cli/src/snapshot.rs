//! Historical reads pinned to one verified snapshot of a case journal.
//!
//! `--snapshot SEQUENCE [--head-hash DIGEST]` resolves the graph as it was
//! known at that head and labels the read historical. Disclosure and byte
//! availability still follow today's audience and retention policy: the
//! current head stays available as the policy state.

use investigation_core::{
    state::State,
    wire::{Digest, ErrorCode, Failure, Snapshot},
};
use investigation_store::journal::CaseReplay;
use serde_json::{Value, json};

/// The policy statement every historical read carries.
pub(crate) const HISTORICAL_POLICY: &str = "Historical read: the records are as known at this snapshot. Disclosure and byte availability follow today's audience, withdrawal and retention state.";

#[derive(clap::Args, Clone, Debug, Default)]
pub(crate) struct SnapshotArgs {
    /// Read the case as of this verified snapshot sequence instead of the head.
    #[arg(long, value_name = "SEQUENCE")]
    pub(crate) snapshot: Option<u64>,
    /// Require this head hash at --snapshot.
    #[arg(long, value_name = "DIGEST", requires = "snapshot")]
    pub(crate) head_hash: Option<String>,
}

/// The current head and, for a historical read, the replay pinned before it.
pub(crate) struct Pinned {
    head: CaseReplay,
    pinned: Option<CaseReplay>,
}

impl Pinned {
    /// The replay every part of this read uses.
    pub(crate) fn read(&self) -> &CaseReplay {
        self.pinned.as_ref().unwrap_or(&self.head)
    }

    /// Today's state, which governs disclosure and availability, when the read
    /// is historical; `None` when the read is the head itself.
    pub(crate) fn today(&self) -> Option<&State> {
        self.pinned.as_ref().map(|_| &self.head.state)
    }

    /// The label a historical read carries; `None` for a read of the head.
    pub(crate) fn label(&self) -> Option<Value> {
        let pinned = self.pinned.as_ref()?;
        Some(json!({
            "snapshot": pinned.snapshot,
            "latest_snapshot": self.head.snapshot,
            "policy": HISTORICAL_POLICY,
        }))
    }
}

/// `--snapshot` and `--head-hash` arguments that pin a follow-up read.
pub(crate) fn pin_argv(snapshot: &Snapshot) -> Vec<String> {
    vec![
        "--snapshot".into(),
        snapshot.sequence.to_string(),
        "--head-hash".into(),
        snapshot.head_hash.to_string(),
    ]
}

/// Resolve the requested snapshot against `head`. A failure carries `retry`
/// as its repair argv.
pub(crate) fn resolve(
    head: CaseReplay,
    args: &SnapshotArgs,
    retry: Vec<String>,
) -> Result<Pinned, Failure> {
    let Some(sequence) = args.snapshot else {
        return Ok(Pinned { head, pinned: None });
    };
    let head_hash = match &args.head_hash {
        Some(text) => text
            .parse::<Digest>()
            .map_err(|error| Failure::new(ErrorCode::InvalidInput, error).at("--head-hash"))?,
        None => head
            .state
            .verified_snapshot(sequence)
            .cloned()
            .ok_or_else(|| {
                let mut failure = Failure::new(
                    ErrorCode::UnknownReference,
                    "The sequence is not a verified snapshot of this case journal.",
                )
                .at("--snapshot");
                failure.error.expected = Some(json!({"latest": head.snapshot}));
                failure.with_argv(retry.clone())
            })?,
    };
    let snapshot = Snapshot {
        sequence,
        head_hash,
    };
    if snapshot == head.snapshot {
        return Ok(Pinned { head, pinned: None });
    }
    let pinned = head
        .at_snapshot(&snapshot)
        .map_err(|failure| failure.with_argv(retry))?;
    Ok(Pinned {
        head,
        pinned: Some(pinned),
    })
}
