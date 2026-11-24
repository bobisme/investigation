//! Derivative artifacts and transformation lineage.
//!
//! A derivative (a redaction, a text extraction, a plot, an aggregation) is an
//! Artifact produced by a transformation of other Artifacts. It never records a
//! new acquisition: its `source` names the acquisition of its primary input, and
//! its `transforms` name every direct input with the transformation identity,
//! version and the input digest the transformation read. A chain of steps is a
//! derivative of a derivative, so lineage is followed transitively until it
//! reaches the originals: the bytes an acquisition retained.
//!
//! Lineage uses the same reference fields as classification propagation
//! (`classification::derivation_inputs`) and the freshness dependency index
//! (`freshness::dependency_edges`): `transforms[].input` and `source`. Nothing
//! here adds a reference field, so derivatives inherit classification and
//! freshness from their inputs without a second edge table.
//!
//! Several derivatives of one acquisition share that origin. They are views of
//! the same captured bytes, not independent confirmations.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    classification::{RedactionDecision, ReviewCoverage},
    command::RevisionRef,
    evidence::{ArtifactBody, ArtifactRole, SelectorRequest},
    identity::{Id, Kind},
    input,
    state::State,
};

/// Derivation steps followed from one artifact before lineage is reported as
/// truncated. Real chains are short; the bound protects reads from a corrupt
/// or adversarial image.
pub const MAX_LINEAGE_STEPS: usize = 256;

/// How the output of a derivation was reviewed. Absent means not reviewed.
/// A review never lowers classification by itself; that needs a reviewed
/// `classification.downgrade` of the derivative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DerivationReview {
    #[schemars(length(min = 1, max = 2048))]
    pub method: String,
    pub coverage: ReviewCoverage,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 32), inner(length(min = 1, max = 2048)))]
    pub limitations: Vec<String>,
}

/// What a derivative records about its production beyond its inputs. The
/// inputs, transformation identity and version, and input digests are in the
/// Artifact's `transforms`; the output digest is the Artifact's `digest`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DerivationRecord {
    /// Short label of the output, as the producer named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Who ran the transformation, as reported. A claim, not verified identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Portions of the inputs the transformation used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selectors: Vec<SelectorRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redaction_decisions: Vec<RedactionDecision>,
    /// Review basis of the output; absent when nobody reviewed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<DerivationReview>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
    /// Namespaced producer metadata, retained opaque and never interpreted.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

/// An original acquisition a lineage reaches.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    /// The Source that records the acquisition.
    pub source_ref: RevisionRef,
    /// The retained original (or manifest) Artifact the derivation chain ends at.
    pub artifact_ref: RevisionRef,
    pub role: ArtifactRole,
}

/// One transformation between an artifact and its origins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DerivationStep {
    /// The derivative this step produced.
    pub artifact_ref: RevisionRef,
    pub transformation: String,
    pub version: String,
    pub inputs: Vec<RevisionRef>,
}

/// Where an artifact's bytes come from, followed to the original acquisitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lineage {
    pub artifact_ref: RevisionRef,
    pub role: ArtifactRole,
    /// Original acquisitions reached through every input, transitively.
    pub origins: Vec<Origin>,
    /// Derivation steps from this artifact towards its origins, nearest first.
    pub steps: Vec<DerivationStep>,
    /// Longest number of derivation steps from this artifact to an origin.
    pub depth: usize,
    /// Inputs that do not resolve to a readable Artifact revision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<RevisionRef>,
    /// True when the step bound stopped the walk before every origin was reached.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

impl Lineage {
    /// Source IDs of the acquisitions this lineage reaches.
    pub fn acquisitions(&self) -> BTreeSet<Id> {
        self.origins
            .iter()
            .map(|origin| origin.source_ref.id)
            .collect()
    }

    /// Acquisitions this lineage shares with another one. Derivatives that share
    /// an acquisition are views of the same captured bytes.
    pub fn shared_acquisitions(&self, other: &Self) -> Vec<Id> {
        self.acquisitions()
            .intersection(&other.acquisitions())
            .copied()
            .collect()
    }
}

/// Decode an Artifact record body, or `None` for any other record.
pub fn artifact_body(state: &State, reference: RevisionRef) -> Option<ArtifactBody> {
    if reference.id.kind() != Kind::Artifact {
        return None;
    }
    input::decode(&state.revision(reference)?.body).ok()
}

/// Lineage of one exact Artifact revision, or `None` when it does not resolve.
pub fn lineage(state: &State, artifact_ref: RevisionRef) -> Option<Lineage> {
    let body = artifact_body(state, artifact_ref)?;
    let mut out = Lineage {
        artifact_ref,
        role: body.role(),
        origins: Vec::new(),
        steps: Vec::new(),
        depth: 0,
        unresolved: Vec::new(),
        truncated: false,
    };
    let mut origins = BTreeSet::new();
    let mut seen = BTreeSet::from([artifact_ref]);
    let mut queue = VecDeque::from([(artifact_ref, body, 0usize)]);
    while let Some((reference, body, distance)) = queue.pop_front() {
        if body.role() != ArtifactRole::Derivative {
            out.depth = out.depth.max(distance);
            origins.insert(Origin {
                source_ref: body.source,
                artifact_ref: reference,
                role: body.role(),
            });
            continue;
        }
        if out.steps.len() >= MAX_LINEAGE_STEPS {
            out.truncated = true;
            break;
        }
        let first = body.transforms.first();
        out.steps.push(DerivationStep {
            artifact_ref: reference,
            transformation: first.map(|t| t.transformation.clone()).unwrap_or_default(),
            version: first.map(|t| t.version.clone()).unwrap_or_default(),
            inputs: body.transforms.iter().map(|t| t.input).collect(),
        });
        for transform in &body.transforms {
            if !seen.insert(transform.input) {
                continue;
            }
            match artifact_body(state, transform.input) {
                Some(input) => queue.push_back((transform.input, input, distance + 1)),
                None => out.unresolved.push(transform.input),
            }
        }
    }
    out.origins = origins.into_iter().collect();
    Some(out)
}

/// Other Artifacts in the snapshot (current revisions) whose lineage reaches
/// an acquisition this lineage reaches, ordered by ID. Includes the originals
/// themselves and every other derivative of them.
pub fn sharing_origin(state: &State, lineage: &Lineage) -> Vec<RevisionRef> {
    let acquisitions = lineage.acquisitions();
    if acquisitions.is_empty() {
        return Vec::new();
    }
    state
        .records()
        .filter(|record| {
            record.header.id.kind() == Kind::Artifact && record.header.id != lineage.artifact_ref.id
        })
        .filter_map(|record| self::lineage(state, record.reference()))
        .filter(|other| {
            other.role != ArtifactRole::Manifest && !other.acquisitions().is_disjoint(&acquisitions)
        })
        .map(|other| other.artifact_ref)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        evidence::{
            ArtifactAvailability, ArtifactIntegrity, ArtifactSensitivity, ArtifactTransform,
        },
        header::{RecordHeader, Revision, SchemaVersion},
        state::{AcceptedTransaction, Record},
        wire::RecordChange,
    };

    fn id(kind: Kind, n: u64) -> Id {
        format!("{}_018d87fc-5234-7000-8000-{n:012x}", kind.prefix())
            .parse()
            .unwrap()
    }

    fn header(id: Id, case_id: Id, sequence: u64) -> RecordHeader {
        RecordHeader {
            id,
            case_id,
            schema_version: SchemaVersion::RecordV0Alpha,
            created_by: "agent:test".parse().unwrap(),
            actor_descriptor_self_reported: None,
            created_at: "2026-11-22T08:12:44.000Z".parse().unwrap(),
            recorded_at: "2026-11-22T08:12:44.000Z".parse().unwrap(),
            recorded_by: "agent:test".parse().unwrap(),
            observed_at: None,
            acquired_at: None,
            acceptance_sequence: sequence,
            revision: Revision(1),
            history: Vec::new(),
            classification: Default::default(),
            provenance: Default::default(),
            extensions: Default::default(),
        }
    }

    fn artifact(source: Id, inputs: &[Id]) -> Value {
        let transforms = inputs
            .iter()
            .map(|input| ArtifactTransform {
                input: RevisionRef {
                    id: *input,
                    revision: 1,
                },
                transformation: "example.step".into(),
                version: "1".into(),
                input_digest: None,
            })
            .collect::<Vec<_>>();
        serde_json::to_value(ArtifactBody {
            digest: None,
            size_bytes: None,
            format: None,
            source: RevisionRef {
                id: source,
                revision: 1,
            },
            role: Some(if transforms.is_empty() {
                ArtifactRole::Original
            } else {
                ArtifactRole::Derivative
            }),
            transforms,
            availability: ArtifactAvailability::Missing,
            integrity: ArtifactIntegrity::Unavailable,
            sensitivity: ArtifactSensitivity::Internal,
            derivation: None,
            external: None,
            status: None,
            withdrawals: Vec::new(),
            availability_change: None,
        })
        .unwrap()
    }

    fn state(records: Vec<(Id, Value)>) -> State {
        let case_id = id(Kind::Case, 0);
        let mut all = vec![(case_id, serde_json::json!({}))];
        all.extend(records);
        let mut state = State::default();
        let transaction = AcceptedTransaction {
            case_id,
            sequence: 1,
            created: all
                .iter()
                .map(|(id, _)| RecordChange {
                    id: *id,
                    revision: 1,
                    client_ref: None,
                })
                .collect(),
            updated: Vec::new(),
            records: all
                .into_iter()
                .map(|(id, body)| Record {
                    header: header(id, case_id, 1),
                    body,
                })
                .collect(),
            warnings: Vec::new(),
        };
        state.apply(&transaction).unwrap();
        state
    }

    fn reference(id: Id) -> RevisionRef {
        RevisionRef { id, revision: 1 }
    }

    #[test]
    fn chains_trace_to_the_original_and_siblings_share_it() {
        let (source, other_source) = (id(Kind::Source, 1), id(Kind::Source, 2));
        let original = id(Kind::Artifact, 1);
        let extracted = id(Kind::Artifact, 2);
        let redacted = id(Kind::Artifact, 3);
        let plot = id(Kind::Artifact, 4);
        let unrelated = id(Kind::Artifact, 5);
        let state = state(vec![
            (source, serde_json::json!({})),
            (other_source, serde_json::json!({})),
            (original, artifact(source, &[])),
            (extracted, artifact(source, &[original])),
            (redacted, artifact(source, &[extracted])),
            (plot, artifact(source, &[original])),
            (unrelated, artifact(other_source, &[])),
        ]);

        let chain = lineage(&state, reference(redacted)).unwrap();
        assert_eq!(chain.role, ArtifactRole::Derivative);
        assert_eq!(chain.depth, 2);
        assert_eq!(
            chain.origins,
            vec![Origin {
                source_ref: reference(source),
                artifact_ref: reference(original),
                role: ArtifactRole::Original,
            }]
        );
        assert_eq!(
            chain
                .steps
                .iter()
                .map(|step| step.artifact_ref.id)
                .collect::<Vec<_>>(),
            vec![redacted, extracted]
        );

        let sibling = lineage(&state, reference(plot)).unwrap();
        assert_eq!(chain.shared_acquisitions(&sibling), vec![source]);
        let separate = lineage(&state, reference(unrelated)).unwrap();
        assert!(chain.shared_acquisitions(&separate).is_empty());
        assert_eq!(separate.depth, 0);

        let related: Vec<Id> = sharing_origin(&state, &chain)
            .into_iter()
            .map(|reference| reference.id)
            .collect();
        assert_eq!(related, vec![original, extracted, plot]);
    }

    #[test]
    fn aggregation_reaches_every_input_acquisition_and_reports_dangling_inputs() {
        let (first, second) = (id(Kind::Source, 1), id(Kind::Source, 2));
        let (a, b) = (id(Kind::Artifact, 1), id(Kind::Artifact, 2));
        let missing = id(Kind::Artifact, 9);
        let aggregate = id(Kind::Artifact, 3);
        let state = state(vec![
            (first, serde_json::json!({})),
            (second, serde_json::json!({})),
            (a, artifact(first, &[])),
            (b, artifact(second, &[])),
            (aggregate, artifact(first, &[a, b, missing])),
        ]);
        let lineage = lineage(&state, reference(aggregate)).unwrap();
        assert_eq!(lineage.acquisitions(), BTreeSet::from([first, second]));
        assert_eq!(lineage.unresolved, vec![reference(missing)]);
        assert_eq!(lineage.depth, 1);
    }
}
