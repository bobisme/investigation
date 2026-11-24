//! Vault load test (plan §12.5): a workspace with tens of thousands of owned
//! notes, measured with and without the optional viewer features.
//!
//! One seed gives one workspace. It is generated once through the public store
//! API, then copied for each variant: `baseline` (plain Markdown) and
//! `viewer_1_15_1` (`[vault_policy] viewer_release = "1.15.1"`, which plans
//! Bases and the case map). Every measurement spawns the real `investigation`
//! binary under `/usr/bin/time -f %M`, so each sample has wall time and peak
//! resident set size. The load test only runs from `just load-test`, never in
//! CI; it needs GNU `time` for memory and says so in the report if it is absent.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use serde_json::{Value, json};

use crate::{
    fixtures::{self, LoadShape, LoadWorkspace},
    measure::{environment, load_average, summarize},
};

pub const LOAD_REPORT_VERSION: &str = "investigation-bench.load-report.v1";
/// The release the viewer variant states; the first one Bases and Map support.
pub const VIEWER_RELEASE: &str = "1.15.1";
/// Plan §10.5 targets, applied here as observations at a larger scale.
const READ_TARGET_MS: f64 = 250.0;
const MUTATION_TARGET_MS: f64 = 150.0;
/// PERF.10 budget for a render with nothing changed (ADR 0012): wall time
/// and peak memory, per planned note, with a floor for small vaults. At
/// 49,406 notes that is about 2.96 s and 803 MiB; before PERF.10 it took
/// about 5.1 s and 0.95 GiB.
const UNCHANGED_RENDER_MS_PER_NOTE: f64 = 0.06;
const UNCHANGED_RENDER_FLOOR_MS: f64 = 250.0;
const UNCHANGED_RENDER_KIB_PER_NOTE: u64 = 16;
const UNCHANGED_RENDER_FLOOR_MIB: u64 = 32;

/// The PERF.10 budget of an unchanged render of `notes` planned notes:
/// milliseconds and MiB of peak resident memory.
fn unchanged_render_budget(notes: u64) -> (f64, u64) {
    let ms = (notes as f64 * UNCHANGED_RENDER_MS_PER_NOTE).max(UNCHANGED_RENDER_FLOOR_MS);
    let mib = notes * UNCHANGED_RENDER_KIB_PER_NOTE / 1024 + UNCHANGED_RENDER_FLOOR_MIB;
    (ms, mib)
}
/// Generated notes edited and deleted to create drift. Edited paths are the
/// ones `vault reconcile --dry-run` is asked about (its limit is 100).
const DRIFT_EDITED: usize = 90;
const DRIFT_DELETED: usize = 10;

#[derive(Clone, Debug)]
pub struct LoadConfig {
    pub binary: PathBuf,
    /// Directory that receives the workspace and its variants; absent or empty.
    pub directory: PathBuf,
    pub shape: LoadShape,
    pub seed: u64,
    /// Timed repetitions of each repeatable measurement.
    pub repetitions: usize,
}

struct Sample {
    ms: f64,
    rss_kib: Option<u64>,
    stdout: Vec<u8>,
}

fn gnu_time() -> bool {
    Path::new("/usr/bin/time").is_file()
}

/// Run the binary once under GNU time; a non-zero exit is an error.
fn run(
    binary: &Path,
    workspace: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<Sample, String> {
    let rss_file = workspace.with_extension("rss");
    let mut command = if gnu_time() {
        let mut command = Command::new("/usr/bin/time");
        command.args(["-f", "%M", "-o"]).arg(&rss_file).arg(binary);
        command
    } else {
        Command::new(binary)
    };
    command
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .current_dir(workspace)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_READ_ONLY")
        .env_remove("INVESTIGATION_LOG")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let started = Instant::now();
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .ok_or("no stdin pipe")?
            .write_all(input)
            .map_err(|error| error.to_string())?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    let ms = started.elapsed().as_secs_f64() * 1e3;
    let rss_kib = fs::read_to_string(&rss_file).ok().and_then(|text| {
        text.lines()
            .last()
            .and_then(|line| line.trim().parse().ok())
    });
    let _ = fs::remove_file(&rss_file);
    if !output.status.success() {
        return Err(format!(
            "investigation {} exited {:?}: {}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout)
                .chars()
                .chain(String::from_utf8_lossy(&output.stderr).chars())
                .take(800)
                .collect::<String>()
        ));
    }
    Ok(Sample {
        ms,
        rss_kib,
        stdout: output.stdout,
    })
}

struct Series {
    name: String,
    argv: Vec<String>,
    target_ms: Option<f64>,
    note: &'static str,
    samples: Vec<f64>,
    rss: Vec<u64>,
}

impl Series {
    fn new(name: &str, args: &[&str], target_ms: Option<f64>, note: &'static str) -> Self {
        let mut argv = vec!["investigation".to_owned()];
        argv.extend(args.iter().map(|arg| (*arg).to_owned()));
        Self {
            name: name.to_owned(),
            argv,
            target_ms,
            note,
            samples: Vec::new(),
            rss: Vec::new(),
        }
    }

    fn push(&mut self, sample: &Sample) {
        self.samples.push(sample.ms);
        self.rss.extend(sample.rss_kib);
    }

    fn json(&self, extra: Value) -> Value {
        let stats = summarize(&self.samples);
        let p95 = stats["p95_ms"].as_f64().unwrap_or(f64::NAN);
        let verdict = match self.target_ms {
            Some(target) if p95 < target => "met",
            Some(_) => "missed",
            None => "no_target",
        };
        let mut sorted = self.rss.clone();
        sorted.sort_unstable();
        let rss_p50 = sorted
            .get(sorted.len().saturating_sub(1) / 2)
            .map(|kib| kib / 1024);
        json!({
            "measurement": self.name, "argv": self.argv, "note": self.note,
            "target_ms": self.target_ms, "verdict": verdict, "stats": stats,
            "peak_rss_mib_max": self.rss.iter().max().map(|kib| kib / 1024),
            "peak_rss_mib_p50": rss_p50,
            "extra": extra,
        })
    }
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|error| format!("{}: {error}", to.display()))?;
    for entry in fs::read_dir(from).map_err(|error| format!("{}: {error}", from.display()))? {
        let entry = entry.map_err(|error| error.to_string())?;
        let target = to.join(entry.file_name());
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// Markdown and other files under `path`, sorted, with sizes.
fn walk(path: &Path, out: &mut Vec<(PathBuf, u64)>) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => walk(&entry.path(), out),
            Ok(_) => out.push((entry.path(), entry.metadata().map_or(0, |meta| meta.len()))),
            Err(_) => {}
        }
    }
}

fn vault_files(workspace: &Path) -> Vec<(PathBuf, u64)> {
    let mut files = Vec::new();
    walk(&workspace.join("vault"), &mut files);
    files.sort();
    files
}

fn vault_size(workspace: &Path) -> Value {
    let files = vault_files(workspace);
    let vault = workspace.join("vault");
    let mut by_extension = std::collections::BTreeMap::<String, (usize, u64)>::new();
    for (path, bytes) in &files {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("none")
            .to_owned();
        let entry = by_extension.entry(extension).or_default();
        entry.0 += 1;
        entry.1 += bytes;
    }
    let manifest =
        fs::metadata(workspace.join("local/render-manifest.json")).map_or(0, |m| m.len());
    let largest = files.iter().map(|(_, bytes)| *bytes).max().unwrap_or(0);
    let largest_directory = {
        let mut counts = std::collections::BTreeMap::<PathBuf, usize>::new();
        for (path, _) in &files {
            if let Some(parent) = path.parent() {
                *counts.entry(parent.to_path_buf()).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(path, count)| {
                json!({"path": path.strip_prefix(&vault).unwrap_or(&path).display().to_string(), "files": count})
            })
    };
    json!({
        "files": files.len(),
        "bytes": files.iter().map(|(_, bytes)| bytes).sum::<u64>(),
        "by_extension": by_extension.iter().map(|(extension, (count, bytes))| (extension.clone(), json!({"files": count, "bytes": bytes}))).collect::<serde_json::Map<_, _>>(),
        "largest_file_bytes": largest,
        "most_populated_directory": largest_directory,
        "render_manifest_bytes": manifest,
    })
}

fn json_of(sample: &Sample) -> Result<Value, String> {
    serde_json::from_slice(&sample.stdout).map_err(|error| error.to_string())
}

/// Paths to edit and to delete: generated Markdown notes under `Cases/`,
/// spread evenly over the sorted listing, so the choice depends only on the
/// vault bytes.
fn drift_targets(workspace: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let notes: Vec<PathBuf> = vault_files(workspace)
        .into_iter()
        .map(|(path, _)| path)
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "md")
                && path
                    .strip_prefix(workspace.join("vault"))
                    .is_ok_and(|rel| rel.starts_with("Cases"))
        })
        .collect();
    let total = DRIFT_EDITED + DRIFT_DELETED;
    let step = (notes.len() / total.max(1)).max(1);
    let picked: Vec<PathBuf> = notes.into_iter().step_by(step).take(total).collect();
    let (edited, deleted) = picked.split_at(DRIFT_EDITED.min(picked.len()));
    (edited.to_vec(), deleted.to_vec())
}

/// Measure one variant on its own copy of the workspace.
fn measure_variant(
    config: &LoadConfig,
    generated: &LoadWorkspace,
    name: &str,
    release: Option<&str>,
) -> Result<Value, String> {
    let root = config.directory.join(name);
    copy_tree(&generated.root, &root)?;
    if let Some(release) = release {
        // `[vault_policy]` is the last table in a fresh workspace config.
        let path = root.join("investigation.toml");
        let mut text = fs::read_to_string(&path).map_err(|error| error.to_string())?;
        text.push_str(&format!("viewer_release = \"{release}\"\n"));
        fs::write(&path, text).map_err(|error| error.to_string())?;
    }
    let binary = &config.binary;
    let reps = config.repetitions.max(1);
    let case = generated.cases[0].to_string();
    let mut out = Vec::new();
    let mut load = |series: Series, extra: Value| {
        let item = series.json(extra);
        // Progress as it happens: a long run that is stopped keeps these lines.
        eprintln!(
            "[{name}] {:<36} n={} p50={} ms rss={} MiB",
            item["measurement"].as_str().unwrap_or_default(),
            item["stats"]["n"],
            item["stats"]["p50_ms"],
            item["peak_rss_mib_max"]
        );
        out.push(item);
    };

    // 1. Full render of a vault that does not exist yet.
    let mut full = Series::new(
        "vault_render_full",
        &["vault", "render", "--format", "json"],
        None,
        "first render: every note written",
    );
    let sample = run(
        binary,
        &root,
        &["vault", "render", "--format", "json"],
        None,
    )?;
    full.push(&sample);
    let report = json_of(&sample)?;
    let counts = report["counts"].clone();
    let size = vault_size(&root);
    load(full, json!({"render_counts": counts, "vault": size}));

    // 2. Re-render with nothing changed, against the PERF.10 budget.
    let notes = counts["planned"].as_u64().unwrap_or(0);
    let (budget_ms, budget_mib) = unchanged_render_budget(notes);
    let mut unchanged = Series::new(
        "vault_render_unchanged",
        &["vault", "render", "--format", "json"],
        Some(budget_ms),
        "same snapshot, nothing edited; target and rss_budget_mib are the PERF.10 budget for this many notes",
    );
    for _ in 0..reps {
        unchanged.push(&run(
            binary,
            &root,
            &["vault", "render", "--format", "json"],
            None,
        )?);
    }
    let peak = unchanged.rss.iter().max().map(|kib| kib / 1024);
    load(
        unchanged,
        json!({
            "planned_notes": notes,
            "rss_budget_mib": budget_mib,
            "rss_verdict": match peak {
                Some(peak) if peak <= budget_mib => "met",
                Some(_) => "missed",
                None => "not_measured",
            },
        }),
    );

    // 3. Status of a current vault.
    let mut status = Series::new(
        "vault_status_current",
        &["vault", "status", "--format", "json"],
        None,
        "no drift, snapshot current",
    );
    for _ in 0..reps {
        status.push(&run(
            binary,
            &root,
            &["vault", "status", "--format", "json"],
            None,
        )?);
    }
    load(status, Value::Null);

    // 4. One record changes, then the vault renders again.
    let (question, case_id) = generated.mutation_target;
    let mut record = Series::new(
        "record_single_operation",
        &["record", "--input", "-", "--format", "json"],
        Some(MUTATION_TARGET_MS),
        "one hypothesis.create, at this workspace's size (plan target is for 10,000 objects)",
    );
    let mut stale = Series::new(
        "vault_status_one_record_stale",
        &["vault", "status", "--format", "json"],
        None,
        "snapshot one commit ahead of the render",
    );
    let mut after = Series::new(
        "vault_render_one_record_changed",
        &["vault", "render", "--format", "json"],
        None,
        "render after one record command",
    );
    let mut written = Vec::new();
    for index in 0..reps {
        let command = json!({
            "schema_version": "inv.command.v1", "command_id": format!("load-mutation-{index}"),
            "case_id": case_id, "actor": "agent:bench-load", "preconditions": [],
            "operations": [{"op": "hypothesis.create", "payload": {
                "question_ref": {"id": question, "revision": generated.revisions[&question]},
                "statement": format!("Load test hypothesis {index}."),
                "possible_falsifiers": [], "rival_refs": [], "assumption_refs": []}}],
        });
        let bytes = serde_json::to_vec(&command).map_err(|error| error.to_string())?;
        record.push(&run(
            binary,
            &root,
            &["record", "--input", "-", "--format", "json"],
            Some(&bytes),
        )?);
        stale.push(&run(
            binary,
            &root,
            &["vault", "status", "--format", "json"],
            None,
        )?);
        let sample = run(
            binary,
            &root,
            &["vault", "render", "--format", "json"],
            None,
        )?;
        after.push(&sample);
        let counts = json_of(&sample)?["counts"].clone();
        written.push(json!({"written": counts["written"], "unchanged": counts["unchanged"]}));
    }
    load(record, Value::Null);
    load(stale, Value::Null);
    load(after, json!({"render_counts_per_repetition": written}));

    // 5. Warm reads of one case in the large workspace.
    for (name, args) in [
        (
            "brief_one_case",
            vec!["--case", &case, "brief", "--format", "json"],
        ),
        (
            "next_one_case",
            vec!["--case", &case, "next", "--format", "json"],
        ),
    ] {
        let mut series = Series::new(
            name,
            &args,
            Some(READ_TARGET_MS),
            "one case of many; plan target is for 10,000 objects in one case",
        );
        run(binary, &root, &args, None)?;
        for _ in 0..reps {
            series.push(&run(binary, &root, &args, None)?);
        }
        load(series, Value::Null);
    }

    // 6. Drift: edit and delete generated notes outside the renderer.
    let (edited, deleted) = drift_targets(&root);
    for path in &edited {
        let mut text = fs::read_to_string(path).map_err(|error| error.to_string())?;
        text.push_str("\nEdited by hand in the load test.\n");
        fs::write(path, text).map_err(|error| error.to_string())?;
    }
    for path in &deleted {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    let vault = root.join("vault");
    let relative: Vec<String> = edited
        .iter()
        .map(|path| {
            path.strip_prefix(&vault)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect();
    let mut before = Series::new(
        "vault_status_edited_before_render",
        &["vault", "status", "--format", "json"],
        None,
        "edits made, no render since: status reads the manifest and snapshot heads, so it does not see them",
    );
    let mut seen = Value::Null;
    for _ in 0..reps {
        let sample = run(
            binary,
            &root,
            &["vault", "status", "--format", "json"],
            None,
        )?;
        let status = json_of(&sample)?;
        seen = json!({"drift_count": status["drift_count"], "drift_kinds": status["drift_kinds"]});
        before.push(&sample);
    }
    load(
        before,
        json!({"edited": edited.len(), "deleted": deleted.len(), "reported": seen}),
    );
    let mut detect = Series::new(
        "vault_render_detecting_drift",
        &["vault", "render", "--format", "json"],
        None,
        "first render after the edits: detects and records drift, leaves the files as found, and completes",
    );
    let sample = run(
        binary,
        &root,
        &["vault", "render", "--format", "json"],
        None,
    )?;
    detect.push(&sample);
    let report = json_of(&sample)?;
    load(
        detect,
        json!({"counts": report["counts"], "drift_listed": report["drift"].as_array().map(Vec::len), "state": report["state"]}),
    );
    let mut after = Series::new(
        "vault_status_with_drift",
        &["vault", "status", "--format", "json"],
        None,
        "drift recorded by the render above",
    );
    let mut recorded = Value::Null;
    for _ in 0..reps {
        let sample = run(
            binary,
            &root,
            &["vault", "status", "--format", "json"],
            None,
        )?;
        let status = json_of(&sample)?;
        recorded =
            json!({"drift_count": status["drift_count"], "drift_kinds": status["drift_kinds"]});
        after.push(&sample);
    }
    load(after, json!({"reported": recorded}));
    let mut conflicts = Series::new(
        "vault_conflicts_with_drift",
        &["vault", "conflicts", "--format", "json"],
        None,
        "lists drifted paths (capped) and where versions are kept",
    );
    for _ in 0..reps {
        conflicts.push(&run(
            binary,
            &root,
            &["vault", "conflicts", "--format", "json"],
            None,
        )?);
    }
    load(conflicts, Value::Null);
    let mut args = vec!["vault", "reconcile"];
    args.extend(relative.iter().map(String::as_str));
    args.extend([
        "--disposition",
        "preserve-as-note",
        "--dry-run",
        "--format",
        "json",
    ]);
    let mut plan = Series::new(
        "vault_reconcile_dry_run",
        &[
            "vault",
            "reconcile",
            "<edited paths>",
            "--disposition",
            "preserve-as-note",
            "--dry-run",
        ],
        None,
        "planning only: writes nothing",
    );
    let mut planned = Value::Null;
    for _ in 0..reps {
        let sample = run(binary, &root, &args, None)?;
        planned = json_of(&sample)?
            .get("counts")
            .cloned()
            .unwrap_or(Value::Null);
        plan.push(&sample);
    }
    load(plan, json!({"paths": relative.len(), "counts": planned}));

    // 7. Index rebuild, for the resident-memory and time of the journal replay.
    let mut rebuild = Series::new(
        "index_rebuild",
        &["repair", "index", "--format", "json"],
        None,
        "journal replay into the derived index",
    );
    rebuild.push(&run(
        binary,
        &root,
        &["repair", "index", "--format", "json"],
        None,
    )?);
    load(rebuild, Value::Null);

    Ok(json!({
        "variant": name,
        "viewer_release": release,
        "final_vault": vault_size(&root),
        "measurements": out,
    }))
}

pub fn run_load(config: &LoadConfig) -> Result<Value, String> {
    if config.directory.exists()
        && config
            .directory
            .read_dir()
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err(format!(
            "{} must be absent or empty",
            config.directory.display()
        ));
    }
    fs::create_dir_all(&config.directory).map_err(|error| error.to_string())?;
    let binary = config
        .binary
        .canonicalize()
        .map_err(|error| format!("{}: {error}", config.binary.display()))?;
    let config = LoadConfig {
        binary: binary.clone(),
        ..config.clone()
    };
    let environment = environment(&binary, &config.directory);
    let generated = fixtures::load(
        &config.directory.join("generated"),
        config.shape,
        config.seed,
    )?;
    let generated = LoadWorkspace {
        root: generated.root.clone(),
        ..generated
    };
    let journal_bytes = {
        let mut files = Vec::new();
        walk(&generated.root.join("state"), &mut files);
        files.iter().map(|(_, bytes)| bytes).sum::<u64>()
    };
    let check = run(
        &binary,
        &generated.root,
        &["check", "--format", "json"],
        None,
    )?;
    let check = json_of(&check)?;
    if check["ok"] != true {
        return Err(format!("check failed on the load workspace: {check}"));
    }
    // Baseline first, then the viewer features; a run order effect (page cache)
    // would favour the second, and the report says so.
    let variants = vec![
        measure_variant(&config, &generated, "baseline", None)?,
        measure_variant(&config, &generated, "viewer_1_15_1", Some(VIEWER_RELEASE))?,
    ];
    let mut environment = environment;
    environment["load_average_1_5_15_at_end"] = load_average();
    environment["gnu_time_available"] = json!(gnu_time());
    let report = json!({
        "schema_version": LOAD_REPORT_VERSION,
        "plan_refs": ["notes/plan.md §12.5", "notes/plan.md §10.5"],
        "seed": config.seed,
        "workspace": {
            "cases": generated.cases.len(),
            "objects": generated.objects,
            "generator_commands": generated.commands,
            "generation_ms": (generated.generation_ms * 10.0).round() / 10.0,
            "journal_bytes": journal_bytes,
            "shape": format!("{:?}", config.shape),
            "check_ok": check["ok"],
        },
        "environment": environment,
        "method": {
            "timer": "wall clock from spawning /usr/bin/time (which runs the binary) to exit; peak RSS is GNU time's %M (maximum resident set size of the process)",
            "repetitions": config.repetitions,
            "order": "baseline variant first, then viewer_1_15_1, each on its own copy of one generated workspace. Within a variant: full render, unchanged renders, status, then repetitions of (record, status, render), warm reads, drift (edit then delete generated notes, status, conflicts, reconcile dry-run, render), index rebuild",
            "first_render": "n=1 by nature: a second full render needs a fresh vault",
            "warmth": "the workspace was just written, so the page cache is warm; cold-cache and Obsidian-side measurements are not made here",
            "targets": "notes/plan.md §10.5 gives warm read and mutation targets for 10,000 objects; the plan has no render or status target. vault_render_unchanged carries the PERF.10 budget instead: 0.06 ms per planned note (at least 250 ms) and 16 KiB of peak RSS per planned note plus 32 MiB. Other render and status measurements carry none. A verdict at this scale is an observation, not a failure of the plan",
            "noise": "The machine may be shared; load averages are recorded at start and end. Compare runs only at similar load.",
        },
        "variants": variants,
        "scope_note": "Obsidian-side behaviour (cold open, Base query, Canvas open, search) cannot be measured by this harness; see docs/vault-load.md for how that is recorded.",
    });
    // The generated workspaces stay in `directory` for inspection; the caller
    // removes them.
    Ok(report)
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn the_unchanged_render_budget_scales_with_planned_notes_above_a_floor() {
        assert_eq!(unchanged_render_budget(0), (250.0, 32));
        let (ms, mib) = unchanged_render_budget(49_406);
        assert!((ms - 2_964.36).abs() < 1e-6, "{ms}");
        assert_eq!(mib, 803);
    }
}
