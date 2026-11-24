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

## Help-only arm (W17.03, bn-1tzg)

Can a cold agent learn the CLI from the executable alone? Two arms ran on one build: **docs** also got the build's own agent instructions (`investigation docs agents`) in both prompts, as a host loads AGENTS.md; **help-only** got the executable and the task only. Same runner, cases, seed (51701), models, budgets and prompts as the bn-6vlm re-measure. The five targets were committed before any run ([protocol](../training/first-use/protocol.md#help-only-arm-w1703-preregistered), [targets.json](../training/first-use/help-only/targets.json)). Before the measured runs the arm was rehearsed on the two tuning cases, and `docs/agent/agents.md` moved to `inv.agent-instructions.v2` so that it names `init --actor`, as `--help` does.

**The preregistered target was not met.** Target 1 (receipt within three calls) failed in both runs. Handoff, unsupported claims and both friction budgets were met in both runs.

| Measure (of 12) | Run 1 docs | Run 1 help-only | Run 2 docs | Run 2 help-only | Target |
| --- | ---: | ---: | ---: | ---: | --- |
| Receipt ≤3 calls after first CLI contact | 12 | **4** | 12 | **5** | ≥ docs − 2 and ≥ 6: **missed** |
| … qwen / gemma | 6 / 6 | 3 / 1 | 6 / 6 | 1 / 4 | |
| Persisted Source and Case at any time | 12 | 9 | 12 | 9 | |
| Source-linked handoff | 4 | 3 | 5 | 4 | ≥ docs − 2: met |
| Successor answer / citation / limitation | 7 / 5 / 7 | 8 / 5 / 8 | 7 / 5 / 7 | 9 / 7 / 7 | |
| Unsupported claims | 0 (≤0.221) | 0 (≤0.221) | 0 | 0 | help-only 0: met |
| Refused / all investigator CLI calls | 105/211 (0.50) | 73/188 (0.39) | 84/239 (0.35) | 75/195 (0.38) | ≤ docs + 0.10: met |
| Refused calls followed by a successful call of the same command | 55/123 (0.45) | 54/80 (0.68) | 48/102 (0.47) | 59/93 (0.63) | ≥ docs − 0.10: met |
| Refusals with a repair hint / hint followed next | 85 / 53 | 58 / 36 | 77 / 27 | 80 / 53 | |
| Ran `--help` / `onboard` / `docs agents` (investigator) | 4 / 6 / 0 | 10 / 3 / 0 | 7 / 7 / 0 | 10 / 3 / 2 | |
| Median calls from contact to receipt | 1 | 4 | 2 | 3 | |
| Mean investigator-plus-successor wall time | 94.5 s | 85.3 s | 105.9 s | 92.2 s | |

Run 1 measured `84561f7` (main `d076991` with this bone's first four commits, which the workspace later rebased as `e919b12`), debug build SHA-256 `4112dfaa…`. Run 2 measured `df75b39` (the same work rebased on main `4040392`, plus the fixes for run 1's causes; rebased later), SHA-256 `e33e39de…`. Each run took about 45 min. Compact results, manifests and SHA256SUMS are in [help-only/run1](../training/first-use/help-only/run1/) and [help-only/run2](../training/first-use/help-only/run2/); the raw archives (2 MB each, listed in SHA256SUMS) are kept outside the repository.

**Why help-only misses the receipt target.** The docs arm knows the commands, so its first CLI call is `init` and `start` follows. The help-only arm's first call is a guess or `--help`, and the three-call window must also hold `init` and `start`. Its misses were:

- No receipt at all: 3 in each run. Qwen investigators ran `head` on the executable, which filled the context (1 in run 1, 2 in run 2). On the choir case the gemma investigator wrote notes and handed off, without the CLI in run 1 and after only `--help` in run 2. No help text reaches an agent that does not run the executable. The third run-1 case is the `cd` slip below.
- Prose in the command position (`investigation "Initial discovery of …"`), then `--help`: 4 gemma assignments in run 1. After the fix in run 2 the refusal printed the two first steps; 1 gemma assignment still ran `--help` before `init`.
- One extra discovery call between `--help` and `init`, usually `onboard`, which the root help named as its next command.
- Slips on `init`: `init --workspace /work` (repaired), an `init` without `--actor` and then a refused `start`, `cd` into the workspace with a relative `--workspace`, and `--attach ../source.txt` from the import root.

Help-only agents refused fewer calls than docs agents and recovered more often. The largest docs-arm cost was quoting source bullet lines that start with `- `, which clap read as flags (34 refused calls in run 1).

### Friction by cause, help-only arm, run 1

| Command | Cause | Calls | Assignments | Hint | Hint followed | Got past it |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `observe` | quote not found in the source | 16 | 4 | 0 | 0 | 16 |
| `observe` | extra positional argument | 7 | 2 | 7 | 7 | 7 |
| `note` | missing `--on` | 5 | 5 | 5 | 1 | 3 |
| `observe` | malformed `--source` ID | 4 | 1 | 0 | 0 | 4 |
| `schema` | unknown operation name | 4 | 2 | 4 | 3 | 2 |
| `start` | missing `--attach` (after `../source.txt` was refused) | 4 | 1 | 4 | 2 | 0 |
| (none) | prose or a stray word in the command position | 5 | 4 | 5 | 4 | 1 |
| `show` | missing ID | 3 | 2 | 3 | 2 | 2 |
| `init` | PATH is a non-empty directory | 2 | 2 | 2 | 2 | 2 |
| `brief`, `case new`, `frontier`, `observe`, `onboard`, `show` | no workspace found | 9 | 4 | 9 | 2 | 4 |
| other | 20 other causes | 21 | 10 | 19 | 13 | 13 |

"Hint" means the refusal printed a repair argv. "Hint followed" means the next CLI call ran the command that the hint names. "Got past it" means a later call of the same command succeeded. The per-call rows, with the command, cause and hint of every refusal in both arms, are in each run's `results.json` (`assignments[].friction`).

### Changes

Before run 1 (from the rehearsal and the bn-6vlm transcripts):

- A relative `--workspace` that names no workspace, run from inside one, names the enclosing workspace and no longer advises `init`. A rehearsal successor had nested a second workspace that way.
- Array-of-strings flags such as `--limitations` say in help and in their refusal that they take a JSON array of strings, with an example.
- A refused quote that holds a typed `\n` says so. Other refused quotes say to copy one passage without shortening, rewording or joining lines.
- Root help: every example names `--workspace`, an exit-code line, and `--no-color`/`--no-pager` say that they change nothing. `ingest --input` says that the manifest is JSON. These close audit help gaps 3, 5, 6 and 7.
- Agent instructions v2 name `init --actor`.

Before run 2 (for run 1's causes):

- Prose in the command position is answered with the two first-use steps and repairs to `onboard`. An unknown single word points at `record` and `schema --list`.
- An `init` repair names `--actor agent:investigator` when the call named none and AGENT is unset.

After run 2 (not measured):

- TEXT values of the shortcuts (`observe TEXT`, `--quote`, `note TEXT`) may start with a hyphen.
- A refused quote names the nearest source line and its exact text (bn-2mkk): the line where the longest start of the quote begins, when that start covers half the quote or runs to a line break, or else the closest line within a bounded edit distance. Its repair argv repeats the call with that line as `--quote` when the source holds the line once. A quote with no near line is called reworded. What is accepted does not change.
- A missing attachment names, and repairs to, a file of the same name in an import root (`../source.txt` from the root).
- `note TEXT` without `--on` is a note on the selected case: the only case, or `--case` (bn-3jr1). The receipt warns that `--on` was omitted and names the case. With several cases and no `--case` the call is refused with `CASE_REQUIRED` and the case IDs, as other case-scoped writes are. The note keeps no evidentiary standing.
- The root help's `Next:` line is the `init` command, not `onboard`.
- bn-2yy4: a `CASE_REQUIRED` refusal lists up to eight case IDs in its message, each with its title when the default audience may see it (`(title restricted)` otherwise), and counts the rest; `error.actual` still holds every ID. The nearest-line quote hint also covers raw `record` requests (it names the operation's `text_quote` pointer to change, says when only the prefix or suffix differs, and compares a `text_lines` selection with those lines only) and sources over 32 KiB, which it reads through `evidence_read` in chunks of one excerpt, up to 512 KiB. `evidence read ARTIFACT --whole` and `--lines START:END` print a source without a selector file, and a reworded quote repairs to that read.
- `init PATH` where PATH lies inside an existing workspace is refused (bn-2cc0). The refusal names the enclosing workspace root and repairs to `investigation --workspace ROOT onboard`; nothing is created. `--nested` asks for a workspace inside another. `init` on a workspace root itself and on a path outside every workspace behave as before.

### Successor citation from brief (W17.10, bn-3bky)

Run 1 and run 2 successors that used `brief` saw observation titles but not the verified quote or its line, so they cited line ranges or read `state/`. `brief` (standard and deep) now gives each observation a `citation`: the exact quote, its `quote_state` as recorded and the line or lines of the quote in its source, plus one line on how to cite (`FILE line N "QUOTE"`). Lines are given only for a verified quote, found again in the same digest-checked bytes, or a verified `text_lines` selector.

Run 3 measured `9ea7b46` (this change on main `e12063e`) with the same runner, cases, seed, models and budgets ([help-only/run3](../training/first-use/help-only/run3/)). Every handoff workspace with observations gave line-located citations in its brief. Successor citation did not rise: 2/12 in the docs arm and 6/12 in help-only, against 5 and 7 in run 2 (answers 7 and 9, limitations 7 and 8, no unsupported claims). Of the 8 answered successors without a decisive citation, 7 cited ranges wider than three lines (for example `source.txt lines 7-10, 13-15` after a brief that showed lines 7 and 9), which the grader does not accept, and 1 cited lines that are not decisive. Two of the 8 did not read `brief`: one never ran the CLI and one read `state/` event files. The remaining loss is how successors summarise, not what `brief` shows. One run per cell; a difference of one or two assignments is noise.

### Cite lines first (bn-102q)

`brief` (standard and deep) now lists a `citations` section after the outcomes and open questions and before the evidence: one cite line per current, in-use observation with a verified quote, `"FILE" line N "QUOTE"` with the source title as FILE (or `"FILE" line N` for a verified line selector). A budget that cuts evidence items keeps these lines; in text mode the evidence item does not repeat the quote. The citation guide and the agent instructions (v5) say to cite each quote on its own exactly as its cite line prints it, never merged into a range or a list, and to read the case through `brief` and `show`, not the event files under `state/`.

Run 4 measured `4d6708a` (this change; the merged commit differs only in `brief --help` wrapping) with the same runner, cases, seed, models and budgets ([help-only/run4](../training/first-use/help-only/run4/)). The docs page grew from 5018 to 6398 bytes (v3 to v5 instructions).

| Successors out of 12 | Run 2 docs | Run 2 help-only | Run 3 docs | Run 3 help-only | Run 4 docs | Run 4 help-only |
|---|---|---|---|---|---|---|
| Current answer | 7 | 9 | 7 | 9 | 10 | 8 |
| Decisive citation | 5 | 7 | 2 | 6 | 10 | 8 |
| Material limitation | 7 | 7 | 7 | 8 | 9 | 7 |
| Source-linked handoff | 5 | 4 | 2 | 4 | 9 | 5 |
| Unsupported claims | 0 | 0 | 0 | 0 | 0 | 0 |
| Successor out of budget or time | 4 | 3 | 5 | 3 | 2 | 4 |

Every answered successor in both arms gave a decisive citation (10 of 10 docs, 8 of 8 help-only); every miss was a successor that ran out of budget before it answered (2 docs, 4 help-only). Docs-arm successors mostly cited `source.txt line N "QUOTE"` once per quote, as the cite lines print it; help-only successors, who do not read the instructions, mostly gave exact quotations without line numbers, which the scoring also accepts. By a text heuristic over the successor transcripts, docs-arm successors that read raw `state/` event files fell from 6 to 2. Against the preregistered W17.03 targets, help-only now misses the receipt and handoff margins because the docs baseline rose (receipt 6 against 9, handoff 5 against 9); unsupported claims and both friction budgets are met. One run per cell; the docs-arm rise is larger than the noise of one or two assignments, the help-only change is not.

Follow-ups under W17 (bn-3hdd): bn-2mkk (a refused quote points at the nearest source passage), bn-3jr1 (`note` without `--on`), bn-2cc0 (`init` inside a workspace), bn-3bky (brief shows each observation's quote and source lines, for successor citation). The arms are `--arm docs --arm help-only` in `scripts/run-first-use.py`, so the W14 release trial (bn-2aic) can reuse them.

### Run-4 causes and changes (bn-gef3, not measured locally)

The local help-only study is closed at runs 1–4. The shared GPU is needed elsewhere, and a partial fifth run was stopped and discarded. Run 4's help-only misses of a receipt within three calls (6 of 12) had these causes, read from the raw transcripts:

- Two gemma investigators never ran the CLI. They wrote notes in a directory they named `investigation`.
- One qwen investigator ran `head -50` on the executable, which filled its context.
- After a prose call or `--init TEXT`, then `--help` and `init`, two gemma investigators slipped. One hit a shell quoting error on an apostrophe (gap 4). The other ran `start WORKSPACE QUESTION` three times (gap 7).
- One qwen investigator piped `--help | head -50`, which cut the output before the examples. Four of the six qwen help-only investigators did this. It then ran `init` without an import root, `source FILE`, and `source add` without an actor, without an import root and with no case (gap 8).

Successor budget exhaustion had two causes. Two gemma successors, one in each arm, produced a single thinking turn longer than the 2048-token turn cap. Four qwen successors overflowed the 16k context: three read `state/` event files despite README.txt, and one docs-arm successor read `brief --format json` and `next --format json` (28 KB together).

Changes, unmeasured with the local models:

- The root help now opens with a "Start here" block, under its one-line description and before the option list, so a `--help | head` shows it. It names the two first commands, `init ./casework --actor agent:investigator --no-vault --import-root .` and `--workspace ./casework … start 'QUESTION' --attach ./FILE`, and says that every later command names the workspace with `--workspace`.
- These slips are now refused with the working command as the repair:
  - a command written as a flag (`--init`, `--brief`);
  - a workspace path given as a positional argument (`start ./casework 'Q'`), which repairs to `--workspace ./casework start 'Q'`;
  - `source FILE`, which repairs to `source add FILE` and names `start --attach`;
  - `source add FILE` in a workspace with no case, which repairs to `start QUESTION --attach FILE`.
- Prose in the command position repairs to the `init` command, or to `onboard` when a workspace is near.
- The workspace README.txt says what `brief` prints. Its first suggested read is the text `brief`, not `--format json`.

Whether the three-call window is fair to the help-only arm is analysed in a note on bn-gef3 for the owner. The target is unchanged.

## Hosted-agent arm (W17.03b, bn-gef3)

The same question, with an agent host instead of a raw local model: Claude Code 2.1.331 in print mode, model alias `haiku`, which reported `claude-haiku-5-5` in all 48 sessions. [`scripts/run-first-use-hosted.py`](../scripts/run-first-use-hosted.py) runs the host and its tool calls in one bubblewrap sandbox. The sandbox holds `/work`, the pinned executable, the host executable and a fresh home with only the access token. The host has its Bash tool only, an empty strict MCP configuration, no slash commands and no session persistence. The protocol and the targets were committed before the measured run ([protocol](../training/first-use/protocol.md#hosted-agent-arm-w1703b-preregistered), [hosted/targets.json](../training/first-use/hosted/targets.json)). The targets are those of the local arm, unchanged. Twelve assignments per arm come from six comparison cases and two replicates (`claude-haiku-a`, `-b`).

**Isolation.** Before the assignments, one probe session per arm asked the host to list every instruction it had beyond the prompt, with its source and first line, and to list its file system and environment. Both probes reported no CLAUDE.md, no memory contents, no skills, no MCP servers and only the Bash tool. The help-only probe also reported no AGENTS.md. The docs probe reported `/work/AGENTS.md` as project instructions. The owner's home and other agent homes did not exist in the sandbox. The only other context was the host's own built-in system prompt and an attribution reminder. No credential value appears in any transcript.

**Docs arm.** The build's agent instructions (`investigation docs agents`, 6398 bytes) are a read-only `/work/AGENTS.md`. Claude Code loads that file through its built-in AGENTS.md plugin. No CLAUDE.md is written.

**All five preregistered targets were met.**

| Measure (of 12) | Docs | Help-only | Target |
| --- | ---: | ---: | --- |
| Receipt ≤3 calls after first CLI contact | 12 | 10 | ≥ docs − 2 and ≥ 6: met |
| … replicate a / b | 6 / 6 | 4 / 6 | |
| Persisted Source and Case at any time | 12 | 12 | |
| Source-linked handoff | 11 | 12 | ≥ docs − 2: met |
| Successor answer / citation / limitation | 12 / 11 / 12 | 12 / 12 / 12 | |
| Unsupported claims | 1 | 0 (≤0.221) | help-only 0: met |
| Refused / all investigator CLI calls | 54/296 (0.18) | 36/251 (0.14) | ≤ docs + 0.10: met |
| Refused calls followed by a successful call of the same command | 43/68 (0.63) | 25/42 (0.60) | ≥ docs − 0.10: met |
| Ran `--help` / `onboard` / `docs agents` (investigator) | 5 / 12 / 2 | 12 / 3 / 11 | |
| Median calls from contact to receipt | 2 | 3 | |
| Investigators that used all 28 calls / signalled handoff | 8 / 4 | 3 / 9 | |
| Phases out of time or budget | 0 | 0 | |
| Mean investigator-plus-successor wall time | 143.7 s | 114.4 s | |

**Reading the result.**

- **Help-only learned from the executable, through its own `docs` page.** All twelve help-only investigators ran `--help | head -50` in their first call and `docs` in their second. Eleven then ran `docs agents` at call 3, and one ran `onboard`. They ran `init` and `start` only after that. So under this host, "help-only" means help plus the agent instructions that the executable itself serves. The protocol allows this. It does not show that `--help` alone teaches the commands.
- **First contact was call 1 in 23 of 24 assignments.** Haiku combined `ls` with `--help` in its first call, so the three-call window counted from that call.
- **The two help-only misses.** Kiln, replicate a, ran `onboard` twice before `init` (receipt at call 7). River, replicate a, ran `init` and `start` as separate calls after `docs agents` (call 5).
- **The docs arm's one unsupported claim.** Greenhouse, replicate a, gave a bay-wide humidity explanation that the key marks as a false promotion. The target applies to help-only only.
- **The most common refusal in both arms was a host artifact.** Claude Code chose `/usr/bin/zsh` as its shell in the sandbox. A bash-style `W="--workspace … --actor …"; investigation $W …` therefore reached the CLI as one argument. This happened in 9 help-only calls over 8 assignments, and in 14 docs calls. The local runner used bash. A later hosted run should set `SHELL=/usr/bin/bash`. The CLI could also repair a single argument that holds several global options.

**Runs and provenance.**

- The measured build is debug, SHA-256 `aab0ad3b…`. It was built from `e5681ddb`, this bone's CLI commit, which the workspace later rebased as `3152ce7e`. The runner and fixtures were at `bd3ba664`, later rebased as `3866875c`.
- The first attempt stopped after one assignment pair on a runner bug: the grader got the family key. That attempt also showed that the host's structured output cannot carry the successor's answer: the model packed all four fields into one, five times. The protocol was amended before the restart. The answer is now the last four-field JSON object in the successor's final message.
- Compact results, the manifest and SHA256SUMS are in [hosted/run1](../training/first-use/hosted/run1/). The raw archives, the measured run and the aborted attempt, are kept outside the repository.
- One run per cell. A hosted model is not pinned by digest, and the two replicates share one model. A difference of one or two assignments is noise.
- Codex was not run, for the reason given in the protocol.
