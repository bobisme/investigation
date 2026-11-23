# Rust example producer

A synthetic producer written against the public `investigation-recording` API only. It is documentation and a conformance subject. Nothing in the core crates depends on it.

The "action" fabricates one deterministic result file. It starts no process, reads no environment and uses no network. The binary is the Rust adapter for the [producer adapter protocol](../../docs/recording.md#producer-adapter-protocol-invproducer-adapterv1) (`inv.producer-adapter.v1`):

```sh
cargo build -p example-rust-producer
investigation init WS --no-vault          # the runner does this; add OUT to import_roots
example-rust-producer describe
example-rust-producer run    --workspace WS --output-dir OUT --scenario synthetic-run \
    --producer example-producer --external-run-id run-0001 [--protocol VERSION] [--fault crash-after:attach:1]
example-rust-producer resume --workspace WS --output-dir OUT --scenario synthetic-run \
    --producer example-producer --external-run-id run-0001
```

Scenarios: `synthetic-run`, `partial-evidence`, `interrupted-run`, `late-item` (a post-finalize append that must be refused with `REVISION_CONFLICT`). `--protocol VERSION` sets the protocol on `begin`.

Exit status: 0 finalized, 1 rejected, 2 usage error, 75 injected crash. One JSON object goes to stdout; diagnostics go to stderr.

Flow: `begin` (new `record` case), `append started`, `attach result.json`, `append phase_changed`, `finalize`. `resume` resends the same `begin`, calls `recover` and continues after `last_position`. If `OUT/result.json` holds bytes it did not write, it cannot vouch for the step, so it finalizes with completion `interrupted` and execution `unknown`.

To remove the example, delete this directory, its line in the workspace `members` in `Cargo.toml`, and its entry in `scripts/check-boundaries.py`. The core build is unchanged.
