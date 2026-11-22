//! Exercise LocalFs publication at deterministic boundaries. The optional
//! LazyFS controller additionally drops the filesystem's userspace cache.

use super::{
    super::{
        blob,
        durable::{CommitLock, DurableFile, LockError},
        workspace::{FilesystemKind, WorkspaceError},
    },
    *,
};
use std::{
    cell::RefCell,
    fs::{self, File},
    io::Read,
    os::unix::fs::FileTypeExt,
    rc::Rc,
    thread,
    time::Instant,
};

const SCENARIOS: [(&str, Boundary); 3] = [
    ("event-file", Boundary::EventFile),
    ("event-directory", Boundary::EventDirectory),
    ("attachment", Boundary::Attachment),
];
const PAYLOAD: &[u8] = b"Durable attachment retained through journal recovery.\n";
const CAPTURE_STAGING: &str = "state/staging/qualification-capture";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Boundary {
    EventFile,
    EventDirectory,
    Attachment,
}

#[derive(Default)]
struct Witness {
    event_file_synced: bool,
    event_linked: bool,
    event_dir_synced: bool,
    attachment_file_synced: bool,
    attachment_parent_synced: bool,
    attachment_linked: Option<PathBuf>,
    attachment_dir_synced: bool,
    interrupted: bool,
}

#[derive(Clone)]
struct Control(PathBuf);

impl Control {
    fn drop_cache(&self) -> io::Result<()> {
        use rustix::fs::{Mode, OFlags, open};

        let command = self.0.join("faults.fifo");
        let completed = self.0.join("completed.fifo");
        for path in [&command, &completed] {
            if !fs::symlink_metadata(path)?.file_type().is_fifo() {
                return Err(io::Error::other("expected LazyFS control FIFOs"));
            }
        }
        let mut reply = File::from(open(
            &completed,
            OFlags::RDWR | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        )?);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut writer = loop {
            match open(
                &command,
                OFlags::WRONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW,
                Mode::empty(),
            ) {
                Ok(fd) => break File::from(fd),
                Err(rustix::io::Errno::NXIO) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.into()),
            }
        };
        writer.write_all(b"lazyfs::clear-cache\n")?;
        drop(writer);

        let mut data = Vec::new();
        let mut chunk = [0; 128];
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "LazyFS did not acknowledge cache loss",
                ));
            }
            match reply.read(&mut chunk) {
                Ok(n) => {
                    data.extend_from_slice(&chunk[..n]);
                    if data.ends_with(b"finished::clear-cache\n")
                        || data.ends_with(b"finished::clear-cache")
                    {
                        return Ok(());
                    }
                    if data.len() > 1024 {
                        return Err(io::Error::other("unexpected LazyFS completion reply"));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[derive(Clone)]
struct Fixture {
    boundary: Option<Boundary>,
    control: Option<Control>,
    observed: Rc<RefCell<Witness>>,
}

impl Fixture {
    fn new(boundary: Option<Boundary>, control: Option<Control>) -> Self {
        Self {
            boundary,
            control,
            observed: Default::default(),
        }
    }

    fn interrupt(&self, boundary: Boundary) -> io::Result<()> {
        if self.boundary == Some(boundary) {
            if let Some(control) = &self.control {
                control.drop_cache()?;
            }
            self.observed.borrow_mut().interrupted = true;
            return Err(io::Error::other(format!(
                "qualification interruption at {boundary:?}"
            )));
        }
        Ok(())
    }
}

struct QualifiedFile {
    file: File,
    fixture: Fixture,
    event: bool,
    attachment: bool,
}

impl Write for QualifiedFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl DurableFile for QualifiedFile {
    fn sync_all(&mut self) -> io::Result<()> {
        if self.event {
            assert!(
                self.file.metadata()?.len() > 0,
                "event staged bytes must precede fsync"
            );
            self.fixture.interrupt(Boundary::EventFile)?;
        }
        self.file.sync_all()?;
        let mut observed = self.fixture.observed.borrow_mut();
        observed.event_file_synced |= self.event;
        observed.attachment_file_synced |= self.attachment;
        Ok(())
    }
    fn set_readonly(&mut self) -> io::Result<()> {
        DurableFile::set_readonly(&mut self.file)
    }
}

struct QualifiedFs {
    fs: LocalFs,
    fixture: Fixture,
}

impl DurableFs for QualifiedFs {
    type File = QualifiedFile;
    type Guard = CommitLock;

    fn create_private(&mut self, path: &Path) -> io::Result<QualifiedFile> {
        let event = path.starts_with(TMP)
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("event-"));
        if event && self.fixture.boundary == Some(Boundary::Attachment) {
            assert!(
                self.fixture.observed.borrow().attachment_dir_synced,
                "attachment publication must sync its directory before event staging"
            );
            self.fixture.interrupt(Boundary::Attachment)?;
        }
        Ok(QualifiedFile {
            file: self.fs.create_private(path)?,
            fixture: self.fixture.clone(),
            event,
            attachment: path == Path::new("state/tmp/qualification-blob.tmp"),
        })
    }
    fn create_dir(&mut self, path: &Path) -> io::Result<()> {
        self.fs.create_dir(path)
    }
    fn read(&self, path: &Path) -> io::Result<Box<dyn Read>> {
        self.fs.read(path)
    }
    fn sync_existing(&mut self, path: &Path) -> io::Result<()> {
        self.fs.sync_existing(path)
    }
    fn list(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.fs.list(path)
    }
    fn rename_no_replace(&mut self, source: &Path, target: &Path) -> io::Result<()> {
        self.fs.rename_no_replace(source, target)
    }
    fn remove(&mut self, path: &Path) -> io::Result<()> {
        self.fs.remove(path)
    }

    fn link_no_replace(&mut self, source: &Path, target: &Path) -> io::Result<()> {
        let event = target.starts_with(CASES);
        let attachment = target.starts_with(BLOBS);
        {
            let observed = self.fixture.observed.borrow();
            if event {
                assert!(
                    observed.event_file_synced,
                    "event file fsync must precede its link"
                );
            }
            if attachment {
                assert!(
                    observed.attachment_file_synced,
                    "attachment data fsync must precede its link"
                );
                assert!(
                    observed.attachment_parent_synced,
                    "new attachment shard must sync its parent"
                );
            }
        }
        self.fs.link_no_replace(source, target)?;
        let mut observed = self.fixture.observed.borrow_mut();
        observed.event_linked |= event;
        if attachment {
            observed.attachment_linked = Some(target.to_owned());
        }
        Ok(())
    }

    fn sync_dir(&mut self, path: &Path) -> io::Result<()> {
        let event =
            path.starts_with(CASES) && path.file_name().is_some_and(|name| name == "events");
        if event {
            assert!(self.fixture.observed.borrow().event_linked);
            self.fixture.interrupt(Boundary::EventDirectory)?;
        }
        self.fs.sync_dir(path)?;
        let mut observed = self.fixture.observed.borrow_mut();
        observed.event_dir_synced |= event;
        observed.attachment_parent_synced |= path == Path::new(BLOBS);
        observed.attachment_dir_synced |= observed
            .attachment_linked
            .as_ref()
            .and_then(|file| file.parent())
            == Some(path);
        Ok(())
    }

    fn lock(&mut self, path: &Path, timeout: Duration) -> Result<Self::Guard, LockError> {
        self.fs.lock(path, timeout)
    }
}

fn command(case: Option<Id>, key: &str) -> (Command, SemanticRequest) {
    let operation = if case.is_some() {
        json!({"op":"question.create","payload":{"wording":"Which observation survives the fault?","importance":"normal"}})
    } else {
        json!({"op":"case.create","payload":{"title":"Scratch durability fixture","mode":"record"}})
    };
    let bytes = serde_json::to_vec(&json!({
        "schema_version":"inv.command.v0-alpha","command_id":key,"case_id":case,
        "actor":"agent:qualification","preconditions":[],"operations":[operation]
    }))
    .unwrap();
    Command::parse(&bytes).unwrap()
}

fn seed(root: &Path) {
    for (name, _) in SCENARIOS {
        let workspace = Workspace::init(&root.join(name), false).unwrap();
        let path = workspace.root().join(CAPTURE_STAGING);
        let mut capture = File::create(&path).unwrap();
        capture.write_all(PAYLOAD).unwrap();
        capture.sync_all().unwrap();
        File::open(path.parent().unwrap())
            .unwrap()
            .sync_all()
            .unwrap();
        let mut store = Store::open(workspace);
        let (command, request) = command(None, "init");
        store
            .commit(
                &command,
                request,
                &[],
                &mut SystemClock,
                &mut SystemIdSource,
            )
            .unwrap();
    }
}

fn open(root: &Path, control: Option<&Control>) -> Workspace {
    if control.is_some() {
        assert!(matches!(
            Workspace::open(root),
            Err(WorkspaceError::UnsupportedFilesystem {
                kind: FilesystemKind::Fuse,
                ..
            })
        ));
        Workspace::open_lazyfs_fixture(root).unwrap()
    } else {
        Workspace::open(root).unwrap()
    }
}

fn commit(
    store: &mut Store<QualifiedFs>,
    command: &Command,
    request: &SemanticRequest,
    with_attachment: bool,
) -> Result<Receipt, Failure> {
    let staged = with_attachment.then(|| {
        blob::stage(
            &mut store.fs,
            &store.gate,
            Path::new("state/tmp/qualification-blob.tmp"),
            PAYLOAD,
        )
        .unwrap()
    });
    let mut request = request.clone();
    if let Some(staged) = &staged {
        request
            .attachments
            .push(format!("sha256:{}", staged.digest).parse().unwrap());
    }
    store.commit(
        command,
        request,
        staged.as_slice(),
        &mut SystemClock,
        &mut SystemIdSource,
    )
}

fn qualification(root: &Path, control: Option<Control>) {
    for (name, boundary) in SCENARIOS {
        let workspace = open(&root.join(name), control.as_ref());
        let fixture = Fixture::new(Some(boundary), control.clone());
        let mut interrupted = Store {
            fs: QualifiedFs {
                fs: LocalFs::new(workspace.root()),
                fixture: fixture.clone(),
            },
            workspace: workspace.clone(),
            gate: WriteGate::isolated(),
            catalog: standard_catalog(),
        };
        let initial = interrupted.list_cases().unwrap().pop().unwrap();
        let (command, request) = command(Some(initial.case_id), "recoverable");
        let with_attachment = boundary == Boundary::Attachment;
        let error = commit(&mut interrupted, &command, &request, with_attachment).unwrap_err();
        assert!(
            fixture.observed.borrow().interrupted,
            "{boundary:?}: required checkpoint was not reached: {error}"
        );
        if boundary != Boundary::Attachment {
            assert_eq!(error.error.code, ErrorCode::DurabilityUnknown);
        }
        drop(interrupted);

        let clean_fixture = Fixture::new(None, None);
        let mut recovered = Store {
            fs: QualifiedFs {
                fs: LocalFs::new(workspace.root()),
                fixture: clean_fixture.clone(),
            },
            workspace,
            gate: WriteGate::isolated(),
            catalog: standard_catalog(),
        };
        let found = match recovered
            .receipt(&command.actor, &command.command_id)
            .unwrap()
        {
            ReceiptLookup::Found { receipt } => {
                assert_eq!(boundary, Boundary::EventDirectory);
                Some(receipt.transaction_id)
            }
            ReceiptLookup::NotFoundAtSnapshot { cases } => {
                assert_eq!(cases[0].snapshot, initial.snapshot);
                None
            }
        };
        if with_attachment {
            let blob = fixture.observed.borrow().attachment_linked.clone().unwrap();
            assert_eq!(
                fs::read(recovered.workspace.root().join(blob)).unwrap(),
                PAYLOAD
            );
        }
        let retry = commit(&mut recovered, &command, &request, with_attachment).unwrap();
        if let Some(found) = found {
            assert_eq!(retry.transaction_id, found);
        } else {
            assert!(
                clean_fixture.observed.borrow().event_dir_synced,
                "a new ack requires event-directory fsync"
            );
        }
        drop(recovered);
        if let Some(control) = &control {
            control.drop_cache().unwrap();
        }
        let workspace = open(&root.join(name), control.as_ref());
        let mut recovered = Store {
            workspace: workspace.clone(),
            fs: LocalFs::new(workspace.root()),
            gate: WriteGate::isolated(),
            catalog: standard_catalog(),
        };
        let replay = recovered.case(initial.case_id).unwrap();
        assert_eq!(replay.events.len(), 2, "{boundary:?}");
        assert_eq!(
            fs::read(recovered.workspace.root().join(CAPTURE_STAGING)).unwrap(),
            PAYLOAD
        );
        let ReceiptLookup::Found { receipt } = recovered
            .receipt(&command.actor, &command.command_id)
            .unwrap()
        else {
            panic!("acknowledged append disappeared at {boundary:?}");
        };
        assert_eq!(retry.transaction_id, receipt.transaction_id);
        assert_eq!(retry.created, receipt.created);
        println!("{name}: verified one append and the original receipt");
    }
}

#[test]
fn local_filesystem_publication_boundaries_and_retry() {
    let root = tempfile::tempdir().unwrap();
    seed(root.path());
    qualification(root.path(), None);
}

#[test]
#[ignore = "run with scripts/qualify-lazyfs.sh"]
fn lazyfs_seed() {
    let root = std::env::var_os("INV_LAZYFS_TEST_ROOT").expect("scratch backing directory");
    seed(Path::new(&root));
}

#[test]
#[ignore = "run with scripts/qualify-lazyfs.sh"]
fn lazyfs_publication_boundaries_and_retry() {
    let root = PathBuf::from(std::env::var_os("INV_LAZYFS_TEST_ROOT").expect("scratch mount"));
    let backing = PathBuf::from(
        std::env::var_os("INV_LAZYFS_TEST_BACKING").expect("scratch backing directory"),
    );
    let control = Control(PathBuf::from(
        std::env::var_os("INV_LAZYFS_TEST_CONTROL_DIR").expect("LazyFS FIFO directory"),
    ));
    assert_ne!(
        root.canonicalize().unwrap(),
        backing.canonicalize().unwrap()
    );
    open(&root.join("event-file"), Some(&control));

    let mut kept = File::create(root.join("synced-canary")).unwrap();
    kept.write_all(b"synced").unwrap();
    kept.sync_all().unwrap();
    let mut lost = File::create(root.join("unsynced-canary")).unwrap();
    lost.write_all(b"unsynced").unwrap();
    control.drop_cache().unwrap();
    assert_eq!(fs::read(backing.join("synced-canary")).unwrap(), b"synced");
    assert!(
        fs::read(backing.join("unsynced-canary"))
            .unwrap()
            .is_empty(),
        "LazyFS did not buffer and drop unsynced bytes"
    );

    qualification(&root, Some(control));
}
