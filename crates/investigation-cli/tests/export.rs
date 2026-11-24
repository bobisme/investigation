//! `export --kind KIND` through the public binary (W11.01): the same snapshot,
//! kind and policy give byte-identical files and manifest; a destination
//! that holds anything is refused before a byte is written; withheld records
//! never reach the export and make it incomplete; an earlier snapshot follows
//! today's classifications; and the manifest matches its published schema.
//!
//! Every run clears the environment and puts an empty folder on `PATH`, so
//! no viewer, model client or network tool could be reached. The export
//! sources are also scanned for network, process and clock use by
//! `investigation_render::export::tests`.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use investigation_core::{
    identity::{Id, Kind},
    review_ops,
    wire::CommitResult,
};
use investigation_store::{Store, Workspace};
use serde_json::{Value, json};

#[path = "support/fixtures.rs"]
mod fixtures;

use fixtures::SCENARIOS;

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

fn export(cwd: &Path, workspace: &Path, case: &str, extra: &[&str]) -> (i32, Value) {
    export_kind(cwd, workspace, case, "case-report", extra)
}

fn export_kind(
    cwd: &Path,
    workspace: &Path,
    case: &str,
    kind: &str,
    extra: &[&str],
) -> (i32, Value) {
    let workspace = workspace.to_str().unwrap();
    let mut args = vec![
        "--workspace",
        workspace,
        "--case",
        case,
        "export",
        "--kind",
        kind,
    ];
    args.extend_from_slice(extra);
    run(cwd, &args)
}

/// Every file below `root` by relative path, with its bytes.
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, found);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                found.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, root, &mut found);
    found
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

fn manifest(directory: &Path) -> Value {
    serde_json::from_slice(&fs::read(directory.join("export-manifest.json")).unwrap()).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    investigation_store::journal::hash_bytes(bytes).to_string()
}

/// Each listed file exists with the listed digest and size, and nothing
/// else but the manifest is in the directory.
fn assert_manifest_lists_the_files(directory: &Path) {
    let manifest = manifest(directory);
    let mut on_disk = files(directory);
    assert!(on_disk.remove("export-manifest.json").is_some());
    let listed: BTreeMap<String, (String, u64)> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            (
                file["path"].as_str().unwrap().to_owned(),
                (
                    file["sha256"].as_str().unwrap().to_owned(),
                    file["bytes"].as_u64().unwrap(),
                ),
            )
        })
        .collect();
    let found: BTreeMap<String, (String, u64)> = on_disk
        .iter()
        .map(|(path, bytes)| (path.clone(), (sha256(bytes), bytes.len() as u64)))
        .collect();
    assert_eq!(listed, found, "{}", directory.display());
}

#[test]
fn the_same_snapshot_kind_and_policy_give_byte_identical_exports() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for scenario in SCENARIOS {
        let first = fixtures::workspace(scenario, &root.join(scenario).join("a"), false);
        let second = fixtures::workspace(scenario, &root.join(scenario).join("b"), true);
        for case in cases(&first) {
            for audience in ["public", "internal", "restricted"] {
                let mut outputs = Vec::new();
                for (label, workspace) in [("a", &first), ("b", &second)] {
                    let out = root.join(format!("{scenario}-{case}-{audience}-{label}"));
                    let (exit, value) = export(
                        root,
                        workspace,
                        &case,
                        &["--audience", audience, "--out", out.to_str().unwrap()],
                    );
                    assert_eq!(exit, 0, "{scenario} {case} {audience}: {value}");
                    assert_eq!(value["result"], "exported");
                    assert_manifest_lists_the_files(&out);
                    outputs.push((value, files(&out)));
                }
                let (first_value, first_files) = &outputs[0];
                let (second_value, second_files) = &outputs[1];
                assert_eq!(
                    first_files, second_files,
                    "{scenario} {case} {audience}: export bytes differ"
                );
                assert_eq!(first_value["export_id"], second_value["export_id"]);
                let manifest: Value =
                    serde_json::from_slice(&first_files["export-manifest.json"]).unwrap();
                assert_eq!(manifest["export_id"], first_value["export_id"]);
                let identifiers = &first_value["publisher_receipt"]["identifiers"];
                assert_eq!(manifest["snapshot"], identifiers["exported_snapshot"]);
                assert_eq!(first_value["snapshot"], identifiers["canonical_snapshot"]);
                assert_eq!(
                    first_value["publisher_receipt"],
                    second_value["publisher_receipt"]
                );
                assert_eq!(manifest["policy"]["audience"], audience);
            }
        }
    }
}

#[test]
fn the_default_destination_is_under_exports_and_a_repeat_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-a", &root.join("ws"), false);
    let case = cases(&workspace).remove(0);
    let (exit, value) = export(root, &workspace, &case, &["--audience", "internal"]);
    assert_eq!(exit, 0, "{value}");
    let destination = value["destination"].as_str().unwrap();
    assert!(
        destination.starts_with("exports/case-report-"),
        "{destination}"
    );
    let written = workspace.join(destination);
    let before = files(&written);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        ["case-report.md", "export-manifest.json"]
    );
    // The same export again lands on the same name, which is not empty.
    let (exit, value) = export(root, &workspace, &case, &["--audience", "internal"]);
    assert_eq!(exit, 2, "{value}");
    assert_eq!(value["error"]["path"], "--out");
    assert_eq!(files(&written), before);
}

#[test]
fn a_destination_that_is_not_empty_or_is_a_link_is_refused_and_left_as_found() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-a", &root.join("ws"), false);
    let case = cases(&workspace).remove(0);

    let occupied = root.join("occupied");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("case-report.md"), b"keep me").unwrap();
    let (exit, value) = export(
        root,
        &workspace,
        &case,
        &[
            "--audience",
            "internal",
            "--out",
            occupied.to_str().unwrap(),
        ],
    );
    assert_eq!(exit, 2, "{value}");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not empty"),
        "{value}"
    );
    assert_eq!(
        files(&occupied),
        BTreeMap::from([("case-report.md".to_owned(), b"keep me".to_vec())])
    );

    // A hidden file counts too.
    let hidden = root.join("hidden");
    fs::create_dir(&hidden).unwrap();
    fs::write(hidden.join(".keep"), b"").unwrap();
    let (exit, _) = export(
        root,
        &workspace,
        &case,
        &["--audience", "internal", "--out", hidden.to_str().unwrap()],
    );
    assert_eq!(exit, 2);
    assert_eq!(files(&hidden).len(), 1);

    // A regular file and a link to an empty directory are refused.
    let file = root.join("file");
    fs::write(&file, b"x").unwrap();
    let (exit, _) = export(
        root,
        &workspace,
        &case,
        &["--audience", "internal", "--out", file.to_str().unwrap()],
    );
    assert_eq!(exit, 2);
    assert_eq!(fs::read(&file).unwrap(), b"x");
    let target = root.join("target");
    fs::create_dir(&target).unwrap();
    let link = root.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let (exit, value) = export(
        root,
        &workspace,
        &case,
        &["--audience", "internal", "--out", link.to_str().unwrap()],
    );
    assert_eq!(exit, 2, "{value}");
    assert!(files(&target).is_empty());

    // An existing empty directory is accepted.
    let empty = root.join("empty");
    fs::create_dir(&empty).unwrap();
    let (exit, value) = export(
        root,
        &workspace,
        &case,
        &["--audience", "internal", "--out", empty.to_str().unwrap()],
    );
    assert_eq!(exit, 0, "{value}");
    assert_eq!(value["created_directory"], false);
    assert_manifest_lists_the_files(&empty);

    // Nothing is written among the workspace's own files.
    let inside = workspace.join("state/export");
    let (exit, value) = export(
        root,
        &workspace,
        &case,
        &["--audience", "internal", "--out", inside.to_str().unwrap()],
    );
    assert_eq!(exit, 2, "{value}");
    assert!(!inside.exists());
}

#[test]
fn withheld_records_never_reach_the_export_and_make_it_incomplete() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-a", &root.join("ws"), false);
    let case = cases(&workspace).remove(0);

    let internal = root.join("internal");
    let (exit, value) = export(
        root,
        &workspace,
        &case,
        &[
            "--audience",
            "internal",
            "--out",
            internal.to_str().unwrap(),
        ],
    );
    assert_eq!(exit, 0, "{value}");
    assert_eq!(value["completeness"], "complete");
    assert_eq!(manifest(&internal)["completeness"], "complete");

    // Scenario A records are internal: a public export withholds them all.
    let public = root.join("public");
    let (exit, value) = export(
        root,
        &workspace,
        &case,
        &["--audience", "public", "--out", public.to_str().unwrap()],
    );
    assert_eq!(exit, 0, "{value}");
    assert_eq!(value["completeness"], "incomplete");
    let receipt = &value["publisher_receipt"];
    let excluded: Vec<&str> = receipt["exclusions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|exclusion| exclusion["id"].as_str().unwrap())
        .collect();
    assert!(!excluded.is_empty());
    let manifest = manifest(&public);
    assert_eq!(manifest["completeness"], "incomplete");
    assert_eq!(manifest["sources"], json!([]));
    for (path, bytes) in files(&public) {
        let text = String::from_utf8(bytes).unwrap();
        for id in &excluded {
            if *id == case {
                continue; // The case ID names the snapshot the export is of.
            }
            assert!(!text.contains(id), "{path} names withheld {id}");
        }
    }

    // --require-complete fails with the exact exclusions and writes nothing.
    let refused = root.join("refused");
    let (exit, value) = export(
        root,
        &workspace,
        &case,
        &[
            "--audience",
            "public",
            "--require-complete",
            "--out",
            refused.to_str().unwrap(),
        ],
    );
    assert_eq!(exit, 5, "{value}");
    assert_eq!(value["error"]["path"], "--require-complete");
    assert_eq!(
        value["error"]["actual"]["exclusion_count"],
        receipt["exclusion_count"]
    );
    assert!(!refused.exists());
}

#[test]
fn an_earlier_snapshot_is_pinned_and_follows_todays_classification() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-a", &root.join("ws"), false);
    let case = cases(&workspace).remove(0);
    let case_id: Id = case.parse().unwrap();

    let head = root.join("head");
    let (exit, value) = export(
        root,
        &workspace,
        &case,
        &["--audience", "internal", "--out", head.to_str().unwrap()],
    );
    assert_eq!(exit, 0, "{value}");
    let sequence = value["snapshot"]["sequence"].as_u64().unwrap();
    let pinned = sequence.to_string();

    // Raise one observation that the head export shows to restricted.
    let mut store = Store::open(Workspace::open(&workspace).unwrap());
    let replay = store.case(case_id).unwrap();
    let observation = replay
        .state
        .records()
        .find(|record| record.header.id.to_string().starts_with("obs_"))
        .unwrap();
    let observation_id = observation.header.id.to_string();
    let report = String::from_utf8(fs::read(head.join("case-report.md")).unwrap()).unwrap();
    assert!(report.contains(&observation_id));
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":"raise-for-export",
        "case_id":case_id, "actor":"agent:exporter", "preconditions":[],
        "operations":[{"op":"classification.raise", "payload":{
            "subject_ref":{"id":observation.header.id, "revision":observation.header.revision.0},
            "level":"restricted", "reason":"Raised after the export."}}]
    });
    let receipt = store.record(&serde_json::to_vec(&input).unwrap()).unwrap();
    assert_eq!(receipt.result, CommitResult::Committed);
    drop(store);

    // The pinned export names the old head, which is now historical, and
    // withholds the raised observation under today's classification.
    let again = root.join("again");
    let (exit, value) = export(
        root,
        &workspace,
        &case,
        &[
            "--audience",
            "internal",
            "--snapshot",
            &pinned,
            "--out",
            again.to_str().unwrap(),
        ],
    );
    assert_eq!(exit, 0, "{value}");
    assert_eq!(value["historical"], true);
    assert_eq!(value["snapshot"]["sequence"], sequence);
    assert!(value["latest_snapshot"]["sequence"].as_u64().unwrap() > sequence);
    assert_eq!(value["completeness"], "incomplete");
    let exclusions = &value["publisher_receipt"]["exclusions"];
    assert!(
        exclusions
            .as_array()
            .unwrap()
            .iter()
            .any(|exclusion| exclusion["id"] == observation_id.as_str()
                && exclusion["reason"] == "audience_today"),
        "{exclusions}"
    );
    for (path, bytes) in files(&again) {
        assert!(
            !String::from_utf8(bytes).unwrap().contains(&observation_id),
            "{path} names the raised observation"
        );
    }
    assert_eq!(manifest(&again)["snapshot"], manifest(&head)["snapshot"]);

    // The same pinned snapshot, kind and policy again: the same bytes.
    let repeat = root.join("repeat");
    let (exit, _) = export(
        root,
        &workspace,
        &case,
        &[
            "--audience",
            "internal",
            "--snapshot",
            &pinned,
            "--out",
            repeat.to_str().unwrap(),
        ],
    );
    assert_eq!(exit, 0);
    assert_eq!(files(&again), files(&repeat));

    // A sequence that is not a verified snapshot is refused.
    let (exit, value) = export(
        root,
        &workspace,
        &case,
        &[
            "--audience",
            "internal",
            "--snapshot",
            "999999",
            "--out",
            root.join("never").to_str().unwrap(),
        ],
    );
    assert_eq!(exit, 3, "{value}");
    assert!(!root.join("never").exists());
}

#[test]
fn a_planned_kind_is_refused_with_the_available_kinds_and_writes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-a", &root.join("ws"), false);
    let case = cases(&workspace).remove(0);
    let out = root.join("handoff");
    let (exit, value) = run(
        root,
        &[
            "--workspace",
            workspace.to_str().unwrap(),
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
    );
    assert_eq!(exit, 2, "{value}");
    assert_eq!(value["error"]["path"], "--kind");
    assert_eq!(
        value["error"]["expected"]["available"],
        json!(["case-report", "decision-record", "workspace-archive"])
    );
    assert!(!out.exists());
    let (exit, value) = run(
        root,
        &["export", "--kind", "vault", "--audience", "internal"],
    );
    assert_eq!(exit, 2, "{value}");
}

#[test]
fn the_manifest_matches_its_published_schema() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (exit, schema) = run(root, &["schema", "export-manifest", "--schema-only"]);
    assert_eq!(exit, 0, "{schema}");
    let published: Value = serde_json::from_slice(
        &fs::read(fixtures::repository().join("schemas/v1/export-manifest.schema.json")).unwrap(),
    )
    .unwrap();
    let mut live = schema.clone();
    live["$id"] = published["$id"].clone();
    assert_eq!(
        live, published,
        "regenerate with check-v1-schemas.py --write"
    );
    let validator = jsonschema::validator_for(&schema).unwrap();
    let workspace = fixtures::workspace("scenario-c", &root.join("ws"), false);
    for case in cases(&workspace) {
        let out: PathBuf = root.join(&case);
        let (exit, value) = export(
            root,
            &workspace,
            &case,
            &["--audience", "internal", "--out", out.to_str().unwrap()],
        );
        assert_eq!(exit, 0, "{value}");
        let manifest = manifest(&out);
        let errors: Vec<String> = validator
            .iter_errors(&manifest)
            .map(|error| error.to_string())
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(manifest["schema_version"], "inv.export-manifest.v1");
        assert_eq!(manifest["resumable"], false);
        assert!(
            manifest["statements"]["identifiers"]
                .as_str()
                .unwrap()
                .contains("UUIDv7")
        );
    }
}

/// The first Conclusion or Decision of `case` that rests on another record,
/// and the record of its dependency closure to raise: an Observation when
/// there is one.
fn statement_and_basis(workspace: &Path, case: Id) -> Option<(Id, Id)> {
    let mut store = Store::open(Workspace::open(workspace).unwrap());
    let replay = store.case(case).unwrap();
    let state = &replay.state;
    state
        .records()
        .filter(|record| matches!(record.header.id.kind(), Kind::Conclusion | Kind::Decision))
        .find_map(|record| {
            let reads = review_ops::required_reads(state, record).ok()?;
            let members: Vec<Id> = reads
                .into_iter()
                .map(|reference| reference.id)
                .filter(|id| *id != record.header.id && id.kind() != Kind::Case)
                .collect();
            let basis = members
                .iter()
                .find(|id| id.kind() == Kind::Observation)
                .or_else(|| members.first())?;
            Some((record.header.id, *basis))
        })
}

fn raise_to_restricted(workspace: &Path, case: Id, id: Id) {
    let mut store = Store::open(Workspace::open(workspace).unwrap());
    let revision = store
        .case(case)
        .unwrap()
        .state
        .current(id)
        .unwrap()
        .header
        .revision
        .0;
    let input = json!({
        "schema_version":"inv.command.v1", "command_id":format!("raise-{id}"),
        "case_id":case, "actor":"agent:exporter", "preconditions":[],
        "operations":[{"op":"classification.raise", "payload":{
            "subject_ref":{"id":id, "revision":revision},
            "level":"restricted", "reason":"Restricted for the audience test."}}]
    });
    let receipt = store.record(&serde_json::to_vec(&input).unwrap()).unwrap();
    assert_eq!(receipt.result, CommitResult::Committed);
}

fn listed_ids(receipt: &Value, list: &str) -> Vec<String> {
    receipt[list]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

/// No file of the export names a withheld record, and every caveated record
/// is listed with exactly the caveat lines the receipt reports for it.
fn assert_receipt_matches_export(label: &str, out: &Path, case: &str, value: &Value) {
    let receipt = &value["publisher_receipt"];
    assert_eq!(receipt["policy"], "inv.export-policy.v2", "{label}");
    let excluded = listed_ids(receipt, "exclusions");
    let count = receipt["exclusion_count"].as_u64().unwrap() as usize;
    assert!(count >= excluded.len(), "{label}");
    for (path, bytes) in files(out) {
        let text = String::from_utf8(bytes).unwrap();
        for id in excluded.iter().filter(|id| *id != case) {
            assert!(
                !text.contains(id.as_str()),
                "{label}: {path} names withheld {id}"
            );
        }
    }
    let report = String::from_utf8(fs::read(out.join("case-report.md")).unwrap()).unwrap();
    let mut caveat_lines = 0;
    for caveated in receipt["caveated"].as_array().unwrap() {
        let id = caveated["id"].as_str().unwrap();
        let mut lines = report.lines().skip_while(|line| !line.contains(id));
        if lines.next().is_none() {
            continue; // A kind of record this report does not list.
        }
        let shown = lines
            .take_while(|line| line.trim_start().starts_with("- Caveat: "))
            .count();
        assert_eq!(
            shown,
            caveated["caveats"].as_array().unwrap().len(),
            "{label}: {id}"
        );
        caveat_lines += shown;
    }
    assert_eq!(
        report.matches("- Caveat: ").count(),
        caveat_lines,
        "{label}: only caveated records carry a caveat"
    );
    let complete = value["completeness"] == "complete";
    assert_eq!(complete, count == 0, "{label}");
    assert_eq!(
        manifest(out)["completeness"],
        value["completeness"],
        "{label}"
    );
}

/// The policy per audience over every case of Scenarios A, C and D, after
/// raising one record a Conclusion or Decision rests on to `restricted`:
/// `restricted` gets everything with no caveat, `internal` gets the
/// statement with its basis caveat, and `public` gets no caveat at all.
#[test]
fn each_audience_gets_its_policy_over_scenarios_a_c_and_d() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut raised_any = 0;
    for scenario in SCENARIOS {
        let workspace = fixtures::workspace(scenario, &root.join(scenario), false);
        for case in cases(&workspace) {
            let case_id: Id = case.parse().unwrap();
            let raised = statement_and_basis(&workspace, case_id);
            if let Some((_, basis)) = raised {
                raise_to_restricted(&workspace, case_id, basis);
                raised_any += 1;
            }
            for audience in ["restricted", "internal", "public"] {
                let label = format!("{scenario} {case} {audience}");
                let out = root.join(format!("{scenario}-{case}-{audience}"));
                let (exit, value) = export(
                    root,
                    &workspace,
                    &case,
                    &["--audience", audience, "--out", out.to_str().unwrap()],
                );
                assert_eq!(exit, 0, "{label}: {value}");
                assert_receipt_matches_export(&label, &out, &case, &value);
                let receipt = &value["publisher_receipt"];
                let excluded = listed_ids(receipt, "exclusions");
                let caveated = listed_ids(receipt, "caveated");
                match audience {
                    "restricted" => {
                        assert!(excluded.is_empty(), "{label}: {receipt}");
                        assert!(caveated.is_empty(), "{label}: {receipt}");
                    }
                    "internal" => match raised {
                        Some((statement, basis)) => {
                            let reason = receipt["exclusions"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .find(|item| item["id"] == basis.to_string().as_str())
                                .map(|item| item["reason"].clone());
                            assert_eq!(reason, Some(json!("audience")), "{label}");
                            let entry = receipt["caveated"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .find(|item| item["id"] == statement.to_string().as_str())
                                .cloned();
                            assert!(
                                entry.is_some_and(|entry| entry["caveats"]
                                    .as_array()
                                    .unwrap()
                                    .contains(&json!("basis_withheld"))),
                                "{label}: {receipt}"
                            );
                            let report = fs::read_to_string(out.join("case-report.md")).unwrap();
                            let shown: Vec<&str> = report
                                .lines()
                                .skip_while(|line| !line.contains(&statement.to_string()))
                                .collect();
                            assert!(
                                shown.get(1..).is_some_and(|rest| rest
                                    .iter()
                                    .take_while(|line| line.starts_with("  - Caveat: "))
                                    .any(|line| line.contains("rests on"))),
                                "{label}: {statement} is listed with its caveat"
                            );
                            // Nothing the internal audience may see was
                            // withheld for any other reason.
                            assert!(
                                receipt["exclusions"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .all(|item| item["reason"] == "audience"),
                                "{label}: {receipt}"
                            );
                        }
                        None => {
                            assert!(excluded.is_empty(), "{label}: {receipt}");
                            assert!(caveated.is_empty(), "{label}: {receipt}");
                        }
                    },
                    _ => {
                        // Scenario records are internal or restricted, so the
                        // public export withholds them all and is told of no
                        // omission per record.
                        assert!(caveated.is_empty(), "{label}: {receipt}");
                        assert!(!excluded.is_empty(), "{label}");
                        let report = fs::read_to_string(out.join("case-report.md")).unwrap();
                        assert!(!report.contains("Caveat"), "{label}");
                    }
                }
            }
        }
    }
    assert!(
        raised_any >= 3,
        "every scenario has a statement with a basis"
    );
}

/// Every bounded hyphenated UUIDv7 value in `bytes`, lowercased.
fn uuids(bytes: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    for start in 0..bytes.len().saturating_sub(35) {
        let candidate = &bytes[start..start + 36];
        let shaped = candidate
            .iter()
            .enumerate()
            .all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => *byte == b'-',
                14 => *byte == b'7',
                _ => byte.is_ascii_hexdigit(),
            });
        let bounded = (start == 0 || !bytes[start - 1].is_ascii_alphanumeric())
            && bytes
                .get(start + 36)
                .is_none_or(|byte| !byte.is_ascii_alphanumeric());
        if shaped && bounded {
            found.push(String::from_utf8_lossy(candidate).to_ascii_lowercase());
        }
    }
    found
}

/// Clear every record of `case` for the public audience by a reviewed
/// downgrade, so a public export has records to show. Returns how many
/// were cleared.
fn clear_for_public(workspace: &Path, case: Id) -> usize {
    let mut store = Store::open(Workspace::open(workspace).unwrap());
    let mut pending: Vec<Id> = store
        .case(case)
        .unwrap()
        .state
        .records()
        .map(|record| record.header.id)
        .collect();
    let mut cleared = 0;
    // A record is cleared once everything it derives from is, so repeat
    // until a pass clears nothing more.
    loop {
        let before = cleared;
        let mut left = Vec::new();
        for id in pending {
            let state = store.case(case).unwrap().state;
            let record = state.current(id).unwrap();
            let subject = json!({"id":id, "revision":record.header.revision.0});
            let digest = investigation_core::derivation::artifact_body(&state, record.reference())
                .and_then(|body| body.digest);
            let input = json!({
                "schema_version":"inv.command.v1", "command_id":format!("clear-{id}-{before}"),
                "case_id":case, "actor":"human:export-reviewer", "preconditions":[],
                "operations":[{"op":"classification.downgrade", "payload":{
                    "subject_ref":subject, "level":"public", "reason":"Reviewed for publication.",
                    "transformation":{
                        "inputs":[{"input_ref":subject, "digest":digest}],
                        "transformation":"manual_review", "version":"1",
                        "redaction_decisions":[{"scope":"Whole record", "action":"retained",
                            "rationale":"Nothing in it is sensitive."}],
                        "output_digest":digest
                    },
                    "review":{"method":"Read the whole record.", "coverage":"whole_output",
                        "examined":[subject]}
                }}]
            });
            match store.record(&serde_json::to_vec(&input).unwrap()) {
                Ok(receipt) if receipt.result == CommitResult::Committed => cleared += 1,
                _ => left.push(id),
            }
        }
        pending = left;
        if cleared == before || pending.is_empty() {
            return cleared;
        }
    }
}

/// Every needle that betrays canonical identity in `workspace`: each UUIDv7
/// value anywhere in its state (records, transactions, workspace, journal),
/// in full, upper-case and 32-digit form with its millisecond time prefix,
/// and each head hash of the case's verified chain.
fn canonical_needles(workspace: &Path, case: Id) -> (Vec<String>, Vec<String>) {
    let mut canonical: Vec<String> = files(&workspace.join("state"))
        .values()
        .flat_map(|bytes| uuids(bytes))
        .collect();
    canonical.sort();
    canonical.dedup();
    let mut needles = Vec::new();
    for uuid in &canonical {
        needles.push(uuid[..13].to_owned());
        needles.push(uuid.replace('-', ""));
        needles.push(uuid.to_ascii_uppercase());
        needles.push(uuid.clone());
    }
    let mut store = Store::open(Workspace::open(workspace).unwrap());
    let replay = store.case(case).unwrap();
    for sequence in 0..=replay.snapshot.sequence {
        let head = replay.state.verified_snapshot(sequence).unwrap();
        needles.push(head.as_str().trim_start_matches("sha256:").to_owned());
    }
    (canonical, needles)
}

/// The receipt's ID mapping, exported value to canonical value.
fn mapping(value: &Value) -> BTreeMap<String, String> {
    value["publisher_receipt"]["identifiers"]["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| {
            (
                pair["exported"].as_str().unwrap().to_owned(),
                pair["canonical"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// W11.13 over every case of Scenarios A, C and D, with every record
/// cleared for the public audience so that the public export shows them:
///
/// - No byte or file name of a public export holds a canonical ID (full,
///   bare, upper-case or 32-digit), the millisecond time prefix of any
///   UUIDv7 value in the workspace, a head hash of the case's chain or the
///   journal sequence; the manifest names the bundle with sequence 0.
/// - Every ID it shows is in the receipt's mapping, which maps it to a value
///   of the workspace; the same export again gives the same bytes and the
///   same mapping.
/// - An internal export with `--remap-ids` maps back, through the receipt
///   alone, to the canonical internal export except for the lines that name
///   the snapshot, and the canonical manifest carries the UUIDv7 disclosure.
#[test]
fn public_and_remapped_exports_disclose_no_canonical_identity_over_scenarios_a_c_and_d() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut shown_by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    for scenario in SCENARIOS {
        let workspace = fixtures::workspace(scenario, &root.join(scenario), false);
        for case in cases(&workspace) {
            let case_id: Id = case.parse().unwrap();
            assert!(
                clear_for_public(&workspace, case_id) > 0,
                "{scenario} {case}"
            );
            let (canonical, needles) = canonical_needles(&workspace, case_id);
            for kind in ["case-report", "decision-record"] {
                let label = format!("{scenario} {case} {kind}");

                let mut runs = Vec::new();
                for run in ["first", "second"] {
                    let out = root.join(format!("{scenario}-{case}-{kind}-public-{run}"));
                    let (exit, value) = export_kind(
                        root,
                        &workspace,
                        &case,
                        kind,
                        &["--audience", "public", "--out", out.to_str().unwrap()],
                    );
                    assert_eq!(exit, 0, "{label}: {value}");
                    runs.push((out, value));
                }
                let (out, value) = &runs[0];
                assert_eq!(files(out), files(&runs[1].0), "{label}: bytes differ");
                assert_eq!(mapping(value), mapping(&runs[1].1), "{label}");
                assert_eq!(value["identifiers"], "remapped");
                let sequence = value["snapshot"]["sequence"].as_u64().unwrap();
                assert!(sequence > 0);
                let public_manifest = manifest(out);
                assert_eq!(public_manifest["identifiers"], "remapped", "{label}");
                assert_eq!(public_manifest["snapshot"]["sequence"], 0, "{label}");
                assert_eq!(
                    public_manifest["snapshot"],
                    value["publisher_receipt"]["identifiers"]["exported_snapshot"]
                );
                let pairs = mapping(value);
                let mut shown = 0;
                for (path, bytes) in files(out) {
                    let text = String::from_utf8(bytes.clone()).unwrap();
                    for needle in &needles {
                        assert!(
                            !path.contains(needle.as_str()),
                            "{label}: {path} names {needle}"
                        );
                        assert!(
                            !text.contains(needle.as_str()),
                            "{label}: {path} has {needle}"
                        );
                    }
                    for form in [
                        format!("sequence {sequence}"),
                        format!("\"sequence\": {sequence}"),
                        format!("--snapshot {sequence}"),
                    ] {
                        assert!(!text.contains(&form), "{label}: {path} has {form}");
                    }
                    for uuid in uuids(&bytes) {
                        let canonical_value = pairs
                            .iter()
                            .find(|(exported, _)| exported.ends_with(&uuid))
                            .map(|(_, canonical)| canonical.clone())
                            .unwrap_or_else(|| panic!("{label}: {path}: {uuid} is not mapped"));
                        assert!(
                            canonical
                                .iter()
                                .any(|value| canonical_value.ends_with(value)),
                            "{label}: {uuid} maps to {canonical_value}"
                        );
                        shown += 1;
                    }
                }
                // The export names the remapped case.
                let report = fs::read_to_string(out.join(format!("{kind}.md"))).unwrap();
                let case_shown = public_manifest["snapshot"]["case_id"].as_str().unwrap();
                assert!(report.contains(case_shown), "{label}");
                assert_eq!(pairs.get(case_shown), Some(&case), "{label}");
                *shown_by_kind.entry(kind).or_default() += shown;

                // Internal: canonical by default and disclosed; remapped on
                // request and restored by the receipt's mapping alone.
                let plain = root.join(format!("{scenario}-{case}-{kind}-internal"));
                let (exit, plain_value) = export_kind(
                    root,
                    &workspace,
                    &case,
                    kind,
                    &["--audience", "internal", "--out", plain.to_str().unwrap()],
                );
                assert_eq!(exit, 0, "{label}: {plain_value}");
                assert_eq!(plain_value["identifiers"], "canonical");
                let plain_manifest = manifest(&plain);
                assert_eq!(plain_manifest["identifiers"], "canonical");
                assert_eq!(plain_manifest["snapshot"]["case_id"], case.as_str());
                assert_eq!(plain_manifest["snapshot"]["sequence"], sequence);
                assert!(
                    plain_manifest["statements"]["identifiers"]
                        .as_str()
                        .unwrap()
                        .contains("embed their creation time")
                );
                let remapped = root.join(format!("{scenario}-{case}-{kind}-internal-remapped"));
                let (exit, remapped_value) = export_kind(
                    root,
                    &workspace,
                    &case,
                    kind,
                    &[
                        "--audience",
                        "internal",
                        "--remap-ids",
                        "--out",
                        remapped.to_str().unwrap(),
                    ],
                );
                assert_eq!(exit, 0, "{label}: {remapped_value}");
                assert_eq!(manifest(&remapped)["identifiers"], "remapped");
                let mut restored = fs::read_to_string(remapped.join(format!("{kind}.md"))).unwrap();
                for (exported, canonical_value) in mapping(&remapped_value) {
                    restored = restored.replace(&exported, &canonical_value);
                }
                let names_snapshot = |line: &&str| {
                    line.starts_with("| Snapshot |")
                        || line.starts_with("| Bundle |")
                        || line.contains("full evidence chain")
                };
                let expected = fs::read_to_string(plain.join(format!("{kind}.md"))).unwrap();
                assert_eq!(
                    restored
                        .lines()
                        .filter(|line| !names_snapshot(line))
                        .collect::<Vec<_>>(),
                    expected
                        .lines()
                        .filter(|line| !names_snapshot(line))
                        .collect::<Vec<_>>(),
                    "{label}"
                );
                assert_ne!(
                    mapping(&remapped_value).keys().collect::<Vec<_>>(),
                    pairs.keys().collect::<Vec<_>>(),
                    "{label}: audiences share remapped IDs"
                );
            }
        }
    }
    // Both kinds show remapped records, so the scan is not vacuous.
    assert!(shown_by_kind["case-report"] > 50, "{shown_by_kind:?}");
    assert!(shown_by_kind["decision-record"] > 10, "{shown_by_kind:?}");
}

/// A workspace archive keeps canonical identity: its manifest says
/// `identifiers: canonical`, so its sequence 0 (the workspace has no
/// sequence; the head is the recovery head) is told apart from a remapped
/// export's undisclosed one by `kind` and `identifiers`. `--remap-ids` is
/// refused before anything is written.
#[test]
fn a_workspace_archive_stays_canonical_and_refuses_remapping() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = fixtures::workspace("scenario-a", &root.join("ws"), false);
    let base = [
        "--workspace",
        workspace.to_str().unwrap(),
        "export",
        "--kind",
        "workspace-archive",
        "--audience",
        "restricted",
    ];
    let refused = root.join("refused");
    let mut args = base.to_vec();
    args.extend(["--remap-ids", "--out", refused.to_str().unwrap()]);
    let (exit, value) = run(root, &args);
    assert_eq!(exit, 2, "{value}");
    assert_eq!(value["error"]["path"], "--remap-ids");
    assert!(!refused.exists());

    let archive = root.join("archive");
    let mut args = base.to_vec();
    args.extend(["--out", archive.to_str().unwrap()]);
    let (exit, value) = run(root, &args);
    assert_eq!(exit, 0, "{value}");
    assert_eq!(value["identifiers"], "canonical");
    let archive_manifest = manifest(&archive);
    assert_eq!(archive_manifest["kind"], "workspace-archive");
    assert_eq!(archive_manifest["identifiers"], "canonical");
    assert_eq!(archive_manifest["snapshot"]["sequence"], 0);
    assert!(
        archive_manifest["statements"]["identifiers"]
            .as_str()
            .unwrap()
            .contains("embed their creation time")
    );
}
