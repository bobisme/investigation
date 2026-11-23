//! Acquisition identity for ingest.
//!
//! A producer namespace plus a stable external ID names one logical acquisition
//! attempt; each retained input inside it is named by its manifest path or stream
//! name. Importing the same attempt again maps onto the recorded Source and
//! reports the prior link. A changed description of a known attempt revises that
//! Source, so the history is explicit lineage rather than a new trial. Blob bytes
//! deduplicate by digest independently: distinct attempts with identical bytes
//! keep distinct Sources. Keys are producer claims, never verified identity.

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::{
    command::RevisionRef,
    evidence::{AcquisitionKey, ArtifactBody, ArtifactRole, SourceBody},
    header::ClassificationLevel,
    identity::{Id, Kind},
    state::{Record, State},
    wire::{Digest, ErrorCode, Failure},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{TEXT_PARSER_VERSION, invalid};
use crate::{
    journal::CaseReplay,
    store::{Store, integrity, show_argv},
};

pub const MAX_NAMESPACE_CHARS: usize = 128;
pub const MAX_EXTERNAL_ID_CHARS: usize = 256;

/// Producer namespace plus stable external ID of one logical acquisition attempt.
/// Reimporting the same attempt maps onto the recorded Source; a changed
/// description of it records a revision, not a new acquisition. Each path or
/// stdin name identifies one item within the attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionKeyRequest {
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
    )]
    pub namespace: String,
    #[schemars(length(min = 1, max = 256))]
    pub external_id: String,
}

impl AcquisitionKeyRequest {
    pub fn key(&self, item: &str) -> AcquisitionKey {
        AcquisitionKey {
            namespace: self.namespace.clone(),
            external_id: self.external_id.clone(),
            item: item.to_owned(),
        }
    }

    pub(super) fn validate(&self) -> Result<(), Failure> {
        let namespace_ok = !self.namespace.is_empty()
            && self.namespace.chars().count() <= MAX_NAMESPACE_CHARS
            && self
                .namespace
                .starts_with(|c: char| c.is_ascii_alphanumeric())
            && self
                .namespace
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !namespace_ok {
            return Err(invalid(
                "Use a namespace of 1–128 ASCII letters, digits, '.', '_' or '-', starting with a letter or digit.",
                "/source/acquisition_key/namespace",
            ));
        }
        if self.external_id.trim().is_empty()
            || self.external_id.chars().count() > MAX_EXTERNAL_ID_CHARS
            || self.external_id.chars().any(char::is_control)
        {
            return Err(invalid(
                "Use an external ID of 1–256 characters without control characters.",
                "/source/acquisition_key/external_id",
            ));
        }
        Ok(())
    }
}

/// What an ingest did with one item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemOutcome {
    /// A new acquisition: new Source and original Artifact records.
    #[default]
    Imported,
    /// The attempt and item were already recorded with the same description and
    /// bytes; the item reports the existing records and creates none.
    Deduplicated,
    /// A known attempt and item with a changed description or bytes: the Source
    /// is revised or gains another retained version, never a second acquisition.
    Revised,
    /// The output of a transformation of recorded Artifacts: a new derivative
    /// Artifact linked to its inputs. No Source or acquisition is created.
    Derived,
}

/// The canonical records an earlier import of the same acquisition produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PriorAcquisition {
    pub case_id: Id,
    /// Source revision that was current before this ingest.
    pub source: RevisionRef,
    /// Latest original artifact of that Source before this ingest.
    pub artifact_id: Option<Id>,
    /// Local transaction order of the latest prior record; not real-world time.
    pub acceptance_sequence: u64,
}

/// Facts about one staged input that resolution needs.
pub(super) struct ItemFacts<'a> {
    pub label: &'a str,
    pub digest: &'a Digest,
    pub decoded: bool,
}

#[derive(Clone, Default)]
pub(super) struct ItemPlan {
    pub key: Option<AcquisitionKey>,
    pub known: Option<Known>,
    pub outcome: ItemOutcome,
}

#[derive(Clone)]
pub(super) struct Known {
    pub case_id: Id,
    /// Current Source revision before this ingest and its decoded body.
    pub source: Record,
    pub body: SourceBody,
    pub revise: bool,
    /// Recorded original artifact with the same bytes, and its text derivative.
    pub artifact: Option<Id>,
    pub derivative: Option<Id>,
    pub prior_artifact: Option<Id>,
    pub prior_sequence: u64,
}

impl ItemPlan {
    pub fn prior(&self) -> Option<PriorAcquisition> {
        self.known.as_ref().map(|known| PriorAcquisition {
            case_id: known.case_id,
            source: known.source.reference(),
            artifact_id: known.prior_artifact,
            acceptance_sequence: known.prior_sequence,
        })
    }
}

/// Plans for items of a manifest without an acquisition key: every ingest is
/// its own acquisition.
pub(super) fn unkeyed(count: usize) -> Vec<ItemPlan> {
    vec![ItemPlan::default(); count]
}

pub(super) fn fresh(request: &AcquisitionKeyRequest, items: &[ItemFacts<'_>]) -> Vec<ItemPlan> {
    items
        .iter()
        .map(|item| ItemPlan {
            key: Some(request.key(item.label)),
            ..ItemPlan::default()
        })
        .collect()
}

fn source_body(record: &Record) -> Option<SourceBody> {
    investigation_core::input::decode(&record.body).ok()
}

/// A Source through which a case currently records any item of this attempt.
pub(super) fn records_attempt(state: &State, request: &AcquisitionKeyRequest) -> Option<Id> {
    state
        .records()
        .filter(|record| record.header.id.kind() == Kind::Source)
        .find(|record| {
            source_body(record)
                .and_then(|body| body.acquisition_key)
                .is_some_and(|key| key.same_attempt(&request.namespace, &request.external_id))
        })
        .map(|record| record.header.id)
}

fn rank(level: ClassificationLevel) -> u8 {
    match level {
        ClassificationLevel::Public => 0,
        ClassificationLevel::Internal => 1,
        ClassificationLevel::Restricted => 2,
    }
}

/// Artifacts that retain an ingest manifest rather than an acquired item.
/// New records carry the explicit `manifest` role; for records written before
/// roles were recorded, the journal names them with the `manifest` client
/// reference.
pub(super) fn manifest_artifacts(case: &CaseReplay, as_of: u64) -> BTreeSet<Id> {
    case.events
        .iter()
        .map(|verified| &verified.event)
        .filter(|event| event.manifest_digest.is_some() && event.sequence <= as_of)
        .flat_map(|event| &event.accepted.created)
        .filter(|change| {
            change.id.kind() == Kind::Artifact && change.client_ref.as_deref() == Some("manifest")
        })
        .map(|change| change.id)
        .collect()
}

/// Resolve items against one case as it stood at acceptance sequence `as_of`.
pub(super) fn resolve(
    case_id: Id,
    case: &CaseReplay,
    as_of: u64,
    request: &AcquisitionKeyRequest,
    description: &Digest,
    level: ClassificationLevel,
    items: &[ItemFacts<'_>],
) -> Result<Vec<ItemPlan>, Failure> {
    let state = &case.state;
    let manifests = manifest_artifacts(case, as_of);
    let visible: Vec<&Record> = state
        .records()
        .filter_map(|record| state.record_at(record.header.id, as_of))
        .collect();
    let mut sources = BTreeMap::new();
    for record in visible
        .iter()
        .filter(|record| record.header.id.kind() == Kind::Source)
    {
        let Some(body) = source_body(record) else {
            continue;
        };
        let Some(key) = body
            .acquisition_key
            .clone()
            .filter(|key| key.same_attempt(&request.namespace, &request.external_id))
        else {
            continue;
        };
        if sources.insert(key.item.clone(), (*record, body)).is_some() {
            return Err(integrity(
                "Two Sources in one case claim the same acquisition item.",
            ));
        }
    }
    let artifacts: Vec<(&Record, ArtifactBody)> = visible
        .iter()
        .filter(|record| {
            record.header.id.kind() == Kind::Artifact && !manifests.contains(&record.header.id)
        })
        .filter_map(|record| {
            Some((
                *record,
                investigation_core::input::decode(&record.body).ok()?,
            ))
        })
        .collect();

    items
        .iter()
        .map(|item| {
            let key = request.key(item.label);
            let Some((source, body)) = sources.get(item.label) else {
                return Ok(ItemPlan {
                    key: Some(key),
                    ..ItemPlan::default()
                });
            };
            let recorded = source.header.classification.level;
            if rank(level) < rank(recorded) {
                return Err(invalid(
                    "A reimport cannot lower the classification of a recorded acquisition. Keep the recorded level or record a reviewed transformation.",
                    "/classification",
                ));
            }
            let originals: Vec<&(&Record, ArtifactBody)> = artifacts
                .iter()
                .filter(|(_, artifact)| {
                    artifact.source.id == source.header.id
                        && artifact.role() == ArtifactRole::Original
                })
                .collect();
            let latest = |records: &mut dyn Iterator<Item = &Record>| {
                records
                    .max_by_key(|record| (record.header.acceptance_sequence, record.header.id))
                    .map(|record| record.header.id)
            };
            let prior_artifact = latest(&mut originals.iter().map(|(record, _)| *record));
            let artifact = latest(
                &mut originals
                    .iter()
                    .filter(|(_, artifact)| artifact.digest.as_ref() == Some(item.digest))
                    .map(|(record, _)| *record),
            );
            let derivative = artifact.and_then(|artifact_id| {
                latest(
                    &mut artifacts
                        .iter()
                        .filter(|(_, body)| {
                            body.role() == ArtifactRole::Derivative
                                && body.transforms.first().is_some_and(|transform| {
                                transform.input.id == artifact_id
                                    && transform.transformation == "decode_utf8"
                                    && transform.version == TEXT_PARSER_VERSION
                            })
                        })
                        .map(|(record, _)| *record),
                )
            });
            let prior_sequence = [Some(source.header.id), artifact, derivative]
                .into_iter()
                .flatten()
                .filter_map(|id| state.record_at(id, as_of))
                .map(|record| record.header.acceptance_sequence)
                .max()
                .unwrap_or(source.header.acceptance_sequence);
            let revise = body.description_digest.as_ref() != Some(description);
            let unchanged =
                !revise && artifact.is_some() && (!item.decoded || derivative.is_some());
            Ok(ItemPlan {
                key: Some(key),
                outcome: if unchanged {
                    ItemOutcome::Deduplicated
                } else {
                    ItemOutcome::Revised
                },
                known: Some(Known {
                    case_id,
                    source: (*source).clone(),
                    body: body.clone(),
                    revise,
                    artifact,
                    derivative,
                    prior_artifact,
                    prior_sequence,
                }),
            })
        })
        .collect()
}

/// The attempt is recorded in another case than the one this manifest targets.
pub(super) fn other_case(home: Id, source: Id) -> Failure {
    let mut failure = Failure::new(
        ErrorCode::InvalidInput,
        format!(
            "This acquisition is already recorded in case {home}. Ingest it into that case to record a revision; another case cannot claim the same acquisition."
        ),
    )
    .at("/source/acquisition_key")
    .with_schema("ingest")
    .with_argv(show_argv(source, home));
    failure.error.actual = Some(serde_json::json!({"case_id": home, "source_id": source}));
    failure
}

/// Canonical JSON: object keys sorted at every depth, whatever map order the
/// serializer was built with.
pub(super) fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .into_iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

/// One recorded acquisition item, as projected into the disposable index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcquisitionCursor {
    pub key: AcquisitionKey,
    pub case_id: Id,
    pub source: RevisionRef,
    pub description_digest: Option<Digest>,
    pub acceptance_sequence: u64,
}

impl Store {
    /// Recorded acquisition items in one producer namespace, so a resumable
    /// importer can skip attempts it already delivered. Reads the index after
    /// bringing it up to the verified journal; ingest itself never trusts it.
    pub fn acquisition_cursors(
        &mut self,
        namespace: &str,
    ) -> Result<Vec<AcquisitionCursor>, Failure> {
        let _lock = self.coordinate()?;
        let workspace = self.load_verified()?;
        self.refresh_index(&workspace)?;
        crate::projection::Projection::open(&self.index_path(), self.workspace.id())?
            .acquisitions(namespace)
    }
}
