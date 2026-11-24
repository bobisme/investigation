//! Versioned, ordered migrations of the workspace format (plan §10.7,
//! W11.15, ADR 0014).
//!
//! A migration moves a workspace from format `version() - 1` to format
//! `version()` (see [`crate::workspace::format`]). The [`Registry`] holds
//! them in order, numbered without gaps from 2. The orchestrator in
//! `investigation_render::migrate` runs them: a dry run first, then a
//! verified workspace archive as the backup, then the changes, then a
//! rebuilt index and `check`, and only then the new format in the marker.
//!
//! **What a migration may change.** Only the areas it declares in
//! [`Migration::touches`]: the disposable `cache/`, the machine-local
//! `local/`, the generated vault notes and their render record, and
//! non-journal layout under `state/`. The orchestrator alone writes the
//! marker's `format` section.
//!
//! **What a migration never changes.** A case genesis or event file, a
//! retained blob, any record, transaction or command ID, the owner
//! configuration `investigation.toml`, the human vault folders (Notes,
//! Boards, Inbox), `exports/`, or anything under an import root. The
//! journal is canonical and append-only: a migration that needed to change
//! committed bytes would instead be a new journal format written to a new
//! workspace with an ID mapping, keeping the original archive (plan §10.7),
//! and is not what this module runs. After every migration the orchestrator
//! checks that every case head, the blobs and the owner configuration are
//! exactly those of the backup, and refuses to publish otherwise.
//!
//! **Reads never migrate.** Opening a workspace, and every read command,
//! changes nothing here; they report a pending migration (`check`,
//! `doctor`, `migrate` without `--apply`).
//!
//! No migration is needed today: every format written so far is read by
//! the current readers (legacy events are upcast, older indexes rebuilt),
//! so [`Registry::builtin`] is empty and [`FORMAT_VERSION`] is 1. The tests
//! run the whole path with a test migration.
//!
//! **Checklist for the first real migration** (ADR 0014). Do all of it in
//! one change:
//!
//! 1. Before changing any code, add a directory to `fixtures/historical/`
//!    with a workspace that the *last release* wrote, at the format you are
//!    leaving. Never regenerate it with the new build. List its files in
//!    `fixtures/historical/SHA256SUMS`, add a row to that README, and add
//!    its name to `HISTORICAL` in `crates/investigation-cli/tests/migration.rs`.
//!    Leave the older directories as they are.
//! 2. Implement [`Migration`] with `version()` 2. Declare every area it
//!    changes in `touches()`, raise `read_requires` for anything an older
//!    reader must not ignore, and make `apply` idempotent.
//! 3. Register it in [`Registry::builtin`].
//! 4. Raise [`FORMAT_VERSION`] to [`Registry::latest`]. The test
//!    `the_builtin_registry_matches_the_format_this_program_writes` fails until the two agree.
//! 5. Make new workspaces start at the new format. `Workspace::init` (and
//!    so `demo` and `init_demo`) writes no `format` section today, which
//!    means format 1. Left alone, every new workspace would need the
//!    migration at once.
//! 6. Update what says "no migration": this module doc, the
//!    `FORMAT_VERSION` doc, ADR 0014 ("No migration is needed today" and
//!    its Consequences), and the assertion that the builtin registry is at
//!    format 1 in `tests/migration.rs`. Record the migration's decision in
//!    ADR 0014 or a new ADR.
//! 7. Expect format-1 workspaces and archives (the scenario fixtures, the
//!    golden workspaces, every restore test) to report
//!    `MIGRATION_AVAILABLE` in `check`. Decide whether each test migrates
//!    first or accepts the warning. Regenerate the CLI goldens with
//!    `scripts/check-cli-goldens.py --write` and review every changed line.

use std::fmt;

use crate::workspace::{FORMAT_VERSION, Workspace, WorkspaceFormat};

/// An area of the workspace a migration may change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Area {
    /// `cache/`: the index, kept journal heads and blob verdicts.
    Cache,
    /// `local/`: machine-local bookkeeping such as the render record.
    Local,
    /// The generated vault notes and their render record, by a render; the
    /// human folders are never written.
    GeneratedVault,
    /// Non-journal layout under `state/`, such as `state/checkpoints` and
    /// `state/tmp`; never a case journal, a blob or the marker.
    StateLayout,
}

impl Area {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cache => "cache",
            Self::Local => "local",
            Self::GeneratedVault => "generated_vault",
            Self::StateLayout => "state_layout",
        }
    }

    /// What the area holds, for the dry run.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Cache => "cache/: the disposable index, kept journal heads and blob verdicts",
            Self::Local => "local/: machine-local bookkeeping",
            Self::GeneratedVault => {
                "generated vault notes and their render record; never Notes, Boards or Inbox"
            }
            Self::StateLayout => {
                "non-journal layout under state/; never a case journal, a blob or the marker"
            }
        }
    }
}

/// What a migration never changes, stated in every dry run.
pub const NEVER_CHANGED: [&str; 7] = [
    "case journals: genesis.json and every event file under state/cases",
    "retained blobs under state/blobs",
    "record, transaction and command IDs, and so every receipt",
    "investigation.toml, the owner configuration",
    "the human vault folders Notes, Boards and Inbox",
    "exports/ and every import root",
    "the workspace ID",
];

/// Why a migration could not describe or apply its change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationError(pub String);

impl fmt::Display for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MigrationError {}

/// One step of the workspace format.
pub trait Migration: Send + Sync {
    /// The format this migration produces; it runs on `version() - 1`.
    fn version(&self) -> u32;
    /// A stable name, such as `0002-split-cache`.
    fn name(&self) -> &'static str;
    /// One line for the dry run.
    fn summary(&self) -> &'static str;
    /// The areas it changes. Everything else is left as it was.
    fn touches(&self) -> &'static [Area];
    /// `read_requires` and `write_requires` once it has run.
    fn requires(&self) -> (u32, u32);
    /// Whether this program refuses writes to a workspace until this
    /// migration has run. Prefer `false`: a reader of the older format.
    fn required_before_writes(&self) -> bool {
        false
    }
    /// What [`Self::apply`] would change, without writing anything.
    fn describe(&self, workspace: &Workspace) -> Result<Vec<String>, MigrationError>;
    /// Make the change. It must be idempotent: after a kill at any point it
    /// runs again on a workspace it may have partly changed.
    fn apply(&self, workspace: &Workspace) -> Result<(), MigrationError>;
}

/// Migrations in order: format 2, 3, and so on, without gaps.
pub struct Registry {
    migrations: Vec<Box<dyn Migration>>,
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.migrations.iter().map(|migration| migration.name()))
            .finish()
    }
}

impl Registry {
    /// The migrations this program ships: none today.
    pub fn builtin() -> Self {
        Self {
            migrations: Vec::new(),
        }
    }

    /// A registry of `migrations`, which must be numbered 2, 3, ... in
    /// order, with unique names and requirements no later than their own
    /// format and no earlier than the one before.
    pub fn new(migrations: Vec<Box<dyn Migration>>) -> Result<Self, MigrationError> {
        let mut names = std::collections::BTreeSet::new();
        let mut previous = (1, 1);
        for (index, migration) in migrations.iter().enumerate() {
            let expected = index as u32 + 2;
            let fail =
                |reason: String| Err(MigrationError(format!("{}: {reason}", migration.name())));
            if migration.version() != expected {
                return fail(format!(
                    "is numbered {}, but the next format is {expected}",
                    migration.version()
                ));
            }
            if !names.insert(migration.name()) {
                return fail("the name is used twice".to_owned());
            }
            let (read, write) = migration.requires();
            if read == 0
                || read > write
                || write > expected
                || read < previous.0
                || write < previous.1
            {
                return fail(format!(
                    "requires read {read} and write {write}, which must satisfy \
                     {} <= read <= write <= {expected} and write >= {}",
                    previous.0, previous.1
                ));
            }
            if migration.touches().is_empty() {
                return fail("declares no area it changes".to_owned());
            }
            previous = (read, write);
        }
        Ok(Self { migrations })
    }

    /// The format a workspace is at once every migration here has run.
    pub fn latest(&self) -> u32 {
        1 + self.migrations.len() as u32
    }

    /// The migrations a workspace at `version` still needs, in order.
    pub fn pending(&self, version: u32) -> &[Box<dyn Migration>] {
        let start = (version.max(1) - 1) as usize;
        self.migrations.get(start..).unwrap_or(&[])
    }

    /// The migration that produces `version`.
    pub fn get(&self, version: u32) -> Option<&dyn Migration> {
        version
            .checked_sub(2)
            .and_then(|index| self.migrations.get(index as usize))
            .map(Box::as_ref)
    }

    /// The format after the last pending migration that must run before
    /// writes, if any.
    pub fn required_before_writes(&self, format: &WorkspaceFormat) -> Option<u32> {
        self.pending(format.version)
            .iter()
            .filter(|migration| migration.required_before_writes())
            .map(|migration| migration.version())
            .next_back()
    }
}

/// Why this program must not write a workspace at `format` now, if it must
/// not: a migration in progress, a newer format, or a required built-in
/// migration.
pub fn write_refusal(format: &WorkspaceFormat) -> Option<crate::workspace::FormatRefusal> {
    let builtin = Registry::builtin();
    format.write_refusal(FORMAT_VERSION, builtin.required_before_writes(format))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Step(u32, &'static str, (u32, u32));

    impl Migration for Step {
        fn version(&self) -> u32 {
            self.0
        }
        fn name(&self) -> &'static str {
            self.1
        }
        fn summary(&self) -> &'static str {
            "test"
        }
        fn touches(&self) -> &'static [Area] {
            &[Area::Cache]
        }
        fn requires(&self) -> (u32, u32) {
            self.2
        }
        fn describe(&self, _: &Workspace) -> Result<Vec<String>, MigrationError> {
            Ok(Vec::new())
        }
        fn apply(&self, _: &Workspace) -> Result<(), MigrationError> {
            Ok(())
        }
    }

    #[test]
    fn the_builtin_registry_matches_the_format_this_program_writes() {
        let builtin = Registry::builtin();
        assert_eq!(builtin.latest(), FORMAT_VERSION);
        assert!(builtin.pending(FORMAT_VERSION).is_empty());
        assert_eq!(write_refusal(&WorkspaceFormat::BASELINE), None);
    }

    #[test]
    fn a_registry_is_numbered_in_order_without_gaps() {
        let ok = Registry::new(vec![
            Box::new(Step(2, "a", (1, 2))),
            Box::new(Step(3, "b", (1, 3))),
        ])
        .unwrap();
        assert_eq!(ok.latest(), 3);
        assert_eq!(ok.pending(1).len(), 2);
        assert_eq!(ok.pending(2)[0].name(), "b");
        assert!(ok.pending(3).is_empty() && ok.pending(9).is_empty());
        assert_eq!(ok.get(3).unwrap().name(), "b");
        assert!(ok.get(1).is_none());
        for broken in [
            vec![Box::new(Step(3, "a", (1, 1))) as Box<dyn Migration>],
            vec![
                Box::new(Step(2, "a", (1, 1))),
                Box::new(Step(4, "b", (1, 1))),
            ],
            vec![
                Box::new(Step(2, "a", (1, 1))),
                Box::new(Step(3, "a", (1, 1))),
            ],
            vec![Box::new(Step(2, "a", (2, 1)))],
            vec![Box::new(Step(2, "a", (1, 3)))],
            vec![
                Box::new(Step(2, "a", (1, 2))),
                Box::new(Step(3, "b", (1, 1))),
            ],
        ] {
            assert!(Registry::new(broken).is_err());
        }
    }
}
