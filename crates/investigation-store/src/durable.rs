//! Narrow filesystem publication boundaries. Paths are relative to the storage root.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub trait DurableFile: Write {
    fn sync_all(&mut self) -> io::Result<()>;
    fn set_readonly(&mut self) -> io::Result<()>;
}
pub trait DurableFs {
    type File: DurableFile;
    type Guard;
    fn create_private(&mut self, path: &Path) -> io::Result<Self::File>;
    fn create_dir(&mut self, path: &Path) -> io::Result<()>;
    fn read(&self, path: &Path) -> io::Result<Box<dyn Read>>;
    fn sync_existing(&mut self, path: &Path) -> io::Result<()>;
    fn list(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    fn link_no_replace(&mut self, source: &Path, target: &Path) -> io::Result<()>;
    fn rename_no_replace(&mut self, source: &Path, target: &Path) -> io::Result<()>;
    fn sync_dir(&mut self, path: &Path) -> io::Result<()>;
    fn remove(&mut self, path: &Path) -> io::Result<()>;
    /// Remove one empty directory. Never recursive.
    fn remove_dir(&mut self, path: &Path) -> io::Result<()>;
    fn lock(&mut self, path: &Path, timeout: Duration) -> Result<Self::Guard, LockError>;
    /// Called after journal verification while the workspace commit lock is held.
    fn reclaim_publication_temporaries(&mut self) {}
}
impl DurableFile for File {
    fn sync_all(&mut self) -> io::Result<()> {
        File::sync_all(self)
    }
    fn set_readonly(&mut self) -> io::Result<()> {
        let mut p = self.metadata()?.permissions();
        p.set_readonly(true);
        self.set_permissions(p)
    }
}
pub struct LocalFs {
    root: PathBuf,
}
impl LocalFs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    fn path(&self, path: &Path) -> io::Result<PathBuf> {
        if path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path must be relative and contain only normal components",
            ));
        }
        Ok(self.root.join(path))
    }
}
impl DurableFs for LocalFs {
    type File = File;
    type Guard = CommitLock;
    fn create_private(&mut self, p: &Path) -> io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.path(p)?)
    }
    fn create_dir(&mut self, p: &Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(self.path(p)?)
    }
    fn read(&self, p: &Path) -> io::Result<Box<dyn Read>> {
        Ok(Box::new(File::open(self.path(p)?)?))
    }
    fn sync_existing(&mut self, p: &Path) -> io::Result<()> {
        File::open(self.path(p)?)?.sync_all()
    }
    fn list(&self, p: &Path) -> io::Result<Vec<PathBuf>> {
        let mut entries = fs::read_dir(self.path(p)?)?
            .map(|x| x.map(|e| p.join(e.file_name())))
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort();
        Ok(entries)
    }
    fn link_no_replace(&mut self, a: &Path, b: &Path) -> io::Result<()> {
        fs::hard_link(self.path(a)?, self.path(b)?)
    }
    fn rename_no_replace(&mut self, a: &Path, b: &Path) -> io::Result<()> {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            self.path(a)?,
            rustix::fs::CWD,
            self.path(b)?,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(Into::into)
    }
    fn sync_dir(&mut self, p: &Path) -> io::Result<()> {
        File::open(self.path(p)?)?.sync_all()
    }
    fn remove(&mut self, p: &Path) -> io::Result<()> {
        fs::remove_file(self.path(p)?)
    }
    fn remove_dir(&mut self, p: &Path) -> io::Result<()> {
        fs::remove_dir(self.path(p)?)
    }
    fn lock(&mut self, p: &Path, timeout: Duration) -> Result<CommitLock, LockError> {
        CommitLock::acquire(&self.path(p).map_err(LockError::Io)?, timeout)
    }

    fn reclaim_publication_temporaries(&mut self) {
        reclaim_publication_temporaries(&self.root.join("state/tmp"), std::time::SystemTime::now());
    }
}

const PUBLICATION_TEMP_GRACE: Duration = Duration::from_secs(24 * 60 * 60);

fn reclaim_publication_temporaries(directory: &Path, now: std::time::SystemTime) {
    use investigation_core::identity::{Id, Kind};

    if !fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.is_dir()) {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let transaction_id = |name: &str| {
        name.parse::<Id>()
            .is_ok_and(|id| id.kind() == Kind::Transaction)
    };
    let import_id = |name: &str| name.parse::<Id>().is_ok_and(|id| id.kind() == Kind::Import);
    // A copy of import bytes not yet published: an ingest or finalization
    // killed mid-stream. Blob publication happens only under the commit lock
    // this sweep holds, so an expired one is no publication in progress. A
    // writer stalled past the grace period between its copy and its commit
    // finds its copy gone and fails without committing.
    let import_copy = |name: &str| {
        ["import-", "capture-"].iter().any(|prefix| {
            name.strip_prefix(prefix)
                .and_then(|name| name.strip_suffix(".tmp"))
                .is_some_and(import_id)
        })
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        let expired = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= PUBLICATION_TEMP_GRACE);
        if !expired {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if metadata.is_file()
            && (name
                .strip_prefix("event-")
                .and_then(|name| name.strip_suffix(".tmp"))
                .is_some_and(transaction_id)
                || import_copy(name))
        {
            let _ = fs::remove_file(path);
        } else if metadata.is_dir() && name.strip_prefix("create-").is_some_and(transaction_id) {
            let _ = remove_unpublished_case(&path);
        }
    }
}

fn remove_unpublished_case(path: &Path) -> io::Result<()> {
    let mut files = Vec::new();
    let mut events = None;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        let kind = entry.file_type()?;
        if kind.is_file() && matches!(name.to_str(), Some("genesis.json" | "commit.lock")) {
            files.push(entry.path());
        } else if kind.is_dir() && name == "events" {
            for event in fs::read_dir(entry.path())? {
                let event = event?;
                if !event.file_type()?.is_file() || event.file_name() != "000000000001.json" {
                    return Ok(());
                }
                files.push(event.path());
            }
            events = Some(entry.path());
        } else {
            return Ok(());
        }
    }
    for file in files {
        fs::remove_file(file)?;
    }
    if let Some(events) = events {
        fs::remove_dir(events)?;
    }
    fs::remove_dir(path)
}

#[derive(Clone)]
pub struct WriteGate(Arc<AtomicBool>);
impl Default for WriteGate {
    fn default() -> Self {
        static PROCESS_GATE: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
        Self(
            PROCESS_GATE
                .get_or_init(|| Arc::new(AtomicBool::new(false)))
                .clone(),
        )
    }
}
impl WriteGate {
    #[cfg(test)]
    pub(crate) fn isolated() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn stop(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn check(&self) -> Result<(), DurableError> {
        if self.is_stopped() {
            Err(DurableError::Stopped)
        } else {
            Ok(())
        }
    }
}
#[derive(Debug)]
pub enum DurableError {
    Io(io::Error),
    SyncUnknown(io::Error),
    Stopped,
}
impl From<io::Error> for DurableError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
/// Never retry a failed sync. A fresh process must recover and verify before writes resume.
pub fn sync_file(file: &mut impl DurableFile, gate: &WriteGate) -> Result<(), DurableError> {
    gate.check()?;
    file.sync_all().map_err(|e| {
        gate.stop();
        DurableError::SyncUnknown(e)
    })
}
pub fn sync_directory(
    fs: &mut impl DurableFs,
    path: &Path,
    gate: &WriteGate,
) -> Result<(), DurableError> {
    gate.check()?;
    fs.sync_dir(path).map_err(|e| {
        gate.stop();
        DurableError::SyncUnknown(e)
    })
}
pub fn sync_existing_file(
    fs: &mut impl DurableFs,
    path: &Path,
    gate: &WriteGate,
) -> Result<(), DurableError> {
    gate.check()?;
    fs.sync_existing(path).map_err(|e| {
        gate.stop();
        DurableError::SyncUnknown(e)
    })
}
/// Publish only a previously synced, closed private file. Existing destinations are never replaced.
pub fn publish(
    fs: &mut impl DurableFs,
    temp: &Path,
    target: &Path,
    gate: &WriteGate,
) -> Result<(), DurableError> {
    gate.check()?;
    fs.link_no_replace(temp, target)?;
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "target needs a parent directory",
            )
        })?;
    sync_directory(fs, parent, gate)
}

/// A persistent lock file; never delete or rename it. Local serialization only, not authorization.
pub struct CommitLock {
    _file: File,
    pub waited: Duration,
}
#[derive(Debug)]
pub enum LockError {
    Busy { waited: Duration },
    Io(io::Error),
}
impl CommitLock {
    pub fn acquire(path: &Path, timeout: Duration) -> Result<Self, LockError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(LockError::Io)?;
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => {
                    return Ok(Self {
                        _file: file,
                        waited: start.elapsed(),
                    });
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    let elapsed = start.elapsed();
                    if elapsed >= timeout {
                        return Err(LockError::Busy { waited: elapsed });
                    }
                    std::thread::sleep(Duration::from_millis(5).min(timeout - elapsed));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(LockError::Io(e)),
            }
        }
    }
}

// In-memory crash model: directory entries and file data become durable separately.
#[derive(Default)]
struct MemState {
    entries: BTreeMap<PathBuf, usize>,
    durable_entries: BTreeMap<PathBuf, usize>,
    dirs: BTreeSet<PathBuf>,
    durable_dirs: BTreeSet<PathBuf>,
    files: Vec<(Vec<u8>, Vec<u8>, bool)>,
    calls: usize,
    fail_at: Option<usize>,
    /// Bytes file writes may still add before the device is full.
    free_bytes: Option<u64>,
    locks: BTreeSet<PathBuf>,
    renames: Vec<(PathBuf, PathBuf)>,
}
#[derive(Clone, Default)]
pub struct MemoryFs(std::rc::Rc<std::cell::RefCell<MemState>>);
impl MemoryFs {
    pub fn new() -> Self {
        let s = Self::default();
        s.0.borrow_mut().dirs.insert(PathBuf::new());
        s.0.borrow_mut().durable_dirs.insert(PathBuf::new());
        s
    }
    pub fn fail_at(&self, call: usize) {
        let mut s = self.0.borrow_mut();
        s.calls = 0;
        s.fail_at = Some(call);
    }
    /// Fill the device after `bytes` more bytes of file data: a write then
    /// stores what fits and the next one fails with `StorageFull`, as a full
    /// disk does. Removing a file frees nothing.
    pub fn fill_after(&self, bytes: u64) {
        self.0.borrow_mut().free_bytes = Some(bytes);
    }
    /// Paths of every lock currently held through this filesystem.
    #[cfg(test)]
    pub(crate) fn held_locks(&self) -> Vec<PathBuf> {
        self.0.borrow().locks.iter().cloned().collect()
    }
    pub fn crash(&self) {
        let mut s = self.0.borrow_mut();
        s.entries = s.durable_entries.clone();
        s.dirs = s.durable_dirs.clone();
        for (data, durable, _) in &mut s.files {
            *data = durable.clone();
        }
        s.fail_at = None;
        s.locks.clear();
        s.renames.clear();
    }
    fn step(&self) -> io::Result<()> {
        let mut s = self.0.borrow_mut();
        s.calls += 1;
        if s.fail_at == Some(s.calls) {
            return Err(io::Error::other("injected failure"));
        }
        Ok(())
    }
}
pub struct MemoryFile {
    fs: MemoryFs,
    inode: usize,
}
impl Write for MemoryFile {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.fs.step()?;
        let mut s = self.fs.0.borrow_mut();
        if s.files[self.inode].2 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "readonly"));
        }
        let fits = match s.free_bytes {
            Some(0) if !b.is_empty() => {
                return Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    "no space left on device",
                ));
            }
            Some(free) => usize::try_from(free).unwrap_or(usize::MAX).min(b.len()),
            None => b.len(),
        };
        if let Some(free) = &mut s.free_bytes {
            *free -= fits as u64;
        }
        s.files[self.inode].0.extend_from_slice(&b[..fits]);
        Ok(fits)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl DurableFile for MemoryFile {
    fn sync_all(&mut self) -> io::Result<()> {
        self.fs.step()?;
        let mut s = self.fs.0.borrow_mut();
        s.files[self.inode].1 = s.files[self.inode].0.clone();
        Ok(())
    }
    fn set_readonly(&mut self) -> io::Result<()> {
        self.fs.step()?;
        self.fs.0.borrow_mut().files[self.inode].2 = true;
        Ok(())
    }
}
impl DurableFs for MemoryFs {
    type File = MemoryFile;
    type Guard = MemoryLock;
    fn create_private(&mut self, p: &Path) -> io::Result<MemoryFile> {
        self.step()?;
        let mut s = self.0.borrow_mut();
        if s.entries.contains_key(p) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        if !s.dirs.contains(p.parent().unwrap_or(Path::new(""))) {
            return Err(io::ErrorKind::NotFound.into());
        }
        let inode = s.files.len();
        s.files.push((vec![], vec![], false));
        s.entries.insert(p.to_owned(), inode);
        Ok(MemoryFile {
            fs: self.clone(),
            inode,
        })
    }
    fn create_dir(&mut self, p: &Path) -> io::Result<()> {
        self.step()?;
        let mut s = self.0.borrow_mut();
        if s.dirs.contains(p) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        if !s.dirs.contains(p.parent().unwrap_or(Path::new(""))) {
            return Err(io::ErrorKind::NotFound.into());
        }
        s.dirs.insert(p.to_owned());
        Ok(())
    }
    fn read(&self, p: &Path) -> io::Result<Box<dyn Read>> {
        self.step()?;
        let s = self.0.borrow();
        let i = *s.entries.get(p).ok_or(io::ErrorKind::NotFound)?;
        Ok(Box::new(io::Cursor::new(s.files[i].0.clone())))
    }
    fn sync_existing(&mut self, p: &Path) -> io::Result<()> {
        self.step()?;
        let mut s = self.0.borrow_mut();
        let i = *s.entries.get(p).ok_or(io::ErrorKind::NotFound)?;
        s.files[i].1 = s.files[i].0.clone();
        Ok(())
    }
    fn list(&self, p: &Path) -> io::Result<Vec<PathBuf>> {
        self.step()?;
        let s = self.0.borrow();
        if !s.dirs.contains(p) {
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(s.entries
            .keys()
            .chain(s.dirs.iter())
            .filter(|v| v.parent() == Some(p))
            .cloned()
            .collect())
    }
    fn link_no_replace(&mut self, a: &Path, b: &Path) -> io::Result<()> {
        self.step()?;
        let mut s = self.0.borrow_mut();
        if s.entries.contains_key(b) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        if !s.dirs.contains(b.parent().unwrap_or(Path::new(""))) {
            return Err(io::ErrorKind::NotFound.into());
        }
        let i = *s.entries.get(a).ok_or(io::ErrorKind::NotFound)?;
        s.entries.insert(b.to_owned(), i);
        Ok(())
    }
    fn rename_no_replace(&mut self, a: &Path, b: &Path) -> io::Result<()> {
        self.step()?;
        let mut s = self.0.borrow_mut();
        if s.dirs.contains(b) || s.entries.contains_key(b) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        if !s.dirs.contains(a) || !s.dirs.contains(b.parent().unwrap_or(Path::new(""))) {
            return Err(io::ErrorKind::NotFound.into());
        }
        let entries: Vec<_> = s
            .entries
            .iter()
            .filter_map(|(p, i)| {
                p.strip_prefix(a)
                    .ok()
                    .map(|rest| (p.clone(), b.join(rest), *i))
            })
            .collect();
        for (old, new, i) in entries {
            s.entries.remove(&old);
            s.entries.insert(new, i);
        }
        let dirs: Vec<_> = s
            .dirs
            .iter()
            .filter_map(|p| p.strip_prefix(a).ok().map(|rest| (p.clone(), b.join(rest))))
            .collect();
        for (old, new) in dirs {
            s.dirs.remove(&old);
            s.dirs.insert(new);
        }
        s.renames.push((a.to_owned(), b.to_owned()));
        Ok(())
    }
    fn sync_dir(&mut self, p: &Path) -> io::Result<()> {
        self.step()?;
        let mut s = self.0.borrow_mut();
        if !s.dirs.contains(p) {
            return Err(io::ErrorKind::NotFound.into());
        }
        let renames: Vec<_> = s
            .renames
            .iter()
            .filter(|(_, b)| b.parent() == Some(p))
            .cloned()
            .collect();
        for (a, b) in renames {
            let entries: Vec<_> = s
                .durable_entries
                .iter()
                .filter_map(|(q, i)| q.strip_prefix(&a).ok().map(|r| (q.clone(), b.join(r), *i)))
                .collect();
            for (old, new, i) in entries {
                s.durable_entries.remove(&old);
                s.durable_entries.insert(new, i);
            }
            let dirs: Vec<_> = s
                .durable_dirs
                .iter()
                .filter_map(|q| q.strip_prefix(&a).ok().map(|r| (q.clone(), b.join(r))))
                .collect();
            for (old, new) in dirs {
                s.durable_dirs.remove(&old);
                s.durable_dirs.insert(new);
            }
            s.renames.retain(|x| x != &(a.clone(), b.clone()));
        }
        s.durable_entries.retain(|k, _| k.parent() != Some(p));
        let entries: Vec<_> = s
            .entries
            .iter()
            .filter(|(k, _)| k.parent() == Some(p))
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        s.durable_entries.extend(entries);
        s.durable_dirs.retain(|k| k.parent() != Some(p));
        let dirs: Vec<_> = s
            .dirs
            .iter()
            .filter(|k| k.parent() == Some(p))
            .cloned()
            .collect();
        s.durable_dirs.extend(dirs);
        Ok(())
    }
    fn remove(&mut self, p: &Path) -> io::Result<()> {
        self.step()?;
        self.0
            .borrow_mut()
            .entries
            .remove(p)
            .ok_or(io::ErrorKind::NotFound)?;
        Ok(())
    }
    fn remove_dir(&mut self, p: &Path) -> io::Result<()> {
        self.step()?;
        let mut s = self.0.borrow_mut();
        if !s.dirs.contains(p) {
            return Err(io::ErrorKind::NotFound.into());
        }
        let occupied = s
            .entries
            .keys()
            .chain(s.dirs.iter())
            .any(|entry| entry.parent() == Some(p));
        if occupied {
            return Err(io::ErrorKind::DirectoryNotEmpty.into());
        }
        s.dirs.remove(p);
        Ok(())
    }
    fn lock(&mut self, p: &Path, _: Duration) -> Result<MemoryLock, LockError> {
        self.step().map_err(LockError::Io)?;
        let mut s = self.0.borrow_mut();
        if !s.entries.contains_key(p) {
            return Err(LockError::Io(io::ErrorKind::NotFound.into()));
        }
        if !s.locks.insert(p.to_owned()) {
            return Err(LockError::Busy {
                waited: Duration::ZERO,
            });
        }
        Ok(MemoryLock {
            fs: self.clone(),
            path: p.to_owned(),
        })
    }
}
pub struct MemoryLock {
    fs: MemoryFs,
    path: PathBuf,
}
impl Drop for MemoryLock {
    fn drop(&mut self) {
        self.fs.0.borrow_mut().locks.remove(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn exercise(fs: &mut impl DurableFs) {
        fs.create_dir(Path::new("tmp")).unwrap();
        fs.create_dir(Path::new("events")).unwrap();
        let mut f = fs.create_private(Path::new("tmp/a")).unwrap();
        f.write_all(b"one").unwrap();
        let gate = WriteGate::isolated();
        sync_file(&mut f, &gate).unwrap();
        drop(f);
        publish(fs, Path::new("tmp/a"), Path::new("events/1"), &gate).unwrap();
        let mut value = Vec::new();
        fs.read(Path::new("events/1"))
            .unwrap()
            .read_to_end(&mut value)
            .unwrap();
        assert_eq!(value, b"one");
        assert!(
            matches!(publish(fs, Path::new("tmp/a"), Path::new("events/1"), &gate), Err(DurableError::Io(e)) if e.kind() == io::ErrorKind::AlreadyExists)
        );
        let mut second = Vec::new();
        fs.read(Path::new("events/1"))
            .unwrap()
            .read_to_end(&mut second)
            .unwrap();
        assert_eq!(second, b"one");
    }
    #[test]
    fn shared_fake_and_local() {
        exercise(&mut MemoryFs::new());
        let root = std::env::temp_dir().join(format!("inv-durable-test-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        exercise(&mut LocalFs::new(&root));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn fake_drops_unsynced_entry_and_stops_after_sync_failure() {
        let mut fs = MemoryFs::new();
        fs.create_dir(Path::new("tmp")).unwrap();
        fs.sync_dir(Path::new("")).unwrap();
        let mut f = fs.create_private(Path::new("tmp/a")).unwrap();
        f.write_all(b"data").unwrap();
        f.sync_all().unwrap();
        drop(f);
        fs.crash();
        assert_eq!(
            fs.read(Path::new("tmp/a")).err().unwrap().kind(),
            io::ErrorKind::NotFound
        );
        let mut f = fs.create_private(Path::new("tmp/b")).unwrap();
        f.write_all(b"x").unwrap();
        fs.fail_at(1);
        let gate = WriteGate::isolated();
        assert!(matches!(
            sync_file(&mut f, &gate),
            Err(DurableError::SyncUnknown(_))
        ));
        assert!(matches!(
            sync_file(&mut f, &gate),
            Err(DurableError::Stopped)
        ));
    }
    #[test]
    fn memory_lock_is_exclusive_and_released() {
        let mut fs = MemoryFs::new();
        drop(fs.create_private(Path::new("lock")).unwrap());
        let lock = fs.lock(Path::new("lock"), Duration::ZERO).unwrap();
        assert!(matches!(
            fs.lock(Path::new("lock"), Duration::ZERO),
            Err(LockError::Busy { .. })
        ));
        drop(lock);
        assert!(fs.lock(Path::new("lock"), Duration::ZERO).is_ok());
    }
    #[test]
    fn crash_boundaries_preserve_only_complete_published_bytes() {
        for sync_target in [false, true] {
            let mut fs = MemoryFs::new();
            fs.create_dir(Path::new("tmp")).unwrap();
            fs.create_dir(Path::new("events")).unwrap();
            fs.sync_dir(Path::new("")).unwrap();
            let mut file = fs.create_private(Path::new("tmp/a")).unwrap();
            file.write_all(b"complete").unwrap();
            file.sync_all().unwrap();
            drop(file);
            fs.link_no_replace(Path::new("tmp/a"), Path::new("events/1"))
                .unwrap();
            if sync_target {
                fs.sync_dir(Path::new("events")).unwrap();
            }
            fs.crash();
            match fs.read(Path::new("events/1")) {
                Ok(mut input) => {
                    let mut bytes = Vec::new();
                    input.read_to_end(&mut bytes).unwrap();
                    assert!(sync_target);
                    assert_eq!(bytes, b"complete");
                }
                Err(e) => {
                    assert!(!sync_target);
                    assert_eq!(e.kind(), io::ErrorKind::NotFound);
                }
            }
        }
    }
}
