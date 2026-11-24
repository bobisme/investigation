//! Stat fingerprints: an unchanged owned file is recognized without reading it.
//!
//! A render with nothing to write used to read and hash every owned file to
//! prove it still held the bytes last generated. The manifest now keeps, for
//! an owned path, the [`Fingerprint`] of the file in which those bytes were
//! last observed: device, inode, size, modification time and status-change
//! time (`ctime`). When the file found at the path has exactly that
//! fingerprint, the publisher takes its bytes to be the owned hash without
//! reading them. Any difference, in any field, and the file is read and
//! hashed as before.
//!
//! **Why this keeps drift detection.** Every way to change a file's bytes
//! sets its `ctime` to the current time: a write, a truncation, an editor
//! that saves through a new file and a rename (which also changes the
//! inode), and a tool that restores the modification time afterwards (which
//! is itself a `ctime` change). No system call sets `ctime` to a chosen
//! value. So an edit that keeps the size and the modification time still
//! changes the fingerprint, and is hashed and diagnosed as before.
//!
//! **The limits, stated plainly.**
//!
//! - Timestamps have a granularity. An edit made in the same clock tick as
//!   the observation can leave `ctime` unchanged. A fingerprint is therefore
//!   recorded only when its `ctime` is at least [`SETTLED`] older than the
//!   start of the render that observed the bytes, and only when the file's
//!   status was the same before and after it was read. A file written or
//!   touched recently is hashed again next time, and recorded then.
//! - Only filesystems known to keep nanosecond `ctime` on every change use
//!   fingerprints ([`trusted`]: ext4, XFS, Btrfs and tmpfs); every other
//!   projection root is hashed in full on every render.
//! - Setting the system clock back far enough to reproduce a recorded
//!   `ctime` exactly, to the nanosecond, while keeping inode, size and
//!   modification time, defeats it. Writing to the block device beneath the
//!   filesystem does too. Neither is an edit through a viewer or a sync
//!   client, the cases drift detection is for.
//! - Writes never rely on a fingerprint: before a planned file replaces an
//!   owned one, the publisher reads and hashes the owned file again just
//!   before the rename ([`crate::publish`]). A fingerprint can only let a file
//!   be confirmed unchanged without reading it.
//!
//! A fingerprint belongs to the manifest record of one owned path and one
//! hash. It is dropped whenever the path is written, so it never vouches for
//! bytes other than the owned ones, and a manifest without fingerprints (an
//! older one, or one rebuilt after loss) only makes the next render read
//! every file once.

use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::Stat;

/// How much older than the render's start a file's last change must be
/// before its fingerprint is recorded: far coarser than the granularity of
/// any trusted filesystem's timestamps.
pub const SETTLED_NANOS: i64 = 2_000_000_000;
/// [`SETTLED_NANOS`] in words, for the documentation.
pub const SETTLED: &str = "two seconds";

/// Device, inode, size, and modification and status-change times in
/// nanoseconds since the epoch, of one regular file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    device: u64,
    inode: u64,
    size: u64,
    modified: i64,
    changed: i64,
}

fn nanos(seconds: i64, nanoseconds: i64) -> i64 {
    seconds
        .saturating_mul(1_000_000_000)
        .saturating_add(nanoseconds)
}

impl Fingerprint {
    #[allow(clippy::useless_conversion, clippy::unnecessary_cast)]
    pub(crate) fn of(stat: &Stat) -> Self {
        Self {
            device: stat.st_dev as u64,
            inode: stat.st_ino as u64,
            size: stat.st_size as u64,
            modified: nanos(stat.st_mtime as i64, stat.st_mtime_nsec as i64),
            changed: nanos(stat.st_ctime as i64, stat.st_ctime_nsec as i64),
        }
    }

    /// Whether the file's last change is at least [`SETTLED_NANOS`] older
    /// than `start`, nanoseconds since the epoch.
    pub(crate) const fn settled(&self, start: i64) -> bool {
        self.changed <= start.saturating_sub(SETTLED_NANOS)
    }

    /// The manifest form: `device:inode:size:mtime:ctime`, decimal.
    pub fn encode(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.device, self.inode, self.size, self.modified, self.changed
        )
    }

    /// The inverse of [`Self::encode`]; `None` for anything else.
    pub fn decode(text: &str) -> Option<Self> {
        let mut fields = text.split(':');
        let fingerprint = Self {
            device: fields.next()?.parse().ok()?,
            inode: fields.next()?.parse().ok()?,
            size: fields.next()?.parse().ok()?,
            modified: fields.next()?.parse().ok()?,
            changed: fields.next()?.parse().ok()?,
        };
        (fields.next().is_none() && fingerprint.encode() == text).then_some(fingerprint)
    }
}

/// Now, in nanoseconds since the epoch; `None` before the epoch.
pub(crate) fn now() -> Option<i64> {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_nanos()).ok()
}

/// Whether the filesystem holding `root` keeps a nanosecond `ctime` that
/// every change of a file's bytes updates.
pub(crate) fn trusted(root: &impl std::os::fd::AsFd) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        const TRUSTED: [u64; 4] = [
            0x0000_ef53, // ext2/3/4
            0x5846_5342, // XFS
            0x9123_683e, // Btrfs
            0x0102_1994, // tmpfs
        ];
        #[allow(clippy::useless_conversion, clippy::unnecessary_cast)]
        rustix::fs::fstatfs(root).is_ok_and(|stats| TRUSTED.contains(&(stats.f_type as u64)))
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = root;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_round_trip_and_reject_anything_else() {
        let fingerprint = Fingerprint {
            device: 66306,
            inode: 1_234_567,
            size: 4096,
            modified: 1_763_900_000_123_456_789,
            changed: -5,
        };
        let text = fingerprint.encode();
        assert_eq!(Fingerprint::decode(&text), Some(fingerprint));
        for other in [
            "",
            "1:2:3:4",
            "1:2:3:4:5:6",
            "1:2:3:4:x",
            "01:2:3:4:5",
            "1:2:3:+4:5",
            " 1:2:3:4:5",
        ] {
            assert_eq!(Fingerprint::decode(other), None, "{other}");
        }
    }

    #[test]
    fn only_a_change_old_enough_is_settled() {
        let at = |changed| Fingerprint {
            device: 1,
            inode: 1,
            size: 1,
            modified: 0,
            changed,
        };
        let start = 10 * SETTLED_NANOS;
        assert!(at(start - SETTLED_NANOS).settled(start));
        assert!(!at(start - SETTLED_NANOS + 1).settled(start));
        assert!(!at(start + 1).settled(start));
    }
}
