//! `demo PATH --scenario NAME`: a reproducible tutorial workspace.
//!
//! A scenario is one of the checked-in fixture workspaces under
//! `fixtures/vault-goldens/`, built by the scenario proofs
//! (`scripts/vault-goldens.py --rebuild-workspaces`) and embedded by
//! `build.rs`. `demo` does not copy their files: it creates a new workspace
//! and submits each committed transaction again, in commit order, through the
//! same store entry points `record` and `ingest` use. A replay seam
//! ([`investigation_core::replay_seams`]) holds the clock at each
//! transaction's recorded time and draws IDs from the scenario's seed, so
//! identical fixtures always give identical canonical bytes.
//!
//! The new IDs differ from the fixture's, so every later request is rewritten
//! before it is submitted: each record ID, journal head hash, manifest digest
//! and review basis digest the fixture's journal holds is mapped to the one
//! the replay produced at the same place. A request the current core rejects
//! stops the demo with the transaction it stopped at.
//!
//! The replay stops before the first transaction that closes a case, so the
//! tutorial case keeps one deliberate step open. The tutorial path that
//! follows (plan §17.5) lives in [`tutorial`], the one place it is written:
//! `demo --help`, the command's output and the tests all read it.

use std::path::PathBuf;

use investigation_core::{
    PROGRAM_NAME,
    replay_seams::Replay,
    wire::{ErrorCode, Failure},
};
use investigation_store::{
    Store, Workspace,
    workspace::{DemoMarker, WorkspaceError},
};
use serde_json::json;

use crate::{Output, display_argv, selection::workspace_failure};

mod replay;

use replay::Fixture;

mod fixtures {
    include!(concat!(env!("OUT_DIR"), "/demo_fixtures.rs"));
}

/// One tutorial scenario and the fixture it replays.
pub(crate) struct Scenario {
    pub(crate) name: &'static str,
    title: &'static str,
    /// The fixture directory, relative to the repository.
    fixture: &'static str,
    files: &'static [(&'static str, &'static [u8])],
    seed: u64,
    /// The operation the open step needs; the tutorial prints its schema.
    pub(crate) operation: &'static str,
    /// The export the tutorial ends with.
    pub(crate) export: &'static str,
    open_step: &'static str,
}

pub(crate) const SCENARIOS: [Scenario; 4] = [
    Scenario {
        name: "debug",
        title: "intermittent request timeout (plan §17.1)",
        fixture: "fixtures/vault-goldens/scenario-a",
        files: fixtures::SCENARIO_A,
        seed: 0x5eed_0a17,
        operation: "retrospective.record",
        export: "case-report",
        open_step: "The validated root cause awaits its attributed retrospective and case closure.",
    },
    Scenario {
        name: "performance",
        title: "p99 latency regression (plan §17.2)",
        fixture: "fixtures/vault-goldens/scenario-b",
        files: fixtures::SCENARIO_B,
        seed: 0x5eed_0b17,
        operation: "retrospective.record",
        export: "case-report",
        open_step: "The bounded conclusions await the attributed retrospective and case closure.",
    },
    Scenario {
        name: "research",
        title: "contradictory source research (plan §17.4)",
        fixture: "fixtures/vault-goldens/scenario-d",
        files: fixtures::SCENARIO_D,
        seed: 0x5eed_0d17,
        operation: "retrospective.record",
        export: "case-report",
        open_step: "The resolved question awaits a retrospective on why downstream reports added no independent support.",
    },
    Scenario {
        name: "choice",
        title: "architecture choice (plan §17.3)",
        fixture: "fixtures/vault-goldens/scenario-c",
        files: fixtures::SCENARIO_C,
        seed: 0x5eed_0c17,
        operation: "comparison.add",
        export: "decision-record",
        open_step: "The superseding decision after the changed hard threshold awaits the retrospective and case closure; unknown comparison cells stay unknown.",
    },
];

fn scenario(name: &str) -> &'static Scenario {
    SCENARIOS
        .iter()
        .find(|scenario| scenario.name == name)
        .expect("clap accepts only scenario names")
}

#[derive(clap::Args)]
pub(crate) struct Args {
    /// New or empty directory for the tutorial workspace; its parent must exist.
    #[arg(value_name = "PATH")]
    path: PathBuf,
    /// Which worked case to generate.
    #[arg(long, value_name = "NAME", value_parser = clap::builder::PossibleValuesParser::new(SCENARIOS.map(|s| s.name)))]
    scenario: String,
}

/// The tutorial path for `scenario` in the workspace `path`: `demo` itself,
/// then the read-only and owned-output commands that explore it. For
/// `--scenario choice` in `./demo-case` this is plan §17.5 word for word.
pub(crate) fn tutorial(path: &str, scenario: &Scenario) -> Vec<Vec<String>> {
    let at = |rest: &[&str]| {
        let mut argv = vec![PROGRAM_NAME.to_owned(), "--workspace".into(), path.into()];
        argv.extend(rest.iter().map(|part| (*part).to_owned()));
        argv
    };
    vec![
        vec![
            PROGRAM_NAME.into(),
            "demo".into(),
            path.into(),
            "--scenario".into(),
            scenario.name.into(),
        ],
        at(&["onboard", "--format", "json"]),
        at(&["brief", "--format", "json"]),
        at(&["schema", scenario.operation, "--format", "json"]),
        at(&["next", "--format", "json"]),
        at(&["check", "--format", "json"]),
        at(&["vault", "render", "--format", "json"]),
        at(&[
            "export",
            "--kind",
            scenario.export,
            "--audience",
            "internal",
        ]),
    ]
}

/// `demo --help` text after the options.
pub(crate) fn after_help() -> String {
    let mut text = String::from(
        "\
Replays a checked-in scenario's committed transactions through the normal
commit path, with the clock held at each transaction's recorded time and IDs
drawn from the scenario's seed, so the same scenario always gives the same
canonical bytes. The case has real IDs and one deliberate step left open.
PATH must be new or empty and its parent must exist; nothing is written
outside PATH. The workspace marker records that it is a demo.

Scenarios:
",
    );
    for scenario in &SCENARIOS {
        text.push_str(&format!(
            "  {:<13}{}, from {}\n",
            scenario.name, scenario.title, scenario.fixture
        ));
    }
    text.push_str("\nExamples: the shipped tutorial path (plan §17.5), run in order:\n");
    let steps = tutorial("./demo-case", scenario("choice"));
    for argv in &steps {
        text.push_str(&format!("  {}\n", display_argv(argv)));
    }
    text.push_str(
        "Every step after demo reads, or writes only the owned vault or a new export;\n\
         none records, validates or promotes anything.\n\n",
    );
    text.push_str(&format!("Next: {}", display_argv(&steps[1])));
    text
}

pub(crate) fn run(args: Args) -> Result<Output, Failure> {
    let scenario = scenario(&args.scenario);
    let fixture = Fixture::load(scenario.files).map_err(|message| integrity(scenario, &message))?;
    let shown = args.path.to_string_lossy().into_owned();
    let first = fixture
        .events
        .first()
        .ok_or_else(|| integrity(scenario, "The fixture has no transactions."))?;
    let clock = Replay::install(scenario.seed, first.recorded_at.clone())
        .map_err(|_| integrity(scenario, "A replay is already running on this thread."))?;
    let marker = DemoMarker {
        scenario: scenario.name.into(),
        fixture: scenario.fixture.into(),
        seed: scenario.seed,
    };
    let workspace = Workspace::init_demo(&args.path, fixture.config, marker).map_err(|error| {
        match error {
            WorkspaceError::WorkspaceAlreadyExists(_) => Failure::new(
                ErrorCode::InvalidInput,
                format!(
                    "{shown} is not empty. demo writes only a new or empty directory; choose another PATH."
                ),
            ),
            WorkspaceError::InvalidRoot(_) => Failure::new(
                ErrorCode::InvalidInput,
                format!(
                    "The parent directory of {shown} does not exist. demo writes nothing outside PATH; create the parent first."
                ),
            ),
            error => workspace_failure(error),
        }
        .at("/path")
    })?;
    let mut store = Store::open(workspace);
    let replayed = replay::replay(&mut store, &clock, &fixture, replay::Event::closes_case)
        .map_err(|message| integrity(scenario, &message))?;
    let root = store.workspace().root().to_path_buf();
    drop(clock);
    let case = replayed
        .first_case
        .ok_or_else(|| integrity(scenario, "No transaction was replayed."))?;
    let replayed = replayed.transactions;
    let steps = tutorial(&shown, scenario);
    let mut text = format!(
        "demo  {}  {}\nfixture  {}  replayed {replayed} transactions  seed {:#x}\ncase  {case}\nopen step  {}\n\nTutorial:\n",
        scenario.name,
        root.display(),
        scenario.fixture,
        scenario.seed,
        scenario.open_step,
    );
    for argv in &steps[1..] {
        text.push_str(&format!("  {}\n", display_argv(argv)));
    }
    text.push_str(&format!("Next: {}", display_argv(&steps[1])));
    let value = json!({
        "workspace": root,
        "workspace_id": store.workspace().id(),
        "scenario": scenario.name,
        "fixture": scenario.fixture,
        "seed": scenario.seed,
        "transactions": replayed,
        "case_id": case,
        "open_step": scenario.open_step,
        "tutorial": &steps[1..],
        "next": [{"kind": "local_read", "argv": &steps[1]}],
    });
    Ok(Output::success("demo_created", value, text))
}

fn integrity(scenario: &Scenario, message: &str) -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        format!(
            "The {} demo could not be built from {}. {message}",
            scenario.name, scenario.fixture
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        replay::{Mapping, tokens},
        *,
    };
    use crate::read_only::{EFFECTS, Effect};

    #[test]
    fn tokens_find_ids_and_digests_inside_text() {
        let id = "obs_01a20751-8ff0-71b3-8533-fa18bea0abb8";
        let other = "assess_01a20a5f-d58a-73c2-8ace-4e610366c5d3";
        let digest = format!("sha256:{}", "ab".repeat(32));
        let text = format!("gap:rule:{id}+{other} {digest} environment_differs xobs_1");
        let found: Vec<_> = tokens(&text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(found, [id, other, digest.as_str()]);
        let mut map = Mapping::default();
        map.learn(id, "obs_01a20751-8ff0-71b3-8533-000000000001")
            .unwrap();
        assert!(
            map.learn(id, "obs_01a20751-8ff0-71b3-8533-000000000002")
                .is_err()
        );
        assert_eq!(
            map.rewrite_text(&format!("{id}+{other}")),
            format!("obs_01a20751-8ff0-71b3-8533-000000000001+{other}")
        );
    }

    #[test]
    fn choice_tutorial_in_demo_case_is_plan_section_17_5() {
        let plan = include_str!("../../../notes/plan.md");
        let section = plan
            .split("### 17.5 Shipped tutorial command path")
            .nth(1)
            .and_then(|rest| rest.split("```sh").nth(1))
            .and_then(|rest| rest.split("```").next())
            .expect("plan §17.5 has a shell block");
        let planned: Vec<String> = section
            .lines()
            .filter_map(|line| line.strip_prefix("inv "))
            // A trailing shell comment (an erratum note) is not part of the command.
            .map(|line| {
                format!(
                    "{PROGRAM_NAME} {}",
                    line.split(" #").next().unwrap_or(line).trim_end()
                )
            })
            .collect();
        let shipped: Vec<String> = tutorial("./demo-case", scenario("choice"))
            .iter()
            .map(|argv| display_argv(argv))
            .collect();
        assert_eq!(planned, shipped);
    }

    /// No tutorial step after `demo` can record, validate or promote
    /// anything: each is a read, or writes only the owned vault projection or
    /// a new export directory, neither of which changes canonical state.
    #[test]
    fn no_tutorial_step_is_a_domain_write() {
        let owned_outputs = ["vault render", "export"];
        let commands = crate::help_teach::command_tree();
        for scenario in &SCENARIOS {
            for argv in &tutorial("./demo-case", scenario)[1..] {
                let matches = commands
                    .clone()
                    .try_get_matches_from(argv)
                    .unwrap_or_else(|error| panic!("{argv:?}: {error}"));
                let mut path = Vec::new();
                let mut current = &matches;
                while let Some((name, sub)) = current.subcommand() {
                    path.push(name);
                    current = sub;
                }
                let path = path.join(" ");
                let effect = EFFECTS
                    .iter()
                    .find(|(name, _)| *name == path)
                    .map(|(_, effect)| *effect)
                    .unwrap_or_else(|| panic!("{path} is unclassified"));
                assert!(
                    effect == Effect::Read || owned_outputs.contains(&path.as_str()),
                    "{}: tutorial step {path} is {effect:?}",
                    scenario.name
                );
            }
        }
    }
}
