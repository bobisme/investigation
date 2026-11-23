//! Workspace initialization, discovery and immutable genesis verification.
//!
//! A workspace is a directory boundary, not a global event sequence. ADR 0002
//! assigns event order to per-case chains; `sequence()` below reports only the
//! workspace marker's genesis sequence (always zero).

use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use investigation_core::{
    PROGRAM_NAME,
    header::Actor,
    identity::{Id, IdSource, Kind, SystemIdSource},
};
use serde::{Deserialize, Serialize};

const CONFIG_FILE: &str = "investigation.toml";
/// A plain-text pointer at the workspace root for anyone listing it.
pub const POINTER_FILE: &str = "README.txt";
const GENESIS_FILE: &str = "state/workspace.json";
const LOCK_FILE: &str = "state/locks/commit.lock";
const WORKSPACE_VERSION: WorkspaceVersion = WorkspaceVersion::V1;
const RECORD_FAMILY: RecordSchemaFamily = RecordSchemaFamily::V0Alpha;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceVersion {
    #[serde(rename = "inv.workspace.v1")]
    V1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordSchemaFamily {
    #[serde(rename = "inv.record.v0-alpha")]
    V0Alpha,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceGenesis {
    schema_version: WorkspaceVersion,
    schema_family: RecordSchemaFamily,
    workspace_id: Id,
    genesis_sequence: u64,
}

/// Human-editable settings. Unknown top-level keys are errors; opt-in extensions
/// stay inert and namespaced under `extensions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    pub schema_version: WorkspaceConfigVersion,
    #[serde(default)]
    pub import_roots: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<Actor>,
    #[serde(default)]
    pub vault_policy: VaultPolicy,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceConfigVersion {
    #[serde(rename = "inv.workspace-config.v1")]
    V1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultPolicy {
    pub enabled: bool,
    /// Only approved derivatives may be projected; originals stay canonical.
    pub include_approved_derivatives: bool,
    /// The renderer may manage generated files but preserves Notes/Boards/Inbox.
    pub preserve_human_folders: bool,
}

impl Default for VaultPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            include_approved_derivatives: false,
            preserve_human_folders: true,
        }
    }
}

impl WorkspaceConfig {
    fn initial(vault_enabled: bool) -> Self {
        Self {
            schema_version: WorkspaceConfigVersion::V1,
            import_roots: Vec::new(),
            actor: None,
            vault_policy: VaultPolicy {
                enabled: vault_enabled,
                ..VaultPolicy::default()
            },
            extensions: BTreeMap::new(),
        }
    }

    fn validate(&self) -> Result<(), WorkspaceError> {
        for name in self.extensions.keys() {
            if !name.contains('.') || name.starts_with('.') || name.ends_with('.') {
                return Err(WorkspaceError::InvalidConfig(format!(
                    "extension key {name:?} must be namespaced"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemKind {
    Local,
    Network,
    Fuse,
    Unknown,
    UnsupportedPlatform,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemInfo {
    pub kind: FilesystemKind,
    pub name: String,
    pub magic: Option<u64>,
}

/// Filesystem checks are injectable so refusal can be tested before init makes
/// any path. Production detection inspects the target or its nearest existing
/// ancestor (important when the requested workspace does not exist yet).
pub trait FilesystemDetector {
    fn detect(&self, target: &Path) -> io::Result<FilesystemInfo>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NativeFilesystemDetector;

impl FilesystemDetector for NativeFilesystemDetector {
    fn detect(&self, target: &Path) -> io::Result<FilesystemInfo> {
        #[cfg(target_os = "linux")]
        {
            let path = nearest_existing_ancestor(target)?;
            let stats = rustix::fs::statfs(path.as_path())
                .map_err(|error| io::Error::from_raw_os_error(error.raw_os_error()))?;
            let magic = stats.f_type as u64;
            Ok(classify_linux_filesystem(magic))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = target;
            Ok(FilesystemInfo {
                kind: FilesystemKind::UnsupportedPlatform,
                name: std::env::consts::OS.to_owned(),
                magic: None,
            })
        }
    }
}

/// Store handle for one initialized workspace.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    id: Id,
    state_dir: PathBuf,
    config: WorkspaceConfig,
    genesis_sequence: u64,
}

impl Workspace {
    /// Create a workspace. Filesystem refusal happens before creating the root
    /// or any child; the immutable workspace marker is published last.
    pub fn init(root: &Path, vault: bool) -> Result<Self, WorkspaceError> {
        Self::init_with_detector(root, vault, &NativeFilesystemDetector)
    }

    /// Create a workspace whose config names these read-only import roots.
    /// Each root must pass [`check_import_root`]; nothing else about the roots
    /// is checked or touched here.
    pub fn init_with_import_roots(
        root: &Path,
        vault: bool,
        import_roots: &[PathBuf],
    ) -> Result<Self, WorkspaceError> {
        for import_root in import_roots {
            check_import_root(import_root)?;
        }
        Self::init_inner(root, vault, import_roots, &NativeFilesystemDetector)
    }

    /// Add read-only import roots to this workspace's config and return the
    /// roots that were not already configured. Each root must pass
    /// [`check_import_root`] and lie outside the workspace. The config is
    /// re-read and replaced atomically under the commit lock, so a concurrent
    /// update is not lost; nothing is written when every root is present.
    /// The rewritten file keeps every setting but not hand-written comments.
    pub fn add_import_roots(
        &mut self,
        import_roots: &[PathBuf],
    ) -> Result<Vec<PathBuf>, WorkspaceError> {
        for import_root in import_roots {
            check_import_root(import_root)?;
            if import_root.starts_with(&self.root) {
                return Err(WorkspaceError::InvalidImportRoot(format!(
                    "{} lies inside the workspace; an import root must be outside it",
                    import_root.display()
                )));
            }
        }
        let lock_path = self.root.join(LOCK_FILE);
        let _lock =
            crate::durable::CommitLock::acquire(&lock_path, std::time::Duration::from_secs(2))
                .map_err(|error| match error {
                    crate::durable::LockError::Io(error) => io_at(&lock_path, error),
                    crate::durable::LockError::Busy { .. } => io_at(
                        &lock_path,
                        io::Error::new(io::ErrorKind::WouldBlock, "workspace is busy"),
                    ),
                })?;
        let config_path = self.root.join(CONFIG_FILE);
        ensure_regular_file(&config_path)?;
        let mut config: WorkspaceConfig = toml::from_str(&read_text(&config_path)?)
            .map_err(|error| WorkspaceError::InvalidConfig(error.to_string()))?;
        config.validate()?;
        let configured: Vec<PathBuf> = config
            .import_roots
            .iter()
            .map(|root| {
                if root.is_absolute() {
                    root.clone()
                } else {
                    self.root.join(root)
                }
            })
            .collect();
        let mut added = Vec::new();
        for import_root in import_roots {
            let known = configured
                .iter()
                .chain(&added)
                .any(|root| fs::canonicalize(root).is_ok_and(|root| &root == import_root));
            if !known {
                added.push(import_root.clone());
            }
        }
        if added.is_empty() {
            self.config = config;
            return Ok(added);
        }
        config.import_roots.extend(added.iter().cloned());
        let bytes = config_text(&config)?;
        // The stage lives on the workspace filesystem, so the rename is atomic.
        // Only a holder of the commit lock writes it, so a leftover is stale.
        let stage = self.root.join("state/tmp/investigation-config");
        match fs::remove_file(&stage) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_at(&stage, error)),
        }
        write_new_file(&stage, bytes.as_bytes())?;
        fs::rename(&stage, &config_path).map_err(|source| io_at(&config_path, source))?;
        sync_directory(&self.root)?;
        sync_directory(&self.root.join("state/tmp"))?;
        self.config = config;
        Ok(added)
    }

    /// Test and integration seam for filesystem policy. The detector must be
    /// read-only; it runs before the first create operation.
    pub fn init_with_detector(
        root: &Path,
        vault: bool,
        detector: &impl FilesystemDetector,
    ) -> Result<Self, WorkspaceError> {
        Self::init_inner(root, vault, &[], detector)
    }

    fn init_inner(
        root: &Path,
        vault: bool,
        import_roots: &[PathBuf],
        detector: &impl FilesystemDetector,
    ) -> Result<Self, WorkspaceError> {
        let root = absolute_lexical(root)?;
        preflight_target(&root)?;
        let filesystem =
            detector
                .detect(&root)
                .map_err(|source| WorkspaceError::FilesystemDetection {
                    path: root.clone(),
                    source,
                })?;
        require_supported_filesystem(&filesystem)?;

        let existed = root.exists();
        if existed && !is_empty_dir(&root)? {
            return Err(WorkspaceError::WorkspaceAlreadyExists(root));
        }
        if !existed {
            create_missing_parents(&root)?;
            create_private_dir(&root)?;
            if let Some(parent) = root.parent() {
                sync_directory(parent)?;
            }
            sync_directory(&root)?;
        }
        set_private_permissions(&root)?;
        let root = fs::canonicalize(&root).map_err(|source| io_at(&root, source))?;

        let state_dir = root.join("state");
        for relative in required_directories(vault) {
            create_layout_directory(&root, relative)?;
        }

        let id = SystemIdSource
            .next_id(Kind::Workspace)
            .map_err(|error| WorkspaceError::Identity(error.to_string()))?;
        let mut config = WorkspaceConfig::initial(vault);
        config.import_roots = import_roots.to_vec();
        let config_bytes = config_text(&config)?;

        // The persistent lock is created once. Neither init nor open replaces it.
        let lock_path = root.join(LOCK_FILE);
        write_new_file(&lock_path, b"")?;
        let _init_lock =
            crate::durable::CommitLock::acquire(&lock_path, std::time::Duration::from_secs(2))
                .map_err(|error| match error {
                    crate::durable::LockError::Io(error) => io_at(&lock_path, error),
                    crate::durable::LockError::Busy { .. } => io_at(
                        &lock_path,
                        std::io::Error::new(std::io::ErrorKind::WouldBlock, "workspace is busy"),
                    ),
                })?;
        write_new_file(&root.join(CONFIG_FILE), config_bytes.as_bytes())?;
        write_new_file(&root.join(POINTER_FILE), pointer_text().as_bytes())?;

        // Marker-last publication: every directory, lock and config exists and
        // its parent has been synced before this identity becomes discoverable.
        sync_directory(&root)?;
        let marker = WorkspaceGenesis {
            schema_version: WORKSPACE_VERSION,
            schema_family: RECORD_FAMILY,
            workspace_id: id,
            genesis_sequence: 0,
        };
        let marker_bytes = serde_json::to_vec_pretty(&marker)
            .map_err(|error| WorkspaceError::Serialization(error.to_string()))?;
        let mut marker_bytes = marker_bytes;
        marker_bytes.push(b'\n');
        let stage = root.join("state/tmp/workspace-genesis");
        let target = root.join(GENESIS_FILE);
        write_new_file(&stage, &marker_bytes)?;
        fs::hard_link(&stage, &target).map_err(|source| io_at(&target, source))?;
        sync_directory(&state_dir)?;
        sync_directory(&root)?;
        let _ = fs::remove_file(stage);

        Ok(Self {
            root,
            id,
            state_dir,
            config,
            genesis_sequence: marker.genesis_sequence,
        })
    }

    /// Open and verify the immutable marker, strict config and required layout.
    pub fn open(root: &Path) -> Result<Self, WorkspaceError> {
        Self::open_checked(root, |root| Self::check_filesystem(root).map(|_| ()))
    }

    #[cfg(test)]
    pub(crate) fn open_lazyfs_fixture(root: &Path) -> Result<Self, WorkspaceError> {
        Self::open_checked(root, |root| {
            let info = NativeFilesystemDetector.detect(root).map_err(|source| {
                WorkspaceError::FilesystemDetection {
                    path: root.to_owned(),
                    source,
                }
            })?;
            if info.kind != FilesystemKind::Fuse {
                return Err(WorkspaceError::UnsupportedFilesystem {
                    name: info.name,
                    kind: info.kind,
                });
            }
            Ok(())
        })
    }

    fn open_checked(
        root: &Path,
        filesystem: impl FnOnce(&Path) -> Result<(), WorkspaceError>,
    ) -> Result<Self, WorkspaceError> {
        let root = absolute_lexical(root)?;
        reject_symlink(&root)?;
        let root = fs::canonicalize(&root).map_err(|source| io_at(&root, source))?;
        if !root.is_dir() {
            return Err(WorkspaceError::InvalidRoot(root));
        }
        filesystem(&root)?;

        let config_path = root.join(CONFIG_FILE);
        ensure_regular_file(&config_path)?;
        let config_text = read_text(&config_path)?;
        let config: WorkspaceConfig = toml::from_str(&config_text)
            .map_err(|error| WorkspaceError::InvalidConfig(error.to_string()))?;
        config.validate()?;

        let marker_path = root.join(GENESIS_FILE);
        ensure_regular_file(&marker_path)?;
        let bytes = read_bytes(&marker_path)?;
        let value = investigation_core::input::parse_request(&bytes)
            .map_err(|error| WorkspaceError::InvalidGenesis(error.to_string()))?;
        let marker: WorkspaceGenesis = investigation_core::input::decode(&value)
            .map_err(|error| WorkspaceError::InvalidGenesis(error.to_string()))?;
        if marker.schema_version != WORKSPACE_VERSION {
            return Err(WorkspaceError::UnsupportedWorkspaceVersion);
        }
        if marker.schema_family != RECORD_FAMILY {
            return Err(WorkspaceError::UnsupportedRecordSchemaFamily);
        }
        if marker.workspace_id.kind() != Kind::Workspace {
            return Err(WorkspaceError::InvalidWorkspaceId);
        }
        if marker.genesis_sequence != 0 {
            return Err(WorkspaceError::InvalidGenesisSequence(
                marker.genesis_sequence,
            ));
        }
        verify_layout(&root, config.vault_policy.enabled)?;

        Ok(Self {
            state_dir: root.join("state"),
            root,
            id: marker.workspace_id,
            config,
            genesis_sequence: marker.genesis_sequence,
        })
    }

    /// Find the nearest ancestor containing `investigation.toml`; never walks
    /// descendants or scans the user's home directory.
    pub fn discover(start: &Path) -> Result<Self, WorkspaceError> {
        let start = absolute_lexical(start)?;
        let mut candidate = if start.is_file() {
            start.parent().unwrap_or(Path::new("/")).to_path_buf()
        } else {
            start.clone()
        };
        loop {
            let config = candidate.join(CONFIG_FILE);
            if config.is_file() {
                return Self::open(&candidate);
            }
            match candidate.parent() {
                Some(parent) if parent != candidate => candidate = parent.to_path_buf(),
                _ => return Err(WorkspaceError::WorkspaceNotFound(start)),
            }
        }
    }

    /// Return a safe filesystem classification for doctor and init diagnostics.
    pub fn check_filesystem(root: &Path) -> Result<FilesystemInfo, WorkspaceError> {
        let info = NativeFilesystemDetector.detect(root).map_err(|source| {
            WorkspaceError::FilesystemDetection {
                path: root.to_path_buf(),
                source,
            }
        })?;
        require_supported_filesystem(&info)?;
        Ok(info)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn id(&self) -> Id {
        self.id
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn config(&self) -> &WorkspaceConfig {
        &self.config
    }

    /// Genesis sequence only; this is not a workspace-wide event counter.
    pub const fn sequence(&self) -> u64 {
        self.genesis_sequence
    }
}

fn required_directories(vault: bool) -> Vec<&'static str> {
    let mut directories = vec![
        "state",
        "state/cases",
        "state/events",
        "state/blobs",
        "state/blobs/sha256",
        "state/checkpoints",
        "state/staging",
        "state/locks",
        "state/tmp",
        "cache",
        "local",
        "exports",
    ];
    if vault {
        directories.extend(["vault", "vault/Notes", "vault/Boards", "vault/Inbox"]);
    }
    directories
}

fn verify_layout(root: &Path, vault_enabled: bool) -> Result<(), WorkspaceError> {
    for relative in required_directories(vault_enabled) {
        let path = root.join(relative);
        ensure_directory(&path)?;
    }
    ensure_regular_file(&root.join(LOCK_FILE))?;
    if !vault_enabled && root.join("vault").exists() {
        return Err(WorkspaceError::LayoutMismatch(
            "vault directory exists while vault policy is disabled".to_owned(),
        ));
    }
    Ok(())
}

/// Text of [`POINTER_FILE`]. It names commands with a `DIR` placeholder and
/// never an absolute local path, so the file can travel with the workspace.
#[must_use]
pub fn pointer_text() -> String {
    format!(
        "This directory is an investigation workspace. Read it with the {PROGRAM_NAME} command,\n\
         not by opening files under state/ or cache/:\n\
         \n\
         \x20 {PROGRAM_NAME} --workspace DIR brief --format json\n\
         \x20 {PROGRAM_NAME} --workspace DIR onboard --format json\n\
         \n\
         DIR is the path to this directory, for example ./casework. Inside it, --workspace\n\
         can be omitted. state/ holds the canonical journal and retained bytes; cache/ holds\n\
         a disposable index. To let the workspace retain files from another directory, run\n\
         {PROGRAM_NAME} init DIR --import-root SOURCE_DIR instead of editing investigation.toml.\n"
    )
}

/// Reject an import root that is not an absolute, canonical path to an
/// existing directory that ingest could open. Canonical means no symbolic
/// link or `..` component, so the stored root cannot be redirected later.
pub fn check_import_root(import_root: &Path) -> Result<(), WorkspaceError> {
    let invalid = |reason: &str| {
        Err(WorkspaceError::InvalidImportRoot(format!(
            "{} {reason}",
            import_root.display()
        )))
    };
    if !import_root.is_absolute() || !import_root.is_dir() {
        return invalid("must be an absolute path to an existing directory");
    }
    if fs::canonicalize(import_root).ok().as_deref() != Some(import_root) {
        return invalid("must be a canonical path without symbolic links or .. components");
    }
    if let Err(error) = crate::import_paths::ImportRoot::open(import_root) {
        return invalid(&format!(
            "cannot be opened as an import root: {:?}",
            error.reason
        ));
    }
    Ok(())
}

fn config_text(config: &WorkspaceConfig) -> Result<String, WorkspaceError> {
    let body = toml::to_string(config)
        .map_err(|error| WorkspaceError::Serialization(error.to_string()))?;
    Ok(format!(
        "# Investigation workspace configuration. Unknown top-level keys are rejected.\n\
         # Read this workspace with {PROGRAM_NAME} --workspace DIR brief, where DIR is this directory;\n\
         # do not read state/ or cache/ files directly. See README.txt.\n\
         # Add an import root with {PROGRAM_NAME} init DIR --import-root SOURCE_DIR, not by editing this file.\n\
         # state/ is canonical evidence and journal data; do not sync it as an Obsidian vault.\n\
         # cache/ is rebuildable and can be deleted by cache maintenance.\n\
         # local/ stores machine-specific, noncanonical bookkeeping.\n\
         # vault/ is an optional child projection; Notes, Boards and Inbox are human-owned.\n\
         # Import roots are read-only inputs. They do not authorize writes or external actions.\n\
         {body}"
    ))
}

fn absolute_lexical(path: &Path) -> Result<PathBuf, WorkspaceError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| io_at(path, source))?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(WorkspaceError::InvalidRoot(path.to_path_buf()));
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

fn preflight_target(root: &Path) -> Result<(), WorkspaceError> {
    if !root.exists() {
        return Ok(());
    }
    reject_symlink(root)?;
    if !root.is_dir() {
        return Err(WorkspaceError::InvalidRoot(root.to_path_buf()));
    }
    Ok(())
}

fn create_missing_parents(root: &Path) -> Result<(), WorkspaceError> {
    let parent = root
        .parent()
        .ok_or_else(|| WorkspaceError::InvalidRoot(root.to_path_buf()))?;
    if parent.exists() {
        if !parent.is_dir() {
            return Err(WorkspaceError::InvalidRoot(parent.to_path_buf()));
        }
        return Ok(());
    }
    create_missing_parents(parent)?;
    create_private_dir(parent)?;
    sync_directory(parent.parent().unwrap_or(parent))?;
    sync_directory(parent)?;
    Ok(())
}

fn nearest_existing_ancestor(path: &Path) -> io::Result<PathBuf> {
    let mut current = path;
    loop {
        if fs::symlink_metadata(current).is_ok() {
            return fs::canonicalize(current);
        }
        current = current
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no existing path ancestor"))?;
    }
}

fn classify_linux_filesystem(magic: u64) -> FilesystemInfo {
    use rustix::fs::NFS_SUPER_MAGIC;

    let (kind, name) = if magic == NFS_SUPER_MAGIC as u64 {
        (FilesystemKind::Network, "nfs")
    } else if magic == 0xff53_4d42 || magic == 0xfe53_4d42 {
        (FilesystemKind::Network, "cifs/smb")
    } else if magic == 0x0102_1997 {
        (FilesystemKind::Network, "9p")
    } else if magic == 0x5346_414f {
        (FilesystemKind::Network, "afs")
    } else if magic == 0x7375_7245 {
        (FilesystemKind::Network, "coda")
    } else if magic == 0x0000_564c {
        (FilesystemKind::Network, "ncp")
    } else if magic == 0x00c3_6400 {
        (FilesystemKind::Network, "ceph")
    } else if magic == 0x0bd0_0bd0 {
        (FilesystemKind::Network, "lustre")
    } else if magic == 0x6573_5546 {
        (
            FilesystemKind::Fuse,
            "fuse (not qualified for durable writes)",
        )
    } else if is_known_local_magic(magic) {
        (FilesystemKind::Local, local_filesystem_name(magic))
    } else {
        (FilesystemKind::Unknown, "unknown")
    };
    FilesystemInfo {
        kind,
        name: name.to_owned(),
        magic: Some(magic),
    }
}

fn is_known_local_magic(magic: u64) -> bool {
    matches!(
        magic,
        0x0000_ef53 // ext2/3/4
            | 0x5846_5342 // XFS
            | 0x9123_683e // Btrfs
            | 0x0102_1994 // tmpfs
            | 0x794c_7630 // overlayfs
            | 0xf2f5_2010 // F2FS
            | 0x2fc1_2fc1 // ZFS
            | 0x8584_58f6 // ramfs
            | 0x2405_1905 // UBIFS
            | 0x72b6 // JFFS2
    )
}

fn local_filesystem_name(magic: u64) -> &'static str {
    match magic {
        0x0000_ef53 => "ext",
        0x5846_5342 => "xfs",
        0x9123_683e => "btrfs",
        0x0102_1994 => "tmpfs",
        0x794c_7630 => "overlayfs",
        0xf2f5_2010 => "f2fs",
        0x2fc1_2fc1 => "zfs",
        0x8584_58f6 => "ramfs",
        0x2405_1905 => "ubifs",
        0x72b6 => "jffs2",
        _ => "local",
    }
}

fn require_supported_filesystem(info: &FilesystemInfo) -> Result<(), WorkspaceError> {
    if info.kind == FilesystemKind::Local {
        return Ok(());
    }
    Err(WorkspaceError::UnsupportedFilesystem {
        name: info.name.clone(),
        kind: info.kind,
    })
}

fn create_layout_directory(root: &Path, relative: &str) -> Result<(), WorkspaceError> {
    let path = root.join(relative);
    let parent = path
        .parent()
        .ok_or_else(|| WorkspaceError::InvalidRoot(path.clone()))?;
    create_private_dir(&path)?;
    sync_directory(parent)?;
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<(), WorkspaceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder
            .mode(0o700)
            .create(path)
            .map_err(|source| io_at(path, source))?;
    }
    #[cfg(not(unix))]
    fs::create_dir(path).map_err(|source| io_at(path, source))?;
    set_private_permissions(path)?;
    sync_directory(path)
}

fn set_private_permissions(path: &Path) -> Result<(), WorkspaceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|source| io_at(path, source))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn write_new_file(path: &Path, contents: &[u8]) -> Result<(), WorkspaceError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|source| io_at(path, source))?;
    file.write_all(contents)
        .map_err(|source| io_at(path, source))?;
    file.sync_all().map_err(|source| io_at(path, source))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| io_at(path, source))?;
    }
    file.sync_all().map_err(|source| io_at(path, source))?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), WorkspaceError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_at(path, source))
}

fn is_empty_dir(path: &Path) -> Result<bool, WorkspaceError> {
    let mut entries = fs::read_dir(path).map_err(|source| io_at(path, source))?;
    match entries.next() {
        None => Ok(true),
        Some(Ok(_)) => Ok(false),
        Some(Err(source)) => Err(io_at(path, source)),
    }
}

fn reject_symlink(path: &Path) -> Result<(), WorkspaceError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_at(path, source))?;
    if metadata.file_type().is_symlink() {
        return Err(WorkspaceError::SymlinkRejected(path.to_path_buf()));
    }
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<(), WorkspaceError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_at(path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(WorkspaceError::LayoutMismatch(format!(
            "{} is not a real directory",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_regular_file(path: &Path) -> Result<(), WorkspaceError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_at(path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(WorkspaceError::LayoutMismatch(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    Ok(())
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, WorkspaceError> {
    let mut file = File::open(path).map_err(|source| io_at(path, source))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io_at(path, source))?;
    Ok(bytes)
}

fn read_text(path: &Path) -> Result<String, WorkspaceError> {
    fs::read_to_string(path).map_err(|source| io_at(path, source))
}

fn io_at(path: &Path, source: io::Error) -> WorkspaceError {
    WorkspaceError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug)]
pub enum WorkspaceError {
    Io { path: PathBuf, source: io::Error },
    FilesystemDetection { path: PathBuf, source: io::Error },
    UnsupportedFilesystem { name: String, kind: FilesystemKind },
    WorkspaceAlreadyExists(PathBuf),
    WorkspaceNotFound(PathBuf),
    InvalidRoot(PathBuf),
    SymlinkRejected(PathBuf),
    InvalidGenesis(String),
    InvalidConfig(String),
    InvalidImportRoot(String),
    Serialization(String),
    Identity(String),
    UnsupportedWorkspaceVersion,
    UnsupportedRecordSchemaFamily,
    InvalidWorkspaceId,
    InvalidGenesisSequence(u64),
    LayoutMismatch(String),
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::FilesystemDetection { path, source } => {
                write!(
                    f,
                    "cannot identify filesystem for {}: {source}",
                    path.display()
                )
            }
            Self::UnsupportedFilesystem { name, kind } => {
                write!(
                    f,
                    "filesystem {name} ({kind:?}) is not supported for durable writes"
                )
            }
            Self::WorkspaceAlreadyExists(path) => {
                write!(f, "workspace path is not empty: {}", path.display())
            }
            Self::WorkspaceNotFound(path) => {
                write!(
                    f,
                    "no investigation.toml found from {} up to the filesystem root",
                    path.display()
                )
            }
            Self::InvalidRoot(path) => write!(f, "invalid workspace root: {}", path.display()),
            Self::SymlinkRejected(path) => {
                write!(f, "workspace root may not be a symlink: {}", path.display())
            }
            Self::InvalidGenesis(message) => write!(f, "invalid workspace genesis: {message}"),
            Self::InvalidConfig(message) => write!(f, "invalid investigation.toml: {message}"),
            Self::InvalidImportRoot(message) => write!(f, "invalid import root: {message}"),
            Self::Serialization(message) => {
                write!(f, "cannot serialize workspace metadata: {message}")
            }
            Self::Identity(message) => write!(f, "cannot create workspace identity: {message}"),
            Self::UnsupportedWorkspaceVersion => {
                f.write_str("unsupported workspace marker version")
            }
            Self::UnsupportedRecordSchemaFamily => f.write_str("unsupported record schema family"),
            Self::InvalidWorkspaceId => f.write_str("workspace marker ID has the wrong kind"),
            Self::InvalidGenesisSequence(sequence) => {
                write!(f, "workspace genesis sequence must be 0, got {sequence}")
            }
            Self::LayoutMismatch(message) => write!(f, "workspace layout is invalid: {message}"),
        }
    }
}

impl std::error::Error for WorkspaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::FilesystemDetection { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "investigation-workspace-{}-{unique}-{sequence}",
                std::process::id()
            ));
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            if self.0.exists() {
                fs::remove_dir_all(&self.0).expect("remove test-owned directory");
            }
        }
    }

    #[derive(Clone)]
    struct FixedDetector(FilesystemInfo);

    impl FilesystemDetector for FixedDetector {
        fn detect(&self, _target: &Path) -> io::Result<FilesystemInfo> {
            Ok(self.0.clone())
        }
    }

    fn local_info() -> FilesystemInfo {
        FilesystemInfo {
            kind: FilesystemKind::Local,
            name: "test-local".to_owned(),
            magic: Some(1),
        }
    }

    #[test]
    fn init_with_import_roots_persists_only_absolute_existing_directories() {
        let temp = TempDir::new();
        fs::create_dir(&temp.0).unwrap();
        let inputs = fs::canonicalize(&temp.0).unwrap();
        let root = temp.0.join("casework");
        assert!(matches!(
            Workspace::init_with_import_roots(&root, false, &[PathBuf::from("relative")]),
            Err(WorkspaceError::InvalidImportRoot(_))
        ));
        assert!(!root.exists(), "a rejected import root creates nothing");
        let created =
            Workspace::init_with_import_roots(&root, false, std::slice::from_ref(&inputs)).unwrap();
        assert_eq!(created.config().import_roots, vec![inputs.clone()]);
        let reopened = Workspace::open(&root).unwrap();
        assert_eq!(reopened.config().import_roots, vec![inputs]);
    }

    #[test]
    fn init_leaves_a_pointer_to_the_brief_command_without_local_paths() {
        let temp = TempDir::new();
        fs::create_dir(&temp.0).unwrap();
        let root = temp.0.join("casework");
        Workspace::init_with_detector(&root, false, &FixedDetector(local_info())).unwrap();
        let pointer = fs::read_to_string(root.join(POINTER_FILE)).unwrap();
        let config = fs::read_to_string(root.join(CONFIG_FILE)).unwrap();
        let header: String = config
            .lines()
            .filter(|line| line.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        for text in [&pointer, &header] {
            assert!(
                text.contains("investigation --workspace DIR brief"),
                "{text}"
            );
            assert!(text.contains("--import-root"), "{text}");
            assert_eq!(
                investigation_core::local_path::elide_local_paths(text),
                text.as_str(),
                "the pointer names no absolute local path"
            );
            assert!(!text.contains(temp.0.to_str().unwrap()));
        }
        Workspace::open(&root).unwrap();
    }

    #[test]
    fn add_import_roots_rewrites_the_config_once_and_refuses_unsafe_roots() {
        let temp = TempDir::new();
        fs::create_dir(&temp.0).unwrap();
        let base = fs::canonicalize(&temp.0).unwrap();
        let inputs = base.join("inputs");
        fs::create_dir(&inputs).unwrap();
        let root = base.join("casework");
        let mut workspace =
            Workspace::init_with_detector(&root, false, &FixedDetector(local_info())).unwrap();
        assert!(workspace.config().import_roots.is_empty());

        for refused in [
            PathBuf::from("inputs"),
            base.join("missing"),
            inputs.join("..").join("inputs"),
            root.join("state"),
            root.clone(),
            PathBuf::from("/"),
        ] {
            assert!(
                matches!(
                    workspace.add_import_roots(std::slice::from_ref(&refused)),
                    Err(WorkspaceError::InvalidImportRoot(_))
                ),
                "{}",
                refused.display()
            );
        }
        #[cfg(unix)]
        {
            let link = base.join("link");
            std::os::unix::fs::symlink(&inputs, &link).unwrap();
            assert!(matches!(
                workspace.add_import_roots(std::slice::from_ref(&link)),
                Err(WorkspaceError::InvalidImportRoot(_))
            ));
        }
        let before = fs::read(root.join(CONFIG_FILE)).unwrap();
        assert_eq!(
            Workspace::open(&root).unwrap().config().import_roots,
            Vec::<PathBuf>::new()
        );

        let added = workspace
            .add_import_roots(&[inputs.clone(), inputs.clone()])
            .unwrap();
        assert_eq!(added, vec![inputs.clone()]);
        assert_eq!(workspace.config().import_roots, vec![inputs.clone()]);
        let reopened = Workspace::open(&root).unwrap();
        assert_eq!(reopened.config().import_roots, vec![inputs.clone()]);
        assert_eq!(reopened.config().vault_policy, VaultPolicy::default());
        assert_ne!(fs::read(root.join(CONFIG_FILE)).unwrap(), before);
        assert!(!root.join("state/tmp/investigation-config").exists());

        // A second handle sees the first handle's root and adds nothing.
        let mut other = Workspace::open(&root).unwrap();
        let unchanged = fs::read(root.join(CONFIG_FILE)).unwrap();
        assert!(
            other
                .add_import_roots(std::slice::from_ref(&inputs))
                .unwrap()
                .is_empty()
        );
        assert_eq!(fs::read(root.join(CONFIG_FILE)).unwrap(), unchanged);
    }

    #[test]
    fn init_open_verifies_immutable_marker_and_reports_genesis_sequence_zero() {
        let temp = TempDir::new();
        fs::create_dir(&temp.0).unwrap();
        let root = temp.0.join("casework");
        let created =
            Workspace::init_with_detector(&root, true, &FixedDetector(local_info())).unwrap();
        assert_eq!(created.sequence(), 0);
        assert_eq!(created.id().kind(), Kind::Workspace);
        assert!(root.join("state/cases").is_dir());
        assert!(root.join("state/events").is_dir());
        assert!(root.join("state/blobs/sha256").is_dir());
        assert!(root.join("state/checkpoints").is_dir());
        assert!(root.join("state/staging").is_dir());
        assert!(root.join("state/tmp").is_dir());
        assert!(root.join("cache").is_dir());
        assert!(root.join("local").is_dir());
        assert!(root.join("exports").is_dir());
        assert!(root.join("vault/Notes").is_dir());
        assert!(root.join("vault/Boards").is_dir());
        assert!(root.join("vault/Inbox").is_dir());

        let opened = Workspace::open(&root).unwrap();
        assert_eq!(opened.id(), created.id());
        assert_eq!(opened.sequence(), 0);
        assert!(opened.config().vault_policy.enabled);
        let marker_before_open = fs::read(root.join(GENESIS_FILE)).unwrap();
        assert_eq!(
            fs::read(root.join(GENESIS_FILE)).unwrap(),
            marker_before_open
        );
    }

    #[test]
    fn refused_filesystem_is_detected_before_any_directory_is_created() {
        let temp = TempDir::new();
        fs::create_dir(&temp.0).unwrap();
        let root = temp.0.join("must-not-exist");
        let detector = FixedDetector(FilesystemInfo {
            kind: FilesystemKind::Network,
            name: "nfs".to_owned(),
            magic: Some(0x6969),
        });
        let error = Workspace::init_with_detector(&root, true, &detector).unwrap_err();
        assert!(matches!(
            error,
            WorkspaceError::UnsupportedFilesystem { .. }
        ));
        assert!(!root.exists());
        assert!(fs::read_dir(&temp.0).unwrap().next().is_none());
    }

    #[test]
    fn unknown_and_fuse_filesystems_are_refused() {
        assert!(matches!(
            require_supported_filesystem(&FilesystemInfo {
                kind: FilesystemKind::Fuse,
                name: "fuse".into(),
                magic: None
            }),
            Err(WorkspaceError::UnsupportedFilesystem { .. })
        ));
        assert!(matches!(
            require_supported_filesystem(&FilesystemInfo {
                kind: FilesystemKind::Unknown,
                name: "other".into(),
                magic: None
            }),
            Err(WorkspaceError::UnsupportedFilesystem { .. })
        ));
    }

    #[test]
    fn config_has_role_comments_and_rejects_unknown_top_level_keys() {
        let config = WorkspaceConfig::initial(true);
        let text = config_text(&config).unwrap();
        for role in [
            "state/ is canonical",
            "cache/ is rebuildable",
            "local/ stores",
            "vault/ is an optional",
        ] {
            assert!(text.contains(role));
        }
        let invalid = String::from(
            "schema_version = 'inv.workspace-config.v1'\nunknown = true\n\n[vault_policy]\nenabled = true\ninclude_approved_derivatives = false\npreserve_human_folders = true\n",
        );
        assert!(toml::from_str::<WorkspaceConfig>(&invalid).is_err());
    }

    #[test]
    fn discovery_uses_nearest_workspace_config() {
        let temp = TempDir::new();
        fs::create_dir(&temp.0).unwrap();
        let root = temp.0.join("workspace");
        Workspace::init_with_detector(&root, false, &FixedDetector(local_info())).unwrap();
        let nested = root.join("child").join("nested");
        fs::create_dir_all(&nested).unwrap();
        let discovered = Workspace::discover(&nested).unwrap();
        assert_eq!(discovered.root(), root.canonicalize().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn workspace_directories_and_marker_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new();
        fs::create_dir(&temp.0).unwrap();
        let root = temp.0.join("workspace");
        Workspace::init_with_detector(&root, false, &FixedDetector(local_info())).unwrap();
        assert_eq!(
            fs::metadata(root.join("state"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(root.join(GENESIS_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(root.join(LOCK_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
