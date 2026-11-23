//! Example producer and `inv.producer-adapter.v1` adapter, written against the
//! public `investigation-recording` API only.
//!
//! The synthetic "action" fabricates one deterministic result file. It starts no
//! process, reads no environment and uses no network. See `docs/recording.md`
//! (Producer adapter protocol) for the invocation, scenarios, faults and output.
//!
//! ```text
//! example-rust-producer describe
//! example-rust-producer run    --workspace DIR --output-dir OUT --scenario synthetic-run \
//!     --producer NAME --external-run-id ID [--fault drop-response:OP:N | crash-after:OP:N]...
//! example-rust-producer resume --workspace DIR --output-dir OUT --scenario synthetic-run \
//!     --producer NAME --external-run-id ID
//! ```
//!
//! Exit status: 0 finalized, 1 rejected, 2 usage error, 75 injected crash.

mod adapter;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    adapter::main(&args)
}
