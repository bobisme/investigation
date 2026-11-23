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
pub(crate) const AGENT_INSTRUCTIONS_VERSION: &str = "inv.agent-instructions.v1";

/// Version of the producer onboarding page.
pub(crate) const PRODUCER_ONBOARDING_VERSION: &str = "inv.producer-onboarding.v2";

/// Version of the consumer migration recipe.
pub(crate) const MIGRATION_GUIDANCE_VERSION: &str = "inv.migration-guidance.v1";

/// Version of the secret-exposure diagnosis and manual remediation page.
pub(crate) const EXPOSURE_GUIDANCE_VERSION: &str = "inv.exposure-guidance.v1";

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
        let tree = crate::shortcuts::augment(crate::Cli::command());
        let catalog = standard_catalog();
        let pack = load(GENERAL_PACK_SOURCE).expect("built-in pack loads");
        let mut problems = Vec::new();
        let mut commands = 0;
        for span in code(markdown) {
            let start = format!("{PROGRAM_NAME} ");
            let Some(at) = span.find(&start) else {
                if span.starts_with("inv ") {
                    problems.push(format!("`{span}` uses the alias; use {PROGRAM_NAME}"));
                }
                continue;
            };
            commands += 1;
            let argv = words(&span[at..]);
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
                Err(error) => problems.push(format!("`{span}`: {error}")),
            }
        }
        for name in operation_names(markdown) {
            if !catalog.available().any(|op| op.info().name == name) {
                problems.push(format!("{name} is not a catalog operation"));
            }
        }
        (problems, commands)
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
