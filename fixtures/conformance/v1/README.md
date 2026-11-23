# Recording conformance suite (`inv.conformance-suite.v1`)

Language-neutral fixtures for producers of protocol `inv.capture.v1`. A producer takes part through the adapter protocol `inv.producer-adapter.v1`. `scripts/run-conformance.py` runs the cases and judges each one from canonical state that it reads through the `investigation` binary. See [Conformance suite](../../../docs/recording.md#conformance-suite).

- `suite.json`: the protocol, the required scenarios, and the coverage map from each §11.7 item and recording rule to its cases.
- `scenarios/`: the exact requests of each scenario, with `${producer}`, `${external_run_id}`, `${output_dir}` and `${handle}` placeholders.
- `cases/`: one file per case. Each case is a list of steps that run in a new workspace.
- `expected/`: the normalized canonical state of each scenario, as a pattern. Extra fields are allowed, lists must match in full, and `<any>` matches any value that is present.

The expected states contain no generated IDs and no times. A change to a scenario request is a new suite version, not an edit.
