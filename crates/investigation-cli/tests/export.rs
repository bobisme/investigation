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
    let workspace = workspace.to_str().unwrap();
    let mut args = vec![
        "--workspace",
        workspace,
        "--case",
        case,
        "export",
        "--kind",
        "case-report",
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
                assert_eq!(manifest["snapshot"], first_value["snapshot"]);
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
        json!(["case-report", "decision-record"])
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
