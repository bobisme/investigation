//! `config list|get|path`: effective settings with their sources, read-only;
//! `config set|unset`: the one settable key, checked against the bundled
//! compatibility profile.

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

fn config(directory: &Path, args: &[&str]) -> (i32, Value, String) {
    let mut argv = vec!["--workspace", "casework"];
    argv.extend_from_slice(args);
    argv.extend(["--format", "json"]);
    run(directory, None, &argv)
}

/// Vault files under `Cases/` with this extension.
fn vault_files(directory: &Path, extension: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![directory.join("casework/vault/Cases")];
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|found| found == extension) {
                found.push(path.to_string_lossy().into_owned());
            }
        }
    }
    found
}

#[test]
fn set_states_a_supported_release_and_the_next_render_follows_it() {
    let directory = workspace(&[]);
    let (code, case, _) = run(
        directory.path(),
        None,
        &[
            "--workspace",
            "casework",
            "--actor",
            "agent:config-test",
            "case",
            "new",
            "Request latency",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 0, "{case}");
    let (code, render, _) = config(directory.path(), &["vault", "render"]);
    assert_eq!(code, 0, "{render}");
    assert!(vault_files(directory.path(), "base").is_empty());

    let (code, set, _) = config(
        directory.path(),
        &["config", "set", "vault_policy.viewer_release", "1.15.1"],
    );
    assert_eq!(code, 0, "{set}");
    assert_eq!(set["result"], "config_set");
    assert_eq!(set["changed"], true);
    assert!(set["previous"].is_null());
    assert_eq!(set["setting"]["value"], "1.15.1");
    assert_eq!(set["setting"]["origin"], "workspace_file");
    assert_eq!(set["next"][0]["argv"][3], "vault");
    let (_, got, _) = config(
        directory.path(),
        &["config", "get", "vault_policy.viewer_release"],
    );
    assert_eq!(got["setting"]["value"], "1.15.1");
    assert_eq!(got["setting"]["source"], "investigation.toml");

    // The same value again writes nothing.
    let toml = directory.path().join("casework/investigation.toml");
    let before = fs::read(&toml).unwrap();
    let (code, again, _) = config(
        directory.path(),
        &["config", "set", "vault_policy.viewer_release", "1.15.1"],
    );
    assert_eq!(code, 0, "{again}");
    assert_eq!(again["changed"], false);
    assert_eq!(fs::read(&toml).unwrap(), before);

    // The render now writes the optional view files the release allows.
    let (code, render, _) = config(directory.path(), &["vault", "render"]);
    assert_eq!(code, 0, "{render}");
    assert!(!vault_files(directory.path(), "base").is_empty());

    let (code, unset, _) = config(
        directory.path(),
        &["config", "unset", "vault_policy.viewer_release"],
    );
    assert_eq!(code, 0, "{unset}");
    assert_eq!(unset["result"], "config_unset");
    assert_eq!(unset["changed"], true);
    assert_eq!(unset["previous"], "1.15.1");
    assert_eq!(unset["setting"]["origin"], "default");
    assert!(
        !fs::read_to_string(&toml)
            .unwrap()
            .contains("viewer_release")
    );
    let (code, render, _) = config(directory.path(), &["vault", "render"]);
    assert_eq!(code, 0, "{render}");
    assert!(vault_files(directory.path(), "base").is_empty());
    let (code, unset, _) = config(
        directory.path(),
        &["config", "unset", "vault_policy.viewer_release"],
    );
    assert_eq!(code, 0, "{unset}");
    assert_eq!(unset["changed"], false);
}

#[test]
fn set_refuses_a_release_the_profile_does_not_support_and_keys_it_does_not_change() {
    let directory = workspace(&[]);
    let toml = directory.path().join("casework/investigation.toml");
    let before = fs::read(&toml).unwrap();
    for value in ["2.0", "1.0.0", "latest", " 1.15.1", "1.15.1-beta", ""] {
        let (code, failure, _) = config(
            directory.path(),
            &["config", "set", "vault_policy.viewer_release", value],
        );
        assert_eq!(code, 2, "{value:?}: {failure}");
        assert_eq!(failure["error"]["code"], "INVALID_INPUT");
        assert_eq!(failure["error"]["path"], "/value");
        assert_eq!(failure["error"]["expected"]["max"], "1.15.1");
        assert_eq!(fs::read(&toml).unwrap(), before, "{value:?} wrote");
    }
    // The repair is a command that works.
    let (_, failure, _) = config(
        directory.path(),
        &["config", "set", "vault_policy.viewer_release", "9.9"],
    );
    let repair: Vec<String> = failure["error"]["repair"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        &repair[3..6],
        ["config", "set", "vault_policy.viewer_release"]
    );
    let repair: Vec<&str> = repair[1..].iter().map(String::as_str).collect();
    let (code, fixed, _) = run(directory.path(), None, &repair);
    assert_eq!(code, 0, "{fixed}");
    fs::write(&toml, &before).unwrap();

    for (key, repaired) in [
        ("actor", "list"),
        ("import_roots", "list"),
        ("vault_policy.enabled", "list"),
        ("vault_policy.existing_vault", "list"),
        ("viewer-release", "set"),
    ] {
        let (code, failure, _) = config(directory.path(), &["config", "set", key, "x"]);
        assert_eq!(code, 2, "{key}: {failure}");
        assert_eq!(failure["error"]["path"], "/key");
        assert_eq!(
            failure["error"]["expected"][0],
            "vault_policy.viewer_release"
        );
        assert_eq!(failure["error"]["repair"]["argv"][4], repaired, "{failure}");
        let (code, failure, _) = config(directory.path(), &["config", "unset", key]);
        assert_eq!(code, 2, "{key}: {failure}");
        assert_eq!(fs::read(&toml).unwrap(), before, "{key} wrote");
    }
    let (_, failure, _) = config(directory.path(), &["config", "set", "actor", "x"]);
    let message = failure["error"]["message"].as_str().unwrap();
    assert!(message.contains("init PATH --actor"), "{message}");
}

#[test]
fn set_and_unset_are_writes_and_teach_themselves() {
    let directory = workspace(&[]);
    let toml = directory.path().join("casework/investigation.toml");
    let before = fs::read(&toml).unwrap();
    for args in [
        &["config", "set", "vault_policy.viewer_release", "1.15.1"][..],
        &["config", "unset", "vault_policy.viewer_release"],
    ] {
        let mut argv = vec!["--read-only"];
        argv.extend_from_slice(args);
        let (code, failure, _) = config(directory.path(), &argv);
        assert_ne!(code, 0, "{failure}");
        assert_eq!(fs::read(&toml).unwrap(), before);
    }
    for (command, example) in [
        ("set", "config set vault_policy.viewer_release 1.15.1"),
        ("unset", "config unset vault_policy.viewer_release"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_investigation"))
            .args(["config", command, "--help"])
            .output()
            .unwrap();
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains(example), "{help}");
        assert!(help.contains("Next: "), "{help}");
        assert!(!help.to_lowercase().contains("obsidian"), "{help}");
    }
}
