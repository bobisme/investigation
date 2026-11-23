//! `coverage.define`, `coverage.revise` and `coverage.link`. The model and
//! the read-time rules are in [`crate::coverage`].

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    catalog::{OperationDescriptor, OperationInfo},
    command::{Reference, RevisionRef},
    coverage::{
        COVERAGE_KEY, COVERAGE_ROLE, COVERAGE_RULES_VERSION, CellLink, DefinitionRevision,
        LinkState, MAX_LINK_REFS, MAX_LINKS, MAX_LINKS_PER_OPERATION, MAX_REVISIONS,
        PlanDefinition, PlanKind, Space, StoredPlan, TEXT_LIMIT, all_coordinate_refs, cell_digest,
        check_definition, integrity, invalid, is_plan, nonblank, stored,
    },
    evidence::MeasurementValidity,
    freshness::successor_change,
    identity::Kind,
    measurement_ops::effective_validity,
    state::{OperationContext, Record},
    wire::{ErrorCode, Failure},
};

/// A new definition for an existing plan. Links are kept; a cell whose
/// definition changed reads `needs_retest` until it is linked again.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverageRevise {
    /// The exact current revision of the plan.
    pub plan_ref: Reference,
    pub definition: PlanDefinition,
    pub reason: String,
}

/// One recorded result for one cell.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkInput {
    /// The cell, naming every dimension of the current plan revision.
    pub cell: BTreeMap<String, String>,
    pub state: LinkState,
    /// Exact current Runs, Observations or Comparisons. Required except for
    /// `blocked`.
    #[serde(default)]
    pub record_refs: Vec<Reference>,
    pub rationale: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverageLink {
    /// The exact current revision of the plan.
    pub plan_ref: Reference,
    pub links: Vec<LinkInput>,
}

const RELATED: &[&str] = &[
    "coverage.define",
    "coverage.revise",
    "coverage.link",
    "run.record",
    "observation.add",
    "comparison.add",
];

pub(crate) fn operations() -> [OperationDescriptor; 3] {
    let plan = json!({"id": "ann_019acd16-0000-7000-8000-000000000001", "revision": 1});
    let definition = json!({
        "title": "Reproduction conditions by component",
        "plan_kind": "general",
        "applicability": "Builds of the current release branch on the recorded test hosts.",
        "dimensions": [
            {"name": "condition", "values": [{"key": "cold_start"}, {"key": "warm_restart"}]},
            {"name": "component", "values": [{"key": "parser"}, {"key": "cache"}, {"key": "writer"}]}
        ],
        "predicate": "The reported failure occurs under the condition in the component.",
        "polarity": {
            "positive": "The failure occurred under the condition in the component.",
            "negative": "The failure did not occur in the recorded attempts, within the stated scope."
        },
        "exclusions": [{
            "matching": {"condition": "cold_start", "component": "writer"},
            "reason": "The writer is not loaded during a cold start."
        }]
    });
    [
        OperationDescriptor::active(
            OperationInfo {
                name: "coverage.define",
                stage: "collect",
                summary: "Define an optional, bounded coverage plan: dimensions, per-cell predicate and polarity, applicability and exclusions.",
                when_to_use: "Only when a finite set of conditions, components or option-criterion cells is explicitly in scope and a denominator is meaningful. Most cases need no plan.",
                lifecycle: "An existing case. An evaluation plan names a Question, one dimension of its eligible Options and one of its active Criteria.",
                effects: "Create one coverage plan at plan revision 1, stored sparsely: declared values, overrides, exclusions and later links, never one entry per cell. Every cell starts untested.",
                returns: "The plan's Annotation ID and revision 1.",
                revision_rule: "Records named by dimension values must be cited at their exact current revision.",
                idempotency: "Retry the identical command with its original actor and command ID.",
                repair: "Keep the Cartesian size within 4096 cells by keeping only values that could change the answer, merging values tested together, or splitting the space into separate plans. Exclusions do not reduce the declared size.",
                related: RELATED,
            },
            definition.clone(),
            |payload: &PlanDefinition| check_definition(payload, "").map(drop),
            define,
        ),
        OperationDescriptor::active(
            OperationInfo {
                name: "coverage.revise",
                stage: "collect",
                summary: "Record a new revision of a coverage plan's definition and keep its denominator history.",
                when_to_use: "When dimensions, predicates, polarity, applicability or exclusions change, or to pin the current revision of a changed option or criterion.",
                lifecycle: "The exact current revision of a coverage plan. The plan kind cannot change.",
                effects: "Replace the definition, raise the plan revision and append the new Cartesian size, exclusions and denominator to the history. Links stay; a cell whose definition changed reads needs_retest until linked again.",
                returns: "The plan's Annotation ID and its new record revision.",
                revision_rule: "Supply the exact current plan_ref. Records named by dimension values must be current.",
                idempotency: "Retry the identical command with its original actor and command ID.",
                repair: "Read the plan with show and revise from its current revision with a changed definition and a reason.",
                related: RELATED,
            },
            json!({"plan_ref": plan, "definition": definition, "reason": "Add the writer under warm restarts after the fix landed."}),
            |payload: &CoverageRevise| {
                nonblank(&payload.reason, TEXT_LIMIT, "/reason")?;
                check_definition(&payload.definition, "/definition").map(drop)
            },
            revise,
        ),
        OperationDescriptor::active(
            OperationInfo {
                name: "coverage.link",
                stage: "collect",
                summary: "Attach Runs, Observations or Comparisons to cells of a coverage plan with the result they show.",
                when_to_use: "After an attempt in a planned cell: record blocked, attempted_inconclusive, observed_negative_within_scope or observed_positive.",
                lifecycle: "The exact current revision of a coverage plan; each cell is declared and not excluded.",
                effects: "Append one link per cell to the plan in a new record revision. The latest link of a cell is its recorded result; it no longer counts as assessed once its cell definition or a linked or coordinate record changes.",
                returns: "The plan's Annotation ID and its new record revision.",
                revision_rule: "Supply the exact current plan_ref and current revisions of linked records.",
                idempotency: "Retry the identical command with its original actor and command ID.",
                repair: "An observed result needs a completed Run, an Observation not judged invalid, or a Comparison of the cell's option and criterion. Record an attempt that could not be read as attempted_inconclusive, and one that could not run as blocked.",
                related: RELATED,
            },
            json!({"plan_ref": plan, "links": [{
                "cell": {"condition": "warm_restart", "component": "cache"},
                "state": "observed_positive",
                "record_refs": [{"id": "run_019acd16-0000-7000-8000-000000000002", "revision": 1}],
                "rationale": "The failure reproduced on the second warm restart."
            }]}),
            validate_link,
            link,
        ),
    ]
}

fn gate(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::GateFailed, message).at(path)
}

/// An exact, current, same-case record.
fn current<'c>(
    ctx: &'c OperationContext<'_>,
    reference: &Reference,
    path: &str,
) -> Result<(RevisionRef, &'c Record), Failure> {
    let exact = ctx.resolve_reference(reference, path)?;
    let record = ctx.exact(exact).ok_or_else(integrity)?;
    if Some(record.header.case_id) != ctx.case_id() {
        return Err(invalid(path, "Cite a record of this case."));
    }
    let now = ctx.current(exact.id).ok_or_else(integrity)?;
    if now.reference() != exact {
        let mut failure = Failure::new(
            ErrorCode::RevisionConflict,
            "Cite the record's current revision.",
        )
        .at(path);
        failure.error.actual = Some(json!(now.reference()));
        return Err(failure);
    }
    Ok((exact, record))
}

/// The exact current revision of a coverage plan and its stored plan.
fn plan(
    ctx: &OperationContext<'_>,
    reference: &Reference,
) -> Result<(RevisionRef, StoredPlan), Failure> {
    let (exact, record) = current(ctx, reference, "/plan_ref")?;
    if !is_plan(record) {
        return Err(invalid("/plan_ref", "Cite a coverage plan."));
    }
    Ok((exact, stored(record)?))
}

/// Pin every reference in a definition to its exact current revision and
/// check what an evaluation plan requires of them.
fn resolve_definition(
    ctx: &OperationContext<'_>,
    mut definition: PlanDefinition,
    base: &str,
) -> Result<PlanDefinition, Failure> {
    let question = match &definition.question_ref {
        Some(reference) => {
            let path = format!("{base}/question_ref");
            let (exact, record) = current(ctx, reference, &path)?;
            if record.header.id.kind() != Kind::Question {
                return Err(invalid(&path, "Expected a Question record reference."));
            }
            definition.question_ref = Some(Reference::Existing(exact));
            Some(exact.id)
        }
        None => None,
    };
    let mut option_dimension = false;
    let mut criterion_dimension = false;
    for (index, dimension) in definition.dimensions.iter_mut().enumerate() {
        let mut kinds = BTreeSet::new();
        for (position, value) in dimension.values.iter_mut().enumerate() {
            let Some(reference) = &value.record_ref else {
                kinds.insert(None);
                continue;
            };
            let path = format!("{base}/dimensions/{index}/values/{position}/record_ref");
            let (exact, record) = current(ctx, reference, &path)?;
            let kind = record.header.id.kind();
            if kind == Kind::Case || is_plan(record) {
                return Err(invalid(
                    &path,
                    "A dimension value names a record inside the case.",
                ));
            }
            if definition.plan_kind == PlanKind::Evaluation {
                let body = &record.body;
                let member = body["question_ref"]["id"].as_str().map(str::to_owned)
                    == question.map(|id| id.to_string());
                match kind {
                    Kind::Option if !member || body["lifecycle"] != "eligible" => {
                        return Err(gate(
                            &path,
                            "An evaluation plan covers eligible Options of its Question.",
                        ));
                    }
                    Kind::Criterion if !member || body["lifecycle"] != "active" => {
                        return Err(gate(
                            &path,
                            "An evaluation plan covers active Criteria of its Question.",
                        ));
                    }
                    _ => {}
                }
            }
            kinds.insert(Some(kind));
            value.record_ref = Some(Reference::Existing(exact));
        }
        option_dimension |= kinds.len() == 1 && kinds.contains(&Some(Kind::Option));
        criterion_dimension |= kinds.len() == 1 && kinds.contains(&Some(Kind::Criterion));
    }
    if definition.plan_kind == PlanKind::Evaluation && !(option_dimension && criterion_dimension) {
        return Err(invalid(
            &format!("{base}/dimensions"),
            "An evaluation plan has one dimension whose values all name eligible Options and one whose values all name active Criteria of its Question.",
        ));
    }
    Ok(definition)
}

fn record_body(plan: &StoredPlan) -> Result<Value, Failure> {
    Ok(json!({
        "subject_refs": all_coordinate_refs(&plan.definition),
        "role": COVERAGE_ROLE,
        "evidentiary_status": "none",
        "content": {"kind": "text", "text": plan.definition.title},
        COVERAGE_KEY: serde_json::to_value(plan).map_err(|_| integrity())?,
    }))
}

fn revision_entry(
    ctx: &OperationContext<'_>,
    plan_revision: u64,
    space: &Space,
    reason: Option<String>,
) -> DefinitionRevision {
    DefinitionRevision {
        plan_revision,
        accepted_at_sequence: ctx.acceptance_sequence(),
        revised_by: ctx.actor().clone(),
        reason,
        cartesian_size: space.cartesian_size,
        excluded: space.excluded,
        denominator: space.denominator(),
    }
}

fn define(payload: PlanDefinition, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let definition = resolve_definition(ctx, payload, "")?;
    let space = check_definition(&definition, "")?;
    let plan = StoredPlan {
        rules_version: COVERAGE_RULES_VERSION.to_owned(),
        plan_revision: 1,
        history: vec![revision_entry(ctx, 1, &space, None)],
        definition,
        links: Vec::new(),
    };
    let body = record_body(&plan)?;
    ctx.create(Kind::Annotation, body)?;
    Ok(())
}

fn revise(payload: CoverageRevise, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let (exact, mut plan) = plan(ctx, &payload.plan_ref)?;
    let definition = resolve_definition(ctx, payload.definition, "/definition")?;
    let space = check_definition(&definition, "/definition")?;
    if definition.plan_kind != plan.definition.plan_kind {
        return Err(invalid(
            "/definition/plan_kind",
            "A plan keeps its kind. Define a separate plan for another kind.",
        ));
    }
    if definition == plan.definition {
        return Err(invalid("/definition", "The definition is unchanged."));
    }
    if plan.history.len() >= MAX_REVISIONS {
        return Err(invalid(
            "/plan_ref",
            format!("A plan keeps at most {MAX_REVISIONS} revisions. Define a new plan."),
        ));
    }
    // Results recorded against a cell definition that changes, or a cell
    // that leaves the plan, no longer count as assessed.
    let mut latest: BTreeMap<&BTreeMap<String, String>, &CellLink> = BTreeMap::new();
    for link in &plan.links {
        latest.insert(&link.cell, link);
    }
    let mut stale = 0usize;
    let mut orphaned = 0usize;
    for link in latest.values() {
        match space.locate(&link.cell) {
            Ok(coordinates) => {
                if cell_digest(&definition, &space, &coordinates)? != link.cell_digest {
                    stale += 1;
                }
            }
            Err(_) => orphaned += 1,
        }
    }
    plan.plan_revision += 1;
    plan.history.push(revision_entry(
        ctx,
        plan.plan_revision,
        &space,
        Some(payload.reason),
    ));
    plan.definition = definition;
    let body = record_body(&plan)?;
    ctx.update(exact, body)?;
    if stale > 0 {
        ctx.warn(format!(
            "coverage.cells_need_retest: {stale} linked cell(s) changed definition and need a new result."
        ));
    }
    if orphaned > 0 {
        ctx.warn(format!(
            "coverage.links_orphaned: {orphaned} linked cell(s) are no longer in the plan."
        ));
    }
    Ok(())
}

fn validate_link(payload: &CoverageLink) -> Result<(), Failure> {
    if payload.links.is_empty() || payload.links.len() > MAX_LINKS_PER_OPERATION {
        return Err(invalid(
            "/links",
            format!("Supply between 1 and {MAX_LINKS_PER_OPERATION} links."),
        ));
    }
    for (index, link) in payload.links.iter().enumerate() {
        let path = format!("/links/{index}");
        nonblank(&link.rationale, TEXT_LIMIT, &format!("{path}/rationale"))?;
        if link.record_refs.len() > MAX_LINK_REFS {
            return Err(invalid(
                &format!("{path}/record_refs"),
                format!("Link at most {MAX_LINK_REFS} records per cell."),
            ));
        }
        if link.record_refs.is_empty() && link.state != LinkState::Blocked {
            return Err(invalid(
                &format!("{path}/record_refs"),
                "Cite the Run, Observation or Comparison of the attempt. Only a blocked cell may cite none.",
            ));
        }
    }
    Ok(())
}

/// Whether a linked record has ended: retracted, superseded, withdrawn,
/// retired or rejected.
fn ended(record: &Record) -> bool {
    let body = &record.body;
    body["retracted"] == true
        || ["status", "lifecycle"].iter().any(|key| {
            matches!(
                body[*key].as_str(),
                Some("retracted" | "superseded" | "withdrawn" | "retired" | "rejected")
            )
        })
}

/// Why a record cannot show an observed result in the cell, if it cannot.
fn not_observable(
    ctx: &OperationContext<'_>,
    record: &Record,
    coordinate_ids: &[RevisionRef],
) -> Option<&'static str> {
    let body = &record.body;
    match record.header.id.kind() {
        Kind::Observation
            if effective_validity(ctx.state(), record) == MeasurementValidity::Invalid =>
        {
            Some("The Observation is judged invalid; it cannot assess the cell.")
        }
        Kind::Run if body["execution_status"] != "completed" => {
            Some("The Run did not complete; record the cell as blocked or attempted_inconclusive.")
        }
        Kind::Run if body["evidence_validity"] == "invalid" => {
            Some("The Run has an invalidity finding; it cannot assess the cell.")
        }
        Kind::Comparison => {
            for (field, kind) in [
                ("option_ref", Kind::Option),
                ("criterion_ref", Kind::Criterion),
            ] {
                let named = coordinate_ids
                    .iter()
                    .find(|reference| reference.id.kind() == kind);
                if let Some(named) = named
                    && body[field]["id"].as_str() != Some(named.id.to_string().as_str())
                {
                    return Some(
                        "The Comparison relates another option or criterion than the cell names.",
                    );
                }
            }
            None
        }
        _ => None,
    }
}

fn link(payload: CoverageLink, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let (exact, mut plan) = plan(ctx, &payload.plan_ref)?;
    let space = check_definition(&plan.definition, "").map_err(|_| integrity())?;
    if plan.links.len() + payload.links.len() > MAX_LINKS {
        return Err(invalid(
            "/links",
            format!(
                "A plan keeps at most {MAX_LINKS} links. Define a new plan for further attempts."
            ),
        ));
    }
    let case = ctx.case_id().ok_or_else(integrity)?;
    let mut seen = BTreeSet::new();
    let mut appended = Vec::new();
    for (index, input) in payload.links.into_iter().enumerate() {
        let path = format!("/links/{index}");
        let coordinates = space
            .locate(&input.cell)
            .map_err(|message| invalid(&format!("{path}/cell"), message))?;
        if !seen.insert(coordinates.clone()) {
            return Err(invalid(
                &format!("{path}/cell"),
                "Link each cell once per operation.",
            ));
        }
        if plan.definition.exclusions.iter().any(|exclusion| {
            exclusion
                .matching
                .iter()
                .all(|(name, key)| input.cell.get(name) == Some(key))
        }) {
            return Err(gate(
                &format!("{path}/cell"),
                "The cell is excluded from the plan. Revise the plan to bring it into scope first.",
            ));
        }
        let coordinate_ids: Vec<RevisionRef> = plan
            .definition
            .dimensions
            .iter()
            .zip(&coordinates)
            .filter_map(
                |(dimension, index)| match &dimension.values[*index].record_ref {
                    Some(Reference::Existing(reference)) => Some(*reference),
                    _ => None,
                },
            )
            .collect();
        for reference in &coordinate_ids {
            if successor_change(ctx.state(), case, *reference).is_some() {
                let mut failure = gate(
                    &format!("{path}/cell"),
                    "A record this cell names changed after the plan pinned it. Revise the plan to pin its current revision, then link.",
                );
                failure.error.actual = Some(json!(reference));
                return Err(failure);
            }
        }
        let mut record_refs = Vec::new();
        for (position, reference) in input.record_refs.iter().enumerate() {
            let ref_path = format!("{path}/record_refs/{position}");
            let (exact, record) = current(ctx, reference, &ref_path)?;
            if !matches!(
                record.header.id.kind(),
                Kind::Run | Kind::Observation | Kind::Comparison
            ) {
                return Err(invalid(&ref_path, "Link a Run, Observation or Comparison."));
            }
            if ended(record) {
                return Err(gate(
                    &ref_path,
                    "The record has ended and cannot be linked.",
                ));
            }
            if input.state.observed()
                && let Some(reason) = not_observable(ctx, record, &coordinate_ids)
            {
                return Err(gate(&ref_path, reason));
            }
            if record_refs.contains(&exact) {
                return Err(invalid(&ref_path, "List each linked record once."));
            }
            record_refs.push(exact);
        }
        appended.push(CellLink {
            cell: input.cell,
            state: input.state,
            record_refs,
            rationale: input.rationale,
            linked_by: ctx.actor().clone(),
            accepted_at_sequence: ctx.acceptance_sequence(),
            plan_revision: plan.plan_revision,
            cell_digest: cell_digest(&plan.definition, &space, &coordinates)?,
        });
    }
    plan.links.extend(appended);
    let body = record_body(&plan)?;
    ctx.update(exact, body)?;
    Ok(())
}
