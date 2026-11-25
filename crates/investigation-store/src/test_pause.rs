//! Points a test can stop a commit at, as a kill there would (bn-dla3).
//!
//! A build with debug assertions compiles in a test pause, the pattern
//! `archive restore` set (bn-f97f, ADR 0013). With
//! `INVESTIGATION_TEST_PAUSE_COMMIT=<point>`, a committing process that
//! reaches the point writes the point's name to the file
//! `INVESTIGATION_TEST_PAUSE_READY` names and waits while that file exists:
//! a test then kills it there, or removes the file to let the commit go on
//! (a race with another process). Without a ready file it waits until it is
//! killed. Release builds have no such code. The point's name is written to
//! a temporary file beside the ready file and renamed into place, so the
//! ready file never exists empty or part written (bn-38ue).
//!
//! The points are the commit stages ([`crate::diag::Stage`] names, at the
//! start of each stage, the first time it is entered) and
//! [`JOURNAL_LINKED`].

/// The commit point to pause at.
#[doc(hidden)]
pub const PAUSE_COMMIT: &str = "INVESTIGATION_TEST_PAUSE_COMMIT";
/// The file a paused process marks, and waits on.
#[doc(hidden)]
pub const PAUSE_READY: &str = "INVESTIGATION_TEST_PAUSE_READY";
/// The event is in the journal (linked, or its new case directory renamed
/// into place); the directory holding it is not synced yet.
#[doc(hidden)]
pub const JOURNAL_LINKED: &str = "journal_linked";

/// Whether this process has paused once: a resumed commit does not pause
/// again where it enters the same stage a second time.
#[cfg(debug_assertions)]
static PAUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Pause here if a test asked for `point`; see the module documentation.
pub(crate) fn at(point: &str) {
    #[cfg(debug_assertions)]
    if std::env::var(PAUSE_COMMIT).is_ok_and(|at| at == point)
        && !PAUSED.swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        let pause = std::time::Duration::from_millis(10);
        match std::env::var_os(PAUSE_READY) {
            Some(ready) => {
                let ready = std::path::PathBuf::from(ready);
                mark_ready(&ready, point);
                while ready.exists() {
                    std::thread::sleep(pause);
                }
            }
            None => loop {
                std::thread::sleep(pause);
            },
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = point;
}

/// Mark `ready` with `point` in one step: write a temporary file in the same
/// directory and rename it onto `ready`, so a reader sees no file or the
/// whole name, never an empty or partly written one. A test hook: a failure
/// leaves no ready file, and the test times out naming the point.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn mark_ready(ready: &std::path::Path, point: &str) {
    // One paused process marks a given ready file, so a fixed name is enough.
    let mut name = ready.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let temporary = ready.with_file_name(name);
    if std::fs::write(&temporary, point).is_ok() && std::fs::rename(&temporary, ready).is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
}
