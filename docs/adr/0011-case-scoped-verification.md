# ADR 0011 – Case-scoped verification

Accepted with PERF.06 (bn-24bx); 2026-11-23. Refines ADR 0002 and ADR 0010, and plan §10.5 and §10.6. [Module](../../crates/investigation-store/src/scope.rs) · [Store](../../crates/investigation-store/src/store.rs)

**Problem.** Every commit, and every read through `Store::case`, verified every case in the workspace under the commit lock, and built the state of every case in memory. ADR 0010 made each case cheaper to verify, but the cost of a command was still in proportion to the whole workspace, not to the case it named. With the W12.08 load generator (64 records per case), one `record` took 135 ms at 6 cases, 165 ms at 60 and 216 ms at 130; `brief --case` took 23, 106 and 214 ms. W12.08 measured 0.55 s and 1.01 s at 650 cases. Generating the 650-case workspace took about 70 minutes, because each commit cost more than the one before it.

**Decision: a command verifies the cases it reads, and no others.** Each case that a command loads is verified exactly as before: from its genesis, with the kept head of ADR 0010 for the prefix and a full check of the tail. Only the set of loaded cases changes.

- **Reads of one case.** `Store::case` verifies the named case only. The CLI selects a case (`--case`, a prefix, or the only case) from the case directory names (`Store::case_ids`), which are checked but not verified. So `brief`, `show`, `next`, `frontier`, `evidence`, `review-packet` and the other single-case reads verify one case.
- **A commit** verifies its target case. It also verifies each case that it reads: the source case of each lesson consultation it creates, the case that already holds its actor and command ID, and each case for which the index is not current (see below).
- **Workspace-wide commands** verify every case, as before: `check` (always in full, never with a kept head), `list_cases`, `cases`, `receipt`, `repair index`, `search`, capture handles and recovery, lessons and retrospective reads, calibration across cases, `vault render`, and `export` without `--case`. Some commits search the workspace in their preparation: `ingest` with an acquisition key, `capture begin`, and producer-run envelopes. An acquisition or a producer attempt can be recorded in any case. Since PERF.09 (bn-352p), these commits find that case through the index, which records each acquisition key and attempt identity as a claim. They verify only the target case and the case that holds the claim, by the same rule as the idempotency identity below. A commit verifies every case when the index is not current. Index version 6 adds the claims.

**The workspace-wide questions of a commit.** ADR 0002 keeps a workspace-wide idempotency identity `(actor, command_id)`. A commit also refuses a record ID or transaction ID that is already recorded in any case. The loaded cases answer these questions from their verified events. For every other case, the answer comes from the disposable index (`cache/index.sqlite`, plan §10.5). The index already projects receipts by `(actor, command_id)` and records by ID. Index version 5 adds the transaction IDs, so older indexes are rebuilt. The index answers for a case only when, under the commit lock that the commit holds, the indexed head of that case is still its canonical head. This means that the event file at the indexed sequence exists and the next event file does not exist. Event files are published once, in sequence, under that lock. So if any writer, of any build, extended the case after the index applied it, the case fails this test. That case is then verified and answers from its events. A case that the index does not hold also fails the test. The index is not used, and every case is verified, if one of these is true:

- The index is absent, of another version or of another workspace.
- The index names a case that is not published.
- The index cannot answer a query.
- The index places a key in a case whose verified events do not hold it.

A key that the index places in a case is answered from that case's verified events, never from the index alone. After the commit, the index is brought up to date for the cases it loaded. Those are the only cases for which it could be behind.

**Guarantees that change.**

- *A tampered case that a command does not load no longer stops the command.* Before this change, an edit to any case refused every commit and read in the workspace. Now it refuses only the commands that load that case: every read of it, every commit to it, every commit that reads it (as a lesson source, or as the holder of a retried key), every workspace-wide command, and `check`. `check` still verifies every byte. Tests: `a_tampered_case_blocks_neither_a_commit_nor_a_read_elsewhere_and_check_reports_it`, `a_tampered_target_still_refuses`, `a_key_recorded_in_another_case_is_found_and_verified_through_the_index`, `a_consultation_verifies_the_lesson_case_and_no_other`.
- *A single-case read no longer checks the uniqueness of IDs and keys across cases.* It checks them within the case that it loads. `check` and the workspace-wide loads still check them across the workspace.
- *A commit trusts the index for cases that it does not load,* in the same way that ADR 0010 trusts a kept head. A writer who can edit `cache/` can make a commit miss a key or an ID that another case holds, and so record a duplicate. That writer could equally write a valid chain into `state/` (§10.6). `check` reports the duplicate: "An actor and command ID appear twice in the workspace" or "A record ID was created twice". The freshness test compares sequences, not bytes. Suppose a crash loses an event that a reader indexed after its writer's sync failed. A later writer then publishes a different event at the same sequence, and that writer's own index update also fails. In that sequence of events, the index stays behind by one event while its head looks current, until a commit that loads that case or `repair index` corrects it. `check` reports the stale head as `INDEX_STALE`.
- *Nothing else changes.* The storage format is unchanged. A missing or corrupt index costs time, never correctness. Commits still serialize through the coordination lock, and then the case lock (ADR 0002).

**Not cross-case.** The actor of a command comes from its request, the flag, the environment or the workspace configuration, not from the journal. So no actor registry needs to be read across cases. Method pack bytes, session reads and blobs are read from their own workspace stores, as before.

**Rejected alternatives.**

- *Verify the hash chains of the other cases without building their state.* This still reads and hashes every byte of the workspace on every commit.
- *A separate workspace-level key file.* This is a second derived copy of the receipts that the index already holds. Rewriting the file costs time in proportion to the workspace, and an append log needs its own recovery protocol.
- *Hash the head event of every other case instead of comparing sequences.* This gives byte equality of the indexed heads. But it reads one event per case on every commit (about 10 ms at 650 cases here). The rare case that it closes leaves a duplicate that `check` reports. PERF.09 closes that case without the read. The index stores the identity of each head's event file: device, inode, size, modification and status-change times. The freshness test compares that identity with the stat it already makes. An event that a crash lost, and that a later commit published again at the same sequence, has another identity, so its case is verified. A copied or restored workspace also changes every identity, which costs one verification of each case. The index records the new identities when it is next brought up to date.
- *A workspace-wide head or sequence.* ADR 0002 rejects it. It would serialize all cases on one chain and change the storage format.

**Consequences.**

- *Cost.* A commit costs one directory listing of `state/cases`, one read of the index heads and two `stat` calls per other case. A keyed commit also makes one index query per claim. It then verifies only the cases it loads. A single-case read costs one directory listing and the verification of one case. Both are independent of the bytes in other cases. The stat calls still grow with the number of cases, at about a microsecond each.
- *Measured* (release build, the W12.08 generator with seed 20261123, a shared 64-thread machine at load 22 to 39, median of 7 CLI runs):

  | Cases | `record` before | `record` after | `brief --case` before | `brief --case` after | per commit while generating, before / after |
  | --- | --- | --- | --- | --- | --- |
  | 6 | 135 ms | 91 ms | 23 ms | 14 ms | 115 / 115 ms |
  | 60 | 165 ms | not re-run | 106 ms | 14 ms | 117 to 129 / 111 to 126 ms |
  | 130 | 216 ms | 88 ms | 214 ms | 14 ms | 96 rising to 189 / 128 to 131 ms |
  | 650 | 550 ms (W12.08) | 85 ms | 1,010 ms (W12.08) | 15 ms | about 18 rising to 800 (W12.08) / 78 ms for the first 50 cases and 79 ms for the last 50 |

  Generating the 650-case workspace took 16 minutes instead of about 70. `next --case` and `show` follow `brief`. Most of what remains of a commit is fsync latency: 6 syncs, 2 canonical and 4 for the index transaction. Before this change there were 10, because every open of the index rewrote its version.

## Reads share the coordination lock

Amended with bn-2vek, 2026-11-24. [Lock](../../crates/investigation-store/src/durable.rs) · [Tests](../../crates/investigation-store/tests/read_lock.rs) · [CLI test](../../crates/investigation-cli/tests/parallel_reads.rs)

**Problem.** Every read took the coordination lock exclusively, for the whole of its verification, and waited a fixed 2 s for it. So parallel reads ran one at a time, and the last read in line could wait past 2 s and refuse with `WORKSPACE_BUSY`. The trial_tasks_8_10 rehearsal runs 4 read-only `show` calls side by side. It failed once in 3 traced full runs (bn-3dy5).

**What the lock protects a read from.** The lock protects a read from writers. A read never needs protection from another read.

- *A publication that is not yet durable.* A commit writes and syncs a temporary file and renames it into place, then syncs the directory. The event is visible after the rename, but it is durable only after the directory sync. The writer releases the lock only after that sync. A read without the lock could report a snapshot, or a `Found` receipt, that a crash then loses, and an agent could cite it.
- *Writers that do more than append.* `repair journal`, archive restore, retention reclaim and the format migration change canonical files. `repair index` removes the index and then renames the new one into place. A `receipt` that inspected the index between those two steps would report it as degraded.
- *Not torn files.* Each canonical file is published with a rename. A kept head that does not match the journal falls back to a full replay (ADR 0010).
- *Not other reads.* The read paths write no canonical file and no kept head, because only a commit keeps heads. The cache entries a read can replace, such as recorded blob verdicts, are written to a unique temporary file and renamed into place.

**Decision.**

- **Reads take a shared lock.** These store reads take the coordination lock shared: `Store::case`, `case_ids`, `list_cases`, `cases`, `visit_cases` and `receipt(s)`. A shared lock is held beside other shared holders and never beside an exclusive one. These reads are the path of `show`, `brief`, `next`, `frontier` and `receipt`, and of every other read that goes through them.
- **Every other holder takes it exclusively, as before.** That covers commits, `search`, `repair index`, `check`, captures, pins, retention, archive, and configuration and local-actor writes. Both locks are `flock` locks on `state/locks/commit.lock`. So an earlier release, which takes only the exclusive lock, still excludes shared holders, and they exclude it.
- **Writers get preference.** `flock` gives no preference to writers. With shared reads alone, a stream of overlapping reads left a commit no moment to get in: at 24 and 32 readers, 1 of 4 and 2 of 3 commits refused. So the lock has a gate, which is a `flock` on its own directory, `state/locks`. There is no new file and no change to the workspace layout.
  - A writer takes the gate exclusively while it waits for the lock. It drops the gate once it holds the lock.
  - A reader passes through the gate, shared, before it takes the lock.
  - While a writer waits, the reads already in finish and new reads wait behind it.
  - The gate is held only while waiting, never together with the lock. The gate and the lock share one deadline.
  - An earlier release does not use the gate. It is excluded exactly as before, but it gets no preference over current readers.
- **A lock-free read was rejected.** It would lose the first two guarantees above. Keeping them without the lock would require every writer to publish in an order that is safe for readers, and a writer's directory sync cannot be made visible to a reader.

**The wait stays a fixed 2 s, for reads and for writes.** A read now waits only while a writer holds the lock, or while a writer waits ahead of it. Under load, a commit holds the lock for 100 to 800 ms (table below). The commands that hold it for longer are maintenance commands: `repair index`, `export`, `retention reclaim --all` and `migrate`. For a read that meets one of those, the right answer is the refusal it already prints: `WORKSPACE_BUSY`, exit 7, "Retry the identical request". A longer or configurable wait would only delay that answer. A retry inside the program would repeat what the refusal tells the caller to do, and the caller can choose when.

**Write side unchanged.** A writer excludes every reader and every other writer, waits 2 s and refuses with the same failure, as before. The only addition is the gate step. Writers now meet each other at the gate first and then at the lock.

**Measured.** These numbers use a release build, the `just bench` large fixture and `show --depth title` (except where `receipt` is named). The machine is a shared 64-thread machine at load 13 to 65. Each reader runs its calls back to back. "Commits" means a `record` loop running beside the readers.

| Readers | Commits | Before: refused reads, p50 / max | After: refused reads, p50 / max | Commits refused, before / after |
| --- | --- | --- | --- | --- |
| 16 | no | 6 of 48, 879 / 2,155 ms | 0 of 48, 282 / 428 ms | — |
| 32 | no | 16 of 64, 1,238 / 2,295 ms | 0 of 64, 452 / 631 ms | — |
| 24 | yes | 52 of 240, 1,263 / 2,869 ms | 0 of 240, 343 / 520 ms | 4 of 13 / 0 of 19 (slowest 338 ms) |
| 32 | yes | 115 of 320, 1,536 / 2,727 ms | 0 of 320, 619 / 1,231 ms | 3 of 15 / 0 of 13 (slowest 819 ms) |
| 32, `receipt` | yes | 49 of 96, 2,012 / 2,135 ms | 0 of 96, 417 / 538 ms | 1 of 8 / 0 of 4 |

Load alone did not reproduce the trial's 4-reader refusal on this fixture: there were no refusals in 32 calls at load 44 to 49. The CLI test reproduces it deterministically instead. Another read holds the lock past the wait while 4 workers run `show`, `brief`, `next`, `frontier` and `receipt`. Before this change every one of those calls refused with the trial's message, and now every one answers.

**Tests.**

- `shared_holders_share_the_lock_and_exclude_every_exclusive_holder`
- `a_waiting_writer_holds_new_readers_back_and_drops_the_gate_once_it_holds_the_lock`
- `a_read_waits_only_for_writers_never_behind_another_read`, which also covers a commit refused while a read holds the lock, and a read refused while a writer holds it
- `parallel_reads_beside_a_slow_read_all_answer`
- `parallel_reads_answer_while_another_read_holds_the_lock_past_the_wait`

Each was checked against a mutation. With reads back on the exclusive lock, 3 tests fail. With the gate wait removed, the writer-preference test fails. With the gate kept after the lock is taken, every test fails.
