//! Record times in exports (bn-18la). Pure.
//!
//! A **record time** is a time this workspace's own clock stamped on a
//! record: when it recorded a revision (`recorded_at`), when it created the
//! record (`created_at`), and the acquisition time ingest, capture and
//! producer-run import stamp on a Source or Artifact (`acquired_at`). These
//! are journal metadata, like an ID's creation time, so an export shows them
//! only as precisely as its audience may see:
//!
//! | Audience | Canonical IDs | Remapped IDs |
//! |---|---|---|
//! | `restricted`, `internal` | exact | to the day |
//! | `public` | — (always remapped) | omitted |
//!
//! A canonical export already discloses every creation time to the
//! millisecond through its UUIDv7 IDs, so coarsening its times would hide
//! nothing. A remapped export removed those times from its IDs, so its
//! record times keep only the UTC day. A public export shows none.
//!
//! Times a source, a producer or a recorder *claims* (`observed_at`, a
//! run's `started_at` and `finished_at`) are evidence content, not record
//! times, and are shown as recorded.
//!
//! Every kind that shows a record time takes its text from
//! [`super::ExportInput::record_time`] (or, in notes planned for a review
//! export, from the note context, which asks the same policy). The leak
//! suite checks every export of every kind against [`RecordTimes::for_export`].

use super::IdentifierMode;
use crate::Audience;

/// How precisely an export shows record times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordTimes {
    /// As recorded, to the fraction of a second.
    Exact,
    /// The UTC day only, `YYYY-MM-DD`.
    Day,
    /// Not at all.
    Omitted,
}

impl RecordTimes {
    /// The policy for an export to `audience` whose identifiers are `mode`.
    pub const fn for_export(audience: Audience, mode: IdentifierMode) -> Self {
        match (audience, mode) {
            (Audience::Public, _) => Self::Omitted,
            (_, IdentifierMode::Remapped) => Self::Day,
            (_, IdentifierMode::Canonical) => Self::Exact,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Day => "day",
            Self::Omitted => "omitted",
        }
    }

    /// The text to show for a record time written as RFC 3339 (`time`), or
    /// `None` when it may not show. A time whose day cannot be read is not
    /// shown under [`Self::Day`] either.
    pub fn show(self, time: &str) -> Option<String> {
        match self {
            Self::Exact => Some(time.to_owned()),
            Self::Day => day(time).map(str::to_owned),
            Self::Omitted => None,
        }
    }

    /// The sentence a remapped export's `identifiers` statement adds about
    /// its record times; `None` when they show exactly.
    pub const fn statement(self) -> Option<&'static str> {
        match self {
            Self::Exact => None,
            Self::Day => Some(
                "Times this workspace stamped on records (recorded, acquired) are shown to the UTC day only.",
            ),
            Self::Omitted => {
                Some("Times this workspace stamped on records (recorded, acquired) are not shown.")
            }
        }
    }
}

/// The `YYYY-MM-DD` date that starts an RFC 3339 time.
fn day(time: &str) -> Option<&str> {
    let date = time.get(..10)?;
    let shaped = date.bytes().enumerate().all(|(index, byte)| match index {
        4 | 7 => byte == b'-',
        _ => byte.is_ascii_digit(),
    });
    let ends = matches!(time.as_bytes().get(10), None | Some(b'T' | b't' | b' '));
    (shaped && ends).then_some(date)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIME: &str = "2026-11-23T07:31:56.728018350Z";

    #[test]
    fn each_audience_and_mode_has_one_policy() {
        use IdentifierMode::{Canonical, Remapped};
        assert_eq!(
            RecordTimes::for_export(Audience::Restricted, Canonical),
            RecordTimes::Exact
        );
        assert_eq!(
            RecordTimes::for_export(Audience::Internal, Canonical),
            RecordTimes::Exact
        );
        assert_eq!(
            RecordTimes::for_export(Audience::Restricted, Remapped),
            RecordTimes::Day
        );
        assert_eq!(
            RecordTimes::for_export(Audience::Internal, Remapped),
            RecordTimes::Day
        );
        // Public is remapped in practice; it shows no time either way.
        for mode in [Canonical, Remapped] {
            assert_eq!(
                RecordTimes::for_export(Audience::Public, mode),
                RecordTimes::Omitted
            );
        }
    }

    #[test]
    fn a_time_shows_exactly_to_the_day_or_not_at_all() {
        assert_eq!(RecordTimes::Exact.show(TIME).as_deref(), Some(TIME));
        assert_eq!(RecordTimes::Day.show(TIME).as_deref(), Some("2026-11-23"));
        assert_eq!(RecordTimes::Omitted.show(TIME), None);
        assert_eq!(
            RecordTimes::Day.show("2026-11-23").as_deref(),
            Some("2026-11-23")
        );
        // Nothing that is not a date shows under the day policy.
        for odd in [
            "",
            "2026-11",
            "20261123T07:31",
            "2026-11-23X",
            "yesterday!!",
        ] {
            assert_eq!(RecordTimes::Day.show(odd), None, "{odd}");
        }
    }
}
