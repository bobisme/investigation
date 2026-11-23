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

## Help-only arm (W17.03, preregistered)

This section and [help-only/targets.json](help-only/targets.json) were committed before any run of the two arms below. They ask whether an agent learns the CLI from the executable alone, as well as an agent that also has the agent instructions.

**Arms.** Both arms use one pinned build, the same runner, cases, prompts, seed scheme (`--seed 51701`, build order counterbalanced within each model), models (`qwen3.5:9b`, `gemma4:26b`, local, one at a time on the shared GPU) and budgets as the bn-6vlm re-measure ([rerun-6vlm](rerun-6vlm/)).

- **docs**: the investigator and successor prompts also carry the build's own agent instructions (`investigation docs agents`, the page that `integration apply` writes into an AGENTS.md block), presented as the repository's AGENTS.md, the way an agent host loads that file.
- **help-only**: the executable and the task only. No agent instructions, docs page, README or integration block is in the prompt or in `/work`. The prompts are the first-use prompts above, word for word. Discovery is only through `--help`, `schema`, `onboard`, errors and receipts. The binary's own `docs` command is part of the executable, so it is not blocked; the results report how many assignments ran it.

**Run.** `scripts/run-first-use.py --arm docs --arm help-only --binary BUILD --binary-commit SHA` on the six comparison cases, 12 assignments per arm. The two tuning cases are used only to rehearse the arm before the measured run. The measured commit and build digest are recorded in the manifest.

**Targets.** The help-only arm meets the W17.03 target when all five hold (counts of 12):

1. Receipt within three calls after first CLI contact: help-only ≥ docs − 2, and help-only ≥ 6/12.
2. Source-linked handoff: help-only ≥ docs − 2.
3. Unsupported claims (false promotions in the successor answer): help-only = 0/12.
4. Friction budget, refusals: the share of investigator CLI calls that are refused is at most 0.10 higher in help-only than in docs.
5. Friction budget, recovery: the share of refused CLI calls (both phases) after which a later call of the same command succeeds is at most 0.10 lower in help-only than in docs.

A margin of 2 of 12 is used because one run per cell makes a difference of one or two assignments noise (see the bn-6vlm re-measure). `scripts/analyze-first-use.py --targets training/first-use/help-only/targets.json` applies the targets.

**Friction.** For each refused CLI call the analysis records the command, the cause (error code and reason, or the parse problem), the repair hint the CLI printed, whether the next CLI call ran the hinted command, and whether a later call of the same command succeeded. A refusal is detected with the same text heuristic as the failed-call count above.

**Second measurement (added after the first, before the second ran).** The first measured run ([help-only/run1](help-only/run1/)) did not meet target 1: the help-only arm reached a receipt within three calls in 4/12 assignments against 12/12 for docs. Its misses were a prose argument in the command position followed by `--help` (4 gemma assignments), an `init` repair without `--actor` followed by a refused `start`, a relative `--workspace` after `cd` into the workspace, and two assignments that never ran the CLI. The CLI was changed for the first three causes. A second run with the same arms, cases, seed, models, budgets and the five targets above measures the changed build ([help-only/run2](help-only/run2/)). Both runs are reported. The first run remains the preregistered result for the build it measured; the second is the result for the changed build.
