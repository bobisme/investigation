//! Optional coverage plans: a declared, bounded space of cells and what was
//! looked at in each.
//!
//! Most cases have no finite space to cover, so a plan exists only when one is
//! defined with `coverage.define`. A plan names its dimensions and their
//! values, a default predicate and assessment polarity (overridable per cell),
//! an applicability statement, and exclusions with reasons. Its Cartesian size
//! is bounded by [`MAX_CELLS`]; storage is sparse: only overrides, exclusion
//! selectors and links are recorded, never one entry per cell.
//!
//! The frozen v1 identity scheme has no CoveragePlan prefix, so a plan is an
//! Annotation with role [`COVERAGE_ROLE`] whose body holds the plan under
//! [`COVERAGE_KEY`]. Its `subject_refs` are the exact records the dimension
//! values name (options, criteria, components), so a changed coordinate shows
//! through the ordinary freshness index without new index rules.
//!
//! The plan revision is `definition_revision`: `coverage.revise` raises it and
//! appends the denominator to the kept history; `coverage.link` appends links
//! without changing it. Cell state is a read-time judgment over one snapshot:
//!
//! - excluded cells are `not_applicable_with_reason` and leave the denominator;
//! - a cell without a link is `untested`;
//! - a linked cell takes the latest link's state, unless the cell's definition
//!   changed since the link, or a linked or coordinate record changed in a way
//!   [`crate::freshness::successor_change`] reports (an invalidated
//!   measurement, a retraction, a material criterion revision). Then the cell
//!   is `needs_retest` and no longer counts as assessed.
//!
//! A percentage is only ever produced as a [`CoveragePercentage`], which
//! carries the plan revision and denominator with it. Link and record counts
//! are reported as activity, never as coverage. Evaluation plans present the
//! labels `assessed`, `unknown`, `excluded` and `stale` and no positive or
//! negative counts.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::{
    command::{Reference, RevisionRef},
    freshness::{Trigger, successor_change},
    header::Actor,
    identity::{Id, Kind},
    input,
    state::{Record, State},
    wire::{Digest, ErrorCode, Failure},
};

/// Annotation role that marks a coverage plan.
pub const COVERAGE_ROLE: &str = "coverage_plan";
/// Annotation body key that holds the stored plan.
pub const COVERAGE_KEY: &str = "coverage_plan";
/// Version of the cell-state and counting rules in this module.
pub const COVERAGE_RULES_VERSION: &str = "coverage.v1";
/// Largest Cartesian size a plan may declare, exclusions included.
pub const MAX_CELLS: u64 = 4096;
pub const MAX_DIMENSIONS: usize = 6;
pub const MAX_VALUES: usize = 512;
pub const MAX_CELL_OVERRIDES: usize = 256;
pub const MAX_EXCLUSIONS: usize = 64;
pub const MAX_COORDINATE_REFS: usize = 64;
/// Links kept in one plan's log.
pub const MAX_LINKS: usize = 1024;
pub const MAX_LINKS_PER_OPERATION: usize = 64;
pub const MAX_LINK_REFS: usize = 16;
/// Definition revisions kept in the denominator history.
pub const MAX_REVISIONS: usize = 64;
const TITLE_LIMIT: usize = 256;
const NAME_LIMIT: usize = 64;
const KEY_LIMIT: usize = 128;
const DESCRIPTION_LIMIT: usize = 512;
pub(crate) const TEXT_LIMIT: usize = 2048;
/// Cells listed per plan view; the rest are counted.
const LISTED_CELLS_SUMMARY: usize = 16;
const LISTED_CELLS_FULL: usize = 256;
/// Untested cells listed in a full view, as candidates for the next attempt.
const LISTED_UNTESTED: usize = 32;
/// Plans listed on a case view.
const LISTED_PLANS: usize = 16;

/// How cells are presented. `evaluation` is eligible options × active
/// criteria of one question and reads as assessed, unknown, excluded or stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    General,
    Evaluation,
}

/// One value of a dimension. `record_ref` names the exact current record the
/// value stands for, such as an Option, a Criterion or a component's Source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DimensionValue {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_ref: Option<Reference>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dimension {
    pub name: String,
    pub values: Vec<DimensionValue>,
}

/// What each recorded result means for a cell's predicate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Polarity {
    /// What `observed_positive` states about the cell.
    pub positive: String,
    /// What `observed_negative_within_scope` states about the cell.
    pub negative: String,
}

/// A cell with its own predicate or polarity. `at` names every dimension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellOverride {
    pub at: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polarity: Option<Polarity>,
}

/// Cells outside the plan's applicability. `matching` names some dimensions;
/// every cell with those values is excluded and leaves the denominator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Exclusion {
    pub matching: BTreeMap<String, String>,
    pub reason: String,
}

/// The declared space. The core never adds a dimension, value or default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanDefinition {
    pub title: String,
    pub plan_kind: PlanKind,
    /// The Question an evaluation plan compares options for. Required for
    /// `evaluation`, optional otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_ref: Option<Reference>,
    /// Exactly where the plan applies, stated once for every cell.
    pub applicability: String,
    pub dimensions: Vec<Dimension>,
    /// The default exact predicate each cell assesses at its coordinates.
    pub predicate: String,
    pub polarity: Polarity,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cells: Vec<CellOverride>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclusions: Vec<Exclusion>,
}

/// A recorded result for one cell. `untested`, `not_applicable_with_reason`
/// and `needs_retest` are computed, never recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    Blocked,
    AttemptedInconclusive,
    ObservedNegativeWithinScope,
    ObservedPositive,
}

impl LinkState {
    pub fn observed(self) -> bool {
        matches!(
            self,
            Self::ObservedNegativeWithinScope | Self::ObservedPositive
        )
    }
}

/// Effective state of one cell.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CellState {
    Untested,
    Blocked,
    AttemptedInconclusive,
    ObservedNegativeWithinScope,
    ObservedPositive,
    NotApplicableWithReason,
    NeedsRetest,
}

impl CellState {
    pub const ALL: [Self; 7] = [
        Self::Untested,
        Self::Blocked,
        Self::AttemptedInconclusive,
        Self::ObservedNegativeWithinScope,
        Self::ObservedPositive,
        Self::NotApplicableWithReason,
        Self::NeedsRetest,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Untested => "untested",
            Self::Blocked => "blocked",
            Self::AttemptedInconclusive => "attempted_inconclusive",
            Self::ObservedNegativeWithinScope => "observed_negative_within_scope",
            Self::ObservedPositive => "observed_positive",
            Self::NotApplicableWithReason => "not_applicable_with_reason",
            Self::NeedsRetest => "needs_retest",
        }
    }

    /// Counted as confidently assessed.
    pub const fn assessed(self) -> bool {
        matches!(
            self,
            Self::ObservedNegativeWithinScope | Self::ObservedPositive
        )
    }

    pub const fn label(self) -> EvaluationLabel {
        match self {
            Self::ObservedNegativeWithinScope | Self::ObservedPositive => EvaluationLabel::Assessed,
            Self::Untested | Self::Blocked | Self::AttemptedInconclusive => {
                EvaluationLabel::Unknown
            }
            Self::NotApplicableWithReason => EvaluationLabel::Excluded,
            Self::NeedsRetest => EvaluationLabel::Stale,
        }
    }
}

impl From<LinkState> for CellState {
    fn from(state: LinkState) -> Self {
        match state {
            LinkState::Blocked => Self::Blocked,
            LinkState::AttemptedInconclusive => Self::AttemptedInconclusive,
            LinkState::ObservedNegativeWithinScope => Self::ObservedNegativeWithinScope,
            LinkState::ObservedPositive => Self::ObservedPositive,
        }
    }
}

/// How an evaluation plan presents a cell.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationLabel {
    Assessed,
    Unknown,
    Excluded,
    Stale,
}

impl EvaluationLabel {
    pub const ALL: [Self; 4] = [Self::Assessed, Self::Unknown, Self::Excluded, Self::Stale];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Assessed => "assessed",
            Self::Unknown => "unknown",
            Self::Excluded => "excluded",
            Self::Stale => "stale",
        }
    }
}

/// One appended link: the recorded result of one cell, its exact records and
/// the cell definition it was recorded against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellLink {
    pub cell: BTreeMap<String, String>,
    pub state: LinkState,
    pub record_refs: Vec<RevisionRef>,
    pub rationale: String,
    pub linked_by: Actor,
    pub accepted_at_sequence: u64,
    pub plan_revision: u64,
    pub cell_digest: Digest,
}

/// One entry of the denominator history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DefinitionRevision {
    pub plan_revision: u64,
    pub accepted_at_sequence: u64,
    pub revised_by: Actor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub cartesian_size: u64,
    pub excluded: u64,
    pub denominator: u64,
}

/// The plan as stored under [`COVERAGE_KEY`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoredPlan {
    pub rules_version: String,
    pub plan_revision: u64,
    pub definition: PlanDefinition,
    pub history: Vec<DefinitionRevision>,
    #[serde(default)]
    pub links: Vec<CellLink>,
}

pub(crate) fn invalid(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(path)
}

pub(crate) fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "A stored coverage plan is invalid.",
    )
}

pub(crate) fn nonblank(value: &str, max: usize, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() || value.chars().count() > max {
        return Err(invalid(
            path,
            format!("Supply nonblank text no longer than {max} characters."),
        ));
    }
    Ok(())
}

/// Whether a record is a coverage plan.
pub fn is_plan(record: &Record) -> bool {
    record.header.id.kind() == Kind::Annotation
        && record.body.get("role").and_then(Value::as_str) == Some(COVERAGE_ROLE)
}

/// Decode the stored plan of a coverage plan record.
pub fn stored(record: &Record) -> Result<StoredPlan, Failure> {
    if !is_plan(record) {
        return Err(integrity());
    }
    input::decode(&record.body[COVERAGE_KEY]).map_err(|_| integrity())
}

/// The checked shape of a definition: dimension names and value keys in
/// declared order, Cartesian size and excluded count.
#[derive(Clone, Debug)]
pub(crate) struct Space {
    names: Vec<String>,
    keys: Vec<Vec<String>>,
    pub(crate) cartesian_size: u64,
    pub(crate) excluded: u64,
}

impl Space {
    pub(crate) fn denominator(&self) -> u64 {
        self.cartesian_size - self.excluded
    }

    /// Value indices of the cell at `index` in declared order.
    fn coordinates(&self, mut index: u64) -> Vec<usize> {
        let mut out = vec![0; self.keys.len()];
        for (slot, keys) in out.iter_mut().zip(&self.keys).rev() {
            let radix = keys.len() as u64;
            *slot = (index % radix) as usize;
            index /= radix;
        }
        out
    }

    /// Value indices of a full cell selector, or the reason it is not a cell.
    pub(crate) fn locate(&self, at: &BTreeMap<String, String>) -> Result<Vec<usize>, String> {
        if at.len() != self.names.len() {
            return Err(format!(
                "Name every dimension exactly once: {}.",
                self.names.join(", ")
            ));
        }
        self.names
            .iter()
            .zip(&self.keys)
            .map(|(name, keys)| {
                let key = at
                    .get(name)
                    .ok_or_else(|| format!("The cell does not name dimension {name:?}."))?;
                keys.iter()
                    .position(|candidate| candidate == key)
                    .ok_or_else(|| format!("Dimension {name:?} has no value {key:?}."))
            })
            .collect()
    }

    fn at(&self, coordinates: &[usize]) -> BTreeMap<String, String> {
        self.names
            .iter()
            .zip(&self.keys)
            .zip(coordinates)
            .map(|((name, keys), index)| (name.clone(), keys[*index].clone()))
            .collect()
    }
}

/// The first exclusion that matches the cell.
fn exclusion<'d>(
    definition: &'d PlanDefinition,
    at: &BTreeMap<String, String>,
) -> Option<&'d Exclusion> {
    definition.exclusions.iter().find(|exclusion| {
        exclusion
            .matching
            .iter()
            .all(|(name, key)| at.get(name) == Some(key))
    })
}

/// Check the structure of a definition without reading state: names, keys,
/// overrides, exclusions, the Cartesian bound and a nonzero denominator.
pub(crate) fn check_definition(definition: &PlanDefinition, base: &str) -> Result<Space, Failure> {
    nonblank(&definition.title, TITLE_LIMIT, &format!("{base}/title"))?;
    nonblank(
        &definition.applicability,
        TEXT_LIMIT,
        &format!("{base}/applicability"),
    )?;
    nonblank(
        &definition.predicate,
        TEXT_LIMIT,
        &format!("{base}/predicate"),
    )?;
    polarity(&definition.polarity, &format!("{base}/polarity"))?;
    if definition.plan_kind == PlanKind::Evaluation && definition.question_ref.is_none() {
        return Err(invalid(
            &format!("{base}/question_ref"),
            "An evaluation plan names the Question whose options and criteria it covers.",
        ));
    }
    let dims_path = format!("{base}/dimensions");
    if definition.dimensions.is_empty() || definition.dimensions.len() > MAX_DIMENSIONS {
        return Err(invalid(
            &dims_path,
            format!("Declare between 1 and {MAX_DIMENSIONS} dimensions."),
        ));
    }
    let mut names = BTreeSet::new();
    let mut size: u64 = 1;
    let mut coordinate_refs = 0usize;
    for (index, dimension) in definition.dimensions.iter().enumerate() {
        let path = format!("{dims_path}/{index}");
        nonblank(&dimension.name, NAME_LIMIT, &format!("{path}/name"))?;
        if !names.insert(dimension.name.as_str()) {
            return Err(invalid(
                &format!("{path}/name"),
                "Name each dimension once.",
            ));
        }
        if dimension.values.is_empty() || dimension.values.len() > MAX_VALUES {
            return Err(invalid(
                &format!("{path}/values"),
                format!("Declare between 1 and {MAX_VALUES} values per dimension."),
            ));
        }
        let mut keys = BTreeSet::new();
        for (position, value) in dimension.values.iter().enumerate() {
            let value_path = format!("{path}/values/{position}");
            nonblank(&value.key, KEY_LIMIT, &format!("{value_path}/key"))?;
            if !keys.insert(value.key.as_str()) {
                return Err(invalid(
                    &format!("{value_path}/key"),
                    "List each value of a dimension once.",
                ));
            }
            if let Some(description) = &value.description {
                nonblank(
                    description,
                    DESCRIPTION_LIMIT,
                    &format!("{value_path}/description"),
                )?;
            }
            coordinate_refs += usize::from(value.record_ref.is_some());
        }
        size = size.saturating_mul(dimension.values.len() as u64);
    }
    if coordinate_refs > MAX_COORDINATE_REFS {
        return Err(invalid(
            &dims_path,
            format!("Name at most {MAX_COORDINATE_REFS} records across dimension values."),
        ));
    }
    if size > MAX_CELLS {
        let shape: Vec<String> = definition
            .dimensions
            .iter()
            .map(|dimension| format!("{} {}", dimension.values.len(), dimension.name))
            .collect();
        let mut failure = invalid(
            &dims_path,
            format!(
                "The plan declares {size} cells ({}), above the bound of {MAX_CELLS}. Narrow it: keep only values that could change the answer, merge values that are tested together, or split the space into separate plans by one dimension. Exclusions do not reduce the declared size.",
                shape.join(" × ")
            ),
        );
        failure.error.expected = Some(json!({"max_cells": MAX_CELLS}));
        failure.error.actual = Some(json!({
            "cartesian_size": size,
            "dimensions": definition.dimensions.iter().map(|dimension| json!({
                "name": dimension.name, "values": dimension.values.len()
            })).collect::<Vec<_>>(),
        }));
        return Err(failure);
    }
    let mut space = Space {
        names: definition
            .dimensions
            .iter()
            .map(|dimension| dimension.name.clone())
            .collect(),
        keys: definition
            .dimensions
            .iter()
            .map(|dimension| dimension.values.iter().map(|v| v.key.clone()).collect())
            .collect(),
        cartesian_size: size,
        excluded: 0,
    };
    if definition.exclusions.len() > MAX_EXCLUSIONS {
        return Err(invalid(
            &format!("{base}/exclusions"),
            format!("Declare at most {MAX_EXCLUSIONS} exclusions."),
        ));
    }
    for (index, exclusion) in definition.exclusions.iter().enumerate() {
        let path = format!("{base}/exclusions/{index}");
        nonblank(&exclusion.reason, TEXT_LIMIT, &format!("{path}/reason"))?;
        if exclusion.matching.is_empty() {
            return Err(invalid(
                &format!("{path}/matching"),
                "Name at least one dimension value the exclusion matches.",
            ));
        }
        for (name, key) in &exclusion.matching {
            let position = space.names.iter().position(|candidate| candidate == name);
            let known = position.is_some_and(|position| space.keys[position].contains(key));
            if !known {
                return Err(invalid(
                    &format!("{path}/matching"),
                    format!("No dimension {name:?} with value {key:?} is declared."),
                ));
            }
        }
    }
    if definition.cells.len() > MAX_CELL_OVERRIDES {
        return Err(invalid(
            &format!("{base}/cells"),
            format!("Declare at most {MAX_CELL_OVERRIDES} cell overrides."),
        ));
    }
    let mut overridden = BTreeSet::new();
    for (index, cell) in definition.cells.iter().enumerate() {
        let path = format!("{base}/cells/{index}");
        let coordinates = space
            .locate(&cell.at)
            .map_err(|message| invalid(&format!("{path}/at"), message))?;
        if !overridden.insert(coordinates) {
            return Err(invalid(&format!("{path}/at"), "Override each cell once."));
        }
        if cell.predicate.is_none() && cell.polarity.is_none() {
            return Err(invalid(
                &path,
                "An override states the cell's own predicate, polarity or both.",
            ));
        }
        if let Some(predicate) = &cell.predicate {
            nonblank(predicate, TEXT_LIMIT, &format!("{path}/predicate"))?;
        }
        if let Some(cell_polarity) = &cell.polarity {
            polarity(cell_polarity, &format!("{path}/polarity"))?;
        }
        if exclusion(definition, &cell.at).is_some() {
            return Err(invalid(
                &format!("{path}/at"),
                "The cell is excluded; an excluded cell has no predicate to assess.",
            ));
        }
    }
    space.excluded = (0..space.cartesian_size)
        .filter(|index| exclusion(definition, &space.at(&space.coordinates(*index))).is_some())
        .count() as u64;
    if space.denominator() == 0 {
        return Err(invalid(
            &format!("{base}/exclusions"),
            "Every cell is excluded, so the plan has no denominator. Narrow the exclusions or the dimensions.",
        ));
    }
    Ok(space)
}

fn polarity(polarity: &Polarity, path: &str) -> Result<(), Failure> {
    nonblank(&polarity.positive, TEXT_LIMIT, &format!("{path}/positive"))?;
    nonblank(&polarity.negative, TEXT_LIMIT, &format!("{path}/negative"))?;
    if polarity.positive.trim() == polarity.negative.trim() {
        return Err(invalid(
            path,
            "State different meanings for a positive and a negative result.",
        ));
    }
    Ok(())
}

fn exact(reference: &Reference) -> Option<RevisionRef> {
    match reference {
        Reference::Existing(reference) => Some(*reference),
        Reference::Client(_) => None,
    }
}

/// Exact records the cell's coordinates name.
fn coordinate_refs(definition: &PlanDefinition, coordinates: &[usize]) -> Vec<RevisionRef> {
    definition
        .dimensions
        .iter()
        .zip(coordinates)
        .filter_map(|(dimension, index)| dimension.values[*index].record_ref.as_ref())
        .filter_map(exact)
        .collect()
}

/// Every exact record the plan's dimension values name, once each.
pub(crate) fn all_coordinate_refs(definition: &PlanDefinition) -> Vec<RevisionRef> {
    let mut seen = BTreeSet::new();
    definition
        .dimensions
        .iter()
        .flat_map(|dimension| &dimension.values)
        .filter_map(|value| value.record_ref.as_ref())
        .filter_map(exact)
        .filter(|reference| seen.insert(*reference))
        .collect()
}

/// The effective predicate and polarity of a cell.
fn cell_terms<'d>(
    definition: &'d PlanDefinition,
    at: &BTreeMap<String, String>,
) -> (&'d str, &'d Polarity) {
    let cell = definition.cells.iter().find(|cell| &cell.at == at);
    (
        cell.and_then(|cell| cell.predicate.as_deref())
            .unwrap_or(&definition.predicate),
        cell.and_then(|cell| cell.polarity.as_ref())
            .unwrap_or(&definition.polarity),
    )
}

/// Digest of everything a result in this cell was recorded against: the
/// applicability, the effective predicate and polarity, and the exact
/// coordinate records. Any change to them makes earlier links stale.
pub(crate) fn cell_digest(
    definition: &PlanDefinition,
    space: &Space,
    coordinates: &[usize],
) -> Result<Digest, Failure> {
    let at = space.at(coordinates);
    let (predicate, polarity) = cell_terms(definition, &at);
    let value = json!({
        "plan_kind": definition.plan_kind,
        "applicability": definition.applicability,
        "at": at,
        "predicate": predicate,
        "polarity": polarity,
        "coordinate_refs": coordinate_refs(definition, coordinates),
    });
    let bytes = serde_json::to_vec(&value).map_err(|_| integrity())?;
    format!("sha256:{:x}", Sha256::digest(bytes))
        .parse()
        .map_err(|_| integrity())
}

/// Why a linked cell no longer counts as assessed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetestReason {
    /// The cell's predicate, polarity, applicability or coordinate records
    /// were revised after the result was linked.
    CellDefinitionChanged {
        linked_plan_revision: u64,
        plan_revision: u64,
    },
    /// A linked Run, Observation or Comparison changed in a way that affects
    /// a reader of the linked revision, such as an invalidated measurement.
    LinkedRecordChanged {
        record_ref: RevisionRef,
        trigger: Trigger,
    },
    /// A record a coordinate names, such as a Criterion, changed materially.
    CoordinateChanged {
        record_ref: RevisionRef,
        trigger: Trigger,
    },
}

/// One cell as read now. Fields that do not apply are omitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CellView {
    pub at: BTreeMap<String, String>,
    /// Effective state, in a general plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<CellState>,
    /// Presentation label, in an evaluation plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<EvaluationLabel>,
    /// Why an evaluation cell is unknown: untested, blocked or inconclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_because: Option<CellState>,
    /// What the recorded result states, from the cell's polarity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusion_reason: Option<String>,
    /// The result recorded before the cell became stale, in a general plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_state: Option<LinkState>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub record_refs: Vec<RevisionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_at_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub retest: Vec<RetestReason>,
}

/// A coverage fraction. It exists only with the exact plan revision and
/// denominator it is computed over; there is no bare percentage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CoveragePercentage {
    percent: String,
    assessed: u64,
    denominator: u64,
    plan_revision: u64,
    plan_ref: RevisionRef,
    statement: String,
}

impl CoveragePercentage {
    fn new(
        plan_ref: RevisionRef,
        plan_revision: u64,
        assessed: u64,
        denominator: u64,
    ) -> Option<Self> {
        if denominator == 0 {
            return None;
        }
        // Tenths, rounded half up, with integer arithmetic only.
        let tenths = (assessed * 2000 + denominator) / (2 * denominator);
        let percent = format!("{}.{}", tenths / 10, tenths % 10);
        let statement = format!(
            "{assessed} of {denominator} applicable cells assessed ({percent}%) under plan revision {plan_revision} of {}",
            plan_ref.id
        );
        Some(Self {
            percent,
            assessed,
            denominator,
            plan_revision,
            plan_ref,
            statement,
        })
    }

    pub fn statement(&self) -> &str {
        &self.statement
    }
}

/// Exact records a coordinate names whose current revision differs in a way
/// that matters; the plan should be revised to pin them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StaleCoordinate {
    pub record_ref: RevisionRef,
    pub trigger: Trigger,
}

/// Links recorded and records they name: activity, not coverage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Activity {
    pub links_recorded: u64,
    pub linked_records: u64,
    pub note: &'static str,
}

/// A plan read over one snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlanEvaluation {
    pub rules_version: &'static str,
    pub plan_ref: RevisionRef,
    pub title: String,
    pub plan_kind: PlanKind,
    pub plan_revision: u64,
    pub applicability: String,
    pub cartesian_size: u64,
    pub cartesian_bound: u64,
    pub excluded: u64,
    pub denominator: u64,
    pub assessed: u64,
    /// Absent only when the denominator is zero.
    pub percentage: Option<CoveragePercentage>,
    /// Cells per state (general) or per label (evaluation), zeros included.
    pub counts: BTreeMap<&'static str, u64>,
    /// In an evaluation plan, why `unknown` cells are unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_because: Option<BTreeMap<&'static str, u64>>,
    /// Every cell that is not plainly untested, in declared order.
    pub cells: Vec<CellView>,
    /// The first untested cells in declared order.
    pub untested_sample: Vec<BTreeMap<String, String>>,
    pub stale_coordinates: Vec<StaleCoordinate>,
    /// Links whose cell is no longer in the plan after a revision.
    pub orphaned_links: u64,
    pub activity: Activity,
    pub denominator_history: Vec<DefinitionRevision>,
}

/// Read one plan record over `state`.
pub fn evaluate(state: &State, record: &Record) -> Result<PlanEvaluation, Failure> {
    let plan = stored(record)?;
    let definition = &plan.definition;
    let space = check_definition(definition, "").map_err(|_| integrity())?;
    let case = record.header.case_id;
    let changed = |reference: RevisionRef| successor_change(state, case, reference);
    let stale_coordinates: Vec<StaleCoordinate> = all_coordinate_refs(definition)
        .into_iter()
        .filter_map(|record_ref| {
            changed(record_ref).map(|trigger| StaleCoordinate {
                record_ref,
                trigger,
            })
        })
        .collect();
    let stale_coordinate = |reference: &RevisionRef| {
        stale_coordinates
            .iter()
            .find(|stale| stale.record_ref == *reference)
            .map(|stale| stale.trigger.clone())
    };
    // The latest link per cell, keyed by value indices.
    let mut latest: BTreeMap<Vec<usize>, &CellLink> = BTreeMap::new();
    let mut orphaned = 0u64;
    let mut linked_records = BTreeSet::new();
    for link in &plan.links {
        linked_records.extend(link.record_refs.iter().map(|reference| reference.id));
        match space.locate(&link.cell) {
            Ok(coordinates) => {
                latest.insert(coordinates, link);
            }
            Err(_) => orphaned += 1,
        }
    }
    let evaluation = plan_kind_is_evaluation(definition);
    let mut counts: BTreeMap<CellState, u64> = BTreeMap::new();
    let mut cells = Vec::new();
    let mut untested_sample = Vec::new();
    for index in 0..space.cartesian_size {
        let coordinates = space.coordinates(index);
        let at = space.at(&coordinates);
        let mut view = CellView {
            at,
            state: None,
            label: None,
            unknown_because: None,
            result: None,
            exclusion_reason: None,
            recorded_state: None,
            record_refs: Vec::new(),
            linked_at_sequence: None,
            retest: Vec::new(),
        };
        let state_now = if let Some(exclusion) = exclusion(definition, &view.at) {
            view.exclusion_reason = Some(exclusion.reason.clone());
            CellState::NotApplicableWithReason
        } else if let Some(link) = latest.get(&coordinates) {
            view.record_refs = link.record_refs.clone();
            view.linked_at_sequence = Some(link.accepted_at_sequence);
            if link.cell_digest != cell_digest(definition, &space, &coordinates)? {
                view.retest.push(RetestReason::CellDefinitionChanged {
                    linked_plan_revision: link.plan_revision,
                    plan_revision: plan.plan_revision,
                });
            }
            for record_ref in &link.record_refs {
                if let Some(trigger) = changed(*record_ref) {
                    view.retest.push(RetestReason::LinkedRecordChanged {
                        record_ref: *record_ref,
                        trigger,
                    });
                }
            }
            for record_ref in coordinate_refs(definition, &coordinates) {
                if let Some(trigger) = stale_coordinate(&record_ref) {
                    view.retest.push(RetestReason::CoordinateChanged {
                        record_ref,
                        trigger,
                    });
                }
            }
            if view.retest.is_empty() {
                let (_, polarity) = cell_terms(definition, &view.at);
                view.result = match link.state {
                    LinkState::ObservedPositive => Some(polarity.positive.clone()),
                    LinkState::ObservedNegativeWithinScope => Some(polarity.negative.clone()),
                    _ => None,
                };
                link.state.into()
            } else {
                if !evaluation {
                    view.recorded_state = Some(link.state);
                }
                CellState::NeedsRetest
            }
        } else {
            CellState::Untested
        };
        *counts.entry(state_now).or_default() += 1;
        if evaluation {
            let label = state_now.label();
            view.label = Some(label);
            if label == EvaluationLabel::Unknown {
                view.unknown_because = Some(state_now);
            }
        } else {
            view.state = Some(state_now);
        }
        if state_now == CellState::Untested {
            if untested_sample.len() < LISTED_UNTESTED {
                untested_sample.push(view.at);
            }
        } else {
            cells.push(view);
        }
    }
    let count = |state: CellState| counts.get(&state).copied().unwrap_or_default();
    let assessed =
        count(CellState::ObservedPositive) + count(CellState::ObservedNegativeWithinScope);
    let denominator = space.denominator();
    let (shown_counts, unknown_because) = if evaluation {
        let mut labels: BTreeMap<&'static str, u64> = EvaluationLabel::ALL
            .iter()
            .map(|label| (label.as_str(), 0))
            .collect();
        for (state, n) in &counts {
            *labels.entry(state.label().as_str()).or_default() += n;
        }
        let because = [
            CellState::Untested,
            CellState::Blocked,
            CellState::AttemptedInconclusive,
        ]
        .iter()
        .map(|state| (state.as_str(), count(*state)))
        .collect();
        (labels, Some(because))
    } else {
        (
            CellState::ALL
                .iter()
                .map(|state| (state.as_str(), count(*state)))
                .collect(),
            None,
        )
    };
    Ok(PlanEvaluation {
        rules_version: COVERAGE_RULES_VERSION,
        plan_ref: record.reference(),
        title: definition.title.clone(),
        plan_kind: definition.plan_kind,
        plan_revision: plan.plan_revision,
        applicability: definition.applicability.clone(),
        cartesian_size: space.cartesian_size,
        cartesian_bound: MAX_CELLS,
        excluded: space.excluded,
        denominator,
        assessed,
        percentage: CoveragePercentage::new(
            record.reference(),
            plan.plan_revision,
            assessed,
            denominator,
        ),
        counts: shown_counts,
        unknown_because,
        cells,
        untested_sample,
        stale_coordinates,
        orphaned_links: orphaned,
        activity: Activity {
            links_recorded: plan.links.len() as u64,
            linked_records: linked_records.len() as u64,
            note: "Links and linked records are activity, not coverage.",
        },
        denominator_history: plan.history,
    })
}

fn plan_kind_is_evaluation(definition: &PlanDefinition) -> bool {
    definition.plan_kind == PlanKind::Evaluation
}

/// Current coverage plans of a case, in creation order.
pub fn plans(state: &State, case: Id) -> Vec<&Record> {
    let mut plans: Vec<&Record> = state
        .records()
        .filter(|record| record.header.case_id == case && is_plan(record))
        .collect();
    plans.sort_by_key(|record| {
        (
            state
                .history(record.header.id)
                .next()
                .map_or(0, |first| first.header.acceptance_sequence),
            record.header.id,
        )
    });
    plans
}

/// The bounded view of one plan. Cells are listed up to a bound and the rest
/// are counted; the untested sample is listed only in full views.
fn plan_view(evaluation: PlanEvaluation, full: bool) -> Value {
    let limit = if full {
        LISTED_CELLS_FULL
    } else {
        LISTED_CELLS_SUMMARY
    };
    let mut evaluation = evaluation;
    let omitted = evaluation.cells.len().saturating_sub(limit);
    evaluation.cells.truncate(limit);
    if !full {
        evaluation.untested_sample.clear();
        evaluation.denominator_history = evaluation
            .denominator_history
            .split_off(evaluation.denominator_history.len().saturating_sub(1));
    }
    let mut value = json!(evaluation);
    value["cells_omitted"] = json!(omitted);
    value
}

/// The one-line summary of a plan on a case view.
fn plan_summary(evaluation: &PlanEvaluation) -> Value {
    let needs_retest = evaluation
        .cells
        .iter()
        .filter(|cell| {
            cell.state == Some(CellState::NeedsRetest) || cell.label == Some(EvaluationLabel::Stale)
        })
        .count();
    json!({
        "plan_ref": evaluation.plan_ref,
        "title": evaluation.title,
        "plan_kind": evaluation.plan_kind,
        "plan_revision": evaluation.plan_revision,
        "cartesian_size": evaluation.cartesian_size,
        "excluded": evaluation.excluded,
        "denominator": evaluation.denominator,
        "percentage": evaluation.percentage,
        "counts": evaluation.counts,
        "needs_retest": needs_retest,
        "stale_coordinates": evaluation.stale_coordinates.len(),
    })
}

/// The `coverage` field of a record view: the plan itself on a plan record,
/// the case's visible plans on a Case. `None` everywhere else, and on a case
/// without a plan, so no coverage section appears there.
pub fn projection(
    state: &State,
    record: &Record,
    full: bool,
    visible: &dyn Fn(&Record) -> bool,
) -> Option<Value> {
    match record.header.id.kind() {
        Kind::Annotation if is_plan(record) => Some(
            evaluate(state, record)
                .map(|evaluation| plan_view(evaluation, full))
                .unwrap_or_else(|_| json!({"state": "unreadable"})),
        ),
        Kind::Case => {
            let plans: Vec<&Record> = plans(state, record.header.id)
                .into_iter()
                .filter(|plan| visible(plan))
                .collect();
            if plans.is_empty() {
                return None;
            }
            let listed: Vec<Value> = plans
                .iter()
                .take(LISTED_PLANS)
                .map(|plan| match evaluate(state, plan) {
                    Ok(evaluation) => plan_summary(&evaluation),
                    Err(_) => json!({"plan_ref": plan.reference(), "state": "unreadable"}),
                })
                .collect();
            Some(json!({
                "rules_version": COVERAGE_RULES_VERSION,
                "plans": listed,
                "plans_omitted": plans.len().saturating_sub(LISTED_PLANS),
            }))
        }
        _ => None,
    }
}

/// Text lines for a view's `coverage` field, or empty. Each line is a full
/// percentage statement with its plan revision and denominator.
pub fn text(view: &Value) -> String {
    let coverage = &view["coverage"];
    let statements: Vec<&Value> = match coverage["plans"].as_array() {
        Some(plans) => plans.iter().map(|plan| &plan["percentage"]).collect(),
        None if coverage.is_object() => vec![&coverage["percentage"]],
        None => Vec::new(),
    };
    statements
        .into_iter()
        .filter_map(|percentage| percentage["statement"].as_str())
        .map(|statement| format!("\nCoverage: {statement}"))
        .collect()
}

#[cfg(test)]
mod tests;
