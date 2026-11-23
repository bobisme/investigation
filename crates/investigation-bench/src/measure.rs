//! Wall-clock timing of the real binary, percentiles and the run environment.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::Instant,
};

use serde_json::{Value, json};

/// One invocation of the `investigation` binary.
#[derive(Clone, Debug)]
pub struct Invocation {
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    /// Prepend `--workspace <fixture>`; off for argv the CLI handed back,
    /// which already names its workspace.
    pub add_workspace: bool,
}

impl Invocation {
    pub fn new(args: &[&str]) -> Self {
        Self {
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            stdin: None,
            add_workspace: true,
        }
    }

    /// An argv returned by the CLI itself, without the program name.
    pub fn returned(args: &[&str]) -> Self {
        Self {
            add_workspace: false,
            ..Self::new(args)
        }
    }

    pub fn with_stdin(mut self, input: Vec<u8>) -> Self {
        self.stdin = Some(input);
        self
    }
}

/// Run once, timing spawn to exit, and return the elapsed milliseconds and
/// stdout. A non-zero exit is an error: a failing command is not a sample.
pub fn time(binary: &Path, workspace: &Path, call: &Invocation) -> Result<(f64, Vec<u8>), String> {
    let mut command = Command::new(binary);
    if call.add_workspace {
        command.arg("--workspace").arg(workspace);
    }
    command
        .args(&call.args)
        .current_dir(workspace)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_READ_ONLY")
        .env_remove("INVESTIGATION_LOG")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if call.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let started = Instant::now();
    let mut child = command
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", binary.display()))?;
    if let Some(input) = &call.stdin {
        let mut pipe = child.stdin.take().ok_or("no stdin pipe")?;
        pipe.write_all(input).map_err(|error| error.to_string())?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    let elapsed = started.elapsed().as_secs_f64() * 1e3;
    if !output.status.success() {
        return Err(format!(
            "investigation {} exited {:?}: {}{}",
            call.args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout)
                .chars()
                .take(600)
                .collect::<String>(),
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(600)
                .collect::<String>(),
        ));
    }
    Ok((elapsed, output.stdout))
}

/// Nearest-rank percentile of sorted samples (`p` in 0..=100).
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Summary statistics in milliseconds, always with their sample count.
pub fn summarize(samples: &[f64]) -> Value {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    let mean = if n == 0 {
        f64::NAN
    } else {
        sorted.iter().sum::<f64>() / n as f64
    };
    json!({
        "n": n,
        "min_ms": round(sorted.first().copied().unwrap_or(f64::NAN)),
        "p50_ms": round(percentile(&sorted, 50.0)),
        "p90_ms": round(percentile(&sorted, 90.0)),
        "p95_ms": round(percentile(&sorted, 95.0)),
        "p99_ms": round(percentile(&sorted, 99.0)),
        "max_ms": round(sorted.last().copied().unwrap_or(f64::NAN)),
        "mean_ms": round(mean),
        "percentile_method": "nearest rank; with n < 20, p95 is the maximum",
        "samples_ms": sorted.iter().map(|value| round(*value)).collect::<Vec<_>>(),
    })
}

/// The 1, 5 and 15 minute load averages, if the platform exposes them.
pub fn load_average() -> Value {
    fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| {
            let fields: Vec<f64> = text
                .split_whitespace()
                .take(3)
                .filter_map(|field| field.parse().ok())
                .collect();
            (fields.len() == 3).then(|| json!(fields))
        })
        .unwrap_or(Value::Null)
}

fn first_line(path: &str, key: &str) -> Option<String> {
    fs::read_to_string(path).ok().and_then(|text| {
        text.lines()
            .find(|line| line.starts_with(key))
            .and_then(|line| line.split_once(':'))
            .map(|(_, value)| value.trim().to_owned())
    })
}

fn command_line(program: &str, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    let mut command = Command::new(program);
    command.args(args).stderr(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command.output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Filesystem type of the mount that holds `path` (longest matching prefix).
fn filesystem(path: &Path) -> Option<String> {
    let path = path.canonicalize().ok()?;
    let mounts = fs::read_to_string("/proc/mounts").ok()?;
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _device = fields.next()?;
            let mount = fields.next()?;
            let kind = fields.next()?;
            path.starts_with(mount)
                .then(|| (mount.len(), kind.to_owned()))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, kind)| kind)
}

/// What the report must say about where and how it was measured.
pub fn environment(binary: &Path, fixtures: &Path) -> Value {
    let cpu = first_line("/proc/cpuinfo", "model name");
    let memory = first_line("/proc/meminfo", "MemTotal");
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|text| text.trim().to_owned());
    let profile = binary
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(str::to_owned);
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "kernel": kernel,
        "cpu_model": cpu,
        "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "memory_total": memory,
        "fixture_dir": fixtures.display().to_string(),
        "fixture_filesystem": filesystem(fixtures),
        "binary": binary.display().to_string(),
        "binary_profile": profile,
        "binary_profile_basis": "name of the binary's target directory (release or debug)",
        "harness_debug_assertions": cfg!(debug_assertions),
        "rustc": command_line("rustc", &["--version"], Some(source_dir)),
        "git_commit": command_line("git", &["rev-parse", "HEAD"], Some(source_dir)),
        "git_dirty": command_line("git", &["status", "--porcelain"], Some(source_dir))
            .map(|status| !status.is_empty()),
        "load_average_1_5_15_at_start": load_average(),
    })
}
