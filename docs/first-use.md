# First-use and handoff check (W05b.17)

This is a development check, not a release claim. It compares two pinned builds of the CLI on six new development cases, with two local models and one run per case, model and build. The revised build changes the first-use path. With it, more agents reach a persisted Source and Case quickly and leave a source-linked handoff. **The time target was missed.** Successor quality was about the same: 8 of 12 against 9 of 12 cited answers with the limitation. [Protocol](../training/first-use/protocol.md) · [Results JSON](../training/first-use/results.json) · [Tuning runs](../training/first-use/tuning.json) · [Raw archive](../training/first-use/archive/)

## What changed

The revised build gives one short path from a local file to a receipt. It uses existing v1 commands and no new operation names.

- `investigation init ./casework --no-vault --import-root .` configures the working directory as an import root when the workspace is created. Before this change, an agent could not ingest a local file without editing `investigation.toml`.
- `investigation --actor agent:investigator start QUESTION --attach FILE` creates the Case, ingests the Source and links them in one transaction. `start` now generates a command ID when none is given.
- Help, `onboard` and `init` now lead with these two steps. The `next` steps give runnable argv templates with named placeholders.
- `onboard` finds a single workspace in a child directory of the working directory. Workspace-required and actor-required errors now carry a repair argv. An import-root refusal says how to fix it.
- A failed `--quote` gets a hint. The `start` receipt in text format shows how to record an observation with a quote and its limitations.
- Brief and show now include `recorded_limitations` for each observation, so a successor can see the material limitation.

## Results

There are 12 matched pairs: 6 cases × 2 models (`qwen3.5:9b`, `gemma4:26b`). The build order is counterbalanced within each model. When a count is zero, the table also gives its exact one-sided 95% upper bound.

| Measure | Current (`c46441d`) | Revised |
| --- | ---: | ---: |
| Source and Case receipt ≤3 calls after first CLI contact | 0/12 (≤0.221) | 6/12 |
| … among assignments that ran the CLI | 0/8 | 6/8 |
| … counted from `onboard`, when it ran | 0/3 | 0/2 |
| Persisted Source and Case at any time | 0/12 (≤0.221) | 8/12 |
| Source-linked handoff (persisted record, cited answer and limitation) | 0/12 (≤0.221) | 5/12 |
| Successor answered | 11/12 | 9/12 |
| Successor answer / decisive citation / limitation correct | 11 / 9 / 10 | 9 / 8 / 9 |
| All three warranted items | 9/12 | 8/12 |
| Unsupported claims (false promotions) | 0/12 (≤0.221) | 0/12 (≤0.221) |
| Investigator failed CLI calls / CLI calls | 75/125 | 63/156 |
| Investigator phases ended by the budget | 5/12 | 7/12 |
| Successor phases ended by the budget | 1/12 | 2/12 |
| Mean investigator-plus-successor wall time | 61.1 s | 64.9 s |
| Pairs in which the revised build was faster | — | 5/12 |

By model, the receipt-within-three rate was 4/6 for qwen and 2/6 for gemma on the revised build, and 0/6 for each model on the current build. On the current build no investigator persisted a Source; they spent their calls on hand-written record envelopes. Gemma investigators never ran the CLI in 4 of 6 current assignments and 3 of 6 revised assignments. Those pairs measure the model, not the build.

On the two tuning cases, the current build reached 0/2 receipts. The final revised iteration reached 2/2 with qwen and 0/2 with gemma ([tuning.json](../training/first-use/tuning.json)). The tuning cases are not part of the comparison.

## Acceptance

- **Receipt within three calls: partly met.** The revised build reached 6/12 counted from first contact, against 0/12 for the current build. It reached 0/2 counted from `onboard`, because agents seldom ran `onboard` before acting.
- **Successor cites the evidence and its limitation: met at about the same level.** The counts were 8/12 against 9/12, with no unsupported claims in either arm. One extra revised successor failed by reading raw workspace files (see below).
- **Time: not met.** Mean total time rose by 3.8 s, a ratio of 1.06. Wall time mostly depends on when investigators stop. Investigators that reached a Source kept recording until the budget ended.

## Remaining friction for reviewer disposition

1. **Successors read raw workspace files.** In 2 of 12 revised assignments (qwen on choir and on oven), the successor ran `cat` on `state/`, blobs and `cache/index.sqlite` and never ran the CLI. It used its budget and gave no answer. The same failure happened in 1 of 12 current assignments (qwen on oven), where the investigator had run `init` but recorded nothing. The revised build leaves a workspace more often, so it exposes successors to this failure more often. A possible fix is a pointer in the workspace, for example a header line in `investigation.toml` that names `investigation --workspace DIR brief`. The fix was not measured, so it is not shipped.
2. **Init without an import root.** One gemma tuning run ran `init` without `--import-root`. It then followed the advice to edit the toml and broke it. No command adds an import root to an existing workspace.
3. **Relative paths after `cd` into the workspace.** One investigator changed into `casework/`, and then `--attach source.txt` resolved to a missing file. The not-found error gives no repair.
4. **Shell quoting of `--quote` and the selector file for `evidence read`.** These caused most of the remaining failed calls.
5. **`onboard` is not the first contact.** Agents start with `--help`, so the help text carries most of the guidance.
6. **The `start` receipt in JSON format still lists `show case` as its next step.** It does not list `observe`.

## Scope and provenance

- **Scale.** The study used 6 comparison cases instead of a larger set, one variant per case and one run per cell. Total session time was about 25 min for the comparison and 11 min for tuning. With this sample, differences of one or two assignments are noise.
- **Current build.** `c46441d`, SHA-256 `d0a7a921…66c63f`.
- **Revised build.** `c46441d` with [revision.patch](../training/first-use/archive/revision.patch) applied, SHA-256 `769d4025…348001`. The shipped commit was rebased onto a later main. Its crate changes are the same as the patch, apart from hunk offsets.
- **Fixtures.** The cases are independent of the A-gate pilot families. The study read no sealed material.
- **Checks.** `scripts/check-first-use.py` checks the cases, keys, schedule and `results.json` in CI and calls no model.
