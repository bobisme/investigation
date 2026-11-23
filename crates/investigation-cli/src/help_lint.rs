//! Help lint: every command and subcommand teaches its own use.
//!
//! The test walks the live clap tree (shortcuts included) and renders each
//! command's `--help`. A command passes when its help has
//!
//! 1. a one-line description (first line, non-empty, one line),
//! 2. a `Usage:` line,
//! 3. an `Example:` or `Examples:` label followed by at least one
//!    `investigation ...` line,
//! 4. a `Next:` line naming the next `investigation ...` command to run,
//! 5. every argument has help text,
//! 6. every `investigation ...` line parses against the live tree, and
//! 7. no example depends on an environment variable assignment, `export` or
//!    `cd`; examples pass flags such as `--workspace` instead.
//!
//! The built-in `help` command is exempt. The core examples also run in a
//! fixture workspace in `tests/help_examples.rs`.

use clap::Command as Cmd;
use investigation_core::PROGRAM_NAME;

/// Every command path in the live tree, root first, excluding `help` and
/// hidden commands, which `--help` never shows and so cannot teach.
fn paths(command: &clap::Command, prefix: &[String], out: &mut Vec<Vec<String>>) {
    out.push(prefix.to_vec());
    for sub in command.get_subcommands() {
        if sub.get_name() == "help" || sub.is_hide_set() {
            continue;
        }
        let mut path = prefix.to_vec();
        path.push(sub.get_name().to_owned());
        paths(sub, &path, out);
    }
}

fn find<'a>(root: &'a clap::Command, path: &[String]) -> &'a clap::Command {
    path.iter().fold(root, |command, name| {
        command.find_subcommand(name).expect("path exists")
    })
}

/// The `investigation ...` command lines in a help text, with the label line
/// (`Example:`, `Examples:`, `Next:`) they sit under, if any.
fn command_lines(help: &str) -> Vec<(String, String)> {
    let start = format!("{PROGRAM_NAME} ");
    let mut label = String::new();
    let mut found = Vec::new();
    for line in help.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("Example:")
            .or_else(|| trimmed.strip_prefix("Next:"))
        {
            label = trimmed[..trimmed.len() - rest.len()].to_owned();
            if rest.trim().starts_with(&start) {
                found.push((label.clone(), rest.trim().to_owned()));
            }
        } else if trimmed.starts_with("Examples:") {
            "Examples:".clone_into(&mut label);
        } else if trimmed.starts_with(&start) {
            found.push((label.clone(), trimmed.to_owned()));
        } else if trimmed.is_empty() {
            label.clear();
        }
    }
    found
}

/// Replace `<NAME>` placeholders with a plain word so the line can parse.
fn placeholders(line: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in line.chars() {
        match c {
            '<' => {
                inside = true;
                out.push_str("PLACEHOLDER");
            }
            '>' if inside => inside = false,
            _ if inside => {}
            c => out.push(c),
        }
    }
    out
}

/// Everything wrong with one command's help; empty when it passes.
fn lint(root: &clap::Command, path: &[String]) -> Vec<String> {
    let name = if path.is_empty() {
        PROGRAM_NAME.to_owned()
    } else {
        format!("{PROGRAM_NAME} {}", path.join(" "))
    };
    let mut problems = Vec::new();
    let rendered = find(root, path)
        .clone()
        .bin_name(&name)
        .render_long_help()
        .to_string();
    let first = rendered.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        problems.push("no one-line description".to_owned());
    } else if first.len() > 120 {
        problems.push(format!(
            "description is {} chars; keep it to one line",
            first.len()
        ));
    }
    if !rendered.contains("Usage:") {
        problems.push("no Usage: line".to_owned());
    }
    let lines = command_lines(&rendered);
    let has = |label: &str| {
        lines
            .iter()
            .any(|(l, _)| l == label || (label == "Example:" && l == "Examples:"))
    };
    if !has("Example:") {
        problems.push("no `Example:` line with an investigation command".to_owned());
    }
    if !has("Next:") {
        problems.push("no `Next:` line naming the next investigation command".to_owned());
    }
    for arg in find(root, path).get_arguments() {
        if !arg.is_hide_set() && arg.get_help().is_none() && arg.get_long_help().is_none() {
            problems.push(format!("argument `{}` has no help text", arg.get_id()));
        }
    }
    for line in rendered.lines().map(str::trim) {
        let shown = line
            .strip_prefix("Example:")
            .or_else(|| line.strip_prefix("Next:"))
            .map_or(line, str::trim);
        if !shown.contains(&format!("{PROGRAM_NAME} ")) {
            continue;
        }
        let before = shown.split(PROGRAM_NAME).next().unwrap_or("");
        let labelled = shown.len() != line.len();
        let env_like = |w: &str| matches!(w, "cd" | "export" | "env" | "&&") || w.contains('=');
        let mut before_words = before.split_whitespace();
        let env_or_cd = if labelled {
            before_words.any(env_like)
        } else {
            before_words.clone().next().is_some() && before_words.all(env_like)
        };
        if env_or_cd || (labelled && (shown.contains("&&") || shown.contains("; "))) {
            problems.push(format!(
                "`{shown}` depends on cd, env vars or chaining; use flags"
            ));
        }
    }
    for (_, line) in &lines {
        let argv = crate::docs::words(&placeholders(line));
        if let Err(error) = root.clone().try_get_matches_from(&argv) {
            let reason = error.to_string();
            problems.push(format!(
                "`{line}` does not parse: {}",
                reason.lines().next().unwrap_or("")
            ));
        }
    }
    problems
}

#[test]
fn every_command_help_teaches_its_own_use() {
    let root = crate::help_teach::command_tree();
    let mut all = Vec::new();
    paths(&root, &[], &mut all);
    assert!(
        all.len() > 30,
        "tree walk found only {} commands",
        all.len()
    );
    let mut failures = Vec::new();
    for path in &all {
        let problems = lint(&root, path);
        if !problems.is_empty() {
            failures.push(format!("{}: {problems:#?}", path.join(" ")));
        }
    }
    assert!(
        failures.is_empty(),
        "{} commands fail the help lint:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn the_lint_rejects_thin_help() {
    let bare = Cmd::new(PROGRAM_NAME).subcommand(Cmd::new("thin").about("Does a thing"));
    assert!(lint(&bare, &["thin".to_owned()]).len() >= 2);
    let env = Cmd::new(PROGRAM_NAME).subcommand(
        Cmd::new("x")
            .about("X")
            .after_help("Example: AGENT=a investigation x\nNext: investigation x"),
    );
    assert!(!lint(&env, &["x".to_owned()]).is_empty());
}
