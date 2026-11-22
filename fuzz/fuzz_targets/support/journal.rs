use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs::File,
    io::{self, Cursor, Read},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use investigation_core::{
    identity::Id,
    wire::{ErrorCode, Failure},
};
use investigation_store::{
    durable::{DurableFs, LockError},
    journal::{CaseGenesis, JournalError, event_filename},
};
use serde_json::json;

pub struct Fixture {
    pub workspace: Id,
    pub case: Id,
    pub files: BTreeMap<PathBuf, Vec<u8>>,
}

pub fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        // The disposable source workspace was created and appended using Store.
        // Preserve its exact bytes and link hashes.
        let bytes = include_bytes!("../../fixtures/w02-journal/genesis.json");
        let genesis: CaseGenesis = serde_json::from_slice(bytes).unwrap();
        let files = BTreeMap::from([
            (PathBuf::from("case/genesis.json"), bytes.to_vec()),
            (
                event_path(1),
                include_bytes!("../../fixtures/w02-journal/events/000000000001.json").to_vec(),
            ),
            (
                event_path(2),
                include_bytes!("../../fixtures/w02-journal/events/000000000002.json").to_vec(),
            ),
            (
                event_path(3),
                include_bytes!("../../fixtures/w02-journal/events/000000000003.json").to_vec(),
            ),
        ]);
        Fixture {
            workspace: genesis.workspace_id,
            case: genesis.case_id,
            files,
        }
    })
}

pub fn event_path(sequence: u64) -> PathBuf {
    Path::new("case/events").join(event_filename(sequence).unwrap())
}

pub struct ReadOnlyJournal {
    pub files: BTreeMap<PathBuf, Vec<u8>>,
    pub reads: RefCell<Vec<PathBuf>>,
}

impl DurableFs for ReadOnlyJournal {
    type File = File;
    type Guard = ();

    fn create_private(&mut self, _: &Path) -> io::Result<File> {
        panic!("replay attempted creation")
    }
    fn create_dir(&mut self, _: &Path) -> io::Result<()> {
        panic!("replay attempted a directory mutation")
    }
    fn sync_existing(&mut self, _: &Path) -> io::Result<()> {
        panic!("replay attempted a file sync")
    }
    fn sync_dir(&mut self, _: &Path) -> io::Result<()> {
        panic!("replay attempted a directory sync")
    }
    fn link_no_replace(&mut self, _: &Path, _: &Path) -> io::Result<()> {
        panic!("replay attempted a link")
    }
    fn rename_no_replace(&mut self, _: &Path, _: &Path) -> io::Result<()> {
        panic!("replay attempted a rename")
    }
    fn remove(&mut self, _: &Path) -> io::Result<()> {
        panic!("replay attempted a removal")
    }
    fn lock(&mut self, _: &Path, _: Duration) -> Result<(), LockError> {
        panic!("pure replay attempted to lock")
    }

    fn read(&self, path: &Path) -> io::Result<Box<dyn Read>> {
        self.reads.borrow_mut().push(path.to_owned());
        self.files
            .get(path)
            .cloned()
            .map(|bytes| Box::new(Cursor::new(bytes)) as Box<dyn Read>)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    fn list(&self, directory: &Path) -> io::Result<Vec<PathBuf>> {
        assert_eq!(directory, Path::new("case/events"));
        Ok(self
            .files
            .keys()
            .filter(|path| path.parent() == Some(directory))
            .cloned()
            .collect())
    }
}

pub fn assert_diagnostic(error: JournalError) {
    let diagnostic = error.kind.code();
    let path = error.object.clone();
    let failure = Failure::from(error);
    assert_eq!(failure.error.code, ErrorCode::IntegrityFailure);
    assert_eq!(
        failure.error.actual.unwrap(),
        json!({"diagnostic_code":diagnostic,"object":path})
    );
    assert!(failure.error.repair.is_none());
}
