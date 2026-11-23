//! W12.03: the bounded JSON Canvas case map (plan §12.6), over the checked-in
//! Scenario A, C and D workspaces.
//!
//! | Acceptance criterion | Test |
//! |---|---|
//! | Supported-version Canvas opens correctly and every node link resolves | `every_case_map_is_json_canvas_whose_nodes_resolve` (and `vault::check` in `vault_fixtures`) |
//! | The same snapshot yields a byte-identical canvas | `the_same_snapshot_yields_a_byte_identical_map` |
//! | The map is understandable with colour disabled | `the_map_reads_the_same_without_colour` |
//!
//! Gating and focus: `without_a_supported_viewer_release_there_is_no_map`,
//! `the_focus_follows_the_case_and_the_bounds_say_what_was_left_out`.

use std::collections::{BTreeMap, BTreeSet};

use investigation_render::{
    RenderPlan, VerifiedSnapshot, ViewPolicy,
    compat::Profile,
    layout,
    map::{LABELS, LEFT_OUT, LEGEND, MAX_EDGES, MAX_NODES, MAX_PER_COLUMN},
    render,
};
use serde_json::Value;

#[path = "../../investigation-render/tests/support/vault.rs"]
mod vault;

#[path = "support/fixtures.rs"]
mod fixtures;

/// Scenarios whose workspaces record a supported viewer release.
const WITH_VIEWER: [&str; 2] = ["scenario-a", "scenario-c"];

fn text<'a>(plan: &'a RenderPlan, path: &str) -> &'a str {
    std::str::from_utf8(
        plan.entry(path)
            .unwrap_or_else(|| panic!("no {path}"))
            .content(),
    )
    .unwrap()
}

/// Every planned case map: case ID and parsed canvas.
fn maps(plan: &RenderPlan) -> Vec<(String, Value)> {
    plan.entries()
        .iter()
        .filter(|entry| entry.path().as_str().ends_with(".canvas"))
        .map(|entry| {
            let path = entry.path().as_str();
            let case = path.split('/').nth(1).unwrap().to_owned();
            assert_eq!(path, format!("Cases/{case}/{}", layout::MAP));
            (case, serde_json::from_slice(entry.content()).unwrap())
        })
        .collect()
}

fn plan_with(scenario: &str, policy: &ViewPolicy) -> RenderPlan {
    let directory = tempfile::tempdir().unwrap();
    let workspace = fixtures::workspace(scenario, &directory.path().join(scenario), false);
    let snapshot = VerifiedSnapshot::read(&mut fixtures::open(&workspace)).unwrap();
    render(&snapshot, policy).unwrap()
}

fn supported() -> ViewPolicy {
    let profile = Profile::bundled();
    ViewPolicy::default().with_viewer(profile.resolve(Some(&profile.pinned_release.version)))
}

#[test]
fn every_case_map_is_json_canvas_whose_nodes_resolve() {
    let directory = tempfile::tempdir().unwrap();
    let mut checked = 0;
    for scenario in WITH_VIEWER {
        let workspace = fixtures::workspace(scenario, &directory.path().join(scenario), false);
        let (_, plan) = fixtures::plan(&workspace);
        let maps = maps(&plan);
        assert!(!maps.is_empty(), "{scenario}: no case map");
        for (case, canvas) in maps {
            let path = format!("Cases/{case}/{}", layout::MAP);
            assert_eq!(
                vault::canvas_problems(&plan, &path),
                Vec::<String>::new(),
                "{scenario}"
            );
            let nodes = canvas["nodes"].as_array().unwrap();
            let edges = canvas["edges"].as_array().unwrap();
            assert!(nodes.len() <= MAX_NODES && edges.len() <= MAX_EDGES);
            // Every file a node opens lies in this case's generated folder,
            // never in a person's area, and the map itself is tool-owned.
            for node in nodes {
                let file = node["file"].as_str().unwrap();
                assert!(file.starts_with(&format!("Cases/{case}/")), "{file}");
                assert!(!layout::is_human_owned(
                    &investigation_render::PlanPath::new(file).unwrap()
                ));
            }
            for name in [LEGEND, LEFT_OUT] {
                let note = format!("Cases/{case}/{name}");
                assert!(
                    nodes.iter().any(|node| node["file"] == note.as_str()),
                    "{scenario}: no node for {note}"
                );
            }
            // At most MAX_PER_COLUMN records share a column (an x).
            let mut columns: BTreeMap<i64, usize> = BTreeMap::new();
            for node in nodes.iter().filter(|node| node["y"].as_i64().unwrap() >= 0) {
                *columns.entry(node["x"].as_i64().unwrap()).or_default() += 1;
            }
            assert!(columns.values().all(|&count| count <= MAX_PER_COLUMN));
            // The Overview links the map, its legend and the left-out note.
            let overview = text(&plan, &format!("Cases/{case}/Overview.md"));
            let section = vault::section(overview, "Map").unwrap();
            for target in [
                format!("[[Cases/{case}/Map.canvas|"),
                format!("[[Cases/{case}/Map Legend|"),
                format!("[[Cases/{case}/Map Left Out|"),
            ] {
                assert!(section.contains(&target), "{scenario}: {section}");
            }
            checked += 1;
        }
        // The whole-vault check covers the canvas and the links to it.
        let snapshot = VerifiedSnapshot::read(&mut fixtures::open(&workspace)).unwrap();
        let report = vault::check(&plan, &snapshot);
        assert!(report.problems.is_empty(), "{:#?}", report.problems);
    }
    assert!(checked >= 2);
}

#[test]
fn the_same_snapshot_yields_a_byte_identical_map() {
    let directory = tempfile::tempdir().unwrap();
    for scenario in WITH_VIEWER {
        // Copies written in opposite orders: nothing may depend on directory order.
        let forward =
            fixtures::workspace(scenario, &directory.path().join("f").join(scenario), false);
        let reverse =
            fixtures::workspace(scenario, &directory.path().join("r").join(scenario), true);
        let (_, one) = fixtures::plan(&forward);
        let (_, two) = fixtures::plan(&reverse);
        let (_, three) = fixtures::plan(&forward);
        let golden = fixtures::golden(scenario);
        let mut compared = 0;
        for entry in one.entries() {
            let path = entry.path().as_str();
            if !(path.ends_with(".canvas") || path.ends_with(LEGEND) || path.ends_with(LEFT_OUT)) {
                continue;
            }
            assert_eq!(
                two.entry(path).unwrap().content(),
                entry.content(),
                "{path}"
            );
            assert_eq!(
                three.entry(path).unwrap().content(),
                entry.content(),
                "{path}"
            );
            assert_eq!(golden[path], entry.content(), "{scenario}: {path}");
            compared += 1;
        }
        assert!(compared >= 3, "{scenario}");
    }
}

#[test]
fn the_map_reads_the_same_without_colour() {
    let directory = tempfile::tempdir().unwrap();
    let meanings: BTreeMap<&str, &str> = LABELS.iter().copied().collect();
    for scenario in WITH_VIEWER {
        let workspace = fixtures::workspace(scenario, &directory.path().join(scenario), false);
        let (_, plan) = fixtures::plan(&workspace);
        for (case, canvas) in maps(&plan) {
            let raw = text(&plan, &format!("Cases/{case}/{}", layout::MAP));
            assert!(
                !raw.contains("\"color\""),
                "{scenario}: a colour is emitted"
            );
            let legend = text(&plan, &format!("Cases/{case}/{LEGEND}"));
            assert!(legend.contains("The map uses no colour"));
            let mut used = BTreeSet::new();
            for edge in canvas["edges"].as_array().unwrap() {
                // Every arrow says its relation in words the legend explains.
                let label = edge["label"].as_str().unwrap();
                let meaning = meanings
                    .get(label)
                    .unwrap_or_else(|| panic!("{label} is not a legend label"));
                assert!(
                    legend.contains(&format!("| `{label}` | {meaning} |")),
                    "{scenario}: the legend does not explain {label}"
                );
                used.insert(label);
            }
            assert!(!used.is_empty(), "{scenario}: no labelled edge");
            // The legend explains only labels the map uses.
            let rows = legend
                .lines()
                .filter(|line| line.starts_with("| `"))
                .count();
            assert_eq!(rows, used.len(), "{scenario}");
        }
    }
}

#[test]
fn without_a_supported_viewer_release_there_is_no_map() {
    let profile = Profile::bundled();
    // Scenario D records no release: its golden vault is the baseline.
    let golden = fixtures::golden("scenario-d");
    assert!(golden.keys().all(|path| path.ends_with(".md")));
    assert!(golden.keys().all(|path| !path.contains("/Map ")));
    for release in [None, Some("1.12.6"), Some("1.15.2"), Some("not-a-version")] {
        let policy = ViewPolicy::default().with_viewer(profile.resolve(release));
        let plan = plan_with("scenario-c", &policy);
        for entry in plan.entries() {
            let path = entry.path().as_str();
            assert!(
                path.ends_with(".md") && !path.contains("/Map "),
                "{release:?}: {path}"
            );
            assert!(
                !text(&plan, path).contains("\n## Map\n"),
                "{release:?}: {path} has a map section"
            );
        }
    }
    // The same snapshot with a supported release gains the map and nothing
    // else changes except the Overview's new section and the entry note.
    let baseline = plan_with("scenario-c", &ViewPolicy::default());
    let mapped = plan_with("scenario-c", &supported());
    let before: BTreeSet<&str> = baseline
        .entries()
        .iter()
        .map(|e| e.path().as_str())
        .collect();
    let after: BTreeSet<&str> = mapped.entries().iter().map(|e| e.path().as_str()).collect();
    let added: Vec<&&str> = after.difference(&before).collect();
    // The three map files, and the native Bases the same release allows.
    let maps = added.iter().filter(|path| !path.ends_with(".base")).count();
    assert_eq!(maps, 3, "{added:?}");
    assert!(before.is_subset(&after));
    // Record notes are untouched; only the Overview, the view notes (which
    // name their Base) and the entry note change.
    for path in before {
        if path.ends_with("Overview.md") || path.contains("/Views/") || path == layout::START_HERE {
            continue;
        }
        assert_eq!(
            baseline.entry(path).unwrap().content(),
            mapped.entry(path).unwrap().content(),
            "{path}"
        );
    }
}

/// The column (by x) of every node whose file lies in `folder`.
fn columns_of(canvas: &Value, folder: &str) -> BTreeSet<i64> {
    canvas["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| {
            node["file"]
                .as_str()
                .unwrap()
                .contains(&format!("/{folder}/"))
        })
        .map(|node| node["x"].as_i64().unwrap())
        .collect()
}

fn labels(canvas: &Value) -> BTreeSet<String> {
    canvas["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|edge| edge["label"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn the_focus_follows_the_case_and_the_bounds_say_what_was_left_out() {
    // Scenario C is an evaluation: question → options → criteria →
    // comparisons → decision, left to right.
    let plan = plan_with("scenario-c", &supported());
    let (case, canvas) = maps(&plan).remove(0);
    let order: Vec<i64> = [
        "Questions",
        "Options",
        "Criteria",
        "Comparisons",
        "Decisions",
    ]
    .iter()
    .map(|folder| {
        let xs = columns_of(&canvas, folder);
        assert_eq!(xs.len(), 1, "{folder}: {xs:?}");
        *xs.first().unwrap()
    })
    .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
    let found = labels(&canvas);
    for label in [
        "option for",
        "criterion for",
        "compares",
        "chooses",
        "relies on",
    ] {
        assert!(found.contains(label), "{label} in {found:?}");
    }
    assert!(
        found
            .iter()
            .any(|label| label.starts_with("judged against: "))
    );
    // Fifteen comparisons, eight shown: the rest are named and linked.
    let left_out = text(&plan, &format!("Cases/{case}/{LEFT_OUT}"));
    assert!(
        left_out.contains(&format!(
            "| comparisons | {MAX_PER_COLUMN} | 7 | [[Cases/{case}/Views/Comparison Matrix\\|"
        )),
        "{left_out}"
    );
    // The comparison a decision relies on is among those shown.
    let relied: Vec<&Value> = canvas["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|edge| edge["label"] == "relies on")
        .collect();
    assert!(!relied.is_empty());

    // Scenario D is an explanation: question → rival hypotheses →
    // assessments → observations → inquiries and runs → conclusions.
    let plan = plan_with("scenario-d", &supported());
    let (case, canvas) = maps(&plan).remove(0);
    let x = |folder: &str| *columns_of(&canvas, folder).first().unwrap();
    assert!(x("Questions") < x("Hypotheses"));
    assert!(x("Hypotheses") < x("Assessments"));
    assert!(x("Assessments") < x("Observations"));
    assert!(x("Observations") < x("Runs"));
    assert!(x("Runs") < x("Conclusions"));
    let found = labels(&canvas);
    for label in ["proposed answer to", "weighs", "supported by"] {
        assert!(found.contains(label), "{label} in {found:?}");
    }
    assert!(
        found.contains("supports") || found.contains("contradicts"),
        "{found:?}"
    );
    let legend = text(&plan, &format!("Cases/{case}/{LEGEND}"));
    assert!(legend.contains("with an explanation focus"), "{legend}");
    assert!(legend.contains(
        "questions, then rival hypotheses, then assessments, then observations, then inquiries \
         and runs, then conclusions"
    ));
}
