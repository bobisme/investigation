# ADR 0012 – Vault publication in durability batches, and unchanged files known by their status

Accepted with PERF.07 (bn-kxx5); 2026-11-23. Refines plan §12.7 and [ADR 0009](0009-incremental-vault-render.md). Modules: [publish](../../crates/investigation-render/src/publish.rs), [fingerprint](../../crates/investigation-render/src/fingerprint.rs), [manifest](../../crates/investigation-render/src/manifest.rs).

**Context.** The W12.08 load test (bn-hli9) found a full render of 49,401 notes took 25 to 30 minutes, and 99% of that was `fsync`: each file was synced, renamed, and its directory synced, about two syncs per note at about 12 ms each on the shared machine. A render with nothing to write took 23 s and 1.4 GiB at that size: it read and hashed every note, held the whole plan more than once, and stored the 25 MB manifest twice with a sync each time.

## Decision 1: durability in batches

The publisher stages up to 2,048 plan entries (`STAGE_BATCH`). For each entry it inspects the path and decides as before. A write puts the planned bytes in a hidden temporary beside the target, without syncing it. Then one barrier makes the whole batch durable: `syncfs` of the projection root's filesystem. Only then is each temporary committed: the path is inspected and decided again, and the temporary is renamed into place, with a no-replace rename when the path was absent. Removals of retired paths are no longer synced one by one. Each later barrier, and a last barrier after the final rename or removal, makes them durable. Where `syncfs` does not exist (not Linux), each file and directory is synced as before and the barrier does nothing.

**Why the guarantees hold.** The three that matter:

1. *A crash never leaves a torn file at a path.* A temporary's bytes are durable before its rename is issued, so after a power loss a path holds its earlier bytes or the complete planned bytes, as with per-file syncs. A process kill loses nothing written, so the same holds after a kill.
2. *The manifest never claims what a crash can undo.* The write-ahead record (`updating`, with the planned hashes as intent) is stored and synced before the first vault change, as before. The final record is stored after the last barrier. On an I/O failure, only outcomes a barrier followed are recorded in the `partial` record (`Publisher::publish_durably`, `Interrupted::durable`). Every other path the render may have written keeps its write-ahead intent, so the next render accepts those bytes as its own and does not report them as edits. `vault remove` syncs once before it forgets the removed paths, and if that barrier fails it stores nothing, which leaves the `updating` record.
3. *A hand edit is never overwritten.* The decision that allows a rename is made just before the rename, on bytes read and hashed then, never on what staging saw. So the window in which an editor can race a write is shorter than before, when it included the temporary's sync. A file that appears at an absent path is still refused by the no-replace rename. A temporary whose folder was moved during the batch is left unrenamed (`UnsafeParent`). The plan §12.7 contract is unchanged: a racing unsaved buffer or sync client is outside what any lock can coordinate.

Convergence after a kill at any point is the same mechanism as before: `updating` plus intent, and the sweep of leftover temporaries. A kill during a batch leaves up to a batch of `.inv-tmp` files, and the next render removes them before it publishes. Tests: every existing kill and crash test in `tests/generations.rs` passes unchanged. New tests cover a kill between the last write and the manifest commit, both before and after the final barrier (`a_render_killed_between_its_writes_and_the_manifest_commit_is_completed_next_time`), and a kill in mid-batch that leaves only temporaries and earlier-or-planned bytes (`a_render_killed_mid_batch_leaves_only_temporaries_the_next_render_sweeps`). `a_file_created_during_publication_is_not_overwritten` now races the second inspection.

**Conflict copies.** A render that finds drift keeps the content it withheld under `local/conflicts/<generation>/`, and the manifest names each copy. These copies are also written without per-file syncs, and one barrier on the filesystem holding `local/` follows them before the manifest that names them is stored. A crash before that barrier leaves at most a copy the manifest does not name yet. An earlier copy is reused only after its bytes are read and hashed again, so a damaged copy is written again. `vault reconcile` still syncs each file it sets aside.

**Cost.** `syncfs` flushes every dirty page on the filesystem, other processes' included. On a shared disk a barrier can wait for someone else's writes; one unchanged render here waited 2 s. A publication that changed nothing, and only confirmed bytes the manifest already records, skips the final barrier. A full render pays about two barriers per 2,048 notes.

## Decision 2: an unchanged render reads and writes less

- *No copies of the plan.* Plan content is shared (`Arc<[u8]>`), and stamping, announcing projection health and keeping an earlier basis change entries in place instead of rebuilding and rehashing the whole plan.
- *The manifest is parsed once.* It is read straight into its wire form, not through a JSON tree of the whole file.
- *No needless stores.* The write-ahead record is not stored when the render cannot change a vault file: the last render completed, no planned hash differs from the owned one, and nothing is retired. In that case every write needs an intent the record does not hold. The final record is not stored when its bytes equal the bytes loaded.
- *Stat fingerprints.* This changes a guarantee. See below.

### Stat fingerprints (a guarantee change)

An owned record can keep the fingerprint of the file its bytes were last observed in: device, inode, size, modification time and status-change time (`ctime`), in nanoseconds. When the file at the path has exactly that fingerprint, the publisher takes it to hold the owned bytes without reading it. On any difference it reads and hashes the file as before.

Drift detection was "every owned file is hashed on every render". It is now "every owned file is hashed unless its status is exactly the status it had when it was last hashed". The two are equivalent for every edit made through the filesystem, because every change of a file's bytes sets its `ctime` to the current time, and no system call sets `ctime` to a chosen value. **An edit that keeps the size and restores the modification time is still detected**, because restoring the modification time is itself a `ctime` change (test `an_edit_that_keeps_the_size_and_modification_time_is_still_drift`). An editor that saves through a new file changes the inode as well.

The limits, stated plainly:

- **Granularity.** An edit in the same clock tick as the observation could keep `ctime`. So a fingerprint is recorded only when the file's status was identical before and after it was read, and its `ctime` is at least two seconds older than the start of the render. A file written or touched recently is hashed again next time and recorded then. As a result, the render right after a full render still reads every file, and the renders after that do not.
- **Filesystems.** Only ext4, XFS, Btrfs and tmpfs projection roots use fingerprints. On every other filesystem, such as an existing vault on FAT or a network share, every file is read on every render.
- **Defeats.** The check is defeated by setting the system clock back far enough to reproduce a recorded `ctime` to the nanosecond, while also keeping inode, size and modification time. It is also defeated by writing to the block device under the filesystem, or by a program that keeps rewriting an already-dirty page of a shared memory map (the kernel updates times when a page first becomes dirty). These are not edits by a viewer or a sync client, which are the cases drift detection is for.
- **Writes never rely on it.** A fingerprint can only confirm a file unchanged. Before any replacement, the commit step reads and hashes the file again (test `a_forged_fingerprint_never_lets_an_edit_be_overwritten`). A fingerprint is dropped whenever its path is written. A manifest without fingerprints (an older one, or one rebuilt after loss) only makes the next render read every file once.

## Measurements

Release builds, on a shared Threadripper (load average 22 to 38 during the runs), ext4 on NVMe, warm page cache. Workspace: the W12.08 load generator, seed 20261123, 130 cases (8,320 objects, 9,881 notes), baseline variant. Method: one generated workspace, two copies, one per binary, with renders alternated between the binaries (A/B, as in PERF.04). "Before" is main at 1a4c6cb.

| 130 cases, 9,881 notes | Before | After |
| --- | --- | --- |
| Full render of an empty vault (n=1 each) | 280.8 s, 263 MB peak RSS | 1.40 s, 200 MB |
| Unchanged render (n=5 each after one warm-up pair) | 1.12 to 1.20 s, 264 to 274 MB | 0.94 to 0.98 s, 202 to 208 MB |
| First render after 90 edits and 10 deletions (100 conflict copies; n=1 each) | 6.5 s in the harness; 6.14 s with batched publication but a sync per copy | 1.65 s |
| Manifest | 5.1 MB, rewritten on every render | 5.8 MB with fingerprints; not rewritten when unchanged |

The two vaults are byte-identical (`diff -r`), and the vault goldens are unchanged, so `RENDERER_VERSION` stays. Of the remaining unchanged-render time at this size, planning takes about 0.65 s and reading the snapshot about 0.14 s (phase timings from a temporary instrumented build). Neither is changed here: planning is PERF.04/05's area and the snapshot read is PERF.06's.

A filesystem microbenchmark (1,000 to 5,000 files of 5 KB, temporary file then rename, on the same disk) gave 25 ms per file with a sync per file and per directory, 2.8 ms per file with `sync_file_range` and then `fdatasync` per file, 0.06 to 0.19 ms per file with one `syncfs` per batch of 512 to 4,096, and 0.01 ms per file with no syncs.

At 650 cases (41,600 objects, 49,401 notes; same generator and seed, A/B on two copies of one generated workspace, alternating order; load average 21 to 28):

| 650 cases, 49,401 notes | Before | After |
| --- | --- | --- |
| Full render of an empty vault (n=1 each) | 1,369 s, 1.20 GiB | 9.4 s, 0.90 GiB |
| First render after the full render | 24.2 s, 1.27 GiB | 9.7 s, 0.95 GiB (reads every file once more and records fingerprints) |
| Later unchanged renders (n=2 each) | 8.3 and 6.3 s, 1.28 GiB | 5.2 and 5.3 s, 0.95 GiB |
| Manifest | 25.3 MB | 29.1 MB |

The 49,401-file vaults are byte-identical. The W12.08 figure of 23 s for an unchanged render matches the first render after a full render here; later ones are faster on both sides, because the files are then in the page cache. In a later unchanged render at 650 cases (phase timings from a temporary instrumented build), planning takes 3.6 s, reading the snapshot 0.7 s and publication 0.2 s; the snapshot accounts for about 590 MiB of peak memory and the plan for about 370 MiB more. Both are now the cost to cut, and both are outside this decision.

Measured again after PERF.06 (case-scoped verification, adf6ed3) landed, unchanged renders only, alternating on the same rendered copies (n=4 each, load average 31 to 48): at 650 cases 5.6 to 6.2 s and 1.24 GiB before, 5.1 to 5.4 s and 0.95 GiB after; at 130 cases 1.16 to 1.24 s and 263 MiB before, 0.98 to 1.05 s and 205 MiB after. Full-render cost is `fsync`, which PERF.06 does not change.

## Rejected alternatives

- *Write without syncing, then sync each directory and `fsync` once before the manifest commit.* After a power loss a rename can be durable while its file's data is not, which leaves an empty or partial file at the path. The next render would see bytes that are neither the earlier nor the planned ones and would report a hand edit that nobody made. Staging the bytes and syncing them before the rename avoids that at the cost of one more barrier per batch.
- *`sync_file_range` then `fdatasync` per file.* This measured 2.8 ms per file on this machine, against under 0.1 ms per file for batched `syncfs` and 25 ms per file before.
- *Size and modification time only, as the bone suggested.* Tools that restore the modification time after an edit would hide the edit. `ctime` closes that gap at no cost.
- *Streaming the plan per case.* This would bound memory by the largest case rather than the workspace. It needs drift, intent, announcement and incremental keeping to work per case. It is a follow-up; shared plan content removes the copies, but not the one plan held in memory.
