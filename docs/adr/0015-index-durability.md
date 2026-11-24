# ADR 0015 – Index durability: keep the fully synced rollback journal; a rebuild syncs once and never inherits a journal

Accepted with PERF.09 (bn-352p); 2026-11-24. Refines [ADR 0011](0011-case-scoped-verification.md) and plan §10.5. Module: [projection](../../crates/investigation-store/src/projection.rs) · [scope](../../crates/investigation-store/src/scope.rs)

**Context.** `cache/index.sqlite` is the disposable index. It is opened with SQLite's defaults: a rollback journal (`journal_mode=DELETE`) and `synchronous=FULL`.

- **A commit** has one index transaction. A transaction costs four syncs: the journal twice, the cache directory and the database. A commit therefore does six syncs: two for the canonical event and four for the index (counted with `strace`).
- **A rebuild** (`repair index`, an index version change, a damaged index) wrote one such transaction per event into a temporary file. For 130 cases (8,320 objects) that was 9,554 syncs, which took 40 s on ext4 and 0.46 s on tmpfs.

ADR 0011 made commits trust the index for cross-case idempotency. PERF.09 extends this trust to acquisition keys and producer attempts. For each case a commit does not load, the commit asks the index whether that case already holds the command's `(actor, command_id)`, a record or transaction ID, or a claim. It trusts the answer only while the indexed head of that case is the case's canonical head. So the sync setting is a question of correctness: what can a torn or lost index write make the index answer?

## What a lost or torn index write can cause

The index answers for a case only when its indexed head is canonical. That means three things:

- the event file at the indexed sequence exists;
- the next event file does not exist;
- (PERF.09) the event file is the one the index applied, with the same device, inode, size, modification time and status-change time.

Every canonical event is synced, file and directory, before any index transaction applies it. So the index is never ahead of the journal, except in the failed-sync case ADR 0011 describes.

- **A lost index transaction** leaves the index one or more events behind. This happens when power fails after the commit but before the index write reaches the disk. The indexed head is then older than the canonical head, so the freshness test fails. The commit verifies that case and answers from its events. A lost write costs time but not correctness, if the loss removes whole transactions, so that a head row is never newer than the rows it covers.
- **A torn index transaction** leaves a head row that claims sequence N while the receipts, records, transactions or claims of N are missing. A transaction is torn when some of its pages reach the disk and others do not. The freshness test then passes, and the index says "no case holds this key" when one does. **A torn write can make a commit record a duplicate.** `check` still reports the duplicate ("An actor and command ID appear twice in the workspace"), but the duplicate stays in the journal. A torn write that only corrupts the file, so that it cannot be opened, is harmless: the commit verifies every case.

So the setting must keep each index transaction atomic and consistent across power loss. It does not need to make each transaction durable.

## Decision

1. **The live index keeps the rollback journal with `synchronous=FULL`.** This pair is atomic across power loss, and it never needs a reader to write. The alternatives:
   - *WAL with `synchronous=NORMAL`*, without a checkpoint on close, is also atomic. It would remove the index's syncs from the commit path. We measured 2 syncs per commit instead of 6, 17 ms instead of 49 ms in syncs on loaded ext4. But in WAL mode every reader writes the `-shm` file, a read-only connection included. Doctor, dry runs, `--read-only` and the other "writes nothing" commands open the index, and their tests found the changed `-shm`. Those commands promise to change no workspace byte, and a workspace on read-only media would need an existing `-shm`. WAL is safe for durability, but it breaks a contract we keep, so we did not adopt it. A later bone could adopt it with a reader path that does not open the index in WAL mode.
   - *`synchronous=NORMAL` with the rollback journal* is rejected. SQLite documents that a power loss in this mode can corrupt the database. That is a torn write, which hides a key.
   - *`synchronous=OFF`* is rejected for the same reason, with a wider window.
   - *`journal_mode=TRUNCATE` or `PERSIST`* saves only the directory sync. It also leaves a `-journal` file in the cache for good. We do not adopt it for one sync.
2. **A rebuild builds its temporary file without syncs and syncs it once.** Nothing reads the temporary file until it is renamed into place, so it is built with `journal_mode=MEMORY` and `synchronous=OFF`. The file is closed and synced, it replaces `index.sqlite`, and the cache directory is synced. A crash before the rename leaves only a `.index-*` file, which nothing reads. A crash after it leaves a complete, synced database. A rebuild did 9,554 syncs for 130 cases and now does 3.
3. **A rebuilt index never inherits the old database's journal.** SQLite pairs `index.sqlite` with `index.sqlite-journal`, and also `-wal` and `-shm`, by name only. Before this ADR, a rebuild renamed the new database over the old one and left a hot journal from a crashed writer in place. On the next open, SQLite rolled that journal into the new database. This was a latent defect of the old code. The fault test below shows the result: the new database went back to the old head, and with other pages it can be the torn state. Under the commit lock, which every index writer holds, a rebuild now does these steps in order:
   1. remove the old database and sync the directory;
   2. remove the three companion files;
   3. rename the new file and sync the directory.
   With this order, no crash leaves a torn old database without its journal, and no crash leaves the new database with the old journal. `Projection::open` creates the index when it is absent, and removes the companion files first. This covers a crash between steps 1 and 2, and an index deleted by hand. Readers do not take the lock, but they open read-only. A read-only connection never writes the database or rolls a journal back: it gets an error, and the command falls back to verifying.

## How `check` and `repair index` recover

- `check` verifies every case and compares the index heads (sequence and head hash) with the verified heads. It reports a lost transaction as `INDEX_STALE`, with `repair index` as the repair. It reports an index that cannot be opened the same way.
- `repair index` verifies every case and rebuilds the index under the commit lock, as decisions 2 and 3 describe.
- A commit verifies every case when it finds the index unusable: absent, of another version or workspace, not answering, or naming an unpublished case. It then brings the index up to date or rebuilds it.
- A case whose indexed head is not canonical is verified, and the commit's index update makes that case current. When only the head file changed, for example after the workspace was copied, the update records the file again.

## What the tests prove

- `the_live_index_keeps_a_fully_synced_rollback_journal` reads the journal mode and sync level of a live connection and of a rebuilt index. It also checks that a rebuild leaves no `.index-*` file and no companion file. It fails if the sync level goes down, if the journal mode changes, or if the rebuild's unsynced build mode leaks into the published file. A mutation that set `synchronous=NORMAL` failed it.
- `a_crashed_writers_journal_never_reaches_a_rebuilt_index` is the fault test. A writer changes the receipts and heads, then spills a large transaction to the database. The test copies the database and its hot journal as a crash leaves them, and rebuilds over that state. The rebuilt index must keep the new head and every receipt under it, both at once and after it is reopened. The test then simulates a crash between removing the old database and removing its journal, and checks that the next open starts clean. With the companion removal taken out, the reopened index went back to the old head, and the test failed.
- The PERF.09 scope tests (`scope_tests.rs`) show that a claim held by another case is found with a current, a stale and a deleted index. They also show that an indexed head whose file changed at the same sequence is verified, not trusted. The ADR 0011 tests still pass.

These tests do not inject a power loss into SQLite's own writes. The guarantee that the fully synced rollback journal is atomic is SQLite's, and it is kept by keeping that setting. The guard test enforces it. The LazyFS qualification (`scripts/qualify-lazyfs.sh`) covers canonical publication, not the index.

## Measurements

Measured with release builds, using the W12.08 load generator (`investigation-bench --load`). The machine is a shared 64-thread machine whose load average was 40 to 180 during these runs, so the times are noisy. The sync counts are exact.

| | before | after |
| --- | --- | --- |
| Syncs in one commit | 6 | 6 |
| Syncs in a rebuild, 130 cases | 9,554 | 3 |
| Rebuild, 130 cases, ext4 | 40.1 s | 0.9 to 8.1 s |
| Rebuild, 650 cases, ext4 | 251.5 s | 13.4 to 18.8 s |
| Rebuild, 650 cases, tmpfs | 2.5 to 3.2 s | 2.2 to 2.3 s |

The PERF.09 report on bn-352p has the commit timings for keyed ingest, capture begin and producer runs.
