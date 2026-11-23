# Replay the A-gate pilot

The scored batch started at 00:21 UTC on November 23, 2026. The runner retained the release candidate from Milestone A commit `bebd8b0e2ec1e25324f5f909ed95fb87308e5d33`. The fixture, protocol and rubric commit was `4eeb50a3e44b18a0fe67e31fb1dbe5a4c737fc9f`. Decision-rule commit `4a17b0f582214e845bf5f883f37b2daca5db564d` is an ancestor of both. The binary digest is `785114714253de4c347a93e125dff64e63e6102d30d31421561b46dfddf24a1a`.

The local Ollama 0.30.0 runner used installed Qwen 3.5 9B and Gemma 4 26B weights. The archive manifest pins each exact model digest, environment version, runner digest, budget and block order. Each of the 13 variants has a pair per model. Each assignment has separate investigator and successor sessions. The serial execution order is recorded with seed `320301`. The binary and working directories are on project disk.

All 52 assignments reached a terminal state. There were 42 completed investigators and 10 investigator budget exhaustions. There were 51 submitted successor answers and one successor budget exhaustion. The archive contains every assigned run, pre-update review snapshots where available, handoff workspaces, final workspaces, responses, traces and recorded costs. The compressed archive and exact grading inputs are committed with a checksum list. [Archive checksums](archive/SHA256SUMS) · [Validation](archive/validation.json)

The primary score is deterministic and consumed the blinded packet before the assignment map was joined. An independent observer guessed six of 16 arms on a balanced sample. The upper-tail probability under balanced random guesses for at least six matches is approximately 0.93. The observer found one residual generic reference to an investigation tool in a treatment response, and no explicit arm fields, commands, paths or distinctive CLI formatting. The score rules do not use tool references.

An exact reanalysis uses the delivered raw archive. Re-executing model inference checks the procedure with another sample. The archived responses are the observations used for the pilot disposition.

```sh
cd training/a-gate/archive
sha256sum -c SHA256SUMS
tar --zstd -xf pilot-raw.tar.zst -C /path/to/extraction
```

From the repository root, substitute the extracted `a-gate-pilot-20261122` directory for `/path/to/raw`.

```sh
python3 scripts/a-gate-archive.py /path/to/raw --binary /path/to/investigation
python3 scripts/grade-a-gate.py blind /path/to/raw /path/to/blinded
python3 scripts/grade-a-gate.py grade /path/to/blinded/blinded.json training/a-gate/evaluator/pilot /path/to/grades.json
```

`a-gate-archive.py` verifies the planned assignments, unique phase IDs, exact source and update hashes, identical first prompts across each pair, pre-update review snapshots, handoff snapshots, trace paths and per-phase byte totals. `--progress` reads only run-status metadata. The blinded grader consumes the shuffled packet and the prewritten keys. Open the assignment mapping for analysis only after the blinded scores have been written. [Paired protocol](protocol.md) · [Blinding and rubric](grading.md)
