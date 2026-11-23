# Four producer kinds, one schema

Synthetic producers: `benchmark` (numeric metrics with units), `file-analysis` (extracted statements), `simulation` (parameters and state snapshots) and `trace-capture` (a time series and an `external_selector` region). Each directory holds `envelope.json` (a complete run), `envelope-incomplete.json` (a failed, cancelled or unknown run with a missing or rejected artifact) and the artifact bytes.

`crates/investigation-cli/tests/producer_kinds.rs` ingests every envelope and replays the same data through `capture begin/append/attach/finalize`. It asserts the recorded Runs differ only in data and that no crate source names these producers.
