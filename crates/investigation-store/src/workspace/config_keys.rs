//! Which settings `investigation.toml` states, as opposed to defaults.
//!
//! [`Workspace::config`] merges file and defaults; `config` needs to tell
//! them apart to name each value's source.

use std::collections::BTreeSet;

use super::{CONFIG_FILE, Workspace, WorkspaceError, ensure_regular_file, read_text};

/// Path of the workspace settings file, relative to the workspace root.
pub const CONFIG_FILE_NAME: &str = CONFIG_FILE;

impl Workspace {
    /// Dotted keys the settings file states: top-level keys, the keys of the
    /// `vault_policy` table and the names under `extensions`.
    pub fn stated_config_keys(&self) -> Result<BTreeSet<String>, WorkspaceError> {
        let path = self.root.join(CONFIG_FILE);
        ensure_regular_file(&path)?;
        let value: toml::Table = toml::from_str(&read_text(&path)?)
            .map_err(|error| WorkspaceError::InvalidConfig(error.to_string()))?;
        let mut keys = BTreeSet::new();
        for (key, value) in &value {
            keys.insert(key.clone());
            if matches!(key.as_str(), "vault_policy" | "extensions")
                && let Some(table) = value.as_table()
            {
                keys.extend(table.keys().map(|inner| format!("{key}.{inner}")));
            }
        }
        Ok(keys)
    }
}
