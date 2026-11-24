//! The teaching tail of every command's help: examples and the next command.
//!
//! Every command's `--help` ends with `Example:` or `Examples:` lines that
//! parse against the live tree and one `Next:` line naming the command an
//! agent runs after it. The commands keep their own explanatory text; this
//! table adds only what is missing, so the label and the shape stay the same
//! everywhere. `help_lint` fails when a command lacks either one, when a line
//! does not parse, or when an example needs an environment variable or `cd`.
//!
//! Examples use `--workspace ./casework`, never the working directory, and
//! `<NAME>` placeholders for IDs the previous command printed.

use clap::Command;

/// Help text added to one command: examples (only when the command has none)
/// and the next command (only when the command names none).
struct Teach {
    path: &'static [&'static str],
    examples: &'static [&'static str],
    next: &'static str,
}

macro_rules! t {
    ($path:expr, [$($example:expr),* $(,)?], $next:expr) => {
        Teach { path: $path, examples: &[$($example),*], next: $next }
    };
}

#[rustfmt::skip]
const TEACH: &[Teach] = &[
    t!(&[], [], "investigation init ./casework --actor agent:investigator --no-vault --import-root . --format json"),
    t!(&["init"], [], "investigation --workspace ./casework --actor agent:investigator start 'Why did requests time out?' --attach ./results.log --format json"),
    t!(&["record"], [], "investigation --workspace ./casework receipt KEY --actor agent:investigator --format json"),
    t!(&["ingest"], ["investigation --workspace ./casework ingest --input manifest.json --format json", "investigation --workspace ./casework ingest --input ./runs/producer-bundle.json --format json"], "investigation --workspace ./casework brief --format json"),
    t!(&["start"], [], "investigation --workspace ./casework brief --format json"),
    t!(&["evidence"], ["investigation --workspace ./casework evidence read <ARTIFACT_ID> --selector-file selector.json --format json"], "investigation --workspace ./casework evidence read <ARTIFACT_ID> --selector-file selector.json --format json"),
    t!(&["evidence", "read"], ["investigation --workspace ./casework evidence read <ARTIFACT_ID> --selector-file selector.json --format json"], "investigation --workspace ./casework observe 'The log reports 28 seconds' --source <ARTIFACT_ID> --quote 'waited 28 seconds' --format json"),
    t!(&["capture"], ["investigation --workspace ./casework capture begin --input begin.json --format json"], "investigation --workspace ./casework capture begin --input begin.json --format json"),
    t!(&["capture", "begin"], [], "investigation --workspace ./casework capture append --input append.json --format json"),
    t!(&["capture", "append"], [], "investigation --workspace ./casework capture finalize --input finalize.json --format json"),
    t!(&["capture", "attach"], [], "investigation --workspace ./casework capture finalize --input finalize.json --format json"),
    t!(&["capture", "finalize"], [], "investigation --workspace ./casework brief --format json"),
    t!(&["capture", "recover"], [], "investigation --workspace ./casework capture finalize --input finalize.json --format json"),
    t!(&["schema"], [], "investigation --workspace ./casework record --input case.json --format json"),
    t!(&["onboard"], ["investigation --workspace ./casework onboard --format json"], "investigation --workspace ./casework next --format json"),
    t!(&["docs"], [], "investigation onboard --format json"),
    t!(&["next"], ["investigation --workspace ./casework next --format json"], "investigation --workspace ./casework show <ID> --depth full --format json"),
    t!(&["frontier"], [], "investigation --workspace ./casework next --format json"),
    t!(&["blindspots"], [], "investigation guide blindspot --format json"),
    t!(&["guide"], [], "investigation schema --list --format json"),
    t!(&["method"], ["investigation method list --format json"], "investigation guide NAME --format json"),
    t!(&["method", "list"], [], "investigation guide NAME --format json"),
    t!(&["receipt"], [], "investigation --workspace ./casework brief --format json"),
    t!(&["show"], ["investigation --workspace ./casework show <ID> --depth full --format json"], "investigation --workspace ./casework explain <ID> --format json"),
    t!(&["brief"], [], "investigation --workspace ./casework next --format json"),
    t!(&["explain"], [], "investigation --workspace ./casework next --format json"),
    t!(&["review-packet"], [], "investigation --workspace ./casework record --input review.json --format json"),
    t!(&["retrospective"], [], "investigation --workspace ./casework next --format json"),
    t!(&["calibration"], [], "investigation --workspace ./casework brief --format json"),
    t!(&["search"], [], "investigation --workspace ./casework show <ID> --format json"),
    t!(&["check"], [], "investigation --workspace ./casework repair index --format json"),
    t!(&["repair"], ["investigation --workspace ./casework repair index --format json"], "investigation --workspace ./casework check --format json"),
    t!(&["repair", "index"], [], "investigation --workspace ./casework check --format json"),
    t!(&["repair", "staging"], [], "investigation --workspace ./casework check --format json"),
    t!(&["vault"], [], "investigation --workspace ./casework vault status --format json"),
    t!(&["vault", "render"], [], "investigation --workspace ./casework vault status --format json"),
    t!(&["vault", "status"], [], "investigation --workspace ./casework vault conflicts --format json"),
    t!(&["vault", "conflicts"], [], "investigation --workspace ./casework vault status --format json"),
    t!(&["doctor"], [], "investigation --workspace ./casework doctor bundle --preview --format json"),
    t!(&["doctor", "bundle"], [], "investigation --workspace ./casework check --format json"),
    t!(&["integration"], [], "investigation integration show --target agents-block --format json"),
    t!(&["integration", "show"], [], "investigation integration apply --target agents-block --path AGENTS.md --format json"),
    t!(&["integration", "apply"], [], "investigation integration inspect --format json"),
    t!(&["integration", "remove"], [], "investigation integration inspect --format json"),
    t!(&["integration", "inspect"], [], "investigation docs agents"),
    t!(&["observe"], ["investigation --workspace ./casework observe 'The log reports 28 seconds' --source <ARTIFACT_ID> --quote 'waited 28 seconds' --format json"], "investigation --workspace ./casework brief --format json"),
    t!(&["note"], ["investigation --workspace ./casework note 'Monday batches ran hot; check the setpoint' --format json", "investigation --workspace ./casework note 'Check the clock source' --on <RECORD_ID> --format json"], "investigation --workspace ./casework brief --format json"),
    t!(&["case"], ["investigation --workspace ./casework case new 'Request latency' --format json"], "investigation --workspace ./casework case new 'Request latency' --format json"),
    t!(&["case", "new"], ["investigation --workspace ./casework case new 'Request latency' --format json"], "investigation --workspace ./casework source add ./results.log --format json"),
    t!(&["source"], ["investigation --workspace ./casework source add ./results.log --format json"], "investigation --workspace ./casework source add ./results.log --format json"),
    t!(&["source", "add"], ["investigation --workspace ./casework source add ./results.log --format json"], "investigation --workspace ./casework observe 'The log reports 28 seconds' --source <ARTIFACT_ID> --quote 'waited 28 seconds' --format json"),
];

fn has_label(text: &str, label: &str) -> bool {
    text.lines()
        .any(|line| line.trim_start().starts_with(label))
}

fn teach_one(command: Command, entry: &Teach) -> Command {
    let mut text = command
        .get_after_help()
        .map(ToString::to_string)
        .unwrap_or_default();
    if !has_label(&text, "Example") && !entry.examples.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("Examples:");
        for example in entry.examples {
            text.push_str("\n  ");
            text.push_str(example);
        }
    }
    if !has_label(&text, "Next:") {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("Next: ");
        text.push_str(entry.next);
    }
    command.after_help(text)
}

fn apply(command: Command, entry: &Teach, depth: usize) -> Command {
    match entry.path.get(depth) {
        None => teach_one(command, entry),
        Some(name) => command.mut_subcommand(*name, |sub| apply(sub, entry, depth + 1)),
    }
}

/// Add the missing examples and next-step lines to a command tree.
pub(crate) fn teach(root: Command) -> Command {
    TEACH.iter().fold(root, |root, entry| apply(root, entry, 0))
}

/// The live command tree: the clap commands, the catalog shortcuts and the
/// teaching tail of each command's help.
pub(crate) fn command_tree() -> Command {
    use clap::CommandFactory;
    teach(crate::shortcuts::augment(crate::Cli::command()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_names_a_command_of_the_tree() {
        let root = command_tree();
        for entry in TEACH {
            let mut command = &root;
            for name in entry.path {
                command = command
                    .find_subcommand(name)
                    .unwrap_or_else(|| panic!("no command {:?}", entry.path));
            }
            assert!(
                command.get_after_help().is_some(),
                "{:?} gained no help",
                entry.path
            );
        }
    }
}
