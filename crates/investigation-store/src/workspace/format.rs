//! The workspace format: which on-disk layout a workspace is at, and what a
//! program must understand to read it and to write it (plan §10.7, W11.15,
//! ADR 0014).
//!
//! The format lives in the workspace marker, `state/workspace.json`, as an
//! optional `format` section, additive like `demo` and `fork`. A marker
//! without it is at [`WorkspaceFormat::BASELINE`], format 1, which every
//! workspace made so far is. Only a migration adds or changes the section
//! (see [`crate::migration`]); `init` never writes it, so a new workspace
//! keeps the marker bytes earlier builds wrote.
//!
//! The section carries three numbers, in the way a filesystem carries
//! compatible, read-only-compatible and incompatible feature sets:
//!
//! - `version`: the format the workspace is at, the last migration applied.
//! - `read_requires`: the oldest format a program must understand to read
//!   it. A program that understands less refuses every command, reads too.
//! - `write_requires`: the oldest format a program must understand to write
//!   it. A program that understands less still reads, and refuses every
//!   write with the reason.
//!
//! `migrating` is present only while a migration runs, from the moment its
//! backup is verified until its result is published. Every write is then
//! refused, by every program, and `check` reports the migration as
//! incomplete; `migrate --apply` with the same plan finishes it.
//!
//! The section is read before the strict decode of the marker, so a marker
//! that a newer program wrote is explained rather than merely rejected.
//! Programs before this section existed refuse any marker that carries it
//! (the marker denies unknown fields): they fail closed.

use std::{fmt, fs};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{GENESIS_FILE, Workspace, WorkspaceError, WorkspaceGenesis, io_at, sync_directory};

/// The highest workspace format this program reads and writes: 1 plus the
/// number of built-in migrations ([`crate::migration::Registry::builtin`]).
/// Raising it is step 4 of the first-migration checklist in
/// [`crate::migration`].
pub const FORMAT_VERSION: u32 = 1;
/// Where a replacement marker is staged before it replaces the marker.
const STAGED_MARKER: &str = "state/tmp/format-marker";
/// Longest plan ID or export ID a marker holds.
const MAX_ID_CHARS: usize = 128;
/// Marker fields this program knows; a newer marker may carry others.
const KNOWN_FIELDS: [&str; 7] = [
    "schema_version",
    "schema_family",
    "workspace_id",
    "genesis_sequence",
    "demo",
    "fork",
    "format",
];

/// The `format` section of the workspace marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceFormat {
    /// The format the workspace is at.
    pub version: u32,
    /// The oldest format a program must understand to read the workspace.
    pub read_requires: u32,
    /// The oldest format a program must understand to write the workspace.
    pub write_requires: u32,
    /// Present only while a migration runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrating: Option<Migrating>,
}

/// A migration that has started and not yet published its result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Migrating {
    /// The format before the migration; equal to `version` until it ends.
    pub from: u32,
    /// The format the migration publishes.
    pub to: u32,
    /// The plan the migration runs: the ID its dry run printed.
    pub plan_id: String,
    /// The `export_id` of the verified workspace archive taken before any
    /// change. Restoring it reverses the migration exactly.
    pub backup_export_id: String,
}

/// The section as a newer program may have written it: unknown fields are
/// ignored, so the numbers can be read to explain a refusal.
#[derive(Deserialize)]
struct LenientFormat {
    version: u32,
    read_requires: u32,
    write_requires: u32,
    #[serde(default)]
    migrating: Option<LenientMigrating>,
}

#[derive(Deserialize)]
struct LenientMigrating {
    from: u32,
    to: u32,
    #[serde(default)]
    plan_id: String,
    #[serde(default)]
    backup_export_id: String,
}

/// Where a workspace's format stands for this program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatStatus {
    /// At the format this program writes.
    Current,
    /// At an older format; migrations up to `latest` are available.
    Older { version: u32, latest: u32 },
    /// At a newer format that this program may still read and write.
    NewerCompatible { version: u32 },
    /// At a newer format that this program reads but must not write.
    WritesRefused { version: u32, write_requires: u32 },
    /// A migration started and has not published its result.
    Migrating { from: u32, to: u32 },
}

/// Why this program refuses to write a workspace, with what to do instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatRefusal {
    /// `WORKSPACE_FORMAT_NEWER`, `MIGRATION_INCOMPLETE` or `MIGRATION_REQUIRED`.
    pub code: &'static str,
    pub message: String,
    /// The command that moves the workspace on, when this program has one.
    pub next: Option<Vec<String>>,
}

impl fmt::Display for FormatRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl WorkspaceFormat {
    /// Every workspace without a `format` section.
    pub const BASELINE: Self = Self {
        version: 1,
        read_requires: 1,
        write_requires: 1,
        migrating: None,
    };

    /// Whether the section is well formed.
    pub fn validate(&self) -> Result<(), WorkspaceError> {
        let invalid =
            |reason: &str| Err(WorkspaceError::InvalidGenesis(format!("format: {reason}")));
        let top = self
            .migrating
            .as_ref()
            .map_or(self.version, |migrating| migrating.to);
        if self.version == 0 || self.read_requires == 0 || self.write_requires == 0 {
            return invalid("versions start at 1");
        }
        if self.read_requires > self.write_requires || self.write_requires > top {
            return invalid("read_requires <= write_requires <= the format must hold");
        }
        if let Some(migrating) = &self.migrating {
            if migrating.from != self.version || migrating.to <= migrating.from {
                return invalid("a migration runs from the current format to a later one");
            }
            for (name, id) in [
                ("plan_id", &migrating.plan_id),
                ("backup_export_id", &migrating.backup_export_id),
            ] {
                if id.is_empty()
                    || id.chars().count() > MAX_ID_CHARS
                    || id.chars().any(char::is_control)
                {
                    return invalid(&format!(
                        "{name} is empty, too long or has a control character"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Where this format stands for a program that understands formats up
    /// to `supported` and has migrations up to `latest`. A format whose
    /// `read_requires` exceeds `supported` never gets this far: opening it
    /// fails with [`WorkspaceError::FormatTooNew`].
    pub fn status(&self, supported: u32, latest: u32) -> FormatStatus {
        if let Some(migrating) = &self.migrating {
            return FormatStatus::Migrating {
                from: migrating.from,
                to: migrating.to,
            };
        }
        if self.write_requires > supported {
            return FormatStatus::WritesRefused {
                version: self.version,
                write_requires: self.write_requires,
            };
        }
        match self.version {
            version if version < latest => FormatStatus::Older { version, latest },
            version if version > supported => FormatStatus::NewerCompatible { version },
            _ => FormatStatus::Current,
        }
    }

    /// Why this program must not write a workspace at this format, if it
    /// must not. `required` says whether a pending built-in migration must
    /// run before writes; see [`crate::migration::required_before_writes`].
    pub fn write_refusal(&self, supported: u32, required: Option<u32>) -> Option<FormatRefusal> {
        let migrate = |apply: bool| {
            let mut argv = vec![
                investigation_core::PROGRAM_NAME.to_owned(),
                "migrate".to_owned(),
            ];
            if apply {
                argv.push("--apply".to_owned());
            }
            argv.extend(["--format".to_owned(), "json".to_owned()]);
            argv
        };
        if let Some(migrating) = &self.migrating {
            return Some(FormatRefusal {
                code: "MIGRATION_INCOMPLETE",
                message: format!(
                    "A migration of this workspace from format {} to {} started and did not \
                     finish, so every write is refused until it does. Nothing was changed. Run \
                     migrate to see the plan and finish it with migrate --apply --plan {}, or \
                     restore the backup taken before it started (export {}).",
                    migrating.from, migrating.to, migrating.plan_id, migrating.backup_export_id
                ),
                next: Some(migrate(false)),
            });
        }
        if self.write_requires > supported {
            return Some(FormatRefusal {
                code: "WORKSPACE_FORMAT_NEWER",
                message: format!(
                    "This workspace is at format {} and a newer program wrote it: writing it \
                     needs a program that understands format {} or later, and this one \
                     understands up to format {supported}. Reads still work. Nothing was \
                     changed. Use the newer program to write here.",
                    self.version, self.write_requires
                ),
                next: None,
            });
        }
        if let Some(to) = required {
            return Some(FormatRefusal {
                code: "MIGRATION_REQUIRED",
                message: format!(
                    "This workspace is at format {} and this program writes only format {to} or \
                     later. Reads still work. Nothing was changed. Run migrate to see the plan, \
                     then migrate --apply with its plan ID.",
                    self.version
                ),
                next: Some(migrate(false)),
            });
        }
        None
    }
}

/// Read the `format` section of a parsed marker leniently, before the
/// strict decode. Absent means [`WorkspaceFormat::BASELINE`].
pub(super) fn peek(marker: &Value) -> Result<WorkspaceFormat, WorkspaceError> {
    let Some(section) = marker.get("format") else {
        return Ok(WorkspaceFormat::BASELINE);
    };
    let lenient: LenientFormat = serde_json::from_value(section.clone())
        .map_err(|error| WorkspaceError::InvalidGenesis(format!("format: {error}")))?;
    Ok(WorkspaceFormat {
        version: lenient.version,
        read_requires: lenient.read_requires,
        write_requires: lenient.write_requires,
        migrating: lenient.migrating.map(|migrating| Migrating {
            from: migrating.from,
            to: migrating.to,
            plan_id: migrating.plan_id,
            backup_export_id: migrating.backup_export_id,
        }),
    })
}

/// The marker as this program decodes it, given its peeked format: a
/// marker this program may not write is read leniently, without the fields
/// a newer program added, and with the section as peeked.
pub(super) fn readable(mut marker: Value, format: &WorkspaceFormat) -> Value {
    if format.write_requires > FORMAT_VERSION
        && let Value::Object(fields) = &mut marker
    {
        fields.retain(|name, _| KNOWN_FIELDS.contains(&name.as_str()));
        fields.insert(
            "format".to_owned(),
            serde_json::to_value(format).expect("a format serializes"),
        );
    }
    marker
}

impl Workspace {
    /// The workspace's format section, [`WorkspaceFormat::BASELINE`] when
    /// the marker has none.
    pub fn format(&self) -> &WorkspaceFormat {
        &self.format
    }

    /// Read the format from the marker on disk now, not as it was when this
    /// workspace was opened. A commit calls it under the coordination lock.
    pub fn current_format(&self) -> Result<WorkspaceFormat, WorkspaceError> {
        let path = self.root.join(GENESIS_FILE);
        let bytes = fs::read(&path).map_err(|source| io_at(&path, source))?;
        let value = investigation_core::input::parse_request(&bytes)
            .map_err(|error| WorkspaceError::InvalidGenesis(error.to_string()))?;
        let format = peek(&value)?;
        format.validate()?;
        Ok(format)
    }

    /// Replace the marker's format section with `next`, keeping every other
    /// field. The new marker is written and synced under a private name and
    /// renamed over the old one, so a kill leaves the old marker or the new
    /// one, never a mix. Refused unless the marker on disk still holds
    /// `expected`. The caller holds the coordination lock. The workspace
    /// ID, its demo and fork sections, and every journal byte are kept.
    pub(crate) fn replace_format(
        &self,
        expected: &WorkspaceFormat,
        next: WorkspaceFormat,
    ) -> Result<Self, WorkspaceError> {
        next.validate()?;
        let current = Self::open(&self.root)?;
        if &current.format != expected {
            return Err(WorkspaceError::InvalidGenesis(format!(
                "format: the marker changed while the migration ran (expected {expected:?}, found {:?})",
                current.format
            )));
        }
        let marker = WorkspaceGenesis {
            schema_version: super::WORKSPACE_VERSION,
            schema_family: super::RECORD_FAMILY,
            workspace_id: current.id,
            genesis_sequence: current.genesis_sequence,
            demo: current.demo,
            fork: current.fork,
            format: (next != WorkspaceFormat::BASELINE).then_some(next),
        };
        let mut bytes = serde_json::to_vec_pretty(&marker)
            .map_err(|error| WorkspaceError::Serialization(error.to_string()))?;
        bytes.push(b'\n');
        let stage = self.root.join(STAGED_MARKER);
        // Only this function writes the staged name; a leftover is from a
        // run that was killed before its rename.
        match fs::remove_file(&stage) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_at(&stage, error)),
        }
        super::write_new_file(&stage, &bytes)?;
        let target = self.root.join(GENESIS_FILE);
        fs::rename(&stage, &target).map_err(|source| io_at(&target, source))?;
        sync_directory(&self.state_dir)?;
        Self::open(&self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(version: u32, read: u32, write: u32) -> WorkspaceFormat {
        WorkspaceFormat {
            version,
            read_requires: read,
            write_requires: write,
            migrating: None,
        }
    }

    #[test]
    fn the_numbers_must_be_ordered() {
        assert!(WorkspaceFormat::BASELINE.validate().is_ok());
        assert!(format(3, 1, 2).validate().is_ok());
        assert!(format(0, 0, 0).validate().is_err());
        assert!(format(2, 2, 1).validate().is_err());
        assert!(format(2, 1, 3).validate().is_err());
        let mut migrating = format(1, 1, 2);
        migrating.migrating = Some(Migrating {
            from: 1,
            to: 2,
            plan_id: "plan".into(),
            backup_export_id: "exp_1".into(),
        });
        assert!(migrating.validate().is_ok());
        migrating.migrating.as_mut().unwrap().from = 2;
        assert!(migrating.validate().is_err());
        migrating.migrating.as_mut().unwrap().from = 1;
        migrating.migrating.as_mut().unwrap().plan_id = String::new();
        assert!(migrating.validate().is_err());
    }

    #[test]
    fn status_and_refusals_follow_the_numbers() {
        assert_eq!(
            WorkspaceFormat::BASELINE.status(1, 1),
            FormatStatus::Current
        );
        assert_eq!(
            format(1, 1, 1).status(2, 2),
            FormatStatus::Older {
                version: 1,
                latest: 2
            }
        );
        assert_eq!(
            format(3, 1, 1).status(1, 1),
            FormatStatus::NewerCompatible { version: 3 }
        );
        assert_eq!(
            format(3, 1, 2).status(1, 1),
            FormatStatus::WritesRefused {
                version: 3,
                write_requires: 2
            }
        );
        assert_eq!(format(3, 1, 1).write_refusal(1, None), None);
        let refusal = format(3, 1, 2).write_refusal(1, None).unwrap();
        assert_eq!(refusal.code, "WORKSPACE_FORMAT_NEWER");
        assert!(refusal.message.contains("format 2") && refusal.message.contains("up to format 1"));
        assert_eq!(
            format(1, 1, 1).write_refusal(2, Some(2)).unwrap().code,
            "MIGRATION_REQUIRED"
        );
    }

    #[test]
    fn a_newer_marker_is_read_without_the_fields_it_added() {
        let marker = serde_json::json!({
            "schema_version": "inv.workspace.v1",
            "later": {"anything": true},
            "format": {"version": 4, "read_requires": 1, "write_requires": 3, "later": 1},
        });
        let peeked = peek(&marker).unwrap();
        assert_eq!(peeked, format(4, 1, 3));
        let readable = readable(marker, &peeked);
        assert!(readable.get("later").is_none());
        assert_eq!(
            readable["format"],
            serde_json::json!({"version": 4, "read_requires": 1, "write_requires": 3})
        );
    }
}
