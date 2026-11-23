//! Open local imports once beneath an explicitly selected directory descriptor.
use rustix::fs::{CWD, Mode, OFlags, ResolveFlags, openat2};
use std::{
    fmt,
    fs::File,
    io,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

#[derive(Debug)]
pub struct ImportError {
    pub root: PathBuf,
    pub path: PathBuf,
    pub reason: ImportErrorKind,
}
#[derive(Debug)]
pub enum ImportErrorKind {
    InvalidPath,
    Unsupported,
    Io(io::Error),
    NotRegular,
    MultipleLinks,
    TooLarge { size: u64, limit: u64 },
    Changed,
}
impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "import root {:?}, path {:?}: {}",
            self.root, self.path, self.reason
        )
    }
}
impl fmt::Display for ImportErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath => f.write_str(
                "invalid path: name a file beneath the import root without `.` or `..` components",
            ),
            Self::Unsupported => f.write_str("this kernel lacks openat2"),
            Self::Io(error) => write!(f, "{error}"),
            Self::NotRegular => f.write_str("not a regular file"),
            Self::MultipleLinks => f.write_str(
                "the file has more than one hard link; import a copy with a single link",
            ),
            Self::TooLarge { size, limit } => {
                write!(f, "the file is {size} bytes; the limit is {limit} bytes")
            }
            Self::Changed => {
                f.write_str("the file changed while it was read; retry the identical request")
            }
        }
    }
}
impl std::error::Error for ImportError {}

pub struct ImportRoot {
    path: PathBuf,
    directory: File,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileWitness {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub modified_sec: i64,
    pub modified_nsec: i64,
    pub changed_sec: i64,
    pub changed_nsec: i64,
    pub links: u64,
}
#[derive(Debug)]
pub struct OpenedImport {
    pub file: File,
    pub witness: FileWitness,
}
impl FileWitness {
    fn from_metadata(m: &std::fs::Metadata) -> Self {
        Self {
            device: m.dev(),
            inode: m.ino(),
            size: m.len(),
            modified_sec: m.mtime(),
            modified_nsec: m.mtime_nsec(),
            changed_sec: m.ctime(),
            changed_nsec: m.ctime_nsec(),
            links: m.nlink(),
        }
    }
    /// Check the same open descriptor after copying, including metadata that can
    /// reveal replacement, truncation or modification. This is not a snapshot
    /// guarantee against a concurrent writer that can restore metadata.
    pub fn verify_unchanged(&self, file: &File) -> io::Result<bool> {
        let m = file.metadata()?;
        Ok(m.is_file() && *self == Self::from_metadata(&m))
    }
}
impl ImportRoot {
    /// Linux openat2 is required; unsupported kernels are refused, not downgraded.
    pub fn open(path: &Path) -> Result<Self, ImportError> {
        let err = |reason| ImportError {
            root: path.to_owned(),
            path: PathBuf::new(),
            reason,
        };
        if !path.is_absolute()
            || path == Path::new("/")
            || path.components().any(|c| matches!(c, Component::ParentDir))
        {
            return Err(err(ImportErrorKind::InvalidPath));
        }
        let fd = openat2(
            CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )
        .map_err(|e| err(map_errno(e)))?;
        Ok(Self {
            path: path.to_owned(),
            directory: File::from(fd),
        })
    }
    pub fn open_file(&self, path: &Path, max_bytes: u64) -> Result<OpenedImport, ImportError> {
        let err = |reason| ImportError {
            root: self.path.clone(),
            path: path.to_owned(),
            reason,
        };
        if path.as_os_str().is_empty()
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(err(ImportErrorKind::InvalidPath));
        }
        let fd = openat2(
            &self.directory,
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )
        .map_err(|e| err(map_errno(e)))?;
        let file = File::from(fd);
        let meta = file.metadata().map_err(|e| err(ImportErrorKind::Io(e)))?;
        if !meta.is_file() {
            return Err(err(ImportErrorKind::NotRegular));
        }
        if meta.nlink() != 1 {
            return Err(err(ImportErrorKind::MultipleLinks));
        }
        if meta.len() > max_bytes {
            return Err(err(ImportErrorKind::TooLarge {
                size: meta.len(),
                limit: max_bytes,
            }));
        }
        Ok(OpenedImport {
            file,
            witness: FileWitness::from_metadata(&meta),
        })
    }
}
fn map_errno(e: rustix::io::Errno) -> ImportErrorKind {
    if e == rustix::io::Errno::NOSYS {
        ImportErrorKind::Unsupported
    } else {
        ImportErrorKind::Io(e.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Read, os::unix::fs::symlink};
    #[test]
    fn confined_files_and_swapped_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("inside")).unwrap();
        fs::write(root.join("inside/a"), b"safe").unwrap();
        fs::write(temp.path().join("outside"), b"secret").unwrap();
        let root = ImportRoot::open(&root).unwrap();
        let mut opened = root.open_file(Path::new("inside/a"), 4).unwrap();
        let mut value = String::new();
        opened.file.read_to_string(&mut value).unwrap();
        assert_eq!(value, "safe");
        assert!(opened.witness.verify_unchanged(&opened.file).unwrap());
        assert!(root.open_file(Path::new("../outside"), 100).is_err());
        assert!(root.open_file(Path::new("inside/a"), 3).is_err());
        fs::remove_file(temp.path().join("root/inside/a")).unwrap();
        symlink(
            temp.path().join("outside"),
            temp.path().join("root/inside/a"),
        )
        .unwrap();
        assert!(root.open_file(Path::new("inside/a"), 100).is_err());
        fs::hard_link(temp.path().join("outside"), temp.path().join("root/hard")).unwrap();
        assert!(matches!(
            root.open_file(Path::new("hard"), 100).unwrap_err().reason,
            ImportErrorKind::MultipleLinks
        ));
        symlink(temp.path(), temp.path().join("root/through")).unwrap();
        assert!(root.open_file(Path::new("through/outside"), 100).is_err());
        fs::write(temp.path().join("root/mutable"), b"a").unwrap();
        let opened = root.open_file(Path::new("mutable"), 100).unwrap();
        fs::write(temp.path().join("root/mutable"), b"changed").unwrap();
        assert!(!opened.witness.verify_unchanged(&opened.file).unwrap());
        assert!(matches!(
            root.open_file(Path::new("inside"), 100).unwrap_err().reason,
            ImportErrorKind::NotRegular
        ));
    }
}
