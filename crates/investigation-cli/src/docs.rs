//! `docs [PAGE]`: offline documentation compiled into the binary.
//!
//! Each page is a Markdown file under `docs/agent/`, embedded with
//! `include_str!`, so it prints with no workspace, network or install step.
//! Adding a page is one entry in [`PAGES`]. The unit tests below parse every
//! command line a page shows with the live clap tree (shortcuts included) and
//! check every operation name against the catalog, so a page cannot name a
//! command, flag, card or operation that the binary lacks.
//!
//! The agent instructions carry [`AGENT_INSTRUCTIONS_VERSION`] in a marker
//! line, `onboard` reports the same version, and a test pins each page's
//! digest to its version so that changed text needs a new version.

use investigation_core::{
    PROGRAM_NAME,
    wire::{ErrorCode, Failure},
};
use investigation_store::journal::hash_bytes;
use serde_json::{Value, json};

use crate::Output;

/// Version of the agent instructions (the `agents` snippet and the `skill`).
pub(crate) const AGENT_INSTRUCTIONS_VERSION: &str = "inv.agent-instructions.v5";

/// Version of the producer onboarding page.
pub(crate) const PRODUCER_ONBOARDING_VERSION: &str = "inv.producer-onboarding.v2";

/// Version of the consumer migration recipe.
pub(crate) const MIGRATION_GUIDANCE_VERSION: &str = "inv.migration-guidance.v1";

/// Version of the secret-exposure diagnosis and manual remediation page.
pub(crate) const EXPOSURE_GUIDANCE_VERSION: &str = "inv.exposure-guidance.v4";

pub(crate) const AFTER_HELP: &str = "\
Prints one embedded page as Markdown; without PAGE, lists the pages.
Pages are compiled into the binary and need no workspace or network.
Examples:
  investigation docs
  investigation docs agents
  investigation docs skill --format json";

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Page name, for example agents. Omit it to list the pages.
    #[arg(value_name = "PAGE")]
    page: Option<String>,
}

/// One embedded page.
pub(crate) struct Page {
    pub(crate) name: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) version: &'static str,
    pub(crate) body: &'static str,
}

/// `page!(NAME, FILE under docs/agent/, VERSION, SUMMARY)`.
macro_rules! page {
    ($name:literal, $file:literal, $version:expr, $summary:literal) => {
        Page {
            name: $name,
            summary: $summary,
            version: $version,
            body: include_str!(concat!("../../../docs/agent/", $file)),
        }
    };
}

/// Every embedded page, in listing order. Add a page with one line here.
#[rustfmt::skip]
pub(crate) const PAGES: &[Page] = &[
    page!("agents", "agents.md", AGENT_INSTRUCTIONS_VERSION, "Agent instructions snippet for an AGENTS.md file: when to use the CLI and the main loop."),
    page!("skill", "skill.md", AGENT_INSTRUCTIONS_VERSION, "Optional installable skill: triggers and a compact main loop."),
    page!("producer", "producer.md", PRODUCER_ONBOARDING_VERSION, "Producer-only onboarding: record a program run with the capture calls, no method knowledge needed."),
    page!("migration", "migration.md", MIGRATION_GUIDANCE_VERSION, "Recipe for moving a producer's own journal onto the shared recorder."),
    page!("exposure", "exposure.md", EXPOSURE_GUIDANCE_VERSION, "A secret reached the workspace: diagnose where it lives, rotate it, and the manual remediation; nothing is erased."),
];

pub(crate) fn page(name: &str) -> Option<&'static Page> {
    PAGES.iter().find(|page| page.name == name)
}

fn argv(page: Option<&str>) -> Vec<String> {
    let mut argv = vec![PROGRAM_NAME.to_owned(), "docs".to_owned()];
    argv.extend(page.map(str::to_owned));
    argv
}

fn describe(page: &Page) -> Value {
    json!({
        "page": page.name, "summary": page.summary, "version": page.version,
        "digest": hash_bytes(page.body.as_bytes()), "bytes": page.body.len(),
        "argv": argv(Some(page.name)),
    })
}

/// What `onboard` reports about the agent instructions.
pub(crate) fn onboard_pointer() -> Value {
    let agents = page("agents").expect("the agents page is embedded");
    json!({
        "version": agents.version,
        "digest": hash_bytes(agents.body.as_bytes()),
        "argv": argv(Some(agents.name)),
    })
}

/// The `onboard` text line that points to the agent instructions.
pub(crate) fn onboard_line() -> String {
    format!(
        "Agent instructions ({AGENT_INSTRUCTIONS_VERSION}), offline: {PROGRAM_NAME} docs agents"
    )
}

pub(crate) fn run(args: Args) -> Result<Output, Failure> {
    let Some(name) = args.page else {
        let pages: Vec<_> = PAGES.iter().map(describe).collect();
        let text = PAGES
            .iter()
            .map(|page| format!("{:<9} {}  {}", page.name, page.version, page.summary))
            .collect::<Vec<_>>()
            .join("\n");
        return Ok(Output::success(
            "docs_list",
            json!({ "pages": pages }),
            format!("{text}\nPrint one with: {PROGRAM_NAME} docs PAGE"),
        ));
    };
    let Some(page) = page(&name) else {
        let names: Vec<_> = PAGES.iter().map(|page| page.name).collect();
        let mut failure = Failure::new(
            ErrorCode::UnknownReference,
            format!(
                "No embedded page has that name. Pages: {}.",
                names.join(", ")
            ),
        )
        .at("/page")
        .with_argv(argv(None));
        failure.error.expected = Some(json!({ "pages": names }));
        return Err(failure);
    };
    let mut value = describe(page);
    value["content"] = json!(page.body);
    Ok(Output::success("docs_page", value, page.body.to_owned()))
}

/// Split one shell command line into words. Handles quotes; stops at a
/// pipe, redirection, separator or line continuation.
#[cfg(test)]
pub(crate) fn words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut started = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, '|' | '<' | '>' | ';' | '&' | '\\') => break,
            (None, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use investigation_core::{
        catalog::standard_catalog,
        method_pack::{GENERAL_PACK_SOURCE, load},
    };

    // The documents as this binary was built with them (see build.rs).
    include!(concat!(env!("OUT_DIR"), "/documents.rs"));

    /// Each page version, with the digest of the page text it names. Changed
    /// text needs a new version: add a row, never edit an existing one.
    const RELEASED: &[(&str, &str, &str)] = &[
        (
            "agents",
            "inv.agent-instructions.v1",
            "sha256:3499c11ee06e24ffe507bf2fea6d4f1680df54057f40f4c6b3d12902c2df378f",
        ),
        (
            "skill",
            "inv.agent-instructions.v1",
            "sha256:59e6ff3cc0f341620d0f309b851b70134272d263e1789a7b5719a90f17bf2e5f",
        ),
        (
            "agents",
            "inv.agent-instructions.v2",
            "sha256:61819e35798e7b9444893a1853c8cf67bfd8c7ec6ebf01a13ab0f15b68c5e583",
        ),
        (
            "skill",
            "inv.agent-instructions.v2",
            "sha256:1394d99a9fae1483de6fbcc8bcc68f4c73c2af1fec0061258e95381905adb397",
        ),
        (
            "agents",
            "inv.agent-instructions.v3",
            "sha256:f2a3de1d87ce634bbabacba5090e02f64940340df4d8df86b720d60295d3d3f0",
        ),
        (
            "skill",
            "inv.agent-instructions.v3",
            "sha256:d455b9782cdd95616deeee8ab68e5a383242a29b3a55b9c2285fc075a49205f5",
        ),
        (
            "agents",
            "inv.agent-instructions.v4",
            "sha256:8e5fcb7a5e77d095f4e626456532e593ba9de5df6ec717fe9c7637b86b5e4706",
        ),
        (
            "skill",
            "inv.agent-instructions.v4",
            "sha256:dd5db4e4ac55a8eb53dd4218bad2f2e59be79100b3c24be9e0f7716b9bc28ba8",
        ),
        (
            "agents",
            "inv.agent-instructions.v5",
            "sha256:aa52aeb92bde4abfd54ff1e9a2f3b04caa757bd9ef7d8cb418d369d0b8dec665",
        ),
        (
            "skill",
            "inv.agent-instructions.v5",
            "sha256:626cafeb6da23388a60c920a77ea3cb05426e782d7fe9279c4ae2d95d613906a",
        ),
        (
            "producer",
            "inv.producer-onboarding.v1",
            "sha256:e8905b26d3f6247306d381b42e0e6f020e3f3809ff270daab75c4d7377196ce1",
        ),
        (
            "producer",
            "inv.producer-onboarding.v2",
            "sha256:5f06006909b0ccdf915e1a0441285faaadf499bc5e9dfb57a5d990dd4ca6c946",
        ),
        (
            "migration",
            "inv.migration-guidance.v1",
            "sha256:b2926052eabd0e70e0c2c36b93519ce382847fc42f1f73d70f7fdedd1f8c76c9",
        ),
        (
            "exposure",
            "inv.exposure-guidance.v1",
            "sha256:47a803e9c4c56d5f8e7703fbd848232bc6e6484a7d56016a059a63dd7717c0dd",
        ),
        (
            "exposure",
            "inv.exposure-guidance.v2",
            "sha256:3bae241f91e5c20d19a0fd6eff12a1e7300c04a746f651d9549dd2dc6bdd4c8d",
        ),
        (
            "exposure",
            "inv.exposure-guidance.v3",
            "sha256:cbf61c4edc8401ddadae110f9d8c8e7a532435e861f156679692c4fe0cdc332f",
        ),
        (
            "exposure",
            "inv.exposure-guidance.v4",
            "sha256:a8294ae3e873494ce17cfd07ff5c04ca2fbb0e34f22fd0374553c715fcdf3ed1",
        ),
    ];

    /// Second halves of `name.ext` file names, which are not operations.
    const FILE_EXTENSIONS: &[&str] = &[
        "json", "md", "toml", "txt", "log", "sqlite", "py", "rs", "sh", "yaml", "yml", "csv",
        "html", "webp",
    ];

    /// Inline code spans and the lines of fenced code blocks.
    fn code(markdown: &str) -> Vec<String> {
        let mut spans = Vec::new();
        let mut prose = String::new();
        let mut fenced = false;
        for line in markdown.lines() {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
            } else if fenced {
                spans.push(line.trim().to_owned());
            } else {
                prose.push_str(line);
                prose.push('\n');
            }
        }
        let parts: Vec<&str> = prose.split('`').collect();
        assert!(parts.len() % 2 == 1, "unbalanced backticks");
        spans.extend(
            parts
                .iter()
                .skip(1)
                .step_by(2)
                .map(|span| span.replace('\n', " ")),
        );
        spans
    }

    /// Tokens shaped like an operation name: `family.verb`.
    fn operation_names(text: &str) -> Vec<String> {
        let token = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | ':' | '/');
        text.split(|c: char| !token(c))
            .map(|word| word.trim_end_matches('.'))
            .filter(|word| !word.contains(['/', ':', '-']))
            .filter_map(|word| word.split_once('.'))
            .filter(|(family, verb)| {
                let part = |s: &str| {
                    s.starts_with(|c: char| c.is_ascii_lowercase())
                        && s.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                };
                part(family) && part(verb) && !FILE_EXTENSIONS.contains(verb)
            })
            .map(|(family, verb)| format!("{family}.{verb}"))
            .collect()
    }

    /// Everything in `markdown` that the binary does not support.
    fn problems(markdown: &str) -> (Vec<String>, usize) {
        check(markdown, false)
    }

    /// Whether a word stands for a value the reader supplies: `<ID>`,
    /// `ID`, `DIR/producer-bundle.json`, `SEQUENCE]`, `$scratch`.
    fn placeholder(word: &str) -> bool {
        let word = word.trim_matches(|c| matches!(c, '[' | ']' | '(' | ')' | ',' | '.'));
        word.contains(['<', '$', '…'])
            || word.contains("...")
            || word.split(['/', '=', '-', ':']).next().is_some_and(|head| {
                head.len() > 1
                    && head
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            })
    }

    /// Whether a parse error only reflects schematic text: a required
    /// argument or subcommand left out, or a value written as a
    /// placeholder. Unknown commands, flags and literal values are real.
    fn schematic(error: &clap::Error) -> bool {
        use clap::error::{ContextKind, ContextValue, ErrorKind};
        match error.kind() {
            ErrorKind::MissingRequiredArgument
            | ErrorKind::MissingSubcommand
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            | ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion => true,
            ErrorKind::InvalidValue | ErrorKind::ValueValidation => matches!(
                error.get(ContextKind::InvalidValue),
                Some(ContextValue::String(value)) if placeholder(value)
            ),
            // An upper-case word in a subcommand slot, as in `capture OP`.
            ErrorKind::InvalidSubcommand => matches!(
                error.get(ContextKind::InvalidSubcommand),
                Some(ContextValue::String(value)) if value.len() > 1
                    && value.chars().all(|c| c.is_ascii_uppercase() || c == '_')
            ),
            _ => false,
        }
    }

    /// [`problems`], and with `lenient` the text may also be schematic:
    /// optional parts in brackets, placeholders for values, required parts
    /// left out ([`schematic`]).
    fn check(markdown: &str, lenient: bool) -> (Vec<String>, usize) {
        let tree = crate::shortcuts::augment(crate::Cli::command());
        let catalog = standard_catalog();
        let pack = load(GENERAL_PACK_SOURCE).expect("built-in pack loads");
        let subcommands: Vec<String> = tree
            .get_subcommands()
            .flat_map(|sub| std::iter::once(sub.get_name()).chain(sub.get_all_aliases()))
            .map(str::to_owned)
            .collect();
        let mut problems = Vec::new();
        let mut commands = 0;
        for span in code(markdown) {
            let start = format!("{PROGRAM_NAME} ");
            let found = if lenient {
                invocation(&span, &subcommands)
            } else {
                span.find(&start).map(|at| span[at..].to_owned())
            };
            let Some(invocation) = found else {
                if span.starts_with("inv ") {
                    problems.push(format!("`{span}` uses the alias; use {PROGRAM_NAME}"));
                }
                continue;
            };
            commands += 1;
            let mut argv = words(&invocation);
            if lenient {
                // A here-document or redirection ends the command line.
                let line = invocation.split(" <<").next().unwrap_or_default();
                argv = words(&angle_placeholders(line))
                    .into_iter()
                    .map(|word| word.trim_matches(['[', ']']).to_owned())
                    .take_while(|word| !matches!(word.as_str(), "..." | "…" | "#"))
                    .filter(|word| !word.is_empty() && word != "OPTIONS")
                    .collect();
            }
            match tree.clone().try_get_matches_from(&argv) {
                Ok(matches) => {
                    if let Some(("guide", guide)) = matches.subcommand()
                        && let Some(name) = guide.get_one::<String>("name")
                        && name.chars().any(|c| c.is_ascii_lowercase())
                        && pack.card(name).is_none()
                    {
                        problems.push(format!("`{span}`: no built-in card {name}"));
                    }
                }
                Err(error) if lenient && schematic(&error) => {}
                Err(error) => problems.push(format!(
                    "`{span}`: {}",
                    error.to_string().lines().next().unwrap_or_default()
                )),
            }
        }
        let families: std::collections::BTreeSet<&str> = catalog
            .available()
            .filter_map(|op| op.info().name.split_once('.').map(|(family, _)| family))
            .collect();
        let verbs: std::collections::BTreeSet<&str> = catalog
            .available()
            .filter_map(|op| op.info().name.split_once('.').map(|(_, verb)| verb))
            .collect();
        for name in operation_names(markdown) {
            if catalog.available().any(|op| op.info().name == name) {
                continue;
            }
            // In general prose `family.word` is also a field path or a rule
            // name; it claims an operation when both halves are an
            // operation family and verb the catalog uses.
            let (family, verb) = name.split_once('.').expect("an operation-shaped name");
            if !lenient || (families.contains(family) && verbs.contains(verb)) {
                problems.push(format!("{name} is not a catalog operation"));
            }
        }
        (problems, commands)
    }

    /// The command a code span runs, from `investigation` on: at the start
    /// of the span, or after a shell separator, prompt or label, and not as
    /// part of a path (`target/debug/investigation`) or of prose ("the
    /// investigation is complete": the next word must be a subcommand or a
    /// flag). Inside quotes the command ends at the closing quote; a version
    /// line (`investigation 0.1.0`) is output, not a command.
    fn invocation(span: &str, subcommands: &[String]) -> Option<String> {
        let start = format!("{PROGRAM_NAME} ");
        let mut from = 0;
        while let Some(found) = span[from..].find(&start) {
            let at = from + found;
            from = at + start.len();
            let before = span[..at].chars().next_back();
            if before.is_some_and(|c| !(c.is_whitespace() || matches!(c, '(' | '\'' | '"'))) {
                continue;
            }
            let mut rest = span[at..].to_owned();
            if let Some(quote) = before.filter(|c| matches!(c, '\'' | '"'))
                && let Some(end) = rest.find(quote)
            {
                rest.truncate(end);
            }
            let next = words(&angle_placeholders(&rest[start.len()..]))
                .into_iter()
                .next()
                .unwrap_or_default();
            let next = next.trim_start_matches('[');
            if next.starts_with('-') || subcommands.iter().any(|name| name == next) {
                return Some(rest);
            }
            if (at == 0 || before.is_some_and(|c| matches!(c, '\'' | '"')))
                && !next.starts_with(|c: char| c.is_ascii_digit())
                && !next.is_empty()
            {
                // A span, or a quoted string, that starts with the program
                // names a command.
                return Some(rest);
            }
        }
        None
    }

    /// `<NAME>` placeholders as plain words, so they parse as values.
    fn angle_placeholders(line: &str) -> String {
        let mut out = String::new();
        let mut inside = false;
        for c in line.chars() {
            match c {
                '<' if !inside => {
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

    #[test]
    fn pages_name_only_commands_cards_and_operations_the_binary_has() {
        for page in PAGES {
            let (problems, commands) = problems(page.body);
            assert!(
                problems.is_empty(),
                "docs/agent page {}: {problems:#?}",
                page.name
            );
            assert!(
                commands >= 5,
                "{} shows only {commands} commands",
                page.name
            );
        }
    }

    #[test]
    fn readme_names_only_commands_and_operations_the_binary_has() {
        let readme = include_str!("../../../README.md");
        let (problems, commands) = problems(readme);
        assert!(problems.is_empty(), "README.md: {problems:#?}");
        assert!(commands >= 5);
    }

    /// REL.02 (bn-29bx): every command, subcommand, flag, literal value and
    /// operation named in a document must exist in this binary. Schematic
    /// text is allowed only as [`check`] describes it, or by an explicit
    /// [`ALLOWED`] entry with its reason.
    ///
    /// The documents are the ones this binary was built with
    /// ([`DOCUMENTS`]), not the files on disk when the test runs: a worktree
    /// synced while the suite ran once paired a document naming a new
    /// subcommand with a binary built before it (bn-3dy5).
    #[test]
    fn every_document_names_only_what_the_binary_has() {
        let mut failures = Vec::new();
        let mut used = vec![false; ALLOWED.len()];
        let mut total = 0;
        for (relative, text) in DOCUMENTS {
            let (problems, commands) = check(text, true);
            total += commands;
            failures.extend(
                problems
                    .into_iter()
                    .filter(|problem| {
                        let allowed = ALLOWED.iter().position(|(file, text, _)| {
                            *file == *relative && problem.contains(text)
                        });
                        if let Some(index) = allowed {
                            used[index] = true;
                        }
                        allowed.is_none()
                    })
                    .map(|problem| format!("{relative}: {problem}")),
            );
        }
        assert!(
            total > 200,
            "only {total} commands found in {} files",
            DOCUMENTS.len()
        );
        for ((file, text, _), used) in ALLOWED.iter().zip(used) {
            assert!(
                used,
                "ALLOWED names {text} in {file}, which no longer needs it"
            );
        }
        assert!(
            failures.is_empty(),
            "{} problem(s):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    /// Text a document shows on purpose although the binary lacks it, with
    /// why: (file, text the problem names, reason).
    const ALLOWED: &[(&str, &str, &str)] = &[
        (
            "docs/cli-conventions-audit.md",
            "`investigation doctor --fix`",
            "the audit quotes the wrong usage whose error it checked",
        ),
        (
            "docs/cli-conventions-audit.md",
            "`investigation --format json vault render --bogus`",
            "the audit quotes the wrong usage whose repair it checked",
        ),
        (
            "docs/first-use.md",
            "`investigation \"Initial discovery of …\"`",
            "the trial report quotes prose a participant ran as a command",
        ),
    ];

    #[test]
    fn the_document_check_allows_schematic_text_and_nothing_else() {
        for fine in [
            "`investigation --workspace <WS> show ID [--depth full] --format json`",
            "Usage: `investigation show [OPTIONS] <ID>`",
            "```sh\ninvestigation --workspace ./w record --input - <<'EOF'\n```",
            "`investigation evidence read`, then `target/debug/investigation x`",
            "`it does not mean the investigation is complete`",
            "`investigation 0.1.0` and `investigation --version`",
            "`run(['investigation', 'brief'])` and `measurement.unit`",
        ] {
            let (problems, _) = check(fine, true);
            assert!(problems.is_empty(), "{fine}: {problems:?}");
        }
        for bad in [
            "`investigation frobnicate ID`",
            "`investigation show ID --depth everything`",
            "`investigation brief --no-such-flag`",
            "`script -qc 'investigation shwo ID' /dev/null`",
            "`investigation \"Initial discovery of …\"`",
            "`inv brief`",
            "`case.explode` and `review.close`",
        ] {
            assert!(!check(bad, true).0.is_empty(), "{bad}");
        }
    }

    #[test]
    fn the_checker_rejects_what_the_binary_lacks() {
        for bad in [
            "`investigation frobnicate --format json`",
            "`investigation show ID --depth everything`",
            "`investigation guide no-such-card`",
            "`investigation brief --no-such-flag`",
            "`inv brief`",
            "`case.explode`",
            "Record a review.verdict for it.",
        ] {
            assert_eq!(problems(bad).0.len(), 1, "{bad}");
        }
        let (fine, _) = problems(
            "`investigation guide NAME` and `investigation.toml`, ./results.log, `review.submit`.",
        );
        assert!(fine.is_empty(), "{fine:?}");
    }

    #[test]
    fn pages_are_versioned_and_changed_text_needs_a_new_version() {
        for page in PAGES {
            assert!(
                page.body.contains(&format!("<!-- {} -->", page.version)),
                "{} lacks its version marker",
                page.name
            );
            let digest = hash_bytes(page.body.as_bytes()).to_string();
            assert!(
                RELEASED.contains(&(page.name, page.version, digest.as_str())),
                "docs/agent page {} changed (now {digest}). Give it a new version and \
                 add a RELEASED row; do not edit a released row.",
                page.name
            );
        }
        let mut versions: Vec<_> = RELEASED.iter().map(|(n, v, _)| (n, v)).collect();
        versions.sort();
        versions.dedup();
        assert_eq!(versions.len(), RELEASED.len(), "a version names two texts");
    }

    #[test]
    fn page_names_are_unique_and_the_agents_page_exists() {
        let mut names: Vec<_> = PAGES.iter().map(|page| page.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), PAGES.len());
        assert_eq!(onboard_pointer()["version"], AGENT_INSTRUCTIONS_VERSION);
    }
}
