//! Evidence reuse and deviations at assessment, kept separate from chronology.
//!
//! Plan §6.5: order, prior exposure and evidence reuse are separate axes. A
//! frozen confirmatory InquiryPlan records chronology strength (on its Runs),
//! the investigator's declared prior exposure and the reserved holdout
//! boundary. An Assessment of such a plan records, in its optional
//! `frozen_basis`, what was actually used: the inputs and partitions, the
//! exclusions with reasons, the stop rule actually applied and every deviation
//! from the frozen plan with its reason.
//!
//! [`evidence_reuse`] compares the actual inputs with what the journal shows
//! was already read before the plan was frozen. It follows derivative lineage
//! ([`crate::derivation::lineage`]) to the original acquisitions, so a renamed
//! or re-extracted derivative counts as the same data. It classifies the test
//! as `fresh`, `known_outcome_reuse`, `held_out_within_declared_limits` or
//! `undetermined`.
//!
//! The classification never rejects an assessment and never changes or reads
//! chronology. It limits how the result is labeled. The chronology of the Runs
//! that produced the inputs is reported beside it, verbatim, as its own axis.
//!
//! Prior reads are the recorded ones:
//!
//! - an Observation accepted before the plan was frozen read the artifact its
//!   basis cites, and every original acquisition that artifact derives from;
//! - an Assessment accepted before the freeze read its cited evidence the same
//!   way;
//! - an Assessment accepted after the freeze but before this assessment, which
//!   is not itself an assessment of the same plan, also read its evidence: a
//!   look at reserved data outside the test is exposure too.
//!
//! Exposure through unrecorded reads, summaries or dashboards is visible only
//! through the plan's declared `prior_exposure`. Order is local journal order.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    command::{Reference, RevisionRef},
    derivation,
    evidence::ArtifactRole,
    identity::{Id, Kind},
    state::{OperationContext, Record, State},
    wire::{ErrorCode, Failure},
};

/// Version of the classification rules. It changes whenever the rules do.
pub const RULE_VERSION: &str = "evidence-reuse.v1";

/// Prior exposures listed per input before the rest are counted as omitted.
pub const MAX_EXPOSURES_PER_INPUT: usize = 16;

/// Runs listed on the chronology axis before the rest are counted as omitted.
pub const MAX_CHRONOLOGY_RUNS: usize = 32;

/// Kinds that may be named as an actual input of a test.
pub const INPUT_KINDS: &[Kind] = &[Kind::Observation, Kind::Artifact, Kind::Run, Kind::Source];

// ---------------------------------------------------------------------------
// Request payload
// ---------------------------------------------------------------------------

/// How an Assessment used a frozen confirmatory InquiryPlan: what it actually
/// read, what it left out and how it differs from what was frozen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrozenBasis {
    /// Exact frozen confirmatory inquiry revision this assessment tests.
    pub plan_ref: Reference,
    /// Observation units or partitions actually analysed.
    #[schemars(length(min = 1, max = 256))]
    pub actual_inputs: Vec<ActualInput>,
    /// Inputs or units left out of the analysis, each with its reason.
    #[serde(default)]
    #[schemars(length(max = 256))]
    pub exclusions: Vec<Exclusion>,
    /// The stop rule actually applied, in the investigator's words.
    #[schemars(length(min = 1, max = 2048))]
    pub stop_rule_applied: String,
    /// Every departure from the frozen plan, each with its reason. A
    /// fixed-sample analysis stopped after a favorable look is an `early_stop`.
    #[serde(default)]
    #[schemars(length(max = 64))]
    pub deviations: Vec<Deviation>,
}

/// One input the analysis actually read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActualInput {
    /// An Observation, Artifact, Run or Source the analysis read.
    pub input_ref: Reference,
    /// The observation unit or partition, as the holdout boundary names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 512))]
    pub partition: Option<String>,
    /// The investigator's declaration of this input relative to the reserved
    /// holdout. It is a claim; the core does not verify the boundary.
    #[serde(default)]
    pub holdout: HoldoutPlacement,
}

/// Where an input sits relative to the plan's reserved holdout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HoldoutPlacement {
    /// Inside the observation units or partitions the plan reserved.
    WithinReserved,
    /// Outside them.
    OutsideReserved,
    #[default]
    NotDeclared,
}

/// One input or unit left out of the analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Exclusion {
    /// What was left out.
    #[schemars(length(min = 1, max = 2048))]
    pub excluded: String,
    /// The excluded record, when it is recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_ref: Option<Reference>,
    #[schemars(length(min = 1, max = 2048))]
    pub reason: String,
}

/// One departure from the frozen plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Deviation {
    pub kind: DeviationKind,
    #[schemars(length(min = 1, max = 2048))]
    pub description: String,
    #[schemars(length(min = 1, max = 2048))]
    pub reason: String,
}

/// What kind of departure from the frozen plan a deviation is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DeviationKind {
    /// Analysis stopped before the frozen stop rule was met.
    EarlyStop,
    /// Collection or analysis continued past the frozen stop rule.
    LateStop,
    /// Inputs differ from the frozen observation units or partitions.
    InputsChanged,
    /// Exclusions not foreseen by the frozen plan.
    ExclusionsChanged,
    /// The analysis differs from the frozen analysis rule.
    AnalysisChanged,
    /// Controls differ from the frozen control conditions.
    ControlsChanged,
    Other,
}

impl DeviationKind {
    fn is_stop(self) -> bool {
        matches!(self, Self::EarlyStop | Self::LateStop)
    }
}

// ---------------------------------------------------------------------------
// Write-time validation
// ---------------------------------------------------------------------------

fn invalid(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(path)
}

fn blocked(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::GateFailed, message).at(path)
}

fn text(value: &str, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() {
        return Err(invalid(path, "Supply nonblank text describing this field."));
    }
    Ok(())
}

fn pin_current(
    ctx: &OperationContext<'_>,
    reference: &mut Reference,
    path: &str,
) -> Result<RevisionRef, Failure> {
    let resolved = ctx.resolve_reference(reference, path)?;
    if !INPUT_KINDS.contains(&resolved.id.kind()) {
        return Err(invalid(
            path,
            "Name an Observation, Artifact, Run or Source as a test input.",
        ));
    }
    let current = ctx.current(resolved.id).map(Record::reference);
    if current != Some(resolved) {
        let mut failure = Failure::new(
            ErrorCode::RevisionConflict,
            "The referenced record changed. Read the current revision and reassess.",
        )
        .at(path);
        failure.error.expected = Some(json!(resolved.revision));
        failure.error.actual = current.map(|current| json!(current.revision));
        failure.error.retry = Some("reassess".into());
        return Err(failure);
    }
    *reference = Reference::Existing(resolved);
    Ok(resolved)
}

/// Validate a `frozen_basis` and pin its references to exact revisions. The
/// plan must be an exact frozen confirmatory inquiry revision; inputs and
/// excluded records must be current. Blocks only on malformed structure; the
/// reuse classification never blocks.
pub fn pin_frozen_basis(
    ctx: &OperationContext<'_>,
    basis: &mut FrozenBasis,
    path: &str,
) -> Result<(), Failure> {
    let plan_path = format!("{path}/plan_ref");
    let plan = ctx.resolve(&basis.plan_ref, Kind::Inquiry, &plan_path)?;
    let record = ctx
        .exact(plan)
        .ok_or_else(|| invalid(&plan_path, "The inquiry revision is unavailable."))?;
    if record.body["lifecycle"] != "frozen" {
        return Err(blocked(
            &plan_path,
            "Pin the frozen revision of a confirmatory inquiry.",
        ));
    }
    if record.body["inquiry"]["mode"] != "confirmatory" {
        return Err(blocked(
            &plan_path,
            "A frozen basis applies to a confirmatory inquiry. Record an exploratory reading as an ordinary assessment.",
        ));
    }
    basis.plan_ref = Reference::Existing(plan);
    text(
        &basis.stop_rule_applied,
        &format!("{path}/stop_rule_applied"),
    )?;
    if basis.actual_inputs.is_empty() {
        return Err(blocked(
            &format!("{path}/actual_inputs"),
            "List the observation units or partitions the analysis actually read.",
        ));
    }
    let mut inputs = BTreeSet::new();
    for (index, input) in basis.actual_inputs.iter_mut().enumerate() {
        let item = format!("{path}/actual_inputs/{index}");
        let reference = pin_current(ctx, &mut input.input_ref, &format!("{item}/input_ref"))?;
        if !inputs.insert(reference.id) {
            return Err(invalid(
                &format!("{item}/input_ref"),
                "Name each input once.",
            ));
        }
        if let Some(partition) = &input.partition {
            text(partition, &format!("{item}/partition"))?;
        }
    }
    for (index, exclusion) in basis.exclusions.iter_mut().enumerate() {
        let item = format!("{path}/exclusions/{index}");
        text(&exclusion.excluded, &format!("{item}/excluded"))?;
        text(&exclusion.reason, &format!("{item}/reason"))?;
        if let Some(reference) = &mut exclusion.input_ref {
            let reference = pin_current(ctx, reference, &format!("{item}/input_ref"))?;
            if inputs.contains(&reference.id) {
                return Err(invalid(
                    &format!("{item}/input_ref"),
                    "A record cannot be both an actual input and an exclusion.",
                ));
            }
        }
    }
    for (index, deviation) in basis.deviations.iter().enumerate() {
        let item = format!("{path}/deviations/{index}");
        text(&deviation.description, &format!("{item}/description"))?;
        text(&deviation.reason, &format!("{item}/reason"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Read: classification beside chronology
// ---------------------------------------------------------------------------

/// How the inputs of a test relate to what was already known.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ReuseClass {
    /// Acquired after the freeze, with no recorded read before the test.
    Fresh,
    /// Already read before the test. It may support reproducibility, but it
    /// is not a fresh prospective test.
    KnownOutcomeReuse,
    /// Collected before the freeze, declared within the reserved holdout and
    /// not read before it.
    HeldOutWithinDeclaredLimits,
    /// The recorded structure cannot show which of the above applies.
    Undetermined,
}

impl ReuseClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::KnownOutcomeReuse => "known_outcome_reuse",
            Self::HeldOutWithinDeclaredLimits => "held_out_within_declared_limits",
            Self::Undetermined => "undetermined",
        }
    }

    /// The fixed label shown with a result of this class.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fresh => {
                "Fresh test: the inputs were acquired after the plan was frozen and no earlier read of them is recorded."
            }
            Self::KnownOutcomeReuse => {
                "Reuse of a known outcome: it may support reproducibility, but it does not count as a fresh prospective test."
            }
            Self::HeldOutWithinDeclaredLimits => {
                "Held out within declared limits: collected before the freeze, declared inside the reserved holdout and not read before it. The core does not verify the boundary."
            }
            Self::Undetermined => {
                "Reuse undetermined: the recorded structure cannot show whether the inputs were fresh or held out."
            }
        }
    }

    /// True when the result may be presented as a prospective test within the
    /// plan's declared limits.
    pub const fn counts_as_prospective(self) -> bool {
        matches!(self, Self::Fresh | Self::HeldOutWithinDeclaredLimits)
    }
}

/// How a prior read reached the data.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ExposureKind {
    /// An Observation extracted it before the plan was frozen.
    ObservedBeforeFreeze,
    /// An Assessment cited it before the plan was frozen.
    AssessedBeforeFreeze,
    /// An Assessment that is not part of this test cited it after the freeze
    /// and before this assessment.
    AssessedOutsideTest,
}

/// One recorded read of the same data before the test.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PriorExposure {
    pub kind: ExposureKind,
    /// The Observation or Assessment revision that read the data.
    pub record_ref: RevisionRef,
    /// The original Artifact (or, for an attributed assertion, the Observation)
    /// both reads reach.
    pub via: Id,
    pub accepted_at_sequence: u64,
}

/// Why one input has its class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputReason {
    /// A recorded read reaches the same captured data.
    ReadBeforeTest,
    /// Lineage does not reach source bytes: an attributed assertion, an
    /// unresolved or truncated derivation, or a Run without artifacts.
    Untraceable,
    /// Every original acquisition was recorded after the freeze.
    AcquiredAfterFreeze,
    /// Collected before the freeze and declared inside the reserved holdout.
    DeclaredWithinReservedHoldout,
    /// Collected before the freeze and not declared inside the reserved holdout.
    CollectedBeforeFreezeNotReserved,
}

/// Whether an input was named in `frozen_basis.actual_inputs`, cited as
/// evidence, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputListing {
    ActualInput,
    EvidenceRef,
    Both,
}

/// The reuse reading of one input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InputReuse {
    pub input_ref: RevisionRef,
    pub listed_as: InputListing,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition: Option<String>,
    pub holdout: HoldoutPlacement,
    /// Original Artifacts this input reaches through derivative lineage.
    pub origins: Vec<RevisionRef>,
    pub class: ReuseClass,
    pub reason: InputReason,
    pub prior_exposures: Vec<PriorExposure>,
    #[serde(skip_serializing_if = "is_zero")]
    pub exposures_omitted: usize,
}

/// One plan-level fact that shapes the overall class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanReason {
    /// The plan declared the investigator had seen the target result.
    DeclaredSeen,
    /// The plan declared the investigator had partly seen the target result.
    DeclaredPartlySeen,
    /// The plan declared prior exposure as unknown.
    DeclaredExposureUnknown,
    /// No input could be read.
    NoInputs,
}

/// One Run that produced an input, as its own record states its chronology.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunChronology {
    pub run_ref: RevisionRef,
    /// True when the Run's recorded plan digest is the digest of the assessed plan.
    pub binds_assessed_plan: bool,
    /// Verbatim from the Run.
    pub chronology: Value,
    /// How the chronology was established: `journal_order`,
    /// `structure_checked` or `unverified`. Never `signature_verified`.
    pub chronology_verification: Option<crate::receipt_check::ChronologyVerification>,
    /// Verbatim from the Run; `null` when none was recorded.
    pub ordering_receipt: Value,
}

/// The chronology axis: reported from the Runs, never derived from reuse.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChronologyAxis {
    pub runs: Vec<RunChronology>,
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted: usize,
    pub note: &'static str,
}

/// The reuse axis and the recorded deviations of one assessment of a frozen
/// confirmatory plan.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvidenceReuse {
    pub rule_version: &'static str,
    pub assessment_ref: RevisionRef,
    pub plan_ref: RevisionRef,
    pub plan_digest: Value,
    pub frozen_at_sequence: u64,
    /// The plan's declared prior exposure, verbatim.
    pub declared_prior_exposure: Value,
    /// The plan's reserved holdout boundary, verbatim.
    pub evidence_holdout_boundary: Value,
    /// The plan's frozen stop and analysis rule, verbatim.
    pub frozen_stop_rule: Value,
    pub stop_rule_applied: String,
    pub classification: ReuseClass,
    pub label: &'static str,
    pub counts_as_prospective: bool,
    pub plan_reasons: Vec<PlanReason>,
    pub inputs: Vec<InputReuse>,
    pub exclusions: Vec<Exclusion>,
    pub deviations: Vec<Deviation>,
    /// True when a deviation records an early stop.
    pub early_stop: bool,
    /// True when the applied stop rule's text differs from the frozen rule and
    /// no stop deviation is recorded.
    pub stop_rule_differs_without_deviation: bool,
    /// Chronology of the Runs behind the inputs, as a separate axis.
    pub chronology: ChronologyAxis,
    pub limits: Vec<&'static str>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

const LIMITS: &[&str] = &[
    "Reuse is detected only from recorded Observations and Assessments. Exposure through unrecorded reads, summaries or dashboards shows only through the plan's declared prior exposure.",
    "Lineage compares whole captured artifacts: different parts of one artifact count as the same data.",
    "Order is local journal order. Chronology is reported separately from the Runs and is never derived from this classification.",
    "The classification never rejects the assessment; it limits how its result is labeled.",
];

const CHRONOLOGY_NOTE: &str = "Reported verbatim from the Runs that produced the inputs. A separate axis: it never changes, and is never changed by, the reuse classification.";

/// The frozen basis recorded on an assessment, if any.
pub fn frozen_basis(record: &Record) -> Option<FrozenBasis> {
    if record.header.id.kind() != Kind::Assessment {
        return None;
    }
    serde_json::from_value(record.body.get("frozen_basis")?.clone()).ok()
}

fn exact(reference: &Reference) -> Option<RevisionRef> {
    match reference {
        Reference::Existing(reference) => Some(*reference),
        Reference::Client(_) => None,
    }
}

fn first_sequence(state: &State, record: &Record) -> u64 {
    state
        .history(record.header.id)
        .next()
        .map_or(record.header.acceptance_sequence, |first| {
            first.header.acceptance_sequence
        })
}

/// What a cited revision reaches: original artifacts, every artifact passed on
/// the way, and whether the walk reached source bytes.
struct Reach {
    origins: BTreeSet<RevisionRef>,
    /// Every artifact passed on the way, including derivatives.
    artifacts: BTreeSet<Id>,
    traceable: bool,
}

struct Reader<'a> {
    state: &'a State,
    case: Id,
    lineages: BTreeMap<RevisionRef, Option<derivation::Lineage>>,
}

impl<'a> Reader<'a> {
    fn lineage(&mut self, artifact: RevisionRef) -> Option<derivation::Lineage> {
        let state = self.state;
        self.lineages
            .entry(artifact)
            .or_insert_with(|| derivation::lineage(state, artifact))
            .clone()
    }

    fn artifact_reach(&mut self, artifact: RevisionRef, reach: &mut Reach) {
        reach.artifacts.insert(artifact.id);
        match self.lineage(artifact) {
            Some(lineage) => {
                if lineage.truncated || !lineage.unresolved.is_empty() || lineage.origins.is_empty()
                {
                    reach.traceable = false;
                }
                for step in &lineage.steps {
                    reach.artifacts.insert(step.artifact_ref.id);
                }
                for origin in lineage.origins {
                    reach.artifacts.insert(origin.artifact_ref.id);
                    reach.origins.insert(origin.artifact_ref);
                }
            }
            None => reach.traceable = false,
        }
    }

    /// Original artifacts a cited revision reaches. Observations reach the
    /// artifact their basis cites; Runs reach their recorded artifacts; a
    /// Source reaches the original artifacts of its acquisition.
    fn reach(&mut self, reference: RevisionRef) -> Reach {
        let mut reach = Reach {
            origins: BTreeSet::new(),
            artifacts: BTreeSet::new(),
            traceable: true,
        };
        let Some(record) = self.state.revision(reference) else {
            reach.traceable = false;
            return reach;
        };
        match reference.id.kind() {
            Kind::Observation => {
                match serde_json::from_value::<RevisionRef>(
                    record.body["basis"]["artifact_ref"].clone(),
                ) {
                    Ok(artifact) => self.artifact_reach(artifact, &mut reach),
                    Err(_) => reach.traceable = false,
                }
            }
            Kind::Artifact => self.artifact_reach(reference, &mut reach),
            Kind::Run => {
                let artifacts: Vec<RevisionRef> =
                    serde_json::from_value(record.body["artifact_refs"].clone())
                        .unwrap_or_default();
                if artifacts.is_empty() {
                    reach.traceable = false;
                }
                for artifact in artifacts {
                    self.artifact_reach(artifact, &mut reach);
                }
            }
            Kind::Source => {
                let originals: Vec<RevisionRef> = self
                    .state
                    .records()
                    .filter(|candidate| {
                        candidate.header.case_id == self.case
                            && candidate.header.id.kind() == Kind::Artifact
                            && candidate.body["source"]["id"] == json!(reference.id)
                    })
                    .map(Record::reference)
                    .filter(|artifact| {
                        derivation::artifact_body(self.state, *artifact)
                            .is_some_and(|body| body.role() == ArtifactRole::Original)
                    })
                    .collect();
                if originals.is_empty() {
                    reach.traceable = false;
                }
                for artifact in originals {
                    self.artifact_reach(artifact, &mut reach);
                }
            }
            _ => reach.traceable = false,
        }
        reach
    }

    fn citations(record: &Record) -> Vec<RevisionRef> {
        let mut out: Vec<RevisionRef> =
            serde_json::from_value(record.body["evidence_refs"].clone()).unwrap_or_default();
        if let Some(basis) = frozen_basis(record) {
            out.extend(
                basis
                    .actual_inputs
                    .iter()
                    .filter_map(|input| exact(&input.input_ref)),
            );
        }
        out
    }

    /// Every recorded read that counts as exposure for a test of `plan`
    /// frozen at `freeze`, assessed by `this` accepted at `cutoff`. Keyed by
    /// the original artifact (or Observation) both reads reach.
    fn exposures(
        &mut self,
        plan: Id,
        this: Id,
        freeze: u64,
        cutoff: u64,
    ) -> BTreeMap<Id, BTreeSet<PriorExposure>> {
        let mut out: BTreeMap<Id, BTreeSet<PriorExposure>> = BTreeMap::new();
        let candidates: Vec<Record> = self
            .state
            .records()
            .filter(|record| {
                record.header.case_id == self.case
                    && record.header.id != this
                    && matches!(
                        record.header.id.kind(),
                        Kind::Observation | Kind::Assessment
                    )
            })
            .flat_map(|record| {
                self.state
                    .history(record.header.id)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .collect();
        for record in candidates {
            let sequence = record.header.acceptance_sequence;
            let kind = match record.header.id.kind() {
                Kind::Observation if sequence < freeze => ExposureKind::ObservedBeforeFreeze,
                Kind::Assessment if sequence < freeze => ExposureKind::AssessedBeforeFreeze,
                Kind::Assessment
                    if sequence < cutoff
                        && frozen_basis(&record)
                            .and_then(|basis| exact(&basis.plan_ref))
                            .is_none_or(|other| other.id != plan) =>
                {
                    ExposureKind::AssessedOutsideTest
                }
                _ => continue,
            };
            let read = if kind == ExposureKind::ObservedBeforeFreeze {
                vec![record.reference()]
            } else {
                Self::citations(&record)
            };
            for cited in read {
                let reach = self.reach(cited);
                let exposure = |via| PriorExposure {
                    kind,
                    record_ref: record.reference(),
                    via,
                    accepted_at_sequence: sequence,
                };
                if cited.id.kind() == Kind::Observation {
                    out.entry(cited.id).or_default().insert(exposure(cited.id));
                }
                for origin in reach.origins {
                    out.entry(origin.id)
                        .or_default()
                        .insert(exposure(origin.id));
                }
            }
        }
        out
    }

    fn runs_for(&self, input: RevisionRef, reach: &Reach) -> BTreeSet<Id> {
        let mut runs = BTreeSet::new();
        if input.id.kind() == Kind::Run {
            runs.insert(input.id);
        }
        if let Some(record) = self.state.revision(input)
            && let Ok(run) = serde_json::from_value::<RevisionRef>(record.body["run_ref"].clone())
        {
            runs.insert(run.id);
        }
        for record in self.state.records().filter(|record| {
            record.header.case_id == self.case && record.header.id.kind() == Kind::Run
        }) {
            let artifacts: Vec<RevisionRef> =
                serde_json::from_value(record.body["artifact_refs"].clone()).unwrap_or_default();
            if artifacts
                .iter()
                .any(|artifact| reach.artifacts.contains(&artifact.id))
            {
                runs.insert(record.header.id);
            }
        }
        runs
    }
}

/// The reuse classification, deviations and chronology of one assessment of
/// a frozen confirmatory plan, or `None` for any other record. A pure read of
/// committed state: it writes nothing and never fails a request.
pub fn evidence_reuse(state: &State, assessment: &Record) -> Option<EvidenceReuse> {
    let basis = frozen_basis(assessment)?;
    let plan_ref = exact(&basis.plan_ref)?;
    let plan = state.revision(plan_ref)?;
    let confirmatory = &plan.body["inquiry"]["confirmatory"];
    let freeze = plan.header.acceptance_sequence;
    let cutoff = first_sequence(state, assessment);
    let mut reader = Reader {
        state,
        case: assessment.header.case_id,
        lineages: BTreeMap::new(),
    };
    let exposures = reader.exposures(plan_ref.id, assessment.header.id, freeze, cutoff);

    // Actual inputs first, then cited evidence not already listed.
    let mut listed: Vec<(RevisionRef, InputListing, Option<String>, HoldoutPlacement)> = Vec::new();
    for input in &basis.actual_inputs {
        if let Some(reference) = exact(&input.input_ref) {
            listed.push((
                reference,
                InputListing::ActualInput,
                input.partition.clone(),
                input.holdout,
            ));
        }
    }
    let cited: Vec<RevisionRef> =
        serde_json::from_value(assessment.body["evidence_refs"].clone()).unwrap_or_default();
    for reference in cited
        .into_iter()
        .filter(|reference| INPUT_KINDS.contains(&reference.id.kind()))
    {
        match listed.iter_mut().find(|item| item.0.id == reference.id) {
            Some(item) => item.1 = InputListing::Both,
            None => listed.push((
                reference,
                InputListing::EvidenceRef,
                None,
                HoldoutPlacement::NotDeclared,
            )),
        }
    }

    let mut inputs = Vec::new();
    let mut runs = BTreeSet::new();
    for (input_ref, listed_as, partition, holdout) in listed {
        let reach = reader.reach(input_ref);
        runs.extend(reader.runs_for(input_ref, &reach));
        let mut found: BTreeSet<PriorExposure> = BTreeSet::new();
        if input_ref.id.kind() == Kind::Observation
            && let Some(own) = exposures.get(&input_ref.id)
        {
            found.extend(own.iter().cloned());
        }
        for origin in &reach.origins {
            if let Some(reads) = exposures.get(&origin.id) {
                found.extend(reads.iter().cloned());
            }
        }
        let acquired_after = reach.origins.iter().all(|origin| {
            state
                .history(origin.id)
                .next()
                .is_some_and(|first| first.header.acceptance_sequence > freeze)
        });
        let (class, reason) = if !found.is_empty() {
            (ReuseClass::KnownOutcomeReuse, InputReason::ReadBeforeTest)
        } else if !reach.traceable || reach.origins.is_empty() {
            (ReuseClass::Undetermined, InputReason::Untraceable)
        } else if acquired_after {
            (ReuseClass::Fresh, InputReason::AcquiredAfterFreeze)
        } else if holdout == HoldoutPlacement::WithinReserved {
            (
                ReuseClass::HeldOutWithinDeclaredLimits,
                InputReason::DeclaredWithinReservedHoldout,
            )
        } else {
            (
                ReuseClass::Undetermined,
                InputReason::CollectedBeforeFreezeNotReserved,
            )
        };
        let mut found: Vec<PriorExposure> = found.into_iter().collect();
        found.sort_by_key(|exposure| {
            (
                exposure.accepted_at_sequence,
                exposure.record_ref,
                exposure.via,
            )
        });
        let omitted = found.len().saturating_sub(MAX_EXPOSURES_PER_INPUT);
        found.truncate(MAX_EXPOSURES_PER_INPUT);
        inputs.push(InputReuse {
            input_ref,
            listed_as,
            partition,
            holdout,
            origins: reach.origins.into_iter().collect(),
            class,
            reason,
            prior_exposures: found,
            exposures_omitted: omitted,
        });
    }

    let mut plan_reasons = Vec::new();
    let declared = plan
        .body
        .get("prior_exposure")
        .cloned()
        .unwrap_or_else(|| confirmatory["prior_exposure"].clone());
    match declared.as_str() {
        Some("seen") => plan_reasons.push(PlanReason::DeclaredSeen),
        Some("partly_seen") => plan_reasons.push(PlanReason::DeclaredPartlySeen),
        Some("unknown") => plan_reasons.push(PlanReason::DeclaredExposureUnknown),
        _ => {}
    }
    if inputs.is_empty() {
        plan_reasons.push(PlanReason::NoInputs);
    }
    let any = |class| inputs.iter().any(|input| input.class == class);
    let classification = if any(ReuseClass::KnownOutcomeReuse)
        || plan_reasons.iter().any(|reason| {
            matches!(
                reason,
                PlanReason::DeclaredSeen | PlanReason::DeclaredPartlySeen
            )
        }) {
        ReuseClass::KnownOutcomeReuse
    } else if inputs.is_empty()
        || any(ReuseClass::Undetermined)
        || plan_reasons.contains(&PlanReason::DeclaredExposureUnknown)
    {
        ReuseClass::Undetermined
    } else if any(ReuseClass::HeldOutWithinDeclaredLimits) {
        ReuseClass::HeldOutWithinDeclaredLimits
    } else {
        ReuseClass::Fresh
    };

    let plan_digest = plan.body["plan_digest"].clone();
    let mut chronology: Vec<RunChronology> = runs
        .into_iter()
        .filter_map(|id| state.current(id))
        .map(|run| RunChronology {
            run_ref: run.reference(),
            binds_assessed_plan: !plan_digest.is_null() && run.body["plan_digest"] == plan_digest,
            chronology: run.body.get("chronology").cloned().unwrap_or(Value::Null),
            chronology_verification: crate::receipt_check::chronology_verification(&run.body),
            ordering_receipt: run
                .body
                .get("ordering_receipt")
                .cloned()
                .unwrap_or(Value::Null),
        })
        .collect();
    let omitted = chronology.len().saturating_sub(MAX_CHRONOLOGY_RUNS);
    chronology.truncate(MAX_CHRONOLOGY_RUNS);

    let frozen_stop_rule = confirmatory["stop_analysis_rule"].clone();
    let stop_deviation = basis.deviations.iter().any(|d| d.kind.is_stop());
    let stop_rule_differs_without_deviation = !stop_deviation
        && frozen_stop_rule.as_str().map(str::trim) != Some(basis.stop_rule_applied.trim());
    Some(EvidenceReuse {
        rule_version: RULE_VERSION,
        assessment_ref: assessment.reference(),
        plan_ref,
        plan_digest,
        frozen_at_sequence: freeze,
        declared_prior_exposure: declared,
        evidence_holdout_boundary: confirmatory["evidence_holdout_boundary"].clone(),
        frozen_stop_rule,
        stop_rule_applied: basis.stop_rule_applied.clone(),
        classification,
        label: classification.label(),
        counts_as_prospective: classification.counts_as_prospective(),
        plan_reasons,
        inputs,
        early_stop: basis
            .deviations
            .iter()
            .any(|deviation| deviation.kind == DeviationKind::EarlyStop),
        stop_rule_differs_without_deviation,
        exclusions: basis.exclusions,
        deviations: basis.deviations,
        chronology: ChronologyAxis {
            runs: chronology,
            omitted,
            note: CHRONOLOGY_NOTE,
        },
        limits: LIMITS.to_vec(),
    })
}

/// Receipt warnings for an assessment just written: the reuse class when the
/// result is not a prospective test, each deviation, and an applied stop rule
/// that differs from the frozen one without a recorded stop deviation. Never
/// fails and never changes the record.
pub fn warn_recorded(context: &mut OperationContext<'_>, reference: RevisionRef) {
    let Some(reuse) = context
        .exact(reference)
        .and_then(|record| evidence_reuse(context.state(), record))
    else {
        return;
    };
    let mut warnings = Vec::new();
    if !reuse.classification.counts_as_prospective() {
        warnings.push(format!(
            "assessment.evidence_reuse {}: {} It does not block recording.",
            reuse.classification.as_str(),
            reuse.label
        ));
    }
    for deviation in &reuse.deviations {
        warnings.push(format!(
            "assessment.deviation {}: the assessment departs from frozen plan {}.",
            serde_json::to_value(deviation.kind)
                .ok()
                .and_then(|kind| kind.as_str().map(str::to_owned))
                .unwrap_or_default(),
            reuse.plan_ref.id
        ));
    }
    if reuse.stop_rule_differs_without_deviation {
        warnings.push(
            "assessment.stop_rule_differs_without_deviation: the applied stop rule differs from the frozen rule and no early_stop or late_stop deviation is recorded."
                .into(),
        );
    }
    for warning in warnings {
        context.warn(warning);
    }
}

#[cfg(test)]
mod tests;
