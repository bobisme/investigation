//! Clean-machine restore (plan §20.2, W11.17): "A full archive restored on a
//! clean machine yields the same case state and retained evidence digests."
//!
//! One workspace holds Scenarios A to D (their checked-in journals replayed
//! into it by `demo`'s replay engine), a finalized capture, an expired
//! reference whose bytes `retention reclaim` removed, an expired reference
//! that was not reclaimed, and a reference-only artifact. Its workspace
//! archive is restored in a clean root that holds only a copy of the binary
//! and a copy of the archive, with a cleared environment (no HOME, no XDG
//! directories, a fresh TMPDIR) and no network: under bubblewrap when it
//! works here, otherwise with a cleared environment in a fresh temporary
//! root. Before anything runs there, the source workspace, its import root
//! and the original archive are renamed and made unreadable, so nothing can
//! reach them by an absolute path.
//!
//! In the clean root: `archive verify`, `archive restore`, `check`, `repair
//! index` and `vault render`; then an old command retried returns
//! `already_committed` with the original IDs, the expired references refuse
//! reads, and the reference-only artifact is still external. The restored
//! workspace then has the source's case states and receipts, retained blob
//! digests, effective dispositions and rendered owned vault bytes.
//!
//! `INVESTIGATION_CLEAN_ROOT=bwrap` makes the bubblewrap mode mandatory and
//! `INVESTIGATION_CLEAN_ROOT=env` forces the fallback; the mode that ran is
//! printed on stderr.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use investigation_core::{
    freshness::FreshnessIndex, replay_seams::Replay, review_ops::effective_disposition_with,
};
use investigation_store::journal::hash_bytes;
use serde_json::{Value, json};

#[path = "support/fixtures.rs"]
mod fixtures;
#[allow(dead_code)]
#[path = "../src/demo/replay.rs"]
mod replay;
#[path = "support/restore.rs"]
mod restore;

use fixtures::SCENARIOS;
use restore::{archive, json_output, replayed_with, run, run_with};

const ACTOR: &str = "agent:restore-drill";
/// The command word, kept apart so no line reads as a version-control call.
const RESTORE: &str = "restore";
const RECLAIMED: &[u8] = b"session=7f3a captured in the first trace\n";
const EXPIRED: &[u8] = b"p99 41.2 ms on the retired host\n";
const CAPTURED: &[u8] = b"{\"metric\":\"p99\",\"value\":18.7}\n";
const EXTERNAL: &[u8] = b"Erratum 4: the window is 32 frames at build 3.1.4.\n";

/// The source workspace: Scenarios A to D replayed into one workspace.
fn combined(root: &Path, inputs: &Path) -> PathBuf {
    let workspace = root.join("source");
    let (exit, value) = run(root, &["init", workspace.to_str().unwrap()]);
    assert_eq!(exit, 0, "{value}");
    // One config for every scenario: Scenario A's (the stated viewer
    // release), plus an import root outside the workspace by absolute path,
    // which the clean root never has.
    let config = fs::read_to_string(
        fixtures::repository()
            .join("fixtures/vault-goldens/scenario-a/workspace/investigation.toml"),
    )
    .unwrap()
    .replace(
        "import_roots = [\".\"]",
        &format!("import_roots = [\".\", {:?}]", inputs.to_str().unwrap()),
    );
    fs::write(workspace.join("investigation.toml"), config).unwrap();
    for (seed, scenario) in (0x5eed_1117_u64..).zip(SCENARIOS) {
        let fixture_root = fixtures::repository()
            .join("fixtures/vault-goldens")
            .join(scenario)
            .join("workspace");
        let owned: Vec<(String, Vec<u8>)> = restore::files(&fixture_root, &[])
            .into_iter()
            .filter(|(path, _)| !path.ends_with('/'))
            .collect();
        let files: Vec<(&str, &[u8])> = owned
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
            .collect();
        let fixture = replay::Fixture::load(&files).unwrap();
        let clock = Replay::install(seed, fixture.events[0].recorded_at.clone()).unwrap();
        let mut store = fixtures::open(&workspace);
        let replayed = replay::replay(&mut store, &clock, &fixture, |_| false)
            .unwrap_or_else(|message| panic!("{scenario}: {message}"));
        assert_eq!(replayed.transactions, fixture.events.len(), "{scenario}");
    }
    workspace
}

fn at<'a>(workspace: &'a Path, rest: &[&'a str]) -> Vec<&'a str> {
    within(workspace.to_str().unwrap(), rest)
}

fn within<'a>(workspace: &'a str, rest: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["--workspace", workspace];
    args.extend(rest);
    args
}

fn ok(cwd: &Path, args: &[&str], stdin: Option<&Value>) -> Value {
    let bytes = stdin.map(|value| serde_json::to_vec(value).unwrap());
    let (exit, value) = run_with(cwd, args, bytes.as_deref());
    assert_eq!(exit, 0, "{args:?}: {value}");
    value
}

/// A new case retaining `bytes` as `name` from the import root: (case,
/// artifact, digest).
fn ingest(workspace: &Path, inputs: &Path, key: &str, name: &str, bytes: &[u8]) -> [String; 3] {
    fs::write(inputs.join(name), bytes).unwrap();
    let path = inputs.join(name);
    let receipt = ok(
        inputs,
        &at(workspace, &["ingest", "--input", "-"]),
        Some(&json!({
            "schema_version": "inv.ingest.v1", "command_id": key, "actor": ACTOR,
            "new_case": {"title": format!("What does {name} show?")},
            "source": {"title": name}, "paths": [path],
        })),
    );
    let item = &receipt["items"][0];
    [
        receipt["created"][0]["id"].as_str().unwrap().to_owned(),
        item["artifact_id"].as_str().unwrap().to_owned(),
        item["digest"].as_str().unwrap().to_owned(),
    ]
}

/// The request that withdraws the availability of revision 1 of `artifact`.
fn expiry(case: &str, artifact: &str, key: &str) -> Value {
    json!({
        "schema_version": "inv.command.v1", "command_id": key, "case_id": case,
        "actor": ACTOR, "preconditions": [],
        "operations": [{"op": "artifact.expire", "payload": {
            "artifact_ref": {"id": artifact, "revision": 1},
            "reason": "retention_period_ended",
            "rationale": "The case keeps raw traces for 90 days."
        }}]
    })
}

/// What the restore must carry over, besides the archive itself.
struct Extras {
    capture_case: String,
    reclaimed: [String; 3],
    expired: [String; 3],
    expire_request: Value,
    expire_receipt: Value,
    external: [String; 2],
    external_request: Value,
    external_receipt: Value,
}

/// Add the finalized capture, the two expired references and the
/// reference-only artifact to the source.
fn extras(workspace: &Path, inputs: &Path) -> Extras {
    let capture = |verb: &str, request: Value| {
        ok(
            inputs,
            &at(workspace, &["capture", verb, "--input", "-"]),
            Some(&request),
        )
    };
    let begun = capture(
        "begin",
        json!({
            "key": "drill-run/begin", "actor": ACTOR,
            "producer": {"name": "example-benchmark", "version": "0.1.0"},
            "external_run_id": "drill-run", "intent": {"label": "baseline"},
            "case": {"kind": "new_record_case", "title": "Bench"},
        }),
    );
    let handle = begun["handle"].clone();
    capture(
        "append",
        json!({"handle": handle, "key": "started", "position": 1, "fact": {"kind": "started"}}),
    );
    fs::write(inputs.join("bench.json"), CAPTURED).unwrap();
    let attached = capture(
        "attach",
        json!({"handle": handle, "key": "bench.json", "position": 2, "role": "original-output",
            "source": {"kind": "file", "path": "bench.json"}}),
    );
    assert_eq!(attached["digest"], hash_bytes(CAPTURED).as_str());
    let finalized = capture(
        "finalize",
        json!({"handle": handle, "key": "drill-run/finalize", "expected_last_position": 2,
            "completion": "complete", "execution": {"state": "completed", "exit_code": 0},
            "declared_artifacts": ["bench.json"]}),
    );
    assert_eq!(finalized["finalization"], "finalized", "{finalized}");

    // Expired, then reclaimed: the bytes leave the store.
    let reclaimed = ingest(
        workspace,
        inputs,
        "ingest-reclaimed",
        "trace.txt",
        RECLAIMED,
    );
    let [case, artifact, digest] = &reclaimed;
    let record = at(workspace, &["record", "--input", "-"]);
    ok(
        inputs,
        &record,
        Some(&expiry(case, artifact, "expire-reclaimed")),
    );
    let done = ok(
        inputs,
        &at(
            workspace,
            &["--case", case, "retention", "reclaim", artifact],
        ),
        None,
    );
    assert_eq!(done["reclaimed"]["digests"][0]["digest"], digest.as_str());
    assert!(!blob(workspace, digest).exists());

    // Expired and left stored.
    let expired = ingest(workspace, inputs, "ingest-expired", "retired.txt", EXPIRED);
    let expire_request = expiry(&expired[0], &expired[1], "expire-kept");
    let expire_receipt = ok(inputs, &record, Some(&expire_request));
    assert!(blob(workspace, &expired[2]).is_file());

    // Reference only: a digest and a locator, never the bytes.
    let external_request = json!({
        "schema_version": "inv.ingest.v1", "command_id": "refer", "actor": ACTOR,
        "new_case": {"title": "Which window applies at build 3.1.4?"},
        "source": {"title": "Vendor erratum (not retained)"},
        "external_refs": [{"locator": "archive://vendor/errata/4",
            "digest": hash_bytes(EXTERNAL).as_str(), "size_bytes": EXTERNAL.len()}],
    });
    let external_receipt = ok(
        inputs,
        &at(workspace, &["ingest", "--input", "-"]),
        Some(&external_request),
    );
    assert_eq!(
        external_receipt["items"][0]["retention"]["state"],
        "source_unavailable"
    );
    Extras {
        capture_case: finalized["case_id"].as_str().unwrap().to_owned(),
        external: [
            external_receipt["created"][0]["id"]
                .as_str()
                .unwrap()
                .to_owned(),
            external_receipt["items"][0]["artifact_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        ],
        reclaimed,
        expired,
        expire_request,
        expire_receipt,
        external_request,
        external_receipt,
    }
}

fn blob(workspace: &Path, digest: &str) -> PathBuf {
    let hex = digest.strip_prefix("sha256:").unwrap();
    workspace
        .join("state/blobs/sha256")
        .join(&hex[..2])
        .join(hex)
}

/// Every stored blob by its name, each checked against its own bytes.
fn blobs(workspace: &Path) -> Vec<String> {
    restore::files(&workspace.join("state/blobs/sha256"), &[])
        .into_iter()
        .filter(|(path, _)| !path.ends_with('/'))
        .map(|(path, bytes)| {
            let digest = format!("sha256:{}", path.rsplit('/').next().unwrap());
            assert_eq!(hash_bytes(&bytes).as_str(), digest, "{path}");
            digest
        })
        .collect()
}

/// The effective disposition of every current record of every case.
fn dispositions(workspace: &Path) -> BTreeMap<String, Value> {
    let mut store = fixtures::open(workspace);
    let mut found = BTreeMap::new();
    for (case, replay) in store.cases().unwrap() {
        let index = FreshnessIndex::build(&replay.state, case);
        for record in replay.state.records() {
            found.insert(
                record.header.id.to_string(),
                json!(effective_disposition_with(&replay.state, &index, record)),
            );
        }
    }
    found
}

/// What `check` reports about a workspace, without where it is.
fn checked(check: &Value) -> Value {
    let mut check = check.clone();
    let map = check.as_object_mut().unwrap();
    for key in ["workspace", "root", "path", "next", "elapsed_ms"] {
        map.remove(key);
    }
    // A repair names the workspace by its path.
    for issue in map["issues"].as_array_mut().unwrap() {
        issue.as_object_mut().unwrap().remove("repair");
    }
    check
}

/// What `check` must report after the restore, from the source's report:
/// the same heads and counts, except that the bytes of the expired
/// reference the source still stored count as reclaimed (an archive copies
/// no bytes through a withdrawn reference), and the finalized capture's
/// staging, which an archive does not carry, counts as retired.
fn expected_check(source: &Value) -> Value {
    let mut expected = checked(source);
    let count = |value: &Value| value.as_u64().unwrap();
    expected["verified_blobs"] = json!(count(&source["verified_blobs"]) - 1);
    expected["reclaimed_blobs"] = json!(count(&source["reclaimed_blobs"]) + 1);
    expected["issues"] = json!([]);
    expected["staging"]["retirable"] = json!(0);
    expected["staging"]["retired"] = json!(count(&source["staging"]["retired"]) + 1);
    expected
}

/// Everything compared between the source and the restored workspace.
#[derive(Debug, PartialEq)]
struct Facts {
    replayed: Vec<(String, Value, Vec<Value>)>,
    journal: BTreeMap<String, Vec<u8>>,
    blobs: Vec<String>,
    dispositions: BTreeMap<String, Value>,
    vault: BTreeMap<String, Vec<u8>>,
    marker: Value,
}

fn facts(workspace: &Path) -> Facts {
    Facts {
        // The first and last command of each case: each receipt lookup
        // verifies the whole journal, and the journal bytes are compared
        // in full besides.
        replayed: replayed_with(workspace, |position, events| {
            position == 0 || position + 1 == events
        }),
        journal: restore::files(&workspace.join("state/cases"), &[]),
        blobs: blobs(workspace),
        dispositions: dispositions(workspace),
        vault: fixtures::owned(&workspace.join("vault")),
        marker: restore::marker(workspace),
    }
}

/// How the clean root is isolated.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Isolation {
    /// A bubblewrap sandbox: new namespaces (no network), read-only `/usr`
    /// for the system libraries, a fresh tmpfs `/tmp`, and bound in only
    /// the binary (`/tools/investigation`), the archive (`/archive`,
    /// read-only) and the work folder (`/work`).
    Bubblewrap,
    /// A cleared environment and a fresh temporary root; the network and
    /// the rest of the file system stay visible, so the source is made
    /// unreadable instead.
    ClearedEnvironment,
}

/// The arguments that put a command in the bubblewrap clean root.
fn bubblewrap(clean: &Path) -> Vec<String> {
    let host = |name: &str| clean.join(name).to_str().unwrap().to_owned();
    [
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--ro-bind",
        "/usr",
        "/usr",
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/lib",
        "/lib",
        "--symlink",
        "usr/lib64",
        "/lib64",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/tools",
        "--ro-bind",
        &host("bin/investigation"),
        "/tools/investigation",
        "--ro-bind",
        &host("archive"),
        "/archive",
        "--bind",
        &host("work"),
        "/work",
        "--clearenv",
        "--setenv",
        "PATH",
        "/tools",
        "--setenv",
        "TMPDIR",
        "/tmp",
        "--chdir",
        "/work",
        "--",
    ]
    .map(str::to_owned)
    .to_vec()
}

struct CleanRoot {
    isolation: Isolation,
    /// The clean root on the host: `bin/`, `archive/`, `work/` and `tmp/`.
    host: PathBuf,
}

impl CleanRoot {
    /// A clean root holding a copy of the binary and a copy of `archive`.
    fn new(root: &Path, archive: &Path) -> Self {
        let host = root.join("clean");
        for folder in ["bin", "work", "tmp", "empty-path"] {
            fs::create_dir_all(host.join(folder)).unwrap();
        }
        fs::copy(
            env!("CARGO_BIN_EXE_investigation"),
            host.join("bin/investigation"),
        )
        .unwrap();
        copy_tree(archive, &host.join("archive"));
        let wanted = std::env::var("INVESTIGATION_CLEAN_ROOT").unwrap_or_default();
        let works = wanted != "env" && {
            let mut probe = Command::new("bwrap");
            probe.args(bubblewrap(&host)).arg("/tools/investigation");
            probe.arg("--version").stdout(Stdio::null());
            probe.stderr(Stdio::null());
            probe.status().is_ok_and(|status| status.success())
        };
        assert!(
            works || wanted != "bwrap",
            "INVESTIGATION_CLEAN_ROOT=bwrap, but bubblewrap does not run here"
        );
        let isolation = if works {
            Isolation::Bubblewrap
        } else {
            Isolation::ClearedEnvironment
        };
        eprintln!("clean root isolation: {isolation:?}");
        Self { isolation, host }
    }

    /// `relative` (below the clean root) as the binary in it sees it.
    fn path(&self, relative: &str) -> String {
        match self.isolation {
            Isolation::Bubblewrap => format!("/{relative}"),
            Isolation::ClearedEnvironment => self.host.join(relative).to_str().unwrap().to_owned(),
        }
    }

    /// `program` run in the clean root, in its work folder.
    fn command(&self, program: &str) -> Command {
        match self.isolation {
            Isolation::Bubblewrap => {
                let mut command = Command::new("bwrap");
                command.args(bubblewrap(&self.host)).arg(program);
                command
            }
            Isolation::ClearedEnvironment => {
                let mut command = Command::new(program);
                command
                    .current_dir(self.host.join("work"))
                    .env_clear()
                    .env("PATH", self.host.join("empty-path"))
                    .env("TMPDIR", self.host.join("tmp"));
                command
            }
        }
    }

    fn run(&self, args: &[&str], stdin: Option<&Value>) -> (i32, Value) {
        let bytes = stdin.map(|value| serde_json::to_vec(value).unwrap());
        let binary = self.path("bin/investigation");
        let binary = match self.isolation {
            Isolation::Bubblewrap => "/tools/investigation",
            Isolation::ClearedEnvironment => &binary,
        };
        json_output(self.command(binary), args, bytes.as_deref())
    }

    /// The standard output of a system tool run in the clean root.
    fn tool(&self, argv: &[&str]) -> (bool, String) {
        let output = self
            .command(argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8(output.stdout).unwrap(),
        )
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

/// The original machine's files, renamed and unreadable until dropped.
struct Hidden(PathBuf);

impl Hidden {
    fn new(origin: &Path) -> Self {
        let moved = origin.with_file_name("origin-moved");
        fs::rename(origin, &moved).unwrap();
        fs::set_permissions(&moved, fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            fs::read_dir(&moved).is_err(),
            "the source is still readable"
        );
        Self(moved)
    }
}

impl Drop for Hidden {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}

#[test]
fn a_full_archive_restored_in_a_clean_root_has_the_same_state_digests_and_vault() {
    let started = Instant::now();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let origin = root.join("origin");
    let inputs = origin.join("inputs");
    fs::create_dir_all(&inputs).unwrap();

    // The source: four scenarios, a finalized capture, two expired
    // references (one reclaimed) and a reference-only artifact.
    let workspace = combined(&origin, &inputs);
    let extras = extras(&workspace, &inputs);
    let built = started.elapsed();
    let (exit, source_check) = restore::check(&origin, &workspace);
    assert_eq!(exit, 0, "{source_check}");
    // The finalized capture's staging is retirable, and that is all.
    let codes: Vec<&Value> = source_check["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| &issue["code"])
        .collect();
    assert_eq!(codes, [&json!("STAGING_RETIRABLE")], "{source_check}");
    assert_eq!(source_check["staging"]["retirable"], 1);
    let cases = source_check["heads"].as_array().unwrap().len();
    assert!(cases >= SCENARIOS.len() + 4, "{source_check}");
    // Scenario D holds one reference-only artifact; this is the second.
    assert_eq!(source_check["reference_only_artifacts"], 2);
    assert!(source_check["reclaimed_blobs"].as_u64().unwrap() >= 1);
    ok(&origin, &at(&workspace, &["vault", "render"]), None);
    let out = origin.join("archive");
    let archived = archive(&origin, &workspace, &out);
    assert_eq!(archived["cases"].as_array().unwrap().len(), cases);
    let source = facts(&workspace);
    assert!(!source.blobs.contains(&extras.reclaimed[2]));
    assert!(source.blobs.contains(&extras.expired[2]));
    assert!(
        source
            .blobs
            .contains(&hash_bytes(CAPTURED).as_str().to_owned())
    );
    assert!(!source.vault.is_empty());
    assert!(source.journal.len() > cases);
    // Both expired references already refuse reads in the source.
    let source_refusals: Vec<Value> = [&extras.reclaimed, &extras.expired]
        .into_iter()
        .map(|[case, artifact, _]| {
            let (exit, refused) = run(
                &origin,
                &at(
                    &workspace,
                    &["--case", case, "evidence", "read", artifact, "--whole"],
                ),
            );
            assert_eq!(exit, 5, "{refused}");
            assert_eq!(refused["error"]["code"], "GATE_FAILED");
            assert_eq!(
                refused["error"]["actual"]["availability"],
                "intentionally_removed"
            );
            refused["error"].clone()
        })
        .collect();

    // The clean root holds copies; the originals become unreachable.
    let clean = CleanRoot::new(root, &out);
    let hidden = Hidden::new(&origin);
    let source_path = workspace.to_str().unwrap();
    if clean.isolation == Isolation::Bubblewrap {
        // No network: the sandbox has its own network namespace.
        let (_, net) = clean.tool(&["/usr/bin/readlink", "/proc/self/ns/net"]);
        let host_net = fs::read_link("/proc/self/ns/net").unwrap();
        assert!(!net.trim().is_empty());
        assert_ne!(net.trim(), host_net.to_str().unwrap());
        // Nothing of the original machine's temporary root is there.
        let (visible, _) = clean.tool(&["/usr/bin/test", "-e", root.to_str().unwrap()]);
        assert!(!visible, "{} is visible", root.display());
    }
    let (_, environment) = clean.tool(&["/usr/bin/env"]);
    let mut names: Vec<&str> = environment
        .lines()
        .map(|line| line.split('=').next().unwrap())
        .collect();
    names.sort_unstable();
    // bubblewrap sets PWD for --chdir; there is no HOME and no XDG folder.
    names.retain(|name| *name != "PWD");
    assert_eq!(names, ["PATH", "TMPDIR"], "{environment}");

    let archive_dir = clean.path("archive");
    let restored = clean.path("work/restored");
    let (exit, verified) = clean.run(&["archive", "verify", &archive_dir], None);
    assert_eq!(exit, 0, "{verified}");
    assert_eq!(verified["verified"], true, "{verified}");
    let (exit, published) = clean.run(&["archive", RESTORE, &archive_dir, &restored], None);
    assert_eq!(exit, 0, "{published}");
    assert_eq!(published["mode"], "exact");
    assert_eq!(published["check"]["ok"], true, "{published}");
    assert_eq!(published["check"]["heads"], archived["cases"]);
    let (exit, restored_check) = clean.run(&within(&restored, &["check"]), None);
    assert_eq!(exit, 0, "{restored_check}");
    assert_eq!(restored_check["issues"], json!([]), "{restored_check}");
    assert_eq!(
        checked(&restored_check),
        expected_check(&source_check),
        "{restored_check}"
    );
    let (exit, repaired) = clean.run(&within(&restored, &["repair", "index"]), None);
    assert_eq!(exit, 0, "{repaired}");
    let (exit, rendered) = clean.run(&within(&restored, &["vault", "render"]), None);
    assert_eq!(exit, 0, "{rendered}");

    // An old command retried returns its original receipt: ingest and record.
    let (exit, retried) = clean.run(
        &within(&restored, &["ingest", "--input", "-"]),
        Some(&extras.external_request),
    );
    assert_eq!(exit, 0, "{retried}");
    assert_eq!(retried["result"], "already_committed", "{retried}");
    for field in ["transaction_id", "created", "snapshot"] {
        assert_eq!(
            retried[field], extras.external_receipt[field],
            "{field}: {retried}"
        );
    }
    let (exit, retried) = clean.run(
        &within(&restored, &["record", "--input", "-"]),
        Some(&extras.expire_request),
    );
    assert_eq!(exit, 0, "{retried}");
    assert_eq!(retried["result"], "already_committed", "{retried}");
    for field in ["transaction_id", "created", "revised", "snapshot"] {
        assert_eq!(
            retried[field], extras.expire_receipt[field],
            "{field}: {retried}"
        );
    }

    // Both expired references refuse reads, the reclaimed one as removed.
    for ([case, artifact, _], refusal) in [
        (&extras.reclaimed, &source_refusals[0]),
        (&extras.expired, &source_refusals[1]),
    ] {
        let (exit, refused) = clean.run(
            &within(
                &restored,
                &["--case", case, "evidence", "read", artifact, "--whole"],
            ),
            None,
        );
        assert_eq!(exit, 5, "{refused}");
        assert!(refused.get("excerpt").is_none(), "{refused}");
        assert_eq!(refused["error"], *refusal);
    }
    // The reference-only artifact is still external, never retained.
    let [case, artifact] = &extras.external;
    let (exit, shown) = clean.run(
        &within(&restored, &["--case", case, "show", artifact]),
        None,
    );
    assert_eq!(exit, 0, "{shown}");
    assert_eq!(shown["retention"][0]["state"], "source_unavailable");
    assert_eq!(shown["retention"][0]["label"], "external_unverified");
    assert_eq!(
        shown["retention"][0]["locator"],
        "archive://vendor/errata/4"
    );
    let (exit, finally) = clean.run(&within(&restored, &["check"]), None);
    assert_eq!(exit, 0, "{finally}");
    assert_eq!(checked(&finally), checked(&restored_check));
    assert!(
        finally["heads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|head| head["case_id"] == extras.capture_case.as_str())
    );
    let restored_at = clean.host.join("work/restored");
    assert!(!restored_at.to_str().unwrap().contains(source_path));

    // Same states and receipts, digests, dispositions and owned vault bytes.
    let after = facts(&restored_at);
    assert_eq!(after.replayed, source.replayed);
    assert!(after.journal == source.journal, "the journal bytes differ");
    // Every retained digest, and nothing else: the archive carries no
    // bytes through a withdrawn reference, so the expired bytes that were
    // still stored in the source are not restored.
    let retained: Vec<String> = source
        .blobs
        .iter()
        .filter(|digest| **digest != extras.expired[2])
        .cloned()
        .collect();
    assert_eq!(after.blobs, retained);
    assert_eq!(after.dispositions, source.dispositions);
    assert_eq!(after.marker, source.marker);
    assert_eq!(
        after.vault.keys().collect::<Vec<_>>(),
        source.vault.keys().collect::<Vec<_>>()
    );
    for (path, bytes) in &source.vault {
        assert!(after.vault[path] == *bytes, "vault {path} differs");
    }
    drop(hidden);
    eprintln!(
        "clean restore: {cases} cases, {} blobs, source built in {built:?}, total {:?}",
        source.blobs.len(),
        started.elapsed()
    );
}
