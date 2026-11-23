# Scripts

Repository checks and cross-language conformance runners.

- `prove_milestone_b.py BIN` runs every Milestone B check on one executable from a `git archive` copy of the committed tree. It reports one JSON object. See [docs/milestone-b.md](../docs/milestone-b.md).
- `run-conformance.py` runs the recording conformance suite in `fixtures/conformance/v1` against one or more producer adapters, and compares the adapters with each other. See [docs/recording.md](../docs/recording.md#conformance-suite).
- `conformance/reference_producer.py` is a standard-library producer that passes the suite. `conformance/non_idempotent_producer.py` is a deliberately broken one that must fail it.
