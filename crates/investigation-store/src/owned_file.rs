//! One file outside any workspace, named by the user, read and replaced
//! without following a symbolic link.
//!
//! `integration apply` and `integration remove` use this to change an agent
//! instructions file. The directory is opened once with `openat2` and
//! `RESOLVE_NO_SYMLINKS`, so no component of the path the caller named may be
//! a link; the file itself is opened with `O_NOFOLLOW`. A change is written to
//! a temporary file in the same directory, synced, and renamed over the target
//! only if the target still holds the bytes the change was computed from. A
//! new file is published without replacing anything that appeared meanwhile.
//! Nothing outside the one directory entry is ever written.

use rustix::{
    fs::{
        AtFlags, CWD, FileType, Mode, OFlags, RenameFlags, ResolveFlags, fchmod, fstat, fsync,
        linkat, openat, openat2, renameat, renameat_with, unlinkat,
    },
    io::Errno,
};
use std::{
    ffi::{OsStr, OsString},
    fmt,
    fs::File,
    io::{self, Read, Write},
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

/// Largest file read or written through this module.
pub const MAX_OWNED_FILE_BYTES: u64 = 1024 * 1024;

/// Temporary names tried beside the target before giving up.
const MAX_TEMPORARY_ATTEMPTS: u32 = 64;

/// Why a named file cannot be read or changed.
#[derive(Debug)]
pub enum OwnedFileError {
    /// The path is empty or does not end in a file name.
    InvalidPath,
    /// A component of the named path, or the file itself, is a symbolic link.
    Symlink,
    /// The directory that should hold the file does not exist.
    DirectoryMissing,
    /// The path names something other than a regular file.
    NotRegular,
    /// The file has more than one hard link, so replacing it would split them.
    MultipleLinks,
    TooLarge {
        size: u64,
        limit: u64,
    },
    /// The file changed after it was read; nothing was written.
    Changed,
    /// The kernel lacks `openat2`.
    Unsupported,
    Io(io::Error),
}

impl fmt::Display for OwnedFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath => f.write_str("the path does not name a file"),
            Self::Symlink => f.write_str("a component of the path is a symbolic link"),
            Self::DirectoryMissing => f.write_str("the directory does not exist"),
            Self::NotRegular => f.write_str("the path is not a regular file"),
            Self::MultipleLinks => f.write_str("the file has more than one hard link"),
            Self::TooLarge { size, limit } => {
                write!(f, "the file holds {size} bytes; the limit is {limit}")
            }
            Self::Changed => f.write_str("the file changed while it was being updated"),
            Self::Unsupported => f.write_str("this kernel lacks openat2"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for OwnedFileError {}

fn from_errno(errno: Errno) -> OwnedFileError {
    match errno {
        Errno::LOOP => OwnedFileError::Symlink,
        Errno::NOSYS => OwnedFileError::Unsupported,
        Errno::NOENT | Errno::NOTDIR => OwnedFileError::DirectoryMissing,
        other => OwnedFileError::Io(other.into()),
    }
}

/// An open directory and one file name in it.
pub struct OwnedFile {
    directory: File,
    directory_path: PathBuf,
    name: OsString,
}

impl OwnedFile {
    /// Open the directory that holds `path`. Relative paths resolve against
    /// the working directory; every component the caller wrote must be a
    /// real directory, not a link.
    pub fn open(path: &Path) -> Result<Self, OwnedFileError> {
        let Some(Component::Normal(name)) = path.components().next_back() else {
            return Err(OwnedFileError::InvalidPath);
        };
        if path.as_os_str().as_bytes().ends_with(b"/") {
            return Err(OwnedFileError::InvalidPath);
        }
        let directory_path = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let fd = openat2(
            CWD,
            &directory_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )
        .map_err(from_errno)?;
        Ok(Self {
            directory: File::from(fd),
            directory_path,
            name: name.to_owned(),
        })
    }

    /// The directory as named, for locating an enclosing workspace.
    pub fn directory_path(&self) -> &Path {
        &self.directory_path
    }

    pub fn name(&self) -> &OsStr {
        &self.name
    }

    /// The file's bytes, or `None` when it does not exist.
    pub fn read(&self) -> Result<Option<Vec<u8>>, OwnedFileError> {
        Ok(self.read_with_mode()?.map(|(bytes, _)| bytes))
    }

    fn read_with_mode(&self) -> Result<Option<(Vec<u8>, u32)>, OwnedFileError> {
        let fd = match openat(
            &self.directory,
            &self.name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(Errno::LOOP) => return Err(OwnedFileError::Symlink),
            Err(errno) => return Err(OwnedFileError::Io(errno.into())),
        };
        let stat = fstat(&fd).map_err(|errno| OwnedFileError::Io(errno.into()))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(OwnedFileError::NotRegular);
        }
        if stat.st_nlink != 1 {
            return Err(OwnedFileError::MultipleLinks);
        }
        let size = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
        if size > MAX_OWNED_FILE_BYTES {
            return Err(OwnedFileError::TooLarge {
                size,
                limit: MAX_OWNED_FILE_BYTES,
            });
        }
        let mut bytes = Vec::new();
        File::from(fd)
            .take(MAX_OWNED_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(OwnedFileError::Io)?;
        if bytes.len() as u64 > MAX_OWNED_FILE_BYTES {
            return Err(OwnedFileError::TooLarge {
                size: bytes.len() as u64,
                limit: MAX_OWNED_FILE_BYTES,
            });
        }
        Ok(Some((bytes, stat.st_mode & 0o7777)))
    }

    /// Fail unless the file still holds `before` (`None`: still absent).
    fn expect(&self, before: Option<&[u8]>) -> Result<Option<u32>, OwnedFileError> {
        let current = self.read_with_mode()?;
        if current.as_ref().map(|(bytes, _)| bytes.as_slice()) != before {
            return Err(OwnedFileError::Changed);
        }
        Ok(current.map(|(_, mode)| mode))
    }

    /// Replace the file's bytes `before` (`None`: the file is absent) with
    /// `after`, atomically. The file keeps its permission bits; a new file
    /// gets 0644 less the umask.
    pub fn replace(&self, before: Option<&[u8]>, after: &[u8]) -> Result<(), OwnedFileError> {
        if after.len() as u64 > MAX_OWNED_FILE_BYTES {
            return Err(OwnedFileError::TooLarge {
                size: after.len() as u64,
                limit: MAX_OWNED_FILE_BYTES,
            });
        }
        let mode = self.expect(before)?;
        let (temporary, fd) = self.create_temporary()?;
        let result = self.publish(fd, &temporary, mode, before.is_none(), after);
        if result.is_err() {
            let _ = unlinkat(&self.directory, &temporary, AtFlags::empty());
        }
        result
    }

    /// A new temporary file beside the target. `O_EXCL` never reuses a name,
    /// so one left by an interrupted run is skipped, not overwritten.
    fn create_temporary(&self) -> Result<(OsString, rustix::fd::OwnedFd), OwnedFileError> {
        for attempt in 0..MAX_TEMPORARY_ATTEMPTS {
            let mut temporary = OsString::from(".");
            temporary.push(&self.name);
            temporary.push(format!(".{attempt}.tmp"));
            match openat(
                &self.directory,
                &temporary,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o644),
            ) {
                Ok(fd) => return Ok((temporary, fd)),
                Err(Errno::EXIST) => {}
                Err(errno) => return Err(OwnedFileError::Io(errno.into())),
            }
        }
        Err(OwnedFileError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "every temporary file name beside the target is taken",
        )))
    }

    fn publish(
        &self,
        fd: rustix::fd::OwnedFd,
        temporary: &OsStr,
        mode: Option<u32>,
        create: bool,
        after: &[u8],
    ) -> Result<(), OwnedFileError> {
        let io = |errno: Errno| OwnedFileError::Io(errno.into());
        if let Some(mode) = mode {
            fchmod(&fd, Mode::from_raw_mode(mode)).map_err(io)?;
        }
        let mut file = File::from(fd);
        file.write_all(after).map_err(OwnedFileError::Io)?;
        file.sync_all().map_err(OwnedFileError::Io)?;
        drop(file);
        if create {
            match renameat_with(
                &self.directory,
                temporary,
                &self.directory,
                &self.name,
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => {}
                Err(Errno::EXIST) => return Err(OwnedFileError::Changed),
                // No RENAME_NOREPLACE on this filesystem: link(2) never replaces.
                Err(Errno::INVAL | Errno::NOSYS) => {
                    linkat(
                        &self.directory,
                        temporary,
                        &self.directory,
                        &self.name,
                        AtFlags::empty(),
                    )
                    .map_err(|errno| match errno {
                        Errno::EXIST => OwnedFileError::Changed,
                        other => io(other),
                    })?;
                    unlinkat(&self.directory, temporary, AtFlags::empty()).map_err(io)?;
                }
                Err(errno) => return Err(io(errno)),
            }
        } else {
            renameat(&self.directory, temporary, &self.directory, &self.name).map_err(io)?;
        }
        fsync(&self.directory).map_err(io)
    }

    /// Delete the file, which must still hold `before`.
    pub fn remove(&self, before: &[u8]) -> Result<(), OwnedFileError> {
        self.expect(Some(before))?;
        let io = |errno: Errno| OwnedFileError::Io(errno.into());
        unlinkat(&self.directory, &self.name, AtFlags::empty()).map_err(io)?;
        fsync(&self.directory).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    #[test]
    fn creates_replaces_and_removes_one_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("notes.md");
        let file = OwnedFile::open(&path).unwrap();
        assert_eq!(file.read().unwrap(), None);
        file.replace(None, b"one\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        file.replace(Some(b"one\n"), b"two\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two\n");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the file keeps its permissions");
        assert!(matches!(
            file.replace(Some(b"one\n"), b"three\n"),
            Err(OwnedFileError::Changed)
        ));
        assert!(matches!(
            file.replace(None, b"three\n"),
            Err(OwnedFileError::Changed)
        ));
        file.remove(b"two\n").unwrap();
        assert!(!path.exists());
        let leftovers: Vec<_> = fs::read_dir(temp.path()).unwrap().collect();
        assert!(leftovers.is_empty(), "no temporary file is left behind");
    }

    #[test]
    fn refuses_links_missing_directories_and_odd_paths() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("target.md"), b"keep").unwrap();
        symlink(&real, temp.path().join("linked")).unwrap();
        assert!(matches!(
            OwnedFile::open(&temp.path().join("linked/target.md")),
            Err(OwnedFileError::Symlink)
        ));
        symlink(real.join("target.md"), real.join("alias.md")).unwrap();
        let alias = OwnedFile::open(&real.join("alias.md")).unwrap();
        assert!(matches!(alias.read(), Err(OwnedFileError::Symlink)));
        assert!(matches!(
            alias.replace(None, b"x"),
            Err(OwnedFileError::Symlink)
        ));
        assert_eq!(fs::read(real.join("target.md")).unwrap(), b"keep");
        assert!(matches!(
            OwnedFile::open(&temp.path().join("missing/target.md")),
            Err(OwnedFileError::DirectoryMissing)
        ));
        for bad in ["", "/", "..", "dir/", "a/.."] {
            assert!(
                matches!(
                    OwnedFile::open(Path::new(bad)),
                    Err(OwnedFileError::InvalidPath)
                ),
                "{bad:?}"
            );
        }
        let directory = OwnedFile::open(&real).unwrap();
        assert!(matches!(directory.read(), Err(OwnedFileError::NotRegular)));
        fs::hard_link(real.join("target.md"), real.join("hard.md")).unwrap();
        let hard = OwnedFile::open(&real.join("hard.md")).unwrap();
        assert!(matches!(hard.read(), Err(OwnedFileError::MultipleLinks)));
    }

    #[test]
    fn refuses_files_over_the_limit() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("big.md");
        let big = vec![b'x'; MAX_OWNED_FILE_BYTES as usize + 1];
        fs::write(&path, &big).unwrap();
        let file = OwnedFile::open(&path).unwrap();
        assert!(matches!(file.read(), Err(OwnedFileError::TooLarge { .. })));
        let other = OwnedFile::open(&temp.path().join("new.md")).unwrap();
        assert!(matches!(
            other.replace(None, &big),
            Err(OwnedFileError::TooLarge { .. })
        ));
    }
}
