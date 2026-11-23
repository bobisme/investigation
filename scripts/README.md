# Scripts

Repository checks and cross-language conformance runners.

- `run-conformance.py` runs the recording conformance suite in `fixtures/conformance/v1` against one or more producer adapters, and compares the adapters with each other. See [docs/recording.md](../docs/recording.md#conformance-suite).
- `conformance/reference_producer.py` is a standard-library producer that passes the suite. `conformance/non_idempotent_producer.py` is a deliberately broken one that must fail it.
