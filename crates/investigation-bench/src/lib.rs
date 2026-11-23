//! Performance harness for plan §10.5.
//!
//! Generates the three synthetic fixtures through the store API, verifies each
//! with `check`, checks that grouped `next` output is complete against the
//! generator's ground truth, then times the real `investigation` binary on
//! warm reads, single-operation mutations, index rebuilds and vault renders.
//! Results are compared with the plan's targets and returned as one JSON
//! report. The numbers describe the machine they ran on; the report records
//! that machine, the build, the sample counts, the warmup and the trial order.

pub mod completeness;
pub mod fixtures;
pub mod measure;
pub mod profile;

use std::{
    fs,
    path::{Path, PathBuf},
};

use fixtures::{ChoiceShape, Fixture, LargeShape};
use investigation_core::identity::{Id, Kind};
use investigation_store::{Store, Workspace};
use measure::{Invocation, load_average, summarize, time};
use serde_json::{Value, json};

pub const REPORT_VERSION: &str = "investigation-bench.report.v1";

/// Plan §10.5: warm brief/next under 250 ms p95 for 10,000 objects and
/// 50,000 relations; warm mutation under 150 ms p95 excluding file
/// ingestion and render. The sparse choice fixture is measured against the
/// same read target.
pub const READ_TARGET_MS: f64 = 250.0;
pub const MUTATION_TARGET_MS: f64 = 150.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    /// The plan's sizes.
    Full,
    /// Tiny fixtures that prove the harness works in seconds.
    Smoke,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// The `investigation` binary to time.
    pub binary: PathBuf,
    /// Directory that receives the generated workspaces.
    pub fixtures_dir: PathBuf,
    pub scale: Scale,
    /// Timed samples per read and per mutation.
    pub samples: usize,
    /// Untimed runs of each command before its timed samples.
    pub warmup: usize,
    /// Timed samples of index rebuilds and vault renders.
    pub slow_samples: usize,
    /// Skip vault render timings.
    pub skip_render: bool,
    /// Timed seconds per measurement after which sampling stops early,
    /// once `min_samples` (1 for rebuilds and renders) are taken.
    pub budget_seconds: f64,
    /// Fewest timed samples per read and per mutation.
    pub min_samples: usize,
}

impl Config {
    pub fn smoke(binary: PathBuf, fixtures_dir: PathBuf) -> Self {
        Self {
            binary,
            fixtures_dir,
            scale: Scale::Smoke,
            samples: 2,
            warmup: 1,
            slow_samples: 1,
            skip_render: false,
            budget_seconds: 60.0,
            min_samples: 2,
        }
    }
}

const SOFT_CELL_MISSING: &str = "decision.option_criterion_cell_missing";

/// Generate, verify, check and measure; return the report.
pub fn run(config: &Config) -> Result<Value, String> {
    fs::create_dir_all(&config.fixtures_dir).map_err(|error| error.to_string())?;
    let binary = config
        .binary
        .canonicalize()
        .map_err(|error| format!("{}: {error}", config.binary.display()))?;
    let environment = measure::environment(&binary, &config.fixtures_dir);
    let (exhaustive_shape, sparse_shape, large_shape) = match config.scale {
        Scale::Full => (
            ChoiceShape::exhaustive(),
            ChoiceShape::sparse(),
            LargeShape::target(),
        ),
        Scale::Smoke => (
            ChoiceShape::exhaustive(),
            ChoiceShape::smoke_sparse(),
            LargeShape::smoke(),
        ),
    };

    let mut fixtures_out = Vec::new();
    let mut measurements = Vec::new();
    let mut profiles = Vec::new();
    let mut completeness_out = Vec::new();

    let exhaustive = fixtures::choice(
        &config.fixtures_dir.join("exhaustive"),
        "exhaustive_small",
        exhaustive_shape,
    )?;
    fixtures_out.push(describe(&binary, &exhaustive)?);
    completeness_out.push(completeness::check(&binary, &exhaustive, true)?);

    let sparse = fixtures::choice(
        &config.fixtures_dir.join("sparse"),
        "sparse_choice",
        sparse_shape,
    )?;
    fixtures_out.push(describe(&binary, &sparse)?);
    completeness_out.push(completeness::check(&binary, &sparse, false)?);
    let sparse_profile = profile::next_phases(&sparse, config.samples.clamp(1, 10))?;
    measurements.extend(measure_fixture(config, &binary, &sparse, &sparse_profile)?);
    profiles.push(sparse_profile);

    let large = fixtures::large(&config.fixtures_dir.join("large"), large_shape)?;
    fixtures_out.push(describe(&binary, &large)?);
    let large_profile = profile::next_phases(&large, config.samples.clamp(1, 10))?;
    measurements.extend(measure_fixture(config, &binary, &large, &large_profile)?);
    profiles.push(large_profile);

    let misses: Vec<Value> = measurements
        .iter()
        .filter(|item| item["verdict"] == "missed")
        .map(|item| {
            json!({
                "fixture": item["fixture"], "measurement": item["measurement"],
                "p95_ms": item["stats"]["p95_ms"], "target_ms": item["target_ms"],
                "profile_hint": item["profile_hint"],
            })
        })
        .collect();
    let complete = completeness_out.iter().all(|item| item["complete"] == true);
    let mut environment = environment;
    environment["load_average_1_5_15_at_end"] = load_average();
    Ok(json!({
        "schema_version": REPORT_VERSION,
        "scale": match config.scale { Scale::Full => "full", Scale::Smoke => "smoke" },
        "targets": {
            "source": "notes/plan.md §10.5",
            "warm_read_p95_ms": READ_TARGET_MS,
            "warm_mutation_p95_ms": MUTATION_TARGET_MS,
            "read_target_applies_to": "brief, next (top three) and the first page of the gap frontier and of a group drill-down",
            "mutation_target_applies_to": "one record command with one operation, excluding file ingestion and render",
            "no_target": "index rebuild and vault render are reported without a target",
            "note": "Engineering targets for a documented ordinary workstation, not promises of current performance.",
        },
        "environment": environment,
        "method": {
            "timer": "wall clock from spawning the binary to its exit, stdout and stderr piped; includes process start, journal replay, rule evaluation and JSON output",
            "warm": "each fixture is generated, its index is current and every command runs `warmup` untimed times first, so the page cache holds the workspace",
            "samples_per_read": config.samples,
            "samples_per_mutation": config.samples,
            "samples_per_rebuild_and_render": config.slow_samples,
            "warmup_runs_per_command": config.warmup,
            "time_budget_seconds_per_measurement": config.budget_seconds,
            "min_samples_per_read_and_mutation": config.min_samples,
            "early_stop": "sampling of one measurement stops when its timed samples exceed the budget and it has the minimum (1 for rebuilds and renders); stopped_by_time_budget and stats.n say so",
            "trial_order": "per fixture: reads interleaved round-robin (warmup rounds first, then timed rounds in the same command order each round, a command leaving the rotation when it has its samples or has spent its budget); then mutations one after another; then index rebuilds; then vault renders (one first render, then rounds of an unchanged re-render and a render after one untimed mutation). Fixtures run in the order exhaustive (completeness only), sparse, large.",
            "mutations_change_the_head": "each timed mutation commits a new record, so later samples see a slightly larger journal",
            "noise": "The machine is shared with other parallel jobs; samples include their interference. Load averages are recorded at start, end and per measurement. Compare runs only at similar load.",
        },
        "fixtures": fixtures_out,
        "completeness": completeness_out,
        "complete": complete,
        "profiles": profiles,
        "measurements": measurements,
        "misses": misses,
    }))
}

/// Counts that describe a generated workspace, and its `check` result.
fn describe(binary: &Path, fixture: &Fixture) -> Result<Value, String> {
    let workspace = Workspace::open(&fixture.root).map_err(|error| error.to_string())?;
    let mut store = Store::open(workspace);
    let replay = store
        .case(fixture.case)
        .map_err(|failure| failure.error.message.clone())?;
    let mut objects = 0usize;
    let mut by_kind = std::collections::BTreeMap::<String, usize>::new();
    let mut relations = 0usize;
    let mut dependency_edges = 0usize;
    for record in replay.state.records() {
        objects += 1;
        *by_kind
            .entry(record.header.id.kind().prefix().to_owned())
            .or_default() += 1;
        relations += count_references(&record.body, record.header.id);
        dependency_edges += investigation_core::freshness::dependency_edges(record)
            .map(|edges| edges.len())
            .unwrap_or(0);
    }
    let (_, output) = time(
        binary,
        &fixture.root,
        &Invocation::new(&["check", "--format", "json"]),
    )?;
    let check: Value = serde_json::from_slice(&output).map_err(|error| error.to_string())?;
    let errors = check["issues"]
        .as_array()
        .map(|issues| {
            issues
                .iter()
                .filter(|issue| issue["severity"] == "error")
                .count()
        })
        .unwrap_or(0);
    let check_ok = check["ok"] == true && check["structurally_valid"] == true && errors == 0;
    if !check_ok {
        return Err(format!("check failed on {}: {check}", fixture.name));
    }
    Ok(json!({
        "name": fixture.name,
        "objects": objects,
        "objects_by_kind": by_kind,
        "relations": relations,
        "relations_definition": "exact record-to-record revision references in current record bodies (case and transaction IDs excluded)",
        "index_dependency_edges": dependency_edges,
        "journal_events": replay.snapshot.sequence,
        "journal_bytes": directory_bytes(&fixture.root.join("state")),
        "generator_commands": fixture.commands,
        "generation_ms": (fixture.generation_ms * 10.0).round() / 10.0,
        "generated_by": "public store API (Store::record, Store::ingest), at most 128 operations per transaction",
        "check": {"ok": check_ok, "index": check["index"], "issues": check["issues"].as_array().map_or(0, Vec::len)},
        "choice": fixture.choice.as_ref().map(|truth| json!({
            "options": truth.options.len(),
            "criteria": truth.criteria.len(),
            "hard_criteria": truth.hard.len(),
            "possible_cells": truth.options.len() * truth.criteria.len(),
            "recorded_comparisons": truth.recorded.len(),
            "hard_failed_options": truth.hard_failed.len(),
        })),
    }))
}

fn count_references(value: &Value, own: Id) -> usize {
    match value {
        Value::Object(map) => {
            let is_ref = map.len() == 2
                && map.get("revision").is_some_and(Value::is_u64)
                && map
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|text| text.parse::<Id>().ok())
                    .is_some_and(|id| {
                        id != own && !matches!(id.kind(), Kind::Case | Kind::Transaction)
                    });
            usize::from(is_ref)
                + map
                    .values()
                    .map(|child| count_references(child, own))
                    .sum::<usize>()
        }
        Value::Array(items) => items.iter().map(|item| count_references(item, own)).sum(),
        _ => 0,
    }
}

fn directory_bytes(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_bytes(&entry.path()),
            Ok(_) => entry.metadata().map(|meta| meta.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

struct Measured {
    name: &'static str,
    command: Invocation,
    target: Option<f64>,
    samples: Vec<f64>,
    /// Timed samples asked for.
    requested: usize,
    /// Fewest timed samples kept even when the budget runs out.
    minimum: usize,
}

impl Measured {
    fn new(name: &'static str, command: Invocation, target: Option<f64>) -> Self {
        Self {
            name,
            command,
            target,
            samples: Vec::new(),
            requested: 0,
            minimum: 0,
        }
    }

    fn plan(mut self, requested: usize, minimum: usize) -> Self {
        self.requested = requested;
        self.minimum = minimum.clamp(1, requested.max(1));
        self
    }

    /// More samples are wanted: fewer than requested, and either below the
    /// minimum or still inside the time budget.
    fn wants_more(&self, budget_ms: f64) -> bool {
        self.samples.len() < self.requested
            && (self.samples.len() < self.minimum || self.samples.iter().sum::<f64>() < budget_ms)
    }

    fn budget_stopped(&self) -> bool {
        self.samples.len() < self.requested
    }
}

fn record(fixture: &Fixture, measured: &Measured, load: &Value, profile: &Value) -> Value {
    let stats = summarize(&measured.samples);
    let p95 = stats["p95_ms"].as_f64().unwrap_or(f64::NAN);
    let verdict = match measured.target {
        Some(target) if p95 < target => "met",
        Some(_) => "missed",
        None => "no_target",
    };
    let mut argv = vec!["investigation".to_owned()];
    argv.extend(measured.command.args.iter().cloned());
    json!({
        "fixture": fixture.name,
        "measurement": measured.name,
        "argv": argv,
        "target_ms": measured.target,
        "verdict": verdict,
        "stats": stats,
        "requested_samples": measured.requested,
        "stopped_by_time_budget": measured.budget_stopped(),
        "load_average_1_5_15": load,
        "profile_hint": (verdict == "missed").then(|| hint(measured.name, profile)),
    })
}

/// Where to look first after a miss, from this fixture's phase profile.
fn hint(name: &str, profile: &Value) -> String {
    let phases = [
        "replay_ms",
        "enumerate_gaps_ms",
        "rank_next_ms",
        "pareto_ms",
    ];
    let summary = phases
        .iter()
        .map(|phase| format!("{phase} p50 {}", profile[phase]["p50_ms"]))
        .collect::<Vec<_>>()
        .join(", ");
    let largest = phases[..3]
        .iter()
        .max_by(|a, b| {
            let value = |phase: &&&str| profile[**phase]["p50_ms"].as_f64().unwrap_or(0.0);
            value(a).total_cmp(&value(b))
        })
        .copied()
        .unwrap_or("replay_ms");
    match name {
        "record_single_operation" => format!(
            "A commit hashes the whole journal and replays it against the kept verified head (ADR 0010), verifying the tail in full, then fsyncs; profile Store::record against the replay cost ({summary})."
        ),
        "brief" => format!(
            "brief also enumerates and ranks gaps and derives effective state per outcome and evidence record; profile brief assembly beyond these phases ({summary})."
        ),
        _ => format!("Largest in-process phase: {largest} ({summary}); profile it first."),
    }
}

fn measure_fixture(
    config: &Config,
    binary: &Path,
    fixture: &Fixture,
    profile: &Value,
) -> Result<Vec<Value>, String> {
    let case = fixture.case.to_string();
    let budget_ms = config.budget_seconds * 1e3;
    let read = |name, args: &[&str]| {
        Measured::new(name, Invocation::new(args), Some(READ_TARGET_MS))
            .plan(config.samples, config.min_samples)
    };
    let mut reads = vec![
        read("brief", &["--case", &case, "brief", "--format", "json"]),
        read(
            "next_top_three",
            &["--case", &case, "next", "--format", "json"],
        ),
        read(
            "frontier_first_page",
            &["--case", &case, "frontier", "--format", "json"],
        ),
    ];
    if let Some(truth) = &fixture.choice {
        let criterion = truth.criteria.last().expect("criteria").to_string();
        reads.push(read(
            "group_drill_down_first_page",
            &[
                "--case",
                &case,
                "frontier",
                "--rule",
                SOFT_CELL_MISSING,
                "--reads",
                &criterion,
                "--format",
                "json",
            ],
        ));
    }
    let mut out = Vec::new();
    for _ in 0..config.warmup {
        for read in &reads {
            time(binary, &fixture.root, &read.command)?;
        }
    }
    // Round-robin; a command leaves the rotation when it has its samples or
    // has used its time budget with at least the minimum.
    while reads.iter().any(|read| read.wants_more(budget_ms)) {
        for read in &mut reads {
            if read.wants_more(budget_ms) {
                read.samples
                    .push(time(binary, &fixture.root, &read.command)?.0);
            }
        }
    }
    let load = load_average();
    for read in &reads {
        out.push(record(fixture, read, &load, profile));
    }

    // Single-operation mutations, each a new command.
    let mutation_count = config.warmup + config.samples + config.slow_samples;
    let mut mutations = mutation_inputs(fixture, mutation_count)?.into_iter();
    let record_command = Invocation::new(&["record", "--input", "-", "--format", "json"]);
    let mut mutate = || -> Result<f64, String> {
        let input = mutations.next().ok_or("ran out of mutations")?;
        Ok(time(
            binary,
            &fixture.root,
            &record_command.clone().with_stdin(input),
        )?
        .0)
    };
    let mut measured = Measured::new(
        "record_single_operation",
        record_command.clone(),
        Some(MUTATION_TARGET_MS),
    )
    .plan(config.samples, config.min_samples);
    for _ in 0..config.warmup {
        mutate()?;
    }
    while measured.wants_more(budget_ms) {
        measured.samples.push(mutate()?);
    }
    out.push(record(fixture, &measured, &load_average(), profile));

    let mut rebuild = Measured::new(
        "index_rebuild",
        Invocation::new(&["repair", "index", "--format", "json"]),
        None,
    )
    .plan(config.slow_samples, 1);
    time(binary, &fixture.root, &rebuild.command)?;
    while rebuild.wants_more(budget_ms) {
        rebuild
            .samples
            .push(time(binary, &fixture.root, &rebuild.command)?.0);
    }
    out.push(record(fixture, &rebuild, &load_average(), profile));

    if !config.skip_render {
        let render = Invocation::new(&["vault", "render", "--format", "json"]);
        let mut first = Measured::new("vault_render_first", render.clone(), None).plan(1, 1);
        first.samples.push(time(binary, &fixture.root, &render)?.0);
        out.push(record(fixture, &first, &load_average(), profile));
        let mut unchanged = Measured::new("vault_render_unchanged", render.clone(), None)
            .plan(config.slow_samples, 1);
        let mut after = Measured::new("vault_render_after_one_mutation", render.clone(), None)
            .plan(config.slow_samples, 1);
        while unchanged.wants_more(budget_ms) || after.wants_more(budget_ms) {
            unchanged
                .samples
                .push(time(binary, &fixture.root, &render)?.0);
            mutate()?;
            after.samples.push(time(binary, &fixture.root, &render)?.0);
        }
        let load = load_average();
        out.push(record(fixture, &unchanged, &load, profile));
        out.push(record(fixture, &after, &load, profile));
    }
    Ok(out)
}

/// Distinct single-operation commands: a Comparison on a free cell for a
/// choice fixture, a new hypothesis otherwise.
fn mutation_inputs(fixture: &Fixture, count: usize) -> Result<Vec<Vec<u8>>, String> {
    let operations: Vec<Value> = match &fixture.choice {
        Some(truth) => {
            let cells = truth.free_cells(count);
            if cells.len() < count {
                return Err(format!(
                    "{}: only {} free cells for {count} mutations",
                    fixture.name,
                    cells.len()
                ));
            }
            cells
                .into_iter()
                .map(|(option, criterion)| {
                    json!({"op": "comparison.add", "payload": {
                        "option_ref": fixture.reference(option),
                        "criterion_ref": fixture.reference(criterion),
                        "outcome": "meets", "method": "Synthetic benchmark",
                        "value": {"decimal": truth.evidence.1, "unit": "ms"},
                        "evidence_refs": [fixture.reference(truth.evidence.0)],
                        "assumption_refs": [], "uncertainty": "Synthetic",
                        "rationale": "Measured mutation"}})
                })
                .collect()
        }
        None => (0..count)
            .map(|index| {
                json!({"op": "hypothesis.create", "payload": {
                    "question_ref": fixture.reference(fixture.question),
                    "statement": format!("Measured hypothesis {index}."),
                    "possible_falsifiers": [], "rival_refs": [], "assumption_refs": []}})
            })
            .collect(),
    };
    operations
        .into_iter()
        .enumerate()
        .map(|(index, operation)| {
            serde_json::to_vec(&json!({
                "schema_version": "inv.command.v1",
                "command_id": format!("bench-mutation-{index}"),
                "case_id": fixture.case, "actor": "agent:bench-mutation", "preconditions": [],
                "operations": [operation],
            }))
            .map_err(|error| error.to_string())
        })
        .collect()
}
