//! One producer attempt identity for every recording path.
//!
//! A producer name plus the producer's external run ID names one logical
//! attempt. Two paths record attempts: the streaming capture protocol
//! (`capture.begin` writes the pair into the Run's `capture` binding) and the
//! one-shot `inv.producer-run.v1` envelope (ingest writes it into the Run's
//! `producer_run` section). This module is the single definition both use: the
//! producer-name rule, and how a recorded Run states its attempt. A store that
//! finds an attempt recorded by the other path refuses the second recording and
//! names the existing Run instead of creating a second Run for one attempt.
//!
//! Attempt identity is a producer claim, never verified identity.

use serde_json::Value;

use crate::{explain::ExecutionStatus, producer_run::PRODUCER_RUN_KEY};

/// Run body key of the capture binding that `capture.begin` writes.
pub const CAPTURE_KEY: &str = "capture";
/// Longest producer name, in characters (all ASCII).
pub const PRODUCER_NAME_MAX_CHARS: usize = 128;
/// JSON Schema pattern of a producer name. The published v1 envelope and ingest
/// schemas carry exactly this pattern, so it cannot widen.
pub const PRODUCER_NAME_PATTERN: &str = r"^[A-Za-z0-9][A-Za-z0-9._-]*$";
/// The producer-name rule in words, for error messages and schema descriptions.
pub const PRODUCER_NAME_RULE: &str =
    "1–128 ASCII letters, digits, '.', '_' or '-', starting with a letter or digit";

/// Whether `value` is a producer name that may name a new attempt.
///
/// The one rule for the recording crate's `ProducerName`, the envelope's
/// `producer.name` and an ingest acquisition `namespace`. `:` and `/` are
/// refused, and so is every non-ASCII character.
pub fn is_producer_name(value: &str) -> bool {
    name_with(value, |byte| matches!(byte, b'.' | b'_' | b'-'))
}

/// Whether `value` is a producer name that an existing recording may hold.
///
/// Capture recordings made before the rules were aligned also accepted `:` and
/// `/`. Readers of recorded data accept those names so the recordings replay,
/// recover and finalize unchanged; no new attempt may use one.
pub fn is_recorded_producer_name(value: &str) -> bool {
    name_with(value, |byte| {
        matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
    })
}

fn name_with(value: &str, punctuation: impl Fn(u8) -> bool) -> bool {
    value.len() <= PRODUCER_NAME_MAX_CHARS
        && value
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || punctuation(byte))
}

/// The producer name and external run ID of one logical attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttemptIdentity<'a> {
    pub producer: &'a str,
    pub external_run_id: &'a str,
}

/// Which recording path declared a Run's attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttemptPath {
    /// `capture.begin`; the Run body holds a `capture` binding with a handle.
    Capture,
    /// An `inv.producer-run.v1` envelope; the Run body holds `producer_run`.
    ProducerRun,
}

/// The attempt a Run body records, if it records one.
///
/// Reads the stored fields only, so an identity recorded by any earlier build
/// reads back exactly as it was written.
pub fn recorded_attempt(run_body: &Value) -> Option<(AttemptIdentity<'_>, AttemptPath)> {
    fn pair<'v>(section: &'v Value, producer: &'v Value) -> Option<AttemptIdentity<'v>> {
        Some(AttemptIdentity {
            producer: producer.as_str()?,
            external_run_id: section.get("external_run_id")?.as_str()?,
        })
    }
    if let Some(capture) = run_body.get(CAPTURE_KEY) {
        return pair(capture, capture.get("producer")?).map(|id| (id, AttemptPath::Capture));
    }
    let envelope = run_body.get(PRODUCER_RUN_KEY)?;
    pair(envelope, envelope.get("producer")?.get("name")?).map(|id| (id, AttemptPath::ProducerRun))
}

/// Whether a Run's execution status may change from `old` to `new`.
///
/// `declared`, `running` and `unknown` are open and may become anything. A
/// terminal status (`completed`, `failed`, `cancelled`, `blocked`) is kept:
/// `run.reconcile` and a changed producer-run envelope both refuse to replace
/// it. A different outcome is a separate attempt with its own external run ID.
pub fn keeps_terminal_status(old: ExecutionStatus, new: ExecutionStatus) -> bool {
    let open = matches!(
        old,
        ExecutionStatus::Declared | ExecutionStatus::Running | ExecutionStatus::Unknown
    );
    open || old == new
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn producer_names_follow_one_rule() {
        let long = "a".repeat(PRODUCER_NAME_MAX_CHARS);
        let too_long = "a".repeat(PRODUCER_NAME_MAX_CHARS + 1);
        for name in [
            "example-producer",
            "example.org_bench-2",
            "0tool",
            long.as_str(),
        ] {
            assert!(is_producer_name(name), "{name}");
            assert!(is_recorded_producer_name(name), "{name}");
        }
        for name in [
            "",
            "-tool",
            ".tool",
            "has space",
            "tab\there",
            too_long.as_str(),
            "caf\u{e9}",
            "\u{30c4}\u{30fc}\u{30eb}",
            "tool\u{200b}",
            "\u{ff21}tool",
        ] {
            assert!(!is_producer_name(name), "{name:?}");
            assert!(!is_recorded_producer_name(name), "{name:?}");
        }
        // Recordings made before the rules were aligned may hold ':' and '/'.
        for name in ["example-org:bench", "example-org/bench", "org:tool/v1"] {
            assert!(!is_producer_name(name), "{name}");
            assert!(is_recorded_producer_name(name), "{name}");
        }
        assert!(!is_recorded_producer_name(":tool"));
        assert!(!is_recorded_producer_name("/tool"));
    }

    #[test]
    fn both_paths_state_the_attempt_the_same_way() {
        let captured = json!({"capture": {"producer": "p", "external_run_id": "r", "handle": "h"}});
        let enveloped =
            json!({PRODUCER_RUN_KEY: {"producer": {"name": "p"}, "external_run_id": "r"}});
        let identity = AttemptIdentity {
            producer: "p",
            external_run_id: "r",
        };
        assert_eq!(
            recorded_attempt(&captured),
            Some((identity, AttemptPath::Capture))
        );
        assert_eq!(
            recorded_attempt(&enveloped),
            Some((identity, AttemptPath::ProducerRun))
        );
        assert_eq!(recorded_attempt(&json!({"intent": {}})), None);
    }

    #[test]
    fn terminal_status_is_kept() {
        use ExecutionStatus::*;
        for old in [Declared, Running, Unknown] {
            for new in [
                Declared, Running, Completed, Failed, Cancelled, Blocked, Unknown,
            ] {
                assert!(keeps_terminal_status(old, new));
            }
        }
        for old in [Completed, Failed, Cancelled, Blocked] {
            assert!(keeps_terminal_status(old, old));
            for new in [
                Declared, Running, Completed, Failed, Cancelled, Blocked, Unknown,
            ] {
                assert_eq!(keeps_terminal_status(old, new), old == new);
            }
        }
    }
}
