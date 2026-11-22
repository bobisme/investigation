#![no_main]

use std::path::Path;

use investigation_core::wire::{ErrorCode, Failure};
use investigation_store::journal::{decode_event, hash_bytes};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let path = Path::new("case/events/000000000002.json");
    match decode_event(path, 2, bytes) {
        Ok(verified) => {
            assert_eq!(verified.event.sequence, 2);
            assert_eq!(verified.hash, hash_bytes(bytes));
        }
        Err(error) => {
            let code = error.kind.code();
            assert_eq!(error.object, path);
            let diagnostic = Failure::from(error);
            assert_eq!(diagnostic.error.code, ErrorCode::IntegrityFailure);
            assert_eq!(diagnostic.error.actual.unwrap()["diagnostic_code"], code);
            assert!(diagnostic.error.repair.is_none());
        }
    }
});
