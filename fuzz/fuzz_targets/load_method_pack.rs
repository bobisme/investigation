#![no_main]

use investigation_core::method_pack::{self, MAX_PACK_BYTES};
use libfuzzer_sys::fuzz_target;

// Arbitrary bytes are a hostile pack. Loading must never panic, must reject
// oversize input before parsing, and an accepted pack must be stable: the same
// bytes give the same digest and the same decoded value.
fuzz_target!(|bytes: &[u8]| {
    match method_pack::load(bytes) {
        Ok(loaded) => {
            assert!(bytes.len() <= MAX_PACK_BYTES);
            assert_eq!(loaded.byte_len(), bytes.len());
            let again = method_pack::load(bytes).unwrap();
            assert_eq!(again.digest(), loaded.digest());
            assert_eq!(again.pack(), loaded.pack());
            let value = serde_json::to_value(loaded.pack()).unwrap();
            assert!(value.is_object());
        }
        Err(error) => {
            if bytes.len() > MAX_PACK_BYTES {
                assert_eq!(error.kind, method_pack::PackErrorKind::TooLarge);
            }
            // Diagnostics are bounded and never carry pack text.
            assert!(error.message.len() < 256);
        }
    }
});
