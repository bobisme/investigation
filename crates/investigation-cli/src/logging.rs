//! Diagnostic logging setup. Logs go to stderr only, never stdout, so
//! machine-readable output is identical at every verbosity. The default level
//! reports warnings and errors with identifiers, stages and error classes. No
//! level prints evidence content: call sites log opaque IDs and counts only.

use std::{env, ffi::OsString, io};

use tracing::level_filters::LevelFilter;

/// Environment variable with the same meaning as `-v` and `-q`:
/// `error`, `warn`, `info`, `debug` or `trace`.
pub(crate) const ENV: &str = "INVESTIGATION_LOG";

/// Pick a level from raw arguments (`-v`, `-vv`, `--verbose`, `-q`,
/// `--quiet`) and the environment. Flags win over the environment.
pub(crate) fn level(args: &[OsString], environment: Option<&str>) -> LevelFilter {
    let mut verbose = 0u8;
    let mut quiet = false;
    for arg in args.iter().skip(1) {
        let Some(arg) = arg.to_str() else { continue };
        if arg == "--" {
            break;
        }
        match arg {
            "--verbose" => verbose = verbose.saturating_add(1),
            "--quiet" => quiet = true,
            _ if arg.starts_with('-')
                && !arg.starts_with("--")
                && arg.len() > 1
                && arg[1..].bytes().all(|b| b == b'v' || b == b'q') =>
            {
                verbose = verbose.saturating_add(arg.bytes().filter(|b| *b == b'v').count() as u8);
                quiet |= arg.contains('q');
            }
            _ => {}
        }
    }
    if quiet {
        return LevelFilter::ERROR;
    }
    match verbose {
        0 => environment
            .and_then(|value| value.parse().ok())
            .unwrap_or(LevelFilter::WARN),
        1 => LevelFilter::INFO,
        2 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    }
}

/// Install the stderr subscriber. Safe to call once; later calls are ignored.
pub(crate) fn init(args: &[OsString]) {
    let level = level(args, env::var(ENV).ok().as_deref());
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(io::stderr)
        .with_ansi(false)
        .with_target(false)
        .without_time()
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn verbosity_flags_and_environment() {
        let base = ["investigation"];
        assert_eq!(level(&args(&base), None), LevelFilter::WARN);
        assert_eq!(level(&args(&base), Some("debug")), LevelFilter::DEBUG);
        assert_eq!(level(&args(&base), Some("nonsense")), LevelFilter::WARN);
        assert_eq!(level(&args(&["i", "-v"]), Some("error")), LevelFilter::INFO);
        assert_eq!(level(&args(&["i", "-vv"]), None), LevelFilter::DEBUG);
        assert_eq!(
            level(&args(&["i", "--verbose", "-vv"]), None),
            LevelFilter::TRACE
        );
        assert_eq!(level(&args(&["i", "-q", "-v"]), None), LevelFilter::ERROR);
        assert_eq!(level(&args(&["i", "--", "-vv"]), None), LevelFilter::WARN);
    }
}
