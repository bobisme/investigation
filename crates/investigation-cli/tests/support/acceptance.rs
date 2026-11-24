//! Observations for the deterministic acceptance report (plan §20.5).
//!
//! When `INV_ACCEPTANCE_OBSERVATIONS` names a directory, a test writes what it
//! measured there as `<measure>.<name>.json`, and
//! `scripts/acceptance-measures.py` copies it into
//! `target/acceptance/deterministic-report.json` beside the target and the
//! test outcomes. Without the variable nothing is written, so an ordinary
//! test run is unchanged. A test asserts its own target either way: the
//! observation only reports the value it checked.
#![allow(dead_code)]

use std::{fs, path::PathBuf};

use serde_json::Value;

/// The variable that names the observation directory.
pub const ENV: &str = "INV_ACCEPTANCE_OBSERVATIONS";

/// Record one observation of `measure` under `name`.
pub fn observe(measure: &str, name: &str, value: &Value) {
    let Some(directory) = std::env::var_os(ENV).map(PathBuf::from) else {
        return;
    };
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{measure}.{name}.json"));
    fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
