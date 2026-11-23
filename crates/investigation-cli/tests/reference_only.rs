//! A reference-only source reports `source_unavailable` in `show`, in
//! `evidence read` and in `check`; nothing presents it as retained bytes.

use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const ACTOR: &str = "agent:reference-cli-test";

fn run(cwd: &Path, args: &[&str], exit: i32) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{stdout}\n{:?}",
        output.stderr
    );
    serde_json::from_str(&stdout).expect(&stdout)
}

fn write_json(root: &Path, name: &str, value: &Value) {
    fs::write(root.join(name), serde_json::to_vec(value).unwrap()).unwrap();
}

#[test]
fn a_reference_only_source_reports_source_unavailable_everywhere_it_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    run(root, &["init", ".", "--no-vault", "--json"], 0);
    let config = root.join("investigation.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("import_roots = []", "import_roots = [\".\"]"),
    )
    .unwrap();
    let bytes = b"Erratum 4: the window is 32 frames at build 3.1.4.\n";
    // SHA-256 of `bytes`.
    let digest = "sha256:8b03ac3a7a9f49164e7be1b5fb9e371fa1a997695b91971abf4aae9b0baf3e8f";
    write_json(
        root,
        "refer.json",
        &json!({
            "schema_version":"inv.ingest.v1","command_id":"refer","actor":ACTOR,
            "new_case":{"title":"Which window applies at build 3.1.4?"},
            "source":{"title":"Vendor erratum (not retained)"},
            "external_refs":[{"locator":"archive://vendor/errata/4","digest":digest,"size_bytes":bytes.len()}],
        }),
    );
    let receipt = run(root, &["ingest", "--input", "refer.json", "--json"], 0);
    let case = receipt["created"][0]["id"].as_str().unwrap().to_owned();
    let item = &receipt["items"][0];
    assert_eq!(item["retention"]["state"], "source_unavailable");
    let artifact = item["artifact_id"].as_str().unwrap().to_owned();
    let source = item["source_id"].as_str().unwrap().to_owned();

    for id in [&artifact, &source] {
        let shown = run(root, &["show", id, "--case", &case, "--json"], 0);
        let retention = &shown["retention"][0];
        assert_eq!(retention["state"], "source_unavailable", "{id}");
        assert_eq!(retention["label"], "external_unverified", "{id}");
        assert_eq!(retention["locator"], "archive://vendor/errata/4", "{id}");
        assert_eq!(retention["offline_complete"], false, "{id}");
    }
    let shown = run(root, &["show", &artifact, "--case", &case, "--json"], 0);
    assert_eq!(
        shown["material_caveat"]["source_unavailable"][0]["id"],
        artifact.as_str()
    );

    write_json(
        root,
        "selector.json",
        &json!({"schema_version":"inv.selector.v1","artifact_digest":digest,"selector":{"type":"whole"}}),
    );
    let read = run(
        root,
        &[
            "evidence",
            "read",
            &artifact,
            "--selector-file",
            "selector.json",
            "--case",
            &case,
            "--json",
        ],
        5,
    );
    assert_eq!(read["error"]["actual"]["state"], "source_unavailable");
    assert!(read.get("excerpt").is_none());

    let check = run(root, &["check", "--json"], 0);
    assert_eq!(check["reference_only_artifacts"], 1);

    // Supplying the bytes verifies and retains them through a new revision.
    fs::write(root.join("erratum.txt"), bytes).unwrap();
    write_json(
        root,
        "verify.json",
        &json!({
            "schema_version":"inv.ingest.v1","command_id":"verify","actor":ACTOR,"case_id":case,
            "source":{"title":"Supplied erratum"},"paths":["erratum.txt"],"extract_text":true,
            "reverify":{"artifact_ref":{"id":artifact,"revision":1}},
        }),
    );
    let verified = run(root, &["ingest", "--input", "verify.json", "--json"], 0);
    assert_eq!(verified["items"][0]["outcome"], "reverified");
    assert_eq!(verified["items"][0]["retention"]["state"], "retained");
    assert!(verified["items"][0]["derivative_id"].is_string());
    let read = run(
        root,
        &[
            "evidence",
            "read",
            &artifact,
            "--selector-file",
            "selector.json",
            "--case",
            &case,
            "--json",
        ],
        0,
    );
    assert_eq!(read["artifact"]["revision"], 2);
}
