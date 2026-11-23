//! Atomic evaluative operations. All canonical references are exact, case-local revisions.
use crate::{
    catalog::{OperationDescriptor, OperationInfo},
    command::{Reference, RevisionRef},
    evaluate::{
        ComparisonBasis, ComparisonOutcome, CriterionCreate, CriterionKind, CriterionLifecycle,
        CriterionScale, CriterionScope, OptionCreate, OptionLifecycle, PreferredDirection,
        Threshold, ThresholdOperator,
    },
    identity::{Id, Kind},
    input::Measurement,
    state::OperationContext,
    wire::{ErrorCode, Failure},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cmp::Ordering;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OptionBody {
    pub question_ref: RevisionRef,
    pub description: String,
    pub context: Option<String>,
    pub feasibility: Option<String>,
    pub dependencies: Vec<String>,
    pub tradeoff_notes: Vec<String>,
    pub lifecycle: OptionLifecycle,
    pub disposition_reason: Option<String>,
    pub amendment_reason: Option<String>,
    pub criterion_ref: Option<RevisionRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OptionAmend {
    pub option_ref: RevisionRef,
    pub reason: String,
    pub description: String,
    pub context: Option<String>,
    pub feasibility: Option<String>,
    pub dependencies: Vec<String>,
    pub tradeoff_notes: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OptionDispositionInput {
    pub option_ref: RevisionRef,
    pub lifecycle: OptionLifecycle,
    pub reason: String,
    pub criterion_ref: Option<Reference>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CriterionBody {
    pub name: String,
    pub kind: CriterionKind,
    pub question_ref: RevisionRef,
    pub option_refs: Option<Vec<RevisionRef>>,
    pub scale: CriterionScale,
    pub preferred_direction: PreferredDirection,
    pub threshold: Option<Threshold>,
    pub weighting_rationale: Option<String>,
    pub lifecycle: CriterionLifecycle,
    pub change_reason: Option<String>,
    pub material_change: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CriterionAmend {
    pub criterion_ref: RevisionRef,
    pub reason: String,
    pub replacement: CriterionCreate,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CriterionRetire {
    pub criterion_ref: RevisionRef,
    pub reason: String,
}
/// Bare IDs preserve the plan's example; they are resolved to current revisions before storage.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum EvidenceInput {
    Id(Id),
    Ref(Reference),
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparisonInput {
    pub option_ref: Reference,
    pub criterion_ref: Reference,
    pub outcome: ComparisonOutcome,
    pub method: String,
    pub value: Option<Measurement>,
    pub basis: Option<ComparisonBasis>,
    pub evidence_refs: Vec<EvidenceInput>,
    pub assumption_refs: Vec<EvidenceInput>,
    pub limitations: Option<String>,
    pub uncertainty: String,
    pub rationale: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparisonBody {
    pub option_ref: RevisionRef,
    pub criterion_ref: RevisionRef,
    pub outcome: ComparisonOutcome,
    pub method: String,
    pub value: Option<Measurement>,
    pub basis: Option<ComparisonBasis>,
    pub evidence_refs: Vec<RevisionRef>,
    pub assumption_refs: Vec<RevisionRef>,
    pub limitations: Option<String>,
    pub uncertainty: String,
    pub rationale: String,
    pub retracted: bool,
    pub change_reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparisonRevise {
    pub comparison_ref: RevisionRef,
    pub reason: String,
    pub replacement: ComparisonInput,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparisonRetract {
    pub comparison_ref: RevisionRef,
    pub reason: String,
}

fn invalid(path: &str, message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(path)
}
fn text(s: &str, path: &str) -> Result<(), Failure> {
    if s.trim().is_empty() || s.chars().count() > 2048 {
        Err(invalid(
            path,
            "Supply nonempty text of at most 2048 characters.",
        ))
    } else {
        Ok(())
    }
}
fn body<T: Serialize>(value: T) -> Result<Value, Failure> {
    serde_json::to_value(value)
        .map_err(|_| Failure::new(ErrorCode::IntegrityFailure, "Cannot encode record."))
}
fn current(
    ctx: &OperationContext<'_>,
    r: RevisionRef,
    kind: Kind,
    path: &str,
) -> Result<(), Failure> {
    if r.id.kind() != kind {
        return Err(invalid(path, "Unexpected record kind."));
    }
    let record = ctx.current(r.id).ok_or_else(|| {
        Failure::new(
            ErrorCode::UnknownReference,
            "Reference is not in this case.",
        )
        .at(path)
    })?;
    if record.header.revision.0 != r.revision {
        let mut failure = Failure::new(ErrorCode::RevisionConflict, "Reference is stale.").at(path);
        failure.error.actual = Some(json!({"id":r.id,"revision":record.header.revision.0}));
        return Err(failure);
    }
    Ok(())
}
fn resolve(
    ctx: &OperationContext<'_>,
    r: &Reference,
    kind: Kind,
    path: &str,
) -> Result<RevisionRef, Failure> {
    let id = ctx.resolve(r, kind, path)?;
    current(ctx, id, kind, path)?;
    Ok(id)
}
fn load<T: serde::de::DeserializeOwned>(
    ctx: &OperationContext<'_>,
    r: RevisionRef,
    kind: Kind,
    path: &str,
) -> Result<T, Failure> {
    current(ctx, r, kind, path)?;
    serde_json::from_value(ctx.current(r.id).unwrap().body.clone()).map_err(|_| {
        Failure::new(
            ErrorCode::IntegrityFailure,
            "Stored record has an invalid body.",
        )
    })
}
fn normalize_option(ctx: &OperationContext<'_>, p: OptionCreate) -> Result<OptionBody, Failure> {
    text(&p.description, "/description")?;
    Ok(OptionBody {
        question_ref: resolve(ctx, &p.question_ref, Kind::Question, "/question_ref")?,
        description: p.description,
        context: p.context,
        feasibility: p.feasibility,
        dependencies: p.dependencies,
        tradeoff_notes: p.tradeoff_notes,
        lifecycle: OptionLifecycle::Proposed,
        disposition_reason: None,
        amendment_reason: None,
        criterion_ref: None,
    })
}
fn normalize_criterion(
    ctx: &OperationContext<'_>,
    p: CriterionCreate,
) -> Result<CriterionBody, Failure> {
    text(&p.name, "/name")?;
    text(&p.scale.description, "/scale/description")?;
    for a in &p.scale.anchors {
        text(&a.label, "/scale/anchors")?;
        text(&a.meaning, "/scale/anchors")?;
    }
    if p.kind == CriterionKind::Hard && p.threshold.is_none() {
        return Err(invalid(
            "/threshold",
            "A hard criterion requires a checkable threshold.",
        ));
    }
    if let Some(t) = &p.threshold {
        match t {
            Threshold::Measured { value, .. } => {
                if p.scale.unit.as_deref() != Some(value.unit.as_str())
                    || value.unit.trim().is_empty()
                {
                    return Err(invalid(
                        "/threshold/value/unit",
                        "Threshold unit must match the declared scale unit.",
                    ));
                }
            }
            Threshold::Qualitative { requirement } => text(requirement, "/threshold/requirement")?,
        }
    }
    let (question_ref, option_refs) = match &p.scope {
        CriterionScope::AllOptions { question_ref } => (
            resolve(ctx, question_ref, Kind::Question, "/scope/question_ref")?,
            None,
        ),
        CriterionScope::SelectedOptions {
            question_ref,
            option_refs,
        } => {
            let q = resolve(ctx, question_ref, Kind::Question, "/scope/question_ref")?;
            let mut refs = Vec::new();
            for r in option_refs {
                let r = resolve(ctx, r, Kind::Option, "/scope/option_refs")?;
                let opt: OptionBody = load(ctx, r, Kind::Option, "/scope/option_refs")?;
                if opt.question_ref.id != q.id {
                    return Err(invalid(
                        "/scope/option_refs",
                        "Option belongs to another question.",
                    ));
                }
                if refs.contains(&r) {
                    return Err(invalid("/scope/option_refs", "Duplicate option."));
                }
                refs.push(r);
            }
            (q, Some(refs))
        }
    };
    Ok(CriterionBody {
        name: p.name,
        kind: p.kind,
        question_ref,
        option_refs,
        scale: p.scale,
        preferred_direction: p.preferred_direction,
        threshold: p.threshold,
        weighting_rationale: p.weighting_rationale,
        lifecycle: CriterionLifecycle::Active,
        change_reason: None,
        material_change: false,
    })
}
fn resolve_inputs(
    ctx: &OperationContext<'_>,
    values: &[EvidenceInput],
    kinds: &[Kind],
    path: &str,
) -> Result<Vec<RevisionRef>, Failure> {
    values
        .iter()
        .map(|v| {
            let r = match v {
                EvidenceInput::Id(id) => {
                    ctx.current(*id).map(|r| r.reference()).ok_or_else(|| {
                        Failure::new(
                            ErrorCode::UnknownReference,
                            "Reference is not in this case.",
                        )
                        .at(path)
                    })?
                }
                EvidenceInput::Ref(Reference::Existing(r)) => *r,
                EvidenceInput::Ref(r @ Reference::Client(_)) => {
                    let mut found = None;
                    for kind in kinds {
                        if let Ok(v) = ctx.resolve(r, *kind, path) {
                            found = Some(v);
                            break;
                        }
                    }
                    found.ok_or_else(|| {
                        Failure::new(ErrorCode::UnknownReference, "Unknown client reference.")
                            .at(path)
                    })?
                }
            };
            if !kinds.contains(&r.id.kind()) {
                return Err(invalid(path, "Unexpected reference kind."));
            }
            current(ctx, r, r.id.kind(), path)?;
            Ok(r)
        })
        .collect()
}
fn compare(ctx: &OperationContext<'_>, p: ComparisonInput) -> Result<ComparisonBody, Failure> {
    text(&p.method, "/method")?;
    text(&p.uncertainty, "/uncertainty")?;
    text(&p.rationale, "/rationale")?;
    let option_ref = resolve(ctx, &p.option_ref, Kind::Option, "/option_ref")?;
    let criterion_ref = resolve(ctx, &p.criterion_ref, Kind::Criterion, "/criterion_ref")?;
    let option: OptionBody = load(ctx, option_ref, Kind::Option, "/option_ref")?;
    let criterion: CriterionBody = load(ctx, criterion_ref, Kind::Criterion, "/criterion_ref")?;
    if criterion.lifecycle != CriterionLifecycle::Active
        || option.question_ref.id != criterion.question_ref.id
        || criterion
            .option_refs
            .as_ref()
            .is_some_and(|v| !v.iter().any(|r| r.id == option_ref.id))
    {
        return Err(invalid(
            "/criterion_ref",
            "Criterion does not apply to this option and question.",
        ));
    }
    if p.evidence_refs.is_empty()
        && p.assumption_refs.is_empty()
        && !matches!(
            p.outcome,
            ComparisonOutcome::Unknown | ComparisonOutcome::NotApplicable
        )
    {
        return Err(invalid(
            "/evidence_refs",
            "Supply evidence or an explicit assumption.",
        ));
    }
    if p.value.is_some() && p.basis.is_some() {
        return Err(invalid(
            "/basis",
            "Choose measured or qualitative basis, not both.",
        ));
    }
    if p.outcome == ComparisonOutcome::Unknown
        && (p.value.is_some()
            || matches!(
                p.basis,
                Some(ComparisonBasis::Preference { .. } | ComparisonBasis::Qualitative { .. })
            ))
    {
        return Err(invalid(
            "/outcome",
            "Unknown must not be presented as a measured or assessed result.",
        ));
    }
    if p.outcome != ComparisonOutcome::Unknown
        && p.outcome != ComparisonOutcome::NotApplicable
        && p.value.is_none()
        && p.basis.is_none()
    {
        return Err(invalid(
            "/basis",
            "A result requires a measured or qualitative basis.",
        ));
    }
    if criterion.kind == CriterionKind::Hard
        && matches!(criterion.threshold, Some(Threshold::Measured { .. }))
        && matches!(
            p.outcome,
            ComparisonOutcome::Meets | ComparisonOutcome::Fails | ComparisonOutcome::PartiallyMeets
        )
        && p.value.is_none()
    {
        return Err(invalid(
            "/value",
            "An unmeasured hard threshold cannot be treated as met or failed.",
        ));
    }
    if let Some(value) = &p.value {
        if criterion.scale.unit.as_deref() != Some(value.unit.as_str()) {
            return Err(invalid(
                "/value/unit",
                "Measurement unit must match the declared criterion unit.",
            ));
        }
        if criterion.kind == CriterionKind::Hard
            && let Some(Threshold::Measured {
                operator,
                value: limit,
            }) = &criterion.threshold
        {
            let cmp = value.decimal.numeric_cmp(&limit.decimal);
            let meets = match operator {
                ThresholdOperator::LessThan => cmp == Ordering::Less,
                ThresholdOperator::AtMost => cmp != Ordering::Greater,
                ThresholdOperator::Equal => cmp == Ordering::Equal,
                ThresholdOperator::AtLeast => cmp != Ordering::Less,
                ThresholdOperator::GreaterThan => cmp == Ordering::Greater,
            };
            let expected = if meets {
                ComparisonOutcome::Meets
            } else {
                ComparisonOutcome::Fails
            };
            if p.outcome != expected {
                return Err(invalid(
                    "/outcome",
                    "Outcome conflicts with the hard threshold.",
                ));
            }
        }
    }
    if matches!(p.basis, Some(ComparisonBasis::Unknown { .. }))
        && p.outcome != ComparisonOutcome::Unknown
    {
        return Err(invalid(
            "/outcome",
            "Unknown basis requires unknown outcome.",
        ));
    }
    Ok(ComparisonBody {
        option_ref,
        criterion_ref,
        outcome: p.outcome,
        method: p.method,
        value: p.value,
        basis: p.basis,
        evidence_refs: resolve_inputs(
            ctx,
            &p.evidence_refs,
            &[Kind::Observation, Kind::Source, Kind::Artifact],
            "/evidence_refs",
        )?,
        assumption_refs: resolve_inputs(
            ctx,
            &p.assumption_refs,
            &[Kind::Assumption],
            "/assumption_refs",
        )?,
        limitations: p.limitations,
        uncertainty: p.uncertainty,
        rationale: p.rationale,
        retracted: false,
        change_reason: None,
    })
}

fn info(name: &'static str, summary: &'static str) -> OperationInfo {
    OperationInfo {
        name,
        stage: "evaluate",
        summary,
        when_to_use: "Record an evaluative choice and its basis.",
        lifecycle: "A case and decision question exist.",
        effects: "Record an attributed revision.",
        returns: "Created or updated record revision.",
        revision_rule: "Existing records require their current exact revision.",
        idempotency: "Retry the same command ID and request.",
        repair: "Inspect the current record and schema.",
        related: &[],
    }
}
fn ok<T>(_: &T) -> Result<(), Failure> {
    Ok(())
}
pub(crate) fn operations() -> Vec<OperationDescriptor> {
    let q = json!({"id":"q_01890f3e-7c00-7000-8000-000000000001","revision":1});
    let opt = json!({"id":"opt_01890f3e-7c00-7000-8000-000000000002","revision":1});
    let crit = json!({"id":"crit_01890f3e-7c00-7000-8000-000000000003","revision":1});
    let cmp = json!({"id":"cmp_01890f3e-7c00-7000-8000-000000000004","revision":1});
    let option = json!({"question_ref":q,"description":"Keep the current design","dependencies":[],"tradeoff_notes":[]});
    let criterion = json!({"name":"Latency","kind":"hard","scope":{"kind":"all_options","question_ref":q},"scale":{"unit":"ms","description":"p99 latency","anchors":[]},"preferred_direction":"lower","threshold":{"kind":"measured","operator":"at_most","value":{"decimal":"25","unit":"ms"}}});
    let comparison = json!({"option_ref":opt,"criterion_ref":crit,"outcome":"unknown","method":"inspection","basis":{"kind":"unknown","reason":"not measured"},"evidence_refs":["obs_01890f3e-7c00-7000-8000-000000000005"],"assumption_refs":[],"uncertainty":"No measurement yet","rationale":"Await measurement"});
    vec![
        OperationDescriptor::active(
            info("option.create", "Record a candidate option."),
            option,
            ok,
            |p: OptionCreate, c| {
                let b = normalize_option(c, p)?;
                c.create(Kind::Option, body(b)?)?;
                Ok(())
            },
        ),
        OperationDescriptor::active(
            info("option.amend", "Amend an option."),
            json!({"option_ref":opt,"reason":"Clarify scope","description":"Keep current design","dependencies":[],"tradeoff_notes":[]}),
            ok,
            |p: OptionAmend, c| {
                text(&p.reason, "/reason")?;
                text(&p.description, "/description")?;
                let mut b: OptionBody = load(c, p.option_ref, Kind::Option, "/option_ref")?;
                b.description = p.description;
                b.context = p.context;
                b.feasibility = p.feasibility;
                b.dependencies = p.dependencies;
                b.tradeoff_notes = p.tradeoff_notes;
                b.amendment_reason = Some(p.reason);
                c.update(p.option_ref, body(b)?)?;
                Ok(())
            },
        ),
        OperationDescriptor::active(
            info("option.disposition", "Record an option disposition."),
            json!({"option_ref":opt,"lifecycle":"eligible","reason":"Feasible"}),
            ok,
            |p: OptionDispositionInput, c| {
                text(&p.reason, "/reason")?;
                if p.lifecycle == OptionLifecycle::Proposed {
                    return Err(invalid("/lifecycle", "Proposed is the initial lifecycle."));
                }
                let mut b: OptionBody = load(c, p.option_ref, Kind::Option, "/option_ref")?;
                if matches!(
                    b.lifecycle,
                    OptionLifecycle::Withdrawn | OptionLifecycle::Superseded
                ) {
                    return Err(invalid("/lifecycle", "This option is terminal."));
                }
                let criterion = p
                    .criterion_ref
                    .as_ref()
                    .map(|r| resolve(c, r, Kind::Criterion, "/criterion_ref"))
                    .transpose()?;
                if p.lifecycle == OptionLifecycle::RuledOut && criterion.is_none() {
                    return Err(invalid(
                        "/criterion_ref",
                        "Ruling out an option requires a criterion.",
                    ));
                }
                if let Some(r) = criterion {
                    let cb: CriterionBody = load(c, r, Kind::Criterion, "/criterion_ref")?;
                    if cb.lifecycle != CriterionLifecycle::Active
                        || cb.question_ref.id != b.question_ref.id
                        || cb
                            .option_refs
                            .as_ref()
                            .is_some_and(|refs| !refs.iter().any(|v| v.id == p.option_ref.id))
                    {
                        return Err(invalid(
                            "/criterion_ref",
                            "Criterion does not apply to this option.",
                        ));
                    }
                }
                b.lifecycle = p.lifecycle;
                b.disposition_reason = Some(p.reason);
                b.criterion_ref = criterion;
                c.update(p.option_ref, body(b)?)?;
                Ok(())
            },
        ),
        OperationDescriptor::active(
            info("criterion.create", "Define a criterion."),
            criterion.clone(),
            ok,
            |p: CriterionCreate, c| {
                let b = normalize_criterion(c, p)?;
                c.create(Kind::Criterion, body(b)?)?;
                Ok(())
            },
        ),
        OperationDescriptor::active(
            info("criterion.amend", "Amend a criterion."),
            json!({"criterion_ref":crit,"reason":"Update limit","replacement":criterion}),
            ok,
            |p: CriterionAmend, c| {
                text(&p.reason, "/reason")?;
                let old: CriterionBody =
                    load(c, p.criterion_ref, Kind::Criterion, "/criterion_ref")?;
                if old.lifecycle == CriterionLifecycle::Retired {
                    return Err(invalid(
                        "/criterion_ref",
                        "Retired criterion cannot be amended.",
                    ));
                }
                let mut next = normalize_criterion(c, p.replacement)?;
                next.material_change = old.kind != next.kind
                    || old.question_ref != next.question_ref
                    || old.option_refs != next.option_refs
                    || old.scale.unit != next.scale.unit
                    || old.preferred_direction != next.preferred_direction
                    || old.threshold != next.threshold;
                next.change_reason = Some(p.reason);
                c.update(p.criterion_ref, body(next)?)?;
                Ok(())
            },
        ),
        OperationDescriptor::active(
            info("criterion.retire", "Retire a criterion."),
            json!({"criterion_ref":crit,"reason":"No longer applicable"}),
            ok,
            |p: CriterionRetire, c| {
                text(&p.reason, "/reason")?;
                let mut b: CriterionBody =
                    load(c, p.criterion_ref, Kind::Criterion, "/criterion_ref")?;
                if b.lifecycle == CriterionLifecycle::Retired {
                    return Err(invalid("/criterion_ref", "Criterion already retired."));
                }
                b.lifecycle = CriterionLifecycle::Retired;
                b.change_reason = Some(p.reason);
                b.material_change = true;
                c.update(p.criterion_ref, body(b)?)?;
                Ok(())
            },
        ),
        OperationDescriptor::active(
            info("comparison.add", "Compare an option against a criterion."),
            comparison.clone(),
            ok,
            |p: ComparisonInput, c| {
                let b = compare(c, p)?;
                let created = c.create(Kind::Comparison, body(b)?)?;
                crate::measurement_flags::warn_recorded(c, created);
                Ok(())
            },
        ),
        OperationDescriptor::active(
            info("comparison.revise", "Revise a comparison."),
            json!({"comparison_ref":cmp,"reason":"New evidence","replacement":comparison}),
            ok,
            |p: ComparisonRevise, c| {
                text(&p.reason, "/reason")?;
                let old: ComparisonBody =
                    load(c, p.comparison_ref, Kind::Comparison, "/comparison_ref")?;
                if old.retracted {
                    return Err(invalid(
                        "/comparison_ref",
                        "Retracted comparison cannot be revised.",
                    ));
                }
                let mut next = compare(c, p.replacement)?;
                if next.option_ref.id != old.option_ref.id
                    || next.criterion_ref.id != old.criterion_ref.id
                {
                    return Err(invalid(
                        "/replacement",
                        "Revision must retain the same option and criterion.",
                    ));
                }
                next.change_reason = Some(p.reason);
                let revised = c.update(p.comparison_ref, body(next)?)?;
                crate::measurement_flags::warn_recorded(c, revised);
                Ok(())
            },
        ),
        OperationDescriptor::active(
            info("comparison.retract", "Retract a comparison."),
            json!({"comparison_ref":cmp,"reason":"Measurement invalid"}),
            ok,
            |p: ComparisonRetract, c| {
                text(&p.reason, "/reason")?;
                let mut b: ComparisonBody =
                    load(c, p.comparison_ref, Kind::Comparison, "/comparison_ref")?;
                if b.retracted {
                    return Err(invalid("/comparison_ref", "Comparison already retracted."));
                }
                b.retracted = true;
                b.change_reason = Some(p.reason);
                c.update(p.comparison_ref, body(b)?)?;
                Ok(())
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::Catalog,
        command::Command,
        header::{Clock, UtcTimestamp},
        identity::IdSource,
        state::{Acceptance, State, validate_and_apply},
    };
    use uuid::Uuid;
    struct Ids(u64);
    impl IdSource for Ids {
        fn next_uuid(&mut self) -> Uuid {
            self.0 += 1;
            format!("018d87fc-5234-7000-8000-{:012x}", self.0)
                .parse()
                .unwrap()
        }
    }
    struct Time;
    impl Clock for Time {
        fn now_utc(&mut self) -> UtcTimestamp {
            "2026-11-22T08:12:44.000Z".parse().unwrap()
        }
    }
    fn catalog() -> Catalog {
        let mut ops = operations();
        for (name, kind) in [
            ("case.create", Kind::Case),
            ("test.question", Kind::Question),
            ("test.observation", Kind::Observation),
        ] {
            ops.push(OperationDescriptor::active(
                info(name, "Test fixture."),
                json!({}),
                ok,
                move_handler(kind),
            ));
        }
        Catalog::new(ops).unwrap()
    }
    fn move_handler(kind: Kind) -> fn(Value, &mut OperationContext<'_>) -> Result<(), Failure> {
        match kind {
            Kind::Case => |v, c| {
                c.create(Kind::Case, v)?;
                Ok(())
            },
            Kind::Question => |v, c| {
                c.create(Kind::Question, v)?;
                Ok(())
            },
            _ => |v, c| {
                c.create(Kind::Observation, v)?;
                Ok(())
            },
        }
    }
    fn cmd(case: Option<Id>, ops: Value) -> Command {
        serde_json::from_value(json!({"schema_version":"inv.command.v0-alpha","command_id":"test","case_id":case,"actor":"agent:test","preconditions":[],"operations":ops})).unwrap()
    }
    fn run(
        s: &mut State,
        c: &Catalog,
        ids: &mut Ids,
        command: Command,
    ) -> Result<Vec<RevisionRef>, Failure> {
        let accepted = validate_and_apply(
            s,
            &command,
            c,
            Acceptance {
                sequence: s.sequence() + 1,
                clock: &mut Time,
                ids,
            },
        )?;
        let refs = accepted
            .created
            .iter()
            .map(|r| RevisionRef {
                id: r.id,
                revision: r.revision,
            })
            .collect();
        s.apply(&accepted)?;
        Ok(refs)
    }
    fn base() -> (
        State,
        Catalog,
        Ids,
        Id,
        RevisionRef,
        RevisionRef,
        RevisionRef,
    ) {
        let mut s = State::default();
        let c = catalog();
        let mut ids = Ids(0);
        let case = run(
            &mut s,
            &c,
            &mut ids,
            cmd(None, json!([{"op":"case.create","payload":{}}])),
        )
        .unwrap()[0]
            .id;
        let ops = json!([
          {"op":"test.question","client_ref":"q","payload":{}},
          {"op":"test.observation","client_ref":"e","payload":{}},
          {"op":"option.create","client_ref":"o","payload":{"question_ref":{"client_ref":"q"},"description":"Keep existing","dependencies":[],"tradeoff_notes":[]}},
          {"op":"criterion.create","client_ref":"k","payload":{"name":"Latency","kind":"hard","scope":{"kind":"all_options","question_ref":{"client_ref":"q"}},"scale":{"unit":"ms","description":"p99","anchors":[]},"preferred_direction":"lower","threshold":{"kind":"measured","operator":"at_most","value":{"decimal":"25","unit":"ms"}}}}
        ]);
        let refs = run(&mut s, &c, &mut ids, cmd(Some(case), ops)).unwrap();
        (s, c, ids, case, refs[1], refs[2], refs[3])
    }
    fn cmp(e: RevisionRef, o: RevisionRef, k: RevisionRef, unit: &str, outcome: &str) -> Value {
        json!({"op":"comparison.add","payload":{"option_ref":o,"criterion_ref":k,"outcome":outcome,"method":"benchmark","value":{"decimal":"18.7","unit":unit},"evidence_refs":[e.id],"assumption_refs":[],"uncertainty":"one workload","rationale":"observed value"}})
    }
    #[test]
    fn commits_exact_comparison_and_rejects_mismatch_and_stale() {
        let (mut s, c, mut ids, case, e, o, k) = base();
        assert!(
            run(
                &mut s,
                &c,
                &mut ids,
                cmd(Some(case), json!([cmp(e, o, k, "seconds", "meets")]))
            )
            .is_err()
        );
        assert!(
            run(
                &mut s,
                &c,
                &mut ids,
                cmd(Some(case), json!([cmp(e, o, k, "ms", "fails")]))
            )
            .is_err()
        );
        let made = run(
            &mut s,
            &c,
            &mut ids,
            cmd(Some(case), json!([cmp(e, o, k, "ms", "meets")])),
        )
        .unwrap()[0];
        let b: ComparisonBody =
            serde_json::from_value(s.current(made.id).unwrap().body.clone()).unwrap();
        assert_eq!(b.evidence_refs, vec![e]);
        run(&mut s,&c,&mut ids,cmd(Some(case),json!([{"op":"criterion.retire","payload":{"criterion_ref":k,"reason":"obsolete"}}]))).unwrap();
        let err = run(
            &mut s,
            &c,
            &mut ids,
            cmd(Some(case), json!([cmp(e, o, k, "ms", "meets")])),
        )
        .unwrap_err();
        assert_eq!(err.error.code, ErrorCode::RevisionConflict);
    }
    #[test]
    fn unknown_is_preserved_and_ruled_out_needs_criterion() {
        let (mut s, c, mut ids, case, e, o, k) = base();
        let mut p = cmp(e, o, k, "ms", "meets");
        p["payload"]["value"] = Value::Null;
        p["payload"]["outcome"] = json!("unknown");
        p["payload"]["basis"] = json!({"kind":"unknown","reason":"not measured"});
        let r = run(&mut s, &c, &mut ids, cmd(Some(case), json!([p]))).unwrap()[0];
        let b: ComparisonBody =
            serde_json::from_value(s.current(r.id).unwrap().body.clone()).unwrap();
        assert_eq!(b.outcome, ComparisonOutcome::Unknown);
        let bad = json!([{"op":"option.disposition","payload":{"option_ref":o,"lifecycle":"ruled_out","reason":"not feasible"}}]);
        assert!(run(&mut s, &c, &mut ids, cmd(Some(case), bad)).is_err());
        let good = json!([{"op":"option.disposition","payload":{"option_ref":o,"lifecycle":"ruled_out","reason":"not feasible","criterion_ref":k}}]);
        run(&mut s, &c, &mut ids, cmd(Some(case), good)).unwrap();
        let b: OptionBody = serde_json::from_value(s.current(o.id).unwrap().body.clone()).unwrap();
        assert_eq!(b.criterion_ref, Some(k));
        assert_eq!(b.disposition_reason.as_deref(), Some("not feasible"));
    }
    #[test]
    fn operation_examples_match_decoders_and_schemas() {
        for op in operations() {
            op.validate_example()
                .unwrap_or_else(|e| panic!("{}: {e:?}", op.info().name));
            let schema = serde_json::to_value(op.payload_schema().unwrap()).unwrap();
            jsonschema::validator_for(&schema)
                .unwrap()
                .validate(op.example().unwrap())
                .unwrap_or_else(|e| panic!("{}: {e}", op.info().name));
        }
    }
    #[test]
    fn same_batch_aliases_resolve_and_cross_question_is_refused() {
        let (mut s, c, mut ids, case, e, _, _) = base();
        let ops = json!([
          {"op":"test.question","client_ref":"q2","payload":{}},
          {"op":"option.create","client_ref":"o2","payload":{"question_ref":{"client_ref":"q2"},"description":"Alternative","dependencies":[],"tradeoff_notes":[]}},
          {"op":"criterion.create","client_ref":"k2","payload":{"name":"Cost","kind":"hard","scope":{"kind":"all_options","question_ref":{"client_ref":"q2"}},"scale":{"unit":"ms","description":"p99","anchors":[]},"preferred_direction":"lower","threshold":{"kind":"measured","operator":"at_most","value":{"decimal":"25","unit":"ms"}}}},
          {"op":"comparison.add","payload":{"option_ref":{"client_ref":"o2"},"criterion_ref":{"client_ref":"k2"},"outcome":"meets","method":"benchmark","value":{"decimal":"18.7","unit":"ms"},"evidence_refs":[e.id],"assumption_refs":[],"uncertainty":"one sample","rationale":"below limit"}}
        ]);
        let refs = run(&mut s, &c, &mut ids, cmd(Some(case), ops)).unwrap();
        let b: ComparisonBody =
            serde_json::from_value(s.current(refs[3].id).unwrap().body.clone()).unwrap();
        assert_eq!(b.option_ref, refs[1]);
        assert_eq!(b.criterion_ref, refs[2]);
        let (_, _, _, _, _, other_option, _) = base();
        let bad = cmp(e, other_option, refs[2], "ms", "meets");
        assert!(run(&mut s, &c, &mut ids, cmd(Some(case), json!([bad]))).is_err());
    }
}
