//! `config list|get|path|set|unset`: the effective workspace settings, where
//! each value comes from, and the one setting a command changes here. It
//! never prints a secret.
//!
//! Settings come from `investigation.toml` (import roots, the vault policy,
//! an optional actor and inert extensions) and from the machine-local actor
//! in `local/actor.toml`. The actor is also named by `--actor` and `AGENT`.
//! Extension values are opaque, so only their names show.
//!
//! `set` and `unset` change only keys with no evidentiary meaning
//! ([`SETTABLE`]): today `vault_policy.viewer_release`, which chooses the
//! optional view files a render writes. A release is accepted only when the
//! bundled compatibility profile supports it, so a stated release always
//! changes what is rendered. Every other write stays on its explicit
//! command: `init --import-root`, `init --actor`, `init --existing-vault`.

use std::{collections::BTreeSet, env, fmt::Write as _};

use investigation_core::wire::{ErrorCode, Failure};
use investigation_render::compat::Profile;
use investigation_store::{
    Workspace,
    workspace::{CONFIG_FILE_NAME, LOCAL_ACTOR_FILE},
};
use serde_json::{Value, json};

use crate::{
    Global, Output,
    selection::{open_workspace, workspace_argv, workspace_failure},
};

pub(crate) const AFTER_HELP: &str = "\
Reports each effective setting with the source it comes from, so an agent never
has to read investigation.toml. Each line is `key  value  source`; JSON adds
`sources`, every place that states the key, in precedence order. The actor is
the first of: --actor, the actor key of investigation.toml, AGENT, then the
machine-local actor in local/actor.toml. Other keys are investigation.toml or
default. Extension values are never printed.
set and unset change one key with no evidentiary meaning:
vault_policy.viewer_release, the viewer release the vault is read with, which
chooses the optional view files a render writes. Change import roots, the actor
and the existing vault with investigation init, not by editing files.
Examples:
  investigation --workspace ./casework config list --format json
  investigation --workspace ./casework config get actor
  investigation --workspace ./casework config path
  investigation --workspace ./casework config set vault_policy.viewer_release 1.15.1 --format json
  investigation --workspace ./casework config unset vault_policy.viewer_release --format json
Next: investigation --workspace ./casework config list --format json";

const SET_AFTER_HELP: &str = "\
Only vault_policy.viewer_release can be set: the viewer release the vault is
read with. It is accepted only inside the supported range of the compatibility
profile built into this binary (doctor --viewer-release RELEASE shows how a
release resolves); a release outside it would render plain Markdown anyway, so
it is refused, and nothing is written. The next vault render adds or removes
the optional view files. investigation.toml is rewritten whole and keeps every
setting, but not hand-written comments. Setting the value already stated
changes nothing.
Examples:
  investigation --workspace ./casework config set vault_policy.viewer_release 1.15.1 --format json
  investigation --workspace ./casework doctor --viewer-release 1.15.1 --format json
Next: investigation --workspace ./casework vault render --format json";

const UNSET_AFTER_HELP: &str = "\
Remove vault_policy.viewer_release from investigation.toml, so the vault is
plain Markdown and tables only; the next vault render removes the optional view
files it wrote. Unsetting a key that is not stated changes nothing.
Example: investigation --workspace ./casework config unset vault_policy.viewer_release --format json
Next: investigation --workspace ./casework vault render --format json";

/// Keys `config set` and `config unset` change, with what each one does.
/// Only keys with no evidentiary meaning belong here.
const SETTABLE: &[(&str, &str)] = &[(
    "vault_policy.viewer_release",
    "the viewer release the vault is read with; chooses the optional view files a render writes",
)];

#[derive(clap::Subcommand)]
pub(crate) enum ConfigCommand {
    /// List every effective setting with the source each value comes from.
    #[command(
        after_help = "Example: investigation --workspace ./casework config list --format json\nNext: investigation --workspace ./casework config get actor --format json"
    )]
    List,
    /// Show one setting, its source and every other place that states it.
    #[command(
        after_help = "Example: investigation --workspace ./casework config get vault_policy.viewer_release --format json\nNext: investigation --workspace ./casework config path --format json"
    )]
    Get {
        /// Setting name, for example actor or vault_policy.viewer_release; config list names them all.
        #[arg(value_name = "KEY")]
        key: String,
    },
    /// Print the path of the investigation.toml that holds the settings.
    #[command(
        after_help = "Example: investigation --workspace ./casework config path --format json\nNext: investigation --workspace ./casework config list --format json"
    )]
    Path,
    /// State vault_policy.viewer_release, checked against the bundled compatibility profile.
    #[command(after_help = SET_AFTER_HELP)]
    Set {
        /// Setting to change; only vault_policy.viewer_release.
        #[arg(value_name = "KEY")]
        key: String,
        /// New value, for example 1.15.1.
        #[arg(value_name = "VALUE")]
        value: String,
    },
    /// Remove vault_policy.viewer_release, so the vault is plain Markdown only.
    #[command(after_help = UNSET_AFTER_HELP)]
    Unset {
        /// Setting to remove; only vault_policy.viewer_release.
        #[arg(value_name = "KEY")]
        key: String,
    },
}

/// One place that states a key.
struct Source {
    /// `flag`, `env`, `workspace_file`, `local_file` or `default`.
    origin: &'static str,
    /// The flag, variable or file that states it; absent for a default.
    name: Option<&'static str>,
    value: Value,
    valid: bool,
}

impl Source {
    fn json(&self) -> Value {
        let mut value = json!({"origin": self.origin, "value": self.value});
        if let Some(name) = self.name {
            value["source"] = json!(name);
        }
        if !self.valid {
            value["valid"] = json!(false);
        }
        value
    }
}

struct Entry {
    key: String,
    sources: Vec<Source>,
    /// Value is opaque: show the key and source only.
    hidden: bool,
}

impl Entry {
    fn effective(&self) -> Option<&Source> {
        self.sources.first().filter(|source| source.valid)
    }

    fn json(&self) -> Value {
        let effective = self.effective();
        json!({
            "key": self.key,
            "value": if self.hidden { Value::Null } else { effective.map_or(Value::Null, |s| s.value.clone()) },
            "origin": effective.map_or("unset", |s| s.origin),
            "source": effective.and_then(|s| s.name),
            "shown": !self.hidden,
            "sources": self.sources.iter().map(|source| {
                let mut value = source.json();
                if self.hidden { value["value"] = Value::Null; }
                value
            }).collect::<Vec<_>>(),
        })
    }

    fn line(&self) -> String {
        let effective = self.effective();
        let value = if self.hidden {
            "(not shown)".to_owned()
        } else if let Some(source) = effective {
            show(&source.value)
        } else if self.sources.is_empty() {
            "(unset)".to_owned()
        } else {
            "(invalid)".to_owned()
        };
        let source = match effective {
            Some(source) => source.name.unwrap_or("default"),
            None if self.sources.is_empty() => "default",
            None => self.sources[0].name.unwrap_or("default"),
        };
        format!("{}  {value}  {source}", self.key)
    }
}

fn show(value: &Value) -> String {
    match value {
        Value::Null => "(unset)".to_owned(),
        Value::String(text) => text.clone(),
        Value::Array(items) if items.is_empty() => "(none)".to_owned(),
        Value::Array(items) => items.iter().map(show).collect::<Vec<_>>().join(","),
        other => other.to_string(),
    }
}

fn entries(workspace: &Workspace, global: &Global) -> Result<Vec<Entry>, Failure> {
    let config = workspace.config();
    let stated: BTreeSet<String> = workspace.stated_config_keys().map_err(workspace_failure)?;
    let file = |key: &str| stated.contains(key);
    let mut entries = Vec::new();

    // Same order as selection::select_actor: flag, investigation.toml, AGENT,
    // then the machine-local actor.
    let mut actor = Vec::new();
    if let Some(flag) = &global.actor {
        actor.push(Source {
            origin: "flag",
            name: Some("--actor"),
            value: json!(flag),
            valid: true,
        });
    }
    if let Some(configured) = &config.actor {
        actor.push(Source {
            origin: "workspace_file",
            name: Some(CONFIG_FILE_NAME),
            value: json!(configured),
            valid: true,
        });
    }
    if let Some(raw) = env::var_os("AGENT") {
        let text = raw.to_string_lossy().into_owned();
        let valid = raw
            .to_str()
            .is_some_and(|text| text.parse::<investigation_core::header::Actor>().is_ok());
        actor.push(Source {
            origin: "env",
            name: Some("AGENT"),
            value: json!(text),
            valid,
        });
    }
    if let Some(local) = workspace.local_actor().map_err(workspace_failure)? {
        actor.push(Source {
            origin: "local_file",
            name: Some(LOCAL_ACTOR_FILE),
            value: json!(local),
            valid: true,
        });
    }
    entries.push(Entry {
        key: "actor".into(),
        sources: actor,
        hidden: false,
    });

    let single = |key: &str, value: Value| {
        let in_file = file(key);
        Entry {
            key: key.into(),
            sources: vec![Source {
                origin: if in_file { "workspace_file" } else { "default" },
                name: in_file.then_some(CONFIG_FILE_NAME),
                value,
                valid: true,
            }],
            hidden: false,
        }
    };
    let policy = &config.vault_policy;
    entries.push(single("import_roots", json!(config.import_roots)));
    entries.push(single("vault_policy.enabled", json!(policy.enabled)));
    entries.push(single(
        "vault_policy.include_approved_derivatives",
        json!(policy.include_approved_derivatives),
    ));
    entries.push(single(
        "vault_policy.preserve_human_folders",
        json!(policy.preserve_human_folders),
    ));
    entries.push(single(
        "vault_policy.viewer_release",
        json!(policy.viewer_release),
    ));
    entries.push(single(
        "vault_policy.existing_vault",
        json!(policy.existing_vault),
    ));
    entries.push(single(
        "vault_policy.managed_prefix",
        json!(policy.managed_prefix),
    ));
    for name in config.extensions.keys() {
        entries.push(Entry {
            key: format!("extensions.{name}"),
            sources: vec![Source {
                origin: "workspace_file",
                name: Some(CONFIG_FILE_NAME),
                value: Value::Null,
                valid: true,
            }],
            hidden: true,
        });
    }
    Ok(entries)
}

pub(crate) fn run(global: &Global, command: ConfigCommand) -> Result<Output, Failure> {
    let mut workspace = open_workspace(global.workspace.as_deref())?;
    match command {
        ConfigCommand::Set { key, value } => set(&mut workspace, global, &key, Some(&value)),
        ConfigCommand::Unset { key } => set(&mut workspace, global, &key, None),
        ConfigCommand::List => {
            let entries = entries(&workspace, global)?;
            let text = entries
                .iter()
                .map(Entry::line)
                .collect::<Vec<_>>()
                .join("\n");
            Ok(Output::success(
                "config",
                json!({
                    "path": config_path(&workspace),
                    "settings": entries.iter().map(Entry::json).collect::<Vec<_>>(),
                }),
                text,
            ))
        }
        ConfigCommand::Get { key } => {
            let entries = entries(&workspace, global)?;
            let Some(entry) = entries.iter().find(|entry| entry.key == key) else {
                let known: Vec<&str> = entries.iter().map(|entry| entry.key.as_str()).collect();
                let mut failure = Failure::new(
                    ErrorCode::InvalidInput,
                    format!(
                        "Unknown setting {key}. Known settings: {}.",
                        known.join(", ")
                    ),
                )
                .at("/key");
                failure.error.expected = Some(json!(known));
                failure.error.actual = Some(json!(key));
                let mut argv = workspace_argv(&workspace);
                argv.extend(["config", "list", "--format", "json"].map(str::to_owned));
                return Err(failure.with_argv(argv));
            };
            let mut text = entry.line();
            for other in entry.sources.iter().skip(1) {
                let _ = write!(
                    text,
                    "\n  also  {}  {}",
                    if entry.hidden {
                        "(not shown)".to_owned()
                    } else {
                        show(&other.value)
                    },
                    other.name.unwrap_or("default")
                );
            }
            Ok(Output::success(
                "config",
                json!({"setting": entry.json()}),
                text,
            ))
        }
        ConfigCommand::Path => {
            let path = config_path(&workspace);
            Ok(Output::success("config_path", json!({"path": path}), path))
        }
    }
}

fn config_argv(workspace: &Workspace, tail: &[&str]) -> Vec<String> {
    let mut argv = workspace_argv(workspace);
    argv.push("config".to_owned());
    argv.extend(tail.iter().map(|arg| (*arg).to_owned()));
    argv.extend(["--format", "json"].map(str::to_owned));
    argv
}

/// `config set KEY VALUE` (`value` given) or `config unset KEY`.
fn set(
    workspace: &mut Workspace,
    global: &Global,
    key: &str,
    value: Option<&str>,
) -> Result<Output, Failure> {
    let profile = Profile::bundled();
    let pinned = profile.pinned_release.version.clone();
    let verb = if value.is_some() { "set" } else { "unset" };
    if !SETTABLE.iter().any(|(name, _)| *name == key) {
        let settable: Vec<&str> = SETTABLE.iter().map(|(name, _)| *name).collect();
        let hint = match key {
            "import_roots" => " Add an import root with investigation init PATH --import-root DIR.",
            "actor" => {
                " Record this machine's actor with investigation init PATH --actor NAME, or pass \
                 --actor on each write."
            }
            "vault_policy.existing_vault" | "vault_policy.managed_prefix" => {
                " Change the existing vault with investigation init PATH --existing-vault VAULT \
                 --managed-prefix FOLDER."
            }
            _ => "",
        };
        let mut failure = Failure::new(
            ErrorCode::InvalidInput,
            format!(
                "config {verb} changes only settings with no evidentiary meaning: {}. {key} is \
                 not one of them, and nothing was written.{hint}",
                settable.join(", ")
            ),
        )
        .at("/key");
        failure.error.expected = Some(json!(settable));
        failure.error.actual = Some(json!(key));
        // A misspelt settable key gets the command it meant; any other key
        // the listing of every setting and its source.
        let meant = key.replace('-', "_").ends_with("viewer_release");
        let argv = match value {
            Some(_) if meant => config_argv(workspace, &["set", settable[0], &pinned]),
            None if meant => config_argv(workspace, &["unset", settable[0]]),
            _ => config_argv(workspace, &["list"]),
        };
        return Err(failure.with_argv(argv));
    }
    if let Some(value) = value {
        let resolved = profile.resolve(Some(value));
        if resolved.release.is_none() || value.trim() != value {
            let (min, max) = (&profile.supported_range.min, &profile.supported_range.max);
            let mut failure = Failure::new(
                ErrorCode::InvalidInput,
                format!(
                    "{value:?} is not a viewer release the bundled compatibility profile \
                     supports, so nothing was written. State a release from {min} to {max} as \
                     dotted numbers, for example {pinned}; a release outside that range gets \
                     plain Markdown and tables, which is what an unset value already gives."
                ),
            )
            .at("/value");
            failure.error.expected =
                Some(json!({"min": min, "max": max, "pinned_release": pinned}));
            failure.error.actual = Some(json!(value));
            return Err(failure.with_argv(config_argv(workspace, &["set", key, &pinned])));
        }
    }
    let (before, changed) = workspace
        .set_viewer_release(value)
        .map_err(workspace_failure)?;
    let entries = entries(workspace, global)?;
    let entry = entries
        .iter()
        .find(|entry| entry.key == key)
        .expect("settable keys are listed");
    let mut next = workspace_argv(workspace);
    next.extend(["vault", "render", "--format", "json"].map(str::to_owned));
    let mut text = entry.line();
    text.push_str(if changed { "  changed" } else { "  unchanged" });
    let _ = write!(text, "\nNext: {}", crate::display_argv(&next));
    Ok(Output::success(
        if value.is_some() {
            "config_set"
        } else {
            "config_unset"
        },
        json!({
            "setting": entry.json(),
            "changed": changed,
            "previous": before,
            "path": config_path(workspace),
            "next": [{"kind": "local_write", "argv": next}],
        }),
        text,
    ))
}

fn config_path(workspace: &Workspace) -> String {
    workspace
        .root()
        .join(CONFIG_FILE_NAME)
        .to_string_lossy()
        .into_owned()
}
