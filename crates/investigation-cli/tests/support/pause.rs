//! The parent side of the test-pause handshake (bn-38ue, bn-2uoc).
//!
//! A process paused by `investigation_store::test_pause` or the restore test
//! pause marks a ready file with the name of the point it reached. The child
//! writes a temporary file and renames it onto the ready path, so the file
//! appears with its whole content. The parent still waits for the complete
//! token rather than for the file to exist: an empty or partly written file
//! is not "ready". A complete name of another point fails at once.
//!
//! Every failure kills and reaps the child first, so a failed wait never
//! leaves a paused process behind holding the test's output pipes, and its
//! message says what the ready file held when the wait gave up.
#![allow(dead_code)]

use std::{
    fs, io,
    path::Path,
    process::Child,
    time::{Duration, Instant},
};

/// How long a debug-build CLI child may take to reach its pause point on a
/// loaded machine.
pub const PATIENCE: Duration = Duration::from_secs(120);

/// Wait until `ready` holds exactly `point`, for at most [`PATIENCE`]. See
/// [`wait_for_ready_within`].
pub fn wait_for_ready(child: &mut Child, ready: &Path, point: &str, what: &str) {
    wait_for_ready_within(child, ready, point, what, PATIENCE);
}

/// Wait until `ready` holds exactly `point`. Kill `child` and panic when it
/// exits first, when the file names another point or cannot be read, or
/// after `patience`. `what` names the command in a failure.
pub fn wait_for_ready_within(
    child: &mut Child,
    ready: &Path,
    point: &str,
    what: &str,
    patience: Duration,
) {
    let started = Instant::now();
    loop {
        let seen = match fs::read_to_string(ready) {
            Ok(content) if content == point => return,
            // Empty or partly written: the child has not finished marking it.
            Ok(content) if point.starts_with(&content) => Some(content),
            Ok(content) => fail(
                child,
                &format!("{what} paused at {content:?}, not at {point:?}"),
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => fail(child, &format!("reading {}: {error}", ready.display())),
        };
        let held = match &seen {
            Some(content) => format!("the ready file held {content:?}"),
            None => "there was no ready file".to_owned(),
        };
        match child.try_wait() {
            Ok(Some(status)) => fail(
                child,
                &format!("{what} exited with {status} before {point}; {held}"),
            ),
            Ok(None) => {}
            Err(error) => fail(child, &format!("waiting for {what}: {error}")),
        }
        if started.elapsed() >= patience {
            fail(
                child,
                &format!("{what} did not reach {point} within {patience:?}; {held}"),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Kill and reap `child`, then panic with `message`.
fn fail(child: &mut Child, message: &str) -> ! {
    let _ = child.kill();
    let _ = child.wait();
    panic!("{message}");
}

/// A child that is killed and reaped when the test leaves its scope, on
/// every path, a failed assertion included.
pub struct Reaped(pub Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
