//! Snapshots of person-owned vault files a record relies on (plan §12.1,
//! W11.14).
//!
//! Human notes and boards live outside the journal and change whenever a
//! person edits them. When a record relies on one, the exact bytes are
//! retained as an Artifact through the ordinary ingest commit, and an
//! `annotation.add` with artifact content cites that Artifact and its digest.
//! The optional `human_file` property of that annotation says which
//! person-owned file the bytes were read from, so readers can compare the live
//! file with the captured bytes and exports can show the captured version
//! with its hash and role. No new record kind or ID prefix is involved.
//!
//! The annotation is authored text with no evidentiary standing, like every
//! annotation; capturing a note fixes what was relied on, it does not make
//! the note evidence.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    command::RevisionRef,
    derivation::artifact_body,
    header::ClassificationLevel,
    identity::Kind,
    state::{Record, State},
    wire::Digest,
};

/// The vault folders that belong to people, in the order they are read.
/// A snapshot's path must start with one of them.
pub const HUMAN_FOLDERS: [&str; 3] = ["Notes", "Boards", "Inbox"];

/// Longest recorded vault-relative path, in UTF-8 bytes.
pub const MAX_PATH_BYTES: usize = 1024;

/// Deepest recorded vault-relative path, in components.
pub const MAX_PATH_COMPONENTS: usize = 32;

/// The `human_file` property of `annotation.add`: the person-owned vault
/// file whose bytes the annotation's artifact content captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HumanFileInput {
    /// Vault-relative, `/`-separated, below `Notes/`, `Boards/` or `Inbox/`
    /// (inside the managed prefix in existing-vault mode), for example
    /// `Notes/plan.md`.
    pub path: String,
}

/// What the annotation body records under `human_file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanFile {
    pub path: String,
    /// The first component of `path`: `Notes`, `Boards` or `Inbox`.
    pub folder: String,
    /// Always `human`: the file belongs to a person, not to the renderer.
    pub owner: String,
}

impl HumanFile {
    /// The body value for a validated `path`.
    pub fn new(path: &str) -> Result<Self, &'static str> {
        let folder = folder(path)?;
        Ok(Self {
            path: path.to_owned(),
            folder: folder.to_owned(),
            owner: "human".to_owned(),
        })
    }
}

/// The provenance role a snapshot of a file in `folder` plays in an export.
pub fn role(folder: &str) -> &'static str {
    match folder {
        "Boards" => "human_board_snapshot",
        "Inbox" => "inbox_draft_snapshot",
        _ => "human_note_snapshot",
    }
}

/// The person-owned folder `path` lies in, when it is a plain vault-relative
/// path below one: no absolute, empty, `.`, `..` or hidden component, no
/// backslash or control character, and within the length bounds.
pub fn folder(path: &str) -> Result<&'static str, &'static str> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return Err("it is empty or longer than 1024 bytes");
    }
    if path
        .chars()
        .any(|character| character == '\\' || character.is_control())
    {
        return Err("it contains a backslash or a control character");
    }
    let components: Vec<&str> = path.split('/').collect();
    if components.len() > MAX_PATH_COMPONENTS {
        return Err("it is more than 32 folders deep");
    }
    if components
        .iter()
        .any(|component| component.is_empty() || component.starts_with('.'))
    {
        return Err(
            "it is not a plain vault-relative path (no absolute, hidden, empty or `..` parts)",
        );
    }
    let Some(folder) = HUMAN_FOLDERS
        .iter()
        .find(|folder| **folder == components[0])
    else {
        return Err("it is outside the person-owned folders Notes/, Boards/ and Inbox/");
    };
    if components.len() < 2 {
        return Err("it names a folder, not a file inside it");
    }
    Ok(folder)
}

/// One recorded snapshot of a person-owned file: an annotation with a
/// `human_file` and retained artifact content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteSnapshot {
    /// The annotation that records the reliance.
    pub annotation: RevisionRef,
    pub subjects: Vec<RevisionRef>,
    pub file: HumanFile,
    /// The exact Artifact revision holding the captured bytes.
    pub artifact: RevisionRef,
    pub sha256: Digest,
    /// Recorded size of the captured bytes, when the Artifact resolves.
    pub bytes: Option<u64>,
    pub classification: ClassificationLevel,
}

impl NoteSnapshot {
    /// The annotation, when `record` is a snapshot of a person-owned file.
    pub fn of(state: &State, record: &Record) -> Option<Self> {
        if record.header.id.kind() != Kind::Annotation {
            return None;
        }
        let file: HumanFile =
            serde_json::from_value(record.body.get("human_file")?.clone()).ok()?;
        let content = record.body.get("content")?;
        if content.get("kind").and_then(Value::as_str) != Some("artifact") {
            return None;
        }
        let artifact: RevisionRef =
            serde_json::from_value(content.get("artifact_ref")?.clone()).ok()?;
        let sha256: Digest = serde_json::from_value(content.get("digest")?.clone()).ok()?;
        let subjects = record
            .body
            .get("subject_refs")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        Some(Self {
            annotation: record.reference(),
            subjects,
            file,
            bytes: artifact_body(state, artifact).and_then(|body| body.size_bytes),
            artifact,
            sha256,
            classification: record.header.classification.level,
        })
    }

    /// Every current snapshot annotation of `state`, in annotation ID order.
    pub fn all(state: &State) -> Vec<Self> {
        state
            .records_of_kind(Kind::Annotation)
            .filter_map(|record| Self::of(state, record))
            .collect()
    }

    /// See [`role`].
    pub fn role(&self) -> &'static str {
        role(&self.file.folder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_paths_below_person_owned_folders_are_accepted() {
        for (good, expected) in [
            ("Notes/plan.md", "Notes"),
            ("Boards/a/b/map.canvas", "Boards"),
            ("Inbox/draft one.md", "Inbox"),
        ] {
            assert_eq!(folder(good), Ok(expected), "{good}");
        }
        for bad in [
            "",
            "Notes",
            "Notes/",
            "/Notes/plan.md",
            "notes/plan.md",
            "Cases/x/Overview.md",
            "Start Here.md",
            "Notes/../Cases/x.md",
            "Notes/./plan.md",
            "Notes//plan.md",
            "Notes/.hidden.md",
            "Notes\\plan.md",
            "Notes/plan\n.md",
            "../Notes/plan.md",
            ".obsidian/app.json",
        ] {
            assert!(folder(bad).is_err(), "{bad:?}");
        }
        let deep = format!("Notes/{}x.md", "a/".repeat(MAX_PATH_COMPONENTS));
        assert!(folder(&deep).is_err());
        let long = format!("Notes/{}.md", "a".repeat(MAX_PATH_BYTES));
        assert!(folder(&long).is_err());
    }

    #[test]
    fn roles_follow_the_folder() {
        assert_eq!(role("Notes"), "human_note_snapshot");
        assert_eq!(role("Boards"), "human_board_snapshot");
        assert_eq!(role("Inbox"), "inbox_draft_snapshot");
        assert_eq!(HumanFile::new("Boards/m.canvas").unwrap().owner, "human");
    }
}
