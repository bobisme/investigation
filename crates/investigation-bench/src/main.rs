//! `investigation-bench`: run the §10.5 benchmarks and write a JSON report.
//!
//! Usually run through `just bench`, which builds the release binaries first.

use std::{path::PathBuf, process::ExitCode};

use investigation_bench::{Config, Scale, run};

const USAGE: &str = "\
Usage: investigation-bench [OPTIONS]

Generates the large (10,000 objects / 50,000 relations), sparse (1,000 x 200)
and exhaustive small choice fixtures, checks grouped `next` completeness, times
the investigation binary against the plan's §10.5 targets and writes a JSON
report. A target miss is reported, not an error; a completeness failure exits 1.

Options:
  --bin <PATH>          investigation binary to time [default: next to this binary]
  --out <PATH>          report path, or - for stdout [default: target/bench/report.json]
  --fixtures <DIR>      keep the generated workspaces here [default: a removed temp dir]
  --samples <N>         timed samples per read and per mutation [default: 30]
  --warmup <N>          untimed runs per command first [default: 3]
  --slow-samples <N>    timed index rebuilds and renders [default: 5]
  --smoke               tiny fixtures and 2 samples, to check the harness works
  --budget <SECONDS>    stop sampling one measurement after this much timed
                        work once it has its minimum samples [default: 60]
  --min-samples <N>     fewest timed samples per read and mutation [default: 5]
  --skip-render         do not time vault renders
  --load                run the vault load test (plan §12.5) instead: about 50,000
                        notes over many cases, with and without the viewer
                        features; needs --fixtures DIR, writes
                        target/bench/load-report.json, and uses --smoke for a
                        tiny run, --samples for repetitions [default: 5 here]
                        and --seed
  --seed <N>            load-test generator seed [default: 20261123]
  --cases <N>           load-test case count [default: 650, about 50,000 notes]
  -h, --help            print this help
";

fn main() -> ExitCode {
    match main_inner() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("investigation-bench: {message}");
            ExitCode::from(2)
        }
    }
}

fn number(value: Option<String>, flag: &str) -> Result<usize, String> {
    value
        .ok_or(format!("{flag} needs a value"))?
        .parse()
        .map_err(|_| format!("{flag} needs a whole number"))
}

fn main_inner() -> Result<ExitCode, String> {
    let mut binary = None;
    let mut out = PathBuf::from("target/bench/report.json");
    let mut fixtures = None;
    let mut samples = 30;
    let mut warmup = 3;
    let mut slow_samples = 5;
    let mut smoke = false;
    let mut skip_render = false;
    let mut budget_seconds = 60.0;
    let mut min_samples = 5;
    let mut load = false;
    let mut cases = None;
    let mut seed = 20_261_123u64;
    let mut samples_given = false;
    let mut out_given = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bin" => binary = Some(PathBuf::from(args.next().ok_or("--bin needs a path")?)),
            "--out" => {
                out = PathBuf::from(args.next().ok_or("--out needs a path")?);
                out_given = true;
            }
            "--load" => load = true,
            "--cases" => cases = Some(number(args.next(), "--cases")?),
            "--seed" => seed = number(args.next(), "--seed")? as u64,
            "--fixtures" => {
                fixtures = Some(PathBuf::from(args.next().ok_or("--fixtures needs a path")?));
            }
            "--samples" => {
                samples = number(args.next(), "--samples")?;
                samples_given = true;
            }
            "--warmup" => warmup = number(args.next(), "--warmup")?,
            "--slow-samples" => slow_samples = number(args.next(), "--slow-samples")?,
            "--smoke" => smoke = true,
            "--skip-render" => skip_render = true,
            "--min-samples" => min_samples = number(args.next(), "--min-samples")?,
            "--budget" => {
                budget_seconds = args
                    .next()
                    .ok_or("--budget needs a value")?
                    .parse()
                    .map_err(|_| "--budget needs a number of seconds")?;
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }
    let binary = match binary {
        Some(path) => path,
        None => std::env::current_exe()
            .map_err(|error| error.to_string())?
            .with_file_name(format!("investigation{}", std::env::consts::EXE_SUFFIX)),
    };
    if !binary.is_file() {
        return Err(format!(
            "{} does not exist; build it (just bench does) or pass --bin",
            binary.display()
        ));
    }
    if load {
        let directory =
            fixtures.ok_or("--load needs --fixtures DIR for the generated workspaces")?;
        let repetitions = if samples_given { samples } else { 5 };
        let report = investigation_bench::load::run_load(&investigation_bench::load::LoadConfig {
            binary,
            directory,
            shape: {
                let mut shape = if smoke {
                    investigation_bench::fixtures::LoadShape::smoke()
                } else {
                    investigation_bench::fixtures::LoadShape::target()
                };
                if let Some(cases) = cases {
                    shape.cases = cases;
                }
                shape
            },
            seed,
            repetitions,
        })?;
        let out = if out_given {
            out
        } else {
            PathBuf::from("target/bench/load-report.json")
        };
        return write_load_report(&report, &out);
    }
    let temporary;
    let fixtures_dir = match fixtures {
        Some(path) => {
            if path.exists() && path.read_dir().map_err(|e| e.to_string())?.next().is_some() {
                return Err(format!("{} must be absent or empty", path.display()));
            }
            path
        }
        None => {
            temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
            temporary.path().to_path_buf()
        }
    };
    let mut config = Config {
        binary,
        fixtures_dir,
        scale: Scale::Full,
        samples,
        warmup,
        slow_samples,
        skip_render,
        budget_seconds,
        min_samples,
    };
    if smoke {
        config = Config {
            skip_render,
            ..Config::smoke(config.binary, config.fixtures_dir)
        };
    }
    if config.samples == 0 || config.slow_samples == 0 {
        return Err("--samples and --slow-samples must be at least 1".into());
    }
    let report = run(&config)?;
    let text = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())? + "\n";
    if out.as_os_str() == "-" {
        print!("{text}");
    } else {
        if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(&out, &text).map_err(|error| format!("{}: {error}", out.display()))?;
        eprintln!("report: {}", out.display());
    }
    for item in report["measurements"].as_array().into_iter().flatten() {
        eprintln!(
            "{:<16} {:<34} n={:<3} p50={:>9.1} ms  p95={:>9.1} ms  {}",
            item["fixture"].as_str().unwrap_or_default(),
            item["measurement"].as_str().unwrap_or_default(),
            item["stats"]["n"],
            item["stats"]["p50_ms"].as_f64().unwrap_or(f64::NAN),
            item["stats"]["p95_ms"].as_f64().unwrap_or(f64::NAN),
            match item["target_ms"].as_f64() {
                Some(target) => format!(
                    "{} (target {target} ms)",
                    item["verdict"].as_str().unwrap_or_default()
                ),
                None => "no target".into(),
            }
        );
    }
    eprintln!(
        "load average at start {} / end {}",
        report["environment"]["load_average_1_5_15_at_start"],
        report["environment"]["load_average_1_5_15_at_end"]
    );
    if report["complete"] != true {
        eprintln!("completeness FAILED: {}", report["completeness"]);
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

fn write_load_report(
    report: &serde_json::Value,
    out: &std::path::Path,
) -> Result<ExitCode, String> {
    let text = serde_json::to_string_pretty(report).map_err(|error| error.to_string())? + "\n";
    if out.as_os_str() == "-" {
        print!("{text}");
    } else {
        if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(out, &text).map_err(|error| format!("{}: {error}", out.display()))?;
        eprintln!("report: {}", out.display());
    }
    eprintln!(
        "{} objects over {} cases",
        report["workspace"]["objects"], report["workspace"]["cases"]
    );
    for variant in report["variants"].as_array().into_iter().flatten() {
        let files = variant["measurements"][0]["extra"]["vault"]["files"].clone();
        eprintln!("== {} ({files} vault files)", variant["variant"]);
        for item in variant["measurements"].as_array().into_iter().flatten() {
            eprintln!(
                "{:<36} n={:<2} p50={:>10.1} ms  max={:>10.1} ms  rss={} MiB  {}",
                item["measurement"].as_str().unwrap_or_default(),
                item["stats"]["n"],
                item["stats"]["p50_ms"].as_f64().unwrap_or(f64::NAN),
                item["stats"]["max_ms"].as_f64().unwrap_or(f64::NAN),
                item["peak_rss_mib_max"],
                match item["target_ms"].as_f64() {
                    Some(target) => format!(
                        "{} (target {target} ms)",
                        item["verdict"].as_str().unwrap_or_default()
                    ),
                    None => String::new(),
                }
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}
