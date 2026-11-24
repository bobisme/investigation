//! The method walkthroughs in `docs/walkthroughs/` (plan §17.1-§17.4, §20.7)
//! run as written, through the tutorial runner `tests/demo.rs` uses for plan
//! §17.5.
//!
//! - Each walkthrough starts from a fresh `demo` workspace in an empty
//!   directory and runs every `sh` block in order against the built binary.
//! - Every quoted output must still match what the command prints, so a
//!   walkthrough that drifts from the binary fails here, naming the line.
//! - Every refusal exits with the status its comment states, prints the
//!   repair the walkthrough quotes, and leaves the canonical journal
//!   byte-identical; so does every `--dry-run`.
//! - A block that is not run must be labelled schematic, and the index lists
//!   exactly the walkthroughs that run here.
//!
//! With `INVESTIGATION_BLESS_WALKTHROUGHS=1` a quote that no longer matches is
//! rewritten in the document instead (see `tutorial::rewrite`), for review as
//! a git diff; statuses, refusals and dry runs are still checked.

use std::fs;

#[path = "support/tutorial.rs"]
mod tutorial;

/// Each walkthrough and the `demo` scenario it must start from.
const WALKTHROUGHS: [(&str, &str); 4] = [
    ("a-request-timeout.md", "debug"),
    ("b-latency-regression.md", "performance"),
    ("c-architecture-choice.md", "choice"),
    ("d-source-research.md", "research"),
];

fn walk(name: &str) {
    let (_, scenario) = WALKTHROUGHS.iter().find(|(file, _)| *file == name).unwrap();
    let document = format!("docs/walkthroughs/{name}");
    let markdown = fs::read_to_string(tutorial::repository().join(&document)).unwrap();
    let steps = tutorial::parse(&markdown, &document, "investigation");
    assert_eq!(
        steps[0].argv(),
        ["demo", "./demo-case", "--scenario", scenario],
        "{document}: a walkthrough starts from a fresh demo workspace"
    );
    assert!(
        steps.iter().any(|step| step.exit != 0),
        "{document}: a walkthrough shows at least one refusal"
    );
    assert!(
        steps.iter().filter(|step| step.quote.is_some()).count() >= steps.len() / 2,
        "{document}: most commands quote what they print"
    );
    let cwd = tempfile::tempdir().unwrap();
    let mut session = tutorial::Session::new(cwd.path());
    if tutorial::blessing() {
        session = session.bless();
    }
    for step in &steps {
        session.run(step);
    }
    if let Some(blessing) = session.blessing().filter(|b| !b.rewrites.is_empty()) {
        let path = tutorial::repository().join(&document);
        fs::write(&path, tutorial::blessed(&markdown, blessing)).unwrap();
        for rewrite in &blessing.rewrites {
            eprintln!("blessed the quote at {}", rewrite.origin);
        }
    }
}

#[test]
fn walkthrough_a_request_timeout_runs_as_written() {
    walk("a-request-timeout.md");
}

#[test]
fn walkthrough_b_latency_regression_runs_as_written() {
    walk("b-latency-regression.md");
}

#[test]
fn walkthrough_c_architecture_choice_runs_as_written() {
    walk("c-architecture-choice.md");
}

#[test]
fn walkthrough_d_source_research_runs_as_written() {
    walk("d-source-research.md");
}

/// Every walkthrough runs here and the index links each; the index itself
/// holds nothing that looks executable.
#[test]
fn every_walkthrough_is_executed_and_indexed() {
    let directory = tutorial::repository().join("docs/walkthroughs");
    let mut found: Vec<String> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name != "README.md")
        .collect();
    found.sort();
    let expected: Vec<&str> = WALKTHROUGHS.iter().map(|(file, _)| *file).collect();
    assert_eq!(found, expected);
    let index = fs::read_to_string(directory.join("README.md")).unwrap();
    for file in expected {
        assert!(index.contains(&format!("]({file})")), "README.md: {file}");
    }
    assert!(
        tutorial::parse(&index, "docs/walkthroughs/README.md", "investigation").is_empty(),
        "the index runs nothing"
    );
}
