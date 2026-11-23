//! Neutral recording API for native producers.
//!
//! An external program that runs something (a benchmark, a trace collector, a
//! simulator, a scraper) links this crate to record what happened as one Run:
//! lifecycle facts, original artifacts and a final manifest. It never needs an
//! investigative case with hypotheses, a vault, or a model.
//!
//! The dependency arrow points inward. Producers import Investigation;
//! Investigation never imports, names or invokes a producer.
//!
//! # Lifecycle
//!
//! ```text
//! begin ──► append / attach (positions 1, 2, 3, …) ──► finalize
//!   ▲                                                      │
//!   └──────────── recover (read-only, any time) ◄──────────┘
//! ```
//!
//! | Call | Operation | Returns |
//! |---|---|---|
//! | [`Recorder::begin`] | `capture.begin` | [`BeginReceipt`] with the [`CaptureHandle`] |
//! | [`Recorder::append`] | `capture.append` | [`ItemReceipt`] |
//! | [`Recorder::attach`] | `capture.attach` | [`ArtifactRef`] with its [`ItemReceipt`] |
//! | [`Recorder::finalize`] | `capture.finalize` | [`FinalizeReceipt`] |
//! | [`Recorder::recover`] | `capture.recover` | [`RecoveryState`] |
//!
//! Every mutating call takes an [`IdempotencyKey`] and returns a durable receipt.
//! `recover` is a read and returns the receipts of earlier calls.
//!
//! # What this crate never does
//!
//! - Authorize, start, schedule, cancel or rerun an external action. The producer
//!   owns execution and its own permissions, limits and cleanup.
//! - Spawn a process, load a plugin, or run a parser or command named in a
//!   request.
//! - Hold the canonical commit lock while caller code or an external process
//!   runs. Calls take plain data; the recorder calls back into nothing.
//! - Promote a producer's result to a finding. Assertions stay attributed
//!   producer claims until an attributed Assessment or Conclusion uses them.
//! - Depend on a frontend (CLI, MCP server, vault renderer) or on any producer.
//!
//! # One implementation
//!
//! [`LocalRecorder`] uses the same validation, journal, blob store and locks as the
//! command-line interface. The request and receipt types here are also the
//! JSON-CLI capture payloads (protocol [`CaptureVersion::V1`], `inv.capture.v1`),
//! so a Rust producer and a producer in another language that speaks JSON to the
//! CLI cannot diverge. See `docs/recording.md` for the protocol and the adapter
//! interface that example producers implement for the conformance suite.
//!
//! # Implementation status
//!
//! This build fixes the public types and signatures. [`Recorder::begin`],
//! [`Recorder::append`] and [`Recorder::attach`] are durable on
//! [`LocalRecorder`] and are also the `capture begin`, `capture append` and
//! `capture attach` CLI commands. The other operations
//! return [`RecordingError::Unsupported`] and record nothing; each one's durable
//! behaviour lands separately without changing these signatures.
//! `docs/recording.md` documents the staging layout.
//!
//! # Compatibility policy
//!
//! - The public surface is the [`Recorder`] trait, [`LocalRecorder`], the request,
//!   receipt and value types defined in this crate, and the core contract types
//!   re-exported at the crate root. Store internals (workspace, journal, blobs,
//!   locks) are not part of it and are never re-exported.
//! - The crate is versioned with the workspace and is pre-1.0. A breaking change
//!   needs a minor version bump and a migration note in `docs/recording.md`.
//! - Additions do not break callers. Request, receipt and enum types are
//!   `#[non_exhaustive]`: build requests with their `new` constructors and set
//!   optional fields directly, and match enums with a wildcard arm. A new
//!   [`Recorder`] method arrives with a default body.
//! - The serialized forms follow the v1 rules: new fields are optional, and
//!   existing names, required fields and value constraints are stable.
//!
//! # Example
//!
//! ```no_run
//! use investigation_recording::{
//!     ArtifactRole, BeginCapture, CaptureCompletion, CaptureEvent, CaptureIntent,
//!     CaseTarget, ExecutionReport, ExecutionStatus, ExternalRunId, FinalizeCapture,
//!     IdempotencyKey, LifecycleFact, LocalRecorder, Position, ProducerIdentity,
//!     ProducerName, Recorder, StableArtifact,
//! };
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let recorder = LocalRecorder::open("./cases")?;
//!     let begin = BeginCapture::new(
//!         IdempotencyKey::new("run-0001/begin")?,
//!         "agent:example-producer".parse()?,
//!         ProducerIdentity::new(ProducerName::new("example-producer")?),
//!         ExternalRunId::new("run-0001")?,
//!         CaptureIntent::new("compare two queue settings"),
//!         CaseTarget::NewRecordCase { title: "Queue setting runs".into() },
//!     );
//!     // Persist the handle before acting, so a restart can call `recover`.
//!     let handle = recorder.begin(&begin)?.handle;
//!
//!     // The producer runs its own action here, under its own authority.
//!     let started = CaptureEvent::new(
//!         handle,
//!         IdempotencyKey::new("started")?,
//!         Position::FIRST,
//!         LifecycleFact::Started,
//!     );
//!     recorder.append(&started)?;
//!     let output = StableArtifact::file(
//!         handle,
//!         IdempotencyKey::new("result")?,
//!         Position::new(2).expect("nonzero"),
//!         ArtifactRole::OriginalOutput,
//!         "./result.json",
//!     );
//!     recorder.attach(&output)?;
//!
//!     let summary = FinalizeCapture::new(
//!         handle,
//!         IdempotencyKey::new("run-0001/finalize")?,
//!         Position::new(2),
//!         CaptureCompletion::Complete,
//!         ExecutionReport::new(ExecutionStatus::Completed),
//!     );
//!     let receipt = recorder.finalize(&summary)?;
//!     println!("finalized {}", receipt.run_id);
//!     Ok(())
//! }
//! ```

#![deny(missing_docs)]

mod append;
mod attach;
mod begin;
mod error;
mod finalize;
mod key;
mod recorder;
mod recover;

pub use append::{CaptureEvent, ChannelEvent, ItemReceipt, LifecycleFact};
pub use attach::{
    ArtifactRef, ArtifactRole, ArtifactSource, Derivation, StableArtifact, StagedAvailability,
};
pub use begin::{BeginCapture, BeginReceipt, CaptureIntent, CaptureLimits, CaseTarget};
pub use error::RecordingError;
pub use finalize::{
    CaptureCompletion, ChannelInventory, ExecutionReport, ExternalReceipt, Finalization,
    FinalizeCapture, FinalizeReceipt, ProducerAssertion, ProducerControl,
};
pub use key::{
    CaptureHandle, CaptureVersion, ExternalRunId, IdempotencyKey, InvalidValue, Metadata,
    Operation, Position, ProducerIdentity, ProducerName, ReceiptResult,
};
pub use recorder::{LocalRecorder, Recorder};
pub use recover::{HandleRecovery, RecoverScope, RecoveryState, SealState};

// Core contract types that appear in this crate's signatures. Producers need no
// direct dependency on `investigation-core`.
pub use investigation_core::{
    command::RevisionRef,
    explain::{EvidenceCompleteness, ExecutionStatus},
    header::{Actor, ClassificationLevel, ClockQuality, Knowledge, SourceTime, UtcTimestamp},
    identity::Id,
    wire::{CommitStatus, Digest, ErrorCode, Failure, Receipt as CommitReceipt},
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_wire_names_are_the_reserved_capture_family() {
        let names: Vec<_> = Operation::ALL.iter().map(|op| op.wire_name()).collect();
        assert_eq!(
            names,
            [
                "capture.begin",
                "capture.append",
                "capture.attach",
                "capture.finalize",
                "capture.recover"
            ]
        );
    }

    #[test]
    fn protocol_tag_round_trips() {
        let tag = serde_json::to_value(CaptureVersion::V1).unwrap();
        assert_eq!(tag, "inv.capture.v1");
        assert_eq!(CaptureVersion::SUPPORTED, &[CaptureVersion::V1]);
    }
}
