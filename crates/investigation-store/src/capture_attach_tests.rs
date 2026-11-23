//! Attachment tests. A child of `capture::attach`, so the crash schedules
//! drive the same private copy and publication path as production.
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

use super::*;
use crate::store::capture::items::{ItemResult, SEAL_FILE, tests as item_tests};
use item_tests::{Ticks, code_and_path, declaration};

fn with_imports(root: &Path, imports: &Path) -> Workspace {
    let config = root.join("investigation.toml");
    let contents = fs::read_to_string(&config).unwrap();
    assert!(contents.contains("import_roots = []"));
    fs::write(
        &config,
        contents.replace(
            "import_roots = []",
            &format!("import_roots = [{:?}]", imports.display().to_string()),
        ),
    )
    .unwrap();
    Workspace::open(root).unwrap()
}

struct Fixture {
    _temporary: tempfile::TempDir,
    imports: PathBuf,
    store: Store,
    declared: DeclaredCapture,
}

fn fixture() -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let imports = temporary.path().join("outputs");
    fs::create_dir(&imports).unwrap();
    Workspace::init(&root, false).unwrap();
    let mut store = Store::open(with_imports(&root, &imports));
    let declared = store.declare_capture(&declaration()).unwrap();
    Fixture {
        _temporary: temporary,
        imports,
        store,
        declared,
    }
}

fn attachment(handle: Id, key: &str, position: u64, source: AttachSource) -> AttachSubmission {
    let source_value = match &source {
        AttachSource::File(path) => json!({"kind": "file", "path": path}),
        AttachSource::Missing => json!({"kind": "missing", "reason": "not produced"}),
    };
    AttachSubmission {
        item: ItemSubmission {
            operation: ItemOperation::Attach,
            handle,
            key: key.into(),
            position,
            request: json!({
                "handle": handle,
                "key": key,
                "position": position,
                "role": "original-output",
                "source": source_value,
            }),
            outcome: None,
        },
        source,
        expected_digest: None,
        expected_size_bytes: None,
        classification: None,
        derived_from: None,
    }
}

fn file(handle: Id, key: &str, position: u64, path: &str) -> AttachSubmission {
    attachment(handle, key, position, AttachSource::File(path.into()))
}

fn digest_of(bytes: &[u8]) -> Digest {
    hash_bytes(bytes)
}

impl Fixture {
    fn handle(&self) -> Id {
        self.declared.binding.handle
    }

    fn staging(&self) -> PathBuf {
        self.store
            .workspace
            .root()
            .join(&self.declared.binding.staging)
    }

    /// The staging directory as the store names it, relative to the root.
    fn relative_staging(&self) -> PathBuf {
        PathBuf::from(&self.declared.binding.staging)
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.imports.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn attach(&mut self, submission: &AttachSubmission) -> Result<ItemAcceptance, Failure> {
        self.store
            .stage_capture_attachment(submission, &mut Ticks(0))
    }

    fn blobs(&self) -> Vec<String> {
        let mut names: Vec<_> = match fs::read_dir(self.staging().join(BLOBS_DIRECTORY)) {
            Ok(entries) => entries
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect(),
            Err(_) => Vec::new(),
        };
        names.sort();
        names
    }

    fn positions(&mut self) -> Vec<u64> {
        let handle = self.handle();
        self.store
            .capture_items(handle)
            .unwrap()
            .items
            .iter()
            .map(|item| item.position)
            .collect()
    }
}

fn outcome(accepted: &ItemAcceptance) -> AttachOutcome {
    AttachOutcome::of(&accepted.item).unwrap().unwrap()
}

#[test]
fn an_attachment_takes_the_next_shared_position_and_stages_verified_read_only_bytes() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    let mut clock = Ticks(0);
    fixture
        .store
        .stage_capture_item(
            &ItemSubmission {
                operation: ItemOperation::Append,
                handle,
                key: "started".into(),
                position: 1,
                request: json!({"fact": {"kind": "started"}}),
                outcome: None,
            },
            &mut clock,
        )
        .unwrap();
    let bytes = b"{\"latency_ms\": 18.7}\n";
    fixture.write("result.json", bytes);
    let mut submission = file(handle, "result", 2, "result.json");
    submission.expected_digest = Some(digest_of(bytes));
    submission.expected_size_bytes = Some(bytes.len() as u64);
    let accepted = fixture
        .store
        .stage_capture_attachment(&submission, &mut clock)
        .unwrap();
    assert_eq!(accepted.result, ItemResult::Accepted);
    assert_eq!(accepted.item.position, 2);
    assert_eq!(accepted.item.operation, ItemOperation::Attach);
    let staged = outcome(&accepted);
    assert_eq!(staged.availability, AttachAvailability::Staged);
    assert_eq!(staged.digest, Some(digest_of(bytes)));
    assert_eq!(staged.size_bytes, Some(bytes.len() as u64));
    assert_eq!(staged.classification, ClassificationLevel::Internal);

    let path = fixture
        .store
        .staged_attachment(&fixture.relative_staging(), &staged)
        .unwrap()
        .unwrap();
    assert_eq!(
        path,
        fixture
            .relative_staging()
            .join(staged.blob.as_ref().unwrap())
    );
    let path = fixture.store.workspace.root().join(path);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let metadata = fs::metadata(&path).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o400);
    // A copy, never a link to the producer's file.
    assert_eq!(std::os::unix::fs::MetadataExt::nlink(&metadata), 1);
    assert_eq!(fixture.blobs(), [blob_hex(&digest_of(bytes))]);

    // A later change to the producer's file does not touch the retained copy,
    // and an identical retry returns the first outcome without copying again.
    fixture.write("result.json", b"rewritten");
    let retry = fixture.attach(&submission).unwrap();
    assert_eq!(retry.result, ItemResult::AlreadyAccepted);
    assert_eq!(retry.item, accepted.item);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fixture.positions(), [1, 2]);
}

#[test]
fn a_file_that_changes_during_the_copy_is_refused_and_its_position_stays_free() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    // Larger than one read buffer, so the copy observes the file mid-stream.
    let original = vec![b'a'; 200 * 1024];
    let path = fixture.write("trace.log", &original);
    let submission = file(handle, "trace", 1, "trace.log");

    // The writer is still appending.
    let mut appended = false;
    let error = fixture
        .store
        .stage_attachment_observed(&submission, &mut Ticks(0), &mut |copied| {
            if !appended && copied >= 64 * 1024 {
                appended = true;
                let mut writer = fs::OpenOptions::new().append(true).open(&path).unwrap();
                writer.write_all(b"more output\n").unwrap();
            }
        })
        .unwrap_err();
    assert!(appended);
    assert_eq!(
        code_and_path(&error),
        (ErrorCode::RevisionConflict, Some("/source/path"))
    );
    assert!(error.error.retry.is_some());

    // The writer rewrites bytes it already wrote, keeping the size.
    fs::write(&path, &original).unwrap();
    let mut rewritten = false;
    let error = fixture
        .store
        .stage_attachment_observed(&submission, &mut Ticks(0), &mut |copied| {
            if !rewritten && copied >= 64 * 1024 {
                rewritten = true;
                let mut writer = fs::OpenOptions::new().write(true).open(&path).unwrap();
                writer.write_all(b"b").unwrap();
            }
        })
        .unwrap_err();
    assert!(rewritten);
    assert_eq!(error.error.code, ErrorCode::RevisionConflict);

    // Nothing was staged: no item, no blob, no temporary.
    assert!(fixture.positions().is_empty());
    assert!(fixture.blobs().is_empty());

    // Once the producer has closed the file, the identical request succeeds
    // at the same position.
    let accepted = fixture.attach(&submission).unwrap();
    assert_eq!(accepted.item.position, 1);
    let mut settled = original;
    settled[0] = b'b';
    assert_eq!(outcome(&accepted).digest, Some(digest_of(&settled)));
}

#[test]
fn a_digest_or_size_mismatch_rejects_the_item_and_leaves_the_handle_usable() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    fixture.write("result.json", b"actual bytes");

    let mut wrong_digest = file(handle, "result", 1, "result.json");
    wrong_digest.expected_digest = Some(digest_of(b"other bytes"));
    let error = fixture.attach(&wrong_digest).unwrap_err();
    assert_eq!(
        code_and_path(&error),
        (ErrorCode::InvalidInput, Some("/expected_digest"))
    );
    assert_eq!(
        error.error.actual.as_ref().unwrap()["digest"],
        json!(digest_of(b"actual bytes"))
    );

    let mut wrong_size = file(handle, "result", 1, "result.json");
    wrong_size.expected_size_bytes = Some(3);
    let error = fixture.attach(&wrong_size).unwrap_err();
    assert_eq!(
        code_and_path(&error),
        (ErrorCode::InvalidInput, Some("/expected_size_bytes"))
    );
    assert!(fixture.positions().is_empty());
    assert!(fixture.blobs().is_empty());

    // The key was never accepted, so the corrected request takes position 1.
    let mut corrected = file(handle, "result", 1, "result.json");
    corrected.expected_digest = Some(digest_of(b"actual bytes"));
    assert_eq!(fixture.attach(&corrected).unwrap().item.position, 1);
    let next = file(handle, "copy", 2, "result.json");
    let second = fixture.attach(&next).unwrap();
    assert_eq!(second.item.position, 2);
    // Identical bytes share one staged blob.
    assert_eq!(fixture.blobs().len(), 1);
}

#[test]
fn missing_artifacts_stay_missing_and_are_never_replaced_by_empty_files() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    let declared = fixture
        .attach(&attachment(handle, "summary", 1, AttachSource::Missing))
        .unwrap();
    let declared = outcome(&declared);
    assert_eq!(declared.availability, AttachAvailability::Missing);
    assert_eq!(declared.missing, Some(MissingCause::Declared));
    assert_eq!((declared.digest, declared.blob), (None, None));

    let absent = fixture
        .attach(&file(handle, "trace", 2, "trace.log"))
        .unwrap();
    let absent_outcome = outcome(&absent);
    assert_eq!(absent_outcome.missing, Some(MissingCause::NotFound));
    assert!(fixture.blobs().is_empty());

    // The file appearing later does not turn the accepted item into bytes.
    fixture.write("trace.log", b"late");
    let retry = fixture
        .attach(&file(handle, "trace", 2, "trace.log"))
        .unwrap();
    assert_eq!(retry.result, ItemResult::AlreadyAccepted);
    assert_eq!(outcome(&retry).missing, Some(MissingCause::NotFound));
    assert!(fixture.blobs().is_empty());
}

#[test]
fn sources_outside_an_import_root_or_reached_through_links_are_refused() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    let outside = fixture.imports.parent().unwrap().join("secret");
    fs::write(&outside, b"secret").unwrap();
    symlink(&outside, fixture.imports.join("link")).unwrap();
    fs::hard_link(&outside, fixture.imports.join("hard")).unwrap();
    fs::create_dir(fixture.imports.join("directory")).unwrap();
    let inside = fixture.write("inside.txt", b"inside");

    let refused = |fixture: &mut Fixture, path: &str| {
        let error = fixture.attach(&file(handle, "item", 1, path)).unwrap_err();
        assert_eq!(
            code_and_path(&error),
            (ErrorCode::InvalidInput, Some("/source/path")),
            "{path}: {}",
            error.error.message
        );
        error.error.message.clone()
    };
    assert!(refused(&mut fixture, "link").contains("symlink"));
    assert!(refused(&mut fixture, "hard").contains("hard link"));
    assert!(refused(&mut fixture, "directory").contains("regular file"));
    assert!(refused(&mut fixture, "../secret").contains(".."));
    assert!(refused(&mut fixture, outside.to_str().unwrap()).contains("outside"));
    assert!(fixture.positions().is_empty());

    // An absolute path beneath the root is accepted.
    let accepted = fixture
        .attach(&file(handle, "item", 1, inside.to_str().unwrap()))
        .unwrap();
    assert_eq!(outcome(&accepted).size_bytes, Some(6));
}

#[test]
fn a_workspace_without_import_roots_refuses_file_sources() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    let mut store = Store::open(Workspace::init(&root, false).unwrap());
    let handle = store
        .declare_capture(&declaration())
        .unwrap()
        .binding
        .handle;
    fs::write(root.join("result.json"), b"x").unwrap();
    let error = store
        .stage_capture_attachment(&file(handle, "r", 1, "result.json"), &mut Ticks(0))
        .unwrap_err();
    assert_eq!(
        code_and_path(&error),
        (ErrorCode::InvalidInput, Some("/source/path"))
    );
    assert!(error.error.message.contains("import_roots"));
    // A declared-missing artifact needs no root.
    store
        .stage_capture_attachment(
            &attachment(handle, "m", 1, AttachSource::Missing),
            &mut Ticks(0),
        )
        .unwrap();
}

#[test]
fn derivatives_name_an_earlier_retained_attachment() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    fixture
        .store
        .stage_capture_item(
            &ItemSubmission {
                operation: ItemOperation::Append,
                handle,
                key: "started".into(),
                position: 1,
                request: json!({"fact": {"kind": "started"}}),
                outcome: None,
            },
            &mut Ticks(0),
        )
        .unwrap();
    fixture.write("trace.log", b"trace");
    fixture
        .attach(&file(handle, "trace", 2, "trace.log"))
        .unwrap();
    fixture
        .attach(&attachment(handle, "lost", 3, AttachSource::Missing))
        .unwrap();
    fixture.write("summary.txt", b"summary");

    let derived = |from: &str| {
        let mut submission = file(handle, "summary", 4, "summary.txt");
        submission.derived_from = Some(from.into());
        submission
    };
    for (from, code) in [
        ("absent", ErrorCode::UnknownReference),
        ("started", ErrorCode::InvalidInput),
        ("lost", ErrorCode::InvalidInput),
    ] {
        let error = fixture.attach(&derived(from)).unwrap_err();
        assert_eq!(
            code_and_path(&error),
            (code, Some("/derived_from/from")),
            "{from}"
        );
    }
    let accepted = fixture.attach(&derived("trace")).unwrap();
    assert_eq!(
        outcome(&accepted).derived_from,
        Some(DerivedFrom {
            key: "trace".into(),
            position: 2
        })
    );
}

#[test]
fn classification_may_be_stricter_than_the_run_never_looser() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    fixture.write("result.json", b"x");
    let mut looser = file(handle, "result", 1, "result.json");
    looser.classification = Some(ClassificationLevel::Public);
    let error = fixture.attach(&looser).unwrap_err();
    assert_eq!(
        code_and_path(&error),
        (ErrorCode::InvalidInput, Some("/classification"))
    );
    let mut stricter = file(handle, "result", 1, "result.json");
    stricter.classification = Some(ClassificationLevel::Restricted);
    let accepted = fixture.attach(&stricter).unwrap();
    assert_eq!(
        outcome(&accepted).classification,
        ClassificationLevel::Restricted
    );
}

#[test]
fn a_sealed_handle_refuses_new_attachments_but_replays_accepted_ones() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    fixture.write("result.json", b"x");
    let first = fixture
        .attach(&file(handle, "result", 1, "result.json"))
        .unwrap();
    fs::write(fixture.staging().join(SEAL_FILE), b"{}\n").unwrap();
    let error = fixture
        .attach(&file(handle, "late", 2, "result.json"))
        .unwrap_err();
    assert_eq!(
        code_and_path(&error),
        (ErrorCode::RevisionConflict, Some("/handle"))
    );
    let retry = fixture
        .attach(&file(handle, "result", 1, "result.json"))
        .unwrap();
    assert_eq!(retry.item, first.item);
}

#[test]
fn a_tampered_staged_blob_fails_verification() {
    let mut fixture = fixture();
    let handle = fixture.handle();
    fixture.write("result.json", b"original");
    let accepted = fixture
        .attach(&file(handle, "result", 1, "result.json"))
        .unwrap();
    let staged = outcome(&accepted);
    let path = fixture.staging().join(staged.blob.as_ref().unwrap());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&path, b"tampered").unwrap();
    let error = fixture
        .store
        .staged_attachment(&fixture.relative_staging(), &staged)
        .unwrap_err();
    assert_eq!(error.error.code, ErrorCode::IntegrityFailure);
}

/// Inject a failure at every filesystem call of an attachment, then crash.
/// Every acknowledged attachment must survive with its bytes, and an identical
/// retry must settle the uncertain one without duplicating it.
#[test]
fn every_acknowledged_attachment_survives_a_crash_with_its_bytes() {
    let imports = tempfile::tempdir().unwrap();
    let bytes = vec![7_u8; 100 * 1024];
    fs::write(imports.path().join("result.bin"), &bytes).unwrap();
    fs::write(imports.path().join("first.bin"), b"first").unwrap();
    let mut injected = 0;
    let mut acknowledged_first_try = 0;
    for seed in 1..=80 {
        let (mut store, root) = item_tests::memory_store();
        store.workspace = with_imports(&root, imports.path());
        let declared = store.declare_capture(&declaration()).unwrap();
        let handle = declared.binding.handle;
        let staging = PathBuf::from(&declared.binding.staging);
        let mut clock = Ticks(0);
        let first = store
            .stage_capture_attachment(&file(handle, "first", 1, "first.bin"), &mut clock)
            .unwrap();
        item_tests::restart(&mut store);

        let submission = file(handle, "result", 2, "result.bin");
        store.fs.fail_at(seed);
        let second = match store.stage_capture_attachment(&submission, &mut clock) {
            Ok(accepted) => {
                acknowledged_first_try += 1;
                Some(accepted)
            }
            Err(_) => {
                injected += 1;
                None
            }
        };
        item_tests::restart(&mut store);
        let retried = store
            .stage_capture_attachment(&submission, &mut Ticks(1000))
            .unwrap();
        if let Some(second) = &second {
            assert_eq!(retried.result, ItemResult::AlreadyAccepted, "seed {seed}");
            assert_eq!(retried.item, second.item, "seed {seed}");
        }
        item_tests::restart(&mut store);

        let staged = store.capture_items(handle).unwrap();
        assert_eq!(staged.items.len(), 2, "seed {seed}");
        assert_eq!(staged.items[0], first.item, "seed {seed}");
        assert_eq!(staged.items[1], retried.item, "seed {seed}");
        for (item, expected) in staged.items.iter().zip([&b"first"[..], &bytes]) {
            let outcome = AttachOutcome::of(item).unwrap().unwrap();
            let path = store
                .staged_attachment(&staging, &outcome)
                .unwrap_or_else(|error| panic!("seed {seed}: {error:?}"))
                .unwrap();
            let mut retained = Vec::new();
            store
                .fs
                .read(&path)
                .unwrap()
                .read_to_end(&mut retained)
                .unwrap();
            assert_eq!(retained, expected, "seed {seed}");
        }
        let third = store
            .stage_capture_attachment(&file(handle, "third", 3, "first.bin"), &mut clock)
            .unwrap();
        assert_eq!(third.item.position, 3, "seed {seed}");
        let _ = fs::remove_dir_all(root);
    }
    assert!(injected > 10, "the schedule must reach the attach path");
    assert!(acknowledged_first_try > 0, "the schedule must run past it");
}
