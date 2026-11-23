# Python example producer

Records runs through the `investigation` JSON CLI and nothing else: Python 3 standard library only, no Investigation code, no private API. Each call spawns `investigation capture OP --input -` with an argument array (no shell) and one strict-JSON request on stdin, then reads one JSON response. See [docs/recording.md](../../docs/recording.md).

## Entry points

```sh
# Self-check against a built binary. Exit 0 on success.
python3 examples/python-producer/producer.py --bin target/debug/investigation \
    --workspace "$(mktemp -d)" --fixtures fixtures [--canonical-out FILE]

# The inv.producer-adapter.v1 interface the conformance runner drives.
python3 examples/python-producer/producer.py describe
python3 examples/python-producer/producer.py run    --workspace DIR --output-dir OUT --scenario NAME \
    --producer NAME --external-run-id ID [--fault FAULT]... [--protocol VERSION]
python3 examples/python-producer/producer.py resume --workspace DIR --output-dir OUT --scenario NAME \
    --producer NAME --external-run-id ID [--protocol VERSION]
```

The adapter reads the binary from `INVESTIGATION_BIN` (or `--bin`). It never creates workspaces. The self-check creates one fresh workspace per case beneath `--workspace`, which must not already hold them. `--fixtures` is the repository `fixtures` directory.

Scenarios: `synthetic-run`, `partial-evidence`, `interrupted-run`, `late-item`. `--protocol` is sent in `begin` only when given. Faults: `drop-response:OP:N`, `crash-after:OP:N` (exit 75), where N counts distinct requests.

## What it shows

- A lost response, an unparseable response or a failure with `commit_status: unknown` is retried with the identical request and key (up to 3 resends).
- `resume` resends `begin` for the same handle, calls `capture recover --handle`, and continues after `last_position`. The synthetic action (writing `result.json`) is not repeated when the file already holds its bytes. If a recorded item is not one this producer sent, it finalizes `interrupted` with execution `unknown`.
- Strict JSON in both directions: `NaN`, `Infinity` and duplicate object keys are refused.
- `record_envelope` replays any `inv.producer-run.v1` document through `capture begin/append/attach/finalize` using only its generic fields. The self-check runs all eight `fixtures/producer-kinds/v1` envelopes. A refused attachment keeps its key in `declared_artifacts`, so it is recorded as missing. Completion is `partial` when any artifact is absent.

## Comparing with another producer

`--canonical-out FILE` writes each run's canonical form: producer identity, execution, evidence completeness, the finalization section, and the source mappings (artifact key, position, role, digest, size). Record ids, times and manifest digests are removed. Case names are `synthetic-run` and `<kind>-<envelope|envelope-incomplete>`.

```sh
python3 examples/python-producer/producer.py compare python.json other.json   # exit 1 on a difference
```

The Rust example (bn-1540) was not in the tree when this was written, so no test runs both. When it can emit the same canonical file, compare the two with the command above in CI.
