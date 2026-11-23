//! Synthetic workspaces generated through the public store API.
//!
//! Every record is created by a real operation, batched up to the per-command
//! limit, so the result is an ordinary workspace that `check` verifies. The
//! generators are deterministic apart from the store's own IDs and clock.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use investigation_core::{command::MAX_OPERATIONS, identity::Id};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

const ACTOR: &str = "agent:bench";

/// Sizes of the mixed workspace. [`LargeShape::target`] is the plan's
/// 10,000-object, 50,000-relation workload.
#[derive(Clone, Copy, Debug)]
pub struct LargeShape {
    pub artifacts: usize,
    pub observations: usize,
    pub questions: usize,
    pub assumptions_per_question: usize,
    pub hypotheses_per_question: usize,
    pub assessments_per_question: usize,
    pub evidence_per_assessment: usize,
    pub options_per_question: usize,
    pub criteria_per_question: usize,
    pub comparisons_per_question: usize,
}

impl LargeShape {
    pub fn target() -> Self {
        Self {
            artifacts: 50,
            observations: 2_000,
            questions: 50,
            assumptions_per_question: 20,
            hypotheses_per_question: 40,
            assessments_per_question: 50,
            evidence_per_assessment: 6,
            options_per_question: 20,
            criteria_per_question: 5,
            comparisons_per_question: 23,
        }
    }

    pub fn smoke() -> Self {
        Self {
            artifacts: 2,
            observations: 20,
            questions: 2,
            assumptions_per_question: 2,
            hypotheses_per_question: 4,
            assessments_per_question: 3,
            evidence_per_assessment: 3,
            options_per_question: 3,
            criteria_per_question: 2,
            comparisons_per_question: 2,
        }
    }
}

/// Sizes of the excerpt fixture: many retained artifacts, each cited by a
/// few observations, so every artifact note lists the excerpts that cite it.
#[derive(Clone, Copy, Debug)]
pub struct ExcerptShape {
    pub artifacts: usize,
    pub observations_per_artifact: usize,
}

impl ExcerptShape {
    pub fn target() -> Self {
        Self {
            artifacts: 1_000,
            observations_per_artifact: 8,
        }
    }

    pub fn smoke() -> Self {
        Self {
            artifacts: 4,
            observations_per_artifact: 3,
        }
    }
}

/// Sizes of a choice fixture: one question, `options` × `criteria` cells.
#[derive(Clone, Copy, Debug)]
pub struct ChoiceShape {
    pub options: usize,
    pub criteria: usize,
    /// The first `hard_criteria` criteria are hard, the rest soft.
    pub hard_criteria: usize,
    /// Options that fail hard criterion 0 on a verified Comparison.
    pub hard_failures: usize,
    /// Options that meet hard criterion 0 on a verified Comparison.
    pub hard_passes: usize,
    /// Recorded soft cells, spread over the matrix.
    pub soft_cells: usize,
}

impl ChoiceShape {
    /// The plan's sparse fixture: 1,000 options × 200 criteria, 200,000
    /// possible cells and about 0.6% of them recorded.
    pub fn sparse() -> Self {
        Self {
            options: 1_000,
            criteria: 200,
            hard_criteria: 10,
            hard_failures: 50,
            hard_passes: 50,
            soft_cells: 1_000,
        }
    }

    /// Small enough to enumerate every cell; same mix of cases.
    pub fn exhaustive() -> Self {
        Self {
            options: 6,
            criteria: 5,
            hard_criteria: 1,
            hard_failures: 1,
            hard_passes: 2,
            soft_cells: 5,
        }
    }

    pub fn smoke_sparse() -> Self {
        Self {
            options: 12,
            criteria: 6,
            hard_criteria: 2,
            hard_failures: 2,
            hard_passes: 2,
            soft_cells: 6,
        }
    }
}

/// One generated workspace and what the generator knows about it.
#[derive(Debug)]
pub struct Fixture {
    pub name: String,
    pub root: PathBuf,
    pub case: Id,
    pub generation_ms: f64,
    pub commands: usize,
    /// Current revision of every record the generator created or revised.
    pub revisions: BTreeMap<Id, u64>,
    pub choice: Option<ChoiceTruth>,
    pub question: Id,
}

impl Fixture {
    pub fn reference(&self, id: Id) -> Value {
        json!({"id": id, "revision": self.revisions[&id]})
    }
}

/// Ground truth of a choice fixture, kept by the generator.
#[derive(Debug, Clone)]
pub struct ChoiceTruth {
    pub options: Vec<Id>,
    pub criteria: Vec<Id>,
    pub hard: BTreeSet<Id>,
    /// Cells with a recorded Comparison, as (option, criterion).
    pub recorded: BTreeSet<(Id, Id)>,
    /// Options that fail a hard criterion on a verified Comparison.
    pub hard_failed: BTreeSet<Id>,
    /// An observation and its value, usable by later Comparisons.
    pub evidence: (Id, String),
}

impl ChoiceTruth {
    /// Every cell the plan's rule must report as missing: each live option
    /// against each active criterion, less the recorded cells.
    pub fn missing(&self) -> BTreeSet<(Id, Id)> {
        let mut out = BTreeSet::new();
        for option in &self.options {
            for criterion in &self.criteria {
                if !self.recorded.contains(&(*option, *criterion)) {
                    out.insert((*option, *criterion));
                }
            }
        }
        out
    }

    /// Unrecorded cells usable by measured `comparison.add` mutations, taken
    /// from the last soft criterion so they never touch hard cells.
    pub fn free_cells(&self, count: usize) -> Vec<(Id, Id)> {
        let criterion = *self.criteria.last().expect("criteria");
        self.options
            .iter()
            .rev()
            .filter(|option| !self.recorded.contains(&(**option, criterion)))
            .filter(|option| !self.hard_failed.contains(option))
            .take(count)
            .map(|option| (*option, criterion))
            .collect()
    }
}

/// Deterministic generator for index choices.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % bound as u64) as usize
    }
}

struct Builder {
    store: Store,
    root: PathBuf,
    inputs: PathBuf,
    case: Option<Id>,
    commands: usize,
    revisions: BTreeMap<Id, u64>,
    digests: BTreeMap<Id, String>,
}

fn op(name: &str, payload: Value) -> Value {
    json!({"op": name, "payload": payload})
}

impl Builder {
    fn new(directory: &Path, vault: bool) -> Result<Self, String> {
        let root = directory.join("workspace");
        let inputs = directory.join("inputs");
        fs::create_dir_all(&inputs).map_err(|error| format!("{}: {error}", inputs.display()))?;
        let workspace =
            Workspace::init_with_import_roots(&root, vault, std::slice::from_ref(&inputs))
                .map_err(|error| format!("init {}: {error}", root.display()))?;
        Ok(Self {
            store: Store::open(workspace),
            root,
            inputs,
            case: None,
            commands: 0,
            revisions: BTreeMap::new(),
            digests: BTreeMap::new(),
        })
    }

    fn r(&self, id: Id) -> Value {
        json!({"id": id, "revision": self.revisions[&id]})
    }

    fn create_case(&mut self, title: &str) -> Result<Id, String> {
        let created = self.submit(vec![op(
            "case.create",
            json!({"title": title, "mode": "evaluate"}),
        )])?;
        self.case = Some(created[0]);
        Ok(created[0])
    }

    /// Commit one transaction and return the IDs it created, in order. A gate
    /// that asks for preconditions is retried once with the revisions it names.
    fn submit(&mut self, operations: Vec<Value>) -> Result<Vec<Id>, String> {
        let mut command = json!({
            "schema_version": "inv.command.v1", "case_id": self.case, "actor": ACTOR,
            "preconditions": [], "operations": operations,
        });
        for attempt in 0..2 {
            self.commands += 1;
            command["command_id"] = json!(format!("bench-{}", self.commands));
            let bytes = serde_json::to_vec(&command).map_err(|error| error.to_string())?;
            match self.store.record(&bytes) {
                Ok(receipt) => {
                    for change in receipt.created.iter().chain(&receipt.updated) {
                        self.revisions.insert(change.id, change.revision);
                    }
                    return Ok(receipt.created.iter().map(|change| change.id).collect());
                }
                Err(failure)
                    if attempt == 0
                        && failure
                            .error
                            .message
                            .contains("as a transaction precondition") =>
                {
                    command["preconditions"] = failure.error.expected.clone().unwrap_or_default();
                }
                Err(failure) => {
                    let first = &command["operations"][0];
                    return Err(format!(
                        "generator command rejected: {} {} at {:?} (first op {})",
                        serde_json::to_string(&failure.error.code).unwrap_or_default(),
                        failure.error.message,
                        failure.error.path,
                        first["op"],
                    ));
                }
            }
        }
        Err("generator command rejected twice".into())
    }

    /// Commit operations in transactions of at most [`MAX_OPERATIONS`].
    fn submit_all(&mut self, operations: Vec<Value>) -> Result<Vec<Id>, String> {
        let mut created = Vec::with_capacity(operations.len());
        let mut pending = operations.into_iter().peekable();
        while pending.peek().is_some() {
            let batch: Vec<Value> = pending.by_ref().take(MAX_OPERATIONS).collect();
            created.extend(self.submit(batch)?);
        }
        Ok(created)
    }

    /// Retain one small text file per artifact through `ingest`.
    fn ingest(&mut self, count: usize) -> Result<Vec<Id>, String> {
        let case = self.case.ok_or("ingest needs a case")?;
        let mut artifacts = Vec::with_capacity(count);
        for index in 0..count {
            let path = self.inputs.join(format!("measurements-{index}.txt"));
            fs::write(&path, format!("synthetic measurements, file {index}\n"))
                .map_err(|error| error.to_string())?;
            self.commands += 1;
            let manifest = json!({
                "schema_version": "inv.ingest.v1", "command_id": format!("bench-{}", self.commands),
                "actor": ACTOR, "case_id": case,
                "source": {"title": format!("Measurement file {index}"), "format": "text/plain"},
                "paths": [path],
            });
            let receipt = self
                .store
                .ingest(
                    &serde_json::to_vec(&manifest).map_err(|e| e.to_string())?,
                    None,
                )
                .map_err(|failure| format!("ingest rejected: {}", failure.error.message))?;
            for change in receipt
                .receipt
                .created
                .iter()
                .chain(&receipt.receipt.updated)
            {
                self.revisions.insert(change.id, change.revision);
            }
            let item = &receipt.items[0];
            self.digests
                .insert(item.artifact_id, item.digest.to_string());
            artifacts.push(item.artifact_id);
        }
        Ok(artifacts)
    }

    /// Retain one small text file per artifact, up to the per-manifest item
    /// limit in each `ingest`.
    fn ingest_many(&mut self, count: usize) -> Result<Vec<Id>, String> {
        const ITEMS: usize = 32;
        let case = self.case.ok_or("ingest needs a case")?;
        let mut artifacts = Vec::with_capacity(count);
        for first in (0..count).step_by(ITEMS) {
            let mut paths = Vec::new();
            for index in first..(first + ITEMS).min(count) {
                let path = self.inputs.join(format!("excerpts-{index}.txt"));
                fs::write(&path, format!("synthetic excerpts, file {index}\n"))
                    .map_err(|error| error.to_string())?;
                paths.push(path);
            }
            self.commands += 1;
            let manifest = json!({
                "schema_version": "inv.ingest.v1", "command_id": format!("bench-{}", self.commands),
                "actor": ACTOR, "case_id": case,
                "source": {"title": format!("Excerpt files from {first}"), "format": "text/plain"},
                "paths": paths,
            });
            let receipt = self
                .store
                .ingest(
                    &serde_json::to_vec(&manifest).map_err(|e| e.to_string())?,
                    None,
                )
                .map_err(|failure| format!("ingest rejected: {}", failure.error.message))?;
            for change in receipt
                .receipt
                .created
                .iter()
                .chain(&receipt.receipt.updated)
            {
                self.revisions.insert(change.id, change.revision);
            }
            for item in &receipt.items {
                self.digests
                    .insert(item.artifact_id, item.digest.to_string());
                artifacts.push(item.artifact_id);
            }
        }
        Ok(artifacts)
    }

    fn observation(&self, artifact: Id, value: &str) -> Value {
        op(
            "observation.add",
            json!({
                "value": {"kind": "measurement", "value": value, "unit": "ms"},
                "basis": {"kind": "source", "artifact_ref": self.r(artifact),
                    "selector": {"schema_version": "inv.selector.v1",
                        "artifact_digest": self.digests[&artifact], "selector": {"type": "whole"}},
                    "text_quote": null},
                "extraction_method": "Synthetic measurement", "validity": "valid",
                "quality": [], "limitations": [], "evidence_kind": "observational",
                "variables_changed": []
            }),
        )
    }

    fn criterion(&self, question: Id, index: usize, hard: bool) -> Value {
        let threshold = if hard {
            json!({"kind": "measured", "operator": "at_most",
                "value": {"decimal": "100", "unit": "ms"}})
        } else {
            Value::Null
        };
        op(
            "criterion.create",
            json!({
                "name": format!("Criterion {index}"), "kind": if hard {"hard"} else {"soft"},
                "scope": {"kind": "all_options", "question_ref": self.r(question)},
                "scale": {"unit": "ms", "description": "Synthetic latency", "anchors": []},
                "preferred_direction": "lower", "threshold": threshold,
                "weighting_rationale": null
            }),
        )
    }

    fn comparison(
        &self,
        option: Id,
        criterion: Id,
        outcome: &str,
        evidence: &[(Id, String)],
    ) -> Value {
        let refs: Vec<Value> = evidence.iter().map(|(id, _)| self.r(*id)).collect();
        op(
            "comparison.add",
            json!({
                "option_ref": self.r(option), "criterion_ref": self.r(criterion),
                "outcome": outcome, "method": "Synthetic benchmark",
                "value": {"decimal": evidence[0].1, "unit": "ms"},
                "evidence_refs": refs, "assumption_refs": [],
                "uncertainty": "Synthetic", "rationale": "Generated cell"
            }),
        )
    }

    fn options(&mut self, question: Id, count: usize) -> Result<Vec<Id>, String> {
        let ops = (0..count)
            .map(|index| {
                op(
                    "option.create",
                    json!({"question_ref": self.r(question), "description": format!("Option {index}"),
                        "dependencies": [], "tradeoff_notes": []}),
                )
            })
            .collect();
        let options = self.submit_all(ops)?;
        let ops = options
            .iter()
            .map(|option| {
                op(
                    "option.disposition",
                    json!({"option_ref": self.r(*option), "lifecycle": "eligible",
                        "reason": "Plausible candidate"}),
                )
            })
            .collect();
        self.submit_all(ops)?;
        Ok(options)
    }

    fn finish(self, name: &str, question: Id, started: Instant) -> Fixture {
        Fixture {
            name: name.into(),
            root: self.root,
            case: self.case.expect("case"),
            generation_ms: started.elapsed().as_secs_f64() * 1e3,
            commands: self.commands,
            revisions: self.revisions,
            choice: None,
            question,
        }
    }
}

/// The mixed workspace: questions with hypotheses, rival links, assumptions,
/// evidence-heavy assessments, options, criteria and verified Comparisons.
pub fn large(directory: &Path, shape: LargeShape) -> Result<Fixture, String> {
    let started = Instant::now();
    let mut b = Builder::new(directory, true)?;
    let mut rng = Lcg(0x5EED_0010_5000);
    b.create_case("Synthetic large workspace")?;
    let artifacts = b.ingest(shape.artifacts)?;
    let observation_values: Vec<String> = (0..shape.observations)
        .map(|index| format!("{}", 10 + index % 90))
        .collect();
    let ops = (0..shape.observations)
        .map(|index| {
            b.observation(
                artifacts[index % artifacts.len()],
                &observation_values[index],
            )
        })
        .collect();
    let observations: Vec<(Id, String)> = b
        .submit_all(ops)?
        .into_iter()
        .zip(observation_values)
        .collect();
    let ops = (0..shape.questions)
        .map(|index| {
            op(
                "question.create",
                json!({"wording": format!("Synthetic question {index}?"), "importance": "normal"}),
            )
        })
        .collect();
    let questions = b.submit_all(ops)?;
    for question in &questions {
        let ops = (0..shape.assumptions_per_question)
            .map(|index| {
                op(
                    "assumption.create",
                    json!({"affected_refs": [b.r(*question)],
                        "claim": format!("Synthetic assumption {index}."),
                        "consequence": "Results transfer.", "materiality": "nonmaterial"}),
                )
            })
            .collect();
        let assumptions = b.submit_all(ops)?;
        // Hypotheses of one question share a transaction so rivals can name
        // earlier ones by client reference.
        let mut ops = Vec::new();
        for index in 0..shape.hypotheses_per_question {
            let rivals: Vec<Value> = (index.saturating_sub(3)..index)
                .map(|rival| json!({"client_ref": format!("h{rival}")}))
                .collect();
            let assumption = assumptions[index % assumptions.len()];
            let mut operation = op(
                "hypothesis.create",
                json!({"question_ref": b.r(*question), "statement": format!("Synthetic hypothesis {index}."),
                    "possible_falsifiers": [], "rival_refs": rivals,
                    "assumption_refs": [b.r(assumption)]}),
            );
            operation["client_ref"] = json!(format!("h{index}"));
            ops.push(operation);
        }
        let hypotheses = b.submit_all(ops)?;
        let ops = (0..shape.assessments_per_question)
            .map(|index| {
                let evidence: BTreeSet<usize> = (0..shape.evidence_per_assessment)
                    .map(|_| rng.next(observations.len()))
                    .collect();
                let evidence: Vec<Value> = evidence
                    .into_iter()
                    .map(|index| b.r(observations[index].0))
                    .collect();
                let first = assumptions[index % assumptions.len()];
                let second = assumptions[(index + 1) % assumptions.len()];
                let mut assumption_refs = vec![b.r(first)];
                if second != first {
                    assumption_refs.push(b.r(second));
                }
                // Live rivals are the declared links in either direction:
                // up to three before and three after the subject.
                let subject = index % hypotheses.len();
                let expectations: Vec<Value> = (subject.saturating_sub(3)
                    ..(subject + 4).min(hypotheses.len()))
                    .filter(|rival| *rival != subject)
                    .map(|rival| {
                        json!({"rival_ref": b.r(hypotheses[rival]),
                            "expectation": "not_assessed", "rationale": "Not compared."})
                    })
                    .collect();
                op(
                    "assessment.add",
                    json!({"subject_ref": b.r(hypotheses[subject]),
                        "evidence_refs": evidence, "assumption_refs": assumption_refs,
                        "rival_expectations": expectations,
                        "relation": "supports", "rationale": "Synthetic assessment."}),
                )
            })
            .collect();
        b.submit_all(ops)?;
        let options = b.options(*question, shape.options_per_question)?;
        let ops = (0..shape.criteria_per_question)
            .map(|index| b.criterion(*question, index, false))
            .collect();
        let criteria = b.submit_all(ops)?;
        let mut cells = BTreeSet::new();
        while cells.len()
            < shape
                .comparisons_per_question
                .min(options.len() * criteria.len())
        {
            cells.insert((rng.next(options.len()), rng.next(criteria.len())));
        }
        let ops = cells
            .into_iter()
            .map(|(option, criterion)| {
                let first = observations[rng.next(observations.len())].clone();
                let second = observations[rng.next(observations.len())].clone();
                b.comparison(
                    options[option],
                    criteria[criterion],
                    "meets",
                    &[first, second],
                )
            })
            .collect();
        b.submit_all(ops)?;
    }
    Ok(b.finish("large", questions[0], started))
}

/// Many artifacts, each cited by `observations_per_artifact` observations
/// created round-robin, so the observations citing one artifact are spread
/// through the snapshot and every artifact note lists its excerpts.
pub fn excerpts(directory: &Path, shape: ExcerptShape) -> Result<Fixture, String> {
    let started = Instant::now();
    let mut b = Builder::new(directory, true)?;
    b.create_case("Synthetic excerpt workspace")?;
    let artifacts = b.ingest_many(shape.artifacts)?;
    let ops = (0..shape.artifacts * shape.observations_per_artifact)
        .map(|index| {
            b.observation(
                artifacts[index % artifacts.len()],
                &format!("{}", 10 + index % 90),
            )
        })
        .collect();
    b.submit_all(ops)?;
    let question = b.submit(vec![op(
        "question.create",
        json!({"wording": "Which measurement applies?", "importance": "normal"}),
    )])?[0];
    Ok(b.finish("excerpts", question, started))
}

/// One question with `options` × `criteria` cells, a few of them recorded.
pub fn choice(directory: &Path, name: &str, shape: ChoiceShape) -> Result<Fixture, String> {
    let started = Instant::now();
    let mut b = Builder::new(directory, true)?;
    let mut rng = Lcg(0xC401_CE00_0200);
    b.create_case("Synthetic choice")?;
    let artifact = b.ingest(1)?[0];
    // Values 10..=109 ms; those above 100 fail the hard threshold.
    let values: Vec<String> = (10..110).map(|value| value.to_string()).collect();
    let ops = values
        .iter()
        .map(|value| b.observation(artifact, value))
        .collect();
    let observations: Vec<(Id, String)> = b.submit_all(ops)?.into_iter().zip(values).collect();
    let (passing, failing) = observations.split_at(91);
    let question = b.submit(vec![op(
        "question.create",
        json!({"wording": "Which option should be chosen?", "importance": "high"}),
    )])?[0];
    let options = b.options(question, shape.options)?;
    let ops = (0..shape.criteria)
        .map(|index| b.criterion(question, index, index < shape.hard_criteria))
        .collect();
    let criteria = b.submit_all(ops)?;
    let mut recorded = BTreeSet::new();
    let mut hard_failed = BTreeSet::new();
    let mut ops = Vec::new();
    let hard = criteria[0];
    for (index, option) in options.iter().enumerate() {
        if index < shape.hard_failures {
            let evidence = failing[index % failing.len()].clone();
            ops.push(b.comparison(*option, hard, "fails", &[evidence]));
            hard_failed.insert(*option);
        } else if index < shape.hard_failures + shape.hard_passes {
            let evidence = passing[index % passing.len()].clone();
            ops.push(b.comparison(*option, hard, "meets", &[evidence]));
        } else {
            continue;
        }
        recorded.insert((*option, hard));
    }
    let soft = &criteria[shape.hard_criteria..];
    // Spread soft cells over the matrix, skipping the last soft criterion so
    // measured mutations always find free cells there.
    let spread = soft.len().saturating_sub(1).max(1);
    let mut attempts = 0;
    while recorded.len() < shape.hard_failures + shape.hard_passes + shape.soft_cells
        && attempts < shape.soft_cells * 20
    {
        attempts += 1;
        let option = options[rng.next(options.len())];
        let criterion = soft[rng.next(spread)];
        if recorded.insert((option, criterion)) {
            let evidence = passing[rng.next(passing.len())].clone();
            ops.push(b.comparison(option, criterion, "meets", &[evidence]));
        }
    }
    b.submit_all(ops)?;
    let evidence = passing[0].clone();
    let mut fixture = b.finish(name, question, started);
    fixture.choice = Some(ChoiceTruth {
        options,
        hard: criteria[..shape.hard_criteria].iter().copied().collect(),
        criteria,
        recorded,
        hard_failed,
        evidence,
    });
    Ok(fixture)
}
