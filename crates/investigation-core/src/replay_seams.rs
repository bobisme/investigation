//! Deterministic clock and ID seams for replaying a fixture through the
//! normal commit path (`investigation demo`).
//!
//! [`SystemClock`](crate::header::SystemClock) and
//! [`SystemIdSource`](crate::identity::SystemIdSource) consult this module
//! first. While a [`Replay`] guard is alive on the current thread, the clock
//! returns the time last given to [`Replay::set_now`] and IDs come from a
//! seeded sequence, so identical fixed inputs give identical canonical bytes.
//! Without a guard nothing changes. The seams are per thread, so a replay
//! never leaks into another thread's commits or into concurrent tests.
//!
//! Generated IDs are valid UUIDv7: the time field is the replay clock's
//! millisecond, and IDs increase strictly in generation order, as the system
//! generator's do.

use std::cell::RefCell;

use uuid::Uuid;

use crate::header::UtcTimestamp;

struct Seams {
    now: UtcTimestamp,
    now_ms: u64,
    seed: u64,
    generated: u64,
    last_ms: u64,
    sequence: u16,
}

thread_local! {
    static SEAMS: RefCell<Option<Seams>> = const { RefCell::new(None) };
}

/// Installed deterministic seams; dropping the guard restores the system
/// clock and ID generator on this thread.
#[must_use = "the seams are removed when the guard is dropped"]
pub struct Replay(());

/// Replay seams are already installed on this thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlreadyInstalled;

impl Replay {
    /// Install seeded seams on this thread with the clock at `now`.
    pub fn install(seed: u64, now: UtcTimestamp) -> Result<Self, AlreadyInstalled> {
        SEAMS.with(|seams| {
            let mut seams = seams.borrow_mut();
            if seams.is_some() {
                return Err(AlreadyInstalled);
            }
            let now_ms = millis(&now);
            *seams = Some(Seams {
                now,
                now_ms,
                seed,
                generated: 0,
                last_ms: 0,
                sequence: 0,
            });
            Ok(Self(()))
        })
    }

    /// Set the time every later clock read returns until the next call.
    pub fn set_now(&self, now: UtcTimestamp) {
        SEAMS.with(|seams| {
            if let Some(seams) = seams.borrow_mut().as_mut() {
                seams.now_ms = millis(&now);
                seams.now = now;
            }
        });
    }
}

impl Drop for Replay {
    fn drop(&mut self) {
        SEAMS.with(|seams| *seams.borrow_mut() = None);
    }
}

/// The replay clock's time, when seams are installed on this thread.
pub(crate) fn now() -> Option<UtcTimestamp> {
    SEAMS.with(|seams| seams.borrow().as_ref().map(|seams| seams.now.clone()))
}

/// The next seeded UUIDv7, when seams are installed on this thread.
pub(crate) fn next_uuid() -> Option<Uuid> {
    SEAMS.with(|seams| seams.borrow_mut().as_mut().map(Seams::next))
}

impl Seams {
    fn next(&mut self) -> Uuid {
        // Strictly increasing: the clock's millisecond, or the last one used
        // if the clock went back, with a 12-bit sequence within it.
        if self.now_ms > self.last_ms {
            self.last_ms = self.now_ms;
            self.sequence = 0;
        } else if self.sequence == 0x0fff {
            self.last_ms += 1;
            self.sequence = 0;
        } else {
            self.sequence += 1;
        }
        self.generated += 1;
        let random = splitmix64(self.seed ^ splitmix64(self.generated)) & ((1 << 62) - 1);
        let value = (u128::from(self.last_ms & 0xffff_ffff_ffff) << 80)
            | (0x7 << 76)
            | (u128::from(self.sequence) << 64)
            | (0b10 << 62)
            | u128::from(random);
        Uuid::from_u128(value)
    }
}

fn millis(now: &UtcTimestamp) -> u64 {
    time::OffsetDateTime::parse(
        &now.to_string(),
        &time::format_description::well_known::Rfc3339,
    )
    .map(|time| u64::try_from(time.unix_timestamp_nanos() / 1_000_000).unwrap_or(0))
    .unwrap_or(0)
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        header::{Clock, SystemClock},
        identity::{Id, IdSource, Kind, SystemIdSource},
    };

    fn at(text: &str) -> UtcTimestamp {
        text.parse().unwrap()
    }

    fn run(seed: u64) -> Vec<(UtcTimestamp, Id)> {
        let replay = Replay::install(seed, at("2026-11-23T07:31:57.165981548Z")).unwrap();
        let mut out = Vec::new();
        for now in [
            "2026-11-23T07:31:57.165981548Z",
            "2026-11-23T07:31:58.000000001Z",
            "2026-11-23T07:31:57.000000000Z",
        ] {
            replay.set_now(at(now));
            for _ in 0..3 {
                out.push((
                    SystemClock.now_utc(),
                    SystemIdSource.next_id(Kind::Observation).unwrap(),
                ));
            }
        }
        out
    }

    #[test]
    fn seeded_replay_repeats_exactly_and_ids_increase() {
        let first = run(7);
        assert_eq!(first, run(7));
        assert_ne!(first, run(8));
        let ids: Vec<_> = first.iter().map(|(_, id)| id.uuid()).collect();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
        assert_eq!(first[3].0, at("2026-11-23T07:31:58.000000001Z"));
        // The UUID time field is the clock's millisecond.
        let (seconds, _) = first[0].1.uuid().get_timestamp().unwrap().to_unix();
        assert_eq!(seconds, 1_795_419_117);
    }

    #[test]
    fn without_a_guard_the_system_seams_are_unchanged() {
        drop(Replay::install(1, at("2000-01-01T00:00:00.0Z")).unwrap());
        assert_ne!(
            SystemClock.now_utc(),
            at("2000-01-01T00:00:00.0Z"),
            "dropping the guard restores the system clock"
        );
        let _replay = Replay::install(1, at("2000-01-01T00:00:00.0Z")).unwrap();
        assert!(Replay::install(1, at("2000-01-01T00:00:00.0Z")).is_err());
        assert!(
            std::thread::spawn(|| now().is_none()).join().unwrap(),
            "another thread keeps the system seams"
        );
    }
}
