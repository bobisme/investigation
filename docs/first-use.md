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

## Follow-up changes (unmeasured)

W05b.18 changed the CLI after this comparison for items 1, 2, 3 and 6 above. **The study was not run again, so these changes are not measured.** The results above apply only to the two pinned builds.

- **Pointer in the workspace.** `init` writes `README.txt` at the workspace root, and the header of `investigation.toml` now names `investigation --workspace DIR brief`. Both say not to read `state/` or `cache/` directly. They use a `DIR` placeholder and contain no absolute local path.
- **Import root for an existing workspace.** `investigation init PATH --import-root DIR` on an existing workspace adds DIR to its import roots. The result is `import_roots_added`, or `already_initialized` if DIR is already a root. The config file is replaced atomically under the commit lock. DIR must be an existing directory with a canonical path, and it must lie outside the workspace. An attachment outside every import root is refused with this `init` command as its repair argv. No message advises an edit to `investigation.toml` now.
- **Missing relative attachments.** If `--attach FILE` (or `source add PATH`) names a file that does not exist, the error gives the resolved path and the import roots. If the file exists beneath an import root, the repair argv repeats the command with that path.
- **Next step after `start`.** The JSON receipt from `start` now has a `next_write` field. It holds the `observe` template with `STATEMENT`, `EXACT TEXT` and `LIMITATION` placeholders. The v1 receipt `next` list can only hold `local_read` steps, so it is not changed.

## Actor named once (bn-6vlm)

The Milestone B re-run ([rerun-mb](../training/first-use/rerun-mb/results.json)) reached a receipt within three calls in only 2/12 assignments. In 3 of 6 gemma assignments the investigator copied the `init` line from the root help, then wrote `start` without `--actor`, although the help and the `init` output both showed it. The refusal gave the exact retry, but the receipt came one call late. In 3 of 6 qwen assignments the investigator first ran `init --workspace /work`, which had no PATH and named a directory that already held files.

Changes:

- `investigation init DIR --actor ACTOR` records ACTOR in `DIR/local/actor.toml`. `local/` is machine-specific and not canonical. Only an explicit `init --actor` writes the file; nothing infers it.
- A write uses that actor only when nothing else names one. `--actor`, the request's actor, the `investigation.toml` actor and `AGENT` all take precedence, so the setting changes only calls that were refused before. Each receipt that used it carries a warning that names the actor and the file. A malformed file fails the write. Without the file, a write with no actor is still refused, and the refusal names both fixes.
- The first `init` example in the root help, the `init` help and `onboard` carries `--actor agent:investigator`. The `start` example keeps `--actor` too.
- `init` accepts `--workspace DIR` as its destination when PATH is omitted or names the same directory. `init` on a directory that holds files but no workspace is refused with a repair: the same call for `DIR/casework`, with DIR as an import root.

Re-measure: the revised arm only, with the same runner (SHA-256 `aa4e9447…`), seed 51701, six comparison cases, both models with the same digests and the same budgets, on `df2d797` (debug build, SHA-256 `adb8cc83…f59472c4`). That build is this change on `abddc9b`. The merged commit is the same change rebased onto later main, which adds the W17.01 and W17.02 help and error changes; the rebased build was not measured. The 12 assignments took 17 min. Results: [results.json](../training/first-use/rerun-6vlm/results.json) · [raw archive](../training/first-use/rerun-6vlm/raw.tar.zst).

| Measure | rerun-mb (`495688d`) | bn-6vlm (`df2d797`) |
| --- | ---: | ---: |
| Source and Case receipt ≤3 calls after first CLI contact | 2/12 | 8/12 |
| … qwen / gemma | 1/6 / 1/6 | 6/6 / 2/6 |
| Median calls from contact to receipt | 4 | 3 |
| Assignments with an actor-required refusal | 5/12 | 0/12 |
| Persisted Source and Case at any time | 8/12 | 9/12 |
| Source-linked handoff | 4/12 | 4/12 |
| Successor answered / decisive citation / limitation | 10 / 10 / 8 | 9 / 6 / 8 |
| Successor used the CLI | 7/12 | 5/12 |
| Unsupported claims | 0/12 | 1/12 |
| Successor phases ended by the budget | 1/12 | 3/12 |
| Mean investigator-plus-successor wall time | 73.8 s | 83.2 s |

Seven investigators passed `--actor` to `init`, and two of them chose their own actor name. Three ran `start` without `--actor` and got the recorded actor with the warning. Two qwen investigators used the new `init --workspace` paths, one through the non-empty repair. Of the four gemma misses, two never ran the CLI, one handed off after `--help` without running `init`, and one spent a call between `--help` and `init` on `ls`, so its receipt came at the fourth call.

The successor measures did not improve, and citation fell. The two gemma successors that used the CLI and lost the citation item cited `source.txt` as a line range (`lines 1-15`, `lines 14-28`), which the grader did not read as a decisive line. Three successors again read `state/` event files and ran out of budget. The one unsupported claim (`bay_wide_humidity`, qwen) is in the successor's answer text. These failures are in the successor phase, which this change does not touch, but more persisted workspaces give successors more raw files to read. This is one unpaired run per cell, so a difference of one or two assignments is noise.

## Scope and provenance

- **Scale.** The study used 6 comparison cases instead of a larger set, one variant per case and one run per cell. Total session time was about 25 min for the comparison and 11 min for tuning. With this sample, differences of one or two assignments are noise.
- **Current build.** `c46441d`, SHA-256 `d0a7a921…66c63f`.
- **Revised build.** `c46441d` with [revision.patch](../training/first-use/archive/revision.patch) applied, SHA-256 `769d4025…348001`. The shipped commit was rebased onto a later main. Its crate changes are the same as the patch, apart from hunk offsets.
- **Fixtures.** The cases are independent of the A-gate pilot families. The study read no sealed material.
- **Checks.** `scripts/check-first-use.py` checks the cases, keys, schedule and `results.json` in CI and calls no model.
