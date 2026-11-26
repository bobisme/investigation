# Release-trial harness (W14.05)

`scripts/release-trial.py` runs the paired release trial of plan §20.5 as the [preregistration](../../docs/release-trial.md) describes it. It plans, runs, grades and freezes every task type in both arms. This bone builds and dry-runs the harness only. The sealed trial is W14.08: it costs real money and needs the owner's approval. The deterministic acceptance measures (W14.07) and the analysis (W14.13) read the records described in [Record format](#record-format). [Sealed families](../release/families/README.md) · [Tasks 5–7](../trial-tasks/protocol.md) · [Tasks 8–10](../trial-tasks/tasks-8-10/protocol.md) · [A-gate pilot](../a-gate/protocol.md)

```sh
python3 scripts/release-trial.py dry-run --bin target/debug/investigation --output DIR    # development set, stub participant
python3 scripts/release-trial.py self-test --bin target/debug/investigation               # unit checks plus a dry run (cargo test)

python3 scripts/release-trial.py adapter-check --backend-command 'ADAPTER ARGS' --model MODEL-A --model MODEL-B
python3 scripts/release-trial.py plan --split sealed --bin RC --binary-commit SHA --output /outside/repo/trial \
  --model MODEL-A --model MODEL-B --seed SEED --backend command --backend-command 'ADAPTER ARGS' \
  --adapter-sha256 REVIEWED-SHA --opened /outside/repo/opened --trial-tasks /outside/repo/opened/trial-tasks/trial-tasks.json
python3 scripts/release-trial.py run /outside/repo/trial --opened /outside/repo/opened
python3 scripts/release-trial.py grade /outside/repo/trial --opened /outside/repo/opened
python3 scripts/release-trial.py verify /outside/repo/trial
```

## Task types and arms

| Task type | Source | Sessions | Arms | Answer key | Mechanics grader |
|---|---|---|---|---|---|
| `family` | Base scenarios 1–4, four domains, `original` and its `missing` or `cosmetic` twin | Investigator, review checkpoint, staged `update.txt`, handoff, fresh successor | `notes`, `inv` | Family key | Boundary and journal facts (graded by W14.06/W14.07) |
| `lookup` | Skip-the-tool tasks | Investigator then successor, no update | `notes`, `inv` | Lookup key | `unnecessary_tool_use`: a journal event in the CLI arm |
| `task-5` | Stale-snapshot resume | One successor session | `notes`, `inv` | Task key | `trial-tasks.py grade` |
| `task-6` | Lost response and conflict | One investigator session with faults | `notes`, `inv` | Task key | `trial-tasks.py grade` |
| `task-7` | Two-stage injection | Stage 1 investigator; stage 2 successor under `guardrails-off` (both arms) and `guardrails-on` (CLI arm) | `notes`, `inv` | Task key per stage | `trial-tasks.py grade` |
| `task-8` | Layered reads on a 40-record case | One successor session | `notes`, `inv` | Instance `answer_rubric` | `trial-tasks-8-10.py grade` |
| `task-9` | Blindspots plus a novel risk | One reviewer session, ends at `done.ready` | `inv` | None | `trial-tasks-8-10.py grade` |
| `task-10` | Bounded retrospective | One closer session, ends at `done.ready` | `inv` | None | `trial-tasks-8-10.py grade` |

Tasks 9 and 10 are graded on journals, and their specifications define no notes arm (see [tasks 8–10, Known limits](../trial-tasks/tasks-8-10/protocol.md#known-limits)). The harness therefore runs them in the CLI arm only.

**Arms.** Both arms get the same system prompt, task prompt, sources, budgets, sandbox and handoff boundary. The `inv` arm also has the frozen binary at `/tools/investigation`, which the participant finds through ordinary help ("discovery only through ordinary help", §20.5). Family and lookup prompts are the A-gate prompts. Tasks 5–7 use their own per-arm prompts followed by the shared four-field answer format. Tasks 8–10 use the prompts in `tasks-8-10.json`.

**Primary and secondary.** Only `family` assignments without a condition are `primary: true`. They form the 672-assignment comparison that the preregistration sizes: 84 families, 2 variants, 2 models and 2 arms. Lookups and tasks 5–10 are diagnostics, which the analysis reports separately. `--secondary docs` adds single-arm `inv` assignments in which the prompts also carry `investigation docs agents`, as in the W17.03 docs arm. These assignments are also `primary: false`. Optional MCP conditions for W13.06 belong in the same table (`SECONDARY` in the script) once that frontend exists.

## Plan: what is frozen before any run

`plan` writes `plan.json` once and records its SHA-256 in `plan.sha256`. `run` and `grade` refuse to continue if the plan file changed. The plan fixes the following:

- **Binary.** A copy is placed at `frozen/investigation` with mode `0555`, and its digest and commit are recorded. Runs use only the copy. Its digest is checked when each assignment starts and when the batch ends.
- **Inputs.** The plan stores the digest of every file the harness reads: the harness and the scripts it loads, the registry, the case-set manifest, the task 8–10 specification, the preregistration, and every trial-task input (each task or instance file, each task 5–7 key and source file, and each task 8–10 instance source or drill). `run` and `grade` refuse to continue if any of them changed.
- **Case set.** `development` reads `training/release-trial/dry-run/manifest.json` and loads sources from `fixtures/`. `sealed` reads the W14.01 manifest and an opened bundle outside the repository. It reads objectives and sources from `<opened>/sealed/` and keys from `<opened>/evaluator/`. Each file is checked against its registered digest when it is used. Keys are read only by `grade`. The plan stores key digests, never key content.
- **Backend, models and inference.** The plan records the backend kind, the model names and the identifiers the backend reports, the adapter's command line, the digest of every adapter file (see [Pinning the adapter](#pinning-the-adapter)), the conformance result, the inference options (the pilot's: temperature 0, a 2,048-token cap per response) and the metering statement.
- **Budgets, prompts and sandbox.** The plan records the pilot budgets: an investigator, reviewer or closer gets 210 s, 28 shell calls, 180,000 input and 14,000 output tokens; a successor gets 120 s, 14 shell calls, 95,000 input and 6,000 output tokens. It also records each shell call's limit of 12 s and 12,000 bytes, the prompt templates, the sandbox mode, the trace, the sandbox limits and the versions of bubblewrap, strace, bash and Python.
- **Schedule.** The plan stores the seed and every assignment. See [Schedule](#schedule).

The sealed split refuses the following: the stub backend, fewer than two models, a trial directory inside the repository, an opened bundle inside the repository, a missing bubblewrap sandbox or syscall trace, and a command adapter that fails the [conformance checks](#adapter-conformance), lives inside the repository or has no `--adapter-sha256`. It also needs either `--trial-tasks FILE` or an explicit `--families-only`. It checks that the primary assignment count equals the preregistered 672.

**Trial-task file.** `--trial-tasks FILE` lists task 5–10 instances as `{"5": [paths], ..., "10": [paths]}`, with two optional keys: `source_root`, the directory that holds task 5–7 sources as `<family>/<variant>/`, and `key_root`, the base of each task 5–7 `key`. Relative paths are relative to the file. Without them, sources come from `fixtures/development/trial-tasks/` and keys from the repository, as in development. The plan stores the absolute paths and the roots.

**Sealed trial tasks.** The sealed instances are sealed in a bundle of their own (see [Trial-task instances](../release/families/README.md#trial-task-instances-w1405b)). `open` writes them to `<opened>/trial-tasks/`, and the index there, `trial-tasks.json`, is the file to pass. In the sealed split, `plan`, `run` and `grade` check the following against the manifest's `trial_tasks` commitments: the opened tree matches its committed digest; the file given is the committed index; the planned instances are exactly the committed ones; and every input outside the repository lies inside the opened tree. Only digests are compared. If the manifest commits no instances, the sealed plan with `--trial-tasks` is refused, and `--families-only` remains the only option.

## Schedule

Assignments come from a recorded seed.

- **Families.** For each model and domain, the families are shuffled and alternate which arm goes first. A family's twin takes the opposite order to its original, and the second model takes the opposite order to the first. Each family is therefore balanced over its four paired blocks.
- **Other paired units.** Lookups, tasks 5–8 and the task 7 stage-2 pair alternate within each task type and model.
- **Blocks.** A block is one variant, one model and its arms in order, with one `block_seed` that both arms use. A variant is always a fresh session, so no session ever sees both variants of a family. Blocks are shuffled within each model, and models run one after another.
- **Sequence.** `sequence` numbers the assignments, and the IDs `r0001`, `r0002` and so on follow that order. An ID reveals neither the arm nor the model.

Sealed assignments run serially (`--parallel` is refused). `run` skips assignments that already have a `run.json`. It moves a partial run directory to `aborted/` and logs the move in `deviations.jsonl`.

## Sessions, sandbox and isolation

Each phase is a new session: a new session ID, and a conversation seeded only with the system prompt and its task prompt. Each assignment has a new work directory. The session loop is the A-gate loop, generalized. It keeps the pilot's fixes: answer submission stays possible after the shell cap; a handoff marker created in the same turn as the review checkpoint is refused (`premature_handoff`); the update is staged before the next call after `review.ready`; and input limits are checked before each turn.

The `bwrap` sandbox gives the participant a private `/work`, a read-only `/usr`, read-only sources (`source.txt` and `update.txt` are bind-mounted read-only), a tmpfs `/tmp`, no network, a cleared environment and the pilot limits (`prlimit --fsize=2097152 --nofile=128`). `/tools` holds the frozen binary, the bridge shim or nothing.

**Bridge.** Tasks 5–7 need evaluator-side behavior: task 6's lost response and concurrent amendment, and task 7's compact-read policy. The harness also needs a call log when the trace is unavailable. For these runs, `/tools/investigation` is a small shim that forwards each call over a Unix socket to the harness. The harness then runs `trial-tasks.py`'s proxy, or the binary itself, in a second sandbox with the same mounts. The proxy's plan, the fault state and the call log stay in `runs/<id>/evaluator/`, which is never mounted.

**Isolation checks for every run.** These checks address "no sealed key or rubric is readable from inside a run":

- Before the first session, a probe runs inside the participant's sandbox. It checks that none of these paths exists: the repository, the trial directory, the run's evaluator directory, the key directory, the case-set manifest directory, `training/`, the task files' directories, the opened bundle and `$HOME`. It also checks that the network namespace differs from the host's. A failure writes the run with status `harness_refused` and stops the trial.
- A content scan hashes every file in the work directory after setup and again after the run. A file whose digest equals any key, rubric or task file is a failure, and so is a symlink that leaves the work directory.
- The self-test shows that both checks are not vacuous: the probe reports a visible path, and the scan finds a copied key.

`--sandbox none` exists only for development dry runs on hosts without a usable bubblewrap. With it, the participant shell runs unsandboxed in the work directory. The plan and every run record say that isolation is not enforced.

## Backends

- **`stub`.** An offline, deterministic participant for dry runs. It plays one generic script in both arms: it lists the directory and reads the sources, uses the CLI if one is on PATH (`init` and `start`, or `brief`) and otherwise reads and appends `NOTES.md`, honors the review checkpoint and end marker, and submits a placeholder answer. Its token counts are an estimate (serialized characters divided by 4), and the metering statement says so.
- **`command`.** An operator-supplied adapter, run once per model turn. Its stdin is `{"kind": "chat", "model", "session_id", "messages", "tools", "options", "deadline_seconds"}`. Its stdout must be `{"message": {"role", "content", "tool_calls": [{"function": {"name", "arguments"}}]}, "prompt_eval_count", "eval_count", "prompt_eval_cached_count"?, "done_reason"?}`. A `{"kind": "describe", "models": [...]}` call at plan time returns `{"provider", "version", "metering", "models": {name: identifier}}`. Each request carries the whole conversation, so an adapter needs no state and cannot carry one session into another. An adapter exits non-zero on a request `kind` it does not know. The repository ships no host-specific adapter: the operator writes it and keeps it outside the repository, and `ADAPTER_EXAMPLE` in the script shows the shape. Report cached input in `prompt_eval_cached_count`. The pilot's engine did not report it, and the record states when it is missing.
- **`local`.** The A-gate pilot's local endpoint (`scripts/run-a-gate.py`).

### Adapter conformance

`adapter-check` runs the checks that every `command` adapter must pass. `plan` runs them again for any command backend, and refuses the plan if one fails. For each model, they make two short chat turns, so they cost a few model turns. The checks are as follows:

- `describe` returns a nonempty `provider`, `version` and `metering`, and an identifier for every model.
- A first turn asks for one `submit_answer` call. The reply has role `assistant` and a string `content`. It names only the tools offered, its arguments are an object or a JSON object string, and the `submit_answer` call carries all four answer fields.
- A later turn carries an assistant tool call and its `tool` result. The reply has the same valid shape.
- `prompt_eval_count` and `eval_count` are non-negative integers, and `prompt_eval_count` is not 0. `prompt_eval_cached_count`, when present, is a non-negative integer.
- Each call returns before its deadline.
- An unknown request kind gets a non-zero exit.

The checks cannot show that an adapter keeps no state between calls. The operator must review that in the adapter's code. The self-test runs the checks against `ADAPTER_EXAMPLE`, written to a temporary directory, and against a deliberately broken adapter, which must fail. Neither is committed as an adapter.

### Pinning the adapter

1. Write the adapter outside the repository, review it, and record its digest: `sha256sum /outside/repo/adapter.py`.
2. Run `adapter-check` with the exact command line the plan will use.
3. Pass that command line to `plan` as `--backend-command`, and pass the reviewed digest as `--adapter-sha256`. Repeat `--adapter-sha256` for each file that you reviewed. The plan refuses the adapter if a pinned digest matches none of its files. The sealed plan requires at least one pin and refuses an adapter file inside the repository.
4. The plan records the digest of every adapter file: the executable, and each argument that names an existing file, such as an interpreter's script. `run` recomputes these digests and refuses to continue if any changed. The same check applies when `run --backend-command` points to the same adapter at another path.

The operator's adapter for hosted CLIs, and what its development dry run showed, are described in [Operator adapter for hosted CLIs](hosted-adapter.md).

## Cost capture

Each phase gets a cost record with fixed keys. A key is `null` only when no meter measured it, and `metering` names the meter.

| Fields | Meter |
|---|---|
| `wall_ms` | Monotonic clock, per phase. The run total is investigator plus successor time, the preregistered burden quantity. Setup and grading time is kept apart in `setup_and_harness_ms`. |
| `model_turns`, `input_tokens`, `output_tokens`, `cached_input_tokens_reported` | Backend-reported. Input includes cached tokens when they are reported. |
| `shell_calls`, `shell_output_bytes`, `truncated_shell_calls` | The session loop. |
| `discovery_calls`, `discovery_bytes`, `onboard_calls`, `evidence_bytes`, `note_read_bytes`, `note_write_bytes`, `preparation_calls` | The A-gate syscall trace (`a-gate-cost.py`), as in the pilot. Without the trace, the CLI-level counts come from the bridge log, and the file-level counts are `null`. |
| `first_receipt_call`, `first_onboard_call`, `first_note_call`, `calls_from_onboard_to_receipt` | As in the pilot. |
| `cli_calls`, `read_depth`, `read_categories` | The call log, classified by `trial-tasks-8-10.py`: calls and bytes per read category (`brief.compact`, `show.full`, `evidence.read`, raw and journal file reads and so on) and per depth group (`discovery`, `layered`, `deep`, `raw`, `notes`, `write`). |

## Grading, reconciliation and freezing

`grade` does the following in order:

1. **Blinding.** For every assignment with an answer key, it normalizes the answer with the A-gate grader: citations become seed-document line selectors, and record IDs, digests and tool names are stripped. It writes `grades/blinded.json` with random response IDs in seeded shuffled order. The rows carry only the key reference, never the arm or model.
2. **Answer scoring.** It scores the rows into `grades/answer-grades.json`. Only then does it write `grades/assignment-map.json`. A planned assignment with no answer still gets a row and misses every warranted item.
3. **Mechanics.** It grades mechanics separately into `grades/mechanics/<id>.json`, each on a scratch copy, so the run's final snapshot stays as the participant left it.
4. **Joined records and summary.** It writes `grades/graded-runs.json`, one row per planned assignment, including runs that never happened (`not_run`). It also writes `grades/summary.json`, descriptive counts with denominators. The paired, family-clustered analysis is W14.13.
5. **Reconciliation.** It writes `grades/reconcile.json`: planned against attempted assignments, statuses, aborted attempts, deviations and any reused session ID.
6. **Freezing.** It writes `frozen.json`, the SHA-256 of every file in the trial and a digest over that list. Then it removes write permission from the whole tree. `run` and `grade` refuse a frozen trial. `verify` recomputes the hashes and reports changed, missing and added files. The freeze is tamper-evident, not tamper-proof: the owner can restore write permission, and `verify` then shows any edit.

## Record format

The trial directory has this layout. W14.07 and W14.13 read it.

```
plan.json  plan.sha256        frozen schedule and inputs (schema inv.release-trial-plan.v1)
frozen/investigation          the binary under test
runs/<id>/run.json            the run record (inv.release-trial-run.v1, run-record.schema.json)
runs/<id>/<phase>.jsonl       transcript per phase: started, model, shell, fault, source_injected,
                              premature_handoff, submitted, deadline, backend_error, finished
runs/<id>/<phase>.trace/      raw syscall traces, one per shell call
runs/<id>/answer.json         the four submitted fields, or null
runs/<id>/calls.jsonl         CLI calls {argv, stdout_bytes, exit?, phase, shell_call, via} and
                              file reads {file, bytes, phase, shell_call, via: "trace"}
runs/<id>/work/               the final workspace: journal (CLI arm) or notes (notes arm)
runs/<id>/handoff/  review/   family and lookup snapshots at handoff and at the review checkpoint
runs/<id>/evaluator/          setup records, the proxy plan and proxy call log; never mounted
grades/…  deviations.jsonl  aborted/  frozen.json
```

**Run record (`run.json`).** The top-level fields are as follows:

- `assignment`: the planned row, with `id`, `sequence`, `block`, `block_seed`, `task_type`, `task`, `family`, `variant`, `domain`, `twin`, `stage`, `condition`, `primary`, `model`, `arm` and `order`.
- `plan_sha256`, `started_at` and `finished_at`.
- `environment`: the binary digest checked at start, the model and its identifier, the backend, the sandbox and the trace.
- `status`: `completed`, `timed_out`, `budget_exhausted`, `crashed`, `harness_error` or `harness_refused`. It is the first phase status that is not `completed`.
- `phases.<name>`: `session_id`, `role`, `status`, `interruption` (`submitted`, `marker`, `returned`, `returned_without_marker` or `null`), `transcript`, `prompt_sha256`, `answer_expected` and `cost`.
- `boundary`, for family and lookup runs: the A-gate review and handoff facts.
- `setup`, for tasks 5–10: the evaluator record's path.
- `isolation`: the probe result, plus `content_scan` and `content_scan_final`.
- `snapshots.final`, and for family and lookup runs also `handoff` and `review`: each has a path, a tree digest, a journal event count and a list of notes files.
- `answer` and `calls`: relative paths, or `null`.
- `cost.phases` and `cost.total`, as described in [Cost capture](#cost-capture).
- `setup_and_harness_ms`.

The self-test checks every run record of the dry run against `run-record.schema.json`.

## What the dry run shows

The self-test dry run uses one model. On the development set it plans 31 assignments:

- 16 family runs: four domains, original and twin, both arms;
- 2 lookup runs;
- 2 runs each of tasks 5, 6 and 8;
- 5 runs of task 7: stage 1 in both arms, stage 2 with `guardrails-off` in both arms, and stage 2 with `guardrails-on` in the CLI arm;
- 1 run each of tasks 9 and 10.

The default `dry-run` uses two models and doubles this to 62. With bubblewrap and strace, all 31 runs complete in about 6 s with six parallel workers on an idle host; parallel dry runs start the slowest task types first. Every run has:

- transcripts;
- an answer where its task asks for one;
- a final snapshot with journal events (CLI arm) or notes files (notes arm);
- a cost record in which every field is metered;
- an isolation probe that passes, and empty content scans.

The checks also confirm these points:

- In CLI-arm family runs, the review checkpoint stages the update, and the handoff snapshot holds a journal.
- Task 6's notes fault fires.
- The bridge feeds the task 5–7 proxy log.
- Grading is blind, and every mechanics grader runs without error.
- Reconciliation is complete.
- The frozen tree is read-only, refuses a rerun, and an edited record fails `verify`.

The stub's placeholder answers score 0, apart from an incidental citation hit on task 8. The mechanics graders give the expected results for a participant that only reads: the task 5 and task 7 journal checks pass because nothing was written; task 6 passes in the notes arm, where the stub's appended line triggers the fault and the colleague's line survives, and fails in the CLI arm, where the conclusion was never brought up to date; task 8 fails on the missing caveat; and the task 9 and 10 checks fail because no review or closure was recorded. These scores are not results. They show only that every grader runs on real run records. `--sandbox none` runs the same dry run without isolation or a trace, and the self-test passes in that mode as well. CI hosts without a usable bubblewrap use that mode automatically.

## Development dry-run set

[`dry-run/manifest.json`](dry-run/manifest.json) lists four synthetic families, one per domain, and one lookup. They are registered in `fixtures/family-registry.json` under the `development` split, with sources in `fixtures/development/release-dry-run/`. The set also lists one development instance for each of tasks 5–10. The keys in [`dry-run/keys/`](dry-run/keys/) use the A-gate rubric format and carry `reference_answers` and an `overclaim_answer`. The self-test checks that each reference answer scores every warranted item and that each overclaim answer is flagged. The set exists to exercise the harness. A builder wrote it, so its results support no claim.

## Known limits and seams

- **Sealed tasks 5–10 are not authored yet.** W14.03 and W14.04 built development instances only. The seal, the commitments, the opened layout and the plan's checks exist (W14.05b). The instances themselves must come from authors who did not build the product (see [Trial-task instances](../release/families/README.md#trial-task-instances-w1405b)), and the owner seals them with the passphrase. Until then, the sealed plan takes only `--families-only`.
- **Sealed task 5–7 rehearsals are content-independent.** The scripted correct and trap paths in `trial-tasks.py rehearse` are written against the development families. For another instance, `trial-tasks.py check` covers prompts, rubric answers, and setup and grading in every arm, stage and condition. It does not play a correct or trap path. Task 8–10 instances carry their own rehearsal, and `self-check` plays it.
- **Bridged runs show a Python shim.** In bridged runs (tasks 5–7), `/tools/investigation` is a Python script, not the ELF binary, and the binary's own file reads are not traced. Both arms of a task are metered the same way.
- **No notes arm for tasks 9 and 10.** Task 9 and 10 runs are in the CLI arm only.
- **Isolation is not proof against every leak.** The checks are a path probe and a byte-identical scan. A key that was paraphrased or re-encoded into the work directory would not match. Keys never enter a run by construction: only `grade` reads them.
- **Parallel dry runs inflate wall times.** With `--parallel`, wall times include contention. Sealed runs are serial.
