//! Diagnostic logging (W05b.14): logs go to stderr only, JSON stdout is the
//! same at every verbosity, and no level prints evidence content, titles or
//! local absolute paths.
use std::{fs, path::Path, process::Command};

const TITLE: &str = "SENTINEL-TITLE-QUESTION";
const EXCERPT: &str = "SENTINEL-EXCERPT-BODY";
const LEVELS: &[&[&str]] = &[&[], &["-q"], &["-v"], &["-vv"], &["-vvv"]];

fn run(cwd: &Path, args: &[&str], extra: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .env_remove("INVESTIGATION_LOG")
        .envs(env.iter().copied())
        .args(args)
        .args(extra)
        .args(["--format", "json", "--actor", "agent:log-test"])
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn stdout_is_identical_at_every_verbosity_and_stderr_holds_no_content() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("evidence.log"),
        format!("{EXCERPT}\nsecond line\n"),
    )
    .unwrap();
    let init = run(
        root,
        &[
            "init",
            "ws",
            "--no-vault",
            "--import-root",
            root.to_str().unwrap(),
        ],
        &[],
        &[],
    );
    assert_eq!(init.0, 0, "{}", init.2);
    let ws = root.join("ws");
    let start = run(
        &ws,
        &["start", TITLE, "--attach", "../evidence.log"],
        &[],
        &[("INVESTIGATION_LOG", "trace")],
    );
    assert_eq!(start.0, 0, "{}", start.2);
    assert!(start.2.contains("stage=\"journal_publish\""), "{}", start.2);

    let commands: &[&[&str]] = &[
        &["next"],
        &["check"],
        &["brief"],
        &["search", "SENTINEL"],
        &["show", "case_does_not_exist"],
        &["receipt", "no-such-command"],
    ];
    let path = root.to_str().unwrap();
    for command in commands {
        let baseline = run(&ws, command, &[], &[]);
        for level in LEVELS {
            let (code, stdout, stderr) = run(&ws, command, level, &[]);
            assert_eq!(
                (code, &stdout),
                (baseline.0, &baseline.1),
                "{command:?} {level:?}"
            );
            for secret in [TITLE, EXCERPT, path] {
                assert!(
                    !stderr.replace(&baseline.2, "").contains(secret),
                    "{command:?} {level:?} logged {secret}: {stderr}"
                );
            }
        }
        // The environment variable is equivalent to a flag.
        let (code, stdout, _) = run(&ws, command, &[], &[("INVESTIGATION_LOG", "trace")]);
        assert_eq!(
            (code, &stdout),
            (baseline.0, &baseline.1),
            "{command:?} env"
        );
    }
}

#[test]
fn a_write_logs_stages_with_ids_only_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let init = run(root, &["init", "ws", "--no-vault"], &[], &[]);
    assert_eq!(init.0, 0);
    let ws = root.join("ws");
    let quiet = run(&ws, &["case", "new", TITLE], &[], &[]);
    let loud = run(&ws, &["case", "new", TITLE], &["-vv"], &[]);
    assert_eq!((quiet.0, loud.0), (0, 0));
    assert!(
        !quiet.2.contains("stage="),
        "default level is noisy: {}",
        quiet.2
    );
    for stage in [
        "lock",
        "verify_tail",
        "validate",
        "journal_write",
        "journal_publish",
        "directory_sync",
        "index_update",
    ] {
        assert!(
            loud.2.contains(&format!("stage=\"{stage}\"")),
            "missing {stage}: {}",
            loud.2
        );
    }
    assert!(loud.2.contains("transaction_id=\"tx_"));
    assert!(!loud.2.contains(TITLE) && !loud.2.contains(root.to_str().unwrap()));
    assert!(!loud.1.contains("stage="));
}
