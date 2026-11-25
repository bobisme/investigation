//! The parent side of the test-pause handshake (bn-38ue).
//!
//! A process paused by `investigation_store::test_pause` or the restore test
//! pause marks a ready file with the name of the point it reached. The child
//! writes a temporary file and renames it onto the ready path, so the file
//! appears with its whole content. The parent still waits for the complete
//! token rather than for the file to exist: an empty or partly written file
//! is not "ready". A complete name of another point fails at once.

use std::{
    fs, io,
    path::Path,
    process::Child,
    time::{Duration, Instant},
};

/// How long a child may take to reach its pause point.
const PATIENCE: Duration = Duration::from_secs(120);

/// Wait until `ready` holds exactly `point`. Panic when `child` exits first,
/// when the file names another point, or after [`PATIENCE`]. `what` names
/// the command in a failure.
pub fn wait_for_ready(child: &mut Child, ready: &Path, point: &str, what: &str) {
    let started = Instant::now();
    loop {
        match fs::read_to_string(ready) {
            Ok(content) if content == point => return,
            // Empty or partly written: the child has not finished marking it.
            Ok(content) if point.starts_with(&content) => {}
            Ok(content) => panic!("{what} paused at {content:?}, not at {point:?}"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("reading {}: {error}", ready.display()),
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("{what} exited with {status} before {point}");
        }
        assert!(started.elapsed() < PATIENCE, "{what} never reached {point}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
