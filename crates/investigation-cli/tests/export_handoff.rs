//! `export --kind handoff` (W11.03) over the checked-in Scenario A, C and D
//! workspaces, for every audience:
//!
//! - both files are planned, and the same snapshot, kind and audience give
//!   the same bytes from two copies of a workspace;
//! - each file contains every §14.4 element, or a line stating its absence;
//! - every record either file names is listed under escalation with its ID,
//!   kind, current revision and title, and the title, summary, full and
//!   evidence reads, pinned to the exported snapshot, are printed once,
//!   runnable as written, on the first record listed; or, in a remapped
//!   (public) export, each is listed as available to the publisher, with no
//!   journal sequence, case head or workspace command anywhere (bn-3qc3);
//! - retained Sources no shown Observation names are listed near the top,
//!   and exactly those;
//! - the question and objective are marked authored, unverified text;
//! - no withheld record is named anywhere in the export, checked on the
//!   publisher's restored copy;
//! - and, through the public binary, a Run with an unknown outcome is listed
//!   under "Do not repeat until reconciled" in both files.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use investigation_core::{
    identity::{Id, Kind},
    state::State,
};
use investigation_render::{
    Audience,
    export::{ExportPlan, ExportSnapshot, IdentifierMode, Pin, kind, plan_export},
};
use serde_json::{Value, json};

#[path = "support/fixtures.rs"]
mod fixtures;

use fixtures::SCENARIOS;

const BRIEF: &str = "handoff-brief.md";
const OVERVIEW: &str = "handoff-overview.md";
const AUDIENCES: [Audience; 3] = [Audience::Public, Audience::Internal, Audience::Restricted];

/// The brief's sections, in order: each §14.4 element has one.
const BRIEF_SECTIONS: [&str; 20] = [
    "## Snapshot",
    "## Read first",
    "## Sources without observations",
    "## Do not repeat until reconciled",
    "## Objective and scope",
    "## Effective conclusions and decisions",
    "## Strongest contradiction and caveat",
    "## Decisive evidence",
    "## Open assumptions",
    "## Unresolved questions",
    "## Missing comparisons",
    "## Frontier highlights",
    "## Audited structural gaps",
    "## Challenge review freshness",
    "## Retrospective and lessons",
    "## Next action",
    "## Stop conditions",
    "## Recording and reading",
    "## Limits",
    "## Escalation",
];

/// The overview's sections and subsections, in order.
const OVERVIEW_SECTIONS: [&str; 20] = [
    "## Summary",
    "## Snapshot",
    "## Sources without observations",
    "### Question and scope",
    "### Conclusions and decisions",
    "### Contradiction and caveat",
    "## What the answer rests on",
    "### Open assumptions",
    "### Unresolved questions",
    "### Missing comparisons",
    "### Do not repeat until reconciled",
    "### Frontier highlights",
    "### Audited structural gaps",
    "### Challenge review freshness",
    "### Retrospective and lessons",
    "### Next action",
    "### Stop conditions",
    "## How to continue",
    "## Limits",
    "## Escalation",
];

/// Lines every file carries for the single-valued elements, present or
/// stated absent.
const ELEMENT_LINES: [&str; 9] = [
    "- Question: ",
    "- Objective",
    "- Strongest outstanding objection: ",
    "- Leading outcome",
    "- Material caveat: ",
    "- Strongest contradiction: ",
    "- Strongest original evidence: ",
    "- Next action: ",
    "- Stop condition",
];

fn cases(workspace: &Path) -> Vec<Id> {
    let mut cases: Vec<Id> = fs::read_dir(workspace.join("state/cases"))
        .unwrap()
        .map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.split('.').next().unwrap().parse().unwrap()
        })
        .collect();
    cases.sort();
    cases.dedup();
    cases
}

fn plan(workspace: &Path, case: Id, audience: Audience) -> ExportPlan {
    let mut store = fixtures::open(workspace);
    let snapshot = ExportSnapshot::read(&mut store, case, &Pin::default()).unwrap();
    plan_export(kind("handoff").unwrap(), &snapshot, audience).unwrap()
}

fn text(plan: &ExportPlan, path: &str) -> String {
    String::from_utf8(plan.files().entry(path).unwrap().content().to_vec()).unwrap()
}

/// Every record ID in `text`, as written (escaped IDs inside quoted prose
/// are not tokens and are not counted).
fn ids(text: &str) -> BTreeSet<Id> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .filter_map(|token| token.parse::<Id>().ok())
        .collect()
}

/// The lines of one section, up to the next heading of any level.
fn section<'t>(text: &'t str, heading: &str) -> Vec<&'t str> {
    text.lines()
        .skip_while(|line| *line != heading)
        .skip(1)
        .take_while(|line| !line.starts_with('#'))
        .filter(|line| !line.trim().is_empty())
        .collect()
}

/// Each heading in order, each followed by at least one list line, and
/// every element line present.
fn assert_every_element(name: &str, text: &str, headings: &[&str]) {
    let mut at = 0;
    for heading in headings {
        let position = text[at..]
            .find(&format!("\n{heading}\n"))
            .unwrap_or_else(|| panic!("{name}: missing or out of order: {heading}\n{text}"));
        at += position + 1;
        let lines = section(text, heading);
        assert!(
            lines.iter().any(|line| {
                line.starts_with("- ")
                    || line.starts_with("1. ")
                    || line.starts_with("We are investigating ")
            }),
            "{name}: {heading} states nothing: {lines:?}"
        );
    }
    for prefix in ELEMENT_LINES {
        assert!(
            text.lines().any(|line| line.starts_with(prefix)),
            "{name}: no line for {prefix:?}"
        );
    }
}

/// Every record named before the escalation section is listed under it,
/// and it names nothing else. A canonical export prints the four pinned
/// reads once, runnable on the first record listed, and lists each record
/// by ID, kind and its current revision at the snapshot; a remapped export
/// lists each as available to the publisher and runs nothing.
fn assert_escalation(name: &str, text: &str, plan: &ExportPlan, state: &State) {
    let snapshot = &plan.manifest().snapshot;
    // The case as the export shows it: remapped when the IDs are.
    let case = snapshot.case_id;
    let pin = format!(
        "--snapshot {} --head-hash {}",
        snapshot.sequence, snapshot.head_hash
    );
    let remapped = plan.identifiers().mode() == IdentifierMode::Remapped;
    let (body, escalation) = text.split_once("\n## Escalation\n").unwrap();
    let mut named = ids(body);
    let listed = ids(escalation);
    // The case ID names the snapshot even when the case record is withheld.
    if !listed.contains(&case) {
        named.remove(&case);
    }
    assert!(
        named.is_subset(&listed),
        "{name}: named without escalation: {:?}",
        named.difference(&listed).collect::<Vec<_>>()
    );
    if remapped {
        let canonical = plan.identifiers().canonical_snapshot();
        for forbidden in [
            "--snapshot",
            "investigation --case",
            "sequence ",
            "(sequences ",
            canonical.head_hash.as_str().trim_start_matches("sha256:"),
        ] {
            assert!(!text.contains(forbidden), "{name}: shows {forbidden:?}");
        }
        for id in &listed {
            let line =
                format!("- `{id}`: title, summary, full and evidence available to the publisher");
            assert!(escalation.contains(&line), "{name}: missing {line}");
        }
        return;
    }
    // One line per record: ID, kind and current revision, in ID order.
    let lines: Vec<(Id, &str)> = escalation
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("- `")?;
            let (id, rest) = rest.split_once("` ")?;
            Some((id.parse().ok()?, rest))
        })
        .collect();
    let mut order: Vec<Id> = lines.iter().map(|(id, _)| *id).collect();
    assert_eq!(
        order.iter().copied().collect::<BTreeSet<_>>(),
        listed,
        "{name}: a record is named in a read but has no line"
    );
    let printed = order.clone();
    order.sort();
    assert_eq!(printed, order, "{name}: records out of ID order");
    for (id, rest) in &lines {
        let record = state.current(*id).unwrap();
        let head = format!(
            "{}, revision {}",
            kind_word(id.kind()),
            record.header.revision.0
        );
        assert!(
            rest.starts_with(&head),
            "{name}: `{id}` {rest} is not {head}"
        );
    }
    // The four reads, once, on the first record listed.
    let example = lines[0].0;
    for tail in [
        format!("show {example} --depth title {pin}"),
        format!("show {example} --depth summary {pin}"),
        format!("show {example} --depth full {pin}"),
        format!("explain {example} {pin}"),
    ] {
        let command = format!("`investigation --case {case} {tail}`");
        assert!(escalation.contains(&command), "{name}: missing {command}");
    }
    assert_eq!(
        escalation.matches(" --snapshot ").count(),
        4,
        "{name}: a pinned read is repeated per record"
    );
}

fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Case => "case",
        Kind::Question => "question",
        Kind::Source => "source",
        Kind::Artifact => "artifact",
        Kind::Observation => "observation",
        Kind::Hypothesis => "hypothesis",
        Kind::Assumption => "assumption",
        Kind::Option => "option",
        Kind::Criterion => "criterion",
        Kind::Comparison => "comparison",
        Kind::Assessment => "assessment",
        Kind::Conclusion => "conclusion",
        Kind::Decision => "decision",
        Kind::Review => "review",
        Kind::Objection => "objection",
        Kind::Annotation => "annotation",
        Kind::Retrospective => "retrospective",
        Kind::Inquiry => "inquiry",
        Kind::Run => "run",
        other => other.prefix(),
    }
}

/// No withheld record is named in any file or the manifest, restored to
/// canonical IDs as the publisher reads it; the case ID names the snapshot
/// and may appear.
fn assert_nothing_withheld_leaks(name: &str, plan: &ExportPlan, case: Id) {
    let mut written = plan.manifest_bytes().to_vec();
    for entry in plan.files().entries() {
        written.extend_from_slice(entry.path().as_str().as_bytes());
        written.extend_from_slice(entry.content());
    }
    let everything = String::from_utf8(plan.identifiers().restore(&written)).unwrap();
    for exclusion in plan.exclusions() {
        if exclusion.id == case {
            continue;
        }
        assert!(
            !everything.contains(&exclusion.id.to_string()),
            "{name}: withheld {} ({}) is named",
            exclusion.id,
            exclusion.reason
        );
    }
}

#[test]
fn every_scenario_and_audience_gets_both_files_with_every_element() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for scenario in SCENARIOS {
        let first = fixtures::workspace(scenario, &root.join(scenario).join("a"), false);
        let second = fixtures::workspace(scenario, &root.join(scenario).join("b"), true);
        for case in cases(&first) {
            for audience in AUDIENCES {
                let name = format!("{scenario} {case} {}", audience.as_str());
                let plan = plan(&first, case, audience);
                let paths: Vec<&str> = plan
                    .files()
                    .entries()
                    .iter()
                    .map(|entry| entry.path().as_str())
                    .collect();
                assert_eq!(paths, [BRIEF, OVERVIEW], "{name}");
                let again = plan_export(
                    kind("handoff").unwrap(),
                    &ExportSnapshot::read(&mut fixtures::open(&second), case, &Pin::default())
                        .unwrap(),
                    audience,
                )
                .unwrap();
                assert_eq!(
                    fixtures::files(plan.files()),
                    fixtures::files(again.files()),
                    "{name}: bytes differ between copies"
                );
                assert_eq!(plan.manifest_bytes(), again.manifest_bytes(), "{name}");

                assert_eq!(
                    plan.identifiers().mode() == IdentifierMode::Remapped,
                    audience == Audience::Public,
                    "{name}"
                );
                let brief = text(&plan, BRIEF);
                let overview = text(&plan, OVERVIEW);
                assert_every_element(&format!("{name} brief"), &brief, &BRIEF_SECTIONS);
                assert_every_element(&format!("{name} overview"), &overview, &OVERVIEW_SECTIONS);
                let state = fixtures::open(&first).case(case).unwrap().state;
                assert_escalation(&format!("{name} brief"), &brief, &plan, &state);
                assert_escalation(&format!("{name} overview"), &overview, &plan, &state);
                assert_nothing_withheld_leaks(&name, &plan, case);
                // The two files tell one story: they name the same records.
                assert_eq!(ids(&brief), ids(&overview), "{name}");
            }
        }
    }
}

/// What the scenarios show beyond structure: their leading outcomes,
/// missing comparison cells and closure, for the internal audience, and
/// nothing but the snapshot for the public one (every scenario record is
/// internal).
#[test]
fn scenario_content_reaches_the_handoff() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();

    let workspace = fixtures::workspace("scenario-d", &root.join("d"), false);
    let case = cases(&workspace)[0];
    let internal = plan(&workspace, case, Audience::Internal);
    let brief = text(&internal, BRIEF);
    let lead = section(&brief, "## Strongest contradiction and caveat");
    assert!(
        lead.iter()
            .any(|line| line.starts_with("- Leading outcome `concl_")),
        "{lead:?}"
    );
    assert!(
        lead.iter()
            .any(|line| line
                .starts_with("- Strongest contradiction: contradicting records `assess_")),
        "{lead:?}"
    );
    // Public names only the case, remapped; restored, it is this case.
    let public = plan(&workspace, case, Audience::Public);
    let exported: Id = public
        .identifiers()
        .exported(&case.to_string())
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(exported, case);
    for path in [BRIEF, OVERVIEW] {
        let written = text(&public, path);
        assert_eq!(ids(&written), BTreeSet::from([exported]), "{path}");
        let restored = String::from_utf8(public.identifiers().restore(written.as_bytes())).unwrap();
        assert_eq!(ids(&restored), BTreeSet::from([case]), "{path}");
    }

    let workspace = fixtures::workspace("scenario-c", &root.join("c"), false);
    let case = cases(&workspace)[0];
    let plan = plan(&workspace, case, Audience::Restricted);
    let brief = text(&plan, BRIEF);
    let missing = section(&brief, "## Missing comparisons");
    assert!(
        missing
            .iter()
            .any(|line| line.contains("`decision.hard_criterion_cell_unknown`")),
        "{missing:?}"
    );
    let stop = section(&brief, "## Stop conditions");
    assert!(
        stop.iter()
            .any(|line| line.starts_with("- Closed with outcome `inconclusive`")),
        "{stop:?}"
    );
    let overview = text(&plan, OVERVIEW);
    assert!(
        overview.contains("Our current choice is `dec_"),
        "{overview}"
    );
}

/// The handoff of a checked-in scenario's every case, for the internal
/// audience, which sees every scenario record.
fn internal_handoffs(root: &Path) -> Vec<(String, Id, State, ExportPlan)> {
    let mut out = Vec::new();
    for scenario in SCENARIOS {
        let workspace = fixtures::workspace(scenario, &root.join(scenario), false);
        for case in cases(&workspace) {
            let state = fixtures::open(&workspace).case(case).unwrap().state;
            let plan = plan(&workspace, case, Audience::Internal);
            out.push((format!("{scenario} {case}"), case, state, plan));
        }
    }
    out
}

/// A Source is listed under "Sources without observations" exactly when no
/// current Observation names it or one of its Artifacts and it has retained
/// bytes to read (DF.05: the successor had to find two such sources itself).
/// Scenario A's timeout case holds three: control traces it never observed.
#[test]
fn retained_sources_no_observation_names_are_listed_near_the_top() {
    let directory = tempfile::tempdir().unwrap();
    let mut listed_anywhere = 0;
    for (name, _, state, plan) in internal_handoffs(directory.path()) {
        let observations: Vec<String> = state
            .records_of_kind(Kind::Observation)
            .map(|record| record.body.to_string())
            .collect();
        let named = |id: Id| {
            let id = format!("\"{id}\"");
            observations.iter().any(|body| body.contains(&id))
        };
        for path in [BRIEF, OVERVIEW] {
            let text = text(&plan, path);
            let lines = section(&text, "## Sources without observations");
            let listed: BTreeSet<Id> = lines
                .iter()
                .filter_map(|line| line.strip_prefix("- `")?.split('`').next()?.parse().ok())
                .collect();
            for source in state.records_of_kind(Kind::Source) {
                let id = source.header.id;
                let artifacts: Vec<_> = state
                    .records_of_kind(Kind::Artifact)
                    .filter(|artifact| artifact.body["source"]["id"] == id.to_string())
                    .collect();
                let observed =
                    named(id) || artifacts.iter().any(|artifact| named(artifact.header.id));
                let retained = artifacts.iter().any(|artifact| {
                    artifact.body["availability"] == "retained"
                        && artifact.body["role"] != "manifest"
                });
                assert_eq!(
                    listed.contains(&id),
                    !observed && retained,
                    "{name} {path}: {id} (observed {observed}, retained {retained}) in {lines:#?}"
                );
                if listed.contains(&id) {
                    let line = lines
                        .iter()
                        .find(|line| line.starts_with(&format!("- `{id}` ")))
                        .unwrap();
                    let head = format!("- `{id}` source, revision {}", source.header.revision.0);
                    assert!(line.starts_with(&head), "{name}: {line}");
                    // Its readable Artifacts follow it, by ID and revision.
                    for artifact in &artifacts {
                        if artifact.body["role"] != "manifest" {
                            let nested = format!(
                                "  - `{}` artifact, revision {}",
                                artifact.header.id, artifact.header.revision.0
                            );
                            assert!(text.contains(&nested), "{name}: no {nested}");
                        }
                    }
                }
            }
            if path == BRIEF && !listed.is_empty() {
                listed_anywhere += listed.len();
                let read_first = section(&text, "## Read first");
                assert!(
                    read_first
                        .iter()
                        .any(|line| line.contains("Sources without observations")),
                    "{name}: {read_first:?}"
                );
            }
        }
    }
    assert_eq!(listed_anywhere, 3, "Scenario A's three control traces");
}

/// Every record a handoff names shows its current revision, so a
/// successor's first write cites it instead of guessing revision 1 (DF.05).
/// The scenarios hold Observations revised to revision 2 and 3.
#[test]
fn every_named_record_shows_its_current_revision() {
    let directory = tempfile::tempdir().unwrap();
    let mut revised_observations = 0;
    for (name, _, state, plan) in internal_handoffs(directory.path()) {
        let brief = text(&plan, BRIEF);
        let (body, escalation) = brief.split_once("\n## Escalation\n").unwrap();
        for id in ids(body) {
            let record = state.current(id).unwrap();
            let line = format!(
                "\n- `{id}` {}, revision {}",
                kind_word(id.kind()),
                record.header.revision.0
            );
            assert!(escalation.contains(&line), "{name}: no {line:?}");
            if id.kind() == Kind::Observation && record.header.revision.0 > 1 {
                revised_observations += 1;
            }
        }
        assert!(
            escalation.contains("{\"id\":ID,\"revision\":N}"),
            "{name}: escalation does not say a write cites the revision"
        );
    }
    assert!(revised_observations > 0, "no revised Observation is named");
}

/// The question and objective are text as written: "over 10 min" in an
/// objective read as a fact to the DF.05 successor. Both files say so
/// before the first authored line.
#[test]
fn the_question_and_objective_are_marked_authored_and_unverified() {
    let directory = tempfile::tempdir().unwrap();
    for (name, _, _, plan) in internal_handoffs(directory.path()) {
        for (path, heading) in [
            (BRIEF, "## Objective and scope"),
            (OVERVIEW, "### Question and scope"),
        ] {
            let text = text(&plan, path);
            let lines = section(&text, heading);
            assert!(
                lines[0].starts_with("- Authored context, not findings: ")
                    && lines[0].contains("unverified until an Observation records it"),
                "{name} {path}: {lines:#?}"
            );
            assert!(lines[1].starts_with("- Question: "), "{name} {path}");
        }
    }
}

/// The escalation reads are printed once, not four per record, so the brief
/// stays small as the case grows: Scenario B's brief was 58,709 bytes with
/// 4 commands for each of its records, and is about 24 KB now.
#[test]
fn the_brief_prints_each_pinned_read_once() {
    let directory = tempfile::tempdir().unwrap();
    for (name, _, _, plan) in internal_handoffs(directory.path()) {
        for path in [BRIEF, OVERVIEW] {
            let text = text(&plan, path);
            let escalation = text.split_once("\n## Escalation\n").unwrap().1;
            assert_eq!(
                escalation.matches("--head-hash").count(),
                4,
                "{name} {path}"
            );
            assert!(
                escalation.matches(" --whole`").count() <= 1,
                "{name} {path}: a whole read is repeated per Artifact"
            );
            assert!(text.len() < 32_768, "{name} {path}: {} bytes", text.len());
        }
    }
}

fn run(cwd: &Path, args: &[&str], stdin: Option<&Value>) -> Value {
    let empty_path = cwd.join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let mut process = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_clear()
        .env("PATH", &empty_path)
        .args(args)
        .args(["--format", "json"])
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(stdin) = stdin {
        let mut pipe = process.stdin.take().unwrap();
        pipe.write_all(&serde_json::to_vec(stdin).unwrap()).unwrap();
    }
    let output = process.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{args:?}: {stdout} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&stdout).unwrap()
}

#[test]
fn an_unknown_outcome_run_is_listed_under_do_not_repeat_until_reconciled() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-a", &root.join("ws"), false);
    let case = cases(&workspace)[0].to_string();
    let workspace_arg = workspace.to_str().unwrap();
    let receipt = run(
        root,
        &["--workspace", workspace_arg, "record", "--input", "-"],
        Some(&json!({
            "schema_version":"inv.command.v1", "command_id":"handoff-unknown-run",
            "case_id":case, "actor":"agent:handoff-test", "preconditions":[],
            "operations":[{"op":"run.record", "payload":{
                "intent":{"kind":"exploration", "intent":"Replay the failing request once."},
                "producer":"agent:handoff-test", "artifact_refs":[],
                "execution_status":"unknown", "evidence_completeness":"partial",
                "evidence_kind":"observational", "variables_changed":[]}}]
        })),
    );
    let run_id = receipt["created"][0]["id"].as_str().unwrap().to_owned();
    assert!(run_id.starts_with("run_"), "{receipt}");

    let out = root.join("handoff");
    let exported = run(
        root,
        &[
            "--workspace",
            workspace_arg,
            "--case",
            &case,
            "export",
            "--kind",
            "handoff",
            "--audience",
            "internal",
            "--out",
            out.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(exported["result"], "exported", "{exported}");
    assert_eq!(
        exported["files"],
        json!([BRIEF, OVERVIEW, "export-manifest.json"])
    );
    let brief = fs::read_to_string(out.join(BRIEF)).unwrap();
    let overview = fs::read_to_string(out.join(OVERVIEW)).unwrap();
    for (name, text, heading) in [
        ("brief", &brief, "## Do not repeat until reconciled"),
        ("overview", &overview, "### Do not repeat until reconciled"),
    ] {
        let listed = section(text, heading);
        let line = listed
            .iter()
            .find(|line| line.contains(&run_id))
            .unwrap_or_else(|| panic!("{name}: {run_id} not in {listed:?}"));
        assert!(
            line.contains("execution `unknown`, evidence `partial`"),
            "{name}: {line}"
        );
        assert!(line.contains("`run.reconcile`"), "{name}: {line}");
        assert!(
            text.split_once("\n## Escalation\n")
                .unwrap()
                .1
                .contains(&format!("\n- `{run_id}` run, revision 1")),
            "{name}: no escalation for {run_id}"
        );
    }
    let read_first = section(&brief, "## Read first");
    assert!(
        read_first
            .iter()
            .any(|line| line.contains("Do not repeat until reconciled")),
        "{read_first:?}"
    );
}
