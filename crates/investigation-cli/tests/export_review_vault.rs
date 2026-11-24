//! `export --kind review-vault` through the public binary (W11.07) over
//! Scenarios A, C and D, for every audience:
//!
//! - the same snapshot gives byte-identical folders, whatever order the
//!   workspace files were created in;
//! - after raising records to restricted, no file name or byte of any
//!   audience's copy names a withheld record, repeats a withheld title or
//!   holds a withheld artifact's retained bytes;
//! - an existing destination is refused and left as it was;
//! - every wiki link of the copy resolves to a file inside it;
//! - nothing of the working vault (stale notes, conflict copies, the
//!   person-owned folders) reaches the copy;
//! - the copy needs no community plugin, script or shell: no plugin code
//!   block, inline query, template tag, plugin frontmatter key, inline
//!   field, script or embedded frame, and no `.obsidian` folder (bn-dla3,
//!   plan §20.4 V14).
//!
//! Every run clears the environment, so nothing outside the workspace can
//! be reached.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use investigation_core::{
    classification::Classifier,
    header::ClassificationLevel,
    identity::{Id, Kind},
    review_ops,
    wire::CommitResult,
};
use investigation_render::{
    Audience,
    export::{ExportSnapshot, IdentifierMode, Pin, kind, plan_export},
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

#[path = "support/fixtures.rs"]
mod fixtures;

use fixtures::SCENARIOS;

const AUDIENCES: [&str; 3] = ["public", "internal", "restricted"];

fn run(cwd: &Path, args: &[&str]) -> (i32, Value) {
    let empty_path = cwd.join(".empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_clear()
        .env("PATH", &empty_path)
        .args(args)
        .args(["--format", "json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value = serde_json::from_str(&stdout).unwrap_or_else(|_| {
        panic!(
            "{args:?}: {stdout} {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap(), value)
}

fn export(root: &Path, workspace: &Path, case: &str, audience: &str, out: &Path) -> (i32, Value) {
    run(
        root,
        &[
            "--workspace",
            workspace.to_str().unwrap(),
            "--case",
            case,
            "export",
            "--kind",
            "review-vault",
            "--audience",
            audience,
            "--out",
            out.to_str().unwrap(),
        ],
    )
}

fn cases(workspace: &Path) -> Vec<String> {
    let mut cases: Vec<String> = fs::read_dir(workspace.join("state/cases"))
        .unwrap()
        .map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.split('.').next().unwrap().to_owned()
        })
        .collect();
    cases.sort();
    cases.dedup();
    cases
}

/// Every file below `root` by its `/`-separated relative path.
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, directory: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path: PathBuf = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, found);
            } else {
                let relative = path.strip_prefix(root).unwrap().to_str().unwrap();
                found.insert(relative.replace('\\', "/"), fs::read(&path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, root, &mut found);
    found
}

/// Commit one operation; `false` when the record is already at that level,
/// which an earlier raise can cause through inheritance.
fn record(workspace: &Path, case: Id, key: &str, op: &str, payload: Value) -> bool {
    let mut store = Store::open(Workspace::open(workspace).unwrap());
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":key,
        "case_id":case, "actor":"agent:exporter", "preconditions":[],
        "operations":[{"op":op, "payload":payload}]
    });
    match store.record(&serde_json::to_vec(&input).unwrap()) {
        Ok(receipt) => {
            assert_eq!(receipt.result, CommitResult::Committed, "{key}");
            true
        }
        Err(failure) if failure.error.message.contains("already handled") => false,
        Err(failure) => panic!("{key}: {failure:?}"),
    }
}

/// Raise records of every kind that bears on others to restricted: an
/// observation a decision rests on, the first observation, objection,
/// review and artifact. Returns how many were raised.
fn raise_some(workspace: &Path, case: Id) -> usize {
    let mut store = Store::open(Workspace::open(workspace).unwrap());
    let state = store.case(case).unwrap().state;
    let mut chosen: BTreeSet<Id> = BTreeSet::new();
    chosen.extend(state.records_of_kind(Kind::Decision).find_map(|decision| {
        review_ops::required_reads(&state, decision)
            .ok()?
            .into_iter()
            .map(|reference| reference.id)
            .find(|id| id.kind() == Kind::Observation)
    }));
    for kind in [
        Kind::Observation,
        Kind::Objection,
        Kind::Review,
        Kind::Artifact,
    ] {
        chosen.extend(state.records_of_kind(kind).next().map(|r| r.header.id));
    }
    // Records already restricted, by label or inheritance, stay as they are.
    let classifier = Classifier::new(&state);
    chosen.retain(|id| {
        state.current(*id).is_some_and(|record| {
            classifier.effective(record).level != ClassificationLevel::Restricted
        })
    });
    drop(store);
    let mut raised = 0;
    for id in &chosen {
        let mut store = Store::open(Workspace::open(workspace).unwrap());
        let revision = store
            .case(case)
            .unwrap()
            .state
            .current(*id)
            .unwrap()
            .header
            .revision
            .0;
        drop(store);
        raised += usize::from(record(
            workspace,
            case,
            &format!("raise-{id}"),
            "classification.raise",
            json!({"subject_ref":{"id":id, "revision":revision},
                "level":"restricted", "reason":"Restricted for the review-vault test."}),
        ));
    }
    raised
}

/// What the audience policy withholds from `case` at its head: every
/// withheld ID, the authored text of withheld records that no shown record
/// repeats, and the retained bytes of withheld artifacts.
struct Withheld {
    ids: Vec<Id>,
    texts: Vec<String>,
    blobs: Vec<Vec<u8>>,
}

fn withheld(workspace: &Path, case: Id, audience: Audience) -> Withheld {
    let mut store = Store::open(Workspace::open(workspace).unwrap());
    let snapshot = ExportSnapshot::read(&mut store, case, &Pin::default()).unwrap();
    let plan = plan_export(kind("review-vault").unwrap(), &snapshot, audience).unwrap();
    let state = snapshot.case().state();
    let ids: Vec<Id> = plan
        .exclusions()
        .iter()
        .map(|exclusion| exclusion.id)
        .filter(|id| *id != case)
        .collect();
    // A remapped (public) copy gives no withheld record an opaque value
    // either: nothing in it names one.
    if plan.identifiers().mode() == IdentifierMode::Remapped {
        for id in &ids {
            let canonical = id.to_string();
            assert!(
                plan.identifiers().exported(&canonical).is_none(),
                "{canonical} was given an exported value"
            );
        }
    }
    let shown: String = state
        .records()
        .filter(|record| !ids.contains(&record.header.id))
        .map(|record| record.body.to_string())
        .collect();
    let mut texts = Vec::new();
    let mut blobs = Vec::new();
    for id in &ids {
        let record = state.current(*id).unwrap();
        for field in [
            "title",
            "statement",
            "wording",
            "description",
            "claim",
            "summary",
            "rationale",
            "scope",
        ] {
            let text = record.body[field]
                .as_str()
                .or_else(|| record.body["value"][field].as_str());
            let Some(text) = text else { continue };
            // A plain prefix survives Markdown escaping unchanged.
            let probe: String = text
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == ' ')
                .take(40)
                .collect();
            if probe.trim().len() >= 16 && !shown.contains(&probe) {
                texts.push(probe);
            }
        }
        if id.kind() == Kind::Artifact
            && let Some(digest) = record.body["digest"].as_str()
            && let Ok(bytes) = store.retained_bytes(&digest.parse().unwrap())
            && bytes.len() >= 16
        {
            blobs.push(bytes);
        }
    }
    Withheld { ids, texts, blobs }
}

fn assert_no_leak(label: &str, out: &BTreeMap<String, Vec<u8>>, withheld: &Withheld) {
    for (path, bytes) in out {
        let text = format!("{path}\n{}", String::from_utf8_lossy(bytes));
        let lower = text.to_ascii_lowercase();
        let plain = text.replace('\\', "");
        for id in &withheld.ids {
            for form in [
                id.uuid().hyphenated().to_string(),
                id.uuid().simple().to_string(),
            ] {
                assert!(
                    !lower.contains(&form),
                    "{label}: {path} names withheld {id}"
                );
            }
        }
        for probe in &withheld.texts {
            assert!(!plain.contains(probe), "{label}: {path} repeats {probe:?}");
        }
        for blob in &withheld.blobs {
            assert!(
                !bytes
                    .windows(blob.len())
                    .any(|window| window == blob.as_slice()),
                "{label}: {path} holds withheld retained bytes"
            );
        }
    }
}

/// Every `[[target]]` or `[[target|label]]` of every note names a file of
/// the copy: a note (`target.md`) or an attachment (`target` itself).
fn assert_links_resolve(label: &str, out: &BTreeMap<String, Vec<u8>>) -> usize {
    let mut checked = 0;
    for (path, bytes) in out.iter().filter(|(path, _)| path.ends_with(".md")) {
        let text = String::from_utf8(bytes.clone()).unwrap();
        for piece in text.split("[[").skip(1) {
            let Some(end) = piece.find("]]") else {
                panic!("{label}: {path} has an unclosed link");
            };
            let inner = &piece[..end];
            let target = inner
                .split(['|', '#'])
                .next()
                .unwrap()
                .trim_end_matches('\\');
            assert!(
                out.contains_key(&format!("{target}.md")) || out.contains_key(target),
                "{label}: {path} links {target:?}, which is not in the copy"
            );
            checked += 1;
        }
        assert!(
            !text.contains("](/") && !text.contains("](file:"),
            "{label}: {path}"
        );
    }
    checked
}

#[test]
fn the_same_snapshot_gives_byte_identical_copies_for_every_audience() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for scenario in SCENARIOS {
        let first = fixtures::workspace(scenario, &root.join(scenario).join("a"), false);
        let second = fixtures::workspace(scenario, &root.join(scenario).join("b"), true);
        for case in cases(&first) {
            for audience in AUDIENCES {
                let mut outputs = Vec::new();
                for (label, workspace) in [("a", &first), ("b", &second)] {
                    let out = root.join(format!("{scenario}-{audience}-{label}"));
                    let (exit, value) = export(root, workspace, &case, audience, &out);
                    assert_eq!(exit, 0, "{scenario} {audience}: {value}");
                    assert_eq!(value["kind"], "review-vault");
                    assert_eq!(value["product"], "review");
                    let written = files(&out);
                    assert!(written.contains_key("Start Here.md"));
                    assert!(written.contains_key("Omitted Data.md"));
                    let manifest: Value =
                        serde_json::from_slice(&written["export-manifest.json"]).unwrap();
                    assert_eq!(manifest["resumable"], false);
                    assert_eq!(manifest["product"], "review");
                    outputs.push(written);
                    fs::remove_dir_all(&out).unwrap();
                }
                assert_eq!(outputs[0], outputs[1], "{scenario} {audience}");
            }
        }
    }
}

#[test]
fn no_audience_copy_names_a_withheld_record_title_or_attachment_over_a_c_and_d() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut raised = 0;
    let mut caveated = 0;
    let mut withheld_ids = 0;
    let (mut probes, mut blobs) = (0, 0);
    for scenario in SCENARIOS {
        let workspace = fixtures::workspace(scenario, &root.join(scenario), false);
        for case in cases(&workspace) {
            let case_id: Id = case.parse().unwrap();
            raised += raise_some(&workspace, case_id);
            for audience in AUDIENCES {
                let label = format!("{scenario} {case} {audience}");
                let out = root.join(format!("{scenario}-{audience}-leak"));
                let (exit, value) = export(root, &workspace, &case, audience, &out);
                assert_eq!(exit, 0, "{label}: {value}");
                let receipt = &value["publisher_receipt"];
                let audience_value = match audience {
                    "public" => Audience::Public,
                    "internal" => Audience::Internal,
                    _ => Audience::Restricted,
                };
                let withheld = withheld(&workspace, case_id, audience_value);
                withheld_ids += withheld.ids.len();
                probes += withheld.texts.len();
                blobs += withheld.blobs.len();
                let written = files(&out);
                assert_no_leak(&label, &written, &withheld);
                let all: String = written
                    .values()
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                    .collect();
                match audience {
                    "restricted" => {
                        assert_eq!(value["completeness"], "complete", "{label}");
                        assert!(!all.contains("Audience caveat"), "{label}");
                    }
                    "internal" => {
                        let count = receipt["caveated_count"].as_u64().unwrap();
                        caveated += count;
                        // Every caveated record's note carries its caveat.
                        for item in receipt["caveated"].as_array().unwrap() {
                            let id = item["id"].as_str().unwrap();
                            let note = written
                                .iter()
                                .find(|(path, _)| path.ends_with(&format!("/{id}.md")))
                                .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned());
                            if let Some(note) = note {
                                assert!(note.contains("- Audience caveat: "), "{label}: {id}");
                            }
                        }
                    }
                    _ => assert!(!all.contains("Audience caveat"), "{label}"),
                }
                fs::remove_dir_all(&out).unwrap();
            }
        }
    }
    assert!(raised >= 3, "records were raised in every scenario");
    assert!(
        withheld_ids > 0 && caveated > 0,
        "the scan saw withheld and caveated records"
    );
    assert!(
        probes > 0 && blobs > 0,
        "the scan looked for withheld titles ({probes}) and retained bytes ({blobs})"
    );
}

/// Code block languages that only a community plugin renders.
const PLUGIN_FENCES: [&str; 12] = [
    "dataview",
    "dataviewjs",
    "tasks",
    "button",
    "tracker",
    "chart",
    "kanban",
    "excalidraw",
    "meta-bind",
    "templater",
    "query-js",
    "jsx",
];
/// Frontmatter keys that hand a note to a community plugin.
const PLUGIN_KEYS: [&str; 4] = ["kanban-plugin", "excalidraw-plugin", "dg-publish", "banner"];

/// Every place in the files of a copy that needs a community plugin, a
/// script or an application setting to be read: "path: what".
fn plugin_syntax(out: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let mut found = Vec::new();
    for (path, bytes) in out {
        if path.starts_with(".obsidian/") || path.contains("/.obsidian/") {
            found.push(format!("{path}: an application settings folder"));
            continue;
        }
        if !path.ends_with(".md") {
            continue;
        }
        let text = String::from_utf8(bytes.clone()).unwrap();
        let mut fenced = false;
        let mut frontmatter = text.starts_with("---\n");
        for (number, line) in text.lines().enumerate() {
            let at = format!("{path}:{}", number + 1);
            if number > 0 && frontmatter && line == "---" {
                frontmatter = false;
                continue;
            }
            if frontmatter {
                let key = line.split(':').next().unwrap_or_default().trim();
                if PLUGIN_KEYS.contains(&key) {
                    found.push(format!("{at}: plugin frontmatter key {key}"));
                }
                continue;
            }
            let trimmed = line.trim_start();
            if let Some(info) = trimmed
                .strip_prefix("```")
                .or_else(|| trimmed.strip_prefix("~~~"))
            {
                if !fenced {
                    let language = info.split_whitespace().next().unwrap_or_default();
                    if PLUGIN_FENCES.contains(&language.to_ascii_lowercase().as_str()) {
                        found.push(format!("{at}: a {language} code block"));
                    }
                }
                fenced = !fenced;
                continue;
            }
            if fenced {
                continue;
            }
            for (pattern, what) in [
                ("`= ", "an inline query"),
                ("`$=", "an inline script query"),
                ("<%", "a template tag"),
                ("%>", "a template tag"),
                ("<script", "a script"),
                ("<iframe", "an embedded frame"),
                ("javascript:", "a script link"),
            ] {
                if line.to_ascii_lowercase().contains(pattern) {
                    found.push(format!("{at}: {what}"));
                }
            }
            // An inline field (`key:: value`) is read only by a query plugin.
            let field = line.split_once("::").is_some_and(|(key, _)| {
                let key = key.trim_start_matches(['-', '*', ' ', '[', '(']);
                !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '_' | '-'))
            });
            if field {
                found.push(format!("{at}: an inline field"));
            }
        }
    }
    found
}

#[test]
fn every_audience_copy_reads_without_a_community_plugin() {
    let mut planted = BTreeMap::new();
    planted.insert(
        "Cases/x/Overview.md".to_owned(),
        b"---\nkanban-plugin: basic\n---\n# T\n\n```dataview\nLIST\n```\n\nStatus:: open\n`= this.file.name`\n<% tp.date.now() %>\n"
            .to_vec(),
    );
    planted.insert(
        ".obsidian/community-plugins.json".to_owned(),
        b"[]".to_vec(),
    );
    // The scan finds each kind it looks for, and nothing in plain text.
    assert_eq!(
        plugin_syntax(&planted).len(),
        7,
        "{:#?}",
        plugin_syntax(&planted)
    );
    let mut plain = BTreeMap::new();
    plain.insert(
        "Cases/x/Overview.md".to_owned(),
        b"---\ntitle: T\n---\n# T\n\n```json\n{\"a\": 1}\n```\n\nSee [[Cases/x/Overview|the case]]. Ratio 1:2, time 12:30.\n"
            .to_vec(),
    );
    assert_eq!(plugin_syntax(&plain), Vec::<String>::new());

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut notes = 0;
    for scenario in SCENARIOS {
        let workspace = fixtures::workspace(scenario, &root.join(scenario), false);
        for case in cases(&workspace) {
            for audience in AUDIENCES {
                let out = root.join(format!("{scenario}-{audience}-plugins"));
                let (exit, value) = export(root, &workspace, &case, audience, &out);
                assert_eq!(exit, 0, "{value}");
                let copy = files(&out);
                notes += copy.keys().filter(|path| path.ends_with(".md")).count();
                let found = plugin_syntax(&copy);
                assert!(found.is_empty(), "{scenario} {audience}: {found:#?}");
                fs::remove_dir_all(&out).unwrap();
            }
        }
    }
    assert!(notes > 100, "{notes} notes scanned");
}

#[test]
fn every_link_of_every_audience_copy_resolves_inside_it() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut checked = 0;
    for scenario in SCENARIOS {
        let workspace = fixtures::workspace(scenario, &root.join(scenario), false);
        for case in cases(&workspace) {
            raise_some(&workspace, case.parse().unwrap());
            for audience in AUDIENCES {
                let out = root.join(format!("{scenario}-{audience}-links"));
                let (exit, value) = export(root, &workspace, &case, audience, &out);
                assert_eq!(exit, 0, "{value}");
                checked += assert_links_resolve(&format!("{scenario} {audience}"), &files(&out));
                fs::remove_dir_all(&out).unwrap();
            }
        }
    }
    assert!(checked > 100, "{checked} links checked");
}

#[test]
fn an_existing_destination_is_refused_and_left_as_it_was() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-a", &root.join("ws"), false);
    let case = cases(&workspace).remove(0);
    let empty = root.join("empty");
    fs::create_dir(&empty).unwrap();
    let occupied = root.join("occupied");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("keep.md"), b"keep").unwrap();
    for (out, before) in [(&empty, BTreeMap::new()), (&occupied, files(&occupied))] {
        let (exit, value) = export(root, &workspace, &case, "internal", out);
        assert_ne!(exit, 0, "{value}");
        assert_eq!(value["error"]["code"], "INVALID_INPUT", "{value}");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("does not exist"),
            "{value}"
        );
        assert_eq!(files(out), before);
    }
    let fresh = root.join("fresh");
    let (exit, value) = export(root, &workspace, &case, "internal", &fresh);
    assert_eq!(exit, 0, "{value}");
    assert_eq!(value["created_directory"], true);
    let (exit, _) = export(root, &workspace, &case, "internal", &fresh);
    assert_ne!(exit, 0, "a second export into the same folder is refused");
}

#[test]
fn nothing_from_the_working_vault_reaches_the_copy() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for scenario in SCENARIOS {
        let workspace = fixtures::workspace(scenario, &root.join(scenario), false);
        for case in cases(&workspace) {
            let vault = workspace.join("vault");
            let folder = vault.join("Cases").join(&case);
            fs::create_dir_all(folder.join("Questions")).unwrap();
            fs::write(
                folder.join("Overview (conflicted copy).md"),
                "STALEMARK conflict copy",
            )
            .unwrap();
            fs::write(folder.join("Questions/q_stale.md"), "STALEMARK stale note").unwrap();
            fs::write(folder.join("Overview.md"), "STALEMARK edited overview").unwrap();
            fs::write(vault.join("Notes/mine.md"), "HUMANMARK private note").unwrap();
            fs::write(vault.join("Boards/board.canvas"), "HUMANMARK board").unwrap();
            fs::write(vault.join("Start Here.md"), "STALEMARK old entry").unwrap();
            for audience in AUDIENCES {
                let out = root.join(format!("{scenario}-{audience}-vault"));
                let (exit, value) = export(root, &workspace, &case, audience, &out);
                assert_eq!(exit, 0, "{value}");
                let listed: BTreeSet<String> = value["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|path| path.as_str().unwrap().to_owned())
                    .collect();
                let written = files(&out);
                assert_eq!(
                    written.keys().cloned().collect::<BTreeSet<_>>(),
                    listed,
                    "{scenario} {audience}: only planned files"
                );
                for (path, bytes) in &written {
                    let text = String::from_utf8_lossy(bytes);
                    assert!(
                        !text.contains("STALEMARK") && !text.contains("HUMANMARK"),
                        "{scenario} {audience}: {path}"
                    );
                    assert!(
                        !path.starts_with("Notes/")
                            && !path.starts_with("Boards/")
                            && !path.starts_with("Inbox/")
                            && !path.contains("conflicted"),
                        "{path}"
                    );
                }
                fs::remove_dir_all(&out).unwrap();
            }
        }
    }
}
