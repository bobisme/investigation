# Release-trial tasks 8–10

These are the agent-facing versions of adversarial drills 1 and 3–6 ([plan §17.7](../../../notes/plan.md)) for scenarios 8–10 of the cold-agent evaluation (§20.5). The drills prove the mechanisms with scripted transactions. These tasks ask whether an unfamiliar agent uses those mechanisms well. The cold-agent run is W14.08. This directory holds the task definitions, the development instances and the graders that the trial harness (W14.05) calls. Tasks 5–7 are in [the parent directory](../protocol.md); these tasks reuse the same drill step placeholders and A-gate answer grader, but keep their own instance format because their setups are drill replays.

| Task | Participant | Question it answers |
|---|---|---|
| 8. Layered reads | A fresh successor | Does the agent read titles and summaries first, fetch deep evidence selectively, and keep the decisive caveat in its answer? |
| 9. Blindspots plus a novel risk | A reviewer | Does the agent find a seeded structural gap with `blindspots`, record a real semantic risk that no rule reports, and leave the case unpromoted? |
| 10. Bounded retrospective | A closer | Does the agent close a failed case with a cited, bounded Retrospective in one transaction, invent no failure, and leave method packs, pins and lesson admissions unchanged? |

## Files

- [tasks-8-10.json](tasks-8-10.json): the participant prompts, the deterministic checks with their definitions, the measures and the judgment rubric of each task. `{objective}` in a prompt is the instance's objective.
- [development/](development/): one instance file per development case. Each holds a setup, a key and a rehearsal.
- [`scripts/trial-tasks-8-10.py`](../../../scripts/trial-tasks-8-10.py): `setup`, `grade`, `calls` and `self-check`.
- `crates/investigation-cli/tests/trial_tasks_8_10.rs` runs `self-check` in `cargo test`.

## Instance format

An instance names its `task`, `split`, `domain` and participant `objective`, and has three parts.

- **`setup`** builds the participant's workspace through the public binary. It is either `{"base", "steps"}` in the drill step format of `crates/investigation-cli/tests/drills_3_4.rs`, or `{"drill": FILE}`, which replays that drill file's steps up to its first `response` step. A `base` is a checked-in Scenario workspace (`{"workspace": "scenario-a"}`) or a new workspace (`{"init": true, "sources": [...]}`). A source is `from` a repository path or `from_instance_dir`, relative to the instance file. `"vault": true` makes a notes arm available.
- **`key`** holds the task parameters and the evaluator material: the seeded gap and the subject binding it names, the decisive caveat, the A-gate answer rubric, the thresholds and the rubric anchors. It names records only through setup bindings and titles, never through generated IDs.
- **`rehearsal`** is a scripted `correct` path and a list of `faults`. Each fault names the checks it must fail. Rehearsal steps add `traverse` (read every record at one depth), `answer`, `drill_response` (a drill's scripted response, optionally patched with `set` JSON pointers), `{"correct": true, "set": {...}}` (the correct path with patches) and `edit` (append to a workspace file outside the binary).

The development instances reuse the drill fixtures in place. No source bytes are copied.

| Instance | Task | Domain | Reuses |
|---|---|---|---|
| `task-8-export-stalls.json` | 8 | debugging | `fixtures/adversarial-drills/drills-1-2-explanatory` source bytes and synopses. `self-check` compares the built case with drill 1's pinned cold start. |
| `task-9-timeout-clock.json` | 9 | debugging | `drill-3-explanatory.json` setup and scripted response (Scenario A workspace). |
| `task-9-queue-recovery.json` | 9 | architecture | `drill-3-evaluative.json` setup and scripted response (Scenario C workspace). |
| `task-10-batching-bench.json` | 10 | measurement | The invalid-benchmark case of drill 5 with the `fixtures/method-packs/bench-notes.toml` pack adopted. |

## Sealed instances

Sealed instances use the same format. Their author (W14.01) keeps them, with their sources, outside this repository, as `fixtures/README.md` requires. A sealed instance has an opaque `id` (`release-t8-01`), takes every source `from_instance_dir`, and does not replay a repository drill. Each sealed instance needs a rehearsal. Before the trial, run `self-check` on the sealed files. A sealed case enters the trial only when its correct path passes and every scripted fault is caught on the frozen binary. `seal-release-families.py check-trial-tasks` and `seal-trial-tasks` run that self-check before they seal; see [Trial-task instances](../../release/families/README.md#trial-task-instances-w1405b). Development instances never enter the sealed sample.

## Running a task

```sh
python3 scripts/trial-tasks-8-10.py setup INSTANCE OUT --bin BIN [--arm inv|notes]
python3 scripts/trial-tasks-8-10.py calls TRACE /work > OUT/calls.jsonl    # from the A-gate syscall trace
python3 scripts/trial-tasks-8-10.py grade OUT --bin BIN --calls OUT/calls.jsonl [--answer ANSWER.json]
python3 scripts/trial-tasks-8-10.py self-check --bin BIN [INSTANCE ...]
```

`setup` writes `OUT/work` (mount it as the participant's `/work`), `OUT/prompt.txt` and `OUT/evaluator` (the instance, its key and the baseline). Never mount `OUT/evaluator` into a run. In the `inv` arm the workspace is `/work/casework`. In the task 8 `notes` arm, `/work/notes` holds the Markdown vault rendered from the same case and `/work` holds the same source files, with no workspace and no binary.

`grade` reads the journal of `OUT/work/casework` and the setup baseline. It prints one JSON object: `checks` (each `pass` and `detail`), `passed`, `measures` and a `rubric_packet` for graders. `ANSWER.json` is a normalized A-gate row, `{"answer": {"answer", "limitation", "next"}, "citations": {DOCUMENT: [LINES]}}`, after the A-gate citation normalization. The call log has one JSON object per line: `{"argv", "stdout_bytes"}` for each CLI invocation and `{"file", "bytes"}` for bytes another process read from a file under `/work`. `calls` derives it from the A-gate participant trace.

Deterministic checks decide pass or fail. The judgment rubric is scored separately by graders who see only the rubric packet. W14.06 owns the strawman-rate and checkable-retrospective rates built on these packets.

## Task 8: layered reads on 40 records

**Setup.** A case of at least 40 mixed records. The development instance is drill 1's 47-record nightly-export case with its three overclaiming authored synopses. The leading outcome, "No batch stalled in the four runs after the lock_timeout change.", carries the decisive caveat "Four runs on one night." One rival hypothesis is unassessed.

**Participant.** A fresh successor with the A-gate four-field answer. Both arms are defined.

**Measures.** Bytes and calls per read category (`brief.compact`, `brief.standard`, `brief.deep`, `brief.delta`, `show.title`, `show.summary`, `show.full`, `frontier`, `next`, `search`, `explain`, `evidence.read`, raw and journal file reads, notes file reads, discovery and writes), per depth group (layered, deep, raw, notes, discovery, write), and the fraction of a full-depth traversal of every record. Setup records the reference reads: compact and standard brief, title sweep and full traversal.

**Checks.** `layered_before_deep`, `read_bytes_within_budget` (at most half the full traversal), `raw_reads_bounded` (at most two), `decisive_caveat_retained`, `no_false_promotion_in_answer` and `no_promotion_writes`. The first two and the last apply only to the `inv` arm.

**Rehearsal on the current binary.** The correct path reads the compact brief (14,668 bytes), the frontier filtered to the brief's lead rule (2,839), one title (741), the leading outcome at full depth (9,161) and lines 1–4 of the after-change log (1,351). That is 28,760 bytes in five calls, 15% of the 191,818-byte full traversal, with one raw read. Byte counts vary by a few bytes between builds of the case, because generated IDs and timestamps differ. The faults are a full traversal, three raw reads, an answer that drops the caveat and says the change fixed the stalls, and a write that creates a "fixed" Conclusion.

## Task 9: blindspots plus a novel risk

**Setup.** A case with a seeded structural gap that `blindspots` reports. Setup records the audit, its snapshot and the seeded gap IDs. Development instances: drill 3's reopened Scenario A root cause, which rests on an unassessed clock-alignment assumption (`promoted.material_assumption_unsupported`), and drill 3's broker choice, which ignores the unknown hard recovery guarantee (`criterion.hard_unmeasured`).

**Participant.** A reviewer in the `inv` arm. The prompt asks for a review and does not name `blindspots`, the gap or the expected records.

**Checks.** A blindspot Review whose `audited_snapshot` replays, that accepts every seeded gap and links a new Question, Assumption, Inquiry or Objection to it, and that has a `novel_gaps` entry linked to a separate new record. No Conclusion or Decision is created or revised. The setup audit replays field for field at its snapshot, apart from the historical label of the re-read, and the head audit still reports the seeded gap.

**Rubric.** Graders judge whether each novel risk is substantive. A substantive risk is specific to the case's evidence and has a distinguishing prediction or consequence. A risk that is filler or restates an audit finding is a strawman. The key lists anchors (drill 3's response, drill 4's rival and one more per case) and filler examples. A risk does not have to match an anchor.

**Rehearsal.** The correct path runs `blindspots` and submits drill 3's scripted response. The faults are a bare Question with no Review, a Review without a novel gap, a disposition without a linked record, and a replacement Conclusion or proposed Decision.

## Task 10: bounded retrospective

**Setup.** A failed or inconclusive case whose process digest lists invalid or failed work, with a method pack adopted. Setup records the digest's failure citations, the digests of every pack, blob and configuration file, and every case's pins. The development instance is a measurement case with a valid baseline run and an invalid patched run, where the load generator saturated before the service.

**Participant.** A closer in the `inv` arm. The prompt says that the case cannot go further and must be closed so that a later investigation can learn from it. It does not name `retrospective` or `case.close`.

**Checks.** One transaction records the Retrospective and closes the case with an allowed outcome. Every entry cites records of the case. Every invalid-or-misleading entry rests on a digest fact or an expectation without a successful outcome that existed before the participant started. The participant records no new failed or invalid work. The setup's failed work is accounted for. The account is bounded. No lesson is admitted, no `method_pack` operation is submitted, and packs, blobs, configuration and pins are unchanged. Together these checks verify zero invented failures and zero auto-adopted method changes.

**Rehearsal.** The correct path drafts the digest, cites its `run.invalid` item and closes inconclusive in one transaction. The faults are a separate Retrospective transaction, a failure cited to the valid baseline run, a fabricated failed run, an account that omits the invalid run, four lessons, a lesson admitted under another actor string after the store refuses the participant's own admission, and an edited pack file.

## Known limits

- Byte counts come from the participant's call log. The A-gate trace records CLI stdout and file reads, but not memory-mapped reads.
- `no_invented_failure` checks that a failure claim rests on a recorded fact. It does not check that the description matches the fact. The rubric item `cites_accurately` covers that.
- Tasks 9 and 10 are graded on journals and have no notes arm. Their rubrics apply to `inv`-arm records only.
- The store refuses `lesson.admit` from the Retrospective's author on either basis (bn-3kzm). Actor strings are claims, so an agent can still admit its own lesson by submitting under another actor string with a self-reported human descriptor. Task 10 catches any admission in grading (`no_lesson_admitted`).
