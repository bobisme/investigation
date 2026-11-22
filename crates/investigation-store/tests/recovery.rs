use std::{
    fs::{self, File, FileTimes},
    time::{Duration, SystemTime},
};

use investigation_store::{Store, Workspace};
use serde_json::json;

#[test]
fn restart_reclaims_only_expired_publication_temporaries() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    Workspace::init(&root, false).unwrap();
    let state = root.join("state");
    let unpublished = state.join("tmp/create-tx_019acd16-0000-7000-8000-000000000001");
    fs::create_dir_all(unpublished.join("events")).unwrap();
    fs::write(unpublished.join("events/000000000001.json"), b"unpublished").unwrap();
    let orphan = state.join("tmp/event-tx_019acd16-0000-7000-8000-000000000002.tmp");
    fs::write(&orphan, b"unpublished append").unwrap();
    let live = state.join("tmp/event-tx_019acd16-0000-7000-8000-000000000003.tmp");
    fs::write(&live, b"within grace").unwrap();
    let import = state.join("tmp/import-import_019acd16-0000-7000-8000-000000000004.tmp");
    fs::write(&import, b"may be open by another writer").unwrap();
    let staged = state.join("staging/capture");
    fs::write(&staged, b"producer recovery evidence").unwrap();
    let blob = state.join("blobs/sha256/unreferenced");
    fs::write(&blob, b"policy-controlled retention").unwrap();
    let unknown = state.join("tmp/create-tx_019acd16-0000-7000-8000-000000000005");
    fs::create_dir(&unknown).unwrap();
    let unexpected = unknown.join("unexpected-data");
    fs::write(&unexpected, b"preserve unknown data").unwrap();
    for path in [&unpublished, &orphan, &import, &staged, &blob, &unknown] {
        let expired = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        File::open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(expired))
            .unwrap();
    }
    let mut restarted = Store::open(Workspace::open(&root).unwrap());
    assert!(restarted.check().unwrap().integrity_ok());
    assert!(orphan.exists()); // read-only checks do not reclaim
    let request = json!({
        "schema_version":"inv.command.v0-alpha", "command_id":"after-restart",
        "actor":"agent:recovery", "case_id":null, "preconditions":[],
        "operations":[{"op":"case.create","payload":{"title":"Verify safe recovery"}}],
    });
    let receipt = restarted
        .record(&serde_json::to_vec(&request).unwrap())
        .unwrap();
    assert_eq!(receipt.snapshot.sequence, 1);
    assert!(!orphan.exists());
    assert!(!unpublished.exists());
    for path in [live, import, staged, blob, unexpected] {
        assert!(path.is_file(), "lost {}", path.display());
    }
}
