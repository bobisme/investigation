//! The casebook's file-checkable promises (plan §20.4) over the Scenario A,
//! C and D vaults, with no viewer application, interpreter or network.
//!
//! Each test copies a checked-in workspace from `fixtures/vault-goldens/`
//! into a temporary folder. The golden vaults beside them change only through
//! `scripts/vault-goldens.py --write`; no test writes one.
//!
//! | §20.4 item | Where it is checked |
//! |---|---|
//! | Fresh vault useful with community plugins disabled | `scenario_vaults_need_no_community_plugin`, `every_conclusion_and_decision_has_a_ladder_from_overview_to_evidence_selector`; `safe_markdown::hostile_titles_render_into_inert_vault_notes` |
//! | Summaries, direct source links, frontier, challenge reviews, retrospectives without a plugin | `vault_scenarios` (Overview agrees with `brief`, Start Here commands run), the ladder test here |
//! | Core CLI and rendering work without the viewer installed | every CLI run here and in `vault_commands` clears the environment and puts an empty folder on `PATH` |
//! | Links, aliases and excerpt blocks resolve | `every_link_alias_and_excerpt_anchor_resolves`; `vault_scenarios` |
//! | Identical snapshots give identical owned bytes, excluding local view state | `golden_vaults_match_their_checked_in_workspaces`, `identical_snapshots_publish_identical_bytes_across_runs_environments_and_file_order`; `render::same_snapshot_and_policy_yield_a_byte_identical_plan`, `comparison_matrix::the_matrix_is_stable_across_renders` |
//! | Title changes rename no file and break no link | `title_changes_keep_file_names_anchors_and_links`; `render::recorded_title_changes_keep_identity_files_and_links` |
//! | Note paths and anchors survive unrelated changes | `unrelated_changes_keep_every_note_path_and_anchor` |
//! | Human Notes, Boards, Inbox byte-identical through render and migration | `human_folders_keep_their_bytes_through_scenario_renders`; `human_areas::human_areas_are_untouched_by_render_rerender_drift_manifest_loss_and_generation_switch`, `vault_commands` |
//! | Edits, property edits, deletes, renames and conflict copies are drift | `drift::*` (one test per kind), `vault_commands::render_succeeds_with_drift_present_and_lists_it` |
//! | Render failure advertises partial or stale state; the CLI still reports canonical state | `generations::a_failed_render_advertises_partial_while_canonical_reads_continue`, `generations::status_reports_updating_while_a_render_runs_and_stale_after_a_commit`, `vault_commands::a_render_that_stops_reports_partial_and_the_next_render_completes_it` |
//! | Fresh read-sets, never mixed-generation Markdown | `generations::every_note_names_its_snapshot_and_other_cases_never_churn`, `generations::killing_a_render_at_each_file_leaves_a_state_the_next_render_completes` |
//! | Hostile HTML, embeds, plugin fences, YAML and URI text stay inert | `safe_markdown::*`, `scenario_vaults_need_no_community_plugin` |
//! | Restricted decisive objection withheld or caveated; nothing hidden leaks; old snapshots follow today's policy | `audience_policy::*` |
//!
//! Opening Bases and Canvas in a supported viewer needs the viewer (W12.07).
//! There is no review export or viewer plugin yet, so the export and plugin
//! setup items have nothing to check in files.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::Command,
};

use investigation_core::{
    identity::{Id, Kind},
    wire::{CommitResult, Receipt},
};
use investigation_render::{
    RENDERER_VERSION, RenderPlan, bases,
    compat::{Profile, Tier, detect},
    layout, map,
    text::EXCERPT_LANGUAGE,
};
use investigation_store::Store;
use pulldown_cmark::{CodeBlockKind, Event, LinkType, Parser, Tag};
use serde_json::{Value, json};

#[path = "../../investigation-render/tests/support/vault.rs"]
mod vault;

#[path = "../../investigation-render/tests/support/human.rs"]
mod human;

#[path = "support/fixtures.rs"]
mod fixtures;

use fixtures::SCENARIOS;

fn text<'a>(plan: &'a RenderPlan, path: &str) -> &'a str {
    std::str::from_utf8(
        plan.entry(path)
            .unwrap_or_else(|| panic!("no {path}"))
            .content(),
    )
    .unwrap()
}

/// The block anchors a note defines (` ^anchor` at a line end).
fn anchors(note: &str) -> BTreeSet<String> {
    note.lines()
        .filter_map(|line| {
            let (_, anchor) = line.rsplit_once(" ^")?;
            (!anchor.is_empty() && !anchor.contains(' ')).then(|| anchor.to_owned())
        })
        .collect()
}

/// Every link in `before` resolves in `after`: the target note exists and
/// still defines the linked block anchor.
fn links_still_resolve(before: &RenderPlan, after: &RenderPlan) -> Vec<String> {
    let mut problems = Vec::new();
    for entry in before.entries() {
        let path = entry.path().as_str();
        if !path.ends_with(".md") {
            continue;
        }
        for (target, anchor) in vault::links(text(before, path)) {
            match after.entry(&format!("{target}.md")) {
                None => problems.push(format!("{path}: {target} is gone")),
                Some(note) => {
                    let note = std::str::from_utf8(note.content()).unwrap();
                    if let Some(anchor) = anchor
                        && !anchors(note).contains(&anchor)
                    {
                        problems.push(format!("{path}: {target}#^{anchor} is gone"));
                    }
                }
            }
        }
    }
    problems
}

fn submit(store: &mut Store, key: &str, case: Option<Id>, operation: Value) -> Receipt {
    submit_after(store, key, case, json!([]), operation)
}

fn submit_after(
    store: &mut Store,
    key: &str,
    case: Option<Id>,
    preconditions: Value,
    operation: Value,
) -> Receipt {
    let input = json!({
        "schema_version": "inv.command.v0-alpha",
        "command_id": key,
        "case_id": case,
        "actor": "agent:vault-fixture",
        "preconditions": preconditions,
        "operations": [operation],
    });
    let receipt = store.record(&serde_json::to_vec(&input).unwrap()).unwrap();
    assert_eq!(receipt.result, CommitResult::Committed, "{receipt:?}");
    receipt
}

/// `vault render` with a cleared environment, an empty folder on `PATH` and
/// the given variables.
fn publish(workspace: &Path, empty_path: &Path, environment: &[(&str, &Path)]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .arg("--workspace")
        .arg(workspace)
        .args(["vault", "render", "--format", "json"])
        .env_clear()
        .env("PATH", empty_path)
        .envs(environment.iter().map(|(key, value)| (*key, *value)))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["state"], "complete", "{report:#}");
    report
}

#[test]
fn golden_vaults_match_their_checked_in_workspaces() {
    let directory = tempfile::tempdir().unwrap();
    for scenario in SCENARIOS {
        let workspace = fixtures::workspace(scenario, &directory.path().join(scenario), false);
        let (_, plan) = fixtures::plan(&workspace);
        let golden = fixtures::golden(scenario);
        assert!(golden.contains_key(layout::START_HERE), "{scenario}");
        let changed = fixtures::differences(&golden, &fixtures::files(&plan));
        if changed.is_empty() {
            continue;
        }
        let start = String::from_utf8_lossy(&golden[layout::START_HERE]).into_owned();
        let advice = if start.contains(&format!("`{RENDERER_VERSION}`")) {
            format!(
                "The golden was rendered by {RENDERER_VERSION} too: generated bytes changed \
                 without a renderer version bump. If the change is intended, bump \
                 RENDERER_VERSION, then run `{}`.",
                fixtures::REGENERATE
            )
        } else {
            format!(
                "RENDERER_VERSION is now {RENDERER_VERSION}; run `{}`.",
                fixtures::REGENERATE
            )
        };
        panic!(
            "{scenario}: {} generated files differ from the golden vault: {changed:#?}\n{advice}",
            changed.len()
        );
    }
}

/// An Overview's `## Sources` names each Source and Artifact once, at the
/// latest revision the answer read, even when the answer also reads an
/// earlier revision through an Artifact acquired before it (bn-bn1m). The
/// Scenario D answer reads four Sources at two revisions each.
#[test]
fn an_overview_lists_each_source_and_artifact_once() {
    let directory = tempfile::tempdir().unwrap();
    let mut collapsed = 0;
    for scenario in SCENARIOS {
        let workspace = fixtures::workspace(scenario, &directory.path().join(scenario), false);
        let (_, plan) = fixtures::plan(&workspace);
        for (path, bytes) in fixtures::files(&plan) {
            if !path.ends_with("/Overview.md") {
                continue;
            }
            let note = String::from_utf8(bytes).unwrap();
            let Some(section) = note.split("\n## Sources\n").nth(1) else {
                continue;
            };
            let section = section.split("\n## ").next().unwrap();
            let mut seen = BTreeSet::new();
            for line in section.lines().filter(|line| line.starts_with("- [[")) {
                let target = line["- [[".len()..].split('|').next().unwrap().to_owned();
                assert!(seen.insert(target), "{scenario}/{path} lists twice: {line}");
                collapsed += usize::from(line.contains(", also revision"));
            }
        }
    }
    assert!(
        collapsed >= 4,
        "only {collapsed} entries name an earlier revision"
    );
}

/// Every stored record in the scenario workspaces is readable, so no golden
/// note shows a renderer fallback for an unreadable body ("The stored body
/// cannot be read as an Observation") in place of the record. A record kind
/// gaining a key in v1 that a renderer reader rejects shows up here (bn-1brz).
#[test]
fn no_golden_note_shows_an_unreadable_record_fallback() {
    const FALLBACKS: [&str; 2] = ["cannot be read", "could not be read"];
    let mut found = Vec::new();
    for scenario in SCENARIOS {
        for (path, bytes) in fixtures::golden(scenario) {
            let note = String::from_utf8_lossy(&bytes);
            for (number, line) in note.lines().enumerate() {
                if FALLBACKS.iter().any(|fallback| line.contains(fallback)) {
                    found.push(format!("{scenario}/{path}:{}: {line}", number + 1));
                }
            }
        }
    }
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn identical_snapshots_publish_identical_bytes_across_runs_environments_and_file_order() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let empty_path = root.join("empty-path");
    fs::create_dir(&empty_path).unwrap();
    let home_one = root.join("home-one");
    let home_two = root.join("a much/deeper home");
    fs::create_dir_all(&home_one).unwrap();
    fs::create_dir_all(&home_two).unwrap();
    let first: Vec<(&str, &Path)> = vec![
        ("TZ", Path::new("UTC")),
        ("LANG", Path::new("C")),
        ("LC_ALL", Path::new("C")),
        ("HOME", &home_one),
    ];
    let second: Vec<(&str, &Path)> = vec![
        ("TZ", Path::new("Pacific/Chatham")),
        ("LANG", Path::new("tr_TR.UTF-8")),
        ("LC_ALL", Path::new("tr_TR.UTF-8")),
        ("HOME", &home_two),
        ("TMPDIR", &home_two),
    ];
    for scenario in SCENARIOS {
        // Two copies at different paths, one written in reverse order.
        let one = fixtures::workspace(scenario, &root.join(scenario).join("one"), false);
        let two = fixtures::workspace(scenario, &root.join(scenario).join("copy two/x"), true);
        // A viewer's local view state is the reader's, not a generated file.
        fs::create_dir(two.join("vault/.obsidian")).unwrap();
        fs::write(
            two.join("vault/.obsidian/workspace.json"),
            "{\"open\":[]}\n",
        )
        .unwrap();

        let report_one = publish(&one, &empty_path, &first);
        let report_two = publish(&two, &empty_path, &second);
        assert_eq!(report_one["generation"], report_two["generation"]);
        assert_eq!(report_one["renderer"], RENDERER_VERSION);
        let bytes_one = fixtures::owned(&one.join("vault"));
        let bytes_two = fixtures::owned(&two.join("vault"));
        assert_eq!(
            fixtures::differences(&bytes_one, &bytes_two),
            Vec::<String>::new(),
            "{scenario}: two environments"
        );
        assert_eq!(
            fs::read(two.join("vault/.obsidian/workspace.json")).unwrap(),
            b"{\"open\":[]}\n"
        );
        // The published bytes are the plan's, and the golden's.
        let (_, plan) = fixtures::plan(&one);
        assert_eq!(
            fixtures::differences(&fixtures::files(&plan), &bytes_one),
            Vec::<String>::new(),
            "{scenario}: published vs planned"
        );
        assert_eq!(
            fixtures::differences(&fixtures::golden(scenario), &bytes_one),
            Vec::<String>::new(),
            "{scenario}: published vs golden; run `{}` after an intended change",
            fixtures::REGENERATE
        );

        // Rendering again in the other environment rewrites nothing.
        let again = publish(&one, &empty_path, &second);
        assert_eq!(again["generation"], report_one["generation"]);
        assert_eq!(again["counts"]["written"], 0, "{again:#}");
        assert_eq!(again["counts"]["unchanged"], again["counts"]["planned"]);
        assert_eq!(fixtures::owned(&one.join("vault")), bytes_one);
    }
}

/// The case the Scenario D workspace holds; it is still active.
fn scenario_d_case(plan: &RenderPlan) -> Id {
    let cases: Vec<Id> = plan
        .entries()
        .iter()
        .filter_map(|entry| entry.projects())
        .filter(|id| id.kind() == Kind::Case)
        .collect();
    assert_eq!(cases.len(), 1);
    cases[0]
}

/// Every planned note's path and the block anchors it defines.
fn note_anchors(plan: &RenderPlan) -> BTreeMap<String, BTreeSet<String>> {
    plan.entries()
        .iter()
        .map(|entry| {
            let path = entry.path().to_string();
            let anchors = anchors(text(plan, &path));
            (path, anchors)
        })
        .collect()
}

#[test]
fn unrelated_changes_keep_every_note_path_and_anchor() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = fixtures::workspace("scenario-d", &directory.path().join("d"), false);
    let (_, before) = fixtures::plan(&workspace);
    let case = scenario_d_case(&before);
    let mut store = fixtures::open(&workspace);

    // A new case changes no note of this one; only Start Here lists it.
    submit(
        &mut store,
        "unrelated-case",
        None,
        json!({"op":"case.create", "payload":{"title":"An unrelated case", "mode":"record"}}),
    );
    let (_, beside) = fixtures::plan(&workspace);
    for entry in before.entries() {
        let path = entry.path().as_str();
        if path != layout::START_HERE {
            assert_eq!(text(&beside, path), text(&before, path), "{path} changed");
        }
    }

    // A new question in the same case moves no note and drops no anchor.
    submit(
        &mut store,
        "unrelated-question",
        Some(case),
        json!({"op":"question.create", "payload":{
            "wording":"Which registry was searched last?", "importance":"normal"}}),
    );
    let (snapshot, after) = fixtures::plan(&workspace);
    let kept = note_anchors(&after);
    let mut defined = 0;
    for (path, anchors) in note_anchors(&before) {
        assert_eq!(kept.get(&path), Some(&anchors), "{path}");
        defined += anchors.len();
    }
    assert!(defined >= 10, "only {defined} anchors checked");
    assert_eq!(links_still_resolve(&before, &after), Vec::<String>::new());
    let report = vault::check(&after, &snapshot);
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
}

#[test]
fn title_changes_keep_file_names_anchors_and_links() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = fixtures::workspace("scenario-d", &directory.path().join("d"), false);
    let (snapshot, before) = fixtures::plan(&workspace);
    let case = scenario_d_case(&before);
    let state = snapshot
        .cases()
        .find(|found| found.case_id() == case)
        .unwrap()
        .state();
    // The hypothesis most other notes link to.
    let overview = layout::case_overview(case).unwrap();
    let hypothesis = state
        .records()
        .filter(|record| record.header.id.kind() == Kind::Hypothesis)
        .max_by_key(|record| {
            let target = layout::record_note(case, record.header.id)
                .unwrap()
                .unwrap()
                .as_str()
                .strip_suffix(".md")
                .unwrap()
                .to_owned();
            before
                .entries()
                .iter()
                .filter(|entry| {
                    vault::links(text(&before, entry.path().as_str()))
                        .iter()
                        .any(|(linked, _)| *linked == target)
                })
                .count()
        })
        .unwrap();
    let mut replacement = serde_json::Map::new();
    for key in [
        "question_ref",
        "possible_falsifiers",
        "rival_refs",
        "assumption_refs",
        "applicability",
    ] {
        if let Some(value) = hypothesis.body.get(key) {
            replacement.insert(key.to_owned(), value.clone());
        }
    }
    replacement.insert(
        "statement".to_owned(),
        json!("Renamed ]] | [[Other]] #tag ^inv-sel-forged"),
    );
    let case_revision = state.current(case).unwrap().header.revision;
    let mut store = fixtures::open(&workspace);
    submit_after(
        &mut store,
        "rename-case",
        Some(case),
        json!([{"id": case, "revision": case_revision}]),
        json!({"op":"case.amend", "payload":{
            "title":"Renamed case ]] | [[Overview]]", "reason":"Sharper title"}}),
    );
    submit(
        &mut store,
        "rename-hypothesis",
        Some(case),
        json!({"op":"hypothesis.amend", "payload":{
            "hypothesis_ref":{"id":hypothesis.header.id, "revision":hypothesis.header.revision},
            "rationale":"Sharper wording",
            "replacement":replacement}}),
    );
    let (snapshot, after) = fixtures::plan(&workspace);

    let paths = |plan: &RenderPlan| -> Vec<String> {
        plan.entries()
            .iter()
            .map(|entry| entry.path().to_string())
            .collect()
    };
    assert_eq!(
        paths(&before),
        paths(&after),
        "a title change renames nothing"
    );
    let h1 = |plan: &RenderPlan, path: &str| -> String {
        text(plan, path)
            .lines()
            .find(|line| line.starts_with("# "))
            .unwrap()
            .to_owned()
    };
    let hypothesis_note = layout::record_note(case, hypothesis.header.id)
        .unwrap()
        .unwrap();
    for path in [overview.as_str(), hypothesis_note.as_str()] {
        assert_ne!(h1(&before, path), h1(&after, path), "{path}");
        assert!(h1(&after, path).contains("Renamed"), "{path}");
    }
    assert_eq!(note_anchors(&before), note_anchors(&after));
    assert_eq!(links_still_resolve(&before, &after), Vec::<String>::new());
    let report = vault::check(&after, &snapshot);
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
}

/// The text of a note with frontmatter, code, code blocks and links blanked
/// out, keeping line structure.
fn prose(note: &str) -> String {
    let mut masked = String::new();
    let mut last = 0;
    for (event, range) in Parser::new_ext(note, vault::options()).into_offset_iter() {
        let opaque = matches!(event, Event::Code(_))
            || matches!(
                event,
                Event::Start(Tag::MetadataBlock(_) | Tag::CodeBlock(_) | Tag::Link { .. })
            );
        if opaque && range.start >= last {
            masked.push_str(&note[last..range.start]);
            masked.extend(
                note[range.clone()]
                    .chars()
                    .map(|c| if c == '\n' { '\n' } else { ' ' }),
            );
            last = range.end;
        }
    }
    masked.push_str(&note[last.min(note.len())..]);
    masked
}

/// Unescaped syntax that only a community plugin gives meaning to:
/// inline fields (`key:: value`), template tags (`<% %>`, `{{ }}`), and
/// comments hiding text from a reader (`%%`).
fn plugin_syntax(prose: &str) -> Option<String> {
    let chars: Vec<char> = prose.chars().collect();
    let mut at = 0;
    while at < chars.len() {
        if chars[at] == '\\' {
            at += 2;
            continue;
        }
        let pair: String = chars[at..(at + 2).min(chars.len())].iter().collect();
        if ["::", "<%", "{{", "%%"].contains(&pair.as_str()) {
            let context: String = chars[at.saturating_sub(30)..(at + 30).min(chars.len())]
                .iter()
                .collect();
            return Some(format!("{pair} in {context:?}"));
        }
        at += 1;
    }
    None
}

#[test]
fn scenario_vaults_need_no_community_plugin() {
    let mut notes = 0;
    let mut spans = 0;
    for scenario in SCENARIOS {
        for (path, bytes) in fixtures::golden(scenario) {
            // A case map is a core Canvas file (W12.03): checked against
            // JSON Canvas and the compatibility profile in `vault_map`.
            if path.ends_with(&format!("/{}", layout::MAP)) {
                continue;
            }
            notes += 1;
            // Native Bases are a core viewer feature, not a plugin's, and
            // optional: `scenario_vaults_use_only_features_the_compatibility_profile_lists`
            // and `each_base_lists_the_rows_of_its_static_table_and_only_its_case`
            // check them.
            if path.ends_with(".base") {
                continue;
            }
            assert!(
                path.ends_with(".md"),
                "{scenario}: {path} is not plain Markdown"
            );
            let note = std::str::from_utf8(&bytes).unwrap();
            let at = format!("{scenario}: {path}");

            // Frontmatter holds only flat `inv_` properties, aliases and tags.
            if let Some(rest) = note.strip_prefix("---\n") {
                let front = rest.split("\n---\n").next().unwrap();
                for line in front.lines() {
                    if line.starts_with(' ') || line.starts_with('-') {
                        continue;
                    }
                    let key = line.split(':').next().unwrap();
                    assert!(
                        key.starts_with("inv_") || key == "aliases" || key == "tags",
                        "{at}: property {key}"
                    );
                }
            }

            for event in Parser::new_ext(note, vault::options()) {
                match &event {
                    Event::Html(_) | Event::InlineHtml(_) => panic!("{at}: HTML {event:?}"),
                    Event::InlineMath(_) | Event::DisplayMath(_) => panic!("{at}: {event:?}"),
                    Event::FootnoteReference(_) | Event::TaskListMarker(_) => {
                        panic!("{at}: {event:?}")
                    }
                    Event::Start(Tag::Image { .. }) => panic!("{at}: embed {event:?}"),
                    Event::Start(Tag::Link { link_type, .. }) => assert!(
                        matches!(link_type, LinkType::WikiLink { .. }),
                        "{at}: not a wikilink {event:?}"
                    ),
                    // No query, script or other plugin fence: only excerpts.
                    Event::Start(Tag::CodeBlock(kind)) => assert_eq!(
                        kind,
                        &CodeBlockKind::Fenced(EXCERPT_LANGUAGE.into()),
                        "{at}"
                    ),
                    // An inline query or script starts with `=` or `$=`.
                    Event::Code(code) => {
                        spans += 1;
                        let code = code.trim_start();
                        assert!(
                            !code.starts_with('=') && !code.starts_with("$="),
                            "{at}: inline query {code:?}"
                        );
                    }
                    _ => {}
                }
            }
            if let Some(found) = plugin_syntax(&prose(note)) {
                panic!("{at}: {found}");
            }
        }
    }
    assert!(notes > 100 && spans > 1000, "{notes} notes, {spans} spans");
}

/// W12.02, W12.01 and W12.03: every viewer feature the rendered vaults use is
/// listed in the compatibility profile as emitted, and each vault needs only
/// what its workspace's stated viewer release resolves to. Scenario D states
/// no release and gets the plain-Markdown baseline; Scenarios A and C state a
/// supported one and also get native Bases and case maps, which no
/// unsupported or absent release unlocks.
#[test]
fn scenario_vaults_use_only_features_the_compatibility_profile_lists() {
    let profile = Profile::bundled();
    let directory = tempfile::tempdir().unwrap();
    let mut all = BTreeSet::new();
    for scenario in SCENARIOS {
        let golden = fixtures::golden(scenario);
        let found = detect(golden.iter().map(|(p, b)| (p.as_str(), b.as_slice())));
        assert_eq!(profile.unlisted(&found), Vec::<String>::new(), "{scenario}");
        let workspace = fixtures::workspace(scenario, &directory.path().join(scenario), false);
        let release = fixtures::open(&workspace)
            .workspace()
            .config()
            .vault_policy
            .viewer_release
            .clone();
        let resolved = profile.resolve(release.as_deref());
        assert_eq!(
            profile.gated(&found, &resolved),
            Vec::<String>::new(),
            "{scenario}"
        );
        let optional: Vec<&String> = found
            .iter()
            .filter(|id| profile.feature(id).unwrap().tier != Tier::Baseline)
            .collect();
        match release {
            // The baseline vault: nothing optional, so nothing is gated
            // whatever the viewer.
            None => {
                assert_eq!(optional, Vec::<&String>::new(), "{scenario}");
                for installed in [None, Some("0.0.1"), Some("garbage")] {
                    assert!(
                        profile
                            .gated(&found, &profile.resolve(installed))
                            .is_empty()
                    );
                }
            }
            // Both optional file families, and nothing an absent release allows.
            Some(_) => {
                for id in bases::FEATURES.iter().chain(map::FEATURES) {
                    assert!(found.contains(*id), "{scenario}: {id} in {found:?}");
                }
                assert!(!profile.gated(&found, &profile.resolve(None)).is_empty());
            }
        }
        all.extend(found);
    }
    for id in &all {
        let feature = profile.feature(id).unwrap();
        assert_ne!(feature.tier, Tier::Unsupported, "{id}");
        assert!(
            profile.feature(id).unwrap().emitted,
            "{id} is used but the profile says it is not emitted"
        );
    }
    for id in [
        "md.frontmatter_properties",
        "md.wikilinks",
        "md.tables",
        "md.block_anchors",
    ]
    .into_iter()
    .chain(bases::FEATURES.iter().copied())
    {
        assert!(all.contains(id), "{id} not detected in {all:?}");
    }
    // Nothing the profile calls unemitted shows up in a vault: no colour, no
    // text, link or group canvas node.
    for feature in profile.features.iter().filter(|f| !f.emitted) {
        assert!(!all.contains(&feature.id), "{}", feature.id);
    }
}

/// A flat frontmatter block as a YAML mapping.
fn frontmatter(note: &str) -> yaml_serde::Mapping {
    let front = note
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or("", |(front, _)| front);
    yaml_serde::from_str(front).unwrap_or_default()
}

/// Whether a note's properties satisfy a Base filter, reading the filter as
/// the syntax page describes it: a statement string, or an `and`/`or`/`not`
/// list. Only the statement shapes the renderer emits are understood, and a
/// compared property must be present, so a filter never relies on how a
/// viewer treats a missing value. Groups stop at the first decisive member.
fn holds(filter: &yaml_serde::Value, front: &yaml_serde::Mapping) -> bool {
    use yaml_serde::Value as Y;
    match filter {
        Y::String(statement) => {
            let parts: Vec<&str> = statement.splitn(3, ' ').collect();
            let [property, operator, value] = parts[..] else {
                panic!("unexpected statement {statement:?}");
            };
            let actual = front.get(property);
            match operator {
                "==" => {
                    let value = value.strip_prefix('"').unwrap().strip_suffix('"').unwrap();
                    actual.and_then(Y::as_str) == Some(value)
                }
                "!=" => {
                    let value = value.strip_prefix('"').unwrap().strip_suffix('"').unwrap();
                    let actual = actual.unwrap_or_else(|| panic!("{property} absent: {front:?}"));
                    actual.as_str() != Some(value)
                }
                ">" => {
                    let actual = actual.unwrap_or_else(|| panic!("{property} absent: {front:?}"));
                    actual.as_u64().unwrap() > value.parse::<u64>().unwrap()
                }
                other => panic!("unexpected operator {other:?}"),
            }
        }
        Y::Mapping(group) => {
            assert_eq!(group.len(), 1, "{group:?}");
            let (key, members) = group.iter().next().unwrap();
            let mut members = members.as_sequence().unwrap().iter();
            match key.as_str().unwrap() {
                "and" => members.all(|member| holds(member, front)),
                "or" => members.any(|member| holds(member, front)),
                "not" => !members.any(|member| holds(member, front)),
                other => panic!("unexpected group {other:?}"),
            }
        }
        other => panic!("unexpected filter {other:?}"),
    }
}

/// The link target in the first cell of each body row of the first table in
/// `note`.
fn first_column(note: &str) -> Vec<String> {
    note.lines()
        .skip_while(|line| !line.starts_with("| "))
        .take_while(|line| line.starts_with('|'))
        .skip(2)
        .map(|row| {
            let cell = row
                .strip_prefix("| [[")
                .expect("the first cell links a note");
            let end = cell.find("\\|").or_else(|| cell.find("]]")).unwrap();
            cell[..end].to_owned()
        })
        .collect()
}

/// W12.01: each native Base selects exactly the notes its static table lists,
/// every one of them in the Base's case. Evaluated offline over the golden
/// vaults' bytes; opening the Bases in the viewer is the integration gate's.
#[test]
fn each_base_lists_the_rows_of_its_static_table_and_only_its_case() {
    let mut views = BTreeSet::new();
    let mut checked = 0;
    for scenario in SCENARIOS {
        let golden = fixtures::golden(scenario);
        let notes: Vec<(String, yaml_serde::Mapping)> = golden
            .iter()
            .filter_map(|(path, bytes)| {
                let target = path.strip_suffix(".md")?;
                Some((
                    target.to_owned(),
                    frontmatter(std::str::from_utf8(bytes).unwrap()),
                ))
            })
            .collect();
        for (path, bytes) in golden.iter().filter(|(path, _)| path.ends_with(".base")) {
            let base: yaml_serde::Value = yaml_serde::from_slice(bytes).unwrap();
            let table_path = format!("{}.md", path.strip_suffix(".base").unwrap());
            let table = std::str::from_utf8(&golden[&table_path]).unwrap();
            let slug = vault::property(table, "inv_view").unwrap();
            let case = vault::property(table, "inv_case_id").unwrap();
            let matched: Vec<&String> = notes
                .iter()
                .filter(|(_, front)| holds(&base["filters"], front))
                .map(|(target, _)| target)
                .collect();
            for target in &matched {
                assert!(
                    target.starts_with(&format!("Cases/{case}/")),
                    "{path}: {target} is not in its case"
                );
            }
            let rows = match slug.as_str() {
                "frontier" => &table[table.find("\n## Records named by an open gap\n").unwrap()..],
                "timeline" => {
                    &table[table.find("\n## Latest revision of each record\n").unwrap()..]
                }
                _ => table,
            };
            let listed = first_column(rows);
            let mut matched_set: Vec<String> = matched.iter().map(|t| (*t).clone()).collect();
            matched_set.sort();
            let mut listed_set = listed.clone();
            listed_set.sort();
            assert_eq!(listed_set, matched_set, "{scenario}: {path}");
            assert!(!listed.is_empty() || slug == "frontier", "{path}");
            views.insert(slug);
            checked += 1;
        }
    }
    // Scenarios A and C together exercise every shipped Base.
    assert_eq!(views.len(), 14, "{views:?}");
    assert!(checked >= 20, "{checked}");
}

#[test]
fn every_link_alias_and_excerpt_anchor_resolves() {
    let directory = tempfile::tempdir().unwrap();
    for scenario in SCENARIOS {
        let workspace = fixtures::workspace(scenario, &directory.path().join(scenario), false);
        let (snapshot, plan) = fixtures::plan(&workspace);
        let report = vault::check(&plan, &snapshot);
        assert!(
            report.problems.is_empty(),
            "{scenario}: {:#?}",
            report.problems
        );
        assert!(report.pairs > 30, "{scenario}: {} pairs", report.pairs);

        let mut anchored = 0;
        for entry in plan.entries() {
            let path = entry.path().as_str();
            let note = text(&plan, path);
            // Anchors are unique in their note, and a link names each one.
            let defined = anchors(note);
            let count = note
                .lines()
                .filter(|line| line.contains(" ^inv-sel-"))
                .count();
            assert_eq!(defined.len(), count, "{scenario}: {path}");
            anchored += count;
            // An alias is the note's own title, so linking by alias finds
            // it; a record without a title has none and shows its ID.
            if let Some(id) = entry.projects() {
                let title = note
                    .lines()
                    .find_map(|line| line.strip_prefix("# "))
                    .unwrap();
                let alias = note
                    .split_once("\naliases:\n  - \"")
                    .and_then(|(_, rest)| rest.split_once("\"\n"))
                    .map(|(alias, _)| alias.replace("\\\"", "\"").replace("\\\\", "\\"));
                match alias {
                    Some(alias) => assert_eq!(alias, title, "{scenario}: {path}"),
                    None => {
                        assert!(note.contains("\naliases: []\n"), "{scenario}: {path}");
                        assert_eq!(title, id.to_string(), "{scenario}: {path}");
                    }
                }
            }
        }
        assert!(anchored >= 4, "{scenario}: {anchored} anchors");
    }
}

/// Observation notes with an exact selector within two links of `from`,
/// not counting the case Overview.
fn selector_observations(plan: &RenderPlan, case: Id, from: &str) -> Vec<String> {
    let overview = layout::case_overview(case).unwrap().to_string();
    let folder = format!("{}/{case}/", layout::CASES);
    let mut seen = BTreeSet::from([from.to_owned()]);
    let mut frontier = vec![from.to_owned()];
    let mut found = Vec::new();
    for _ in 0..2 {
        let mut next = Vec::new();
        for note in &frontier {
            for (linked, _) in vault::links(text(plan, note)) {
                let linked = format!("{linked}.md");
                if linked == overview
                    || !linked.starts_with(&folder)
                    || !seen.insert(linked.clone())
                {
                    continue;
                }
                if linked.contains("/Observations/") && text(plan, &linked).contains(" ^inv-sel-") {
                    found.push(linked.clone());
                }
                next.push(linked);
            }
        }
        frontier = next;
    }
    found
}

#[test]
fn every_conclusion_and_decision_has_a_ladder_from_overview_to_evidence_selector() {
    let directory = tempfile::tempdir().unwrap();
    let mut ladders = 0;
    for scenario in SCENARIOS {
        let workspace = fixtures::workspace(scenario, &directory.path().join(scenario), false);
        let (snapshot, plan) = fixtures::plan(&workspace);
        let target = |path: &str| path.strip_suffix(".md").unwrap().to_owned();
        for case in snapshot.cases() {
            let overview = layout::case_overview(case.case_id()).unwrap();
            let from_overview: BTreeSet<String> = vault::links(text(&plan, overview.as_str()))
                .into_iter()
                .map(|(linked, _)| linked)
                .collect();
            for record in case.state().records() {
                let kind = record.header.id.kind();
                if !matches!(kind, Kind::Conclusion | Kind::Decision) {
                    continue;
                }
                let path = layout::record_note(case.case_id(), record.header.id)
                    .unwrap()
                    .unwrap();
                let at = format!("{scenario}: {path}");
                // Overview -> entity note.
                assert!(from_overview.contains(&target(path.as_str())), "{at}");
                // Entity note -> an Observation with an exact selector,
                // directly or through the Assessment that weighs it.
                let observations = selector_observations(&plan, case.case_id(), path.as_str());
                assert!(
                    !observations.is_empty(),
                    "{at}: no observation with a selector"
                );
                // Observation -> the evidence note, which links back to the
                // exact selector anchor.
                let grounded = observations.iter().any(|observation| {
                    let note = text(&plan, observation);
                    let selectors = anchors(note);
                    vault::links(note)
                        .into_iter()
                        .filter(|(linked, _)| linked.contains("/Evidence/"))
                        .any(|(artifact, _)| {
                            vault::links(text(&plan, &format!("{artifact}.md")))
                                .iter()
                                .any(|(linked, anchor)| {
                                    *linked == target(observation)
                                        && anchor.as_ref().is_some_and(|a| selectors.contains(a))
                                })
                        })
                });
                assert!(
                    grounded,
                    "{at}: no evidence note links a selector of {observations:?}"
                );
                ladders += 1;
            }
        }
    }
    assert!(ladders >= 6, "only {ladders} ladders");
}

#[test]
fn human_folders_keep_their_bytes_through_scenario_renders() {
    let directory = tempfile::tempdir().unwrap();
    let empty_path = directory.path().join("empty-path");
    fs::create_dir(&empty_path).unwrap();
    let workspace = fixtures::workspace("scenario-c", &directory.path().join("c"), false);
    let vault = workspace.join("vault");
    human::seed(&vault);
    // Human copies of generated notes at their plan paths.
    let golden = fixtures::golden("scenario-c");
    let (sample, bytes) = golden.iter().nth(5).unwrap();
    human::seed_mirror(&vault, sample, bytes);
    let seeded = human::fingerprint(&vault);
    assert!(seeded.len() > 10);

    publish(&workspace, &empty_path, &[]);
    assert_eq!(human::fingerprint(&vault), seeded, "first render");
    submit(
        &mut fixtures::open(&workspace),
        "later-case",
        None,
        json!({"op":"case.create", "payload":{"title":"A later case", "mode":"record"}}),
    );
    let report = publish(&workspace, &empty_path, &[]);
    assert!(
        report["counts"]["written"].as_u64().unwrap() > 0,
        "{report:#}"
    );
    assert_eq!(human::fingerprint(&vault), seeded, "render after a commit");
    assert!(
        report["unknown"].as_array().unwrap().is_empty(),
        "{report:#}"
    );
}
