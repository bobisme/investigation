# Scripts

Repository checks and cross-language conformance runners.

- `prove_milestone_b.py BIN` runs every Milestone B check on one executable from a `git archive` copy of the committed tree. It reports one JSON object. See [docs/milestone-b.md](../docs/milestone-b.md).
- `run-conformance.py` runs the recording conformance suite in `fixtures/conformance/v1` against one or more producer adapters, and compares the adapters with each other. See [docs/recording.md](../docs/recording.md#conformance-suite).
- `conformance/reference_producer.py` is a standard-library producer that passes the suite. `conformance/non_idempotent_producer.py` is a deliberately broken one that must fail it.
- `seal-release-families.py` checks authored release-trial families and skip-the-tool tasks, seals them into an encrypted bundle and verifies the seal, with or without the passphrase. See [training/release/families](../training/release/families/README.md).
- `trial-tasks-8-10.py` sets up, grades and rehearses release-trial tasks 8–10. `self-check --bin BIN` plays each instance's correct path and scripted faults. See [training/trial-tasks/tasks-8-10](../training/trial-tasks/tasks-8-10/protocol.md).
- `release-trial.py` plans, runs, grades and freezes the release trial: four domains, lookups and tasks 5–10, both arms, per-run cost records and frozen run records. `self-test --bin BIN` runs an offline dry run of the development set. See [training/release-trial](../training/release-trial/protocol.md).
