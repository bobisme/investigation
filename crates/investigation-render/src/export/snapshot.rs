//! The one disk read of an export: a case pinned to one verified head.

use std::collections::BTreeMap;

use investigation_core::{
    evidence::{ArtifactBody, ObservationBasis, ObservationBody},
    identity::{Id, Kind},
    state::State,
    wire::{Digest, ErrorCode, Failure, Snapshot},
};
use investigation_store::{
    Store,
    durable::DurableFs,
    store::{
        cited_lines::{CitedLines, CitedSpan},
        evidence_read::MAX_EXCERPT_BYTES,
    },
};
use serde_json::json;

use crate::{CaseSnapshot, VerifiedSnapshot};

/// Which head to export: the current one, or an earlier verified one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pin {
    /// A verified case journal sequence; `None` is the current head.
    pub sequence: Option<u64>,
    /// The head hash required at `sequence`.
    pub head_hash: Option<Digest>,
}

/// A case at one verified head, with today's state beside it when the head
/// is an earlier one. Holds no file handle or path.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSnapshot {
    case_id: Id,
    pinned: VerifiedSnapshot,
    latest: Snapshot,
    today: Option<State>,
    long_citations: BTreeMap<Id, (u64, u64)>,
}

impl ExportSnapshot {
    /// Verify the case journal under the commit lock, then pin it. The lock
    /// is released before this returns, so planning never blocks a commit.
    pub fn read<F: DurableFs>(
        store: &mut Store<F>,
        case_id: Id,
        pin: &Pin,
    ) -> Result<Self, Failure> {
        let workspace_id = store.workspace().id();
        let head = store.case(case_id)?;
        let latest = head.snapshot.clone();
        let Some(sequence) = pin.sequence else {
            let long_citations = long_citations(store, &head.state, None);
            return Ok(Self {
                case_id,
                pinned: VerifiedSnapshot::single_case(workspace_id, case_id, head),
                latest,
                today: None,
                long_citations,
            });
        };
        let head_hash = match &pin.head_hash {
            Some(hash) => hash.clone(),
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
                    failure.error.expected = Some(json!({"latest": latest}));
                    failure
                })?,
        };
        let snapshot = Snapshot {
            sequence,
            head_hash,
        };
        if snapshot == latest {
            let long_citations = long_citations(store, &head.state, None);
            return Ok(Self {
                case_id,
                pinned: VerifiedSnapshot::single_case(workspace_id, case_id, head),
                latest,
                today: None,
                long_citations,
            });
        }
        let pinned = head.at_snapshot(&snapshot)?;
        let long_citations = long_citations(store, &pinned.state, Some(&head.state));
        Ok(Self {
            case_id,
            pinned: VerifiedSnapshot::single_case(workspace_id, case_id, pinned),
            latest,
            today: Some(head.state),
            long_citations,
        })
    }

    pub const fn case_id(&self) -> Id {
        self.case_id
    }

    /// A snapshot holding only the pinned case.
    pub const fn pinned(&self) -> &VerifiedSnapshot {
        &self.pinned
    }

    /// The pinned case.
    pub fn case(&self) -> &CaseSnapshot {
        self.pinned
            .case(self.case_id)
            .expect("an export snapshot holds its case")
    }

    /// The case's current head when the export was read.
    pub const fn latest(&self) -> &Snapshot {
        &self.latest
    }

    /// Today's state when the pinned head is an earlier one.
    pub const fn today(&self) -> Option<&State> {
        self.today.as_ref()
    }

    /// Whether the export pins an earlier head than the current one.
    pub const fn historical(&self) -> bool {
        self.today.is_some()
    }

    /// The one-based, inclusive lines each Observation cites in an Artifact
    /// too large for one `evidence read`, keyed by Observation ID: where
    /// `brief` locates its citation. Citations of smaller Artifacts, and
    /// those that cannot be located, are absent.
    pub const fn long_citations(&self) -> &BTreeMap<Id, (u64, u64)> {
        &self.long_citations
    }
}

/// Locate the citations of every Observation in `state` whose cited Artifact
/// holds more bytes than one `evidence read` prints, the only ones a reader
/// must read in line ranges. Disclosure follows `today` when the state is
/// pinned before it, as in `brief`; restricted bytes are never opened.
fn long_citations<F: DurableFs>(
    store: &Store<F>,
    state: &State,
    today: Option<&State>,
) -> BTreeMap<Id, (u64, u64)> {
    let long = |reference| {
        state
            .revision(reference)
            .and_then(|record| serde_json::from_value::<ArtifactBody>(record.body.clone()).ok())
            .and_then(|body| body.size_bytes)
            .is_some_and(|size| size > MAX_EXCERPT_BYTES as u64)
    };
    let bodies: Vec<(Id, ObservationBody)> = state
        .records()
        .filter(|record| record.header.id.kind() == Kind::Observation)
        .filter_map(|record| {
            ObservationBody::from_stored(&record.body)
                .ok()
                .map(|body| (record.header.id, body))
        })
        .collect();
    let spans: Vec<(Id, CitedSpan<'_>)> = bodies
        .iter()
        .filter_map(|(id, body)| match &body.basis {
            ObservationBasis::Source {
                artifact_ref,
                selector,
                text_quote,
            } if long(*artifact_ref) => Some((
                *id,
                CitedSpan {
                    artifact: *artifact_ref,
                    selector,
                    quote: text_quote,
                },
            )),
            _ => None,
        })
        .collect();
    if spans.is_empty() {
        return BTreeMap::new();
    }
    store
        .cited_lines(state, today.unwrap_or(state), &spans)
        .into_iter()
        .filter_map(|(id, lines)| match lines {
            CitedLines::Located { start, end } => Some((id, (start, end))),
            _ => None,
        })
        .collect()
}
