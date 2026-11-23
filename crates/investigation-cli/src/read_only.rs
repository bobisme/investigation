//! `--read-only`: the one place that says which commands may write.
//!
//! Every leaf command of the clap tree, including catalog shortcuts, is
//! classified once in [`EFFECTS`]. The check runs on the parsed argv before
//! dispatch, so a refused command never opens the workspace, takes the commit
//! lock or creates a file. A test walks the live command tree and fails when a
//! command is missing here or listed here but gone, so a new command cannot
//! ship without an effect.
//!
//! The mode is switched on by `--read-only` or by `INVESTIGATION_READ_ONLY`
//! (any value except `0`, `false`, `no`, `off`), so a host can enforce it
//! without trusting the agent's argv.

use clap::{ArgMatches, parser::ValueSource};
use investigation_core::wire::{ErrorCode, Failure};
use serde_json::json;

/// Name of the environment variable that enables the mode.
pub(crate) const ENV: &str = "INVESTIGATION_READ_ONLY";

/// What a command does to the workspace or to files outside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    /// Never writes and never exports.
    Read,
    /// Writes workspace data, exports files or changes a file the user
    /// names outside the workspace (`integration apply`).
    Write,
    /// A read, except that these flags (clap argument ids) make it write.
    ReadUnlessFlag(&'static [&'static str]),
    /// A write, except that this flag (clap argument id) turns it into a dry read.
    WriteUnlessFlag(&'static str),
}

/// Declared once: command path and its effect. Parent commands are not listed.
pub(crate) const EFFECTS: &[(&str, Effect)] = &[
    ("init", Effect::Write),
    ("record", Effect::Write),
    ("ingest", Effect::Write),
    ("start", Effect::Write),
    ("evidence read", Effect::Read),
    ("capture begin", Effect::Write),
    ("capture append", Effect::Write),
    ("capture attach", Effect::Write),
    ("capture finalize", Effect::Write),
    ("capture recover", Effect::Read),
    ("schema", Effect::Read),
    ("onboard", Effect::Read),
    ("docs", Effect::Read),
    ("next", Effect::Read),
    ("frontier", Effect::Read),
    ("blindspots", Effect::Read),
    ("guide", Effect::Read),
    ("method list", Effect::Read),
    ("receipt", Effect::Read),
    ("show", Effect::Read),
    ("brief", Effect::Read),
    ("explain", Effect::Read),
    ("review-packet", Effect::Read),
    ("retrospective", Effect::ReadUnlessFlag(&["consult"])),
    ("calibration", Effect::Read),
    ("search", Effect::Read),
    ("check", Effect::Read),
    ("repair index", Effect::Write),
    ("repair staging", Effect::Write),
    ("observe", Effect::WriteUnlessFlag("print_envelope")),
    ("note", Effect::WriteUnlessFlag("print_envelope")),
    ("case new", Effect::WriteUnlessFlag("print_envelope")),
    ("source add", Effect::WriteUnlessFlag("print_envelope")),
    // Writes the vault, its manifest and the conflicts area under local/.
    ("vault render", Effect::Write),
    ("vault status", Effect::Read),
    ("vault conflicts", Effect::Read),
    // `doctor bundle` requires --preview and writes no archive.
    ("doctor", Effect::Read),
    ("doctor bundle", Effect::Read),
    // Apply and remove change one file the user names outside any workspace,
    // like an export; --dry-run prints the diff and writes nothing.
    ("integration show", Effect::Read),
    ("integration inspect", Effect::Read),
    ("integration apply", Effect::WriteUnlessFlag("dry_run")),
    ("integration remove", Effect::WriteUnlessFlag("dry_run")),
];

fn effect_of(path: &str) -> Option<Effect> {
    EFFECTS
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, effect)| *effect)
}

fn flag_given(matches: &ArgMatches, id: &str) -> bool {
    matches.try_contains_id(id).unwrap_or(false)
        && matches.value_source(id) == Some(ValueSource::CommandLine)
}

fn enabled(matches: &ArgMatches) -> bool {
    let flag = matches.try_get_one::<bool>("read_only").ok().flatten() == Some(&true);
    flag || env_enabled(std::env::var(ENV).ok().as_deref())
}

/// Any set value switches the mode on except the usual spellings of "off".
fn env_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

/// Why this invocation writes, or `None` when it only reads.
fn write_reason(path: &str, leaf: &ArgMatches) -> Option<String> {
    match effect_of(path) {
        // An unclassified command is refused: fail closed.
        None => Some(format!("`{path}` has no declared effect")),
        Some(Effect::Read) => None,
        Some(Effect::Write) => Some(format!("`{path}` writes or exports")),
        Some(Effect::ReadUnlessFlag(flags)) => flags
            .iter()
            .find(|flag| flag_given(leaf, flag))
            .map(|flag| format!("`{path} --{flag}` writes")),
        Some(Effect::WriteUnlessFlag(flag)) => (!flag_given(leaf, flag)).then(|| {
            format!(
                "`{path}` writes unless --{} is given",
                flag.replace('_', "-")
            )
        }),
    }
}

/// The refusal for this parsed command line, if read-only mode forbids it.
pub(crate) fn refusal(matches: &ArgMatches) -> Option<Failure> {
    if !enabled(matches) {
        return None;
    }
    let mut names = Vec::new();
    let mut leaf = matches;
    while let Some((name, next)) = leaf.subcommand() {
        names.push(name);
        leaf = next;
    }
    let path = names.join(" ");
    let reason = write_reason(&path, leaf)?;
    let alternatives: Vec<&str> = EFFECTS
        .iter()
        .filter(|(_, effect)| matches!(effect, Effect::Read | Effect::ReadUnlessFlag(_)))
        .map(|(name, _)| *name)
        .collect();
    let mut failure = Failure::new(
        ErrorCode::InvalidInput,
        format!(
            "READ_ONLY_REFUSED: {reason} and read-only mode is on (--read-only or {ENV}). \
             Nothing was changed. Read-only commands: {}.",
            alternatives.join(", ")
        ),
    )
    .at("--read-only");
    failure.error.expected = Some(json!({
        "reason": "READ_ONLY_REFUSED",
        "read_only_commands": alternatives,
    }));
    failure.error.actual = Some(json!({ "command": path }));
    Some(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use std::collections::BTreeSet;

    fn leaves(command: &clap::Command, prefix: &str, out: &mut BTreeSet<String>) {
        let subs: Vec<_> = command
            .get_subcommands()
            .filter(|sub| sub.get_name() != "help")
            .collect();
        // A parent that runs without a subcommand is itself a command.
        if subs.is_empty() || (!prefix.is_empty() && !command.is_subcommand_required_set()) {
            out.insert(prefix.to_owned());
        }
        for sub in subs {
            let path = if prefix.is_empty() {
                sub.get_name().to_owned()
            } else {
                format!("{prefix} {}", sub.get_name())
            };
            leaves(sub, &path, out);
        }
    }

    #[test]
    fn env_spellings() {
        assert!(env_enabled(Some("1")) && env_enabled(Some("true")));
        assert!(!env_enabled(None) && !env_enabled(Some("0")) && !env_enabled(Some("Off")));
    }

    #[test]
    fn every_leaf_command_has_a_declared_effect_and_no_stale_entries() {
        let tree = crate::shortcuts::augment(crate::Cli::command());
        let mut live = BTreeSet::new();
        leaves(&tree, "", &mut live);
        let declared: BTreeSet<String> = EFFECTS.iter().map(|(n, _)| (*n).to_owned()).collect();
        assert_eq!(declared.len(), EFFECTS.len(), "duplicate effect entries");
        let missing: Vec<_> = live.difference(&declared).collect();
        assert!(
            missing.is_empty(),
            "commands without an effect: {missing:?}"
        );
        let stale: Vec<_> = declared.difference(&live).collect();
        assert!(stale.is_empty(), "effects for missing commands: {stale:?}");
    }

    #[test]
    fn flags_named_by_effects_exist_on_their_commands() {
        let tree = crate::shortcuts::augment(crate::Cli::command());
        for (path, effect) in EFFECTS {
            let flags: &[&str] = match effect {
                Effect::ReadUnlessFlag(flags) => flags,
                Effect::WriteUnlessFlag(flag) => std::slice::from_ref(flag),
                _ => continue,
            };
            let mut command = &tree;
            for part in path.split(' ') {
                command = command.find_subcommand(part).expect("command exists");
            }
            for flag in flags {
                assert!(
                    command.get_arguments().any(|arg| arg.get_id() == *flag),
                    "{path} has no argument {flag}"
                );
            }
        }
    }
}
