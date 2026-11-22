#![no_main]

use investigation_core::{
    command::Command,
    input::MAX_REQUEST_BYTES,
    wire::{CommitStatus, ErrorCode},
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    match Command::parse(bytes) {
        Ok((command, request)) => {
            assert!(bytes.len() <= MAX_REQUEST_BYTES);
            assert!(request.input.is_object());
            command
                .validate_shape()
                .expect("parsed envelope satisfies the same shape rules");
        }
        Err(failure) => {
            assert_eq!(failure.commit_status, CommitStatus::NotCommitted);
            assert!(matches!(
                failure.error.code,
                ErrorCode::InvalidInput | ErrorCode::EmptyTransaction
            ));
            let path = failure
                .error
                .path
                .as_deref()
                .expect("typed rejection has a location");
            assert!(path.is_empty() || path.starts_with('/'));
        }
    }
});
