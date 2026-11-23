//! `config list|get|path`: the effective workspace settings and where each
//! value comes from. Read-only; it never writes and never prints a secret.
//!
//! Settings come from `investigation.toml` (import roots, the vault policy,
//! an optional actor and inert extensions) and from the machine-local actor
//! in `local/actor.toml`. The actor is also named by `--actor` and `AGENT`.
//! Writes stay on their explicit commands: `init --import-root` and
//! `init --actor`. Extension values are opaque, so only their names show.

use std::{collections::BTreeSet, env, fmt::Write as _};

use investigation_core::wire::{ErrorCode, Failure};
use investigation_store::{
    Workspace,
    workspace::{CONFIG_FILE_NAME, LOCAL_ACTOR_FILE},
};
use serde_json::{Value, json};

use crate::{
    Global, Output,
    selection::{open_workspace, workspace_failure},
};

pub(crate) const AFTER_HELP: &str = "\
Reports each effective setting with the source it comes from, so an agent never
has to read investigation.toml. Each line is `key  value  source`; JSON adds
`sources`, every place that states the key, in precedence order. The actor is
the first of: --actor, the actor key of investigation.toml, AGENT, then the
machine-local actor in local/actor.toml. Other keys are investigation.toml or
default. Extension values are never printed. This command only reads; change
import roots and the actor with investigation init, not by editing files.
Examples:
  investigation --workspace ./casework config list --format json
  investigation --workspace ./casework config get actor
  investigation --workspace ./casework config path
Next: investigation --workspace ./casework config list --format json";

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
    let workspace = open_workspace(global.workspace.as_deref())?;
    match command {
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
                let mut argv = crate::selection::workspace_argv(&workspace);
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

fn config_path(workspace: &Workspace) -> String {
    workspace
        .root()
        .join(CONFIG_FILE_NAME)
        .to_string_lossy()
        .into_owned()
}
