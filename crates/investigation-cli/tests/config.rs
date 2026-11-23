//! `config list|get|path`: effective settings with their sources, read-only.

use std::{fs, path::Path, process::Command};

use serde_json::Value;

fn run(cwd: &Path, agent: Option<&str>, args: &[&str]) -> (i32, Value, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_investigation"));
    command
        .current_dir(cwd)
        .env_remove("AGENT")
        .env_remove("FORMAT")
        .args(args);
    if let Some(agent) = agent {
        command.env("AGENT", agent);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    (
        output.status.code().unwrap(),
        serde_json::from_str(&stdout).unwrap_or(Value::Null),
        stdout,
    )
}

fn workspace(extra: &[&str]) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let mut args = vec!["init", "./casework", "--format", "json"];
    args.extend_from_slice(extra);
    let (code, value, _) = run(directory.path(), None, &args);
    assert_eq!(code, 0, "{value}");
    directory
}

fn setting<'a>(list: &'a Value, key: &str) -> &'a Value {
    list["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|setting| setting["key"] == key)
        .unwrap_or_else(|| panic!("no setting {key}"))
}

#[test]
fn list_names_every_setting_and_its_source() {
    let directory = workspace(&["--actor", "agent:local"]);
    let (code, list, _) = run(
        directory.path(),
        None,
        &[
            "--workspace",
            "casework",
            "config",
            "list",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 0, "{list}");
    assert_eq!(list["result"], "config");
    let actor = setting(&list, "actor");
    assert_eq!(actor["value"], "agent:local");
    assert_eq!(actor["origin"], "local_file");
    assert_eq!(actor["source"], "local/actor.toml");
    assert_eq!(
        setting(&list, "vault_policy.enabled")["origin"],
        "workspace_file"
    );
    let viewer = setting(&list, "vault_policy.viewer_release");
    assert_eq!(viewer["origin"], "default");
    assert!(viewer["value"].is_null());
    assert_eq!(setting(&list, "import_roots")["origin"], "workspace_file");

    let (_, _, text) = run(
        directory.path(),
        None,
        &[
            "--workspace",
            "casework",
            "config",
            "list",
            "--format",
            "text",
        ],
    );
    assert!(
        text.contains("actor  agent:local  local/actor.toml"),
        "{text}"
    );
    assert!(
        text.contains("vault_policy.viewer_release  (unset)  default"),
        "{text}"
    );
}

#[test]
fn the_actor_follows_the_precedence_of_a_write_and_lists_what_it_shadows() {
    let directory = workspace(&["--actor", "agent:local"]);
    let get = |agent: Option<&str>, flag: &[&str]| {
        let mut args = vec!["--workspace", "casework"];
        args.extend_from_slice(flag);
        args.extend(["config", "get", "actor", "--format", "json"]);
        run(directory.path(), agent, &args).1
    };
    let both = get(Some("agent:env"), &["--actor", "agent:flag"]);
    assert_eq!(both["setting"]["value"], "agent:flag");
    assert_eq!(both["setting"]["origin"], "flag");
    let origins: Vec<&str> = both["setting"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| source["origin"].as_str().unwrap())
        .collect();
    assert_eq!(origins, ["flag", "env", "local_file"]);
    let env = get(Some("agent:env"), &[]);
    assert_eq!(env["setting"]["origin"], "env");
    assert_eq!(env["setting"]["source"], "AGENT");

    // The file setting outranks AGENT, as it does for a write.
    let toml = directory.path().join("casework/investigation.toml");
    let text = fs::read_to_string(&toml).unwrap();
    fs::write(&toml, format!("actor = \"agent:filed\"\n{text}")).unwrap();
    let filed = get(Some("agent:env"), &[]);
    assert_eq!(filed["setting"]["value"], "agent:filed");
    assert_eq!(filed["setting"]["origin"], "workspace_file");
}

#[test]
fn an_unset_actor_and_the_viewer_release_report_their_state() {
    let directory = workspace(&[]);
    let (_, actor, _) = run(
        directory.path(),
        None,
        &[
            "--workspace",
            "casework",
            "config",
            "get",
            "actor",
            "--format",
            "json",
        ],
    );
    assert_eq!(actor["setting"]["origin"], "unset");
    assert!(actor["setting"]["value"].is_null());

    let toml = directory.path().join("casework/investigation.toml");
    let text = fs::read_to_string(&toml).unwrap();
    fs::write(
        &toml,
        text.replace(
            "[vault_policy]",
            "[vault_policy]\nviewer_release = \"1.15.1\"",
        ),
    )
    .unwrap();
    let (code, viewer, _) = run(
        directory.path(),
        None,
        &[
            "--workspace",
            "casework",
            "config",
            "get",
            "vault_policy.viewer_release",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 0, "{viewer}");
    assert_eq!(viewer["setting"]["value"], "1.15.1");
    assert_eq!(viewer["setting"]["origin"], "workspace_file");
}

#[test]
fn an_unknown_key_is_refused_with_the_known_keys() {
    let directory = workspace(&[]);
    let (code, failure, _) = run(
        directory.path(),
        None,
        &[
            "--workspace",
            "casework",
            "config",
            "get",
            "colour",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 2);
    assert_eq!(failure["error"]["code"], "INVALID_INPUT");
    let known = failure["error"]["expected"].as_array().unwrap();
    assert!(known.iter().any(|key| key == "actor"));
    assert!(known.iter().any(|key| key == "vault_policy.viewer_release"));
}

#[test]
fn extension_values_are_never_printed() {
    let directory = workspace(&[]);
    let toml = directory.path().join("casework/investigation.toml");
    let mut text = fs::read_to_string(&toml).unwrap();
    text.push_str("\n[extensions.\"acme.tool\"]\ntoken = \"hunter2-secret\"\n");
    fs::write(&toml, text).unwrap();
    for format in ["json", "text"] {
        let (code, _, out) = run(
            directory.path(),
            None,
            &[
                "--workspace",
                "casework",
                "config",
                "list",
                "--format",
                format,
            ],
        );
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("extensions.acme.tool"), "{out}");
        assert!(!out.contains("hunter2"), "{out}");
    }
}

#[test]
fn path_prints_the_settings_file_and_the_command_is_read_only() {
    let directory = workspace(&[]);
    let (code, _, text) = run(
        directory.path(),
        None,
        &[
            "--workspace",
            "casework",
            "--read-only",
            "config",
            "path",
            "--format",
            "text",
        ],
    );
    assert_eq!(code, 0);
    assert!(
        text.trim_end().ends_with("casework/investigation.toml"),
        "{text}"
    );
    assert!(Path::new(text.trim_end()).is_file());
    let before = fs::read(directory.path().join("casework/investigation.toml")).unwrap();
    for args in [&["list"][..], &["get", "actor"], &["path"]] {
        let mut argv = vec!["--workspace", "casework", "--read-only", "config"];
        argv.extend_from_slice(args);
        argv.extend(["--format", "json"]);
        assert_eq!(run(directory.path(), None, &argv).0, 0);
    }
    assert_eq!(
        fs::read(directory.path().join("casework/investigation.toml")).unwrap(),
        before
    );
}
