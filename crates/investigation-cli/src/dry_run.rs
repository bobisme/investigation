//! `--dry-run` on writes: `record`, `ingest`, `start` and the catalog
//! shortcuts.
//!
//! A dry run goes through the write's own code with the store in dry-run
//! mode (`Store::set_dry_run`): the same parsing, actor and case selection,
//! commit lock, journal verification, idempotency check, validation and
//! gates. A refusal is therefore the commit's own failure, with its code,
//! path and exit status. A request that passes returns the receipt the
//! commit would return, marked `result: dry_run`, and writes nothing: no
//! journal, blob, staging, cache entry or index. `next_write` is the exact
//! command that commits it.

use std::ffi::OsString;

use investigation_core::{
    PROGRAM_NAME,
    wire::{Failure, ResponseVersion},
};
use investigation_store::Store;
use schemars::{JsonSchema, Schema, json_schema};
use serde_json::{Value, json};

use crate::{Output, display_argv};

/// Help for every `--dry-run` flag.
pub(crate) const HELP: &str =
    "Validate against the current snapshot and print the would-be receipt; commit nothing.";

/// The first warning of a dry-run response, by the commit's result.
const PLANNED: &str = "Dry run: nothing was committed. The transaction ID, record IDs and snapshot are a plan's; the commit assigns its own.";
const RETRY: &str = "Dry run: nothing was committed. The commit would commit nothing either and return this earlier receipt.";

/// Run `write` as itself, or as a dry run when `dry` is set.
pub(crate) fn run(
    store: &mut Store,
    dry: bool,
    write: impl FnOnce(&mut Store) -> Result<Output, Failure>,
) -> Result<Output, Failure> {
    if !dry {
        return write(store);
    }
    store.set_dry_run();
    write(store).map(present)
}

/// Turn the receipt output of a dry-run write into the dry-run response.
pub(crate) fn present(mut output: Output) -> Output {
    let value = &mut output.value;
    let would = value["result"].clone();
    let warning = if would == "already_committed" {
        RETRY
    } else {
        PLANNED
    };
    value["result"] = json!("dry_run");
    value["would_result"] = would.clone();
    // The planned records do not exist, so no read names them yet.
    value["next"] = json!([]);
    if let Some(warnings) = value["warnings"].as_array_mut() {
        warnings.insert(0, json!(warning));
    }
    let argv = commit_argv(std::env::args_os());
    let commit = display_argv(&argv);
    value["next_write"] = json!({
        "kind": "commit",
        "argv": argv,
        "unresolved_fields": [],
        "effect": "Runs the same command without --dry-run, which validates again against the snapshot current then and commits.",
    });
    // The receipt's text, with the dry run said first and the commit last.
    let mut lines = output.text.lines();
    let mut text = format!(
        "dry_run  would_result {}  nothing committed",
        would.as_str().unwrap_or("committed")
    );
    lines.next();
    for line in lines.filter(|line| !line.starts_with("Next: ")) {
        text.push('\n');
        text.push_str(line);
    }
    text.push_str(&format!("\nWarning: {warning}\nCommit: {commit}"));
    output.text = text;
    output
}

/// The command line that ran, without the flags that kept it from writing.
fn commit_argv(args: impl IntoIterator<Item = OsString>) -> Vec<String> {
    let mut argv = vec![PROGRAM_NAME.to_owned()];
    let mut operands = false;
    for arg in args.into_iter().skip(1) {
        let arg = arg.to_string_lossy().into_owned();
        if !operands && (arg == "--dry-run" || arg == "--read-only") {
            continue;
        }
        operands |= arg == "--";
        argv.push(arg);
    }
    argv
}

/// The published shape of a dry-run response: the would-be receipt of the
/// command (`receipt`, `ingest-receipt` or `producer-run-receipt`) with
/// these fields replaced or added.
#[derive(JsonSchema)]
#[allow(dead_code)]
pub(crate) struct DryRunResponse {
    schema_version: ResponseVersion,
    ok: bool,
    /// Always `dry_run`: nothing was committed.
    #[schemars(schema_with = "dry_run_result")]
    result: String,
    /// The `result` the commit would return: `committed`, or
    /// `already_committed` when it would change nothing (this actor already
    /// committed the command ID, or an ingest's items are all recorded).
    #[schemars(schema_with = "unconstrained")]
    would_result: Value,
    /// Empty: the planned records do not exist yet.
    #[schemars(schema_with = "unconstrained")]
    next: Value,
    /// The command that commits the request: this one without `--dry-run`.
    #[schemars(schema_with = "unconstrained")]
    next_write: Value,
    /// The commit's warnings, after one that says nothing was committed.
    warnings: Vec<String>,
}

fn dry_run_result(_: &mut schemars::SchemaGenerator) -> Schema {
    json_schema!({"type": "string", "const": "dry_run"})
}

fn unconstrained(_: &mut schemars::SchemaGenerator) -> Schema {
    Schema::default()
}

pub(crate) fn schema() -> Schema {
    schemars::schema_for!(DryRunResponse)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_commit_command_drops_only_the_flags_that_stop_a_write() {
        assert_eq!(
            commit_argv(os(&[
                "/bin/investigation",
                "--read-only",
                "record",
                "--input",
                "tx.json",
                "--dry-run",
                "--format",
                "json",
            ])),
            [
                "investigation",
                "record",
                "--input",
                "tx.json",
                "--format",
                "json"
            ]
        );
        // After `--` an argument is an operand, even one spelled like the flag.
        assert_eq!(
            commit_argv(os(&["x", "note", "--dry-run", "--", "--dry-run"])),
            ["investigation", "note", "--", "--dry-run"]
        );
    }
}
