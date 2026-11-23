# First-use and handoff protocol (W05b.17)

This study asks whether an unfamiliar agent can get from one local file in an ordinary working directory to a receipt that names a persisted Source and Case, and whether a fresh successor can then cite that evidence and its material limitation. It compares two pinned builds of the CLI: the trunk at the start of the work (`current`) and the build with the first-use changes (`revised`). It is not a notes-versus-CLI comparison and makes no release claim; the A-gate and the sealed release trial cover that. [A-gate pilot](../../docs/a-gate.md) · [Release trial](../../docs/release-trial.md)

## Cases

Eight new development families live under `fixtures/development/first-use/` and are registered with split `development` in [family-registry.json](../../fixtures/family-registry.json). Each has one `source.txt` and no update stage. They were written for this study, share no source bytes with the pilot families, and were written without reading sealed material. Two families (`print-queue-stall`, `ferry-ticket-storage`) are tuning cases. Only they were run while the revision was being changed. The other six are comparison cases. They cover debugging (three), architecture choice (two) and source research (one).

Each family has a frozen key in [keys/](keys/). It uses the A-gate rubric format and grader. The key has three warranted items: the current answer, the decisive citation lines of `source.txt`, and the material limitation stated in the source. It also has one or more false-promotion patterns. The keys were committed before any comparison-case run.

## Assignment

[`scripts/run-first-use.py`](../../scripts/run-first-use.py) reuses the A-gate runner's isolated participant shell, session loop, inference settings, budgets and successor answer function. Each tool call runs in a network-isolated bubblewrap namespace with a private `/work` and the arm's executable at `/tools/investigation`. The investigator gets `/work/source.txt` and nothing else. The successor gets the same directory after the investigator stops, a fresh session, the objective, and the A-gate successor prompt. Budgets match the A-gate: 210 s and 28 shell calls for the investigator, 120 s and 14 calls for the successor, with the same token limits.

One deliberate difference from the A-gate is that the investigator prompt says the team keeps investigation records with the `investigation` command on PATH, and asks for durable work in it. The successor prompt says the previous investigator kept records with that command. The prompts do not name any subcommand, flag, or workflow. Discovery is only through the executable's own help, onboarding, and errors. The study measures first use given intent to use the tool. It does not measure whether an agent chooses the tool.

For each model, the schedule shuffles the comparison cases with a fixed seed and alternates which build runs first, so build order is counterbalanced within each model. Both builds see the same case, prompts, budgets and seed. Models run one at a time on the shared local GPU. Each model is unloaded before the next starts.

## Measures

- **First-use receipt.** After each participant call, the runner checks whether any workspace under `/work` holds a Case and a blob whose SHA-256 equals the original source. The primary pass requires that state within three calls after the first call that ran the executable at all (normally help or onboarding). Reaching it at the third call after that contact counts. The secondary measure counts from the first `onboard` call, for assignments that ran it.
- **Source-linked handoff.** The handoff directory holds the persisted Source and Case, the successor submitted an answer, and the answer cites `source.txt` at a decisive line and states the material limitation.
- **Successor quality.** The A-gate deterministic rubric grades the answer, citation and limitation items. False-promotion patterns count unsupported claims. Record identifiers that resolve to the original source bytes count as citations of `source.txt`, as in the A-gate grader. Grading reads no arm or build metadata.
- **Burden and failures.** The measures are wall time per phase and in total, shell calls, CLI calls, and failed CLI calls. A failed CLI call is one whose output carries an error envelope or a text-mode error and repair line. This is a text heuristic. The measures also include phases ended by deadline or budget, and shell calls killed at the 12 s limit.

Counts are reported with denominators. A zero count also gets its exact one-sided 95% upper bound. Paired wall-time differences use the matched case and model pairs. With six cases and two models this is a development check, not an estimate that generalizes.

## Successor check

The same assignment runs the successor check. No separate session is staged. The successor gets the working directory as the investigator left it: the original `source.txt`, any workspace and any notes. It gets only the ordinary handoff context described above. The original source stays in place, as in an ordinary handoff, so citing `source.txt` alone does not show that the successor used the record. Each row therefore also records whether the successor ran the CLI.
