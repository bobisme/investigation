#![no_main]

use investigation_core::{evidence::SelectorRequest, input};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    if let Ok(raw) = input::parse_request(bytes)
        && let Ok(request) = input::decode::<SelectorRequest>(&raw)
        && request.selector.validate_shape().is_ok()
    {
        let normalized = serde_json::to_value(&request).unwrap();
        let reparsed: SelectorRequest = input::decode(&normalized).unwrap();
        assert_eq!(reparsed, request);
        assert!(reparsed.selector.validate_shape().is_ok());
    }
});
